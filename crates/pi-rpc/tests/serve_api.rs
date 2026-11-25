//! A Gentle Shell server's API around sessions — files, git, uploads,
//! services and server-level forwards, terminals and the terminal host —
//! against a small stand-in for `gentle-shell serve` that answers as
//! Gentle Shell's `docs/serve.md` says.

use std::io::{BufRead as _, BufReader, Read as _, Write as _};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use pi_rpc::serve::{EntryKind, ForwardedPage, ServeClient, TermEvent, WatchEvent};
use serde_json::{Value, json};

const TOKEN: &str = "gss_test";
const FILE: &[u8] = b"hello, server world";

/// One request as the fake read it.
#[derive(Debug, Clone)]
struct Request {
    method: String,
    target: String,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

impl Request {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(n, _)| n.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }

    fn json(&self) -> Value {
        serde_json::from_slice(&self.body).unwrap_or_default()
    }

    /// The decoded value of `key` in the query.
    fn query(&self, key: &str) -> Option<String> {
        let (_, query) = self.target.split_once('?')?;
        query.split('&').find_map(|pair| {
            let (k, v) = pair.split_once('=')?;
            (k == key).then(|| decode(v))
        })
    }

    fn path(&self) -> &str {
        self.target.split('?').next().unwrap_or_default()
    }
}

fn decode(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            out.push(u8::from_str_radix(&value[i + 1..i + 3], 16).unwrap());
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).unwrap()
}

#[derive(Clone)]
struct Fake {
    url: String,
    log: Arc<Mutex<Vec<Request>>>,
}

