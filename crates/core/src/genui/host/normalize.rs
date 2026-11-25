//! Each kind's spec checked and with its defaults, as the extension's
//! `normalize*` functions return it. The kind's own parser (the one the app
//! draws with, which mirrors the extension's rules) checks it and gives its
//! wire JSON; on top of it comes what only the tool does: code refs made
//! relative to the session's folder, a review's `before`/`after` texts
//! diffed into hunks, and the html kind's checks.

use std::sync::LazyLock;

use regex::Regex;
use serde_json::{Map, Value, json};

use super::super::chart::Chart;
use super::super::checklist::Checklist;
use super::super::diagram::Diagram;
use super::super::edits::Editable;
use super::super::erd::Erd;
use super::super::form::Form;
use super::super::kanban::Board;
use super::super::review::Review;
use super::super::sequence::Sequence;
use super::super::states::Machine;
use super::super::table::Table;
use super::super::timeline::Timeline;
use super::{MAX_ERRORS, cut, kind};

/// `raw` as a `kind` spec, or the readable hard errors (the extension's
/// `invalid <noun> spec:` list). `root`: the session's folder, for refs.
pub fn normalize(kind: &str, raw: &Value, root: &str) -> Result<Value, String> {
    let Some(def) = kind::find(kind) else {
        return Err(format!("unknown kind \"{kind}\""));
    };
    let parsed = match kind {
        "diagram" => Diagram::parse(raw).map(|d| d.to_value()),
        "table" => Table::parse(raw).map(|t| t.to_value()),
        "form" => Form::parse(raw).map(|f| f.to_value()),
        "checklist" => Checklist::parse(raw).map(|c| c.to_value()),
        "kanban" => Board::parse(raw).map(|b| b.to_value()),
        "chart" => Chart::parse(raw).map(|c| c.to_value()),
        "html" => html(raw),
        "review" => Review::parse(&review_from_text(raw)).map(|r| r.to_value()),
        "sequence" => Sequence::parse(raw).map(|s| s.to_value()),
        "erd" => Erd::parse(raw).map(|e| e.to_value()),
        "state" => Machine::parse(raw).map(|m| m.to_value()),
        "timeline" => Timeline::parse(raw).map(|t| t.to_value()),
        _ => unreachable!("every kind has a parser"),
    };
    let invalid = |mut errors: Vec<String>| {
        errors.truncate(MAX_ERRORS);
        format!("invalid {} spec:\n- {}", def.noun, errors.join("\n- "))
    };
    let mut spec = parsed.map_err(invalid)?;
    let errors = rebase_refs(kind, &mut spec, root);
    if errors.is_empty() {
        Ok(spec)
    } else {
        Err(invalid(errors))
    }
}

/// Absolute ref paths inside `root` made relative to it; one outside it is
/// a hard error (the extension's `normalizeRef`).
fn rebase_refs(kind: &str, spec: &mut Value, root: &str) -> Vec<String> {
    let root = root.replace('\\', "/");
    let root = root.trim_end_matches('/');
    if root.is_empty() {
        return Vec::new();
    }
    let (list, noun) = match kind {
        "diagram" => ("nodes", "node"),
        "table" => ("rows", "row"),
        "checklist" => ("items", "item"),
        "kanban" => ("cards", "card"),
        "erd" => ("tables", "table"),
        _ => return Vec::new(),
    };
    let mut errors = Vec::new();
    let prefix = format!("{root}/");
    for item in spec[list].as_array_mut().into_iter().flatten() {
        rebase_ref(item, noun, &prefix, &mut errors);
        // A checklist's sub-items carry refs too.
        for sub in item
            .get_mut("items")
            .and_then(Value::as_array_mut)
            .into_iter()
            .flatten()
        {
            rebase_ref(sub, noun, &prefix, &mut errors);
        }
    }
    errors
}

fn rebase_ref(item: &mut Value, noun: &str, prefix: &str, errors: &mut Vec<String>) {
    let id = item["id"].as_str().unwrap_or_default().to_string();
    let Some(path) = item.get_mut("ref").and_then(|r| r.get_mut("path")) else {
        return;
    };
    let text = path.as_str().unwrap_or_default().to_string();
    if let Some(rest) = text.strip_prefix(prefix).filter(|rest| !rest.is_empty()) {
        *path = json!(rest);
    } else if is_absolute(&text) {
        errors.push(format!(
            "{noun} \"{id}\": ref path \"{}\" is outside the workspace",
            cut(&text, 80)
        ));
    }
}

