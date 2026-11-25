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
        view.usage.context_test_ready = true;
        view.usage.context_stats_received = true;
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

/// A tab under the window's text-selection layer, as the app's Root draws it.
struct Selectable(Entity<SessionView>);

impl gpui_kit::Render for Selectable {
    fn render(
        &mut self,
        _: &mut gpui_kit::Window,
        _: &mut gpui_kit::Context<Self>,
    ) -> impl gpui_kit::IntoElement {
        use gpui_kit::{ParentElement as _, Styled as _};
        gpui_kit::div()
            .size_full()
            .child(gpui_kit::base::TextSelectionLayer)
            .child(self.0.clone())
    }
}

/// A message you sent can be selected with the mouse, like an answer, and
/// what is selected is its text.
#[gpui::test]
fn context_feed_updates_exact_usage_and_clears_it(cx: &mut TestAppContext) {
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
            json!({
                "schema": SCHEMA, "sequence": 1, "state": {"session": {
                    "context": {"percent": 61.0, "tokens": 122000, "window": 200000}
                }}
            }),
        ));
        view.sync_sections(cx);
        assert_eq!(
            (
                view.usage.context_percent,
                view.usage.context_tokens,
                view.usage.context_window
            ),
            (Some(61.0), Some(122000), Some(200000))
        );

        let event = |seq, sections| {
            widget(json!({
                "schema": SCHEMA, "kind": "event", "seq": seq,
                "event": "session_snapshot", "data": {"sections": sections},
            }))
        };
        view.desktop.ui(&event(2, json!({})));
        view.sync_sections(cx);
        assert_eq!(
            (
                view.usage.context_percent,
                view.usage.context_tokens,
                view.usage.context_window
            ),
            (Some(61.0), Some(122000), Some(200000))
        );

        view.desktop.ui(&event(
            3,
            json!({"context": {"percent": 25.0, "tokens": null, "window": 100000}}),
        ));
        view.sync_sections(cx);
        assert_eq!(
            (
                view.usage.context_percent,
                view.usage.context_tokens,
                view.usage.context_window
            ),
            (Some(25.0), None, Some(100000))
        );

        view.desktop.ui(&event(4, json!({"context": null})));
        view.sync_sections(cx);
        assert_eq!(
            (
                view.usage.context_percent,
                view.usage.context_tokens,
                view.usage.context_window
            ),
            (None, None, None)
        );
    });
}

#[gpui::test]
fn context_ring_is_hidden_during_startup_even_with_context_data(cx: &mut TestAppContext) {
    let (view, cx) = tab(cx);
    view.update(cx, |view, cx| {
        view.usage.context_test_ready = false;
        view.usage.context_stats_received = false;
        cx.notify();
    });
    assert!(!drawn(cx, "context-ring"));
    view.update(cx, |view, cx| {
        view.usage.context_percent = Some(61.0);
        view.usage.context_tokens = Some(122000);
        view.usage.context_stats_received = true;
        cx.notify();
    });
    assert!(!drawn(cx, "context-ring"));
    view.update(cx, |view, cx| {
        view.usage.reset_context();
        view.usage.context_test_ready = true;
        cx.notify();
    });
    assert!(!drawn(cx, "context-ring"));
    view.update(cx, |view, cx| {
        assert_eq!(view.usage.context_percent, None);
        assert_eq!(view.usage.context_tokens, None);
        // A successful empty-session stats response has no percentage.
        view.usage.context_stats_received = true;
        cx.notify();
    });
    assert!(drawn(cx, "context-ring"));
    click(cx, "context-ring");
    assert!(drawn(cx, "context-compact-control"));
}

#[gpui::test]
fn post_compaction_estimate_is_shown_approximately(cx: &mut TestAppContext) {
    let (view, cx) = tab(cx);
    view.update(cx, |view, _| {
        view.usage.context_window = Some(200000);
        view.usage.context_tokens = Some(120000);
        view.usage.context_percent = Some(60.0);
    });
    click(cx, "context-ring");
    view.update_in(cx, |view, window, cx| {
        view.handle_batch(
            vec![pi_rpc::Incoming::Event(json!({
                "type": "compaction_end", "result": {"estimatedTokensAfter": 40000}
            }))],
            window,
            cx,
        );
    });
    assert!(drawn(cx, "context-ring-used-tokens-~40k"));
    assert!(drawn(cx, "context-ring-usage-~20%"));
    assert!(!drawn(cx, "context-ring-used-tokens-40k"));
    click(cx, "context-ring");
    let at = cx.debug_bounds("context-ring").unwrap().center();
    cx.simulate_mouse_move(at, None, Modifiers::none());
    cx.executor()
        .advance_clock(std::time::Duration::from_secs(2));
    assert!(drawn(cx, "context-ring-overview-~40k (~20%)"));
}

#[gpui::test]
fn compaction_without_estimate_restores_interaction_without_inventing_usage(
    cx: &mut TestAppContext,
) {
    let (view, cx) = tab(cx);
    view.update(cx, |view, cx| {
        view.usage.context_window = Some(200000);
        cx.notify();
    });
    click(cx, "context-ring");
    assert!(drawn(cx, "context-compact-control"));
    view.update_in(cx, |view, window, cx| {
        view.handle_batch(
            vec![pi_rpc::Incoming::Event(json!({"type": "compaction_start"}))],
            window,
            cx,
        );
    });
    assert!(!drawn(cx, "context-compact-control"));
    assert!(drawn(cx, "context-compaction-orbit"));
    click(cx, "context-ring");
    assert!(!drawn(cx, "context-compact-control"));
    view.update_in(cx, |view, window, cx| {
        view.handle_batch(
            vec![pi_rpc::Incoming::Event(json!({
                "type": "compaction_end", "result": {}
            }))],
            window,
            cx,
        );
    });
    assert!(!drawn(cx, "context-compaction-orbit"));
    view.read_with(cx, |view, _| {
        assert_eq!(view.usage.context_tokens, None);
        assert_eq!(view.usage.context_percent, None);
        assert_eq!(view.usage.context_window, Some(200000));
        assert!(!view.usage.context_estimated);
    });
    let at = cx.debug_bounds("context-ring").unwrap().center();
    cx.simulate_mouse_move(
        gpui_kit::point(gpui_kit::px(0.), gpui_kit::px(0.)),
        None,
        Modifiers::none(),
    );
    cx.simulate_mouse_move(at, None, Modifiers::none());
    cx.executor()
        .advance_clock(std::time::Duration::from_secs(2));
    assert!(drawn(cx, "context-ring-overview-hint"));
    click(cx, "context-ring");
    assert!(drawn(cx, "context-compact-control"));
    assert!(drawn(cx, "context-auto-compaction"));
    assert!(!drawn(cx, "context-ring-usage-0%"));
    assert!(!drawn(cx, "context-ring-used-tokens-0"));
}

