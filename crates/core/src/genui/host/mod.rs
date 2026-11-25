//! `gentle_ui` as a host tool: what the app's Pi extension (`gentle-ui.ts`)
//! does, run by the app itself for an agent on a Gentle Shell server, which
//! doesn't load the app's extensions. The agent calls the host tool; the
//! user's edits reach it as `host_message`s ([`HOST_EVENT_MESSAGE`]); the
//! context an html page shares, and the Off rule, as the app's host
//! context. The surfaces, their versions and every answer are the
//! extension's, so the app draws a host tool's results unchanged and the
//! conversation rebuilds them on resume.
//!
//! - [`normalize`]: each kind's spec checked, with its defaults.
//! - [`patch`]: `update` patches.
//! - [`summary`]: what the model reads of a surface.
//! - [`guide`]: the tool's description, parameters and rules per mode.
//! - [`Engine`]: the open surfaces and what a call or an event does.

pub mod guide;
pub mod normalize;
pub mod patch;
pub mod summary;

use std::collections::HashSet;
use std::sync::LazyLock;
use std::time::Duration;

use regex::Regex;
use serde_json::{Map, Value, json};

use super::EVENT_MESSAGE;
pub use super::mode::Mode;
use guide::{OFF_ERROR, OFF_NOTE, OFF_RULE};

/// The custom message type the app's events are sent as (`host_message`
/// requires the `host.` prefix); the app draws it as [`EVENT_MESSAGE`].
pub const HOST_EVENT_MESSAGE: &str = "host.gentle-ui.event";
/// What the agent is told events arrive as.
const EVENT: &str = HOST_EVENT_MESSAGE;
/// Hard errors listed at most.
const MAX_ERRORS: usize = 10;
/// Items a summary line lists at most.
const MAX_LISTED: usize = 60;
/// How long a `wait: true` review call waits for the user's decisions
/// before answering that it stopped (the review stays open). The extension
/// waits 30 minutes; a Gentle Shell server fails a host tool call nobody
/// answered after 10, so the app answers first.
pub const REVIEW_WAIT: Duration = Duration::from_secs(9 * 60);

/// The custom message type a call's surface or an event is drawn from.
pub fn is_event_message(custom_type: &str) -> bool {
    custom_type == EVENT_MESSAGE || custom_type == HOST_EVENT_MESSAGE
}

static SLUG: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[a-z0-9][a-z0-9\-_]{0,63}$").expect("a valid pattern"));
static UI_ID: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^ui-(\d+)$").expect("a valid pattern"));

/// What each kind is called: its noun in sentences and default title.
pub(crate) mod kind {
    pub struct Def {
        pub name: &'static str,
        pub noun: &'static str,
        pub title: &'static str,
    }

    pub const ALL: [Def; 12] = [
        Def {
            name: "diagram",
            noun: "diagram",
            title: "Diagram",
        },
        Def {
            name: "table",
            noun: "table",
            title: "Table",
        },
        Def {
            name: "form",
            noun: "form",
            title: "Form",
        },
        Def {
            name: "checklist",
            noun: "checklist",
            title: "Checklist",
        },
        Def {
            name: "kanban",
            noun: "board",
            title: "Board",
        },
        Def {
            name: "chart",
            noun: "chart",
            title: "Chart",
        },
        Def {
            name: "html",
            noun: "HTML UI",
            title: "HTML",
        },
        Def {
            name: "review",
            noun: "review",
            title: "Review",
        },
        Def {
            name: "sequence",
            noun: "sequence diagram",
            title: "Sequence",
        },
        Def {
            name: "erd",
            noun: "data model",
            title: "Data model",
        },
        Def {
            name: "state",
            noun: "state machine",
            title: "State machine",
        },
        Def {
            name: "timeline",
            noun: "timeline",
            title: "Timeline",
        },
    ];

