//! A Gentle Shell server's session as a backend (`gentle-shell serve`,
//! protocol `gentle-shell.serve/v1`, Gentle Shell's `docs/serve.md`): the
//! same records and commands as a child's stdio, over HTTP and Server-Sent
//! Events, so a tab reads and drives it exactly like a local one.
//!
//! - **Commands** go one at a time to `POST /api/sessions/:id/rpc`; its
//!   answer (the Pi response, under this client's id) is read as if the
//!   child had printed it. One that takes longer than the server waits
//!   (`202 pending`) is answered later in the stream, under the server's id.
//! - **Records** come from `GET /api/sessions/:id/events` with the device's
//!   bearer token. The stream repeats every response under the server's own
//!   id (`serve-rpc-N`); those are skipped unless they settle a pending
//!   command ([`gentle_core::serve::RpcIds`]).
//! - **Reconnects** resume after the last `seq` seen; records the server no
//!   longer keeps become [`Incoming::Resync`]. 45 s without a byte (the
//!   server sends a heartbeat every 20 s) is a dead connection.
//! - **Ending**: closing the tab stops the session there (`DELETE`); the app
//!   quitting leaves it running ([`PiProcess::detach`]) for the next launch
//!   to reattach to ([`PiProcess::serve`] with its id).

use std::io::Read as _;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};

use anyhow::{Result, anyhow};
use gentle_core::serve::{
    self as protocol, ConnectionCode, Probe, RpcIds, Search, SseParser, Step, Stream,
};
use serde_json::{Value, json};

use crate::{Incoming, Io, Pending, PiProcess, Traffic, dispatch, record::record_from};

/// How long a connection may take.
const CONNECT: Duration = Duration::from_secs(10);
/// Silence after which a stream is dead: two heartbeats missed.
const SILENCE: Duration = Duration::from_secs(45);
/// How long an answer may take: the server waits 30 s for Pi's.
const ANSWER: Duration = Duration::from_secs(45);
/// How long the next command waits for the one before it to be answered.
/// Commands reach the server in order; one that runs long (`bash`,
/// `compact`) must not hold up an `abort` behind it.
const OVERTAKE: Duration = Duration::from_secs(2);
/// The largest answer read (`get_messages` of a long conversation with
/// images); the server caps a record at 16 MiB.
const BODY_LIMIT: u64 = 64 * 1024 * 1024;
/// How long one address may take to say who it is, looking for the server
/// a connection code names.
const PROBE: Duration = Duration::from_millis(1500);
/// How long looking for that server may take in all.
const SEARCH: Duration = Duration::from_secs(4);

/// What went wrong talking to a server, in words for the user.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServeError {
    /// The HTTP status, when the server answered.
    pub status: Option<u16>,
    /// The server's error code (`invalid_code`, `rate_limited`, …), when
    /// it sent one.
    pub error: Option<String>,
    pub message: String,
}

impl std::fmt::Display for ServeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for ServeError {}

/// This device, as a pairing made it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Paired {
    /// The device id (`dev_…`).
    pub device: String,
    pub name: String,
    /// The device token (`gss_…`): shown once, a secret.
    pub token: String,
}

/// `GET /api/server`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerInfo {
    /// `srv_…`, new on every server run.
    pub server_id: String,
    /// The server's Gentle Shell version.
    pub version: Option<String>,
}

/// One of the server's sessions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionInfo {
    /// `ses_…`.
    pub id: String,
    pub running: bool,
    pub exit_code: Option<i32>,
}

impl SessionInfo {
    fn parse(value: &Value) -> Option<Self> {
        Some(Self {
            id: value["id"].as_str()?.to_string(),
            running: value["state"] == "running",
            exit_code: value["exitCode"]
                .as_i64()
                .and_then(|c| i32::try_from(c).ok()),
        })
    }
}

/// The server at one address, as one paired device (or none, to pair).
#[derive(Clone)]
pub struct ServeClient {
    /// Requests: answers within [`ANSWER`].
    agent: ureq::Agent,
    /// Event streams: open for good, dead after [`SILENCE`].
    stream: ureq::Agent,
    url: String,
    token: Option<String>,
}

