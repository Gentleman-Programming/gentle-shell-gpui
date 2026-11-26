//! A Gentle Shell server's session as a backend (`PiProcess::serve`),
//! against a small stand-in for `gentle-shell serve`: plain HTTP/1.1 and a
//! Server-Sent Events stream, with the server's own ids on the stream's
//! copies of responses, `202 pending`, reconnects and `snapshot-required`;
//! finding the server a connection code names, or the servers on the
//! network (a stand-in UDP responder on 127.0.0.1), then pairing there; and
//! the same over pinned TLS with the pairing proof (a TLS front).

// Through a relay (`gentle-shell relay`).
#[path = "serve/relay.rs"]
mod relay;

// Forwarded pages through a proxy on this computer.
#[path = "serve/local_pages.rs"]
mod local_pages;

use std::io::{BufRead as _, BufReader, Read as _, Write as _};
use std::net::{SocketAddr, TcpListener, TcpStream, UdpSocket};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use gentle_core::serve::ConnectionCode;
use pi_rpc::serve::ServeClient;
use pi_rpc::{Incoming, PiProcess};
use serde_json::{Value, json};

const TOKEN: &str = "gss_test";
const SERVER_ID: &str = "srv_fake";
const PAIRING_CODE: &str = "ABCD-EFGH";

#[derive(Default)]
struct State {
    /// Every record the session printed: `(seq, json)`.
    records: Vec<(u64, String)>,
    /// Open event streams.
    subscribers: Vec<TcpStream>,
    /// `METHOD path` of every request, in order.
    log: Vec<String>,
    next_rpc: u64,
    /// The next stream connection answers `snapshot-required`.
    evicted: bool,
    running: bool,
    /// The pairing code was used.
    paired: bool,
    /// Every `/desktop` body, in order.
    desktop: Vec<Value>,
    /// The server predates `POST /api/tower`.
    no_tower: bool,
    /// The certificate a TLS front ([`TlsFront`]) shows for it: said in its
    /// identity, and what its pairing proofs bind the code to.
    fingerprint: Option<String>,
    /// Its pairing proofs are made up: it doesn't know the code.
    proof_lies: bool,
}

#[derive(Clone)]
struct Fake {
    url: String,
    state: Arc<Mutex<State>>,
}

