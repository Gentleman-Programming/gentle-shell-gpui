//! Host tools: the app's own tools, offered to an agent that runs on a
//! Gentle Shell server (`gentle-shell serve`) and run here, on the computer
//! the user watches. Gentle Shell's Desktop RPC carries them
//! (`set_host_tools`, `host_tool_call`, `host_tool_result`); the agent sees
//! them as ordinary tools, with the guidance a local agent reads in the
//! app's instructions moved into each tool's description and guidelines.
//!
//! - `open_page`: what `gentle-open <url>` does.
//! - `browser`: what `gentle-browser` does, on the app's browser cards.
//! - `screen`: what `gentle-screen` does, on this computer.
//! - `canvas_read`, `canvas_edit`, `canvas_shot`: for an agent opened beside
//!   a canvas, on that open canvas.
//! - `gentle_ui`: what the app's generative UI extension does
//!   (`gentle_core::genui::host`), when the server takes host messages and
//!   Agent UI isn't off.
//!
//! This module is the pure part: the definitions sent, how a call's
//! arguments become the operation the app's existing code runs, and how its
//! answer becomes the result sent back. `session::host_tools` runs them.

use std::path::Path;

use base64::Engine as _;
use gentle_core::genui::host::{Mode, guide};
use pi_rpc::desktop_rpc::{HostTool, HostToolContent};
use serde_json::{Value, json};

pub mod device;
pub mod themes;

/// The provider name the app registers under. A server replaces it with
/// this device's id, which the call events then carry.
pub const PROVIDER: &str = "desktop";

/// The `browser` ops that act on an element (`act` in the app's driver).
const BROWSER_ACTIONS: [&str; 10] = [
    "click", "fill", "type", "press", "select", "check", "uncheck", "hover", "focus", "scroll",
];
const BROWSER_OPS: [&str; 24] = [
    "open",
    "tabs",
    "goto",
    "back",
    "forward",
    "reload",
    "state",
    "eval",
    "snapshot",
    "click",
    "fill",
    "type",
    "press",
    "select",
    "check",
    "uncheck",
    "hover",
    "focus",
    "scroll",
    "wait",
    "screenshot",
    "pick",
    "close",
    "release",
];
const SCREEN_OPS: [&str; 8] = [
    "launch",
    "windows",
    "screenshot",
    "click",
    "type",
    "key",
    "scroll",
    "quit",
];

/// What the agent is told the user's side is.
const WHERE: &str =
    "the user's computer, where the Gentle Shell app runs (not the machine you run on)";

fn tool(
    name: &str,
    label: &str,
    description: String,
    parameters: Value,
    guidelines: &[&str],
) -> HostTool {
    HostTool {
        name: name.into(),
        label: label.into(),
        description,
        parameters,
        guidelines: guidelines.iter().map(|line| line.to_string()).collect(),
    }
}

/// The tools the app offers a server session; the canvas tools only to an
/// agent opened beside a canvas, `gentle_ui` with the rules of the Agent UI
/// mode given (none: not offered).
pub fn definitions(canvas: bool, genui: Option<Mode>) -> Vec<HostTool> {
    let mut tools = vec![open_page(), browser(), screen()];
    if device::offered() {
        tools.push(device::definition());
    }
    tools.push(themes::definition());
    if canvas {
        tools.extend(canvas_tools());
    }
    if let Some(mode) = genui.filter(|mode| *mode != Mode::Off) {
        tools.push(gentle_ui(mode));
    }
    tools
}

fn gentle_ui(mode: Mode) -> HostTool {
    HostTool {
        name: gentle_core::genui::TOOL.into(),
        label: guide::LABEL.into(),
        description: guide::DESCRIPTION.into(),
        parameters: guide::parameters(),
        guidelines: guide::guidelines(mode),
    }
}

