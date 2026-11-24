//! Floating web pages and the title bar's instance tray. What an agent opens
//! (a dev server's URL, the design canvas, `gentle-open`) starts hidden in the
//! tray, so the window never rearranges on its own; the user looks at it from
//! the tray's circle. What the user opens (`/web`) shows at once. A hidden
//! page costs nothing: it has no native view. Simulators and emulators have
//! their own panel (`features::devices`).

use super::*;
use crate::features::browser::{BrowserEvent, BrowserPanel, CanvasNotice};
use crate::shared::floating;
use gpui_kit::component::popover::{Popover, PopoverState};
use gpui_kit::component::{ActiveTheme as _, Selectable};

/// One web page, shown floating or kept in the tray.
pub(super) struct Stage {
    id: u64,
    panel: Entity<BrowserPanel>,
    shown: bool,
    /// The agent pane that opened it, named in the tray.
    origin: Option<PaneId>,
    /// What the tray last drew for it: title and address. The page's own
    /// redraws only redraw the window when this changes.
    summary: (String, String),
    _subscriptions: [Subscription; 2],
}

impl Stage {
    /// The floating card, while shown.
    pub(super) fn element(&self) -> Option<AnyElement> {
        self.shown.then(|| self.panel.clone().into_any_element())
    }

    fn set_shown(&mut self, shown: bool, cx: &mut App) {
        self.shown = shown;
        self.panel.update(cx, |panel, cx| {
            if shown {
                panel.show(cx)
            } else {
                panel.hide(cx)
            }
        });
    }

    /// What the tray shows for it: title and address.
    fn summary(panel: &Entity<BrowserPanel>, cx: &App) -> (String, String) {
        let panel = panel.read(cx);
        (panel.title(), address(panel.url()))
    }
}

/// A URL without its scheme, for status lines.
fn address(url: &str) -> String {
    url.trim_start_matches("http://")
        .trim_start_matches("https://")
        .to_string()
}

impl Workspace {
    // ── Opening ─────────────────────────────────────────────────────────────

    /// Show a page in its own card. `reveal` is false for pages an agent
    /// opened (a dev server's URL, `gentle-open`): those wait in the tray. A
    /// page that is already open reuses its card.
    pub(super) fn open_web(
        &mut self,
        url: String,
        origin: Option<PaneId>,
        reveal: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let existing = self.stages.iter().find_map(|stage| {
            let panel = stage.panel.read(cx);
            (!panel.is_design() && panel.url() == url).then(|| (stage.id, stage.panel.clone()))
        });
        if let Some((id, entity)) = existing {
            entity.update(cx, |panel, cx| panel.navigate(url, None, cx));
            if reveal {
                self.set_stage_shown(id, true, window, cx);
            }
            return;
        }
        let panel = cx.new(|cx| BrowserPanel::new(url, None, cx));
        self.add_stage(panel, origin, reveal, window, cx);
    }

    /// Gentle Design's canvas: kept in the tray when it opens, reloaded in
    /// place when it refreshes, removed when it stops.
    pub(super) fn show_design_canvas(
        &mut self,
        canvas: CanvasNotice,
        origin: PaneId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let design = |stage: &Stage, cx: &App| {
            stage
                .panel
                .read(cx)
                .is_design()
                .then(|| stage.panel.clone())
        };
        match canvas {
            CanvasNotice::Stopped => {
                let ids: Vec<u64> = self
                    .stages
                    .iter()
                    .filter(|stage| design(stage, cx).is_some())
                    .map(|stage| stage.id)
                    .collect();
                for id in ids {
                    self.remove_stage(id, window, cx);
                }
            }
            CanvasNotice::Open { url, .. } => {
                let label = Some("Design canvas".to_string());
                match self.stages.iter().find_map(|stage| design(stage, cx)) {
                    Some(entity) => entity.update(cx, |panel, cx| panel.navigate(url, label, cx)),
                    None => {
                        let panel = cx.new(|cx| BrowserPanel::new(url, label, cx));
                        self.add_stage(panel, Some(origin), false, window, cx);
                    }
                }
            }
        }
    }

    fn add_stage(
        &mut self,
        panel: Entity<BrowserPanel>,
        origin: Option<PaneId>,
        shown: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let id = self.next_stage;
        self.next_stage += 1;
        // The card's own button only hides it; the tray closes it.
        let subscriptions = [
            cx.subscribe_in(
                &panel,
                window,
                move |this, _, _: &BrowserEvent, window, cx| {
                    this.set_stage_shown(id, false, window, cx)
                },
            ),
            cx.observe(&panel, move |this, _, cx| this.stage_changed(id, cx)),
        ];
        let summary = Stage::summary(&panel, cx);
        let mut stage = Stage {
            id,
            panel,
            shown: false,
            origin,
            summary,
            _subscriptions: subscriptions,
        };
        stage.set_shown(shown, cx);
        self.stages.push(stage);
        if shown {
            self.tile_stages(window, cx);
        }
        cx.notify();
    }

