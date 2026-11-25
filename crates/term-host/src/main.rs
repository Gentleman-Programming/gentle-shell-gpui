//! `gentle-term-host`: terminals that outlive their clients, for a Gentle
//! Shell server (`gentle-shell serve`). The server starts it on first use and
//! talks to it over a Unix socket; a client that comes back attaches again
//! and gets the screen as it is.
//!
//! ```text
//! gentle-term-host --socket <path> [--state-dir <dir>] [--idle-timeout <secs>]
//! ```
//!
//! The socket is owner-only (0600, its folder 0700). Each connection speaks
//! JSON Lines both ways (binary data base64). A request `{"id", "op", ...}`
//! gets one reply, `{"id", "ok": true, ...}` or `{"id", "ok": false, "error":
//! {"code", "message"}}`; codes are `bad_request`, `not_found` (no such
//! term), `exited` (writing to or resizing a term that has exited), `alive`
//! (forgetting one that hasn't) and `spawn_failed`.
//!
//! - `hello` → `{protocol: 1, version, pid}`
//! - `open {cwd, argv?, env?, cols, rows}` → `{term, pid}`; `argv` defaults
//!   to the login shell with `-l`, `env` is added to the host's (with
//!   `TERM=xterm-256color`).
//! - `list` → `{terms: [{term, pid, cwd, argv, cols, rows, title, alive,
//!   exitCode?, createdAt (ms since the epoch), offset}]}`
//! - `write {term, data}`, `resize {term, cols, rows}`
//! - `close {term}`: unlisted at once; SIGHUP, SIGKILL 3 s later.
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

#[cfg(unix)]
mod host;
#[cfg(unix)]
mod snapshot;
#[cfg(unix)]
mod term;

#[cfg(unix)]
fn main() {
    if let Err(err) = unix::run() {
        log(&format!("error: {err}"));
        eprintln!("gentle-term-host: {err}");
        std::process::exit(1);
    }
}

#[cfg(not(unix))]
fn main() {
    eprintln!("gentle-term-host: only Unix is supported for now");
    std::process::exit(1);
}

/// Where log lines go: `term-host.log` in the state folder, else stderr.
#[cfg(unix)]
static LOG: std::sync::OnceLock<std::sync::Mutex<std::fs::File>> = std::sync::OnceLock::new();

#[cfg(unix)]
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

#[cfg(unix)]
mod unix {
    use std::fs::{DirBuilder, Permissions};
    use std::io::ErrorKind;
    use std::os::unix::fs::{DirBuilderExt as _, FileTypeExt as _, MetadataExt as _};
    use std::os::unix::fs::{OpenOptionsExt as _, PermissionsExt as _};
    use std::os::unix::net::{UnixListener, UnixStream};
    use std::path::{Path, PathBuf};
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    use crate::host::Host;

    struct Args {
        socket: PathBuf,
        state_dir: Option<PathBuf>,
        idle_timeout: Duration,
    }

    fn args() -> Result<Args, String> {
        let mut socket = None;
        let mut state_dir = None;
        let mut idle_timeout = Duration::from_secs(600);
        let mut args = std::env::args_os().skip(1);
        while let Some(arg) = args.next() {
            let mut value = || {
                args.next()
                    .ok_or(format!("{} needs a value", arg.display()))
            };
            match arg.to_str() {
                Some("--socket") => socket = Some(PathBuf::from(value()?)),
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
        Ok(Args {
            socket: socket.ok_or("--socket <path> is required")?,
            state_dir,
            idle_timeout,
        })
    }

    /// Creates `dir` owner-only, or checks it's ours and makes it so.
    fn private_dir(dir: &Path) -> Result<(), String> {
        DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(dir)
            .map_err(|err| format!("{}: {err}", dir.display()))?;
        let meta = std::fs::metadata(dir).map_err(|err| format!("{}: {err}", dir.display()))?;
        // SAFETY: getuid(2) can't fail.
        if meta.uid() != unsafe { libc::getuid() } {
            return Err(format!("{} belongs to another user", dir.display()));
        }
        if meta.mode() & 0o077 != 0 {
            std::fs::set_permissions(dir, Permissions::from_mode(0o700))
                .map_err(|err| format!("{}: {err}", dir.display()))?;
        }
        Ok(())
    }

    /// Binds the socket owner-only, replacing a stale one; refuses if
    /// another host answers on it.
    fn bind(path: &Path) -> Result<UnixListener, String> {
        if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
            private_dir(dir)?;
        }
        if let Ok(meta) = std::fs::symlink_metadata(path) {
            if !meta.file_type().is_socket() {
                return Err(format!("{} exists and isn't a socket", path.display()));
            }
            if UnixStream::connect(path).is_ok() {
                return Err(format!("another host is running on {}", path.display()));
            }
            std::fs::remove_file(path).map_err(|err| format!("{}: {err}", path.display()))?;
        }
        // SAFETY: umask(2) can't fail; the old mask is back right after
        // bind, before any terminal starts.
        let old = unsafe { libc::umask(0o077) };
        let listener = UnixListener::bind(path);
        unsafe { libc::umask(old) };
        let listener = listener.map_err(|err| format!("{}: {err}", path.display()))?;
        std::fs::set_permissions(path, Permissions::from_mode(0o600))
            .map_err(|err| format!("{}: {err}", path.display()))?;
        Ok(listener)
    }

    pub fn run() -> Result<(), String> {
        let args = args()?;
        if let Some(dir) = &args.state_dir {
            private_dir(dir)?;
            let log = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .mode(0o600)
                .open(dir.join("term-host.log"))
                .map_err(|err| format!("{}: {err}", dir.display()))?;
            let _ = crate::LOG.set(std::sync::Mutex::new(log));
        }
        // SAFETY: plain syscalls. Our own session, so the server's terminal
        // closing doesn't hang the host up (the PTYs' children get SIGHUP
        // back to its default).
        unsafe {
            libc::setsid();
            libc::signal(libc::SIGHUP, libc::SIG_IGN);
        }

        let listener = bind(&args.socket)?;
        let ino = std::fs::symlink_metadata(&args.socket)
            .map(|m| m.ino())
            .map_err(|err| err.to_string())?;
        let host = Arc::new(Host::new(args.socket.clone(), ino));
        crate::log(&format!("listening on {}", args.socket.display()));

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

        for stream in listener.incoming() {
            match stream {
                Ok(stream) => {
                    let host = host.clone();
                    let spawned = std::thread::Builder::new()
                        .name("conn".into())
                        .spawn(move || host.serve(stream));
                    if let Err(err) = spawned {
                        crate::log(&format!("no thread for a connection: {err}"));
                    }
                }
                Err(err) if err.kind() == ErrorKind::Interrupted => {}
                Err(err) => {
                    crate::log(&format!("accept failed: {err}"));
                    std::thread::sleep(Duration::from_millis(100));
                }
            }
        }
        Ok(())
    }
}