impl Fake {
    fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let fake = Self {
            url,
            state: Arc::new(Mutex::new(State {
                running: true,
                ..State::default()
            })),
        };
        let server = fake.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(stream) = stream else { return };
                let server = server.clone();
                std::thread::spawn(move || server.serve(stream));
            }
        });
        fake
    }

    fn client(&self) -> ServeClient {
        ServeClient::new(&self.url, Some(TOKEN.into()))
    }

    /// The session prints `record`.
    fn emit(&self, record: Value) {
        let mut state = self.state.lock().unwrap();
        let seq = state.records.len() as u64 + 1;
        let line = record.to_string();
        state.records.push((seq, line.clone()));
        let frame = format!("id: {seq}\ndata: {line}\n\n");
        state
            .subscribers
            .retain_mut(|sub| sub.write_all(frame.as_bytes()).is_ok());
    }

    /// Close every open stream, as a dropped connection would.
    fn drop_streams(&self) {
        let mut state = self.state.lock().unwrap();
        for sub in state.subscribers.drain(..) {
            let _ = sub.shutdown(std::net::Shutdown::Both);
        }
    }

    fn log(&self) -> Vec<String> {
        self.state.lock().unwrap().log.clone()
    }

    fn wait_for(&self, what: &str, test: impl Fn(&State) -> bool) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while !test(&self.state.lock().unwrap()) {
            assert!(Instant::now() < deadline, "timed out waiting for {what}");
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    fn serve(&self, stream: TcpStream) {
        let mut reader = BufReader::new(stream.try_clone().unwrap());
        let mut line = String::new();
        if reader.read_line(&mut line).unwrap_or(0) == 0 {
            return;
        }
        let mut parts = line.split_whitespace();
        let method = parts.next().unwrap_or_default().to_string();
        let target = parts.next().unwrap_or_default().to_string();
        let mut length = 0;
        let mut authorized = false;
        loop {
            let mut header = String::new();
            reader.read_line(&mut header).unwrap();
            let header = header.trim_end();
            if header.is_empty() {
                break;
            }
            let (name, value) = header.split_once(':').unwrap();
            match name.to_ascii_lowercase().as_str() {
                "content-length" => length = value.trim().parse().unwrap(),
                "authorization" => authorized = value.trim() == format!("Bearer {TOKEN}"),
                _ => {}
            }
        }
        let mut body = vec![0; length];
        reader.read_exact(&mut body).unwrap();
        let body: Value = serde_json::from_slice(&body).unwrap_or_default();
        self.state
            .lock()
            .unwrap()
            .log
            .push(format!("{method} {target}"));
        let mut stream = stream;
        // Who the server is, and pairing: no token.
        match (method.as_str(), target.as_str()) {
            ("GET", "/.well-known/gentle-shell") => {
                let mut identity = json!({
                    "schema": "gentle-shell.serve.identity/v1",
                    "serverId": SERVER_ID,
                    "name": "fake",
                    "protocol": "gentle-shell.serve/v1",
                    "version": "0.0.0",
                    "tls": false
                });
                if let Some(fp) = &self.state.lock().unwrap().fingerprint {
                    identity["tls"] = true.into();
                    identity["fp"] = fp.clone().into();
                }
                return reply(&mut stream, 200, &identity);
            }
            ("POST", "/api/pair/proof") => {
                use base64::Engine as _;
                let b64 = base64::engine::general_purpose::URL_SAFE_NO_PAD;
                let state = self.state.lock().unwrap();
                let fp = gentle_core::serve::fingerprint_bytes(state.fingerprint.as_ref().unwrap())
                    .unwrap();
                let nonce = b64.decode(body["nonce"].as_str().unwrap()).unwrap();
                assert!((16..=64).contains(&nonce.len()));
                let code = if state.proof_lies {
                    "ZZZZ-ZZZZ"
                } else {
                    PAIRING_CODE
                };
                let key = gentle_core::serve::pair_proof_key(code, &fp);
                let proof = gentle_core::serve::pair_proof(&key, &fp, &nonce);
                return reply(&mut stream, 200, &json!({"proofs": [b64.encode(proof)]}));
            }
            ("POST", "/api/pair") => {
                let mut state = self.state.lock().unwrap();
                return if !state.paired && body["code"] == PAIRING_CODE {
                    state.paired = true;
                    reply(
                        &mut stream,
                        201,
                        &json!({"id": "dev_1", "name": body["name"], "token": TOKEN}),
                    )
                } else {
                    reply(
                        &mut stream,
                        403,
                        &json!({"error": {"code": "invalid_code", "message": "Invalid pairing code"}}),
                    )
                };
            }
            _ => {}
        }
        if !authorized {
            return reply(
                &mut stream,
                401,
                &json!({"error": {"code": "unauthorized", "message": "no"}}),
            );
        }
        let (path, query) = target.split_once('?').unwrap_or((&target, ""));
        match (method.as_str(), path) {
            ("DELETE", "/api/devices/self") => {
                reply(&mut stream, 200, &json!({"revoked": "dev_1"}));
            }
            ("POST", "/api/devices/self/replace") => {
                if body["token"] == "gss_earlier" {
                    reply(&mut stream, 200, &json!({"replaced": "dev_0"}));
                } else {
                    reply(
                        &mut stream,
                        404,
                        &json!({"error": {"code": "not_found", "message": "No paired device has that token"}}),
                    );
                }
            }
            ("POST", "/api/sessions") => {
                assert_eq!(body["cwd"], "/srv/project");
                reply(&mut stream, 201, &json!({"session": session(true)}));
            }
            ("GET", "/api/sessions/ses_1") => {
                let running = self.state.lock().unwrap().running;
                reply(&mut stream, 200, &json!({"session": session(running)}));
            }
            ("DELETE", "/api/sessions/ses_1") => {
                self.state.lock().unwrap().running = false;
                self.emit(json!({"type": "serve_session_exited", "code": 0, "signal": null}));
                self.drop_streams();
                reply(&mut stream, 200, &json!({"session": session(false)}));
            }
            ("GET", "/api/sessions/ses_gone") => reply(
                &mut stream,
                404,
                &json!({"error": {"code": "not_found", "message": "No such session"}}),
            ),
            ("POST", "/api/sessions/ses_1/rpc") => self.rpc(&mut stream, body),
            ("POST", "/api/sessions/ses_1/desktop") => {
                self.state.lock().unwrap().desktop.push(body.clone());
                let answer = |ok: bool, data: Value| {
                    let mut answer = json!({"schema": "gentle-shell.desktop-rpc/v1", "kind": "response", "requestId": "serve-desktop-1", "ok": ok});
                    answer[if ok { "data" } else { "error" }] = data;
                    answer
                };
                match (body["method"].as_str(), body["params"]["callId"].as_str()) {
                    (Some("set_host_tools"), _) => reply(
                        &mut stream,
                        200,
                        &answer(true, json!({"registered": ["open_page"], "rejected": []})),
                    ),
                    (_, Some("foreign")) => reply(
                        &mut stream,
                        403,
                        &json!({"error": {"code": "forbidden", "message": "Another device provides that call"}}),
                    ),
                    (_, Some("done")) => reply(
                        &mut stream,
                        200,
                        &answer(
                            false,
                            json!({"code": "already_answered", "message": "answered"}),
                        ),
                    ),
                    _ => reply(&mut stream, 200, &answer(true, json!({"accepted": true}))),
                }
            }
            ("GET", "/api/server") => reply(
                &mut stream,
                200,
                &json!({"serverId": SERVER_ID, "version": "0.0.0", "home": "/home/ana"}),
            ),
            ("POST", "/api/sessions/ses_1/forwards") => {
                match (body["port"].as_u64(), body["path"].as_str()) {
                    (Some(8787), _) => reply(
                        &mut stream,
                        400,
                        &json!({"error": {"code": "invalid_port", "message": "That port is the server's own"}}),
                    ),
                    (Some(port), Some(path)) => reply(
                        &mut stream,
                        201,
                        &json!({
                            "id": "fwd_1",
                            "port": port,
                            "listenPort": 40001,
                            "sessionId": "ses_1",
                            "url": format!("http://127.0.0.1:40001{path}?gs_ticket=t1"),
                            "expiresAt": "2026-01-01T00:01:00.000Z"
                        }),
                    ),
                    _ => reply(
                        &mut stream,
                        400,
                        &json!({"error": {"code": "invalid_port"}}),
                    ),
                }
            }
            ("POST", "/api/tower") if self.state.lock().unwrap().no_tower => reply(
                &mut stream,
                404,
                &json!({"error": {"code": "not_found", "message": "Not found"}}),
            ),
            ("POST", "/api/tower") => reply(
                &mut stream,
                201,
                &json!({
                    "id": "fwd_2",
                    "port": 47321,
                    "listenPort": 40002,
                    "sessionId": null,
                    "url": "http://127.0.0.1:40002/tok/?gs_ticket=t2",
                    "started": true
                }),
            ),
            ("GET", "/api/sessions/ses_1/events") => self.subscribe(stream, query),
            ("POST", "/api/desktop") => {
                self.state.lock().unwrap().desktop.push(body.clone());
                if body["method"] == "set_api_key" {
                    return reply(
                        &mut stream,
                        403,
                        &json!({"error": {"code": "forbidden", "message": "This device may only read"}}),
                    );
                }
                let answer = json!({"schema": "gentle-shell.desktop-rpc/v1", "kind": "response", "requestId": "serve-desktop-9", "ok": true, "data": {"method": body["method"], "params": body["params"]}});
                reply(&mut stream, 200, &answer);
            }
            ("GET", "/api/desktop/events") => self.subscribe(stream, query),
            _ => reply(&mut stream, 404, &json!({"error": {"code": "not_found"}})),
        }
    }

    /// The server sends the command under its own id: the stream gets the
    /// response under that id, the caller under its own.
    fn rpc(&self, stream: &mut TcpStream, command: Value) {
        let server_id = {
            let mut state = self.state.lock().unwrap();
            state.next_rpc += 1;
            format!("serve-rpc-{}", state.next_rpc)
        };
        let kind = command["type"].as_str().unwrap().to_string();
        let answer = |id: &str| match kind.as_str() {
            "fail" => {
                json!({"type": "response", "id": id, "command": kind, "success": false, "error": "nope"})
            }
            _ => {
                json!({"type": "response", "id": id, "command": kind, "success": true, "data": {"ok": kind}})
            }
        };
        match kind.as_str() {
            // Another device answered `taken` first.
            "extension_ui_response" if command["id"] == "taken" => reply(
                stream,
                409,
                &json!({"error": {"code": "dialog_answered", "message": "Another client already answered this dialog"}}),
            ),
            "extension_ui_response" => reply(stream, 202, &json!({"accepted": true})),
            // Longer than the server waits: answered in the stream only.
            "bash" => {
                reply(stream, 202, &json!({"pending": true, "id": server_id}));
                std::thread::sleep(Duration::from_millis(100));
                self.emit(answer(&server_id));
            }
            _ => {
                self.emit(answer(&server_id));
                reply(stream, 200, &answer(command["id"].as_str().unwrap()));
            }
        }
    }

    fn subscribe(&self, mut stream: TcpStream, query: &str) {
        let after = query
            .strip_prefix("after=")
            .map(|after| after.parse::<u64>().unwrap());
        let head =
            "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\n";
        let mut state = self.state.lock().unwrap();
        let seq = state.records.len();
        let mut frames = format!(
            "retry: 3000\n\nevent: hello\ndata: {}\n\n",
            json!({"session": "ses_1", "state": "running", "seq": seq, "oldest": 1})
        );
        if std::mem::take(&mut state.evicted) {
            frames.push_str(&format!(
                "event: snapshot-required\ndata: {}\n\n",
                json!({"after": after, "oldest": seq, "seq": seq})
            ));
        } else if let Some(after) = after {
            for (n, line) in state.records.iter().filter(|(n, _)| *n > after) {
                frames.push_str(&format!("id: {n}\ndata: {line}\n\n"));
            }
        }
        if stream.write_all(head.as_bytes()).is_ok() && stream.write_all(frames.as_bytes()).is_ok()
        {
            state.subscribers.push(stream);
        }
    }
}

