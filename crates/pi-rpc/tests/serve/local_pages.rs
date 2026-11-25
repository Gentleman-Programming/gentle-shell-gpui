//! A forward's page opened on this computer (`ServeClient::local_page`):
//! a stand-in forward (its `Host`, `Origin`, ticket and cookie rules, as
//! Gentle Shell's `lib/serve-forward.ts` has them) behind a TLS front with
//! a pinned certificate, reached through the proxy over plain HTTP/1.1 on
//! loopback: keep-alive requests, bodies, redirects, SSE, a WebSocket, the
//! relay, and the proxy closing.

use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc;

use pi_rpc::serve::{Forward, Trust};

use super::relay::FakeRelay;
use super::*;

/// One request the forward got.
#[derive(Debug, Clone)]
struct Seen {
    line: String,
    host: String,
    origin: Option<String>,
    referer: Option<String>,
}

/// The stand-in forward, plain HTTP behind a [`TlsFront`].
struct FakeForward {
    addr: String,
    /// The `Host` it accepts: the forward link's `host:port`.
    host: Arc<Mutex<String>>,
    seen: Arc<Mutex<Vec<Seen>>>,
    connections: Arc<AtomicUsize>,
    /// Lets the event stream send its second event.
    go: mpsc::Sender<()>,
}

impl FakeForward {
    fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap().to_string();
        let host = Arc::new(Mutex::new(String::new()));
        let seen = Arc::new(Mutex::new(Vec::new()));
        let connections = Arc::new(AtomicUsize::new(0));
        let (go, wait) = mpsc::channel();
        let wait = Arc::new(Mutex::new(wait));
        let (h, s, c) = (host.clone(), seen.clone(), connections.clone());
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(stream) = stream else { return };
                c.fetch_add(1, Ordering::SeqCst);
                let (h, s, wait) = (h.clone(), s.clone(), wait.clone());
                std::thread::spawn(move || forward_connection(stream, &h, &s, &wait));
            }
        });
        Self {
            addr,
            host,
            seen,
            connections,
            go,
        }
    }

    fn seen(&self) -> Vec<Seen> {
        self.seen.lock().unwrap().clone()
    }
}

fn answer(stream: &mut TcpStream, status: &str, headers: &str, body: &str) {
    let text = format!(
        "HTTP/1.1 {status}\r\n{headers}Content-Length: {}\r\n\r\n{body}",
        body.len()
    );
    stream.write_all(text.as_bytes()).unwrap();
}

fn chunk(stream: &mut TcpStream, data: &str) {
    write!(stream, "{:x}\r\n{data}\r\n", data.len()).unwrap();
}

/// Reads a chunked body to its end.
fn dechunk(reader: &mut impl std::io::BufRead) -> String {
    let mut body = String::new();
    loop {
        let mut size = String::new();
        reader.read_line(&mut size).unwrap();
        let size = usize::from_str_radix(size.trim(), 16).unwrap();
        let mut data = vec![0u8; size + 2];
        reader.read_exact(&mut data).unwrap();
        if size == 0 {
            return body;
        }
        body.push_str(std::str::from_utf8(&data[..size]).unwrap());
    }
}

fn read_head(reader: &mut impl std::io::BufRead) -> Option<(String, HashMap<String, String>)> {
    let mut line = String::new();
    if reader.read_line(&mut line).unwrap_or(0) == 0 {
        return None;
    }
    let mut headers = HashMap::new();
    loop {
        let mut header = String::new();
        reader.read_line(&mut header).unwrap();
        let header = header.trim_end();
        if header.is_empty() {
            break;
        }
        let (name, value) = header.split_once(':').unwrap();
        headers.insert(name.to_ascii_lowercase(), value.trim().to_string());
    }
    Some((line.trim_end().to_string(), headers))
}

