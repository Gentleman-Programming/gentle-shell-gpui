//! The host over loopback TCP (`--tcp`, the only transport on Windows): the
//! token file, authentication, and a terminal's round trip. Runs everywhere,
//! so Windows CI drives a real ConPTY through it.

mod common;

use std::net::TcpStream;
use std::path::PathBuf;
use std::process::{Child, Command};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use serde_json::{Value, json};

use common::{BIN, Client, output};

struct Host {
    child: Child,
    dir: PathBuf,
    port: u16,
    token: String,
}

impl Host {
    fn start() -> Self {
        static N: AtomicUsize = AtomicUsize::new(0);
        let dir = std::env::temp_dir().join(format!(
            "gth-tcp-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::SeqCst)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        let child = Command::new(BIN)
            .arg("--tcp")
            .arg("--state-dir")
            .arg(dir.join("state"))
            .spawn()
            .unwrap();
        let file = dir.join("state").join("term-host.json");
        let deadline = Instant::now() + Duration::from_secs(10);
        let written = loop {
            let parsed = std::fs::read(&file)
                .ok()
                .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok());
            if let Some(written) = parsed {
                break written;
            }
            assert!(Instant::now() < deadline, "the host never wrote its port");
            std::thread::sleep(Duration::from_millis(20));
        };
        assert_eq!(written["pid"], child.id());
        let token = written["token"].as_str().unwrap().to_string();
        assert_eq!(token.len(), 64);
        let port = written["port"].as_u64().unwrap() as u16;
        Self {
            child,
            dir,
            port,
            token,
        }
    }

    fn file(&self) -> PathBuf {
        self.dir.join("state").join("term-host.json")
    }

    fn connect(&self) -> Client {
        Client::new(TcpStream::connect(("127.0.0.1", self.port)).unwrap())
    }

    /// A connection that has authenticated.
    fn client(&self) -> Client {
        let mut client = self.connect();
        let reply = client.ok(json!({ "op": "auth", "token": self.token }));
        assert_eq!(reply.as_object().unwrap().len(), 2, "{reply}");
        client
    }
}

impl Drop for Host {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// A program that says "ready", reads a line, answers "got <line>" and
/// exits with 3.
fn script() -> Value {
    if cfg!(windows) {
        json!([
            "cmd.exe",
            "/v:on",
            "/c",
            "echo ready & set /p line= & echo got !line! & exit 3"
        ])
    } else {
        json!([
            "/bin/sh",
            "-c",
            "printf 'ready\\n'; read line; printf 'got %s\\n' \"$line\"; exit 3"
        ])
    }
}

#[test]
fn the_token_file_and_authentication() {
    let host = Host::start();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let mode = |path: PathBuf| std::fs::metadata(path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(host.file()), 0o600);
        assert_eq!(mode(host.dir.join("state")), 0o700);
    }

    // Anything but the token first: refused, and the connection closes.
    let mut c = host.connect();
    let reply = c.call(json!({ "op": "hello" }));
    assert_eq!(reply["error"]["code"], "unauthorized", "{reply}");
    assert!(c.closed(Duration::from_secs(5)));
    let mut c = host.connect();
    let wrong = format!("{}0", &host.token[1..]);
    let reply = c.call(json!({ "op": "auth", "token": wrong }));
    assert_eq!(reply["error"]["code"], "unauthorized", "{reply}");
    assert!(c.closed(Duration::from_secs(5)));
    let mut c = host.connect();
    c.send(&json!("not an object"));
    let reply = c.read_line(Duration::from_secs(5)).unwrap();
    assert_eq!(reply["error"]["code"], "unauthorized", "{reply}");
    assert!(c.closed(Duration::from_secs(5)));

    // With it, the protocol as over a socket.
    let mut c = host.client();
    let hello = c.ok(json!({ "op": "hello" }));
    assert_eq!(hello["protocol"], 1);
    assert_eq!(hello["pid"], host.child.id());
    assert_eq!(
        c.error(json!({ "op": "auth", "token": host.token })),
        "bad_request"
    );

    // A second host on the same state folder refuses to start, and leaves
    // the token file alone.
    let second = Command::new(BIN)
        .arg("--tcp")
        .arg("--state-dir")
        .arg(host.dir.join("state"))
        .output()
        .unwrap();
    assert!(!second.status.success());
    assert!(String::from_utf8_lossy(&second.stderr).contains("another host"));
    c.ok(json!({ "op": "hello" }));
    host.client().ok(json!({ "op": "hello" }));

    // Without a state folder there's nowhere to write the token.
    let alone = Command::new(BIN).arg("--tcp").output().unwrap();
    assert!(!alone.status.success());
}

#[test]
fn a_terminal_over_tcp_and_shutdown() {
    let mut host = Host::start();
    let mut c = host.client();
    let cwd = std::env::temp_dir();
    let reply = c.ok(json!({
        "op": "open", "cwd": cwd, "argv": script(), "cols": 80, "rows": 24,
    }));
    let term = reply["term"].as_str().unwrap().to_string();
    assert!(reply["pid"].as_u64().unwrap() > 0);

    c.ok(json!({ "op": "attach", "term": term, "from": 0 }));
    let mut seen = String::new();
    c.events_until(|e| {
        seen.push_str(&output(std::slice::from_ref(e)));
        seen.contains("ready")
    });
    c.ok(json!({ "op": "write", "term": term, "data": BASE64.encode("typed\r") }));
    let events = c.events_until(|e| e["ev"] == "exit");
    seen.push_str(&output(&events));
    assert!(seen.contains("got typed"), "{seen:?}");
    assert_eq!(events.last().unwrap()["code"], 3);

    // Closing a running one ends it.
    let reply = c.ok(json!({
        "op": "open", "cwd": cwd, "argv": script(), "cols": 80, "rows": 24,
    }));
    let running = reply["term"].as_str().unwrap().to_string();
    c.ok(json!({ "op": "attach", "term": running, "from": 0 }));
    c.ok(json!({ "op": "close", "term": running }));
    let events = c.events_until(|e| e["ev"] == "exit" && e["term"] == running);
    assert_ne!(events.last().unwrap()["code"], 3);

    c.ok(json!({ "op": "shutdown" }));
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(status) = host.child.try_wait().unwrap() {
            assert!(status.success(), "{status}");
            break;
        }
        assert!(Instant::now() < deadline, "the host didn't exit");
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(!host.file().exists(), "the token file is still there");
}