fn session(running: bool) -> Value {
    json!({"id": "ses_1", "name": "project", "state": if running { "running" } else { "exited" }, "seq": 0})
}

fn reply(stream: &mut TcpStream, status: u16, body: &Value) {
    let body = body.to_string();
    let _ = write!(
        stream,
        "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
}

/// The next record that isn't a diagnostic line, within a few seconds.
fn next(rx: &async_channel::Receiver<Incoming>, stderr: &mut Vec<String>) -> Incoming {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        match rx.try_recv() {
            Ok(Incoming::Stderr(line)) => stderr.push(line),
            Ok(incoming) => return incoming,
            Err(_) => {
                assert!(Instant::now() < deadline, "timed out waiting for a record");
                std::thread::sleep(Duration::from_millis(10));
            }
        }
    }
}

#[test]
fn a_server_session_reads_and_answers_like_a_child() {
    let fake = Fake::start();
    let (process, rx) = PiProcess::serve(fake.client(), "/srv/project", None, None).unwrap();
    assert_eq!(process.server_session(), Some("ses_1"));
    assert_eq!(process.pid(), None);
    fake.wait_for("the stream", |s| !s.subscribers.is_empty());
    let mut stderr = Vec::new();

    // The HTTP answer settles the request; the stream's copy is skipped.
    let state = pollster::block_on(process.request(json!({"type": "get_state"}))).unwrap();
    assert_eq!(state.data["ok"], "get_state");
    let failed = pollster::block_on(process.request(json!({"type": "fail"})));
    assert_eq!(failed.unwrap_err().to_string(), "nope");

    // A command longer than the server waits is settled from the stream.
    let bash = pollster::block_on(process.request(json!({"type": "bash", "command": "true"})));
    assert_eq!(bash.unwrap().data["ok"], "bash");

    fake.emit(json!({"type": "agent_start"}));
    let Incoming::Event(event) = next(&rx, &mut stderr) else {
        panic!("expected the event")
    };
    assert_eq!(event["type"], "agent_start");

    // Dialog answers go through /rpc too.
    process
        .respond_ui("d1", pi_rpc::UiAnswer::Confirmed(true))
        .unwrap();
    fake.wait_for("the dialog answer", |s| {
        s.log.iter().filter(|l| l.ends_with("/rpc")).count() == 4
    });

    // A dropped connection resumes after the last record seen: what the
    // session printed meanwhile arrives once.
    let seen = fake.state.lock().unwrap().records.len();
    fake.drop_streams();
    fake.emit(json!({"type": "agent_end"}));
    let Incoming::Event(event) = next(&rx, &mut stderr) else {
        panic!("expected the replayed event")
    };
    assert_eq!(event["type"], "agent_end");
    assert!(
        fake.log()
            .contains(&format!("GET /api/sessions/ses_1/events?after={seen}")),
        "{:?}",
        fake.log()
    );

    // Records evicted while away: read the whole state again.
    fake.state.lock().unwrap().evicted = true;
    fake.drop_streams();
    assert!(matches!(next(&rx, &mut stderr), Incoming::Resync));

    // Only the failure the caller saw; no duplicate from the stream.
    assert!(
        stderr.iter().all(|line| !line.contains("nope")),
        "{stderr:?}"
    );

    // Closing the tab stops the session there.
    process.shutdown(Duration::from_secs(1));
    assert!(matches!(next(&rx, &mut stderr), Incoming::Exited(_)));
    fake.wait_for("the stop", |s| {
        s.log.contains(&"DELETE /api/sessions/ses_1".to_string())
    });
}