fn forward_connection(
    mut stream: TcpStream,
    host: &Mutex<String>,
    seen: &Mutex<Vec<Seen>>,
    wait: &Mutex<mpsc::Receiver<()>>,
) {
    let mut reader = BufReader::new(stream.try_clone().unwrap());
    while let Some((line, headers)) = read_head(&mut reader) {
        let body = if headers.get("transfer-encoding").map(String::as_str) == Some("chunked") {
            dechunk(&mut reader)
        } else {
            let length = headers
                .get("content-length")
                .map_or(0, |l| l.parse().unwrap());
            let mut body = vec![0u8; length];
            reader.read_exact(&mut body).unwrap();
            String::from_utf8(body).unwrap()
        };
        let got = headers.get("host").cloned().unwrap_or_default();
        seen.lock().unwrap().push(Seen {
            line: line.clone(),
            host: got.clone(),
            origin: headers.get("origin").cloned(),
            referer: headers.get("referer").cloned(),
        });
        let own = host.lock().unwrap().clone();
        if got != own {
            answer(
                &mut stream,
                "421 Misdirected Request",
                "",
                "Unexpected Host header",
            );
            continue;
        }
        if let Some(origin) = headers.get("origin")
            && *origin != format!("https://{own}")
            && *origin != format!("http://{own}")
        {
            answer(&mut stream, "403 Forbidden", "", "Origin not allowed");
            continue;
        }
        let mut parts = line.split(' ');
        let (method, target) = (parts.next().unwrap(), parts.next().unwrap());
        if target.contains("gs_ticket=T1") {
            let path = target.split('?').next().unwrap();
            answer(
                &mut stream,
                "303 See Other",
                &format!(
                    "Location: {path}\r\nSet-Cookie: gs_fwd_t=secret; HttpOnly; SameSite=Strict; Path=/\r\n"
                ),
                "",
            );
            continue;
        }
        if !headers
            .get("cookie")
            .is_some_and(|c| c.contains("gs_fwd_t=secret"))
        {
            answer(
                &mut stream,
                "401 Unauthorized",
                "",
                "Open this page from Gentle Shell",
            );
            continue;
        }
        match target {
            "/page" if method == "HEAD" => {
                stream
                    .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\n\r\n")
                    .unwrap();
            }
            "/page" => answer(
                &mut stream,
                "200 OK",
                "Content-Type: text/plain\r\n",
                "hello",
            ),
            "/redirect" => answer(
                &mut stream,
                "302 Found",
                &format!("Location: https://{own}/page\r\n"),
                "",
            ),
            "/echo" => {
                stream
                    .write_all(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n")
                    .unwrap();
                chunk(&mut stream, &body);
                stream.write_all(b"0\r\n\r\n").unwrap();
            }
            "/events" => {
                stream
                    .write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nTransfer-Encoding: chunked\r\n\r\n")
                    .unwrap();
                chunk(&mut stream, "data: 1\n\n");
                // The second waits for the first to arrive through the proxy.
                wait.lock().unwrap().recv().unwrap();
                chunk(&mut stream, "data: 2\n\n");
                stream.write_all(b"0\r\n\r\n").unwrap();
            }
            "/ws" if headers.contains_key("upgrade") => {
                stream
                    .write_all(b"HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n\r\n")
                    .unwrap();
                let mut buf = [0u8; 1024];
                loop {
                    let n = reader.read(&mut buf).unwrap_or(0);
                    if n == 0 {
                        return;
                    }
                    let upper = buf[..n].to_ascii_uppercase();
                    if stream.write_all(&upper).is_err() {
                        return;
                    }
                }
            }
            _ => answer(&mut stream, "404 Not Found", "", "nothing"),
        }
    }
}

/// A browser's connection to the proxy.
struct Browser {
    reader: BufReader<TcpStream>,
    stream: TcpStream,
    port: u16,
}

struct Answer {
    status: u16,
    headers: HashMap<String, String>,
    body: String,
}

impl Browser {
    fn open(port: u16) -> Self {
        let stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(10)))
            .unwrap();
        Self {
            reader: BufReader::new(stream.try_clone().unwrap()),
            stream,
            port,
        }
    }

    /// Sends `request`, its `{port}` filled in.
    fn send(&mut self, request: &str) {
        let request = request.replace("{port}", &self.port.to_string());
        self.stream.write_all(request.as_bytes()).unwrap();
    }

    fn head(&mut self) -> (u16, HashMap<String, String>) {
        let (line, headers) = read_head(&mut self.reader).expect("an answer");
        let status = line.split(' ').nth(1).unwrap().parse().unwrap();
        (status, headers)
    }

    fn answer(&mut self, head: bool) -> Answer {
        let (status, headers) = self.head();
        let body = if head {
            String::new()
        } else if headers.get("transfer-encoding").map(String::as_str) == Some("chunked") {
            dechunk(&mut self.reader)
        } else {
            let length = headers
                .get("content-length")
                .map_or(0, |l| l.parse().unwrap());
            let mut body = vec![0u8; length];
            self.reader.read_exact(&mut body).unwrap();
            String::from_utf8(body).unwrap()
        };
        Answer {
            status,
            headers,
            body,
        }
    }

    fn get(&mut self, request: &str) -> Answer {
        self.send(request);
        self.answer(false)
    }
}

