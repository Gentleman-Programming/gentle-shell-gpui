//! A Gentle Shell server's session as a backend (`gentle-shell serve`,
//! protocol `gentle-shell.serve/v1`, Gentle Shell's `docs/serve.md`): the
//! same records and commands as a child's stdio, over HTTP and Server-Sent
//! Events, so a tab reads and drives it exactly like a local one. A server
//! on the network speaks HTTPS with a certificate this device pins, and may
//! be reached through a relay ([`Trust`], `tls`).
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

mod files;
mod local;
mod services;
mod terminals;
mod tls;
mod upgrade;

pub use files::{
    Entry, EntryKind, FileChunk, GitOutput, Listing, READ_CHUNK, SearchHit, Stat, Uploaded,
    WRITE_LIMIT, WatchEvent, WatchStream, Written,
};
pub use local::{LocalHold, hold_local_page};
pub use services::{ForwardedPage, Service};
pub use terminals::{TermEvent, TermHost, TermStream, Terminal};
pub use upgrade::{UpgradeEvent, UpgradeStream};

use std::io::Read as _;
use std::net::{IpAddr, Ipv4Addr, SocketAddr, UdpSocket};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock, mpsc};
use std::time::{Duration, Instant};

use anyhow::{Result, anyhow};
use gentle_core::serve::{
    self as protocol, Announce, ConnectionCode, Nearby, Probe, RpcIds, Search, SseParser, Step,
    Stream,
};
use serde_json::{Value, json};

use crate::{Incoming, Io, Pending, PiProcess, Traffic, dispatch, record::record_from};

/// How long a connection may take.
const CONNECT: Duration = Duration::from_secs(10);
/// Silence after which a stream is dead: two heartbeats missed.
const SILENCE: Duration = Duration::from_secs(45);
/// How long an answer may take: the server waits 30 s for Pi's.
const ANSWER: Duration = Duration::from_secs(45);
/// How long work the server bounds itself may take: a network git command
/// (600 s there), a 64 MiB upload or terminal host on a slow network.
const LONG: Duration = Duration::from_secs(11 * 60);
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
/// How long servers on the network have to answer the discovery query.
const LISTEN: Duration = Duration::from_millis(1500);
/// The most distinct discovery answers kept (each is then checked).
const ANSWERS: usize = 32;

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
    /// How the server it paired with is reached: kept with the token.
    pub trust: Trust,
}

/// How a server is reached safely, kept with its device token: the
/// certificate pinned when it speaks HTTPS, and the relay that reaches it
/// from anywhere. The default is plain HTTP (a server on this computer, or
/// one started with `--no-tls`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Trust {
    /// Its certificate's fingerprint (base64url SHA-256 of its DER): every
    /// `https://` connection accepts that certificate and no other.
    pub fingerprint: Option<String>,
    /// `gsr://host:port`: requests to that address go through the relay to
    /// the server with [`Self::fingerprint`].
    pub relay: Option<String>,
}

impl Trust {
    /// Pinning `fingerprint`, through `relay` when given.
    pub fn pinned(fingerprint: &str, relay: Option<&str>) -> Self {
        Self {
            fingerprint: Some(fingerprint.to_string()),
            relay: relay.map(str::to_string),
        }
    }

    fn link(&self, relay_port: Option<u16>) -> Arc<tls::Link> {
        let pin = match &self.fingerprint {
            Some(fp) => tls::Pin::Exact(fp.clone()),
            None => tls::Pin::None,
        };
        tls::Link::new(pin, self.relay.as_deref(), relay_port)
    }
}

/// `GET /api/server`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ServerInfo {
    /// `srv_…`, new on every server run.
    pub server_id: String,
    /// The server's Gentle Shell version.
    pub version: Option<String>,
    /// The home folder of the user the server runs as, a path there (only
    /// to devices that may operate sessions; older servers don't say).
    pub home: Option<String>,
    /// What the server offers its devices (`files`, `git`, `uploads`,
    /// `terminals`, `server-forwards`, `services`, …).
    pub capabilities: Vec<String>,
    /// Node's `process.platform` there (`darwin`, `linux`, `win32`).
    pub platform: Option<String>,
    /// Node's `os.arch()` there (`arm64`, `x64`).
    pub arch: Option<String>,
    /// Its path separator (`/` or `\`).
    pub path_separator: Option<String>,
}

