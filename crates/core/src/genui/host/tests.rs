//! The extension's behaviour, call by call and event by event.

use serde_json::{Value, json};

use super::*;

fn engine() -> Engine {
    Engine::new(Mode::Proactive)
}

fn done(reply: Result<Reply, String>) -> Answer {
    match reply.expect("the call succeeds") {
        Reply::Done(answer) => answer,
        other => panic!("not answered: {other:?}"),
    }
}

fn show(engine: &mut Engine, args: Value) -> Answer {
    let mut args = args;
    args["action"] = json!("show");
    done(engine.call("call-1", &args))
}

fn diagram() -> Value {
    json!({"nodes": [{"id": "web", "label": "Web", "kind": "client"}, {"id": "api", "label": "API"}],
           "edges": [{"from": "web", "to": "api", "label": "calls"}]})
}

fn event(engine: &mut Engine, event: Value) -> Handled {
    engine.event(&event.to_string())
}

fn outcome(handled: &Handled) -> Value {
    serde_json::from_str(handled.outcome.as_deref().expect("an outcome")).unwrap()
}

#[test]
fn show_update_close_carry_ids_versions_and_details() {
    let mut e = engine();
    let shown = show(&mut e, json!({"id": "arch", "spec": diagram()}));
    assert_eq!(shown.details["id"], "arch");
    assert_eq!(shown.details["version"], 1);
    assert_eq!(shown.details["kind"], "diagram");
    assert_eq!(shown.details["title"], "Diagram");
    assert_eq!(shown.details["spec"]["edges"][0]["id"], "web->api");
    assert!(shown.text.starts_with(
        "Diagram \"Diagram\" (arch, v1) is shown to the user (2 nodes, 1 edge, 0 groups, LR)."
    ));
    assert!(
        shown
            .text
            .contains("Nodes: web \"Web\" (client), api \"API\" (service)")
    );
    assert!(shown.text.contains("Edges: web → api \"calls\""));
    assert!(shown.text.contains("\"host.gentle-ui.event\" messages"));

    let updated = done(e.call(
        "call-2",
        &json!({"action": "update", "id": "arch", "title": "Checkout",
                "patch": {"nodes": [{"id": "db", "kind": "database"}], "edges": [{"from": "api", "to": "db", "style": "data"}]}}),
    ));
    assert_eq!(updated.details["version"], 2);
    assert_eq!(updated.details["title"], "Checkout");
    assert!(updated.text.contains("api → db [data]"));

    let closed = done(e.call("call-3", &json!({"action": "close", "id": "arch"})));
    assert_eq!(closed.text, "Closed diagram \"Checkout\" (arch).");
    assert_eq!(closed.details["closed"], true);
    assert_eq!(closed.details["spec"], Value::Null);
    assert_eq!(
        e.call(
            "call-4",
            &json!({"action": "update", "id": "arch", "patch": {}})
        ),
        Err("gentle_ui: no open UI with id \"arch\" (open: none)".into())
    );
}

#[test]
fn ids_are_fresh_and_never_repeat() {
    let mut e = engine();
    assert_eq!(
        show(&mut e, json!({"spec": diagram()})).details["id"],
        "ui-1"
    );
    // Taken or not a slug: the next ui-<n>.
    assert_eq!(
        show(&mut e, json!({"id": "ui-1", "spec": diagram()})).details["id"],
        "ui-2"
    );
    assert_eq!(
        show(&mut e, json!({"id": "Not A Slug", "spec": diagram()})).details["id"],
        "ui-3"
    );
    done(e.call("c", &json!({"action": "close", "id": "ui-3"})));
    assert_eq!(
        show(&mut e, json!({"spec": diagram()})).details["id"],
        "ui-4"
    );
}

