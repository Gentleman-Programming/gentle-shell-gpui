//! `gentle-term-host`: terminals that outlive their clients, for a Gentle
//! Shell server (`gentle-shell serve`). The server starts it on first use and
//! talks to it over a Unix socket (loopback TCP on Windows); a client that
//! comes back attaches again and gets the screen as it is.
//!
//! ```text
//! gentle-term-host --socket <path> [--state-dir <dir>] [--idle-timeout <secs>]
//! gentle-term-host --tcp --state-dir <dir> [--idle-timeout <secs>]
//! ```
//!
//! `--socket` (Unix only) listens on an owner-only socket (0600, its folder
//! 0700). `--tcp` (any system; the only transport on Windows) listens on
//! 127.0.0.1 at a port the system picks and writes `{"port", "token",
//! "pid"}` to `<state-dir>/term-host.json`, readable only by its owner (0600
//! in a 0700 folder; on Windows a protected DACL granting only the owner).
//! Over TCP a connection's first line must be `{"id", "op": "auth",
//! "token"}`, answered `{"id", "ok": true}`; anything else is answered with
//! the error `unauthorized` and the connection closes (as it does after 10 s
//! without that line).
//!
//! Each connection speaks JSON Lines both ways (binary data base64). A
//! request `{"id", "op", ...}` gets one reply, `{"id", "ok": true, ...}` or
//! `{"id", "ok": false, "error": {"code", "message"}}`; codes are
//! `bad_request`, `not_found` (no such term), `exited` (writing to or
//! resizing a term that has exited), `alive` (forgetting one that hasn't)
//! and `spawn_failed`.
//!
//! - `hello` → `{protocol: 1, version, pid}`
//! - `open {cwd, argv?, env?, cols, rows}` → `{term, pid}`; `argv` defaults
//!   to the login shell with `-l` (PowerShell on Windows: `pwsh.exe` when
//!   it's on `PATH`, else `powershell.exe`, with `-NoLogo`), `env` is added
//!   to the host's (with `TERM=xterm-256color`).
//! - `list` → `{terms: [{term, pid, cwd, argv, cols, rows, title, alive,
//!   exitCode?, createdAt (ms since the epoch), offset}]}`
//! - `write {term, data}`, `resize {term, cols, rows}`
//! - `close {term}`: unlisted at once; SIGHUP, SIGKILL 3 s later. On Windows
//!   the program is ended at once (TerminateProcess), and closing its
//!   pseudoconsole ends what else ran in it.
//! - `forget {term}`: unlists a term that has exited.
//! - `attach {term, from?}` → `{offset, snapshot?}`, then events on this
//!   connection: `{"ev": "output", term, offset, data}` (`offset` is where
//!   the chunk ends in the term's output), `exit {code}`, `title {title}`,
//!   `resized {cols, rows}` and `overflow` (more than 8 MiB queued: detached,
//!   attach again). With a `from` the replay ring still has, output resumes
//!   there (the reply's `offset` is `from`); otherwise `snapshot` draws the
//!   screen as of `offset` in a fresh emulator of the term's size.
//!   Attaching to a term that has exited replies, then sends its `exit`.
//! - `detach {term}`, `shutdown` (replies, hangs up every term and exits).
//!
//! Terms live on when connections drop; the host exits after
//! `--idle-timeout` (default 600 s) with no terms and no connections. While
//! no client is attached to a term, the host answers the program's terminal
//! queries itself (an attached client's emulator does otherwise).

mod host;
mod snapshot;
mod sys;
mod term;
mod transport;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::host::Host;

fn main() {
    if let Err(err) = run() {
        log(&format!("error: {err}"));
        eprintln!("gentle-term-host: {err}");
        std::process::exit(1);
    }
}

/// Where log lines go: `term-host.log` in the state folder, else stderr.
static LOG: std::sync::OnceLock<std::sync::Mutex<std::fs::File>> = std::sync::OnceLock::new();

