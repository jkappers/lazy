//! The list screen: the workspace's pages and boards (or one conversation's, or the trash), a
//! search across the workspace, and the banners that belong to docs as a whole.

use gpui::{
    AnyElement, ClipboardItem, Context, FontWeight, IntoElement, ParentElement, Role,
    ScrollStrategy, Styled, Window, div, prelude::*, px, transparent_black, uniform_list,
};
use jiff::Zoned;
use lazy_client_core::{CreateState, DocsInput, LoadStatus};
use lazy_domain::page::PageContent;
use lazy_domain::{ConversationId, ConversationKind, PageId, PageKind};
use lazy_protocol::{PageState, PageSummary};

use super::{DocsEvent, DocsView, HEADER_HEIGHT, MenuKind, Screen, text};
use crate::form::{self, ButtonKind};
use crate::inbox_view::note_block;
use crate::menu::MenuItem;
use crate::theme::Theme;
use crate::time_fmt::{ago_label, to_zoned};

/// The height of a row, so the list never re-measures.
pub const ROW_HEIGHT: f32 = 56.0;
const FALLBACK_PAGE: usize = 8;

/// What the list shows now.
pub(crate) struct Rows {
    pub pages: Vec<PageSummary>,
    pub has_more: bool,
    pub load: LoadStatus,
    pub searching: bool,
}

impl DocsView {
    pub(crate) fn list_rows(&self, cx: &gpui::App) -> Rows {
        let core = self.model.read(cx).core();
        let searching = !self.search.read(cx).text().trim().is_empty();
        let conversation = self.conversation();
        if searching {
            return match core.page_search() {
                Some(view) => Rows {
                    // A search looks at the whole workspace; narrowed to a conversation it shows
                    // only that conversation's hits.
                    pages: view
                        .results
                        .iter()
                        .filter(|page| conversation.is_none_or(|c| page.conversation_id == c))
                        .cloned()
                        .collect(),
                    has_more: view.has_more,
                    load: view.load.clone(),
                    searching,
                },
                None => Rows {
                    pages: Vec::new(),
                    has_more: false,
                    load: LoadStatus::Loading,
                    searching,
                },
            };
        }
        let held = match (conversation, self.target) {
            (Some(id), _) => core.page_list(id, self.state),
            (None, Some(target)) => core.workspace_pages(target.workspace, self.state),
            (None, None) => None,
        };
        match held {
            Some(list) => Rows {
                pages: list.pages.clone(),
                has_more: list.has_more,
                load: list.load.clone(),
                searching,
            },
            None => Rows {
                pages: Vec::new(),
                has_more: false,
                load: LoadStatus::Loading,
                searching,
            },
        }
    }

    pub(crate) fn open_row(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        let page = self.list_rows(cx).pages.get(index).map(|page| page.id);
        if let Some(page) = page {
            self.open_page(page, window, cx);
        }
    }

    fn step(&mut self, delta: isize, cx: &mut Context<Self>) {
        let rows = self.list_rows(cx);
        let count = rows.pages.len();
        if count == 0 {
            return;
        }
        let next = (self.active as isize + delta).clamp(0, count as isize - 1) as usize;
        self.active = next;
        self.list_scroll
            .scroll_to_item(next, ScrollStrategy::Nearest);
        if next + 1 == count && rows.has_more && matches!(rows.load, LoadStatus::Ready) {
            self.load_more(cx);
        }
        cx.notify();
    }

    fn page_rows(&self) -> usize {
        let state = self.list_scroll.0.borrow();
        state
            .last_item_size
            .map(|size| (f32::from(size.item.height) / ROW_HEIGHT).floor() as usize)
            .filter(|rows| *rows > 0)
            .unwrap_or(FALLBACK_PAGE)
    }

    fn load_more(&self, cx: &mut Context<Self>) {
        let Some(target) = self.target else { return };
        if !self.search.read(cx).text().trim().is_empty() {
            self.docs(DocsInput::LoadMorePageSearch, cx);
        } else if let Some(conversation) = target.conversation {
            self.docs(
                DocsInput::LoadMorePages {
                    conversation,
                    state: self.state,
                },
                cx,
            );
        } else {
            self.docs(
                DocsInput::LoadMoreWorkspacePages {
                    workspace: target.workspace,
                    state: self.state,
                },
                cx,
            );
        }
    }