    // ── Showing, hiding, closing ────────────────────────────────────────────

    /// Redraw the tray only when what it shows for this stage changed.
    fn stage_changed(&mut self, id: u64, cx: &mut Context<Self>) {
        let Some(stage) = self.stages.iter_mut().find(|stage| stage.id == id) else {
            return;
        };
        let summary = Stage::summary(&stage.panel, cx);
        if stage.summary != summary {
            stage.summary = summary;
            cx.notify();
        }
    }

    fn set_stage_shown(
        &mut self,
        id: u64,
        shown: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(stage) = self.stages.iter_mut().find(|stage| stage.id == id) else {
            return;
        };
        if stage.shown == shown {
            return;
        }
        stage.set_shown(shown, cx);
        self.tile_stages(window, cx);
        cx.notify();
    }

    fn hide_all_stages(&mut self, cx: &mut Context<Self>) {
        for stage in self.stages.iter_mut().filter(|stage| stage.shown) {
            stage.set_shown(false, cx);
        }
        cx.notify();
    }

    /// Close a page.
    fn remove_stage(&mut self, id: u64, window: &mut Window, cx: &mut Context<Self>) {
        let Some(index) = self.stages.iter().position(|stage| stage.id == id) else {
            return;
        };
        let stage = self.stages.remove(index);
        if stage.shown {
            self.tile_stages(window, cx);
        }
        cx.notify();
    }

    // ── Agents driving pages (`gentle-browser`) ─────────────────────────────

    /// Answer a `gentle-browser` operation; the answer goes to `reply`, now
    /// or once the page did it.
    pub fn browser_request(
        &mut self,
        op: serde_json::Value,
        reply: std::sync::mpsc::Sender<serde_json::Value>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        use gentle_core::browser::{Op, Profile, error, ok};
        use serde_json::json;
        let op = match serde_json::from_value::<Op>(op) {
            Ok(op) => op,
            Err(err) => {
                let _ = reply.send(error("bad_op", err.to_string()));
                return;
            }
        };
        let not_found = |tab: &str| {
            error(
                "not_found",
                format!("no page named \"{tab}\" is open (see `gentle-browser tabs`)"),
            )
        };
        match op {
            Op::Tabs => {
                let _ = reply.send(ok(json!({ "tabs": self.browser_tabs(cx) })));
            }
            Op::Open {
                name,
                url,
                profile,
                show,
            } => {
                if let Some(url) = &url
                    && !(url.starts_with("http://") || url.starts_with("https://"))
                {
                    let _ = reply.send(error("bad_url", "only http(s) pages open here"));
                    return;
                }
                let profile = profile.as_deref().map(Profile::parse);
                let found = name.as_deref().and_then(|name| self.find_stage(name, cx));
                let (id, panel) = match found {
                    Some((id, panel)) => {
                        panel.update(cx, |panel, cx| {
                            if let Some(profile) = profile {
                                panel.set_profile(profile, cx);
                            }
                            panel.set_driven(true, cx);
                            if let Some(url) = url {
                                panel.navigate(url, None, cx);
                            }
                        });
                        (id, panel)
                    }
                    None => {
                        let Some(url) = url else {
                            let _ = reply.send(error("bad_op", "open needs a URL for a new page"));
                            return;
                        };
                        let root = self.root().display().to_string();
                        let panel = cx.new(|cx| {
                            let mut panel = BrowserPanel::new(url, None, cx);
                            panel.set_name(name.clone(), cx);
                            panel.set_repository(Some(root));
                            panel.set_profile(profile.unwrap_or(Profile::Agent), cx);
                            panel.set_driven(true, cx);
                            panel
                        });
                        let id = self.next_stage;
                        self.add_stage(panel.clone(), None, show, window, cx);
                        (id, panel)
                    }
                };
                if show {
                    self.set_stage_shown(id, true, window, cx);
                }
                let tab = self.tab_label(id, cx);
                let surface = panel.read(cx).surface().clone();
                cx.spawn_in(window, async move |_, cx| {
                    let mut answer =
                        crate::features::browser::automation::opened(surface, cx).await;
                    if let Some(state) = answer.get_mut("ok") {
                        state["tab"] = json!(tab);
                    }
                    let _ = reply.send(answer);
                })
                .detach();
            }
            other => {
                let tab = other.tab().unwrap_or_default().to_string();
                let Some((id, panel)) = self.find_stage(&tab, cx) else {
                    let _ = reply.send(not_found(&tab));
                    return;
                };
                match other {
                    Op::Close { .. } => {
                        self.remove_stage(id, window, cx);
                        let _ = reply.send(ok(json!({ "closed": tab })));
                    }
                    Op::Release { .. } => {
                        panel.update(cx, |panel, cx| panel.set_driven(false, cx));
                        let _ = reply.send(ok(json!({ "released": tab })));
                    }
                    op => {
                        if op.drives() {
                            panel.update(cx, |panel, cx| panel.set_driven(true, cx));
                        }
                        // The user picks on a page they can see.
                        if matches!(op, Op::Pick { .. }) {
                            self.set_stage_shown(id, true, window, cx);
                        }
                        let surface = panel.read(cx).surface().clone();
                        let weak = panel.downgrade();
                        cx.spawn_in(window, async move |_, cx| {
                            let answer =
                                crate::features::browser::automation::run(op, surface, cx).await;
                            // The header follows where the agent took the page.
                            if let Some(url) = answer["ok"]["url"].as_str() {
                                let url = url.to_string();
                                let _ = weak.update(cx, |panel, cx| panel.set_url(url, cx));
                            }
                            let _ = reply.send(answer);
                        })
                        .detach();
                    }
                }
            }
        }
    }

