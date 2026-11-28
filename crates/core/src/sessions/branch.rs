//! The conversation a Pi session file holds: the messages `get_messages`
//! answers once Pi resumes the file (`--session`), rebuilt from the file
//! alone so a restored tab can show them before its backend is up.
//!
//! Mirrors pi-coding-agent's `SessionManager`: the active branch runs from
//! the file's last entry up its `parentId`s (`_buildIndex`,
//! `buildSessionPath`); the latest compaction on it stands for what it
//! summarized (`buildContextEntries`); each entry becomes its messages, with
//! its latest `context_edit` applied (`buildSessionProjection`).

use std::collections::HashMap;

use serde_json::{Map, Value, json};

use super::parse_iso_ms;

/// The session format read here. Pi migrates older files when it loads
/// them (and rewrites them), so they are left to it.
const VERSION: u64 = 3;

/// The messages of `text`'s active branch, in order, as Pi's `get_messages`
/// lists them. Blank and malformed lines are skipped, as Pi does. `None`
/// when the text is not a session in the current format, or when its
/// entries loop (Pi would never finish resuming it).
pub fn messages(text: &str) -> Option<Vec<Value>> {
    let mut lines = text
        .lines()
        .filter(|line| !line.trim().is_empty())
        .filter_map(|line| serde_json::from_str::<Value>(line).ok());
    let header = lines.next()?;
    if header["type"] != "session"
        || !header["id"].is_string()
        || header["version"].as_u64().unwrap_or(1) < VERSION
    {
        return None;
    }
    let entries: Vec<Value> = lines.filter(|entry| entry["type"] != "session").collect();
    let context = context_entries(active_path(&entries)?);
    let mut edits = HashMap::new();
    for entry in &context {
        if entry["type"] == "context_edit"
            && let Some(target) = entry["targetId"].as_str()
        {
            edits.insert(target, *entry);
        }
    }
    Some(
        context
            .iter()
            .enumerate()
            // Only the newest compaction (first) speaks; an older one kept
            // inside its range adds nothing.
            .filter(|(ix, entry)| *ix == 0 || entry["type"] != "compaction")
            .flat_map(|(_, entry)| {
                let edit = entry["id"].as_str().and_then(|id| edits.get(id));
                edited(entry_messages(entry), edit.copied())
            })
            .collect(),
    )
}

/// The entries from the root to the file's last entry, Pi's leaf after a
/// load (`_buildIndex`). `None` when the parents loop.
fn active_path(entries: &[Value]) -> Option<Vec<&Value>> {
    let by_id: HashMap<&str, &Value> = entries
        .iter()
        .filter_map(|entry| Some((entry["id"].as_str()?, entry)))
        .collect();
    let mut path = Vec::new();
    let mut current = entries.last();
    while let Some(entry) = current {
        if path.len() == entries.len() {
            return None;
        }
        path.push(entry);
        current = entry["parentId"]
            .as_str()
            .filter(|id| !id.is_empty())
            .and_then(|id| by_id.get(id).copied());
    }
    path.reverse();
    Some(path)
}

/// `buildContextEntries`: with a compaction on the path, the newest one,
/// then the entries it kept (from `firstKeptEntryId`, without system
/// messages), then everything after it.
fn context_entries(path: Vec<&Value>) -> Vec<&Value> {
    let Some(at) = path.iter().rposition(|entry| entry["type"] == "compaction") else {
        return path;
    };
    let compaction = path[at];
    let first_kept = compaction["firstKeptEntryId"].as_str();
    let mut context = vec![compaction];
    let mut kept = false;
    for entry in &path[..at] {
        kept |= first_kept.is_some() && entry["id"].as_str() == first_kept;
        let system = entry["type"] == "message" && entry["message"]["role"] == "system";
        if kept && !system {
            context.push(entry);
        }
    }
    context.extend(&path[at + 1..]);
    context
}

