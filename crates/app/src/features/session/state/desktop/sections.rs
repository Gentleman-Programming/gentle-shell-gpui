//! Gentle Shell's session state over the channel (`get_snapshot { sections }`
//! and the `session_snapshot` event) in one tab.
//!
//! When the peer advertises `session_snapshot`, the channel's own probe and
//! resync reads ask for [`SECTIONS`], so connecting and every sequence gap
//! re-read the whole copy, and each event merges only the sections that
//! changed. This slice keeps that typed, path-free copy; the tab reads what
//! it shows from it. A Shell without the feed (4.8.0) never advertises the
//! event: nothing is asked and the copy stays empty, so every screen keeps
//! its own source.

use std::time::{Duration, Instant};

use gentle_core::commands::SlashCommand;
use gentle_core::tools::TodoTask;
use pi_rpc::desktop_rpc::{
    Capabilities, CommandSource, DesktopEvent, SessionContext, SessionPendingQuestion,
    SessionSection, SessionSections,
};

/// The event that carries the sections that changed.
pub(in crate::features::session) const SESSION_SNAPSHOT: &str = "session_snapshot";

/// The sections the tab reads. The others have their own sources here (the
/// supervision lists, the Profiles screen, git, `get_session_stats` for the
/// cost), so they are not asked.
pub(in crate::features::session) const SECTIONS: [SessionSection; 5] = [
    SessionSection::Context,
    SessionSection::Todo,
    SessionSection::Activity,
    SessionSection::PendingQuestion,
    SessionSection::Commands,
];

/// How long a pending question waits before the activity line mentions it:
/// a question the app can answer arrives as its own dialog within this.
pub(in crate::features::session) const QUESTION_GRACE: Duration = Duration::from_secs(1);

#[derive(Default)]
pub(in crate::features::session) struct Sections {
    /// The peer advertised `session_snapshot` (and `get_snapshot`).
    supported: bool,
    /// A sections read failed on this backend: the channel's reads are the
    /// legacy `{}` from here on, so one failure never stops the channel.
    failed: bool,
    /// The merged copy, once a read carried it.
    current: Option<SessionSections>,
    /// When the pending question on record was first seen, by its
    /// `openedAt`.
    question_seen: Option<(String, Instant)>,
    /// The copy moved since [`Self::take_changed`].
    changed: bool,
}

impl Sections {
    pub(in crate::features::session) fn set_capabilities(&mut self, capabilities: &Capabilities) {
        self.supported = capabilities.supports_session_sections();
    }

    /// Whether the channel's `get_snapshot` asks for [`SECTIONS`].
    pub(in crate::features::session) fn wanted(&self) -> bool {
        self.supported && !self.failed
    }

    /// A sections read was refused: keep the channel on the legacy read.
    pub(in crate::features::session) fn fail(&mut self) {
        self.failed = true;
    }

    /// A full read (probe or resync): it replaces the copy.
    pub(in crate::features::session) fn replace(&mut self, sections: SessionSections) {
        self.current = Some(sections);
        self.moved();
    }

    /// A `session_snapshot` event, applied only when the peer advertised it
    /// and a full read already set the copy: the event carries only what
    /// changed, so before that there is nothing to merge it into (the read
    /// that follows covers it). Any other event is not this slice's.
    pub(in crate::features::session) fn apply_event(&mut self, event: &DesktopEvent) {
        if event.event != SESSION_SNAPSHOT || !self.supported {
            return;
        }
        let (Some(current), Ok(update)) = (self.current.as_mut(), event.session_snapshot()) else {
            return;
        };
        merge(current, update.sections);
        self.moved();
    }

    /// The backend was replaced or the conversation changed: the copy goes,
    /// and the tab hears it moved.
    pub(in crate::features::session) fn reset(&mut self) {
        *self = Self {
            changed: self.current.is_some(),
            ..Self::default()
        };
    }

    /// Whether the copy moved since the last call.
    pub(in crate::features::session) fn take_changed(&mut self) -> bool {
        std::mem::take(&mut self.changed)
    }

