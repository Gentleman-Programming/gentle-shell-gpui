//! `device`: what `gentle-device` (list, open, close) and the `agent-device`
//! calls the app's instructions tell an agent to use do, on the iOS
//! simulators and Android emulators of the user's computer. The window
//! boots and shows a device as `gentle-device` asks it to; the rest runs
//! `agent-device` here, pinned to the device as `gentle-device open` pins it.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, Instant};

use gentle_core::devices::{Device, Platform, State};
use pi_rpc::desktop_rpc::{HostTool, HostToolContent};
use serde_json::{Value, json};

use super::{Outcome, WHERE, tool};
use crate::features::devices::{cli, find_device, hub};
use crate::features::setup::devices as setup;

const ACTIONS: [&str; 9] = [
    "list",
    "open",
    "close",
    "launch",
    "snapshot",
    "press",
    "fill",
    "screenshot",
    "install",
];
/// How long an `agent-device` call may take; an install up to what
/// agent-device itself allows.
const DRIVE_TIMEOUT: Duration = Duration::from_secs(180);
const INSTALL_TIMEOUT: Duration = Duration::from_secs(480);
/// Larger downloads are refused.
const MAX_DOWNLOAD: u64 = 4 << 30;

/// Where an app to install comes from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Source {
    /// A file (or `.app` folder) on the user's computer.
    Path(PathBuf),
    /// An http(s) URL the app downloads first.
    Url(String),
}

/// A `device` call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Op {
    List,
    Open(String),
    Close {
        device: String,
        shutdown: bool,
    },
    /// `agent-device <args>` pinned to `device`; `screenshot` adds the
    /// picture's path and answers with the picture.
    Drive {
        device: String,
        args: Vec<String>,
        screenshot: bool,
    },
    Install {
        device: String,
        app: Source,
    },
}

/// Whether the user let agents use devices (Configuration → Devices).
pub fn offered() -> bool {
    let prefs = setup::prefs();
    prefs.enabled && prefs.agents
}