#[gpui::test]
fn post_compaction_estimate_handles_missing_failure_and_stale_stats(cx: &mut TestAppContext) {
    let (view, cx) = tab(cx);
    view.update_in(cx, |view, window, cx| {
        view.usage.context_window = None;
        let old = view.usage.context_revision;
        view.handle_batch(
            vec![pi_rpc::Incoming::Event(json!({
                "type": "compaction_end", "result": {"estimatedTokensAfter": 40000}
            }))],
            window,
            cx,
        );
        assert_eq!(view.usage.context_tokens, Some(40000));
        assert_eq!(view.usage.context_percent, None);
        view.usage.context_window = Some(0);
        view.handle_batch(
            vec![pi_rpc::Incoming::Event(json!({
                "type": "compaction_end", "result": {"estimatedTokensAfter": 40000}
            }))],
            window,
            cx,
        );
        assert_eq!(view.usage.context_percent, None);
        view.apply_stats_at(old, Ok(json!({"contextUsage": {"percent": 80.0}})), cx);
        assert_eq!(view.usage.context_percent, None);
        view.apply_stats_at(
            view.usage.context_revision,
            Ok(json!({"contextUsage": {"percent": null}})),
            cx,
        );
        assert_eq!(view.usage.context_tokens, Some(40000));
        for event in [
            json!({"errorMessage": "failed", "result": {"estimatedTokensAfter": 1}}),
            json!({"aborted": true, "result": {"estimatedTokensAfter": 2}}),
            json!({"cancelled": true, "result": {"estimatedTokensAfter": 3}}),
            json!({"success": false, "result": {"estimatedTokensAfter": 4}}),
            json!({"result": {"estimatedTokensAfter": null}}),
        ] {
            let mut event = event;
            event["type"] = json!("compaction_end");
            view.handle_batch(vec![pi_rpc::Incoming::Event(event)], window, cx);
            assert_eq!(view.usage.context_tokens, Some(40000));
        }
        view.apply_stats_at(
            view.usage.context_revision,
            Ok(json!({"contextUsage": {"percent": 30.0}})),
            cx,
        );
        assert_eq!(view.usage.context_percent, Some(30.0));
        assert_eq!(view.usage.context_tokens, None);
        assert!(!view.usage.context_estimated);
    });
}

#[gpui::test]
fn post_compaction_estimate_survives_empty_context(cx: &mut TestAppContext) {
    let (view, cx) = tab(cx);
    view.update_in(cx, |view, window, cx| {
        let probe = view.desktop.ready(true).sends.remove(0);
        let snapshot = view
            .desktop
            .ui(&response(
                probe.request.request_id(),
                json!({
                    "schema": SCHEMA, "command": PROMPT_COMMAND, "widget": WIDGET_KEY,
                    "methods": ["capabilities", "get_snapshot"], "events": ["session_snapshot"]
                }),
            ))
            .sends
            .remove(0);
        view.desktop.ui(&response(
            snapshot.request.request_id(),
            json!({
                "schema": SCHEMA, "sequence": 1, "state": {"session": {
                    "context": {"percent": 60.0, "tokens": 163200, "window": 272000}
                }}
            }),
        ));
        view.sync_sections(cx);
        view.handle_batch(
            vec![pi_rpc::Incoming::Event(json!({
                "type": "compaction_end", "result": {"estimatedTokensAfter": 62800}
            }))],
            window,
            cx,
        );
        for (seq, context) in [
            (
                2,
                json!({"percent": null, "tokens": null, "window": 272000}),
            ),
            (3, json!({"window": 272000})),
        ] {
            view.handle_batch(
                vec![pi_rpc::Incoming::Ui(widget(json!({
                    "schema": SCHEMA, "kind": "event", "seq": seq,
                    "event": "session_snapshot", "data": {"sections": {"context": context}}
                })))],
                window,
                cx,
            );
        }
    });
    click(cx, "context-ring");
    assert!(drawn(cx, "context-ring-used-tokens-~62.8k"));
    assert!(drawn(cx, "context-ring-usage-~23%"));
    click(cx, "context-ring");
    let at = cx.debug_bounds("context-ring").unwrap().center();
    cx.simulate_mouse_move(at, None, Modifiers::none());
    cx.executor()
        .advance_clock(std::time::Duration::from_secs(2));
    assert!(drawn(cx, "context-ring-overview-~62.8k (~23%)"));
    view.update_in(cx, |view, window, cx| {
        for (seq, context, expected_percent) in [
            (4, json!({"window": 314000}), Some(20.0)),
            (5, json!({"window": 0}), None),
            (6, json!({"window": null}), None),
        ] {
            view.handle_batch(
                vec![pi_rpc::Incoming::Ui(widget(json!({
                    "schema": SCHEMA, "kind": "event", "seq": seq,
                    "event": "session_snapshot", "data": {"sections": {"context": context}}
                })))],
                window,
                cx,
            );
            assert_eq!(view.usage.context_tokens, Some(62800));
            assert_eq!(view.usage.context_percent, expected_percent);
            assert!(view.usage.context_estimated);
        }
        view.handle_batch(
            vec![pi_rpc::Incoming::Ui(widget(json!({
                "schema": SCHEMA, "kind": "event", "seq": 7,
                "event": "session_snapshot", "data": {"sections": {"context": {
                    "tokens": 50000, "percent": 25.0, "window": 200000
                }}}
            })))],
            window,
            cx,
        );
        assert_eq!(view.usage.context_tokens, Some(50000));
        assert_eq!(view.usage.context_percent, Some(25.0));
        assert!(!view.usage.context_estimated);
    });
}

#[gpui::test]
fn post_compaction_estimate_accepts_current_stats_after_empty_feed(cx: &mut TestAppContext) {
    let (view, cx) = tab(cx);
    view.update_in(cx, |view, window, cx| {
        let probe = view.desktop.ready(true).sends.remove(0);
        let snapshot = view
            .desktop
            .ui(&response(
                probe.request.request_id(),
                json!({
                    "schema": SCHEMA, "command": PROMPT_COMMAND, "widget": WIDGET_KEY,
                    "methods": ["capabilities", "get_snapshot"], "events": ["session_snapshot"]
                }),
            ))
            .sends
            .remove(0);
        view.desktop.ui(&response(
            snapshot.request.request_id(),
            json!({
                "schema": SCHEMA, "sequence": 1, "state": {"session": {
                    "context": {"percent": 60.0, "tokens": 120000, "window": 200000}
                }}
            }),
        ));
        view.sync_sections(cx);
        let delta = |seq, sections| {
            widget(json!({
                "schema": SCHEMA, "kind": "event", "seq": seq,
                "event": "session_snapshot", "data": {"sections": sections}
            }))
        };
        for (seq, sections) in [
            (2, json!({})),
            (3, json!({"context": null})),
            (
                4,
                json!({"context": {"tokens": null, "percent": null, "window": 200000}}),
            ),
            (5, json!({"context": {"window": 272000}})),
        ] {
            let stale = view.usage.context_revision;
            view.handle_batch(
                vec![pi_rpc::Incoming::Event(json!({
                    "type": "compaction_end", "result": {"estimatedTokensAfter": 40000}
                }))],
                window,
                cx,
            );
            let current = view.usage.context_revision;
            view.handle_batch(vec![pi_rpc::Incoming::Ui(delta(seq, sections))], window, cx);
            view.apply_stats_at(stale, Ok(json!({"contextUsage": {"percent": 80.0}})), cx);
            assert!(view.usage.context_estimated);
            view.apply_stats_at(current, Ok(json!({"contextUsage": {"percent": 30.0}})), cx);
            assert_eq!(view.usage.context_percent, Some(30.0), "sequence {seq}");
            assert_eq!(view.usage.context_tokens, None);
            assert!(!view.usage.context_estimated);
        }
        view.handle_batch(
            vec![pi_rpc::Incoming::Event(json!({
                "type": "compaction_end", "result": {"estimatedTokensAfter": 40000}
            }))],
            window,
            cx,
        );
        let current = view.usage.context_revision;
        view.handle_batch(
            vec![pi_rpc::Incoming::Ui(delta(
                6,
                json!({"context": {
                    "tokens": 50000, "percent": 25.0, "window": 200000
                }}),
            ))],
            window,
            cx,
        );
        view.apply_stats_at(current, Ok(json!({"contextUsage": {"percent": 30.0}})), cx);
        assert_eq!(view.usage.context_percent, Some(25.0));
        assert_eq!(view.usage.context_tokens, Some(50000));
        assert!(!view.usage.context_estimated);
    });
}

