//! One terminal: its PTY (alacritty's `tty`), the I/O thread that drives it,
//! the headless emulator fed the same bytes for snapshots, and the replay
//! ring of its recent output.

use std::collections::VecDeque;
use std::io::{ErrorKind, Read, Write};
use std::process::ExitStatus;
use std::sync::mpsc::{Receiver, Sender, TryRecvError};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use alacritty_terminal::event::{Event, EventListener, OnResize, WindowSize};
use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::term::{Config, Term};
use alacritty_terminal::tty::{self, ChildEvent, EventedPty, EventedReadWrite, Pty};
use alacritty_terminal::vte::ansi::Processor;
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use polling::{Event as Interest, Events, PollMode, Poller};
use serde_json::{Value, json};

use crate::host::{Outbox, ProtoError};
use crate::snapshot::{self, SCROLLBACK_LINES, ScrollRegion};

/// Output kept for clients resuming from an offset.
pub const REPLAY_BYTES: usize = 2 << 20;

/// Output a connection may have queued before it's detached from a term.
pub const QUEUE_CAP: usize = 8 << 20;

/// How long a closed term has after SIGHUP before SIGKILL.
const CLOSE_GRACE: Duration = Duration::from_secs(3);

/// Largest read batch turned into one `output` event.
const BATCH_BYTES: usize = 256 << 10;

const PTY_TOKEN: usize = 0;

pub struct Spec {
    pub cwd: String,
    pub argv: Vec<String>,
    pub env: Vec<(String, String)>,
    pub cols: u16,
    pub rows: u16,
}

/// What `Term` reports while parsing: titles, and replies to queries.
#[derive(Clone, Default)]
struct Listener(Arc<Mutex<Vec<Note>>>);

enum Note {
    Title(String),
    PtyWrite(String),
}

impl EventListener for Listener {
    fn send_event(&self, event: Event) {
        let note = match event {
            Event::Title(title) => Note::Title(title),
            Event::ResetTitle => Note::Title(String::new()),
            Event::PtyWrite(text) => Note::PtyWrite(text),
            _ => return,
        };
        lock(&self.0).push(note);
    }
}

struct Size {
    cols: u16,
    rows: u16,
}

impl Dimensions for Size {
    fn total_lines(&self) -> usize {
        self.rows as usize
    }
    fn screen_lines(&self) -> usize {
        self.rows as usize
    }
    fn columns(&self) -> usize {
        self.cols as usize
    }
}

fn window_size(cols: u16, rows: u16) -> WindowSize {
    WindowSize {
        num_lines: rows,
        num_cols: cols,
        cell_width: 0,
        cell_height: 0,
    }
}

/// The last [`REPLAY_BYTES`] of output, at absolute offsets.
struct Ring {
    bytes: VecDeque<u8>,
    end: u64,
}

impl Ring {
    fn push(&mut self, data: &[u8]) {
        self.end += data.len() as u64;
        let data = &data[data.len().saturating_sub(REPLAY_BYTES)..];
        let overflow = (self.bytes.len() + data.len()).saturating_sub(REPLAY_BYTES);
        self.bytes.drain(..overflow);
        self.bytes.extend(data);
    }

    /// The output since `from`, if the ring still has all of it.
    fn since(&self, from: u64) -> Option<Vec<u8>> {
        let start = self.end - self.bytes.len() as u64;
        if from < start || from > self.end {
            return None;
        }
        Some(
            self.bytes
                .range((from - start) as usize..)
                .copied()
                .collect(),
        )
    }
}

struct Subscriber {
    conn: u64,
    out: Arc<Outbox>,
}

struct State {
    emu: Term<Listener>,
    parser: Processor,
    region: ScrollRegion,
    region_parser: Processor,
    notes: Listener,
    ring: Ring,
    cols: u16,
    rows: u16,
    title: String,
    exit_code: Option<i32>,
    subscribers: Vec<Subscriber>,
}

impl State {
    fn broadcast(&mut self, event: &Value) {
        let line = event_line(event);
        for sub in &self.subscribers {
            sub.out.send(line.clone());
        }
    }

    /// Ends a synchronized update early, so the emulator has every byte the
    /// ring has.
    fn flush_sync(&mut self) {
        if self.parser.sync_timeout().sync_timeout().is_some() {
            self.parser.stop_sync(&mut self.emu);
        }
        if self.region_parser.sync_timeout().sync_timeout().is_some() {
            self.region_parser.stop_sync(&mut self.region);
        }
    }
}

enum Command {
    Write(Vec<u8>),
    Resize(u16, u16),
    Close,
}

