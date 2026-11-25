//! Gentle Shell servers (`gentle-shell serve`, protocol
//! `gentle-shell.serve/v1`): the rules a client of one follows, without the
//! network. The server address a user types ([`normalize_url`]), the
//! codes `gentle-shell serve share` prints ([`read_code`]: the short pairing
//! code, or the connection code and how the server it names is found,
//! [`decode_connection_code`], [`search`]), finding servers on the local
//! network by UDP broadcast ([`discovery_targets`], [`read_announce`],
//! [`pick`]), what its errors mean to them ([`error_text`]), its
//! Server-Sent Events framing ([`SseParser`]), the stream's cursor and what
//! each event asks of the client ([`Stream`]), and the Pi responses the
//! stream repeats under the server's own ids ([`RpcIds`]).

use std::collections::{HashMap, HashSet, VecDeque};
use std::net::{IpAddr, Ipv4Addr, SocketAddr};

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// The address Configuration suggests: `gentle-shell serve --listen :8787`.
pub const DEFAULT_URL: &str = "http://127.0.0.1:8787";

/// The ids the server sends Pi commands under (`serve-rpc-N`): several
/// clients share a session, so their own ids could collide.
const SERVER_RPC_ID: &str = "serve-rpc-";

/// The dialogs a server keeps pending until answered (`select`, `confirm`,
/// `input`, `editor`); a reconnect may carry them again as `dialog` events.
const DIALOGS: [&str; 4] = ["select", "confirm", "input", "editor"];

/// How many stream responses nobody claimed yet are kept, in case their
/// `202 pending` answer is still on its way.
const UNCLAIMED: usize = 64;

/// How many dialog ids are remembered, so a reconnect doesn't show one twice.
const SEEN_DIALOGS: usize = 256;

/// A server session an agent tab is attached to, saved with the tab so the
/// next launch reattaches to it: the server's address and its session id
/// (`ses_…`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServerSession {
    pub url: String,
    pub id: String,
}

/// `input` as the base URL requests are made against: `http://host[:port]`,
/// the host lowercased and any trailing `/` dropped. A bare `host:port` gets
/// `http://`. The server speaks plain HTTP (no TLS in phase 1) and serves
/// from its root, so `https://`, a path, a query or credentials are refused
/// with what to type instead.
pub fn normalize_url(input: &str) -> Result<String, String> {
    let input = input.trim();
    if input.is_empty() {
        return Err("Enter the server's address, like http://127.0.0.1:8787.".into());
    }
    let rest = match input.split_once("://") {
        Some((scheme, rest)) if scheme.eq_ignore_ascii_case("http") => rest,
        Some((scheme, _)) if scheme.eq_ignore_ascii_case("https") => {
            return Err(
                "Gentle Shell servers speak plain HTTP: use http://, through an SSH \
                 tunnel or a network you trust."
                    .into(),
            );
        }
        Some((scheme, _)) => return Err(format!("Use http://, not {scheme}://.")),
        None => input,
    };
    let (authority, path) = match rest.find(['/', '?', '#']) {
        Some(at) => rest.split_at(at),
        None => (rest, ""),
    };
    if !(path.is_empty() || path == "/") {
        return Err("Use the server's address only (http://host:port), without a path.".into());
    }
    if authority.contains('@') {
        return Err(
            "The address takes no user name: the pairing code signs this device in.".into(),
        );
    }
    let (host, port) = if let Some(inner) = authority.strip_prefix('[') {
        let (host, after) = inner
            .split_once(']')
            .ok_or("The IPv6 address is missing its closing ].")?;
        if host.is_empty()
            || !host
                .chars()
                .all(|c| c.is_ascii_hexdigit() || c == ':' || c == '.')
        {
            return Err(format!("[{host}] isn't an IPv6 address."));
        }
        let port = match after {
            "" => None,
            _ => Some(after.strip_prefix(':').ok_or("Put the port after ]:.")?),
        };
        (format!("[{}]", host.to_ascii_lowercase()), port)
    } else {
        let (host, port) = match authority.rsplit_once(':') {
            Some((host, port)) => (host, Some(port)),
            None => (authority, None),
        };
        let valid = !host.is_empty()
            && host
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-');
        if !valid {
            return Err(format!("{authority} isn't a host name or address."));
        }
        (host.to_ascii_lowercase(), port)
    };
    match port {
        None => Ok(format!("http://{host}")),
        Some(port) => match port.parse::<u16>() {
            Ok(n) if n > 0 && port.chars().all(|c| c.is_ascii_digit()) => {
                Ok(format!("http://{host}:{n}"))
            }
            _ => Err(format!("{port} isn't a port number.")),
        },
    }
}

/// A pairing code as typed: trimmed; the server forgives case, dashes and
/// look-alike letters itself.
pub fn normalize_code(input: &str) -> Result<String, String> {
    let code = input.trim();
    if code.is_empty() {
        Err("Enter the code `gentle-shell serve pair` printed.".into())
    } else {
        Ok(code.to_string())
    }
}

/// What a connection code (`gentle-shell serve share`) starts with.
pub const CODE_PREFIX: &str = "gsc1.";

/// Where a server says who it is, without a token.
pub const IDENTITY_PATH: &str = "/.well-known/gentle-shell";

/// The schema of a server's identity answer.
pub const IDENTITY_SCHEMA: &str = "gentle-shell.serve.identity/v1";

/// The most addresses a connection code is looked for at.
pub const CODE_ADDRESSES: usize = 8;

/// The letters a pairing code is made of (Crockford base32: no I, L, O, U).
const PAIRING_ALPHABET: &str = "0123456789ABCDEFGHJKMNPQRSTVWXYZ";

/// A server and a pairing code for it, as one string to copy and paste:
/// `gsc1.` and the base64url (no padding) of
/// `{"v":1,"id","n","p","a":[…],"c"}`. A secret while the pairing code in
/// it is valid (5 minutes, once).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConnectionCode {
    /// The server's id (`srv_…`): only the server answering with it is
    /// sent the pairing code.
    pub server_id: String,
    /// The server's name (its host name), for the user.
    pub name: String,
    pub port: u16,
    /// The base URLs to look for the server at, in the code's order (its
    /// network addresses, then its host names), each `http://host:port`.
    pub urls: Vec<String>,
    /// `XXXX-XXXX`, as the server prints it.
    pub pairing_code: String,
}

/// Why a pasted connection code can't be used.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CodeError {
    /// It doesn't start with `gsc1.`.
    NotACode,
    /// Not base64url, not JSON, or not the code's shape: cut short or
    /// mangled when copied.
    Damaged,
    /// A version of the code this app doesn't read.
    Version(u64),
    NoServerId,
    /// No address the server could be looked for at.
    NoAddress,
    BadPort,
    BadPairingCode,
}

impl std::fmt::Display for CodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CodeError::NotACode => f.write_str(
                "That isn't a code Gentle Shell GPUI reads. On the other computer, run \
                 gentle-shell serve share and enter the 8-character code it shows (like \
                 ABCD-EFGH), or paste the long code that starts with gsc1.",
            ),
            CodeError::Damaged => f.write_str(
                "This connection code is damaged or cut short. Copy the whole code again.",
            ),
            CodeError::Version(v) if *v > 1 => f.write_str(
                "This connection code is from a newer Gentle Shell. Update Gentle Shell GPUI \
                 to use it.",
            ),
            CodeError::Version(_) => f.write_str("This connection code's version is unknown."),
            CodeError::NoServerId => f.write_str(
                "This connection code doesn't say which server it is for. Run gentle-shell \
                 serve share again and copy the new code.",
            ),
            CodeError::NoAddress => f.write_str(
                "This connection code has no network address to look for the server at. \
                 Connect the other computer to the network, then run gentle-shell serve \
                 share again.",
            ),
            CodeError::BadPort => f.write_str(
                "This connection code's port isn't valid. Run gentle-shell serve share again \
                 and copy the new code.",
            ),
            CodeError::BadPairingCode => f.write_str(
                "This connection code's pairing code isn't valid. Run gentle-shell serve \
                 share again and copy the new code.",
            ),
        }
    }
}

impl std::error::Error for CodeError {}

/// Whether `input` looks like a connection code (it may still be damaged).
pub fn is_connection_code(input: &str) -> bool {
    input.trim_start().starts_with(CODE_PREFIX)
}

/// What the one Code field holds: the short pairing code `gentle-shell
/// serve share` shows (its server is looked for on the network), or the
/// long connection code it prints as a fallback.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Entered {
    /// `XXXX-XXXX`, read as the server reads it.
    Short(String),
    Connection(ConnectionCode),
}

/// The Code field's text, read: a short pairing code ([`short_code`]), or
/// a connection code ([`decode_connection_code`]); anything else (empty
/// too) is [`CodeError::NotACode`].
pub fn read_code(input: &str) -> Result<Entered, CodeError> {
    if is_connection_code(input) {
        return decode_connection_code(input).map(Entered::Connection);
    }
    short_code(input)
        .map(Entered::Short)
        .ok_or(CodeError::NotACode)
}

