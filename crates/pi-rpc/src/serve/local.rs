//! Forwarded pages opened on this computer (Gentle Shell's
//! `lib/serve-forward.ts`). A server's forward speaks TLS with its pinned,
//! self-signed certificate, which the system's web view and browser don't
//! know, and through a relay its link names the relay's host. So the app
//! opens each forward through a proxy of its own: a listener on
//! `127.0.0.1:<random>` that carries every connection to the forward's
//! listener over the same pinned TLS (and relay) the API uses. The page
//! opens at `http://127.0.0.1:<local>/<path>?gs_ticket=…`; its cookies are
//! that origin's.
//!
//! The forward checks each request's `Host` (the server's names for its
//! port) and `Origin` (the forward's own), so the proxy reads HTTP/1.1
//! heads, keeping connections alive: a request's `Host` becomes the
//! forward link's `host:port`, and an `Origin` or `Referer` of the local
//! page becomes the forward's; a response's `Location` back at the forward
//! comes back here. Bodies pass as they are (chunked or not), streams (SSE)
//! as they come, and after a WebSocket's `101` the bytes go both ways
//! untouched. Requests naming another `Host` are refused, so a page
//! elsewhere can't reach the forward through this computer's loopback
//! (DNS rebinding).
//!
//! One proxy per forward, reused while it runs. It closes when the last
//! web view showing it lets go ([`hold_local_page`]), when the forward
//! closed (its listener, or the relay, says nothing is there), and after
//! [`IDLE`] unheld with no connection and no request, as the server closes
//! its forward.

use std::collections::HashMap;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, LazyLock, Mutex, MutexGuard, RwLock, mpsc};
use std::time::{Duration, Instant};

use super::tls::{self, Link};
use super::{Forward, ServeClient};

/// An unheld proxy with no connection and no request for this long closes
/// (the server closes its forward after as long).
const IDLE: Duration = Duration::from_secs(12 * 60 * 60);
/// How often unheld proxies are looked at.
const SWEEP: Duration = Duration::from_secs(60);
/// Connecting to the forward, its TLS handshake included.
const CONNECT: Duration = Duration::from_secs(10);
/// The longest head (start line and headers) read.
const HEAD_LIMIT: usize = 64 * 1024;
/// The longest chunk-size or trailer line read.
const LINE_LIMIT: usize = 8 * 1024;
/// Open connections per proxy (the forward allows 64).
const MAX_CONNECTIONS: usize = 64;

/// Proxies by their local port.
static PROXIES: LazyLock<Mutex<HashMap<u16, Arc<Proxy>>>> = LazyLock::new(Mutex::default);
/// The thread closing idle proxies runs.
static SWEEPING: AtomicBool = AtomicBool::new(false);

fn proxies() -> MutexGuard<'static, HashMap<u16, Arc<Proxy>>> {
    PROXIES.lock().unwrap_or_else(|e| e.into_inner())
}

impl ServeClient {
    /// The address that opens `forward` on this computer: its link as it
    /// is for a plain-HTTP server, else through a proxy on this computer's
    /// loopback ([`self`]) reached as this client reaches its server. The
    /// forward's proxy is reused, so a page opened again keeps its cookies.
    pub fn local_page(&self, forward: &Forward) -> Result<String, String> {
        let (origin, rest) =
            super::services::split_link(&forward.url).ok_or("The forward's link has no host.")?;
        let base = self.url();
        let relay = base.strip_prefix(gentle_core::serve::RELAY_PREFIX);
        let (scheme, authority) = origin.split_once("://").unwrap_or_default();
        if scheme == "http" && relay.is_none() {
            return Ok(forward.url.clone());
        }
        // Through a relay the link names the relay's host and the forward's
        // port: the relay is asked for that port.
        let port = authority
            .rsplit_once(':')
            .and_then(|(_, port)| port.parse::<u16>().ok())
            .ok_or("The forward's link has no port.")?;
        let upstream = Upstream {
            link: self.trust().link(relay.map(|_| port)),
            dial: relay.unwrap_or(authority).to_string(),
            authority: authority.to_string(),
            relayed: relay.is_some(),
        };
        let key = format!(
            "{}/{}",
            self.trust().fingerprint.as_deref().unwrap_or_default(),
            forward.id
        );
        let local = open(key, upstream)
            .map_err(|e| format!("Couldn't open the forwarded page on this computer: {e}"))?;
        let slash = if rest.starts_with('/') { "" } else { "/" };
        Ok(format!("http://127.0.0.1:{local}{slash}{rest}"))
    }
}

