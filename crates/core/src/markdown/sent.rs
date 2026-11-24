//! A message you sent as the Markdown its bubble renders: shown as typed,
//! with complete HTML fragments literal and authored line breaks kept.

use std::borrow::Cow;
use std::ops::Range;

use markdown::mdast::Node;

use super::{fence, fence_run};

/// `text` as Markdown that shows it as typed: complete HTML fragments
/// literal ([`literal_html`]) and every authored line break kept.
pub fn sent_message(text: &str) -> String {
    keep_line_breaks(&literal_html(text))
}

/// A start of [`sent_message`]'s Markdown cut at a line end, without the
/// blank lines it ends with. Inside literal HTML a blank line is a hard
/// break, a lone `\` ([`escape`]): as the cut's last line it would show.
pub fn trim_folded(head: &str) -> &str {
    let mut head = head.trim_end();
    loop {
        let (rest, last) = head.rsplit_once('\n').unwrap_or(("", head));
        let Some(mut indent) = last.strip_suffix('\\') else {
            return head;
        };
        while let Some(rest) = indent
            .strip_prefix("&#32;")
            .or_else(|| indent.strip_prefix("&#9;"))
        {
            indent = rest;
        }
        if !indent.is_empty() {
            return head;
        }
        head = rest.trim_end();
    }
}

/// Escape the complete HTML fragments the Markdown parser finds, so they
/// show as typed. One parse finds every fragment start; starts inside a
/// fragment already escaped are skipped. The text is parsed again only when
/// a fragment ends inside raw text the parser kept (an HTML block or code
/// running past it) holding what could start markup: escaping the fragment
/// uncovers it.
fn literal_html(text: &str) -> String {
    if !text.contains('<') {
        return text.to_string();
    }
    let mut options = markdown::ParseOptions::gfm();
    // TextView reparses unclaimed math containing markup as ordinary prose.
    options.constructs.math_text = false;
    options.constructs.math_flow = false;
    let mut text = Cow::Borrowed(text);
    // Text before `from` is final.
    let mut from = 0;
    loop {
        let Ok(root) = markdown::to_mdast(&text, &options) else {
            return text.into_owned();
        };
        let (mut html, mut raw) = (Vec::new(), Vec::new());
        raw_nodes(&root, &mut html, &mut raw);
        let mut out = String::with_capacity(text.len() + text.len() / 4);
        // Text before `copied` is in `out`.
        let mut copied = 0;
        let mut resume = None;
        for node in html {
            // An HTML block indented one to three spaces starts at its indent.
            let Some(start) = text[node.clone()].find('<').map(|at| node.start + at) else {
                continue;
            };
            if start < from.max(copied) {
                continue;
            }
            let end = fragment_end(&text, start).unwrap_or(start + 1);
            out.push_str(&text[copied..start]);
            escape(&text[start..end], &mut out);
            copied = end;
            if uncovers(&text, &raw, end) {
                resume = Some(out.len());
                break;
            }
        }
        out.push_str(&text[copied..]);
        let Some(at) = resume else {
            return out;
        };
        (text, from) = (Cow::Owned(out), at);
    }
}

/// The HTML nodes' ranges into `html`, and theirs and the code's (the text
/// the parser keeps raw) into `raw`, in order.
fn raw_nodes(node: &Node, html: &mut Vec<Range<usize>>, raw: &mut Vec<Range<usize>>) {
    let range = || node.position().map(|p| p.start.offset..p.end.offset);
    match node {
        Node::Html(_) => {
            if let Some(range) = range() {
                html.push(range.clone());
                raw.push(range);
            }
        }
        Node::InlineCode(_) | Node::Code(_) => raw.extend(range()),
        _ => {
            for child in node.children().into_iter().flatten() {
                raw_nodes(child, html, raw);
            }
        }
    }
}

/// Whether a fragment ending at `end` cuts a `raw` node short, leaving after
/// it what may open markup (a tag, a code span or a fence) once no longer raw.
fn uncovers(text: &str, raw: &[Range<usize>], end: usize) -> bool {
    // Raw nodes never nest: only the last one starting before `end` can hold it.
    let at = raw.partition_point(|node| node.start < end);
    at.checked_sub(1)
        .map(|at| &raw[at])
        .is_some_and(|node| end < node.end && text[end..node.end].contains(['<', '`', '~']))
}

/// `fragment` escaped into `out` so Markdown shows it as typed: punctuation
/// escaped, a line's indent as character references (it would make code),
/// and a blank line as a hard break (it would end the paragraph).
fn escape(fragment: &str, out: &mut String) {
    let mut line_start = false;
    for ch in fragment.chars() {
        if line_start && matches!(ch, ' ' | '\t') {
            out.push_str(if ch == ' ' { "&#32;" } else { "&#9;" });
            continue;
        }
        if line_start && ch == '\n' {
            out.push('\\');
        }
        line_start = ch == '\n';
        if ch.is_ascii_punctuation() {
            out.push('\\');
        }
        out.push(ch);
    }
}

