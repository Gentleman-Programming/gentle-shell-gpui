//! A Gentle Shell server's session as a backend (`PiProcess::serve`),
//! against a small stand-in for `gentle-shell serve`: plain HTTP/1.1 and a
//! Server-Sent Events stream, with the server's own ids on the stream's
//! copies of responses, `202 pending`, reconnects and `snapshot-required`.

use std::io::{BufRead as _, BufReader, Read as _, Write as _};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use pi_rpc::serve::ServeClient;
use pi_rpc::{Incoming, PiProcess};
use serde_json::{Value, json};

const TOKEN: &str = "gss_test";

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
        if !authorized {
            return reply(
                &mut stream,
                401,
                &json!({"error": {"code": "unauthorized", "message": "no"}}),
            );
        }
        let (path, query) = target.split_once('?').unwrap_or((&target, ""));
        match (method.as_str(), path) {
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
            ("GET", "/api/sessions/ses_1/events") => self.subscribe(stream, query),
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
fn a_revoked_token_says_so() {
    let fake = Fake::start();
    let client = ServeClient::new(&fake.url, Some("gss_revoked".into()));
    let error = PiProcess::serve(client, "/srv/project", None, None)
        .err()
        .unwrap();
    assert!(error.to_string().contains("Pair it again"), "{error}");
}