#[gpui::test]
fn post_compaction_estimate_survives_unrelated_feed_delta(cx: &mut TestAppContext) {
    let (view, cx) = tab(cx);
    view.update_in(cx, |view, window, cx| {
        let probe = view.desktop.ready(true).sends.remove(0);
        let snapshot = view
            .desktop
            .ui(&response(
                probe.request.request_id(),
                json!({
                    "schema": SCHEMA, "command": PROMPT_COMMAND, "widget": WIDGET_KEY,
                    "methods": ["capabilities", "get_snapshot"], "events": ["session_snapshot"]
                }),
            ))
            .sends
            .remove(0);
        view.desktop.ui(&response(
            snapshot.request.request_id(),
            json!({
                "schema": SCHEMA, "sequence": 1, "state": {"session": {
                    "context": {"percent": 60.0, "tokens": 120000, "window": 200000}
                }}
            }),
        ));
        view.sync_sections(cx);
        view.handle_batch(
            vec![pi_rpc::Incoming::Event(json!({
                "type": "compaction_end", "result": {"estimatedTokensAfter": 40000}
            }))],
            window,
            cx,
        );
        let delta = |seq, sections| {
            widget(json!({
                "schema": SCHEMA, "kind": "event", "seq": seq,
                "event": "session_snapshot", "data": {"sections": sections}
            }))
        };
        view.handle_batch(vec![pi_rpc::Incoming::Ui(delta(2, json!({})))], window, cx);
        assert_eq!(view.usage.context_tokens, Some(40000));
        assert!(view.usage.context_estimated);
        view.handle_batch(
            vec![pi_rpc::Incoming::Ui(delta(
                3,
                json!({
                    "context": {"percent": 25.0, "tokens": 50000, "window": 200000}
                }),
            ))],
            window,
            cx,
        );
        assert_eq!(view.usage.context_tokens, Some(50000));
        assert_eq!(view.usage.context_percent, Some(25.0));
        assert!(!view.usage.context_estimated);
        view.handle_batch(
            vec![pi_rpc::Incoming::Ui(delta(4, json!({"context": null})))],
            window,
            cx,
        );
        assert_eq!(view.usage.context_tokens, None);
        assert_eq!(view.usage.context_percent, None);
    });
}

#[gpui::test]
fn post_compaction_estimate_rejects_pre_compaction_snapshot(cx: &mut TestAppContext) {
    let (view, cx) = tab(cx);
    view.update_in(cx, |view, window, cx| {
        view.usage.context_window = Some(200000);
        let probe = view.desktop.ready(true).sends.remove(0);
        let snapshot = view
            .desktop
            .ui(&response(
                probe.request.request_id(),
                json!({
                    "schema": SCHEMA, "command": PROMPT_COMMAND, "widget": WIDGET_KEY,
                    "methods": ["capabilities", "get_snapshot"], "events": ["session_snapshot"]
                }),
            ))
            .sends
            .remove(0);
        view.handle_batch(
            vec![pi_rpc::Incoming::Event(json!({
                "type": "compaction_end", "result": {"estimatedTokensAfter": 40000}
            }))],
            window,
            cx,
        );
        view.handle_batch(
            vec![pi_rpc::Incoming::Ui(response(
                snapshot.request.request_id(),
                json!({
                    "schema": SCHEMA, "sequence": 1, "state": {"session": {
                        "context": {"percent": 60.0, "tokens": 120000, "window": 200000}
                    }}
                }),
            ))],
            window,
            cx,
        );
        assert_eq!(view.usage.context_tokens, Some(40000));
        assert_eq!(view.usage.context_percent, Some(20.0));
        assert!(view.usage.context_estimated);
        let fresh = view.desktop.resync().remove(0);
        view.handle_batch(
            vec![pi_rpc::Incoming::Ui(response(
                fresh.request.request_id(),
                json!({
                    "schema": SCHEMA, "sequence": 2, "state": {"session": {
                        "context": {"percent": 25.0, "tokens": 50000, "window": 200000}
                    }}
                }),
            ))],
            window,
            cx,
        );
        assert_eq!(view.usage.context_tokens, Some(50000));
        assert_eq!(view.usage.context_percent, Some(25.0));
        assert!(!view.usage.context_estimated);
    });
}

#[gpui::test]
fn post_compaction_estimate_preserves_normal_stats(cx: &mut TestAppContext) {
    let (view, cx) = tab(cx);
    view.update(cx, |view, cx| {
        view.apply_stats(
            Ok(json!({"cost": 0.5, "contextUsage": {"percent": 61.0}})),
            cx,
        );
        assert_eq!(view.usage.cost, 0.5);
        assert_eq!(view.usage.context_percent, Some(61.0));
        assert!(!view.usage.context_estimated);
    });
    click(cx, "context-ring");
    assert!(drawn(cx, "context-ring-usage-61%"));
}

#[gpui::test]
fn context_ring_failed_stats_then_refresh(cx: &mut TestAppContext) {
    let (view, cx) = tab(cx);
    view.update(cx, |view, cx| {
        view.usage.reset_context();
        cx.notify();
    });
    assert!(!drawn(cx, "context-ring"));
    view.update(cx, |view, cx| {
        view.apply_stats(Err(anyhow::anyhow!("stats failed")), cx);
        cx.notify();
    });
    assert!(drawn(cx, "context-ring"));
    click(cx, "context-ring");
    assert!(drawn(cx, "context-compact-control"));
    assert!(drawn(cx, "context-auto-compaction"));
    assert!(!drawn(cx, "context-ring-usage-0%"));
    view.update(cx, |view, cx| {
        view.apply_stats(
            Ok(json!({"cost": 0.5, "contextUsage": {"percent": 61.0}})),
            cx,
        );
        cx.notify();
    });
    assert!(drawn(cx, "context-ring-usage-61%"));
    view.update(cx, |view, _| assert_eq!(view.usage.cost, 0.5));
}

#[gpui::test]
fn context_ring_empty_stats_response(cx: &mut TestAppContext) {
    let (view, cx) = tab(cx);
    view.update(cx, |view, cx| {
        view.usage.reset_context();
        cx.notify();
    });
    assert!(!drawn(cx, "context-ring"));
    view.update(cx, |view, cx| {
        view.apply_stats(Ok(json!({})), cx);
        cx.notify();
    });
    assert!(drawn(cx, "context-ring"));
    click(cx, "context-ring");
    assert!(drawn(cx, "context-compact-control"));
    assert!(!drawn(cx, "context-ring-usage-0%"));
}