pub fn definition() -> HostTool {
    tool(
        "device",
        "Device",
        format!(
            "iOS simulators and Android emulators on {WHERE}, shown live in the app's device panel \
where the user watches them. list; open a device (boots it without a separate window); then \
launch an app, snapshot what's on screen (with @refs), press and fill those refs, screenshot, \
install an app build; close it when done."
        ),
        json!({
            "type": "object",
            "properties": {
                "action": { "type": "string", "enum": ACTIONS },
                "device": { "type": "string", "description": "The device's name or id, as list shows it (every action but list)" },
                "app": { "type": "string", "description": "launch: the bundle or package id. install: an absolute path on the user's computer or an http(s) URL the app downloads (.app, .zip holding a .app, .ipa, .apk)" },
                "target": { "type": "string", "description": "press, fill: an @ref from the latest snapshot (e.g. @e3) or a selector" },
                "text": { "type": "string", "description": "fill: the text that replaces the field's" },
                "shutdown": { "type": "boolean", "description": "close: also turn the device off" }
            },
            "required": ["action"]
        }),
        &[
            "Use device for every iOS simulator and Android emulator: list, then open \"<name>\"; drive it only with this tool's actions. Never boot or drive one with shell commands (no xcrun simctl, open -a Simulator, emulator -avd, adb shell input, idb, or scripts that wrap them).",
            "After launch or press/fill, read the snapshot it answers with; snapshot again only when the ref you need isn't there. Refs go stale after the screen changes.",
            "install runs on the user's computer: a path you built on your own machine doesn't exist there. Build with the project's usual tools, then make the build reachable (an http(s) URL, e.g. a file server or artifact link) and pass that URL, or ask the user for a path on their computer.",
            "close with shutdown: true turns a device off when you're done with it.",
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
    if action == "list" {
        return Ok(Op::List);
    }
    if !ACTIONS.contains(&action) {
        return Err(format!("device: unknown action {action:?}"));
    }
    let device = text("device")
        .ok_or_else(|| format!("device {action} needs the device's name or id"))?
        .to_string();
    // An argument agent-device would read as a flag.
    let operand = |key: &str| -> Result<String, String> {
        let value = text(key).ok_or_else(|| format!("device {action} needs {key}"))?;
        if value.starts_with('-') {
            return Err(format!("device {action}: {key} can't start with '-'"));
        }
        Ok(value.to_string())
    };
    let drive = |args: Vec<String>| Op::Drive {
        device: device.clone(),
        args,
        screenshot: false,
    };
    Ok(match action {
        "open" => Op::Open(device),
        "close" => Op::Close {
            shutdown: args.get("shutdown").and_then(Value::as_bool) == Some(true),
            device,
        },
        "launch" => drive(vec!["open".into(), operand("app")?]),
        "snapshot" => drive(vec!["snapshot".into(), "-i".into()]),
        "press" => drive(vec!["press".into(), operand("target")?, "--settle".into()]),
        "fill" => {
            let text = args
                .get("text")
                .and_then(Value::as_str)
                .ok_or("device fill needs text")?;
            drive(vec![
                "fill".into(),
                operand("target")?,
                text.to_string(),
                "--settle".into(),
            ])
        }
        "screenshot" => Op::Drive {
            device,
            args: vec!["screenshot".into()],
            screenshot: true,
        },
        _ => {
            let app = text("app").ok_or("device install needs app (a path or URL)")?;
            let app = if app.starts_with("http://") || app.starts_with("https://") {
                Source::Url(app.to_string())
            } else if Path::new(app).is_absolute() {
                Source::Path(PathBuf::from(app))
            } else {
                return Err(
                    "device install needs an absolute path on the user's computer or an http(s) URL"
                        .into(),
                );
            };
            Op::Install { device, app }
        }
    })
}

/// The `gentle-device` arguments the window runs for `op` (it starts the
/// hub, boots and shows a device, closes one); `None` for the rest.
pub fn panel_args(op: &Op) -> Option<Vec<String>> {
    match op {
        Op::List => Some(vec!["list".into()]),
        Op::Open(device) => Some(vec!["open".into(), device.clone()]),
        Op::Close { device, shutdown } => {
            let mut args = vec!["close".into(), device.clone()];
            if *shutdown {
                args.push("--shutdown".into());
            }
            Some(args)
        }
        Op::Drive { .. } | Op::Install { .. } => None,
    }
}

/// Run `op` (after the window ran its [`panel_args`]). Blocking.
pub fn run(op: Op) -> Outcome {
    let result = match op {
        Op::List => list(),
        Op::Open(query) => cli::wait_ready(&query, |_| {}).map(|device| {
            Outcome::text(format!(
                "{} ({}) is running in the app's device panel, where the user watches it. Drive it with this tool, device: \"{}\".",
                device.name,
                device.os.as_deref().unwrap_or(device.platform.label()),
                device.id
            ))
        }),
        Op::Close { device, shutdown } => Ok(Outcome::text(if shutdown {
            format!("Closed {device} and turned it off.")
        } else {
            format!("Closed {device}'s tab.")
        })),
        Op::Drive {
            device,
            args,
            screenshot,
        } => running(&device).and_then(|device| {
            if screenshot {
                shot(&device)
            } else {
                drive(&device, &args, DRIVE_TIMEOUT).map(Outcome::text)
            }
        }),
        Op::Install { device, app } => running(&device).and_then(|device| install(&device, app)),
    };
    result.unwrap_or_else(Outcome::error)
}

fn list() -> Result<Outcome, String> {
    let (devices, errors) = cli::wait_for_hub().and_then(hub::list)?;
    let rows: Vec<Value> = devices
        .iter()
        .map(|d| {
            json!({
                "name": d.name, "id": d.id, "platform": cli::platform_name(d.platform),
                "os": d.os, "running": d.state.is_running(),
            })
        })
        .collect();
    let mut answer = json!({ "devices": rows });
    if devices.is_empty() {
        answer["note"] = "No simulators or emulators: the user needs Xcode (iOS) or Android Studio and one created.".into();
    }
    if !errors.is_empty() {
        answer["errors"] = errors.into();
    }
    Ok(Outcome::text(
        serde_json::to_string_pretty(&answer).unwrap_or_default(),
    ))
}

/// The open device `query` names.
fn running(query: &str) -> Result<Device, String> {
    let port = hub::running_port().ok_or("No device is open: open one first.")?;
    let (devices, _) = hub::list(port)?;
    let device = find_device(&devices, query)
        .ok_or_else(|| format!("No simulator or emulator named \"{query}\"; list shows them."))?;
    if device.state != State::Ready {
        return Err(format!("{} isn't running: open it first.", device.name));
    }
    Ok(device.clone())
}

/// `agent-device` pinned to `device`, as `gentle-device open` prints it.
fn drive(device: &Device, args: &[String], timeout: Duration) -> Result<String, String> {
    let (node, entry) = setup::agent_command().ok_or(
        "agent-device isn't installed: in the app, Configuration → Devices → Agents can use devices.",
    )?;
    let ios = device.platform == Platform::Ios;
    let mut child = pi_rpc::command(&node)
        .arg(&entry)
        .args([
            "--platform",
            cli::platform_name(device.platform),
            if ios { "--udid" } else { "--serial" },
            &device.id,
        ])
        .args(args)
        .env("PATH", pi_rpc::child_path())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| format!("agent-device didn't start: {error}"))?;
    let pipe = |mut reader: Box<dyn Read + Send>| {
        std::thread::spawn(move || {
            let mut text = String::new();
            let _ = reader.read_to_string(&mut text);
            text
        })
    };
    let stdout = pipe(Box::new(child.stdout.take().expect("piped")));
    let stderr = pipe(Box::new(child.stderr.take().expect("piped")));
    let deadline = Instant::now() + timeout;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(100)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                break None;
            }
        }
    };
    let out = stdout.join().unwrap_or_default();
    let err = stderr.join().unwrap_or_default();
    let text = [out.trim(), err.trim()]
        .into_iter()
        .filter(|t| !t.is_empty())
        .collect::<Vec<_>>()
        .join("\n");
    match status {
        Some(status) if status.success() => Ok(if text.is_empty() {
            "Done.".into()
        } else {
            text
        }),
        Some(_) => Err(format!("agent-device failed: {text}")),
        None => Err(format!(
            "agent-device didn't finish within {}s. {text}",
            timeout.as_secs()
        )),
    }
}