    /// A page by the name it was opened with, or its number in `tabs`.
    fn find_stage(&self, tab: &str, cx: &App) -> Option<(u64, Entity<BrowserPanel>)> {
        let tab = tab.trim();
        self.stages
            .iter()
            .find(|stage| stage.panel.read(cx).name() == Some(tab))
            .or_else(|| {
                let n: usize = tab.parse().ok()?;
                self.stages.get(n.checked_sub(1)?)
            })
            .map(|stage| (stage.id, stage.panel.clone()))
    }

    /// What `gentle-browser` calls a page: its name, or its number.
    fn tab_label(&self, id: u64, cx: &App) -> String {
        let Some(index) = self.stages.iter().position(|stage| stage.id == id) else {
            return String::new();
        };
        self.stages[index]
            .panel
            .read(cx)
            .name()
            .map_or_else(|| (index + 1).to_string(), str::to_string)
    }

    fn browser_tabs(&self, cx: &App) -> Vec<serde_json::Value> {
        self.stages
            .iter()
            .map(|stage| {
                let panel = stage.panel.read(cx);
                serde_json::json!({
                    "tab": self.tab_label(stage.id, cx),
                    "url": panel.url(),
                    "title": panel.title(),
                    "shown": stage.shown,
                    "driven": panel.is_driven(),
                    "profile": panel.profile(cx).key(),
                })
            })
            .collect()
    }

    /// A screen or dialog opened or closed over the window: web views hide
    /// under it.
    pub(super) fn set_stages_covered(&self, covered: bool, cx: &mut App) {
        for stage in self.stages.iter().filter(|stage| stage.shown) {
            stage
                .panel
                .update(cx, |panel, cx| panel.set_covered(covered, cx));
        }
        self.devices
            .update(cx, |panel, cx| panel.set_covered(covered, cx));
    }

    /// Tell agent panes whether their web views (an expanded HTML surface)
    /// may show: only those in the shown tab, with nothing over them. Only
    /// changes reach the panes, so hidden tabs aren't touched every frame.
    /// Tower panes are told apart: a hidden tab drops their page, while a
    /// screen or popover over them only conceals it, so it keeps its state.
    pub(super) fn sync_web_views(&mut self, covered: bool, cx: &mut App) {
        let in_tab: std::collections::HashSet<PaneId> = self
            .tab()
            .map(|tab| tab.leaves().into_iter().collect())
            .unwrap_or_default();
        for (id, pane) in &self.panes {
            if pane.item.kind == PaneKind::Tower
                && let Ok(tower) = pane.item.view.clone().downcast::<TowerView>()
            {
                let shown = in_tab.contains(id);
                tower.update(cx, |tower, cx| tower.set_shown(shown, covered, cx));
            }
        }
        let shown: std::collections::HashSet<PaneId> = if covered {
            Default::default()
        } else {
            in_tab
                .into_iter()
                .filter(|id| self.panes.get(id).is_some_and(|p| p.session.is_some()))
                .collect()
        };
        if shown == self.web_shown {
            return;
        }
        for (id, pane) in &self.panes {
            let Some(session) = &pane.session else {
                continue;
            };
            let on = shown.contains(id);
            if on != self.web_shown.contains(id) {
                session.update(cx, |session, cx| session.set_web_shown(on, cx));
            }
        }
        self.web_shown = shown;
    }

