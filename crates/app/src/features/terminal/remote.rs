//! A terminal pane on a Gentle Shell server: no PTY on this computer but
//! one of the server's persistent terminals (`/api/terminals`, kept by its
//! `gentle-term-host`), drawn by the pane's own `Term`. A reader thread
//! follows the terminal's stream (reconnecting after where it left off) and
//! hands its bytes to a drawing thread, which feeds them through the vte
//! parser as alacritty's event loop does; a writer thread sends keystrokes,
//! the `Term`'s replies and sizes. Quitting the app detaches (the terminal
//! keeps running there and the restored pane reattaches); closing the pane
//! closes it there. A server without terminals, or a device that may not
//! open them, gets the pane's way out instead: installing the terminal host,
//! or pairing again.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use alacritty_terminal::event::{Event as TermEvent, EventListener as _};
use alacritty_terminal::grid::Dimensions as _;
use alacritty_terminal::sync::FairMutex;
use alacritty_terminal::term::{Config as TermConfig, Term};
use alacritty_terminal::vte::ansi::{Processor, StdSyncHandler};
use gentle_core::backend::Backend;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::{ActiveTheme as _, Disableable as _, Sizable as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use pi_rpc::serve::{self, ServeClient, ServeError, TermHost};

use super::{Listener, PADDING, Pty, TermDims, TerminalEvent, TerminalView};
use crate::features::backend::{self, server};
use crate::features::settings::{ConnectEvent, ServerConnect};
use crate::shared::ui;

/// How long sizes settle before the last one goes to the server: dragging
/// a split resizes the pane every frame.
const RESIZE_SETTLE: Duration = Duration::from_millis(120);

/// The first wait before attaching again after the stream dropped, and the
/// longest.
const RETRY_FIRST: Duration = Duration::from_millis(250);
const RETRY_MAX: Duration = Duration::from_secs(5);

/// Why installing the terminal host was refused.
const NEEDS_ADMIN: &str = "Installing needs the admin scope, which this device wasn't paired \
     with. On the server, run `gentle-shell serve pair --scope \
     sessions:read,sessions:operate,terminal,admin` and pair again with that code below, or \
     install it on the server yourself.";

/// What goes to the server terminal, in order.
enum Command {
    /// Bytes as typed (keys, pastes, IME text) or the `Term`'s replies.
    Input(Vec<u8>),
    /// The pane's size in columns and rows.
    Resize(u16, u16),
}

/// What the drawing thread feeds the `Term`.
enum Feed {
    /// The terminal's screen as it is, an ANSI sequence for a fresh
    /// emulator of its size (`cols` × `rows`).
    Reset {
        snapshot: Vec<u8>,
        cols: u16,
        rows: u16,
    },
    /// Output as it comes.
    Output(Vec<u8>),
}

/// What the threads tell the pane.
pub(super) enum Status {
    /// The terminal is open, or reattached ([`Remote::id`] has its id).
    Opened,
    /// What keeps the pane from having one.
    Problem(Problem),
    /// The terminal ended there without exiting: why.
    Lost(String),
}

/// What keeps a server terminal pane from having its terminal.
#[derive(Clone, Debug)]
pub(super) enum Problem {
    /// Why, in words: not paired, unreachable, a server too old.
    Error(String),
    /// The server has no terminal host; `bundled` is this app's own, when
    /// it fits the server.
    NoHost {
        host: TermHost,
        bundled: Option<PathBuf>,
    },
    /// This device may not open terminals there (paired without the
    /// `terminal` scope).
    NoScope,
}

/// The pane's way out of a [`Problem`].
pub(super) struct Blocked {
    problem: Problem,
    /// The server, as the user named it.
    server: String,
    /// Pairing this device again: for a missing scope, or the `admin` an
    /// install needs.
    connect: Option<(Entity<ServerConnect>, Subscription)>,
    /// A terminal host is being installed.
    installing: bool,
    install_error: Option<String>,
    install: Task<()>,
}

/// Where a terminal's output resumes: the offset after the last byte drawn
/// (`from` when attaching again).
#[derive(Debug, Default)]
struct Resume {
    from: Option<u64>,
}

impl Resume {
    /// An attachment: output continues at `offset` (after a snapshot, the
    /// end of what it shows).
    fn attached(&mut self, offset: u64) {
        self.from = Some(offset);
    }

    /// The part of `data`, output ending at `offset`, not drawn yet; the
    /// resume point moves past it.
    fn fresh<'a>(&mut self, offset: u64, data: &'a [u8]) -> &'a [u8] {
        let start = offset.saturating_sub(data.len() as u64);
        let seen = match self.from {
            Some(from) if from > start => (from - start).min(data.len() as u64) as usize,
            _ => 0,
        };
        if self.from.is_none_or(|from| offset > from) {
            self.from = Some(offset);
        }
        &data[seen..]
    }
}

