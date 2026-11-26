//! The app's host tools in a tab whose agent takes them
//! (`crate::features::host_tools`): a Gentle Shell server's session, or a
//! Gentle Shell here (or in WSL, or through a command) new enough to, which
//! then starts without the app's extensions and instructions. Registered
//! once the Desktop channel says it takes them (and again after a server's
//! stream reconnects), each call run where the app's own code runs it (the
//! window for pages and browser cards, this process for the screen, the
//! open canvas for an agent beside one), and its result sent back through
//! the server's `/desktop`, or as a `/gentle:desktop` prompt on the child's
//! stdin (`PiProcess::desktop`).
//!
//! A call can run long (a review waiting for the user, a browser wait, a
//! Maestro flow, a device download): while it runs, and the Gentle Shell
//! offers `host_tool_progress`, the tab says so every
//! [`HOST_TOOL_PROGRESS_EVERY`] and the server restarts the call's
//! 10-minute no-result deadline. It stops once the call is answered,
//! cancelled or the backend is gone; an app that hangs or loses the server
//! stops saying so, and the server fails the call as before.
//!
//! An older Gentle Shell keeps the extensions and instructions it launches
//! with: it registers nothing.

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use gentle_core::transcript::NoticeLevel;
use gpui_kit::*;
use pi_rpc::PiProcess;
use pi_rpc::desktop_rpc::{
    DesktopEvent, DesktopMethod, DesktopRequest, DesktopRpcError, HOST_TOOL_CALL, HOST_TOOL_CANCEL,
    HOST_TOOL_PROGRESS_EVERY, HostToolCall, HostToolContent,
};

use super::{SessionEvent, SessionView};
use crate::features::host_tools::{self as tools, Call, Outcome};
use gentle_core::genui::host::Mode;

/// How long the open canvas has to answer, as its `.agent` requests do.
const CANVAS_ANSWER: Duration = Duration::from_secs(10);
/// How long the window has to say where a page opened: a server's page
/// waits for its forward first.
const PAGE_ANSWER: Duration = Duration::from_secs(60);
/// How many answered call ids are remembered, so a call the server re-sends
/// after a reconnect runs once.
const SEEN: usize = 128;

/// What a call asks of the window.
pub enum HostToolRequest {
    /// Show a page as `gentle-open` does; where it opened, or why it
    /// couldn't, goes to `reply`.
    OpenPage {
        url: String,
        reply: mpsc::Sender<Result<tools::PageOpened, String>>,
    },
    /// A `gentle-browser` operation; its answer goes to `reply`.
    Browser {
        op: serde_json::Value,
        reply: mpsc::Sender<serde_json::Value>,
    },
    /// A request for the canvas this agent floats over (only that canvas
    /// answers it).
    Canvas {
        request: serde_json::Value,
        reply: mpsc::Sender<CanvasAnswer>,
    },
    /// `gentle-device` arguments for the window's device panel.
    Device(Vec<String>),
}

/// The open canvas' answer: its JSON, and the picture a shot asked for.
pub struct CanvasAnswer {
    pub json: serde_json::Value,
    pub shot: Option<gentle_core::canvas::agent::Shot>,
}

/// A Pi tool result's `details`.
pub(super) type Details = serde_json::Map<String, serde_json::Value>;

/// The host tool state of one tab.
#[derive(Default)]
pub(super) struct HostCalls {
    /// The tools were offered on this connection (or the offer is in flight).
    registered: bool,
    /// Calls running now, each with when its next `host_tool_progress` is
    /// due; a cancel takes one out, and its answer is dropped.
    active: HashMap<String, Instant>,
    /// A heartbeat loop runs for [`Self::active`].
    beating: bool,
    /// Calls taken, newest last.
    seen: VecDeque<String>,
    next_request: u64,
    /// The host context last sent (`set_host_context`), `None` when none
    /// is held: sent again only when it changes.
    pub(super) context: Option<String>,
    /// What the tab answered, with its details, for the tests (a test tab
    /// has no server).
    #[cfg(test)]
    pub(super) sent: Vec<(String, Outcome, Option<Details>)>,
    /// The calls a progress was due for, in order, for the tests.
    #[cfg(test)]
    pub(super) beats: Vec<String>,
}

impl HostCalls {
    /// The stream reconnected: offer the tools again. Calls still running
    /// keep going; the server holds them for this device.
    pub(super) fn reconnected(&mut self) {
        self.registered = false;
    }