#[test]
fn the_app_quitting_leaves_the_session_for_the_next_launch() {
    let fake = Fake::start();
    let (process, _rx) = PiProcess::serve(fake.client(), "/srv/project", None, None).unwrap();
    fake.wait_for("the stream", |s| !s.subscribers.is_empty());
    assert!(process.detach());
    drop(process);
    std::thread::sleep(Duration::from_millis(300));
    assert!(!fake.log().iter().any(|l| l.starts_with("DELETE")));

    // The next launch reattaches to it instead of starting another.
    let (process, _rx) =
        PiProcess::serve(fake.client(), "/srv/project", Some("ses_1"), Some("pi-1")).unwrap();
    assert_eq!(process.server_session(), Some("ses_1"));
    assert_eq!(
        fake.log()
            .iter()
            .filter(|l| *l == "POST /api/sessions")
            .count(),
        1
    );
    process.detach();

    // A session the server no longer has: a new one on the same folder.
    let (process, _rx) =
        PiProcess::serve(fake.client(), "/srv/project", Some("ses_gone"), None).unwrap();
    assert_eq!(process.server_session(), Some("ses_1"));
    assert_eq!(
        fake.log()
            .iter()
            .filter(|l| *l == "POST /api/sessions")
            .count(),
        2
    );
    process.detach();
}

#[test]
fn the_servers_desktop_rpc_answers_like_a_session_without_one() {
    let fake = Fake::start();
    let (process, rx) = PiProcess::serve_desktop(fake.client()).unwrap();
    assert!(process.desktop_only());
    assert_eq!(process.server_session(), None);
    fake.wait_for("the stream", |s| !s.subscribers.is_empty());
    assert!(fake.log().contains(&"GET /api/desktop/events".to_string()));
    let mut stderr = Vec::new();

    // The Desktop command is there: the capability is the extension.
    let commands = pollster::block_on(process.request(json!({"type": "get_commands"}))).unwrap();
    assert_eq!(commands.data["commands"][0]["name"], "gentle:desktop");
    // No conversation: Pi RPC fails, nothing is sent.
    let state = pollster::block_on(process.request(json!({"type": "get_state"})));
    assert!(
        state
            .unwrap_err()
            .to_string()
            .contains("needs an agent session"),
        "a clear refusal"
    );

    // A Desktop prompt is answered as a session would: handled, then the
    // widget record under the app's own requestId.
    let ask = |request_id: &str, method: &str| {
        let inner = json!({"schema": "gentle-shell.desktop-rpc/v1", "requestId": request_id, "method": method, "params": {"store": "pi_settings"}});
        pollster::block_on(
            process
                .request(json!({"type": "prompt", "message": format!("/gentle:desktop {inner}")})),
        )
        .unwrap()
    };
    let widget_answer = |incoming: Incoming| -> Value {
        let Incoming::Ui(request) = incoming else {
            panic!("expected the Desktop widget")
        };
        let pi_rpc::UiMethod::SetWidget { key, lines, .. } = request.method else {
            panic!("expected setWidget")
        };
        assert_eq!(key, "gentle-desktop-rpc");
        serde_json::from_str(&lines.unwrap()[0]).unwrap()
    };
    assert_eq!(ask("app-1", "get_config").data["disposition"], "handled");
    let answer = widget_answer(next(&rx, &mut stderr));
    assert_eq!(answer["requestId"], "app-1");
    assert_eq!(answer["ok"], true);
    assert_eq!(answer["data"]["method"], "get_config");
    assert_eq!(
        fake.state.lock().unwrap().desktop.last().unwrap()["params"]["store"],
        "pi_settings"
    );
    // A refusal of the server is the answer's error, in its words.
    ask("app-2", "set_api_key");
    let refused = widget_answer(next(&rx, &mut stderr));
    assert_eq!(refused["requestId"], "app-2");
    assert_eq!(refused["ok"], false);
    assert_eq!(refused["error"]["code"], "forbidden");

    // Desktop events arrive from the server's stream.
    let event = json!({"schema": "gentle-shell.desktop-rpc/v1", "kind": "event", "seq": 1, "epoch": "e", "event": "login_prompt", "data": {}});
    fake.emit(json!({"type": "extension_ui_request", "id": "w-1", "method": "setWidget", "widgetKey": "gentle-desktop-rpc", "widgetLines": [event.to_string()]}));
    assert_eq!(
        widget_answer(next(&rx, &mut stderr))["event"],
        "login_prompt"
    );

    // Closing only lets go (the stream ends by the next heartbeat): no
    // session was started, none is stopped.
    process.shutdown(Duration::from_secs(1));
    assert!(pollster::block_on(process.request(json!({"type": "get_commands"}))).is_err());
    std::thread::sleep(Duration::from_millis(200));
    assert!(
        !fake
            .log()
            .iter()
            .any(|l| l.starts_with("DELETE") || l.starts_with("POST /api/sessions")),
        "{:?}",
        fake.log()
    );
}