/// The tag at `start`: where it ends, its name, and whether it closes or is
/// void. This intentionally handles ordinary tags, not the full HTML grammar.
fn tag(text: &str, start: usize) -> Option<(usize, &str, bool, bool)> {
    let bytes = text.as_bytes();
    let mut i = start + 1;
    let closing = bytes.get(i) == Some(&b'/');
    i += usize::from(closing);
    let name_start = i;
    if !bytes.get(i)?.is_ascii_alphabetic() {
        return None;
    }
    while bytes
        .get(i)
        .is_some_and(|b| b.is_ascii_alphanumeric() || *b == b'-')
    {
        i += 1;
    }
    let name = &text[name_start..i];
    let mut quote = None;
    while let Some(&b) = bytes.get(i) {
        match (quote, b) {
            (Some(q), c) if q == c => quote = None,
            (None, b'\'' | b'"') => quote = Some(b),
            (None, b'>') => {
                let self_closing = text[start..i].trim_end().ends_with('/')
                    || [
                        "area", "base", "br", "col", "embed", "hr", "img", "input", "link", "meta",
                        "param", "source", "track", "wbr",
                    ]
                    .iter()
                    .any(|v| name.eq_ignore_ascii_case(v));
                return Some((i + 1, name, closing, self_closing));
            }
            _ => {}
        }
        i += 1;
    }
    None
}

/// Where the fragment whose tag starts at `start` ends: after its matching
/// closing tag, or after the tag itself when it closes or is void.
fn fragment_end(text: &str, start: usize) -> Option<usize> {
    let (end, name, closing, void) = tag(text, start)?;
    if closing || void {
        return Some(end);
    }
    let mut stack = vec![name];
    let mut cursor = end;
    while let Some(relative) = text[cursor..].find('<') {
        let at = cursor + relative;
        if text[at..].starts_with("<!--") {
            cursor = text[at + 4..]
                .find("-->")
                .map_or(text.len(), |end| at + 4 + end + 3);
            continue;
        }
        if let Some((next, name, closing, void)) = tag(text, at) {
            if closing {
                if let Some(index) = stack
                    .iter()
                    .rposition(|open| open.eq_ignore_ascii_case(name))
                {
                    stack.truncate(index);
                    if stack.is_empty() {
                        return Some(next);
                    }
                }
            } else if !void {
                stack.push(name);
            }
            cursor = next;
        } else {
            cursor = at + 1;
        }
    }
    Some(end)
}

