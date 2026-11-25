//! Live check of pinned TLS, the pairing proof and the relay against a real
//! `gentle-shell serve` and `gentle-shell relay` from a Gentle Shell
//! checkout, in a temporary home (nothing of this computer's Gentle Shell
//! is touched; both listen on loopback only).
//!
//! `#[ignore]` by default: ordinary runs stay hermetic (`tests/serve.rs`
//! covers the same paths against a fake). To run it:
//!
//! ```text
//! GENTLE_SHELL_CHECKOUT=/path/to/gentle-shell \
//!   cargo test -p pi-rpc --test serve_live -- --ignored --nocapture
//! ```

use std::io::{BufRead as _, BufReader};
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
