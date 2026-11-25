//! Live check of pinned TLS, the pairing proof, the relay and forwarded
//! pages opened through this computer's proxy against a real `gentle-shell
//! serve` and `gentle-shell relay` from a Gentle Shell checkout, in a
//! temporary home (nothing of this computer's Gentle Shell is touched; both
//! listen on loopback only), with `python3 -m http.server` as the page.
//!
//! `#[ignore]` by default: ordinary runs stay hermetic (`tests/serve.rs`
//! covers the same paths against a fake). To run it:
//!
//! ```text
//! GENTLE_SHELL_CHECKOUT=/path/to/gentle-shell \
//!   cargo test -p pi-rpc --test serve_live -- --ignored --nocapture
//! ```

use std::io::{BufRead as _, BufReader, Read as _, Write as _};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use pi_rpc::serve::{ServeClient, Trust};
use serde_json::Value;

/// A process ended when the test ends, however it ends.
struct Running(Child);

impl Drop for Running {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn gentle_shell(checkout: &Path, home: &Path, args: &[&str]) -> Command {
    let mut command = Command::new("node");
    command
        .arg(checkout.join("bin/gentle-shell.mjs"))
        .arg("--home")
        .arg(home)
        .args(args)
        .env("GENTLE_SHELL_HOME", home)
        .env("GENTLE_SHELL_RELAY_SECRET", "serve-live-test")
        .stdin(Stdio::null())
        .stderr(Stdio::inherit());
    command
}

/// Starts `args` and reads the JSON line it prints when it listens.
fn start(checkout: &Path, home: &Path, args: &[&str]) -> (Running, Value) {
    let mut child = gentle_shell(checkout, home, args)
        .stdout(Stdio::piped())
        .spawn()
        .expect("node runs");
    let mut line = String::new();
    BufReader::new(child.stdout.take().unwrap())
        .read_line(&mut line)
        .unwrap();
    let value = serde_json::from_str(&line).unwrap_or_else(|e| panic!("{line:?}: {e}"));
    (Running(child), value)
}

#[test]
#[ignore = "needs GENTLE_SHELL_CHECKOUT (a gentle-shell checkout with serve TLS and the relay) and node"]
fn a_real_server_is_paired_over_pinned_tls_and_reached_through_a_real_relay() {
    let checkout = PathBuf::from(
        std::env::var_os("GENTLE_SHELL_CHECKOUT").expect("GENTLE_SHELL_CHECKOUT is set"),
    );
    let home = std::env::temp_dir().join(format!("gentle-serve-live-{}", std::process::id()));
    std::fs::create_dir_all(&home).unwrap();

    let (_relay, relay) = start(
        &checkout,
        &home,
        &["relay", "--listen", "127.0.0.1:0", "--json"],
    );
    let relay_url = format!("gsr://127.0.0.1:{}", relay["port"]);
    // A relay makes a loopback server speak TLS too.
    let (_serve, serve) = start(
        &checkout,
        &home,
        &[
            "serve",
            "--foreground",
            "--json",
            "--listen",
            "127.0.0.1:0",
            "--relay",
            &relay_url,
        ],
    );
    let plain_url = serve["url"].as_str().unwrap().to_string();
    let fingerprint = serve["fingerprint"].as_str().unwrap().to_string();
    let pair = gentle_shell(&checkout, &home, &["serve", "pair", "--json"])
        .output()
        .unwrap();
    let code = serde_json::from_slice::<Value>(&pair.stdout).unwrap()["code"]
        .as_str()
        .unwrap()
        .to_string();

    // Typed as http://: its identity says it speaks HTTPS, so pairing
    // moves there and waits for the proof.
    let (url, paired) = pi_rpc::serve::pair_at(&plain_url, &code, "serve live test").unwrap();
    assert_eq!(url, plain_url.replace("http://", "https://"));
    assert_eq!(paired.trust, Trust::pinned(&fingerprint, None));

    let pinned = ServeClient::with_trust(&url, Some(paired.token.clone()), paired.trust.clone());
    let info = pinned.server().unwrap();
    assert_eq!(info.server_id, serve["serverId"]);

    let changed = Trust::pinned("AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA", None);
    let error = ServeClient::with_trust(&url, Some(paired.token.clone()), changed)
        .server()
        .unwrap_err();
    assert_eq!(error.error.as_deref(), Some("certificate_changed"));

    // Through the relay, pinned end to end (it registers within moments).
    let through = ServeClient::with_trust(
        &relay_url,
        Some(paired.token),
        Trust::pinned(&fingerprint, Some(&relay_url)),
    );
    let deadline = Instant::now() + Duration::from_secs(10);
    let info = loop {
        match through.server() {
            Ok(info) => break info,
            Err(e) if Instant::now() < deadline => {
                eprintln!("waiting for the relay: {}", e.message);
                std::thread::sleep(Duration::from_millis(200));
            }
            Err(e) => panic!("{}", e.message),
        }
    };
    assert_eq!(info.server_id, serve["serverId"]);
    let _ = std::fs::remove_dir_all(&home);
}

/// `GET` of `url` (`http://127.0.0.1:<port>/…`) with `cookie`: the status,
/// the forward's cookie it set (if any) and the body.
fn fetch(url: &str, cookie: Option<&str>) -> (u16, Option<String>, String) {
    let rest = url.strip_prefix("http://").unwrap();
    let (authority, path) = rest.split_at(rest.find('/').unwrap());
    let mut stream = TcpStream::connect(authority).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(20)))
        .unwrap();
    let cookie = cookie.map_or(String::new(), |c| format!("Cookie: {c}\r\n"));
    write!(
        stream,
        "GET {path} HTTP/1.1\r\nHost: {authority}\r\n{cookie}Connection: close\r\n\r\n"
    )
    .unwrap();
    let mut answer = Vec::new();
    stream.read_to_end(&mut answer).unwrap();
    let answer = String::from_utf8_lossy(&answer).to_string();
    let (head, body) = answer.split_once("\r\n\r\n").unwrap();
    let status = head.split(' ').nth(1).unwrap().parse().unwrap();
    let set = head
        .lines()
        .filter_map(|l| l.split_once(": "))
        .find(|(name, value)| {
            name.eq_ignore_ascii_case("set-cookie") && value.starts_with("gs_fwd_")
        })
        .map(|(_, value)| value.split(';').next().unwrap().to_string());
    (status, set, body.to_string())
}