pub struct TermHandle {
    pub id: String,
    pub pid: u32,
    pub cwd: String,
    pub argv: Vec<String>,
    /// Milliseconds since the Unix epoch.
    pub created_at: u64,
    state: Mutex<State>,
    commands: Sender<Command>,
    poller: Arc<Poller>,
}

pub fn event_line(event: &Value) -> Arc<[u8]> {
    let mut line = serde_json::to_vec(event).expect("JSON values serialize");
    line.push(b'\n');
    line.into()
}

pub fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

impl TermHandle {
    /// Starts `spec`'s program in a new PTY and the thread that drives it.
    pub fn open(id: String, spec: Spec, created_at: u64) -> std::io::Result<Arc<Self>> {
        let mut env = std::collections::HashMap::from([
            ("TERM".to_string(), "xterm-256color".to_string()),
            ("COLORTERM".to_string(), "truecolor".to_string()),
            ("TERM_PROGRAM".to_string(), "GentleShell".to_string()),
        ]);
        env.extend(spec.env);
        let options = tty::Options {
            shell: Some(tty::Shell::new(
                spec.argv[0].clone(),
                spec.argv[1..].to_vec(),
            )),
            working_directory: Some(spec.cwd.clone().into()),
            drain_on_exit: true,
            env,
        };
        let mut pty = tty::new(&options, window_size(spec.cols, spec.rows), 0)?;
        let poller = Arc::new(Poller::new()?);
        // SAFETY: the PTY is deregistered before it's dropped (`drive`).
        unsafe { pty.register(&poller, Interest::readable(PTY_TOKEN), PollMode::Level)? };

        let notes = Listener::default();
        let config = Config {
            scrolling_history: SCROLLBACK_LINES,
            ..Config::default()
        };
        let size = Size {
            cols: spec.cols,
            rows: spec.rows,
        };
        let state = State {
            emu: Term::new(config, &size, notes.clone()),
            parser: Processor::new(),
            region: ScrollRegion::new(spec.rows as usize),
            region_parser: Processor::new(),
            notes,
            ring: Ring {
                bytes: VecDeque::new(),
                end: 0,
            },
            cols: spec.cols,
            rows: spec.rows,
            title: String::new(),
            exit_code: None,
            subscribers: Vec::new(),
        };
        let (commands, receiver) = std::sync::mpsc::channel();
        let term = Arc::new(Self {
            id,
            pid: pty.child().id(),
            cwd: spec.cwd,
            argv: spec.argv,
            created_at,
            state: Mutex::new(state),
            commands,
            poller,
        });
        let driven = term.clone();
        std::thread::Builder::new()
            .name(format!("term {}", term.id))
            .spawn(move || driven.drive(pty, receiver))?;
        Ok(term)
    }

    pub fn alive(&self) -> bool {
        lock(&self.state).exit_code.is_none()
    }

    pub fn info(&self) -> Value {
        let state = lock(&self.state);
        let mut info = json!({
            "term": self.id,
            "pid": self.pid,
            "cwd": self.cwd,
            "argv": self.argv,
            "cols": state.cols,
            "rows": state.rows,
            "title": state.title,
            "alive": state.exit_code.is_none(),
            "createdAt": self.created_at,
            "offset": state.ring.end,
        });
        if let Some(code) = state.exit_code {
            info["exitCode"] = code.into();
        }
        info
    }

    fn command(&self, command: Command) {
        // The thread is gone only once the term has exited.
        let _ = self.commands.send(command);
        let _ = self.poller.notify();
    }

    pub fn write(&self, data: Vec<u8>) -> Result<(), ProtoError> {
        if !self.alive() {
            return Err(ProtoError::exited());
        }
        self.command(Command::Write(data));
        Ok(())
    }

    pub fn resize(&self, cols: u16, rows: u16) -> Result<(), ProtoError> {
        let mut state = lock(&self.state);
        if state.exit_code.is_some() {
            return Err(ProtoError::exited());
        }
        if (state.cols, state.rows) != (cols, rows) {
            state.emu.resize(Size { cols, rows });
            state.region.resize(rows as usize);
            state.cols = cols;
            state.rows = rows;
            let event = json!({ "ev": "resized", "term": self.id, "cols": cols, "rows": rows });
            state.broadcast(&event);
            self.command(Command::Resize(cols, rows));
        }
        Ok(())
    }

    /// SIGHUP now, SIGKILL after [`CLOSE_GRACE`] if it's still running.
    pub fn close(&self) {
        self.command(Command::Close);
    }