/// How long to wait before attaching again after `failures` attempts in a
/// row: doubling from [`RETRY_FIRST`] up to [`RETRY_MAX`].
fn backoff(failures: u32) -> Duration {
    RETRY_FIRST
        .saturating_mul(1 << failures.min(16))
        .min(RETRY_MAX)
}

/// Whether a server whose Node says `platform` and `arch` runs what this
/// app is built for, `os` and `rust_arch` as `std::env::consts` names them.
fn same_build(platform: &str, arch: &str, os: &str, rust_arch: &str) -> bool {
    let os = match os {
        "macos" => "darwin",
        "windows" => "win32",
        other => other,
    };
    let rust_arch = match rust_arch {
        "aarch64" => "arm64",
        "x86_64" => "x64",
        "x86" => "ia32",
        other => other,
    };
    platform == os && arch == rust_arch
}

/// This app's own `gentle-term-host`, when it runs on `host`'s server:
/// beside the app's executable (in the app bundle, or a cargo target folder
/// in development, where the other profile's build counts too).
fn bundled_for(host: &TermHost) -> Option<PathBuf> {
    if !same_build(
        &host.platform,
        &host.arch,
        std::env::consts::OS,
        std::env::consts::ARCH,
    ) {
        return None;
    }
    let name = if cfg!(windows) {
        "gentle-term-host.exe"
    } else {
        "gentle-term-host"
    };
    let exe = std::env::current_exe().ok()?;
    let dir = exe.parent()?;
    let mut places = vec![dir.to_path_buf()];
    if let Some(target) = dir.parent() {
        places.extend(["release", "debug"].map(|profile| target.join(profile)));
    }
    places
        .into_iter()
        .map(|place| place.join(name))
        .find(|path| path.is_file())
}

/// What an error from the server's terminal endpoints means for the pane.
fn problem(client: &ServeClient, error: ServeError) -> Problem {
    match (error.status, error.error.as_deref()) {
        (Some(403), Some("forbidden")) => Problem::NoScope,
        (Some(501), _) => match client.term_host() {
            Ok(host) if !host.available => Problem::NoHost {
                bundled: bundled_for(&host),
                host,
            },
            _ => Problem::Error(error.message),
        },
        _ => Problem::Error(error.message),
    }
}

/// The terminal the pane shows on `choice`: `resume` while it still runs
/// there, else a new one in `cwd` at `cols` × `rows`; or what keeps the
/// pane from having one. Blocking.
fn open(
    choice: &Backend,
    cwd: &Path,
    resume: Option<&str>,
    (cols, rows): (u16, u16),
) -> Result<(ServeClient, serve::Terminal), Problem> {
    let client = server::client(choice).map_err(|e| Problem::Error(e.to_string()))?;
    if !server::offers(choice, "terminals") {
        let host = match client.term_host() {
            Ok(host) => host,
            Err(error) if error.status == Some(404) => {
                return Err(Problem::Error(server::UNAVAILABLE.into()));
            }
            Err(error) => return Err(problem(&client, error)),
        };
        if !host.available {
            return Err(Problem::NoHost {
                bundled: bundled_for(&host),
                host,
            });
        }
        // Installed since the server last said what it offers.
        server::forget_info(choice);
    }
    if let Some(id) = resume {
        let terminals = client.terminals().map_err(|e| problem(&client, e))?;
        match terminals.into_iter().find(|t| t.id == id) {
            Some(terminal) if terminal.alive => return Ok((client, terminal)),
            // It ended while the app was closed: forget it there.
            Some(_) => {
                let _ = client.close_terminal(id);
            }
            None => {}
        }
    }
    let terminal = client
        .open_terminal(&cwd.to_string_lossy(), cols, rows)
        .map_err(|e| problem(&client, e))?;
    Ok((client, terminal))
}