impl ServeClient {
    /// A client of the server at `url` (`http://host:port`, as
    /// [`protocol::normalize_url`] makes it), with the device `token`.
    pub fn new(url: &str, token: Option<String>) -> Self {
        let agent = ureq::Agent::config_builder()
            .http_status_as_error(false)
            .max_redirects(0)
            .timeout_connect(Some(CONNECT))
            .timeout_recv_response(Some(ANSWER))
            .timeout_per_read(Some(ANSWER))
            .build()
            .into();
        let stream = ureq::Agent::config_builder()
            .http_status_as_error(false)
            .max_redirects(0)
            .timeout_connect(Some(CONNECT))
            .timeout_recv_response(Some(SILENCE))
            .timeout_per_read(Some(SILENCE))
            .build()
            .into();
        Self {
            agent,
            stream,
            url: url.trim_end_matches('/').to_string(),
            token,
        }
    }

    pub fn url(&self) -> &str {
        &self.url
    }

    fn unreachable(&self, error: ureq::Error) -> ServeError {
        ServeError {
            status: None,
            error: None,
            message: format!("Couldn't reach the server at {}: {error}", self.url),
        }
    }

    /// `method` on `path` with an optional JSON body: the answer's status
    /// and body for a 2xx, the server's error in words otherwise.
    fn call(
        &self,
        agent: &ureq::Agent,
        method: &str,
        path: &str,
        body: Option<&Value>,
    ) -> Result<(u16, ureq::http::Response<ureq::Body>), ServeError> {
        let url = format!("{}{path}", self.url);
        let mut request = ureq::http::Request::builder().method(method).uri(&url);
        if let Some(token) = &self.token {
            request = request.header("Authorization", format!("Bearer {token}"));
        }
        let response = match body {
            Some(body) => {
                let request = request
                    .header("Content-Type", "application/json")
                    .body(serde_json::to_vec(body).unwrap_or_default())
                    .map_err(|e| ServeError {
                        status: None,
                        error: None,
                        message: format!("Bad request to {url}: {e}"),
                    })?;
                agent.run(request)
            }
            None => {
                let request = request.body(()).map_err(|e| ServeError {
                    status: None,
                    error: None,
                    message: format!("Bad request to {url}: {e}"),
                })?;
                agent.run(request)
            }
        }
        .map_err(|e| self.unreachable(e))?;
        let status = response.status().as_u16();
        if (200..300).contains(&status) {
            return Ok((status, response));
        }
        let mut response = response;
        let body = read_json(&mut response).unwrap_or_default();
        let (code, message) = protocol::error_body(&body);
        Err(ServeError {
            status: Some(status),
            error: code.map(str::to_string),
            message: protocol::error_text(status, code, message),
        })
    }

    fn json(&self, method: &str, path: &str, body: Option<&Value>) -> Result<Value, ServeError> {
        let (status, mut response) = self.call(&self.agent, method, path, body)?;
        read_json(&mut response).map_err(|message| ServeError {
            status: Some(status),
            error: None,
            message,
        })
    }

    /// `POST /api/pair`: this device, named `name`, with the code `gentle-shell
    /// serve pair` printed. Needs no token.
    pub fn pair(&self, code: &str, name: &str) -> Result<Paired, ServeError> {
        let answer = self.json(
            "POST",
            "/api/pair",
            Some(&json!({"code": code, "name": name})),
        )?;
        let field = |key: &str| answer[key].as_str().map(str::to_string);
        match (field("id"), field("token")) {
            (Some(device), Some(token)) => Ok(Paired {
                device,
                name: field("name").unwrap_or_else(|| name.to_string()),
                token,
            }),
            _ => Err(ServeError {
                status: None,
                error: None,
                message: "The server's pairing answer has no device or token.".into(),
            }),
        }
    }