    /// Asks the core for the first page of what the list shows now (its conversation, or the
    /// whole workspace).
    pub(crate) fn load_list(&self, cx: &mut Context<Self>) {
        let Some(target) = self.target else { return };
        match target.conversation {
            Some(conversation) => self.docs(
                DocsInput::ListPages {
                    conversation,
                    state: self.state,
                },
                cx,
            ),
            None => self.docs(
                DocsInput::ListWorkspacePages {
                    workspace: target.workspace,
                    state: self.state,
                },
                cx,
            ),
        }
    }

    pub(crate) fn set_state(&mut self, state: PageState, cx: &mut Context<Self>) {
        if self.state == state {
            return;
        }
        self.state = state;
        self.active = 0;
        self.load_list(cx);
        cx.notify();
    }

    /// Narrows the list to one conversation, or widens it to every page the person can read.
    pub(crate) fn set_place(
        &mut self,
        conversation: Option<ConversationId>,
        cx: &mut Context<Self>,
    ) {
        let Some(target) = &mut self.target else {
            return;
        };
        if target.conversation == conversation {
            return;
        }
        target.conversation = conversation;
        self.active = 0;
        self.load_list(cx);
        cx.notify();
    }

    pub(crate) fn create(&mut self, kind: PageKind, cx: &mut Context<Self>) {
        let Some(conversation) = self.new_place(cx) else {
            return;
        };
        if self.creating.is_some() || self.state != PageState::Live {
            return;
        }
        let page = PageId::generate();
        self.creating = Some(page);
        self.docs(
            DocsInput::CreatePage {
                page,
                conversation,
                kind,
                title: String::new(),
                content: None,
            },
            cx,
        );
        cx.notify();
    }

    // ----- Drawing -----

    pub(crate) fn render_list(
        &mut self,
        theme: &Theme,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let header = self.list_header(theme, window, cx);
        let banners = self.banners(theme, cx);
        let body = self.list_body(theme, cx);
        div()
            .id("docs-list-screen")
            .debug_selector(|| "docs-list-screen".into())
            .size_full()
            .flex()
            .flex_col()
            .on_action(
                cx.listener(|this, _: &crate::text_input::actions::MoveDown, _, cx| {
                    this.step(1, cx)
                }),
            )
            .on_action(
                cx.listener(|this, _: &crate::text_input::actions::MoveUp, _, cx| {
                    this.step(-1, cx)
                }),
            )
            .on_key_down(
                cx.listener(|this, event: &gpui::KeyDownEvent, _window, cx| {
                    if event.keystroke.modifiers.modified() {
                        return;
                    }
                    match event.keystroke.key.as_str() {
                        "pagedown" => this.step(this.page_rows() as isize, cx),
                        "pageup" => this.step(-(this.page_rows() as isize), cx),
                        _ => return,
                    }
                    cx.stop_propagation();
                }),
            )
            .child(header)
            .children(banners)
            .child(div().flex_1().min_h_0().flex().flex_col().child(body))
            .into_any_element()
    }