/// A web view showing a page through a proxy ([`hold_local_page`]): while
/// one is held the proxy stays open; dropping the last closes it.
pub struct LocalHold(Arc<Proxy>);

impl Drop for LocalHold {
    fn drop(&mut self) {
        let last = {
            let mut state = self.0.state();
            state.holds = state.holds.saturating_sub(1);
            state.holds == 0
        };
        if last {
            self.0.close();
        }
    }
}

impl std::fmt::Debug for LocalHold {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("LocalHold").field(&self.0.port).finish()
    }
}

/// A hold on the proxy serving `url` ([`ServeClient::local_page`]), for a
/// web view that shows it; `None` for any other address.
pub fn hold_local_page(url: &str) -> Option<LocalHold> {
    let port = local_port(url)?;
    let proxy = proxies().get(&port).cloned()?;
    {
        let mut state = proxy.state();
        if state.closed {
            return None;
        }
        state.holds += 1;
    }
    Some(LocalHold(proxy))
}

/// The port of an `http://127.0.0.1:<port>` address.
fn local_port(url: &str) -> Option<u16> {
    let rest = url.strip_prefix("http://127.0.0.1:")?;
    let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    rest[..end].parse().ok()
}

/// Where a proxy's connections go.
struct Upstream {
    link: Arc<Link>,
    /// `host:port` connected to: the forward's listener, or the relay.
    dial: String,
    /// The forward link's `host:port`: every request's `Host`.
    authority: String,
    relayed: bool,
}

struct Proxy {
    key: String,
    port: u16,
    upstream: RwLock<Arc<Upstream>>,
    state: Mutex<ProxyState>,
}

#[derive(Default)]
struct ProxyState {
    closed: bool,
    holds: usize,
    last_used: Option<Instant>,
    next: u64,
    /// Each open connection's sockets (this computer's side, then the
    /// forward's), shut down when the proxy closes.
    connections: HashMap<u64, Vec<TcpStream>>,
}

impl Proxy {
    fn state(&self) -> MutexGuard<'_, ProxyState> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn upstream(&self) -> Arc<Upstream> {
        self.upstream
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    fn touch(&self) {
        self.state().last_used = Some(Instant::now());
    }

    fn closed(&self) -> bool {
        self.state().closed
    }

    /// Closes the listener and every connection, and forgets the proxy.
    fn close(&self) {
        let connections = {
            let mut state = self.state();
            if state.closed {
                return;
            }
            state.closed = true;
            std::mem::take(&mut state.connections)
        };
        for socket in connections.values().flatten() {
            let _ = socket.shutdown(Shutdown::Both);
        }
        {
            let mut proxies = proxies();
            if proxies
                .get(&self.port)
                .is_some_and(|p| std::ptr::eq(Arc::as_ptr(p), self))
            {
                proxies.remove(&self.port);
            }
        }
        // Wakes the accepting thread, which sees it closed.
        let _ =
            TcpStream::connect_timeout(&([127, 0, 0, 1], self.port).into(), Duration::from_secs(1));
    }

    /// Takes a new connection's socket; `None` past the limit or closed.
    fn admit(&self, client: &TcpStream) -> Option<u64> {
        let mut state = self.state();
        if state.closed || state.connections.len() >= MAX_CONNECTIONS {
            return None;
        }
        let id = state.next;
        state.next += 1;
        state.connections.insert(id, vec![client.try_clone().ok()?]);
        Some(id)
    }

    /// Adds the forward's socket to connection `id`; false once closed.
    fn adopt(&self, id: u64, socket: &TcpStream) -> bool {
        let mut state = self.state();
        match (
            state.closed,
            state.connections.get_mut(&id),
            socket.try_clone(),
        ) {
            (false, Some(sockets), Ok(socket)) => {
                sockets.push(socket);
                true
            }
            _ => false,
        }
    }

    /// Connection `id` ended: its sockets shut down, it counts no more.
    fn release(&self, id: u64) {
        let sockets = {
            let mut state = self.state();
            state.last_used = Some(Instant::now());
            state.connections.remove(&id)
        };
        for socket in sockets.into_iter().flatten() {
            let _ = socket.shutdown(Shutdown::Both);
        }
    }

