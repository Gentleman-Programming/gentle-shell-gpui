//! Where the host listens: an owner-only Unix socket, or loopback TCP with a
//! token in an owner-only file (the only choice on Windows).

use std::io::{BufRead as _, BufReader, ErrorKind, Read, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use serde_json::{Value, json};

use crate::host::Host;

/// Where a `--tcp` host writes `{port, token, pid}`, in its state folder.
pub const TCP_FILE: &str = "term-host.json";

/// A connection, whatever the transport.
pub trait Stream: Read + Write + Send + Sized + 'static {
    fn try_clone(&self) -> std::io::Result<Self>;
    fn shutdown(&self);
    fn set_read_timeout(&self, timeout: Option<Duration>);
}

impl Stream for TcpStream {
    fn try_clone(&self) -> std::io::Result<Self> {
        TcpStream::try_clone(self)
    }
    fn shutdown(&self) {
        let _ = TcpStream::shutdown(self, std::net::Shutdown::Both);
    }
    fn set_read_timeout(&self, timeout: Option<Duration>) {
        let _ = TcpStream::set_read_timeout(self, timeout);
    }
}

#[cfg(unix)]
impl Stream for std::os::unix::net::UnixStream {
    fn try_clone(&self) -> std::io::Result<Self> {
        std::os::unix::net::UnixStream::try_clone(self)
    }
    fn shutdown(&self) {
        let _ = std::os::unix::net::UnixStream::shutdown(self, std::net::Shutdown::Both);
    }
    fn set_read_timeout(&self, timeout: Option<Duration>) {
        let _ = std::os::unix::net::UnixStream::set_read_timeout(self, timeout);
    }
}

/// What the host put on disk to be found, removed when it exits.
pub enum Endpoint {
    #[cfg(unix)]
    Socket {
        path: PathBuf,
        /// The socket's inode, so exiting removes only our own.
        ino: u64,
    },
    Tcp {
        port: u16,
        file: PathBuf,
        token: String,
    },
}

impl Endpoint {
    /// The token a connection must authenticate with first, if any.
    pub fn token(&self) -> Option<&str> {
        match self {
            #[cfg(unix)]
            Self::Socket { .. } => None,
            Self::Tcp { token, .. } => Some(token),
        }
    }

    pub fn describe(&self) -> String {
        match self {
            #[cfg(unix)]
            Self::Socket { path, .. } => path.display().to_string(),
            Self::Tcp { port, file, .. } => {
                format!("127.0.0.1:{port} (token in {})", file.display())
            }
        }
    }

    /// Removes the socket or the token file, if it's still ours.
    pub fn remove(&self) {
        match self {
            #[cfg(unix)]
            Self::Socket { path, ino } => {
                use std::os::unix::fs::MetadataExt as _;
                if std::fs::symlink_metadata(path).is_ok_and(|m| m.ino() == *ino) {
                    let _ = std::fs::remove_file(path);
                }
            }
            Self::Tcp { file, token, .. } => {
                let ours = read_tcp_file(file).is_some_and(|(_, t)| t == *token);
                if ours {
                    let _ = std::fs::remove_file(file);
                }
            }
        }
    }
}

pub enum Listener {
    #[cfg(unix)]
    Unix(std::os::unix::net::UnixListener),
    Tcp(TcpListener),
}

impl Listener {
    /// Accepts connections forever, each served on its own thread.
    pub fn run(self, host: &Arc<Host>) {
        match self {
            #[cfg(unix)]
            Self::Unix(listener) => accept(listener.incoming(), host),
            Self::Tcp(listener) => accept(listener.incoming(), host),
        }
    }
}

fn accept<S: Stream>(incoming: impl Iterator<Item = std::io::Result<S>>, host: &Arc<Host>) {
    for stream in incoming {
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
}

/// Binds the socket owner-only, replacing a stale one; refuses if another
/// host answers on it.
#[cfg(unix)]
pub fn bind_socket(path: &Path) -> Result<(Listener, Endpoint), String> {
    use std::fs::Permissions;
    use std::os::unix::fs::{FileTypeExt as _, MetadataExt as _, PermissionsExt as _};
    use std::os::unix::net::{UnixListener, UnixStream};

    if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
        crate::sys::private_dir(dir)?;
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
    // SAFETY: umask(2) can't fail; the old mask is back right after bind,
    // before any terminal starts.
    let old = unsafe { libc::umask(0o077) };
    let listener = UnixListener::bind(path);
    unsafe { libc::umask(old) };
    let listener = listener.map_err(|err| format!("{}: {err}", path.display()))?;
    std::fs::set_permissions(path, Permissions::from_mode(0o600))
        .map_err(|err| format!("{}: {err}", path.display()))?;
    let ino = std::fs::symlink_metadata(path)
        .map(|m| m.ino())
        .map_err(|err| err.to_string())?;
    let endpoint = Endpoint::Socket {
        path: path.to_path_buf(),
        ino,
    };
    Ok((Listener::Unix(listener), endpoint))
}

/// Listens on 127.0.0.1 at a port the system picks, and writes it with a
/// fresh token to `<state_dir>/term-host.json`, owner-only; refuses if the
/// host already there answers.
pub fn bind_tcp(state_dir: &Path) -> Result<(Listener, Endpoint), String> {
    let file = state_dir.join(TCP_FILE);
    if let Some((port, token)) = read_tcp_file(&file)
        && answers(port, &token)
    {
        return Err(format!("another host is running on 127.0.0.1:{port}"));
    }
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).map_err(|err| err.to_string())?;
    let port = listener.local_addr().map_err(|err| err.to_string())?.port();
    let mut bytes = [0u8; 32];
    crate::sys::random_bytes(&mut bytes).map_err(|err| format!("no random token: {err}"))?;
    let token: String = bytes.iter().map(|b| format!("{b:02x}")).collect();

    // Written whole, then moved into place: a reader never sees half of it.
    let temp = state_dir.join(format!("{TCP_FILE}.{}.tmp", std::process::id()));
    let _ = std::fs::remove_file(&temp);
    let contents = json!({ "port": port, "token": token, "pid": std::process::id() });
    let written = crate::sys::private_file(&temp).and_then(|mut out| {
        out.write_all(format!("{contents}\n").as_bytes())?;
        out.sync_all()
    });
    let moved = written.and_then(|()| std::fs::rename(&temp, &file));
    if let Err(err) = moved {
        let _ = std::fs::remove_file(&temp);
        return Err(format!("{}: {err}", file.display()));
    }
    let endpoint = Endpoint::Tcp { port, file, token };
    Ok((Listener::Tcp(listener), endpoint))
}

/// The port and token in a token file.
fn read_tcp_file(file: &Path) -> Option<(u16, String)> {
    let value: Value = serde_json::from_slice(&std::fs::read(file).ok()?).ok()?;
    let port = u16::try_from(value["port"].as_u64()?).ok()?;
    Some((port, value["token"].as_str()?.to_string()))
}

/// Whether a host accepts `token` on `port`.
fn answers(port: u16, token: &str) -> bool {
    let address = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
    let Ok(mut stream) = TcpStream::connect_timeout(&address, Duration::from_secs(1)) else {
        return false;
    };
    let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
    let auth = json!({ "id": 1, "op": "auth", "token": token });
    if stream.write_all(format!("{auth}\n").as_bytes()).is_err() {
        return false;
    }
    let mut line = String::new();
    let read = BufReader::new(stream.take(4096)).read_line(&mut line);
    read.is_ok() && serde_json::from_str::<Value>(&line).is_ok_and(|reply| reply["ok"] == true)
}
