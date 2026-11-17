//! Interaction tests: a real [`SessionView`] (no backend started) in a
//! headless window, its Desktop channel fed the records Gentle Shell would
//! send, driven by simulated clicks.

// Not a glob: GPUI exports a `test` attribute that would shadow `#[test]`.
use gpui_kit::{Entity, Modifiers, TestAppContext, VisualTestContext};
use pi_rpc::desktop_rpc::{PROMPT_COMMAND, SCHEMA, WIDGET_KEY};
use pi_rpc::{UiMethod, UiRequest};
use serde_json::{Value, json};

use super::{AgentsTab, SessionView};

fn widget(record: Value) -> UiRequest {
    UiRequest {
        id: "ui-1".into(),
        timeout: None,
        method: UiMethod::SetWidget {
            key: WIDGET_KEY.into(),
            lines: Some(vec![record.to_string()]),
            placement: None,
        },
    }
}

fn response(request_id: &str, data: Value) -> UiRequest {
    widget(json!({
        "schema": SCHEMA, "kind": "response", "requestId": request_id, "ok": true, "data": data,
    }))
}

/// A tab in a headless window that never starts a backend (drawing a tab
/// starts one otherwise).
fn tab(cx: &mut TestAppContext) -> (Entity<SessionView>, &mut VisualTestContext) {
    cx.update(|cx| {
        gpui_kit::init(cx);
        cx.set_reduce_motion(true);
    });
    let cwd = std::env::temp_dir();
    cx.add_window_view(|window, cx| {
        let mut view = SessionView::new(cwd, None, window, cx, false);
        view.backend.started = true;
        view
    })
}

/// A tab whose channel is probed with `methods`, and whose job and helper
/// lists hold one running job (`job-1`) and one helper (`agent-1`) the
/// `gentle-agents` widget has no thread for.
fn open<'a>(
    methods: &[&str],
    cx: &'a mut TestAppContext,
) -> (Entity<SessionView>, &'a mut VisualTestContext) {
    let (view, cx) = tab(cx);
    let mut all = vec![
        "capabilities",
        "get_snapshot",
        "list_jobs",
        "list_subagents",
    ];
    all.extend_from_slice(methods);
    view.update(cx, |view, _| {
        let desktop = &mut view.desktop;
        let probe = desktop.ready(true).sends.remove(0);
        let snapshot = desktop
            .ui(&response(
                probe.request.request_id(),
                json!({
                    "schema": SCHEMA, "command": PROMPT_COMMAND, "widget": WIDGET_KEY,
                    "methods": all, "events": ["jobs_update", "agents_update"],
                }),
            ))
            .sends
            .remove(0);
        desktop.ui(&response(
            snapshot.request.request_id(),
            json!({"schema": SCHEMA, "sequence": 1, "state": {}}),
        ));
        let agents = desktop.supervision_seed().sends.remove(0);
        desktop.supervision_apply(&response(
            agents.request.request_id(),
            json!({"agents": [{
                "id": "agent-1", "agent": "worker", "status": "running",
                "model": "anthropic/claude-sonnet-4", "lastStep": "writing",
                "usage": {"total": 100, "cost": 0.01},
            }]}),
        ));
        let jobs = desktop.supervision_seed().sends.remove(0);
        desktop.supervision_apply(&response(
            jobs.request.request_id(),
            json!({"jobs": [{"id": "job-1", "label": "npm test", "status": "running", "startedAt": 1}]}),
        ));
        assert!(desktop.jobs_ready() && desktop.subagents_ready());
    });
    (view, cx)
}

fn drawn(cx: &mut VisualTestContext, selector: &'static str) -> bool {
    cx.run_until_parked();
    cx.debug_bounds(selector).is_some()
}

fn click(cx: &mut VisualTestContext, selector: &'static str) {
    let at = cx
        .debug_bounds(selector)
        .unwrap_or_else(|| panic!("nothing drawn as {selector}"))
        .center();
    cx.simulate_click(at, Modifiers::none());
    cx.run_until_parked();
}

/// Answer the output read the slice owes now, as Gentle Shell would.
fn answer(view: &Entity<SessionView>, cx: &mut VisualTestContext, data: Value) {
    view.update(cx, |view, cx| {
        let sends = view.desktop.output_pump().sends;
        let id = sends[0].request.request_id().to_string();
        view.desktop.output_apply(&response(&id, data));
        cx.notify();
    });
    cx.run_until_parked();
}

#[gpui::test]
fn a_job_row_folds_its_output_open_and_shut(cx: &mut TestAppContext) {
    let (view, cx) = open(&["get_job_output"], cx);
    view.update(cx, |view, cx| view.open_agents_panel(AgentsTab::Jobs, cx));
    assert!(drawn(cx, "job-row-job-1"));
    assert!(!drawn(cx, "job-output"));

    click(cx, "job-row-job-1");
    // The tab has no backend, so the first read was dropped; the well shows
    // that it is reading, and the next owed read gets its answer.
    assert!(drawn(cx, "job-output"));
    view.update(cx, |view, _| {
        assert!(view.desktop.output_mut().tick_job());
    });
    answer(
        &view,
        cx,
        json!({
            "job": {"id": "job-1", "label": "npm test", "status": "running", "startedAt": 1},
            "output": "✓ lint\nrunning tests",
            "truncated": true,
        }),
    );
    view.read_with(cx, |view, _| {
        let job = view.desktop.output().job().unwrap();
        assert_eq!(job.output.as_ref().unwrap().output, "✓ lint\nrunning tests");
    });
    assert!(drawn(cx, "job-output"));

    // Clicking it again folds it and stops reading.
    click(cx, "job-row-job-1");
    assert!(!drawn(cx, "job-output"));
    view.read_with(cx, |view, _| assert!(view.desktop.output().job().is_none()));
}

