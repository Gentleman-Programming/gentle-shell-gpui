//! Running one `gentle-screen` op: the launched-apps list, finding windows,
//! scaling captures, the input rule, and the JSON answer.

use std::path::PathBuf;
use std::process::Stdio;
use std::time::{Duration, Instant};

use gentle_core::screen::{
    Op, Window, error, matches, may_drive, ok, parse_args, parse_keys, pick,
};
use serde_json::{Value, json};

use super::native;

/// `gentle-shell-gpui --screen <args>`: the exit code (1 for an error, 2
/// for bad arguments, 124 for a timeout).
pub fn run(args: Vec<String>) -> i32 {
    let op = match parse_args(&args) {
        Ok(op) => op,
        Err(usage) => {
            eprintln!("{usage}");
            return 2;
        }
    };
    let answer = run_op(op);
    println!("{answer}");
    match answer["error"]["kind"].as_str() {
        None => 0,
        Some("timeout") => 124,
        Some(_) => 1,
    }
}

/// Run one op here (the `screen` host tool too): its `{ok}` or `{error}`
/// answer. Blocking.
pub fn run_op(op: Op) -> Value {
    answer(op).unwrap_or_else(|e| e)
}

fn answer(op: Op) -> Result<Value, Value> {
    let mut launched = Launched::load();
    match op {
        Op::Windows { app, pid } => {
            let list: Vec<Value> = windows()?
                .into_iter()
                .filter(|w| matches(w, app.as_deref(), pid))
                .map(|w| describe(&w, &launched))
                .collect();
            Ok(ok(json!({ "windows": list })))
        }
        Op::Launch {
            command,
            cwd,
            app,
            wait,
        } => launch(
            &mut launched,
            command,
            cwd,
            app.as_deref(),
            wait.unwrap_or(20.),
        ),
        Op::Screenshot { window, app, pid } => {
            if window.is_none() && app.is_none() && pid.is_none() {
                let png = native::capture_screen().map_err(|e| error("failed", e))?;
                return save(&png, None);
            }
            let list = windows()?;
            let found = match window {
                Some(id) => list.iter().find(|w| w.id == id),
                None => pick(&list, app.as_deref(), pid),
            }
            .ok_or_else(|| error("not_found", "no such window (see `gentle-screen windows`)"))?;
            let png = native::capture_window(found).map_err(|e| error("failed", e))?;
            save(&png, Some(found))
        }
        Op::Quit { pid } => {
            if !launched.pids.contains(&pid) {
                return Err(error(
                    "not_allowed",
                    "gentle-screen only quits apps it launched",
                ));
            }
            kill(pid);
            launched.pids.retain(|p| *p != pid);
            launched.save();
            Ok(ok(json!({ "quit": pid })))
        }
        input => drive(&launched, input),
    }
}

fn windows() -> Result<Vec<Window>, Value> {
    native::windows().map_err(|e| error("failed", e))
}

fn describe(w: &Window, launched: &Launched) -> Value {
    json!({
        "window": w.id,
        "pid": w.pid,
        "app": w.app,
        "title": w.title,
        "x": w.x, "y": w.y, "width": w.width, "height": w.height,
        "launched": launched.allows(w.pid),
    })
}

/// Click, type, key or scroll in a window of an app `launch` started.
fn drive(launched: &Launched, op: Op) -> Result<Value, Value> {
    let id = op.input_window().unwrap_or_default();
    let list = windows()?;
    let w = list
        .iter()
        .find(|w| w.id == id)
        .ok_or_else(|| error("not_found", "no such window (see `gentle-screen windows`)"))?;
    if !launched.allows(w.pid) {
        return Err(error(
            "not_allowed",
            format!(
                "{} wasn't started by `gentle-screen launch`; input only reaches apps it launched, never one you're using",
                w.app
            ),
        ));
    }
    native::input_ready().map_err(|e| error("permission", e))?;
    native::focus(w).map_err(|e| error("failed", e))?;
    let done = match op {
        Op::Click {
            x,
            y,
            button,
            count,
            ..
        } => {
            inside(w, x, y)?;
            native::click(w, x, y, button, count)
        }
        Op::Type { text, .. } => native::type_text(w, &text),
        Op::Key { keys, .. } => parse_keys(&keys)
            .map_err(|e| e.to_string())
            .and_then(|chords| chords.iter().try_for_each(|c| native::key(w, c))),
        Op::Scroll { x, y, dy, .. } => {
            let (x, y) = (x.unwrap_or(w.width / 2.), y.unwrap_or(w.height / 2.));
            inside(w, x, y)?;
            native::scroll(w, x, y, dy)
        }
        _ => Ok(()),
    };
    done.map_err(|e| error("failed", e))?;
    // Let the app react before the agent looks again.
    std::thread::sleep(Duration::from_millis(200));
    Ok(ok(json!({ "done": true })))
}

