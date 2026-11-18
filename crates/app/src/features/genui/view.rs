//! Drawing a diagram surface, React Flow style: a header with the editing
//! tools, a dotted canvas with group boxes, edges, node cards and edge
//! labels, floating controls (bottom-left), a minimap (bottom-right) and the
//! selection's details (top-right, `details.rs`), and a footer with the
//! pending edits; and the conversation rows around it: a surface being
//! drawn, one that failed, older versions and the user's sent edits.

use std::time::Instant;

use gentle_core::genui::diagram::{Direction, EdgeStyle, NodeKind, Status};
use gentle_core::genui::layout::{self, GROUP_PAD, Rect};
use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::Input;
use gpui_kit::component::{Disableable as _, Icon, Sizable as _, Size, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use super::diagram::{DiagramEvent, DiagramView, Drag, READABLE_ZOOM, Selection, SendState};
use super::motion::{self, MARCH_SPEED};
use super::paint::{self, EdgeDraw, EdgeLook, Frame, MiniMap};
use super::pointer::ZOOM_STEP;
use crate::shared::{backdrop, ui};

/// The card's height in the conversation (px at 100 %): a fixed size, so
/// the surface can be a cached view that only redraws when it changes.
pub(super) const CARD_HEIGHT: f32 = 460.;
/// Below this scale node descriptions, the kind · tech line and edge labels
/// are left out: smaller than ~4 px they are noise, not text.
const DETAIL_ZOOM: f32 = 0.35;
/// The minimap's size (px at 100 %).
const MINIMAP_W: f32 = 168.;
const MINIMAP_H: f32 = 108.;

pub(super) fn kind_icon(kind: NodeKind) -> IconName {
    match kind {
        NodeKind::Service => IconName::Server,
        NodeKind::Database => IconName::Database,
        NodeKind::Queue => IconName::Layers,
        NodeKind::Cache => IconName::Zap,
        NodeKind::Client => IconName::Monitor,
        NodeKind::External => IconName::Globe,
        NodeKind::Storage => IconName::HardDrive,
        NodeKind::Function => IconName::SquareFunction,
        NodeKind::Note => IconName::StickyNote,
    }
}

pub(super) fn kind_color(kind: NodeKind) -> Hsla {
    match kind {
        NodeKind::Service | NodeKind::Function => ui::info(),
        NodeKind::Database | NodeKind::Storage => ui::success(),
        NodeKind::Queue => ui::warning(),
        NodeKind::Cache => ui::champagne(),
        NodeKind::Client => ui::accent(),
        NodeKind::External => ui::muted(),
        NodeKind::Note => ui::faint(),
    }
}

pub(super) fn status_color(status: Status) -> Hsla {
    match status {
        Status::Planned => ui::info(),
        Status::Existing => ui::success(),
        Status::Deprecated => ui::faint(),
        Status::Risk => ui::danger(),
    }
}

pub(super) fn style_name(style: EdgeStyle) -> &'static str {
    match style {
        EdgeStyle::Sync => "sync call",
        EdgeStyle::Async => "async",
        EdgeStyle::Data => "data flow",
        EdgeStyle::Dependency => "dependency",
    }
}

/// Where nodes and edges show this frame: the layout, or on their way to
/// it while the agent's update eases in.
struct Shown {
    nodes: Vec<(Rect, f32)>,
    edges: Vec<(layout::EdgePath, f32)>,
    /// Removed by the update, fading out: (index into the arrival's lists).
    leaving: f32,
}

impl Render for DiagramView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let rem = window.rem_size();
        let now = Instant::now();
        let mut tweening = self.advance_view(now);
        let size = self.bounds.get().size;
        let refit = self.viewport.fitted_for.is_none()
            || (!self.viewport.touched
                && self.view_tween.is_none()
                && self.viewport.fitted_for != Some(size));
        if refit && size.width > px(1.) && size.height > px(1.) {
            self.fit(READABLE_ZOOM, rem);
        }
        if self.arrival.as_ref().is_some_and(|a| a.done(now)) {
            self.arrival = None;
        }
        tweening |= self.arrival.is_some();
        self.sync_fields(window, cx);
        let shown = self.shown(now);

        // Ask for the next frame only while something moves.
        let reduce = cx.reduce_motion();
        let motion_now = backdrop::motion_now(cx);
        let active = window.is_window_active();
        let marches = motion::may_move(active, reduce, motion_now);
        let visible = self.visible(rem);
        let marching_on_screen = self
            .working
            .edges
            .iter()
            .zip(&shown.edges)
            .any(|(e, (p, _))| paint::animated(e.style) && p.bounds().intersects(&visible));
        if let Some(delay) =
            motion::frame_delay(tweening, marching_on_screen, active, reduce, motion_now)
        {
            ui::request_frame_in(delay, window, cx);
        }
        let march = marches.then(|| ui::live_clock() * MARCH_SPEED);

        v_flex()
            .track_focus(&self.focus)
            .on_key_down(cx.listener(Self::key_down))
            .size_full()
            .rounded_xl()
            .bg(ui::surface())
            .border_1()
            .border_color(ui::border())
            .overflow_hidden()
            .child(self.render_header(cx))
            .child(self.render_canvas(rem, &shown, march, cx))
            .child(self.render_footer(cx))
    }
}