impl Fake {
    fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let fake = Self {
            url: format!("http://{}", listener.local_addr().unwrap()),
            log: Arc::default(),
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

    fn requests(&self, path: &str) -> Vec<Request> {
        self.log
            .lock()
            .unwrap()
            .iter()
            .filter(|r| r.path() == path)
            .cloned()
            .collect()
    }

    fn serve(&self, stream: TcpStream) {
        let Some(request) = read_request(&stream) else {
            return;
        };
        self.log.lock().unwrap().push(request.clone());
        let mut stream = stream;
        if request.header("authorization") != Some(&format!("Bearer {TOKEN}")) {
            return reply(
                &mut stream,
                401,
                &json!({"error": {"code": "unauthorized"}}),
            );
        }
        let body = request.json();
        match (request.method.as_str(), request.path()) {
            ("GET", "/api/server") => reply(
                &mut stream,
                200,
                &json!({
                    "serverId": "srv_fake",
                    "capabilities": ["files", "git", "uploads", "services", "server-forwards"],
                    "platform": "linux",
                    "arch": "x64",
                    "pathSeparator": "/",
                    "home": "/home/ana"
                }),
            ),
            ("GET", "/api/fs/list") => {
                assert_eq!(request.query("path").as_deref(), Some("/srv/my project"));
                reply(
                    &mut stream,
                    200,
                    &json!({"path": "/srv/my project", "truncated": false, "entries": [
                        {"name": "src", "kind": "dir", "size": 64, "mtime": 1.5e12},
                        {"name": "docs", "kind": "symlink", "size": 4, "mtime": 0, "linkKind": "dir"},
                        {"name": "a.rs", "kind": "file", "size": 19, "mtime": 1700000000000u64},
                    ]}),
                )
            }
            ("GET", "/api/fs/stat") => match request.query("path").as_deref() {
                Some("/srv/p/a.rs") => reply(
                    &mut stream,
                    200,
                    &json!({"path": "/srv/p/a.rs", "name": "a.rs", "kind": "file", "size": FILE.len(), "mtime": 5, "mode": 420, "etag": "e1", "realPath": "/srv/p/a.rs"}),
                ),
                _ => reply(
                    &mut stream,
                    404,
                    &json!({"error": {"code": "not_found", "message": "No such file"}}),
                ),
            },
            // Five bytes at a time, as a server with a small cap would.
            ("GET", "/api/fs/read") => {
                let offset: usize = request.query("offset").unwrap().parse().unwrap();
                let end = (offset + 5).min(FILE.len());
                let chunk = &FILE[offset..end];
                let truncated = if end < FILE.len() {
                    "X-Truncated: 1\r\n"
                } else {
                    ""
                };
                let _ = write!(
                    stream,
                    "HTTP/1.1 200 OK\r\nContent-Type: application/octet-stream\r\nETag: e1\r\nX-File-Size: {}\r\nX-File-Mtime: 5\r\nX-Offset: {offset}\r\n{truncated}Content-Length: {}\r\nConnection: close\r\n\r\n",
                    FILE.len(),
                    chunk.len()
                );
                let _ = stream.write_all(chunk);
            }
            ("PUT", "/api/fs/write") => match request.header("if-match") {
                Some("stale") => reply(
                    &mut stream,
                    412,
                    &json!({"error": {"code": "changed", "message": "The file changed"}}),
                ),
                _ if request.query("create_only").as_deref() == Some("1") => reply(
                    &mut stream,
                    409,
                    &json!({"error": {"code": "exists", "message": "Already exists"}}),
                ),
                _ => reply(
                    &mut stream,
                    200,
                    &json!({"path": request.query("path"), "size": request.body.len(), "mtime": 6, "etag": "e2", "created": false}),
                ),
            },
            ("POST", "/api/fs/mkdir") => reply(
                &mut stream,
                201,
                &json!({"path": body["path"], "created": true}),
            ),
            ("POST", "/api/fs/rename") => reply(
                &mut stream,
                200,
                &json!({"from": body["from"], "to": body["to"]}),
            ),
            ("POST", "/api/fs/remove") => reply(&mut stream, 200, &json!({"removed": true})),
            ("GET", "/api/fs/search") => reply(
                &mut stream,
                200,
                &json!({"root": "/srv/p", "truncated": false, "results": [
                    {"path": "/srv/p/src/main.rs", "name": "main.rs", "kind": "file", "score": 3}
                ]}),
            ),
            ("GET", "/api/fs/watch") => sse(
                &mut stream,
                &[
                    ("ready", json!({"path": "/srv/p", "recursive": true})),
                    ("changes", json!({"paths": ["/srv/p/a.rs"]})),
                    ("changes", json!({"paths": [], "overflow": true})),
                    ("error", json!({"message": "inotify limit"})),
                ],
            ),
            ("POST", "/api/git") => {
                let args: Vec<String> = serde_json::from_value(body["args"].clone()).unwrap();
                let stdout = format!("{}\n", args.join(" "));
                reply(
                    &mut stream,
                    200,
                    &json!({
                        "code": if args[0] == "fail" { 1 } else { 0 },
                        "signal": null,
                        "stdout": STANDARD.encode(stdout),
                        "stderr": STANDARD.encode("warn\n"),
                        "truncated": false,
                        "timedOut": false
                    }),
                )
            }
            ("POST", "/api/print") if body["prompt"] == "fail" => reply(
                &mut stream,
                502,
                &json!({"error": {"code": "print_failed", "message": "No API key for the default model"}, "exitCode": 1}),
            ),
            ("POST", "/api/print") => reply(
                &mut stream,
                200,
                &json!({"text": "feat: add a thing\n\nBecause.\n"}),
            ),
            ("POST", "/api/uploads") => reply(
                &mut stream,
                201,
                &json!({
                    "path": format!("/home/ana/.gentle-shell/agent/serve/uploads/dev_1/0a/{}", request.query("name").unwrap()),
                    "name": request.query("name"),
                    "size": request.body.len(),
                    "sha256": "x",
                    "expiresAt": "2026-01-01T00:00:00Z"
                }),
            ),
            ("GET", "/api/services") => reply(
                &mut stream,
                200,
                &json!({"services": [
                    {"id": "engram", "name": "Engram", "url": "http://127.0.0.1:7437", "port": 7437, "available": true, "forwardable": true, "via": "forwards"},
                    {"id": "tower", "name": "Gentle Tower", "port": 41234, "available": true, "forwardable": true, "via": "tower"}
                ]}),
            ),
            ("POST", "/api/forwards") => reply(
                &mut stream,
                201,
                &json!({"id": "fwd_9", "port": body["port"], "listenPort": 40009, "sessionId": null, "kind": "server",
                        "url": format!("http://127.0.0.1:40009{}?gs_ticket=t9", body["path"].as_str().unwrap())}),
            ),
            ("GET", "/api/terminals") => {
                reply(&mut stream, 200, &json!({"terminals": [terminal(true)]}))
            }
            ("POST", "/api/terminals") => {
                assert_eq!(body["cwd"], "/srv/p");
                reply(&mut stream, 201, &json!({"terminal": terminal(true)}))
            }
            ("POST", "/api/terminals/t_1/input" | "/api/terminals/t_1/resize") => {
                reply(&mut stream, 200, &json!({"ok": true}))
            }
            ("DELETE", "/api/terminals/t_1") => reply(&mut stream, 200, &json!({"closed": true})),
            ("GET", "/api/terminals/t_1/stream") => {
                let snapshot = request.query("from").is_none();
                let mut attached = json!({"offset": 10});
                if snapshot {
                    attached["snapshot"] = STANDARD.encode("\x1b[2J$ ").into();
                }
                sse(
                    &mut stream,
                    &[
                        ("attached", attached),
                        (
                            "output",
                            json!({"offset": 13, "data": STANDARD.encode("hi\n")}),
                        ),
                        ("title", json!({"title": "zsh"})),
                        ("resized", json!({"cols": 100, "rows": 30})),
                        ("exit", json!({"code": 0})),
                    ],
                )
            }
            ("GET", "/api/terminals/t_gone/stream") => {
                sse(&mut stream, &[("error", json!({"code": "term_host_gone"}))])
            }
            ("GET", "/api/term-host") => reply(
                &mut stream,
                200,
                &json!({"available": false, "running": false, "platform": "darwin", "arch": "arm64"}),
            ),
            ("PUT", "/api/term-host") => {
                use sha2::Digest as _;
                let sha: String = sha2::Sha256::digest(&request.body)
                    .iter()
                    .map(|b| format!("{b:02x}"))
                    .collect();
                if request.header("x-sha256") == Some(sha.as_str()) {
                    reply(
                        &mut stream,
                        201,
                        &json!({"path": "/home/ana/.gentle-shell/agent/serve/bin/gentle-term-host", "sha256": sha, "size": request.body.len(), "active": true}),
                    )
                } else {
                    reply(
                        &mut stream,
                        400,
                        &json!({"error": {"code": "sha256_mismatch"}}),
                    )
                }
            }
            _ => reply(&mut stream, 404, &json!({"error": {"code": "not_found"}})),
        }
    }
}

fn terminal(alive: bool) -> Value {
    json!({"id": "t_1", "pid": 7, "cwd": "/srv/p", "argv": ["/bin/zsh", "-l"], "cols": 80, "rows": 24,
           "title": "", "alive": alive, "createdAt": 1, "offset": 10, "owner": "dev_1"})
}

fn read_request(stream: &TcpStream) -> Option<Request> {
    let mut reader = BufReader::new(stream.try_clone().unwrap());
    let mut line = String::new();
    if reader.read_line(&mut line).unwrap_or(0) == 0 {
        return None;
    }
    let mut parts = line.split_whitespace();
    let method = parts.next()?.to_string();
    let target = parts.next()?.to_string();
    let mut headers = Vec::new();
    let mut length = 0;
    loop {
        let mut header = String::new();
        reader.read_line(&mut header).ok()?;
        let header = header.trim_end();
        if header.is_empty() {
            break;
        }
        let (name, value) = header.split_once(':')?;
        if name.eq_ignore_ascii_case("content-length") {
            length = value.trim().parse().ok()?;
        }
        headers.push((name.to_string(), value.trim().to_string()));
    }
    let mut body = vec![0; length];
    reader.read_exact(&mut body).ok()?;
    Some(Request {
        method,
        target,
        headers,
        body,
    })
}

fn reply(stream: &mut TcpStream, status: u16, body: &Value) {
    let body = body.to_string();
    let _ = write!(
        stream,
        "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
}

/// An SSE answer with `events`, then the connection closes.
fn sse(stream: &mut TcpStream, events: &[(&str, Value)]) {
    let mut out = String::from(
        "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\n: ping\n\n",
    );
    for (event, data) in events {
        if let Some(offset) = data["offset"].as_u64().filter(|_| *event == "output") {
            out.push_str(&format!("id: {offset}\n"));
        }
        out.push_str(&format!("event: {event}\ndata: {data}\n\n"));
    }
    let _ = stream.write_all(out.as_bytes());
}

#[test]
fn the_server_says_what_it_offers_and_where() {
    let info = Fake::start().client().server().unwrap();
    assert!(info.has("files") && info.has("git") && !info.has("terminals"));
    assert_eq!(info.platform.as_deref(), Some("linux"));
    assert_eq!(info.arch.as_deref(), Some("x64"));
    assert_eq!(info.path_separator.as_deref(), Some("/"));
    assert_eq!(info.home.as_deref(), Some("/home/ana"));
}

#[test]
fn folders_and_files_are_read_on_the_server() {
    let fake = Fake::start();
    let client = fake.client();
    let listing = client.fs_list("/srv/my project").unwrap();
    let names: Vec<(&str, bool)> = listing
        .entries
        .iter()
        .map(|e| (e.name.as_str(), e.is_dir()))
        .collect();
    assert_eq!(names, [("src", true), ("docs", true), ("a.rs", false)]);
    assert_eq!(listing.entries[0].mtime, 1_500_000_000_000);
    // The space travels encoded.
    assert!(fake.requests("/api/fs/list")[0].target.contains("%20"));

    let stat = client.fs_stat("/srv/p/a.rs").unwrap();
    assert_eq!(stat.entry.kind, EntryKind::File);
    assert_eq!(stat.etag.as_deref(), Some("e1"));
    let missing = client.fs_stat("/srv/p/nope").unwrap_err();
    assert_eq!(missing.status, Some(404));

    // Chunk after chunk until the server says nothing remains.
    let whole = client.fs_read_all("/srv/p/a.rs", 1024).unwrap();
    assert_eq!(whole.bytes, FILE);
    assert_eq!(whole.etag.as_deref(), Some("e1"));
    assert!(!whole.truncated);
    assert_eq!(fake.requests("/api/fs/read").len(), 4);
    let too_large = client.fs_read_all("/srv/p/a.rs", 4).unwrap_err();
    assert_eq!(too_large.status, Some(413));

    let hits = client.fs_search("/srv/p", "main", 50).unwrap();
    assert_eq!(hits[0].path, "/srv/p/src/main.rs");
    assert_eq!(hits[0].kind, EntryKind::File);
}

#[test]
fn writes_are_guarded_by_the_files_tag() {
    let fake = Fake::start();
    let client = fake.client();
    let written = client
        .fs_write("/srv/p/a.rs", b"new", Some("e1"), false)
        .unwrap();
    assert_eq!(written.etag.as_deref(), Some("e2"));
    let sent = &fake.requests("/api/fs/write")[0];
    assert_eq!(sent.body, b"new");
    assert_eq!(sent.header("if-match"), Some("e1"));
    assert_eq!(
        sent.header("content-type"),
        Some("application/octet-stream")
    );

    let changed = client
        .fs_write("/srv/p/a.rs", b"x", Some("stale"), false)
        .unwrap_err();
    assert_eq!(
        (changed.status, changed.error.as_deref()),
        (Some(412), Some("changed"))
    );
    let exists = client.fs_write("/srv/p/a.rs", b"", None, true).unwrap_err();
    assert_eq!(exists.error.as_deref(), Some("exists"));

    assert!(client.fs_mkdir("/srv/p/new").unwrap());
    client
        .fs_rename("/srv/p/a.rs", "/srv/p/b.rs", false)
        .unwrap();
    client.fs_remove("/srv/p/new", true).unwrap();
    assert_eq!(
        fake.requests("/api/fs/rename")[0].json(),
        json!({"from": "/srv/p/a.rs", "to": "/srv/p/b.rs", "overwrite": false})
    );
    assert_eq!(fake.requests("/api/fs/remove")[0].json()["recursive"], true);
}

#[test]
fn a_watch_reports_changes_until_it_fails() {
    let mut watch = Fake::start().client().fs_watch("/srv/p").unwrap();
    let stop = AtomicBool::new(false);
    assert_eq!(watch.next(&stop), Some(WatchEvent::Ready));
    assert_eq!(
        watch.next(&stop),
        Some(WatchEvent::Changes {
            paths: vec!["/srv/p/a.rs".into()],
            overflow: false
        })
    );
    assert_eq!(
        watch.next(&stop),
        Some(WatchEvent::Changes {
            paths: vec![],
            overflow: true
        })
    );
    assert_eq!(
        watch.next(&stop),
        Some(WatchEvent::Error("inotify limit".into()))
    );
    assert_eq!(watch.next(&stop), None);
}

#[test]
fn git_runs_there_and_answers_whatever_its_exit() {
    let fake = Fake::start();
    let client = fake.client();
    let args = ["status".to_string(), "--porcelain=v2".to_string()];
    let env = [("GIT_AUTHOR_NAME".to_string(), "Ana".to_string())];
    let out = client.git("/srv/p", &args, &env, Some(b"in")).unwrap();
    assert!(out.success());
    assert_eq!(out.stdout, b"status --porcelain=v2\n");
    assert_eq!(out.stderr, b"warn\n");
    let sent = fake.requests("/api/git")[0].json();
    assert_eq!(sent["cwd"], "/srv/p");
    assert_eq!(sent["env"], json!({"GIT_AUTHOR_NAME": "Ana"}));
    assert_eq!(sent["stdin"], STANDARD.encode("in"));

    let failed = client.git("/srv/p", &["fail".into()], &[], None).unwrap();
    assert!(!failed.success());
    assert_eq!(failed.code, Some(1));
}

#[test]
fn a_one_shot_prompt_runs_there() {
    let fake = Fake::start();
    let client = fake.client();
    let text = client.print("/srv/p", "Write a commit message").unwrap();
    assert_eq!(text, "feat: add a thing\n\nBecause.\n");
    let sent = fake.requests("/api/print")[0].json();
    assert_eq!(
        sent,
        json!({"cwd": "/srv/p", "prompt": "Write a commit message"})
    );

    let failed = client.print("/srv/p", "fail").unwrap_err();
    assert_eq!(failed.error.as_deref(), Some("print_failed"));
    assert_eq!(
        failed.message,
        "gentle-shell failed on the server: No API key for the default model"
    );
}

#[test]
fn an_upload_names_its_path_on_the_server() {
    let fake = Fake::start();
    let uploaded = fake.client().upload("notes 1.txt", b"abc").unwrap();
    assert!(uploaded.path.ends_with("/notes 1.txt"));
    assert!(uploaded.path.starts_with("/home/ana/"));
    assert_eq!(uploaded.size, 3);
    assert_eq!(fake.requests("/api/uploads")[0].body, b"abc");
}

#[test]
fn services_open_through_server_level_forwards() {
    let fake = Fake::start();
    let client = fake.client();
    let services = client.services().unwrap();
    let engram = services.iter().find(|s| s.id == "engram").unwrap();
    assert!(engram.available && engram.forwardable);
    assert_eq!(engram.via.as_deref(), Some("forwards"));
    let forward = client.server_forward(engram.port, "/").unwrap();
    assert_eq!(forward.url, "http://127.0.0.1:40009/?gs_ticket=t9");
    assert_eq!(fake.requests("/api/forwards")[0].json()["port"], 7437);
}

#[test]
fn a_forwarded_page_trades_its_ticket_for_the_cookie() {
    // The forward's own listener: a ticket gets the cookie, the cookie the page.
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let log = seen.clone();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { return };
            let Some(request) = read_request(&stream) else {
                continue;
            };
            log.lock().unwrap().push(request.clone());
            if request.target.contains("gs_ticket=t9") {
                let _ = write!(
                    stream,
                    "HTTP/1.1 303 See Other\r\nLocation: /\r\nSet-Cookie: gs_fwd_fwd_9=s3cret; HttpOnly; SameSite=Strict; Path=/\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                );
            } else if request.header("cookie") == Some("gs_fwd_fwd_9=s3cret") {
                reply(&mut stream, 200, &json!({"project": "p"}));
            } else {
                reply(&mut stream, 401, &json!({}));
            }
        }
    });
    let forward = pi_rpc::serve::Forward {
        id: "fwd_9".into(),
        port: 7437,
        url: format!("http://127.0.0.1:{port}/?gs_ticket=t9"),
    };
    let page = ForwardedPage::open(&forward, std::time::Duration::from_secs(5)).unwrap();
    let (status, body) = page.get("/project/current?cwd=%2Fsrv%2Fp").unwrap();
    assert_eq!(status, 200);
    assert_eq!(
        serde_json::from_str::<Value>(&body).unwrap()["project"],
        "p"
    );
    let seen = seen.lock().unwrap();
    assert_eq!(seen.len(), 2);
    assert_eq!(seen[1].target, "/project/current?cwd=%2Fsrv%2Fp");
}