/// `sessionEntryToContextMessages`: what one entry adds to the
/// conversation. Other entry types (model and thinking changes, labels,
/// names, extension state, edits) add nothing.
fn entry_messages(entry: &Value) -> Vec<Value> {
    match entry["type"].as_str() {
        Some("message") => {
            let Some(mut message) = entry.get("message").filter(|m| m.is_object()).cloned() else {
                return Vec::new();
            };
            if message.get("content").is_none_or(Value::is_null) {
                match message["role"].as_str() {
                    Some("system") => message["content"] = json!(""),
                    Some("user" | "assistant" | "toolResult") => message["content"] = json!([]),
                    _ => {}
                }
            }
            vec![message]
        }
        Some("custom_message") => {
            let mut message = role("custom");
            copy(entry, "customType", &mut message);
            let content = entry.get("content").filter(|c| !c.is_null());
            message.insert("content".into(), content.cloned().unwrap_or(json!([])));
            copy(entry, "display", &mut message);
            copy(entry, "details", &mut message);
            vec![stamped(message, entry)]
        }
        Some("branch_summary") if truthy(&entry["summary"]) => {
            let mut message = role("branchSummary");
            copy(entry, "summary", &mut message);
            copy(entry, "fromId", &mut message);
            vec![stamped(message, entry)]
        }
        Some("compaction") => {
            let mut message = role("compactionSummary");
            copy(entry, "summary", &mut message);
            copy(entry, "tokensBefore", &mut message);
            let summary = stamped(message, entry);
            match entry.get("systemMessage").filter(|m| truthy(m)) {
                Some(system) => vec![system.clone(), summary],
                None => vec![summary],
            }
        }
        _ => Vec::new(),
    }
}

/// `projectContextEntry`: a `context_edit` that removed the entry drops its
/// messages; one that replaced it swaps the content of its conversation
/// messages (a string becomes one text block for the assistant and tool
/// results).
fn edited(mut messages: Vec<Value>, edit: Option<&Value>) -> Vec<Value> {
    let Some(replacement) = edit.and_then(|edit| edit.get("replacement")) else {
        return messages;
    };
    if replacement.is_null() {
        return Vec::new();
    }
    for message in &mut messages {
        let (edits, wraps) = match message["role"].as_str() {
            Some("user" | "custom") => (true, false),
            Some("assistant" | "toolResult") => (true, true),
            _ => (false, false),
        };
        let Some(fields) = message.as_object_mut().filter(|_| edits) else {
            continue;
        };
        match replacement.get("content") {
            Some(Value::String(text)) if wraps => {
                fields.insert("content".into(), json!([{"type": "text", "text": text}]));
            }
            Some(content) => {
                fields.insert("content".into(), content.clone());
            }
            None => {
                fields.remove("content");
            }
        }
    }
    messages
}

fn role(role: &str) -> Map<String, Value> {
    let mut message = Map::new();
    message.insert("role".into(), json!(role));
    message
}

/// Copy `key` when the entry has it (a `null` too): Pi builds these
/// messages from the entry's fields, and JSON leaves out only the missing.
fn copy(entry: &Value, key: &str, message: &mut Map<String, Value>) {
    if let Some(value) = entry.get(key) {
        message.insert(key.into(), value.clone());
    }
}

/// The entry's time in milliseconds, as `new Date(timestamp).getTime()`
/// gives it (`null` for one it can't read).
fn stamped(mut message: Map<String, Value>, entry: &Value) -> Value {
    let at = match entry.get("timestamp") {
        Some(Value::String(iso)) => parse_iso_ms(iso).map_or(Value::Null, Value::from),
        Some(Value::Number(ms)) => Value::Number(ms.clone()),
        Some(Value::Null) => json!(0),
        _ => Value::Null,
    };
    message.insert("timestamp".into(), at);
    Value::Object(message)
}