#[test]
fn calls_report_the_extensions_errors() {
    let mut e = engine();
    assert_eq!(
        e.call("c", &json!({"action": "show"})),
        Err("gentle_ui show needs a spec".into())
    );
    assert_eq!(
        e.call(
            "c",
            &json!({"action": "show", "spec": diagram(), "patch": [1]})
        ),
        Err("patch must be an object".into())
    );
    show(
        &mut e,
        json!({"id": "t", "kind": "table", "spec": {"columns": ["a"]}}),
    );
    assert_eq!(
        e.call(
            "c",
            &json!({"action": "update", "id": "t", "kind": "chart", "patch": {}})
        ),
        Err("gentle_ui: \"t\" is a table; show a new UI for a chart".into())
    );
    assert_eq!(
        e.call("c", &json!({"action": "update", "id": "t"})),
        Err("gentle_ui update needs a spec (replace) or a patch (merge)".into())
    );
    let error = e
        .call(
            "c",
            &json!({"action": "show", "spec": {"nodes": [{"id": "a"}, {"id": "a"}]}}),
        )
        .unwrap_err();
    assert_eq!(error, "invalid diagram spec:\n- duplicate node id \"a\"");
    assert_eq!(
        e.call(
            "c",
            &json!({"action": "show", "kind": "chart", "spec": {"series": []}, "wait": true})
        ),
        Err("invalid chart spec:\n- a chart needs at least one series".into())
    );
}

#[test]
fn a_spec_sent_as_a_json_string_is_read() {
    let mut e = engine();
    let shown = show(&mut e, json!({"spec": diagram().to_string()}));
    assert_eq!(shown.details["spec"]["nodes"][1]["id"], "api");
}

#[test]
fn off_refuses_calls_and_adds_its_rule_and_note() {
    let mut e = Engine::new(Mode::Off);
    assert_eq!(
        e.call("c", &json!({"action": "show", "spec": diagram()})),
        Err(OFF_ERROR.into())
    );
    assert_eq!(
        e.context(),
        Some((OFF_RULE.to_string(), json!({"ids": []})))
    );

    let mut e = engine();
    assert_eq!(e.context(), None);
    show(&mut e, json!({"id": "d", "spec": diagram()}));
    let handled = event(&mut e, json!({"op": "mode", "mode": "off"}));
    assert_eq!(
        (handled.mode, handled.context_changed, handled.outcome),
        (Some(Mode::Off), true, None)
    );
    let spec = e.surface("d").unwrap().spec.clone();
    let handled = event(
        &mut e,
        json!({"id": "d", "version": 1, "changes": [], "summary": [], "spec": spec}),
    );
    assert!(handled.message.unwrap().content.ends_with(OFF_NOTE));
    assert_eq!(
        event(&mut e, json!({"op": "mode", "mode": "sometimes"})).error,
        Some("gentle_ui: unknown mode \"sometimes\"".into())
    );
}

#[test]
fn edits_become_a_new_version_and_a_message() {
    let mut e = engine();
    show(&mut e, json!({"id": "d", "spec": diagram()}));
    let mut spec = e.surface("d").unwrap().spec.clone();
    spec["nodes"][1]["label"] = json!("Gateway");
    let handled = event(
        &mut e,
        json!({"id": "d", "version": 1, "changes": [{"op": "rename", "node": "api", "label": "Gateway"}],
               "summary": ["renamed API to Gateway"], "spec": spec, "note": " keep it "}),
    );
    assert_eq!(
        outcome(&handled),
        json!({"id": "d", "version": 1, "ok": true, "current": 2})
    );
    let message = handled.message.unwrap();
    assert!(message.trigger_turn);
    assert!(message.content.starts_with(
        "The user edited the diagram \"Diagram\" (d, now v2) in the app:\n- renamed API to Gateway\nNote from the user: keep it\nChanges: [{\"op\":\"rename\",\"node\":\"api\",\"label\":\"Gateway\"}]\nDiagram"
    ));
    assert_eq!(message.details["version"], 2);
    assert_eq!(message.details["baseVersion"], 1);
    assert_eq!(message.details["note"], "keep it");
    assert_eq!(message.details["spec"]["nodes"][1]["label"], "Gateway");
    assert_eq!(e.surface("d").unwrap().version, 2);

    // Based on an older version: refused, nothing changes.
    let stale = event(
        &mut e,
        json!({"id": "d", "version": 1, "changes": [], "spec": spec}),
    );
    assert_eq!(
        outcome(&stale)["error"],
        "The agent changed this diagram meanwhile (now v2); review your edits on it and send again."
    );
    assert_eq!(stale.message, None);
    // Unknown surface, unreadable spec.
    let gone = event(&mut e, json!({"id": "x", "version": 1}));
    assert_eq!(
        outcome(&gone)["error"],
        "The agent no longer has the UI \"x\" open (open: d)."
    );
    let bad = event(
        &mut e,
        json!({"id": "d", "version": 2, "spec": {"nodes": 3}}),
    );
    assert_eq!(outcome(&bad)["ok"], false);
    assert_eq!(
        e.event("{")
            .error
            .map(|e| e.starts_with("gentle_ui: unreadable event")),
        Some(true)
    );
}