/// What the pane's handles share with its threads.
struct Shared {
    /// The pane went: the threads stop; the terminal keeps running.
    stop: AtomicBool,
    /// The pane closed: the terminal closes there too, even one still
    /// opening.
    closing: AtomicBool,
    /// The open terminal's client and id.
    opened: Mutex<Option<(ServeClient, String)>>,
}

/// A server terminal as its pane drives it. Clones share it.
#[derive(Clone)]
pub(super) struct Remote {
    commands: mpsc::Sender<Command>,
    shared: Arc<Shared>,
}

impl Remote {
    /// Open (or reattach to `resume`) a terminal in `cwd`, on the server
    /// that folder is on, drawn into `term`; `status` hears how it went.
    fn start(
        cwd: PathBuf,
        resume: Option<String>,
        term: Arc<FairMutex<Term<Listener>>>,
        listener: Listener,
        status: async_channel::Sender<Status>,
    ) -> Self {
        let (commands, received) = mpsc::channel();
        let shared = Arc::new(Shared {
            stop: AtomicBool::new(false),
            closing: AtomicBool::new(false),
            opened: Mutex::new(None),
        });
        let thread_shared = shared.clone();
        std::thread::spawn(move || {
            run(
                &cwd,
                resume.as_deref(),
                term,
                listener,
                status,
                received,
                &thread_shared,
            )
        });
        Self { commands, shared }
    }

    pub(super) fn input(&self, bytes: Vec<u8>) {
        let _ = self.commands.send(Command::Input(bytes));
    }

    pub(super) fn resize(&self, cols: u16, rows: u16) {
        let _ = self.commands.send(Command::Resize(cols, rows));
    }

    /// The terminal's id, once open.
    fn id(&self) -> Option<String> {
        self.shared
            .opened
            .lock()
            .unwrap()
            .as_ref()
            .map(|(_, id)| id.clone())
    }

    /// Stop following the terminal; it keeps running there.
    pub(super) fn detach(&self) {
        self.shared.stop.store(true, Ordering::Release);
    }

    /// Close the terminal there (in the background), or as soon as it opens.
    fn close(&self, cx: &App) {
        let opened = {
            let opened = self.shared.opened.lock().unwrap();
            self.shared.closing.store(true, Ordering::Release);
            opened.clone()
        };
        self.detach();
        if let Some((client, id)) = opened {
            cx.background_executor()
                .spawn(async move { close_terminal(&client, &id) })
                .detach();
        }
    }
}

/// `DELETE` the terminal `id`, saying why when that failed (a terminal
/// already gone is closed).
fn close_terminal(client: &ServeClient, id: &str) {
    match client.close_terminal(id) {
        Err(error) if error.status != Some(404) => {
            eprintln!("gentle-shell-gpui: could not close the server terminal {id}: {error}")
        }
        _ => {}
    }
}

/// Close the server terminal `id` a pane in `cwd` had but never reattached
/// to (its tab was never shown), in the background. Nothing off a server.
pub fn close_on_server(cwd: &Path, id: String, cx: &App) {
    let cwd = cwd.to_path_buf();
    cx.background_executor()
        .spawn(async move {
            if let Some(Ok(client)) = server::at(&cwd) {
                close_terminal(&client, &id);
            }
        })
        .detach();
}