fn is_absolute(path: &str) -> bool {
    let bytes = path.as_bytes();
    path.starts_with('/')
        || (bytes.len() >= 3
            && bytes[0].is_ascii_alphabetic()
            && bytes[1] == b':'
            && bytes[2] == b'/')
}

// ── html: an agent-written page, run as an MCP App ─────────────────────────

const HTML_MAX: usize = 512 * 1024;
const CSP_KEYS: [&str; 4] = [
    "connectDomains",
    "resourceDomains",
    "frameDomains",
    "baseUriDomains",
];
const MAX_ORIGINS: usize = 20;
/// Web origins only: https/wss anywhere, http/ws on loopback.
static ORIGIN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?i)^(?:(?:https|wss)://(?:\*\.)?[a-z0-9-]+(?:\.[a-z0-9-]+)*|(?:http|ws)://(?:localhost|127\.0\.0\.1))(?::\d{1,5})?(?:/[A-Za-z0-9\-._~%/]*)?$",
    )
    .expect("a valid pattern")
});
/// A wildcard over a top-level domain (`https://*.com`).
static WILD_TLD: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)^(?:https|wss)://\*\.[^.]+$").expect("a valid pattern"));

/// An html spec `{ html, height?, csp?, data? }`, checked.
fn html(raw: &Value) -> Result<Value, Vec<String>> {
    let Some(raw) = raw.as_object() else {
        return Err(vec![
            "spec must be an object: { html, height?, csp?, data? }".into(),
        ]);
    };
    let mut errors = Vec::new();
    let mut spec = Map::new();
    match raw.get("html").and_then(Value::as_str) {
        Some(html) if !html.trim().is_empty() => {
            if html.len() > HTML_MAX {
                errors.push(format!("spec.html is larger than {} KiB", HTML_MAX / 1024));
            } else {
                spec.insert("html".into(), json!(html));
            }
        }
        _ => errors.push("spec.html must be a non-empty string with the page".into()),
    }
    match raw.get("height") {
        None | Some(Value::Null) => {}
        Some(Value::Number(n)) if n.as_f64().is_some_and(f64::is_finite) => {
            let height = n.as_f64().unwrap_or_default().clamp(120., 2000.).round();
            spec.insert("height".into(), json!(height as i64));
        }
        Some(_) => errors.push("spec.height must be a number (px)".into()),
    }
    match raw.get("csp") {
        None | Some(Value::Null) => {}
        Some(Value::Object(given)) => {
            let mut csp = Map::new();
            for key in CSP_KEYS {
                let origins = match given.get(key) {
                    None | Some(Value::Null) => continue,
                    Some(Value::Array(origins)) if origins.len() <= MAX_ORIGINS => origins,
                    Some(_) => {
                        errors.push(format!(
                            "spec.csp.{key} must be an array of at most {MAX_ORIGINS} origins"
                        ));
                        continue;
                    }
                };
                let trimmed: Vec<String> = origins
                    .iter()
                    .map(|o| {
                        o.as_str()
                            .unwrap_or_default()
                            .trim()
                            .trim_end_matches('/')
                            .to_string()
                    })
                    .collect();
                let bad: Vec<String> = trimmed
                    .iter()
                    .filter(|o| !ORIGIN.is_match(o) || WILD_TLD.is_match(o))
                    .map(|o| json!(o).to_string())
                    .collect();
                if !bad.is_empty() {
                    errors.push(format!(
                        "spec.csp.{key}: not origins like https://api.example.com: {}",
                        bad.join(", ")
                    ));
                } else if !trimmed.is_empty() {
                    let mut unique: Vec<String> = Vec::new();
                    for origin in trimmed {
                        if !unique.contains(&origin) {
                            unique.push(origin);
                        }
                    }
                    csp.insert(key.into(), json!(unique));
                }
            }
            if !csp.is_empty() {
                spec.insert("csp".into(), Value::Object(csp));
            }
        }
        Some(_) => errors.push("spec.csp must be an object".into()),
    }
    match raw.get("data") {
        None | Some(Value::Null) => {}
        Some(data @ Value::Object(_)) => {
            spec.insert("data".into(), data.clone());
        }
        Some(_) => errors.push("spec.data must be an object".into()),
    }
    if errors.is_empty() {
        Ok(Value::Object(spec))
    } else {
        Err(errors)
    }
}

