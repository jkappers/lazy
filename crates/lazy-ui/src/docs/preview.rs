//! The rendered view of a page's markdown.
//!
//! Headings (`#` to `######`), task items (`- [ ]` and `- [x]`), and rules (`---`) are drawn here;
//! everything else (paragraphs, bold, italic, code, quotes, lists, links) goes through the message
//! document parser and renderer, so a page reads like a message does. The page editor
//! (`text_input::live`) reads the same syntax. The text is cut at headings and, for very long runs, at blank lines, so
//! the message limits (100 blocks, 40,000 bytes) never refuse a long page. A piece the parser
//! refuses is shown as plain text. Parsing is pure and the result is cached by the view; nothing
//! here does I/O.

use gpui::{
    AnyElement, FontWeight, IntoElement, ParentElement, Pixels, Styled, div, prelude::*, px,
};
use lazy_domain::MessageDocument;

use crate::rich_text::{DocumentProps, HandlesOnly, open_external_only, render_document};
use crate::theme::Theme;

/// Longest run of text handed to the document parser at once, in bytes.
const CHUNK_BYTES: usize = 16_000;

/// One item of a task list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Task {
    pub checked: bool,
    pub text: String,
    /// The item's text with its formatting, when the message parser takes it.
    pub document: Option<MessageDocument>,
}

/// One drawn piece of a page.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Piece {
    Heading {
        level: u8,
        text: String,
    },
    /// A run of consecutive `- [ ]` and `- [x]` lines.
    Tasks(Vec<Task>),
    /// `---` on a line of its own.
    Rule,
    Rich(MessageDocument),
    Plain(String),
}

/// Splits markdown into pieces.
pub fn parse(text: &str) -> Vec<Piece> {
    let mut pieces = Vec::new();
    let mut chunk = String::new();
    let mut fenced = false;
    for line in text.split('\n') {
        let trimmed = line.trim_start();
        if trimmed.starts_with("```") {
            fenced = !fenced;
        }
        if !fenced
            && !trimmed.starts_with("```")
            && let Some((level, title)) = heading(line)
        {
            flush(&mut chunk, &mut pieces);
            pieces.push(Piece::Heading {
                level,
                text: title.to_owned(),
            });
            continue;
        }
        if !fenced && !trimmed.starts_with("```") {
            if is_rule(line) {
                flush(&mut chunk, &mut pieces);
                pieces.push(Piece::Rule);
                continue;
            }
            if let Some((checked, text)) = task_item(line) {
                flush(&mut chunk, &mut pieces);
                let task = Task {
                    checked,
                    text: text.to_owned(),
                    document: MessageDocument::from_markdown(text).ok(),
                };
                match pieces.last_mut() {
                    Some(Piece::Tasks(tasks)) => tasks.push(task),
                    _ => pieces.push(Piece::Tasks(vec![task])),
                }
                continue;
            }
        }
        if !fenced && line.trim().is_empty() && chunk.len() >= CHUNK_BYTES {
            flush(&mut chunk, &mut pieces);
            continue;
        }
        if !chunk.is_empty() {
            chunk.push('\n');
        }
        chunk.push_str(line);
        // A single enormous line still has to fit the parser.
        if chunk.len() >= CHUNK_BYTES * 2 && !fenced {
            flush(&mut chunk, &mut pieces);
        }
    }
    flush(&mut chunk, &mut pieces);
    pieces
}

fn heading(line: &str) -> Option<(u8, &str)> {
    let hashes = line.bytes().take_while(|b| *b == b'#').count();
    if !(1..=6).contains(&hashes) {
        return None;
    }
    let rest = &line[hashes..];
    let title = rest.strip_prefix(' ').or_else(|| rest.strip_prefix('\t'))?;
    Some((hashes as u8, title.trim()))
}

fn is_rule(line: &str) -> bool {
    let trimmed = line.trim();
    trimmed.len() >= 3 && trimmed.bytes().all(|b| b == b'-')
}