#[test]
fn host_tool_requests_go_to_the_desktop_endpoint_and_calls_arrive_in_the_stream() {
    use pi_rpc::desktop_rpc::{DesktopRequest, HostTool, HostToolContent};
    let fake = Fake::start();
    let (process, rx) = PiProcess::serve(fake.client(), "/srv/project", None, None).unwrap();
    fake.wait_for("the stream", |s| !s.subscribers.is_empty());
    let tool = HostTool {
        name: "open_page".into(),
        label: "Open page".into(),
        description: "Open a page".into(),
        parameters: json!({"type": "object"}),
        guidelines: Vec::new(),
    };
    let request = DesktopRequest::set_host_tools("host-1", "desktop", &[tool]).unwrap();
    let answer = process.desktop(&request).unwrap();
    assert_eq!(
        answer.set_host_tools().unwrap().registered,
        vec!["open_page"]
    );
    let body = fake.state.lock().unwrap().desktop[0].clone();
    assert_eq!(body["method"], "set_host_tools");
    assert_eq!(body["params"]["provider"], "desktop");
    // Nothing went through /rpc.
    assert!(!fake.log().iter().any(|l| l.ends_with("/rpc")));

    let result = |call: &str| {
        DesktopRequest::host_tool_result(
            "host-2",
            call,
            &[HostToolContent::text("ok")],
            false,
            None,
        )
        .unwrap()
    };
    assert!(
        process
            .desktop(&result("c1"))
            .unwrap()
            .host_tool_result()
            .unwrap()
            .accepted
    );
    let answered = process.desktop(&result("done")).unwrap();
    assert_eq!(answered.error().unwrap().code, "already_answered");
    // Another device's call: refused by the server, never "answered".
    assert!(process.desktop(&result("foreign")).is_err());

    // A call the replay didn't carry is re-sent as a named frame: it reaches
    // the tab like any record.
    let line = json!({"schema": "gentle-shell.desktop-rpc/v1", "kind": "event", "seq": 1, "event": "host_tool_call", "data": {"callId": "c2", "provider": "dev_1", "tool": "open_page", "args": {}}}).to_string();
    let record = json!({"type": "extension_ui_request", "id": "w9", "method": "setWidget", "widgetKey": "gentle-desktop-rpc", "widgetLines": [line]});
    {
        let mut state = fake.state.lock().unwrap();
        let frame = format!("event: host_tool_call\ndata: {record}\n\n");
        state
            .subscribers
            .retain_mut(|sub| sub.write_all(frame.as_bytes()).is_ok());
    }
    let mut stderr = Vec::new();
    let Incoming::Ui(request) = next(&rx, &mut stderr) else {
        panic!("expected the re-sent call")
    };
    let Some(pi_rpc::desktop_rpc::DesktopRecord::Event(event)) =
        pi_rpc::desktop_rpc::decode_widget(&request).unwrap()
    else {
        panic!("expected a desktop event")
    };
    assert_eq!(event.host_tool_call().unwrap().call_id, "c2");
    process.detach();
}

#[test]
fn a_dialog_another_device_answered_closes_here() {
    let fake = Fake::start();
    let (process, rx) = PiProcess::serve(fake.client(), "/srv/project", None, None).unwrap();
    fake.wait_for("the stream", |s| !s.subscribers.is_empty());
    let mut stderr = Vec::new();

    // Another device answered a dialog this one shows.
    fake.emit(json!({"type": "serve_dialog_resolved", "id": "d1"}));
    let Incoming::DialogResolved { id, refused } = next(&rx, &mut stderr) else {
        panic!("expected the dialog to close")
    };
    assert_eq!((id.as_str(), refused), ("d1", false));

    // This device answered second: the server kept the other answer.
    process
        .respond_ui("taken", pi_rpc::UiAnswer::Confirmed(true))
        .unwrap();
    let Incoming::DialogResolved { id, refused } = next(&rx, &mut stderr) else {
        panic!("expected the refused answer")
    };
    assert_eq!((id.as_str(), refused), ("taken", true));
    // Not a failure to report.
    assert!(stderr.is_empty(), "{stderr:?}");
    process.detach();
}

/// An address nothing listens at any more.
fn gone() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    format!("http://{}", listener.local_addr().unwrap())
}

#[test]
fn a_server_that_moved_is_found_by_its_id() {
    let fake = Fake::start();
    let gone = gone();
    let plain = pi_rpc::serve::Trust::default();
    assert_eq!(
        pi_rpc::serve::find(SERVER_ID, &[gone.clone(), fake.url.clone()], &plain),
        Some(fake.url.clone())
    );
    // Another server at a known address isn't this one.
    assert_eq!(
        pi_rpc::serve::find("srv_other", std::slice::from_ref(&fake.url), &plain),
        None
    );
    assert_eq!(pi_rpc::serve::find(SERVER_ID, &[], &plain), None);
    assert_eq!(
        pi_rpc::serve::identify(&fake.url, &plain),
        Some((SERVER_ID.to_string(), "fake".to_string()))
    );
    assert_eq!(pi_rpc::serve::identify(&gone, &plain), None);
}

#[test]
fn a_client_follows_its_server_to_a_new_address() {
    let fake = Fake::start();
    let gone = gone();
    let asked = Arc::new(Mutex::new(Vec::<String>::new()));
    let relocate: pi_rpc::serve::Relocate = {
        let (asked, to) = (asked.clone(), fake.url.clone());
        Arc::new(move |failed: &str| {
            asked.lock().unwrap().push(failed.to_string());
            Some(to.clone())
        })
    };
    let client = ServeClient::new(&gone, Some(TOKEN.into())).relocating(relocate.clone());
    let copy = client.clone();
    // The request that couldn't connect is sent again where it went.
    assert_eq!(client.server().unwrap().server_id, SERVER_ID);
    assert_eq!(copy.url(), fake.url, "every clone follows");
    assert_eq!(*asked.lock().unwrap(), std::slice::from_ref(&gone));
    // Asked once: the next request goes there directly.
    copy.server().unwrap();
    assert_eq!(asked.lock().unwrap().len(), 1);

    // A session started at the old address runs at the new one: its
    // commands and its stream.
    let client = ServeClient::new(&gone, Some(TOKEN.into())).relocating(relocate);
    let (process, _rx) = PiProcess::serve(client, "/srv/project", None, None).unwrap();
    fake.wait_for("the stream", |s| !s.subscribers.is_empty());
    let state = pollster::block_on(process.request(json!({"type": "get_state"}))).unwrap();
    assert_eq!(state.data["ok"], "get_state");
    process.detach();

    // An answer from the server, even an error, is never a reason to move.
    let client = ServeClient::new(&fake.url, Some("gss_revoked".into()))
        .relocating(Arc::new(|_: &str| panic!("the server answered")));
    assert_eq!(client.server().unwrap_err().status, Some(401));
    // Nowhere else to go: the failure as it was.
    let client = ServeClient::new(&gone, Some(TOKEN.into())).relocating(Arc::new(|_: &str| None));
    let error = client.server().unwrap_err();
    assert!(error.message.starts_with("Couldn't reach"), "{error}");
    assert_eq!(client.url(), gone);
}

#[test]
fn a_revoked_token_says_so() {
    let fake = Fake::start();
    let client = ServeClient::new(&fake.url, Some("gss_revoked".into()));
    let error = PiProcess::serve(client, "/srv/project", None, None)
        .err()
        .unwrap();
    assert!(error.to_string().contains("Pair it again"), "{error}");
}

