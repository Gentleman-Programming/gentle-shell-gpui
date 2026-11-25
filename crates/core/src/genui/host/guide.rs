//! What the agent is told about `gentle_ui`: the extension's description,
//! parameters and rules, per Agent UI mode. A host tool's guidelines are
//! capped (at most [`MAX_GUIDELINES`] of [`MAX_GUIDELINE_CHARS`] each), so
//! the rules are packed into that many at sentence boundaries and the
//! worked examples go with the `spec` parameter's description.

use serde_json::{Value, json};

use super::super::style::{ICONS, Tint};
use super::{EVENT, Mode};

/// The Desktop RPC caps on one host tool's guidelines.
pub const MAX_GUIDELINES: usize = 16;
pub const MAX_GUIDELINE_CHARS: usize = 512;

pub const LABEL: &str = "Gentle UI";

pub const DESCRIPTION: &str = "Show an interactive UI in the user's Gentle Shell app conversation (a diagram, sequence diagram, data model, state machine, timeline, table, form, checklist, kanban board, chart, change review or html page), update it (spec replaces, patch merges) or close it. The user can edit or use it (charts are read-only); the result summarizes its current state.";

/// What the model is told while the tool is off.
pub const OFF_RULE: &str = "The user turned Agent UI off: gentle_ui is unavailable. Answer in text; gentle_ui surfaces earlier in the conversation are read-only for you.";
/// Added to what an event says while the tool is off.
pub const OFF_NOTE: &str = "Agent UI is off in the app's settings: gentle_ui is unavailable, so don't try to update this surface; answer in text.";
/// The error a call gets while the tool is off.
pub const OFF_ERROR: &str = "gentle_ui is off: the user turned it off in the app (Configuration › Agent UI). Answer in text instead.";

const NODE_KINDS: &str = "service|database|queue|cache|client|external|storage|function|note";
const STATUSES: &str = "planned|existing|deprecated|risk";
const EDGE_STYLES: &str = "sync|async|data|dependency";
const COLUMN_TYPES: &str = "text|number|date|status|link";
const FIELD_TYPES: &str = "text|textarea|number|select|multiselect|checkbox|radio|slider|date";
const CHART_TYPES: &str = "bar|line|area|pie|donut|scatter";
const LIVE_SOURCES: &str = "jobs|subagents|git:status|git:log|todo";
const PARTICIPANT_KINDS: &str = "actor|service|db|queue|external";
const MESSAGE_KINDS: &str = "sync|async|return|self";
const FRAGMENT_KINDS: &str = "alt|opt|loop|par";
const STATE_KINDS: &str = "initial|normal|final|choice";
pub const KINDS: [&str; 12] = [
    "diagram",
    "table",
    "form",
    "checklist",
    "kanban",
    "chart",
    "html",
    "review",
    "sequence",
    "erd",
    "state",
    "timeline",
];

/// The policy each mode puts first among the tool's rules.
fn policy(mode: Mode) -> &'static [&'static str] {
    match mode {
        Mode::Proactive => &[
            "Use gentle_ui on your own initiative, without being asked, whenever a surface clearly beats prose: a system with 4+ interacting parts you explain or propose → diagram (with refs to the code); calls between components over time → sequence; a schema or migration → erd; a workflow or lifecycle → state; dated work with dependencies → timeline; a plan of 4+ steps or work you are about to execute → checklist with run: true; 3+ options or items compared on 2+ attributes → table; 2+ decisions you need from the user at once → one form instead of a string of questions; numbers over time or by category → chart; multi-file or risky edits → review before writing them.",
            "Don't use gentle_ui for simple answers, one-liners, small edits, when the user wants text only, or to decorate. When a gentle_ui surface for the same thing is already open, update it instead of showing another. At most one new gentle_ui surface per reply unless the user asks for more; keep the prose around it short.",
        ],
        Mode::WhenAsked => &[
            "The user wants gentle_ui surfaces only on request: use gentle_ui only when they ask for one (a diagram, table, form, chart, review… or to see, map, draw or plan something visually), or to update a surface already open; otherwise answer in text.",
        ],
        Mode::Off => &[],
    }
}

