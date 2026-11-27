//! Where a virtualized conversation is scrolled to, before every row has
//! been laid out. The list only knows the heights of rows it has drawn; the
//! rest are estimated from their content (lines of text, cards) scaled by
//! what the drawn rows turned out to measure, so the scroll thumb and the
//! message marks stay put on the first pass instead of jumping each time a
//! new row is measured.

use std::collections::HashMap;

use crate::transcript::{Block, Item};

/// Pixels per weight unit before any row has been measured: one line of
/// body text with its share of paragraph spacing, as the app draws it.
const DEFAULT_SCALE: f32 = 17.;
/// Characters per wrapped line of body text in the 760 px column.
const LINE_CHARS: usize = 90;
/// Lines a folded card shows besides its header.
const CARD_LINES: usize = 6;
/// A message you sent with more lines than `BUBBLE_FOLD` shows folded to its
/// first `BUBBLE_LINES` and a toggle (the session view's `USER_LINES` and
/// `USER_SLACK`).
const BUBBLE_FOLD: usize = 10;
const BUBBLE_LINES: usize = 8;
/// A sent message's attachment strip: one row of 120 px tiles, images and
/// files alike, at [`DEFAULT_SCALE`].
const STRIP: f32 = 7.;

/// The wrapped lines `text` takes, roughly.
fn text_lines(text: &str) -> f32 {
    wrapped_lines(text.lines())
}

fn wrapped_lines<'a>(lines: impl Iterator<Item = &'a str>) -> f32 {
    lines
        .map(|line| 1 + line.chars().count() / LINE_CHARS)
        .sum::<usize>()
        .max(1) as f32
}

/// The lines a message's own text weighs, folded past [`BUBBLE_FOLD`] as the
/// bubble draws it: blank text is the one line [`text_lines`] counts, and
/// trailing blank lines count as they are.
fn user_text(text: &str) -> f32 {
    if text.trim_end().lines().nth(BUBBLE_FOLD).is_some() {
        wrapped_lines(text.lines().take(BUBBLE_LINES)) + 1.
    } else {
        text_lines(text)
    }
}

/// What a confirmed skill invocation weighs for its collapsed skill row.
pub const SKILL_ROW: f32 = 1.5;

/// A row's size in "lines", from its content alone. Only its proportion to
/// other rows matters: [`estimate`] scales it by what measured rows show.
pub fn item_weight(item: &Item) -> f32 {
    item_weight_confirmed(item, None)
}

/// [`item_weight`] for a row the app confirmed as the live invocation of
/// `skill` (the command it sent): its body is collapsed, so only the skill
/// row and the typed args weigh. Without a matching confirmation the text is
/// ordinary.
pub fn item_weight_confirmed(item: &Item, skill: Option<&str>) -> f32 {
    match item {
        // The bubble's padding and the row gap, then its text (and the
        // strip of image and file tiles above it). The paths' block the
        // prompt carries weighs nothing: the bubble shows cards, not the
        // block. A message with no block weighs what it always did, blank
        // text included; one that is only a block has no text to weigh.
        Item::User { text, images } => {
            let visible = crate::attachments::mixed_attachment_text(text, images.len());
            let block = skill.and_then(|c| crate::attachments::parse_skill_block(visible, c));
            let visible = block
                .as_ref()
                .map_or(visible, |b| b.args.unwrap_or_default());
            let (body, files) = crate::attachments::split_files(visible);
            let text = if block.is_some() {
                // Only the skill row and the typed args show by default.
                SKILL_ROW
                    + if body.trim().is_empty() {
                        0.
                    } else {
                        user_text(body)
                    }
            } else if files.is_empty() {
                user_text(text)
            } else if body.trim().is_empty() {
                0.
            } else {
                user_text(body)
            };
            3.8 + text
                + if images.is_empty() && files.is_empty() {
                    0.
                } else {
                    STRIP
                }
        }
        Item::Assistant { .. } if !crate::transcript::assistant_visible(item) => 0.,
        Item::Assistant {
            blocks,
            streaming,
            error,
        } => {
            let text: f32 = blocks
                .iter()
                .enumerate()
                .map(|(ix, block)| match block {
                    // Paragraph gaps add about half a line each.
                    Block::Text(text) => text_lines(text) * 1.25,
                    // The one being streamed reads along, open under its
                    // header; the others fold to the header.
                    Block::Thinking(text) if *streaming && ix + 1 == blocks.len() => {
                        1.5 + text_lines(text.trim())
                    }
                    Block::Thinking(_) => 1.5,
                    Block::ToolCall => 0.,
                })
                .sum();
            2. + text + if error.is_some() { 3. } else { 0. }
        }
        // A card: header, status footer and a few lines of its body.
        Item::Tool { .. } | Item::Shell { .. } => 13.,
        Item::Notice { text, .. } => 3. + text_lines(text),
        Item::Custom { text, .. } => 5. + text_lines(text).min(CARD_LINES as f32),
        Item::CommandOutput { notices, .. } => {
            5. + notices
                .iter()
                .map(|(_, text)| text_lines(text))
                .sum::<f32>()
                .min(CARD_LINES as f32 * 2.)
        }
    }
}