    fn list_header(
        &mut self,
        theme: &Theme,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let colors = theme.colors;
        let place_label = self.place_label(self.conversation(), cx);
        let can_post = self.new_place(cx).is_some();
        let creating = self.creating.is_some();
        let live = self.state == PageState::Live;
        let focus_toggle = self.stop("docs-state", cx);
        let this = cx.entity().downgrade();
        let toggle = form::toggle_group(
            theme,
            "docs-state",
            "Show pages or the trash",
            &[
                (text::LIVE, "docs-state-live"),
                (text::TRASH, "docs-state-trash"),
            ],
            usize::from(!live),
            &focus_toggle,
            move |index, _, cx| {
                this.update(cx, |view, cx| {
                    view.set_state(
                        if index == 0 {
                            PageState::Live
                        } else {
                            PageState::Trashed
                        },
                        cx,
                    )
                })
                .ok();
            },
        );
        let place = self.button(
            theme,
            "docs-place",
            format!("{place_label} ▾"),
            ButtonKind::Quiet,
            true,
            cx,
            |view, window, cx| view.open_place_menu(window, cx),
        );
        let place = self.with_menu(MenuKind::Place, place, false);
        let new = self.button(
            theme,
            "docs-new",
            "New ▾",
            ButtonKind::Primary,
            can_post && !creating && live,
            cx,
            |view, window, cx| view.open_new_menu(window, cx),
        );
        let new = self.with_menu(MenuKind::New, new, true);
        let notes = self.button(
            theme,
            "docs-open-notes",
            text::NOTES_TITLE,
            ButtonKind::Quiet,
            true,
            cx,
            |view, window, cx| {
                if let Some(target) = view.target {
                    view.open_notes(target.workspace, target.home, window, cx);
                }
            },
        );
        div()
            .flex_none()
            .flex()
            .flex_wrap()
            .items_center()
            .gap_x_3()
            .gap_y_1()
            .px_4()
            .py_1p5()
            .min_h(px(HEADER_HEIGHT))
            .border_b_1()
            .border_color(colors.border)
            .child(
                div()
                    .flex_none()
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_size(theme.type_scale.title)
                    .text_color(colors.text)
                    .child(text::DOCS_TITLE),
            )
            .child(place)
            .child(
                div()
                    .flex_1()
                    .min_w(px(180.))
                    .max_w(px(360.))
                    .px_3()
                    .py_0p5()
                    .rounded(theme.radii.medium)
                    .bg(colors.surface_raised)
                    .border_1()
                    .border_color(if self.search.read(cx).is_focused(window) {
                        colors.focus_ring
                    } else {
                        colors.border_strong
                    })
                    .child(self.search.clone()),
            )
            .child(toggle)
            .child(
                div()
                    .flex_none()
                    .flex()
                    .items_center()
                    .gap_3()
                    .ml_auto()
                    .child(notes)
                    .child(new),
            )
            .into_any_element()
    }

    /// What the conversation chip says: where the list is narrowed to.
    fn place_label(&self, conversation: Option<ConversationId>, cx: &gpui::App) -> String {
        let core = self.model.read(cx).core();
        match conversation {
            None => text::ALL_PLACES.to_owned(),
            Some(id) => place_title(core, id),
        }
    }

    /// A page or a board.
    pub(crate) fn open_new_menu(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.open_menu(
            MenuKind::New,
            "docs",
            vec![
                MenuItem::new("new-page", text::NEW_PAGE),
                MenuItem::new("new-board", text::NEW_BOARD),
            ],
            window,
            cx,
        );
    }

    /// The conversations to narrow the list to, and everything.
    pub(crate) fn open_place_menu(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let items = {
            let core = self.model.read(cx).core();
            let sidebar = core.sidebar();
            let mut items = vec![MenuItem::new("all", text::ALL_PLACES)];
            for id in sidebar
                .favorites
                .iter()
                .chain(&sidebar.channels)
                .chain(&sidebar.direct_and_group)
            {
                items.push(MenuItem::new(id.to_string(), place_title(core, *id)));
            }
            items
        };
        self.open_menu(MenuKind::Place, "docs-place-menu", items, window, cx);
    }

