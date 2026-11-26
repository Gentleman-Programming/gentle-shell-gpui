//! `gentle_ui` in a tab whose agent takes the app's tools as host tools (a
//! Gentle Shell server's session, or a Gentle Shell here new enough to),
//! which doesn't load the app's generative UI extension: the app runs it
//! itself (`gentle_core::genui::host`). The agent calls the `gentle_ui` host
//! tool; the user's edits, sent by the surfaces as `/gentle-ui:event`
//! commands, are answered here and reach the agent as `host_message`s; what
//! html pages share, and the Off rule, go as the app's host context. The
//! tool results and event messages carry the details the surfaces are drawn
//! from, so the conversation draws and rebuilds them as it does a tab
//! with the extension's.
//!
//! An older Gentle Shell keeps the extension.

use gentle_core::genui::host::{self, Handled, Mode, Reply};
use gentle_core::genui::{self, EVENT_COMMAND};
use gentle_core::transcript::{Item, ToolStatus};
use gpui_kit::component::notification::Notification;
use gpui_kit::*;
use pi_rpc::desktop_rpc::{DesktopMethod, DesktopRequest, HostDelivery, HostMessage};
use serde_json::{Map, Value, json};

use super::SessionView;
use crate::features::host_tools::{self as tools, Outcome};

/// What a surface's command is told once it ran.
type Done = Box<dyn FnOnce(Result<(), String>, &mut App)>;

fn details(value: Value) -> Option<Map<String, Value>> {
    match value {
        Value::Object(map) => Some(map),
        _ => None,
    }
}

fn outcome(answer: host::Answer) -> (Outcome, Option<Map<String, Value>>) {
    (Outcome::text(answer.text), details(answer.details))
}

impl SessionView {
    /// The Agent UI mode `gentle_ui` runs in here; `None` when this tab
    /// doesn't run it (its agent loads the extension, or its Gentle Shell
    /// takes no host messages and contexts).
    pub(super) fn host_genui_mode(&self) -> Option<Mode> {
        let host_tools = self.backend.host_tools;
        let takes = self.desktop.capabilities().is_some_and(|capabilities| {
            [
                DesktopMethod::SetHostTools,
                DesktopMethod::HostMessage,
                DesktopMethod::SetHostContext,
            ]
            .into_iter()
            .all(|method| capabilities.supports(method))
        });
        (host_tools && takes).then(|| self.host_genui.mode())
    }

    /// The surfaces follow the conversation after items `from..` changed
    /// (`reset`: it was replaced, as on resume or a switch: rebuilt).
    pub(super) fn sync_host_genui(&mut self, from: usize, reset: bool, cx: &mut Context<Self>) {
        self.host_genui.set_root(&self.cwd.to_string_lossy());
        let versions = |item: &Item| match item {
            Item::Tool { name, details, .. } if name == genui::TOOL => Some(details.clone()),
            Item::Custom { kind, details, .. } if kind == genui::EVENT_MESSAGE => {
                Some(details.clone())
            }
            _ => None,
        };
        if reset {
            for (call, answer) in self.host_genui.reset() {
                let (answer, details) = outcome(answer);
                self.answer_host_call_with(call, genui::TOOL.into(), answer, details, cx);
            }
            let found: Vec<Value> = self.transcript.items.iter().filter_map(versions).collect();
            for details in &found {
                self.host_genui.remember(details);
            }
            // A server clears a session's context when it changes.
            self.host.context = None;
            self.send_host_context(cx);
        } else {
            let found: Vec<Value> = self
                .transcript
                .items
                .get(from..)
                .unwrap_or_default()
                .iter()
                .filter_map(versions)
                .collect();
            for details in &found {
                self.host_genui.observe(details);
            }
        }
    }

