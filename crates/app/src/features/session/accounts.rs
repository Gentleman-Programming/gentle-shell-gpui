//! The events of a sign-in or a package job the Shell runs for a screen
//! (`login_prompt`, `login_finished`, `package_progress`,
//! `package_finished`): the requests that start them go through the reads
//! queue ([`super::ReadRequest`]); their events arrive here, whichever
//! request is pending, and wait for the screen that started them to take
//! them. A session followed by several devices sees the others' too: the
//! screen keeps only the ids it started.

use gpui_kit::*;
use pi_rpc::desktop_rpc::{
    DesktopEvent, DesktopRpcError, LOGIN_FINISHED, LOGIN_PROMPT, LoginFinished, LoginPrompt,
    PACKAGE_FINISHED, PACKAGE_PROGRESS, PackageFinished, PackageProgress,
};

use super::SessionView;

/// Events nobody took (no screen open) are not kept forever.
const MAX_EVENTS: usize = 256;

/// One sign-in or package event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum AccountEvent {
    LoginPrompt(LoginPrompt),
    LoginFinished(LoginFinished),
    PackageProgress(PackageProgress),
    PackageFinished(PackageFinished),
}

impl AccountEvent {
    /// The event decoded, `None` for one that doesn't fit its shape.
    fn decode(event: &DesktopEvent) -> Option<Self> {
        match event.event.as_str() {
            LOGIN_PROMPT => event.login_prompt().ok().map(Self::LoginPrompt),
            LOGIN_FINISHED => event.login_finished().ok().map(Self::LoginFinished),
            PACKAGE_PROGRESS => event.package_progress().ok().map(Self::PackageProgress),
            PACKAGE_FINISHED => event.package_finished().ok().map(Self::PackageFinished),
            _ => None,
        }
    }
}

/// Whether `event` is one of these.
pub(super) fn is_account_event(event: &DesktopEvent) -> bool {
    matches!(
        event.event.as_str(),
        LOGIN_PROMPT | LOGIN_FINISHED | PACKAGE_PROGRESS | PACKAGE_FINISHED
    )
}

/// The events waiting for their screen, oldest first.
#[derive(Debug, Default)]
pub(super) struct AccountEvents(Vec<AccountEvent>);

impl AccountEvents {
    fn push(&mut self, event: AccountEvent) {
        if self.0.len() >= MAX_EVENTS {
            self.0.remove(0);
        }
        self.0.push(event);
    }
}

impl SessionView {
    /// A sign-in or package record. The session accepts it once for its
    /// sequence (a gap resyncs, as for every event); the screen gets it even
    /// then, since a missed one is not replayed by a snapshot.
    pub(super) fn handle_account_event(
        &mut self,
        request: &pi_rpc::UiRequest,
        event: &DesktopEvent,
        cx: &mut Context<Self>,
    ) {
        match self.desktop.session().accept(request) {
            Err(DesktopRpcError::OutOfOrderSequence { .. }) => {
                let sends = self.desktop.resync();
                self.send_desktop(sends, cx);
            }
            // A replay already taken.
            Err(DesktopRpcError::DuplicateSequence { .. }) => return,
            _ => {}
        }
        if let Some(event) = AccountEvent::decode(event) {
            self.accounts.push(event);
            cx.notify();
        }
    }

    /// The sign-in and package events that arrived since the last call.
    pub(crate) fn take_account_events(&mut self) -> Vec<AccountEvent> {
        std::mem::take(&mut self.accounts.0)
    }
}

#[cfg(test)]
mod tests {
    use pi_rpc::desktop_rpc::{DesktopEvent, LoginPromptKind};
    use serde_json::json;

    use super::{AccountEvent, AccountEvents, MAX_EVENTS, is_account_event};

    fn event(name: &str, data: serde_json::Value) -> DesktopEvent {
        DesktopEvent {
            seq: 1,
            epoch: None,
            event: name.into(),
            data,
        }
    }

    #[test]
    fn only_well_formed_account_events_are_kept() {
        let prompt = event(
            "login_prompt",
            json!({ "loginId": "l1", "kind": "progress", "message": "Waiting…" }),
        );
        assert!(is_account_event(&prompt));
        assert!(matches!(
            AccountEvent::decode(&prompt),
            Some(AccountEvent::LoginPrompt(p)) if p.kind == LoginPromptKind::Progress
        ));
        let odd = event(
            "login_prompt",
            json!({ "loginId": "l1", "kind": "open_url" }),
        );
        assert_eq!(AccountEvent::decode(&odd), None);
        assert!(!is_account_event(&event("config_changed", json!({}))));
        assert!(matches!(
            AccountEvent::decode(&event(
                "package_finished",
                json!({ "jobId": "j", "ok": true })
            )),
            Some(AccountEvent::PackageFinished(_))
        ));
    }

    #[test]
    fn untaken_events_are_bounded() {
        let mut events = AccountEvents::default();
        for n in 0..MAX_EVENTS + 5 {
            events.push(AccountEvent::PackageProgress(
                pi_rpc::desktop_rpc::PackageProgress {
                    job_id: "j".into(),
                    line: n.to_string(),
                },
            ));
        }
        assert_eq!(events.0.len(), MAX_EVENTS);
        assert!(matches!(&events.0[0], AccountEvent::PackageProgress(p) if p.line == "5"));
    }
}