#[gpui::test]
fn context_ring_shows_model_window_without_context_feed(cx: &mut TestAppContext) {
    let (view, cx) = tab(cx);
    view.update(cx, |view, cx| {
        view.models.current = super::state::ModelInfo::parse(&json!({
            "id": "legacy-model", "contextWindow": 200000
        }));
        view.apply_stats(Ok(json!({"contextUsage": {"percent": 25.0}})), cx);
        cx.notify();
    });
    let at = cx
        .debug_bounds("context-ring")
        .expect("ring drawn")
        .center();
    cx.simulate_mouse_move(at, None, Modifiers::none());
    cx.executor()
        .advance_clock(std::time::Duration::from_secs(2));
    assert!(drawn(cx, "context-ring-overview-25%"));
    assert!(drawn(cx, "context-ring-overview-window-200k"));
    click(cx, "context-ring");
    assert!(drawn(cx, "context-ring-context-window-200k"));
    assert!(drawn(cx, "context-ring-usage-25%"));
    assert!(!drawn(cx, "context-ring-used-tokens-50k"));
}

#[gpui::test]
fn context_ring_hover_overview_updates_live(cx: &mut TestAppContext) {
    let (view, cx) = tab(cx);
    view.update(cx, |view, cx| {
        view.usage.context_percent = Some(5.0);
        view.usage.context_tokens = Some(48100);
        view.usage.context_window = Some(200000);
        cx.notify();
    });
    assert!(drawn(cx, "context-ring"));
    let at = cx.debug_bounds("context-ring").unwrap().center();
    cx.simulate_mouse_move(at, None, Modifiers::none());
    view.update(cx, |view, _| assert!(view.usage.context_ring_hovered));
    cx.executor()
        .advance_clock(std::time::Duration::from_secs(2));
    assert!(drawn(cx, "context-ring-overview-48.1k (5%)"));
    assert!(drawn(cx, "context-ring-overview-window-200k"));
    view.update(cx, |view, cx| {
        view.usage.context_tokens = None;
        view.usage.context_percent = Some(25.0);
        cx.notify();
    });
    assert!(drawn(cx, "context-ring-overview-25%"));
    assert!(!drawn(cx, "context-ring-overview-48.1k (5%)"));
    cx.simulate_mouse_move(
        gpui_kit::point(gpui_kit::px(0.), gpui_kit::px(0.)),
        None,
        Modifiers::none(),
    );
    view.update(cx, |view, _| assert!(!view.usage.context_ring_hovered));
}

#[gpui::test]
fn context_ring_hover_returns_after_toggle_and_outside_click(cx: &mut TestAppContext) {
    let (_, cx) = tab(cx);
    assert!(drawn(cx, "context-ring"));
    let at = cx.debug_bounds("context-ring").unwrap().center();
    let outside = gpui_kit::point(gpui_kit::px(0.), gpui_kit::px(0.));
    for outside_click in [false, true] {
        cx.simulate_mouse_move(outside, None, Modifiers::none());
        cx.simulate_mouse_move(at, None, Modifiers::none());
        cx.executor()
            .advance_clock(std::time::Duration::from_secs(2));
        assert!(drawn(cx, "context-ring-overview-hint"));
        click(cx, "context-ring");
        assert!(drawn(cx, "context-compact-control"));
        assert!(!drawn(cx, "context-ring-overview-hint"));
        cx.simulate_mouse_move(outside, None, Modifiers::none());
        cx.simulate_mouse_move(at, None, Modifiers::none());
        cx.executor()
            .advance_clock(std::time::Duration::from_secs(2));
        assert!(!drawn(cx, "context-ring-overview-hint"));
        if outside_click {
            cx.simulate_click(outside, Modifiers::none());
        } else {
            click(cx, "context-ring");
        }
        assert!(!drawn(cx, "context-compact-control"));
        cx.simulate_mouse_move(outside, None, Modifiers::none());
        cx.simulate_mouse_move(at, None, Modifiers::none());
        cx.executor()
            .advance_clock(std::time::Duration::from_secs(2));
        assert!(drawn(cx, "context-ring-overview-hint"));
    }
}

#[gpui::test]
fn context_ring_hover_returns_after_escape(cx: &mut TestAppContext) {
    let (_, cx) = tab(cx);
    assert!(drawn(cx, "context-ring"));
    let at = cx.debug_bounds("context-ring").unwrap().center();
    let outside = gpui_kit::point(gpui_kit::px(0.), gpui_kit::px(0.));
    click(cx, "context-ring");
    assert!(drawn(cx, "context-compact-control"));
    cx.simulate_keystrokes("escape");
    assert!(!drawn(cx, "context-compact-control"));
    cx.simulate_mouse_move(outside, None, Modifiers::none());
    cx.simulate_mouse_move(at, None, Modifiers::none());
    cx.executor()
        .advance_clock(std::time::Duration::from_secs(2));
    assert!(drawn(cx, "context-ring-overview-hint"));
}

#[gpui::test]
fn context_ring_opens_existing_compaction_controls(cx: &mut TestAppContext) {
    let (view, cx) = tab(cx);
    view.update(cx, |view, cx| {
        view.usage.context_percent = Some(35.0);
        cx.notify();
    });
    assert!(drawn(cx, "context-ring"));
    click(cx, "context-ring");
    assert!(drawn(cx, "context-auto-compaction"));
    assert!(drawn(cx, "context-compact-control"));
}

#[gpui::test]
fn context_ring_details_update_live_and_hide_missing_values(cx: &mut TestAppContext) {
    let (view, cx) = tab(cx);
    view.update(cx, |view, cx| {
        view.usage.context_percent = Some(61.0);
        view.usage.context_tokens = Some(122000);
        view.usage.context_window = Some(200000);
        cx.notify();
    });
    click(cx, "context-ring");
    assert!(drawn(cx, "context-ring-used-tokens-122k"));
    assert!(drawn(cx, "context-ring-context-window-200k"));
    assert!(drawn(cx, "context-ring-usage-61%"));
    assert!(!drawn(cx, "context-ring-header-count"));
    assert!(drawn(cx, "context-compact-control"));
    assert!(drawn(cx, "context-auto-compaction"));

    view.update(cx, |view, cx| {
        view.usage.context_percent = Some(25.0);
        view.usage.context_tokens = None;
        view.usage.context_window = Some(100000);
        cx.notify();
    });
    assert!(!drawn(cx, "context-ring-used-tokens-122k"));
    assert!(!drawn(cx, "context-ring-context-window-200k"));
    assert!(!drawn(cx, "context-ring-usage-61%"));
    assert!(drawn(cx, "context-ring-context-window-100k"));
    assert!(drawn(cx, "context-ring-usage-25%"));

    view.update(cx, |view, cx| {
        view.usage.context_percent = None;
        view.usage.context_tokens = Some(48100);
        view.usage.context_window = None;
        cx.notify();
    });
    assert!(drawn(cx, "context-ring-used-tokens-48.1k"));
    assert!(!drawn(cx, "context-ring-context-window-100k"));
    assert!(!drawn(cx, "context-ring-usage-25%"));
    view.update(cx, |view, cx| {
        view.usage.context_tokens = None;
        cx.notify();
    });
    assert!(!drawn(cx, "context-ring-used-tokens-48.1k"));
    assert!(!drawn(cx, "context-ring-header-count"));
}