impl ServerInfo {
    /// Whether the server offers `capability`.
    pub fn has(&self, capability: &str) -> bool {
        self.capabilities.iter().any(|c| c == capability)
    }
}

/// A request body: JSON, or raw bytes (a file, an upload, a binary).
#[derive(Clone, Copy)]
enum Body<'a> {
    Json(&'a [u8]),
    Raw(&'a [u8]),
}

/// A page on the server's own loopback, reachable from this device through
/// one of the server's forwards (`POST /api/sessions/:id/forwards`,
/// `POST /api/tower`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Forward {
    /// `fwd_…`.
    pub id: String,
    /// The page's port on the server's loopback.
    pub port: u16,
    /// The address to open, as the server built it: the forward's own
    /// port on the host this client reached the server at, the page's path
    /// and a single-use ticket (`gs_ticket`, 60 s) the first visit trades
    /// for a cookie. Every call mints a new one, so a page opened again
    /// asks again (the forward itself is reused).
    pub url: String,
}

impl Forward {
    fn parse(value: &Value) -> Option<Self> {
        Some(Self {
            id: value["id"].as_str()?.to_string(),
            port: u16::try_from(value["port"].as_u64()?).ok()?,
            url: value["url"].as_str()?.to_string(),
        })
    }
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

/// Where the server went when its address stopped answering: given the
/// address that failed, the address it answers at now (checked to be the
/// same server), or `None`. The app looks on the network and at the
/// addresses it knows ([`find`], [`nearby`]).
pub type Relocate = Arc<dyn Fn(&str) -> Option<String> + Send + Sync>;

/// The server at one address, as one paired device (or none, to pair).
/// Clones share the address: when the server is found at another one
/// ([`Self::relocating`]), every clone follows.
#[derive(Clone)]
pub struct ServeClient {
    /// Requests: answers within [`ANSWER`].
    agent: ureq::Agent,
    /// Event streams: open for good, dead after [`SILENCE`].
    stream: ureq::Agent,
    /// Work the server bounds itself (a `git fetch`, an upload, a
    /// binary): answers within [`LONG`].
    long: ureq::Agent,
    shared: Arc<Shared>,
    token: Option<String>,
    relocate: Option<Relocate>,
}

/// What every clone of a [`ServeClient`] shares.
struct Shared {
    /// The address requests go to now.
    url: RwLock<String>,
    trust: Trust,
    /// The connections' certificate pin and relay.
    link: Arc<tls::Link>,
}

/// An agent whose connections go over `link` (plain, pinned TLS or a
/// relay), never redirected, with the server's errors as answers.
fn linked_agent(
    link: &Arc<tls::Link>,
    timeouts: impl FnOnce(ConfigBuilder) -> ConfigBuilder,
) -> ureq::Agent {
    let config = timeouts(
        ureq::Agent::config_builder()
            .http_status_as_error(false)
            .max_redirects(0),
    )
    .build();
    ureq::Agent::with_parts(
        config,
        tls::LinkConnector(link.clone()),
        ureq::unversioned::resolver::DefaultResolver::default(),
    )
}

type ConfigBuilder = ureq::config::ConfigBuilder<ureq::typestate::AgentScope>;

impl ServeClient {
    /// A client of the server at `url` (`http://host:port`, as
    /// [`protocol::normalize_url`] makes it) over plain HTTP, with the
    /// device `token`.
    pub fn new(url: &str, token: Option<String>) -> Self {
        Self::with_trust(url, token, Trust::default())
    }

    /// A client of the server at `url` (`https://host:port`, or the
    /// relay's `gsr://host:port`) reached as `trust` says, with the device
    /// `token`.
    pub fn with_trust(url: &str, token: Option<String>, trust: Trust) -> Self {
        let link = trust.link(None);
        Self::over(url, token, trust, link)
    }

