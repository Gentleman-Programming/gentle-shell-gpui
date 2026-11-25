//! What the model reads of a surface (each kind's summary, the extension's
//! `summary*`), and what an event asks of it beyond the edits: run a
//! checklist step, dispatch a subagent for a kanban card, apply a review's
//! accepted hunks.

use serde_json::{Value, json};

use super::super::timeline::day_of;
use super::{EVENT, Surface, cut, js_num, js_string, listed, plural, quoted};

const SUMMARY_ROWS: usize = 30;
const SUMMARY_LINES: usize = 80;

fn arr(value: &Value) -> &[Value] {
    value.as_array().map_or(&[], Vec::as_slice)
}

fn s(value: &Value) -> &str {
    value.as_str().unwrap_or_default()
}

/// A JS truthy check for the values specs hold.
fn truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().is_some_and(|n| n != 0.),
        Value::String(text) => !text.is_empty(),
        _ => true,
    }
}

fn head(surface: &Surface, what: &str) -> String {
    format!(
        "{what} \"{}\" ({}, v{})",
        surface.title, surface.id, surface.version
    )
}

/// Cut `lines` to the summary's limit, ending in "… and N more <what>".
fn limit(lines: &mut Vec<String>, what: &str) {
    if lines.len() > SUMMARY_LINES {
        let more = lines.len() - SUMMARY_LINES;
        lines.truncate(SUMMARY_LINES);
        lines.push(format!("… and {more} more {what}"));
    }
}

/// The surface as the model reads it.
pub fn summary(surface: &Surface) -> String {
    match surface.kind.as_str() {
        "table" => with_live(table(surface), surface),
        "form" => form(surface),
        "checklist" => with_live(checklist(surface), surface),
        "kanban" => kanban(surface),
        "chart" => with_live(chart(surface), surface),
        "html" => html(surface),
        "review" => review(surface),
        "sequence" => sequence(surface),
        "erd" => erd(surface),
        "state" => state(surface),
        "timeline" => timeline(surface),
        _ => diagram(surface),
    }
}

fn diagram(surface: &Surface) -> String {
    let spec = &surface.spec;
    let (groups, nodes, edges) = (
        arr(&spec["groups"]),
        arr(&spec["nodes"]),
        arr(&spec["edges"]),
    );
    let mut lines = vec![format!(
        "{} is shown to the user ({}, {}, {}, {}).",
        head(surface, "Diagram"),
        plural(nodes.len(), "node"),
        plural(edges.len(), "edge"),
        plural(groups.len(), "group"),
        s(&spec["direction"])
    )];
    if !groups.is_empty() {
        lines.push(listed(
            "Groups",
            groups
                .iter()
                .map(|g| format!("{} \"{}\"", s(&g["id"]), s(&g["label"]))),
        ));
    }
    if !nodes.is_empty() {
        lines.push(listed(
            "Nodes",
            nodes.iter().map(|n| {
                let parts: Vec<&str> = [&n["kind"], &n["group"], &n["status"]]
                    .into_iter()
                    .filter(|v| truthy(v))
                    .map(s)
                    .collect();
                format!(
                    "{} \"{}\" ({})",
                    s(&n["id"]),
                    s(&n["label"]),
                    parts.join(", ")
                )
            }),
        ));
    }
    if !edges.is_empty() {
        lines.push(listed(
            "Edges",
            edges.iter().map(|e| {
                let label = if truthy(&e["label"]) {
                    format!(" \"{}\"", s(&e["label"]))
                } else {
                    String::new()
                };
                let style = match s(&e["style"]) {
                    "sync" => String::new(),
                    style => format!(" [{style}]"),
                };
                format!("{} → {}{label}{style}", s(&e["from"]), s(&e["to"]))
            }),
        ));
    }
    lines.push(format!(
        "The user can move, rename, connect, group and delete parts, change a node's kind, status, tech and description, and flip the direction; their edits arrive as \"{EVENT}\" messages."
    ));
    lines.join("\n")
}

/// The summary's line about a live spec: the app refreshes it.
fn with_live(text: String, surface: &Surface) -> String {
    let live = &surface.spec["live"];
    if !live.is_object() {
        return text;
    }
    let from = if live["source"] == "command" && live["command"].is_object() {
        format!(
            "the command `{}` (run only if the user allows it)",
            js_string(&live["command"]["cmd"])
        )
    } else {
        format!("the app's {} data", js_string(&live["source"]))
    };
    let every = match live["interval"].as_f64() {
        Some(n) => format!(", every {} s", js_num(n)),
        None => String::new(),
    };
    format!(
        "{text}\nLive: the app fills it from {from}{every}; don't update its rows, values or items yourself."
    )
}

/// A table cell as text.
fn cell_text(value: &Value) -> String {
    match value {
        Value::Null => String::new(),
        Value::Bool(b) => if *b { "yes" } else { "no" }.into(),
        Value::Object(_) => {
            let text = &value["text"];
            if !text.is_null() {
                js_string(text)
            } else if !value["url"].is_null() {
                js_string(&value["url"])
            } else {
                String::new()
            }
        }
        other => js_string(other),
    }
}