#[test]
fn a_terminal_opens_streams_and_resumes_from_its_offset() {
    let fake = Fake::start();
    let client = fake.client();
    let opened = client.open_terminal("/srv/p", 80, 24).unwrap();
    assert_eq!((opened.id.as_str(), opened.alive), ("t_1", true));
    assert_eq!(client.terminals().unwrap(), vec![opened.clone()]);

    let stop = AtomicBool::new(false);
    let mut stream = client.terminal_stream("t_1", None).unwrap();
    assert_eq!(
        stream.next(&stop),
        Some(TermEvent::Attached {
            offset: 10,
            snapshot: Some(b"\x1b[2J$ ".to_vec())
        })
    );
    assert_eq!(
        stream.next(&stop),
        Some(TermEvent::Output {
            offset: 13,
            data: b"hi\n".to_vec()
        })
    );
    assert_eq!(stream.next(&stop), Some(TermEvent::Title("zsh".into())));
    assert_eq!(
        stream.next(&stop),
        Some(TermEvent::Resized {
            cols: 100,
            rows: 30
        })
    );
    assert_eq!(stream.next(&stop), Some(TermEvent::Exit(Some(0))));
    assert_eq!(stream.next(&stop), None);

    // Resuming: the missed bytes follow, no snapshot.
    let mut resumed = client.terminal_stream("t_1", Some(10)).unwrap();
    assert_eq!(
        resumed.next(&stop),
        Some(TermEvent::Attached {
            offset: 10,
            snapshot: None
        })
    );
    assert!(
        fake.requests("/api/terminals/t_1/stream")
            .iter()
            .any(|r| r.target.ends_with("?from=10"))
    );
    let mut gone = client.terminal_stream("t_gone", None).unwrap();
    assert_eq!(gone.next(&stop), Some(TermEvent::HostGone));

    client.terminal_input("t_1", b"echo hi\r").unwrap();
    client.terminal_resize("t_1", 100, 30).unwrap();
    client.close_terminal("t_1").unwrap();
    let input = fake.requests("/api/terminals/t_1/input")[0].json();
    assert_eq!(input["data"], STANDARD.encode("echo hi\r"));
    assert_eq!(
        fake.requests("/api/terminals/t_1/resize")[0].json(),
        json!({"cols": 100, "rows": 30})
    );
    assert_eq!(fake.requests("/api/terminals/t_1")[0].method, "DELETE");
    // A malformed id never reaches the server.
    assert_eq!(client.close_terminal("../x").unwrap_err().status, Some(404));
}

#[test]
fn the_terminal_host_is_installed_with_its_hash() {
    let fake = Fake::start();
    let client = fake.client();
    let host = client.term_host().unwrap();
    assert!(!host.available);
    assert_eq!(
        (host.platform.as_str(), host.arch.as_str()),
        ("darwin", "arm64")
    );
    let path = client.install_term_host(b"\x7fELF binary").unwrap();
    assert!(path.ends_with("/serve/bin/gentle-term-host"));
    let sent = &fake.requests("/api/term-host")[1];
    assert_eq!(sent.method, "PUT");
    assert_eq!(sent.body, b"\x7fELF binary");
}
