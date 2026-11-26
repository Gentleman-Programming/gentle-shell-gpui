//! `maestro`: Maestro flows, the view hierarchy and screenshots on the iOS
//! simulator or Android emulator open in the app's device panel, run here
//! with this computer's Maestro CLI (`setup::maestro`). An agent that takes
//! the app's host tools gets this tool instead of Maestro's MCP server
//! (which on a server would run Maestro there, where no simulator is).

use std::io::Read;
use std::path::Path;
use std::process::{Command, ExitStatus, Stdio};
use std::sync::Mutex;
use std::sync::mpsc::{self, RecvTimeoutError};
use std::time::{Duration, Instant};

use gentle_core::devices::{Device, Platform, State};
use pi_rpc::desktop_rpc::{HostTool, HostToolContent};
use serde_json::{Value, json};

use super::{Outcome, Place, tool};
use crate::features::devices::{android_sdk, find_device, hub, simctl_path};
use crate::features::setup::maestro as setup;

const ACTIONS: [&str; 3] = ["run_flow", "hierarchy", "screenshot"];
/// How long a flow may run; the server ends a call after 10 minutes.
const FLOW_TIMEOUT: Duration = Duration::from_secs(300);
/// How long `maestro hierarchy` may take (it starts Maestro's driver).
const HIERARCHY_TIMEOUT: Duration = Duration::from_secs(120);
/// How long a screenshot may take.
const SHOT_TIMEOUT: Duration = Duration::from_secs(30);
/// How long output is still read after the program ended: a process it
/// left running may hold the pipes open.
const PIPE_GRACE: Duration = Duration::from_secs(2);
/// Larger flows are refused.
const MAX_FLOW: usize = 256 * 1024;
/// The end of a flow's output kept in the result (failures are at the end).
const MAX_OUTPUT: usize = 24 * 1024;
/// The start of the view hierarchy kept in the result.
const MAX_HIERARCHY: usize = 64 * 1024;

/// One Maestro run at a time: two on the same device would fight over it.
static RUNNING: Mutex<()> = Mutex::new(());

/// What a `maestro` call does.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    /// `maestro test` on this flow's text.
    RunFlow(String),
    /// `maestro hierarchy`.
    Hierarchy,
    /// A picture of the device's screen.
    Screenshot,
}

/// A `maestro` call: the action, on the open device `device` names (the
/// only open one when `None`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Op {
    pub device: Option<String>,
    pub action: Action,
}

/// Whether the tool is offered: agents may use devices (the `device` tool's
/// switch) and the Maestro CLI is on this computer.
pub fn offered() -> bool {
    gated(super::device::offered(), setup::installed_here())
}

/// The offer rule: both devices and Maestro.
fn gated(devices: bool, maestro: bool) -> bool {
    devices && maestro
}

pub fn definition(place: Place) -> HostTool {
    let side = place.user_side();
    let guidelines: [&str; 2] = match place {
        Place::Here => [
            "Use maestro for Maestro flows on simulators and emulators, not the maestro CLI or Maestro MCP tools: it drives the device open in the app's device panel, where the user watches it.",
            "run_flow takes the whole flow in yaml: appId: <id>, a line with ---, then commands (launchApp, tapOn, inputText, assertVisible, scrollUntilVisible, takeScreenshot…). It runs from a temporary file: name files the flow uses (runFlow, addMedia, runScript) by absolute path, or inline those steps.",
        ],
        Place::Inside | Place::Server => [
            "Use maestro for Maestro flows on simulators and emulators. Never run the maestro CLI or Maestro MCP tools on your own machine: it has no simulator.",
            "run_flow takes the whole flow in yaml: appId: <id>, a line with ---, then commands (launchApp, tapOn, inputText, assertVisible, scrollUntilVisible, takeScreenshot…). It runs on the user's computer: files the flow names (runFlow, addMedia, runScript) aren't there, so inline those steps.",
        ],
    };
    tool(
        "maestro",
        "Maestro",
        format!(
            "Maestro UI testing on {side}, on the iOS simulator or Android emulator open in the \
app's device panel (open one with the device tool first). run_flow runs a Maestro flow and \
answers with Maestro's output and whether it passed; hierarchy answers with the screen's view \
hierarchy (JSON, trimmed); screenshot answers with a picture of the screen."
        ),
        json!({
            "type": "object",
            "properties": {
                "action": { "type": "string", "enum": ACTIONS },
                "yaml": { "type": "string", "description": "run_flow: the whole flow file: its config (appId: <bundle or package id>), a line with ---, then the commands" },
                "device": { "type": "string", "description": "The open device's name or id, as device list shows it; may be left out when one device is open" }
            },
            "required": ["action"]
        }),
        &[
            guidelines[0],
            guidelines[1],
            "A flow may run up to 5 minutes; keep each flow short and read its output: a failure names the step that failed.",
            "Use hierarchy to find the ids and texts to select on, and screenshot to see the screen.",
        ],
    )
}