    /// `GET /api/server`: who the server is (and that this token works).
    pub fn server(&self) -> Result<ServerInfo, ServeError> {
        let answer = self.json("GET", "/api/server", None)?;
        Ok(ServerInfo {
            server_id: answer["serverId"].as_str().unwrap_or_default().to_string(),
            version: answer["version"].as_str().map(str::to_string),
        })
    }

    /// `GET /api/sessions/:id`.
    pub fn session(&self, id: &str) -> Result<SessionInfo, ServeError> {
        let answer = self.json("GET", &session_path(id, "")?, None)?;
        SessionInfo::parse(&answer["session"]).ok_or_else(malformed)
    }

    /// `POST /api/sessions`: a session in `cwd` (a folder as the server sees
    /// it), resuming the Pi session `resume` when given.
    pub fn start(&self, cwd: &str, resume: Option<&str>) -> Result<SessionInfo, ServeError> {
        let mut body = json!({"cwd": cwd});
        if let Some(resume) = resume {
            body["sessionId"] = resume.into();
        }
        let answer = self.json("POST", "/api/sessions", Some(&body))?;
        SessionInfo::parse(&answer["session"]).ok_or_else(malformed)
    }

    /// `DELETE /api/sessions/:id`: stop the session and forget it.
    pub fn stop(&self, id: &str) -> Result<(), ServeError> {
        self.call(&self.agent, "DELETE", &session_path(id, "")?, None)
            .map(drop)
    }

    /// `POST /api/sessions/:id/rpc`: the answer's status and body.
    fn rpc(&self, id: &str, command: &Value) -> Result<(u16, Vec<u8>), ServeError> {
        let (status, mut response) = self.call(
            &self.agent,
            "POST",
            &session_path(id, "/rpc")?,
            Some(command),
        )?;
        let body = response
            .body_mut()
            .with_config()
            .limit(BODY_LIMIT)
            .read_to_vec()
            .map_err(|e| self.unreachable(e))?;
        Ok((status, body))
    }

    /// `GET /api/sessions/:id/events`, after `after` (live without it).
    fn events(
        &self,
        id: &str,
        after: Option<u64>,
    ) -> Result<ureq::BodyReader<'static>, ServeError> {
        let mut path = session_path(id, "/events")?;
        if let Some(after) = after {
            path.push_str(&format!("?after={after}"));
        }
        let (_, response) = self.call(&self.stream, "GET", &path, None)?;
        Ok(response.into_body().into_with_config().reader())
    }
}

/// The server a connection code names, looked for at every address in it
/// at once (`GET /.well-known/gentle-shell`, no token): the base URL of the
/// first that answers as that server, or what to do in words. Each address
/// gets [`PROBE`], the whole search [`SEARCH`]. The pairing code is sent
/// nowhere here. Blocking.
pub fn locate(code: &ConnectionCode) -> Result<String, String> {
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .http_status_as_error(false)
        .max_redirects(0)
        .timeout_global(Some(PROBE))
        .build()
        .into();
    let (tx, rx) = mpsc::channel();
    for (index, url) in code.urls.iter().enumerate() {
        let (agent, tx) = (agent.clone(), tx.clone());
        let url = format!("{url}{}", protocol::IDENTITY_PATH);
        // One that can't start counts as unreachable: its sender is gone.
        let _ = std::thread::Builder::new()
            .name("serve-probe".into())
            .spawn(move || {
                let _ = tx.send((index, probe(&agent, &url)));
            });
    }
    drop(tx);
    let deadline = Instant::now() + SEARCH;
    let mut arrived = Vec::new();
    loop {
        let finished = match rx.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
            Ok(answer) => {
                arrived.push(answer);
                arrived.len() == code.urls.len()
            }
            // Time is up, or every probe that started answered.
            Err(_) => true,
        };
        match protocol::search(&code.server_id, &arrived, finished) {
            Search::Found(index) => return Ok(code.urls[index].clone()),
            Search::Waiting => {}
            Search::OtherServer => return Err(code.other_server()),
            Search::NotFound => return Err(code.not_found()),
        }
    }
}