    /// The notice, the recovered edits, and the state of a creation, for any screen.
    pub(crate) fn banners(&mut self, theme: &Theme, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let colors = theme.colors;
        let mut out: Vec<AnyElement> = Vec::new();
        let (notice, recovered, create) = {
            let core = self.model.read(cx).core();
            (
                core.docs_notice().map(str::to_owned),
                core.recovered_edits().to_vec(),
                self.creating
                    .and_then(|page| core.create_state(page).cloned().map(|state| (page, state))),
            )
        };
        if let Some((page, state)) = create {
            match state {
                CreateState::InFlight => out.push(
                    bar(theme, "docs-creating", colors.accent_surface, colors.text)
                        .child("Making it…")
                        .into_any_element(),
                ),
                CreateState::Failed(reason) => {
                    let retry = self.button(
                        theme,
                        "docs-create-retry",
                        crate::states::TRY_AGAIN,
                        ButtonKind::Secondary,
                        true,
                        cx,
                        move |view, _, cx| view.docs(DocsInput::RetryCreate(page), cx),
                    );
                    let dismiss = self.button(
                        theme,
                        "docs-create-dismiss",
                        text::DISMISS,
                        ButtonKind::Secondary,
                        true,
                        cx,
                        |view, _, cx| {
                            view.creating = None;
                            cx.notify();
                        },
                    );
                    out.push(
                        bar(
                            theme,
                            "docs-create-failed",
                            colors.danger_surface,
                            colors.danger,
                        )
                        .child(div().flex_1().child(text::creating_failed(&reason)))
                        .child(retry)
                        .child(dismiss)
                        .into_any_element(),
                    );
                }
            }
        }
        if let Some(notice) = notice {
            let dismiss = self.button(
                theme,
                "docs-notice-dismiss",
                text::DISMISS,
                ButtonKind::Secondary,
                true,
                cx,
                |view, _, cx| view.docs(DocsInput::DismissDocsNotice, cx),
            );
            out.push(
                bar(theme, "docs-notice", colors.warning_surface, colors.text)
                    .child(div().flex_1().child(notice))
                    .child(dismiss)
                    .into_any_element(),
            );
        }
        for (index, edit) in recovered.into_iter().enumerate() {
            let operation = edit.operation;
            let body = match &edit.content {
                Some(PageContent::Page { body }) => Some(body.clone()),
                _ => None,
            };
            let copy = body.map(|body| {
                self.button(
                    theme,
                    format!("docs-recovered-copy-{index}"),
                    text::COPY_TEXT,
                    ButtonKind::Secondary,
                    true,
                    cx,
                    move |_, _, cx| cx.write_to_clipboard(ClipboardItem::new_string(body.clone())),
                )
            });
            let dismiss = self.button(
                theme,
                format!("docs-recovered-dismiss-{index}"),
                text::DISMISS,
                ButtonKind::Secondary,
                true,
                cx,
                move |view, _, cx| view.docs(DocsInput::DismissRecoveredEdit(operation), cx),
            );
            let title = edit.title.clone().unwrap_or_default();
            out.push(
                bar(theme, "docs-recovered", colors.warning_surface, colors.text)
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .child(div().font_weight(FontWeight::SEMIBOLD).child(
                                if title.is_empty() {
                                    text::RECOVERED_TITLE.to_owned()
                                } else {
                                    format!("{}: {title}", text::RECOVERED_TITLE)
                                },
                            ))
                            .child(
                                div()
                                    .text_size(theme.type_scale.small)
                                    .text_color(colors.text_muted)
                                    .child(text::RECOVERED_BODY),
                            ),
                    )
                    .children(copy)
                    .child(dismiss)
                    .into_any_element(),
            );
        }
        out
    }

    fn list_body(&mut self, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let colors = theme.colors;
        let rows = self.list_rows(cx);
        let can_post = self.new_place(cx).is_some();
        let this = cx.entity().downgrade();
        let query = self.search.read(cx).text().to_owned();
        let trash = self.state == PageState::Trashed && !rows.searching;
        match (&rows.load, rows.pages.len()) {
            (LoadStatus::Loading | LoadStatus::Idle, 0) => skeleton(theme),
            (LoadStatus::Failed { reason, can_retry }, 0) => {
                let retry = can_retry.then(|| {
                    self.button(
                        theme,
                        "docs-list-retry",
                        crate::states::TRY_AGAIN,
                        ButtonKind::Primary,
                        true,
                        cx,
                        |view, _, cx| view.load_list(cx),
                    )
                });
                note_block(
                    theme,
                    "docs-list-failed",
                    Some(text::LIST_FAILED_TITLE),
                    text::list_failed(reason),
                    retry,
                )
            }
            (_, 0) if rows.searching => {
                note_block(theme, "docs-no-match", None, text::no_match(&query), None)
            }
            (_, 0) if trash => note_block(
                theme,
                "docs-trash-empty",
                Some(text::TRASH_EMPTY_TITLE),
                text::TRASH_EMPTY_BODY.to_owned(),
                None,
            ),
            (_, 0) => note_block(
                theme,
                "docs-empty",
                Some(text::EMPTY_TITLE),
                if can_post {
                    text::EMPTY_BODY.to_owned()
                } else {
                    text::EMPTY_READ_ONLY.to_owned()
                },
                None,
            ),
            _ => {
                let count = rows.pages.len();
                let list = uniform_list(
                    "docs-items",
                    count,
                    cx.processor(
                        |this,
                         range: std::ops::Range<usize>,
                         _window: &mut Window,
                         cx: &mut Context<Self>| {
                            let theme = *Theme::get(cx);
                            this.rows_for(range, &theme, cx)
                        },
                    ),
                )
                .track_scroll(&self.list_scroll)
                .flex_1()
                .min_h_0();
                let more = (rows.has_more
                    && matches!(rows.load, LoadStatus::Ready | LoadStatus::Idle))
                .then(|| {
                    let to = this.clone();
                    form::button(
                        theme,
                        "docs-show-more",
                        text::SHOW_MORE,
                        ButtonKind::Secondary,
                        true,
                        &self.stop("docs-show-more", cx),
                        move |_, cx| {
                            to.update(cx, |view, cx| view.load_more(cx)).ok();
                        },
                    )
                    .into_any_element()
                });
                let status = match &rows.load {
                    LoadStatus::Loading => Some(
                        div()
                            .debug_selector(|| "docs-loading-more".into())
                            .text_size(theme.type_scale.small)
                            .text_color(colors.text_muted)
                            .child(text::LOADING)
                            .into_any_element(),
                    ),
                    LoadStatus::Failed { reason, .. } => Some(
                        div()
                            .debug_selector(|| "docs-more-failed".into())
                            .text_size(theme.type_scale.small)
                            .text_color(colors.danger)
                            .child(text::list_failed(reason))
                            .into_any_element(),
                    ),
                    _ => None,
                };
                div()
                    .flex_1()
                    .min_h_0()
                    .flex()
                    .flex_col()
                    .child(
                        div()
                            .id("docs-list")
                            .debug_selector(|| "docs-list".into())
                            .role(Role::List)
                            .aria_label(if trash { "Trash" } else { "Pages and boards" })
                            .w_full()
                            .max_w(px(900.))
                            .flex_1()
                            .min_h_0()
                            .flex()
                            .flex_col()
                            .child(list),
                    )
                    .children((more.is_some() || status.is_some()).then(|| {
                        div()
                            .flex_none()
                            .flex()
                            .items_center()
                            .gap_3()
                            .px_4()
                            .py_2()
                            .border_t_1()
                            .border_color(colors.border)
                            .children(status)
                            .children(more)
                    }))
                    .into_any_element()
            }
        }
    }

    fn rows_for(
        &mut self,
        range: std::ops::Range<usize>,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let colors = theme.colors;
        let rows = self.list_rows(cx);
        let trash = self.state == PageState::Trashed && !rows.searching;
        let now = Zoned::now();
        let narrowed = self.conversation();
        let this = cx.entity().downgrade();
        let mut out = Vec::new();
        for index in range {
            let Some(page) = rows.pages.get(index).cloned() else {
                continue;
            };
            let active = self.active == index;
            let can_post_here = self.can_post(page.conversation_id, cx);
            // Where it lives, unless the whole list is that one conversation.
            let place = (narrowed != Some(page.conversation_id))
                .then(|| place_title(self.model.read(cx).core(), page.conversation_id))
                .filter(|place| !place.is_empty() && place != "#");
            let when = ago_label(&to_zoned(page.updated_at, &self.zone), &now);
            let counts = text::counts_line(page.open_notations, page.file_count);
            let purge_note = page.purge_at.map(|at| {
                text::days_until_purge(text::days_left(at, lazy_domain::Timestamp::now()))
            });
            let id = page.id;
            let title = display_title(&page.title);
            let kind = page.kind;
            let to_open = this.clone();
            let hover_to = this.clone();
            let label = format!(
                "{}, {}, {}",
                title,
                if kind == PageKind::Board {
                    "board"
                } else {
                    "page"
                },
                when
            );
            let restore = (trash && can_post_here).then(|| {
                self.button(
                    theme,
                    format!("docs-restore-{index}"),
                    text::RESTORE,
                    ButtonKind::Quiet,
                    true,
                    cx,
                    move |view, _, cx| view.docs(DocsInput::RestorePage(id), cx),
                )
            });
            let purge = (trash && can_post_here).then(|| {
                let title = title.clone();
                self.button(
                    theme,
                    format!("docs-purge-{index}"),
                    text::DELETE_FOREVER,
                    ButtonKind::Quiet,
                    true,
                    cx,
                    move |_, _, cx| {
                        cx.emit(DocsEvent::ConfirmPurge {
                            page: id,
                            title: title.clone(),
                        })
                    },
                )
            });
            let excerpt = if page.excerpt.is_empty() {
                match kind {
                    PageKind::Page => "No text yet".to_owned(),
                    PageKind::Board => "Empty board".to_owned(),
                }
            } else {
                plain_excerpt(&page.excerpt)
            };
            out.push(
                div()
                    .id(("docs-row", index))
                    .debug_selector(move || format!("docs-row-{index}"))
                    .role(Role::ListItem)
                    .aria_label(label)
                    .h(px(ROW_HEIGHT))
                    .w_full()
                    .px_2()
                    .py_px()
                    .rounded(theme.radii.medium)
                    .cursor_pointer()
                    .when(!active, |d| d.hover(move |s| s.bg(colors.surface_hover)))
                    .on_mouse_move(move |_, _, cx| {
                        hover_to
                            .update(cx, |view, cx| {
                                if view.active != index {
                                    view.active = index;
                                    cx.notify();
                                }
                            })
                            .ok();
                    })
                    .on_click(move |_, window, cx| {
                        to_open
                            .update(cx, |view, cx| {
                                view.active = index;
                                view.open_page(id, window, cx);
                            })
                            .ok();
                    })
                    .child(
                        div()
                            .h_full()
                            .w_full()
                            .px_3()
                            .flex()
                            .items_center()
                            .gap_3()
                            .rounded(theme.radii.medium)
                            .border_1()
                            .border_color(if active {
                                colors.focus_ring
                            } else {
                                transparent_black()
                            })
                            .when(active, |d| d.bg(colors.selection))
                            .child(kind_badge(theme, kind))
                            .child(
                                div()
                                    .flex_1()
                                    .min_w(px(120.))
                                    .flex()
                                    .flex_col()
                                    .child(
                                        div()
                                            .truncate()
                                            .text_size(theme.type_scale.body)
                                            .font_weight(FontWeight::SEMIBOLD)
                                            .text_color(colors.text)
                                            .child(title.clone()),
                                    )
                                    .child(
                                        div()
                                            .truncate()
                                            .text_size(theme.type_scale.small)
                                            .text_color(colors.text_muted)
                                            .child(excerpt),
                                    ),
                            )
                            .child(
                                div()
                                    .min_w_0()
                                    .max_w(px(300.))
                                    .truncate()
                                    .text_size(theme.type_scale.small)
                                    .text_color(colors.text_faint)
                                    .child(meta_line(
                                        place.as_deref(),
                                        &when,
                                        counts.as_deref(),
                                        purge_note.as_deref(),
                                    )),
                            )
                            // A click on one of these is not a click on the row under it.
                            .children((restore.is_some() || purge.is_some()).then(|| {
                                div()
                                    .id(("docs-row-actions", index))
                                    .flex_none()
                                    .flex()
                                    .items_center()
                                    .gap_1()
                                    .on_click(|_, _, cx| cx.stop_propagation())
                                    .children(restore)
                                    .children(purge)
                            })),
                    )
                    .into_any_element(),
            );
        }
        out
    }
}