    /// The todo list as the island shows it, when the feed reports one.
    pub(in crate::features::session) fn todo(&self) -> Option<Vec<TodoTask>> {
        let todo = self.current.as_ref()?.todo.as_ref()?;
        Some(
            todo.items
                .iter()
                .map(|item| TodoTask {
                    title: item.title.clone(),
                    status: item.state.clone(),
                    note: item.note.clone(),
                })
                .collect(),
        )
    }

    /// The context use: `None` until the feed reported it, `Some(None)` when
    /// the feed has no number (no model yet).
    pub(in crate::features::session) fn context(&self) -> Option<Option<&SessionContext>> {
        self.current.as_ref()?.context.as_ref().map(Option::as_ref)
    }

    /// The ODD phase label (`implementing…`), while the feed reports one.
    pub(in crate::features::session) fn phase_label(&self) -> Option<&str> {
        self.current
            .as_ref()?
            .activity
            .as_ref()?
            .phase_label
            .as_deref()
    }

    /// The question the session waits on, once it has waited
    /// [`QUESTION_GRACE`] at `now`.
    pub(in crate::features::session) fn pending_question(
        &self,
        now: Instant,
    ) -> Option<&SessionPendingQuestion> {
        let question = self.current.as_ref()?.pending_question.as_ref()?.as_ref()?;
        let (_, seen) = self.question_seen.as_ref()?;
        (now.saturating_duration_since(*seen) >= QUESTION_GRACE).then_some(question)
    }

    /// Whether a question waits that is not shown yet.
    pub(in crate::features::session) fn question_pending(&self) -> bool {
        self.question_seen.is_some()
    }