    pub(super) fn shown_stages(&self) -> usize {
        self.stages.iter().filter(|stage| stage.shown).count()
    }

    /// Dock the shown stages at the bottom-right ([`floating::docked`]): side
    /// by side at their natural size, shrunk together when they don't fit in
    /// the part of the window they may take, wrapped into rows past that.
    /// Cards stay draggable and resizable afterwards.
    pub(super) fn tile_stages(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let shown: Vec<&Stage> = self.stages.iter().filter(|stage| stage.shown).collect();
        let frame = crate::features::browser::DEFAULT_FRAME;
        let sizes: Vec<(f32, f32)> = shown.iter().map(|_| (frame.w, frame.h)).collect();
        let size = window.viewport_size();
        let title_bar = f32::from(rems(floating::TITLE_BAR / 16.).to_pixels(window.rem_size()));
        let frames = floating::docked(
            &sizes,
            (size.width.as_f32(), size.height.as_f32()),
            title_bar,
        );
        for (stage, frame) in shown.iter().zip(frames) {
            stage
                .panel
                .update(cx, |panel, cx| panel.set_frame(frame, cx));
        }
    }

    // ── The tray ────────────────────────────────────────────────────────────

    /// The title bar's circle: how many instances are open, colored by the
    /// one that most needs a look, and the list of them in a popover. Nothing
    /// while there are none.
    pub(super) fn render_tray(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        if self.stages.is_empty() {
            return None;
        }
        let rows: Vec<TrayRow> = self
            .stages
            .iter()
            .map(|stage| {
                let (title, status) = stage.summary.clone();
                TrayRow {
                    id: stage.id,
                    design: stage.panel.read(cx).is_design(),
                    title,
                    status,
                    color: if stage.shown {
                        ui::accent()
                    } else {
                        ui::faint()
                    },
                    source: stage
                        .origin
                        .map(|origin| self.pane_title(origin))
                        .unwrap_or_default(),
                    shown: stage.shown,
                }
            })
            .collect();
        let count = rows.len();
        let weak = cx.entity().downgrade();
        Some(
            div()
                // Clicks here are not title-bar drags.
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .child(
                    Popover::new("instances")
                        .anchor(Anchor::TopRight)
                        .trigger(TrayTrigger { count, open: false })
                        .w(rems(380. / 16.))
                        .p_0()
                        .on_open_change(|open, window, cx| {
                            crate::features::browser::popover_open("instances", *open, window, cx)
                        })
                        .content(move |_, _, cx| {
                            ui::popover_in(
                                ElementId::Name("instances-in".into()),
                                tray_panel(rows.clone(), weak.clone(), cx),
                            )
                            .into_any_element()
                        }),
                )
                .into_any_element(),
        )
    }
}

/// What the tray shows for one stage, captured when the window draws.
#[derive(Clone)]
struct TrayRow {
    id: u64,
    design: bool,
    title: String,
    status: String,
    color: Hsla,
    /// The agent tab that opened it.
    source: SharedString,
    shown: bool,
}