impl DiagramView {
    fn shown(&self, now: Instant) -> Shown {
        let Some(arrival) = &self.arrival else {
            return Shown {
                nodes: self.layout.nodes.iter().map(|r| (*r, 1.)).collect(),
                edges: self.layout.edges.iter().map(|p| (p.clone(), 1.)).collect(),
                leaving: 0.,
            };
        };
        let p = arrival.progress(now);
        Shown {
            nodes: self
                .working
                .nodes
                .iter()
                .zip(&self.layout.nodes)
                .map(|(n, r)| arrival.node(&n.id, *r, p))
                .collect(),
            edges: self
                .working
                .edges
                .iter()
                .zip(&self.layout.edges)
                .map(|(e, path)| arrival.edge(&e.id, path, p))
                .collect(),
            leaving: 1. - p,
        }
    }

    fn render_header(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let tool = |id: &'static str, icon: IconName, tip: &'static str| {
            Button::new(id)
                .debug_selector(|| id.into())
                .ghost()
                .xsmall()
                .text_color(ui::muted())
                .icon(Icon::new(icon))
                .tooltip(tip)
        };
        let nodes = self.working.nodes.len();
        let edges = self.working.edges.len();
        let has_nodes = self
            .selection
            .as_ref()
            .is_some_and(|s| !s.nodes().is_empty());
        let (direction_icon, direction_tip) = match self.working.direction {
            Direction::LeftRight => (
                IconName::ArrowRight,
                "Left to right (switch to top to bottom)",
            ),
            Direction::TopBottom => (
                IconName::ArrowDown,
                "Top to bottom (switch to left to right)",
            ),
        };
        h_flex()
            .px_3()
            .py_1p5()
            .gap_2()
            .min_w_0()
            .child(Icon::new(IconName::Workflow).small().text_color(ui::info()))
            .child(
                div()
                    .min_w_0()
                    .flex_shrink(1.)
                    .truncate()
                    .text_size(rems(13. / 16.))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(ui::champagne())
                    .child(if self.title.is_empty() {
                        "Diagram".to_string()
                    } else {
                        self.title.clone()
                    }),
            )
            .child(ui::chip(format!("v{}", self.version), ui::info()).flex_shrink_0())
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .font_family(ui::MONO)
                    .text_size(rems(12. / 16.))
                    .text_color(ui::muted())
                    .child(format!(
                        "{nodes} node{} · {edges} edge{}",
                        if nodes == 1 { "" } else { "s" },
                        if edges == 1 { "" } else { "s" }
                    )),
            )
            .child(
                h_flex()
                    .flex_shrink_0()
                    .gap_0p5()
                    .child(
                        tool("genui-add", IconName::Plus, "Add node")
                            .disabled(self.locked)
                            .on_click(cx.listener(|this, _, window, cx| this.add_node(window, cx))),
                    )
                    .child(
                        tool("genui-group", IconName::Group, "Group the selected nodes")
                            .disabled(self.locked || !has_nodes)
                            .on_click(
                                cx.listener(|this, _, window, cx| this.group_selection(window, cx)),
                            ),
                    )
                    .child(
                        tool("genui-direction", direction_icon, direction_tip)
                            .disabled(self.locked)
                            .on_click(cx.listener(|this, _, _, cx| this.toggle_direction(cx))),
                    )
                    .child(
                        tool("genui-undo", IconName::Undo2, "Undo")
                            .disabled(!self.can_undo())
                            .on_click(cx.listener(|this, _, _, cx| this.undo(cx))),
                    )
                    .child(
                        tool("genui-redo", IconName::Redo2, "Redo")
                            .disabled(!self.can_redo())
                            .on_click(cx.listener(|this, _, _, cx| this.redo(cx))),
                    )
                    .child(super::share::tools(
                        &self.env,
                        "diagram",
                        DiagramView::source,
                        cx,
                    ))
                    .child(
                        tool(
                            "genui-expand",
                            if self.expanded {
                                IconName::Minimize2
                            } else {
                                IconName::Maximize2
                            },
                            if self.expanded {
                                "Back to the conversation"
                            } else {
                                "Expand"
                            },
                        )
                        .on_click(cx.listener(|this, _, _, cx| {
                            cx.emit(if this.expanded {
                                DiagramEvent::Collapse
                            } else {
                                DiagramEvent::Expand
                            })
                        })),
                    ),
            )
    }

    fn render_canvas(
        &self,
        rem: Pixels,
        shown: &Shown,
        march: Option<f32>,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let s = self.scale(rem);
        let pan = self.viewport.pan;
        let visible = self.visible(rem).inflate(40.);
        let at = |x: f32, y: f32| (px(x * s + pan.x), px(y * s + pan.y));
        let selected_edge = match &self.selection {
            Some(Selection::Edge(id)) => self.working.edges.iter().position(|e| &e.id == id),
            _ => None,
        };
        let selected_group = match &self.selection {
            Some(Selection::Group(id)) => self.working.groups.iter().position(|g| &g.id == id),
            _ => None,
        };
        let connect = match &self.drag {
            Some(Drag::Connect { node, to, .. }) => self
                .working
                .node_index(node)
                .map(|ix| (self.layout.nodes[ix].center(), *to)),
            _ => None,
        };
        let select = match &self.drag {
            Some(Drag::Select { from, to }) => Some(Rect {
                x: from.x.min(to.x),
                y: from.y.min(to.y),
                w: (to.x - from.x).abs(),
                h: (to.y - from.y).abs(),
            }),
            _ => None,
        };
        // Group boxes follow their members while they ease in.
        let groups: Vec<(Rect, bool)> = self
            .working
            .groups
            .iter()
            .enumerate()
            .filter_map(|(gi, g)| {
                let target = self.layout.groups.get(gi).copied().flatten()?;
                let members = self
                    .working
                    .nodes
                    .iter()
                    .zip(&shown.nodes)
                    .filter(|(n, _)| n.group.as_deref() == Some(g.id.as_str()))
                    .map(|(_, (r, _))| *r)
                    .reduce(|a, b| a.union(&b));
                let rect = match (self.arrival.is_some(), members) {
                    (true, Some(union)) => {
                        let mut r = union.inflate(GROUP_PAD);
                        r.y -= layout::GROUP_HEADER;
                        r.h += layout::GROUP_HEADER;
                        r
                    }
                    _ => target,
                };
                Some((rect, selected_group == Some(gi)))
            })
            .collect();
        let group_rects: Vec<Option<Rect>> = {
            let mut it = groups.iter();
            self.layout
                .groups
                .iter()
                .map(|g| g.and_then(|_| it.next().map(|(r, _)| *r)))
                .collect()
        };
        let mut edges: Vec<EdgeDraw> = self
            .arrival
            .iter()
            .flat_map(|a| a.leaving_edges.iter())
            .map(|(path, style)| EdgeDraw {
                path: path.clone(),
                style: *style,
                alpha: shown.leaving,
                look: EdgeLook::Normal,
            })
            .collect();
        edges.extend(self.working.edges.iter().zip(&shown.edges).enumerate().map(
            |(ix, (e, (path, alpha)))| EdgeDraw {
                path: path.clone(),
                style: e.style,
                alpha: *alpha,
                look: if selected_edge == Some(ix) {
                    EdgeLook::Selected
                } else if self.hover_edge == Some(ix) {
                    EdgeLook::Hovered
                } else {
                    EdgeLook::Normal
                },
            },
        ));
        let frame = Frame {
            groups,
            edges,
            scale: s,
            pan,
            march,
            connect,
            select,
        };
        let cell = self.bounds.clone();
        let painter = canvas(
            move |bounds, window, _| {
                let old = cell.get();
                cell.set(bounds);
                // The fit and the visible edges were worked out for the
                // last size: render again for this one on the next frame
                // (notifying from prepaint would draw this frame stale).
                if (old.size.width - bounds.size.width).abs() > px(1.)
                    || (old.size.height - bounds.size.height).abs() > px(1.)
                {
                    window.request_animation_frame();
                }
            },
            move |bounds, _, window, _| frame.paint(bounds, window),
        )
        .absolute()
        .size_full();

        let mut body = div()
            .id("genui-canvas")
            .relative()
            .flex_1()
            .min_h_0()
            .overflow_hidden()
            .bg(ui::well())
            .cursor(match self.drag {
                Some(Drag::Pan { .. }) => CursorStyle::ClosedHand,
                Some(Drag::Connect { .. } | Drag::Select { .. }) => CursorStyle::Crosshair,
                _ if self.hover_edge.is_some() => CursorStyle::PointingHand,
                _ => CursorStyle::OpenHand,
            })
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, event: &MouseDownEvent, window, cx| {
                    this.press_background(event, window, cx)
                }),
            )
            .on_mouse_move(cx.listener(Self::pointer_moved))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, _, _, cx| this.released(cx)),
            )
            .on_mouse_up_out(
                MouseButton::Left,
                cx.listener(|this, _, _, cx| this.released(cx)),
            )
            .on_scroll(cx.listener(Self::scrolled))
            .on_pinch(cx.listener(Self::pinched))
            .child(painter);

        // Group titles, in their boxes' top padding: press to select and
        // drag the group, double-click to rename.
        for (gi, (group, rect)) in self.working.groups.iter().zip(&group_rects).enumerate() {
            let Some(rect) = rect.filter(|r| r.intersects(&visible)) else {
                continue;
            };
            let (x, y) = at(rect.x + GROUP_PAD * 0.75, rect.y + 5.);
            let id = group.id.clone();
            let selected = selected_group == Some(gi);
            body = body.child(
                div()
                    .id(("genui-group", gi))
                    .absolute()
                    .left(x)
                    .top(y)
                    .max_w(px((rect.w - GROUP_PAD) * s))
                    .px(px(4. * s))
                    .rounded(px(4. * s))
                    .truncate()
                    .cursor(CursorStyle::PointingHand)
                    .text_size(px(11. * s))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(if selected { ui::accent() } else { ui::muted() })
                    .hover(|d| d.text_color(ui::fg()))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                            this.press_group(&id, event, window, cx)
                        }),
                    )
                    .child(group.label.clone()),
            );
        }

        // Edge labels: the phrase on the curve, up to two lines, on the
        // canvas color so the line doesn't run through the text; the
        // style (async, data flow, dependency) under it in MONO.
        if s >= DETAIL_ZOOM {
            for (ix, (edge, (path, alpha))) in
                self.working.edges.iter().zip(&shown.edges).enumerate()
            {
                let Some(label) = edge.label.as_ref().filter(|l| !l.is_empty()) else {
                    continue;
                };
                let p = path.label_at();
                if !visible.contains(p) {
                    continue;
                }
                let (width, height) = (layout::RANK_GAP + 24., 48.);
                let (x, y) = at(p.x - width / 2., p.y - height / 2.);
                let highlighted = selected_edge == Some(ix) || self.hover_edge == Some(ix);
                let style =
                    (edge.style != EdgeStyle::Sync && s >= 0.5).then(|| style_name(edge.style));
                body = body.child(
                    v_flex()
                        .absolute()
                        .left(x)
                        .top(y)
                        .w(px(width * s))
                        .h(px(height * s))
                        .items_center()
                        .justify_center()
                        .opacity(*alpha)
                        .child(
                            v_flex()
                                .max_w_full()
                                .items_center()
                                .px(px(4. * s))
                                .rounded(px(4. * s))
                                .bg(ui::well())
                                .child(
                                    div()
                                        .max_w_full()
                                        .overflow_hidden()
                                        .line_clamp(2)
                                        .text_center()
                                        .text_size(px(10.5 * s))
                                        .text_color(if highlighted {
                                            ui::accent()
                                        } else {
                                            ui::muted()
                                        })
                                        .child(label.clone()),
                                )
                                .when_some(style, |d, style| {
                                    d.child(
                                        div()
                                            .font_family(ui::MONO)
                                            .text_size(px(9. * s))
                                            .text_color(ui::faint())
                                            .child(style),
                                    )
                                }),
                        ),
                );
            }
        }

        // Nodes the update removed, fading out where they were.
        if let Some(arrival) = &self.arrival {
            for (node, rect) in &arrival.leaving_nodes {
                if !rect.intersects(&visible) {
                    continue;
                }
                let (x, y) = at(rect.x, rect.y);
                body = body.child(
                    node_card(node, *rect, s)
                        .absolute()
                        .left(x)
                        .top(y)
                        .opacity(shown.leaving),
                );
            }
        }

        let selected_nodes = self.selection.as_ref().map(|s| s.nodes()).unwrap_or(&[]);
        for (ix, node) in self.working.nodes.iter().enumerate() {
            let (rect, alpha) = shown.nodes[ix];
            if !rect.intersects(&visible) {
                continue;
            }
            let (x, y) = at(rect.x, rect.y);
            let selected = selected_nodes.contains(&node.id);
            body = body.child(self.render_node(ix, x, y, rect, alpha, s, selected, cx));
        }

        if let Some(rename) = &self.rename {
            let place = match &rename.target {
                Selection::Node(id) => self.working.node_index(id).map(|ix| self.layout.nodes[ix]),
                Selection::Edge(id) => {
                    self.working
                        .edges
                        .iter()
                        .position(|e| &e.id == id)
                        .map(|ix| {
                            let p = self.layout.edges[ix].label_at();
                            Rect {
                                x: p.x - layout::NODE_W / 2.,
                                y: p.y - 14.,
                                w: layout::NODE_W,
                                h: 28.,
                            }
                        })
                }
                Selection::Group(id) => self
                    .working
                    .groups
                    .iter()
                    .position(|g| &g.id == id)
                    .and_then(|gi| self.layout.groups[gi])
                    .map(|r| Rect {
                        x: r.x + GROUP_PAD / 2.,
                        y: r.y,
                        w: layout::NODE_W,
                        h: layout::GROUP_HEADER,
                    }),
                Selection::Nodes(_) => None,
            };
            if let Some(rect) = place {
                let (x, y) = at(rect.x, rect.y + rect.h / 2.);
                body = body.child(
                    div()
                        .absolute()
                        .left(x)
                        .top(y - rems(15. / 16.).to_pixels(rem))
                        .w(px(rect.w * s).max(rems(160. / 16.).to_pixels(rem)))
                        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                        .child(
                            ui::field()
                                .h(rems(30. / 16.))
                                .bg(ui::popover_bg())
                                .border_color(ui::accent().opacity(0.6))
                                .child(Input::new(&rename.input).appearance(false).small()),
                        ),
                );
            }
        }

        if self.working.nodes.is_empty() {
            body = body.child(
                v_flex()
                    .absolute()
                    .inset_0()
                    .items_center()
                    .justify_center()
                    .gap_2()
                    .child(Icon::new(IconName::Workflow).text_color(ui::faint()))
                    .child(
                        div()
                            .text_sm()
                            .text_color(ui::muted())
                            .child("Nothing drawn yet. Add a node with +."),
                    ),
            );
        }
        if let Some(error) = &self.error {
            body = body.child(
                div().absolute().left_2().right_2().top_2().child(
                    ui::card()
                        .px_3()
                        .py_2()
                        .gap_1()
                        .bg(ui::popover_bg())
                        .border_color(ui::danger().opacity(0.35))
                        .child(
                            h_flex()
                                .gap_2()
                                .text_size(rems(12. / 16.))
                                .text_color(ui::danger())
                                .child(Icon::new(IconName::CircleAlert).xsmall())
                                .child("The agent's latest version can't be drawn"),
                        )
                        .child(
                            div()
                                .font_family(ui::MONO)
                                .text_size(rems(11. / 16.))
                                .text_color(ui::muted())
                                .whitespace_normal()
                                .child(error.clone()),
                        ),
                ),
            );
        }
        body.child(self.render_controls(cx))
            .children(self.render_minimap(rem, shown, cx))
            .children(self.render_details(cx))
    }

    #[allow(clippy::too_many_arguments)]
    fn render_node(
        &self,
        ix: usize,
        x: Pixels,
        y: Pixels,
        rect: Rect,
        alpha: f32,
        s: f32,
        selected: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let node = &self.working.nodes[ix];
        let id = node.id.clone();
        let dragging = selected && matches!(&self.drag, Some(Drag::Nodes { moved: true, .. }));
        let lr = self.working.direction == Direction::LeftRight;
        let handle = |outgoing: bool| {
            let id = node.id.clone();
            let size = 10. * s;
            let along = if lr { rect.h } else { rect.w } * s / 2. - size / 2.;
            div()
                .id((
                    if outgoing {
                        "genui-handle-out"
                    } else {
                        "genui-handle-in"
                    },
                    ix,
                ))
                .absolute()
                .size(px(size))
                .rounded_full()
                .bg(ui::surface())
                .border_2()
                .border_color(ui::accent())
                .cursor(CursorStyle::Crosshair)
                .map(|d| match (lr, outgoing) {
                    (true, true) => d.right(px(-size / 2.)).top(px(along)),
                    (true, false) => d.left(px(-size / 2.)).top(px(along)),
                    (false, true) => d.bottom(px(-size / 2.)).left(px(along)),
                    (false, false) => d.top(px(-size / 2.)).left(px(along)),
                })
                .when(!selected || self.locked, |d| {
                    d.opacity(0.).when(!self.locked, |d| {
                        d.group_hover("genui-node", |d| d.opacity(1.))
                    })
                })
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                        this.press_handle(&id, outgoing, event, window, cx)
                    }),
                )
        };
        let card = node_card(node, rect, s)
            .id(("genui-node", ix))
            .group("genui-node")
            .absolute()
            .left(x)
            .top(y)
            .opacity(alpha)
            .when(selected, |d| d.border_color(ui::accent()))
            .when(dragging, |d| d.shadow_lg())
            .cursor(if self.locked {
                CursorStyle::PointingHand
            } else {
                CursorStyle::OpenHand
            })
            .hover(|d| d.bg(ui::surface_hover()))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                    this.press_node(&id, event, window, cx)
                }),
            )
            // The selection ring fades in around the card.
            .when(selected, |d| {
                d.child(ui::popover_in(
                    ("genui-ring", ix),
                    div()
                        .absolute()
                        .inset(px(-4. * s))
                        .rounded(px(12. * s))
                        .border_2()
                        .border_color(ui::accent().opacity(0.35)),
                ))
            })
            .when_some(node.code_ref.clone().filter(|_| s >= 0.5), |d, code_ref| {
                let selector = format!("genui-node-ref-{}", node.id);
                let found = matches!(
                    self.env.ref_state(&code_ref),
                    super::share::RefState::Found(_) | super::share::RefState::Unknown
                );
                d.child(
                    div()
                        .id(("genui-node-ref", ix))
                        .debug_selector(move || selector)
                        .absolute()
                        .top(px(4. * s))
                        .right(px(4. * s))
                        .size(px(18. * s))
                        .flex()
                        .items_center()
                        .justify_center()
                        .rounded(px(5. * s))
                        .cursor_pointer()
                        .text_color(if found { ui::accent() } else { ui::faint() })
                        .hover(|d| d.bg(ui::surface_hover()))
                        .child(
                            Icon::new(if found {
                                IconName::FileCode
                            } else {
                                IconName::FileX
                            })
                            .size(px(12. * s)),
                        )
                        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                        .on_click(cx.listener(move |_, _, _, cx| {
                            cx.stop_propagation();
                            cx.emit(DiagramEvent::OpenRef(code_ref.clone()));
                        })),
                )
            })
            .child(handle(false))
            .child(handle(true));
        card.into_any_element()
    }

    /// React Flow's controls: zoom, fit and lock, floating bottom-left.
    fn render_controls(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let control = |id: &'static str, icon: IconName, tip: &'static str| {
            Button::new(id)
                .debug_selector(|| id.into())
                .ghost()
                .xsmall()
                .text_color(ui::muted())
                .icon(Icon::new(icon))
                .tooltip(tip)
        };
        v_flex()
            .absolute()
            .left_2()
            .bottom_2()
            .p_0p5()
            .gap_0p5()
            .rounded_lg()
            .bg(ui::popover_bg())
            .border_1()
            .border_color(ui::border())
            .shadow_sm()
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .child(
                control("genui-zoom-in", IconName::ZoomIn, "Zoom in").on_click(cx.listener(
                    |this, _, window, cx| {
                        this.zoom_center(ZOOM_STEP, window.rem_size(), cx);
                        cx.notify();
                    },
                )),
            )
            .child(
                control("genui-zoom-out", IconName::ZoomOut, "Zoom out").on_click(cx.listener(
                    |this, _, window, cx| {
                        this.zoom_center(1. / ZOOM_STEP, window.rem_size(), cx);
                        cx.notify();
                    },
                )),
            )
            .child(
                control("genui-fit", IconName::Scan, "Fit the whole diagram").on_click(
                    cx.listener(|this, _, window, cx| {
                        this.fit_animated(window.rem_size(), cx);
                        cx.notify();
                    }),
                ),
            )
            .child(
                control(
                    "genui-lock",
                    if self.locked {
                        IconName::Lock
                    } else {
                        IconName::LockOpen
                    },
                    if self.locked {
                        "Unlock editing"
                    } else {
                        "Lock: view only"
                    },
                )
                .when(self.locked, |b| b.text_color(ui::accent()))
                .on_click(cx.listener(|this, _, _, cx| this.toggle_lock(cx))),
            )
    }

    /// The minimap, bottom-right: press or drag to move the view there.
    fn render_minimap(
        &self,
        rem: Pixels,
        shown: &Shown,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        if self.working.nodes.is_empty() {
            return None;
        }
        let view = self.visible(rem);
        let map = MiniMap {
            nodes: self
                .working
                .nodes
                .iter()
                .zip(&shown.nodes)
                .map(|(n, (r, _))| (*r, kind_color(n.kind).opacity(0.7)))
                .collect(),
            content: self.layout.bounds.union(&view),
            view,
        };
        let cell = self.minimap.clone();
        Some(
            div()
                .id("genui-minimap")
                .absolute()
                .right_2()
                .bottom_2()
                .w(rems(MINIMAP_W / 16.))
                .h(rems(MINIMAP_H / 16.))
                .rounded_lg()
                .bg(ui::popover_bg())
                .border_1()
                .border_color(ui::border())
                .shadow_sm()
                .overflow_hidden()
                .cursor(CursorStyle::PointingHand)
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, event: &MouseDownEvent, window, cx| {
                        this.press_minimap(event, window, cx)
                    }),
                )
                .on_scroll(|_, _, cx| cx.stop_propagation())
                .child(
                    canvas(
                        move |bounds, _, _| cell.set(bounds),
                        move |bounds, _, window, _| map.paint(bounds, window),
                    )
                    .size_full(),
                )
                .into_any_element(),
        )
    }

    fn render_footer(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let footer = h_flex()
            .px_3()
            .py_1p5()
            .gap_2()
            .min_h(rems(38. / 16.))
            .border_t_1()
            .border_color(ui::border())
            .text_size(rems(12. / 16.));
        if let Some(refused) = &self.refused {
            return footer
                .text_color(ui::danger())
                .child(Icon::new(IconName::CircleAlert).xsmall())
                .child(div().flex_1().min_w_0().truncate().child(refused.clone()));
        }
        if self.pending > 0 {
            let sending = matches!(self.send, SendState::Sending { .. });
            let failed = match &self.send {
                SendState::Failed(error) => Some(error.clone()),
                _ => None,
            };
            let retry = failed.is_some();
            return footer
                .child(ui::chip(
                    format!(
                        "{} change{}",
                        self.pending,
                        if self.pending == 1 { "" } else { "s" }
                    ),
                    ui::accent(),
                ))
                .child(match (&failed, &self.note) {
                    (Some(error), _) => h_flex()
                        .id("genui-send-error")
                        .debug_selector(|| "genui-send-error".into())
                        .flex_1()
                        .min_w_0()
                        .gap_1p5()
                        .text_color(ui::danger())
                        .child(Icon::new(IconName::CircleAlert).xsmall().flex_shrink_0())
                        .child(div().min_w_0().truncate().child(error.clone()))
                        .tooltip({
                            let error = error.clone();
                            move |window, cx| {
                                gpui_kit::component::tooltip::Tooltip::new(error.clone())
                                    .build(window, cx)
                            }
                        })
                        .into_any_element(),
                    (None, Some(note)) => ui::field()
                        .flex_1()
                        .min_w_0()
                        .h(rems(26. / 16.))
                        .child(Input::new(note).appearance(false).xsmall())
                        .into_any_element(),
                    (None, None) => div()
                        .id("genui-add-note")
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .cursor_pointer()
                        .text_color(ui::faint())
                        .hover(|d| d.text_color(ui::muted()))
                        .child("Add a note…")
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.note_input(window, cx);
                            if let Some(note) = &this.note {
                                note.update(cx, |state, cx| state.focus(window, cx));
                            }
                            cx.notify();
                        }))
                        .into_any_element(),
                })
                .child(
                    Button::new("genui-discard")
                        .debug_selector(|| "genui-discard".into())
                        .ghost()
                        .xsmall()
                        .label("Discard")
                        .disabled(sending)
                        .on_click(cx.listener(|this, _, _, cx| this.discard(cx))),
                )
                .child(
                    Button::new("genui-send")
                        .debug_selector(|| "genui-send".into())
                        .primary()
                        .xsmall()
                        .icon(Icon::new(if retry {
                            IconName::RefreshCw
                        } else {
                            IconName::Send
                        }))
                        .label(if retry { "Retry" } else { "Send to agent" })
                        .loading(sending)
                        .on_click(cx.listener(|this, _, _, cx| this.send(cx))),
                );
        }
        match &self.send {
            SendState::Sent => footer
                .text_color(ui::success())
                .child(Icon::new(IconName::CircleCheck).xsmall())
                .child("Sent to the agent"),
            _ => footer.text_color(ui::faint()).child(
                div().flex_1().min_w_0().truncate().child(if self.locked {
                    "View only · unlock to edit"
                } else {
                    "Drag to move · double-click to rename · drag ● to connect · shift-drag to select · ⌘/Ctrl-scroll to zoom"
                }),
            ),
        }
    }
}

