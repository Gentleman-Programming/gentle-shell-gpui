//! The terms, the connections and the protocol between them: JSON Lines, one
//! reply per request, events for attached terms (see the crate docs).

use std::collections::{HashMap, VecDeque};
use std::io::{BufRead, BufReader, BufWriter, Read, Write};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use serde_json::{Value, json};

use crate::term::{Spec, TermHandle, event_line, lock};
use crate::transport::{Endpoint, Stream};

pub const PROTOCOL: u32 = 1;

/// Longest request line: a `write` of a large paste, base64.
const MAX_LINE: u64 = 32 << 20;

/// Longest line before a TCP connection has authenticated, and how long it
/// has to send it.
const AUTH_LINE: u64 = 4 << 10;
const AUTH_TIMEOUT: Duration = Duration::from_secs(10);

const MAX_COLS: u64 = 1000;
const MAX_ROWS: u64 = 500;

pub struct ProtoError {
    code: &'static str,
    message: String,
}

impl ProtoError {
    fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }

    fn bad_request(message: impl Into<String>) -> Self {
        Self::new("bad_request", message)
    }

    pub fn exited() -> Self {
        Self::new("exited", "the terminal has exited")
    }
}

/// A connection's outgoing lines, written by its own thread so a slow
/// client never holds up a term.
#[derive(Default)]
pub struct Outbox {
    inner: Mutex<OutboxInner>,
    changed: Condvar,
}

#[derive(Default)]
struct OutboxInner {
    lines: VecDeque<Arc<[u8]>>,
    bytes: usize,
    writing: bool,
    closed: bool,
}

impl Outbox {
    pub fn send(&self, line: Arc<[u8]>) {
        let mut inner = lock(&self.inner);
        if inner.closed {
            return;
        }
        inner.bytes += line.len();
        inner.lines.push_back(line);
        self.changed.notify_all();
    }

    /// Queues `line` unless that would put more than `cap` bytes in the
    /// queue. A closed outbox takes anything (and drops it).
    pub fn try_send(&self, line: Arc<[u8]>, cap: usize) -> bool {
        let mut inner = lock(&self.inner);
        if inner.closed {
            return true;
        }
        if inner.bytes + line.len() > cap {
            return false;
        }
        inner.bytes += line.len();
        inner.lines.push_back(line);
        self.changed.notify_all();
        true
    }

    fn close(&self) {
        let mut inner = lock(&self.inner);
        inner.closed = true;
        inner.lines.clear();
        inner.bytes = 0;
        self.changed.notify_all();
    }

    /// Everything queued, waiting for some; `None` once closed.
    fn take(&self) -> Option<Vec<Arc<[u8]>>> {
        let mut inner = lock(&self.inner);
        inner.writing = false;
        self.changed.notify_all();
        while inner.lines.is_empty() && !inner.closed {
            inner = self
                .changed
                .wait(inner)
                .unwrap_or_else(|poisoned| poisoned.into_inner());
        }
        if inner.closed {
            return None;
        }
        inner.bytes = 0;
        inner.writing = true;
        Some(inner.lines.drain(..).collect())
    }

    /// Waits (up to `timeout`) until everything queued has been written.
    fn drain(&self, timeout: Duration) {
        let deadline = Instant::now() + timeout;
        let mut inner = lock(&self.inner);
        while (inner.writing || !inner.lines.is_empty()) && !inner.closed {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return;
            }
            inner = self
                .changed
                .wait_timeout(inner, left)
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .0;
        }
    }
}

pub struct Host {
    terms: Mutex<HashMap<String, Arc<TermHandle>>>,
    connections: AtomicUsize,
    next_connection: AtomicU64,
    endpoint: Endpoint,
}

impl Host {
    pub fn new(endpoint: Endpoint) -> Self {
        Self {
            terms: Mutex::default(),
            connections: AtomicUsize::new(0),
            next_connection: AtomicU64::new(1),
            endpoint,
        }
    }

