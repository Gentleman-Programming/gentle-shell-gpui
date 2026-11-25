//! The agent floating over a canvas. The canvas' Agent button opens a
//! conversation in a card over the page. Started on this computer, it gets
//! `GENTLE_CANVAS_FILE` and the `gentle-canvas.ts` extension, whose tools
//! drop requests in the canvas' `.agent` folder; the open canvas picks them
//! up. On a Gentle Shell server it gets the same tools as host tools
//! (`features::host_tools`), whose requests reach the canvas in-process.
//! Either way the canvas answers through `gentle_core::canvas::agent`
//! (edits as one undo step, pictures rasterized here).

use std::path::{Path, PathBuf};
use std::time::Duration;

use gentle_core::canvas::Camera;
use gentle_core::canvas::agent::{self, Shot};
use serde_json::{Value, json};

use super::*;
use crate::features::session::{CanvasAnswer, HostToolRequest, SessionEvent, SessionView};
use crate::shared::ui;
use gpui_kit::assets::IconName;
use gpui_kit::component::{Icon, Sizable as _, h_flex, v_flex};

/// The variable naming the canvas an agent works on.
pub const AGENT_ENV: &str = "GENTLE_CANVAS_FILE";
const SOURCE: &str = include_str!("gentle-canvas.ts");
/// How often an open canvas looks for requests.
const POLL: Duration = Duration::from_millis(200);
/// The view assumed for a canvas never drawn yet (its tab not shown).
const UNSEEN_VIEW: (f32, f32) = (1200., 800.);
/// The floating agent card's width, px.
pub(super) const AGENT_W: f32 = 420.;

/// The conversation floating over the canvas.
pub(super) struct FloatingAgent {
    pub session: Entity<SessionView>,
    /// Hidden keeps it running; the button shows it again.
    pub shown: bool,
    /// Its host tool calls for this canvas (a server session's).
    pub _host: Subscription,
}

/// Writes the extension to the app's data folder; its path, or `None` when
/// it can't be written.
pub fn install_extension() -> Option<PathBuf> {
    let path = crate::shared::data_dir::app()?
        .join("extensions")
        .join("gentle-canvas.ts");
    crate::features::genui::write_if_changed(&path, SOURCE)?;
    Some(path)
}

/// Requests waiting beside the canvas at `canvas`, oldest first, each read
/// and removed.
fn take_requests(canvas: &Path) -> Vec<(PathBuf, Result<Value, String>)> {
    let Ok(entries) = std::fs::read_dir(canvas.with_extension("agent")) else {
        return Vec::new();
    };
    let mut requests: Vec<PathBuf> = entries
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.to_string_lossy().ends_with(".req.json"))
        .collect();
    requests.sort();
    requests
        .into_iter()
        .map(|path| {
            let parsed = std::fs::read_to_string(&path)
                .map_err(|e| e.to_string())
                .and_then(|text| serde_json::from_str(&text).map_err(|e| e.to_string()));
            let _ = std::fs::remove_file(&path);
            (path, parsed)
        })
        .collect()
}

struct Answer {
    request: PathBuf,
    json: Value,
    shot: Option<Shot>,
}

/// A shot's picture as PNG bytes.
pub fn shot_png(shot: &Shot) -> Result<Vec<u8>, String> {
    crate::shared::backdrop::preview::rasterize_at(&shot.svg, (shot.width, shot.height), shot.scale)
        .and_then(|pixmap| pixmap.encode_png().map_err(|e| e.to_string()))
}

/// Writes `answer` beside its request: `<id>.res.json`, and `<id>.png` for a
/// picture (named in the JSON as `image`).
fn write_answer(answer: Answer) {
    let request = answer.request.to_string_lossy();
    let base = request.trim_end_matches(".req.json");
    let mut json = answer.json;
    if let Some(shot) = answer.shot {
        let png = shot_png(&shot);
        let image = format!("{base}.png");
        json = match png.map(|png| std::fs::write(&image, png).map_err(|e| e.to_string())) {
            Ok(Ok(())) => {
                json["image"] = image.into();
                json
            }
            Ok(Err(e)) | Err(e) => json!({ "ok": false, "error": format!("no picture: {e}") }),
        };
    }
    // Renamed into place: the agent never reads half an answer.
    let tmp = format!("{base}.res.tmp");
    if std::fs::write(&tmp, json.to_string()).is_ok() {
        let _ = std::fs::rename(&tmp, format!("{base}.res.json"));
    }
}