fn open_page() -> HostTool {
    tool(
        "open_page",
        "Open page",
        format!(
            "Show a web page (a dev server, a preview, docs) to the user on {WHERE}: in the app's \
             browser, or in their own browser when they chose that. The page loads there, so its \
             URL must reach that computer."
        ),
        json!({
            "type": "object",
            "properties": {
                "url": { "type": "string", "description": "An http(s) URL" }
            },
            "required": ["url"],
            "additionalProperties": false
        }),
        &[
            "To show the user a page, call open_page; never run open, xdg-open, start or a browser yourself: you run on a server and nothing you open there reaches the user.",
            "A server on your machine's localhost (a dev server you started) only loads on the user's computer through an address that reaches your machine (its network address, or a forwarded port); say so when the page can't load.",
        ],
    )
}

fn browser() -> HostTool {
    tool(
        "browser",
        "Browser",
        format!(
            "Drive a web page in the Gentle Shell app's browser, on {WHERE}, where the user watches it. \
             open {{url, name?, profile?, hidden?}} opens a page (named so later calls find it; profile agent by default, shared for the user's sign-ins). \
             tabs lists every page. snapshot {{tab, text?, root?}} is an outline with refs (e12) to act on. \
             click / hover / focus / check / uncheck {{tab, target}}; fill {{tab, target, text}}; type {{tab, target?, text}}; select {{tab, target, text: option}}; \
             press {{tab, text: keys like Enter or Meta+a}}; scroll {{tab, target?, dy?}}; wait {{tab, target, gone?, timeout?}}; \
             goto {{tab, url}}; back / forward / reload / state {{tab}}; eval {{tab, js}} (an expression, or statements with return; awaited); \
             screenshot {{tab}} shows you the page; pick {{tab}} waits for the user to click an element; release {{tab}} hands it back; close {{tab}}. \
             Targets: a ref (e12), text/<text>, aria/[role/]<name>, xpath/<path>, or CSS. Take a snapshot after the page changes; refs go stale. \
             A link or form that loads a new page answers with that page (navigated: true); a single-page app may change a moment after the answer, so wait for what you expect before reading it."
        ),
        json!({
            "type": "object",
            "properties": {
                "op": { "type": "string", "enum": BROWSER_OPS, "description": "What to do" },
                "tab": { "type": "string", "description": "The page: the name it was opened with, or its number in tabs" },
                "url": { "type": "string" },
                "name": { "type": "string", "description": "open: the page's name" },
                "profile": { "type": "string", "description": "open: agent, shared, private, or a profile name" },
                "hidden": { "type": "boolean", "description": "open: keep it out of sight (still live)" },
                "target": { "type": "string" },
                "text": { "type": "string", "description": "fill/type text, select option, press keys" },
                "js": { "type": "string" },
                "dy": { "type": "number" },
                "gone": { "type": "boolean" },
                "timeout": { "type": "number", "description": "wait: seconds (10 by default)" },
                "root": { "type": "string", "description": "snapshot: only under this target" },
                "nodes": { "type": "number", "description": "snapshot: most elements to list" }
            },
            "required": ["op"],
            "additionalProperties": false
        }),
        &[
            "To look at or use a page yourself (check a UI, fill a form, read what it shows), use the browser tool; don't start Playwright, Puppeteer or a browser of your own for that. The project's own test suites run as usual.",
            "Pages you open use the Agent profile, apart from the user's sign-ins; profile shared uses theirs. tabs lists the user's pages too; release one you're done driving.",
            "After a click in a single-page app, wait for what you expect before reading the page.",
        ],
    )
}

