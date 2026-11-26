//! Generative UI: interactive surfaces the agent shows through the
//! `gentle_ui` tool (phase 1: architecture diagrams) and the user's edits
//! sent back to it as `gentle-ui:event` commands.

pub mod apps;
pub mod changes;
pub mod chart;
pub mod checklist;
pub mod diagram;
pub mod edits;
pub mod erd;
pub mod export;
pub mod form;
pub mod host;
pub mod html;
pub mod kanban;
pub mod layout;
pub mod live;
pub mod mode;
pub mod refs;
pub mod review;
pub mod sequence;
pub mod states;
pub mod style;
pub mod table;
pub mod template;
pub mod timeline;

use serde_json::{Map, Value, json};

pub const TOOL: &str = "gentle_ui";
pub const EVENT_MESSAGE: &str = "gentle-ui.event";
pub const EVENT_COMMAND: &str = "gentle-ui:event";
/// The widget key the extension answers each `/gentle-ui:event` with: one
/// line of JSON, `{id, version, ok, error?, current?}`, cleared right after.
pub const OUTCOME_WIDGET: &str = "gentle-ui.outcome";

/// The extension's answer to one `/gentle-ui:event`: `version` is the one
/// the event was based on; `result` the surface's new version, or why the
/// edits were rejected.
#[derive(Debug, Clone, PartialEq)]
pub struct Outcome {
    pub id: String,
    pub version: u64,
    pub result: Result<u64, String>,
}

/// None unless the line is an object with a non-empty `id` and a boolean `ok`.
pub fn outcome(line: &str) -> Option<Outcome> {
    let value: Value = serde_json::from_str(line).ok()?;
    let id = value.get("id")?.as_str().filter(|id| !id.is_empty())?;
    let version = value.get("version").and_then(Value::as_u64).unwrap_or(0);
    let result = if value.get("ok")?.as_bool()? {
        Ok(value
            .get("current")
            .and_then(Value::as_u64)
            .unwrap_or(version + 1))
    } else {
        Err(value
            .get("error")
            .and_then(Value::as_str)
            .filter(|e| !e.trim().is_empty())
            .unwrap_or("the extension rejected the edits")
            .to_string())
    };
    Some(Outcome {
        id: id.to_string(),
        version,
        result,
    })
}

/// One version of a surface: a gentle_ui tool result's details or a gentle-ui.event message's details.
#[derive(Debug, Clone, PartialEq)]
pub struct Snapshot {
    pub id: String,
    pub version: u64,
    pub kind: String,
    pub title: String,
    pub spec: Value,
    pub closed: bool,
}

/// None unless `details.id` is a non-empty string.
pub fn snapshot(details: &Value) -> Option<Snapshot> {
    let id = details.get("id")?.as_str().filter(|id| !id.is_empty())?;
    let text = |key: &str| details.get(key).and_then(Value::as_str);
    Some(Snapshot {
        id: id.to_string(),
        version: details.get("version").and_then(Value::as_u64).unwrap_or(0),
        kind: text("kind").unwrap_or("diagram").to_string(),
        title: text("title").unwrap_or_default().to_string(),
        spec: details.get("spec").cloned().unwrap_or(Value::Null),
        closed: details.get("closed") == Some(&Value::Bool(true)),
    })
}

/// `/gentle-ui:event {json}` for the user's edits; the JSON stays on one line
/// because the extension reads everything after the first space.
pub fn event_command(
    id: &str,
    version: u64,
    changes: &[changes::Change],
    summary: &[String],
    spec: &diagram::Diagram,
    note: Option<&str>,
) -> String {
    let changes: Vec<Value> = changes.iter().map(changes::Change::to_value).collect();
    event_command_json(id, version, &changes, summary, &spec.to_value(), note)
}

/// [`event_command`] for any kind: its changes and spec already as JSON.
pub fn event_command_json(
    id: &str,
    version: u64,
    changes: &[Value],
    summary: &[String],
    spec: &Value,
    note: Option<&str>,
) -> String {
    let mut payload = Map::new();
    payload.insert("id".into(), json!(id));
    payload.insert("version".into(), json!(version));
    payload.insert("changes".into(), Value::Array(changes.to_vec()));
    payload.insert("summary".into(), json!(summary));
    payload.insert("spec".into(), spec.clone());
    if let Some(note) = note.map(str::trim).filter(|n| !n.is_empty()) {
        payload.insert("note".into(), json!(note));
    }
    let json = serde_json::to_string(&Value::Object(payload)).expect("JSON values serialize");
    format!("/{EVENT_COMMAND} {json}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snapshot_reads_details_with_defaults() {
        assert_eq!(snapshot(&json!({"id": ""})), None);
        assert_eq!(snapshot(&json!({"version": 2})), None);
        assert_eq!(
            snapshot(&json!({"id": "ui-1"})),
            Some(Snapshot {
                id: "ui-1".into(),
                version: 0,
                kind: "diagram".into(),
                title: String::new(),
                spec: Value::Null,
                closed: false,
            })
        );
        let closed = snapshot(&json!({
            "id": "ui-2", "version": 3, "kind": "diagram", "title": "Checkout",
            "spec": null, "closed": true,
        }))
        .unwrap();
        assert_eq!(
            (closed.version, closed.title.as_str(), closed.closed),
            (3, "Checkout", true)
        );
    }

    #[test]
    fn event_command_is_one_line_with_optional_note() {
        let d = diagram::Diagram::parse(&json!({"nodes": [{"id": "a"}]})).unwrap();
        let moved = changes::Change::Move {
            node: "a".into(),
            x: 1.,
            y: 2.,
        };
        let summary = vec!["moved a\nsomewhere".to_string()];
        let cmd = event_command("ui-1", 3, &[moved], &summary, &d, Some("  "));
        assert!(!cmd.contains('\n'));
        let json = cmd.strip_prefix("/gentle-ui:event ").unwrap();
        let value: Value = serde_json::from_str(json).unwrap();
        assert_eq!(value["version"], 3);
        assert_eq!(value["changes"][0]["op"], "move");
        assert_eq!(value["spec"]["nodes"][0]["id"], "a");
        assert!(value.get("note").is_none());

        let with_note = event_command("ui-1", 3, &[], &[], &d, Some("keep it small"));
        assert!(with_note.ends_with(r#""note":"keep it small"}"#));
    }

    #[test]
    fn outcome_reads_acceptance_and_rejection() {
        assert_eq!(
            outcome(r#"{"id":"ui-1","version":3,"ok":true,"current":4}"#),
            Some(Outcome {
                id: "ui-1".into(),
                version: 3,
                result: Ok(4)
            })
        );
        assert_eq!(
            outcome(r#"{"id":"ui-1","version":3,"ok":false,"error":"stale"}"#)
                .unwrap()
                .result,
            Err("stale".into())
        );
        assert_eq!(
            outcome(r#"{"id":"ui-1","version":3,"ok":false}"#)
                .unwrap()
                .result,
            Err("the extension rejected the edits".into())
        );
        assert_eq!(outcome(r#"{"id":"ui-1","version":3}"#), None);
        assert_eq!(outcome(r#"{"id":"","ok":true}"#), None);
        assert_eq!(outcome("not json"), None);
    }
}