/// JavaScript's truthiness.
fn truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().is_some_and(|n| n != 0.0),
        Value::String(s) => !s.is_empty(),
        Value::Array(_) | Value::Object(_) => true,
    }
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};

    use super::messages;

    const HEADER: &str = r#"{"type":"session","version":3,"id":"s1","timestamp":"2026-01-01T00:00:00.000Z","cwd":"/w"}"#;

    fn file(entries: &[Value]) -> String {
        let mut text = format!("{HEADER}\n");
        for entry in entries {
            text.push_str(&entry.to_string());
            text.push('\n');
        }
        text
    }

    fn message(id: &str, parent: Option<&str>, role: &str, text: &str) -> Value {
        json!({
            "type": "message", "id": id, "parentId": parent,
            "timestamp": "2026-01-01T00:00:01.000Z",
            "message": {"role": role, "content": [{"type": "text", "text": text}], "timestamp": 1},
        })
    }

    /// Role and first text of each message, for compact assertions.
    fn said(messages: &[Value]) -> Vec<(String, String)> {
        messages
            .iter()
            .map(|m| {
                let text = m["content"][0]["text"]
                    .as_str()
                    .or(m["summary"].as_str())
                    .unwrap_or_default();
                (m["role"].as_str().unwrap_or_default().into(), text.into())
            })
            .collect()
    }

    fn pairs(expected: &[(&str, &str)]) -> Vec<(String, String)> {
        expected
            .iter()
            .map(|(role, text)| (role.to_string(), text.to_string()))
            .collect()
    }

    #[test]
    fn a_linear_session_lists_its_messages_as_stored() {
        let user = message("a", None, "user", "hi");
        let answer = json!({
            "type": "message", "id": "c", "parentId": "b", "timestamp": "2026-01-01T00:00:02.000Z",
            "message": {"role": "assistant", "content": [{"type": "toolCall", "id": "t1", "name": "read", "arguments": {}}],
                        "provider": "p", "model": "m", "stopReason": "toolUse", "timestamp": 2},
        });
        let text = file(&[
            user.clone(),
            json!({"type": "model_change", "id": "b", "parentId": "a", "provider": "p", "modelId": "m"}),
            answer.clone(),
            message("d", Some("c"), "toolResult", "file"),
            json!({"type": "thinking_level_change", "id": "e", "parentId": "d", "thinkingLevel": "high"}),
        ]);
        let got = messages(&text).unwrap();
        assert_eq!(got[0], user["message"], "a message is passed through whole");
        assert_eq!(got[1], answer["message"]);
        assert_eq!(said(&got[2..]), pairs(&[("toolResult", "file")]));
        assert_eq!(got.len(), 3);
    }

    /// Pi resumes at the file's last entry: a branch left earlier in the
    /// file (a re-edited prompt) is not on the conversation, and the summary
    /// of the abandoned branch is.
    #[test]
    fn only_the_branch_ending_at_the_last_entry_is_listed() {
        let text = file(&[
            message("a", None, "user", "first"),
            message("b", Some("a"), "assistant", "answer"),
            message("c", Some("b"), "user", "abandoned"),
            message("d", Some("c"), "assistant", "abandoned answer"),
            json!({"type": "branch_summary", "id": "e", "parentId": "b", "fromId": "d",
                   "summary": "tried something", "timestamp": "2026-01-01T00:00:03.000Z"}),
            message("f", Some("e"), "user", "second"),
            message("g", Some("f"), "assistant", "second answer"),
        ]);
        let got = messages(&text).unwrap();
        assert_eq!(
            said(&got),
            pairs(&[
                ("user", "first"),
                ("assistant", "answer"),
                ("branchSummary", "tried something"),
                ("user", "second"),
                ("assistant", "second answer"),
            ])
        );
        assert_eq!(got[2]["fromId"], "d");
        assert_eq!(got[2]["timestamp"], 1_767_225_603_000_i64);

        // The last line can sit on the earlier branch too.
        let back = format!(
            "{text}{}\n",
            message("h", Some("d"), "user", "back on the first branch")
        );
        assert_eq!(
            said(&messages(&back).unwrap()),
            pairs(&[
                ("user", "first"),
                ("assistant", "answer"),
                ("user", "abandoned"),
                ("assistant", "abandoned answer"),
                ("user", "back on the first branch"),
            ])
        );
    }

    #[test]
    fn entries_that_are_not_conversation_and_broken_lines_are_skipped() {
        let mut text = file(&[
            message("a", None, "user", "hi"),
            json!({"type": "custom", "id": "b", "parentId": "a", "customType": "state", "data": {}}),
            json!({"type": "label", "id": "c", "parentId": "b", "targetId": "a", "label": "x"}),
            json!({"type": "session_info", "id": "d", "parentId": "c", "name": "n"}),
            json!({"type": "something_new", "id": "e", "parentId": "d"}),
        ]);
        text.push_str("{\"type\":\"message\",\"id\":\"broken\n\n");
        text.push_str(&format!(
            "{}\n",
            message("f", Some("e"), "assistant", "hello")
        ));
        // A line cut off mid-write at the end of the file.
        text.push_str(r#"{"type":"message","id":"g","parentId":"f","mess"#);
        assert_eq!(
            said(&messages(&text).unwrap()),
            pairs(&[("user", "hi"), ("assistant", "hello")])
        );
    }

    #[test]
    fn only_a_current_session_file_is_read() {
        let entry = message("a", None, "user", "hi").to_string();
        assert_eq!(messages(""), None);
        assert_eq!(messages(&entry), None, "no header");
        let old = r#"{"type":"session","version":2,"id":"s1"}"#;
        assert_eq!(messages(&format!("{old}\n{entry}")), None, "Pi migrates it");
        let unversioned = r#"{"type":"session","id":"s1"}"#;
        assert_eq!(messages(&format!("{unversioned}\n{entry}")), None);
        assert_eq!(
            messages(HEADER),
            Some(Vec::new()),
            "a session with nothing said"
        );
    }

    #[test]
    fn a_compaction_stands_for_what_it_summarized() {
        let text = file(&[
            message("a", None, "user", "old"),
            message("b", Some("a"), "assistant", "old answer"),
            message("c", Some("b"), "user", "kept"),
            message("d", Some("c"), "system", "a system prompt"),
            message("e", Some("d"), "assistant", "kept answer"),
            json!({"type": "compaction", "id": "f", "parentId": "e", "summary": "earlier work",
                   "firstKeptEntryId": "c", "tokensBefore": 1200, "timestamp": "2026-01-01T00:00:05.000Z"}),
            message("g", Some("f"), "user", "after"),
        ]);
        let got = messages(&text).unwrap();
        assert_eq!(
            said(&got),
            pairs(&[
                ("compactionSummary", "earlier work"),
                ("user", "kept"),
                ("assistant", "kept answer"),
                ("user", "after"),
            ])
        );
        assert_eq!(
            got[0],
            json!({"role": "compactionSummary", "summary": "earlier work", "tokensBefore": 1200,
                   "timestamp": 1_767_225_605_000_i64})
        );
    }

    #[test]
    fn context_edits_replace_or_remove_what_they_target() {
        let text = file(&[
            message("a", None, "user", "hi"),
            message("b", Some("a"), "assistant", "long answer"),
            message("c", Some("b"), "user", "secret"),
            json!({"type": "context_edit", "id": "d", "parentId": "c", "targetId": "b",
                   "replacement": {"content": "short"}}),
            json!({"type": "context_edit", "id": "e", "parentId": "d", "targetId": "c", "replacement": null}),
        ]);
        let got = messages(&text).unwrap();
        assert_eq!(said(&got), pairs(&[("user", "hi"), ("assistant", "short")]));
        assert_eq!(
            got[1]["content"],
            json!([{"type": "text", "text": "short"}])
        );
    }

    #[test]
    fn extension_messages_and_empty_content_take_pis_shape() {
        let text = file(&[
            json!({"type": "custom_message", "id": "a", "parentId": null, "customType": "gentle-agents.result",
                   "content": "done", "display": true, "details": {"agent": "x"},
                   "timestamp": "2026-01-01T00:00:00.500Z"}),
            json!({"type": "message", "id": "b", "parentId": "a", "message": {"role": "user", "content": null}}),
            json!({"type": "custom_message", "id": "c", "parentId": "b", "customType": "note"}),
        ]);
        let got = messages(&text).unwrap();
        assert_eq!(
            got,
            [
                json!({"role": "custom", "customType": "gentle-agents.result", "content": "done",
                       "display": true, "details": {"agent": "x"}, "timestamp": 1_767_225_600_500_i64}),
                json!({"role": "user", "content": []}),
                json!({"role": "custom", "customType": "note", "content": [], "timestamp": null}),
            ]
        );
    }

    #[test]
    fn looping_parents_are_not_followed_forever() {
        let text = file(&[
            message("a", Some("b"), "user", "one"),
            message("b", Some("a"), "assistant", "two"),
        ]);
        assert_eq!(messages(&text), None);
    }
}