/// How to use the tool, in every mode it is on (the examples are in
/// [`parameters`]).
fn rules() -> Vec<String> {
    vec![
        "Pick the gentle_ui kind by what the user will do: diagram for architecture, data flow and dependencies; sequence for calls between components over time; erd for data models (read from the migrations or ORM models); state for state machines and lifecycles (current: the live state); timeline for dated plans with dependencies; table for rows to compare, select or correct; form to ask several structured questions at once; checklist for plans and TODOs; kanban for work items across stages; chart for numbers (read-only); review for file changes you propose.".into(),
        "The user is in the Gentle Shell desktop app, where gentle_ui surfaces appear right in the conversation and they can move, connect and correct them. When they ask to see, map, draw or plan an architecture, system or flow, use gentle_ui, not gentle_design: gentle_design is for HTML design canvases (screens, mockups, visual directions) opened in a browser.".into(),
        "To change a UI already shown, call gentle_ui with action update and a small patch instead of a new show; ids are stable slugs (api, orders-db) reused across updates.".into(),
        "Attach ref { path (repo-relative), line?, endLine? } to every gentle_ui item that corresponds to code (a diagram node, erd table, table row, checklist item or kanban card) so the user can open it, e.g. \"ref\":{\"path\":\"src/auth/mod.rs\",\"line\":42}; paths outside the workspace are rejected.".into(),
        format!("The user's edits to a gentle_ui UI arrive as \"{EVENT}\" messages with the full current spec; they are authoritative, so build on them and don't undo them. Such a message may also insert a saved UI (treat it like your own, update it by id) or ask you to add one to the project's docs."),
        "In gentle_ui specs omit x/y and let auto-layout place things; keep labels short and put details in description, tech or note.".into(),
        "In a gentle_ui diagram give every node a one-line description of what it does (it shows on the node's card) and its tech when known; label every edge with a short verb phrase saying what flows (\"Sends emails to users\", \"Reads orders\"), not a single word; use kind client for people and apps that start the flow and external for systems you don't own.".into(),
        "A gentle_ui form's answers arrive once, when the user submits (op submit); wait for them instead of guessing. To ask again or change the questions, update the form: an update reopens it.".into(),
        "A gentle_ui checklist with run: true is a plan the user runs (Run, Run next, skip, reorder); an event tells you which step to do. Before a step update it to status running, after it to done or failed (with a note). After Run go on through the pending steps; after Run next stop after that step; after Stop start no new step.".into(),
        "A gentle_ui kanban column with starts: true dispatches work: a card dragged into it asks you to dispatch a subagent (card.agent {name, task} says which and what) with your own subagent tool; then update the card with agentId so it shows the subagent's live status and the user can stop it.".into(),
        "gentle_ui tables, charts and checklists can be live: spec.live {source: jobs|subagents|git:status|git:log|todo} or {command: {cmd, cwd?, parse}, interval} with a map of JSONPath-lite picks. The app keeps them current itself (commands only once the user allows them), so don't push rows or values to a live UI.".into(),
        "A gentle_ui review (before every edit when the user asked for review mode) gives per file hunks (lines \"+…\", \"-…\", \" …\" with oldStart/newStart) or before/after texts. Write none of it until the decisions arrive, then apply ONLY the accepted hunks (as proposed or as their comments ask), never rejected or undecided ones, and update with patch {applied: [hunk ids]}. Use wait: true only when you can't continue without the decisions (the call returns them); otherwise end your turn after showing it.".into(),
        "Use gentle_ui kind html only for custom interactive visuals, simulations, calculators or working prototypes no other kind fits (design screens and mockups stay in gentle_design). The page is an MCP App: self-contained (inline JS/CSS, no network unless spec.csp lists origins), put changing values in spec.data and update them with patch {data}.".into(),
        "A gentle_ui html page talks JSON-RPC 2.0 over window.parent.postMessage(msg, \"*\"): request ui/initialize {protocolVersion:\"2026-01-26\",appInfo:{name,version},appCapabilities:{}} (result.hostContext.styles.variables: theme CSS vars like --color-background-primary, --color-text-primary, --color-ring-primary, --font-sans; apply them), then notify ui/notifications/initialized; it receives ui/notifications/tool-result {structuredContent: spec.data}. Request ui/message {role:\"user\",content:[{type:\"text\",text}]} to message you, ui/update-model-context {content|structuredContent} to share state, ui/open-link {url}; notify ui/notifications/size-changed {height}.".into(),
    ]
}