fn screen() -> HostTool {
    tool(
        "screen",
        "Screen",
        format!(
            "Launch a desktop app on {WHERE}, look at its windows and use it, to check what you built. \
             launch {{command: [program, ...args], app?, cwd?, wait?}} starts it there and answers its first window (app names the app when the command hands off, e.g. ['open', '-a', 'Calculator'] with app 'Calculator'). \
             windows {{app?, pid?}} lists windows (frontmost first). screenshot {{window | app | pid}} shows you a window (or the whole screen with none). \
             click {{window, x, y, button?, count?}}, type {{window, text}}, key {{window, keys: 'enter', 'cmd+s', 'ctrl+shift+tab'}}, scroll {{window, dy, x?, y?}}: x and y are pixels of that window's screenshot. \
             quit {{pid}} stops an app you launched. Input only reaches apps launched with this tool, never the user's own. Take a screenshot after each step to check it."
        ),
        json!({
            "type": "object",
            "properties": {
                "op": { "type": "string", "enum": SCREEN_OPS, "description": "What to do" },
                "command": { "type": "array", "items": { "type": "string" }, "description": "launch: the program and its arguments, on the user's computer" },
                "app": { "type": "string", "description": "An app's name (part of it)" },
                "cwd": { "type": "string", "description": "launch: a folder on the user's computer" },
                "wait": { "type": "number", "description": "launch: seconds to wait for a window (20)" },
                "pid": { "type": "number" },
                "window": { "type": "number", "description": "A window id from windows or launch" },
                "x": { "type": "number" },
                "y": { "type": "number" },
                "button": { "type": "string", "enum": ["left", "right", "middle"] },
                "count": { "type": "number", "description": "click: 2 for a double click" },
                "text": { "type": "string" },
                "keys": { "type": "string" },
                "dy": { "type": "number", "description": "scroll: pixels, positive is down" }
            },
            "required": ["op"],
            "additionalProperties": false
        }),
        &[
            "screen runs on the user's computer, not where you run: launch starts a program installed there, and your files and builds are not there unless the user's computer shares them.",
            "Take a screenshot after each step to check it; quit what you launched when you're done.",
        ],
    )
}

const CANVAS_SHAPES: &str = "Shapes (page units, y grows down): \
rect / ellipse {x, y, w, h, text?: label centered inside}; \
line / arrow {x1, y1, x2, y2}; text {x, y (top-left), text}; note {x, y, w?=200, h?=200, text} (sticky note, yellow by default); \
ink {points: [[x, y], …], closed?} (freehand strokes, handwriting). \
Style on any shape: color and fill_color (ink, red, orange, yellow, green, emerald, cyan, sky, blue, violet, pink, indigo, slate, light_slate, pale_slate, paper; they follow the light or dark theme), \
fill (none, semi, solid, pattern) and fill_color (rect, ellipse and closed ink only), size 0-5 (stroke width and font size; default 1), opacity (25, 50, 75, 100), pen (pen, monoline, fountain; ink only, not highlighter).";

const CANVAS_OPS: &str = "ops, applied in order, all or nothing: \
{op: \"add\", shapes: [shape, …]} (or one shape's fields inline with type); \
{op: \"update\", id | ids, …fields to change} (geometry, text, style; ink x, y move the stroke); \
{op: \"move\", id | ids, dx, dy}; {op: \"delete\", id | ids}; {op: \"clear\"}; \
{op: \"front\" | \"back\", id | ids}; {op: \"show\", ids | x, y, w, h} (moves the user's view there).";