fn table(surface: &Surface) -> String {
    let spec = &surface.spec;
    let (columns, rows, selection) = (
        arr(&spec["columns"]),
        arr(&spec["rows"]),
        arr(&spec["selection"]),
    );
    let mut flags = vec![if spec["editable"] == true {
        "editable".to_string()
    } else {
        "read-only".to_string()
    }];
    if !selection.is_empty() {
        flags.push(format!("{} selected", selection.len()));
    }
    let mut lines = vec![
        format!(
            "{} is shown to the user ({}, {}; {}).",
            head(surface, "Table"),
            plural(columns.len(), "column"),
            plural(rows.len(), "row"),
            flags.join(", ")
        ),
        listed(
            "Columns",
            columns.iter().map(|c| {
                format!(
                    "{} \"{}\" ({})",
                    s(&c["key"]),
                    s(&c["label"]),
                    s(&c["type"])
                )
            }),
        ),
    ];
    for row in rows.iter().take(SUMMARY_ROWS) {
        let cells: Vec<String> = columns
            .iter()
            .map(|c| {
                let text = cut(&cell_text(&row["cells"][s(&c["key"])]), 40);
                if text.is_empty() { "—".into() } else { text }
            })
            .collect();
        lines.push(format!("{}: {}", s(&row["id"]), cells.join(" | ")));
    }
    if rows.len() > SUMMARY_ROWS {
        lines.push(format!("… and {} more rows", rows.len() - SUMMARY_ROWS));
    }
    if spec["sort"].is_object() {
        lines.push(format!(
            "Sorted by {} ({}).",
            js_string(&spec["sort"]["column"]),
            js_string(&spec["sort"]["direction"])
        ));
    }
    if !selection.is_empty() {
        lines.push(listed("Selected", selection.iter().map(js_string)));
    }
    let select = if spec["selectable"] == true {
        ", select rows"
    } else {
        ""
    };
    let edit = if spec["editable"] == true {
        " and edit cells"
    } else {
        ""
    };
    lines.push(format!(
        "The user can sort and filter{select}{edit}; their selection and edits arrive as \"{EVENT}\" messages."
    ));
    lines.join("\n")
}

/// A form value as shown: option labels, "yes"/"no", "—" when empty.
fn shown_value(field: &Value, value: &Value) -> String {
    let label = |v: &Value| {
        arr(&field["options"])
            .iter()
            .find(|o| &o["value"] == v)
            .map(|o| js_string(&o["label"]))
            .unwrap_or_else(|| js_string(v))
    };
    match value {
        Value::Null => "—".into(),
        Value::Array(list) if list.is_empty() => "—".into(),
        Value::Array(list) => list.iter().map(label).collect::<Vec<_>>().join(", "),
        Value::Bool(b) => if *b { "yes" } else { "no" }.into(),
        other if field["options"].is_array() => cut(&label(other), 80),
        other => cut(&js_string(other), 80),
    }
}

fn form(surface: &Surface) -> String {
    let spec = &surface.spec;
    let fields = arr(&spec["fields"]);
    let submitted = spec["submitted"] == true;
    let mut lines = vec![
        format!(
            "{} is shown to the user ({}; {}).",
            head(surface, "Form"),
            plural(fields.len(), "field"),
            if submitted {
                "submitted"
            } else {
                "waiting for the user to submit"
            }
        ),
        listed(
            "Fields",
            fields.iter().map(|f| {
                let options = match f["options"].as_array() {
                    Some(options) => format!(
                        ": {}",
                        options
                            .iter()
                            .map(|o| js_string(&o["value"]))
                            .collect::<Vec<_>>()
                            .join("|")
                    ),
                    None => String::new(),
                };
                let required = if truthy(&f["required"]) {
                    ", required"
                } else {
                    ""
                };
                format!(
                    "{} \"{}\" ({}{options}{required})",
                    s(&f["id"]),
                    s(&f["label"]),
                    s(&f["type"])
                )
            }),
        ),
    ];
    let values = &spec["values"];
    let answered: Vec<&Value> = fields
        .iter()
        .filter(|f| values.get(s(&f["id"])).is_some())
        .collect();
    if !answered.is_empty() {
        lines.push(listed(
            if submitted { "Answers" } else { "Prefilled" },
            answered
                .iter()
                .map(|f| format!("{} = {}", s(&f["id"]), shown_value(f, &values[s(&f["id"])]))),
        ));
    }
    lines.push(if submitted {
        "The form is read-only now; update it to ask again (an update reopens it).".into()
    } else {
        format!(
            "When the user submits, the answers arrive as a \"{EVENT}\" message (op \"submit\"); wait for it."
        )
    });
    lines.join("\n")
}

/// A checklist's steps (items without sub-items), the first pending one
/// and the running one.
struct Plan<'a> {
    steps: Vec<&'a Value>,
    next: Option<&'a Value>,
    running: Option<&'a Value>,
}