/// A pasted connection code, checked. White space is ignored (a terminal
/// may wrap a long code), as is `=` padding. Addresses that aren't a host
/// name or an IP address are skipped, and only the first
/// [`CODE_ADDRESSES`] are kept.
pub fn decode_connection_code(input: &str) -> Result<ConnectionCode, CodeError> {
    use base64::Engine as _;
    let compact: String = input.chars().filter(|c| !c.is_whitespace()).collect();
    let payload = compact
        .strip_prefix(CODE_PREFIX)
        .ok_or(CodeError::NotACode)?;
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(payload.trim_end_matches('='))
        .map_err(|_| CodeError::Damaged)?;
    let value: Value = serde_json::from_slice(&bytes).map_err(|_| CodeError::Damaged)?;
    if !value.is_object() {
        return Err(CodeError::Damaged);
    }
    match value["v"].as_u64() {
        Some(1) => {}
        Some(v) => return Err(CodeError::Version(v)),
        None => return Err(CodeError::Damaged),
    }
    let server_id = value["id"]
        .as_str()
        .map(str::trim)
        .filter(|id| {
            (1..=128).contains(&id.len())
                && id
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
        })
        .ok_or(CodeError::NoServerId)?
        .to_string();
    let port = value["p"]
        .as_u64()
        .and_then(|p| u16::try_from(p).ok())
        .filter(|p| *p > 0)
        .ok_or(CodeError::BadPort)?;
    let pairing_code = value["c"]
        .as_str()
        .and_then(short_code)
        .ok_or(CodeError::BadPairingCode)?;
    let mut urls: Vec<String> = Vec::new();
    for address in value["a"].as_array().into_iter().flatten() {
        let Some(url) = address.as_str().and_then(|a| address_url(a, port)) else {
            continue;
        };
        if !urls.contains(&url) {
            urls.push(url);
        }
        if urls.len() == CODE_ADDRESSES {
            break;
        }
    }
    let first = urls.first().ok_or(CodeError::NoAddress)?;
    let name = match value["n"].as_str().map(str::trim) {
        Some(name) if !name.is_empty() => name.to_string(),
        _ => url_host(first).to_string(),
    };
    Ok(ConnectionCode {
        server_id,
        name,
        port,
        urls,
        pairing_code,
    })
}

/// `http://address:port` for an address in a connection code (a host name,
/// an IPv4 or an IPv6 address); `None` for anything else.
fn address_url(address: &str, port: u16) -> Option<String> {
    let address = address.trim();
    let host = if address.contains(':') {
        let ip: std::net::Ipv6Addr = address
            .trim_start_matches('[')
            .trim_end_matches(']')
            .parse()
            .ok()?;
        format!("[{ip}]")
    } else {
        address.to_string()
    };
    normalize_url(&format!("http://{host}:{port}")).ok()
}

/// The host of a base URL [`normalize_url`] made.
fn url_host(url: &str) -> &str {
    let rest = url.trim_start_matches("http://");
    match rest.rsplit_once(':') {
        Some((host, port)) if port.chars().all(|c| c.is_ascii_digit()) => host,
        _ => rest,
    }
}

/// The most addresses kept for one paired server, newest last.
pub const KNOWN_ADDRESSES: usize = 16;

/// Where to look for a paired server whose address `failed` stopped
/// answering (its computer got another address on the network): the
/// addresses it was reached at before (`known`, newest first), then its
/// machine's `host` name and `<host>.local` on the same port. Never `failed`
/// itself, nor one address twice. Discovery on the local network finds it
/// too; these are for where discovery doesn't reach.
pub fn relocation_urls(failed: &str, known: &[String], host: Option<&str>) -> Vec<String> {
    let port = failed
        .rsplit_once(':')
        .and_then(|(_, port)| port.parse::<u16>().ok());
    let host = host.map(str::trim).filter(|host| !host.is_empty());
    let names = port.zip(host).into_iter().flat_map(|(port, host)| {
        let bare = host.strip_suffix(".local").unwrap_or(host);
        [bare.to_string(), format!("{bare}.local")]
            .into_iter()
            .filter_map(move |name| address_url(&name, port))
    });
    let mut urls: Vec<String> = Vec::new();
    for url in known.iter().rev().cloned().chain(names) {
        if url != failed && !urls.contains(&url) {
            urls.push(url);
        }
    }
    urls
}

/// `known` (a paired server's addresses, newest last) with `url` as the
/// newest, at most [`KNOWN_ADDRESSES`].
pub fn remember_address(known: &mut Vec<String>, url: &str) {
    known.retain(|known| known != url);
    known.push(url.to_string());
    let extra = known.len().saturating_sub(KNOWN_ADDRESSES);
    known.drain(..extra);
}

/// A pairing code read as the server reads it (case, dashes and white
/// space ignored, O as 0, I and L as 1, the server's alphabet), as
/// `XXXX-XXXX`; `None` when it can't be one.
pub fn short_code(code: &str) -> Option<String> {
    let code: String = code
        .chars()
        .filter(|c| !c.is_whitespace() && *c != '-')
        .map(|c| match c.to_ascii_uppercase() {
            'O' => '0',
            'I' | 'L' => '1',
            c => c,
        })
        .collect();
    (code.len() == 8 && code.chars().all(|c| PAIRING_ALPHABET.contains(c)))
        .then(|| format!("{}-{}", &code[..4], &code[4..]))
}

impl ConnectionCode {
    /// No address answered as this server.
    pub fn not_found(&self) -> String {
        format!(
            "Couldn't find {} on your network. Is it on and on the same Wi-Fi? On it, run \
             gentle-shell serve share again. If it still can't be found, its firewall may \
             block port {}: allow it there.",
            self.name, self.port
        )
    }

    /// A Gentle Shell server answered, but not the one in the code.
    pub fn other_server(&self) -> String {
        format!(
            "A different Gentle Shell server answered instead of {} (it may have restarted). \
             On {}, run gentle-shell serve share again and paste the new code.",
            self.name, self.name
        )
    }

    /// The server refused the pairing code: unknown, used or expired.
    pub fn expired(&self) -> String {
        format!(
            "This connection code has expired or was already used. On {}, run gentle-shell \
             serve share again and paste the new code.",
            self.name
        )
    }
}

/// What one address answered at [`IDENTITY_PATH`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Probe {
    /// Nothing answered in time.
    Unreachable,
    /// Something answered, but not as a Gentle Shell server.
    NotAServer,
    /// A Gentle Shell server, with its id.
    Server(String),
}

/// What an identity answer says: the server's id when it is one.
pub fn identity(body: &Value) -> Probe {
    match body["serverId"].as_str() {
        Some(id) if body["schema"] == IDENTITY_SCHEMA && !id.is_empty() => {
            Probe::Server(id.to_string())
        }
        _ => Probe::NotAServer,
    }
}

/// Where the search for a connection code's server stands.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Search {
    /// The address (its index in [`ConnectionCode::urls`]) that answered
    /// as the server.
    Found(usize),
    /// Not found yet; addresses are still being asked.
    Waiting,
    /// Only other Gentle Shell servers answered.
    OtherServer,
    /// No Gentle Shell server answered.
    NotFound,
}

/// The search for the server `server_id` once `arrived` answers came (in
/// the order they came: `(address index, answer)`): the first address that
/// answered as that server wins. `finished` is whether no more answers
/// will come (every address answered, or time is up).
pub fn search(server_id: &str, arrived: &[(usize, Probe)], finished: bool) -> Search {
    let found = arrived
        .iter()
        .find(|(_, probe)| matches!(probe, Probe::Server(id) if id == server_id));
    if let Some((index, _)) = found {
        Search::Found(*index)
    } else if !finished {
        Search::Waiting
    } else if arrived
        .iter()
        .any(|(_, probe)| matches!(probe, Probe::Server(_)))
    {
        Search::OtherServer
    } else {
        Search::NotFound
    }
}

/// What a client broadcasts to find servers on the local network.
pub const DISCOVER_TYPE: &str = "gentle-shell/discover";

/// What a server answers a [`DISCOVER_TYPE`] query with.
pub const HERE_TYPE: &str = "gentle-shell/here";

/// The ports servers are looked for at: a server answers discovery on
/// its HTTP port (8787 by default; the next free ones when that's taken).
pub const DISCOVERY_PORTS: std::ops::RangeInclusive<u16> = 8787..=8796;

/// The largest discovery datagram either side reads or sends.
pub const DATAGRAM_MAX: usize = 512;

/// The query a client broadcasts: `{"t":"gentle-shell/discover","v":1}`.
pub fn discover_query() -> Vec<u8> {
    serde_json::json!({"t": DISCOVER_TYPE, "v": 1})
        .to_string()
        .into_bytes()
}

