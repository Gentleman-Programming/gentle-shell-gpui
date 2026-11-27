//! Conservative source-range planning for rendering top-level code separately.
//!
//! Ranges always refer to the exact input passed to `segment_source`, not a
//! transformed or displayed version of it. Replan whenever that input changes.

use std::ops::Range;

use markdown::mdast::Node;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SegmentKind {
    Markdown,
    Code,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MarkdownSegment {
    pub kind: SegmentKind,
    pub range: Range<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MarkdownPlan {
    /// Render the entire input in one native Markdown view.
    Native,
    /// Contiguous ranges that reconstruct the input exactly.
    Segmented(Vec<MarkdownSegment>),
}

fn has_cross_document_construct(node: &Node) -> bool {
    matches!(
        node,
        Node::Definition(_)
            | Node::LinkReference(_)
            | Node::ImageReference(_)
            | Node::FootnoteDefinition(_)
            | Node::FootnoteReference(_)
            | Node::Html(_)
    ) || node
        .children()
        .is_some_and(|children| children.iter().any(has_cross_document_construct))
}

/// Plan rendering from the input's own byte offsets. This deliberately does not
/// claim semantic equivalence for every Markdown construct.
pub fn segment_source(source: &str) -> MarkdownPlan {
    let Ok(root) = markdown::to_mdast(source, &markdown::ParseOptions::gfm()) else {
        return MarkdownPlan::Native;
    };
    if has_cross_document_construct(&root) {
        return MarkdownPlan::Native;
    }
    let Some(children) = root.children() else {
        return MarkdownPlan::Native;
    };
    let mut segments = Vec::new();
    let mut cursor = 0;
    for node in children {
        let Node::Code(code) = node else { continue };
        let Some(position) = &code.position else {
            return MarkdownPlan::Native;
        };
        let range = position.start.offset..position.end.offset;
        if range.start < cursor
            || range.start >= range.end
            || range.end > source.len()
            || !source.is_char_boundary(range.start)
            || !source.is_char_boundary(range.end)
        {
            return MarkdownPlan::Native;
        }
        if cursor < range.start {
            segments.push(MarkdownSegment {
                kind: SegmentKind::Markdown,
                range: cursor..range.start,
            });
        }
        cursor = range.end;
        segments.push(MarkdownSegment {
            kind: SegmentKind::Code,
            range,
        });
    }
    if cursor < source.len() {
        segments.push(MarkdownSegment {
            kind: SegmentKind::Markdown,
            range: cursor..source.len(),
        });
    }
    MarkdownPlan::Segmented(segments)
}
