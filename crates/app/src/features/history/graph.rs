//! Paints one row of the commit graph: the lanes passing by, the forks and
//! joins as curves, and the node. Paint-only, no input.

use gentle_core::git::log::{Edge, GraphRow};
use gpui_kit::{
    BorderStyle, Bounds, Hsla, IntoElement, PathBuilder, Pixels, Point, Styled as _, Window,
    canvas, point, px, quad, size, transparent_black,
};

use crate::shared::ui;

/// Space before the first lane: lane 0's center.
pub(super) const PAD: f32 = 12.;
const STROKE: f32 = 1.5;
const NODE: f32 = 7.;

/// One geometry for every visible row, including rows with fewer lanes:
/// parent edges must meet at the same x coordinate between rows.
#[derive(Clone, Copy)]
pub(super) struct LaneLayout {
    pub width: f32,
    step: f32,
}

impl LaneLayout {
    pub fn new(lanes: usize, pane_width: f32) -> Self {
        // A sidebar has a small text budget even at its maximum width.
        // Reserve most of it for the subject, author and time.
        let pane_width = if pane_width > 0. { pane_width } else { 480. };
        let preferred = if pane_width < 640. { 9. } else { 14. };
        let intervals = lanes.saturating_sub(1) as f32;
        let budget = (pane_width * 0.30).max(PAD * 2.);
        // Maintain usable spacing. The list scrolls horizontally if all
        // lanes plus the message column cannot fit in the viewport.
        let step = if intervals > 0. {
            ((budget - PAD * 2.) / intervals).clamp(8., preferred)
        } else {
            preferred
        };
        let width = PAD * 2. + step * intervals;
        Self { width, step }
    }

    fn x(self, lane: usize) -> f32 {
        PAD + self.step * lane as f32
    }

    fn node_scale(self) -> f32 {
        0.85
    }
}

/// Keep the main line, visible commit lanes and the endpoints of visible
/// forks/joins. Straight-through lanes without a node or junction in this
/// viewport do not need a column. Sorting preserves lane order.
pub(super) fn visible_lanes<'a>(rows: impl Iterator<Item = &'a GraphRow>) -> Vec<usize> {
    let mut lanes = vec![0];
    for row in rows {
        lanes.push(row.lane);
        for edge in row.top.iter().chain(&row.bottom) {
            if edge.from != edge.to {
                lanes.extend([edge.from, edge.to]);
            }
        }
    }
    lanes.sort_unstable();
    lanes.dedup();
    lanes
}

/// A lane's color: the tip's first-parent line in `info`, the others cycling
/// through the semantic tones.
pub(super) fn lane_color(color: usize) -> Hsla {
    match color % 6 {
        0 => ui::info(),
        1 => ui::warning(),
        2 => ui::danger(),
        3 => ui::success(),
        4 => ui::champagne(),
        _ => ui::petal(),
    }
}

/// What sits on a row's lane.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Node {
    Commit,
    /// HEAD: a ring.
    Head,
    /// Uncommitted changes: a dashed ring.
    Uncommitted,
    /// Nothing: the count bubble is drawn as an element over it.
    None,
}

/// One row's graph. `dashed_top` draws lane 0's top half dashed (the line
/// from the uncommitted row into HEAD); `dashed_bottom` does the same below
/// the uncommitted node.
pub(super) struct RowGraph {
    pub top: Vec<Edge>,
    pub bottom: Vec<Edge>,
    pub lane: usize,
    pub color: usize,
    pub node: Node,
    pub dashed_top: bool,
    pub dashed_bottom: bool,
}

impl RowGraph {
    pub fn commit(row: &GraphRow, head: bool, dashed_top: bool) -> Self {
        Self {
            top: row.top.clone(),
            bottom: row.bottom.clone(),
            lane: row.lane,
            color: row.color,
            node: if head { Node::Head } else { Node::Commit },
            dashed_top,
            dashed_bottom: false,
        }
    }

    /// Only the node, for a row the filter kept: its neighbors in the list
    /// aren't its neighbors in the graph.
    pub fn node_only(row: &GraphRow, head: bool) -> Self {
        Self {
            top: Vec::new(),
            bottom: Vec::new(),
            lane: 0,
            ..Self::commit(row, head, false)
        }
    }

    pub fn uncommitted() -> Self {
        Self {
            top: Vec::new(),
            bottom: Vec::new(),
            lane: 0,
            color: 0,
            node: Node::Uncommitted,
            dashed_top: false,
            dashed_bottom: true,
        }
    }

    /// Lane 0 coming down into the "Earlier history" bubble.
    pub fn earlier() -> Self {
        Self {
            top: vec![Edge {
                from: 0,
                to: 0,
                color: 0,
            }],
            bottom: Vec::new(),
            lane: 0,
            color: 0,
            node: Node::None,
            dashed_top: false,
            dashed_bottom: false,
        }
    }

    /// Project the same original lanes into the same compact columns in
    /// every row of the viewport. Only irrelevant passing lines disappear.
    pub fn compact(mut self, lanes: &[usize]) -> Self {
        let remap = |lane| lanes.binary_search(&lane).ok();
        self.lane = remap(self.lane).unwrap_or(0);
        for edges in [&mut self.top, &mut self.bottom] {
            edges.retain_mut(|edge| {
                let (Some(from), Some(to)) = (remap(edge.from), remap(edge.to)) else {
                    return false;
                };
                edge.from = from;
                edge.to = to;
                true
            });
        }
        self
    }

    pub fn element(self, layout: LaneLayout) -> impl IntoElement {
        canvas(
            |_, _, _| (),
            move |bounds, _, window, _| self.paint(bounds, layout, window),
        )
        .size_full()
    }

