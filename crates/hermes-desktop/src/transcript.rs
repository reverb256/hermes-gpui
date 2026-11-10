//! Transcript model. Kept free of GPUI so the streaming merge rules can be
//! tested without a window.

/// Who produced a line.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Role {
    User,
    Assistant,
    Reasoning,
    System,
}

/// One rendered block.
#[derive(Clone, Debug)]
pub struct Line {
    pub role: Role,
    pub text: String,
}

impl Line {
    pub fn new(role: Role, text: impl Into<String>) -> Self {
        Self {
            role,
            text: text.into(),
        }
    }
}

/// One line of roster copy: whitespace collapsed and ellipsized on a char
/// boundary, so a preview never reflows the rail.
pub fn clip(text: &str, max_chars: usize) -> String {
    let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() <= max_chars {
        return flat;
    }
    let mut out: String = flat.chars().take(max_chars.saturating_sub(1)).collect();
    out.push('…');
    out
}

/// Recognize standalone Markdown image references to host-local image paths.
/// Fetching is explicit: ordinary links and remote URLs are never requested.
pub fn host_image_path(line: &str) -> Option<&str> {
    let line = line.trim();
    let (_, rest) = line.strip_prefix("![")?.split_once("](")?;
    let path = rest.strip_suffix(')')?;
    if !path.starts_with('/') || path.contains(['\n', '\r']) {
        return None;
    }
    let lower = path.to_ascii_lowercase();
    [".png", ".jpg", ".jpeg", ".webp"]
        .iter()
        .any(|extension| lower.ends_with(extension))
        .then_some(path)
}

/// Inline ranges are UTF-8 byte offsets suitable for GPUI's StyledText.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct MarkdownText {
    pub text: String,
    pub spans: Vec<MarkdownSpan>,
}