/// The call `args` asks for, or what is wrong with it.
pub fn route(args: &Value) -> Result<Op, String> {
    let text = |key: &str| {
        args.get(key)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|text| !text.is_empty())
    };
    let action = text("action").unwrap_or_default();
    if !ACTIONS.contains(&action) {
        return Err(format!("maestro: unknown action {action:?}"));
    }
    let device = text("device").map(str::to_string);
    if device
        .as_deref()
        .is_some_and(|device| device.starts_with('-'))
    {
        return Err(format!("maestro {action}: device can't start with '-'"));
    }
    let action = match action {
        "run_flow" => {
            // The flow as sent: YAML's indentation counts.
            let yaml = args
                .get("yaml")
                .and_then(Value::as_str)
                .filter(|yaml| !yaml.trim().is_empty())
                .ok_or("maestro run_flow needs yaml (the flow's text)")?;
            if yaml.len() > MAX_FLOW {
                return Err(format!(
                    "maestro run_flow: the flow is over {} KB",
                    MAX_FLOW / 1024
                ));
            }
            Action::RunFlow(yaml.to_string())
        }
        "hierarchy" => Action::Hierarchy,
        _ => Action::Screenshot,
    };
    Ok(Op { device, action })
}

/// The Maestro arguments on the device `id`: `maestro test` on the file
/// `flow`, or `maestro hierarchy` without one. Maestro has no screenshot of
/// its own outside a flow.
fn maestro_args(id: &str, flow: Option<&Path>) -> Vec<String> {
    let mut args = vec!["--device".to_string(), id.to_string()];
    match flow {
        Some(flow) => args.extend(["test".into(), flow.to_string_lossy().into_owned()]),
        None => args.push("hierarchy".into()),
    }
    args
}

/// The arguments of the platform tool that takes a screenshot of `device`:
/// `simctl` writes it to `path`; `adb` prints it.
fn shot_args(device: &Device, path: &Path) -> Vec<String> {
    match device.platform {
        Platform::Ios => vec![
            "io".into(),
            device.id.clone(),
            "screenshot".into(),
            "--type=png".into(),
            path.to_string_lossy().into_owned(),
        ],
        Platform::Android => vec![
            "-s".into(),
            device.id.clone(),
            "exec-out".into(),
            "screencap".into(),
            "-p".into(),
        ],
    }
}

/// The running device `query` names, or the only running one.
fn pick(devices: &[Device], query: Option<&str>) -> Result<Device, String> {
    let running: Vec<Device> = devices
        .iter()
        .filter(|device| device.state == State::Ready)
        .cloned()
        .collect();
    if let Some(query) = query {
        let device = find_device(devices, query).ok_or_else(|| {
            format!(
                "No simulator or emulator named \"{query}\"; the device tool's list shows them."
            )
        })?;
        if device.state != State::Ready {
            return Err(format!(
                "{} isn't running: open it with the device tool first.",
                device.name
            ));
        }
        return Ok(device.clone());
    }
    match running.as_slice() {
        [] => Err(
            "No device is open in the app's device panel: open one with the device tool first."
                .into(),
        ),
        [device] => Ok(device.clone()),
        several => Err(format!(
            "Several devices are open ({}): say which one with device.",
            several
                .iter()
                .map(|device| format!("\"{}\"", device.name))
                .collect::<Vec<_>>()
                .join(", ")
        )),
    }
}

