//! The host over its real socket: a host process per test, in a temporary
//! folder, driven the way `gentle-shell serve` drives it.
#![cfg(unix)]

mod common;

use std::io::Write;
use std::os::unix::fs::PermissionsExt as _;
use std::os::unix::net::UnixStream;
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
    socket: PathBuf,
}

impl Host {
    fn start(extra: &[&str]) -> Self {
        static N: AtomicUsize = AtomicUsize::new(0);
        // Short: socket paths are limited to about 100 bytes.
        let dir = PathBuf::from(format!(
            "/tmp/gth-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::SeqCst)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        let socket = dir.join("run").join("term.sock");
        let child = Command::new(BIN)
            .arg("--socket")
            .arg(&socket)
            .arg("--state-dir")
            .arg(dir.join("state"))
            .args(extra)
            .spawn()
            .unwrap();
        let host = Self { child, dir, socket };
        let deadline = Instant::now() + Duration::from_secs(10);
        while UnixStream::connect(&host.socket).is_err() {
            assert!(Instant::now() < deadline, "the host never listened");
            std::thread::sleep(Duration::from_millis(20));
        }
        host
    }

    fn connect(&self) -> Client {
        Client::new(UnixStream::connect(&self.socket).unwrap())
    }

    /// Exits within `within`, with its socket gone.
    fn wait_exit(&mut self, within: Duration) {
        let deadline = Instant::now() + within;
        loop {
            if let Some(status) = self.child.try_wait().unwrap() {
                assert!(status.success(), "{status}");
                break;
            }
            assert!(Instant::now() < deadline, "the host didn't exit");
            std::thread::sleep(Duration::from_millis(50));
        }
        assert!(!self.socket.exists(), "the socket is still there");
    }
}

impl Drop for Host {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn open(client: &mut Client, script: &str) -> String {
    let reply = client.ok(json!({
        "op": "open", "cwd": "/", "argv": ["/bin/sh", "-c", script], "cols": 80, "rows": 24,
    }));
    assert!(reply["pid"].as_u64().unwrap() > 0);
    reply["term"].as_str().unwrap().to_string()
}

#[test]
fn hello_list_and_errors() {
    let host = Host::start(&[]);
    let mut c = host.connect();
    let hello = c.ok(json!({ "op": "hello" }));
    assert_eq!(hello["protocol"], 1);
    assert_eq!(hello["pid"], host.child.id());
    assert!(hello["version"].is_string());
    assert_eq!(c.ok(json!({ "op": "list" }))["terms"], json!([]));

    assert_eq!(c.error(json!({ "op": "nope" })), "bad_request");
    assert_eq!(
        c.error(json!({ "op": "write", "term": "t_0", "data": "" })),
        "not_found"
    );
    let bad_open = json!({ "op": "open", "cwd": "/no/such/dir", "cols": 80, "rows": 24 });
    assert_eq!(c.error(bad_open), "bad_request");
    let bad_size = json!({ "op": "open", "cwd": "/", "cols": 0, "rows": 24 });
    assert_eq!(c.error(bad_size), "bad_request");
    let missing =
        json!({ "op": "open", "cwd": "/", "argv": ["/no/such/program"], "cols": 80, "rows": 24 });
    assert_eq!(c.error(missing), "spawn_failed");

    // A line that isn't JSON gets an error with no id, and the connection
    // goes on.
    c.stream.write_all(b"not json\n").unwrap();
    let reply = c.read_line(Duration::from_secs(5)).unwrap();
    assert_eq!(reply["id"], Value::Null);
    assert_eq!(reply["error"]["code"], "bad_request");
    c.ok(json!({ "op": "hello" }));

    // Owner-only.
    let mode = |path: &PathBuf| std::fs::metadata(path).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode(&host.socket), 0o600);
    assert_eq!(mode(&host.socket.parent().unwrap().to_path_buf()), 0o700);

    // A second host on the same socket refuses to start.
    let second = Command::new(BIN)
        .arg("--socket")
        .arg(&host.socket)
        .output()
        .unwrap();
    assert!(!second.status.success());
    c.ok(json!({ "op": "hello" }));
}

#[test]
fn output_exit_and_forget() {
    let host = Host::start(&[]);
    let mut c = host.connect();
    let term = c.ok(json!({
        "op": "open", "cwd": "/tmp", "argv": ["/bin/sh", "-c", "printf \"hello $FOO $TERM $PWD\""],
        "env": { "FOO": "bar" }, "cols": 80, "rows": 24,
    }))["term"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(term.starts_with("t_"));
    // From 0: everything, however fast the program was.
    let reply = c.ok(json!({ "op": "attach", "term": term, "from": 0 }));
    assert_eq!(reply["offset"], 0);
    assert!(reply.get("snapshot").is_none());
    let events = c.events_until(|e| e["ev"] == "exit");
    let text = output(&events);
    assert!(text.contains("hello bar xterm-256color /"), "{text:?}");
    assert_eq!(events.last().unwrap()["code"], 0);
    let last_offset = events.iter().rev().find(|e| e["ev"] == "output").unwrap()["offset"].clone();

    let list = c.ok(json!({ "op": "list" }));
    let info = &list["terms"][0];
    assert_eq!(info["term"], term);
    assert_eq!(info["alive"], false);
    assert_eq!(info["exitCode"], 0);
    assert_eq!(info["offset"], last_offset);
    assert_eq!(info["cols"], 80);
    assert!(info["createdAt"].as_u64().unwrap() > 1_600_000_000_000);

    assert_eq!(
        c.error(json!({ "op": "write", "term": term, "data": "eA==" })),
        "exited"
    );
    assert_eq!(
        c.error(json!({ "op": "resize", "term": term, "cols": 10, "rows": 10 })),
        "exited"
    );
    // Attaching to an exited term: the last screen, then its exit.
    let reply = c.ok(json!({ "op": "attach", "term": term }));
    let snapshot = BASE64.decode(reply["snapshot"].as_str().unwrap()).unwrap();
    assert!(String::from_utf8_lossy(&snapshot).contains("hello bar"));
    assert_eq!(c.event(Duration::from_secs(5)).unwrap()["ev"], "exit");

    c.ok(json!({ "op": "forget", "term": term }));
    assert_eq!(c.ok(json!({ "op": "list" }))["terms"], json!([]));

    let exit_code = open(&mut c, "exit 3");
    c.ok(json!({ "op": "attach", "term": exit_code, "from": 0 }));
    let events = c.events_until(|e| e["ev"] == "exit");
    assert_eq!(events.last().unwrap()["code"], 3);
}

#[test]
fn input_resize_and_title() {
    let host = Host::start(&[]);
    let mut c = host.connect();
    let term = open(
        &mut c,
        "stty size; read line; printf 'got %s\\n' \"$line\"; printf '\\033]2;my title\\007'; read line; stty size",
    );
    assert_eq!(c.error(json!({ "op": "forget", "term": term })), "alive");
    c.ok(json!({ "op": "attach", "term": term, "from": 0 }));
    let mut seen = String::new();
    c.events_until(|e| {
        seen.push_str(&output(std::slice::from_ref(e)));
        seen.contains("24 80")
    });

    c.ok(json!({ "op": "write", "term": term, "data": BASE64.encode("typed\n") }));
    let events = c.events_until(|e| e["ev"] == "title");
    assert!(output(&events).contains("got typed"));
    assert_eq!(events.last().unwrap()["title"], "my title");

    c.ok(json!({ "op": "resize", "term": term, "cols": 100, "rows": 30 }));
    let resized = c.events_until(|e| e["ev"] == "resized");
    let resized = resized.last().unwrap();
    assert_eq!(
        (resized["cols"].as_u64(), resized["rows"].as_u64()),
        (Some(100), Some(30))
    );
    let info = &c.ok(json!({ "op": "list" }))["terms"][0];
    assert_eq!(
        (info["cols"].as_u64(), info["rows"].as_u64()),
        (Some(100), Some(30))
    );
    assert_eq!(info["title"], "my title");

    c.ok(json!({ "op": "write", "term": term, "data": BASE64.encode("\n") }));
    let events = c.events_until(|e| e["ev"] == "exit");
    assert!(output(&events).contains("30 100"), "{events:?}");
}

#[test]
fn replay_from_an_offset_and_snapshot() {
    let host = Host::start(&[]);
    let mut control = host.connect();
    let term = open(&mut control, "printf one; read line; printf two; read line");

    // A client sees "one", then goes away.
    let mut first = host.connect();
    first.ok(json!({ "op": "attach", "term": term, "from": 0 }));
    let mut seen = String::new();
    let mut offset = 0;
    first.events_until(|e| {
        seen.push_str(&output(std::slice::from_ref(e)));
        offset = e["offset"].as_u64().unwrap_or(offset);
        seen.contains("one")
    });
    drop(first);

    // The term goes on without it: the echoed newline, then "two".
    control.ok(json!({ "op": "write", "term": term, "data": BASE64.encode("\n") }));
    let deadline = Instant::now() + Duration::from_secs(10);
    let end = loop {
        let list = control.ok(json!({ "op": "list" }));
        let end = list["terms"][0]["offset"].as_u64().unwrap();
        if end >= offset + 5 {
            break end;
        }
        assert!(Instant::now() < deadline, "no more output");
        std::thread::sleep(Duration::from_millis(20));
    };

    // Coming back from where it was: only what it missed.
    let mut back = host.connect();
    let reply = back.ok(json!({ "op": "attach", "term": term, "from": offset }));
    assert_eq!(reply["offset"], offset);
    assert!(reply.get("snapshot").is_none());
    let event = back.event(Duration::from_secs(5)).unwrap();
    assert_eq!(event["offset"], end);
    let missed = output(&[event]);
    assert!(
        missed.contains("two") && !missed.contains("one"),
        "{missed:?}"
    );

    // Without an offset, or one the ring doesn't have: the screen.
    for request in [
        json!({ "op": "attach", "term": term }),
        json!({ "op": "attach", "term": term, "from": end + 1000 }),
    ] {
        let mut fresh = host.connect();
        let reply = fresh.ok(request);
        assert_eq!(reply["offset"], end);
        let snapshot = BASE64.decode(reply["snapshot"].as_str().unwrap()).unwrap();
        let snapshot = String::from_utf8_lossy(&snapshot);
        assert!(
            snapshot.contains("one") && snapshot.contains("two"),
            "{snapshot:?}"
        );
    }

    // Detached, a connection gets nothing more.
    back.ok(json!({ "op": "detach", "term": term }));
    control.ok(json!({ "op": "write", "term": term, "data": BASE64.encode("\n") }));
    assert!(back.event(Duration::from_millis(500)).is_none());
}

#[test]
fn a_connection_that_falls_behind_overflows() {
    let host = Host::start(&[]);
    let mut control = host.connect();
    // ~27 MB of events: well past the 8 MiB queue and the socket's buffer.
    let term = open(
        &mut control,
        "head -c 20000000 /dev/zero | tr '\\0' x; read line",
    );
    let mut slow = host.connect();
    slow.ok(json!({ "op": "attach", "term": term, "from": 0 }));

    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        let list = control.ok(json!({ "op": "list" }));
        if list["terms"][0]["offset"].as_u64().unwrap() >= 20_000_000 {
            break;
        }
        assert!(Instant::now() < deadline, "the output never ended");
        std::thread::sleep(Duration::from_millis(100));
    }
    let events = slow.events_until(|e| e["ev"] == "overflow");
    assert_eq!(events.last().unwrap()["term"], term);
    // Detached: nothing after the overflow, until it attaches again.
    assert!(slow.event(Duration::from_millis(300)).is_none());
    let reply = slow.ok(json!({ "op": "attach", "term": term }));
    assert!(reply["snapshot"].is_string());
    assert_eq!(reply["offset"].as_u64(), Some(20_000_000));
}

#[test]
fn close_hangs_up_then_kills() {
    let host = Host::start(&[]);
    let mut c = host.connect();
    let hangs_up = open(&mut c, "printf ready; sleep 30");
    let ignores = open(&mut c, "trap '' HUP; printf ready; sleep 30");
    // Once both are running (and one ignores SIGHUP).
    for term in [&hangs_up, &ignores] {
        c.ok(json!({ "op": "attach", "term": term, "from": 0 }));
        c.events_until(|e| e["term"] == *term && output(std::slice::from_ref(e)).contains("ready"));
    }
    let started = Instant::now();
    c.ok(json!({ "op": "close", "term": hangs_up }));
    c.ok(json!({ "op": "close", "term": ignores }));
    // Unlisted at once.
    assert_eq!(c.ok(json!({ "op": "list" }))["terms"], json!([]));
    let mut codes = Vec::new();
    while codes.len() < 2 {
        let event = c.events_until(|e| e["ev"] == "exit").pop().unwrap();
        codes.push((
            event["term"].as_str().unwrap().to_string(),
            started.elapsed(),
        ));
    }
    let after = |term: &str| codes.iter().find(|(t, _)| t == term).unwrap().1;
    assert!(after(&hangs_up) < Duration::from_secs(2));
    assert!(after(&ignores) >= Duration::from_millis(2900));
}

#[test]
fn terms_outlive_connections_and_the_host_exits_when_idle() {
    let mut host = Host::start(&["--idle-timeout", "1"]);
    let mut c = host.connect();
    let term = open(&mut c, "read line; printf done");
    drop(c);
    std::thread::sleep(Duration::from_millis(1500));
    // Still there, with its term.
    let mut c = host.connect();
    let list = c.ok(json!({ "op": "list" }));
    assert_eq!(list["terms"][0]["term"], term);
    c.ok(json!({ "op": "write", "term": term, "data": BASE64.encode("\n") }));
    c.ok(json!({ "op": "attach", "term": term, "from": 0 }));
    c.events_until(|e| e["ev"] == "exit");
    c.ok(json!({ "op": "forget", "term": term }));
    drop(c);
    host.wait_exit(Duration::from_secs(5));
}

#[test]
fn shutdown_replies_and_exits() {
    let mut host = Host::start(&[]);
    let mut c = host.connect();
    open(&mut c, "sleep 30");
    c.ok(json!({ "op": "shutdown" }));
    host.wait_exit(Duration::from_secs(5));
}