#[test]
fn unpairing_asks_the_server_to_remove_this_device() {
    let fake = Fake::start();
    fake.client().unpair().unwrap();
    assert!(
        fake.log()
            .iter()
            .any(|line| line == "DELETE /api/devices/self"),
        "{:?}",
        fake.log()
    );
    // A token the server no longer accepts: removed there already.
    let error = ServeClient::new(&fake.url, Some("gss_revoked".into()))
        .unpair()
        .unwrap_err();
    assert_eq!(error.status, Some(401));
}

#[test]
fn pairing_again_replaces_the_earlier_pairing_on_the_server() {
    let fake = Fake::start();
    fake.client().replace("gss_earlier").unwrap();
    assert!(
        fake.log()
            .iter()
            .any(|line| line == "POST /api/devices/self/replace"),
        "{:?}",
        fake.log()
    );
    let gone = fake.client().replace("gss_nobody").unwrap_err();
    assert_eq!(gone.status, Some(404));
}

/// A connection code for the fake: its port at `hosts`.
fn code_for(fake: &Fake, server_id: &str, hosts: &[&str]) -> ConnectionCode {
    let port: u16 = fake.url.rsplit_once(':').unwrap().1.parse().unwrap();
    ConnectionCode {
        server_id: server_id.into(),
        name: "omarchy".into(),
        port,
        urls: hosts
            .iter()
            .map(|host| format!("http://{host}:{port}"))
            .collect(),
        pairing_code: PAIRING_CODE.into(),
        fingerprint: None,
        relay: None,
    }
}

#[test]
fn a_connection_code_finds_its_server_and_pairs_there_once() {
    let fake = Fake::start();
    // A name that never resolves and an address nothing answers at come
    // first: the address that answers as the server wins all the same.
    let code = code_for(
        &fake,
        SERVER_ID,
        &["nowhere.invalid", "192.0.2.1", "127.0.0.1"],
    );
    let url = pi_rpc::serve::locate(&code).unwrap();
    assert_eq!(url, fake.url);
    let paired = pi_rpc::serve::pair_code(&code, &url, "Gentle Shell GPUI on test").unwrap();
    assert_eq!(paired.token, TOKEN);
    assert_eq!(paired.device, "dev_1");
    let again = pi_rpc::serve::pair_code(&code, &url, "Gentle Shell GPUI on test").unwrap_err();
    assert!(again.contains("expired or was already used"), "{again}");
    assert!(again.contains("omarchy"), "{again}");
}

#[test]
fn the_pairing_code_goes_only_to_the_server_named_in_the_code() {
    let fake = Fake::start();
    let code = code_for(&fake, "srv_restarted", &["127.0.0.1"]);
    let error = pi_rpc::serve::locate(&code).unwrap_err();
    assert!(error.contains("different Gentle Shell server"), "{error}");
    assert_eq!(fake.log(), vec!["GET /.well-known/gentle-shell"]);
}

#[test]
fn a_server_nowhere_on_the_network_says_what_to_check() {
    let fake = Fake::start();
    let code = code_for(&fake, SERVER_ID, &["nowhere.invalid"]);
    let error = pi_rpc::serve::locate(&code).unwrap_err();
    assert!(
        error.starts_with("Couldn't find omarchy on your network."),
        "{error}"
    );
    assert!(fake.log().is_empty());
}

/// A stand-in for a server's discovery responder on 127.0.0.1: every
/// datagram it gets is kept, and answered with each of `answers`.
fn responder(answers: Vec<Vec<u8>>) -> (SocketAddr, Arc<Mutex<Vec<Vec<u8>>>>) {
    let socket = UdpSocket::bind("127.0.0.1:0").unwrap();
    let addr = socket.local_addr().unwrap();
    let got = Arc::new(Mutex::new(Vec::new()));
    let kept = got.clone();
    std::thread::spawn(move || {
        let mut buf = [0u8; 2048];
        while let Ok((len, from)) = socket.recv_from(&mut buf) {
            kept.lock().unwrap().push(buf[..len].to_vec());
            for answer in &answers {
                let _ = socket.send_to(answer, from);
            }
        }
    });
    (addr, got)
}

fn here(server_id: &str, port: u16) -> Vec<u8> {
    json!({"t": "gentle-shell/here", "v": 1, "id": server_id, "n": "omarchy", "p": port})
        .to_string()
        .into_bytes()
}

fn port_of(fake: &Fake) -> u16 {
    fake.url.rsplit_once(':').unwrap().1.parse().unwrap()
}