/// A temporary path of the app's own for `name`.
fn scratch(name: &str) -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or_default();
    std::env::temp_dir().join(format!(
        "gentle-device-{}-{nanos}-{name}",
        std::process::id()
    ))
}

fn shot(device: &Device) -> Result<Outcome, String> {
    let path = scratch("shot.png");
    let path_text = path.to_string_lossy().into_owned();
    let result = drive(device, &["screenshot".into(), path_text], DRIVE_TIMEOUT)
        .and_then(|_| std::fs::read(&path).map_err(|error| format!("no picture: {error}")));
    let _ = std::fs::remove_file(&path);
    let bytes = result?;
    Ok(Outcome {
        content: vec![
            HostToolContent::text(format!("Screenshot of {}", device.name)),
            super::png(&bytes),
        ],
        is_error: false,
    })
}

fn install(device: &Device, app: Source) -> Result<Outcome, String> {
    let (path, cleanup) = match app {
        Source::Path(path) => {
            if !path.exists() {
                return Err(format!(
                    "{} doesn't exist on the user's computer (a path on your machine isn't there; pass an http(s) URL to the build).",
                    path.display()
                ));
            }
            (path, None)
        }
        Source::Url(url) => {
            let dir = scratch("install");
            let path = download(&url, &dir).inspect_err(|_| {
                let _ = std::fs::remove_dir_all(&dir);
            })?;
            (path, Some(dir))
        }
    };
    let result = drive(
        device,
        &["install".into(), path.to_string_lossy().into_owned()],
        INSTALL_TIMEOUT,
    );
    if let Some(dir) = cleanup {
        let _ = std::fs::remove_dir_all(dir);
    }
    result.map(Outcome::text)
}

/// The file name a download is kept under: the URL's last segment.
fn download_name(url: &str) -> String {
    let path = url.split(['?', '#']).next().unwrap_or_default();
    let name: String = path
        .rsplit('/')
        .next()
        .unwrap_or_default()
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'))
        .collect();
    if name.trim_matches('.').is_empty() {
        "app".into()
    } else {
        name
    }
}

/// Download `url` into `dir`; a `.zip` is unpacked to the `.app`, `.ipa`
/// or `.apk` inside.
fn download(url: &str, dir: &Path) -> Result<PathBuf, String> {
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(600)))
        .build()
        .into();
    let mut response = agent
        .get(url)
        .call()
        .map_err(|error| format!("couldn't download {url}: {error}"))?;
    let path = dir.join(download_name(url));
    let mut file = std::fs::File::create(&path).map_err(|e| e.to_string())?;
    std::io::copy(
        &mut response
            .body_mut()
            .with_config()
            .limit(MAX_DOWNLOAD)
            .reader(),
        &mut file,
    )
    .map_err(|error| format!("couldn't download {url}: {error}"))?;
    drop(file);
    if path.extension().is_none_or(|ext| ext != "zip") {
        return Ok(path);
    }
    let unpacked = dir.join("unpacked");
    let archive = std::fs::File::open(&path).map_err(|e| e.to_string())?;
    zip::ZipArchive::new(archive)
        .and_then(|mut zip| zip.extract(&unpacked))
        .map_err(|error| format!("couldn't unpack {url}: {error}"))?;
    find_app(&unpacked, 2).ok_or_else(|| format!("{url} holds no .app, .ipa or .apk"))
}