fn status_of(item: &Value) -> &str {
    match item["status"].as_str() {
        Some(status) => status,
        None if truthy(&item["done"]) => "done",
        None => "pending",
    }
}

fn all_items(items: &[Value]) -> Vec<&Value> {
    items
        .iter()
        .flat_map(|top| std::iter::once(top).chain(arr(&top["items"])))
        .collect()
}

fn plan(items: &[Value]) -> Plan<'_> {
    let all = all_items(items);
    let steps: Vec<&Value> = all
        .iter()
        .copied()
        .filter(|item| arr(&item["items"]).is_empty())
        .collect();
    Plan {
        next: steps.iter().copied().find(|i| status_of(i) == "pending"),
        running: all.iter().copied().find(|i| status_of(i) == "running"),
        steps,
    }
}

impl Plan<'_> {
    /// 1-based, among the steps.
    fn number(&self, item: &Value) -> usize {
        self.steps
            .iter()
            .position(|step| std::ptr::eq(*step, item))
            .map_or(0, |at| at + 1)
    }
}

fn checklist(surface: &Surface) -> String {
    let items = arr(&surface.spec["items"]);
    let plan = plan(items);
    let all = all_items(items);
    let done = all.iter().filter(|i| truthy(&i["done"])).count();
    let mut lines = vec![format!(
        "{} is shown to the user ({done} of {} done).",
        head(surface, "Checklist"),
        all.len()
    )];
    let line = |item: &Value, indent: &str| {
        let status = status_of(item);
        let word = match status {
            "done" | "pending" => String::new(),
            other => format!(" ({other})"),
        };
        let mark = match status {
            "pending" => " ",
            "done" => "x",
            "running" => "~",
            "failed" => "!",
            "skipped" => "-",
            _ => " ",
        };
        let note = if truthy(&item["note"]) {
            format!(" — {}", cut(&js_string(&item["note"]), 80))
        } else {
            String::new()
        };
        format!(
            "{indent}- [{mark}] {} {}{word}{note}",
            s(&item["id"]),
            quoted(&item["label"], 80)
        )
    };
    for top in items {
        lines.push(line(top, ""));
        for sub in arr(&top["items"]) {
            lines.push(line(sub, "  "));
        }
    }
    limit(&mut lines, "items");
    match surface.spec["run"].as_str() {
        None => lines.push(format!(
            "The user can check, add, edit, remove and reorder items; their edits arrive as \"{EVENT}\" messages."
        )),
        Some(run) => lines.push(run_next(run, &plan)),
    }
    lines.join("\n")
}

/// What a run plan expects of the agent now.
fn run_next(run: &str, plan: &Plan) -> String {
    let name = |item: &Value| {
        format!(
            "step {} {} ({})",
            plan.number(item),
            quoted(&item["label"], 80),
            s(&item["id"])
        )
    };
    let report = "report it with gentle_ui update {patch: {items: [{id, status: \"done\"}]}} (or \"failed\" with a note saying why)";
    if let Some(running) = plan.running {
        return format!(
            "Run plan: {} is running: when it is through, {report}{}",
            name(running),
            if run == "all" {
                ", then go on with the next pending step."
            } else {
                ", then stop and wait for the user."
            }
        );
    }
    if run == "all"
        && let Some(next) = plan.next
    {
        return format!(
            "Run plan: the user is running it. Do {} next: first update it to status \"running\", do it, then {report}; then continue with the next pending step.",
            name(next)
        );
    }
    format!(
        "Run plan: the user skips and reorders steps and presses Run or Run next; wait for their \"{EVENT}\" saying which step to do, and report each step's status with update patches."
    )
}

/// What an event asks of the agent beyond the edits, by kind.
pub fn instructions(after: &Surface, changes: &[Value]) -> Vec<String> {
    match after.kind.as_str() {
        "checklist" => checklist_instructions(after, changes),
        "kanban" => kanban_instructions(after, changes),
        _ => Vec::new(),
    }
}

/// The checklist events that drive the agent: run, stop, run plan on.
fn checklist_instructions(after: &Surface, changes: &[Value]) -> Vec<String> {
    let plan = plan(arr(&after.spec["items"]));
    let mut lines = Vec::new();
    for change in changes.iter().filter(|c| c.is_object()) {
        match change["op"].as_str() {
            Some("run") => {
                let Some(item) = plan
                    .steps
                    .iter()
                    .copied()
                    .find(|step| step["id"] == change["item"])
                else {
                    continue;
                };
                let id = s(&item["id"]);
                lines.push(format!(
                    "Do step {} next: {} ({id}); it is marked running. Then report it with gentle_ui update {{patch: {{items: [{{id: \"{id}\", status: \"done\"}}]}}}} (or \"failed\" with a note){}",
                    plan.number(item),
                    quoted(&item["label"], 120),
                    if change["mode"] == "all" {
                        ", and go on through the next pending steps the same way (mark each running, do it, report it) until none is pending or the user stops the plan."
                    } else {
                        ", then stop and wait for the user."
                    }
                ));
            }
            Some("stop") => lines.push(
                "The user stopped the plan: don't start another step. If one is running, finish or abandon it and report its status."
                    .into(),
            ),
            Some("run_mode") if change["on"] == true => lines.push(
                "The user turned this checklist into a run plan: wait for Run or Run next before doing a step."
                    .into(),
            ),
            _ => {}
        }
    }
    lines
}