    fn paint(&self, bounds: Bounds<Pixels>, layout: LaneLayout, window: &mut Window) {
        let x = |lane: usize| bounds.origin.x + px(layout.x(lane));
        let scale = layout.node_scale();
        let stroke = (STROKE * scale).max(0.5);
        let top = bounds.origin.y;
        let mid = bounds.origin.y + bounds.size.height / 2.;
        let bottom = bounds.origin.y + bounds.size.height;
        // Lines stop short of the node, so a ring stays hollow on any row
        // background (hover, selection) without masking.
        let gap = px(if self.node == Node::None {
            0.
        } else {
            NODE * scale
        });
        let into = |lane: usize| if lane == self.lane { mid - gap } else { mid };
        let out_of = |lane: usize| if lane == self.lane { mid + gap } else { mid };

        for edge in &self.top {
            let dashed = self.dashed_top && edge.from == 0 && edge.to == 0;
            let end = point(x(edge.to), into(edge.to));
            line(window, point(x(edge.from), top), end, edge.color, dashed, stroke);
        }
        if self.dashed_top && !self.top.iter().any(|e| e.to == 0) {
            line(window, point(x(0), top), point(x(0), into(0)), 0, true, stroke);
        }
        for edge in &self.bottom {
            let start = point(x(edge.from), out_of(edge.from));
            line(window, start, point(x(edge.to), bottom), edge.color, false, stroke);
        }
        if self.dashed_bottom {
            line(window, point(x(0), out_of(0)), point(x(0), bottom), 0, true, stroke);
        }

        let color = lane_color(self.color);
        let center = point(x(self.lane), mid);
        match self.node {
            Node::Commit => {
                circle(window, center, (NODE - 1.5) * scale, color.opacity(0.22), None);
                circle(window, center, 3.5 * scale, color, None);
            }
            Node::Head => circle(
                window,
                center,
                (NODE - 1.) * scale,
                transparent_black(),
                Some((color, BorderStyle::Solid)),
            ),
            Node::Uncommitted => circle(
                window,
                center,
                (NODE - 1.) * scale,
                transparent_black(),
                Some((color.opacity(0.8), BorderStyle::Dashed)),
            ),
            Node::None => {}
        }
    }
}

/// A straight line between equal lanes, else an S-curve that leaves and
/// arrives vertically.
fn line(
    window: &mut Window,
    from: Point<Pixels>,
    to: Point<Pixels>,
    color: usize,
    dashed: bool,
    stroke: f32,
) {
    let mut path = PathBuilder::stroke(px(stroke));
    if dashed {
        path = path.dash_array(&[px(3.), px(3.)]);
    }
    path.move_to(from);
    if from.x == to.x {
        path.line_to(to);
    } else {
        let ym = (from.y + to.y) / 2.;
        path.cubic_bezier_to(to, point(from.x, ym), point(to.x, ym));
    }
    if let Ok(path) = path.build() {
        let color = lane_color(color);
        window.paint_path(path, if dashed { color.opacity(0.8) } else { color });
    }
}

fn circle(
    window: &mut Window,
    center: Point<Pixels>,
    radius: f32,
    fill: Hsla,
    border: Option<(Hsla, BorderStyle)>,
) {
    let r = px(radius);
    let bounds = Bounds::new(point(center.x - r, center.y - r), size(r * 2., r * 2.));
    let (width, color, style) = match border {
        Some((color, style)) => (px((radius / 3.).min(2.)), color, style),
        None => (px(0.), transparent_black(), BorderStyle::Solid),
    };
    window.paint_quad(quad(bounds, r, fill, width, color, style));
}

#[cfg(test)]
mod tests {
    use super::{LaneLayout, PAD, RowGraph, visible_lanes};
    use gentle_core::git::log::{Edge, GraphRow};

    #[test]
    fn viewport_uses_only_commit_and_junction_lanes() {
        let edge = |from, to| Edge { from, to, color: 1 };
        let row = GraphRow {
            lane: 8,
            color: 1,
            top: vec![edge(3, 3), edge(8, 8)],
            bottom: vec![edge(8, 12), edge(3, 3)],
            mainline: false,
            branch: None,
            lane_tip: false,
        };
        let lanes = visible_lanes(std::iter::once(&row));
        assert_eq!(lanes, vec![0, 8, 12]);
        let compact = RowGraph::commit(&row, false, false).compact(&lanes);
        assert_eq!(compact.lane, 1);
        assert_eq!(compact.top.len(), 1);
        assert_eq!((compact.bottom[0].from, compact.bottom[0].to), (1, 2));
        assert_eq!(visible_lanes(std::iter::empty()), vec![0]);
    }

    #[test]
    fn dense_graph_preserves_every_lane_and_readable_nodes() {
        for width in [200., 320., 480., 800.] {
            let layout = LaneLayout::new(24, width);
            assert!(layout.x(23) <= layout.width - PAD + 0.01);
            assert!(layout.x(1) - layout.x(0) >= 8.);
            assert!(layout.node_scale() >= 0.8);
        }
        assert!(LaneLayout::new(8, 480.).width <= 480. * 0.30);
    }

    #[test]
    fn graph_keeps_its_lanes_aligned_and_grows_with_the_pane() {
        let narrow = LaneLayout::new(20, 320.);
        let wide = LaneLayout::new(20, 1000.);
        assert!(narrow.step <= wide.step);
        assert!(narrow.width < wide.width);
        assert_eq!(LaneLayout::new(1, 320.).width, PAD * 2.);
    }
}
