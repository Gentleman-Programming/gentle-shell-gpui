//! Desktop requests to a child over its stdio (`PiProcess::desktop`), as a
//! local tab registers and answers host tools: the `/gentle:desktop` prompt
//! goes in on stdin, and the widget record with its `requestId` comes back
//! to the caller, never to `incoming`, while every other record still does.
//! The child is this test binary itself ([`fake_child`]), so it runs on
//! every system.

use std::io::{BufRead as _, Write as _};
use std::time::{Duration, Instant};

use pi_rpc::desktop_rpc::{DesktopRequest, HostToolContent, SCHEMA, WIDGET_KEY, decode_widget};
use pi_rpc::{Incoming, LaunchSpec, PiProcess};
use serde_json::{Value, json};

/// Set when this test binary runs as [`fake_child`].
const FAKE: &str = "PI_RPC_TEST_FAKE_DESKTOP_CHILD";

fn widget(record: Value) -> Value {
    json!({
        "type": "extension_ui_request",
        "id": format!("w-{}", record["requestId"].as_str().unwrap_or("event")),
        "method": "setWidget",
        "widgetKey": WIDGET_KEY,
        "widgetLines": [record.to_string()],
    })
}

/// Not a test of its own: the Gentle Shell the other test starts. For each
/// `/gentle:desktop` prompt it prints an event and a response for another
/// request (the app's channel's), then the answer (before the prompt's own
/// response: the order is Pi's to choose). A request whose id starts with
/// `refuse` is not taken and gets no answer.
#[test]
fn fake_child() {
    if std::env::var_os(FAKE).is_none() {
        return;
    }
    let mut out = std::io::stdout().lock();
    let mut print = |record: Value| {
        out.write_all(format!("{record}\n").as_bytes()).unwrap();
        out.flush().unwrap();
    };
    let mut seq = 0;
    for line in std::io::stdin().lock().lines() {
        let Ok(line) = line else { break };
        let Ok(command) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        let Some(inner) = command["message"]
            .as_str()
            .and_then(|message| message.strip_prefix("/gentle:desktop "))
        else {
            continue;
        };
        let inner: Value = serde_json::from_str(inner).unwrap();
        let request_id = inner["requestId"].as_str().unwrap().to_string();
        let taken = |disposition: &str| {
            json!({
                "type": "response", "id": command["id"], "command": "prompt",
                "success": true, "data": { "disposition": disposition },
            })
        };
        if request_id.starts_with("refuse") {
            print(taken("unknown_command"));
            continue;
        }
        seq += 1;
        print(widget(json!({
            "schema": SCHEMA, "kind": "event", "seq": seq,
            "event": "host_tool_cancel", "data": { "callId": format!("c{seq}") },
        })));
        print(widget(json!({
            "schema": SCHEMA, "kind": "response", "requestId": "desktop-1",
            "ok": true, "data": {},
        })));
        let data = match inner["method"].as_str() {
            Some("set_host_tools") => json!({ "registered": ["open_page"], "rejected": [] }),
            _ => json!({ "accepted": true }),
        };
        print(widget(json!({
            "schema": SCHEMA, "kind": "response", "requestId": request_id,
            "ok": true, "data": data,
        })));
        print(taken("handled"));
    }
}

/// The first `count` Desktop widget records that reached `incoming` (within
/// 10 s), as `(kind, seq or requestId)`.
fn widgets(incoming: &async_channel::Receiver<Incoming>, count: usize) -> Vec<(String, Value)> {
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut seen = Vec::new();
    while seen.len() < count && Instant::now() < deadline {
        match incoming.try_recv() {
            Ok(Incoming::Ui(request)) => {
                let decoded = decode_widget(&request).unwrap().unwrap();
                seen.push(match decoded {
                    pi_rpc::desktop_rpc::DesktopRecord::Event(event) => {
                        ("event".to_string(), json!(event.seq))
                    }
                    pi_rpc::desktop_rpc::DesktopRecord::Response(response) => {
                        ("response".to_string(), json!(response.request_id()))
                    }
                });
            }
            Ok(_) => {}
            Err(_) => std::thread::sleep(Duration::from_millis(10)),
        }
    }
    seen
}

#[test]
fn desktop_answers_come_back_by_request_id() {
    let exe = std::env::current_exe().unwrap();
    let spec = LaunchSpec {
        launcher: exe.clone(),
        program: exe,
        args: [
            "--exact",
            "fake_child",
            "--nocapture",
            "--test-threads=1",
            "-q",
        ]
        .map(str::to_string)
        .to_vec(),
        cwd: std::env::temp_dir(),
        env: vec![(FAKE.to_string(), "1".into())],
        bundled: None,
    };
    let (process, incoming) = PiProcess::spawn(&spec).unwrap();

    let tools = DesktopRequest::set_host_tools("host-1", "desktop", &[]).unwrap();
    let answer = process.desktop(&tools).unwrap();
    assert_eq!(answer.request_id(), Some("host-1"));
    assert_eq!(answer.set_host_tools().unwrap().registered, ["open_page"]);

    let result = DesktopRequest::host_tool_result(
        "host-2",
        "c1",
        &[HostToolContent::text("done")],
        false,
        None,
    )
    .unwrap();
    let answer = process.desktop(&result).unwrap();
    assert_eq!(answer.request_id(), Some("host-2"));
    assert!(answer.is_ok());

    // The events and the channel's own response went on, in order, with
    // their sequence untouched; the answers didn't.
    assert_eq!(
        widgets(&incoming, 4),
        [
            ("event".to_string(), json!(1)),
            ("response".to_string(), json!("desktop-1")),
            ("event".to_string(), json!(2)),
            ("response".to_string(), json!("desktop-1")),
        ]
    );

    // A request the child doesn't take fails without waiting for an answer.
    let refused = DesktopRequest::set_host_tools("refuse-3", "desktop", &[]).unwrap();
    let started = Instant::now();
    let error = process.desktop(&refused).unwrap_err().to_string();
    assert!(error.contains("unknown_command"), "{error}");
    assert!(started.elapsed() < Duration::from_secs(10));

    process.shutdown(Duration::from_secs(1));
}