/// The connection thread: open the terminal, start the writer and drawing
/// threads, then follow its stream.
fn run(
    cwd: &Path,
    resume: Option<&str>,
    term: Arc<FairMutex<Term<Listener>>>,
    listener: Listener,
    status: async_channel::Sender<Status>,
    commands: mpsc::Receiver<Command>,
    shared: &Shared,
) {
    let choice = backend::for_tab(cwd);
    let ours = {
        let term = term.lock();
        (term.columns() as u16, term.screen_lines() as u16)
    };
    let (client, terminal) = match open(&choice, cwd, resume, ours) {
        Ok(opened) => opened,
        Err(problem) => {
            let _ = status.send_blocking(Status::Problem(problem));
            return;
        }
    };
    {
        let mut opened = shared.opened.lock().unwrap();
        if shared.closing.load(Ordering::Acquire) {
            drop(opened);
            close_terminal(&client, &terminal.id);
            return;
        }
        if shared.stop.load(Ordering::Acquire) {
            return;
        }
        *opened = Some((client.clone(), terminal.id.clone()));
    }
    let _ = status.send_blocking(Status::Opened);
    // A reattached terminal has the size it had: the pane's goes out.
    let resize = (ours != (terminal.cols, terminal.rows)).then_some(ours);
    std::thread::spawn({
        let (client, id) = (client.clone(), terminal.id.clone());
        move || write(&client, &id, &commands, resize)
    });
    let (feed, fed) = mpsc::channel();
    std::thread::spawn({
        let listener = listener.clone();
        move || draw(&term, &listener, &fed)
    });
    if let Some(lost) = follow(&client, &terminal, &listener, &feed, &status, &shared.stop) {
        let _ = status.send_blocking(lost);
    }
}

/// Follow the terminal's stream until it exits, is lost or the pane goes,
/// attaching again after where it left off when the stream drops or falls
/// behind. What to tell the pane when it can't go on.
fn follow(
    client: &ServeClient,
    terminal: &serve::Terminal,
    listener: &Listener,
    feed: &mpsc::Sender<Feed>,
    status: &async_channel::Sender<Status>,
    stop: &AtomicBool,
) -> Option<Status> {
    let mut resume = Resume::default();
    let mut size = (terminal.cols, terminal.rows);
    let mut failures = 0;
    while !stop.load(Ordering::Acquire) {
        let mut stream = match client.terminal_stream(&terminal.id, resume.from) {
            Ok(stream) => stream,
            Err(error) => match (error.status, error.error.as_deref()) {
                (Some(404), _) => {
                    return Some(Status::Lost(
                        "This terminal was closed on the server.".into(),
                    ));
                }
                (Some(403), Some("forbidden")) => return Some(Status::Problem(Problem::NoScope)),
                _ => {
                    pause(backoff(failures), stop);
                    failures += 1;
                    continue;
                }
            },
        };
        while let Some(event) = stream.next(stop) {
            match event {
                serve::TermEvent::Attached { offset, snapshot } => {
                    failures = 0;
                    resume.attached(offset);
                    if let Some(snapshot) = snapshot {
                        let (cols, rows) = size;
                        let _ = feed.send(Feed::Reset {
                            snapshot,
                            cols,
                            rows,
                        });
                    }
                }
                serve::TermEvent::Output { offset, data } => {
                    let fresh = resume.fresh(offset, &data);
                    if !fresh.is_empty() {
                        let _ = feed.send(Feed::Output(fresh.to_vec()));
                    }
                }
                serve::TermEvent::Title(title) if title.trim().is_empty() => {
                    listener.send_event(TermEvent::ResetTitle)
                }
                serve::TermEvent::Title(title) => listener.send_event(TermEvent::Title(title)),
                serve::TermEvent::Resized { cols, rows } => size = (cols, rows),
                serve::TermEvent::Exit(_) => {
                    listener.send_event(TermEvent::Exit);
                    return None;
                }
                serve::TermEvent::Overflow => break,
                serve::TermEvent::HostGone => {
                    return Some(Status::Lost(
                        "The server's terminal host went away, and this terminal with it.".into(),
                    ));
                }
            }
        }
        if status.is_closed() {
            return None;
        }
        pause(backoff(failures), stop);
        failures += 1;
    }
    None
}

/// Wait `duration`, or until `stop` is set.
fn pause(duration: Duration, stop: &AtomicBool) {
    let until = Instant::now() + duration;
    while !stop.load(Ordering::Acquire) {
        let left = until.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return;
        }
        std::thread::sleep(left.min(Duration::from_millis(100)));
    }
}