/// The scroll position in estimated pixels. Kept between frames: the row
/// tops are re-laid ([`Estimate::measure`]) only when weights or measured
/// heights change, the offset ([`Estimate::scroll_to`]) on every scroll.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Estimate {
    /// The top of every row.
    pub tops: Vec<f32>,
    pub content: f32,
    /// From the top of the first row to the top of the pane, at most
    /// `content - viewport`.
    pub offset: f32,
}

impl Estimate {
    /// Lay out row tops and the total height. `weights` has one entry per
    /// row and `known` the measured heights by row; unmeasured rows count as
    /// their weight times the measured rows' pixels per weight. Reuses the
    /// tops' allocation.
    pub fn measure(&mut self, weights: &[f32], known: &HashMap<usize, f32>) {
        let (measured, measured_weight) = known
            .iter()
            .filter_map(|(row, height)| Some((*height, *weights.get(*row)?)))
            .fold((0., 0.), |(h, w), (height, weight)| {
                (h + height, w + weight)
            });
        let scale = if measured_weight > 0. {
            measured / measured_weight
        } else {
            DEFAULT_SCALE
        };
        self.tops.clear();
        let mut y = 0.;
        for (row, weight) in weights.iter().enumerate() {
            self.tops.push(y);
            y += known.get(&row).copied().unwrap_or(weight * scale);
        }
        self.content = y;
    }

    /// Place the pane's top `offset_in_row` px into `top_row`
    /// (`top_row == tops.len()` is the end).
    pub fn scroll_to(&mut self, top_row: usize, offset_in_row: f32, viewport: f32) {
        let content = self.content;
        self.offset = (self.tops.get(top_row).copied().unwrap_or(content) + offset_in_row)
            .min((content - viewport).max(0.));
    }

    /// The row at `y` px from the top and how far into it `y` is.
    pub fn row_at(&self, y: f32) -> (usize, f32) {
        let row = self.tops.partition_point(|top| *top <= y).saturating_sub(1);
        (row, y - self.tops.get(row).copied().unwrap_or(0.))
    }
}

/// Rail marks closer than this many pixels draw as one.
pub const MARK_GAP: f32 = 2.;

/// A mark on the scroll rail: where it sits and the marks it stands for
/// (`first..=last`, by their position among all marks).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RailMark {
    pub y: f32,
    pub first: usize,
    pub last: usize,
}