#[gpui::test]
fn context_ring_advice_updates_at_boundaries_and_keeps_controls(cx: &mut TestAppContext) {
    let (view, cx) = tab(cx);
    view.update(cx, |view, cx| {
        view.usage.context_percent = Some(69.0);
        cx.notify();
    });
    click(cx, "context-ring");
    let low = "Compacting summarizes the conversation to free context.";
    let medium = "Consider compacting or starting a new session.";
    let high = "Context nearly full. Compact or start a new session.";
    for (percent, expected) in [(69.0, low), (70.0, medium), (89.0, medium), (90.0, high)] {
        view.update(cx, |view, cx| {
            view.usage.context_percent = Some(percent);
            cx.notify();
        });
        for text in [low, medium, high] {
            assert_eq!(drawn(cx, text), text == expected, "at {percent}%: {text}");
        }
        assert!(drawn(cx, "Add what to keep with /compact <instructions>."));
        assert!(drawn(cx, "context-ring-advice-divider"));
        assert!(drawn(cx, "context-compact-control"));
        assert!(drawn(cx, "context-auto-compaction"));
    }
}

#[gpui::test]
fn context_ring_replaces_legacy_context_meter(cx: &mut TestAppContext) {
    let (view, cx) = tab(cx);
    view.update(cx, |view, cx| {
        view.usage.context_percent = Some(95.0);
        cx.notify();
    });
    assert!(!drawn(cx, "legacy-context-chip"));
    assert!(!drawn(cx, "context-percent"));
    assert!(!drawn(cx, "context-window"));
    for end in [
        json!({"type": "compaction_end"}),
        json!({"type": "compaction_end", "errorMessage": "failed"}),
    ] {
        click(cx, "context-ring");
        assert!(drawn(cx, "context-compact-control"));
        view.update_in(cx, |view, window, cx| {
            view.handle_batch(
                vec![pi_rpc::Incoming::Event(json!({"type": "compaction_start"}))],
                window,
                cx,
            );
        });
        assert!(!drawn(cx, "context-compact-control"));
        assert!(drawn(cx, "context-compaction-orbit"));
        click(cx, "context-ring");
        cx.simulate_keystrokes("enter");
        assert!(!drawn(cx, "context-compact-control"));
        let at = cx.debug_bounds("context-ring").unwrap().center();
        cx.simulate_mouse_move(at, None, Modifiers::none());
        cx.executor()
            .advance_clock(std::time::Duration::from_secs(2));
        assert!(!drawn(cx, "context-ring-overview-hint"));
        view.update_in(cx, |view, window, cx| {
            view.handle_batch(vec![pi_rpc::Incoming::Event(end)], window, cx);
        });
        assert!(!drawn(cx, "context-compaction-orbit"));
        cx.simulate_mouse_move(
            gpui_kit::point(gpui_kit::px(0.), gpui_kit::px(0.)),
            None,
            Modifiers::none(),
        );
        cx.simulate_mouse_move(at, None, Modifiers::none());
        cx.executor()
            .advance_clock(std::time::Duration::from_secs(2));
        assert!(drawn(cx, "context-ring-overview-hint"));
        click(cx, "context-ring");
        assert!(drawn(cx, "context-compact-control"));
        assert!(drawn(cx, "context-auto-compaction"));
        click(cx, "context-ring");
    }
}

struct SizedSession {
    session: Entity<SessionView>,
    width: f32,
}

impl gpui_kit::Render for SizedSession {
    fn render(
        &mut self,
        _: &mut gpui_kit::Window,
        _: &mut gpui_kit::Context<Self>,
    ) -> impl gpui_kit::IntoElement {
        use gpui_kit::{ParentElement as _, Styled as _};
        gpui_kit::div()
            .w(gpui_kit::px(self.width))
            .h_full()
            .child(self.session.clone())
    }
}

#[gpui::test]
fn context_ring_fits_and_opens_in_every_style_and_narrow_pane(cx: &mut TestAppContext) {
    use gentle_core::input_style::InputStyle;
    use gpui_kit::AppContext as _;

    cx.update(|cx| {
        gpui_kit::init(cx);
        cx.set_reduce_motion(true);
    });
    let (root, cx) = cx.add_window_view(|window, cx| {
        let session = cx.new(|cx| {
            let mut view = SessionView::new(std::env::temp_dir(), None, window, cx, false);
            view.backend.started = true;
            view.usage.context_test_ready = true;
            view.usage.context_stats_received = true;
            view.usage.cost = 0.13;
            view
        });
        SizedSession {
            session,
            width: 700.,
        }
    });
    for style in InputStyle::ALL {
        for width in [700., 320.] {
            cx.update(|_, cx| super::input_style::set_test_style(style, cx));
            root.update(cx, |root, cx| {
                root.width = width;
                root.session.update(cx, |view, cx| {
                    view.pane_width.set(width);
                    cx.notify();
                });
                cx.notify();
            });
            cx.run_until_parked();
            let ring = cx.debug_bounds("context-ring").expect("ring drawn");
            assert!(
                f32::from(ring.left()) >= 0. && f32::from(ring.right()) <= width,
                "{style:?} at {width}: {ring:?}"
            );
            if width == 700. {
                let usage = cx.debug_bounds("context-usage-chip").expect("usage drawn");
                assert!(ring.right() <= usage.left(), "{style:?}: ring beside usage");
            }
            click(cx, "context-ring");
            assert!(drawn(cx, "context-auto-compaction"), "{style:?} at {width}");
            assert!(drawn(cx, "context-compact-control"), "{style:?} at {width}");
            click(cx, "context-ring");
            assert!(!drawn(cx, "context-auto-compaction"));
        }
    }
}

#[gpui::test]
fn a_sent_prompt_can_be_selected(cx: &mut TestAppContext) {
    use gpui_kit::{AppContext as _, MouseButton, base::TextSelection};

    struct FocusedSession {
        session: Entity<SessionView>,
        focus: gpui_kit::FocusHandle,
    }

    impl gpui_kit::Render for FocusedSession {
        fn render(
            &mut self,
            _: &mut gpui_kit::Window,
            _: &mut gpui_kit::Context<Self>,
        ) -> impl gpui_kit::IntoElement {
            use gpui_kit::{InteractiveElement as _, ParentElement as _, Styled as _};
            gpui_kit::div()
                .id("focus")
                .track_focus(&self.focus)
                .size_full()
                .child(self.session.clone())
        }
    }

    cx.update(|cx| {
        gpui_kit::init(cx);
        cx.set_reduce_motion(true);
    });
    let (_root, cx) = cx.add_window_view(|window, cx| {
        let focus = cx.focus_handle();
        window.focus(&focus, cx);
        let session = cx.new(|cx| {
            let mut view = SessionView::new(std::env::temp_dir(), None, window, cx, false);
            view.backend.started = true;
            view.transcript
                .items
                .push(gentle_core::transcript::Item::User {
                    text: "fix the parser".into(),
                    images: Vec::new(),
                });
            view.sync_view(cx);
            view
        });
        let view = cx.new(|_| FocusedSession { session, focus });
        gpui_kit::base::Root::new(view, window, cx)
    });
    cx.run_until_parked();
    let bounds = cx
        .debug_bounds("user-text-0")
        .expect("the sent prompt is drawn");
    let (start, end) = (
        gpui_kit::point(bounds.left() + gpui_kit::px(1.), bounds.center().y),
        gpui_kit::point(bounds.right() - gpui_kit::px(1.), bounds.center().y),
    );
    cx.simulate_mouse_down(start, MouseButton::Left, Modifiers::none());
    cx.simulate_mouse_move(end, Some(MouseButton::Left), Modifiers::none());
    cx.simulate_mouse_up(end, MouseButton::Left, Modifiers::none());
    cx.update(|window, cx| {
        let _ = window.draw(cx);
        assert_eq!(TextSelection::selected_text(window, cx), "fix the parser\n");
        cx.write_to_clipboard(gpui_kit::ClipboardItem::new_string("sentinel".into()));
    });
    cx.simulate_keystrokes(if cfg!(target_os = "macos") {
        "cmd-c"
    } else {
        "ctrl-c"
    });
    cx.run_until_parked();
    assert_eq!(
        cx.update(|_, cx| cx.read_from_clipboard().and_then(|item| item.text())),
        Some("fix the parser".into())
    );
}