/// A conversation as the docs name it: a channel with its `#`, anyone else by the people in it.
fn place_title(core: &lazy_client_core::ClientCore, id: ConversationId) -> String {
    let title = core.conversation_title(id);
    match core.conversation(id).map(|conversation| conversation.kind) {
        Some(ConversationKind::Channel) => format!("#{title}"),
        _ => title,
    }
}

/// A page's excerpt without the markdown marks that read as noise on one line: heading hashes,
/// list dashes, quote arrows, emphasis stars, and backticks.
pub(crate) fn plain_excerpt(excerpt: &str) -> String {
    excerpt
        .split_whitespace()
        .filter(|word| !word.chars().all(|c| matches!(c, '#' | '-' | '>' | '*')))
        .map(|word| word.replace(['*', '`'], ""))
        .collect::<Vec<_>>()
        .join(" ")
}

/// The title as shown: a page with no title reads "Untitled".
pub(crate) fn display_title(title: &str) -> String {
    if title.trim().is_empty() {
        text::UNTITLED.to_owned()
    } else {
        title.to_owned()
    }
}

/// The right side of a row: where it lives (when the list is not one conversation), when it
/// changed, and what is on it.
fn meta_line(place: Option<&str>, when: &str, counts: Option<&str>, purge: Option<&str>) -> String {
    let mut parts: Vec<String> = Vec::new();
    parts.extend(place.map(str::to_owned));
    parts.push(when.to_owned());
    parts.extend(counts.map(str::to_owned));
    parts.extend(purge.map(str::to_owned));
    parts.join(" · ")
}