    pub fn find(name: &str) -> Option<&'static Def> {
        ALL.iter().find(|def| def.name == name)
    }

    /// A surface's kind, the diagram's for one unknown.
    pub fn of(name: &str) -> &'static Def {
        find(name).unwrap_or(&ALL[0])
    }
}

// ── Text helpers, as the extension writes its sentences ────────────────────

fn cut(text: &str, max: usize) -> String {
    super::diagram::truncate(text, max)
}

fn id_text(value: Option<&Value>) -> Option<String> {
    value.and_then(super::diagram::id_of)
}

fn plural(count: usize, word: &str) -> String {
    format!("{count} {word}{}", if count == 1 { "" } else { "s" })
}

fn listed(label: &str, items: impl IntoIterator<Item = String>) -> String {
    let items: Vec<String> = items.into_iter().collect();
    let shown = items
        .iter()
        .take(MAX_LISTED)
        .cloned()
        .collect::<Vec<_>>()
        .join(", ");
    let more = if items.len() > MAX_LISTED {
        format!(", … and {} more", items.len() - MAX_LISTED)
    } else {
        String::new()
    };
    format!("{label}: {shown}{more}")
}

/// A number as JavaScript writes it (`120`, not `120.0`).
fn js_num(n: f64) -> String {
    if n.fract() == 0. && n.abs() < 1e15 {
        format!("{}", n as i64)
    } else {
        format!("{n}")
    }
}

/// `String(value)` for the values specs hold.
fn js_string(value: &Value) -> String {
    match value {
        Value::Null => "null".into(),
        Value::Bool(b) => b.to_string(),
        Value::Number(n) => match n.as_i64() {
            Some(i) => i.to_string(),
            None => js_num(n.as_f64().unwrap_or_default()),
        },
        Value::String(text) => text.clone(),
        Value::Array(items) => items.iter().map(js_string).collect::<Vec<_>>().join(","),
        Value::Object(_) => "[object Object]".into(),
    }
}

/// `"text"` cut to `max` (an absent value is empty).
fn quoted(value: &Value, max: usize) -> String {
    let text = if value.is_null() {
        String::new()
    } else {
        js_string(value)
    };
    format!("\"{}\"", cut(&text, max))
}

// ── Surfaces ───────────────────────────────────────────────────────────────

/// One version of a surface the agent showed.
#[derive(Debug, Clone, PartialEq)]
pub struct Surface {
    pub id: String,
    pub version: u64,
    pub kind: String,
    pub title: String,
    pub spec: Value,
}

impl Surface {
    /// The tool result's `details` (what the app draws).
    pub fn details(&self) -> Map<String, Value> {
        let mut map = Map::new();
        map.insert("id".into(), json!(self.id));
        map.insert("version".into(), json!(self.version));
        map.insert("kind".into(), json!(self.kind));
        map.insert("title".into(), json!(self.title));
        map.insert("spec".into(), self.spec.clone());
        map
    }

    fn closed(&self) -> Value {
        json!({
            "id": self.id, "version": self.version, "kind": self.kind,
            "title": self.title, "spec": null, "closed": true,
        })
    }
}

/// What a call answers: the text the model reads and the details the app
/// draws.
#[derive(Debug, Clone, PartialEq)]
pub struct Answer {
    pub text: String,
    pub details: Value,
}

/// A call's outcome.
#[derive(Debug, Clone, PartialEq)]
pub enum Reply {
    /// Answer it now.
    Done(Answer),
    /// A `wait: true` review: the call waits for the user's decisions
    /// ([`Handled::decided`]) or [`Engine::stop_waiting`]. `replaced`: an
    /// earlier call that waited on the same review, answered now.
    Waiting {
        surface: Surface,
        replaced: Option<(String, Answer)>,
    },
}

/// A custom message to send with `host_message` (`deliver: auto`).
#[derive(Debug, Clone, PartialEq)]
pub struct Message {
    pub content: String,
    pub details: Value,
    /// Start a turn when the agent is idle.
    pub trigger_turn: bool,
}