/// The writer thread: keystrokes as they come (those already waiting go
/// together), sizes once they settle ([`RESIZE_SETTLE`]); `resize` is one
/// to send first. Ends when the pane's handles are gone.
fn write(
    client: &ServeClient,
    id: &str,
    commands: &mpsc::Receiver<Command>,
    mut resize: Option<(u16, u16)>,
) {
    loop {
        let first = if resize.is_some() {
            match commands.recv_timeout(RESIZE_SETTLE) {
                Ok(command) => Some(command),
                Err(RecvTimeoutError::Timeout) => None,
                Err(RecvTimeoutError::Disconnected) => return,
            }
        } else {
            match commands.recv() {
                Ok(command) => Some(command),
                Err(_) => return,
            }
        };
        let Some(first) = first else {
            if let Some((cols, rows)) = resize.take()
                && let Err(error) = client.terminal_resize(id, cols, rows)
                && error.status != Some(409)
            {
                eprintln!("gentle-shell-gpui: could not resize the server terminal: {error}");
            }
            continue;
        };
        let mut typed = Vec::new();
        for command in std::iter::once(first).chain(commands.try_iter()) {
            match command {
                Command::Input(bytes) => typed.extend(bytes),
                Command::Resize(cols, rows) => resize = Some((cols, rows)),
            }
        }
        // An exited terminal refuses input (409); its stream says so.
        if !typed.is_empty()
            && let Err(error) = client.terminal_input(id, &typed)
            && error.status != Some(409)
        {
            eprintln!("gentle-shell-gpui: could not send keys to the server terminal: {error}");
        }
    }
}

/// The drawing thread: what the stream brings, through the vte parser into
/// the `Term`, as alacritty's event loop does (synchronized updates end on
/// their own after their timeout). Ends when the stream's thread does.
fn draw(term: &FairMutex<Term<Listener>>, listener: &Listener, feed: &mpsc::Receiver<Feed>) {
    let mut parser: Processor<StdSyncHandler> = Processor::new();
    loop {
        let next = match parser.sync_timeout().sync_timeout() {
            Some(deadline) => feed.recv_timeout(deadline.saturating_duration_since(Instant::now())),
            None => feed.recv().map_err(|_| RecvTimeoutError::Disconnected),
        };
        match next {
            Ok(Feed::Output(bytes)) => parser.advance(&mut *term.lock(), &bytes),
            // A fresh emulator of the terminal's size takes the snapshot,
            // then the pane's size (it sends its own when they differ).
            Ok(Feed::Reset {
                snapshot,
                cols,
                rows,
            }) => {
                parser = Processor::new();
                let mut term = term.lock();
                let ours = TermDims {
                    columns: term.columns(),
                    lines: term.screen_lines(),
                };
                let theirs = TermDims {
                    columns: usize::from(cols.max(1)),
                    lines: usize::from(rows.max(1)),
                };
                *term = Term::new(TermConfig::default(), &theirs, listener.clone());
                parser.advance(&mut *term, &snapshot);
                term.resize(ours);
            }
            Err(RecvTimeoutError::Timeout) => parser.stop_sync(&mut *term.lock()),
            Err(RecvTimeoutError::Disconnected) => return,
        }
        listener.send_event(TermEvent::Wakeup);
    }
}

impl TerminalView {
    /// Reattach to the server terminal `id` (a restored pane's) rather than
    /// open a new one; a terminal gone or exited there gives a new one.
    pub fn resume_server_terminal(&mut self, id: String) {
        self.resume = Some(id);
    }

    /// The server terminal this pane shows (or a restored pane will
    /// reattach to), to save with the pane.
    pub fn server_terminal(&self) -> Option<String> {
        match &self.pty {
            Some(Pty::Server(remote)) => remote.id(),
            _ => None,
        }
        .or_else(|| self.resume.clone())
    }

    /// The pane closes for good: its server terminal closes there too
    /// (dropping the pane, as quitting does, only detaches from it). A
    /// local shell ends when the pane drops.
    pub fn close(&self, cx: &App) {
        match &self.pty {
            Some(Pty::Server(remote)) => remote.close(cx),
            _ => {
                if let Some(id) = self.resume.clone() {
                    close_on_server(&self.cwd, id, cx);
                }
            }
        }
    }