/// A `- [ ] text` or `- [x] text` line (also with `*`, and up to three spaces of indent): whether
/// it is ticked, and the text.
fn task_item(line: &str) -> Option<(bool, &str)> {
    let indent = line.bytes().take_while(|b| *b == b' ').count();
    if indent > 3 {
        return None;
    }
    let rest = line[indent..].strip_prefix(['-', '*'])?;
    let rest = rest.strip_prefix(char::is_whitespace)?;
    let (checked, rest) = match rest.strip_prefix("[ ]") {
        Some(rest) => (false, rest),
        None => (
            true,
            rest.strip_prefix("[x]")
                .or_else(|| rest.strip_prefix("[X]"))?,
        ),
    };
    if !rest.is_empty() && !rest.starts_with(char::is_whitespace) {
        return None;
    }
    Some((checked, rest.trim()))
}

fn flush(chunk: &mut String, pieces: &mut Vec<Piece>) {
    let text = std::mem::take(chunk);
    if text.trim().is_empty() {
        return;
    }
    pieces.push(match MessageDocument::from_markdown(&text) {
        Ok(document) => Piece::Rich(document),
        Err(_) => Piece::Plain(text),
    });
}

fn heading_size(theme: &Theme, level: u8) -> Pixels {
    match level {
        1 => theme.type_scale.heading * 1.4,
        2 => theme.type_scale.heading * 1.2,
        3 => theme.type_scale.heading,
        _ => theme.type_scale.title,
    }
}

/// Draws the pieces one under another. `id` tells this page's links apart from other documents.
pub fn render(pieces: &[Piece], theme: &Theme, id: &'static str) -> AnyElement {
    let colors = theme.colors;
    div()
        .flex()
        .flex_col()
        .gap_3()
        .w_full()
        .min_w_0()
        .children(pieces.iter().enumerate().map(|(index, piece)| {
            match piece {
                Piece::Heading { level, text } => div()
                    .debug_selector(move || format!("docs-preview-heading-{index}"))
                    .mt_2()
                    .font_weight(FontWeight::BOLD)
                    .text_size(heading_size(theme, *level))
                    .text_color(colors.text)
                    .child(text.clone())
                    .into_any_element(),
                Piece::Rule => div()
                    .debug_selector(move || format!("docs-preview-rule-{index}"))
                    .my_2()
                    .h(px(1.))
                    .w_full()
                    .bg(colors.border_strong)
                    .into_any_element(),
                Piece::Tasks(tasks) => div()
                    .debug_selector(move || format!("docs-preview-tasks-{index}"))
                    .flex()
                    .flex_col()
                    .gap_1()
                    .children(tasks.iter().enumerate().map(|(n, task)| {
                        let color = if task.checked {
                            colors.text_muted
                        } else {
                            colors.text
                        };
                        div()
                            .flex()
                            .items_start()
                            .gap_2()
                            .child(
                                div()
                                    .flex_none()
                                    .text_color(colors.accent)
                                    .child(if task.checked { "☑" } else { "☐" }),
                            )
                            .child(div().flex_1().min_w_0().text_color(color).child(
                                match &task.document {
                                    Some(document) => render_document(DocumentProps {
                                        document,
                                        theme,
                                        id: gpui::ElementId::Name(
                                            format!("{id}-{index}-{n}").into(),
                                        ),
                                        on_link: open_external_only(),
                                        mentions: &HandlesOnly,
                                        color,
                                        select: None,
                                    }),
                                    None => div().child(task.text.clone()).into_any_element(),
                                },
                            ))
                    }))
                    .into_any_element(),
                Piece::Rich(document) => render_document(DocumentProps {
                    document,
                    theme,
                    id: gpui::ElementId::Name(format!("{id}-{index}").into()),
                    on_link: open_external_only(),
                    mentions: &HandlesOnly,
                    color: colors.text,
                    select: None,
                }),
                Piece::Plain(text) => div()
                    .text_size(theme.type_scale.body)
                    .text_color(colors.text)
                    .child(text.clone())
                    .into_any_element(),
            }
        }))
        .into_any_element()
}