/// Run `op`. Blocking.
pub fn run(op: Op) -> Outcome {
    let _running = RUNNING.lock().unwrap_or_else(|e| e.into_inner());
    let result = hub::running_port()
        .ok_or_else(|| {
            "No device is open in the app's device panel: open one with the device tool first."
                .to_string()
        })
        .and_then(hub::list)
        .and_then(|(devices, _)| pick(&devices, op.device.as_deref()))
        .and_then(|device| match &op.action {
            Action::Screenshot => shot(&device),
            Action::RunFlow(yaml) => run_flow(&device, yaml),
            Action::Hierarchy => hierarchy(&device),
        });
    result.unwrap_or_else(Outcome::error)
}

/// This computer's `maestro` with `args`, for at most `timeout`.
fn run_maestro(args: Vec<String>, timeout: Duration) -> Result<Ran, String> {
    let mut command = setup::command_here()?;
    command.args(args);
    run_within(command, timeout)
}

fn run_flow(device: &Device, yaml: &str) -> Result<Outcome, String> {
    let flow = super::scratch("maestro", "flow.yaml");
    std::fs::write(&flow, yaml).map_err(|error| format!("couldn't write the flow: {error}"))?;
    let ran = run_maestro(maestro_args(&device.id, Some(&flow)), FLOW_TIMEOUT);
    let _ = std::fs::remove_file(&flow);
    let ran = ran?;
    let output = tail(&strip_ansi(&ran.text()), MAX_OUTPUT);
    let on = describe(device);
    Ok(match ran.status {
        Some(status) if status.success() => {
            Outcome::text(format!("Flow passed on {on}.\n\n{output}"))
        }
        Some(status) => Outcome::error(format!(
            "Flow failed on {on} ({}).\n\n{output}",
            exit(status)
        )),
        None => Outcome::error(format!(
            "The flow didn't finish within {}s on {on} and was stopped.\n\n{output}",
            FLOW_TIMEOUT.as_secs()
        )),
    })
}

fn hierarchy(device: &Device) -> Result<Outcome, String> {
    let ran = run_maestro(maestro_args(&device.id, None), HIERARCHY_TIMEOUT)?;
    let output = || tail(&strip_ansi(&ran.text()), MAX_OUTPUT);
    match ran.status {
        Some(status) if status.success() => Ok(Outcome::text(compact_hierarchy(
            &String::from_utf8_lossy(&ran.stdout),
        ))),
        Some(status) => Err(format!(
            "maestro hierarchy failed ({}): {}",
            exit(status),
            output()
        )),
        None => Err(format!(
            "maestro hierarchy didn't finish within {}s. {}",
            HIERARCHY_TIMEOUT.as_secs(),
            output()
        )),
    }
}