    /// The pane was laid out at a new size before its server terminal
    /// opened: open it once no other size came for [`RESIZE_SETTLE`]. A
    /// window still taking its saved bounds lays the pane out at a passing
    /// size first; a full-screen program reattached through it would keep a
    /// garbled screen when the final size is the one it already had.
    pub(super) fn settle_start(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.start_on_layout {
            return;
        }
        self.start_settle = Some(cx.spawn_in(window, async move |this, cx| {
            cx.background_executor().timer(RESIZE_SETTLE).await;
            let _ = this.update_in(cx, |this, window, cx| {
                if std::mem::take(&mut this.start_on_layout) {
                    this.start_remote(window, cx);
                }
            });
        }));
    }

    /// Open the pane's server terminal (again, after its problem was
    /// solved).
    pub(super) fn start_remote(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(Pty::Server(old)) = self.pty.take() {
            old.detach();
        }
        self.blocked = None;
        self.error = None;
        let (status, statuses) = async_channel::unbounded();
        self.pty = Some(Pty::Server(Remote::start(
            self.cwd.clone(),
            self.resume.clone(),
            self.term.clone(),
            self.listener.clone(),
            status,
        )));
        self.remote_status = cx.spawn_in(window, async move |this, cx| {
            while let Ok(status) = statuses.recv().await {
                let alive =
                    this.update_in(cx, |this, window, cx| this.on_status(status, window, cx));
                if alive.is_err() {
                    break;
                }
            }
        });
        cx.notify();
    }

    fn on_status(&mut self, status: Status, window: &mut Window, cx: &mut Context<Self>) {
        match status {
            Status::Opened => {
                self.resume = None;
                cx.emit(TerminalEvent::Attached);
            }
            Status::Problem(problem) => {
                if let Some(Pty::Server(remote)) = self.pty.take() {
                    remote.detach();
                }
                let server = match backend::for_tab(&self.cwd) {
                    Backend::Server { name, .. } => name,
                    other => other.label(),
                };
                let mut blocked = Blocked {
                    problem,
                    server,
                    connect: None,
                    installing: false,
                    install_error: None,
                    install: Task::ready(()),
                };
                if matches!(blocked.problem, Problem::NoScope) {
                    blocked.connect = Some(self.pair_again(window, cx));
                }
                self.blocked = Some(blocked);
            }
            Status::Lost(why) => {
                self.error = Some(why.into());
                self.exited = true;
            }
        }
        cx.notify();
    }