#[test]
fn inserted_templates_and_docs_requests() {
    let mut e = engine();
    let handled = event(
        &mut e,
        json!({"op": "insert", "id": "sprint", "kind": "kanban", "title": "Sprint", "template": "Board",
               "spec": {"columns": ["To do", "Done"], "cards": [{"title": "Ship it"}]}}),
    );
    assert_eq!(
        outcome(&handled),
        json!({"id": "sprint", "version": 0, "ok": true, "current": 1})
    );
    let message = handled.message.unwrap();
    assert!(!message.trigger_turn);
    assert!(message.content.starts_with("The user inserted the board \"Sprint\" (sprint, v1) from their saved template \"Board\"; it is shown in the conversation and is yours to update like any gentle_ui surface.\nBoard \"Sprint\" (sprint, v1)"));
    assert_eq!(message.details["inserted"], true);
    assert_eq!(message.details["spec"]["cards"][0]["id"], "ship-it");

    let bad = event(
        &mut e,
        json!({"op": "insert", "id": "z", "kind": "pie", "spec": {}}),
    );
    assert_eq!(
        outcome(&bad),
        json!({"id": "z", "version": 0, "ok": false, "error": "unknown kind \"pie\""})
    );

    let docs = event(
        &mut e,
        json!({"op": "docs", "id": "sprint", "version": 1, "format": "Mermaid", "fence": "mermaid", "text": "graph LR\n"}),
    );
    assert_eq!(outcome(&docs)["current"], 1);
    let message = docs.message.unwrap();
    assert!(message.trigger_turn);
    assert!(message.content.ends_with("as Mermaid: put it where it belongs under docs/ (extend a fitting file or create one), with a short introduction, and say where it went.\n```mermaid\ngraph LR\n```"));
    assert_eq!(message.details["request"], "docs");
}

#[test]
fn html_context_waits_for_the_next_turn() {
    let mut e = engine();
    show(
        &mut e,
        json!({"id": "calc", "kind": "html", "title": "Calc", "spec": {"html": "<p>1</p>"}}),
    );
    let shared = event(
        &mut e,
        json!({"id": "calc", "version": 7, "changes": [
            {"op": "context", "content": [{"type": "text", "text": "old"}]},
            {"op": "context", "content": [{"type": "text", "text": "total 4"}, {"type": "image"}], "structuredContent": {"total": 4}},
        ]}),
    );
    assert!(shared.context_changed);
    assert_eq!(shared.message, None);
    assert_eq!(outcome(&shared)["current"], 1);
    assert_eq!(
        e.context(),
        Some((
            "Context shared by the HTML UI \"Calc\" (calc):\ntotal 4\n[image omitted]\n{\"total\":4}".into(),
            json!({"ids": ["calc"]})
        ))
    );
    assert!(e.turn_started());
    assert_eq!(e.context(), None);

    event(
        &mut e,
        json!({"id": "calc", "changes": [{"op": "context", "content": [{"type": "text", "text": "x"}]}]}),
    );
    let said = event(
        &mut e,
        json!({"id": "calc", "changes": [{"op": "message", "text": "add 2"}]}),
    );
    assert!(said.context_changed);
    assert_eq!(
        said.message.unwrap().content,
        "From the HTML UI \"Calc\" (calc, v2) the user says:\nadd 2\nContext the page shared: x"
    );
    assert_eq!(e.context(), None);
}

fn review_spec() -> Value {
    json!({"files": [{"path": "src/a.rs", "hunks": [
        {"id": "h1", "oldStart": 1, "newStart": 1, "lines": ["-a", "+b"]},
        {"id": "h2", "oldStart": 10, "newStart": 10, "lines": ["-c", "+d"]},
    ]}]})
}