fn kanban(surface: &Surface) -> String {
    let spec = &surface.spec;
    let (columns, cards) = (arr(&spec["columns"]), arr(&spec["cards"]));
    let mut lines = vec![format!(
        "{} is shown to the user ({} in {}).",
        head(surface, "Board"),
        plural(cards.len(), "card"),
        plural(columns.len(), "column")
    )];
    for column in columns {
        let in_column: Vec<&Value> = cards
            .iter()
            .filter(|c| c["column"] == column["id"])
            .collect();
        let count = if truthy(&column["limit"]) {
            format!("{}/{}", in_column.len(), js_string(&column["limit"]))
        } else {
            in_column.len().to_string()
        };
        let shown = in_column.iter().map(|c| {
            let assignee = if truthy(&c["assignee"]) {
                format!(" @{}", s(&c["assignee"]))
            } else {
                String::new()
            };
            let tags = match c["tags"].as_array() {
                Some(tags) => format!(
                    " [{}]",
                    tags.iter().map(js_string).collect::<Vec<_>>().join(", ")
                ),
                None => String::new(),
            };
            let agent = if truthy(&c["agentId"]) {
                format!(" [subagent {}]", js_string(&c["agentId"]))
            } else if c["agent"].is_object() {
                let name = c["agent"]["name"].as_str().unwrap_or("a subagent");
                format!(" [for {name}]")
            } else {
                String::new()
            };
            format!(
                "{} {}{assignee}{tags}{agent}",
                s(&c["id"]),
                quoted(&c["title"], 60)
            )
        });
        let starts = if truthy(&column["starts"]) {
            ", dropping a card here starts a subagent"
        } else {
            ""
        };
        let line = listed(
            &format!(
                "{} ({}, {count}{starts})",
                s(&column["label"]),
                s(&column["id"])
            ),
            shown,
        );
        lines.push(line.strip_suffix(": ").map(str::to_string).unwrap_or(line));
    }
    lines.push(format!(
        "The user can drag cards between and within columns, add, edit and remove them; their edits arrive as \"{EVENT}\" messages."
    ));
    lines.join("\n")
}

/// A card dropped into a `starts` column: dispatch a subagent for it (the
/// app can't start one: Desktop RPC has no start method by design).
fn kanban_instructions(after: &Surface, changes: &[Value]) -> Vec<String> {
    let cards = arr(&after.spec["cards"]);
    changes
        .iter()
        .filter(|c| c.is_object() && c["op"] == "dispatch")
        .filter_map(|change| {
            let card = cards.iter().find(|c| c["id"] == change["card"])?;
            let agent = &card["agent"];
            let task = match agent["task"].as_str() {
                Some(task) => task.to_string(),
                None if truthy(&card["description"]) => {
                    format!("{}: {}", js_string(&card["title"]), js_string(&card["description"]))
                }
                None => js_string(&card["title"]),
            };
            let using = match agent["name"].as_str() {
                Some(name) => format!(" using the {name} agent"),
                None => String::new(),
            };
            let id = s(&card["id"]);
            Some(format!(
                "Dispatch a subagent for the card {} ({id}){using} with this task: {task}. Use your own subagent tool; once it is dispatched, call gentle_ui update with patch {{cards: [{{id: \"{id}\", agentId: \"<the subagent's id>\"}}]}} so the card follows its live status.",
                quoted(&card["title"], 80)
            ))
        })
        .collect()
}

fn chart(surface: &Surface) -> String {
    let spec = &surface.spec;
    let labels = spec["labels"].as_array();
    let count = match labels {
        Some(labels) => format!(" over {}", plural(labels.len(), "label")),
        None => String::new(),
    };
    let opt = |key: &str, before: &str| {
        if truthy(&spec[key]) {
            format!("{before}{}", s(&spec[key]))
        } else {
            String::new()
        }
    };
    let series = arr(&spec["series"]);
    let mut lines = vec![format!(
        "{} is shown to the user: {} chart, {} series{count}{}{}{}.",
        head(surface, "Chart"),
        s(&spec["type"]),
        series.len(),
        opt("unit", " in "),
        opt("yTitle", ", y: "),
        opt("xTitle", ", x: ")
    )];
    if let Some(labels) = labels {
        lines.push(listed("Labels", labels.iter().map(js_string)));
    }
    for one in series {
        let values: Vec<String> = match one["points"].as_array() {
            Some(points) => points
                .iter()
                .map(|p| format!("({}, {})", js_string(&p[0]), js_string(&p[1])))
                .collect(),
            None => arr(&one["values"]).iter().map(js_string).collect(),
        };
        lines.push(listed(&js_string(&one["name"]), values));
    }
    lines.push(
        "It is read-only; the user can hover values and ask about one in their next message."
            .into(),
    );
    lines.join("\n")
}