pub fn log(message: &str) {
    use std::io::Write as _;
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let line = format!("{secs} [{}] {message}\n", std::process::id());
    match LOG.get() {
        Some(file) => {
            let _ = term::lock(file).write_all(line.as_bytes());
        }
        None => {
            let _ = std::io::stderr().write_all(line.as_bytes());
        }
    }
}

enum Listen {
    #[cfg(unix)]
    Socket(PathBuf),
    Tcp,
}

struct Args {
    listen: Listen,
    state_dir: Option<PathBuf>,
    idle_timeout: Duration,
}

fn args() -> Result<Args, String> {
    let mut listen = None;
    let mut state_dir = None;
    let mut idle_timeout = Duration::from_secs(600);
    let mut args = std::env::args_os().skip(1);
    while let Some(arg) = args.next() {
        let mut value = || {
            args.next()
                .ok_or(format!("{} needs a value", arg.display()))
        };
        match arg.to_str() {
            #[cfg(unix)]
            Some("--socket") => listen = Some(Listen::Socket(PathBuf::from(value()?))),
            #[cfg(not(unix))]
            Some("--socket") => return Err("no Unix sockets here: use --tcp".into()),
            Some("--tcp") => listen = Some(Listen::Tcp),
            Some("--state-dir") => state_dir = Some(PathBuf::from(value()?)),
            Some("--idle-timeout") => {
                let secs = value()?
                    .to_str()
                    .and_then(|s| s.parse().ok())
                    .ok_or("--idle-timeout takes seconds")?;
                idle_timeout = Duration::from_secs(secs);
            }
            Some("--version") => {
                println!("gentle-term-host {}", env!("CARGO_PKG_VERSION"));
                std::process::exit(0);
            }
            _ => return Err(format!("unknown argument {}", arg.display())),
        }
    }
    let listen = listen.ok_or(if cfg!(unix) {
        "--socket <path> or --tcp is required"
    } else {
        "--tcp is required"
    })?;
    if matches!(listen, Listen::Tcp) && state_dir.is_none() {
        return Err("--tcp needs --state-dir <dir>, where it writes its port and token".into());
    }
    Ok(Args {
        listen,
        state_dir,
        idle_timeout,
    })
}

fn run() -> Result<(), String> {
    let args = args()?;
    if let Some(dir) = &args.state_dir {
        sys::private_dir(dir)?;
        let log = sys::append_file(&dir.join("term-host.log"))
            .map_err(|err| format!("{}: {err}", dir.display()))?;
        let _ = LOG.set(std::sync::Mutex::new(log));
    }
    // SAFETY: plain syscalls. Our own session, so the server's terminal
    // closing doesn't hang the host up (the PTYs' children get SIGHUP back
    // to its default).
    #[cfg(unix)]
    unsafe {
        libc::setsid();
        libc::signal(libc::SIGHUP, libc::SIG_IGN);
    }

    let (listener, endpoint) = match &args.listen {
        #[cfg(unix)]
        Listen::Socket(path) => transport::bind_socket(path)?,
        Listen::Tcp => transport::bind_tcp(args.state_dir.as_deref().expect("checked in args"))?,
    };
    log(&format!("listening on {}", endpoint.describe()));
    let host = Arc::new(Host::new(endpoint));

    let idle = host.clone();
    let idle_timeout = args.idle_timeout;
    std::thread::Builder::new()
        .name("idle".into())
        .spawn(move || {
            let tick =
                (idle_timeout / 10).clamp(Duration::from_millis(100), Duration::from_secs(5));
            let mut since = Instant::now();
            loop {
                std::thread::sleep(tick);
                if !idle.idle() {
                    since = Instant::now();
                } else if since.elapsed() >= idle_timeout {
                    idle.exit();
                }
            }
        })
        .map_err(|err| err.to_string())?;

    listener.run(&host);
    Ok(())
}