    /// The backend was replaced or exited: nothing it asked for is owed.
    pub(super) fn reset(&mut self) {
        self.registered = false;
        self.active.clear();
        self.seen.clear();
        self.context = None;
    }

    pub(super) fn request_id(&mut self) -> String {
        self.next_request += 1;
        format!("host-{}", self.next_request)
    }

    /// Take `call_id` once, at `now`: false for a call already taken.
    fn take(&mut self, call_id: &str, now: Instant) -> bool {
        if self.seen.iter().any(|seen| seen == call_id) {
            return false;
        }
        if self.seen.len() == SEEN {
            self.seen.pop_front();
        }
        self.seen.push_back(call_id.to_string());
        self.active
            .insert(call_id.to_string(), now + HOST_TOOL_PROGRESS_EVERY);
        true
    }

    /// Whether `call_id` still wants its answer; it no longer does after.
    fn finish(&mut self, call_id: &str) -> bool {
        self.active.remove(call_id).is_some()
    }

    fn cancel(&mut self, call_id: &str) {
        self.active.remove(call_id);
    }

    /// When the next progress is due; `None` once no call runs.
    fn next_beat(&self) -> Option<Instant> {
        self.active.values().min().copied()
    }

    /// The calls whose progress is due at `now`, sorted; each is next due
    /// [`HOST_TOOL_PROGRESS_EVERY`] after `now`.
    fn due(&mut self, now: Instant) -> Vec<String> {
        let mut due: Vec<String> = self
            .active
            .iter_mut()
            .filter(|(_, at)| **at <= now)
            .map(|(call_id, at)| {
                *at = now + HOST_TOOL_PROGRESS_EVERY;
                call_id.clone()
            })
            .collect();
        due.sort();
        due
    }
}

/// Whether a Desktop event is a host tool's.
pub(super) fn is_host_event(event: &DesktopEvent) -> bool {
    event.event == HOST_TOOL_CALL || event.event == HOST_TOOL_CANCEL
}

impl SessionView {
    /// Offer the app's tools once this tab's agent says it takes them.
    pub(super) fn sync_host_tools(&mut self, cx: &mut Context<Self>) {
        if self.host.registered || !self.backend.host_tools {
            return;
        }
        let Some(process) = self.process() else {
            return;
        };
        if !self
            .desktop
            .capabilities()
            .is_some_and(|capabilities| capabilities.supports(DesktopMethod::SetHostTools))
        {
            return;
        }
        self.host.registered = true;
        let request = DesktopRequest::set_host_tools(
            self.host.request_id(),
            tools::PROVIDER,
            &tools::definitions(
                self.canvas.is_some(),
                self.host_genui_mode(),
                tools::Place::of(&self.runs_on()),
            ),
        );
        // Gentle Shell holds the context per session; offer it with the tools.
        self.host.context = None;
        self.send_host_context(cx);
        let request = match request {
            Ok(request) => request,
            Err(error) => {
                self.host_notice(format!("The app's tools couldn't be offered: {error}"), cx);
                return;
            }
        };
        cx.spawn(async move |this, cx| {
            let sent = cx
                .background_executor()
                .spawn(async move { process.desktop(&request) })
                .await;
            let _ = this.update(cx, |this, cx| {
                let problem = match sent.map_err(|e| format!("{e:#}")).and_then(|answer| {
                    answer
                        .set_host_tools()
                        .map_err(|error| error.to_string())
                }) {
                    Ok(result) if result.rejected.is_empty() => return,
                    Ok(result) => {
                        let names: Vec<String> = result
                            .rejected
                            .iter()
                            .map(|tool| format!("{} ({})", tool.name, tool.reason))
                            .collect();
                        format!(
                            "Gentle Shell took only some of the app's tools; not: {}",
                            names.join(", ")
                        )
                    }
                    Err(error) => {
                        format!("The app's tools (browser, screen, pages) aren't available to this session: {error}")
                    }
                };
                this.host_notice(problem, cx);
            });
        })
        .detach();
    }