const CSP_KEYS: [&str; 4] = [
    "connectDomains",
    "resourceDomains",
    "frameDomains",
    "baseUriDomains",
];

fn html(surface: &Surface) -> String {
    let spec = &surface.spec;
    let size = spec["html"]
        .as_str()
        .unwrap_or_default()
        .len()
        .div_ceil(1024);
    let mut origins: Vec<String> = Vec::new();
    for key in CSP_KEYS {
        for origin in arr(&spec["csp"][key]) {
            let origin = js_string(origin);
            if !origins.contains(&origin) {
                origins.push(origin);
            }
        }
    }
    let reach = if origins.is_empty() {
        "no network".to_string()
    } else {
        format!("may reach {}", origins.join(", "))
    };
    let data: Vec<String> = spec["data"]
        .as_object()
        .map(|data| data.keys().cloned().collect())
        .unwrap_or_default();
    [
        format!(
            "{} is shown to the user as an isolated MCP App page ({size} KiB, {reach}).",
            head(surface, "HTML UI")
        ),
        if data.is_empty() {
            "No data: the page gets an empty tool input.".into()
        } else {
            listed("Data keys (the page's tool input and structuredContent)", data)
        },
        format!(
            "The user opens it from the conversation. Messages it sends arrive as \"{EVENT}\" messages; context it shares arrives with the next turn."
        ),
    ]
    .join("\n")
}

fn hunk_counts(hunk: &Value) -> (usize, usize) {
    let lines = arr(&hunk["lines"]);
    (
        lines.iter().filter(|l| l["kind"] != "add").count(),
        lines.iter().filter(|l| l["kind"] != "del").count(),
    )
}

fn hunk_ref(file: &Value, hunk: &Value) -> String {
    let (old, new) = hunk_counts(hunk);
    let header = if truthy(&hunk["header"]) {
        format!(" {}", s(&hunk["header"]))
    } else {
        String::new()
    };
    format!(
        "{}: {} @@ -{},{old} +{},{new} @@{header}",
        s(&hunk["id"]),
        s(&file["path"]),
        js_string(&hunk["oldStart"]),
        js_string(&hunk["newStart"])
    )
}

pub(super) fn review(surface: &Surface) -> String {
    let files = arr(&surface.spec["files"]);
    let hunks: Vec<&Value> = files.iter().flat_map(|f| arr(&f["hunks"])).collect();
    let all = |kind: &str| -> usize {
        hunks
            .iter()
            .map(|h| {
                arr(&h["lines"])
                    .iter()
                    .filter(|l| l["kind"] == kind)
                    .count()
            })
            .sum()
    };
    let decided = hunks.iter().filter(|h| !h["decision"].is_null()).count();
    let mut lines = vec![format!(
        "{} is shown to the user: {}, +{} −{}, {decided} of {} decided.",
        head(surface, "Review"),
        plural(files.len(), "file"),
        all("add"),
        all("del"),
        plural(hunks.len(), "hunk")
    )];
    if truthy(&surface.spec["summary"]) {
        lines.push(cut(s(&surface.spec["summary"]), 200));
    }
    for file in files {
        let from = if truthy(&file["oldPath"]) {
            format!(" from {}", s(&file["oldPath"]))
        } else {
            String::new()
        };
        lines.push(format!(
            "- {} ({}{from})",
            s(&file["path"]),
            s(&file["status"])
        ));
        for hunk in arr(&file["hunks"]) {
            let state = if truthy(&hunk["applied"]) {
                "applied"
            } else {
                hunk["decision"].as_str().unwrap_or("undecided")
            };
            let comment = if truthy(&hunk["comment"]) {
                format!(" — comment: {}", quoted(&hunk["comment"], 120))
            } else {
                String::new()
            };
            lines.push(format!("  - [{state}] {}{comment}", hunk_ref(file, hunk)));
        }
    }
    limit(&mut lines, "hunks");
    let unapplied = hunks
        .iter()
        .filter(|h| h["decision"] == "accepted" && !truthy(&h["applied"]))
        .count();
    lines.push(if decided < hunks.len() {
        format!(
            "The user decides each hunk (accept, reject, comment); decisions arrive as a \"{EVENT}\" message (op decide). Write no undecided or rejected hunk; apply only accepted ones."
        )
    } else if unapplied > 0 {
        "Every hunk is decided: apply only the accepted ones, then mark them with update patch {applied: [hunk ids]}.".into()
    } else {
        "Every hunk is decided and every accepted one is applied.".into()
    });
    lines.join("\n")
}