    /// SIGHUP, for the host shutting down.
    pub fn hang_up(&self) {
        if self.alive() {
            // SAFETY: plain kill(2).
            unsafe { libc::kill(self.pid as i32, libc::SIGHUP) };
        }
    }

    /// Subscribes `out` to this term's events, replying to request `id`
    /// first: from `from` when the ring still has it, else with a snapshot.
    /// Under the state lock, so nothing is missed or sent twice.
    pub fn attach(&self, conn: u64, out: &Arc<Outbox>, id: &Value, from: Option<u64>) {
        let mut state = lock(&self.state);
        let state = &mut *state;
        let end = state.ring.end;
        match from.and_then(|from| Some((from, state.ring.since(from)?))) {
            Some((from, replay)) => {
                out.send(event_line(&json!({ "id": id, "ok": true, "offset": from })));
                if !replay.is_empty() {
                    out.send(output_line(&self.id, end, &replay));
                }
            }
            None => {
                state.flush_sync();
                let title = Some(state.title.as_str());
                let bytes = snapshot::snapshot(&mut state.emu, &state.region, title);
                out.send(event_line(&json!({
                    "id": id,
                    "ok": true,
                    "offset": end,
                    "snapshot": BASE64.encode(bytes),
                })));
            }
        }
        match state.exit_code {
            Some(code) => out.send(event_line(
                &json!({ "ev": "exit", "term": self.id, "code": code }),
            )),
            None => {
                state.subscribers.retain(|sub| sub.conn != conn);
                state.subscribers.push(Subscriber {
                    conn,
                    out: out.clone(),
                });
            }
        }
    }

    pub fn detach(&self, conn: u64) {
        lock(&self.state).subscribers.retain(|sub| sub.conn != conn);
    }

    /// Feeds output to the emulator, the ring and the subscribers. Returns
    /// replies to the program's queries, written for it while no client is
    /// attached (an attached client's emulator answers them).
    fn ingest(&self, data: &[u8]) -> Vec<u8> {
        let mut state = lock(&self.state);
        let state = &mut *state;
        state.parser.advance(&mut state.emu, data);
        state.region_parser.advance(&mut state.region, data);
        state.ring.push(data);
        let line = output_line(&self.id, state.ring.end, data);
        let overflow = event_line(&json!({ "ev": "overflow", "term": self.id }));
        state.subscribers.retain(|sub| {
            if sub.out.try_send(line.clone(), QUEUE_CAP) {
                return true;
            }
            sub.out.send(overflow.clone());
            false
        });

        let notes = std::mem::take(&mut *lock(&state.notes.0));
        let mut replies = Vec::new();
        for note in notes {
            match note {
                Note::Title(title) => {
                    state.broadcast(&json!({ "ev": "title", "term": self.id, "title": title }));
                    state.title = title;
                }
                Note::PtyWrite(text) if state.subscribers.is_empty() => {
                    replies.extend_from_slice(text.as_bytes());
                }
                Note::PtyWrite(_) => {}
            }
        }
        replies
    }

    fn finish(&self, code: i32) {
        let mut state = lock(&self.state);
        state.exit_code = Some(code);
        state.broadcast(&json!({ "ev": "exit", "term": self.id, "code": code }));
        state.subscribers.clear();
    }

