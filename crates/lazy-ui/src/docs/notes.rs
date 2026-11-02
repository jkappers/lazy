//! Notations (notes anchored to text): the pane beside a page, writing one, replying, and the
//! "Notes for me" screen.

use gpui::{
    AnyElement, Context, FontWeight, IntoElement, ParentElement, Role, Styled, Window, div,
    prelude::*, px,
};
use jiff::Zoned;
use lazy_client_core::{DocsInput, LoadStatus};
use lazy_domain::{NotationId, NotationReplyId, UserId};
use lazy_protocol::{Notation, NotationScope, PageDetail};

use super::side::pane_frame;
use super::{DocsView, HEADER_HEIGHT, NoteDraft, Pane, text};
use crate::form::{self, ButtonKind};
use crate::inbox_view::note_block;
use crate::people_picker::{PeoplePicker, Person};
use crate::theme::Theme;
use crate::time_fmt::{ago_label, to_zoned};

/// Longest quote shown in a card, in characters.
const QUOTE_SHOWN: usize = 140;

fn shorten(text: &str, max: usize) -> String {
    let flat: String = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() <= max {
        return flat;
    }
    let cut: String = flat.chars().take(max).collect();
    format!("{}…", cut.trim_end())
}

impl DocsView {
    // ----- Writing a note -----

    /// "Add note": starts a draft on the selected text and puts the keyboard in its box.
    pub(crate) fn start_note(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let (anchor, head) = self.editor.read(cx).selection();
        let (start, end) = (anchor.min(head), anchor.max(head));
        if start == end {
            return;
        }
        let (quote, from, to) = {
            let editor = self.editor.read(cx);
            let body = editor.text();
            let (Some(quote), Some(before)) = (body.get(start..end), body.get(..start)) else {
                return;
            };
            (
                quote.to_owned(),
                before.chars().count() as u32,
                (before.chars().count() + quote.chars().count()) as u32,
            )
        };
        let Some(conversation) = self
            .model
            .read(cx)
            .core()
            .open_page()
            .and_then(|open| open.base.as_ref())
            .map(|base| base.summary.conversation_id)
        else {
            return;
        };
        let people = self.assignable(conversation, cx);
        self.assignee_picker =
            Some(cx.new(|cx| PeoplePicker::new(people, 1, "Assign to someone else", cx)));
        self.draft = Some(NoteDraft {
            quote,
            from,
            to,
            assignee: None,
        });
        self.pane = Pane::Notes;
        self.note_input.update(cx, |input, cx| input.clear(cx));
        let focus = self.note_input.read(cx).focus_handle.clone();
        window.focus(&focus, cx);
        cx.notify();
    }

    /// Who a note can be given to: the people of the conversation (or, until they are known,
    /// of the workspace).
    fn assignable(&self, conversation: lazy_domain::ConversationId, cx: &gpui::App) -> Vec<Person> {
        let core = self.model.read(cx).core();
        let me = core.me().map(|user| user.id);
        let members: Option<Vec<UserId>> =
            core.conversation_members(conversation).map(<[_]>::to_vec);
        core.members()
            .into_iter()
            .filter(|user| Some(user.id) != me)
            .filter(|user| members.as_ref().is_none_or(|list| list.contains(&user.id)))
            .map(|user| Person {
                id: user.id,
                name: user.display_name.to_string().into(),
                detail: format!("@{}", user.username).into(),
                status: None,
            })
            .collect()
    }

    pub(crate) fn submit_note(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(draft) = self.draft.clone() else {
            return;
        };
        let body = self.note_input.read(cx).text().trim().to_owned();
        if body.is_empty() {
            return;
        }
        let assignee = self
            .assignee_picker
            .as_ref()
            .and_then(|picker| picker.read(cx).selected().first().copied());
        self.docs(
            DocsInput::CreateNotation {
                notation: NotationId::generate(),
                quote: draft.quote,
                from: draft.from,
                to: draft.to,
                body,
                assignee,
            },
            cx,
        );
        self.draft = None;
        self.assignee_picker = None;
        self.note_input.update(cx, |input, cx| input.clear(cx));
        self.focus_body(window, cx);
        cx.notify();
    }

    pub(crate) fn submit_reply(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        let Some(notation) = self.reply_to else {
            return;
        };
        let body = self.reply_input.read(cx).text().trim().to_owned();
        if body.is_empty() {
            return;
        }
        self.docs(
            DocsInput::ReplyToNotation {
                notation,
                reply: NotationReplyId::generate(),
                body,
            },
            cx,
        );
        self.reply_to = None;
        self.reply_input.update(cx, |input, cx| input.clear(cx));
        cx.notify();
    }