/// Whether the pieces draw nothing (an empty page).
pub fn is_empty(pieces: &[Piece]) -> bool {
    pieces.is_empty()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn headings_split_the_text() {
        let pieces = parse("# Title\nsome *text*\n\n## Next\nmore");
        assert!(matches!(&pieces[0], Piece::Heading { level: 1, text } if text == "Title"));
        assert!(matches!(&pieces[1], Piece::Rich(_)));
        assert!(matches!(&pieces[2], Piece::Heading { level: 2, text } if text == "Next"));
        assert!(matches!(&pieces[3], Piece::Rich(_)));
    }

    #[test]
    fn a_hash_without_a_space_is_text() {
        let pieces = parse("#tag and more");
        assert!(matches!(&pieces[..], [Piece::Rich(_)]));
        assert_eq!(heading("####### seven"), None);
        assert_eq!(heading("#"), None);
    }

    #[test]
    fn a_fenced_hash_line_is_code_not_a_heading() {
        let pieces = parse("```\n# not a heading\n```");
        assert_eq!(pieces.len(), 1);
        assert!(matches!(&pieces[0], Piece::Rich(_)));
    }

    #[test]
    fn task_items_become_one_list_and_stay_checked_or_not() {
        let pieces = parse(
            "intro
- [ ] one *a*
- [x] two

* [X] three
after",
        );
        assert!(matches!(&pieces[0], Piece::Rich(_)));
        let Piece::Tasks(tasks) = &pieces[1] else {
            panic!("a task list, got {pieces:?}");
        };
        assert_eq!(
            tasks
                .iter()
                .map(|t| (t.checked, t.text.as_str()))
                .collect::<Vec<_>>(),
            [(false, "one *a*"), (true, "two"), (true, "three")]
        );
        assert!(tasks[0].document.is_some());
        assert!(matches!(&pieces[2], Piece::Rich(_)));
        assert_eq!(pieces.len(), 3);
    }

    #[test]
    fn only_a_checkbox_followed_by_a_space_is_a_task() {
        for plain in ["- [y] a", "- [ ]a", "-[ ] a", "    - [ ] a", "[ ] a"] {
            assert!(
                parse(plain).iter().all(|p| matches!(p, Piece::Rich(_))),
                "{plain:?}"
            );
        }
        assert!(matches!(&parse("- [ ]")[..], [Piece::Tasks(t)] if t[0].text.is_empty()));
    }

    #[test]
    fn a_rule_is_three_dashes_on_a_line_of_its_own() {
        let pieces = parse(
            "a
---
b
-----",
        );
        assert!(matches!(
            &pieces[..],
            [Piece::Rich(_), Piece::Rule, Piece::Rich(_), Piece::Rule]
        ));
        assert!(matches!(&parse("--")[..], [Piece::Rich(_)]));
    }

    #[test]
    fn a_fenced_rule_or_task_is_code() {
        let pieces = parse(
            "```
---
- [ ] x
```",
        );
        assert!(matches!(&pieces[..], [Piece::Rich(_)]));
    }

    #[test]
    fn the_editor_and_the_preview_agree_on_what_a_block_is() {
        use crate::text_input::live::{self, Block};
        for (line, block) in [
            ("# a", Block::Heading(1)),
            ("---", Block::Rule),
            ("- [ ] a", Block::Task { checked: false }),
            ("- [x] a", Block::Task { checked: true }),
            ("- [y] a", Block::Bullet),
            ("- [ ]a", Block::Bullet),
            ("    - [ ] a", Block::Plain),
            ("####### a", Block::Plain),
        ] {
            let styled = live::analyze(line)[0].block;
            assert_eq!(styled, block, "{line:?}");
            let previewed = parse(line);
            match styled {
                Block::Heading(_) => assert!(matches!(&previewed[..], [Piece::Heading { .. }])),
                Block::Rule => assert_eq!(previewed, [Piece::Rule]),
                Block::Task { checked } => {
                    assert!(matches!(&previewed[..], [Piece::Tasks(t)] if t[0].checked == checked))
                }
                _ => assert!(
                    matches!(&previewed[..], [Piece::Rich(_)]),
                    "{line:?} is not a block of its own: {previewed:?}"
                ),
            }
        }
    }

    #[test]
    fn an_empty_page_has_no_pieces() {
        assert!(is_empty(&parse("")));
        assert!(is_empty(&parse("\n\n  \n")));
    }

    #[test]
    fn a_very_long_page_is_still_drawn() {
        let line = "word ".repeat(40);
        let text = (0..400)
            .map(|_| line.as_str())
            .collect::<Vec<_>>()
            .join("\n\n");
        let pieces = parse(&text);
        assert!(pieces.len() >= 2);
        assert!(pieces.iter().all(|p| matches!(p, Piece::Rich(_))));
    }
}