    /// Unheld, no connection, nothing for [`IDLE`] as of `now`.
    fn idle(&self, now: Instant) -> bool {
        let state = self.state();
        state.holds == 0
            && state.connections.is_empty()
            && state
                .last_used
                .is_none_or(|used| now.saturating_duration_since(used) >= IDLE)
    }
}

/// The proxy for `key` (a server's forward), started or reused with
/// `upstream`: its local port.
fn open(key: String, upstream: Upstream) -> io::Result<u16> {
    let mut all = proxies();
    if let Some(proxy) = all.values().find(|p| p.key == key && !p.closed()) {
        // The server may answer elsewhere now, or be paired again.
        *proxy.upstream.write().unwrap_or_else(|e| e.into_inner()) = Arc::new(upstream);
        proxy.touch();
        return Ok(proxy.port);
    }
    let listener = TcpListener::bind("127.0.0.1:0")?;
    let port = listener.local_addr()?.port();
    let proxy = Arc::new(Proxy {
        key,
        port,
        upstream: RwLock::new(Arc::new(upstream)),
        state: Mutex::new(ProxyState {
            last_used: Some(Instant::now()),
            ..ProxyState::default()
        }),
    });
    all.insert(port, proxy.clone());
    if !SWEEPING.swap(true, Ordering::AcqRel) {
        std::thread::Builder::new()
            .name("gs-local-pages".into())
            .spawn(sweeper)?;
    }
    drop(all);
    std::thread::Builder::new()
        .name(format!("gs-local-page-{port}"))
        .spawn(move || accept(proxy, listener))?;
    Ok(port)
}

/// Closes idle proxies until none is left.
fn sweeper() {
    loop {
        std::thread::sleep(SWEEP);
        sweep(Instant::now());
        let all = proxies();
        if all.is_empty() {
            SWEEPING.store(false, Ordering::Release);
            return;
        }
    }
}

/// Closes the proxies idle as of `now`.
fn sweep(now: Instant) {
    let idle: Vec<Arc<Proxy>> = proxies()
        .values()
        .filter(|p| p.idle(now))
        .cloned()
        .collect();
    for proxy in idle {
        proxy.close();
    }
}

fn accept(proxy: Arc<Proxy>, listener: TcpListener) {
    for client in listener.incoming() {
        if proxy.closed() {
            return;
        }
        let Ok(client) = client else {
            // Out of descriptors, say: don't spin.
            std::thread::sleep(Duration::from_millis(50));
            continue;
        };
        let Some(id) = proxy.admit(&client) else {
            continue;
        };
        let proxy = proxy.clone();
        let spawned = std::thread::Builder::new()
            .name(format!("gs-local-page-{}", proxy.port))
            .spawn({
                let proxy = proxy.clone();
                move || {
                    let _ = connection(&proxy, id, client);
                    proxy.release(id);
                }
            });
        if spawned.is_err() {
            proxy.release(id);
        }
    }
}

// --- One connection ------------------------------------------------------------------

/// What the answer to a request needs to know of it.
struct Asked {
    /// A `HEAD`: its answer has no body.
    head: bool,
    /// It asked to switch protocols (a WebSocket): after the answer, bytes.
    upgrade: bool,
    /// The `Host` this computer's side used, for redirects back.
    host: String,
}