    /// The I/O thread: PTY output in, input out, commands, the child's exit.
    fn drive(self: Arc<Self>, mut pty: Pty, commands: Receiver<Command>) {
        let poller = self.poller.clone();
        let mut events = Events::new();
        let mut buf = vec![0u8; 64 << 10];
        let mut input = VecDeque::<u8>::new();
        let mut interest = Interest::readable(PTY_TOKEN);
        let mut kill_at: Option<Instant> = None;
        let status = 'run: loop {
            let sync_at = {
                let state = lock(&self.state);
                state.parser.sync_timeout().sync_timeout()
            };
            let deadline = [kill_at, sync_at].into_iter().flatten().min();
            let timeout = deadline.map(|at| at.saturating_duration_since(Instant::now()));
            events.clear();
            if let Err(err) = poller.wait(&mut events, timeout) {
                if err.kind() == ErrorKind::Interrupted {
                    continue;
                }
                crate::log(&format!("{}: poll failed: {err}", self.id));
                break 'run None;
            }

            let mut exited = None;
            for event in events.iter() {
                if event.key == PTY_TOKEN {
                    if event.readable && !self.read(&mut pty, &mut buf, &mut input) {
                        // The other side hung up; the child's exit follows.
                        interest.readable = false;
                    }
                    if event.writable {
                        write_input(&mut pty, &mut input);
                    }
                } else if let Some(ChildEvent::Exited(status)) = pty.next_child_event() {
                    exited = Some(status);
                }
            }

            loop {
                match commands.try_recv() {
                    Ok(Command::Write(data)) => input.extend(data),
                    Ok(Command::Resize(cols, rows)) => pty.on_resize(window_size(cols, rows)),
                    Ok(Command::Close) => {
                        // SAFETY: plain kill(2).
                        unsafe { libc::kill(self.pid as i32, libc::SIGHUP) };
                        kill_at.get_or_insert(Instant::now() + CLOSE_GRACE);
                    }
                    Err(TryRecvError::Empty | TryRecvError::Disconnected) => break,
                }
            }
            if !input.is_empty() {
                write_input(&mut pty, &mut input);
            }

            let now = Instant::now();
            if kill_at.is_some_and(|at| at <= now) {
                // SAFETY: plain kill(2); the child leads its process group.
                unsafe {
                    libc::kill(-(self.pid as i32), libc::SIGKILL);
                    libc::kill(self.pid as i32, libc::SIGKILL);
                }
                kill_at = None;
            }
            if sync_at.is_some_and(|at| at <= now) {
                lock(&self.state).flush_sync();
            }

            if let Some(status) = exited {
                // What the child wrote before exiting.
                self.read(&mut pty, &mut buf, &mut input);
                break 'run status;
            }

            interest.writable = !input.is_empty();
            if let Err(err) = pty.reregister(&poller, interest, PollMode::Level) {
                crate::log(&format!("{}: poll failed: {err}", self.id));
                break 'run None;
            }
        };
        let _ = pty.deregister(&poller);
        // Dropping the PTY hangs up and reaps the child, which has exited
        // unless polling failed.
        drop(pty);
        self.finish(status.map_or(-1, exit_code));
    }

    /// Reads what's available. False once the PTY is closed.
    fn read(&self, pty: &mut Pty, buf: &mut [u8], input: &mut VecDeque<u8>) -> bool {
        let mut batch = Vec::new();
        let open = loop {
            match pty.reader().read(buf) {
                Ok(0) => break false,
                Ok(n) => {
                    batch.extend_from_slice(&buf[..n]);
                    if batch.len() >= BATCH_BYTES {
                        input.extend(self.ingest(&batch));
                        batch.clear();
                    }
                }
                Err(err) if err.kind() == ErrorKind::Interrupted => {}
                Err(err) if err.kind() == ErrorKind::WouldBlock => break true,
                Err(_) => break false,
            }
        };
        if !batch.is_empty() {
            input.extend(self.ingest(&batch));
        }
        open
    }
}

fn write_input(pty: &mut Pty, input: &mut VecDeque<u8>) {
    while !input.is_empty() {
        let (front, _) = input.as_slices();
        match pty.writer().write(front) {
            Ok(n) => {
                input.drain(..n);
            }
            Err(err) if err.kind() == ErrorKind::Interrupted => {}
            Err(err) if err.kind() == ErrorKind::WouldBlock => return,
            // Nobody reads it anymore.
            Err(_) => {
                input.clear();
                return;
            }
        }
    }
}

fn output_line(term: &str, offset: u64, data: &[u8]) -> Arc<[u8]> {
    event_line(&json!({
        "ev": "output",
        "term": term,
        "offset": offset,
        "data": BASE64.encode(data),
    }))
}

/// The exit status as a shell reports it: the code, or 128 + the signal.
fn exit_code(status: ExitStatus) -> i32 {
    use std::os::unix::process::ExitStatusExt as _;
    status
        .code()
        .or_else(|| status.signal().map(|signal| 128 + signal))
        .unwrap_or(-1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ring_keeps_the_tail_at_absolute_offsets() {
        let mut ring = Ring {
            bytes: VecDeque::new(),
            end: 0,
        };
        ring.push(b"hello");
        assert_eq!(ring.since(0).as_deref(), Some(&b"hello"[..]));
        assert_eq!(ring.since(3).as_deref(), Some(&b"lo"[..]));
        assert_eq!(ring.since(5).as_deref(), Some(&b""[..]));
        assert_eq!(ring.since(6), None);

        ring.push(&vec![b'x'; REPLAY_BYTES]);
        assert_eq!(ring.end, 5 + REPLAY_BYTES as u64);
        assert_eq!(ring.since(4), None);
        assert_eq!(ring.since(5).map(|b| b.len()), Some(REPLAY_BYTES));
        ring.push(b"!");
        assert_eq!(ring.since(5), None);
        assert_eq!(ring.since(6).map(|b| b.len()), Some(REPLAY_BYTES));
    }
}