    /// The pairing fields for this pane's server: once paired again (the
    /// new token replaces the old for its address), the terminal is opened
    /// again.
    fn pair_again(
        &self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> (Entity<ServerConnect>, Subscription) {
        let url = match backend::for_tab(&self.cwd) {
            Backend::Server { url, .. } => url,
            _ => String::new(),
        };
        let connect = cx.new(|cx| ServerConnect::new(url, window, cx));
        let paired = cx.subscribe_in(
            &connect,
            window,
            |this, _, event: &ConnectEvent, window, cx| {
                let ConnectEvent::Paired(choice, _) = event;
                // The same server keeps the name the user knows it by.
                let choice = match (backend::saved(), choice) {
                    (
                        Backend::Server {
                            url: saved, name, ..
                        },
                        Backend::Server { url, device, .. },
                    ) if saved == *url => Backend::Server {
                        name,
                        url: url.clone(),
                        device: device.clone(),
                    },
                    (_, choice) => choice.clone(),
                };
                if let Err(err) = backend::save(&choice) {
                    eprintln!("gentle-shell-gpui: could not save where agents run: {err}");
                }
                server::forget_info(&choice);
                this.start_remote(window, cx);
            },
        );
        (connect, paired)
    }

    /// Install the terminal host at `binary` (a file on this computer) on
    /// the server, then open the terminal.
    fn install_host(&mut self, binary: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        let Some(blocked) = &mut self.blocked else {
            return;
        };
        blocked.installing = true;
        blocked.install_error = None;
        let cwd = self.cwd.clone();
        blocked.install = cx.spawn_in(window, async move |this, cx| {
            let installed = cx
                .background_executor()
                .spawn(async move { install(&cwd, &binary) })
                .await;
            let _ = this.update_in(cx, |this, window, cx| match installed {
                Ok(()) => this.start_remote(window, cx),
                Err(refused) => {
                    let admin = matches!(refused, Refused::NeedsAdmin);
                    let connect = (admin
                        && this.blocked.as_ref().is_some_and(|b| b.connect.is_none()))
                    .then(|| this.pair_again(window, cx));
                    if let Some(blocked) = &mut this.blocked {
                        blocked.installing = false;
                        blocked.install_error = Some(refused.to_string());
                        if connect.is_some() {
                            blocked.connect = connect;
                        }
                    }
                    cx.notify();
                }
            });
        });
        cx.notify();
    }

    /// Ask for a terminal host binary on this computer, then install it.
    fn choose_host(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let picked = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some("Install".into()),
        });
        cx.spawn_in(window, async move |this, cx| {
            let Ok(Ok(Some(paths))) = picked.await else {
                return;
            };
            let Some(path) = paths.into_iter().next() else {
                return;
            };
            let _ = this.update_in(cx, |this, window, cx| this.install_host(path, window, cx));
        })
        .detach();
    }

    /// The pane while its server terminal can't show: what's wrong and the
    /// way out.
    pub(super) fn render_blocked(
        &self,
        blocked: &Blocked,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let paragraph = |text: String| {
            div()
                .text_size(rems(12.5 / 16.))
                .text_color(ui::muted())
                .child(text)
        };
        let command = |text: &'static str| {
            ui::well_box()
                .px_2()
                .py_1()
                .text_color(ui::fg())
                .child(text)
        };
        let server = &blocked.server;
        let (title, body) = match &blocked.problem {
            Problem::Error(why) => (
                "Couldn't open a terminal on the server".to_string(),
                v_flex().gap_3().child(paragraph(why.clone())).child(
                    h_flex().child(
                        Button::new("terminal-retry")
                            .small()
                            .primary()
                            .label("Try again")
                            .on_click(
                                cx.listener(|this, _, window, cx| this.start_remote(window, cx)),
                            ),
                    ),
                ),
            ),
            Problem::NoHost { host, bundled } => {
                let installing = blocked.installing;
                let here = if cfg!(target_os = "macos") {
                    "Install from this Mac"
                } else {
                    "Install from this computer"
                };
                let buttons = h_flex()
                    .gap_2()
                    .when_some(bundled.clone(), |row, bundled| {
                        row.child(
                            Button::new("terminal-install-bundled")
                                .small()
                                .primary()
                                .when(installing, |b| b.loading(true))
                                .label(here)
                                .disabled(installing)
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    this.install_host(bundled.clone(), window, cx)
                                })),
                        )
                    })
                    .child(
                        Button::new("terminal-install-choose")
                            .small()
                            .ghost()
                            .label("Choose binary…")
                            .disabled(installing)
                            .on_click(
                                cx.listener(|this, _, window, cx| this.choose_host(window, cx)),
                            ),
                    );
                let build = format!(
                    "Or build it on {server} ({} {}) in a gentle-shell-gpui checkout:",
                    host.platform, host.arch
                );
                (
                    format!("Install terminal support on {server}"),
                    v_flex()
                        .gap_3()
                        .child(paragraph(format!(
                            "{server} has no terminal host (gentle-term-host), which keeps \
                             its terminals running while this app is closed."
                        )))
                        .child(buttons)
                        .child(paragraph(build))
                        .child(command("cargo build --release -p gentle-term-host"))
                        .child(paragraph(
                            "Then put target/release/gentle-term-host in the serve/bin folder \
                             of the agent home there (~/.gentle-shell/agent/serve/bin by \
                             default) or anywhere on its PATH, or point GENTLE_TERM_HOST at \
                             it, and try again."
                                .into(),
                        ))
                        .child(
                            h_flex().child(
                                Button::new("terminal-retry")
                                    .small()
                                    .ghost()
                                    .label("Try again")
                                    .disabled(installing)
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.start_remote(window, cx)
                                    })),
                            ),
                        ),
                )
            }
            Problem::NoScope => (
                "Pair again to allow terminals".to_string(),
                v_flex().gap_3().child(paragraph(format!(
                    "This device was paired with {server} without the terminal scope. Run \
                     `gentle-shell serve pair` there and enter the code it prints."
                ))),
            ),
        };
        div()
            .id("terminal-blocked")
            .track_focus(&self.focus_handle)
            .size_full()
            .overflow_y_scroll()
            .flex()
            .justify_center()
            .p(rems(PADDING / 16.))
            .font_family(cx.theme().font_family.clone())
            .child(
                ui::card()
                    .my_auto()
                    .w_full()
                    .max_w(rems(34.))
                    .p_4()
                    .gap_3()
                    .child(
                        div()
                            .text_size(rems(14. / 16.))
                            .text_color(ui::fg())
                            .child(title),
                    )
                    .child(body)
                    .children(blocked.install_error.clone().map(|error| {
                        div()
                            .text_size(rems(12.5 / 16.))
                            .text_color(ui::danger())
                            .child(error)
                    }))
                    .children(blocked.connect.as_ref().map(|(connect, _)| connect.clone())),
            )
    }
}