/// Preserve authored line breaks in Markdown paragraphs without changing fenced code.
fn keep_line_breaks(text: &str) -> String {
    let mut result = String::with_capacity(text.len());
    // The open fence's character, length and blockquote depth.
    let mut open: Option<(char, usize, usize)> = None;
    let mut paragraph = false;
    let mut list_indents = Vec::new();
    for line in text.split_inclusive('\n') {
        let content = line.strip_suffix('\n').unwrap_or(line);
        let content = content.strip_suffix('\r').unwrap_or(content);
        let mut container = content;
        let mut quote_depth = 0;
        loop {
            let candidate = container.trim_start_matches(' ');
            if container.len() - candidate.len() > 3 || !candidate.starts_with('>') {
                break;
            }
            container = &candidate[1..];
            container = container.strip_prefix(' ').unwrap_or(container);
            quote_depth += 1;
        }
        if open.is_some_and(|(_, _, depth)| quote_depth < depth) {
            open = None;
        }
        let trimmed = container.trim_start_matches(' ');
        let indent = container.len() - trimmed.len();
        if open.is_none() && !trimmed.is_empty() {
            while list_indents.last().is_some_and(|&level| indent < level) {
                list_indents.pop();
            }
            let bytes = trimmed.as_bytes();
            let marker_len = if matches!(bytes.first(), Some(b'-' | b'+' | b'*')) {
                1
            } else {
                let digits = bytes.iter().take_while(|b| b.is_ascii_digit()).count();
                if digits > 0 && digits <= 9 && matches!(bytes.get(digits), Some(b'.' | b')')) {
                    digits + 1
                } else {
                    0
                }
            };
            if marker_len > 0 && bytes.get(marker_len) == Some(&b' ') {
                list_indents.push(indent + marker_len + 1);
            }
        }
        let list_indent = list_indents.last().copied().unwrap_or(0);
        let relative_indent = indent.saturating_sub(list_indent);
        // The line inside its list item, for the fence helpers' indent rule.
        let item_line = &container[indent.min(list_indent)..];
        let inside = open.is_some();
        let opens = match open {
            Some((c, n, _)) => {
                if fence_run(item_line)
                    .is_some_and(|(r, len, rest)| r == c && len >= n && rest.trim().is_empty())
                {
                    open = None;
                }
                false
            }
            None => match fence(item_line) {
                Some((c, n, _)) => {
                    open = Some((c, n, quote_depth));
                    true
                }
                None => false,
            },
        };
        let paragraph_line =
            !inside && !opens && !content.trim().is_empty() && (relative_indent < 4 || paragraph);
        let heading_marks = trimmed.bytes().take_while(|b| *b == b'#').count();
        let heading = relative_indent <= 3
            && (1..=6).contains(&heading_marks)
            && trimmed
                .as_bytes()
                .get(heading_marks)
                .is_none_or(|b| matches!(b, b' ' | b'\t'));
        paragraph = paragraph_line && !heading;
        result.push_str(content);
        if line.ends_with('\n') {
            let backslashes = content.bytes().rev().take_while(|b| *b == b'\\').count();
            if paragraph_line && backslashes % 2 == 0 {
                result.push_str("  ");
            }
            if line.ends_with("\r\n") {
                result.push('\r');
            }
            result.push('\n');
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn html_block_makes_embedded_fence_literal() {
        let source = "<div>\n```\n<span>\n```\n</div>";
        assert_eq!(
            literal_html(source),
            "\\<div\\>\n\\`\\`\\`\n\\<span\\>\n\\`\\`\\`\n\\<\\/div\\>"
        );
    }

    #[test]
    fn html_preprocessing_preserves_code_and_links() {
        let source = "`<div>` [link](<https://example.com>) &amp;\n\n```html\n<div>\n</div>\n```";
        assert_eq!(literal_html(source), source);
    }

    #[test]
    fn markup_an_escaped_fragment_uncovers_is_literal_too() {
        // The `<div>` block runs to the line's end, the `<b>` block to the
        // blank line: what follows their fragments was raw HTML at first.
        assert_eq!(
            literal_html("<div>a</div><span>b</span>"),
            r"\<div\>a\<\/div\>\<span\>b\<\/span\>"
        );
        assert_eq!(
            literal_html("<b>\n</b>\nmore <i>y</i>\n\nafter"),
            "\\<b\\>\n\\<\\/b\\>\nmore \\<i\\>y\\<\\/i\\>\n\nafter"
        );
        // The fragment ends inside a code span: its other backtick no longer
        // hides the tag after it.
        assert_eq!(
            literal_html("<span>`</span> <b>`"),
            r"\<span\>\`\<\/span\> \<b\>`"
        );
    }

    #[test]
    fn a_folded_start_drops_the_blank_lines_it_ends_with() {
        let markdown = sent_message("<div>\na\n\nb\n</div>");
        let head = |lines: usize| {
            let end = markdown.match_indices('\n').nth(lines - 1).unwrap().0;
            trim_folded(&markdown[..end])
        };
        assert_eq!(head(3), "\\<div\\>  \na");
        assert_eq!(head(4), "\\<div\\>  \na  \n\\\nb");
        // A blank line with spaces keeps them as character references.
        let markdown = sent_message("<div>\na\n  \nb\n</div>");
        let end = markdown.find("\nb").unwrap();
        assert_eq!(trim_folded(&markdown[..end]), "\\<div\\>  \na");
        // An authored hard break at the end stays.
        assert_eq!(trim_folded("one\\"), "one\\");
    }

    #[test]
    fn indented_html_is_literal() {
        assert_eq!(literal_html("  <div>x</div>"), r"  \<div\>x\<\/div\>");
        assert_eq!(
            literal_html("text\n  <img src=x>"),
            "text\n  \\<img src\\=x\\>"
        );
        assert_eq!(
            literal_html("   <a href=\"https://x\">y</a>"),
            r#"   \<a href\=\"https\:\/\/x\"\>y\<\/a\>"#
        );
    }

    #[test]
    fn preserves_existing_breaks() {
        let source = "paragraph `code`\nnext\\\nlast\n- item\n  continuation\n";
        let expected = "paragraph `code`  \nnext\\\nlast  \n- item  \n  continuation  \n";
        assert_eq!(sent_message(source), expected);
    }

    #[test]
    fn blockquote_fence() {
        let source = "> ~~~\n> one\n> two\n> ~~~\n";
        assert_eq!(sent_message(source), source);
    }

    #[test]
    fn nested_list_fence() {
        let source = "- outer\n  - inner\n\n    ```\n    one\n    two\n    ```\n";
        let expected = "- outer  \n  - inner  \n\n    ```\n    one\n    two\n    ```\n";
        assert_eq!(sent_message(source), expected);
    }

    #[test]
    fn indented_paragraph_continuation() {
        let source = "one\n    two\nthree\n";
        let expected = "one  \n    two  \nthree  \n";
        assert_eq!(sent_message(source), expected);
    }

    #[test]
    fn unclosed_quoted_fence() {
        let source = "> ```\n> code\none\ntwo\n";
        let expected = "> ```\n> code\none  \ntwo  \n";
        assert_eq!(sent_message(source), expected);
    }

    #[test]
    fn indented_code() {
        let source = "    one\n    two\n";
        assert_eq!(sent_message(source), source);
    }

    #[test]
    fn indented_code_after_heading() {
        let source = "# Heading\n    one\n    two\n";
        let expected = "# Heading  \n    one\n    two\n";
        assert_eq!(sent_message(source), expected);
    }
}
