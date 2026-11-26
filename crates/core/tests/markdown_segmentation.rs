use gentle_core::markdown::{MarkdownPlan, SegmentKind, segment_source};

fn parts(source: &str) -> Vec<(SegmentKind, &str)> {
    let MarkdownPlan::Segmented(segments) = segment_source(source) else {
        panic!("expected segmented document: {source:?}");
    };
    let mut cursor = 0;
    let result: Vec<_> = segments
        .into_iter()
        .map(|segment| {
            assert_eq!(segment.range.start, cursor);
            cursor = segment.range.end;
            (segment.kind, &source[segment.range])
        })
        .collect();
    assert_eq!(cursor, source.len());
    assert_eq!(
        result.iter().map(|(_, text)| *text).collect::<String>(),
        source
    );
    result
}

#[test]
fn exact_source_and_only_root_code() {
    let source = "# 世界\r\n\r\n- item\r\n\r\n  ```rust\r\n  nested\r\n  ```\r\n\r\n> ```\r\n> quoted\r\n> ```\r\n\r\n```rust\r\n α  \r\n```\r\nend";
    let split = parts(source);
    assert_eq!(split.len(), 3);
    assert_eq!(split[0].0, SegmentKind::Markdown);
    assert_eq!(split[1], (SegmentKind::Code, "```rust\r\n α  \r\n```"));
    assert_eq!(split[2], (SegmentKind::Markdown, "\r\nend"));
}

#[test]
fn reparsing_stream_and_replacements() {
    let mut source = String::new();
    for chunk in ["intro\n\n", "```r", "ust\n", "α", "\n```", "\n\nafter"] {
        source.push_str(chunk);
        let segments = parts(&source);
        if source.contains("```") {
            assert_eq!(segments[1].0, SegmentKind::Code);
            assert!(segments[1].1.starts_with("```"));
        }
    }
    assert_eq!(parts(&source)[1].1, "```rust\nα\n```");
    assert_eq!(parts("plain"), vec![(SegmentKind::Markdown, "plain")]);
    assert!(parts("").is_empty());
}

#[test]
fn semantic_risks_fall_back_for_whole_document_and_updates() {
    let unresolved = "[guide][ref]\n\n```rust\nx\n```";
    assert!(matches!(
        segment_source(unresolved),
        MarkdownPlan::Segmented(_)
    ));
    let resolved = format!("{unresolved}\n\n[ref]: https://example.org\n");
    assert_eq!(segment_source(&resolved), MarkdownPlan::Native);
    for source in [
        "```\nx\n```\n\n[unused]: /url",
        "![alt][id]\n\n```\nx\n```\n\n[id]: /img",
        "footnote[^1]\n\n```\nx\n```\n\n[^1]: note",
        "<div>hello</div>\n\n```\nx\n```",
        "inline <b>html</b>\n\n```\nx\n```",
    ] {
        assert_eq!(segment_source(source), MarkdownPlan::Native, "{source:?}");
    }
    assert!(matches!(
        segment_source(unresolved),
        MarkdownPlan::Segmented(_)
    ));
}

#[test]
fn ordinary_gfm_remains_segmentable() {
    let source = "- [x] [inline](https://example.org)\n\n|a|b|\n|-|-|\n|α|β|\n\n~~~text\nx\n~~~";
    let segments = parts(source);
    assert_eq!(segments.len(), 2);
    assert_eq!(segments[1], (SegmentKind::Code, "~~~text\nx\n~~~"));
}