    /// A `host_tool_call` or `host_tool_cancel` record. The session accepts
    /// it once for its sequence; a call the server re-sends after a
    /// reconnect (an old sequence) still runs, once.
    pub(super) fn handle_host_event(
        &mut self,
        request: &pi_rpc::UiRequest,
        event: &DesktopEvent,
        cx: &mut Context<Self>,
    ) {
        if let Err(DesktopRpcError::OutOfOrderSequence { .. }) =
            self.desktop.session().accept(request)
        {
            let sends = self.desktop.resync();
            self.send_desktop(sends, cx);
        }
        if event.event == HOST_TOOL_CANCEL {
            if let Ok(cancel) = event.host_tool_cancel() {
                self.end_host_call(&cancel.call_id);
            }
            return;
        }
        let Ok(call) = event.host_tool_call() else {
            return;
        };
        // Other devices following this session provide their own tools: a
        // server names the provider by the device that registered it.
        let now = cx.background_executor().now();
        if !self.provides(&call.provider) || !self.host.take(&call.call_id, now) {
            return;
        }
        self.run_host_call(call, cx);
        self.keep_host_calls_alive(cx);
    }

    /// The server ended `call_id` (cancelled, expired, answered): nothing
    /// is owed for it. A review waiting for decisions stays open.
    fn end_host_call(&mut self, call_id: &str) {
        self.host.cancel(call_id);
        let _ = self.host_genui.stop_waiting(call_id, "");
    }

    /// Whether this tab's Gentle Shell takes `host_tool_progress`: a call
    /// then runs as long as the app works on it.
    pub(super) fn host_progress(&self) -> bool {
        self.desktop
            .capabilities()
            .is_some_and(|capabilities| capabilities.supports(DesktopMethod::HostToolProgress))
    }

    /// Run the heartbeat while calls run: wake when the next progress is
    /// due, send it, and end once no call runs (a later call starts it
    /// again).
    fn keep_host_calls_alive(&mut self, cx: &mut Context<Self>) {
        if self.host.beating || !self.host_progress() {
            return;
        }
        self.host.beating = true;
        cx.spawn(async move |this, cx| {
            loop {
                let next = this.update(cx, |this, _| {
                    let next = this.host.next_beat();
                    this.host.beating = next.is_some();
                    next
                });
                let Ok(Some(at)) = next else {
                    break;
                };
                let executor = cx.background_executor().clone();
                executor
                    .timer(at.saturating_duration_since(executor.now()))
                    .await;
                if this
                    .update(cx, |this, cx| this.send_host_progress(cx))
                    .is_err()
                {
                    break;
                }
            }
        })
        .detach();
    }

    /// Send `host_tool_progress` for every call due now. A failed send is
    /// left to the next beat; a call the server no longer holds ends.
    fn send_host_progress(&mut self, cx: &mut Context<Self>) {
        let due = self.host.due(cx.background_executor().now());
        let Some(capabilities) = self.desktop.capabilities().cloned() else {
            return;
        };
        for call_id in due {
            #[cfg(test)]
            self.host.beats.push(call_id.clone());
            let Some(process) = self.process() else {
                return;
            };
            let id = self.host.request_id();
            let Ok(request) = DesktopRequest::host_tool_progress(id, &capabilities, &call_id)
            else {
                continue;
            };
            cx.spawn(async move |this, cx| {
                let sent = cx
                    .background_executor()
                    .spawn(async move { process.desktop(&request) })
                    .await;
                let over = sent.is_ok_and(|answer| {
                    answer.error().is_some_and(|error| {
                        matches!(error.code.as_str(), "unknown_call" | "already_answered")
                    })
                });
                if over {
                    let _ = this.update(cx, |this, _| this.end_host_call(&call_id));
                }
            })
            .detach();
        }
    }

    /// Whether `provider` is this app: on a server it is this device's id
    /// (the server replaces the name the app registered with it).
    fn provides(&self, provider: &str) -> bool {
        match &self.runs_on {
            Some(gentle_core::backend::Backend::Server { device, .. }) => provider == device,
            _ => provider == tools::PROVIDER,
        }
    }