#[test]
fn review_decisions_say_what_to_apply() {
    let mut e = engine();
    let shown = show(
        &mut e,
        json!({"id": "r", "kind": "review", "spec": review_spec()}),
    );
    assert!(shown.text.contains("0 of 2 hunks decided"));
    assert!(
        shown
            .text
            .contains("  - [undecided] h1: src/a.rs @@ -1,1 +1,1 @@")
    );
    let mut spec = e.surface("r").unwrap().spec.clone();
    spec["files"][0]["hunks"][0]["decision"] = json!("accepted");
    let handled = event(
        &mut e,
        json!({"id": "r", "version": 1, "spec": spec,
               "changes": [{"op": "decide", "hunk": "h1", "decision": "accepted", "comment": "ok"}]}),
    );
    let content = handled.message.unwrap().content;
    assert!(content.starts_with("The user reviewed the changes in \"Review\" (r, now v2):\nAccepted: apply exactly these hunks as proposed:\n- h1: src/a.rs @@ -1,1 +1,1 @@\nComments to address"));
    assert!(content.contains("- h1 (accepted): \"ok\""));
    assert!(
        content.contains(
            "Still undecided (don't apply these yet; decisions on them arrive later): h2"
        )
    );
    assert!(content.contains(
        r#"call gentle_ui update with {"action":"update","id":"r","patch":{"applied":["h1"]}}"#
    ));

    let applied = done(e.call(
        "c",
        &json!({"action": "update", "id": "r", "patch": {"applied": ["h1"]}}),
    ));
    assert!(applied.text.contains("[applied] h1"));
    assert_eq!(
        e.call(
            "c",
            &json!({"action": "update", "id": "r", "patch": {"applied": ["nope"]}})
        ),
        Err("patch.applied: unknown hunk \"nope\" (add hunks through files)".into())
    );
}

#[test]
fn a_waiting_review_gets_the_decisions_as_its_result() {
    let mut e = engine();
    let reply = e
        .call("call-1", &json!({"action": "show", "id": "r", "kind": "review", "spec": review_spec(), "wait": true}))
        .unwrap();
    assert!(matches!(&reply, Reply::Waiting { replaced: None, surface } if surface.id == "r"));
    // A newer waiting call answers the earlier one.
    let reply = e
        .call(
            "call-2",
            &json!({"action": "update", "id": "r", "patch": {"summary": "x"}, "wait": true}),
        )
        .unwrap();
    let Reply::Waiting {
        replaced: Some((call, answer)),
        ..
    } = reply
    else {
        panic!("the first call is answered")
    };
    assert_eq!(call, "call-1");
    assert!(
        answer.text.starts_with(
            "A newer version of this review is waiting instead. The review stays open"
        )
    );
    let spec = e.surface("r").unwrap().spec.clone();
    let handled = event(
        &mut e,
        json!({"id": "r", "version": 2, "spec": spec, "changes": [{"op": "decide", "hunk": "h2", "decision": "rejected"}]}),
    );
    assert_eq!(handled.message, None);
    let (call, answer) = handled.decided.unwrap();
    assert_eq!(call, "call-2");
    assert!(
        answer
            .text
            .contains("Rejected: do NOT apply these hunks:\n- h2")
    );
    assert_eq!(answer.details["version"], 3);
    // Not waiting any more: stopping finds nothing.
    assert_eq!(e.stop_waiting("call-2", "x"), None);
}

#[test]
fn checklist_runs_and_kanban_dispatches_instruct_the_agent() {
    let mut e = engine();
    let shown = show(
        &mut e,
        json!({"id": "plan", "kind": "checklist", "spec": {"run": true, "items": [
            {"id": "a", "label": "Back up"}, {"id": "b", "label": "Migrate"}]}}),
    );
    assert!(shown.text.contains("- [ ] a \"Back up\""));
    assert!(
        shown
            .text
            .contains("Run plan: the user skips and reorders steps")
    );
    let mut spec = e.surface("plan").unwrap().spec.clone();
    spec["items"][0]["status"] = json!("running");
    spec["items"][0]["done"] = json!(false);
    spec["run"] = json!("all");
    let handled = event(
        &mut e,
        json!({"id": "plan", "version": 1, "spec": spec, "changes": [{"op": "run", "item": "a", "mode": "all"}]}),
    );
    let content = handled.message.unwrap().content;
    assert!(content.contains("Do step 1 next: \"Back up\" (a); it is marked running. Then report it with gentle_ui update {patch: {items: [{id: \"a\", status: \"done\"}]}} (or \"failed\" with a note), and go on through the next pending steps"));
    assert!(
        content
            .contains("Run plan: step 1 \"Back up\" (a) is running: when it is through, report it")
    );

    show(
        &mut e,
        json!({"id": "board", "kind": "kanban", "spec": {"columns": [{"id": "todo", "label": "To do"}, {"id": "go", "label": "Go", "starts": true}],
               "cards": [{"id": "c1", "title": "Fix login", "column": "todo", "agent": {"name": "worker"}}]}}),
    );
    let mut spec = e.surface("board").unwrap().spec.clone();
    spec["cards"][0]["column"] = json!("go");
    let handled = event(
        &mut e,
        json!({"id": "board", "version": 1, "spec": spec, "changes": [{"op": "dispatch", "card": "c1"}]}),
    );
    let content = handled.message.unwrap().content;
    assert!(content.contains("Dispatch a subagent for the card \"Fix login\" (c1) using the worker agent with this task: Fix login. Use your own subagent tool"));
    assert!(content.contains(
        "Go (go, 1, dropping a card here starts a subagent): c1 \"Fix login\" [for worker]"
    ));
}