/// Requests from this computer go to the forward, rewritten, in this
/// thread; answers come back in another ([`answers`]).
fn connection(proxy: &Arc<Proxy>, id: u64, client: TcpStream) -> io::Result<()> {
    client.set_nodelay(true)?;
    let mut from_client = BufReader::new(client.try_clone()?);
    let mut to_client = client;
    let Some(mut head) = Head::read(&mut from_client)? else {
        return Ok(());
    };
    let Some(mut host) = local_host(&head, proxy.port) else {
        return refuse(
            &mut to_client,
            421,
            "Misdirected Request",
            "Unexpected Host header",
        );
    };
    let upstream = proxy.upstream();
    let tunnel = match upstream.link.open(&upstream.dial, CONNECT) {
        Ok(tunnel) => tunnel,
        Err(e) if tls::nothing_there(&e, upstream.relayed) => {
            refuse(
                &mut to_client,
                502,
                "Bad Gateway",
                "This page's forward closed. Open the page again from Gentle Shell.",
            )?;
            proxy.close();
            return Ok(());
        }
        Err(e) => {
            let why = format!("Couldn't reach the forwarded page: {e}");
            return refuse(&mut to_client, 502, "Bad Gateway", &why);
        }
    };
    let (from_forward, mut to_forward, socket) = tunnel.split()?;
    if !proxy.adopt(id, &socket) {
        return Ok(());
    }
    let (asked, answers_for) = mpsc::channel::<Asked>();
    let authority = upstream.authority.clone();
    let answering = std::thread::Builder::new()
        .name(format!("gs-local-page-{}", proxy.port))
        .spawn({
            let proxy = proxy.clone();
            let to_client = to_client.try_clone()?;
            move || {
                let _ = answers(
                    &upstream.authority,
                    BufReader::new(from_forward),
                    to_client,
                    answers_for,
                );
                proxy.release(id);
            }
        })?;
    let result = (|| -> io::Result<()> {
        loop {
            proxy.touch();
            let upgrade = head.upgrade();
            let body = head.request_body()?;
            let next = Asked {
                head: head.method().eq_ignore_ascii_case("HEAD"),
                upgrade,
                host: host.clone(),
            };
            rewrite_request(&mut head, &host, &authority);
            if asked.send(next).is_err() {
                return Ok(());
            }
            to_forward.write_all(&head.bytes())?;
            if upgrade {
                io::copy(&mut from_client, &mut to_forward)?;
                return Ok(());
            }
            if !copy_body(body, &mut from_client, &mut to_forward)? {
                return Ok(());
            }
            head = match Head::read(&mut from_client)? {
                Some(next) => next,
                None => return Ok(()),
            };
            // Another Host on a connection already open: nothing more.
            host = match local_host(&head, proxy.port) {
                Some(host) => host,
                None => return Ok(()),
            };
        }
    })();
    proxy.release(id);
    let _ = answering.join();
    result
}

/// Answers from the forward to this computer, in the order asked: heads
/// rewritten, bodies as they come.
fn answers(
    authority: &str,
    mut from_forward: BufReader<tls::TunnelReader>,
    mut to_client: TcpStream,
    asked: mpsc::Receiver<Asked>,
) -> io::Result<()> {
    loop {
        let Some(mut head) = Head::read(&mut from_forward)? else {
            return Ok(());
        };
        let status = head.status().ok_or_else(|| bad("not an HTTP answer"))?;
        // A 100 Continue comes before the answer of the same request.
        if (100..200).contains(&status) && status != 101 {
            to_client.write_all(&head.bytes())?;
            continue;
        }
        let Ok(request) = asked.recv() else {
            return Ok(());
        };
        rewrite_answer(&mut head, authority, &request.host);
        to_client.write_all(&head.bytes())?;
        if status == 101 || request.upgrade {
            io::copy(&mut from_forward, &mut to_client)?;
            return Ok(());
        }
        let body = head.answer_body(status, request.head)?;
        if !copy_body(body, &mut from_forward, &mut to_client)? {
            return Ok(());
        }
    }
}

/// The `Host` of a request to this proxy, when it names this computer's
/// loopback and the proxy's port.
fn local_host(head: &Head, port: u16) -> Option<String> {
    let host = head.header("host")?.trim();
    let ok = [format!("127.0.0.1:{port}"), format!("localhost:{port}")]
        .iter()
        .any(|own| host.eq_ignore_ascii_case(own));
    ok.then(|| host.to_ascii_lowercase())
}

/// A request's `Host` becomes the forward's; this page's own `Origin` and
/// `Referer` become the forward's, which it takes for its own (another
/// origin stays, and the forward refuses it).
fn rewrite_request(head: &mut Head, host: &str, authority: &str) {
    let own = format!("http://{host}");
    let theirs = format!("https://{authority}");
    for (name, value) in &mut head.headers {
        let Ok(text) = std::str::from_utf8(value) else {
            continue;
        };
        if name.eq_ignore_ascii_case("host") {
            *value = authority.as_bytes().to_vec();
        } else if name.eq_ignore_ascii_case("origin") {
            if text.eq_ignore_ascii_case(&own) {
                *value = theirs.as_bytes().to_vec();
            }
        } else if name.eq_ignore_ascii_case("referer")
            && let Some(rest) = strip_origin(text, &own)
        {
            *value = format!("{theirs}{rest}").into_bytes();
        }
    }
}