fn tray_panel(
    rows: Vec<TrayRow>,
    workspace: WeakEntity<Workspace>,
    cx: &mut Context<PopoverState>,
) -> Div {
    let any_shown = rows.iter().any(|row| row.shown);
    let header = h_flex()
        .px_3()
        .pt_2p5()
        .pb_2()
        .gap_2()
        .child(
            div()
                .text_size(rems(13. / 16.))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(ui::fg())
                .child("Pages"),
        )
        .child(div().flex_1())
        .when(any_shown, |d| {
            let workspace = workspace.clone();
            d.child(
                div()
                    .id("instances-hide-all")
                    .px_2()
                    .h(rems(24. / 16.))
                    .flex()
                    .items_center()
                    .rounded(rems(6. / 16.))
                    .text_size(rems(11.5 / 16.))
                    .text_color(ui::muted())
                    .cursor_pointer()
                    .hover(|s| s.bg(ui::surface_hover()).text_color(ui::fg()))
                    .child("Hide all")
                    .on_click(move |_, _, cx| {
                        let _ = workspace.update(cx, |this, cx| this.hide_all_stages(cx));
                    }),
            )
        });
    let rows = rows.into_iter().map(|row| {
        let id = row.id;
        let icon = if row.design {
            IconName::Palette
        } else {
            IconName::Globe
        };
        let mut meta = row.source.to_string();
        if !meta.is_empty() {
            meta.push_str("  ·  ");
        }
        meta.push_str(&row.status);
        let open = workspace.clone();
        let toggle = workspace.clone();
        let close = workspace.clone();
        let shown = row.shown;
        h_flex()
            .id(("instance", id as usize))
            .w_full()
            .items_start()
            .gap_2p5()
            .px_2()
            .py_2()
            .rounded(rems(8. / 16.))
            .hover(|s| s.bg(ui::surface_hover()))
            .child(
                div()
                    .mt(px(1.))
                    .size(rems(22. / 16.))
                    .flex_none()
                    .relative()
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(rems(6. / 16.))
                    .bg(row.color.opacity(0.12))
                    .child(Icon::new(icon).xsmall().text_color(row.color)),
            )
            .child(
                // The row's text opens the card and closes the list.
                v_flex()
                    .id(("instance-open", id as usize))
                    .flex_1()
                    .min_w_0()
                    .gap_0p5()
                    .cursor_pointer()
                    .on_click(cx.listener(move |state: &mut PopoverState, _, window, cx| {
                        state.dismiss(window, cx);
                        let _ =
                            open.update(cx, |this, cx| this.set_stage_shown(id, true, window, cx));
                    }))
                    .child(
                        div()
                            .text_size(rems(12.5 / 16.))
                            .truncate()
                            .text_color(if shown { ui::fg() } else { ui::muted() })
                            .child(row.title),
                    )
                    .child(
                        div()
                            .text_size(rems(11. / 16.))
                            .text_color(ui::faint())
                            .truncate()
                            .child(meta),
                    ),
            )
            .child(
                h_flex()
                    .flex_none()
                    .gap_0p5()
                    .child(
                        Button::new(("instance-toggle", id as usize))
                            .ghost()
                            .xsmall()
                            .icon(Icon::new(if shown {
                                IconName::EyeOff
                            } else {
                                IconName::Eye
                            }))
                            .tooltip(if shown { "Hide" } else { "Show" })
                            .on_click(move |_, window, cx| {
                                let _ = toggle.update(cx, |this, cx| {
                                    this.set_stage_shown(id, !shown, window, cx)
                                });
                            }),
                    )
                    .child(
                        Button::new(("instance-close", id as usize))
                            .ghost()
                            .xsmall()
                            .icon(Icon::new(IconName::X))
                            .tooltip("Close")
                            .on_click(move |_, window, cx| {
                                let _ =
                                    close.update(cx, |this, cx| this.remove_stage(id, window, cx));
                            }),
                    ),
            )
    });
    v_flex()
        .w_full()
        .font_family(cx.theme().font_family.clone())
        .child(header)
        .child(div().h(px(1.)).bg(ui::border()))
        .child(
            v_flex()
                .id("instances-list")
                .max_h(rems(420. / 16.))
                .overflow_y_scroll()
                .px_1()
                .py_1()
                .children(rows),
        )
}

/// The circle in the title bar, with a badge counting the pages.
#[derive(IntoElement)]
struct TrayTrigger {
    count: usize,
    open: bool,
}

impl Selectable for TrayTrigger {
    fn selected(mut self, selected: bool) -> Self {
        self.open = selected;
        self
    }

    fn is_selected(&self) -> bool {
        self.open
    }
}

impl RenderOnce for TrayTrigger {
    fn render(self, _: &mut Window, _: &mut App) -> impl IntoElement {
        div()
            .id("instances-tray")
            .relative()
            .size(rems(28. / 16.))
            .flex()
            .items_center()
            .justify_center()
            .rounded_full()
            .cursor_pointer()
            .text_color(ui::fg())
            .hover(|s| s.bg(ui::surface_hover()))
            .when(self.open, |d| d.bg(ui::surface_hover()))
            .when(!self.open, |d| {
                d.tooltip(|window, cx| Tooltip::new("Pages agents opened").build(window, cx))
            })
            .child(Icon::new(IconName::Globe).small())
            .child(
                div()
                    .absolute()
                    .top(px(2.))
                    .right(px(1.))
                    .h(rems(13. / 16.))
                    .min_w(rems(13. / 16.))
                    .px(rems(3. / 16.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded_full()
                    .bg(ui::petal())
                    .text_color(ui::well().opacity(1.))
                    .text_size(rems(9. / 16.))
                    .font_weight(FontWeight::BOLD)
                    .line_height(relative(1.))
                    .child(self.count.to_string()),
            )
    }
}