/// What the address at `url` says it is.
fn probe(agent: &ureq::Agent, url: &str) -> Probe {
    let Ok(mut response) = agent.get(url).call() else {
        return Probe::Unreachable;
    };
    if !response.status().is_success() {
        return Probe::NotAServer;
    }
    read_json(&mut response).map_or(Probe::NotAServer, |body| protocol::identity(&body))
}

/// Pair this device, named `name`, with the server a connection code names
/// at `url` (where [`locate`] found it). A code the server no longer takes
/// reads as one to make again. Blocking.
pub fn pair_code(code: &ConnectionCode, url: &str, name: &str) -> Result<Paired, String> {
    ServeClient::new(url, None)
        .pair(&code.pairing_code, name)
        .map_err(|e| match (e.status, e.error.as_deref()) {
            (Some(403), Some("invalid_code")) => code.expired(),
            _ => e.message,
        })
}

fn session_path(id: &str, rest: &str) -> Result<String, ServeError> {
    if protocol::is_session_id(id) {
        Ok(format!("/api/sessions/{id}{rest}"))
    } else {
        Err(ServeError {
            status: Some(404),
            error: None,
            message: protocol::error_text(404, None, None),
        })
    }
}

fn malformed() -> ServeError {
    ServeError {
        status: None,
        error: None,
        message: "The server's answer isn't what Gentle Shell servers send.".into(),
    }
}

fn read_json(response: &mut ureq::http::Response<ureq::Body>) -> Result<Value, String> {
    let body = response
        .body_mut()
        .with_config()
        .limit(BODY_LIMIT)
        .read_to_vec()
        .map_err(|e| e.to_string())?;
    if body.is_empty() {
        return Ok(Value::Null);
    }
    serde_json::from_slice(&body).map_err(|e| format!("The server's answer isn't JSON: {e}"))
}

/// The server side of a [`PiProcess`]: where commands queue, and how the
/// session ends.
pub(crate) struct Remote {
    client: ServeClient,
    session: String,
    commands: mpsc::Sender<Value>,
    /// Set when this client stops following the session (closed or detached).
    stop: Arc<AtomicBool>,
}

impl Remote {
    pub(crate) fn send(&self, command: Value) -> Result<()> {
        if self.stop.load(Ordering::Acquire) {
            return Err(anyhow!("the server session is closing"));
        }
        self.commands
            .send(command)
            .map_err(|_| anyhow!("the server session is closed"))
    }

    /// Stop the session there, in the background (the server waits for its
    /// process to exit).
    pub(crate) fn stop(&self) {
        self.stop.store(true, Ordering::Release);
        let (client, session) = (self.client.clone(), self.session.clone());
        let _ = std::thread::Builder::new()
            .name("pi-rpc-serve-stop".into())
            .spawn(move || {
                let _ = client.stop(&session);
            });
    }

    /// Stop following the session, leaving it running there.
    pub(crate) fn detach(&self) {
        self.stop.store(true, Ordering::Release);
    }
}