/// An answer's redirect to the forward's own address comes back here.
fn rewrite_answer(head: &mut Head, authority: &str, host: &str) {
    for (name, value) in &mut head.headers {
        if !(name.eq_ignore_ascii_case("location") || name.eq_ignore_ascii_case("content-location"))
        {
            continue;
        }
        let Ok(text) = std::str::from_utf8(value) else {
            continue;
        };
        let rest = strip_origin(text, &format!("https://{authority}"))
            .or_else(|| strip_origin(text, &format!("http://{authority}")));
        if let Some(rest) = rest {
            *value = format!("http://{host}{rest}").into_bytes();
        }
    }
}

/// What follows `origin` in `url` (a path, query or fragment, or
/// nothing), when `url` is on that origin.
fn strip_origin<'a>(url: &'a str, origin: &str) -> Option<&'a str> {
    let head = url.get(..origin.len())?;
    let rest = &url[origin.len()..];
    (head.eq_ignore_ascii_case(origin) && (rest.is_empty() || rest.starts_with(['/', '?', '#'])))
        .then_some(rest)
}

/// This proxy's own answer, and the end of the connection.
fn refuse(to: &mut TcpStream, status: u16, reason: &str, message: &str) -> io::Result<()> {
    let answer = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: text/plain; charset=utf-8\r\nContent-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n{message}",
        message.len()
    );
    to.write_all(answer.as_bytes())?;
    to.flush()
}

fn bad(what: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, what.to_string())
}

// --- HTTP/1.1 framing ----------------------------------------------------------------

/// A message's start line and headers, as sent (values are bytes: a few
/// servers send Latin-1 in them).
#[derive(Debug, Clone, PartialEq, Eq)]
struct Head {
    start: Vec<u8>,
    headers: Vec<(String, Vec<u8>)>,
}

/// How a message's body ends.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Body {
    None,
    Length(u64),
    Chunked,
    /// When the connection ends.
    ToEnd,
}

impl Head {
    /// The next head on `from`; `None` when the connection ended between
    /// messages.
    fn read(from: &mut impl BufRead) -> io::Result<Option<Head>> {
        let mut start = None;
        let mut headers = Vec::new();
        let mut total = 0;
        loop {
            let mut line = Vec::new();
            let limit = (HEAD_LIMIT - total) as u64 + 1;
            let n = from.by_ref().take(limit).read_until(b'\n', &mut line)?;
            if n == 0 {
                return match (&start, total) {
                    (None, 0) => Ok(None),
                    _ => Err(io::ErrorKind::UnexpectedEof.into()),
                };
            }
            total += n;
            if total > HEAD_LIMIT || line.last() != Some(&b'\n') {
                return Err(bad("head too long"));
            }
            line.pop();
            if line.last() == Some(&b'\r') {
                line.pop();
            }
            match &start {
                // Empty lines before a request are allowed (RFC 9112 §2.2).
                None if line.is_empty() => continue,
                None => start = Some(line),
                Some(_) if line.is_empty() => break,
                Some(_) => {
                    let colon = line
                        .iter()
                        .position(|b| *b == b':')
                        .ok_or_else(|| bad("bad header"))?;
                    let name = std::str::from_utf8(&line[..colon])
                        .ok()
                        .filter(|n| !n.is_empty() && !n.contains([' ', '\t']))
                        .ok_or_else(|| bad("bad header"))?;
                    headers.push((name.to_string(), line[colon + 1..].trim_ascii().to_vec()));
                }
            }
        }
        Ok(start.map(|start| Head { start, headers }))
    }

    fn bytes(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(512);
        out.extend_from_slice(&self.start);
        out.extend_from_slice(b"\r\n");
        for (name, value) in &self.headers {
            out.extend_from_slice(name.as_bytes());
            out.extend_from_slice(b": ");
            out.extend_from_slice(value);
            out.extend_from_slice(b"\r\n");
        }
        out.extend_from_slice(b"\r\n");
        out
    }