    /// No terms and no connections.
    pub fn idle(&self) -> bool {
        self.connections.load(Ordering::SeqCst) == 0 && lock(&self.terms).is_empty()
    }

    /// Removes the socket or token file (if it's still ours) and exits.
    pub fn exit(&self) -> ! {
        self.endpoint.remove();
        crate::log("exiting");
        std::process::exit(0)
    }

    /// Serves one connection until it closes. Over TCP, its first line must
    /// authenticate.
    pub fn serve<S: Stream>(self: &Arc<Self>, stream: S) {
        let (Ok(read_half), Ok(write_half)) = (stream.try_clone(), stream.try_clone()) else {
            return;
        };
        self.connections.fetch_add(1, Ordering::SeqCst);
        let conn = self.next_connection.fetch_add(1, Ordering::SeqCst);
        let out = Arc::new(Outbox::default());
        let writer_out = out.clone();
        let writer = std::thread::Builder::new()
            .name(format!("conn {conn} writer"))
            .spawn(move || {
                let mut writer = BufWriter::new(write_half);
                while let Some(lines) = writer_out.take() {
                    let sent = lines
                        .iter()
                        .try_for_each(|line| writer.write_all(line))
                        .and_then(|()| writer.flush());
                    if sent.is_err() {
                        writer_out.close();
                        writer.get_ref().shutdown();
                    }
                }
            });

        let mut token = self.endpoint.token();
        if token.is_some() {
            stream.set_read_timeout(Some(AUTH_TIMEOUT));
        }
        let refuse = |id: Value, code: &str, message: &str| {
            out.send(event_line(&json!({ "id": id, "ok": false, "error": {
                "code": code, "message": message } })));
            // Said before hanging up.
            out.drain(Duration::from_secs(1));
        };
        let mut attached: HashMap<String, Arc<TermHandle>> = HashMap::new();
        let mut reader = BufReader::new(read_half);
        let mut line = Vec::new();
        loop {
            line.clear();
            let max = if token.is_some() { AUTH_LINE } else { MAX_LINE };
            match reader.by_ref().take(max).read_until(b'\n', &mut line) {
                Ok(0) | Err(_) => break,
                Ok(_) if !line.ends_with(b"\n") && line.len() as u64 >= max => {
                    refuse(Value::Null, "bad_request", "request line too long");
                    break;
                }
                Ok(_) => {}
            }
            if line.trim_ascii().is_empty() {
                continue;
            }
            let request = serde_json::from_slice(&line).ok().filter(Value::is_object);
            if let Some(expected) = token {
                let request = request.unwrap_or_default();
                let given = request["token"].as_str().unwrap_or_default();
                let id = request.get("id").cloned().unwrap_or(Value::Null);
                if request["op"] != "auth"
                    || !crate::sys::same_secret(given.as_bytes(), expected.as_bytes())
                {
                    refuse(
                        id,
                        "unauthorized",
                        "authenticate first: {\"op\": \"auth\", \"token\"}",
                    );
                    break;
                }
                token = None;
                stream.set_read_timeout(None);
                out.send(event_line(&json!({ "id": id, "ok": true })));
                continue;
            }
            let Some(request) = request else {
                let error = json!({ "id": null, "ok": false, "error": {
                    "code": "bad_request", "message": "not a JSON object" } });
                out.send(event_line(&error));
                continue;
            };
            let id = request.get("id").cloned().unwrap_or(Value::Null);
            let shutdown = request["op"] == "shutdown";
            let reply = self.handle(conn, &out, &mut attached, &id, &request);
            match reply {
                Ok(None) => {}
                Ok(Some(mut fields)) => {
                    fields["id"] = id;
                    fields["ok"] = true.into();
                    out.send(event_line(&fields));
                }
                Err(error) => out.send(event_line(&json!({
                    "id": id,
                    "ok": false,
                    "error": { "code": error.code, "message": error.message },
                }))),
            }
            if shutdown {
                self.shutdown(&out);
            }
        }

        for term in attached.values() {
            term.detach(conn);
        }
        out.close();
        stream.shutdown();
        if let Ok(writer) = writer {
            let _ = writer.join();
        }
        self.connections.fetch_sub(1, Ordering::SeqCst);
    }