impl PiProcess {
    /// Follow a session of the server `client` reaches, as a backend: the
    /// session `attach` when it still runs there (a tab the app restored),
    /// else a new one in `cwd` (a folder as the server sees it) resuming the
    /// Pi session `resume`. `incoming` receives what a child's stdout would
    /// carry, then `Exited` when the session ends. Blocking: it asks the
    /// server first.
    pub fn serve(
        client: ServeClient,
        cwd: &str,
        attach: Option<&str>,
        resume: Option<&str>,
    ) -> Result<(Arc<Self>, async_channel::Receiver<Incoming>)> {
        let found = match attach.map(|id| client.session(id)) {
            Some(Ok(found)) if found.running => Some(found.id),
            // Stopped, exited, or from a server run since restarted.
            Some(Ok(_)) | None => None,
            Some(Err(error)) if error.status == Some(404) => None,
            // Not reachable: never a second session beside the one running.
            Some(Err(error)) => return Err(error.into()),
        };
        let session = match found {
            Some(id) => id,
            None => client.start(cwd, resume)?.id,
        };
        let (tx, rx) = async_channel::unbounded();
        let pending: Pending = Arc::default();
        let traffic = Arc::new(Traffic::default());
        let ids = Arc::new(Mutex::new(RpcIds::default()));
        let stop = Arc::new(AtomicBool::new(false));
        let (commands, queue) = mpsc::channel();
        {
            let (client, session) = (client.clone(), session.clone());
            let (pending, ids, tx) = (pending.clone(), ids.clone(), tx.clone());
            std::thread::Builder::new()
                .name("pi-rpc-serve-commands".into())
                .spawn(move || write_commands(&client, &session, queue, &pending, &ids, &tx))?;
        }
        {
            let (client, session) = (client.clone(), session.clone());
            let (pending, traffic, stop) = (pending.clone(), traffic.clone(), stop.clone());
            std::thread::Builder::new()
                .name("pi-rpc-serve-events".into())
                .spawn(move || {
                    let follow = Follow {
                        client: &client,
                        session: &session,
                        pending: &pending,
                        ids: &ids,
                        traffic: &traffic,
                        tx: &tx,
                        stop: &stop,
                    };
                    let code = follow.run();
                    // Unblock every caller still waiting for a response.
                    pending.lock().unwrap().clear();
                    let _ = tx.send_blocking(Incoming::Exited(code));
                })?;
        }
        let process = Arc::new(Self {
            io: Io::Server(Remote {
                client,
                session,
                commands,
                stop,
            }),
            pending,
            next_id: AtomicU64::new(1),
            stopping: AtomicBool::new(false),
            traffic,
        });
        Ok((process, rx))
    }

    /// The server session this backend follows (`ses_…`); `None` for a
    /// child process.
    pub fn server_session(&self) -> Option<&str> {
        match &self.io {
            Io::Server(remote) => Some(&remote.session),
            Io::Child { .. } => None,
        }
    }
}

/// Post each queued command in order; one still unanswered after
/// [`OVERTAKE`] lets the next go.
fn write_commands(
    client: &ServeClient,
    session: &str,
    queue: mpsc::Receiver<Value>,
    pending: &Pending,
    ids: &Arc<Mutex<RpcIds>>,
    tx: &async_channel::Sender<Incoming>,
) {
    for command in queue {
        let (done, answered) = mpsc::channel::<()>();
        let (client, session) = (client.clone(), session.to_string());
        let (pending, ids, tx) = (pending.clone(), ids.clone(), tx.clone());
        let posted = std::thread::Builder::new()
            .name("pi-rpc-serve-command".into())
            .spawn(move || {
                post_command(&client, &session, command, &pending, &ids, &tx);
                let _ = done.send(());
            });
        if posted.is_ok() {
            let _ = answered.recv_timeout(OVERTAKE);
        }
    }
}

/// One command: its answer reaches the caller as if read from stdout; a
/// failure answers it with the error.
fn post_command(
    client: &ServeClient,
    session: &str,
    command: Value,
    pending: &Pending,
    ids: &Mutex<RpcIds>,
    tx: &async_channel::Sender<Incoming>,
) {
    let id = command["id"].as_str().map(str::to_string);
    let kind = command["type"].as_str().unwrap_or("command").to_string();
    match client.rpc(session, &command) {
        // 202: a dialog answer accepted, or a command still running whose
        // response comes in the stream.
        Ok((202, body)) => {
            let answer: Value = serde_json::from_slice(&body).unwrap_or_default();
            if answer["pending"] == true
                && let (Some(server), Some(id)) = (answer["id"].as_str(), id.as_deref())
                && let Some(response) = ids.lock().unwrap().pending(server, id)
            {
                dispatch(record_from(response), pending, tx);
            }
        }
        Ok((_, body)) => {
            dispatch(crate::parse_record(&body), pending, tx);
        }
        Err(error) => match id {
            Some(id) => {
                let failed = json!({
                    "type": "response",
                    "id": id,
                    "command": kind,
                    "success": false,
                    "error": error.message,
                });
                dispatch(record_from(failed), pending, tx);
            }
            None => {
                let _ = tx.send_blocking(Incoming::Stderr(format!("{kind} failed: {error}")));
            }
        },
    }
}