/// A node's card, read top to bottom: the kind tile and title, what the node
/// does (its description, at most two lines) and `KIND · tech` in `MONO`.
/// The kind sets its weight: a client is an actor (its icon in a badge above
/// the card, on `raised`), an external system sits sunken on `well` with a
/// lighter border, a note has no tile. Status changes the border (risk),
/// dashes it (planned) or dims the card (deprecated), and puts a dot by the
/// title.
fn node_card(node: &gentle_core::genui::diagram::Node, rect: Rect, s: f32) -> Div {
    let kind = node.kind;
    let actor = kind == NodeKind::Client;
    let external = kind == NodeKind::External;
    let note = kind == NodeKind::Note;
    let color = kind_color(kind);
    let border = match node.status {
        Some(Status::Risk) => ui::danger().opacity(0.7),
        _ if external || note => ui::border(),
        _ => ui::border_strong(),
    };
    let meta = match node.tech.as_deref().filter(|t| !t.is_empty()) {
        Some(tech) => format!("{} · {tech}", kind.name().to_uppercase()),
        None => kind.name().to_uppercase(),
    };
    let description = node
        .description
        .clone()
        .filter(|d| !d.trim().is_empty() && s >= DETAIL_ZOOM);
    let tile = div()
        .flex_shrink_0()
        .size(px(20. * s))
        .rounded(px(6. * s))
        .bg(color.opacity(if external { 0.08 } else { 0.14 }))
        .flex()
        .items_center()
        .justify_center()
        .child(
            Icon::new(kind_icon(kind))
                .size(px(13. * s))
                .text_color(color),
        );
    let header = h_flex()
        .w_full()
        .min_w_0()
        .gap(px(8. * s))
        .when(!actor && !note, |d| d.child(tile))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .truncate()
                .when(actor, |d| d.text_center())
                .text_size(px(13. * s))
                .line_height(px(16. * s))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(ui::fg())
                .child(node.label.clone()),
        )
        .when_some(node.status.filter(|_| s >= 0.6), |d, status| {
            d.child(
                div()
                    .flex_shrink_0()
                    .size(px(7. * s))
                    .rounded_full()
                    .bg(status_color(status)),
            )
        });
    v_flex()
        .relative()
        .w(px(rect.w * s))
        .h(px(rect.h * s))
        .px(px(12. * s))
        .py(px(9. * s))
        .gap(px(4. * s))
        .rounded(px(10. * s))
        .bg(if actor {
            ui::raised()
        } else if external || note {
            ui::well()
        } else {
            ui::surface()
        })
        .border_1()
        .border_color(border)
        .when(!external && !note, |d| d.shadow_sm())
        .when(node.status == Some(Status::Planned), |d| d.border_dashed())
        .when(node.status == Some(Status::Deprecated), |d| d.opacity(0.55))
        .child(header)
        .when_some(description, |d, text| {
            d.child(
                div()
                    .flex_shrink_0()
                    .w_full()
                    .min_w_0()
                    .overflow_hidden()
                    .line_clamp(2)
                    .when(actor, |d| d.text_center())
                    .when(note, |d| d.italic())
                    .text_size(px(11. * s))
                    .line_height(px(14. * s))
                    .text_color(ui::muted())
                    .child(text),
            )
        })
        .child(div().flex_1())
        .when(s >= DETAIL_ZOOM, |d| {
            d.child(
                div()
                    .flex_shrink_0()
                    .w_full()
                    .truncate()
                    .when(actor, |d| d.text_center())
                    .font_family(ui::MONO)
                    .text_size(px(9.5 * s))
                    .line_height(px(12. * s))
                    .text_color(ui::faint())
                    .child(meta),
            )
        })
        // An actor wears its kind icon in a round badge above the card.
        .when(actor, |d| {
            let size = 26. * s;
            d.child(
                div()
                    .absolute()
                    .top(px(-size / 2.))
                    .left(px(rect.w * s / 2. - size / 2.))
                    .size(px(size))
                    .rounded_full()
                    .bg(ui::surface())
                    .border_1()
                    .border_color(ui::border_strong())
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(
                        Icon::new(kind_icon(kind))
                            .size(px(13. * s))
                            .text_color(color),
                    ),
            )
        })
}