    fn term(&self, request: &Value) -> Result<Arc<TermHandle>, ProtoError> {
        let id = request["term"]
            .as_str()
            .ok_or_else(|| ProtoError::bad_request("`term` is required"))?;
        lock(&self.terms)
            .get(id)
            .cloned()
            .ok_or_else(|| ProtoError::new("not_found", format!("no terminal {id}")))
    }

    fn handle(
        &self,
        conn: u64,
        out: &Arc<Outbox>,
        attached: &mut HashMap<String, Arc<TermHandle>>,
        id: &Value,
        request: &Value,
    ) -> Result<Option<Value>, ProtoError> {
        let op = request["op"].as_str().unwrap_or_default();
        let reply = match op {
            "hello" => json!({
                "protocol": PROTOCOL,
                "version": env!("CARGO_PKG_VERSION"),
                "pid": std::process::id(),
            }),
            "open" => self.open(request)?,
            "list" => {
                let mut terms: Vec<_> = lock(&self.terms).values().cloned().collect();
                terms.sort_by(|a, b| (a.created_at, &a.id).cmp(&(b.created_at, &b.id)));
                json!({ "terms": terms.iter().map(|t| t.info()).collect::<Vec<_>>() })
            }
            "write" => {
                let term = self.term(request)?;
                let data = request["data"]
                    .as_str()
                    .and_then(|data| BASE64.decode(data).ok())
                    .ok_or_else(|| ProtoError::bad_request("`data` must be base64"))?;
                term.write(data)?;
                json!({})
            }
            "resize" => {
                let term = self.term(request)?;
                let (cols, rows) = size(request)?;
                term.resize(cols, rows)?;
                json!({})
            }
            "close" => {
                let term = self.term(request)?;
                lock(&self.terms).remove(&term.id);
                term.close();
                json!({})
            }
            "forget" => {
                let term = self.term(request)?;
                if term.alive() {
                    return Err(ProtoError::new("alive", "the terminal is still running"));
                }
                lock(&self.terms).remove(&term.id);
                json!({})
            }
            "attach" => {
                let term = self.term(request)?;
                let from =
                    match &request["from"] {
                        Value::Null => None,
                        from => Some(from.as_u64().ok_or_else(|| {
                            ProtoError::bad_request("`from` must be a byte offset")
                        })?),
                    };
                term.attach(conn, out, id, from);
                attached.insert(term.id.clone(), term);
                return Ok(None);
            }
            "detach" => {
                let id = request["term"]
                    .as_str()
                    .ok_or_else(|| ProtoError::bad_request("`term` is required"))?;
                // A closed term is no longer listed but may still be attached.
                let term = attached.remove(id).or_else(|| self.term(request).ok());
                match term {
                    Some(term) => term.detach(conn),
                    None => {
                        return Err(ProtoError::new("not_found", format!("no terminal {id}")));
                    }
                }
                json!({})
            }
            "shutdown" => json!({}),
            _ => return Err(ProtoError::bad_request(format!("unknown op `{op}`"))),
        };
        Ok(Some(reply))
    }

