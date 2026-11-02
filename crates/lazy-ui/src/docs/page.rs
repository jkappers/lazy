//! The open page or board: one bar of controls, the title and text, banners, files, and the pane
//! beside it.

use gpui::{
    AnyElement, Context, Entity, FontWeight, IntoElement, ParentElement, PathPromptOptions, Role,
    Styled, Window, div, prelude::*, px,
};
use jiff::Zoned;
use lazy_client_core::{ConflictChoice, DocsInput, FileSource, LoadStatus, SaveState};
use lazy_domain::page::PageContent;
use lazy_domain::{AttachmentId, PageKind};
use lazy_protocol::{Attachment, PageDetail, PageVersionDetail};

use super::list::{bar, display_title};
use super::{DocsEvent, DocsView, HEADER_HEIGHT, MenuKind, Pane, text};
use crate::form::ButtonKind;
use crate::inbox_view::note_block;
use crate::menu::MenuItem;
use crate::text_input::{TextInput, TextInputOptions, TextInputStyle};
use crate::theme::Theme;
use crate::time_fmt::{ago_label, to_zoned};

/// The text field the body of a page is typed in. Every page's body is made here, so how it
/// looks and what it does as you type is one place's business.
pub(crate) fn body_editor(cx: &mut Context<DocsView>) -> Entity<TextInput> {
    cx.new(|cx| {
        TextInput::new(
            TextInputOptions::new(text::BODY_PLACEHOLDER)
                .accessible_label("Page text")
                .live_markdown(true)
                .max_lines(4_000)
                .max_bytes(lazy_domain::page::PAGE_BODY_MAX_BYTES),
            cx,
        )
    })
}

/// Size of a page's title, which is the first line of the page; a board's is ordinary text in the
/// bar.
const PAGE_TITLE_SIZE: f32 = 28.0;
const PAGE_TITLE_LINE: f32 = 36.0;

/// Width of the pane beside a page.
pub const PANE_WIDTH: f32 = 340.0;
/// Widest the text of a page is drawn, so lines stay readable on a wide window.
const TEXT_WIDTH: f32 = 820.0;

/// What the screen needs from the core, copied out so drawing can borrow the view.
struct Facts {
    load: LoadStatus,
    base: Option<PageDetail>,
    save: SaveState,
    has_edits: bool,
    newer: Option<u32>,
    gone: bool,
    trashed: bool,
    preview: Option<PageVersionDetail>,
    working: Option<PageContent>,
    open_notes: u32,
}

impl DocsView {
    fn facts(&self, cx: &gpui::App) -> Option<Facts> {
        self.model.read(cx).core().open_page().map(|open| Facts {
            load: open.load.clone(),
            base: open.base.clone(),
            save: open.save.clone(),
            has_edits: open.has_edits(),
            newer: open.newer_revision,
            gone: open.gone,
            trashed: open.is_trashed(),
            preview: open.versions.preview.clone(),
            working: open.content.clone(),
            open_notes: open
                .notations
                .iter()
                .filter(|notation| notation.completed_at.is_none())
                .count() as u32,
        })
    }

