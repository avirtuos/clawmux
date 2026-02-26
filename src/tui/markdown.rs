//! Shared markdown rendering utilities for ratatui.
//!
//! Provides [`markdown_to_lines`] and [`visual_line_count`] used by multiple
//! tab modules (Agent Activity, Review) to convert markdown text into styled
//! ratatui [`Line`]s and compute wrapped visual heights.

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

/// Converts a markdown string into styled ratatui [`Line`]s.
///
/// Uses `pulldown-cmark` to parse the input and applies ratatui styles:
/// - Bold (`**text**`) -> [`Modifier::BOLD`]
/// - Italic (`*text*`) -> [`Modifier::ITALIC`]
/// - Inline code (`` `code` ``) -> cyan text
/// - Code blocks -> dark gray text, split on newlines
/// - Headings -> bold + color (H1=Cyan, H2=Blue, other=LightBlue)
/// - Soft/hard breaks -> new [`Line`]
/// - List items -> `- ` prefix
///
/// Returns a `Vec<Line<'static>>` suitable for rendering with ratatui's [`Paragraph`].
pub fn markdown_to_lines(input: &str) -> Vec<Line<'static>> {
    use pulldown_cmark::{Event, HeadingLevel, Options, Parser, Tag, TagEnd};

    let mut lines: Vec<Line<'static>> = Vec::new();
    let mut current_spans: Vec<Span<'static>> = Vec::new();
    let mut bold = false;
    let mut italic = false;
    let mut in_code_block = false;
    let mut heading_color: Option<Color> = None;
    let mut list_depth: usize = 0;

    let parser = Parser::new_ext(input, Options::empty());

    for event in parser {
        match event {
            Event::Start(Tag::Heading { level, .. }) => {
                bold = true;
                heading_color = Some(match level {
                    HeadingLevel::H1 => Color::Cyan,
                    HeadingLevel::H2 => Color::Blue,
                    _ => Color::LightBlue,
                });
            }
            Event::End(TagEnd::Heading(_)) => {
                lines.push(Line::from(std::mem::take(&mut current_spans)));
                lines.push(Line::from(""));
                bold = false;
                heading_color = None;
            }
            Event::Start(Tag::Paragraph) => {}
            Event::End(TagEnd::Paragraph) => {
                if !current_spans.is_empty() {
                    lines.push(Line::from(std::mem::take(&mut current_spans)));
                }
                lines.push(Line::from(""));
            }
            Event::Start(Tag::Strong) => {
                bold = true;
            }
            Event::End(TagEnd::Strong) => {
                bold = false;
            }
            Event::Start(Tag::Emphasis) => {
                italic = true;
            }
            Event::End(TagEnd::Emphasis) => {
                italic = false;
            }
            Event::Start(Tag::CodeBlock(_)) => {
                in_code_block = true;
            }
            Event::End(TagEnd::CodeBlock) => {
                if !current_spans.is_empty() {
                    lines.push(Line::from(std::mem::take(&mut current_spans)));
                }
                lines.push(Line::from(""));
                in_code_block = false;
            }
            Event::Code(text) => {
                // Inline code span.
                let style = Style::default().fg(Color::Cyan);
                current_spans.push(Span::styled(format!("`{}`", text.as_ref()), style));
            }
            Event::Start(Tag::List(_)) => {
                list_depth += 1;
            }
            Event::End(TagEnd::List(_)) => {
                list_depth = list_depth.saturating_sub(1);
            }
            Event::Start(Tag::Item) => {
                let indent = "  ".repeat(list_depth.saturating_sub(1));
                current_spans.push(Span::raw(format!("{}- ", indent)));
            }
            Event::End(TagEnd::Item) => {
                if !current_spans.is_empty() {
                    lines.push(Line::from(std::mem::take(&mut current_spans)));
                }
            }
            Event::Text(text) => {
                let mut style = Style::default();
                if bold {
                    style = style.add_modifier(Modifier::BOLD);
                }
                if italic {
                    style = style.add_modifier(Modifier::ITALIC);
                }
                if let Some(color) = heading_color {
                    style = style.fg(color);
                }
                if in_code_block {
                    style = style.fg(Color::DarkGray);
                    // Code block text may contain embedded newlines; split into separate lines.
                    for (i, segment) in text.as_ref().split('\n').enumerate() {
                        if i > 0 {
                            lines.push(Line::from(std::mem::take(&mut current_spans)));
                        }
                        if !segment.is_empty() {
                            current_spans.push(Span::styled(segment.to_string(), style));
                        }
                    }
                } else {
                    current_spans.push(Span::styled(text.to_string(), style));
                }
            }
            Event::SoftBreak | Event::HardBreak => {
                lines.push(Line::from(std::mem::take(&mut current_spans)));
            }
            _ => {}
        }
    }

    if !current_spans.is_empty() {
        lines.push(Line::from(current_spans));
    }

    lines
}

/// Computes the total number of visual (wrapped) lines for `lines` at a given `width`.
///
/// Each `Line` whose display width exceeds `width` wraps into multiple visual rows.
/// Empty lines always contribute exactly one visual row.
pub fn visual_line_count(lines: &[Line], width: u16) -> usize {
    let w = width.max(1) as usize;
    lines
        .iter()
        .map(|line| {
            let lw = line.width();
            if lw == 0 {
                1
            } else {
                lw.div_ceil(w)
            }
        })
        .sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Verifies that `**bold**` markdown produces a span with the BOLD modifier.
    #[test]
    fn test_markdown_bold_produces_bold_span() {
        let lines = markdown_to_lines("**bold**");
        let has_bold = lines.iter().any(|line| {
            line.spans.iter().any(|span| {
                span.style
                    .add_modifier
                    .contains(ratatui::style::Modifier::BOLD)
            })
        });
        assert!(
            has_bold,
            "**bold** should produce a span with BOLD modifier; lines: {lines:?}"
        );
    }

    /// Verifies that two lines separated by a newline produce at least 2 non-empty Lines.
    #[test]
    fn test_markdown_multiline_splits_lines() {
        let lines = markdown_to_lines("line1\nline2");
        let non_empty: Vec<_> = lines.iter().filter(|l| l.width() > 0).collect();
        assert!(
            non_empty.len() >= 2,
            "should produce at least 2 non-empty lines from 'line1\\nline2'; got {} total: {lines:?}",
            lines.len()
        );
    }

    /// Verifies that plain text passes through without losing content.
    #[test]
    fn test_markdown_plain_text_passthrough() {
        let lines = markdown_to_lines("hello world");
        assert!(!lines.is_empty(), "should produce at least one line");
        let found = lines
            .iter()
            .any(|l| l.spans.iter().any(|s| s.content.contains("hello world")));
        assert!(
            found,
            "plain text 'hello world' should appear in output; lines: {lines:?}"
        );
    }

    /// Verifies that visual_line_count correctly counts wrapped lines.
    #[test]
    fn test_visual_line_count_wrapping() {
        // A line of width 10 in a 5-wide viewport wraps to 2 visual rows.
        let lines = vec![
            Line::from("abcdefghij"), // width 10
            Line::from("ab"),         // width 2
            Line::from(""),           // empty -> 1 row
        ];
        // At width=5: ceil(10/5)=2 + ceil(2/5)=1 + 1(empty) = 4
        assert_eq!(visual_line_count(&lines, 5), 4);

        // At width=10: ceil(10/10)=1 + 1 + 1 = 3
        assert_eq!(visual_line_count(&lines, 10), 3);
    }
}