/// The tool's rules in `mode` (none when off: the tool isn't offered),
/// packed into the host tool caps.
pub fn guidelines(mode: Mode) -> Vec<String> {
    if mode == Mode::Off {
        return Vec::new();
    }
    let mut lines: Vec<String> = policy(mode).iter().map(|l| l.to_string()).collect();
    lines.extend(rules());
    pack(&lines, MAX_GUIDELINE_CHARS)
}

/// `lines` joined into as few as fit `max` characters each: whole lines
/// first, a longer one split after its sentences (`. `, `; `, `: `).
fn pack(lines: &[String], max: usize) -> Vec<String> {
    let mut pieces: Vec<String> = Vec::new();
    for line in lines {
        let mut rest = line.as_str();
        while rest.chars().count() > max {
            let limit = rest
                .char_indices()
                .nth(max)
                .map_or(rest.len(), |(at, _)| at);
            let cut = [". ", "; ", ": ", ", "]
                .iter()
                .find_map(|sep| rest[..limit].rfind(sep).map(|at| at + sep.len() - 1))
                .unwrap_or(limit);
            pieces.push(rest[..cut].trim_end().to_string());
            rest = rest[cut..].trim_start();
        }
        pieces.push(rest.to_string());
    }
    let mut packed: Vec<String> = Vec::new();
    for piece in pieces {
        match packed.last_mut() {
            Some(last) if last.chars().count() + 1 + piece.chars().count() <= max => {
                last.push(' ');
                last.push_str(&piece);
            }
            _ => packed.push(piece),
        }
    }
    packed
}

fn colors() -> String {
    Tint::ALL
        .iter()
        .map(|t| t.name())
        .collect::<Vec<_>>()
        .join("|")
}