const COOKIE: &str = "Cookie: other=1; gs_fwd_t=secret\r\n";

fn port_of(url: &str) -> u16 {
    let rest = url.strip_prefix("http://127.0.0.1:").unwrap();
    rest[..rest.find('/').unwrap()].parse().unwrap()
}

/// Waits for nothing to listen on `port` any more.
fn closes(port: u16) -> bool {
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        if TcpStream::connect(("127.0.0.1", port)).is_err() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    false
}

/// A forward behind a pinned TLS front with certificate `a`, its link
/// naming the front.
fn pinned_forward(id: &str) -> (FakeForward, TlsFront, ServeClient, Forward) {
    let forward = FakeForward::start();
    let front = TlsFront::to(&forward.addr, "a");
    *forward.host.lock().unwrap() = format!("127.0.0.1:{}", front.port);
    let client = ServeClient::with_trust(
        &front.url(),
        Some(TOKEN.into()),
        Trust::pinned(&front.fingerprint, None),
    );
    let link = Forward {
        id: id.into(),
        port: 5173,
        url: format!("https://127.0.0.1:{}/page?gs_ticket=T1#top", front.port),
    };
    (forward, front, client, link)
}

#[test]
fn a_forwarded_page_opens_here_over_the_pinned_connection() {
    let (forward, front, client, link) = pinned_forward("fwd_page");
    let url = client.local_page(&link).unwrap();
    let port = port_of(&url);
    assert_eq!(
        url,
        format!("http://127.0.0.1:{port}/page?gs_ticket=T1#top")
    );
    assert_ne!(port, front.port);
    let _hold = pi_rpc::serve::hold_local_page(&url).unwrap();
    // Opened again: the same proxy, so the same origin and cookies.
    assert_eq!(client.local_page(&link).unwrap(), url);

    let mut browser = Browser::open(port);
    let ticket = browser.get("GET /page?gs_ticket=T1 HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\n\r\n");
    assert_eq!(ticket.status, 303);
    assert_eq!(ticket.headers["location"], "/page");
    assert!(ticket.headers["set-cookie"].starts_with("gs_fwd_t=secret"));

    // The same connection, kept alive: the page, from its own origin.
    let page = browser.get(&format!(
        "GET /page HTTP/1.1\r\nHost: 127.0.0.1:{{port}}\r\nOrigin: http://127.0.0.1:{{port}}\r\nReferer: http://127.0.0.1:{{port}}/page?x\r\n{COOKIE}\r\n"
    ));
    assert_eq!((page.status, page.body.as_str()), (200, "hello"));
    // A redirect to the forward's own address comes back here.
    let moved = browser.get(&format!(
        "GET /redirect HTTP/1.1\r\nHost: 127.0.0.1:{{port}}\r\n{COOKIE}\r\n"
    ));
    assert_eq!(moved.status, 302);
    assert_eq!(
        moved.headers["location"],
        format!("http://127.0.0.1:{port}/page")
    );
    browser.send(&format!(
        "HEAD /page HTTP/1.1\r\nHost: 127.0.0.1:{{port}}\r\n{COOKIE}\r\n"
    ));
    let head = browser.answer(true);
    assert_eq!(
        (head.status, head.headers["content-length"].as_str()),
        (200, "5")
    );
    let echo = browser.get(&format!(
        "POST /echo HTTP/1.1\r\nHost: 127.0.0.1:{{port}}\r\nContent-Length: 3\r\n{COOKIE}\r\nabc"
    ));
    assert_eq!(echo.body, "abc");
    let chunked = browser.get(&format!(
        "POST /echo HTTP/1.1\r\nHost: localhost:{{port}}\r\nTransfer-Encoding: chunked\r\n{COOKIE}\r\n4\r\nWiki\r\n5\r\npedia\r\n0\r\n\r\n"
    ));
    assert_eq!(chunked.body, "Wikipedia");

    // The forward saw its own Host, Origin and Referer, over one connection.
    let seen = forward.seen();
    let own = format!("127.0.0.1:{}", front.port);
    assert_eq!(seen.len(), 6, "{seen:?}");
    assert!(seen.iter().all(|s| s.host == own), "{seen:?}");
    assert_eq!(seen[1].origin, Some(format!("https://{own}")));
    assert_eq!(seen[1].referer, Some(format!("https://{own}/page?x")));
    assert_eq!(seen[0].line, "GET /page?gs_ticket=T1 HTTP/1.1");
    assert_eq!(forward.connections.load(Ordering::SeqCst), 1);
}