    /// A `gentle_ui` call.
    pub(super) fn run_genui_call(&mut self, call_id: String, args: Value, cx: &mut Context<Self>) {
        let tool = genui::TOOL.to_string();
        match self.host_genui.call(&call_id, &args) {
            Err(error) => {
                self.answer_host_call_with(call_id, tool, Outcome::error(error), None, cx)
            }
            Ok(Reply::Done(answer)) => {
                let (answer, details) = outcome(answer);
                self.answer_host_call_with(call_id, tool, answer, details, cx);
            }
            Ok(Reply::Waiting { surface, replaced }) => {
                if let Some((earlier, answer)) = replaced {
                    let (answer, details) = outcome(answer);
                    self.answer_host_call_with(earlier, tool, answer, details, cx);
                }
                self.show_waiting(&args, &surface, cx);
                // The heartbeat keeps the call alive while the review waits.
                if self.host_progress() {
                    return;
                }
                // An older server fails it after 10 minutes: give up first;
                // the review stays open.
                cx.spawn(async move |this, cx| {
                    cx.background_executor().timer(host::REVIEW_WAIT).await;
                    let _ = this.update(cx, |this, cx| {
                        let why = format!(
                            "No decisions after {} minutes.",
                            host::REVIEW_WAIT.as_secs() / 60
                        );
                        if let Some((call, answer)) = this.host_genui.stop_waiting(&call_id, &why) {
                            let (answer, details) = outcome(answer);
                            this.answer_host_call_with(
                                call,
                                genui::TOOL.into(),
                                answer,
                                details,
                                cx,
                            );
                        }
                    });
                })
                .detach();
            }
        }
    }

    /// A review that waits for the user's decisions shows while its call
    /// runs, as the extension's partial result shows it (a host tool has
    /// none): its running tool item gets the surface.
    fn show_waiting(&mut self, args: &Value, surface: &host::Surface, cx: &mut Context<Self>) {
        let call = self
            .transcript
            .items
            .iter()
            .rev()
            .find_map(|item| match item {
                Item::Tool {
                    call_id,
                    name,
                    args: item_args,
                    status: ToolStatus::Running,
                    ..
                } if name == genui::TOOL && item_args == args => Some(call_id.clone()),
                _ => None,
            });
        let Some(call) = call else {
            return;
        };
        self.transcript.apply(&json!({
            "type": "tool_execution_update",
            "toolCallId": call,
            "partialResult": {
                "content": [{"type": "text", "text": "Waiting for the user's decisions…"}],
                "details": Value::Object(surface.details()),
            },
        }));
        self.sync_view(cx);
        cx.notify();
    }

    /// A `/gentle-ui:event {json}` a surface sends, in a tab that runs
    /// `gentle_ui` itself: `None` (handled) or the text back for the
    /// extension when it doesn't.
    pub(super) fn host_genui_command(
        &mut self,
        text: String,
        done: Done,
        cx: &mut Context<Self>,
    ) -> Option<(String, Done)> {
        let prefix = format!("/{EVENT_COMMAND} ");
        let (Some(args), Some(_)) = (text.strip_prefix(&prefix), self.host_genui_mode()) else {
            return Some((text, done));
        };
        let handled = self.host_genui.event(args);
        self.apply_genui_event(handled, done, cx);
        None
    }

    fn apply_genui_event(&mut self, handled: Handled, done: Done, cx: &mut Context<Self>) {
        let Handled {
            outcome: line,
            message,
            mode,
            context_changed,
            decided,
            error,
        } = handled;
        if let Some(error) = error {
            done(Err(error), cx);
            return;
        }
        // Agent UI changed: the tool's offer (and its rules) follow it.
        if mode.is_some() {
            self.host.reconnected();
            self.sync_host_tools(cx);
        }
        if context_changed {
            self.send_host_context(cx);
        }
        if let Some((call, answer)) = decided {
            let (answer, details) = outcome(answer);
            self.answer_host_call_with(call, genui::TOOL.into(), answer, details, cx);
        }
        let Some(message) = message else {
            if let Some(line) = line {
                self.genui_outcome(&line, cx);
            }
            done(Ok(()), cx);
            return;
        };
        // The surface hears the outcome once the agent has the message, as
        // the extension answers after `sendMessage`.
        let message = HostMessage {
            custom_type: host::HOST_EVENT_MESSAGE.into(),
            content: message.content,
            display: true,
            details: details(message.details),
            deliver: HostDelivery::Auto,
            trigger_turn: Some(message.trigger_turn),
        };
        self.send_host_message(
            message,
            move |this, result, cx| {
                if result.is_ok()
                    && let Some(line) = line
                {
                    this.genui_outcome(&line, cx);
                }
                done(result, cx);
            },
            cx,
        );
    }