impl CanvasView {
    /// Answers the agent's requests for as long as the canvas is open.
    pub(super) fn watch_agent(canvas: PathBuf, cx: &mut Context<Self>) -> Task<()> {
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(POLL).await;
                let file = canvas.clone();
                let requests = cx
                    .background_executor()
                    .spawn(async move { take_requests(&file) })
                    .await;
                if requests.is_empty() {
                    continue;
                }
                let Ok(answers) = this.update(cx, |this, cx| {
                    requests
                        .into_iter()
                        .map(|(request, parsed)| this.answer(request, parsed, cx))
                        .collect::<Vec<_>>()
                }) else {
                    return;
                };
                cx.background_executor()
                    .spawn(async move { answers.into_iter().for_each(write_answer) })
                    .await;
            }
        })
    }

    fn answer(
        &mut self,
        request: PathBuf,
        parsed: Result<Value, String>,
        cx: &mut Context<Self>,
    ) -> Answer {
        let req = match parsed {
            Ok(req) => req,
            Err(error) => {
                return Answer {
                    request,
                    json: json!({ "ok": false, "error": format!("unreadable request: {error}") }),
                    shot: None,
                };
            }
        };
        let (json, shot) = self.respond(&req, cx);
        Answer {
            request,
            json,
            shot,
        }
    }

    /// Answer one request (`{"action": "read" | "edit" | "shot", …}`) on
    /// this canvas: its JSON, and a shot's picture to rasterize.
    fn respond(&mut self, req: &Value, cx: &mut Context<Self>) -> (Value, Option<Shot>) {
        let (w, h) = match self.size() {
            (w, h) if w >= 1. && h >= 1. => (w, h),
            _ => UNSEEN_VIEW,
        };
        let view = self.doc.camera.visible(w, h);
        let before = (req["action"] == "edit").then(|| self.doc.shapes.clone());
        let response = agent::handle(&mut self.doc, view, paint::light(), req);
        if response.changed {
            if let Some(before) = before {
                self.history.record(before);
            }
            self.touched_all(cx);
        }
        if let Some(area) = response.show {
            self.doc.camera = Camera::fitting(&area, w, h, 64.);
            self.camera_moved(cx);
        }
        (response.json, response.shot)
    }
}

impl CanvasView {
    /// The folder the agent works in; the window sets the pane's.
    pub fn set_cwd(&mut self, cwd: PathBuf) {
        self.cwd = cwd;
    }

    /// The floating agent, once it has been opened.
    pub fn agent_session(&self) -> Option<Entity<SessionView>> {
        self.agent.as_ref().map(|agent| agent.session.clone())
    }

    /// The conversation to resume when the agent opens (saved with the pane).
    pub fn resume_agent(&mut self, session: PathBuf) {
        self.agent_resume = Some(session);
    }

    /// The Gentle Shell server session the agent follows, saved with the
    /// pane: quitting leaves it running there.
    pub fn agent_server_session(&self, cx: &App) -> Option<gentle_core::serve::ServerSession> {
        self.agent.as_ref()?.session.read(cx).server_session()
    }

    /// The agent's server session kept running while the app was closed:
    /// reattach to it now, the agent hidden (its button shows it), so its
    /// canvas tools reach this canvas again.
    pub fn reattach_agent(
        &mut self,
        server: gentle_core::serve::ServerSession,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.agent.is_none() {
            self.start_agent(false, Some(server), window, cx);
        }
    }

    /// Opens the agent over the canvas (starting it the first time), or
    /// hides it; hidden, it keeps working.
    pub(super) fn toggle_agent(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        match &mut self.agent {
            Some(agent) if agent.shown => {
                agent.shown = false;
                window.focus(&self.focus, cx);
            }
            Some(agent) => {
                agent.shown = true;
                let composer = agent.session.read(cx).composer();
                composer.update(cx, |c, cx| c.focus(window, cx));
            }
            None => self.start_agent(true, None, window, cx),
        }
        cx.notify();
    }