/// Selecting a sent bubble copies the displayed text, without matched delimiters.
#[gpui::test]
fn user_bubble_inline_code_selection_omits_matched_delimiters(cx: &mut TestAppContext) {
    use gpui_kit::{AppContext as _, MouseButton, base::TextSelection};

    cx.update(|cx| {
        gpui_kit::init(cx);
        cx.set_reduce_motion(true);
    });
    let (_root, cx) = cx.add_window_view(|window, cx| {
        let session = cx.new(|cx| {
            let mut view = SessionView::new(std::env::temp_dir(), None, window, cx, false);
            view.backend.started = true;
            view.transcript
                .items
                .push(gentle_core::transcript::Item::User {
                    text: "fix `café` and `x` then `open".into(),
                    images: Vec::new(),
                });
            view.sync_view(cx);
            view
        });
        Selectable(session)
    });
    cx.run_until_parked();
    let bounds = cx.debug_bounds("user-text-0").expect("sent prompt");
    let start = gpui_kit::point(bounds.left() + gpui_kit::px(1.), bounds.center().y);
    let end = gpui_kit::point(bounds.right() - gpui_kit::px(1.), bounds.center().y);
    cx.simulate_mouse_down(start, MouseButton::Left, Modifiers::none());
    cx.simulate_mouse_move(end, Some(MouseButton::Left), Modifiers::none());
    cx.simulate_mouse_up(end, MouseButton::Left, Modifiers::none());
    cx.update(|window, cx| {
        let _ = window.draw(cx);
        assert_eq!(
            TextSelection::selected_text(window, cx),
            "fix café and x then `open\n"
        );
    });
    cx.simulate_keystrokes(if cfg!(target_os = "macos") {
        "cmd-c"
    } else {
        "ctrl-c"
    });
    cx.run_until_parked();
    assert_eq!(
        cx.update(|_, cx| cx
            .read_from_clipboard()
            .and_then(|item| item.text())
            .unwrap_or_default()),
        "fix café and x then `open"
    );
}

/// Authored soft breaks stay visible and selection copies the formatted text.
#[gpui::test]
fn user_bubble_authored_newlines_and_clipboard(cx: &mut TestAppContext) {
    use gpui_kit::{AppContext as _, MouseButton, point, px};

    cx.update(|cx| {
        gpui_kit::init(cx);
        cx.set_reduce_motion(true);
    });
    let (_, cx) = cx.add_window_view(|window, cx| {
        let session = cx.new(|cx| {
            let mut view = SessionView::new(std::env::temp_dir(), None, window, cx, false);
            view.backend.started = true;
            view.transcript
                .items
                .push(gentle_core::transcript::Item::User {
                    text: "1- `code`\n2- **bold**\n3- ~~strike~~".into(),
                    images: Vec::new(),
                });
            view.sync_view(cx);
            view
        });
        Selectable(session)
    });
    cx.run_until_parked();
    let bounds = cx.debug_bounds("user-text-0").expect("sent prompt");
    let start = point(bounds.left() + px(1.), bounds.top() + px(5.));
    let first_end = point(bounds.right() - px(1.), bounds.top() + px(5.));
    cx.simulate_mouse_down(start, MouseButton::Left, Modifiers::none());
    cx.simulate_mouse_move(first_end, Some(MouseButton::Left), Modifiers::none());
    cx.simulate_mouse_up(first_end, MouseButton::Left, Modifiers::none());
    cx.simulate_keystrokes(if cfg!(target_os = "macos") {
        "cmd-c"
    } else {
        "ctrl-c"
    });
    cx.run_until_parked();
    let partial = cx.update(|_, cx| {
        cx.read_from_clipboard()
            .and_then(|item| item.text())
            .unwrap_or_default()
    });
    assert_eq!(partial, "1- code");

    let end = point(bounds.right() - px(1.), bounds.bottom() - px(5.));
    cx.simulate_mouse_down(start, MouseButton::Left, Modifiers::none());
    cx.simulate_mouse_move(end, Some(MouseButton::Left), Modifiers::none());
    cx.simulate_mouse_up(end, MouseButton::Left, Modifiers::none());
    cx.simulate_keystrokes(if cfg!(target_os = "macos") {
        "cmd-c"
    } else {
        "ctrl-c"
    });
    cx.run_until_parked();
    let clipboard = cx.update(|_, cx| {
        cx.read_from_clipboard()
            .and_then(|item| item.text())
            .unwrap_or_default()
    });
    assert!(
        bounds.size.height > px(40.),
        "three authored lines must occupy three rows"
    );
    assert_eq!(clipboard, "1- code\n2- bold\n3- strike");
}

/// Existing list and fenced-code blocks retain their rendered lines and copy text.
#[gpui::test]
fn user_bubble_list_and_fenced_code_preserved(cx: &mut TestAppContext) {
    use gpui_kit::{AppContext as _, MouseButton, point, px};

    cx.update(|cx| {
        gpui_kit::init(cx);
        cx.set_reduce_motion(true);
    });
    for (source, expected) in [
        ("- one\n- two\n- three", "one\ntwo\nthree"),
        ("```\none\ntwo\nthree\n```", "one\ntwo\nthree"),
    ] {
        let (_, cx) = cx.add_window_view(|window, cx| {
            let session = cx.new(|cx| {
                let mut view = SessionView::new(std::env::temp_dir(), None, window, cx, false);
                view.backend.started = true;
                view.transcript
                    .items
                    .push(gentle_core::transcript::Item::User {
                        text: source.into(),
                        images: Vec::new(),
                    });
                view.sync_view(cx);
                view
            });
            Selectable(session)
        });
        cx.run_until_parked();
        let bounds = cx.debug_bounds("user-text-0").expect("sent prompt");
        assert!(
            bounds.size.height > px(40.),
            "block must occupy multiple rows: {source:?}"
        );
        let start = point(bounds.left() + px(1.), bounds.top() + px(5.));
        let end = point(bounds.right() - px(1.), bounds.bottom() - px(5.));
        cx.simulate_mouse_down(start, MouseButton::Left, Modifiers::none());
        cx.simulate_mouse_move(end, Some(MouseButton::Left), Modifiers::none());
        cx.simulate_mouse_up(end, MouseButton::Left, Modifiers::none());
        cx.simulate_keystrokes(if cfg!(target_os = "macos") {
            "cmd-c"
        } else {
            "ctrl-c"
        });
        cx.run_until_parked();
        let clipboard = cx.update(|_, cx| {
            cx.read_from_clipboard()
                .and_then(|item| item.text())
                .unwrap_or_default()
        });
        assert_eq!(clipboard, expected, "source: {source:?}");
    }
}