    /// First contact with a server that speaks HTTPS, before a pairing
    /// proof: any certificate is accepted and the one seen remembered.
    fn first_contact(url: &str) -> Self {
        let link = tls::Link::new(tls::Pin::FirstContact, None, None);
        Self::over(url, None, Trust::default(), link)
    }

    fn over(url: &str, token: Option<String>, trust: Trust, link: Arc<tls::Link>) -> Self {
        let agent = linked_agent(&link, |c| {
            c.timeout_connect(Some(CONNECT))
                .timeout_recv_response(Some(ANSWER))
                .timeout_per_read(Some(ANSWER))
        });
        let stream = linked_agent(&link, |c| {
            c.timeout_connect(Some(CONNECT))
                .timeout_recv_response(Some(SILENCE))
                .timeout_per_read(Some(SILENCE))
        });
        let long = linked_agent(&link, |c| {
            c.timeout_connect(Some(CONNECT))
                .timeout_recv_response(Some(LONG))
                .timeout_per_read(Some(LONG))
        });
        Self {
            agent,
            stream,
            long,
            shared: Arc::new(Shared {
                url: RwLock::new(url.trim_end_matches('/').to_string()),
                trust,
                link,
            }),
            token,
            relocate: None,
        }
    }

    /// How this client reaches its server.
    pub fn trust(&self) -> &Trust {
        &self.shared.trust
    }

    /// This client, finding the server again with `relocate` when its
    /// address can't be connected to (the server's computer got another
    /// address on the network): the request is sent once more at the
    /// address `relocate` gives, which every clone uses from then on. Only
    /// a request that never reached the server is retried, so none runs
    /// twice.
    pub fn relocating(mut self, relocate: Relocate) -> Self {
        self.relocate = Some(relocate);
        self
    }