    /// Start the agent (on `attach`, a server session still running there),
    /// `shown` over the page or hidden.
    fn start_agent(
        &mut self,
        shown: bool,
        attach: Option<gentle_core::serve::ServerSession>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let (cwd, canvas) = (self.cwd.clone(), self.path.clone());
        let resume = self.agent_resume.take();
        let session = cx.new(|cx| {
            let mut session = SessionView::new(cwd, None, window, cx, false);
            session.set_canvas(canvas);
            if let Some(server) = attach {
                session.attach_server_session(server);
            }
            if let Some(path) = resume {
                session.resume_when_ready(path, cx);
            }
            session.ensure_started(window, cx);
            session
        });
        if shown {
            let composer = session.read(cx).composer();
            composer.update(cx, |c, cx| c.focus(window, cx));
        }
        cx.emit(CanvasEvent::AgentStarted(session.clone()));
        // A server session's canvas tools reach this canvas here.
        let host = cx.subscribe(&session, |this, _, event: &SessionEvent, cx| {
            if let SessionEvent::HostTool(HostToolRequest::Canvas { request, reply }) = event {
                let (json, shot) = this.respond(request, cx);
                let _ = reply.send(CanvasAnswer { json, shot });
            }
        });
        self.agent = Some(FloatingAgent {
            session,
            shown,
            _host: host,
        });
        cx.notify();
    }

    /// Ends the agent: its backend stops; the next one starts fresh.
    fn close_agent(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(agent) = self.agent.take() {
            agent.session.read(cx).shutdown_backend();
        }
        window.focus(&self.focus, cx);
        cx.notify();
    }

    pub(super) fn agent_shown(&self) -> bool {
        self.agent.as_ref().is_some_and(|a| a.shown)
    }

    /// The agent's card, floating at the right of the page.
    pub(super) fn agent_panel(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let agent = self.agent.as_ref().filter(|a| a.shown)?;
        let button = |id: &'static str, icon: IconName, tip: &'static str| {
            div()
                .id(id)
                .size(px(24.))
                .flex()
                .items_center()
                .justify_center()
                .rounded(px(7.))
                .cursor_pointer()
                .text_color(ui::muted())
                .hover(|s| s.bg(ui::surface_hover()).text_color(ui::fg()))
                .tooltip(move |window, cx| {
                    gpui_kit::component::tooltip::Tooltip::new(tip).build(window, cx)
                })
                .child(Icon::new(icon).small())
        };
        Some(
            v_flex()
                .id("canvas-agent-card")
                .absolute()
                .top_3()
                .right_3()
                .bottom(px(84.))
                .w(px(AGENT_W))
                // Pointer and wheel stay in the card, off the page under it.
                .occlude()
                .overflow_hidden()
                .rounded(px(14.))
                .bg(ui::popover_bg())
                .border_1()
                .border_color(ui::border_strong().opacity(0.6))
                .shadow(vec![BoxShadow {
                    color: ui::backdrop().opacity(0.3),
                    offset: point(px(0.), px(8.)),
                    blur_radius: px(24.),
                    spread_radius: px(0.),
                    inset: false,
                }])
                .child(
                    h_flex()
                        .flex_none()
                        .gap_2()
                        .px_3()
                        .py_2()
                        .child(Icon::new(IconName::Bot).small().text_color(ui::petal()))
                        .child(div().text_sm().text_color(ui::fg()).child("Agent"))
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .truncate()
                                .text_xs()
                                .text_color(ui::muted())
                                .child("sees and draws on this canvas"),
                        )
                        .child(
                            button("canvas-agent-hide", IconName::Minus, "Hide (keeps working)")
                                .on_click(
                                    cx.listener(|this, _, window, cx| {
                                        this.toggle_agent(window, cx)
                                    }),
                                ),
                        )
                        .child(
                            button("canvas-agent-close", IconName::X, "End this agent").on_click(
                                cx.listener(|this, _, window, cx| this.close_agent(window, cx)),
                            ),
                        ),
                )
                // Its live indicators, over it (see `SessionView::live_layer`).
                .child(
                    div()
                        .relative()
                        .flex_1()
                        .min_h_0()
                        .child(agent.session.clone())
                        .child(agent.session.read(cx).live_layer()),
                )
                .into_any_element(),
        )
    }
}