fn shot(device: &Device) -> Result<Outcome, String> {
    let path = super::scratch("maestro", "shot.png");
    let args = shot_args(device, &path);
    let program = match device.platform {
        Platform::Ios => simctl_path().ok_or("Xcode's simctl isn't on the user's computer.")?,
        Platform::Android => android_sdk()
            .map(|sdk| {
                sdk.join("platform-tools")
                    .join(if cfg!(windows) { "adb.exe" } else { "adb" })
            })
            .filter(|adb| adb.is_file())
            .or_else(|| pi_rpc::find_program("adb"))
            .ok_or("adb isn't on the user's computer.")?,
    };
    let mut command = pi_rpc::command(program);
    command.args(&args).env("PATH", pi_rpc::child_path());
    let ran = run_within(command, SHOT_TIMEOUT);
    let bytes = match device.platform {
        Platform::Ios => std::fs::read(&path).map_err(|error| format!("no picture: {error}")),
        Platform::Android => Ok(ran
            .as_ref()
            .map(|ran| ran.stdout.clone())
            .unwrap_or_default()),
    };
    let _ = std::fs::remove_file(&path);
    let ran = ran?;
    if !ran.status.is_some_and(|status| status.success()) {
        return Err(format!("The screenshot failed: {}", ran.text().trim()));
    }
    let bytes = bytes?;
    if !bytes.starts_with(b"\x89PNG") {
        return Err("The screenshot isn't a PNG picture.".into());
    }
    Ok(Outcome {
        content: vec![
            HostToolContent::text(format!("Screenshot of {}", describe(device))),
            super::png(&bytes),
        ],
        is_error: false,
    })
}

/// `iPhone 17 (iOS 26.0)`.
fn describe(device: &Device) -> String {
    format!(
        "{} ({})",
        device.name,
        device.os.as_deref().unwrap_or(device.platform.label())
    )
}

/// `exit code 1`, or how the program ended without one.
fn exit(status: ExitStatus) -> String {
    match status.code() {
        Some(code) => format!("exit code {code}"),
        None => "stopped by a signal".into(),
    }
}

/// What a program printed and how it ended.
struct Ran {
    /// `None`: it ran past its time and was stopped.
    status: Option<ExitStatus>,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
}

impl Ran {
    /// Stdout, then stderr, as text.
    fn text(&self) -> String {
        [&self.stdout, &self.stderr]
            .map(|bytes| String::from_utf8_lossy(bytes).trim().to_string())
            .into_iter()
            .filter(|text| !text.is_empty())
            .collect::<Vec<_>>()
            .join("\n")
    }
}

/// Run `command` without input for at most `timeout`, then stop it. The
/// pipes are read on threads of their own and given up shortly after the
/// program ends, so a process it left running can't keep the call waiting.
fn run_within(mut command: Command, timeout: Duration) -> Result<Ran, String> {
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| format!("couldn't start {:?}: {error}", command.get_program()))?;
    pi_rpc::contain(&child);
    let (tx, rx) = mpsc::channel::<(bool, Vec<u8>)>();
    let pipe = |mut reader: Box<dyn Read + Send>, is_err: bool| {
        let tx = tx.clone();
        std::thread::spawn(move || {
            let mut buf = [0u8; 8192];
            while let Ok(n) = reader.read(&mut buf) {
                if n == 0 || tx.send((is_err, buf[..n].to_vec())).is_err() {
                    break;
                }
            }
        });
    };
    pipe(Box::new(child.stdout.take().expect("piped")), false);
    pipe(Box::new(child.stderr.take().expect("piped")), true);
    drop(tx);
    let deadline = Instant::now() + timeout;
    let mut ran = Ran {
        status: None,
        stdout: Vec::new(),
        stderr: Vec::new(),
    };
    let mut ended: Option<Instant> = None;
    loop {
        match rx.recv_timeout(Duration::from_millis(50)) {
            Ok((true, chunk)) => ran.stderr.extend_from_slice(&chunk),
            Ok((false, chunk)) => ran.stdout.extend_from_slice(&chunk),
            Err(RecvTimeoutError::Disconnected) if ended.is_some() => break,
            Err(RecvTimeoutError::Disconnected) => std::thread::sleep(Duration::from_millis(50)),
            Err(RecvTimeoutError::Timeout) => {}
        }
        match ended {
            Some(at) if at.elapsed() > PIPE_GRACE => break,
            Some(_) => {}
            None => match child.try_wait() {
                Ok(Some(status)) => {
                    ran.status = Some(status);
                    ended = Some(Instant::now());
                }
                Ok(None) if Instant::now() < deadline => {}
                _ => {
                    let _ = child.kill();
                    let _ = child.wait();
                    ended = Some(Instant::now());
                }
            },
        }
    }
    Ok(ran)
}