fn assert_user_bubble_copy(
    cx: &mut TestAppContext,
    source: &str,
    expected: &str,
    start_inset: Option<f32>,
    end_inset: f32,
) {
    use gpui_kit::{AppContext as _, MouseButton, base::TextSelection, point, px};

    cx.update(|cx| {
        gpui_kit::init(cx);
        cx.set_reduce_motion(true);
    });
    let (_, cx) = cx.add_window_view(|window, cx| {
        let session = cx.new(|cx| {
            let mut view = SessionView::new(std::env::temp_dir(), None, window, cx, false);
            view.backend.started = true;
            view.transcript
                .items
                .push(gentle_core::transcript::Item::User {
                    text: source.into(),
                    images: Vec::new(),
                });
            view.sync_view(cx);
            view
        });
        Selectable(session)
    });
    cx.run_until_parked();
    let bounds = cx.debug_bounds("user-text-0").expect("sent prompt");
    let end = point(bounds.right() - px(end_inset), bounds.bottom() - px(10.));
    let start_inset = start_inset.unwrap_or_else(|| {
        // Find a hit inside the first glyph's leading half rather than assuming
        // the list marker's indentation and glyph metrics in pixels.
        (1..200)
            .find(|x| {
                let start = point(bounds.left() + px(*x as f32), bounds.top() + px(10.));
                cx.simulate_mouse_down(start, MouseButton::Left, Modifiers::none());
                cx.simulate_mouse_move(end, Some(MouseButton::Left), Modifiers::none());
                cx.simulate_mouse_up(end, MouseButton::Left, Modifiers::none());
                cx.update(|window, cx| {
                    let _ = window.draw(cx);
                    TextSelection::selected_text(window, cx).trim_end_matches('\n') == expected
                })
            })
            .expect("selection reached the first glyph") as f32
    });
    let start = point(bounds.left() + px(start_inset), bounds.top() + px(10.));
    cx.simulate_mouse_down(start, MouseButton::Left, Modifiers::none());
    cx.simulate_mouse_move(end, Some(MouseButton::Left), Modifiers::none());
    cx.simulate_mouse_up(end, MouseButton::Left, Modifiers::none());
    let selected = cx.update(|window, cx| {
        let _ = window.draw(cx);
        TextSelection::selected_text(window, cx)
    });
    cx.simulate_keystrokes(if cfg!(target_os = "macos") {
        "cmd-c"
    } else {
        "ctrl-c"
    });
    cx.run_until_parked();
    let clipboard = cx.update(|_, cx| {
        cx.read_from_clipboard()
            .and_then(|item| item.text())
            .unwrap_or_default()
    });
    assert_eq!(selected.trim_end_matches('\n'), expected, "selection");
    assert_eq!(clipboard, expected, "clipboard");
}

#[gpui::test]
fn user_bubble_complete_html_fragments_are_literal(cx: &mut TestAppContext) {
    for (source, expected) in [
        (
            "<div><span>hola</span></div>",
            "<div><span>hola</span></div>",
        ),
        (
            "**outside** <span title=\"**attr**\">**inside**</span> **after**",
            "outside <span title=\"**attr**\">**inside**</span> after",
        ),
        (
            "<div><br><span title=\"a > b\">**hola**</span></div>",
            "<div><br><span title=\"a > b\">**hola**</span></div>",
        ),
    ] {
        assert_user_bubble_copy(cx, source, expected, Some(1.), 1.);
    }
}

#[gpui::test]
fn user_bubble_html_comment_does_not_close_fragment(cx: &mut TestAppContext) {
    let source = "<div><!-- </div> -->**inside**</div>";
    assert_user_bubble_copy(cx, source, source, Some(1.), 1.);
}

#[gpui::test]
fn user_bubble_folded_html_keeps_content_literal(cx: &mut TestAppContext) {
    let source = "<div>\n**one**\n**two**\n**three**\n**four**\n**five**\n**six**\n**seven**\n**eight**\n**nine**\n</div>";
    let expected = "<div>\n**one**\n**two**\n**three**\n**four**\n**five**\n**six**\n**seven**";
    assert_user_bubble_copy(cx, source, expected, Some(1.), 1.);
}

#[gpui::test]
fn user_bubble_html_indented_content_has_no_escape_backslashes(cx: &mut TestAppContext) {
    let source = "<div>\n\n    **literal**\n</div>";
    assert_user_bubble_copy(cx, source, source, Some(1.), 1.);
}

#[gpui::test]
fn user_bubble_html_tab_indentation_is_literal(cx: &mut TestAppContext) {
    let source = "<div>\n\n\t**literal**\n</div>";
    assert_user_bubble_copy(cx, source, source, Some(1.), 1.);
}

#[gpui::test]
fn user_bubble_raw_html_is_visible(cx: &mut TestAppContext) {
    assert_user_bubble_copy(
        cx,
        "<span title=\"**important**\">text</span>",
        "<span title=\"**important**\">text</span>",
        Some(1.),
        1.,
    );
    assert_user_bubble_copy(
        cx,
        "hello <div>world</div>",
        "hello <div>world</div>",
        Some(1.),
        1.,
    );
    assert_user_bubble_copy(
        cx,
        "<div>\nhello\n</div>",
        "<div>\nhello\n</div>",
        Some(1.),
        1.,
    );
    assert_user_bubble_copy(
        cx,
        "**bold** `<div>` [link](https://example.com) &amp;",
        "bold <div> link &",
        Some(1.),
        1.,
    );
}

#[gpui::test]
fn user_bubble_container_fenced_code_copy(cx: &mut TestAppContext) {
    assert_user_bubble_copy(
        cx,
        "- item\n\n  ```\n  one\n  two\n  ```",
        "item\none\ntwo",
        None,
        8.,
    );
}

#[gpui::test]
fn user_bubble_triple_backtick_inline_copy(cx: &mut TestAppContext) {
    assert_user_bubble_copy(
        cx,
        "```code```\nnext\nlast",
        "code\nnext\nlast",
        Some(1.),
        8.,
    );
}

#[gpui::test]
fn user_bubble_backslash_hard_break_copy(cx: &mut TestAppContext) {
    assert_user_bubble_copy(cx, "one\\\ntwo", "one\ntwo", Some(1.), 1.);
}