fn canvas_tools() -> Vec<HostTool> {
    vec![
        tool(
            "canvas_read",
            "Read canvas",
            "Read the canvas (whiteboard) the user has open beside you in the Gentle Shell app: every shape back to front with its id, type, geometry, text and style, \
             and `view`, the page area the user sees. Ink (handwriting, sketches) comes as a few points of each stroke: use canvas_shot to see what it says or draws."
                .into(),
            json!({ "type": "object", "properties": {}, "additionalProperties": false }),
            &[
                "You were opened from a canvas (a whiteboard) in the Gentle Shell app and float over it; the user sees your answers there. The canvas is your output.",
                "Whatever the user asks you to create, draw, diagram, sketch, map, plan, list, compare, explain or solve goes ON THE CANVAS with canvas_edit, even when they don't say \"on the canvas\". Don't write files (Mermaid, Markdown, SVG, docs) for it unless they explicitly ask for a file.",
                "Start a turn about the canvas with canvas_read (and canvas_shot when there is ink: handwriting, sketches, math); answer questions about what they drew by reading it.",
                "Lay out cleanly: boxes (rect with text) about 0.55 × 18 px per character wide plus 32 px, 40 px+ apart on a grid, arrows from edge to edge, a title as text above; group related boxes by color. Keep new work inside view and clear of the user's shapes, then check it with canvas_shot and fix overlaps.",
                "Change or delete the user's own shapes only when asked; every edit is one step the user can undo. Reply in chat briefly: the drawing is the answer.",
            ],
        ),
        tool(
            "canvas_edit",
            "Edit canvas",
            format!(
                "Add, change, move, restyle, reorder or delete shapes on the canvas the user has open beside you, as one undoable step. {CANVAS_OPS} {CANVAS_SHAPES} Answers with the ids added, changed and deleted."
            ),
            json!({
                "type": "object",
                "properties": {
                    "ops": {
                        "type": "array",
                        "items": { "type": "object", "additionalProperties": true },
                        "description": CANVAS_OPS
                    }
                },
                "required": ["ops"],
                "additionalProperties": false
            }),
            &[],
        ),
        tool(
            "canvas_shot",
            "Look at canvas",
            "See part of the canvas as a picture (shapes on the page color, no grid or selection): what the user sees (default), all of it, or a page area {x, y, w, h}. \
             Use it to read handwriting and sketches and to check a layout after editing."
                .into(),
            json!({
                "type": "object",
                "properties": {
                    "area": {
                        "anyOf": [
                            { "type": "string", "enum": ["view", "all"] },
                            {
                                "type": "object",
                                "properties": {
                                    "x": { "type": "number" }, "y": { "type": "number" },
                                    "w": { "type": "number" }, "h": { "type": "number" }
                                },
                                "required": ["x", "y", "w", "h"]
                            }
                        ]
                    }
                },
                "additionalProperties": false
            }),
            &[],
        ),
    ]
}

/// A call, as the operation the app's existing code runs.
#[derive(Debug, Clone, PartialEq)]
pub enum Call {
    /// `gentle-open`'s page.
    OpenPage(String),
    /// A `gentle-browser` operation, as the workspace's driver reads it.
    Browser { op: Value, screenshot: bool },
    /// A `gentle-screen` operation.
    Screen(gentle_core::screen::Op),
    /// A request for the open canvas (`gentle_core::canvas::agent`).
    Canvas(Value),
    /// A `gentle-device` operation (iOS/Android simulators).
    Device(device::Op),
    /// An app theme operation.
    Theme(themes::Op),
    /// A `gentle_ui` call, as the generative UI extension takes it.
    GenUi(Value),
}