// ── review: a file given as whole texts becomes hunks ──────────────────────

/// Context lines around a change.
const CONTEXT: usize = 3;
/// Past this many differences, a file is a plain replacement.
const EDITS: usize = 2000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LineKind {
    Context,
    Add,
    Del,
}

impl LineKind {
    fn name(self) -> &'static str {
        match self {
            LineKind::Context => "context",
            LineKind::Add => "add",
            LineKind::Del => "del",
        }
    }
}

/// The spec with every file given as `before`/`after` texts (and no hunks)
/// turned into hunks.
fn review_from_text(raw: &Value) -> Value {
    let mut spec = raw.clone();
    if let Some(files) = spec.get_mut("files").and_then(Value::as_array_mut) {
        for file in files {
            if let Some(hunks) = file_from_text(file) {
                *file = hunks;
            }
        }
    }
    spec
}

fn file_from_text(file: &Value) -> Option<Value> {
    let object = file.as_object()?;
    if object.contains_key("hunks") {
        return None;
    }
    let before = object.get("before").and_then(Value::as_str);
    let after = object.get("after").and_then(Value::as_str);
    if before.is_none() && after.is_none() {
        return None;
    }
    let mut rest = object.clone();
    rest.remove("before");
    rest.remove("after");
    if !rest.contains_key("status") {
        match (before, after) {
            (None, _) => {
                rest.insert("status".into(), json!("added"));
            }
            (_, None) => {
                rest.insert("status".into(), json!("deleted"));
            }
            _ => {}
        }
    }
    let lines = line_diff(
        &text_lines(before.unwrap_or_default()),
        &text_lines(after.unwrap_or_default()),
    );
    rest.insert("hunks".into(), Value::Array(diff_hunks(&lines)));
    Some(Value::Object(rest))
}

/// `text` as lines: one trailing newline ends the last line, `\r` dropped.
fn text_lines(text: &str) -> Vec<&str> {
    if text.is_empty() {
        return Vec::new();
    }
    let text = text.strip_suffix('\n').unwrap_or(text);
    text.split('\n')
        .map(|line| line.strip_suffix('\r').unwrap_or(line))
        .collect()
}

/// The lines of `after` against `before` (Myers' O(ND) diff on lines; a
/// plain replacement when they differ in more than [`EDITS`] places).
fn line_diff<'a>(before: &[&'a str], after: &[&'a str]) -> Vec<(LineKind, &'a str)> {
    let mut start = 0;
    while start < before.len() && start < after.len() && before[start] == after[start] {
        start += 1;
    }
    let (mut end_a, mut end_b) = (before.len(), after.len());
    while end_a > start && end_b > start && before[end_a - 1] == after[end_b - 1] {
        end_a -= 1;
        end_b -= 1;
    }
    let a = &before[start..end_a];
    let b = &after[start..end_b];
    let (n, m) = (a.len() as i64, b.len() as i64);
    let offset = n + m + 1;
    let mut v = vec![0_i64; (2 * offset + 1) as usize];
    // trace[d]: v for k in -d..=d before step d.
    let mut trace: Vec<Vec<i64>> = Vec::new();
    let mut found = n + m == 0;
    let mut d = 0;
    while d <= (n + m).min(EDITS as i64) && !found {
        trace.push(v[(offset - d) as usize..(offset + d + 1) as usize].to_vec());
        let mut k = -d;
        while k <= d {
            let at = |k: i64| v[(offset + k) as usize];
            let mut x = if k == -d || (k != d && at(k - 1) < at(k + 1)) {
                at(k + 1)
            } else {
                at(k - 1) + 1
            };
            let mut y = x - k;
            while x < n && y < m && a[x as usize] == b[y as usize] {
                x += 1;
                y += 1;
            }
            v[(offset + k) as usize] = x;
            if x >= n && y >= m {
                found = true;
                break;
            }
            k += 2;
        }
        d += 1;
    }
    let mut middle: Vec<(LineKind, &str)> = Vec::new();
    if !found {
        middle.extend(a.iter().map(|text| (LineKind::Del, *text)));
        middle.extend(b.iter().map(|text| (LineKind::Add, *text)));
    } else {
        let (mut x, mut y) = (n, m);
        for d in (1..trace.len() as i64).rev() {
            let prev = &trace[d as usize];
            let at = |k: i64| prev[(k + d) as usize];
            let k = x - y;
            let down = k == -d || (k != d && at(k - 1) < at(k + 1));
            let prev_k = if down { k + 1 } else { k - 1 };
            let prev_x = at(prev_k);
            let prev_y = prev_x - prev_k;
            while x > prev_x && y > prev_y {
                middle.push((LineKind::Context, a[(x - 1) as usize]));
                x -= 1;
                y -= 1;
            }
            if down {
                middle.push((LineKind::Add, b[(y - 1) as usize]));
                y -= 1;
            } else {
                middle.push((LineKind::Del, a[(x - 1) as usize]));
                x -= 1;
            }
        }
        while x > 0 && y > 0 {
            middle.push((LineKind::Context, a[(x - 1) as usize]));
            x -= 1;
            y -= 1;
        }
        middle.reverse();
    }
    let mut lines: Vec<(LineKind, &str)> = before[..start]
        .iter()
        .map(|text| (LineKind::Context, *text))
        .collect();
    lines.extend(middle);
    lines.extend(
        before[end_a..]
            .iter()
            .map(|text| (LineKind::Context, *text)),
    );
    lines
}