/// `text` without ANSI escape sequences (colors, cursor moves).
fn strip_ansi(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        if c != '\u{1b}' {
            out.push(c);
            continue;
        }
        if chars.next() == Some('[') {
            for c in chars.by_ref() {
                if ('@'..='~').contains(&c) {
                    break;
                }
            }
        }
    }
    out
}

/// The last `max` bytes of `text`, cut at a character.
fn tail(text: &str, max: usize) -> String {
    let text = text.trim();
    if text.len() <= max {
        return text.to_string();
    }
    let mut start = text.len() - max;
    while !text.is_char_boundary(start) {
        start += 1;
    }
    format!("…(earlier output cut)\n{}", &text[start..])
}

/// `maestro hierarchy`'s JSON without empty attributes and children, on one
/// line, its first [`MAX_HIERARCHY`] bytes; what it printed as it is when
/// that isn't JSON.
fn compact_hierarchy(out: &str) -> String {
    let compact = out
        .find('{')
        .and_then(|start| serde_json::from_str::<Value>(&out[start..]).ok())
        .map(|mut value| {
            prune(&mut value);
            value.to_string()
        })
        .unwrap_or_else(|| strip_ansi(out).trim().to_string());
    if compact.len() <= MAX_HIERARCHY {
        return compact;
    }
    let mut end = MAX_HIERARCHY;
    while !compact.is_char_boundary(end) {
        end -= 1;
    }
    format!(
        "{}\n…(cut at {} KB of {} KB; select by the ids and texts above, or take a screenshot)",
        &compact[..end],
        MAX_HIERARCHY / 1024,
        compact.len() / 1024
    )
}

/// Drop nulls and empty strings, arrays and objects from `value`, deepest
/// first.
fn prune(value: &mut Value) {
    match value {
        Value::Object(map) => {
            map.values_mut().for_each(prune);
            map.retain(|_, value| !is_empty(value));
        }
        Value::Array(items) => {
            items.iter_mut().for_each(prune);
            items.retain(|value| !is_empty(value));
        }
        _ => {}
    }
}