/// Opens `forward` here and loads it as a browser would: the link (its
/// ticket for the cookie), then the page.
fn load(client: &ServeClient, forward: &pi_rpc::serve::Forward) -> String {
    let url = client.local_page(forward).unwrap();
    assert!(url.starts_with("http://127.0.0.1:"), "{url}");
    let (status, cookie, _) = fetch(&url, None);
    assert_eq!(status, 303, "{url}");
    let page = format!("{}/", &url[..url[7..].find('/').unwrap() + 7]);
    let (status, _, body) = fetch(&page, cookie.as_deref());
    assert_eq!(status, 200, "{body}");
    body
}

#[test]
#[ignore = "needs GENTLE_SHELL_CHECKOUT (a gentle-shell checkout with serve TLS and the relay), node and python3"]
fn a_real_forward_opens_here_through_the_local_proxy_directly_and_through_the_relay() {
    let checkout = PathBuf::from(
        std::env::var_os("GENTLE_SHELL_CHECKOUT").expect("GENTLE_SHELL_CHECKOUT is set"),
    );
    let home = std::env::temp_dir().join(format!("gentle-serve-fwd-{}", std::process::id()));
    let site = home.join("site");
    std::fs::create_dir_all(&site).unwrap();
    std::fs::write(site.join("index.html"), "hello from python\n").unwrap();

    // The page: a plain HTTP server on the server's loopback.
    let page_port = {
        let probe = TcpListener::bind("127.0.0.1:0").unwrap();
        probe.local_addr().unwrap().port()
    };
    let _page = Running(
        Command::new("python3")
            .args(["-m", "http.server", "--bind", "127.0.0.1"])
            .arg(page_port.to_string())
            .current_dir(&site)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("python3 runs"),
    );
    let deadline = Instant::now() + Duration::from_secs(10);
    while TcpStream::connect(("127.0.0.1", page_port)).is_err() {
        assert!(
            Instant::now() < deadline,
            "python3 -m http.server never listened"
        );
        std::thread::sleep(Duration::from_millis(100));
    }

    let (_relay, relay) = start(
        &checkout,
        &home,
        &["relay", "--listen", "127.0.0.1:0", "--json"],
    );
    let relay_url = format!("gsr://127.0.0.1:{}", relay["port"]);
    let (_serve, serve) = start(
        &checkout,
        &home,
        &[
            "serve",
            "--foreground",
            "--json",
            "--listen",
            "127.0.0.1:0",
            "--relay",
            &relay_url,
        ],
    );
    let plain_url = serve["url"].as_str().unwrap().to_string();
    let fingerprint = serve["fingerprint"].as_str().unwrap().to_string();
    let pair = gentle_shell(&checkout, &home, &["serve", "pair", "--json"])
        .output()
        .unwrap();
    let code = serde_json::from_slice::<Value>(&pair.stdout).unwrap()["code"]
        .as_str()
        .unwrap()
        .to_string();
    let (url, paired) = pi_rpc::serve::pair_at(&plain_url, &code, "serve live test").unwrap();

    // Directly: the forward's link is https:// with the pinned certificate.
    let pinned = ServeClient::with_trust(&url, Some(paired.token.clone()), paired.trust.clone());
    let forward = pinned.server_forward(page_port, "/").unwrap();
    assert!(forward.url.starts_with("https://"), "{}", forward.url);
    assert!(load(&pinned, &forward).contains("hello from python"));

    // Through the relay: the link names the relay's host.
    let through = ServeClient::with_trust(
        &relay_url,
        Some(paired.token),
        Trust::pinned(&fingerprint, Some(&relay_url)),
    );
    let deadline = Instant::now() + Duration::from_secs(10);
    let forward = loop {
        match through.server_forward(page_port, "/") {
            Ok(forward) => break forward,
            Err(e) if Instant::now() < deadline => {
                eprintln!("waiting for the relay: {}", e.message);
                std::thread::sleep(Duration::from_millis(200));
            }
            Err(e) => panic!("{}", e.message),
        }
    };
    assert!(load(&through, &forward).contains("hello from python"));
    let _ = std::fs::remove_dir_all(&home);
}