/// A server's answer to the query: who it is and its HTTP port. Not yet
/// trusted: anything on the network can send one, so it is checked at
/// [`IDENTITY_PATH`] before a pairing code goes there.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Announce {
    pub server_id: String,
    /// The server's host name, cleaned for showing (may be empty).
    pub name: String,
    pub port: u16,
}

/// A datagram read as a server's answer
/// (`{"t":"gentle-shell/here","v":1,"id","n","p"}`); `None` for anything
/// else: too long, not JSON, another type or version, no valid id or port.
pub fn read_announce(datagram: &[u8]) -> Option<Announce> {
    if datagram.len() > DATAGRAM_MAX {
        return None;
    }
    let value: Value = serde_json::from_slice(datagram).ok()?;
    if value["t"] != HERE_TYPE || value["v"] != 1 {
        return None;
    }
    let server_id = value["id"].as_str().filter(|id| is_server_id(id))?;
    let port = value["p"]
        .as_u64()
        .and_then(|p| u16::try_from(p).ok())
        .filter(|p| *p > 0)?;
    let name: String = value["n"]
        .as_str()
        .unwrap_or_default()
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .take(64)
        .collect();
    Some(Announce {
        server_id: server_id.to_string(),
        name: name.trim().to_string(),
        port,
    })
}

/// Whether `id` can be a server id (`srv_…`).
fn is_server_id(id: &str) -> bool {
    (1..=128).contains(&id.len())
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

/// The directed broadcast address of the IPv4 network `addr/prefix`
/// (192.168.1.20/24 → 192.168.1.255); `None` for loopback, link-local and
/// networks without one (/0, /31, /32).
pub fn directed_broadcast(addr: Ipv4Addr, prefix: u8) -> Option<Ipv4Addr> {
    if addr.is_loopback() || addr.is_link_local() || addr.is_unspecified() {
        return None;
    }
    if !(1..=30).contains(&prefix) {
        return None;
    }
    let host_bits = u32::MAX >> prefix;
    Some(Ipv4Addr::from(u32::from(addr) | host_bits))
}

/// Where the discovery query goes, given this computer's interface
/// networks (`address`, prefix length): 255.255.255.255 and each IPv4
/// network's directed broadcast address, each at every port in
/// [`DISCOVERY_PORTS`], without repeats.
pub fn discovery_targets(networks: &[(IpAddr, u8)]) -> Vec<SocketAddr> {
    let mut hosts = vec![Ipv4Addr::BROADCAST];
    for (addr, prefix) in networks {
        if let IpAddr::V4(addr) = addr
            && let Some(broadcast) = directed_broadcast(*addr, *prefix)
            && !hosts.contains(&broadcast)
        {
            hosts.push(broadcast);
        }
    }
    hosts
        .into_iter()
        .flat_map(|host| DISCOVERY_PORTS.map(move |port| SocketAddr::from((host, port))))
        .collect()
}

/// A server found on the network: it answered the query from `ip` and
/// says the same id at [`IDENTITY_PATH`] there.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Nearby {
    pub server_id: String,
    /// Its host name, or its address when it gave none.
    pub name: String,
    pub ip: IpAddr,
    pub port: u16,
}

impl Nearby {
    /// The server answering at `ip` as `announce` says.
    pub fn new(announce: &Announce, ip: IpAddr) -> Self {
        Self {
            server_id: announce.server_id.clone(),
            name: if announce.name.is_empty() {
                ip.to_string()
            } else {
                announce.name.clone()
            },
            ip,
            port: announce.port,
        }
    }

    /// Its base URL, `http://ip:port`.
    pub fn url(&self) -> String {
        match self.ip {
            IpAddr::V4(ip) => format!("http://{ip}:{}", self.port),
            IpAddr::V6(ip) => format!("http://[{ip}]:{}", self.port),
        }
    }
}

/// What the search for servers on the network found, for a short code.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Pick {
    /// No server: the code can't go anywhere.
    None,
    /// One server: the code goes there.
    One(Nearby),
    /// Several: the user picks one (by name, then address).
    Many(Vec<Nearby>),
}

/// What to do with the servers found (each checked): a server reached at
/// several addresses (one per interface) counts once, at the first.
pub fn pick(found: Vec<Nearby>) -> Pick {
    let mut servers: Vec<Nearby> = Vec::new();
    for server in found {
        if !servers.iter().any(|s| s.server_id == server.server_id) {
            servers.push(server);
        }
    }
    match servers.len() {
        0 => Pick::None,
        1 => Pick::One(servers.remove(0)),
        _ => {
            servers.sort_by(|a, b| {
                a.name
                    .to_lowercase()
                    .cmp(&b.name.to_lowercase())
                    .then(a.ip.cmp(&b.ip))
            });
            Pick::Many(servers)
        }
    }
}

/// No server answered on the network.
pub const NONE_NEARBY: &str = "Couldn't find a computer running gentle-shell serve on your \
     network. Make sure both are on the same Wi-Fi and its firewall allows port 8787 (on \
     Omarchy: sudo ufw allow 8787). Or paste the long code (gsc1.…) gentle-shell serve share \
     shows instead.";

/// The server `name` refused the short code: unknown, used or expired.
pub fn short_code_refused(name: &str) -> String {
    format!(
        "{name} doesn't know that code, or it expired or was already used. On {name}, run \
         gentle-shell serve share again and enter the new code."
    )
}

/// The `{ "error": { "code", "message" } }` every server error carries.
pub fn error_body(body: &Value) -> (Option<&str>, Option<&str>) {
    (
        body["error"]["code"].as_str(),
        body["error"]["message"].as_str(),
    )
}

/// What a server error means to the user, from its status and the error's
/// `code` and `message`: pairing (`invalid_code`, `rate_limited`,
/// `too_many_devices`), the address guards (`421 bad_host`, `403
/// forbidden_origin`), a token no longer valid and session limits; anything
/// else is the server's own message.
pub fn error_text(status: u16, code: Option<&str>, message: Option<&str>) -> String {
    match (status, code) {
        (403, Some("invalid_code")) => "That pairing code is unknown, used or expired. Run \
             gentle-shell serve pair on the server for a new one."
            .into(),
        (429, Some("rate_limited")) => {
            "Too many failed pairing attempts: the server pauses pairing for a minute.".into()
        }
        (409, Some("too_many_devices")) => "The server has too many paired devices. Revoke one \
             there with gentle-shell serve devices --revoke <id>."
            .into(),
        (421, _) => "The server doesn't answer to this address (it checks the Host). Use the \
             URL gentle-shell serve printed, or the address it listens on."
            .into(),
        (403, Some("forbidden_origin")) => "The server refused this request's origin.".into(),
        (401, _) => "This device isn't paired with the server any more (its token was revoked \
             or the server's devices were reset). Pair it again in Configuration › General."
            .into(),
        (403, Some("forbidden")) => {
            "This device's pairing doesn't allow that (it lacks the scope).".into()
        }
        (429, Some("too_many_sessions")) => {
            "The server already runs as many sessions as it allows (16). Close one first.".into()
        }
        (400, Some("invalid_cwd")) => "The server has no such folder. A server tab's folder is \
             a path on the server, not on this computer."
            .into(),
        (429, Some("too_many_forwards")) => "The server already forwards as many pages as it \
             allows (32). Forwards close after 12 hours unused, or when their session ends."
            .into(),
        (409, Some("session_exited")) => "The session that opened this page has ended.".into(),
        (503, Some("tower_unavailable")) => match message {
            Some(message) if !message.trim().is_empty() => {
                format!(
                    "Gentle Tower couldn't start on the server: {}",
                    message.trim()
                )
            }
            _ => "Gentle Tower couldn't start on the server.".into(),
        },
        (501, Some("not_supported")) => {
            "The server can't do that: its gentle-shell serve doesn't support it.".into()
        }
        // `POST /api/print`: a one-shot `gentle-shell --print` there.
        (502, Some("print_failed")) => match message {
            Some(message) if !message.trim().is_empty() => {
                format!("gentle-shell failed on the server: {}", message.trim())
            }
            _ => "gentle-shell failed on the server.".into(),
        },
        (502, Some("output_too_large")) => {
            "gentle-shell on the server answered more than it allows (1 MiB).".into()
        }
        (504, Some("print_timeout")) => {
            "gentle-shell on the server didn't answer within 2 minutes.".into()
        }
        (429, Some("too_many_prints")) => {
            "The server is already writing 4 answers like this one. Try again in a moment.".into()
        }
        (404, _) => "The server doesn't know this session any more (it was stopped, or the \
             server restarted)."
            .into(),
        _ => match message {
            Some(message) if !message.trim().is_empty() => {
                format!("The server answered {status}: {}", message.trim())
            }
            _ => format!("The server answered {status}."),
        },
    }
}