#[test]
fn a_server_on_the_network_is_found_checked_and_paired_with_a_short_code() {
    let fake = Fake::start();
    let port = port_of(&fake);
    // The same answer twice is one server.
    let (addr, got) = responder(vec![here(SERVER_ID, port), here(SERVER_ID, port)]);
    let found = pi_rpc::serve::nearby(&[addr]).unwrap();
    assert_eq!(
        got.lock().unwrap().as_slice(),
        [br#"{"t":"gentle-shell/discover","v":1}"#.to_vec()]
    );
    assert_eq!(found.len(), 1);
    let server = &found[0];
    assert_eq!(server.server_id, SERVER_ID);
    assert_eq!(server.name, "omarchy");
    assert_eq!(server.url(), fake.url);
    let paired =
        pi_rpc::serve::pair_nearby(server, PAIRING_CODE, "Gentle Shell GPUI on test").unwrap();
    assert_eq!(paired.token, TOKEN);
    let again =
        pi_rpc::serve::pair_nearby(server, PAIRING_CODE, "Gentle Shell GPUI on test").unwrap_err();
    assert!(
        again.contains("On omarchy, run gentle-shell serve share"),
        "{again}"
    );
}

#[test]
fn an_answer_the_server_there_doesnt_confirm_is_dropped() {
    let fake = Fake::start();
    let port = port_of(&fake);
    // Claims an id the HTTP server there doesn't have, or a port nothing
    // listens on; junk and too-long datagrams are ignored.
    let closed = TcpListener::bind("127.0.0.1:0").unwrap();
    let closed_port = closed.local_addr().unwrap().port();
    drop(closed);
    let mut long = here(SERVER_ID, port);
    long.extend(std::iter::repeat_n(b' ', 600));
    let (addr, _) = responder(vec![
        here("srv_impostor", port),
        here(SERVER_ID, closed_port),
        b"hello".to_vec(),
        long,
    ]);
    let found = pi_rpc::serve::nearby(&[addr]).unwrap();
    assert!(found.is_empty(), "{found:?}");
    // Only asked who it is: no pairing code went anywhere.
    assert_eq!(fake.log(), vec!["GET /.well-known/gentle-shell"]);
}

#[test]
fn nothing_answering_finds_nothing() {
    let silent = UdpSocket::bind("127.0.0.1:0").unwrap();
    let started = Instant::now();
    let found = pi_rpc::serve::nearby(&[silent.local_addr().unwrap()]).unwrap();
    assert!(found.is_empty());
    assert!(started.elapsed() < Duration::from_secs(4));
}

#[test]
fn a_page_on_the_servers_loopback_opens_through_a_forward() {
    let fake = Fake::start();
    let client = fake.client();
    let forward = client.forward("ses_1", 5173, "/app?x=1").unwrap();
    assert_eq!(forward.id, "fwd_1");
    assert_eq!(forward.port, 5173);
    assert!(forward.url.starts_with("http://127.0.0.1:40001/app?x=1"));
    let refused = client.forward("ses_1", 8787, "/").unwrap_err();
    assert_eq!(refused.status, Some(400));
    assert_eq!(refused.error.as_deref(), Some("invalid_port"));
    assert!(
        refused.message.contains("server's own"),
        "{}",
        refused.message
    );
    let gone = client.forward("ses_gone", 5173, "/").unwrap_err();
    assert_eq!(gone.status, Some(404));
    assert!(client.forward("../x", 1, "/").is_err());

    let tower = client.tower().unwrap();
    assert_eq!(tower.url, "http://127.0.0.1:40002/tok/?gs_ticket=t2");
    assert_eq!(client.server().unwrap().home.as_deref(), Some("/home/ana"));
    assert!(
        fake.log()
            .iter()
            .any(|line| line == "POST /api/sessions/ses_1/forwards")
    );
    assert!(fake.log().iter().any(|line| line == "POST /api/tower"));
}

#[test]
fn a_server_without_tower_says_to_upgrade_it() {
    let fake = Fake::start();
    let client = ServeClient::new(&fake.url, Some("gss_revoked".into()));
    // Unpaired: the token error, not an upgrade hint.
    assert!(
        client
            .tower()
            .unwrap_err()
            .message
            .contains("Pair it again")
    );
    let old = Fake::start();
    old.state.lock().unwrap().no_tower = true;
    let error = old.client().tower().unwrap_err();
    assert!(
        error.message.contains("upgrade Gentle Shell"),
        "{}",
        error.message
    );
}

// --- TLS and the pairing proof ------------------------------------------------------

fn fixture(name: &str) -> Vec<u8> {
    std::fs::read(format!(
        "{}/tests/fixtures/serve-tls/{name}",
        env!("CARGO_MANIFEST_DIR")
    ))
    .unwrap()
}

/// TLS in front of a fake, as `gentle-shell serve` speaks it on the
/// network: a self-signed certificate (`tests/fixtures/serve-tls`), and
/// `400 tls_required` for a connection that doesn't start a handshake.
struct TlsFront {
    port: u16,
    fingerprint: String,
}

impl TlsFront {
    fn start(fake: &Fake, cert: &str) -> Self {
        Self::to(fake.url.trim_start_matches("http://"), cert)
    }

    /// TLS in front of the plain server at `backend` (`host:port`).
    fn to(backend: &str, cert: &str) -> Self {
        use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
        let der = fixture(&format!("cert-{cert}.der"));
        let key = PrivatePkcs8KeyDer::from(fixture(&format!("key-{cert}.der")));
        let fingerprint = gentle_core::serve::certificate_fingerprint(&der);
        let provider = Arc::new(rustls::crypto::ring::default_provider());
        let config = rustls::ServerConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions()
            .unwrap()
            .with_no_client_auth()
            .with_single_cert(vec![CertificateDer::from(der)], PrivateKeyDer::Pkcs8(key))
            .unwrap();
        let config = Arc::new(config);
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let backend = backend.to_string();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(stream) = stream else { return };
                let (config, backend) = (config.clone(), backend.clone());
                std::thread::spawn(move || terminate_tls(stream, config, &backend));
            }
        });
        Self { port, fingerprint }
    }

    fn url(&self) -> String {
        format!("https://127.0.0.1:{}", self.port)
    }
}

/// One connection to a [`TlsFront`]: its plaintext to and from the fake,
/// one blocking thread each way (no read timeouts: on Windows a timed-out
/// socket read leaves the socket unusable).
fn terminate_tls(mut client: TcpStream, config: Arc<rustls::ServerConfig>, backend: &str) {
    let mut first = [0u8; 1];
    if client.peek(&mut first).unwrap_or(0) == 0 {
        return;
    }
    if first[0] != 0x16 {
        // Read the request first, as the server does: closing on unread
        // bytes would reset the connection.
        let _ = client.read(&mut [0u8; 4096]);
        return reply(
            &mut client,
            400,
            &json!({"error": {"code": "tls_required", "message": "This server speaks HTTPS on the network"}}),
        );
    }
    let Ok(plain) = TcpStream::connect(backend) else {
        return;
    };
    let conn = Arc::new(Mutex::new(rustls::ServerConnection::new(config).unwrap()));
    // What rustls has to send, sent.
    fn flush(conn: &mut rustls::ServerConnection, to: &mut TcpStream) -> std::io::Result<()> {
        while conn.wants_write() {
            conn.write_tls(to)?;
        }
        Ok(())
    }
    // The fake's answers, encrypted; its end closes the TLS session.
    let (mut from_fake, mut to_client) = (plain.try_clone().unwrap(), client.try_clone().unwrap());
    let down = conn.clone();
    std::thread::spawn(move || {
        let mut buf = [0u8; 16 * 1024];
        loop {
            let n = from_fake.read(&mut buf).unwrap_or(0);
            let mut conn = down.lock().unwrap();
            if n == 0 {
                conn.send_close_notify();
                let _ = flush(&mut conn, &mut to_client);
                let _ = to_client.shutdown(std::net::Shutdown::Write);
                return;
            }
            if conn.writer().write_all(&buf[..n]).is_err()
                || flush(&mut conn, &mut to_client).is_err()
            {
                return;
            }
        }
    });
    // The client's handshake and requests, decrypted for the fake.
    let (mut from_client, mut to_fake) = (client, plain);
    let mut buf = [0u8; 16 * 1024];
    let mut text = [0u8; 16 * 1024];
    loop {
        let n = from_client.read(&mut buf).unwrap_or(0);
        if n == 0 {
            let _ = to_fake.shutdown(std::net::Shutdown::Write);
            return;
        }
        let mut conn = conn.lock().unwrap();
        let mut incoming = &buf[..n];
        while !incoming.is_empty() {
            if conn.read_tls(&mut incoming).is_err() || conn.process_new_packets().is_err() {
                let _ = flush(&mut conn, &mut from_client);
                return;
            }
        }
        loop {
            match conn.reader().read(&mut text) {
                Ok(0) => return,
                Ok(n) if to_fake.write_all(&text[..n]).is_err() => return,
                Ok(_) => {}
                Err(_) => break,
            }
        }
        if flush(&mut conn, &mut from_client).is_err() {
            return;
        }
    }
}