/// What the agent is told about a review's newly sent decisions.
pub fn review_decisions(surface: &Surface, changes: &[Value], note: Option<&str>) -> String {
    let files = arr(&surface.spec["files"]);
    let placed: Vec<(&Value, &Value)> = files
        .iter()
        .flat_map(|file| arr(&file["hunks"]).iter().map(move |hunk| (file, hunk)))
        .collect();
    let refer = |id: &str| {
        placed
            .iter()
            .find(|(_, hunk)| hunk["id"] == id)
            .map_or_else(|| id.to_string(), |(file, hunk)| hunk_ref(file, hunk))
    };
    let decided: Vec<&Value> = changes
        .iter()
        .filter(|c| c.is_object() && c["op"] == "decide" && c["hunk"].is_string())
        .collect();
    let pick = |decision: &str| -> Vec<&Value> {
        decided
            .iter()
            .copied()
            .filter(|c| c["decision"] == decision)
            .collect()
    };
    let accepted = pick("accepted");
    let rejected = pick("rejected");
    let commented: Vec<&Value> = decided
        .iter()
        .copied()
        .filter(|c| c["comment"].as_str().is_some_and(|c| !c.is_empty()))
        .collect();
    let undecided: Vec<String> = placed
        .iter()
        .filter(|(_, hunk)| hunk["decision"].is_null() && !truthy(&hunk["applied"]))
        .map(|(_, hunk)| js_string(&hunk["id"]))
        .collect();
    let mut lines = vec![format!(
        "The user reviewed the changes in \"{}\" ({}, now v{}):",
        surface.title, surface.id, surface.version
    )];
    if !accepted.is_empty() {
        lines.push("Accepted: apply exactly these hunks as proposed:".into());
        lines.extend(
            accepted
                .iter()
                .map(|c| format!("- {}", refer(s(&c["hunk"])))),
        );
    }
    if !rejected.is_empty() {
        lines.push("Rejected: do NOT apply these hunks:".into());
        lines.extend(
            rejected
                .iter()
                .map(|c| format!("- {}", refer(s(&c["hunk"])))),
        );
    }
    if !commented.is_empty() {
        lines.push("Comments to address (on an accepted hunk, apply it as the comment asks; on a rejected one, it says why or what to do instead; on an undecided one, answer it or revise the hunk with update):".into());
        lines.extend(commented.iter().map(|c| {
            format!(
                "- {} ({}): {}",
                s(&c["hunk"]),
                c["decision"].as_str().unwrap_or("undecided"),
                quoted(&c["comment"], 400)
            )
        }));
    }
    if !undecided.is_empty() {
        lines.push(listed(
            "Still undecided (don't apply these yet; decisions on them arrive later)",
            undecided,
        ));
    }
    if let Some(note) = note {
        lines.push(format!("Note from the user: {note}"));
    }
    if !accepted.is_empty() {
        let applied: Vec<&Value> = accepted.iter().map(|c| &c["hunk"]).collect();
        lines.push(format!(
            "After applying, call gentle_ui update with {} so the user sees them applied.",
            json!({"action": "update", "id": surface.id, "patch": {"applied": applied}})
        ));
    }
    lines.push(format!("Changes: {}", Value::Array(changes.to_vec())));
    lines.join("\n")
}

fn sequence(surface: &Surface) -> String {
    let spec = &surface.spec;
    let (participants, messages, fragments) = (
        arr(&spec["participants"]),
        arr(&spec["messages"]),
        arr(&spec["fragments"]),
    );
    let who = |id: &Value| {
        participants
            .iter()
            .find(|p| &p["id"] == id)
            .and_then(|p| p["label"].as_str())
            .map_or_else(|| js_string(id), str::to_string)
    };
    let mut lines = vec![
        format!(
            "{} is shown to the user ({}, {}, {}).",
            head(surface, "Sequence diagram"),
            plural(participants.len(), "participant"),
            plural(messages.len(), "message"),
            plural(fragments.len(), "fragment")
        ),
        listed(
            "Participants, left to right",
            participants.iter().map(|p| {
                format!(
                    "{} {} ({})",
                    s(&p["id"]),
                    quoted(&p["label"], 40),
                    s(&p["kind"])
                )
            }),
        ),
    ];
    for (i, m) in messages.iter().enumerate() {
        let label = if truthy(&m["label"]) {
            format!(" {}", quoted(&m["label"], 60))
        } else {
            String::new()
        };
        let kind = match s(&m["kind"]) {
            "sync" => String::new(),
            kind => format!(" [{kind}]"),
        };
        let note = if truthy(&m["note"]) {
            format!(" — {}", cut(&js_string(&m["note"]), 80))
        } else {
            String::new()
        };
        lines.push(format!(
            "{}. {} {} → {}{label}{kind}{note}",
            i + 1,
            s(&m["id"]),
            who(&m["from"]),
            who(&m["to"])
        ));
    }
    for f in fragments {
        let branches: String = arr(&f["else"])
            .iter()
            .map(|b| {
                let label = if truthy(&b["label"]) {
                    format!(" {}", quoted(&b["label"], 40))
                } else {
                    String::new()
                };
                format!(" else at {}{label}", js_string(&b["at"]))
            })
            .collect::<Vec<_>>()
            .join(",");
        let label = if truthy(&f["label"]) {
            format!(" {}", quoted(&f["label"], 40))
        } else {
            String::new()
        };
        lines.push(format!(
            "Fragment {} {}{label}: {}–{}{branches}",
            s(&f["id"]),
            s(&f["kind"]),
            js_string(&f["from"]),
            js_string(&f["to"])
        ));
    }
    limit(&mut lines, "lines");
    lines.push(format!(
        "The user can reorder participants and messages, rename them, add and remove messages and change a message's kind; their edits arrive as \"{EVENT}\" messages."
    ));
    lines.join("\n")
}