    /// Selects a note's text in the editor.
    fn show_in_text(&mut self, notation: &Notation, window: &mut Window, cx: &mut Context<Self>) {
        let Some((from, to)) = self.model.read(cx).core().notation_anchor(notation) else {
            return;
        };
        let (start, end) = {
            let body = self.editor.read(cx).text();
            let byte = |chars: u32| {
                body.char_indices()
                    .nth(chars as usize)
                    .map_or(body.len(), |(index, _)| index)
            };
            (byte(from), byte(to))
        };
        self.editor
            .update(cx, |input, cx| input.set_selection(start, end, cx));
        let focus = self.editor.read(cx).focus_handle.clone();
        window.focus(&focus, cx);
        cx.notify();
    }

    // ----- The pane -----

    pub(crate) fn notes_pane(
        &mut self,
        theme: &Theme,
        base: &PageDetail,
        editable: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let colors = theme.colors;
        let (notations, load) = self
            .model
            .read(cx)
            .core()
            .open_page()
            .map(|open| (open.notations.clone(), open.notations_load.clone()))
            .unwrap_or((Vec::new(), LoadStatus::Idle));
        let draft = self
            .draft
            .clone()
            .map(|draft| self.draft_card(theme, &draft, cx));
        let cards: Vec<AnyElement> = notations
            .iter()
            .enumerate()
            .map(|(index, notation)| self.notation_card(theme, index, notation, editable, true, cx))
            .collect();
        let _ = base;
        let body = if cards.is_empty() && draft.is_none() {
            match load {
                LoadStatus::Failed { reason, .. } => div()
                    .debug_selector(|| "docs-notes-failed".into())
                    .p_3()
                    .text_color(colors.danger)
                    .child(format!("{}: {reason}", text::NOTES_FAILED_TITLE))
                    .into_any_element(),
                LoadStatus::Loading | LoadStatus::Idle => div()
                    .p_3()
                    .text_color(colors.text_muted)
                    .child(text::LOADING)
                    .into_any_element(),
                _ => div()
                    .debug_selector(|| "docs-notes-empty".into())
                    .p_3()
                    .text_size(theme.type_scale.small)
                    .text_color(colors.text_muted)
                    .child(text::NOTES_PANE_EMPTY)
                    .into_any_element(),
            }
        } else {
            div()
                .id("docs-notes-list")
                .debug_selector(|| "docs-notes-list".into())
                .role(Role::List)
                .aria_label("Notes")
                .flex_1()
                .min_h_0()
                .overflow_y_scroll()
                .p_2()
                .flex()
                .flex_col()
                .gap_2()
                .children(draft)
                .children(cards)
                .into_any_element()
        };
        pane_frame(theme, "docs-notes-pane", text::NOTES, body)
    }

    fn draft_card(
        &mut self,
        theme: &Theme,
        draft: &NoteDraft,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let colors = theme.colors;
        let add = self.button(
            theme,
            "docs-note-submit",
            text::ADD_NOTE,
            ButtonKind::Primary,
            !self.note_input.read(cx).text().trim().is_empty(),
            cx,
            |view, window, cx| view.submit_note(window, cx),
        );
        let cancel = self.button(
            theme,
            "docs-note-cancel",
            "Cancel",
            ButtonKind::Quiet,
            true,
            cx,
            |view, window, cx| {
                view.draft = None;
                view.assignee_picker = None;
                view.focus_body(window, cx);
                cx.notify();
            },
        );
        div()
            .id("docs-note-draft")
            .debug_selector(|| "docs-note-draft".into())
            .flex()
            .flex_col()
            .gap_2()
            .p_3()
            .rounded(theme.radii.medium)
            .border_1()
            .border_color(colors.accent)
            .bg(colors.surface_raised)
            .child(quote_block(theme, &draft.quote))
            .child(
                div()
                    .px_2()
                    .py_1()
                    .rounded(theme.radii.small)
                    .border_1()
                    .border_color(colors.border_strong)
                    .child(self.note_input.clone()),
            )
            .children(self.assignee_picker.clone().map(|picker| {
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(
                        div()
                            .text_size(theme.type_scale.small)
                            .text_color(colors.text_muted)
                            .child("Assigned to you unless you choose someone."),
                    )
                    .child(picker)
            }))
            .child(div().flex().justify_end().gap_2().child(cancel).child(add))
            .into_any_element()
    }