impl From<&str> for MarkdownText {
    fn from(value: &str) -> Self {
        Self {
            text: value.to_string(),
            spans: Vec::new(),
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub struct MarkdownSpan {
    pub range: std::ops::Range<usize>,
    pub bold: bool,
    pub italic: bool,
    pub code: bool,
    pub link: bool,
}

/// Renderable Markdown blocks. Links remain text; remote content is never fetched.
#[derive(Debug, PartialEq, Eq)]
pub enum MarkdownBlock {
    Paragraph(MarkdownText),
    Heading(MarkdownText),
    Code(MarkdownText),
    Quote(MarkdownText),
    ListItem(MarkdownText),
    Image(String),
}

pub fn markdown_blocks(source: &str) -> Vec<MarkdownBlock> {
    use pulldown_cmark::{Event, Parser, Tag, TagEnd};
    #[derive(Clone, Copy)]
    enum Kind {
        Paragraph,
        Heading,
        Code,
        Quote,
        Item,
    }
    fn finish(blocks: &mut Vec<MarkdownBlock>, text: &mut MarkdownText, kind: Kind) {
        if text.text.is_empty() {
            return;
        }
        let content = std::mem::take(text);
        blocks.push(match kind {
            Kind::Paragraph => MarkdownBlock::Paragraph(content),
            Kind::Heading => MarkdownBlock::Heading(content),
            Kind::Code => MarkdownBlock::Code(content),
            Kind::Quote => MarkdownBlock::Quote(content),
            Kind::Item => MarkdownBlock::ListItem(content),
        });
    }
    let mut blocks = Vec::new();
    let mut text = MarkdownText::default();
    let mut bold = 0usize;
    let mut italic = 0usize;
    let mut link = 0usize;
    let mut kind = Kind::Paragraph;
    let mut quote = false;
    let mut item = false;
    let mut image = None;
    for event in Parser::new(source) {
        match event {
            Event::Start(Tag::Heading { .. }) => {
                finish(&mut blocks, &mut text, kind);
                kind = Kind::Heading;
            }
            Event::Start(Tag::CodeBlock(_)) => {
                finish(&mut blocks, &mut text, kind);
                kind = Kind::Code;
            }
            Event::Start(Tag::BlockQuote(_)) => {
                quote = true;
            }
            Event::Start(Tag::Item) => {
                item = true;
            }
            Event::Start(Tag::Paragraph) => {
                kind = if item {
                    Kind::Item
                } else if quote {
                    Kind::Quote
                } else {
                    Kind::Paragraph
                };
            }
            Event::Start(Tag::Strong) => bold += 1,
            Event::End(TagEnd::Strong) => bold = bold.saturating_sub(1),
            Event::Start(Tag::Emphasis) => italic += 1,
            Event::End(TagEnd::Emphasis) => italic = italic.saturating_sub(1),
            Event::Start(Tag::Link { .. }) => link += 1,
            Event::End(TagEnd::Link) => link = link.saturating_sub(1),
            Event::Start(Tag::Image { dest_url, .. }) => {
                image = Some(dest_url.to_string());
            }
            Event::End(TagEnd::Image) => {
                if let Some(path) = image.take()
                    && host_image_path(&format!("![]({path})")).is_some()
                {
                    finish(&mut blocks, &mut text, kind);
                    blocks.push(MarkdownBlock::Image(path));
                }
            }
            Event::End(TagEnd::Paragraph | TagEnd::Heading(_) | TagEnd::CodeBlock) => {
                finish(&mut blocks, &mut text, kind)
            }
            Event::End(TagEnd::BlockQuote(_)) => quote = false,
            Event::End(TagEnd::Item) => {
                finish(&mut blocks, &mut text, Kind::Item);
                item = false;
            }
            event @ (Event::Text(_) | Event::Code(_)) => {
                let (value, code) = match event {
                    Event::Text(value) => (value, false),
                    Event::Code(value) => (value, true),
                    _ => unreachable!(),
                };
                let start = text.text.len();
                text.text.push_str(&value);
                let end = text.text.len();
                if start < end && (bold > 0 || italic > 0 || link > 0 || code) {
                    text.spans.push(MarkdownSpan {
                        range: start..end,
                        bold: bold > 0,
                        italic: italic > 0,
                        code,
                        link: link > 0,
                    });
                }
            }
            Event::SoftBreak | Event::HardBreak => text.text.push('\n'),
            Event::Rule => {
                finish(&mut blocks, &mut text, kind);
                blocks.push(MarkdownBlock::Paragraph("────────".into()));
            }
            Event::Html(value) | Event::InlineHtml(value) => text.text.push_str(&value),
            _ => {}
        }
    }
    finish(&mut blocks, &mut text, kind);
    blocks
}

/// Line indices whose text contains `needle`, in reading order. Matching is
/// case-insensitive; an empty or whitespace-only needle has no matches, so the
/// find bar never reports the whole transcript as a hit.
pub fn find_lines(lines: &[Line], needle: &str) -> Vec<usize> {
    let needle = needle.trim().to_lowercase();
    if needle.is_empty() {
        return Vec::new();
    }
    lines
        .iter()
        .enumerate()
        .filter(|(_, line)| line.text.to_lowercase().contains(&needle))
        .map(|(index, _)| index)
        .collect()
}

/// Append streamed text to the trailing line of the same role, or start a new
/// one when the speaker changed. Deltas from the gateway arrive per token, so
/// this runs once per frame.
pub fn append_streaming(lines: &mut Vec<Line>, role: Role, text: &str) {
    match lines.last_mut() {
        Some(line) if line.role == role => line.text.push_str(text),
        _ => lines.push(Line::new(role, text)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognizes_only_standalone_host_image_references() {
        assert_eq!(
            host_image_path("![chart](/home/user/.hermes/images/a.png)"),
            Some("/home/user/.hermes/images/a.png")
        );
        assert_eq!(host_image_path("![x](https://example.test/a.png)"), None);
        assert_eq!(host_image_path("prefix ![x](/tmp/a.png)"), None);
        assert_eq!(host_image_path("![x](/tmp/a.pdf)"), None);
    }

    #[test]
    fn markdown_preserves_blocks_streaming_and_untrusted_text() {
        assert_eq!(
            markdown_blocks("# Heading\n\nHello **world**\n\n```rust\nlet x = 1;\n```"),
            vec![
                MarkdownBlock::Heading("Heading".into()),
                MarkdownBlock::Paragraph(MarkdownText {
                    text: "Hello world".into(),
                    spans: vec![MarkdownSpan {
                        range: 6..11,
                        bold: true,
                        italic: false,
                        code: false,
                        link: false
                    }]
                }),
                MarkdownBlock::Code("let x = 1;\n".into()),
            ]
        );
        assert_eq!(
            markdown_blocks("```\npartial"),
            vec![MarkdownBlock::Code("partial".into())]
        );
        assert_eq!(
            markdown_blocks("<script>alert(1)</script>"),
            vec![MarkdownBlock::Paragraph("<script>alert(1)</script>".into())]
        );
        assert!(
            markdown_blocks("![chart](/tmp/a.png)")
                .contains(&MarkdownBlock::Image("/tmp/a.png".into()))
        );
        assert!(
            !markdown_blocks("![remote](https://example.test/a.png)")
                .iter()
                .any(|block| matches!(block, MarkdownBlock::Image(_)))
        );
    }

    #[test]
    fn markdown_inline_spans_are_non_overlapping_utf8_offsets() {
        let blocks = markdown_blocks("é **bold *both*** and `code` [link](https://example.test)");
        let MarkdownBlock::Paragraph(text) = &blocks[0] else {
            panic!("expected paragraph")
        };
        assert!(
            text.spans
                .iter()
                .all(|span| text.text.is_char_boundary(span.range.start)
                    && text.text.is_char_boundary(span.range.end))
        );
        assert!(
            text.spans
                .windows(2)
                .all(|pair| pair[0].range.end <= pair[1].range.start)
        );
        assert!(
            text.spans
                .iter()
                .any(|span| span.bold && span.italic && &text.text[span.range.clone()] == "both")
        );
        assert!(
            text.spans
                .iter()
                .any(|span| span.code && &text.text[span.range.clone()] == "code")
        );
        assert!(
            text.spans
                .iter()
                .any(|span| span.link && &text.text[span.range.clone()] == "link")
        );
    }

    #[test]
    fn find_lines_is_case_insensitive_and_ignores_blank_needles() {
        let lines = vec![
            Line::new(Role::User, "Show me the Cluster health."),
            Line::new(Role::Assistant, "All four nodes are Ready."),
            Line::new(Role::System, "shell: cluster ok"),
        ];
        assert_eq!(find_lines(&lines, "cluster"), vec![0, 2]);
        assert_eq!(find_lines(&lines, "  READY "), vec![1]);
        assert_eq!(find_lines(&lines, ""), Vec::<usize>::new());
        assert_eq!(find_lines(&lines, "   "), Vec::<usize>::new());
        assert_eq!(find_lines(&lines, "absent"), Vec::<usize>::new());
    }

    #[test]
    fn consecutive_deltas_merge_into_one_line() {
        let mut lines = vec![Line::new(Role::User, "hi")];
        append_streaming(&mut lines, Role::Assistant, "Hel");
        append_streaming(&mut lines, Role::Assistant, "lo");
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[1].text, "Hello");
    }

    #[test]
    fn role_change_starts_a_new_line() {
        let mut lines = vec![Line::new(Role::User, "hi")];
        append_streaming(&mut lines, Role::Assistant, "answer");
        append_streaming(&mut lines, Role::Reasoning, "because");
        append_streaming(&mut lines, Role::Assistant, "more");
        assert_eq!(lines.len(), 4);
        assert_eq!(lines[2].role, Role::Reasoning);
        assert_eq!(lines[3].text, "more");
    }

    #[test]
    fn clip_collapses_whitespace_and_ellipsizes() {
        assert_eq!(clip("line one\n\nline two", 40), "line one line two");
        assert_eq!(clip("short", 40), "short");
        let clipped = clip("abcdefghij", 5);
        assert_eq!(clipped.chars().count(), 5);
        assert!(clipped.ends_with('…'));
        assert_eq!(clip("", 5), "");
    }

    #[test]
    fn streaming_into_empty_transcript_creates_a_line() {
        let mut lines = Vec::new();
        append_streaming(&mut lines, Role::System, "connected");
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].text, "connected");
    }
}