/// Why installing a terminal host didn't.
enum Refused {
    /// The device lacks the `admin` scope.
    NeedsAdmin,
    /// Anything else, in words.
    Other(String),
}

impl std::fmt::Display for Refused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Refused::NeedsAdmin => f.write_str(NEEDS_ADMIN),
            Refused::Other(why) => f.write_str(why),
        }
    }
}

/// Upload `binary` (a file on this computer) as the terminal host of the
/// server `cwd` is on, and ask the server what it offers again. Blocking.
fn install(cwd: &Path, binary: &Path) -> Result<(), Refused> {
    let choice = backend::for_tab(cwd);
    let client = server::client(&choice).map_err(|e| Refused::Other(e.to_string()))?;
    let bytes = std::fs::read(binary)
        .map_err(|e| Refused::Other(format!("Couldn't read {}: {e}", binary.display())))?;
    match client.install_term_host(&bytes) {
        Ok(_) => {
            server::forget_info(&choice);
            Ok(())
        }
        Err(error) if error.status == Some(403) => Err(Refused::NeedsAdmin),
        Err(error) => Err(Refused::Other(error.message)),
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::{Resume, backoff, same_build};

    /// Output is drawn once: a chunk the resume point already passed is
    /// skipped, one that overlaps it is drawn from there, and an attachment
    /// moves the point to where the server continues.
    #[test]
    fn output_resumes_after_the_last_byte_drawn() {
        let mut resume = Resume::default();
        assert_eq!(resume.from, None);
        resume.attached(10);
        assert_eq!(resume.fresh(13, b"abc"), b"abc");
        assert_eq!(resume.from, Some(13));
        // Sent again after a reconnect: already drawn.
        assert_eq!(resume.fresh(13, b"abc"), b"");
        assert_eq!(resume.fresh(12, b"ab"), b"");
        assert_eq!(resume.from, Some(13));
        // Overlapping: only the new tail.
        assert_eq!(resume.fresh(16, b"bcdef"), b"def");
        assert_eq!(resume.from, Some(16));
        // A snapshot's attachment starts over at the terminal's end.
        resume.attached(100);
        assert_eq!(resume.fresh(104, b"wxyz"), b"wxyz");
        assert_eq!(resume.from, Some(104));
        // Output before any attachment is all new.
        let mut fresh = Resume::default();
        assert_eq!(fresh.fresh(5, b"hello"), b"hello");
        assert_eq!(fresh.from, Some(5));
    }

    #[test]
    fn reconnects_back_off_up_to_a_limit() {
        assert_eq!(backoff(0), Duration::from_millis(250));
        assert_eq!(backoff(1), Duration::from_millis(500));
        assert_eq!(backoff(3), Duration::from_secs(2));
        assert_eq!(backoff(5), Duration::from_secs(5));
        assert_eq!(backoff(u32::MAX), Duration::from_secs(5));
    }

    /// This app's own terminal host goes only to a server of the same
    /// platform and processor, in Node's names for them.
    #[test]
    fn the_bundled_host_fits_only_the_same_build() {
        assert!(same_build("darwin", "arm64", "macos", "aarch64"));
        assert!(same_build("linux", "x64", "linux", "x86_64"));
        assert!(same_build("win32", "x64", "windows", "x86_64"));
        assert!(!same_build("linux", "arm64", "macos", "aarch64"));
        assert!(!same_build("darwin", "x64", "macos", "aarch64"));
        assert!(!same_build("", "", "macos", "aarch64"));
    }
}