    /// Pi's built-in commands the feed lists (`source: builtin`), with the
    /// ones that only work in the terminal marked.
    pub(in crate::features::session) fn pi_commands(&self) -> Vec<SlashCommand> {
        self.current
            .as_ref()
            .and_then(|sections| sections.commands.as_ref())
            .map(|commands| {
                commands
                    .items
                    .iter()
                    .filter(|command| command.source == CommandSource::Builtin)
                    .map(|command| SlashCommand {
                        name: command.name.clone(),
                        description: command.description.clone().unwrap_or_default(),
                        terminal: command.terminal,
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    fn moved(&mut self) {
        self.changed = true;
        let opened = self
            .current
            .as_ref()
            .and_then(|sections| sections.pending_question.as_ref()?.as_ref())
            .map(|question| question.opened_at.clone());
        self.question_seen = match (opened, self.question_seen.take()) {
            (Some(opened), Some((seen, at))) if seen == opened => Some((seen, at)),
            (Some(opened), _) => Some((opened, Instant::now())),
            (None, _) => None,
        };
    }
}

/// Fold an event's sections into the copy: each section it carries replaces
/// the one held (a `null` included), the rest stay.
fn merge(current: &mut SessionSections, update: SessionSections) {
    fn set<T>(slot: &mut Option<T>, value: Option<T>) {
        if value.is_some() {
            *slot = value;
        }
    }
    let SessionSections {
        project,
        model,
        context,
        cost,
        usage,
        profiles,
        permission,
        todo,
        changes,
        agents,
        jobs,
        review,
        stats,
        activity,
        pending_question,
        commands,
    } = update;
    set(&mut current.project, project);
    set(&mut current.model, model);
    set(&mut current.context, context);
    set(&mut current.cost, cost);
    set(&mut current.usage, usage);
    set(&mut current.profiles, profiles);
    set(&mut current.permission, permission);
    set(&mut current.todo, todo);
    set(&mut current.changes, changes);
    set(&mut current.agents, agents);
    set(&mut current.jobs, jobs);
    set(&mut current.review, review);
    set(&mut current.stats, stats);
    set(&mut current.activity, activity);
    set(&mut current.pending_question, pending_question);
    set(&mut current.commands, commands);
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use pi_rpc::desktop_rpc::{DesktopMethod, PROMPT_COMMAND, SCHEMA, WIDGET_KEY};
    use pi_rpc::{UiMethod, UiRequest};
    use serde_json::{Value, json};

    use super::super::{Desktop, Send, Step};

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

    fn capabilities(request_id: &str, events: &[&str]) -> UiRequest {
        response(
            request_id,
            json!({
                "schema": SCHEMA, "command": PROMPT_COMMAND, "widget": WIDGET_KEY,
                "methods": ["capabilities", "get_snapshot"], "events": events,
            }),
        )
    }

    fn session() -> Value {
        json!({
            "context": {"percent": 42.5, "tokens": 85000, "window": 200000},
            "todo": {
                "items": [
                    {"id": 1, "title": "Map", "state": "done", "note": null},
                    {"id": 2, "title": "Build", "state": "in_progress", "note": "half"},
                ],
                "progress": {"done": 1, "total": 2, "open": 1},
                "current": "Build",
                "truncated": false,
            },
            "activity": {
                "state": "working", "streaming": true, "phase": "implementing",
                "phaseLabel": "implementing…", "blockedLabel": null,
                "agents": 0, "jobs": 0, "label": "Working",
            },
            "pendingQuestion": null,
            "commands": {"items": [
                {"name": "new", "description": "Start a new session", "args": null, "terminal": false, "source": "builtin"},
                {"name": "model", "description": "Select model", "args": null, "terminal": true, "source": "builtin"},
                {"name": "gentle:jobs", "description": "Jobs", "args": null, "terminal": true, "source": "extension"},
            ], "truncated": false},
        })
    }

    fn snapshot(request_id: &str, sequence: u64, session: Option<Value>) -> UiRequest {
        let mut data =
            json!({"schema": SCHEMA, "sequence": sequence, "state": {"config": {"stores": []}}});
        if let Some(session) = session {
            data["state"]["session"] = session;
        }
        response(request_id, data)
    }

    fn event(seq: u64, sections: Value) -> UiRequest {
        widget(json!({
            "schema": SCHEMA, "kind": "event", "seq": seq,
            "event": "session_snapshot", "data": {"sections": sections},
        }))
    }

    fn params(send: &Send) -> Value {
        serde_json::from_str::<Value>(send.request.payload()).unwrap()["params"].clone()
    }

    /// Probe a channel whose capabilities list `events`; the snapshot request
    /// it sends is returned unanswered.
    fn probe(events: &[&str]) -> (Desktop, Send) {
        let mut desktop = Desktop::new();
        let first = desktop.ready(true).sends.remove(0);
        let mut sends = desktop
            .ui(&capabilities(first.request.request_id(), events))
            .sends;
        (desktop, sends.remove(0))
    }

    const MAIN: &[&str] = &["session_snapshot", "jobs_update"];

    #[test]
    fn a_shell_without_the_feed_keeps_the_legacy_snapshot_read() {
        let (mut desktop, send) = probe(&["jobs_update"]);
        assert_eq!(send.request.method(), DesktopMethod::GetSnapshot);
        assert_eq!(params(&send), json!({}), "4.8.0 is asked exactly as before");
        desktop.ui(&snapshot(send.request.request_id(), 3, None));
        assert_eq!(desktop.step(), Step::Ready);
        // Even an unadvertised event changes nothing.
        desktop.ui(&event(4, session()));
        assert!(desktop.sections().todo().is_none());
        assert!(desktop.sections().pi_commands().is_empty());
        assert!(!desktop.take_sections_changed());
    }

    #[test]
    fn the_probe_reads_the_sections_and_events_merge_what_changed() {
        let (mut desktop, send) = probe(MAIN);
        assert_eq!(
            params(&send),
            json!({"sections": ["context", "todo", "activity", "pendingQuestion", "commands"]})
        );
        desktop.ui(&snapshot(send.request.request_id(), 3, Some(session())));
        assert!(desktop.take_sections_changed());
        let sections = desktop.sections();
        let todo = sections.todo().unwrap();
        assert_eq!(
            (
                todo[1].title.as_str(),
                todo[1].status.as_str(),
                todo[1].note.as_deref()
            ),
            ("Build", "in_progress", Some("half"))
        );
        assert_eq!(sections.phase_label(), Some("implementing…"));
        assert_eq!(
            sections.context().flatten().and_then(|c| c.percent),
            Some(42.5)
        );
        let commands: Vec<(String, bool)> = sections
            .pi_commands()
            .into_iter()
            .map(|c| (c.name, c.terminal))
            .collect();
        assert_eq!(
            commands,
            [("new".to_string(), false), ("model".to_string(), true)],
            "only Pi's built-ins; extensions come from get_commands"
        );

        // Only the todo changed: the rest stays.
        desktop.ui(&event(
            4,
            json!({"todo": {"items": [], "progress": {"done": 0, "total": 0, "open": 0}, "current": null, "truncated": false},
                   "context": null}),
        ));
        assert!(desktop.take_sections_changed());
        let sections = desktop.sections();
        assert_eq!(sections.todo(), Some(Vec::new()));
        assert_eq!(sections.context().map(|c| c.is_none()), Some(true));
        assert_eq!(sections.phase_label(), Some("implementing…"));
    }

    #[test]
    fn a_gap_rereads_every_section() {
        let (mut desktop, send) = probe(MAIN);
        desktop.ui(&snapshot(send.request.request_id(), 3, Some(session())));
        let sends = desktop.ui(&event(6, json!({}))).sends;
        assert_eq!(sends.len(), 1, "a gap asks again");
        assert_eq!(
            params(&sends[0])["sections"].as_array().map(Vec::len),
            Some(5)
        );
        let mut fresh = session();
        fresh["activity"]["phaseLabel"] = json!("verifying…");
        desktop.ui(&snapshot(sends[0].request.request_id(), 6, Some(fresh)));
        assert_eq!(desktop.sections().phase_label(), Some("verifying…"));
    }

    #[test]
    fn a_refused_sections_read_falls_back_to_the_legacy_read() {
        let (mut desktop, send) = probe(MAIN);
        let refused = widget(json!({
            "schema": SCHEMA, "kind": "response", "requestId": send.request.request_id(),
            "ok": false, "error": {"code": "session_unavailable", "message": "no feed"},
        }));
        let sends = desktop.ui(&refused).sends;
        assert_eq!(sends.len(), 1);
        assert_eq!(params(&sends[0]), json!({}));
        let applied = desktop.ui(&snapshot(sends[0].request.request_id(), 2, None));
        assert!(applied.report.is_none());
        assert_eq!(
            desktop.step(),
            Step::Ready,
            "the channel itself keeps working"
        );
        // A later gap reads the legacy way too.
        let sends = desktop.ui(&event(5, json!({}))).sends;
        assert_eq!(params(&sends[0]), json!({}));
    }

    #[test]
    fn a_pending_question_shows_after_its_grace_and_reset_forgets_it() {
        let (mut desktop, send) = probe(MAIN);
        desktop.ui(&snapshot(send.request.request_id(), 1, Some(session())));
        desktop.ui(&event(
            2,
            json!({"pendingQuestion": {
                "kind": "consent", "tool": null, "toolCallId": null, "question": "Allow the push?",
                "header": null, "options": [], "multi": false, "custom": false, "questions": [],
                "promptKind": null, "terminalOnly": true, "openedAt": "2026-11-17T10:00:00Z",
            }}),
        ));
        let sections = desktop.sections();
        assert!(sections.question_pending());
        let now = Instant::now();
        assert!(sections.pending_question(now).is_none(), "not yet");
        let later = now + Duration::from_secs(2);
        let question = sections.pending_question(later).unwrap();
        assert!(question.terminal_only);
        assert_eq!(question.question, "Allow the push?");

        desktop.ui(&event(3, json!({"pendingQuestion": null})));
        assert!(!desktop.sections().question_pending());

        desktop.reset();
        assert!(desktop.take_sections_changed());
        assert!(desktop.sections().todo().is_none());
    }
}