    /// The extension's answer line, as its outcome widget carries it.
    fn genui_outcome(&mut self, line: &str, cx: &mut Context<Self>) {
        if let Some(error) = self.genui.outcome(line, cx) {
            self.toast(
                Notification::warning(format!("The template couldn't be inserted: {error}")),
                cx,
            );
        }
        cx.notify();
    }

    fn send_host_message(
        &mut self,
        message: HostMessage,
        then: impl FnOnce(&mut Self, Result<(), String>, &mut Context<Self>) + 'static,
        cx: &mut Context<Self>,
    ) {
        let request = DesktopRequest::host_message(self.host.request_id(), &message);
        #[cfg(test)]
        self.host_messages.push(message);
        let request = match request {
            Ok(request) => request,
            Err(error) => {
                then(
                    self,
                    Err(format!("The edits couldn't be sent: {error}")),
                    cx,
                );
                return;
            }
        };
        let Some(process) = self.process() else {
            // A test tab has no server: the message is as good as sent.
            let result = if cfg!(test) {
                Ok(())
            } else {
                Err("Gentle Shell is still starting in this tab; try again in a moment.".into())
            };
            then(self, result, cx);
            return;
        };
        cx.spawn(async move |this, cx| {
            let sent = cx
                .background_executor()
                .spawn(async move { process.desktop(&request) })
                .await;
            let result = sent
                .map_err(|error| format!("{error:#}"))
                .and_then(|answer| answer.host_message().map_err(|error| error.to_string()))
                .map(|_| ());
            let _ = this.update(cx, |this, cx| then(this, result, cx));
        })
        .detach();
    }

    /// Give the server the host context (the pages' shared context, the
    /// Off rule) when it changed; an empty one clears it.
    pub(super) fn send_host_context(&mut self, cx: &mut Context<Self>) {
        if self.host_genui_mode().is_none() {
            return;
        }
        let (content, details) = match self.host_genui.context() {
            Some((content, more)) => (content, details(more)),
            None => (String::new(), None),
        };
        let held = (!content.is_empty()).then(|| content.clone());
        if held == self.host.context {
            return;
        }
        self.host.context = held;
        let Some(process) = self.process() else {
            return;
        };
        let request = match DesktopRequest::set_host_context(
            self.host.request_id(),
            tools::PROVIDER,
            &content,
            details.as_ref(),
        ) {
            Ok(request) => request,
            Err(error) => {
                self.transcript.notice(
                    gentle_core::transcript::NoticeLevel::Warning,
                    format!("The app's context for the agent couldn't be sent: {error}"),
                );
                return;
            }
        };
        cx.spawn(async move |this, cx| {
            let sent = cx
                .background_executor()
                .spawn(async move { process.desktop(&request) })
                .await;
            let problem = match sent {
                Ok(answer) => match answer.set_host_context() {
                    Ok(_) => return,
                    Err(error) => error.to_string(),
                },
                Err(error) => format!("{error:#}"),
            };
            let _ = this.update(cx, |this, cx| {
                // Not held: the next change sends it again.
                this.host.context = None;
                this.transcript.notice(
                    gentle_core::transcript::NoticeLevel::Warning,
                    format!("The app's context for the agent couldn't be sent: {problem}"),
                );
                cx.notify();
            });
        })
        .detach();
    }

    /// A turn started: the context html pages shared went with it.
    pub(super) fn host_genui_turn_started(&mut self, cx: &mut Context<Self>) {
        if self.host_genui.turn_started() {
            self.send_host_context(cx);
        }
    }
}