#[test]
fn patches_merge_like_the_extension() {
    let mut e = engine();
    // table: rows by id (cells merge), remove, scalars.
    show(
        &mut e,
        json!({"id": "t", "kind": "table", "spec": {"columns": [{"key": "n"}, {"key": "v", "type": "number"}],
        "rows": [{"id": "a", "cells": {"n": "A", "v": 1}}, {"id": "b", "cells": {"n": "B"}}]}}),
    );
    let table = done(e.call(
        "c",
        &json!({"action": "update", "id": "t", "patch": {
        "rows": [{"id": "a", "v": null, "n": "AA"}, {"id": "c", "cells": {"n": "C"}}],
        "remove": {"rows": ["b"]}, "sort": {"column": "n", "direction": "desc"}}}),
    ));
    let spec = &table.details["spec"];
    assert_eq!(
        spec["rows"],
        json!([{"id": "a", "cells": {"n": "AA"}}, {"id": "c", "cells": {"n": "C"}}])
    );
    assert!(table.text.contains("a: AA | —"));
    assert!(table.text.contains("Sorted by n (desc)."));

    // form: an update reopens it.
    show(
        &mut e,
        json!({"id": "f", "kind": "form", "spec": {"fields": [{"id": "env", "type": "radio", "options": ["a", "b"]}], "submitted": true}}),
    );
    let form = done(e.call(
        "c",
        &json!({"action": "update", "id": "f", "patch": {"values": {"env": "b"}}}),
    ));
    assert_eq!(form.details["spec"]["submitted"], false);
    assert!(form.text.contains("Prefilled: env = b"));

    // checklist: parent moves an item; status replaces done.
    show(
        &mut e,
        json!({"id": "l", "kind": "checklist", "spec": {"items": [
        {"id": "x", "label": "X", "done": true}, {"id": "y", "label": "Y"}]}}),
    );
    let list = done(e.call("c", &json!({"action": "update", "id": "l", "patch": {"items": [
        {"id": "x", "parent": "y"}, {"id": "z", "label": "Z", "parent": "y", "status": "failed"}]}})));
    assert_eq!(list.details["spec"]["items"][0]["id"], "y");
    assert_eq!(
        list.details["spec"]["items"][0]["items"][1]["status"],
        "failed"
    );
    assert!(list.text.contains("  - [!] z \"Z\" (failed)"));
    assert_eq!(
        e.call("c", &json!({"action": "update", "id": "l", "patch": {"items": [{"id": "q", "parent": "nope"}]}})),
        Err("patch.items: \"nope\" is not a top-level item".into())
    );

    // sequence: at moves a message.
    show(
        &mut e,
        json!({"id": "s", "kind": "sequence", "spec": {"messages": [
        {"id": "m1", "from": "a", "to": "b"}, {"id": "m2", "from": "b", "to": "a"}]}}),
    );
    let seq = done(e.call(
        "c",
        &json!({"action": "update", "id": "s", "patch": {"messages": [{"id": "m2", "at": 1}]}}),
    ));
    assert_eq!(seq.details["spec"]["messages"][0]["id"], "m2");

    // timeline: duration replaces end; removing a task drops it from dependsOn.
    show(
        &mut e,
        json!({"id": "tl", "kind": "timeline", "spec": {"tasks": [
        {"id": "a", "label": "A", "start": "2026-03-02", "end": "2026-03-04"},
        {"id": "b", "label": "B", "start": "2026-03-05", "duration": 2, "dependsOn": ["a"]}]}}),
    );
    let tl = done(e.call(
        "c",
        &json!({"action": "update", "id": "tl", "patch": {"tasks": [{"id": "a", "duration": 1}]}}),
    ));
    assert_eq!(tl.details["spec"]["tasks"][0]["end"], "2026-03-02");
    assert!(tl.text.contains("Critical path: a → b"));
    let tl = done(e.call(
        "c",
        &json!({"action": "update", "id": "tl", "patch": {"remove": {"tasks": ["a"]}}}),
    ));
    assert_eq!(tl.details["spec"]["tasks"][0].get("dependsOn"), None);

    // html: data merges one level.
    show(
        &mut e,
        json!({"id": "h", "kind": "html", "spec": {"html": "<p/>", "data": {"a": 1, "b": 2}}}),
    );
    let html = done(e.call(
        "c",
        &json!({"action": "update", "id": "h", "patch": {"data": {"a": null, "c": 3}}}),
    ));
    assert_eq!(html.details["spec"]["data"], json!({"b": 2, "c": 3}));
    assert!(
        html.text
            .contains("Data keys (the page's tool input and structuredContent): b, c")
    );

    // erd: removing a column drops the fk and relation using it.
    show(
        &mut e,
        json!({"id": "e", "kind": "erd", "spec": {"tables": [
        {"id": "users", "columns": [{"name": "id", "pk": true}]},
        {"id": "orders", "columns": [{"name": "id", "pk": true}, {"name": "user_id", "fk": "users.id"}]}]}}),
    );
    assert_eq!(
        e.surface("e").unwrap().spec["relations"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    let erd = done(e.call(
        "c",
        &json!({"action": "update", "id": "e", "patch": {"remove": {"columns": ["users.id"]}}}),
    ));
    assert_eq!(erd.details["spec"]["relations"], json!([]));
    assert_eq!(
        erd.details["spec"]["tables"][1]["columns"][1].get("fk"),
        None
    );

    // state, chart, kanban.
    show(
        &mut e,
        json!({"id": "st", "kind": "state", "spec": {"states": ["A", "B"], "transitions": [{"from": "A", "to": "B"}], "current": "B"}}),
    );
    let st = done(e.call(
        "c",
        &json!({"action": "update", "id": "st", "patch": {"remove": {"states": ["B"]}}}),
    ));
    assert_eq!(st.details["spec"]["transitions"], json!([]));
    assert_eq!(st.details["spec"].get("current"), None);
    show(
        &mut e,
        json!({"id": "ch", "kind": "chart", "spec": {"labels": ["a"], "series": [{"name": "s", "values": [1]}]}}),
    );
    let ch = done(e.call("c", &json!({"action": "update", "id": "ch", "patch": {"series": [{"name": "s", "values": [2.5]}], "unit": "ms"}})));
    assert!(ch.text.contains("bar chart, 1 series over 1 label in ms."));
    assert!(ch.text.contains("s: 2.5"));
    show(
        &mut e,
        json!({"id": "k", "kind": "kanban", "spec": {"columns": ["Todo", "Done"], "cards": [{"title": "A", "column": "done"}]}}),
    );
    let k = done(e.call(
        "c",
        &json!({"action": "update", "id": "k", "patch": {"remove": {"columns": ["done"]}}}),
    ));
    assert_eq!(k.details["spec"]["cards"], json!([]));
}

#[test]
fn a_reloaded_conversation_rebuilds_the_surfaces() {
    let mut first = engine();
    let shown = show(&mut first, json!({"id": "d", "spec": diagram()}));
    let other = show(&mut first, json!({"spec": diagram()}));
    let closed = done(first.call(
        "c",
        &json!({"action": "close", "id": other.details["id"].as_str().unwrap()}),
    ));

    let mut e = engine();
    e.reset();
    for details in [&shown.details, &other.details, &closed.details] {
        e.remember(details);
    }
    assert_eq!(e.surface("d").unwrap().version, 1);
    assert!(e.surface("ui-1").is_none());
    // The closed id still counts.
    assert_eq!(
        show(&mut e, json!({"spec": diagram()})).details["id"],
        "ui-2"
    );
    // An older version seen live doesn't undo a newer one.
    done(e.call(
        "c",
        &json!({"action": "update", "id": "d", "patch": {"direction": "TB"}}),
    ));
    e.observe(&shown.details);
    assert_eq!(e.surface("d").unwrap().version, 2);
}

#[test]
fn event_messages_of_both_kinds_draw_alike() {
    assert!(is_event_message("gentle-ui.event"));
    assert!(is_event_message("host.gentle-ui.event"));
    assert!(!is_event_message("host.context.desktop"));
}