/// What one app event did.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Handled {
    /// The answer the surface waits for: an outcome line
    /// ([`super::outcome`] reads it).
    pub outcome: Option<String>,
    /// The message the agent gets.
    pub message: Option<Message>,
    /// Agent UI changed: the tool's offer and the context follow it.
    pub mode: Option<Mode>,
    /// The host context changed ([`Engine::context`]).
    pub context_changed: bool,
    /// A waiting review call to answer with the user's decisions.
    pub decided: Option<(String, Answer)>,
    /// An event the app sent that couldn't be read.
    pub error: Option<String>,
}

/// The surfaces of one conversation, rebuilt from it, and what the tool's
/// calls and the app's events do to them.
#[derive(Debug, Default)]
pub struct Engine {
    /// Open surfaces, in the order first shown.
    surfaces: Vec<Surface>,
    /// Every id seen, closed ones included, so `ui-<n>` never repeats.
    seen: HashSet<String>,
    /// The latest context each html page shared, sent with the next turn.
    pending: Vec<(Surface, Value)>,
    /// Reviews a `wait: true` call waits on: surface id, call id.
    waiting: Vec<(String, String)>,
    mode: Mode,
    /// The session's folder: absolute refs inside it are made relative.
    root: String,
}

impl Engine {
    pub fn new(mode: Mode) -> Self {
        Self {
            mode,
            ..Self::default()
        }
    }

    pub fn mode(&self) -> Mode {
        self.mode
    }

    pub fn set_root(&mut self, root: &str) {
        self.root = root.to_string();
    }

    pub fn surface(&self, id: &str) -> Option<&Surface> {
        self.surfaces.iter().find(|s| s.id == id)
    }

    fn set(&mut self, surface: Surface) {
        self.seen.insert(surface.id.clone());
        match self.surfaces.iter_mut().find(|s| s.id == surface.id) {
            Some(found) => *found = surface,
            None => self.surfaces.push(surface),
        }
    }

    fn remove(&mut self, id: &str) {
        self.surfaces.retain(|s| s.id != id);
        self.pending.retain(|(s, _)| s.id != id);
    }

    /// The conversation changed (a new, resumed or switched session): start
    /// over. Waiting calls are answered with why they stopped.
    pub fn reset(&mut self) -> Vec<(String, Answer)> {
        let stopped = self
            .waiting
            .clone()
            .into_iter()
            .filter_map(|(_, call)| {
                self.stop_waiting(
                    &call,
                    "The session changed while waiting for the user's decisions.",
                )
            })
            .collect();
        self.surfaces.clear();
        self.pending.clear();
        self.seen.clear();
        stopped
    }

    /// Fold in one version from the conversation: a `gentle_ui` tool
    /// result's or an event message's details.
    pub fn remember(&mut self, details: &Value) {
        let Some(id) = details["id"].as_str().filter(|id| !id.is_empty()) else {
            return;
        };
        self.seen.insert(id.to_string());
        if details["closed"] == true || !details["spec"].is_object() {
            self.remove(id);
            return;
        }
        let text = |key: &str| details[key].as_str().unwrap_or_default().to_string();
        self.set(Surface {
            id: id.to_string(),
            version: details["version"].as_u64().unwrap_or(0),
            kind: details["kind"].as_str().unwrap_or("diagram").to_string(),
            title: text("title"),
            spec: details["spec"].clone(),
        });
    }

    /// [`Self::remember`] for a version arriving live: one older than what
    /// this app already has (its own answer, echoed later) is ignored.
    pub fn observe(&mut self, details: &Value) {
        let id = details["id"].as_str().unwrap_or_default();
        let version = details["version"].as_u64().unwrap_or(0);
        if self
            .surface(id)
            .is_some_and(|known| known.version > version)
        {
            return;
        }
        self.remember(details);
    }