fn erd(surface: &Surface) -> String {
    let spec = &surface.spec;
    let (tables, relations) = (arr(&spec["tables"]), arr(&spec["relations"]));
    let mut lines = vec![format!(
        "{} is shown to the user ({}, {}).",
        head(surface, "Data model"),
        plural(tables.len(), "table"),
        plural(relations.len(), "relation")
    )];
    for t in tables {
        let columns: Vec<String> = arr(&t["columns"])
            .iter()
            .map(|c| {
                let fk = if c["fk"].is_object() {
                    format!(
                        "FK→{}.{}",
                        js_string(&c["fk"]["table"]),
                        js_string(&c["fk"]["column"])
                    )
                } else {
                    String::new()
                };
                [
                    js_string(&c["name"]),
                    c["type"].as_str().unwrap_or_default().to_string(),
                    if truthy(&c["pk"]) {
                        "PK".into()
                    } else {
                        String::new()
                    },
                    fk,
                    if truthy(&c["unique"]) {
                        "unique".into()
                    } else {
                        String::new()
                    },
                    if truthy(&c["nullable"]) {
                        "null".into()
                    } else {
                        String::new()
                    },
                ]
                .into_iter()
                .filter(|part| !part.is_empty())
                .collect::<Vec<_>>()
                .join(" ")
            })
            .collect();
        let reference = if t["ref"].is_object() {
            let r = &t["ref"];
            let lines = if truthy(&r["line"]) {
                let end = if truthy(&r["endLine"]) {
                    format!("-{}", js_string(&r["endLine"]))
                } else {
                    String::new()
                };
                format!(":{}{end}", js_string(&r["line"]))
            } else {
                String::new()
            };
            format!(" @ {}{lines}", js_string(&r["path"]))
        } else {
            String::new()
        };
        let name = if t["name"] != t["id"] {
            format!(" {}", quoted(&t["name"], 40))
        } else {
            String::new()
        };
        let columns = if columns.is_empty() {
            "no columns".to_string()
        } else {
            columns.join(", ")
        };
        lines.push(format!("{}{name}{reference}: {columns}", s(&t["id"])));
    }
    if !relations.is_empty() {
        lines.push(listed(
            "Relations",
            relations.iter().map(|r| {
                let label = if truthy(&r["label"]) {
                    format!(" {}", quoted(&r["label"], 40))
                } else {
                    String::new()
                };
                format!(
                    "{} → {} ({}){label}",
                    js_string(&r["from"]),
                    js_string(&r["to"]),
                    js_string(&r["cardinality"])
                )
            }),
        ));
    }
    limit(&mut lines, "tables");
    lines.push(format!(
        "The user can move and rename tables, add, rename and retype columns and relate columns; their edits arrive as \"{EVENT}\" messages."
    ));
    lines.join("\n")
}

fn state(surface: &Surface) -> String {
    let spec = &surface.spec;
    let (states, transitions, groups) = (
        arr(&spec["states"]),
        arr(&spec["transitions"]),
        arr(&spec["groups"]),
    );
    let current = if truthy(&spec["current"]) {
        format!(", current: {}", js_string(&spec["current"]))
    } else {
        String::new()
    };
    let mut lines = vec![format!(
        "{} is shown to the user ({}, {}{current}).",
        head(surface, "State machine"),
        plural(states.len(), "state"),
        plural(transitions.len(), "transition")
    )];
    if !groups.is_empty() {
        lines.push(listed(
            "Composite states",
            groups
                .iter()
                .map(|g| format!("{} {}", s(&g["id"]), quoted(&g["label"], 40))),
        ));
    }
    lines.push(listed(
        "States",
        states.iter().map(|st| {
            let label = if st["label"] != st["id"] {
                format!(" {}", quoted(&st["label"], 40))
            } else {
                String::new()
            };
            let kind = match s(&st["kind"]) {
                "normal" => String::new(),
                kind => format!(" ({kind})"),
            };
            let group = if truthy(&st["group"]) {
                format!(" in {}", s(&st["group"]))
            } else {
                String::new()
            };
            format!("{}{label}{kind}{group}", s(&st["id"]))
        }),
    ));
    let label = |t: &Value| {
        let mut parts = Vec::new();
        if truthy(&t["event"]) {
            parts.push(js_string(&t["event"]));
        }
        if truthy(&t["guard"]) {
            parts.push(format!("[{}]", js_string(&t["guard"])));
        }
        if truthy(&t["action"]) {
            parts.push(format!("/ {}", js_string(&t["action"])));
        }
        parts.join(" ")
    };
    if !transitions.is_empty() {
        lines.push(listed(
            "Transitions",
            transitions.iter().map(|t| {
                let text = label(t);
                let text = if text.is_empty() {
                    String::new()
                } else {
                    format!(" {}", quoted(&json!(text), 60))
                };
                format!("{} → {}{text}", s(&t["from"]), s(&t["to"]))
            }),
        ));
    }
    lines.push(format!(
        "The user can move, rename, add and delete states and transitions, change a state's kind and relabel transitions; their edits arrive as \"{EVENT}\" messages."
    ));
    lines.join("\n")
}