    fn open(&self, request: &Value) -> Result<Value, ProtoError> {
        let cwd = request["cwd"]
            .as_str()
            .filter(|cwd| std::path::Path::new(cwd).is_dir())
            .ok_or_else(|| ProtoError::bad_request("`cwd` must be a folder"))?
            .to_string();
        let (cols, rows) = size(request)?;
        let argv = match &request["argv"] {
            Value::Null => login_shell(),
            Value::Array(items) if !items.is_empty() => items
                .iter()
                .map(|item| item.as_str().map(str::to_string))
                .collect::<Option<Vec<_>>>()
                .ok_or_else(|| ProtoError::bad_request("`argv` must be strings"))?,
            _ => return Err(ProtoError::bad_request("`argv` must be a non-empty array")),
        };
        let env = match &request["env"] {
            Value::Null => Vec::new(),
            Value::Object(vars) => vars
                .iter()
                .map(|(key, value)| Some((key.clone(), value.as_str()?.to_string())))
                .collect::<Option<Vec<_>>>()
                .ok_or_else(|| ProtoError::bad_request("`env` values must be strings"))?,
            _ => return Err(ProtoError::bad_request("`env` must be an object")),
        };
        let id = format!("t_{}", random_hex());
        let created_at = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_millis() as u64);
        let spec = Spec {
            cwd,
            argv,
            env,
            cols,
            rows,
        };
        let term = TermHandle::open(id.clone(), spec, created_at)
            .map_err(|err| ProtoError::new("spawn_failed", err.to_string()))?;
        let pid = term.pid;
        lock(&self.terms).insert(id.clone(), term);
        Ok(json!({ "term": id, "pid": pid }))
    }

    /// Replies, hangs up every term and exits.
    fn shutdown(&self, out: &Outbox) -> ! {
        for term in lock(&self.terms).values() {
            term.hang_up();
        }
        out.drain(Duration::from_secs(1));
        self.exit()
    }
}

fn size(request: &Value) -> Result<(u16, u16), ProtoError> {
    let get = |key: &str, max: u64| {
        request[key]
            .as_u64()
            .filter(|n| (1..=max).contains(n))
            .map(|n| n as u16)
            .ok_or_else(|| ProtoError::bad_request(format!("`{key}` must be 1 to {max}")))
    };
    Ok((get("cols", MAX_COLS)?, get("rows", MAX_ROWS)?))
}

/// The user's login shell, as a login shell.
#[cfg(unix)]
fn login_shell() -> Vec<String> {
    let shell = std::env::var("SHELL")
        .ok()
        .filter(|s| !s.is_empty() && std::path::Path::new(s).exists())
        .or_else(passwd_shell)
        .unwrap_or_else(|| "/bin/sh".to_string());
    vec![shell, "-l".to_string()]
}

/// PowerShell: the newer `pwsh.exe` when it's on `PATH`, else the one
/// Windows ships.
#[cfg(windows)]
fn login_shell() -> Vec<String> {
    let pwsh = std::env::var_os("PATH")
        .is_some_and(|path| std::env::split_paths(&path).any(|dir| dir.join("pwsh.exe").is_file()));
    let shell = if pwsh { "pwsh.exe" } else { "powershell.exe" };
    vec![shell.to_string(), "-NoLogo".to_string()]
}

#[cfg(unix)]
fn passwd_shell() -> Option<String> {
    let mut buf = vec![0 as libc::c_char; 4096];
    // SAFETY: getpwuid_r writes `pwd` and strings into `buf`, both alive
    // while `shell` is read.
    unsafe {
        let mut pwd: libc::passwd = std::mem::zeroed();
        let mut result = std::ptr::null_mut();
        let found = libc::getpwuid_r(
            libc::getuid(),
            &mut pwd,
            buf.as_mut_ptr(),
            buf.len(),
            &mut result,
        );
        if found != 0 || result.is_null() || pwd.pw_shell.is_null() {
            return None;
        }
        let shell = std::ffi::CStr::from_ptr(pwd.pw_shell).to_str().ok()?;
        (!shell.is_empty()).then(|| shell.to_string())
    }
}

fn random_hex() -> String {
    let mut bytes = [0u8; 8];
    if crate::sys::random_bytes(&mut bytes).is_err() {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos());
        bytes = (nanos as u64 ^ u64::from(std::process::id()) << 32).to_le_bytes();
    }
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}