/// Lay out marks at `ys` (in order, never decreasing) into `out`: a mark
/// closer than [`MARK_GAP`] to the one before joins it, so a long
/// conversation draws at most one mark per couple of pixels.
pub fn rail_marks(ys: impl IntoIterator<Item = f32>, out: &mut Vec<RailMark>) {
    out.clear();
    for (ix, y) in ys.into_iter().enumerate() {
        match out.last_mut().filter(|mark| y - mark.y < MARK_GAP) {
            Some(mark) => mark.last = ix,
            None => out.push(RailMark {
                y,
                first: ix,
                last: ix,
            }),
        }
    }
}

/// The mark in `marks` ([`rail_marks`]) nearest to `y`, if within `reach`.
pub fn nearest_mark(marks: &[RailMark], y: f32, reach: f32) -> Option<usize> {
    let after = marks.partition_point(|mark| mark.y < y);
    let distance = |ix: usize| (marks[ix].y - y).abs();
    let nearest = match (after.checked_sub(1), (after < marks.len()).then_some(after)) {
        (Some(before), Some(after)) if distance(after) <= distance(before) => after,
        (Some(before), _) => before,
        (None, Some(after)) => after,
        (None, None) => return None,
    };
    (distance(nearest) <= reach).then_some(nearest)
}

