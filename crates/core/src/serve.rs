//! Gentle Shell servers (`gentle-shell serve`, protocol
//! `gentle-shell.serve/v1`): the rules a client of one follows, without the
//! network. The server address a user types ([`normalize_url`]), what its
//! errors mean to them ([`error_text`]), its Server-Sent Events framing
//! ([`SseParser`]), the stream's cursor and what each event asks of the
//! client ([`Stream`]), and the Pi responses the stream repeats under the
//! server's own ids ([`RpcIds`]).

use std::collections::{HashMap, HashSet, VecDeque};

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
            Some("serve_dialog_resolved") => Step::Ignore,
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
            Step::Ignore
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
}