    fn run_host_call(&mut self, call: HostToolCall, cx: &mut Context<Self>) {
        let HostToolCall {
            call_id,
            tool,
            args,
            ..
        } = call;
        let genui = self.host_genui_mode().is_some_and(|mode| mode != Mode::Off);
        let call = match tools::route(&tool, &args, self.canvas.is_some(), genui) {
            Ok(call) => call,
            Err(message) => {
                self.answer_host_call(call_id, tool, Outcome::error(message), cx);
                return;
            }
        };
        let work: Box<dyn FnOnce() -> Outcome + Send> = match call {
            Call::OpenPage(url) => {
                let (reply, answer) = mpsc::channel();
                cx.emit(SessionEvent::HostTool(HostToolRequest::OpenPage {
                    url: url.clone(),
                    reply,
                }));
                Box::new(move || match answer.recv_timeout(PAGE_ANSWER) {
                    Ok(opened) => tools::page_outcome(&url, opened),
                    Err(mpsc::RecvTimeoutError::Timeout) => Outcome::text(format!(
                        "Asked the app to open {url} for the user; it is still opening."
                    )),
                    Err(mpsc::RecvTimeoutError::Disconnected) => {
                        tools::unavailable("open_page", "no window took the page")
                    }
                })
            }
            Call::Browser { op, screenshot } => {
                let (reply, answer) = mpsc::channel();
                let wait = gentle_core::browser::answer_within(&op);
                let tab = op["tab"].as_str().unwrap_or_default().to_string();
                cx.emit(SessionEvent::HostTool(HostToolRequest::Browser {
                    op,
                    reply,
                }));
                Box::new(move || {
                    let answer = answer
                        .recv_timeout(wait)
                        .unwrap_or_else(|error| match error {
                            mpsc::RecvTimeoutError::Timeout => gentle_core::browser::error(
                                "timeout",
                                "the app didn't answer in time",
                            ),
                            mpsc::RecvTimeoutError::Disconnected => {
                                gentle_core::browser::error("closed", "the window is closed")
                            }
                        });
                    if screenshot {
                        tools::driver_outcome(&answer, tools::browser_caption(&tab))
                    } else {
                        tools::driver_outcome(&answer, |_| String::new())
                    }
                })
            }
            Call::Screen(op) => Box::new(move || {
                let answer = crate::features::screen::cli::run_op(op);
                tools::driver_outcome(&answer, tools::screen_caption)
            }),
            Call::Canvas(request) => {
                let (reply, answer) = mpsc::channel();
                cx.emit(SessionEvent::HostTool(HostToolRequest::Canvas {
                    request,
                    reply,
                }));
                Box::new(move || match answer.recv_timeout(CANVAS_ANSWER) {
                    Ok(CanvasAnswer { json, shot }) => {
                        let png = shot.map(|shot| crate::features::canvas::shot_png(&shot));
                        tools::canvas_outcome(json, png)
                    }
                    Err(_) => tools::unavailable(
                        "The canvas",
                        "its tab must be open in the app (a closed tab can be reopened with ⌥⌘T)",
                    ),
                })
            }
            Call::Device(op) => {
                if let Some(args) = tools::device::panel_args(&op) {
                    cx.emit(SessionEvent::HostTool(HostToolRequest::Device(args)));
                }
                Box::new(move || tools::device::run(op))
            }
            Call::Maestro(op) => Box::new(move || tools::maestro::run(op)),
            Call::Theme(op) => Box::new(move || tools::themes::run(op)),
            // Runs here, on the conversation's own surfaces.
            Call::GenUi(args) => {
                self.run_genui_call(call_id, args, cx);
                return;
            }
        };
        cx.spawn(async move |this, cx| {
            let outcome = cx.background_executor().spawn(async move { work() }).await;
            let _ = this.update(cx, |this, cx| {
                this.answer_host_call(call_id, tool, outcome, cx)
            });
        })
        .detach();
    }

    /// Send `outcome` for `call_id`, unless the call was cancelled or the
    /// backend replaced meanwhile.
    fn answer_host_call(
        &mut self,
        call_id: String,
        tool: String,
        outcome: Outcome,
        cx: &mut Context<Self>,
    ) {
        self.answer_host_call_with(call_id, tool, outcome, None, cx);
    }

    /// [`Self::answer_host_call`] with the Pi tool result's `details`.
    pub(super) fn answer_host_call_with(
        &mut self,
        call_id: String,
        tool: String,
        outcome: Outcome,
        details: Option<Details>,
        cx: &mut Context<Self>,
    ) {
        if !self.host.finish(&call_id) {
            return;
        }
        #[cfg(test)]
        self.host
            .sent
            .push((call_id.clone(), outcome.clone(), details.clone()));
        let Some(process) = self.process() else {
            return;
        };
        let id = self.host.request_id();
        let request = DesktopRequest::host_tool_result(
            &id,
            &call_id,
            &outcome.content,
            outcome.is_error,
            details.as_ref(),
        )
        .or_else(|error| {
            let text = format!("{tool} answered more than the app can send: {error}");
            DesktopRequest::host_tool_result(
                &id,
                &call_id,
                &[HostToolContent::text(text)],
                true,
                None,
            )
        });
        let Ok(request) = request else {
            return;
        };
        send_result(process, request, tool, cx);
    }