/// The operation `tool` called with `args` asks for, or what is wrong with
/// the call, in words for the agent. `canvas`: this agent is beside one;
/// `genui`: `gentle_ui` is offered.
pub fn route(tool: &str, args: &Value, canvas: bool, genui: bool) -> Result<Call, String> {
    let text = |key: &str| args.get(key).and_then(Value::as_str);
    match tool {
        "open_page" => {
            let url = text("url").unwrap_or_default().trim();
            if !(url.starts_with("http://") || url.starts_with("https://")) {
                return Err("open_page needs an http(s) url".into());
            }
            Ok(Call::OpenPage(url.to_string()))
        }
        "browser" => {
            let name = text("op").unwrap_or_default();
            if !BROWSER_OPS.contains(&name) {
                return Err(format!("browser: unknown op {name:?}"));
            }
            let op = if BROWSER_ACTIONS.contains(&name) {
                json!({
                    "op": "act", "action": name, "tab": args.get("tab"),
                    "target": args.get("target"), "text": args.get("text"), "dy": args.get("dy"),
                })
            } else if name == "open" {
                json!({
                    "op": "open", "url": args.get("url"), "name": args.get("name"),
                    "profile": args.get("profile"),
                    "show": !args.get("hidden").and_then(Value::as_bool).unwrap_or(false),
                })
            } else {
                args.clone()
            };
            serde_json::from_value::<gentle_core::browser::Op>(op.clone())
                .map_err(|error| format!("browser {name}: {error}"))?;
            Ok(Call::Browser {
                op,
                screenshot: name == "screenshot",
            })
        }
        "screen" => {
            let op = serde_json::from_value::<gentle_core::screen::Op>(args.clone())
                .map_err(|error| format!("screen: {error}"))?;
            if let gentle_core::screen::Op::Launch { command, .. } = &op
                && command.is_empty()
            {
                return Err("screen launch needs a command".into());
            }
            Ok(Call::Screen(op))
        }
        "canvas_read" | "canvas_edit" | "canvas_shot" if canvas => Ok(Call::Canvas(match tool {
            "canvas_read" => json!({ "action": "read" }),
            "canvas_edit" => {
                // Some models send the ops as a JSON string.
                let ops = match args.get("ops") {
                    Some(Value::String(text)) => serde_json::from_str(text)
                        .map_err(|error| format!("canvas_edit ops: {error}"))?,
                    Some(ops) => ops.clone(),
                    None => return Err("canvas_edit needs ops".into()),
                };
                json!({ "action": "edit", "ops": ops })
            }
            _ => {
                json!({ "action": "shot", "area": args.get("area").cloned().unwrap_or("view".into()) })
            }
        })),
        "device" if device::offered() => device::route(args).map(Call::Device),
        "app_theme" => themes::route(args).map(Call::Theme),
        "gentle_ui" if genui => Ok(Call::GenUi(args.clone())),
        other => Err(format!("The Gentle Shell app has no tool {other} here")),
    }
}

/// What a call answers: the content sent back, and whether it failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Outcome {
    pub content: Vec<HostToolContent>,
    pub is_error: bool,
}

impl Outcome {
    pub fn text(text: impl Into<String>) -> Self {
        Self {
            content: vec![HostToolContent::text(text)],
            is_error: false,
        }
    }

    /// A failure the agent reads.
    pub fn error(message: impl Into<String>) -> Self {
        Self {
            content: vec![HostToolContent::text(message)],
            is_error: true,
        }
    }
}

/// A PNG file as image content, removed once read (the drivers write
/// screenshots to the temporary folder for the CLI to pick up).
fn png_file(path: &Path) -> Result<HostToolContent, String> {
    let bytes = std::fs::read(path).map_err(|error| format!("no picture: {error}"))?;
    let _ = std::fs::remove_file(path);
    Ok(png(&bytes))
}

/// PNG bytes as image content.
pub fn png(bytes: &[u8]) -> HostToolContent {
    HostToolContent::Image {
        data: base64::engine::general_purpose::STANDARD.encode(bytes),
        mime_type: "image/png".into(),
    }
}

/// A driver's `{ok}` / `{error}` answer (`gentle-browser`, `gentle-screen`)
/// as a result: a screenshot's file as the picture with `caption`, anything
/// else as its JSON. Blocking (it reads the picture).
pub fn driver_outcome(answer: &Value, caption: impl FnOnce(&Value) -> String) -> Outcome {
    if let Some(error) = answer.get("error") {
        return Outcome::error(error.to_string());
    }
    let ok = answer.get("ok").cloned().unwrap_or(Value::Null);
    if let Some(path) = ok.get("path").and_then(Value::as_str) {
        return match png_file(Path::new(path)) {
            Ok(image) => Outcome {
                content: vec![HostToolContent::text(caption(&ok)), image],
                is_error: false,
            },
            Err(error) => Outcome::error(error),
        };
    }
    match ok {
        Value::String(text) => Outcome::text(text),
        ok => Outcome::text(serde_json::to_string_pretty(&ok).unwrap_or_default()),
    }
}

/// The caption of a browser screenshot of `tab`.
pub fn browser_caption(tab: &str) -> impl FnOnce(&Value) -> String + '_ {
    move |ok| format!("Screenshot of {tab} ({}×{})", ok["width"], ok["height"])
}