#[cfg(test)]
mod meta_tests {
    use super::meta_line;

    #[test]
    fn a_row_says_where_it_lives_only_when_it_is_not_obvious() {
        assert_eq!(meta_line(None, "2h ago", None, None), "2h ago");
        assert_eq!(
            meta_line(
                Some("#general"),
                "2h ago",
                Some("2 notes"),
                Some("5 days left")
            ),
            "#general · 2h ago · 2 notes · 5 days left"
        );
    }
}

/// A small pill saying what a row is, so pages and boards read apart without an icon font.
pub(crate) fn kind_badge(theme: &Theme, kind: PageKind) -> AnyElement {
    let colors = theme.colors;
    let (label, bg, fg) = match kind {
        PageKind::Page => ("Page", colors.accent_surface, colors.accent),
        PageKind::Board => ("Board", colors.warning_surface, colors.warning),
    };
    div()
        .flex_none()
        .w(px(52.))
        .py_0p5()
        .flex()
        .justify_center()
        .rounded(theme.radii.small)
        .bg(bg)
        .text_color(fg)
        .text_size(theme.type_scale.small)
        .font_weight(FontWeight::SEMIBOLD)
        .child(label)
        .into_any_element()
}

/// A full-width strip for a notice.
pub(crate) fn bar(
    theme: &Theme,
    selector: &'static str,
    background: gpui::Hsla,
    color: gpui::Hsla,
) -> gpui::Div {
    div()
        .debug_selector(move || selector.into())
        .flex_none()
        .flex()
        .items_center()
        .gap_3()
        .px_4()
        .py_2()
        .bg(background)
        .text_color(color)
        .text_size(theme.type_scale.body)
        .border_b_1()
        .border_color(theme.colors.border)
}