// ── Conversation rows ──────────────────────────────────────────────────────

fn row(icon: IconName, color: Hsla, title: String, note: Option<String>) -> Div {
    ui::card()
        .flex_row()
        .w_full()
        .items_center()
        .gap_2()
        .px_3()
        .py_2()
        .child(Icon::new(icon).small().text_color(color))
        .child(
            div()
                .min_w_0()
                .flex_shrink(1.)
                .truncate()
                .text_size(rems(13. / 16.))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(ui::champagne())
                .child(title),
        )
        .children(note.map(|note| {
            div()
                .flex_1()
                .min_w_0()
                .truncate()
                .text_size(rems(12. / 16.))
                .text_color(ui::muted())
                .child(note)
        }))
}

/// The agent is still sending the surface.
pub(super) fn drawing_card(title: &str) -> AnyElement {
    row(
        IconName::Workflow,
        ui::info(),
        title.to_string(),
        Some("Drawing…".into()),
    )
    .child(ui::spinner(Size::XSmall, ui::info()))
    .into_any_element()
}

/// The tool call failed: the extension's message (what the agent saw).
pub(super) fn failed_card(title: &str, error: &str) -> AnyElement {
    ui::card()
        .w_full()
        .border_color(ui::danger().opacity(0.35))
        .child(
            row(IconName::Workflow, ui::danger(), title.to_string(), None)
                .border_0()
                .bg(transparent_black()),
        )
        .child(
            div()
                .px_3()
                .pb_2()
                .font_family(ui::MONO)
                .text_size(rems(12. / 16.))
                .text_color(ui::muted())
                .whitespace_normal()
                .child(error.to_string()),
        )
        .into_any_element()
}