/// ↑ and ↓ in the composer walk the prompts sent before and come back to
/// the draft; inside a multi-line prompt ↑ moves the cursor first.
#[gpui::test]
fn arrows_recall_sent_prompts_and_return_to_the_draft(cx: &mut TestAppContext) {
    let (view, cx) = tab(cx);
    view.update(cx, |view, _| {
        view.prompt_history.record("first");
        view.prompt_history.record("second\nline two");
    });
    cx.update(|window, cx| {
        let composer = view.read(cx).composer.clone();
        composer.update(cx, |state, cx| {
            state.focus(window, cx);
            state.set_value("draft", window, cx);
        });
    });
    let composer = |cx: &mut VisualTestContext| {
        cx.run_until_parked();
        view.read_with(cx, |view, cx| view.composer.read(cx).value().to_string())
    };
    assert_eq!(composer(cx), "draft");

    cx.simulate_keystrokes("up");
    assert_eq!(composer(cx), "second\nline two");
    // The cursor sits at the end, on the second line: ↑ moves it up first.
    cx.simulate_keystrokes("up");
    assert_eq!(composer(cx), "second\nline two");
    cx.simulate_keystrokes("up");
    assert_eq!(composer(cx), "first");
    cx.simulate_keystrokes("up");
    assert_eq!(composer(cx), "first", "nothing older than the oldest");
    cx.simulate_keystrokes("down");
    assert_eq!(composer(cx), "second\nline two");
    cx.simulate_keystrokes("down");
    assert_eq!(composer(cx), "draft");
}

/// Esc closes what is open first; with nothing open it stops the running
/// turn, unless it comes right after an Esc that closed something.
#[gpui::test]
fn escape_closes_first_then_stops_the_turn(cx: &mut TestAppContext) {
    let (view, cx) = tab(cx);
    let escape = |cx: &mut VisualTestContext| {
        cx.update(|window, cx| {
            view.update(cx, |view, cx| view.abort(&super::Abort, window, cx));
        });
    };
    let stopping = |cx: &mut VisualTestContext| view.read_with(cx, |view, _| view.live.stopping);
    view.update(cx, |view, cx| {
        view.transcript.running = true;
        view.sync_view(cx);
        view.agents_panel.open = true;
    });
    assert!(view.read_with(cx, |view, _| view.turn_since().is_some()));

    escape(cx);
    assert!(view.read_with(cx, |view, _| !view.agents_panel.open));
    assert_eq!(stopping(cx), None, "the first Esc only closed the panel");
    escape(cx);
    assert_eq!(stopping(cx), None, "a quick second Esc doesn't stop");

    view.update(cx, |view, _| view.live.escape_closed = None);
    escape(cx);
    let turn = view.read_with(cx, |view, _| view.turn_since());
    assert_eq!(stopping(cx), turn, "with nothing open, Esc stops the turn");
}

/// Moving an agent to another folder asks first only when that would lose
/// something: what was said, a running turn or a conversation about to resume.
#[gpui::test]
fn has_conversation_counts_turns_and_pending_resumes(cx: &mut TestAppContext) {
    let (view, cx) = tab(cx);
    let has = |cx: &mut VisualTestContext| view.read_with(cx, |view, _| view.has_conversation());
    assert!(!has(cx), "a fresh tab has nothing to lose");

    view.update(cx, |view, _| view.transcript.running = true);
    assert!(has(cx), "a running turn counts");

    view.update(cx, |view, _| {
        view.transcript.running = false;
        view.backend.pending_session = Some("saved.jsonl".into());
    });
    assert!(has(cx), "a conversation waiting to be resumed counts");
}

/// Changing folder starts fresh: a conversation waiting to be resumed is
/// dropped instead of resuming in the new folder.
#[gpui::test]
fn change_cwd_drops_a_pending_resume(cx: &mut TestAppContext) {
    let (view, cx) = tab(cx);
    // Not started yet: the next start would resume it (`--session`).
    view.update(cx, |view, _| {
        view.backend.started = false;
        view.backend.pending_session = Some("saved.jsonl".into());
    });
    let folder = std::env::temp_dir().join("elsewhere");
    view.update_in(cx, |view, window, cx| {
        view.change_cwd(folder.clone(), window, cx)
    });
    view.read_with(cx, |view, _| {
        assert_eq!(view.cwd(), &folder);
        assert!(view.backend.pending_session.is_none());
        assert!(!view.has_conversation());
    });
}

/// A host tool's call or cancel as the server's stream carries it.
fn host_event(seq: u64, event: &str, data: Value) -> pi_rpc::Incoming {
    pi_rpc::Incoming::Ui(widget(json!({
        "schema": SCHEMA, "kind": "event", "seq": seq, "event": event, "data": data,
    })))
}

#[gpui::test]
fn host_tool_calls_reach_their_handler_once_and_answer_unless_cancelled(cx: &mut TestAppContext) {
    use super::{CanvasAnswer, HostToolRequest, SessionEvent};
    use pi_rpc::desktop_rpc::HostToolContent;

    let (view, cx) = tab(cx);
    view.update(cx, |view, _| {
        view.set_canvas(std::env::temp_dir().join("board.json"));
        // This tab runs on a server as device dev_1.
        view.runs_on = Some(gentle_core::backend::Backend::Server {
            url: "http://127.0.0.1:8787".into(),
            device: "dev_1".into(),
            name: "pc".into(),
        });
    });
    // A fake canvas, as the canvas view answers its agent's requests.
    let asked = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
    let seen = asked.clone();
    cx.update(|_, cx| {
        cx.subscribe(&view, move |_, event: &SessionEvent, _| {
            if let SessionEvent::HostTool(HostToolRequest::Canvas { request, reply }) = event {
                seen.borrow_mut().push(request.clone());
                let _ = reply.send(CanvasAnswer {
                    json: json!({"ok": true, "shapes": [], "view": [0, 0, 10, 10]}),
                    shot: None,
                });
            }
        })
        .detach();
    });
    let call = |seq: u64, id: &str, tool: &str| {
        host_event(
            seq,
            "host_tool_call",
            json!({"callId": id, "provider": "dev_1", "tool": tool, "args": {}}),
        )
    };
    view.update_in(cx, |view, window, cx| {
        view.handle_batch(
            vec![
                call(1, "c1", "canvas_read"),
                // Re-sent after a reconnect, with its old sequence: once.
                call(1, "c1", "canvas_read"),
                call(2, "c2", "no_such_tool"),
                call(3, "c3", "canvas_read"),
                host_event(4, "host_tool_cancel", json!({"callId": "c3"})),
                // Another device's tool on the same session: not this tab's.
                host_event(
                    5,
                    "host_tool_call",
                    json!({"callId": "c5", "provider": "dev_2", "tool": "canvas_read", "args": {}}),
                ),
            ],
            window,
            cx,
        );
    });
    cx.run_until_parked();
    assert_eq!(asked.borrow().len(), 2, "c1 and c3 reached the canvas");
    assert_eq!(asked.borrow()[0], json!({"action": "read"}));
    view.update(cx, |view, _| {
        let sent = &view.host.sent;
        let ids: Vec<&str> = sent.iter().map(|(id, _)| id.as_str()).collect();
        // c2 answers at once; c1 once the canvas did; c3 was cancelled.
        assert_eq!(ids, ["c2", "c1"]);
        assert!(sent[0].1.is_error);
        let HostToolContent::Text { text } = &sent[1].1.content[0] else {
            panic!("expected text")
        };
        assert!(!sent[1].1.is_error);
        assert!(text.contains("shapes"), "{text}");
    });
}