    /// One note with its replies and what can be done to it. `in_page` is false on the notes
    /// screen, where there is no text to show it in.
    pub(crate) fn notation_card(
        &mut self,
        theme: &Theme,
        index: usize,
        notation: &Notation,
        editable: bool,
        in_page: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let colors = theme.colors;
        let (me, author, assignee, anchored) = {
            let core = self.model.read(cx).core();
            (
                core.me().map(|user| user.id),
                core.display_name(notation.author_id).to_owned(),
                core.display_name(notation.assignee_id).to_owned(),
                core.notation_anchor(notation).is_some(),
            )
        };
        let done = notation.completed_at.is_some();
        let id = notation.id;
        let when = ago_label(&to_zoned(notation.created_at, &self.zone), &Zoned::now());
        let involved = me == Some(notation.author_id) || me == Some(notation.assignee_id);
        let mut actions: Vec<AnyElement> = Vec::new();
        if in_page && anchored {
            let shown = notation.clone();
            actions.push(self.button(
                theme,
                format!("docs-note-show-{index}"),
                "Show in text",
                ButtonKind::Quiet,
                true,
                cx,
                move |view, window, cx| view.show_in_text(&shown, window, cx),
            ));
        }
        if !done && editable && in_page {
            actions.push(self.button(
                theme,
                format!("docs-note-reply-{index}"),
                "Reply",
                ButtonKind::Quiet,
                true,
                cx,
                move |view, window, cx| {
                    view.reply_to = Some(id);
                    view.reply_input.update(cx, |input, cx| input.clear(cx));
                    let focus = view.reply_input.read(cx).focus_handle.clone();
                    window.focus(&focus, cx);
                    cx.notify();
                },
            ));
        }
        if involved && (editable || !in_page) {
            actions.push(self.button(
                theme,
                format!("docs-note-done-{index}"),
                if done { "Reopen" } else { "Mark done" },
                ButtonKind::Quiet,
                true,
                cx,
                move |view, _, cx| {
                    view.docs(
                        DocsInput::SetNotationCompleted {
                            notation: id,
                            completed: !done,
                        },
                        cx,
                    )
                },
            ));
        }
        if me == Some(notation.author_id) && (editable || !in_page) {
            actions.push(self.button(
                theme,
                format!("docs-note-delete-{index}"),
                "Delete",
                ButtonKind::Danger,
                true,
                cx,
                move |view, _, cx| view.docs(DocsInput::DeleteNotation(id), cx),
            ));
        }
        let replies: Vec<AnyElement> = notation
            .replies
            .iter()
            .enumerate()
            .map(|(r, reply)| {
                let name = self
                    .model
                    .read(cx)
                    .core()
                    .display_name(reply.author_id)
                    .to_owned();
                let reply_id = reply.id;
                let remove = (me == Some(reply.author_id) && editable && in_page).then(|| {
                    self.button(
                        theme,
                        format!("docs-reply-delete-{index}-{r}"),
                        "Delete",
                        ButtonKind::Quiet,
                        true,
                        cx,
                        move |view, _, cx| {
                            view.docs(
                                DocsInput::DeleteNotationReply {
                                    notation: id,
                                    reply: reply_id,
                                },
                                cx,
                            )
                        },
                    )
                });
                div()
                    .debug_selector(move || format!("docs-reply-{index}-{r}"))
                    .pl_3()
                    .border_l_2()
                    .border_color(colors.border_strong)
                    .flex()
                    .items_start()
                    .gap_2()
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .child(
                                div()
                                    .text_size(theme.type_scale.small)
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .text_color(colors.text)
                                    .child(name),
                            )
                            .child(
                                div()
                                    .text_size(theme.type_scale.body)
                                    .text_color(colors.text)
                                    .child(reply.body.clone()),
                            ),
                    )
                    .children(remove)
                    .into_any_element()
            })
            .collect();
        let replying = self.reply_to == Some(id) && in_page;
        let reply_box = replying.then(|| {
            let send = self.button(
                theme,
                format!("docs-reply-send-{index}"),
                "Reply",
                ButtonKind::Primary,
                !self.reply_input.read(cx).text().trim().is_empty(),
                cx,
                |view, window, cx| view.submit_reply(window, cx),
            );
            let cancel = self.button(
                theme,
                format!("docs-reply-cancel-{index}"),
                "Cancel",
                ButtonKind::Quiet,
                true,
                cx,
                |view, _, cx| {
                    view.reply_to = None;
                    cx.notify();
                },
            );
            div()
                .flex()
                .flex_col()
                .gap_2()
                .child(
                    div()
                        .px_2()
                        .py_1()
                        .rounded(theme.radii.small)
                        .border_1()
                        .border_color(colors.border_strong)
                        .child(self.reply_input.clone()),
                )
                .child(div().flex().justify_end().gap_2().child(cancel).child(send))
        });
        div()
            .id(("docs-note", index))
            .debug_selector(move || format!("docs-note-{index}"))
            .role(Role::ListItem)
            .aria_label(format!("Note by {author}"))
            .flex()
            .flex_col()
            .gap_2()
            .p_3()
            .rounded(theme.radii.medium)
            .border_1()
            .border_color(colors.border)
            .bg(colors.surface_raised)
            .when(done, |card| card.opacity(0.75))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_size(theme.type_scale.small)
                            .text_color(colors.text_muted)
                            .child(format!("{author} · {when} · to {assignee}")),
                    )
                    .children(done.then(|| {
                        div()
                            .debug_selector(move || format!("docs-note-done-badge-{index}"))
                            .px_1p5()
                            .rounded(theme.radii.small)
                            .bg(colors.accent_surface)
                            .text_color(colors.success)
                            .text_size(theme.type_scale.small)
                            .font_weight(FontWeight::SEMIBOLD)
                            .child("Done")
                    })),
            )
            .child(quote_block(theme, &notation.quote))
            .children((in_page && !anchored).then(|| {
                div()
                    .debug_selector(move || format!("docs-note-detached-{index}"))
                    .text_size(theme.type_scale.small)
                    .text_color(colors.warning)
                    .child(text::NOTE_DETACHED)
            }))
            .child(
                div()
                    .text_size(theme.type_scale.body)
                    .text_color(colors.text)
                    .child(notation.body.clone()),
            )
            .children(replies)
            .children(reply_box)
            .children(
                (!actions.is_empty()).then(|| div().flex().flex_wrap().gap_2().children(actions)),
            )
            .into_any_element()
    }

    // ----- Notes for me -----

    pub(crate) fn load_notes(&mut self, cx: &mut Context<Self>) {
        let Some(target) = self.target else { return };
        self.docs(
            DocsInput::LoadNotationInbox {
                workspace: target.workspace,
                scope: self.notes_scope,
                completed: self.notes_completed,
            },
            cx,
        );
    }

    pub(crate) fn render_notes_screen(
        &mut self,
        theme: &Theme,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let colors = theme.colors;
        let (notations, load) = self
            .model
            .read(cx)
            .core()
            .notation_inbox()
            .map(|view| (view.notations.clone(), view.load.clone()))
            .unwrap_or((Vec::new(), LoadStatus::Loading));
        let scope_focus = self.stop("notes-scope", cx);
        let done_focus = self.stop("notes-done", cx);
        let scope_to = cx.entity().downgrade();
        let done_to = scope_to.clone();
        let scope_toggle = form::toggle_group(
            theme,
            "notes-scope",
            "Whose notes",
            &[
                ("Assigned to me", "notes-scope-assigned"),
                ("Written by me", "notes-scope-created"),
            ],
            usize::from(self.notes_scope == NotationScope::Created),
            &scope_focus,
            move |index, _, cx| {
                scope_to
                    .update(cx, |view, cx| {
                        view.notes_scope = if index == 0 {
                            NotationScope::Assigned
                        } else {
                            NotationScope::Created
                        };
                        view.load_notes(cx);
                        cx.notify();
                    })
                    .ok();
            },
        );
        let done_toggle = form::toggle_group(
            theme,
            "notes-done",
            "Open or done",
            &[("Open", "notes-open"), ("Done", "notes-completed")],
            usize::from(self.notes_completed),
            &done_focus,
            move |index, _, cx| {
                done_to
                    .update(cx, |view, cx| {
                        view.notes_completed = index == 1;
                        view.load_notes(cx);
                        cx.notify();
                    })
                    .ok();
            },
        );
        let back = self.button(
            theme,
            "notes-back",
            text::DOCS_TITLE,
            ButtonKind::Quiet,
            true,
            cx,
            |view, window, cx| {
                if let Some(target) = view.target {
                    view.docs(DocsInput::CloseNotationInbox, cx);
                    view.open_list(target.workspace, None, target.home, window, cx);
                }
            },
        );
        let header = div()
            .flex_none()
            .flex()
            .items_center()
            .gap_3()
            .px_4()
            .h(px(HEADER_HEIGHT))
            .border_b_1()
            .border_color(colors.border)
            .child(back)
            .child(
                div()
                    .flex_none()
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_size(theme.type_scale.title)
                    .text_color(colors.text)
                    .child(text::NOTES_TITLE),
            )
            .child(scope_toggle)
            .child(done_toggle);
        let banners = self.banners(theme, cx);
        let body = match (&load, notations.is_empty()) {
            (LoadStatus::Failed { reason, can_retry }, true) => {
                let retry = can_retry.then(|| {
                    self.button(
                        theme,
                        "notes-retry",
                        crate::states::TRY_AGAIN,
                        ButtonKind::Primary,
                        true,
                        cx,
                        |view, _, cx| view.load_notes(cx),
                    )
                });
                note_block(
                    theme,
                    "notes-failed",
                    Some(text::NOTES_FAILED_TITLE),
                    crate::states::sentence(reason),
                    retry,
                )
            }
            (LoadStatus::Loading | LoadStatus::Idle, true) => div()
                .id("notes-loading")
                .debug_selector(|| "notes-loading".into())
                .role(Role::Status)
                .aria_label("Loading notes")
                .flex_1()
                .p_6()
                .text_color(colors.text_muted)
                .child(text::LOADING)
                .into_any_element(),
            (_, true) => note_block(
                theme,
                "notes-empty",
                Some(text::NOTES_EMPTY_TITLE),
                match self.notes_scope {
                    NotationScope::Assigned => text::NOTES_EMPTY_ASSIGNED,
                    NotationScope::Created => text::NOTES_EMPTY_CREATED,
                }
                .to_owned(),
                None,
            ),
            _ => {
                let cards: Vec<AnyElement> = notations
                    .iter()
                    .enumerate()
                    .map(|(index, notation)| self.inbox_row(theme, index, notation, cx))
                    .collect();
                div()
                    .id("notes-list")
                    .debug_selector(|| "notes-list".into())
                    .role(Role::List)
                    .aria_label("Notes")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .flex()
                    .justify_center()
                    .p_4()
                    .child(
                        div()
                            .w_full()
                            .max_w(px(760.))
                            .flex()
                            .flex_col()
                            .gap_3()
                            .children(cards),
                    )
                    .into_any_element()
            }
        };
        div()
            .id("docs-notes-screen")
            .debug_selector(|| "docs-notes-screen".into())
            .size_full()
            .flex()
            .flex_col()
            .child(header)
            .children(banners)
            .child(div().flex_1().min_h_0().flex().flex_col().child(body))
            .into_any_element()
    }

    /// A note on the notes screen: the card, and a way to open its page.
    fn inbox_row(
        &mut self,
        theme: &Theme,
        index: usize,
        notation: &Notation,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let colors = theme.colors;
        let place = self
            .model
            .read(cx)
            .core()
            .conversation_title(notation.conversation_id);
        let page = notation.page_id;
        let open = self.button(
            theme,
            format!("notes-open-{index}"),
            "Open the page",
            ButtonKind::Quiet,
            true,
            cx,
            move |view, window, cx| view.open_page(page, window, cx),
        );
        let card = self.notation_card(theme, index, notation, false, false, cx);
        div()
            .flex()
            .flex_col()
            .gap_1()
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .flex_1()
                            .truncate()
                            .text_size(theme.type_scale.small)
                            .text_color(colors.text_muted)
                            .child(format!("In {place}")),
                    )
                    .child(open),
            )
            .child(card)
            .into_any_element()
    }
}

/// The text a note is on, quoted.
fn quote_block(theme: &Theme, quote: &str) -> AnyElement {
    let colors = theme.colors;
    div()
        .pl_2()
        .border_l_2()
        .border_color(colors.quote_bar)
        .text_size(theme.type_scale.small)
        .text_color(colors.text_muted)
        .child(shorten(quote, QUOTE_SHOWN))
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_long_quote_is_cut_on_one_line() {
        let long = "word ".repeat(100);
        let shown = shorten(&long, QUOTE_SHOWN);
        assert!(shown.chars().count() <= QUOTE_SHOWN + 1);
        assert!(shown.ends_with('…'));
        assert_eq!(shorten("a\n b", 10), "a b");
    }
}