/// The thumb's top for this frame. A row measured taller or shorter than
/// estimated changes the total, which would nudge the thumb against the
/// scroll or throw it ahead; this keeps it moving only the way the pane
/// moved, at most twice its normal speed while it catches up with the
/// estimate (and still while the pane is still), and exact at both ends.
/// `last` is the previous frame's `(offset, thumb top)`; `scale` is thumb
/// pixels per content pixel.
pub fn steady_thumb(
    last: Option<(f32, f32)>,
    offset: f32,
    top: f32,
    scale: f32,
    max_offset: f32,
    max_top: f32,
) -> f32 {
    if offset <= 0. {
        return 0.;
    }
    if offset >= max_offset {
        return max_top;
    }
    let Some((last_offset, last_top)) = last else {
        return top.clamp(0., max_top);
    };
    let reach = (offset - last_offset).abs() * scale * 2. + 0.5;
    let next = if offset > last_offset {
        top.clamp(last_top, last_top + reach)
    } else if offset < last_offset {
        top.clamp(last_top - reach, last_top)
    } else {
        last_top
    };
    next.clamp(0., max_top)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn estimate(
        weights: &[f32],
        known: &HashMap<usize, f32>,
        top_row: usize,
        offset_in_row: f32,
        viewport: f32,
    ) -> Estimate {
        let mut e = Estimate::default();
        e.measure(weights, known);
        e.scroll_to(top_row, offset_in_row, viewport);
        e
    }

    /// A conversation's rows: short questions and answers of very different
    /// lengths, tool cards between them; real heights deviate from the
    /// weights by up to ±30 %.
    fn conversation() -> (Vec<f32>, Vec<f32>) {
        let mut weights = Vec::new();
        let mut heights = Vec::new();
        let mut noise = 7u32;
        let mut push = |weight: f32, weights: &mut Vec<f32>, heights: &mut Vec<f32>| {
            noise = noise.wrapping_mul(1_103_515_245).wrapping_add(12_345);
            let factor = 0.7 + (noise >> 16) as f32 % 61. / 100.;
            weights.push(weight);
            heights.push(weight * 22. * factor);
        };
        for turn in 0..30 {
            push(3., &mut weights, &mut heights);
            for _ in 0..turn % 4 {
                push(5.6, &mut weights, &mut heights);
            }
            let answer = if turn % 3 == 0 { 60. } else { 8. };
            push(answer, &mut weights, &mut heights);
        }
        (weights, heights)
    }

    /// Scroll from the top to the end once, the way the list measures rows
    /// (the pane plus 600 px of overdraw), and record the thumb at each step:
    /// `(top, size)` on a rail as tall as the pane.
    fn first_pass(weights: &[f32], heights: &[f32], viewport: f32) -> Vec<(f32, f32)> {
        let total: f32 = heights.iter().sum();
        let mut known = HashMap::new();
        let mut thumbs = Vec::new();
        let mut last = None;
        let mut offset = 0.;
        while offset <= total - viewport {
            let mut top = 0.;
            let mut top_row = 0;
            let mut within = 0.;
            for (row, height) in heights.iter().enumerate() {
                if top + height > offset - 600. && top < offset + viewport + 600. {
                    known.insert(row, *height);
                }
                if top <= offset {
                    top_row = row;
                    within = offset - top;
                }
                top += height;
            }
            let e = estimate(weights, &known, top_row, within, viewport);
            let scale = viewport / e.content;
            let size = viewport * scale;
            let thumb = steady_thumb(
                last,
                e.offset,
                e.offset * scale,
                scale,
                e.content - viewport,
                viewport - size,
            );
            last = Some((e.offset, thumb));
            thumbs.push((thumb, size));
            offset += 40.;
        }
        thumbs
    }

    #[test]
    fn the_thumb_moves_steadily_on_the_first_pass() {
        let (weights, heights) = conversation();
        let viewport = 700.;
        let thumbs = first_pass(&weights, &heights, viewport);
        let total: f32 = heights.iter().sum();
        let true_size = viewport * viewport / total;
        for pair in thumbs.windows(2) {
            let ((top_a, size_a), (top_b, size_b)) = (pair[0], pair[1]);
            // Scrolling down never pulls the thumb up, nor throws it ahead.
            assert!(top_b >= top_a - 0.5, "thumb went up: {top_a} → {top_b}");
            assert!(top_b - top_a < 3.5, "thumb jumped: {top_a} → {top_b}");
            // Its size settles gradually instead of snapping.
            assert!(
                (size_b - size_a).abs() < 3.,
                "size jumped: {size_a} → {size_b}"
            );
        }
        // And it is about the right size from the very first frame.
        let first = thumbs[0].1;
        assert!(
            (first - true_size).abs() / true_size < 0.35,
            "first size {first}, true {true_size}"
        );
    }

    #[test]
    fn the_end_and_unmeasured_lists_stay_in_range() {
        let weights = vec![2., 10., 4.];
        let e = estimate(&weights, &HashMap::new(), 3, 0., 100.);
        assert_eq!(e.content, 16. * DEFAULT_SCALE);
        // Scrolled to the end: the pane's top is a pane above the end.
        assert_eq!(e.offset, e.content - 100.);
        assert_eq!(e.tops, vec![0., 2. * DEFAULT_SCALE, 12. * DEFAULT_SCALE]);
        // Measured rows set the scale for the others.
        let known = HashMap::from([(0, 40.)]);
        let e = estimate(&weights, &known, 0, 0., 100.);
        assert_eq!(e.tops, vec![0., 40., 240.]);
    }

    /// S1: a confirmed live invocation weighs its collapsed row and args,
    /// whatever the hidden body; the same wrapper unconfirmed is ordinary text.
    #[test]
    fn confirmed_skill_weight_ignores_body_but_unconfirmed_does_not() {
        let wrap = |body: &str| Item::User {
            text: format!("<skill name=\"docs\" location=\"/x\">\n{body}\n</skill>\n\ntest"),
            images: vec![],
        };
        let (small, big) = (wrap("short"), wrap(&"long body\n".repeat(80)));
        let weight = |item: &Item, cmd| item_weight_confirmed(item, cmd);
        assert_eq!(
            weight(&small, Some("/skill:docs test")),
            3.8 + SKILL_ROW + 1.
        );
        assert_eq!(weight(&big, Some("/skill:docs test")), 3.8 + SKILL_ROW + 1.);
        assert_eq!(weight(&big, Some("/skill:docs other")), item_weight(&big));
        assert_eq!(weight(&big, None), item_weight(&big));
        assert!(item_weight(&big) > item_weight(&small));
    }

    #[test]
    fn unconfirmed_skill_weight_includes_loaded_body() {
        for separator in ["\n", "\n\n"] {
            for body in ["short".to_string(), "long body\n".repeat(80)] {
                let item = Item::User {
                    text: format!(
                        "<skill name=\"impeccable\" location=\"/x\">\n{body}\n</skill>{separator}test"
                    ),
                    images: vec![],
                };
                let Item::User { text, .. } = &item else {
                    unreachable!()
                };
                assert_eq!(item_weight(&item), 3.8 + user_text(text));
            }
        }
    }

    #[test]
    fn weights_follow_content() {
        let user = |text: &str| Item::User {
            text: text.into(),
            images: Vec::new(),
        };
        let answer = |text: &str| Item::Assistant {
            blocks: vec![Block::Text(text.into())],
            streaming: false,
            error: None,
        };
        assert!(item_weight(&user("hola")) < item_weight(&user(&"x".repeat(400))));
        let long = (0..40).map(|_| "una línea").collect::<Vec<_>>().join("\n");
        assert!(item_weight(&answer(&long)) > 10. * item_weight(&answer("ok")));
    }

    #[test]
    fn long_messages_weigh_what_they_show_folded() {
        let user = |lines: usize| Item::User {
            text: (0..lines).map(|i| format!("line {i}\n")).collect(),
            images: Vec::new(),
        };
        // Past BUBBLE_FOLD lines a message folds: its length stops counting.
        assert_eq!(item_weight(&user(BUBBLE_FOLD + 1)), item_weight(&user(500)));
        assert!(item_weight(&user(500)) < item_weight(&user(BUBBLE_FOLD)));
        assert!(item_weight(&user(BUBBLE_FOLD - 1)) < item_weight(&user(BUBBLE_FOLD)));
    }

    /// S9, S13: a prompt's files weigh their tile row, and the `Attached
    /// files:` block the prompt carries weighs nothing — the bubble shows
    /// tiles, not the paths — even with thirty of them. Images and file
    /// tiles share one row, so having both weighs one row.
    #[test]
    fn a_prompt_with_files_weighs_its_tiles_not_the_block() {
        let user = |text: &str| Item::User {
            text: text.into(),
            images: Vec::new(),
        };
        let block: String = (0..30).map(|i| format!("- /work/file-{i}.pdf\n")).collect();
        let plain = user("look");
        let with_files = user(&format!("look\n\nAttached files:\n{block}"));
        assert_eq!(item_weight(&with_files), item_weight(&plain) + STRIP);
        let with_image = |text: &str| match user(text) {
            Item::User { text, .. } => Item::User {
                text,
                images: vec![crate::transcript::UserImage::new(
                    "image/png",
                    "aGk=".into(),
                )],
            },
            _ => unreachable!(),
        };
        assert_eq!(
            item_weight(&with_image(&format!("look\n\nAttached files:\n{block}"))),
            item_weight(&with_image("look")),
            "images and file tiles share one row"
        );
        // A prompt of files only is its tiles: no line of text under them,
        // though a message with no text at all still weighs its blank line.
        let only = user(&format!("Attached files:\n{block}"));
        assert_eq!(item_weight(&only), 3.8 + STRIP);
        assert_eq!(item_weight(&user("")), item_weight(&plain));
        // The block's lines never count: thirty paths weigh one card, not
        // thirty lines of text.
        assert!(item_weight(&only) < item_weight(&user(&block)));
    }

    /// T9: a message that carries no `Attached files:` block weighs what it
    /// always did: blank text is the one line `text_lines` counts, and
    /// trailing blank lines count as they are; images keep their strip.
    #[test]
    fn a_message_without_a_block_weighs_what_it_always_did() {
        let user = |text: &str| Item::User {
            text: text.into(),
            images: Vec::new(),
        };
        // Weights are estimates in lines, good to a tenth.
        let weigh = |weight: f32, expected: f32| {
            assert!(
                (weight - expected).abs() < 0.05,
                "{weight} lines, want {expected}"
            );
        };
        // The bubble's 3.8 lines plus the one line a blank message counts.
        weigh(item_weight(&user("")), 4.8);
        weigh(item_weight(&user("   ")), 4.8);
        // Three lines: the text and the two blank lines its breaks draw.
        weigh(item_weight(&user("trailing\n\n\n")), 6.8);
        // An image-only message adds its strip above the same blank line.
        let image_only = Item::User {
            text: String::new(),
            images: vec![crate::transcript::UserImage::new(
                "image/png",
                "aGk=".into(),
            )],
        };
        weigh(item_weight(&image_only), 11.8);
    }

    #[test]
    fn live_thinking_counts_its_text() {
        let thinking = |text: &str, streaming| Item::Assistant {
            blocks: vec![Block::Thinking(text.into())],
            streaming,
            error: None,
        };
        let long = (0..40).map(|_| "un paso").collect::<Vec<_>>().join("\n");
        // Streamed, it shows open: it weighs at least the 40 lines it draws.
        assert!(item_weight(&thinking(&long, true)) > 40.);
        // Done, it folds to its header whatever its length.
        assert_eq!(
            item_weight(&thinking(&long, false)),
            item_weight(&thinking("ok", false))
        );
        // Followed by the answer, it isn't the one being streamed anymore.
        let answered = Item::Assistant {
            blocks: vec![Block::Thinking(long), Block::Text("ok".into())],
            streaming: true,
            error: None,
        };
        assert!(item_weight(&answered) < 10.);
    }

    #[test]
    fn a_textless_assistant_message_weighs_nothing() {
        // A finished message that only announced tool calls renders no row;
        // its estimate is zero.
        let tool_calls = Item::Assistant {
            blocks: vec![Block::ToolCall, Block::ToolCall],
            streaming: false,
            error: None,
        };
        assert_eq!(item_weight(&tool_calls), 0.);
        // With text (or live) the message keeps its usual weight.
        let text = Item::Assistant {
            blocks: vec![Block::Text("hi".into())],
            streaming: false,
            error: None,
        };
        assert!(item_weight(&text) > 0.);
        let live = Item::Assistant {
            blocks: vec![],
            streaming: true,
            error: None,
        };
        assert!(item_weight(&live) > 0.);
    }

    #[test]
    fn close_marks_draw_as_one() {
        let mut marks = Vec::new();
        rail_marks([0., 1., 1.9, 5., 6.5, 30.], &mut marks);
        let mark = |y, first, last| RailMark { y, first, last };
        // A run joins its first mark, so close marks can't chain down the rail.
        assert_eq!(marks, vec![mark(0., 0, 2), mark(5., 3, 4), mark(30., 5, 5)]);
        rail_marks([], &mut marks);
        assert!(marks.is_empty());
    }

    #[test]
    fn the_nearest_mark_is_found_within_reach() {
        let mut marks = Vec::new();
        rail_marks([10., 20., 40.], &mut marks);
        assert_eq!(nearest_mark(&marks, 12., 4.5), Some(0));
        assert_eq!(nearest_mark(&marks, 18., 4.5), Some(1));
        // Halfway: the later one, drawn on top.
        assert_eq!(nearest_mark(&marks, 15., 5.), Some(1));
        assert_eq!(nearest_mark(&marks, 30., 4.5), None);
        assert_eq!(nearest_mark(&marks, 43., 4.5), Some(2));
        assert_eq!(nearest_mark(&marks, 5.5, 4.5), Some(0));
        assert_eq!(nearest_mark(&[], 5., 4.5), None);
    }

    #[test]
    fn rows_are_found_by_offset() {
        let e = estimate(&[2., 10., 4.], &HashMap::from([(0, 40.)]), 0, 0., 100.);
        assert_eq!(e.row_at(0.), (0, 0.));
        assert_eq!(e.row_at(50.), (1, 10.));
        assert_eq!(e.row_at(400.), (2, 160.));
    }
}