/// An older version of a surface shown again further down.
pub(super) fn superseded_row(title: &str, version: u64, closed: bool) -> AnyElement {
    row(
        IconName::Workflow,
        ui::faint(),
        title.to_string(),
        Some(if closed {
            "closed by the agent".into()
        } else {
            format!("v{version} · updated below")
        }),
    )
    .opacity(0.7)
    .into_any_element()
}

/// The surface is open in place of the conversation.
pub(super) fn expanded_row(surface: super::Surface, title: &str) -> AnyElement {
    row(
        IconName::Workflow,
        ui::info(),
        title.to_string(),
        Some("open in the expanded view".into()),
    )
    .child(
        Button::new("genui-collapse")
            .ghost()
            .xsmall()
            .icon(Icon::new(IconName::Minimize2))
            .label("Back here")
            .on_click(move |_, _, cx| surface.emit(DiagramEvent::Collapse, cx)),
    )
    .into_any_element()
}

/// The surface is pinned to the conversation's side panel.
pub(super) fn pinned_row(surface: super::Surface, title: &str) -> AnyElement {
    row(
        IconName::Pin,
        ui::info(),
        title.to_string(),
        Some("pinned to the side panel".into()),
    )
    .child(
        Button::new("genui-unpin-here")
            .debug_selector(|| "genui-unpin-here".into())
            .ghost()
            .xsmall()
            .icon(Icon::new(IconName::PinOff))
            .label("Back here")
            .on_click(move |_, _, cx| surface.emit(DiagramEvent::Pin(false), cx)),
    )
    .into_any_element()
}