/// Unified hunks of a line diff, with [`CONTEXT`] lines around changes.
fn diff_hunks(lines: &[(LineKind, &str)]) -> Vec<Value> {
    let (mut old, mut now) = (0, 0);
    let at: Vec<(usize, usize)> = lines
        .iter()
        .map(|(kind, _)| {
            let place = (old, now);
            if *kind != LineKind::Add {
                old += 1;
            }
            if *kind != LineKind::Del {
                now += 1;
            }
            place
        })
        .collect();
    let changed: Vec<usize> = lines
        .iter()
        .enumerate()
        .filter(|(_, (kind, _))| *kind != LineKind::Context)
        .map(|(i, _)| i)
        .collect();
    let mut hunks = Vec::new();
    let mut i = 0;
    while i < changed.len() {
        let mut j = i;
        while j + 1 < changed.len() && changed[j + 1] - changed[j] <= 2 * CONTEXT + 1 {
            j += 1;
        }
        let from = changed[i].saturating_sub(CONTEXT);
        let to = (changed[j] + CONTEXT + 1).min(lines.len());
        let body = &lines[from..to];
        let old_count = body.iter().filter(|(k, _)| *k != LineKind::Add).count();
        let new_count = body.iter().filter(|(k, _)| *k != LineKind::Del).count();
        hunks.push(json!({
            "oldStart": at[from].0 + usize::from(old_count > 0),
            "newStart": at[from].1 + usize::from(new_count > 0),
            "lines": body
                .iter()
                .map(|(kind, text)| json!({"kind": kind.name(), "text": text}))
                .collect::<Vec<_>>(),
        }));
        i = j + 1;
    }
    hunks
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(lines: &[(LineKind, &str)]) -> String {
        lines
            .iter()
            .map(|(kind, text)| {
                let mark = match kind {
                    LineKind::Context => ' ',
                    LineKind::Add => '+',
                    LineKind::Del => '-',
                };
                format!("{mark}{text}")
            })
            .collect::<Vec<_>>()
            .join("|")
    }

    #[test]
    fn line_diff_finds_the_shortest_edit() {
        let before = ["a", "b", "c", "d"];
        let after = ["a", "x", "c", "d", "e"];
        assert_eq!(kinds(&line_diff(&before, &after)), " a|-b|+x| c| d|+e");
        assert_eq!(kinds(&line_diff(&[], &["new"])), "+new");
        assert_eq!(kinds(&line_diff(&["old"], &[])), "-old");
        assert_eq!(kinds(&line_diff(&["same"], &["same"])), " same");
    }

    #[test]
    fn text_files_become_hunks_with_context() {
        let before: String = (1..=20).map(|n| format!("line {n}\n")).collect();
        let after = before.replace("line 10\n", "line ten\n");
        let spec = review_from_text(&json!({"files": [
            {"path": "a.txt", "before": before, "after": after},
            {"path": "new.txt", "after": "hi\n"},
        ]}));
        let hunk = &spec["files"][0]["hunks"][0];
        assert_eq!(
            (hunk["oldStart"].as_u64(), hunk["newStart"].as_u64()),
            (Some(7), Some(7))
        );
        assert_eq!(hunk["lines"].as_array().unwrap().len(), 8);
        assert_eq!(spec["files"][0].get("before"), None);
        assert_eq!(spec["files"][1]["status"], "added");
        assert_eq!(
            spec["files"][1]["hunks"][0],
            json!({"oldStart": 0, "newStart": 1, "lines": [{"kind": "add", "text": "hi"}]})
        );
    }

    #[test]
    fn far_apart_changes_are_separate_hunks() {
        let before: Vec<String> = (1..=30).map(|n| n.to_string()).collect();
        let mut after = before.clone();
        after[2] = "three".into();
        after[25] = "twenty-six".into();
        let before: Vec<&str> = before.iter().map(String::as_str).collect();
        let after: Vec<&str> = after.iter().map(String::as_str).collect();
        let hunks = diff_hunks(&line_diff(&before, &after));
        assert_eq!(hunks.len(), 2);
        assert_eq!(hunks[0]["oldStart"], 1);
        assert_eq!(hunks[1]["oldStart"], 23);
    }

    #[test]
    fn html_specs_are_checked_like_the_extension() {
        let spec = html(&json!({
            "html": "<p>hi</p>", "height": 50.4,
            "csp": {"connectDomains": ["https://api.example.com/", "https://api.example.com"]},
            "data": {"n": 1},
        }))
        .unwrap();
        assert_eq!(
            spec,
            json!({"html": "<p>hi</p>", "height": 120,
                   "csp": {"connectDomains": ["https://api.example.com"]}, "data": {"n": 1}})
        );
        let errors = html(&json!({
            "html": " ", "height": "tall",
            "csp": {"frameDomains": ["https://*.com", "http://example.com"]},
            "data": [],
        }))
        .unwrap_err();
        assert_eq!(
            errors,
            [
                "spec.html must be a non-empty string with the page",
                "spec.height must be a number (px)",
                "spec.csp.frameDomains: not origins like https://api.example.com: \"https://*.com\", \"http://example.com\"",
                "spec.data must be an object",
            ]
        );
    }

    #[test]
    fn refs_inside_the_root_become_relative_and_outside_fail() {
        let raw = json!({"nodes": [
            {"id": "a", "ref": "/work/app/src/a.rs:3"},
            {"id": "b", "ref": {"path": "src/b.rs"}},
        ]});
        let spec = normalize("diagram", &raw, "/work/app/").unwrap();
        assert_eq!(
            spec["nodes"][0]["ref"],
            json!({"path": "src/a.rs", "line": 3})
        );
        assert_eq!(spec["nodes"][1]["ref"], json!({"path": "src/b.rs"}));
        let outside = json!({"items": [{"id": "x", "label": "X", "items": [
            {"id": "y", "label": "Y", "ref": "/etc/passwd"},
        ]}]});
        assert_eq!(
            normalize("checklist", &outside, "/work/app").unwrap_err(),
            "invalid checklist spec:\n- item \"y\": ref path \"/etc/passwd\" is outside the workspace"
        );
        // No folder known: refs stay as given.
        assert!(normalize("checklist", &outside, "").is_ok());
    }

    #[test]
    fn errors_name_the_kind_and_stop_at_ten() {
        let nodes: Vec<Value> = (0..12).map(|_| json!({"label": "x"})).collect();
        let error = normalize("diagram", &json!({"nodes": nodes}), "").unwrap_err();
        assert!(error.starts_with("invalid diagram spec:\n- node #1 has no id"));
        assert_eq!(error.matches("\n- ").count(), MAX_ERRORS);
        assert_eq!(
            normalize("pie", &json!({}), "").unwrap_err(),
            "unknown kind \"pie\""
        );
    }
}