    /// The first value of `name`, when it is text.
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(n, _)| n.eq_ignore_ascii_case(name))
            .and_then(|(_, v)| std::str::from_utf8(v).ok())
    }

    /// Every comma-separated token of every `name` header, lowercase.
    fn tokens(&self, name: &str) -> Vec<String> {
        self.headers
            .iter()
            .filter(|(n, _)| n.eq_ignore_ascii_case(name))
            .filter_map(|(_, v)| std::str::from_utf8(v).ok())
            .flat_map(|v| v.split(','))
            .map(|t| t.trim().to_ascii_lowercase())
            .filter(|t| !t.is_empty())
            .collect()
    }

    /// The start line's first word: a request's method.
    fn method(&self) -> &str {
        let end = self
            .start
            .iter()
            .position(|b| *b == b' ')
            .unwrap_or(self.start.len());
        std::str::from_utf8(&self.start[..end]).unwrap_or_default()
    }

    /// An answer's status.
    fn status(&self) -> Option<u16> {
        let mut parts = self.start.split(|b| *b == b' ');
        parts.next().filter(|v| v.starts_with(b"HTTP/"))?;
        std::str::from_utf8(parts.next()?).ok()?.parse().ok()
    }

    /// A request asking to switch protocols, or a tunnel (`CONNECT`).
    fn upgrade(&self) -> bool {
        self.method().eq_ignore_ascii_case("CONNECT")
            || (self.header("upgrade").is_some()
                && self.tokens("connection").iter().any(|t| t == "upgrade"))
    }

    fn length(&self) -> io::Result<Option<u64>> {
        let mut found = None;
        for (_, value) in self
            .headers
            .iter()
            .filter(|(n, _)| n.eq_ignore_ascii_case("content-length"))
        {
            let value = std::str::from_utf8(value).map_err(|_| bad("bad length"))?;
            for part in value.split(',') {
                let n: u64 = part.trim().parse().map_err(|_| bad("bad length"))?;
                if found.is_some_and(|f| f != n) {
                    return Err(bad("two lengths"));
                }
                found = Some(n);
            }
        }
        Ok(found)
    }

    fn chunked(&self) -> Option<bool> {
        let codings = self.tokens("transfer-encoding");
        (!codings.is_empty()).then(|| codings.last().is_some_and(|c| c == "chunked"))
    }

    /// RFC 9112 §6.3 for a request: chunked, a length, or none.
    fn request_body(&self) -> io::Result<Body> {
        match self.chunked() {
            Some(true) => Ok(Body::Chunked),
            Some(false) => Err(bad("unknown transfer coding")),
            None => Ok(self.length()?.map_or(Body::None, Body::Length)),
        }
    }

    /// RFC 9112 §6.3 for an answer to a request (`head`: a HEAD).
    fn answer_body(&self, status: u16, head: bool) -> io::Result<Body> {
        if head || (100..200).contains(&status) || status == 204 || status == 304 {
            return Ok(Body::None);
        }
        match self.chunked() {
            Some(true) => Ok(Body::Chunked),
            Some(false) => Ok(Body::ToEnd),
            None => Ok(self.length()?.map_or(Body::ToEnd, Body::Length)),
        }
    }
}

/// Passes one body along as it comes; false when it ended with the
/// connection.
fn copy_body(body: Body, from: &mut impl BufRead, to: &mut impl Write) -> io::Result<bool> {
    match body {
        Body::None => Ok(true),
        Body::Length(n) => {
            if io::copy(&mut from.by_ref().take(n), to)? < n {
                return Err(io::ErrorKind::UnexpectedEof.into());
            }
            Ok(true)
        }
        Body::Chunked => {
            loop {
                let line = read_line(from)?;
                to.write_all(&line)?;
                let size = std::str::from_utf8(&line)
                    .ok()
                    .map(|l| l.split(';').next().unwrap_or_default().trim())
                    .and_then(|hex| u64::from_str_radix(hex, 16).ok())
                    .ok_or_else(|| bad("bad chunk size"))?;
                if size == 0 {
                    // Trailers, then the empty line.
                    loop {
                        let line = read_line(from)?;
                        to.write_all(&line)?;
                        if line == b"\r\n" || line == b"\n" {
                            return Ok(true);
                        }
                    }
                }
                if io::copy(&mut from.by_ref().take(size), to)? < size {
                    return Err(io::ErrorKind::UnexpectedEof.into());
                }
                let end = read_line(from)?;
                to.write_all(&end)?;
            }
        }
        Body::ToEnd => {
            io::copy(from, to)?;
            Ok(false)
        }
    }
}