    pub(crate) fn render_page(
        &mut self,
        theme: &Theme,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let Some(facts) = self.facts(cx) else {
            return note_block(
                theme,
                "docs-page-missing",
                None,
                text::GONE_BODY.to_owned(),
                None,
            );
        };
        let Some(base) = facts.base.clone() else {
            return self.page_unavailable(theme, &facts, cx);
        };
        let kind = base.summary.kind;
        let editable = self.page_editable(cx);
        self.style_title(theme, kind, cx);
        let header = self.page_header(theme, &facts, &base, editable, cx);
        let tools = (kind == PageKind::Board && facts.preview.is_none()).then(|| {
            let inner = self.board_tools(theme, editable, cx);
            div()
                .flex_none()
                .px_4()
                .py_1p5()
                .border_b_1()
                .border_color(theme.colors.border)
                .child(inner)
        });
        let mut banners = self.banners(theme, cx);
        banners.extend(self.page_banners(theme, &facts, &base, cx));
        let main = if let Some(version) = &facts.preview {
            self.version_main(theme, version, cx)
        } else if kind == PageKind::Board {
            self.render_board(theme, facts.working.as_ref(), editable, window, cx)
        } else {
            self.editor_main(theme, window, cx)
        };
        let pane = match self.pane {
            Pane::None => None,
            Pane::History => Some(self.history_pane(theme, cx)),
            Pane::Notes => Some(self.notes_pane(theme, &base, editable, cx)),
        };
        let files = self.files_strip(theme, &base, cx);
        div()
            .id("docs-page-screen")
            .debug_selector(|| "docs-page-screen".into())
            .size_full()
            .flex()
            .flex_col()
            .child(header)
            .children(tools)
            .children(banners)
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .flex()
                    .child(div().flex_1().min_w_0().h_full().child(main))
                    .children(pane),
            )
            .children(files)
            .into_any_element()
    }

    /// The title is the first line of a page, large, and an ordinary line of the bar on a board.
    fn style_title(&mut self, theme: &Theme, kind: PageKind, cx: &mut Context<Self>) {
        let mut style = TextInputStyle::from_theme(theme);
        if kind == PageKind::Page {
            style.font_size = px(PAGE_TITLE_SIZE);
            style.line_height = px(PAGE_TITLE_LINE);
        }
        self.title
            .update(cx, |title, cx| title.set_style(style, cx));
    }

    fn page_unavailable(
        &mut self,
        theme: &Theme,
        facts: &Facts,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let back = self.button(
            theme,
            "docs-page-back",
            text::DOCS_TITLE,
            ButtonKind::Quiet,
            true,
            cx,
            |view, window, cx| view.close_page(window, cx),
        );
        let inner = match &facts.load {
            LoadStatus::Failed { reason, can_retry } => {
                let retry = can_retry.then(|| {
                    self.button(
                        theme,
                        "docs-page-retry",
                        crate::states::TRY_AGAIN,
                        ButtonKind::Primary,
                        true,
                        cx,
                        |view, _, cx| view.docs(DocsInput::ReloadPage, cx),
                    )
                });
                note_block(
                    theme,
                    "docs-page-failed",
                    Some(text::PAGE_FAILED_TITLE),
                    crate::states::sentence(reason),
                    retry,
                )
            }
            _ if facts.gone => note_block(
                theme,
                "docs-page-gone",
                Some(text::GONE_TITLE),
                text::GONE_BODY.to_owned(),
                None,
            ),
            _ => div()
                .id("docs-page-loading")
                .debug_selector(|| "docs-page-loading".into())
                .role(Role::Status)
                .aria_label("Loading the page")
                .flex_1()
                .p_6()
                .text_color(theme.colors.text_muted)
                .child(text::LOADING)
                .into_any_element(),
        };
        div()
            .id("docs-page-screen")
            .debug_selector(|| "docs-page-screen".into())
            .size_full()
            .flex()
            .flex_col()
            .child(
                div()
                    .flex_none()
                    .h(px(52.))
                    .px_4()
                    .flex()
                    .items_center()
                    .border_b_1()
                    .border_color(theme.colors.border)
                    .child(back),
            )
            .child(inner)
            .into_any_element()
    }

    /// The one bar above a page: where it is (back to the docs, and the conversation it lives
    /// in), how it is saved, and the few things you do to it. Everything else is under "…".
    fn page_header(
        &mut self,
        theme: &Theme,
        facts: &Facts,
        base: &PageDetail,
        editable: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let colors = theme.colors;
        let kind = base.summary.kind;
        let page = base.summary.id;
        let conversation = base.summary.conversation_id;
        let previewing = facts.preview.is_some();
        let can_post = self.can_post(conversation, cx);
        let place = {
            let core = self.model.read(cx).core();
            let title = core.conversation_title(conversation);
            match core.conversation(conversation).map(|c| c.kind) {
                Some(lazy_domain::ConversationKind::Channel) => format!("#{title}"),
                _ => title,
            }
        };
        let back = self.button(
            theme,
            "docs-page-back",
            text::DOCS_TITLE,
            ButtonKind::Quiet,
            true,
            cx,
            |view, window, cx| view.close_page(window, cx),
        );
        let place = self.button(
            theme,
            "docs-page-place",
            place,
            ButtonKind::Quiet,
            can_post && !previewing && !facts.trashed,
            cx,
            move |_, _, cx| {
                cx.emit(DocsEvent::ChoosePlace {
                    page,
                    from: conversation,
                })
            },
        );
        let (status, status_color) = match &facts.save {
            // Saved is the ordinary state and says nothing.
            SaveState::Saved if can_post => ("", colors.text_muted),
            SaveState::Saved => (text::VIEW_ONLY, colors.text_muted),
            SaveState::Dirty => (text::UNSAVED, colors.text_muted),
            SaveState::Saving => (text::SAVING, colors.text_muted),
            SaveState::Failed(_) => (text::SAVE_RETRYING, colors.danger),
            SaveState::Conflict => (text::SAVE_CONFLICT, colors.danger),
        };
        let mut bar = div()
            .flex_none()
            .h(px(HEADER_HEIGHT))
            .px_4()
            .flex()
            .items_center()
            .gap_1()
            .border_b_1()
            .border_color(colors.border)
            .child(back)
            .child(div().flex_none().text_color(colors.text_faint).child("/"))
            .child(place);
        // A board has no big first line, so its title sits in the bar.
        bar = if kind == PageKind::Board {
            bar.child(
                div()
                    .flex_1()
                    .min_w_0()
                    .mx_2()
                    .px_2()
                    .py_0p5()
                    .rounded(theme.radii.medium)
                    .border_1()
                    .border_color(gpui::transparent_black())
                    .hover(move |s| s.border_color(colors.border_strong))
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(self.title.clone()),
            )
        } else {
            bar.child(div().flex_1())
        };
        let notes = (kind == PageKind::Page).then(|| {
            self.button(
                theme,
                "docs-notes-toggle",
                text::open_notes_label(facts.open_notes),
                ButtonKind::Quiet,
                true,
                cx,
                |view, window, cx| view.toggle_pane(Pane::Notes, window, cx),
            )
            .into_any_element()
        });
        let notes = notes.map(|button| {
            div()
                .when(self.pane == Pane::Notes, |d| {
                    d.rounded(theme.radii.medium).bg(colors.selection)
                })
                .child(button)
                .into_any_element()
        });
        let history = self.button(
            theme,
            "docs-history-toggle",
            text::HISTORY,
            ButtonKind::Quiet,
            true,
            cx,
            |view, window, cx| view.toggle_pane(Pane::History, window, cx),
        );
        let history = div()
            .when(self.pane == Pane::History, |d| {
                d.rounded(theme.radii.medium).bg(colors.selection)
            })
            .child(history)
            .into_any_element();
        let more = self.icon_button(
            theme,
            "docs-more",
            "•••",
            text::MORE_LABEL,
            !previewing,
            cx,
            move |view, window, cx| view.open_more_menu(editable, window, cx),
        );
        let more = self.with_menu(MenuKind::More, more, true);
        bar.child(
            div()
                .id("docs-save-status")
                .debug_selector(|| "docs-save-status".into())
                .role(Role::Status)
                .flex_none()
                .mr_2()
                .text_size(theme.type_scale.small)
                .text_color(status_color)
                .child(status),
        )
        .children(notes)
        .child(history)
        .child(more)
        .into_any_element()
    }

    /// The actions of the open page that are not worth a button of their own.
    pub(crate) fn open_more_menu(
        &mut self,
        editable: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(open) = self.model.read(cx).core().open_page() else {
            return;
        };
        let Some(base) = open.base.as_ref() else {
            return;
        };
        let is_page = base.summary.kind == PageKind::Page;
        let trashed = open.is_trashed();
        let can_post = self.can_post(base.summary.conversation_id, cx);
        let mut items = Vec::new();
        if is_page {
            items.push(
                MenuItem::new("add-note", text::ADD_NOTE)
                    .enabled(editable && self.has_selection(cx)),
            );
        }
        items.push(MenuItem::new("attach", text::ATTACH_FILE).enabled(editable));
        if !trashed {
            items.push(MenuItem::new("move", text::MOVE).enabled(can_post));
            items.push(MenuItem::new("trash", text::MOVE_TO_TRASH).enabled(can_post));
        }
        self.open_menu(MenuKind::More, "docs", items, window, cx);
    }

    /// "Move…": asks the shell for the conversation to move the open page to.
    pub(crate) fn choose_place_for_open_page(&mut self, cx: &mut Context<Self>) {
        let Some(base) = self
            .model
            .read(cx)
            .core()
            .open_page()
            .and_then(|open| open.base.as_ref())
        else {
            return;
        };
        cx.emit(DocsEvent::ChoosePlace {
            page: base.summary.id,
            from: base.summary.conversation_id,
        });
    }

    /// Moves the open page to the trash and goes back to the list.
    pub(crate) fn trash_open_page(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(page) = self
            .model
            .read(cx)
            .core()
            .open_page()
            .and_then(|open| open.base.as_ref())
            .map(|base| base.summary.id)
        else {
            return;
        };
        self.docs(DocsInput::TrashPage(page), cx);
        self.close_page(window, cx);
    }

    fn page_banners(
        &mut self,
        theme: &Theme,
        facts: &Facts,
        base: &PageDetail,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let colors = theme.colors;
        let mut out: Vec<AnyElement> = Vec::new();
        let page = base.summary.id;
        let conversation = base.summary.conversation_id;
        if facts.gone {
            out.push(
                bar(theme, "docs-gone", colors.danger_surface, colors.danger)
                    .child(div().flex_1().child(format!(
                        "{} {}",
                        text::GONE_TITLE,
                        text::GONE_BODY
                    )))
                    .into_any_element(),
            );
        }
        if matches!(facts.save, SaveState::Conflict) {
            let mine = self.button(
                theme,
                "docs-keep-mine",
                text::KEEP_MINE,
                ButtonKind::Primary,
                true,
                cx,
                |view, _, cx| view.docs(DocsInput::ResolveConflict(ConflictChoice::KeepMine), cx),
            );
            let theirs = self.button(
                theme,
                "docs-take-theirs",
                text::TAKE_THEIRS,
                ButtonKind::Secondary,
                true,
                cx,
                |view, _, cx| view.docs(DocsInput::ResolveConflict(ConflictChoice::TakeTheirs), cx),
            );
            out.push(
                bar(theme, "docs-conflict", colors.danger_surface, colors.text)
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .child(
                                div()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .text_color(colors.danger)
                                    .child(text::CONFLICT_TITLE),
                            )
                            .child(
                                div()
                                    .text_size(theme.type_scale.small)
                                    .text_color(colors.text_muted)
                                    .child(text::CONFLICT_BODY),
                            ),
                    )
                    .child(mine)
                    .child(theirs)
                    .into_any_element(),
            );
        } else if facts.newer.is_some() && facts.has_edits {
            let load = self.button(
                theme,
                "docs-load-theirs",
                text::LOAD_THEIRS,
                ButtonKind::Secondary,
                true,
                cx,
                |view, _, cx| view.docs(DocsInput::ReloadPage, cx),
            );
            out.push(
                bar(theme, "docs-newer", colors.warning_surface, colors.text)
                    .child(div().flex_1().child(format!(
                        "{} {}",
                        text::NEWER_TITLE,
                        text::NEWER_BODY
                    )))
                    .child(load)
                    .into_any_element(),
            );
        }
        if facts.trashed {
            let restore = self.button(
                theme,
                "docs-page-restore",
                text::RESTORE,
                ButtonKind::Secondary,
                self.can_post(conversation, cx),
                cx,
                move |view, _, cx| view.docs(DocsInput::RestorePage(page), cx),
            );
            let title = display_title(&base.summary.title);
            let purge = self.button(
                theme,
                "docs-page-purge",
                text::DELETE_FOREVER,
                ButtonKind::Danger,
                self.can_post(conversation, cx),
                cx,
                move |_, _, cx| {
                    cx.emit(DocsEvent::ConfirmPurge {
                        page,
                        title: title.clone(),
                    })
                },
            );
            let days = base
                .summary
                .purge_at
                .map(|at| {
                    text::days_until_purge(text::days_left(at, lazy_domain::Timestamp::now()))
                })
                .unwrap_or_default();
            out.push(
                bar(theme, "docs-trashed", colors.warning_surface, colors.text)
                    .child(
                        div()
                            .flex_1()
                            .child(format!("{} {days}.", text::TRASHED_NOTE)),
                    )
                    .child(restore)
                    .child(purge)
                    .into_any_element(),
            );
        } else if !self.can_post(conversation, cx) && !facts.gone {
            out.push(
                bar(
                    theme,
                    "docs-read-only",
                    colors.surface_hover,
                    colors.text_muted,
                )
                .child(text::READ_ONLY_NOTE)
                .into_any_element(),
            );
        }
        out
    }

    /// A page is a document, not a form: the title is its first line and the text runs under it,
    /// in a column that stays readable on a wide window. Nothing is boxed.
    fn editor_main(
        &mut self,
        _theme: &Theme,
        _window: &Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let editor = self.editor.clone();
        let focus = self.editor.read(cx).focus_handle.clone();
        div()
            .id("docs-editor-scroll")
            .size_full()
            .overflow_y_scroll()
            .flex()
            .justify_center()
            .px_6()
            .pt_8()
            .pb_6()
            .child(
                div()
                    .w_full()
                    .max_w(px(TEXT_WIDTH))
                    .min_h_full()
                    .flex()
                    .flex_col()
                    .gap_3()
                    .child(
                        div()
                            .id("docs-title")
                            .debug_selector(|| "docs-title".into())
                            .flex_none()
                            .font_weight(FontWeight::BOLD)
                            .child(self.title.clone()),
                    )
                    .child(
                        div()
                            .id("docs-editor")
                            .debug_selector(|| "docs-editor".into())
                            .flex_1()
                            .w_full()
                            .min_h(px(240.))
                            .cursor_text()
                            .on_click(move |_, window, cx| window.focus(&focus, cx))
                            .child(editor),
                    ),
            )
            .into_any_element()
    }

    /// The pieces of `body`, parsed again only when the text changed.
    fn pieces_view(&mut self, theme: &Theme, body: &str, selector: &'static str) -> AnyElement {
        if self.pieces.as_ref().is_none_or(|(text, _)| text != body) {
            self.pieces = Some((body.to_owned(), super::preview::parse(body)));
        }
        let pieces = &self.pieces.as_ref().expect("just set").1;
        let inner = if super::preview::is_empty(pieces) {
            div()
                .text_color(theme.colors.text_faint)
                .child("Nothing to show yet.")
                .into_any_element()
        } else {
            super::preview::render(pieces, theme, selector)
        };
        div()
            .id(selector)
            .debug_selector(move || selector.into())
            .size_full()
            .overflow_y_scroll()
            .flex()
            .justify_center()
            .px_4()
            .py_4()
            .child(div().w_full().max_w(px(TEXT_WIDTH)).px_4().child(inner))
            .into_any_element()
    }

    /// An earlier version, read-only, with what restoring it does.
    fn version_main(
        &mut self,
        theme: &Theme,
        version: &PageVersionDetail,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let colors = theme.colors;
        let number = version.summary.version;
        let who = version
            .summary
            .created_by
            .map(|user| self.model.read(cx).core().display_name(user).to_owned());
        let when = ago_label(
            &to_zoned(version.summary.created_at, &self.zone),
            &Zoned::now(),
        );
        let waiting = self
            .model
            .read(cx)
            .core()
            .open_page()
            .is_some_and(|open| open.has_edits() || open.versions.restoring);
        let restore = self.button(
            theme,
            "docs-version-restore",
            text::RESTORE_VERSION,
            ButtonKind::Primary,
            !waiting,
            cx,
            move |view, _, cx| view.docs(DocsInput::RestoreVersion(number), cx),
        );
        let back = self.button(
            theme,
            "docs-version-back",
            text::BACK_TO_CURRENT,
            ButtonKind::Secondary,
            true,
            cx,
            |view, _, cx| view.docs(DocsInput::ClosePreview, cx),
        );
        let content = match &version.content {
            PageContent::Page { body } => {
                let body = body.clone();
                self.pieces_view(theme, &body, "docs-version-text")
            }
            PageContent::Board { board } => self.read_only_board(theme, board),
        };
        let banner = bar(
            theme,
            "docs-version-banner",
            colors.accent_surface,
            colors.text,
        )
        .flex_wrap()
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .child(div().font_weight(FontWeight::SEMIBOLD).child(format!(
                    "{}{} · {when}",
                    text::version_label(number, version.summary.reason),
                    who.map(|who| format!(" · {who}")).unwrap_or_default()
                )))
                .child(
                    div()
                        .id("docs-version-note")
                        .debug_selector(|| "docs-version-note".into())
                        .text_size(theme.type_scale.small)
                        .text_color(colors.text_muted)
                        .child(if waiting {
                            text::RESTORE_WAIT
                        } else {
                            text::RESTORE_NOTE
                        }),
                ),
        )
        .child(restore)
        .child(back);
        div()
            .size_full()
            .flex()
            .flex_col()
            .child(banner)
            .child(div().flex_1().min_h_0().child(content))
            .into_any_element()
    }

    fn files_strip(
        &mut self,
        theme: &Theme,
        base: &PageDetail,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let colors = theme.colors;
        // A page with no files shows no strip; "Attach a file" is in the … menu.
        if base.files.is_empty() {
            return None;
        }
        let chips: Vec<AnyElement> = base
            .files
            .iter()
            .enumerate()
            .map(|(index, file)| self.file_chip(theme, index, file, cx))
            .collect();
        Some(
            div()
                .id("docs-files")
                .debug_selector(|| "docs-files".into())
                .flex_none()
                .flex()
                .flex_wrap()
                .items_center()
                .gap_2()
                .px_4()
                .py_2()
                .border_t_1()
                .border_color(colors.border)
                .child(
                    div()
                        .text_size(theme.type_scale.small)
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(colors.text_muted)
                        .child(text::FILES),
                )
                .children(chips)
                .into_any_element(),
        )
    }

    fn file_chip(
        &mut self,
        theme: &Theme,
        index: usize,
        file: &Attachment,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let colors = theme.colors;
        let attachment = file.clone();
        let download = self.button(
            theme,
            format!("docs-file-download-{index}"),
            "Download",
            ButtonKind::Quiet,
            true,
            cx,
            move |view, _, cx| {
                crate::downloads::start(
                    view.model.clone(),
                    attachment.clone(),
                    None,
                    |_, _| {},
                    cx,
                );
            },
        );
        div()
            .debug_selector(move || format!("docs-file-{index}"))
            .flex()
            .items_center()
            .gap_2()
            .pl_3()
            .pr_1()
            .py_0p5()
            .rounded(theme.radii.pill)
            .border_1()
            .border_color(colors.border_strong)
            .child(
                div()
                    .max_w(px(220.))
                    .truncate()
                    .text_size(theme.type_scale.body)
                    .text_color(colors.text)
                    .child(file.name.clone()),
            )
            .child(
                div()
                    .text_size(theme.type_scale.small)
                    .text_color(colors.text_muted)
                    .child(text::file_size(file.size)),
            )
            .child(download)
            .into_any_element()
    }

    /// Opens the system file picker and adds what is chosen to the page.
    pub(crate) fn attach_files(&mut self, cx: &mut Context<Self>) {
        let receiver = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: true,
            prompt: Some("Attach".into()),
        });
        cx.spawn(async move |this, cx| {
            if let Ok(Ok(Some(paths))) = receiver.await {
                this.update(cx, |view, cx| view.attach_paths(paths, cx))
                    .ok();
            }
        })
        .detach();
    }

    pub fn attach_paths(&mut self, paths: Vec<std::path::PathBuf>, cx: &mut Context<Self>) {
        for path in paths {
            self.docs(
                DocsInput::AddPageFile {
                    attachment: AttachmentId::generate(),
                    source: FileSource::Path(path),
                },
                cx,
            );
        }
    }

    pub(crate) fn toggle_pane(&mut self, pane: Pane, window: &mut Window, cx: &mut Context<Self>) {
        if self.pane == pane {
            self.pane = Pane::None;
            self.focus_body(window, cx);
        } else {
            self.pane = pane;
            if pane == Pane::History {
                self.docs(DocsInput::LoadVersions, cx);
            }
        }
        cx.notify();
    }

    /// Whether the editor has some text selected.
    pub(crate) fn has_selection(&self, cx: &gpui::App) -> bool {
        let (anchor, head) = self.editor.read(cx).selection();
        anchor != head
    }
}