    /// The address requests go to now.
    pub fn url(&self) -> String {
        self.shared
            .url
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    fn unreachable(&self, error: ureq::Error) -> ServeError {
        if self.shared.link.changed() {
            return ServeError {
                status: None,
                error: Some("certificate_changed".into()),
                message: protocol::CERTIFICATE_CHANGED.into(),
            };
        }
        ServeError {
            status: None,
            error: None,
            message: format!("Couldn't reach the server at {}: {error}", self.url()),
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
        let body = body.map(|body| serde_json::to_vec(body).unwrap_or_default());
        self.send(agent, method, path, body.as_deref().map(Body::Json), &[])
    }

    /// `method` on `path` with an optional body and extra headers: the
    /// answer's status and body for a 2xx, the server's error in words
    /// otherwise. An address that can't be connected to is looked for
    /// again ([`Self::relocating`]) and the request sent there.
    fn send(
        &self,
        agent: &ureq::Agent,
        method: &str,
        path: &str,
        body: Option<Body<'_>>,
        headers: &[(&str, &str)],
    ) -> Result<(u16, ureq::http::Response<ureq::Body>), ServeError> {
        let base = self.url();
        let response = match self.run(agent, &base, method, path, body, headers)? {
            Ok(response) => response,
            Err(error) => {
                let moved = self
                    .relocate
                    .as_ref()
                    .filter(|_| never_connected(&error))
                    .and_then(|relocate| relocate(&base));
                let Some(moved) = moved.map(|url| url.trim_end_matches('/').to_string()) else {
                    return Err(self.unreachable(error));
                };
                *self.shared.url.write().unwrap_or_else(|e| e.into_inner()) = moved.clone();
                self.run(agent, &moved, method, path, body, headers)?
                    .map_err(|e| self.unreachable(e))?
            }
        };
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

    /// One request to the server at `base`: the transport's answer or
    /// failure; an error for a request that can't be built.
    fn run(
        &self,
        agent: &ureq::Agent,
        base: &str,
        method: &str,
        path: &str,
        body: Option<Body<'_>>,
        headers: &[(&str, &str)],
    ) -> Result<Result<ureq::http::Response<ureq::Body>, ureq::Error>, ServeError> {
        let url = format!("{}{path}", tls::transport_url(base));
        let mut request = ureq::http::Request::builder().method(method).uri(&url);
        if let Some(token) = &self.token {
            request = request.header("Authorization", format!("Bearer {token}"));
        }
        for (name, value) in headers {
            request = request.header(*name, *value);
        }
        let bad = |e: ureq::http::Error| ServeError {
            status: None,
            error: None,
            message: format!("Bad request to {url}: {e}"),
        };
        Ok(match body {
            Some(Body::Json(body)) => agent.run(
                request
                    .header("Content-Type", "application/json")
                    .body(body)
                    .map_err(bad)?,
            ),
            Some(Body::Raw(body)) => agent.run(
                request
                    .header("Content-Type", "application/octet-stream")
                    .body(body)
                    .map_err(bad)?,
            ),
            None => agent.run(request.body(()).map_err(bad)?),
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
                trust: self.shared.trust.clone(),
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
            home: answer["home"]
                .as_str()
                .filter(|home| !home.is_empty())
                .map(str::to_string),
            capabilities: answer["capabilities"]
                .as_array()
                .map(|all| {
                    all.iter()
                        .filter_map(|c| c.as_str().map(str::to_string))
                        .collect()
                })
                .unwrap_or_default(),
            platform: answer["platform"].as_str().map(str::to_string),
            arch: answer["arch"].as_str().map(str::to_string),
            path_separator: answer["pathSeparator"].as_str().map(str::to_string),
        })
    }

    /// `POST /api/sessions/:id/forwards { port, path }`: the page at `path`
    /// on `port` of the server's loopback, opened by the session `session`.
    /// The server reuses this device's forward of the same port, with a
    /// fresh ticket.
    pub fn forward(&self, session: &str, port: u16, path: &str) -> Result<Forward, ServeError> {
        let answer = self.json(
            "POST",
            &session_path(session, "/forwards")?,
            Some(&json!({"port": port, "path": path})),
        )?;
        Forward::parse(&answer).ok_or_else(malformed)
    }

    /// `POST /api/tower`: Gentle Tower running on the server (started when
    /// it isn't), through a forward. A server without the route says so.
    pub fn tower(&self) -> Result<Forward, ServeError> {
        // An empty object rather than no body: a POST always says its length.
        let answer = self
            .json("POST", "/api/tower", Some(&json!({})))
            .map_err(|e| {
                if e.status == Some(404) || e.status == Some(405) {
                    ServeError {
                        message: "This server's gentle-shell serve can't run Gentle Tower for its \
                              devices: upgrade Gentle Shell there."
                            .into(),
                        ..e
                    }
                } else {
                    e
                }
            })?;
        Forward::parse(&answer).ok_or_else(malformed)
    }

    /// `POST /api/print { cwd, prompt }` (capability `print`): one
    /// `gentle-shell --print --no-session --no-tools` run in `cwd` (a folder
    /// there) on `prompt` with the server's default model, as this computer
    /// runs one for a commit message. Its stdout as it printed it. The
    /// server gives it 120 s.
    pub fn print(&self, cwd: &str, prompt: &str) -> Result<String, ServeError> {
        let body = json!({"cwd": cwd, "prompt": prompt});
        let (status, mut response) = self.call(&self.long, "POST", "/api/print", Some(&body))?;
        let answer = read_json(&mut response).map_err(|message| ServeError {
            status: Some(status),
            error: None,
            message,
        })?;
        answer["text"]
            .as_str()
            .map(str::to_string)
            .ok_or_else(malformed)
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

    /// `DELETE /api/devices/self` (capability `unpair`): the server forgets
    /// this device; its token stops working there, its streams close.
    pub fn unpair(&self) -> Result<(), ServeError> {
        self.call(&self.agent, "DELETE", "/api/devices/self", None)
            .map(drop)
    }

    /// `POST /api/devices/self/replace { token }` (capability `unpair`):
    /// this device, just paired again, takes the place of its earlier
    /// pairing, whose token it shows: that device's terminals are this
    /// one's now, and it is revoked.
    pub fn replace(&self, token: &str) -> Result<(), ServeError> {
        let body = json!({"token": token});
        self.call(
            &self.agent,
            "POST",
            "/api/devices/self/replace",
            Some(&body),
        )
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

    /// `POST /api/sessions/:id/desktop` with an inner Desktop request
    /// document as the body (the server reads its `method` and `params`):
    /// the answer the server correlated, as the record it is.
    fn desktop(&self, id: &str, payload: &str) -> Result<Value, ServeError> {
        let (status, mut response) = self.send(
            &self.agent,
            "POST",
            &session_path(id, "/desktop")?,
            Some(Body::Json(payload.as_bytes())),
            &[],
        )?;
        read_json(&mut response).map_err(|message| ServeError {
            status: Some(status),
            error: None,
            message,
        })
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
/// and through its relay at once (`GET /.well-known/gentle-shell`, no
/// token; over HTTPS pinned to the code's certificate when it has one): the
/// base URL of the first that answers as that server (the relay's
/// `gsr://host:port` when only it reached the server), or what to do in
/// words. Each address gets [`PROBE`], the whole search [`SEARCH`]. The
/// pairing code is sent nowhere here. Blocking.
pub fn locate(code: &ConnectionCode) -> Result<String, String> {
    let trust = code_trust(code);
    let urls: Vec<String> = code.urls.iter().chain(&code.relay).cloned().collect();
    match search(&code.server_id, &urls, &trust) {
        Search::Found(index) => Ok(urls[index].clone()),
        Search::OtherServer => Err(code.other_server()),
        Search::NotFound | Search::Waiting => Err(code.not_found()),
    }
}

/// How the server a connection code names is reached: the code came from
/// the server itself, so its certificate is pinned as it is.
fn code_trust(code: &ConnectionCode) -> Trust {
    Trust {
        fingerprint: code.fingerprint.clone(),
        relay: code.relay.clone(),
    }
}

/// The first of `urls` where the server `server_id` answers
/// (`GET /.well-known/gentle-shell` at all of them at once, no token, as
/// [`locate`] does), reached as `trust` says: where a paired server went
/// after its address changed. A server showing another certificate than
/// the pinned one isn't it. Blocking, [`SEARCH`] at most.
pub fn find(server_id: &str, urls: &[String], trust: &Trust) -> Option<String> {
    match search(server_id, urls, trust) {
        Search::Found(index) => Some(urls[index].clone()),
        _ => None,
    }
}

/// Ask every one of `urls` who it is, at once, until the server
/// `server_id` answers or [`SEARCH`] passes.
fn search(server_id: &str, urls: &[String], trust: &Trust) -> Search {
    let agent = probe_agent(&trust.link(None));
    let (tx, rx) = mpsc::channel();
    for (index, url) in urls.iter().enumerate() {
        let (agent, tx) = (agent.clone(), tx.clone());
        let url = url.clone();
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
                arrived.len() == urls.len()
            }
            // Time is up, or every probe that started answered.
            Err(_) => true,
        };
        match protocol::search(server_id, &arrived, finished) {
            Search::Waiting => {}
            done => return done,
        }
    }
}

/// Who answers at `url` (`GET /.well-known/gentle-shell`, no token),
/// reached as `trust` says: its server id and its machine's host name.
/// Blocking, [`PROBE`] at most.
pub fn identify(url: &str, trust: &Trust) -> Option<(String, String)> {
    let body = identity_at(&probe_agent(&trust.link(None)), url).ok()?;
    let Probe::Server(id) = protocol::identity(&body) else {
        return None;
    };
    Some((id, body["name"].as_str().unwrap_or_default().to_string()))
}

/// The identity answer at the base URL `url`: its body for a 2xx, else
/// the status and body; `None` when nothing answered.
fn identity_at(agent: &ureq::Agent, url: &str) -> Result<Value, Option<(u16, Value)>> {
    let url = format!("{}{}", tls::transport_url(url), protocol::IDENTITY_PATH);
    let mut response = agent.get(&url).call().map_err(|_| None)?;
    let status = response.status().as_u16();
    let body = read_json(&mut response).unwrap_or_default();
    if (200..300).contains(&status) {
        Ok(body)
    } else {
        Err(Some((status, body)))
    }
}

/// Whether a request failed before reaching the server (no such host, or
/// nothing accepts connections at its address): sending it elsewhere can't
/// run it twice.
fn never_connected(error: &ureq::Error) -> bool {
    use std::io::ErrorKind;
    match error {
        ureq::Error::HostNotFound | ureq::Error::ConnectionFailed => true,
        ureq::Error::Timeout(timeout) => {
            matches!(timeout, ureq::Timeout::Resolve | ureq::Timeout::Connect)
        }
        ureq::Error::Io(e) => matches!(
            e.kind(),
            ErrorKind::ConnectionRefused
                | ErrorKind::HostUnreachable
                | ErrorKind::NetworkUnreachable
                | ErrorKind::AddrNotAvailable
        ),
        _ => false,
    }
}

/// What the server at the base URL `url` says it is.
fn probe(agent: &ureq::Agent, url: &str) -> Probe {
    match identity_at(agent, url) {
        Ok(body) => protocol::identity(&body),
        Err(Some(_)) => Probe::NotAServer,
        Err(None) => Probe::Unreachable,
    }
}

/// An agent for `GET /.well-known/gentle-shell` over `link`: [`PROBE`] in
/// all.
fn probe_agent(link: &Arc<tls::Link>) -> ureq::Agent {
    linked_agent(link, |c| c.timeout_global(Some(PROBE)))
}

/// The Gentle Shell servers on the local network: the discovery query
/// (`{"t":"gentle-shell/discover","v":1}`) goes from one UDP socket to every
/// address in `targets` (broadcast addresses and ports,
/// [`protocol::discovery_targets`]); answers are collected for [`LISTEN`],
/// then each is checked at `/.well-known/gentle-shell` on the address it
/// came from: only those saying the same server id are kept, in the order
/// they answered. No secret is sent. Blocking.
pub fn nearby(targets: &[SocketAddr]) -> Result<Vec<Nearby>, String> {
    let socket = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0))
        .map_err(|e| format!("Couldn't look for servers on the network: {e}"))?;
    // Without it, sending to a broadcast address is refused; a target that
    // still can't be sent to (no route) is skipped.
    let _ = socket.set_broadcast(true);
    let query = protocol::discover_query();
    for target in targets {
        let _ = socket.send_to(&query, target);
    }
    let deadline = Instant::now() + LISTEN;
    let mut heard: Vec<(Announce, IpAddr)> = Vec::new();
    // One byte more than an answer may be, so a longer one reads as too long.
    let mut buf = [0u8; protocol::DATAGRAM_MAX + 1];
    while heard.len() < ANSWERS {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() || socket.set_read_timeout(Some(left)).is_err() {
            break;
        }
        match socket.recv_from(&mut buf) {
            Ok((len, from)) => {
                if let Some(announce) = protocol::read_announce(&buf[..len]) {
                    let answer = (announce, from.ip());
                    if !heard.contains(&answer) {
                        heard.push(answer);
                    }
                }
            }
            Err(e)
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) =>
            {
                break;
            }
            // An ICMP error for one target (port unreachable): keep listening.
            Err(_) => {}
        }
    }
    let (tx, rx) = mpsc::channel();
    for (index, (announce, ip)) in heard.into_iter().enumerate() {
        let tx = tx.clone();
        let _ = std::thread::Builder::new()
            .name("serve-verify".into())
            .spawn(move || {
                let mut server = Nearby::new(&announce, ip);
                // Over HTTPS, the certificate the handshake shows is the
                // one a pairing proof is then checked against: the reply's
                // is only a hint.
                let link = if server.fingerprint.is_some() {
                    tls::Link::new(tls::Pin::FirstContact, None, None)
                } else {
                    Trust::default().link(None)
                };
                let verified = probe(&probe_agent(&link), &server.url())
                    == Probe::Server(server.server_id.clone());
                if server.fingerprint.is_some() {
                    server.fingerprint = link.seen();
                }
                let _ = tx.send((index, verified.then_some(server)));
            });
    }
    drop(tx);
    let mut checked: Vec<(usize, Nearby)> = rx
        .iter()
        .filter_map(|(index, server)| Some((index, server?)))
        .collect();
    checked.sort_by_key(|(index, _)| *index);
    Ok(checked.into_iter().map(|(_, server)| server).collect())
}

/// Pair this device, named `name`, with `server` (found on the network by
/// [`nearby`]) using the short pairing `code`. Over HTTPS the server first
/// proves it knows the code for the certificate it shows ([`prove`]), and
/// the code goes only to that certificate. A code the server doesn't take
/// reads as one to make again. Blocking.
pub fn pair_nearby(server: &Nearby, code: &str, name: &str) -> Result<Paired, String> {
    let url = server.url();
    let trust = match server.fingerprint {
        Some(_) => Trust {
            fingerprint: Some(prove(&url, code, &server.name)?),
            relay: None,
        },
        None => Trust::default(),
    };
    ServeClient::with_trust(&url, None, trust)
        .pair(code, name)
        .map_err(|e| match (e.status, e.error.as_deref()) {
            (Some(403), Some("invalid_code")) => protocol::short_code_refused(&server.name),
            _ => e.message,
        })
}

/// Pair this device, named `name`, with the server a connection code names
/// at `url` (where [`locate`] found it, maybe its relay), pinned to the
/// code's certificate. A code the server no longer takes reads as one to
/// make again. Blocking.
pub fn pair_code(code: &ConnectionCode, url: &str, name: &str) -> Result<Paired, String> {
    ServeClient::with_trust(url, None, code_trust(code))
        .pair(&code.pairing_code, name)
        .map_err(|e| match (e.status, e.error.as_deref()) {
            (Some(403), Some("invalid_code")) => code.expired(),
            _ => e.message,
        })
}

/// Pair this device, named `name`, with the server at the typed `url`
/// using `code`: where it paired (`https://` when the server speaks HTTPS:
/// its identity says so, or it refuses plain HTTP from the network) and the
/// pairing. Over HTTPS the server first proves it knows the code
/// ([`prove`]). Blocking.
pub fn pair_at(url: &str, code: &str, name: &str) -> Result<(String, Paired), String> {
    let mut url = url.to_string();
    if let Some(rest) = url.strip_prefix("http://").map(str::to_string) {
        let tls = match identity_at(&probe_agent(&Trust::default().link(None)), &url) {
            Ok(body) => protocol::announced_fingerprint(&body).is_some(),
            Err(Some((status, body))) => {
                protocol::error_body(&body).0 == Some("tls_required") && status == 400
            }
            Err(None) => false,
        };
        if tls {
            url = format!("https://{rest}");
        }
    }
    let trust = if url.starts_with("https://") {
        Trust {
            fingerprint: Some(prove(&url, code, &host_of(&url))?),
            relay: None,
        }
    } else {
        Trust::default()
    };
    let paired = ServeClient::with_trust(&url, None, trust)
        .pair(code, name)
        .map_err(|e| e.message)?;
    Ok((url, paired))
}

/// The host of a base URL.
fn host_of(url: &str) -> String {
    let rest = url.split_once("://").map_or(url, |(_, rest)| rest);
    match rest.rsplit_once(':') {
        Some((host, port)) if port.chars().all(|c| c.is_ascii_digit()) => host.to_string(),
        _ => rest.to_string(),
    }
}

/// Before a pairing code goes to the server at `url` (`https://`, found
/// through something anyone can answer, like discovery or a typed
/// address): the certificate it shows, once it proved it knows `code` for
/// it (`POST /api/pair/proof` with a fresh nonce; one of its proofs must
/// be HMAC(scrypt(code, salt ‖ fingerprint), fingerprint ‖ nonce)). The
/// code itself is never sent here. `name` names the server in what went
/// wrong. Blocking: the proof's scrypt takes a fraction of a second.
pub fn prove(url: &str, code: &str, name: &str) -> Result<String, String> {
    use base64::Engine as _;
    let mut nonce = [0u8; protocol::PROOF_NONCE_BYTES];
    rustls::crypto::ring::default_provider()
        .secure_random
        .fill(&mut nonce)
        .map_err(|_| "Couldn't make a random number for the pairing proof.".to_string())?;
    let client = ServeClient::first_contact(url);
    let body = json!({"nonce": base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(nonce)});
    let answer = client
        .json("POST", protocol::PROOF_PATH, Some(&body))
        .map_err(|e| e.message)?;
    let seen = client
        .shared
        .link
        .seen()
        .ok_or_else(|| protocol::proof_failed(name))?;
    if protocol::verify_pair_proof(code, &seen, &nonce, &answer["proofs"]) {
        Ok(seen)
    } else {
        Err(protocol::proof_failed(name))
    }
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
            io: Io::Server(Box::new(Remote {
                client,
                session,
                commands,
                stop,
            })),
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

    /// Send a Desktop request outside the app's Desktop channel and wait
    /// for its answer, as `set_host_tools`, `host_tool_result`,
    /// `host_message` and `set_host_context` go. A server session sends it
    /// to the server's `/desktop` endpoint, since the server checks who
    /// provides a tool and who answers its calls; the answer carries the
    /// server's own `requestId` and answers this HTTP request. A child gets
    /// it as a `/gentle:desktop` prompt on stdin, its answer taken from
    /// stdout by `requestId`. Blocking.
    pub fn desktop(
        &self,
        request: &crate::desktop_rpc::DesktopRequest,
    ) -> Result<crate::desktop_rpc::DesktopResponse> {
        let remote = match &self.io {
            Io::Server(remote) => remote,
            Io::Child { desktop, .. } => return self.desktop_stdin(desktop, request),
        };
        let answer = remote.client.desktop(&remote.session, request.payload())?;
        match crate::desktop_rpc::decode_value(answer)? {
            crate::desktop_rpc::DesktopRecord::Response(response) => Ok(response),
            crate::desktop_rpc::DesktopRecord::Event(_) => {
                Err(anyhow!("the server answered /desktop with an event"))
            }
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
        // Another client answered this dialog first: the server kept that
        // answer, and this one's dialog closes with a note.
        Err(error) if kind == "extension_ui_response" && error.status == Some(409) => {
            if let Some(id) = id {
                let _ = tx.send_blocking(Incoming::DialogResolved { id, refused: true });
            }
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
                    Step::DialogResolved(id) => {
                        let resolved = Incoming::DialogResolved { id, refused: false };
                        if self.tx.send_blocking(resolved).is_err() {
                            return Ended::Closed;
                        }
                    }
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

/// `?key=value&…` with each value percent-encoded (paths, queries).
fn query(pairs: &[(&str, &str)]) -> String {
    let mut out = String::new();
    for (n, (key, value)) in pairs.iter().enumerate() {
        out.push(if n == 0 { '?' } else { '&' });
        out.push_str(key);
        out.push('=');
        for byte in value.bytes() {
            if byte.is_ascii_alphanumeric() || b"-_.~/".contains(&byte) {
                out.push(byte as char);
            } else {
                out.push_str(&format!("%{byte:02X}"));
            }
        }
    }
    out
}

/// One of the server's own SSE streams (a folder's changes, a terminal):
/// its events in order, ending when the connection does.
struct Events {
    body: ureq::BodyReader<'static>,
    sse: SseParser,
    queue: std::collections::VecDeque<gentle_core::serve::SseEvent>,
    buf: Vec<u8>,
}

impl Events {
    fn new(response: ureq::http::Response<ureq::Body>) -> Self {
        Self {
            body: response.into_body().into_with_config().reader(),
            sse: SseParser::default(),
            queue: std::collections::VecDeque::new(),
            buf: vec![0; 64 * 1024],
        }
    }

    /// The next event; `None` when the stream ended, went silent for
    /// [`SILENCE`], or `stop` was set (checked whenever bytes arrive, a
    /// heartbeat included, so within 20 s).
    fn next(&mut self, stop: &AtomicBool) -> Option<gentle_core::serve::SseEvent> {
        loop {
            if stop.load(Ordering::Acquire) {
                return None;
            }
            if let Some(event) = self.queue.pop_front() {
                return Some(event);
            }
            let n = match self.body.read(&mut self.buf) {
                Ok(0) | Err(_) => return None,
                Ok(n) => n,
            };
            self.queue.extend(self.sse.push(&self.buf[..n]));
        }
    }
}