    fn host_notice(&mut self, message: String, cx: &mut Context<Self>) {
        self.transcript.notice(NoticeLevel::Warning, message);
        cx.notify();
    }
}

/// Send a result in the background; a failure other than the call being
/// over already (cancelled, expired, answered) is said in the conversation.
fn send_result(
    process: Arc<PiProcess>,
    request: DesktopRequest,
    tool: String,
    cx: &mut Context<SessionView>,
) {
    cx.spawn(async move |this, cx| {
        let sent = cx
            .background_executor()
            .spawn(async move { process.desktop(&request) })
            .await;
        let problem = match sent {
            Ok(answer) => match answer.error() {
                None => return,
                Some(error)
                    if matches!(error.code.as_str(), "unknown_call" | "already_answered") =>
                {
                    return;
                }
                Some(error) => format!("{} ({})", error.message, error.code),
            },
            Err(error) => format!("{error:#}"),
        };
        let _ = this.update(cx, |this, cx| {
            this.host_notice(format!("The app couldn't answer {tool}: {problem}"), cx)
        });
    })
    .detach();
}

#[cfg(test)]
mod tests {
    use super::{HOST_TOOL_PROGRESS_EVERY, HostCalls};
    use std::time::{Duration, Instant};

    #[test]
    fn a_call_runs_once_and_a_cancelled_one_is_not_answered() {
        let now = Instant::now();
        let mut calls = HostCalls::default();
        assert!(calls.take("c1", now));
        // Re-sent after a reconnect: not run again.
        assert!(!calls.take("c1", now));
        assert!(calls.finish("c1"));
        assert!(!calls.finish("c1"));

        assert!(calls.take("c2", now));
        calls.cancel("c2");
        assert!(!calls.finish("c2"));

        // A reconnect re-offers the tools but keeps running calls.
        assert!(calls.take("c3", now));
        calls.registered = true;
        calls.reconnected();
        assert!(!calls.registered);
        assert!(calls.finish("c3"));

        // A new backend owes nothing; ids keep advancing.
        assert!(calls.take("c4", now));
        let before = calls.request_id();
        calls.reset();
        assert!(!calls.finish("c4"));
        assert_ne!(calls.request_id(), before);
    }

    #[test]
    fn progress_is_due_every_minute_per_call_until_it_ends() {
        let every = HOST_TOOL_PROGRESS_EVERY;
        assert_eq!(every, Duration::from_secs(60));
        let start = Instant::now();
        let mut calls = HostCalls::default();
        assert_eq!(calls.next_beat(), None);
        calls.take("c1", start);
        calls.take("c2", start + Duration::from_secs(20));
        assert_eq!(calls.next_beat(), Some(start + every));

        // Nothing is due before a minute has passed.
        assert!(
            calls
                .due(start + every - Duration::from_millis(1))
                .is_empty()
        );
        // c1's minute: only c1, next due a minute later.
        assert_eq!(calls.due(start + every), ["c1"]);
        assert_eq!(
            calls.next_beat(),
            Some(start + Duration::from_secs(20) + every)
        );
        assert_eq!(calls.due(start + Duration::from_secs(80)), ["c2"]);
        // A late wake sends each once and counts the next minute from then.
        let late = start + Duration::from_secs(300);
        assert_eq!(calls.due(late), ["c1", "c2"]);
        assert_eq!(calls.next_beat(), Some(late + every));
        assert!(calls.due(late + Duration::from_secs(59)).is_empty());

        // Answered, cancelled, or the backend gone: no more progress.
        assert!(calls.finish("c1"));
        assert_eq!(calls.due(late + every), ["c2"]);
        calls.cancel("c2");
        assert_eq!(calls.next_beat(), None);
        assert!(calls.due(late + every * 10).is_empty());
        calls.take("c3", late);
        calls.reset();
        assert_eq!(calls.next_beat(), None);
    }
}