fn inside(w: &Window, x: f64, y: f64) -> Result<(), Value> {
    if (0. ..w.width).contains(&x) && (0. ..w.height).contains(&y) {
        Ok(())
    } else {
        Err(error(
            "bad_op",
            format!(
                "{x},{y} is outside the window ({}×{}): use its screenshot's pixels",
                w.width, w.height
            ),
        ))
    }
}

/// Write `png` (a window's scaled to its size) to the temporary folder.
fn save(png: &[u8], window: Option<&Window>) -> Result<Value, Value> {
    let mut image = image::load_from_memory(png).map_err(|e| error("failed", e.to_string()))?;
    if let Some(w) = window {
        let (width, height) = (
            w.width.round().max(1.) as u32,
            w.height.round().max(1.) as u32,
        );
        if (image.width(), image.height()) != (width, height) {
            image = image.resize_exact(width, height, image::imageops::FilterType::Triangle);
        }
    }
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis());
    let path =
        std::env::temp_dir().join(format!("gentle-screen-{}-{stamp}.png", std::process::id()));
    image
        .save_with_format(&path, image::ImageFormat::Png)
        .map_err(|e| error("failed", e.to_string()))?;
    Ok(ok(json!({
        "path": path,
        "width": image.width(),
        "height": image.height(),
        "window": window.map(|w| w.id),
    })))
}

fn launch(
    launched: &mut Launched,
    command: Vec<String>,
    cwd: Option<String>,
    app: Option<&str>,
    wait: f64,
) -> Result<Value, Value> {
    let before: Vec<u64> = windows().unwrap_or_default().iter().map(|w| w.id).collect();
    let mut process = std::process::Command::new(&command[0]);
    process
        .args(&command[1..])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    if let Some(cwd) = cwd {
        process.current_dir(cwd);
    }
    detach(&mut process);
    let child = process
        .spawn()
        .map_err(|e| error("failed", format!("couldn't start {}: {e}", command[0])))?;
    let pid = child.id();
    launched.pids.push(pid);
    launched.save();
    let until = Instant::now() + Duration::from_secs_f64(wait.clamp(1., 120.));
    while Instant::now() < until {
        std::thread::sleep(Duration::from_millis(250));
        let Ok(list) = native::windows() else {
            continue;
        };
        // The app's first new window: from the process started or a child
        // of it, or (when the command hands off, `open -a`) from `app`.
        let found = list.into_iter().find(|w| {
            !before.contains(&w.id)
                && (launched.allows(w.pid) || app.is_some_and(|app| matches(w, Some(app), None)))
        });
        if let Some(w) = found {
            if !launched.allows(w.pid) {
                launched.pids.push(w.pid);
                launched.save();
            }
            return Ok(ok(json!({ "pid": pid, "window": describe(&w, launched) })));
        }
    }
    Err(error(
        "timeout",
        format!(
            "{} started (pid {pid}) but showed no window in {wait} s; `gentle-screen windows` lists them",
            command[0]
        ),
    ))
}

#[cfg(unix)]
fn detach(process: &mut std::process::Command) {
    use std::os::unix::process::CommandExt as _;
    // Its own process group: the agent's shell ending doesn't stop it.
    process.process_group(0);
}

#[cfg(windows)]
fn detach(process: &mut std::process::Command) {
    use std::os::windows::process::CommandExt as _;
    const DETACHED_PROCESS: u32 = 0x0000_0008;
    const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
    process.creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP);
}

fn kill(pid: u32) {
    #[cfg(unix)]
    let _ = std::process::Command::new("kill")
        .arg(pid.to_string())
        .status();
    #[cfg(windows)]
    let _ = pi_rpc::command("taskkill")
        .args(["/PID", &pid.to_string(), "/T"])
        .status();
}

/// The processes `launch` started (`<data dir>/screen/launched.json`),
/// the only ones input may reach.
struct Launched {
    pids: Vec<u32>,
}

impl Launched {
    fn path() -> Option<PathBuf> {
        Some(
            crate::shared::data_dir::app()?
                .join("screen")
                .join("launched.json"),
        )
    }

    /// The list, without processes that ended.
    fn load() -> Self {
        let pids: Vec<u32> = Self::path()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or_default();
        Self {
            pids: pids.into_iter().filter(|p| native::alive(*p)).collect(),
        }
    }

    fn save(&self) {
        let Some(path) = Self::path() else { return };
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let _ = std::fs::write(path, serde_json::to_string(&self.pids).unwrap_or_default());
    }

    fn allows(&self, pid: u32) -> bool {
        may_drive(pid, &self.pids, native::parent)
    }
}