/// One line, its line ending included.
fn read_line(from: &mut impl BufRead) -> io::Result<Vec<u8>> {
    let mut line = Vec::new();
    from.by_ref()
        .take(LINE_LIMIT as u64)
        .read_until(b'\n', &mut line)?;
    if line.last() != Some(&b'\n') {
        return Err(if line.len() >= LINE_LIMIT {
            bad("line too long")
        } else {
            io::ErrorKind::UnexpectedEof.into()
        });
    }
    Ok(line)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn head(text: &str) -> Head {
        Head::read(&mut text.as_bytes()).unwrap().unwrap()
    }

    #[test]
    fn a_request_names_the_forward_and_its_own_origin() {
        let mut request = head(
            "\r\nGET /a HTTP/1.1\r\nHost: 127.0.0.1:5000\r\nOrigin: http://127.0.0.1:5000\r\nReferer: http://127.0.0.1:5000/x?y\r\nX: 1\r\n\r\n",
        );
        assert_eq!(
            local_host(&request, 5000).as_deref(),
            Some("127.0.0.1:5000")
        );
        rewrite_request(&mut request, "127.0.0.1:5000", "10.0.0.2:8801");
        assert_eq!(
            String::from_utf8(request.bytes()).unwrap(),
            "GET /a HTTP/1.1\r\nHost: 10.0.0.2:8801\r\nOrigin: https://10.0.0.2:8801\r\nReferer: https://10.0.0.2:8801/x?y\r\nX: 1\r\n\r\n"
        );

        // Another page's origin stays (the forward refuses it); a referer
        // elsewhere stays too.
        let mut other = head(
            "POST / HTTP/1.1\r\nHost: localhost:5000\r\nOrigin: http://127.0.0.1:50001\r\nReferer: http://127.0.0.1:50001/\r\n\r\n",
        );
        rewrite_request(&mut other, "localhost:5000", "h:1");
        assert_eq!(other.header("origin"), Some("http://127.0.0.1:50001"));
        assert_eq!(other.header("referer"), Some("http://127.0.0.1:50001/"));

        // A name or port other than the proxy's is refused.
        for host in ["evil.example:5000", "127.0.0.1:5001", "127.0.0.1"] {
            let request = head(&format!("GET / HTTP/1.1\r\nHost: {host}\r\n\r\n"));
            assert_eq!(local_host(&request, 5000), None, "{host}");
        }
        assert_eq!(local_host(&head("GET / HTTP/1.1\r\n\r\n"), 5000), None);
    }

    #[test]
    fn a_redirect_to_the_forward_comes_back_here() {
        let mut answer = head(
            "HTTP/1.1 302 Found\r\nLocation: https://10.0.0.2:8801/next?a#b\r\nContent-Location: http://10.0.0.2:8801\r\n\r\n",
        );
        rewrite_answer(&mut answer, "10.0.0.2:8801", "127.0.0.1:5000");
        assert_eq!(
            answer.header("location"),
            Some("http://127.0.0.1:5000/next?a#b")
        );
        assert_eq!(
            answer.header("content-location"),
            Some("http://127.0.0.1:5000")
        );
        for elsewhere in [
            "https://10.0.0.2:88010/",
            "/relative",
            "https://example.com/",
        ] {
            let mut answer = head(&format!(
                "HTTP/1.1 302 Found\r\nLocation: {elsewhere}\r\n\r\n"
            ));
            rewrite_answer(&mut answer, "10.0.0.2:8801", "127.0.0.1:5000");
            assert_eq!(answer.header("location"), Some(elsewhere));
        }
    }

    #[test]
    fn bodies_end_where_http_says() {
        let get = head("GET / HTTP/1.1\r\nHost: h\r\n\r\n");
        assert_eq!(get.request_body().unwrap(), Body::None);
        let post = head("POST / HTTP/1.1\r\nContent-Length: 5\r\n\r\n");
        assert_eq!(post.request_body().unwrap(), Body::Length(5));
        let chunked = head("POST / HTTP/1.1\r\nTransfer-Encoding: gzip, chunked\r\n\r\n");
        assert_eq!(chunked.request_body().unwrap(), Body::Chunked);
        assert!(
            head("POST / HTTP/1.1\r\nTransfer-Encoding: gzip\r\n\r\n")
                .request_body()
                .is_err()
        );
        assert!(
            head("POST / HTTP/1.1\r\nContent-Length: 1\r\nContent-Length: 2\r\n\r\n")
                .request_body()
                .is_err()
        );

        let ok = head("HTTP/1.1 200 OK\r\nContent-Length: 3\r\n\r\n");
        assert_eq!(ok.status(), Some(200));
        assert_eq!(ok.answer_body(200, false).unwrap(), Body::Length(3));
        assert_eq!(ok.answer_body(200, true).unwrap(), Body::None);
        let stream = head("HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\n\r\n");
        assert_eq!(stream.answer_body(200, false).unwrap(), Body::ToEnd);
        assert_eq!(stream.answer_body(304, false).unwrap(), Body::None);
        assert_eq!(stream.answer_body(204, false).unwrap(), Body::None);

        let ws =
            head("GET / HTTP/1.1\r\nConnection: keep-alive, Upgrade\r\nUpgrade: websocket\r\n\r\n");
        assert!(ws.upgrade());
        assert!(!get.upgrade());
    }

    #[test]
    fn a_chunked_body_passes_whole_and_stops_at_its_end() {
        let body = "4;x=y\r\nWiki\r\n5\r\npedia\r\n0\r\nT: 1\r\n\r\nGET /next";
        let mut from = body.as_bytes();
        let mut to = Vec::new();
        assert!(copy_body(Body::Chunked, &mut from, &mut to).unwrap());
        assert_eq!(to, &body.as_bytes()[..body.len() - 9]);
        assert_eq!(from, b"GET /next");

        let mut from = &b"3\r\nab"[..];
        assert!(copy_body(Body::Chunked, &mut from, &mut Vec::new()).is_err());
        let mut from = &b"zz\r\n"[..];
        assert!(copy_body(Body::Chunked, &mut from, &mut Vec::new()).is_err());
    }

    #[test]
    fn a_head_too_long_is_refused_and_an_end_between_messages_is_none() {
        assert_eq!(Head::read(&mut &b""[..]).unwrap(), None);
        assert!(Head::read(&mut &b"GET / HTTP/1.1\r\nHost"[..]).is_err());
        let long = format!("GET / HTTP/1.1\r\nX: {}\r\n\r\n", "a".repeat(HEAD_LIMIT));
        assert!(Head::read(&mut long.as_bytes()).is_err());
    }

    /// The tests that open proxies share the registry (a sweep closes
    /// every idle one): one at a time.
    static REGISTRY: Mutex<()> = Mutex::new(());

    fn test_proxy(key: &str) -> u16 {
        let link = Link::new(tls::Pin::Exact("fp".into()), None, None);
        let upstream = Upstream {
            link,
            dial: "127.0.0.1:9".into(),
            authority: "127.0.0.1:9".into(),
            relayed: false,
        };
        open(key.into(), upstream).unwrap()
    }

    fn listening(port: u16) -> bool {
        // The accepting thread closes the listener just after `close`.
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let open =
                TcpStream::connect(("127.0.0.1", port)).is_ok() && proxies().contains_key(&port);
            if open || Instant::now() > deadline {
                return open;
            }
            if !proxies().contains_key(&port) {
                return false;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    #[test]
    fn a_proxy_is_reused_held_and_closed_by_its_last_holder() {
        let _one = REGISTRY.lock().unwrap_or_else(|e| e.into_inner());
        let port = test_proxy("t/fwd_held");
        assert_eq!(test_proxy("t/fwd_held"), port, "one proxy per forward");
        let url = format!("http://127.0.0.1:{port}/x?gs_ticket=t");
        let first = hold_local_page(&url).unwrap();
        let second = hold_local_page(&format!("http://127.0.0.1:{port}")).unwrap();
        assert!(hold_local_page("http://127.0.0.1:1/").is_none());
        assert!(hold_local_page("https://example.com/").is_none());
        drop(first);
        assert!(listening(port));
        drop(second);
        assert!(!listening(port));
        assert!(hold_local_page(&url).is_none());
        // Opened again: a new proxy.
        let again = test_proxy("t/fwd_held");
        assert!(listening(again));
        drop(hold_local_page(&format!("http://127.0.0.1:{again}/")));
        assert!(!listening(again));
    }

    #[test]
    fn an_unheld_proxy_closes_once_idle() {
        let _one = REGISTRY.lock().unwrap_or_else(|e| e.into_inner());
        let port = test_proxy("t/fwd_idle");
        let held = test_proxy("t/fwd_idle_held");
        let hold = hold_local_page(&format!("http://127.0.0.1:{held}/")).unwrap();
        sweep(Instant::now());
        assert!(proxies().contains_key(&port), "not idle yet");
        sweep(Instant::now() + IDLE);
        assert!(!proxies().contains_key(&port));
        assert!(proxies().contains_key(&held), "a held one stays");
        drop(hold);
        assert!(!proxies().contains_key(&held));
    }
}