/// Whether `id` can be a server session id (`ses_…`), as the server's
/// routes take it: a saved one that can't is never put in a URL.
pub fn is_session_id(id: &str) -> bool {
    (1..=64).contains(&id.len()) && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// The Pi session id in a session file's name (`<timestamp>_<id>.jsonl`), as
/// a server's `POST /api/sessions { sessionId }` takes it: an id, never a
/// path.
pub fn pi_session_id(session_file: &str) -> Option<String> {
    let name = session_file.rsplit(['/', '\\']).next()?;
    let stem = name.strip_suffix(".jsonl").unwrap_or(name);
    let (_, id) = stem.split_once('_')?;
    let valid = id.len() <= 128
        && id.chars().next().is_some_and(|c| c.is_ascii_alphanumeric())
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'));
    valid.then(|| id.to_string())
}

/// One Server-Sent Event: its `event` name (`None` for a plain message),
/// its `id` and its `data` lines joined.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SseEvent {
    pub event: Option<String>,
    pub id: Option<String>,
    pub data: String,
}

/// The `text/event-stream` framing, fed bytes as they arrive: lines end in
/// LF (an optional CR before it dropped), a blank line dispatches the event,
/// `:` lines are comments (the heartbeat) and `retry` is ignored.
#[derive(Debug, Default)]
pub struct SseParser {
    buf: Vec<u8>,
    event: Option<String>,
    id: Option<String>,
    data: Option<String>,
}

impl SseParser {
    /// The events `bytes` completes, in order.
    pub fn push(&mut self, bytes: &[u8]) -> Vec<SseEvent> {
        let scanned = self.buf.len();
        self.buf.extend_from_slice(bytes);
        let mut events = Vec::new();
        let mut start = 0;
        let mut from = scanned;
        while let Some(end) = self.buf[from..].iter().position(|b| *b == b'\n') {
            let end = from + end;
            let mut line = &self.buf[start..end];
            if let Some(stripped) = line.strip_suffix(b"\r") {
                line = stripped;
            }
            let line = String::from_utf8_lossy(line).into_owned();
            if let Some(event) = self.line(&line) {
                events.push(event);
            }
            start = end + 1;
            from = start;
        }
        self.buf.drain(..start);
        events
    }

    fn line(&mut self, line: &str) -> Option<SseEvent> {
        if line.is_empty() {
            let data = self.data.take();
            let event = self.event.take();
            let id = self.id.take();
            return data.map(|data| SseEvent { event, id, data });
        }
        if line.starts_with(':') {
            return None;
        }
        let (field, value) = line.split_once(':').unwrap_or((line, ""));
        let value = value.strip_prefix(' ').unwrap_or(value);
        match field {
            "event" => self.event = Some(value.to_string()),
            "id" => self.id = Some(value.to_string()),
            "data" => match &mut self.data {
                Some(data) => {
                    data.push('\n');
                    data.push_str(value);
                }
                None => self.data = Some(value.to_string()),
            },
            _ => {}
        }
        None
    }
}

/// What one stream event asks of the client.
#[derive(Debug, Clone, PartialEq)]
pub enum Step {
    /// A record the session printed, to handle as if read from its stdout.
    Record(Value),
    /// Records the client never saw are gone (`snapshot-required`): read the
    /// whole state again; the live stream continues on the same connection.
    Snapshot,
    /// The server closed the stream for falling behind (`resync`):
    /// reconnect after the cursor.
    Reconnect,
    /// The session's process exited; the stream ends.
    Exited(Option<i32>),
    /// Something to tell the user (a dropped record).
    Note(String),
    /// A client answered the dialog with this id (`serve_dialog_resolved`):
    /// close it if it is still shown.
    DialogResolved(String),
    /// Nothing to do (`hello`, a record already seen, server bookkeeping).
    Ignore,
}

/// One session's event stream as a client follows it: the last `seq` seen
/// (the `after` of a reconnect) and the dialogs already shown, so neither a
/// replay nor a `dialog` event repeats one.
#[derive(Debug, Default)]
pub struct Stream {
    seq: Option<u64>,
    dialogs: HashSet<String>,
    dialog_order: VecDeque<String>,
}

impl Stream {
    /// The `after` to (re)connect with: `None` the first time, for the live
    /// stream (the client reads the state itself).
    pub fn after(&self) -> Option<u64> {
        self.seq
    }

    /// Handle one event.
    pub fn step(&mut self, event: &SseEvent) -> Step {
        match event.event.as_deref() {
            None | Some("message") => self.record(event),
            Some("snapshot-required") => {
                // The live stream goes on from the server's current seq.
                if let Some(seq) = serde_json::from_str::<Value>(&event.data)
                    .ok()
                    .and_then(|data| data["seq"].as_u64())
                {
                    self.seq = Some(seq);
                }
                Step::Snapshot
            }
            Some("resync") => Step::Reconnect,
            Some("dialog") => {
                let Ok(record) = serde_json::from_str::<Value>(&event.data) else {
                    return Step::Ignore;
                };
                match record["id"].as_str() {
                    Some(id) if self.dialogs.contains(id) => Step::Ignore,
                    Some(id) => {
                        self.remember_dialog(id);
                        Step::Record(record)
                    }
                    None => Step::Ignore,
                }
            }
            // A host tool call still waiting for this device that the replay
            // didn't carry: the original record, re-sent without an `id` (it
            // never moves the cursor). The app answers each call once.
            Some("host_tool_call") => {
                serde_json::from_str::<Value>(&event.data).map_or(Step::Ignore, Step::Record)
            }
            _ => Step::Ignore,
        }
    }

    fn record(&mut self, event: &SseEvent) -> Step {
        if let Some(seq) = event.id.as_deref().and_then(|id| id.parse::<u64>().ok()) {
            if self.seq.is_some_and(|last| seq <= last) {
                return Step::Ignore;
            }
            self.seq = Some(seq);
        }
        let Ok(record) = serde_json::from_str::<Value>(&event.data) else {
            return Step::Ignore;
        };
        match record["type"].as_str() {
            Some("serve_session_exited") => {
                Step::Exited(record["code"].as_i64().and_then(|c| i32::try_from(c).ok()))
            }
            // Some client answered this dialog (another device, or this one):
            // one still shown here closes.
            Some("serve_dialog_resolved") => match record["id"].as_str() {
                Some(id) => Step::DialogResolved(id.to_string()),
                None => Step::Ignore,
            },
            Some("serve_record_dropped") => Step::Note(format!(
                "The server dropped a record over {} bytes.",
                record["maxBytes"].as_u64().unwrap_or_default()
            )),
            Some("extension_ui_request") => {
                let dialog = record["method"]
                    .as_str()
                    .is_some_and(|method| DIALOGS.contains(&method));
                match record["id"].as_str() {
                    Some(id) if dialog && self.dialogs.contains(id) => Step::Ignore,
                    Some(id) if dialog => {
                        self.remember_dialog(id);
                        Step::Record(record)
                    }
                    _ => Step::Record(record),
                }
            }
            _ => Step::Record(record),
        }
    }

    fn remember_dialog(&mut self, id: &str) {
        if self.dialogs.insert(id.to_string()) {
            self.dialog_order.push_back(id.to_string());
            if self.dialog_order.len() > SEEN_DIALOGS
                && let Some(old) = self.dialog_order.pop_front()
            {
                self.dialogs.remove(&old);
            }
        }
    }
}

/// Pi responses the stream repeats under the server's ids. The `/rpc`
/// answer already carries the client's id, so the stream's copy is skipped;
/// a command that took longer than the server waits answers `202 { pending,
/// id }`, and its response then arrives only in the stream, under that id,
/// which goes back to the client's.
#[derive(Debug, Default)]
pub struct RpcIds {
    /// Server id → the client's, for commands answered `202 pending`.
    waiting: HashMap<String, String>,
    /// Stream responses no one claimed yet, newest last: a `202` may arrive
    /// after the stream's copy.
    unclaimed: VecDeque<(String, Value)>,
}

impl RpcIds {
    /// A command sent with `client` as its id was answered `202 pending`
    /// under `server`: its response, if the stream already carried it.
    pub fn pending(&mut self, server: &str, client: &str) -> Option<Value> {
        if let Some(at) = self.unclaimed.iter().position(|(id, _)| id == server) {
            let (_, mut response) = self.unclaimed.remove(at)?;
            response["id"] = Value::String(client.to_string());
            return Some(response);
        }
        self.waiting.insert(server.to_string(), client.to_string());
        None
    }