/// The event stream's reader.
struct Follow<'a> {
    client: &'a ServeClient,
    session: &'a str,
    pending: &'a Pending,
    ids: &'a Mutex<RpcIds>,
    traffic: &'a Traffic,
    tx: &'a async_channel::Sender<Incoming>,
    stop: &'a AtomicBool,
}

/// How one connection ended.
enum Ended {
    /// The session exited (its code).
    Exited(Option<i32>),
    /// Nobody listens any more.
    Closed,
    /// The server asked for a reconnect (`resync`).
    Resync,
    /// The connection dropped or went silent.
    Lost,
}

impl Follow<'_> {
    /// Follow the stream until the session ends or this client stops,
    /// reconnecting after the last record seen: its exit code.
    fn run(&self) -> Option<i32> {
        let mut stream = Stream::default();
        let mut failures = 0u32;
        loop {
            if self.stop.load(Ordering::Acquire) {
                return None;
            }
            match self.client.events(self.session, stream.after()) {
                Ok(body) => {
                    failures = 0;
                    match self.read(body, &mut stream) {
                        Ended::Exited(code) => return code,
                        Ended::Closed => return None,
                        Ended::Resync => continue,
                        Ended::Lost => {}
                    }
                    if self.stop.load(Ordering::Acquire) {
                        return None;
                    }
                    // An exited session's stream ends after its replay; one
                    // whose exit record was already evicted ends this way.
                    match self.client.session(self.session) {
                        Ok(info) if !info.running => return info.exit_code,
                        Err(error) if matches!(error.status, Some(401 | 403 | 404)) => {
                            self.note(error.message);
                            return None;
                        }
                        _ => {}
                    }
                }
                // Gone, or this device's token no longer works: nothing to
                // reconnect to.
                Err(error) if matches!(error.status, Some(401 | 403 | 404)) => {
                    self.note(error.message);
                    return None;
                }
                Err(error) => {
                    if failures == 0 {
                        self.note(format!("{error}. Reconnecting…"));
                    }
                }
            }
            let wait = Duration::from_secs(protocol::backoff_secs(failures));
            failures = failures.saturating_add(1);
            let mut waited = Duration::ZERO;
            while waited < wait && !self.stop.load(Ordering::Acquire) {
                std::thread::sleep(Duration::from_millis(100));
                waited += Duration::from_millis(100);
            }
        }
    }

    fn read(&self, mut body: ureq::BodyReader<'static>, stream: &mut Stream) -> Ended {
        let mut sse = SseParser::default();
        let mut buf = vec![0u8; 64 * 1024];
        loop {
            let n = match body.read(&mut buf) {
                Ok(0) | Err(_) => return Ended::Lost,
                Ok(n) => n,
            };
            if self.stop.load(Ordering::Acquire) {
                return Ended::Closed;
            }
            self.traffic.bytes.fetch_add(n as u64, Ordering::Relaxed);
            for event in sse.push(&buf[..n]) {
                match stream.step(&event) {
                    Step::Record(record) => {
                        let Some(record) = self.ids.lock().unwrap().from_stream(record) else {
                            continue;
                        };
                        self.traffic.records.fetch_add(1, Ordering::Relaxed);
                        if !dispatch(record_from(record), self.pending, self.tx) {
                            return Ended::Closed;
                        }
                    }
                    Step::Snapshot => {
                        if self.tx.send_blocking(Incoming::Resync).is_err() {
                            return Ended::Closed;
                        }
                    }
                    Step::Reconnect => return Ended::Resync,
                    Step::Exited(code) => return Ended::Exited(code),
                    Step::Note(note) => self.note(note),
                    Step::Ignore => {}
                }
            }
        }
    }

    fn note(&self, line: String) {
        let _ = self.tx.send_blocking(Incoming::Stderr(line));
    }
}