#[gpui::test]
fn without_get_job_output_a_job_row_stays_as_it_was(cx: &mut TestAppContext) {
    let (view, cx) = open(&[], cx);
    view.update(cx, |view, cx| view.open_agents_panel(AgentsTab::Jobs, cx));
    click(cx, "job-row-job-1");
    assert!(!drawn(cx, "job-output"));
    view.read_with(cx, |view, _| assert!(view.desktop.output().job().is_none()));
}

#[gpui::test]
fn the_activity_tab_reads_a_thread_the_widget_did_not_publish(cx: &mut TestAppContext) {
    let (view, cx) = open(&["get_subagent_thread"], cx);
    view.update(cx, |view, cx| {
        view.open_agents_panel(AgentsTab::Activity, cx);
    });
    cx.run_until_parked();
    view.update(cx, |view, _| {
        let thread = view.desktop.output().thread().expect("watched on open");
        assert_eq!(thread.id, "agent-1");
        // No backend here: the dropped first read is asked again, as an
        // `agents_update` would.
        view.desktop.output_mut().refresh_thread();
    });
    assert!(!drawn(cx, "agent-thread"));
    answer(
        &view,
        cx,
        json!({
            "id": "agent-1", "version": 3, "dropped": 0, "truncated": false,
            "items": [{"kind": "text", "text": "Mapping the repository layout."}],
        }),
    );
    assert!(drawn(cx, "agent-thread"));

    // Leaving the tab stops reading it.
    view.update(cx, |view, cx| view.close_agents(cx));
    view.read_with(cx, |view, _| {
        assert!(view.desktop.output().thread().is_none())
    });
}

#[gpui::test]
fn without_get_subagent_thread_the_activity_tab_keeps_its_empty_state(cx: &mut TestAppContext) {
    let (view, cx) = open(&[], cx);
    view.update(cx, |view, cx| {
        view.open_agents_panel(AgentsTab::Activity, cx)
    });
    cx.run_until_parked();
    view.read_with(cx, |view, _| {
        assert!(view.desktop.output().thread().is_none())
    });
    assert!(!drawn(cx, "agent-thread"));
}

/// The session feed's sections drive the todo island and the command menu;
/// a feed-listed Pi built-in that only works in the terminal is never sent.
#[gpui::test]
fn the_session_feed_drives_the_todo_list_and_the_command_menu(cx: &mut TestAppContext) {
    let (view, cx) = tab(cx);
    view.update(cx, |view, cx| {
        let probe = view.desktop.ready(true).sends.remove(0);
        let snapshot = view
            .desktop
            .ui(&response(
                probe.request.request_id(),
                json!({
                    "schema": SCHEMA, "command": PROMPT_COMMAND, "widget": WIDGET_KEY,
                    "methods": ["capabilities", "get_snapshot"], "events": ["session_snapshot"],
                }),
            ))
            .sends
            .remove(0);
        view.desktop.ui(&response(
            snapshot.request.request_id(),
            json!({"schema": SCHEMA, "sequence": 1, "state": {"session": {
                "context": {"percent": 61.0, "tokens": 122000, "window": 200000},
                "todo": {
                    "items": [{"id": 1, "title": "Wire the feed", "state": "in_progress", "note": null}],
                    "progress": {"done": 0, "total": 1, "open": 1}, "current": "Wire the feed", "truncated": false,
                },
                "activity": {"state": "idle", "streaming": false, "phase": null, "phaseLabel": null,
                             "blockedLabel": null, "agents": 0, "jobs": 0, "label": "Idle"},
                "pendingQuestion": null,
                "commands": {"items": [
                    {"name": "compact", "description": "Pi compact", "args": null, "terminal": true, "source": "builtin"},
                    {"name": "model", "description": "Select model", "args": null, "terminal": true, "source": "builtin"},
                ], "truncated": false},
            }}}),
        ));
        view.sync_sections(cx);
    });
    cx.run_until_parked();
    view.update(cx, |view, cx| {
        let titles: Vec<&str> = view.todo.tasks.iter().map(|t| t.title.as_str()).collect();
        assert_eq!(titles, ["Wire the feed"]);
        assert_eq!(view.usage.context_percent, Some(61.0));

        let commands = view.commands();
        let compact = commands.iter().find(|c| c.name == "compact").unwrap();
        assert!(!compact.terminal, "the app's own /compact wins");
        let model = commands.iter().find(|c| c.name == "model").unwrap();
        assert!(model.terminal);

        let before = view.transcript.items.len();
        view.run_prompt("/model".into(), cx);
        assert_eq!(view.transcript.items.len(), before + 1);
        assert!(matches!(
            view.transcript.items.last(),
            Some(gentle_core::transcript::Item::Notice { text, .. })
                if text == "/model only works in Gentle Shell's terminal."
        ));
    });
}