    /// A record from the stream: what to deliver, if anything. A response
    /// under a server id is someone's `/rpc` answer: delivered (with the
    /// client's id) only when it settles a pending command of this client.
    pub fn from_stream(&mut self, mut record: Value) -> Option<Value> {
        if record["type"] != "response" {
            return Some(record);
        }
        let Some(id) = record["id"]
            .as_str()
            .filter(|id| id.starts_with(SERVER_RPC_ID))
        else {
            return Some(record);
        };
        let id = id.to_string();
        if let Some(client) = self.waiting.remove(&id) {
            record["id"] = Value::String(client);
            return Some(record);
        }
        self.unclaimed.push_back((id, record));
        if self.unclaimed.len() > UNCLAIMED {
            self.unclaimed.pop_front();
        }
        None
    }
}

/// How long to wait before reconnecting after `failures` attempts in a
/// row: 1 s, doubling, at most 30 s.
pub fn backoff_secs(failures: u32) -> u64 {
    1u64.checked_shl(failures.min(5)).unwrap_or(32).min(30)
}

/// The longest path a forward takes (`POST /api/sessions/:id/forwards`).
const FORWARD_PATH: usize = 2048;

/// Where a URL a server session gave (the design canvas, `open_page`, a
/// dev server in a tool's output) is, seen from a device of that server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ServerPage {
    /// Not on the server's loopback: it opens as it is.
    Elsewhere,
    /// On the server's own loopback (`localhost`, `127.0.0.0/8`, `[::1]`, or
    /// the unspecified `0.0.0.0`/`[::]` a dev server prints): only a forward
    /// reaches it, with `port` and `path` (query and fragment included) as
    /// `POST /api/sessions/:id/forwards` takes them.
    Loopback { port: u16, path: String },
    /// On the server's loopback, but no forward can carry it: what's wrong,
    /// in words.
    Unreachable(String),
}

/// Where `url`, which a server session gave, is ([`ServerPage`]). A page
/// on the server's loopback never opens as it is on this device: there
/// it is this device's own loopback, another page or none.
pub fn server_page(url: &str) -> ServerPage {
    let url = url.trim();
    let Some((scheme, rest)) = url.split_once("://") else {
        return ServerPage::Elsewhere;
    };
    let scheme = scheme.to_ascii_lowercase();
    if !matches!(scheme.as_str(), "http" | "https" | "ws" | "wss") {
        return ServerPage::Elsewhere;
    }
    let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let (authority, tail) = rest.split_at(end);
    let host_port = authority.rsplit('@').next().unwrap_or(authority);
    let (host, port) = if let Some(bracketed) = host_port.strip_prefix('[') {
        let Some((host, after)) = bracketed.split_once(']') else {
            return ServerPage::Elsewhere;
        };
        (host, after.strip_prefix(':'))
    } else {
        match host_port.split_once(':') {
            Some((host, port)) => (host, Some(port)),
            None => (host_port, None),
        }
    };
    if !is_loopback_host(host) {
        return ServerPage::Elsewhere;
    }
    if matches!(scheme.as_str(), "https" | "wss") {
        return ServerPage::Unreachable(format!(
            "{url} is served over HTTPS on the server's own loopback; the server forwards \
             plain HTTP pages only."
        ));
    }
    let port = match port {
        None | Some("") => 80,
        Some(port) => match port.parse::<u16>() {
            Ok(port) if port > 0 => port,
            _ => return ServerPage::Unreachable(format!("{url} has no valid port.")),
        },
    };
    let mut path = String::with_capacity(tail.len() + 1);
    if !tail.starts_with('/') {
        path.push('/');
    }
    for byte in tail.bytes() {
        if (0x21..=0x7e).contains(&byte) {
            path.push(byte as char);
        } else {
            path.push_str(&format!("%{byte:02X}"));
        }
    }
    // The server refuses a path starting `//` (a scheme-relative URL).
    while path.starts_with("//") {
        path.remove(0);
    }
    if path.len() > FORWARD_PATH {
        return ServerPage::Unreachable(format!(
            "That page's address is too long to forward (over {FORWARD_PATH} characters)."
        ));
    }
    ServerPage::Loopback { port, path }
}

/// The folder a new agent tab starts in while agents run on a Gentle Shell
/// server: `cwd` when it is known to be a folder there (`there`: the
/// focused agent runs there, or the workspace was opened there), else the
/// server user's `home`: a folder on this computer is no folder on the
/// server. Without a home to go to (an older server doesn't say), `cwd`.
pub fn tab_folder(cwd: &std::path::Path, there: bool, home: Option<&str>) -> std::path::PathBuf {
    match home {
        Some(home) if !there => home.into(),
        _ => cwd.to_path_buf(),
    }
}

/// A folder on a Gentle Shell server as workspaces compare it: normalized
/// by its text alone, since this computer can't resolve the server's links
/// (resolving it here would read this computer's file system). `.` segments
/// and repeated or trailing separators go; `..` drops the segment before it,
/// never climbing above the root. The separator is the path's own: `/`, or
/// `\` for a path written only with those (a Windows server's).
pub fn folder_key(path: &str) -> String {
    let sep = if !path.contains('/') && path.contains('\\') {
        '\\'
    } else {
        '/'
    };
    let rooted = path.starts_with(sep);
    let mut parts: Vec<&str> = Vec::new();
    for part in path.split(sep) {
        match part {
            "" | "." => {}
            ".." => {
                let drive = parts.len() == 1 && parts[0].ends_with(':');
                match parts.last() {
                    Some(&last) if last != ".." && !drive => {
                        parts.pop();
                    }
                    // Above the root (or a drive) there is nothing.
                    _ if rooted || drive => {}
                    _ => parts.push(".."),
                }
            }
            _ => parts.push(part),
        }
    }
    let sep = sep.to_string();
    let joined = parts.join(&sep);
    match (rooted, parts.as_slice()) {
        (true, _) => format!("{sep}{joined}"),
        (false, [drive]) if drive.ends_with(':') => format!("{drive}{sep}"),
        (false, []) => ".".into(),
        (false, _) => joined,
    }
}