/// The user's sent edits (the extension's `gentle-ui.event` message), a
/// template they inserted (once its surface is gone) or their request to
/// add a surface to the docs.
pub(super) fn event_card(details: &serde_json::Value) -> AnyElement {
    let title = details["title"]
        .as_str()
        .filter(|t| !t.is_empty())
        .unwrap_or("Diagram");
    if details["request"] == "docs" {
        return row(
            IconName::BookOpen,
            ui::accent(),
            format!("You asked to add {title} to the docs"),
            details["format"].as_str().map(|f| format!("as {f}")),
        )
        .into_any_element();
    }
    let summary: Vec<&str> = details["summary"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|s| s.as_str())
        .collect();
    const SHOWN: usize = 6;
    let more = summary.len().saturating_sub(SHOWN);
    let note = details["note"].as_str().filter(|n| !n.trim().is_empty());
    ui::card()
        .w_full()
        .child(
            row(
                IconName::Pencil,
                ui::accent(),
                match details["kind"].as_str() {
                    _ if details["inserted"] == true => {
                        format!("You inserted {title} from a template")
                    }
                    Some("html") => format!("You, from {title}"),
                    Some("review") => format!("You reviewed {title}"),
                    _ => format!("You edited {title}"),
                },
                None,
            )
            .border_0()
            .bg(transparent_black())
            .child(div().flex_1())
            .when_some(details["version"].as_u64(), |d, v| {
                d.child(ui::chip(format!("v{v}"), ui::info()))
            }),
        )
        .child(
            v_flex()
                .px_3()
                .pb_2()
                .gap_0p5()
                .text_size(rems(12. / 16.))
                .text_color(ui::muted())
                .children(summary.iter().take(SHOWN).map(|line| {
                    h_flex()
                        .gap_1p5()
                        .child(div().text_color(ui::faint()).child("•"))
                        .child(div().min_w_0().whitespace_normal().child(line.to_string()))
                }))
                .when(more > 0, |d| {
                    d.child(
                        div()
                            .text_color(ui::faint())
                            .child(format!("… {more} more")),
                    )
                })
                .when_some(note, |d, note| {
                    d.child(
                        div()
                            .pt_1()
                            .text_color(ui::fg())
                            .whitespace_normal()
                            .child(format!("“{note}”")),
                    )
                }),
        )
        .into_any_element()
}