/// The first page loading: rows the shape of pages, so nothing moves when they arrive.
fn skeleton(theme: &Theme) -> AnyElement {
    let colors = theme.colors;
    div()
        .id("docs-loading")
        .debug_selector(|| "docs-loading".into())
        .role(Role::Status)
        .aria_label("Loading pages")
        .flex_1()
        .min_h_0()
        .overflow_hidden()
        .children((0..7).map(|index| {
            let wide = 180.0 + (index % 3) as f32 * 60.0;
            div()
                .h(px(ROW_HEIGHT))
                .w_full()
                .px_6()
                .flex()
                .items_center()
                .gap_3()
                .child(
                    div()
                        .w(px(52.))
                        .h(px(20.))
                        .rounded(theme.radii.small)
                        .bg(colors.border),
                )
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap_1p5()
                        .child(
                            div()
                                .h(px(12.))
                                .w(px(wide))
                                .rounded(theme.radii.small)
                                .bg(colors.border),
                        )
                        .child(
                            div()
                                .h(px(10.))
                                .w(px(wide + 120.))
                                .max_w_full()
                                .rounded(theme.radii.small)
                                .bg(colors.border),
                        ),
                )
        }))
        .into_any_element()
}

impl DocsView {
    /// For tests: which row is active.
    pub fn active_row(&self) -> usize {
        self.active
    }

    /// For tests: the screen showing the list.
    pub fn showing_list(&self) -> bool {
        self.screen == Screen::List && self.target.is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_excerpt_loses_its_markdown_marks() {
        assert_eq!(
            plain_excerpt("# Launch plan We ship the **docs** - Write `export.rs` > quoted"),
            "Launch plan We ship the docs Write export.rs quoted"
        );
        assert_eq!(plain_excerpt("plain words"), "plain words");
        assert_eq!(plain_excerpt("# "), "");
    }

    #[test]
    fn an_untitled_page_reads_untitled() {
        assert_eq!(display_title("  "), "Untitled");
        assert_eq!(display_title("Plan"), "Plan");
    }
}