fn timeline<'a>(surface: &'a Surface) -> String {
    let tasks = arr(&surface.spec["tasks"]);
    let day = |t: &Value, key: &str| day_of(s(&t[key])).unwrap_or_default();
    let span = if tasks.is_empty() {
        String::new()
    } else {
        let mut starts: Vec<&str> = tasks.iter().map(|t| s(&t["start"])).collect();
        let mut ends: Vec<&str> = tasks.iter().map(|t| s(&t["end"])).collect();
        starts.sort_unstable();
        ends.sort_unstable();
        format!(", {} to {}", starts[0], ends[ends.len() - 1])
    };
    let mut lines = vec![format!(
        "{} is shown to the user ({}{span}).",
        head(surface, "Timeline"),
        plural(tasks.len(), "task")
    )];
    for t in tasks {
        let span = if truthy(&t["milestone"]) {
            format!("{} (milestone)", s(&t["start"]))
        } else {
            let days = (day(t, "end") - day(t, "start") + 1).max(0) as usize;
            format!(
                "{} → {} ({})",
                s(&t["start"]),
                s(&t["end"]),
                plural(days, "day")
            )
        };
        let progress = if t["progress"].is_null() {
            String::new()
        } else {
            format!(", {}%", js_string(&t["progress"]))
        };
        let group = if truthy(&t["group"]) {
            format!(" [{}]", s(&t["group"]))
        } else {
            String::new()
        };
        let deps = match t["dependsOn"].as_array() {
            Some(deps) => format!(
                " after {}",
                deps.iter().map(js_string).collect::<Vec<_>>().join(", ")
            ),
            None => String::new(),
        };
        lines.push(format!(
            "- {} {}: {span}{progress}{group}{deps}",
            s(&t["id"]),
            quoted(&t["label"], 60)
        ));
    }
    limit(&mut lines, "tasks");
    // Critical path: from the task ending last back through the dependency ending last.
    let by_id = |id: &Value| tasks.iter().find(|t| &t["id"] == id);
    // The first task ending last.
    let latest = |list: Vec<&'a Value>| -> Option<&'a Value> {
        list.into_iter()
            .fold(None, |best: Option<&'a Value>, t| match best {
                Some(best) if day(t, "end") <= day(best, "end") => Some(best),
                _ => Some(t),
            })
    };
    let mut at = latest(tasks.iter().collect());
    let mut chain: Vec<String> = Vec::new();
    while let Some(task) = at {
        let id = js_string(&task["id"]);
        if chain.contains(&id) {
            break;
        }
        chain.insert(0, id);
        at = latest(arr(&task["dependsOn"]).iter().filter_map(by_id).collect());
    }
    if chain.len() > 1 {
        lines.push(format!("Critical path: {}", chain.join(" → ")));
    }
    let late: Vec<String> = tasks
        .iter()
        .flat_map(|t| {
            arr(&t["dependsOn"]).iter().filter_map(move |d| {
                let dep = by_id(d)?;
                let gap = if truthy(&dep["milestone"]) { 0 } else { 1 };
                (day(t, "start") < day(dep, "end") + gap)
                    .then(|| format!("{} starts before {} ends", s(&t["id"]), js_string(d)))
            })
        })
        .collect();
    if !late.is_empty() {
        lines.push(listed("Late", late));
    }
    lines.push(format!(
        "The user can drag tasks to move or resize them, link and unlink dependencies, rename, add and remove tasks; their edits arrive as \"{EVENT}\" messages."
    ));
    lines.join("\n")
}

/// The text of a page's `ui/update-model-context` change.
pub fn context_text(change: &Value) -> String {
    let mut parts: Vec<String> = arr(&change["content"])
        .iter()
        .filter(|b| b.is_object())
        .map(|b| match b["text"].as_str() {
            Some(text) if b["type"] == "text" => text.to_string(),
            _ => format!("[{} omitted]", js_string(&b["type"])),
        })
        .collect();
    if change["structuredContent"].is_object() {
        parts.push(change["structuredContent"].to_string());
    }
    parts.join("\n")
}