#[test]
fn another_host_never_reaches_the_forward_and_another_origin_stays() {
    let (forward, _front, client, link) = pinned_forward("fwd_rules");
    let port = port_of(&client.local_page(&link).unwrap());
    // A page elsewhere resolving its name to this computer (DNS rebinding).
    let rebound = Browser::open(port)
        .get("GET /page HTTP/1.1\r\nHost: evil.example:{port}\r\nCookie: gs_fwd_t=secret\r\n\r\n");
    assert_eq!(rebound.status, 421);
    assert!(forward.seen().is_empty());

    // Another page on this computer: its Origin reaches the forward as it
    // is, and the forward refuses it.
    let other = Browser::open(port).get(&format!(
        "POST /echo HTTP/1.1\r\nHost: 127.0.0.1:{{port}}\r\nOrigin: http://127.0.0.1:1\r\nContent-Length: 0\r\n{COOKIE}\r\n"
    ));
    assert_eq!(other.status, 403);
    assert_eq!(
        forward.seen()[0].origin.as_deref(),
        Some("http://127.0.0.1:1")
    );
}

#[test]
fn streams_and_websockets_pass_as_they_come_and_close_with_the_page() {
    let (forward, _front, client, link) = pinned_forward("fwd_streams");
    let url = client.local_page(&link).unwrap();
    let port = port_of(&url);
    let hold = pi_rpc::serve::hold_local_page(&url).unwrap();

    let mut events = Browser::open(port);
    events.send(&format!(
        "GET /events HTTP/1.1\r\nHost: 127.0.0.1:{{port}}\r\nAccept: text/event-stream\r\n{COOKIE}\r\n"
    ));
    let (status, headers) = events.head();
    assert_eq!(status, 200);
    assert_eq!(headers["content-type"], "text/event-stream");
    // The first event arrives while the forward holds back the second.
    let mut size = String::new();
    events.reader.read_line(&mut size).unwrap();
    let mut first = vec![0u8; usize::from_str_radix(size.trim(), 16).unwrap() + 2];
    events.reader.read_exact(&mut first).unwrap();
    assert_eq!(first, b"data: 1\n\n\r\n");
    forward.go.send(()).unwrap();
    assert_eq!(dechunk(&mut events.reader), "data: 2\n\n");

    let mut socket = Browser::open(port);
    socket.send(&format!(
        "GET /ws HTTP/1.1\r\nHost: 127.0.0.1:{{port}}\r\nConnection: Upgrade\r\nUpgrade: websocket\r\nOrigin: http://127.0.0.1:{{port}}\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\nSec-WebSocket-Version: 13\r\n{COOKIE}\r\n"
    ));
    let (status, headers) = socket.head();
    assert_eq!(status, 101);
    assert_eq!(headers["upgrade"], "websocket");
    socket.stream.write_all(b"ping").unwrap();
    let mut pong = [0u8; 4];
    socket.reader.read_exact(&mut pong).unwrap();
    assert_eq!(&pong, b"PING");

    // The page closes: the proxy, its listener and its open WebSocket.
    drop(hold);
    assert!(closes(port));
    let mut rest = [0u8; 1];
    assert!(matches!(socket.reader.read(&mut rest), Ok(0) | Err(_)));
    assert!(pi_rpc::serve::hold_local_page(&url).is_none());
}