#[test]
fn a_server_on_https_answers_only_with_the_pinned_certificate() {
    use pi_rpc::serve::Trust;
    let fake = Fake::start();
    let front = TlsFront::start(&fake, "a");
    let other = TlsFront::start(&fake, "b").fingerprint;
    let pinned = Trust::pinned(&front.fingerprint, None);
    let client = ServeClient::with_trust(&front.url(), Some(TOKEN.into()), pinned.clone());
    assert_eq!(client.server().unwrap().server_id, SERVER_ID);
    assert_eq!(fake.log(), ["GET /api/server"]);

    // Another certificate at its address: refused before a byte of the
    // request (or the token) is sent, in words.
    let changed = Trust::pinned(&other, None);
    let error = ServeClient::with_trust(&front.url(), Some(TOKEN.into()), changed.clone())
        .relocating(Arc::new(|_: &str| {
            panic!("a changed certificate isn't a move")
        }))
        .server()
        .unwrap_err();
    assert_eq!(error.error.as_deref(), Some("certificate_changed"));
    assert_eq!(error.message, gentle_core::serve::CERTIFICATE_CHANGED);
    assert!(error.message.contains("pair again"));
    // Without a certificate to pin, HTTPS isn't spoken at all.
    let error = ServeClient::new(&front.url(), Some(TOKEN.into()))
        .server()
        .unwrap_err();
    assert!(error.message.contains("pair it again"), "{}", error.message);
    assert_eq!(fake.log(), ["GET /api/server"]);

    // Looking for a server that moved checks the certificate too.
    assert_eq!(
        pi_rpc::serve::find(SERVER_ID, &[gone(), front.url()], &pinned),
        Some(front.url())
    );
    assert_eq!(
        pi_rpc::serve::find(SERVER_ID, &[front.url()], &changed),
        None
    );
}

#[test]
fn a_typed_address_moves_to_https_and_the_code_waits_for_the_proof() {
    let fake = Fake::start();
    let front = TlsFront::start(&fake, "a");
    fake.state.lock().unwrap().fingerprint = Some(front.fingerprint.clone());
    // Typed as plain HTTP: the server refuses it from the network, so the
    // pairing moves to HTTPS.
    let typed = format!("http://127.0.0.1:{}", front.port);
    let (url, paired) = pi_rpc::serve::pair_at(&typed, PAIRING_CODE, "test").unwrap();
    assert_eq!(url, front.url());
    assert_eq!(paired.token, TOKEN);
    assert_eq!(
        paired.trust,
        pi_rpc::serve::Trust::pinned(&front.fingerprint, None)
    );
    assert_eq!(fake.log(), ["POST /api/pair/proof", "POST /api/pair"]);
    assert_eq!(
        pi_rpc::serve::identify(&url, &paired.trust),
        Some((SERVER_ID.to_string(), "fake".to_string()))
    );
}

#[test]
fn a_server_that_cant_prove_the_code_never_gets_it() {
    let fake = Fake::start();
    let front = TlsFront::start(&fake, "a");
    {
        let mut state = fake.state.lock().unwrap();
        state.fingerprint = Some(front.fingerprint.clone());
        state.proof_lies = true;
    }
    let error = pi_rpc::serve::pair_at(&front.url(), PAIRING_CODE, "test").unwrap_err();
    assert_eq!(error, gentle_core::serve::proof_failed("127.0.0.1"));

    // Found on the network: the certificate checked is the one the
    // handshake shows, whatever the reply announced.
    let other = TlsFront::start(&fake, "b").fingerprint;
    let mut announce: Value = serde_json::from_slice(&here(SERVER_ID, front.port)).unwrap();
    announce["tls"] = true.into();
    announce["fp"] = other.into();
    let (addr, _) = responder(vec![announce.to_string().into_bytes()]);
    let found = pi_rpc::serve::nearby(&[addr]).unwrap();
    assert_eq!(found.len(), 1);
    assert_eq!(
        found[0].fingerprint.as_deref(),
        Some(front.fingerprint.as_str())
    );
    assert_eq!(found[0].url(), front.url());
    let error = pi_rpc::serve::pair_nearby(&found[0], PAIRING_CODE, "test").unwrap_err();
    assert_eq!(error, gentle_core::serve::proof_failed("omarchy"));
    assert!(
        !fake.log().iter().any(|l| l == "POST /api/pair"),
        "the code went out: {:?}",
        fake.log()
    );

    // Once it proves the code, it is paired with, pinned.
    fake.state.lock().unwrap().proof_lies = false;
    let paired = pi_rpc::serve::pair_nearby(&found[0], PAIRING_CODE, "test").unwrap();
    assert_eq!(paired.trust.fingerprint, Some(front.fingerprint.clone()));
}
