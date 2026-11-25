//! What window screens read from this tab's Gentle Shell over the Desktop
//! channel (`state::desktop::reads`): whether to ask it at all, queueing a
//! read, and handing the screen its answer. The Sessions browser, Usage ›
//! Local stats and Configuration's update status ask the window's active
//! agent tab; with a Shell that does not advertise a method they keep their
//! own path.

use std::time::Duration;

use gpui_kit::*;
use pi_rpc::desktop_rpc::DesktopMethod;

use super::SessionView;
use super::state::BackendStatus;
use super::state::desktop::reads::{
    Applied, ReadFailure, ReadOutcome, ReadRequest, ReadSupport, ReadTicket,
};

/// How long a read answered `session_switching` waits before it is sent
/// again: the switch in flight answers first.
const RETRY_DELAY: Duration = Duration::from_millis(300);

/// How long a screen waits for a tab still probing its Shell before it takes
/// its own path: a starting backend must not keep a screen empty.
pub(crate) const READ_DECIDE_WAIT: Duration = Duration::from_secs(3);

impl SessionView {
    /// Whether a screen asks this tab for `method`. A backend that failed or
    /// exited has nobody to ask: the screen's own path.
    pub(crate) fn read_support(&self, method: DesktopMethod) -> ReadSupport {
        match self.backend.status {
            BackendStatus::Failed(_) | BackendStatus::Exited(_) => ReadSupport::Legacy,
            BackendStatus::Starting | BackendStatus::Ready(_) => self.desktop.reads_support(method),
        }
    }

    /// Queue `request`; its answer arrives as an outcome for the returned
    /// ticket ([`Self::take_read`]), and the tab notifies its observers then.
    pub(crate) fn read(
        &mut self,
        request: ReadRequest,
        cx: &mut Context<Self>,
    ) -> Result<ReadTicket, ReadFailure> {
        let ticket = self.desktop.reads_enqueue(request)?;
        self.sync_config(cx);
        Ok(ticket)
    }

    /// The answer to `ticket`, once, if it arrived.
    pub(crate) fn take_read(&mut self, ticket: ReadTicket) -> Option<ReadOutcome> {
        self.desktop.reads_take(ticket)
    }

    /// Send the next queued read once the shared slot is free.
    pub(super) fn sync_reads(&mut self, cx: &mut Context<Self>) {
        let applied = self.desktop.reads_pump();
        if !applied.sends.is_empty() {
            self.apply_reads(applied, cx);
        }
    }

    /// Act on a reads decision. A `switch_session` that changed the session
    /// replaced it under this tab, exactly as Pi's own `switch_session` does:
    /// the conversation is forgotten, the channel re-probed (its event
    /// sequence restarted at 1) and everything re-read, `sessionFile`
    /// included, so resume-on-restart follows the new session.
    pub(super) fn apply_reads(&mut self, applied: Applied, cx: &mut Context<Self>) {
        if applied.switched {
            self.reset_conversation();
            self.reset_desktop_channel();
            self.refresh_state(cx);
        }
        self.send_desktop(applied.sends, cx);
        let resync = self.desktop.flush_resync();
        self.send_desktop(resync, cx);
        if applied.retry {
            cx.spawn(async move |this, cx| {
                cx.background_executor().timer(RETRY_DELAY).await;
                let _ = this.update(cx, |this, cx| {
                    this.desktop.reads_release();
                    this.sync_config(cx);
                });
            })
            .detach();
        }
        self.settle_credentials(cx);
        self.settle_models(cx);
        cx.notify();
    }
}