/// The worked examples, one per kind.
fn examples() -> String {
    let example = json!({
        "action": "show", "id": "checkout", "title": "Checkout architecture",
        "spec": {
            "groups": [{ "id": "backend", "label": "Backend", "color": "blue" }],
            "nodes": [
                { "id": "web", "label": "Web app", "kind": "client", "icon": "globe", "description": "Customers browse and pay" },
                { "id": "api", "label": "API", "group": "backend", "tech": "Rust", "icon": "credit-card", "description": "Validates carts and takes payments" },
                { "id": "db", "label": "Orders DB", "kind": "database", "group": "backend", "tech": "Postgres", "description": "Orders and their line items" },
            ],
            "edges": [
                { "from": "web", "to": "api", "label": "Places orders over HTTPS" },
                { "from": "api", "to": "db", "label": "Writes orders", "style": "data" },
            ],
        },
    });
    let kinds = [
        json!({"kind": "table", "title": "Flaky tests", "spec": {"columns": [{"key": "name", "label": "Test"}, {"key": "fails", "label": "Fails", "type": "number"}, {"key": "state", "type": "status"}], "rows": [{"id": "login", "cells": {"name": "login flow", "fails": 4, "state": "failing"}}]}}),
        json!({"kind": "form", "title": "Deploy options", "spec": {"fields": [{"id": "env", "type": "radio", "label": "Environment", "options": ["staging", "prod"], "required": true}, {"id": "notes", "type": "textarea", "label": "Notes"}]}}),
        json!({"kind": "checklist", "title": "Migration plan", "spec": {"items": [{"id": "backup", "label": "Back up the DB"}, {"id": "migrate", "label": "Run migrations", "items": [{"id": "users", "label": "users table"}]}]}}),
        json!({"kind": "kanban", "title": "Sprint", "spec": {"columns": [{"id": "todo", "label": "To do"}, {"id": "doing", "label": "Doing", "limit": 3}, {"id": "done", "label": "Done"}], "cards": [{"id": "oauth", "title": "OAuth login", "column": "doing", "tags": ["backend"]}]}}),
        json!({"kind": "chart", "title": "p95 latency", "spec": {"type": "line", "labels": ["Mon", "Tue", "Wed"], "series": [{"name": "api", "values": [120, 135, 98]}], "unit": "ms"}}),
        json!({"kind": "review", "title": "Rename login", "spec": {"summary": "login → sign_in", "files": [{"path": "src/auth.ts", "hunks": [{"id": "auth-1", "header": "export function", "oldStart": 12, "newStart": 12, "lines": [" export function", "-login(user) {", "+signIn(user) {", "   return check(user);"]}]}, {"path": "src/app.ts", "before": "login(u);\n", "after": "signIn(u);\n"}]}}),
        json!({"kind": "sequence", "title": "Login", "spec": {"participants": [{"id": "web", "label": "Web", "kind": "actor"}, {"id": "api", "label": "API"}, {"id": "db", "label": "Users DB", "kind": "db"}], "messages": [{"from": "web", "to": "api", "label": "POST /login"}, {"from": "api", "to": "db", "label": "find user"}, {"from": "db", "to": "api", "label": "row", "kind": "return"}, {"from": "api", "to": "web", "label": "token", "kind": "return"}], "fragments": [{"kind": "opt", "label": "not cached", "from": 2, "to": 3}]}}),
        json!({"kind": "erd", "title": "Orders schema", "spec": {"tables": [{"id": "users", "ref": {"path": "db/migrations/001_users.sql"}, "columns": [{"name": "id", "type": "uuid", "pk": true}, {"name": "email", "type": "text", "unique": true}]}, {"id": "orders", "ref": {"path": "src/models/order.ts", "line": 8}, "columns": [{"name": "id", "type": "uuid", "pk": true}, {"name": "user_id", "type": "uuid", "fk": "users.id"}]}]}}),
        json!({"kind": "state", "title": "Pull request", "spec": {"states": [{"id": "start", "kind": "initial"}, "Draft", "Review", {"id": "merged", "kind": "final"}], "transitions": [{"from": "start", "to": "Draft"}, {"from": "Draft", "to": "Review", "event": "submit"}, {"from": "Review", "to": "Draft", "event": "reject"}, {"from": "Review", "to": "merged", "event": "approve", "guard": "CI green"}], "current": "Review"}}),
        json!({"kind": "timeline", "title": "Launch plan", "spec": {"tasks": [{"id": "design", "label": "Design", "start": "2026-03-02", "duration": 5}, {"id": "build", "label": "Build", "start": "2026-03-09", "end": "2026-03-20", "dependsOn": ["design"], "progress": 30}, {"id": "launch", "label": "Launch", "start": "2026-03-23", "milestone": true, "dependsOn": ["build"]}]}}),
    ];
    let mut lines = vec![format!("gentle_ui example: {example}")];
    for mut example in kinds {
        let kind = example["kind"].as_str().unwrap_or_default().to_string();
        let mut call = serde_json::Map::new();
        call.insert("action".into(), json!("show"));
        if let Some(object) = example.as_object_mut() {
            call.append(object);
        }
        lines.push(format!("gentle_ui {kind} example: {}", Value::Object(call)));
    }
    lines.join("\n")
}