fn is_empty(value: &Value) -> bool {
    match value {
        Value::Null => true,
        Value::String(text) => text.is_empty(),
        Value::Array(items) => items.is_empty(),
        Value::Object(map) => map.is_empty(),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn device(name: &str, id: &str, platform: Platform, state: State) -> Device {
        Device {
            platform,
            id: id.into(),
            name: name.into(),
            state,
            os: None,
        }
    }

    #[test]
    fn offered_with_devices_and_maestro_only() {
        assert!(gated(true, true));
        assert!(!gated(true, false));
        assert!(!gated(false, true));
        assert!(!gated(false, false));
        // Valid however this computer is set up, wherever the agent runs.
        for place in [Place::Here, Place::Inside, Place::Server] {
            definition(place)
                .validate()
                .unwrap_or_else(|error| panic!("{error}"));
        }
    }

    #[test]
    fn actions_become_calls() {
        assert_eq!(
            route(&json!({"action": "run_flow", "yaml": "appId: a\n---\n- launchApp\n"})),
            Ok(Op {
                device: None,
                action: Action::RunFlow("appId: a\n---\n- launchApp\n".into()),
            })
        );
        assert_eq!(
            route(&json!({"action": "hierarchy", "device": " iPhone 17 "})),
            Ok(Op {
                device: Some("iPhone 17".into()),
                action: Action::Hierarchy,
            })
        );
        assert_eq!(
            route(&json!({"action": "screenshot"})).map(|op| op.action),
            Ok(Action::Screenshot)
        );
    }

    #[test]
    fn bad_calls_say_what_is_wrong() {
        assert!(route(&json!({})).unwrap_err().contains("unknown action"));
        assert!(route(&json!({"action": "test"})).is_err());
        assert!(
            route(&json!({"action": "run_flow"}))
                .unwrap_err()
                .contains("yaml")
        );
        assert!(route(&json!({"action": "run_flow", "yaml": "  "})).is_err());
        let huge = "x".repeat(MAX_FLOW + 1);
        assert!(
            route(&json!({"action": "run_flow", "yaml": huge}))
                .unwrap_err()
                .contains("KB")
        );
        assert!(
            route(&json!({"action": "hierarchy", "device": "--udid"}))
                .unwrap_err()
                .contains("'-'")
        );
    }

    #[test]
    fn maestro_runs_pinned_to_the_device() {
        assert_eq!(
            maestro_args("ABC-123", Some(Path::new("/tmp/flow.yaml"))),
            ["--device", "ABC-123", "test", "/tmp/flow.yaml"].map(String::from)
        );
        assert_eq!(
            maestro_args("emulator-5554", None),
            ["--device", "emulator-5554", "hierarchy"].map(String::from)
        );
    }

    #[test]
    fn screenshots_use_the_platform_tool() {
        let path = Path::new("/tmp/shot.png");
        let ios = device("iPhone 17", "ABC-123", Platform::Ios, State::Ready);
        assert_eq!(
            shot_args(&ios, path),
            ["io", "ABC-123", "screenshot", "--type=png", "/tmp/shot.png"].map(String::from)
        );
        let android = device("Pixel 7", "emulator-5554", Platform::Android, State::Ready);
        assert_eq!(
            shot_args(&android, path),
            ["-s", "emulator-5554", "exec-out", "screencap", "-p"].map(String::from)
        );
    }

    #[test]
    fn the_device_is_the_open_one_or_the_one_named() {
        let phone = device("iPhone 17", "ABC", Platform::Ios, State::Ready);
        let off = device("iPad", "DEF", Platform::Ios, State::Shutdown);
        let pixel = device("Pixel_7", "emulator-5554", Platform::Android, State::Ready);
        assert_eq!(pick(&[phone.clone(), off.clone()], None), Ok(phone.clone()));
        assert!(
            pick(std::slice::from_ref(&off), None)
                .unwrap_err()
                .contains("No device is open")
        );
        let both = [phone.clone(), pixel.clone()];
        assert!(pick(&both, None).unwrap_err().contains("\"Pixel_7\""));
        assert_eq!(pick(&both, Some("pixel 7")), Ok(pixel));
        assert!(
            pick(&[off], Some("iPad"))
                .unwrap_err()
                .contains("isn't running")
        );
        assert!(
            pick(&both, Some("Galaxy"))
                .unwrap_err()
                .contains("No simulator")
        );
    }

    #[test]
    fn output_is_plain_and_bounded() {
        assert_eq!(
            strip_ansi("\u{1b}[32m✓ Launch\u{1b}[0m app"),
            "✓ Launch app"
        );
        assert_eq!(tail("  short  ", 10), "short");
        let cut = tail("aaaaé-end", 5);
        assert!(cut.starts_with("…(earlier output cut)\n"));
        assert!(cut.ends_with("-end"));
    }

    #[test]
    fn the_hierarchy_drops_empty_attributes() {
        let out = r#"Running on iPhone
{"attributes": {"text": "Sign in", "resource-id": "", "enabled": "true"},
 "children": [{"attributes": {"text": ""}, "children": []}], "clickable": null}"#;
        assert_eq!(
            compact_hierarchy(out),
            r#"{"attributes":{"text":"Sign in","enabled":"true"}}"#
        );
        assert_eq!(compact_hierarchy("No device\n"), "No device");
        let big = json!({ "attributes": { "text": "y".repeat(MAX_HIERARCHY * 2) } }).to_string();
        let cut = compact_hierarchy(&big);
        assert!(cut.contains("…(cut at 64 KB"));
        assert!(cut.len() < MAX_HIERARCHY + 200);
    }
}