/// Whether `host` (no brackets) names the machine it is read on.
fn is_loopback_host(host: &str) -> bool {
    let host = host.trim_end_matches('.').to_ascii_lowercase();
    if host == "localhost" || host.ends_with(".localhost") {
        return true;
    }
    match host.parse::<IpAddr>() {
        Ok(ip) => ip.is_loopback() || ip.is_unspecified(),
        Err(_) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn addresses_read_as_the_base_url_requests_use() {
        for (typed, url) in [
            ("http://127.0.0.1:8787", "http://127.0.0.1:8787"),
            ("  127.0.0.1:8787/ ", "http://127.0.0.1:8787"),
            ("HTTP://Omarchy.Local:8787", "http://omarchy.local:8787"),
            ("http://[::1]:8787", "http://[::1]:8787"),
            ("http://[FE80::1]", "http://[fe80::1]"),
            ("pc.tailnet.ts.net", "http://pc.tailnet.ts.net"),
        ] {
            assert_eq!(normalize_url(typed).as_deref(), Ok(url), "{typed}");
        }
        for typed in [
            "",
            "https://127.0.0.1:8787",
            "ws://127.0.0.1:8787",
            "http://127.0.0.1:8787/api",
            "http://127.0.0.1:8787?x=1",
            "http://me@127.0.0.1:8787",
            "http://127.0.0.1:0",
            "http://127.0.0.1:99999",
            "http://127.0.0.1:-1",
            "http://[::1",
            "http://bad host:1",
        ] {
            assert!(normalize_url(typed).is_err(), "{typed}");
        }
        assert!(
            normalize_url("https://x")
                .unwrap_err()
                .contains("plain HTTP")
        );
    }

    #[test]
    fn pages_on_the_servers_loopback_go_through_a_forward() {
        let loopback = |port: u16, path: &str| ServerPage::Loopback {
            port,
            path: path.into(),
        };
        for (url, page) in [
            ("http://localhost:5173", loopback(5173, "/")),
            (
                "http://127.0.0.1:8080/a/b?x=1#top",
                loopback(8080, "/a/b?x=1#top"),
            ),
            ("http://[::1]:3000/", loopback(3000, "/")),
            ("HTTP://LocalHost:4321/x", loopback(4321, "/x")),
            ("http://0.0.0.0:8000", loopback(8000, "/")),
            ("http://127.0.1.1:9/x", loopback(9, "/x")),
            ("http://app.localhost:5000?q", loopback(5000, "/?q")),
            ("http://localhost", loopback(80, "/")),
            ("ws://localhost:24678/hmr", loopback(24678, "/hmr")),
            ("  http://localhost:1/a b  ", loopback(1, "/a%20b")),
            ("http://localhost:1/ñ", loopback(1, "/%C3%B1")),
            ("http://localhost:1//evil.com/x", loopback(1, "/evil.com/x")),
        ] {
            assert_eq!(server_page(url), page, "{url}");
        }
        for url in [
            "https://example.com",
            "http://192.168.1.20:5173",
            "http://localhost.example.com:80",
            "file:///tmp/x.html",
            "about:blank",
            "localhost:5173",
            "http://[fe80::1]:80/",
        ] {
            assert_eq!(server_page(url), ServerPage::Elsewhere, "{url}");
        }
        for url in [
            "https://localhost:8443/",
            "http://localhost:0/",
            "http://localhost:99999/",
        ] {
            assert!(
                matches!(server_page(url), ServerPage::Unreachable(_)),
                "{url}"
            );
        }
        let long = format!("http://localhost:1/{}", "a".repeat(FORWARD_PATH));
        assert!(matches!(server_page(&long), ServerPage::Unreachable(_)));
    }

    #[test]
    fn new_tabs_on_a_server_never_start_in_a_folder_of_this_computer() {
        use std::path::{Path, PathBuf};
        let mac = Path::new("/Users/ana/project");
        assert_eq!(
            tab_folder(mac, false, Some("/home/ana")),
            PathBuf::from("/home/ana")
        );
        let there = Path::new("/home/ana/api");
        assert_eq!(tab_folder(there, true, Some("/home/ana")), there);
        assert_eq!(tab_folder(mac, false, None), mac);
    }

    #[test]
    fn server_folders_compare_by_their_text_alone() {
        for (path, key) in [
            ("/tmp/repo", "/tmp/repo"),
            ("/tmp//repo/", "/tmp/repo"),
            ("/tmp/./x/../repo", "/tmp/repo"),
            ("/../..", "/"),
            ("/", "/"),
            ("home/./ana/..", "home"),
            ("../a", "../a"),
            (r"C:\Users\ana\..\bob\", r"C:\Users\bob"),
            (r"C:\..", r"C:\"),
        ] {
            assert_eq!(folder_key(path), key, "{path}");
        }
    }

    #[test]
    fn pairing_codes_only_lose_their_spaces() {
        assert_eq!(normalize_code(" abcd-efgh ").as_deref(), Ok("abcd-efgh"));
        assert!(normalize_code("  ").is_err());
    }

    #[test]
    fn server_errors_say_what_to_do() {
        let body = json!({"error": {"code": "invalid_code", "message": "Unknown"}});
        let (code, message) = error_body(&body);
        assert!(error_text(403, code, message).contains("serve pair"));
        assert!(error_text(429, Some("rate_limited"), None).contains("a minute"));
        assert!(error_text(409, Some("too_many_devices"), None).contains("--revoke"));
        assert!(error_text(421, Some("bad_host"), None).contains("Host"));
        assert!(error_text(403, Some("forbidden_origin"), None).contains("origin"));
        assert!(error_text(401, Some("unauthorized"), None).contains("Pair it again"));
        assert!(error_text(400, Some("invalid_cwd"), None).contains("path on the server"));
        assert_eq!(
            error_text(500, Some("internal_error"), Some("Server error ")),
            "The server answered 500: Server error"
        );
        assert_eq!(error_text(502, None, None), "The server answered 502.");
        assert!(
            error_text(502, Some("print_failed"), Some("no API key\n"))
                .ends_with("on the server: no API key")
        );
        assert!(error_text(504, Some("print_timeout"), None).contains("2 minutes"));
    }

    #[test]
    fn pi_session_ids_come_from_the_file_name() {
        assert_eq!(
            pi_session_id(
                "/home/ana/.pi/agent/sessions/--a--/2026-11-24T10-00-00-000Z_0198a1b2-c3d4.jsonl"
            )
            .as_deref(),
            Some("0198a1b2-c3d4")
        );
        assert_eq!(
            pi_session_id(r"C:\s\2026_abc.jsonl").as_deref(),
            Some("abc")
        );
        assert_eq!(pi_session_id("/s/no-id.jsonl"), None);
        assert_eq!(pi_session_id("/s/2026_../x.jsonl"), None);
        assert_eq!(pi_session_id("/s/2026_-x.jsonl"), None);
        assert!(is_session_id("ses_0a1b2c3d4e5f6a7b"));
        assert!(!is_session_id("../api/server"));
        assert!(!is_session_id(""));
    }

    #[test]
    fn events_frame_on_blank_lines_across_chunks() {
        let mut sse = SseParser::default();
        let stream = "retry: 3000\n\nevent: hello\ndata: {\"seq\":2}\n\n: ping\n\nid: 3\r\ndata: {\"a\":\ndata: 1}\r\n\r\n";
        let mut events = Vec::new();
        // Byte by byte: a record may be cut anywhere, a UTF-8 character too.
        for byte in stream.as_bytes() {
            events.extend(sse.push(std::slice::from_ref(byte)));
        }
        assert_eq!(
            events,
            vec![
                SseEvent {
                    event: Some("hello".into()),
                    id: None,
                    data: "{\"seq\":2}".into()
                },
                SseEvent {
                    event: None,
                    id: Some("3".into()),
                    data: "{\"a\":\n1}".into()
                },
            ]
        );
        // U+2028 inside a JSON string is data, not a line break.
        let events = sse.push("id: 4\ndata: {\"t\":\"a\u{2028}b\"}\n\n".as_bytes());
        assert_eq!(events[0].data, "{\"t\":\"a\u{2028}b\"}");
    }

    fn message(seq: u64, record: Value) -> SseEvent {
        SseEvent {
            event: None,
            id: Some(seq.to_string()),
            data: record.to_string(),
        }
    }

    fn named(name: &str, data: Value) -> SseEvent {
        SseEvent {
            event: Some(name.into()),
            id: None,
            data: data.to_string(),
        }
    }

    #[test]
    fn a_re_sent_host_tool_call_is_a_record_that_leaves_the_cursor() {
        let mut stream = Stream::default();
        let record = json!({"type": "extension_ui_request", "id": "w1", "method": "setWidget"});
        stream.step(&message(5, json!({"type": "agent_start"})));
        assert_eq!(
            stream.step(&named("host_tool_call", record.clone())),
            Step::Record(record)
        );
        assert_eq!(stream.after(), Some(5));
        let broken = SseEvent {
            event: Some("host_tool_call".into()),
            id: None,
            data: "not json".into(),
        };
        assert_eq!(stream.step(&broken), Step::Ignore);
    }

    #[test]
    fn the_cursor_follows_seq_and_skips_what_it_saw() {
        let mut stream = Stream::default();
        assert_eq!(stream.after(), None);
        assert_eq!(
            stream.step(&named("hello", json!({"seq": 9, "oldest": 1}))),
            Step::Ignore
        );
        let event = json!({"type": "agent_start"});
        assert_eq!(
            stream.step(&message(10, event.clone())),
            Step::Record(event.clone())
        );
        assert_eq!(stream.after(), Some(10));
        // A replay overlapping what was seen.
        assert_eq!(stream.step(&message(10, event.clone())), Step::Ignore);
        assert_eq!(
            stream.step(&message(11, event.clone())),
            Step::Record(event)
        );
        assert_eq!(
            stream.step(&named("resync", json!({"seq": 40}))),
            Step::Reconnect
        );
        assert_eq!(stream.after(), Some(11));
        // Evicted records: read the state again, then live from the server's seq.
        assert_eq!(
            stream.step(&named(
                "snapshot-required",
                json!({"after": 11, "oldest": 30, "seq": 41})
            )),
            Step::Snapshot
        );
        assert_eq!(stream.after(), Some(41));
    }

    #[test]
    fn dialogs_show_once_across_reconnects() {
        let mut stream = Stream::default();
        let dialog = json!({"type": "extension_ui_request", "id": "d1", "method": "confirm"});
        assert_eq!(
            stream.step(&message(1, dialog.clone())),
            Step::Record(dialog.clone())
        );
        // The reconnect after seq 1 carries the pending dialog again.
        assert_eq!(stream.step(&named("dialog", dialog.clone())), Step::Ignore);
        let other = json!({"type": "extension_ui_request", "id": "d2", "method": "select"});
        assert_eq!(
            stream.step(&named("dialog", other.clone())),
            Step::Record(other)
        );
        // Fire-and-forget requests are never held back.
        let status = json!({"type": "extension_ui_request", "id": "s", "method": "setStatus"});
        assert_eq!(
            stream.step(&message(2, status.clone())),
            Step::Record(status.clone())
        );
        assert_eq!(
            stream.step(&message(3, status.clone())),
            Step::Record(status)
        );
    }

    #[test]
    fn server_records_end_or_note_the_stream() {
        let mut stream = Stream::default();
        assert_eq!(
            stream.step(&message(
                1,
                json!({"type": "serve_dialog_resolved", "id": "d"})
            )),
            Step::DialogResolved("d".into())
        );
        assert!(matches!(
            stream.step(&message(2, json!({"type": "serve_record_dropped", "maxBytes": 16}))),
            Step::Note(note) if note.contains("16 bytes")
        ));
        assert_eq!(
            stream.step(&message(
                3,
                json!({"type": "serve_session_exited", "code": 0, "signal": null})
            )),
            Step::Exited(Some(0))
        );
        assert_eq!(
            stream.step(&message(
                4,
                json!({"type": "serve_session_exited", "code": null, "signal": "SIGKILL"})
            )),
            Step::Exited(None)
        );
    }

    #[test]
    fn stream_copies_of_rpc_answers_are_skipped_unless_pending() {
        let mut ids = RpcIds::default();
        let response =
            |id: &str| json!({"type": "response", "id": id, "command": "bash", "success": true});
        // Answered over HTTP: the stream's copy is a duplicate.
        assert_eq!(ids.from_stream(response("serve-rpc-1")), None);
        // Pi's own records and responses without a server id pass.
        let event = json!({"type": "agent_end"});
        assert_eq!(ids.from_stream(event.clone()), Some(event));
        assert_eq!(ids.from_stream(response("x")), Some(response("x")));
        // A `202 pending` first: the stream's response settles it.
        assert_eq!(ids.pending("serve-rpc-2", "gs-7"), None);
        assert_eq!(
            ids.from_stream(response("serve-rpc-2")),
            Some(response("gs-7"))
        );
        // The stream first, then the `202`: still settled, once.
        assert_eq!(ids.from_stream(response("serve-rpc-3")), None);
        assert_eq!(ids.pending("serve-rpc-3", "gs-8"), Some(response("gs-8")));
        assert_eq!(ids.pending("serve-rpc-3", "gs-8"), None);
    }

    #[test]
    fn unclaimed_answers_are_bounded() {
        let mut ids = RpcIds::default();
        for n in 0..(UNCLAIMED + 10) {
            ids.from_stream(json!({"type": "response", "id": format!("serve-rpc-{n}")}));
        }
        assert_eq!(ids.unclaimed.len(), UNCLAIMED);
        assert_eq!(ids.pending("serve-rpc-0", "gs-1"), None);
    }

    #[test]
    fn reconnects_back_off_to_thirty_seconds() {
        assert_eq!(
            (0..8).map(backoff_secs).collect::<Vec<_>>(),
            vec![1, 2, 4, 8, 16, 30, 30, 30]
        );
    }

    fn encode(value: &Value) -> String {
        use base64::Engine as _;
        format!(
            "{CODE_PREFIX}{}",
            base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(value.to_string())
        )
    }

    fn shared() -> Value {
        json!({
            "v": 1,
            "id": "srv_abc123",
            "n": "omarchy",
            "p": 8787,
            "a": ["192.168.1.20", "10.0.0.5", "omarchy", "omarchy.local"],
            "c": "ABCD-EFGH"
        })
    }

    #[test]
    fn a_connection_code_reads_as_its_server_and_addresses() {
        let code = decode_connection_code(&encode(&shared())).unwrap();
        assert_eq!(
            code,
            ConnectionCode {
                server_id: "srv_abc123".into(),
                name: "omarchy".into(),
                port: 8787,
                urls: vec![
                    "http://192.168.1.20:8787".into(),
                    "http://10.0.0.5:8787".into(),
                    "http://omarchy:8787".into(),
                    "http://omarchy.local:8787".into(),
                ],
                pairing_code: "ABCD-EFGH".into(),
            }
        );
        assert!(is_connection_code(&format!("  {}", encode(&shared()))));
        assert!(!is_connection_code("ABCD-EFGH"));
    }

    #[test]
    fn a_connection_code_survives_wrapping_padding_and_a_sloppy_pairing_code() {
        let mut value = shared();
        value["c"] = "abcdefgh".into();
        let text = encode(&value);
        let (head, tail) = text.split_at(20);
        let wrapped = format!("\n {head}\n  {tail}==\n");
        let code = decode_connection_code(&wrapped).unwrap();
        assert_eq!(code.pairing_code, "ABCD-EFGH");
        value["c"] = "o1l2-i3o4".into();
        assert_eq!(
            decode_connection_code(&encode(&value))
                .unwrap()
                .pairing_code,
            "0112-1304"
        );
    }

    #[test]
    fn addresses_are_checked_deduplicated_and_capped() {
        let mut value = shared();
        value["a"] = json!([
            "fd00::5",
            "bad host",
            "10.0.0.1/admin",
            "user@10.0.0.2",
            "10.0.0.3:99",
            7,
            "10.0.0.4",
            "10.0.0.4",
            "a1",
            "a2",
            "a3",
            "a4",
            "a5",
            "a6",
            "a7",
            "a8"
        ]);
        let code = decode_connection_code(&encode(&value)).unwrap();
        assert_eq!(code.urls.len(), CODE_ADDRESSES);
        assert_eq!(code.urls[0], "http://[fd00::5]:8787");
        assert_eq!(code.urls[1], "http://10.0.0.4:8787");
        assert_eq!(code.urls[7], "http://a6:8787");
        // No name: the first address's host.
        value["n"] = "  ".into();
        value["a"] = json!(["192.168.1.20"]);
        assert_eq!(
            decode_connection_code(&encode(&value)).unwrap().name,
            "192.168.1.20"
        );
    }

    #[test]
    fn a_connection_code_that_cant_be_used_says_why() {
        let with = |key: &str, field: Value| {
            let mut value = shared();
            value[key] = field;
            decode_connection_code(&encode(&value)).unwrap_err()
        };
        assert_eq!(
            decode_connection_code("ABCD-EFGH").unwrap_err(),
            CodeError::NotACode
        );
        assert_eq!(
            decode_connection_code("gsc1.!!!").unwrap_err(),
            CodeError::Damaged
        );
        let cut = encode(&shared());
        assert_eq!(
            decode_connection_code(&cut[..cut.len() - 6]).unwrap_err(),
            CodeError::Damaged
        );
        assert_eq!(
            decode_connection_code(&encode(&json!([1, 2]))).unwrap_err(),
            CodeError::Damaged
        );
        assert_eq!(with("v", json!(2)), CodeError::Version(2));
        assert!(with("v", json!(2)).to_string().contains("newer"));
        assert_eq!(with("v", json!("1")), CodeError::Damaged);
        assert_eq!(with("id", json!("")), CodeError::NoServerId);
        assert_eq!(with("id", json!("srv/../x")), CodeError::NoServerId);
        assert_eq!(with("id", Value::Null), CodeError::NoServerId);
        assert_eq!(with("a", json!([])), CodeError::NoAddress);
        assert_eq!(with("a", json!(["bad host"])), CodeError::NoAddress);
        assert_eq!(with("a", Value::Null), CodeError::NoAddress);
        assert_eq!(with("p", json!(0)), CodeError::BadPort);
        assert_eq!(with("p", json!(65536)), CodeError::BadPort);
        assert_eq!(with("p", json!("8787")), CodeError::BadPort);
        assert_eq!(with("c", json!("ABCD-EFG")), CodeError::BadPairingCode);
        assert_eq!(with("c", json!("ABCD-EFGU")), CodeError::BadPairingCode);
        assert_eq!(with("c", Value::Null), CodeError::BadPairingCode);
    }

    #[test]
    fn the_first_address_answering_as_the_server_wins() {
        let id = "srv_abc123";
        let server = |id: &str| Probe::Server(id.into());
        // Answers in the order they came, not the code's.
        let arrived = [(0, Probe::Unreachable), (3, server(id)), (2, server(id))];
        assert_eq!(search(id, &arrived, false), Search::Found(3));
        assert_eq!(search(id, &arrived[..1], false), Search::Waiting);
        assert_eq!(search(id, &arrived[..1], true), Search::NotFound);
        let others = [
            (0, Probe::NotAServer),
            (1, server("srv_other")),
            (2, Probe::Unreachable),
        ];
        assert_eq!(search(id, &others, false), Search::Waiting);
        assert_eq!(search(id, &others, true), Search::OtherServer);
        assert_eq!(search(id, &[], true), Search::NotFound);
    }

    #[test]
    fn an_identity_answer_names_a_server_only_with_its_schema() {
        assert_eq!(
            identity(&json!({
                "schema": IDENTITY_SCHEMA,
                "serverId": "srv_1",
                "name": "pc",
                "protocol": "gentle-shell.serve/v1",
                "version": "0.1.0"
            })),
            Probe::Server("srv_1".into())
        );
        assert_eq!(identity(&json!({"serverId": "srv_1"})), Probe::NotAServer);
        assert_eq!(
            identity(&json!({"schema": IDENTITY_SCHEMA, "serverId": ""})),
            Probe::NotAServer
        );
        assert_eq!(identity(&Value::Null), Probe::NotAServer);
    }

    #[test]
    fn search_failures_read_as_what_to_do() {
        let code = decode_connection_code(&encode(&shared())).unwrap();
        let missing = code.not_found();
        assert!(missing.starts_with("Couldn't find omarchy on your network."));
        assert!(missing.contains("gentle-shell serve share again"));
        assert!(missing.contains("port 8787"));
        assert!(code.other_server().contains("restarted"));
        assert!(code.expired().contains("expired or was already used"));
    }

    #[test]
    fn the_code_field_reads_a_short_or_a_long_code() {
        assert_eq!(
            read_code(" abcd efgh "),
            Ok(Entered::Short("ABCD-EFGH".into()))
        );
        assert_eq!(
            read_code("o1il-2345"),
            Ok(Entered::Short("0111-2345".into()))
        );
        assert_eq!(
            read_code("ABCDEFGH"),
            Ok(Entered::Short("ABCD-EFGH".into()))
        );
        assert_eq!(
            read_code("AB-CD-EF-GH"),
            Ok(Entered::Short("ABCD-EFGH".into()))
        );
        let long = read_code(&encode(&shared())).unwrap();
        assert!(matches!(long, Entered::Connection(code) if code.server_id == "srv_abc123"));
        assert_eq!(read_code("gsc1.!!!"), Err(CodeError::Damaged));
        for bad in [
            "",
            "  ",
            "ABCD-EFG",
            "ABCD-EFGHJ",
            "ABCD-EFGU",
            "ABCD_EFGH",
            "http://pc",
        ] {
            assert_eq!(read_code(bad), Err(CodeError::NotACode), "{bad:?}");
        }
        assert!(CodeError::NotACode.to_string().contains("8-character code"));
    }

    #[test]
    fn the_discovery_query_and_answer_follow_the_contract() {
        assert_eq!(
            String::from_utf8(discover_query()).unwrap(),
            r#"{"t":"gentle-shell/discover","v":1}"#
        );
        let here = |value: Value| read_announce(value.to_string().as_bytes());
        assert_eq!(
            here(json!({"t": HERE_TYPE, "v": 1, "id": "srv_1", "n": "omarchy", "p": 8788})),
            Some(Announce {
                server_id: "srv_1".into(),
                name: "omarchy".into(),
                port: 8788
            })
        );
        // A name is cleaned for showing; none is allowed.
        assert_eq!(
            here(json!({"t": HERE_TYPE, "v": 1, "id": "srv_1", "n": " pc\u{7}\n", "p": 1}))
                .unwrap()
                .name,
            "pc"
        );
        assert_eq!(
            here(json!({"t": HERE_TYPE, "v": 1, "id": "srv_1", "p": 1}))
                .unwrap()
                .name,
            ""
        );
        let long = "x".repeat(200);
        assert_eq!(
            here(json!({"t": HERE_TYPE, "v": 1, "id": "srv_1", "n": long, "p": 1}))
                .unwrap()
                .name
                .len(),
            64
        );
        for bad in [
            json!({"t": DISCOVER_TYPE, "v": 1, "id": "srv_1", "p": 8787}),
            json!({"t": HERE_TYPE, "v": 2, "id": "srv_1", "p": 8787}),
            json!({"t": HERE_TYPE, "v": 1, "id": "", "p": 8787}),
            json!({"t": HERE_TYPE, "v": 1, "id": "srv/x", "p": 8787}),
            json!({"t": HERE_TYPE, "v": 1, "id": "srv_1", "p": 0}),
            json!({"t": HERE_TYPE, "v": 1, "id": "srv_1", "p": 70000}),
            json!({"t": HERE_TYPE, "v": 1, "id": "srv_1", "p": "8787"}),
            json!([HERE_TYPE]),
        ] {
            assert_eq!(here(bad.clone()), None, "{bad}");
        }
        assert_eq!(read_announce(b"not json"), None);
        let padded = format!(
            r#"{{"t":"{HERE_TYPE}","v":1,"id":"srv_1","p":8787}}{}"#,
            " ".repeat(DATAGRAM_MAX)
        );
        assert_eq!(read_announce(padded.as_bytes()), None);
    }

    #[test]
    fn broadcasts_go_to_every_network_and_port() {
        let v4 = Ipv4Addr::new;
        assert_eq!(
            directed_broadcast(v4(192, 168, 1, 20), 24),
            Some(v4(192, 168, 1, 255))
        );
        assert_eq!(
            directed_broadcast(v4(10, 1, 2, 3), 8),
            Some(v4(10, 255, 255, 255))
        );
        assert_eq!(
            directed_broadcast(v4(172, 16, 5, 9), 20),
            Some(v4(172, 16, 15, 255))
        );
        assert_eq!(directed_broadcast(v4(127, 0, 0, 1), 8), None);
        assert_eq!(directed_broadcast(v4(169, 254, 3, 4), 16), None);
        assert_eq!(directed_broadcast(v4(192, 168, 1, 20), 32), None);
        assert_eq!(directed_broadcast(v4(192, 168, 1, 20), 31), None);
        assert_eq!(directed_broadcast(v4(192, 168, 1, 20), 0), None);

        let targets = discovery_targets(&[
            (IpAddr::V4(v4(192, 168, 1, 20)), 24),
            (IpAddr::V4(v4(192, 168, 1, 21)), 24),
            (IpAddr::V4(v4(127, 0, 0, 1)), 8),
            ("fe80::1".parse().unwrap(), 64),
            (IpAddr::V4(v4(10, 0, 0, 5)), 16),
        ]);
        let ports = DISCOVERY_PORTS.count();
        assert_eq!(targets.len(), 3 * ports);
        assert_eq!(targets[0], SocketAddr::from((Ipv4Addr::BROADCAST, 8787)));
        assert_eq!(
            targets[ports - 1],
            SocketAddr::from((Ipv4Addr::BROADCAST, 8796))
        );
        assert_eq!(
            targets[ports],
            SocketAddr::from((v4(192, 168, 1, 255), 8787))
        );
        assert_eq!(
            targets[2 * ports],
            SocketAddr::from((v4(10, 0, 255, 255), 8787))
        );
        assert_eq!(discovery_targets(&[]).len(), ports);
    }

    #[test]
    fn one_server_found_is_used_several_are_picked_from() {
        let found = |id: &str, name: &str, ip: &str| {
            Nearby::new(
                &Announce {
                    server_id: id.into(),
                    name: name.into(),
                    port: 8787,
                },
                ip.parse().unwrap(),
            )
        };
        assert_eq!(pick(vec![]), Pick::None);
        let pc = found("srv_1", "omarchy", "192.168.1.20");
        // The same server over two interfaces is one.
        assert_eq!(
            pick(vec![pc.clone(), found("srv_1", "omarchy", "10.0.0.5")]),
            Pick::One(pc.clone())
        );
        assert_eq!(pc.url(), "http://192.168.1.20:8787");
        let nameless = found("srv_2", "", "192.168.1.30");
        assert_eq!(nameless.name, "192.168.1.30");
        let Pick::Many(servers) = pick(vec![
            pc.clone(),
            found("srv_3", "Mac-mini", "192.168.1.40"),
            nameless,
        ]) else {
            panic!("several servers are picked from");
        };
        let names: Vec<&str> = servers.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, ["192.168.1.30", "Mac-mini", "omarchy"]);
        assert!(NONE_NEARBY.contains("sudo ufw allow 8787"));
        assert!(NONE_NEARBY.contains("gsc1."));
        assert!(short_code_refused("omarchy").contains("On omarchy, run gentle-shell serve share"));
    }

    #[test]
    fn a_moved_server_is_looked_for_at_its_other_addresses_and_names() {
        let known = [
            "http://192.168.1.20:8787".to_string(),
            "http://100.101.102.103:8787".to_string(),
            "http://192.168.1.31:8787".to_string(),
        ];
        // Newest first, never the one that failed, then the machine's names.
        assert_eq!(
            relocation_urls("http://192.168.1.31:8787", &known, Some("linux-box")),
            [
                "http://100.101.102.103:8787",
                "http://192.168.1.20:8787",
                "http://linux-box:8787",
                "http://linux-box.local:8787",
            ]
        );
        // A name already `.local` isn't doubled; a known one isn't repeated.
        let known = ["http://linux-box.local:8787".to_string()];
        assert_eq!(
            relocation_urls("http://10.0.0.2:8787", &known, Some("linux-box.local")),
            ["http://linux-box.local:8787", "http://linux-box:8787"]
        );
        // Without a name, only what was known.
        assert!(relocation_urls("http://10.0.0.2:8787", &[], None).is_empty());

        let mut known: Vec<String> = (0..KNOWN_ADDRESSES)
            .map(|n| format!("http://10.0.0.{n}:8787"))
            .collect();
        remember_address(&mut known, "http://10.0.0.3:8787");
        assert_eq!(known.len(), KNOWN_ADDRESSES);
        assert_eq!(known.last().unwrap(), "http://10.0.0.3:8787");
        remember_address(&mut known, "http://10.0.0.99:8787");
        assert_eq!(known.len(), KNOWN_ADDRESSES);
        assert_eq!(known[0], "http://10.0.0.1:8787");
    }
}