#[test]
fn a_changed_certificate_never_reaches_the_page() {
    let (forward, front, _, link) = pinned_forward("fwd_changed");
    let other = gentle_core::serve::certificate_fingerprint(&fixture("cert-b.der"));
    let client = ServeClient::with_trust(
        &front.url(),
        Some(TOKEN.into()),
        Trust::pinned(&other, None),
    );
    let port = port_of(&client.local_page(&link).unwrap());
    let refused = Browser::open(port)
        .get("GET /page?gs_ticket=T1 HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\n\r\n");
    assert_eq!(refused.status, 502);
    assert!(
        refused
            .body
            .contains(gentle_core::serve::CERTIFICATE_CHANGED),
        "{}",
        refused.body
    );
    assert!(forward.seen().is_empty());
}

#[test]
fn a_closed_forward_closes_its_proxy() {
    let gone = TcpListener::bind("127.0.0.1:0").unwrap();
    let gone_port = gone.local_addr().unwrap().port();
    drop(gone);
    let front = TlsFront::to("127.0.0.1:9", "a");
    let client = ServeClient::with_trust(
        &front.url(),
        Some(TOKEN.into()),
        Trust::pinned(&front.fingerprint, None),
    );
    let link = Forward {
        id: "fwd_gone".into(),
        port: 5173,
        url: format!("https://127.0.0.1:{gone_port}/?gs_ticket=T1"),
    };
    let url = client.local_page(&link).unwrap();
    let port = port_of(&url);
    let answer =
        Browser::open(port).get("GET /?gs_ticket=T1 HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\n\r\n");
    assert_eq!(answer.status, 502);
    assert!(answer.body.contains("forward closed"), "{}", answer.body);
    assert!(closes(port));
    assert!(pi_rpc::serve::hold_local_page(&url).is_none());
}

#[test]
fn a_forwarded_page_opens_through_the_relay() {
    let forward = FakeForward::start();
    let front = TlsFront::to(&forward.addr, "a");
    let relay = FakeRelay::start(front.port, false);
    // Through a relay the link names the relay's host and the forward's
    // listen port (the relay is asked for it).
    *forward.host.lock().unwrap() = "127.0.0.1:8801".into();
    let client = ServeClient::with_trust(
        &relay.url,
        Some(TOKEN.into()),
        Trust::pinned(&front.fingerprint, Some(&relay.url)),
    );
    let link = Forward {
        id: "fwd_relayed".into(),
        port: 5173,
        url: "https://127.0.0.1:8801/page?gs_ticket=T1".into(),
    };
    let port = port_of(&client.local_page(&link).unwrap());
    let mut browser = Browser::open(port);
    let ticket = browser.get("GET /page?gs_ticket=T1 HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\n\r\n");
    assert_eq!(ticket.status, 303);
    let page = browser.get(&format!(
        "GET /page HTTP/1.1\r\nHost: 127.0.0.1:{{port}}\r\nOrigin: http://127.0.0.1:{{port}}\r\n{COOKIE}\r\n"
    ));
    assert_eq!((page.status, page.body.as_str()), (200, "hello"));
    let hellos = relay.hellos.lock().unwrap().clone();
    assert_eq!(hellos.len(), 1, "one connection, kept alive");
    assert_eq!(hellos[0]["port"], 8801);
    assert_eq!(hellos[0]["id"], front.fingerprint.as_str());
    assert_eq!(
        forward.seen()[1].origin.as_deref(),
        Some("https://127.0.0.1:8801")
    );
}

#[test]
fn a_plain_http_forward_opens_as_it_is() {
    let client = ServeClient::new("http://127.0.0.1:1", Some(TOKEN.into()));
    let link = Forward {
        id: "fwd_plain".into(),
        port: 5173,
        url: "http://127.0.0.1:40001/x?gs_ticket=t".into(),
    };
    assert_eq!(client.local_page(&link).unwrap(), link.url);
}