/// The first `.app`, `.ipa` or `.apk` in `dir`, `depth` folders deep.
fn find_app(dir: &Path, depth: u8) -> Option<PathBuf> {
    let mut entries: Vec<PathBuf> = std::fs::read_dir(dir)
        .ok()?
        .flatten()
        .map(|e| e.path())
        .collect();
    entries.sort();
    let is_app = |p: &Path| {
        p.extension()
            .is_some_and(|ext| ext == "app" || ext == "ipa" || ext == "apk")
    };
    entries.iter().find(|p| is_app(p)).cloned().or_else(|| {
        (depth > 0)
            .then(|| {
                entries
                    .iter()
                    .filter(|p| p.is_dir())
                    .find_map(|p| find_app(p, depth - 1))
            })
            .flatten()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn drive(args: &[&str]) -> Op {
        Op::Drive {
            device: "iPhone 17".into(),
            args: args.iter().map(|a| a.to_string()).collect(),
            screenshot: false,
        }
    }

    #[test]
    fn actions_become_gentle_device_and_agent_device_calls() {
        assert_eq!(route(&json!({"action": "list"})), Ok(Op::List));
        let open = route(&json!({"action": "open", "device": " iPhone 17 "})).unwrap();
        assert_eq!(open, Op::Open("iPhone 17".into()));
        assert_eq!(
            panel_args(&open),
            Some(vec!["open".into(), "iPhone 17".into()])
        );
        let close = route(&json!({"action": "close", "device": "Pixel_7", "shutdown": true}));
        assert_eq!(
            panel_args(&close.unwrap()),
            Some(vec!["close".into(), "Pixel_7".into(), "--shutdown".into()])
        );
        let on = |extra: Value| {
            let mut args = json!({"device": "iPhone 17"});
            args.as_object_mut()
                .unwrap()
                .extend(extra.as_object().unwrap().clone());
            route(&args)
        };
        assert_eq!(
            on(json!({"action": "launch", "app": "com.apple.mobilesafari"})),
            Ok(drive(&["open", "com.apple.mobilesafari"]))
        );
        assert_eq!(
            on(json!({"action": "snapshot"})),
            Ok(drive(&["snapshot", "-i"]))
        );
        assert_eq!(
            on(json!({"action": "press", "target": "@e3"})),
            Ok(drive(&["press", "@e3", "--settle"]))
        );
        assert_eq!(
            on(json!({"action": "fill", "target": "@e5", "text": "hi there"})),
            Ok(drive(&["fill", "@e5", "hi there", "--settle"]))
        );
        let shot = on(json!({"action": "screenshot"})).unwrap();
        assert!(matches!(
            shot,
            Op::Drive {
                screenshot: true,
                ..
            }
        ));
        assert_eq!(panel_args(&shot), None);
        assert_eq!(
            on(json!({"action": "install", "app": "https://ci.example/app.zip"})),
            Ok(Op::Install {
                device: "iPhone 17".into(),
                app: Source::Url("https://ci.example/app.zip".into())
            })
        );
        assert_eq!(
            on(json!({"action": "install", "app": "/Users/me/App.app"})),
            Ok(Op::Install {
                device: "iPhone 17".into(),
                app: Source::Path("/Users/me/App.app".into())
            })
        );
    }

    #[test]
    fn bad_calls_say_what_is_missing() {
        assert!(route(&json!({"action": "boot"})).is_err());
        assert!(route(&json!({})).is_err());
        assert!(route(&json!({"action": "open"})).is_err());
        let on = |args: Value| route(&args).unwrap_err();
        assert!(on(json!({"action": "launch", "device": "x"})).contains("app"));
        assert!(on(json!({"action": "press", "device": "x", "target": "--udid"})).contains("'-'"));
        assert!(on(json!({"action": "fill", "device": "x", "target": "@e1"})).contains("text"));
        assert!(
            on(json!({"action": "install", "device": "x", "app": "build/App.app"}))
                .contains("absolute")
        );
    }

    #[test]
    fn downloads_keep_a_safe_name() {
        assert_eq!(download_name("https://h/a/My App.apk?x=1"), "MyApp.apk");
        assert_eq!(download_name("https://h/builds/"), "app");
        assert_eq!(download_name("https://h/../.."), "app");
    }
}