/// The caption of a screen screenshot.
pub fn screen_caption(ok: &Value) -> String {
    let what = match ok.get("window") {
        Some(Value::Number(id)) => format!("window {id}"),
        _ => "the screen".into(),
    };
    format!(
        "Screenshot of {what} ({}×{}; click in these pixels)",
        ok["width"], ok["height"]
    )
}

/// The canvas' answer to a request: its JSON, or for a shot the picture
/// (`png`) with the area it shows.
pub fn canvas_outcome(answer: Value, png: Option<Result<Vec<u8>, String>>) -> Outcome {
    if answer.get("ok") == Some(&Value::Bool(false)) {
        let error = answer
            .get("error")
            .and_then(Value::as_str)
            .unwrap_or("the canvas refused the request");
        return Outcome::error(error.to_string());
    }
    match png {
        Some(Ok(bytes)) => Outcome {
            content: vec![
                HostToolContent::text(format!("The canvas, page area {}.", answer["area"])),
                self::png(&bytes),
            ],
            is_error: false,
        },
        Some(Err(error)) => Outcome::error(format!("no picture: {error}")),
        None => Outcome::text(answer.to_string()),
    }
}

/// The failure a tool the app could not reach answers with.
pub fn unavailable(tool: &str, why: &str) -> Outcome {
    Outcome::error(format!(
        "{tool} couldn't run in the Gentle Shell app: {why}"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use pi_rpc::desktop_rpc::DesktopRequest;

    #[test]
    fn the_registration_fits_the_contract() {
        let modes = [
            None,
            Some(Mode::Proactive),
            Some(Mode::WhenAsked),
            Some(Mode::Off),
        ];
        for (canvas, genui) in [false, true]
            .into_iter()
            .flat_map(|c| modes.map(|m| (c, m)))
        {
            let tools = definitions(canvas, genui);
            let names: Vec<&str> = tools.iter().map(|tool| tool.name.as_str()).collect();
            let mut expected = vec!["open_page", "browser", "screen"];
            if device::offered() {
                expected.push("device");
            }
            expected.push("app_theme");
            if canvas {
                expected.extend(["canvas_read", "canvas_edit", "canvas_shot"]);
            }
            // Agent UI off: not offered.
            if genui.is_some_and(|mode| mode != Mode::Off) {
                expected.push("gentle_ui");
            }
            assert_eq!(names, expected);
            for tool in &tools {
                tool.validate().unwrap_or_else(|error| panic!("{error}"));
            }
            // The whole request the server receives stays within its cap.
            DesktopRequest::set_host_tools("host-1", PROVIDER, &tools).unwrap();
        }
        // Each mode's policy leads the rules.
        let rules = |mode| definitions(false, Some(mode)).pop().unwrap().guidelines;
        assert!(rules(Mode::Proactive)[0].starts_with("Use gentle_ui on your own initiative"));
        assert!(
            rules(Mode::WhenAsked)[0]
                .starts_with("The user wants gentle_ui surfaces only on request")
        );
    }

    #[test]
    fn no_tool_tells_the_agent_to_run_the_local_commands() {
        for tool in definitions(true, Some(Mode::Proactive)) {
            let text = format!("{} {}", tool.description, tool.guidelines.join(" "));
            for command in [
                "gentle-open",
                "gentle-browser",
                "gentle-screen",
                "gentle-device",
            ] {
                assert!(!text.contains(command), "{} mentions {command}", tool.name);
            }
        }
    }

    #[test]
    fn calls_become_the_drivers_operations() {
        assert_eq!(
            route(
                "open_page",
                &json!({"url": " https://x.dev "}),
                false,
                false
            ),
            Ok(Call::OpenPage("https://x.dev".into()))
        );
        assert!(
            route(
                "open_page",
                &json!({"url": "file:///etc/passwd"}),
                false,
                false
            )
            .is_err()
        );

        let Ok(Call::Browser { op, screenshot }) = route(
            "browser",
            &json!({"op": "fill", "tab": "app", "target": "e5", "text": "hi"}),
            false,
            false,
        ) else {
            panic!("expected a browser op")
        };
        assert_eq!(op["op"], "act");
        assert_eq!(op["action"], "fill");
        assert_eq!(op["text"], "hi");
        assert!(!screenshot);
        let Ok(Call::Browser { op, .. }) = route(
            "browser",
            &json!({"op": "open", "url": "https://x.dev", "hidden": true}),
            false,
            false,
        ) else {
            panic!("expected open")
        };
        assert_eq!(op["show"], false);
        assert!(matches!(
            route(
                "browser",
                &json!({"op": "screenshot", "tab": "app"}),
                false,
                false
            ),
            Ok(Call::Browser {
                screenshot: true,
                ..
            })
        ));
        assert!(
            route(
                "browser",
                &json!({"op": "bogus", "tab": "app"}),
                false,
                false
            )
            .is_err()
        );
        assert!(
            route(
                "browser",
                &json!({"op": "goto", "tab": "app"}),
                false,
                false
            )
            .is_err()
        );

        assert!(matches!(
            route(
                "screen",
                &json!({"op": "key", "window": 4, "keys": "cmd+s"}),
                false,
                false
            ),
            Ok(Call::Screen(gentle_core::screen::Op::Key { window: 4, .. }))
        ));
        assert!(
            route(
                "screen",
                &json!({"op": "launch", "command": []}),
                false,
                false
            )
            .is_err()
        );

        // Canvas tools only beside a canvas; string ops are read as JSON.
        assert!(route("canvas_read", &json!({}), false, false).is_err());
        assert_eq!(
            route(
                "canvas_edit",
                &json!({"ops": "[{\"op\":\"clear\"}]"}),
                true,
                false
            ),
            Ok(Call::Canvas(
                json!({"action": "edit", "ops": [{"op": "clear"}]})
            ))
        );
        assert_eq!(
            route("canvas_shot", &json!({}), true, false),
            Ok(Call::Canvas(json!({"action": "shot", "area": "view"})))
        );
        assert!(route("bash", &json!({}), true, false).is_err());

        // gentle_ui only when offered; its arguments go as they came.
        let args = json!({"action": "show", "spec": "{}"});
        assert!(route("gentle_ui", &args, false, false).is_err());
        assert_eq!(
            route("gentle_ui", &args, false, true),
            Ok(Call::GenUi(args))
        );
    }

    #[test]
    fn answers_become_results() {
        let failed = driver_outcome(
            &json!({"error": {"kind": "not_found", "message": "no page"}}),
            |_| String::new(),
        );
        assert!(failed.is_error);
        let ok = driver_outcome(&json!({"ok": {"tabs": []}}), |_| String::new());
        assert!(!ok.is_error);
        assert!(matches!(&ok.content[0], HostToolContent::Text { text } if text.contains("tabs")));

        let path = std::env::temp_dir().join(format!("host-tool-test-{}.png", std::process::id()));
        std::fs::write(&path, [137, 80, 78, 71]).unwrap();
        let shot = driver_outcome(
            &json!({"ok": {"path": path, "width": 10, "height": 20}}),
            browser_caption("app"),
        );
        assert_eq!(
            shot.content[0],
            HostToolContent::text("Screenshot of app (10×20)")
        );
        assert!(
            matches!(&shot.content[1], HostToolContent::Image { data, mime_type } if data == "iVBORw==" && mime_type == "image/png")
        );
        assert!(!path.exists(), "the picture is removed once read");

        let refused = canvas_outcome(json!({"ok": false, "error": "bad op"}), None);
        assert_eq!(refused, Outcome::error("bad op"));
        let picture = canvas_outcome(json!({"ok": true, "area": [0, 0, 1, 1]}), Some(Ok(vec![1])));
        assert_eq!(picture.content.len(), 2);
    }

    /// The registration against a running `gentle-shell serve` (no model
    /// turn): `GENTLE_SHELL_LIVE_SERVE_URL`, `GENTLE_SHELL_LIVE_SERVE_TOKEN`
    /// (a paired device's) and `GENTLE_SHELL_LIVE_SERVE_CWD` (a folder on
    /// the server). It starts a session there, offers every tool, checks the
    /// server took them all and that a result for a call nobody made is
    /// refused, then stops the session.
    #[test]
    #[ignore = "needs a running gentle-shell serve"]
    fn live_registration_on_a_server() {
        use pi_rpc::desktop_rpc::DesktopMethod;
        let var = |name: &str| std::env::var(name).unwrap_or_else(|_| panic!("set {name}"));
        let client = pi_rpc::serve::ServeClient::new(
            &var("GENTLE_SHELL_LIVE_SERVE_URL"),
            Some(var("GENTLE_SHELL_LIVE_SERVE_TOKEN")),
        );
        let (process, _rx) =
            pi_rpc::PiProcess::serve(client, &var("GENTLE_SHELL_LIVE_SERVE_CWD"), None, None)
                .unwrap();
        // The session's extension answers once it loaded.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
        let capabilities = loop {
            let asked = DesktopRequest::capabilities("live-1").unwrap();
            match process
                .desktop(&asked)
                .and_then(|answer| Ok(answer.capabilities()?))
            {
                Ok(capabilities) => break capabilities,
                Err(error) => {
                    assert!(std::time::Instant::now() < deadline, "{error:#}");
                    std::thread::sleep(std::time::Duration::from_millis(500));
                }
            }
        };
        assert!(capabilities.supports(DesktopMethod::SetHostTools));
        assert!(capabilities.supports(DesktopMethod::HostToolResult));
        let tools = definitions(true, Some(Mode::Proactive));
        let request = DesktopRequest::set_host_tools("live-2", PROVIDER, &tools).unwrap();
        let result = process.desktop(&request).unwrap().set_host_tools().unwrap();
        assert!(result.rejected.is_empty(), "{:?}", result.rejected);
        let mut registered = result.registered.clone();
        registered.sort();
        let mut names: Vec<String> = tools.iter().map(|tool| tool.name.clone()).collect();
        names.sort();
        assert_eq!(registered, names);
        let details = json!({"id": "ui-1", "version": 1})
            .as_object()
            .cloned()
            .unwrap();
        let stray = DesktopRequest::host_tool_result(
            "live-3",
            "htc_000000000000000000000000",
            &[HostToolContent::text("nobody asked")],
            false,
            Some(&details),
        )
        .unwrap();
        match process.desktop(&stray) {
            Ok(answer) => assert_eq!(
                answer.error().map(|e| e.code.as_str()),
                Some("unknown_call")
            ),
            Err(error) => panic!("{error:#}"),
        }
        // gentle_ui's messages and context (no turn: the agent is idle and
        // the message doesn't start one).
        assert!(capabilities.supports(DesktopMethod::HostMessage));
        assert!(capabilities.supports(DesktopMethod::SetHostContext));
        let message = pi_rpc::desktop_rpc::HostMessage {
            custom_type: gentle_core::genui::host::HOST_EVENT_MESSAGE.into(),
            content: "The user inserted a template (live check).".into(),
            display: true,
            details: Some(details.clone()),
            deliver: pi_rpc::desktop_rpc::HostDelivery::Auto,
            trigger_turn: Some(false),
        };
        let sent = DesktopRequest::host_message("live-5", &message).unwrap();
        assert!(
            process
                .desktop(&sent)
                .unwrap()
                .host_message()
                .unwrap()
                .delivered
        );
        for (id, content) in [("live-6", "Shared context (live check)."), ("live-7", "")] {
            let context = DesktopRequest::set_host_context(id, PROVIDER, content, None).unwrap();
            assert!(
                process
                    .desktop(&context)
                    .unwrap()
                    .set_host_context()
                    .unwrap()
                    .set
            );
        }
        // Unregister, then stop the session there.
        let empty = DesktopRequest::set_host_tools("live-4", PROVIDER, &[]).unwrap();
        assert!(process.desktop(&empty).unwrap().set_host_tools().is_ok());
        process.shutdown(std::time::Duration::from_secs(2));
    }
}