/// The parameters' JSON Schema.
pub fn parameters() -> Value {
    let colors = colors();
    let icons = ICONS.join("|");
    let spec = format!(
        "Full spec for the kind. diagram: {{ direction?: LR|TB, groups?: [{{id,label,description?,color?}}], nodes: [{{id,label,kind?,group?,description?,tech?,status?,ref?,color?,icon?}}], edges?: [{{id?,from,to,label?,style?,color?}}] }}; \
         node kind: {NODE_KINDS}, status: {STATUSES}, edge style: {EDGE_STYLES}. \
         color (diagram nodes, groups, edges; state states and groups; sequence participants; erd tables; timeline tasks), drawn in the user's theme: {colors}; default: the kind's. \
         icon (diagram nodes, states, participants, tables; default: the kind's), a Lucide name: {icons}. Use color to group meaning (one color per layer or team) and icon to say what a thing is. \
         table: {{ columns: [{{key,label?,type?: {COLUMN_TYPES},align?}}], rows: [{{id,cells:{{key:value}},ref?}}], sort?: {{column,direction:asc|desc}}, editable?, selectable?, selection?: [row ids], live? }}. \
         form: {{ fields: [{{id,type: {FIELD_TYPES},label,help?,placeholder?,required?,options?: [value|{{value,label}}],min?,max?,step?,default?}}], description?, submitLabel?, values? }}. \
         checklist: {{ items: [{{id,label,done?,status?: pending|running|done|failed|skipped,note?,ref?,items?: [sub-items, one level]}}], run?: true (a plan the user runs), live? }}. \
         kanban: {{ columns: [{{id,label,limit?,starts?: true (a card dropped here asks you to dispatch a subagent)}}], cards: [{{id,title,column,description?,tags?,assignee?,ref?,agent?: {{name?,task?}},agentId?}}] }}. \
         ref (any item that is code): {{ path: repo-relative, line?, endLine? }} or \"path:line\". \
         chart: {{ type: {CHART_TYPES}, labels: [...], series: [{{name,values:[numbers, one per label]}}] (scatter: points: [[x,y]]), xTitle?, yTitle?, unit?, stacked?, live? }}. \
         live (table, chart, checklist; the app fills the data): {{ source: {LIVE_SOURCES}, map? }} or {{ command: {{cmd, cwd?, parse: json|lines|csv}}, interval?: seconds ≥ 5, map? }}; \
         map: {{ items?: path to the records, id?, columns?: {{key: path}} (table), label?, status?, note? (checklist), label?, series?: {{name: path}} (chart) }} with JSONPath-lite paths ($.a.b, $.list[0], $.list[*], relative to each record). \
         sequence: {{ participants?: [{{id,label,kind?: {PARTICIPANT_KINDS},color?,icon?}}] (default: from the messages), messages: [{{id?,from,to,label?,kind?: {MESSAGE_KINDS},note?}}] (in time order), fragments?: [{{id?,kind: {FRAGMENT_KINDS},label?,from,to (message ids or 1-based numbers),else?: [{{at,label?}}]}}], activations?: [{{participant,from,to}}] (default: each sync call until its return) }}. \
         erd: {{ tables: [{{id,name?,columns: [{{name,type?,pk?,fk?: \"table.column\"|{{table,column}},nullable?,unique?,note?}}],note?,ref?,color?,icon?}}], relations?: [{{id?,from: \"table.column\",to: \"table.column\",cardinality?: 1-1|1-n|n-1|n-n,label?}}] }} (every fk is drawn as a relation). \
         state: {{ direction?: LR|TB, groups?: [{{id,label,color?}}] (composite states), states: [{{id,label?,kind?: {STATE_KINDS},group?,description?,color?,icon?}}], transitions: [{{id?,from,to,event?,guard?,action?}}], current?: state id }}. \
         timeline: {{ tasks: [{{id,label,start: YYYY-MM-DD,end?: YYYY-MM-DD (inclusive) | duration?: days,progress?: 0-1 or percent,group?,dependsOn?: [task ids],milestone?,color?}}] }}. \
         html: {{ html: self-contained page ≤512 KiB, height?: px, data?: object (the page's tool input/result), csp?: {{ connectDomains?, resourceDomains?, frameDomains?, baseUriDomains? }} (origins; default no network) }}. \
         review: {{ summary?, files: [{{path, status?: added|modified|deleted|renamed, oldPath?, language?, hunks: [{{id, header?, oldStart, newStart, lines: [\"+added\"|\"-removed\"|\" context\" or {{kind: context|add|del, text}}], note?}}]}}] }} or per file {{path, before?, after?}} (whole texts; hunks are computed; no before = added, no after = deleted).\n\
         Examples:\n{}",
        examples()
    );
    let patch = "Changes for update; lists upsert (null clears a field) and remove: {...} deletes. diagram: { nodes?, edges? (by id or from+to), groups?, remove?: {nodes,edges,groups}, direction? }. \
         table: { rows? (by id, cells merge), columns? (by key), remove?: {rows,columns}, sort?, selection?, editable? }. \
         form: { fields? (by id), values?: {id: value}, remove?: {fields}, submitted? } (an update reopens a submitted form). \
         checklist: { items? (by id at any level; parent: id|null places it; status: pending|running|done|failed|skipped), remove?: {items}, run?, live? }. \
         kanban: { cards? (by id; agentId: the subagent you dispatched), columns? (by id), remove?: {cards,columns} }. chart: { series? (by name), remove?: {series}, labels?, type?, titles?, live? }. table: also live?. \
         sequence: { participants?, messages? (by id; at: 1-based place), fragments? (by id), remove?: {participants,messages,fragments}, activations? (replace; null derives them) }. erd: { tables? (by id; columns replace), relations? (by id), remove?: {tables,relations,columns: [\"table.column\"]} }. \
         state: { states?, transitions?, groups? (by id), remove?: {states,transitions,groups}, current?, direction? }. timeline: { tasks? (by id; end or duration replaces the other), remove?: {tasks} }. \
         html: { html?, height?, csp? (replace; null clears), data?: {merged; null deletes a key} } (data-only updates reach the open page without reloading). \
         review: { applied?: [hunk ids you wrote], hunks? (by id; decision: null asks again), files? (by path, replace), remove?: {files,hunks}, summary? }.";
    json!({
        "type": "object",
        "properties": {
            "action": { "type": "string", "enum": ["show", "update", "close"] },
            "id": { "type": "string", "description": "Stable slug for the UI (show) or the id to update/close" },
            "title": { "type": "string", "description": "Short title, default the kind's name (\"Diagram\", \"Table\"…)" },
            "kind": { "type": "string", "enum": KINDS, "description": "For show, default diagram; an id keeps its kind" },
            "spec": { "type": "object", "properties": {}, "additionalProperties": true, "description": spec },
            "patch": { "type": "object", "properties": {}, "additionalProperties": true, "description": patch },
            "wait": { "type": "boolean", "description": "review only: wait for the user's decisions and get them as this call's result (default false: they arrive later as a message)" }
        },
        "required": ["action"]
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_mode_fits_the_host_tool_caps() {
        for mode in [Mode::Proactive, Mode::WhenAsked] {
            let lines = guidelines(mode);
            assert!(
                lines.len() <= MAX_GUIDELINES,
                "{mode:?}: {} guidelines",
                lines.len()
            );
            for line in &lines {
                assert!(
                    !line.is_empty() && line.chars().count() <= MAX_GUIDELINE_CHARS,
                    "{line}"
                );
            }
            // Nothing is lost in packing.
            let joined = lines.join(" ");
            for rule in policy(mode).iter().map(|r| r.to_string()).chain(rules()) {
                for word in rule.split_whitespace() {
                    assert!(joined.contains(word), "{word} went missing");
                }
            }
        }
        assert!(guidelines(Mode::Off).is_empty());
        assert!(DESCRIPTION.chars().count() <= 4096);
        assert!(serde_json::to_string(&parameters()).unwrap().len() <= 16 * 1024);
    }

    #[test]
    fn the_mode_puts_its_policy_first() {
        assert!(guidelines(Mode::Proactive)[0].starts_with("Use gentle_ui on your own initiative"));
        assert!(
            guidelines(Mode::WhenAsked)[0]
                .starts_with("The user wants gentle_ui surfaces only on request")
        );
    }

    #[test]
    fn pack_splits_long_lines_at_sentences() {
        let long = format!("{}. {}", "a".repeat(30), "b".repeat(30));
        assert_eq!(
            pack(&[long, "c".into()], 40),
            [
                format!("{}.", "a".repeat(30)),
                format!("{} c", "b".repeat(30))
            ]
        );
    }
}