    fn existing(&self, id: Option<&str>) -> Result<Surface, String> {
        if let Some(surface) = id.and_then(|id| self.surface(id)) {
            return Ok(surface.clone());
        }
        Err(format!(
            "gentle_ui: no open UI with id \"{}\" (open: {})",
            id.unwrap_or_default(),
            self.open_ids()
        ))
    }

    fn open_ids(&self) -> String {
        if self.surfaces.is_empty() {
            return "none".into();
        }
        self.surfaces
            .iter()
            .map(|s| s.id.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    }

    fn fresh_id(&self, wanted: Option<&str>) -> String {
        if let Some(wanted) = wanted.filter(|w| SLUG.is_match(w) && self.surface(w).is_none()) {
            return wanted.to_string();
        }
        let highest = self
            .seen
            .iter()
            .filter_map(|id| UI_ID.captures(id)?[1].parse::<u64>().ok())
            .max()
            .unwrap_or(0);
        format!("ui-{}", highest + 1)
    }

    /// One `gentle_ui` call; `call` is its id (a review that waits is
    /// answered later under it).
    pub fn call(&mut self, call: &str, args: &Value) -> Result<Reply, String> {
        // A call already on its way when the user turned Agent UI off.
        if self.mode == Mode::Off {
            return Err(OFF_ERROR.into());
        }
        // Some models send the spec or patch as a JSON string.
        let parsed = |key: &str| match args.get(key) {
            Some(Value::String(text)) => {
                serde_json::from_str(text).unwrap_or_else(|_| Value::String(text.clone()))
            }
            Some(value) => value.clone(),
            None => Value::Null,
        };
        let (spec, patch) = (parsed("spec"), parsed("patch"));
        let id = args["id"].as_str();
        let title = args["title"]
            .as_str()
            .map(str::trim)
            .filter(|t| !t.is_empty());
        match args["action"].as_str() {
            Some("close") => {
                let surface = self.existing(id)?;
                self.remove(&surface.id);
                let noun = kind::of(&surface.kind).noun;
                return Ok(Reply::Done(Answer {
                    text: format!("Closed {noun} \"{}\" ({}).", surface.title, surface.id),
                    details: surface.closed(),
                }));
            }
            Some("show") | Some("update") => {}
            other => {
                return Err(format!(
                    "gentle_ui: action must be show, update or close, not {}",
                    other.map_or("none".into(), |a| format!("\"{a}\""))
                ));
            }
        }
        if !patch.is_null() && !patch.is_object() {
            return Err("patch must be an object".into());
        }
        let surface = if args["action"] == "show" {
            if spec.is_null() {
                return Err("gentle_ui show needs a spec".into());
            }
            let kind = args["kind"].as_str().unwrap_or("diagram");
            let Some(def) = kind::find(kind) else {
                return Err(format!("gentle_ui: unknown kind \"{kind}\""));
            };
            let base = normalize::normalize(kind, &spec, &self.root)?;
            let spec = if patch.is_null() {
                base
            } else {
                patch::patch(kind, &base, &patch, &self.root)?
            };
            Surface {
                id: self.fresh_id(id),
                version: 1,
                kind: kind.to_string(),
                title: title.unwrap_or(def.title).to_string(),
                spec,
            }
        } else {
            let current = self.existing(id)?;
            if let Some(kind) = args["kind"].as_str().filter(|k| *k != current.kind) {
                return Err(format!(
                    "gentle_ui: \"{}\" is a {}; show a new UI for a {kind}",
                    current.id, current.kind
                ));
            }
            if spec.is_null() && patch.is_null() {
                return Err("gentle_ui update needs a spec (replace) or a patch (merge)".into());
            }
            let base = if spec.is_null() {
                current.spec.clone()
            } else {
                normalize::normalize(&current.kind, &spec, &self.root)?
            };
            let spec = if patch.is_null() {
                base
            } else {
                patch::patch(&current.kind, &base, &patch, &self.root)?
            };
            Surface {
                version: current.version + 1,
                title: title.map_or(current.title.clone(), str::to_string),
                spec,
                ..current
            }
        };
        self.set(surface.clone());
        let text = summary::summary(&surface);
        if args["wait"] != true {
            return Ok(Reply::Done(Answer {
                text,
                details: Value::Object(surface.details()),
            }));
        }
        if surface.kind != "review" {
            return Err("gentle_ui: wait is only for kind review".into());
        }
        let replaced = self
            .waiting
            .iter()
            .find(|(id, _)| *id == surface.id)
            .map(|(_, call)| call.clone())
            .and_then(|earlier| {
                self.stop_waiting(
                    &earlier,
                    "A newer version of this review is waiting instead.",
                )
                .map(|(_, answer)| (earlier, answer))
            });
        self.waiting.push((surface.id.clone(), call.to_string()));
        Ok(Reply::Waiting { surface, replaced })
    }

    /// Stop `call` waiting for a review's decisions (stopped, timed out):
    /// its answer says why; the review stays open.
    pub fn stop_waiting(&mut self, call: &str, why: &str) -> Option<(String, Answer)> {
        let at = self.waiting.iter().position(|(_, c)| c == call)?;
        let (id, call) = self.waiting.remove(at);
        let details = self
            .surface(&id)
            .map_or(Value::Null, |s| Value::Object(s.details()));
        Some((
            call,
            Answer {
                text: format!(
                    "{why} The review stays open: decisions the user sends later arrive as \"{EVENT}\" messages; write none of its hunks before then."
                ),
                details,
            },
        ))
    }

    /// The host context: the Off rule while the tool is off, and the
    /// context html pages shared since the last turn started; `None` when
    /// there is none (the context is cleared).
    pub fn context(&self) -> Option<(String, Value)> {
        let mut parts = Vec::new();
        if self.mode == Mode::Off {
            parts.push(OFF_RULE.to_string());
        }
        for (surface, change) in &self.pending {
            parts.push(format!(
                "Context shared by the HTML UI \"{}\" ({}):\n{}",
                surface.title,
                surface.id,
                summary::context_text(change)
            ));
        }
        if parts.is_empty() {
            return None;
        }
        let ids: Vec<&str> = self.pending.iter().map(|(s, _)| s.id.as_str()).collect();
        Some((parts.join("\n\n"), json!({ "ids": ids })))
    }

    /// A turn started: the pages' context went with it (once, as MCP Apps
    /// defer it). Whether the context changed.
    pub fn turn_started(&mut self) -> bool {
        let had = !self.pending.is_empty();
        self.pending.clear();
        had
    }

    /// One `/gentle-ui:event {json}` the app's surfaces send: `args` is the
    /// JSON after the command.
    pub fn event(&mut self, args: &str) -> Handled {
        let mut handled = Handled::default();
        let event = match serde_json::from_str::<Value>(args.trim()) {
            Ok(event @ Value::Object(_)) => event,
            Ok(_) => {
                handled.error = Some("gentle_ui: unreadable event (not an object)".into());
                return handled;
            }
            Err(error) => {
                handled.error = Some(format!("gentle_ui: unreadable event ({error})"));
                return handled;
            }
        };
        // Agent UI changed in the app's Configuration: no surface, no answer.
        if event["op"] == "mode" {
            match event["mode"].as_str().and_then(Mode::parse) {
                Some(mode) => {
                    handled.context_changed = (mode == Mode::Off) != (self.mode == Mode::Off);
                    self.mode = mode;
                    handled.mode = Some(mode);
                }
                None => handled.error = Some(format!("gentle_ui: unknown mode {}", event["mode"])),
            }
            return handled;
        }
        let id = event["id"].as_str().unwrap_or_default().to_string();
        let base_version = event["version"].as_f64();
        // Every event is answered, whatever its kind: the app's card shows
        // Sending until then, and Sent, or the error with Retry.
        let answer = |result: Result<u64, String>| {
            let version = base_version.map_or(json!(0), |v| json!(v as u64));
            Some(
                match result {
                    Ok(current) => {
                        json!({"id": id, "version": version, "ok": true, "current": current})
                    }
                    Err(error) => {
                        json!({"id": id, "version": version, "ok": false, "error": error})
                    }
                }
                .to_string(),
            )
        };
        // A saved template the user inserts: a new surface, as if shown.
        if event["op"] == "insert" {
            let kind = event["kind"].as_str().unwrap_or_default();
            let spec = match kind::find(kind) {
                None => Err(format!("unknown kind \"{kind}\"")),
                Some(_) => normalize::normalize(kind, &event["spec"], &self.root),
            };
            let spec = match spec {
                Ok(spec) => spec,
                Err(error) => {
                    handled.outcome = answer(Err(error));
                    return handled;
                }
            };
            let def = kind::of(kind);
            let title = event["title"]
                .as_str()
                .map(str::trim)
                .filter(|t| !t.is_empty())
                .unwrap_or(def.title)
                .to_string();
            let surface = Surface {
                id: self.fresh_id(Some(id.as_str()).filter(|id| !id.is_empty())),
                version: 1,
                kind: kind.to_string(),
                title: title.clone(),
                spec,
            };
            self.set(surface.clone());
            let template = event["template"]
                .as_str()
                .map(str::trim)
                .filter(|t| !t.is_empty())
                .unwrap_or(&title)
                .to_string();
            let theirs = if self.mode == Mode::Off {
                format!("is theirs. {OFF_NOTE}")
            } else {
                "is yours to update like any gentle_ui surface.".into()
            };
            let content = format!(
                "The user inserted the {} \"{title}\" ({}, v1) from their saved template \"{template}\"; it is shown in the conversation and {theirs}\n{}",
                def.noun,
                surface.id,
                summary::summary(&surface)
            );
            let mut details = surface.details();
            details.insert("inserted".into(), json!(true));
            details.insert("template".into(), json!(template));
            details.insert("changes".into(), json!([]));
            details.insert("summary".into(), json!([]));
            // Shown at once; the agent reads it with the next prompt (or now, mid-turn).
            handled.message = Some(Message {
                content,
                details: Value::Object(details),
                trigger_turn: false,
            });
            handled.outcome =
                Some(json!({"id": surface.id, "version": 0, "ok": true, "current": 1}).to_string());
            return handled;
        }
        let Some(current) = self.surface(&id).cloned() else {
            handled.outcome = answer(Err(format!(
                "The agent no longer has the UI \"{id}\" open (open: {}).",
                self.open_ids()
            )));
            return handled;
        };
        let def = kind::of(&current.kind);
        // "Ask agent to add to docs": the surface as the app exported it.
        if event["op"] == "docs" {
            let format = event["format"].as_str().unwrap_or("Markdown");
            let fence = event["fence"].as_str().unwrap_or_default();
            let body = match event["text"].as_str() {
                None => summary::summary(&current),
                Some(text) if fence.is_empty() => text.to_string(),
                Some(text) => format!(
                    "```{fence}\n{}\n```",
                    text.strip_suffix('\n').unwrap_or(text)
                ),
            };
            let content = format!(
                "The user asks you to add the {} \"{}\" ({}, v{}) to the project's documentation, as {format}: put it where it belongs under docs/ (extend a fitting file or create one), with a short introduction, and say where it went.\n{body}",
                def.noun, current.title, current.id, current.version
            );
            handled.message = Some(Message {
                content,
                details: json!({
                    "request": "docs", "surface": current.id, "kind": current.kind,
                    "title": current.title, "version": current.version, "format": format,
                }),
                trigger_turn: true,
            });
            handled.outcome = answer(Ok(current.version));
            return handled;
        }
        let changes: Vec<Value> = event["changes"].as_array().cloned().unwrap_or_default();
        let html = current.kind == "html";
        if html {
            // A page doesn't edit its spec: what it says or shares is about the
            // page as the agent last sent it, so no version check applies.
            // Model context waits for the next turn instead of starting one.
            if !changes.is_empty() && changes.iter().all(|c| c["op"] == "context") {
                let change = changes[changes.len() - 1].clone();
                match self.pending.iter_mut().find(|(s, _)| s.id == current.id) {
                    Some(entry) => *entry = (current.clone(), change),
                    None => self.pending.push((current.clone(), change)),
                }
                handled.context_changed = true;
                handled.outcome = answer(Ok(current.version));
                return handled;
            }
        } else if base_version.is_some_and(|v| v != current.version as f64) {
            handled.outcome = answer(Err(format!(
                "The agent changed this {} meanwhile (now v{}); review your edits on it and send again.",
                def.noun, current.version
            )));
            return handled;
        }
        let spec = if html {
            current.spec.clone()
        } else {
            match normalize::normalize(&current.kind, &event["spec"], &self.root) {
                Ok(spec) => spec,
                Err(error) => {
                    handled.outcome = answer(Err(error));
                    return handled;
                }
            }
        };
        let surface = Surface {
            version: current.version + 1,
            spec,
            ..current.clone()
        };
        self.set(surface.clone());
        let said: Vec<String> = event["summary"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|s| s.as_str().map(str::to_string))
            .collect();
        let note = event["note"]
            .as_str()
            .map(str::trim)
            .filter(|n| !n.is_empty())
            .map(str::to_string);
        let mut content = if html {
            let at = self.pending.iter().position(|(s, _)| s.id == current.id);
            let shared = at.map(|at| self.pending.remove(at));
            handled.context_changed = shared.is_some();
            let mut lines = vec![format!(
                "From the HTML UI \"{}\" ({}, v{}) the user says:",
                surface.title, surface.id, surface.version
            )];
            lines.extend(
                changes
                    .iter()
                    .filter(|c| c["op"] == "message")
                    .filter_map(|c| c["text"].as_str().map(str::to_string)),
            );
            if let Some((_, change)) = shared {
                lines.push(format!(
                    "Context the page shared: {}",
                    summary::context_text(&change)
                ));
            }
            lines.join("\n")
        } else if current.kind == "review" {
            let content = format!(
                "{}\n{}",
                summary::review_decisions(&surface, &changes, note.as_deref()),
                summary::review(&surface)
            );
            // A `wait: true` call gets the decisions as its result instead.
            if let Some(at) = self.waiting.iter().position(|(id, _)| *id == surface.id) {
                let (_, call) = self.waiting.remove(at);
                handled.decided = Some((
                    call,
                    Answer {
                        text: content,
                        details: Value::Object(surface.details()),
                    },
                ));
                handled.outcome = answer(Ok(surface.version));
                return handled;
            }
            content
        } else {
            let mut lines = vec![format!(
                "The user edited the {} \"{}\" ({}, now v{}) in the app:",
                def.noun, surface.title, surface.id, surface.version
            )];
            lines.extend(said.iter().map(|line| format!("- {line}")));
            if let Some(note) = &note {
                lines.push(format!("Note from the user: {note}"));
            }
            lines.extend(summary::instructions(&surface, &changes));
            lines.push(format!("Changes: {}", Value::Array(changes.clone())));
            lines.push(summary::summary(&surface));
            lines.join("\n")
        };
        if self.mode == Mode::Off {
            content.push('\n');
            content.push_str(OFF_NOTE);
        }
        let mut details = surface.details();
        details.insert("changes".into(), Value::Array(changes));
        details.insert("summary".into(), json!(said));
        if let Some(note) = note {
            details.insert("note".into(), json!(note));
        }
        details.insert("baseVersion".into(), json!(current.version));
        handled.message = Some(Message {
            content,
            details: Value::Object(details),
            trigger_turn: true,
        });
        handled.outcome = answer(Ok(surface.version));
        handled
    }
}

#[cfg(test)]
mod tests;
