//! The docs views (D30): every page and board the person can read (or one conversation's), one
//! open page or board with its history and notes, the trash, and the person's notes across the
//! workspace.
//!
//! [`DocsView`] stands in the conversation's place, like the people directory and the saved
//! messages do. Which screen it shows is its own state ([`Screen`]); everything about the pages
//! themselves is read from the client core and changed by `Input::Docs`.
//!
//! # Keyboard
//!
//! Every screen is reachable and operable without a pointer. Escape steps back one level: it
//! closes a menu, a side pane, or a version preview, then leaves the open page for the list, then
//! closes docs. The list's search box has the keyboard when it opens; Up, Down, Page Up, and Page
//! Down move the active row and Enter opens it. In a page the tab order is: the breadcrumb, the
//! header buttons, the title, the editor, the side pane, the files. A board is one tab stop for
//! its canvas (see [`board`]) between the header and the files.
//!
//! # Saving
//!
//! Typing changes the core's working copy at once and a save is requested after
//! [`AUTOSAVE_AFTER`] without typing. A save that fails is tried again after [`RETRY_AFTER`]. The
//! core journals before it sends, resolves conflicts, and keeps edits across a restart; this view
//! only asks.

mod board_view;
mod list;
mod notes;
mod page;
mod side;

pub mod board;
pub mod preview;
pub mod text;

use std::collections::HashMap;
use std::time::Duration;

use gpui::{
    AnyElement, Context, Entity, EventEmitter, FocusHandle, Focusable, IntoElement, ParentElement,
    Render, SharedString, Styled, Subscription, Task, UniformListScrollHandle, Window, div,
    prelude::*, px,
};
use lazy_client_core::{Change, DocsInput, Input};
use lazy_domain::{ConversationId, NotationId, PageId, PageKind, WorkspaceId};
use lazy_protocol::{NotationScope, PageState};

use crate::app_model::{AppEvent, AppModel};
use crate::form::{self, ButtonKind};
use crate::menu::{Menu, MenuEvent, MenuItem};
use crate::text_input::{TextInput, TextInputEvent, TextInputOptions};
use crate::theme::Theme;

/// Height of the bar at the top of every docs screen.
pub const HEADER_HEIGHT: f32 = 44.0;
/// How long after the last keystroke a save is requested.
pub const AUTOSAVE_AFTER: Duration = Duration::from_millis(800);
/// How long after a failed save it is tried again.
pub const RETRY_AFTER: Duration = Duration::from_secs(5);

/// What the docs view asks the shell to do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DocsEvent {
    /// Leave docs and give the keyboard back.
    Close,
    /// Ask before deleting a trashed page for good.
    ConfirmPurge { page: PageId, title: String },
    /// Choose the conversation a page moves to.
    ChoosePlace { page: PageId, from: ConversationId },
}

/// Which screen the view shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Screen {
    /// A conversation's pages and boards.
    List,
    /// One open page or board.
    Page,
    /// The person's notes across the workspace.
    Notes,
}

/// The pane beside an open page.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pane {
    None,
    History,
    Notes,
}

/// Where the view was opened.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Target {
    pub workspace: WorkspaceId,
    /// The conversation whose pages are listed; `None` lists every page of the workspace the
    /// person can read.
    pub conversation: Option<ConversationId>,
    /// The conversation that was open when docs were: where a new page goes when the list is not
    /// narrowed to one.
    pub home: Option<ConversationId>,
}

/// Which menu is open.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MenuKind {
    /// "New": a page or a board.
    New,
    /// The conversation the list is narrowed to.
    Place,
    /// The rest of an open page's actions.
    More,
}

/// A note being written: the text it is on, and where.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NoteDraft {
    pub quote: String,
    pub from: u32,
    pub to: u32,
    pub assignee: Option<lazy_domain::UserId>,
}

pub struct DocsView {
    pub(crate) model: Entity<AppModel>,
    pub(crate) target: Option<Target>,
    pub(crate) screen: Screen,
    /// Where an open page returns to: the list, or the notes it was opened from.
    pub(crate) came_from: Screen,
    pub(crate) state: PageState,
    pub(crate) search: Entity<TextInput>,
    pub(crate) active: usize,
    pub(crate) list_scroll: UniformListScrollHandle,
    /// The page whose creation the person asked for and has not yet seen open.
    pub(crate) creating: Option<PageId>,
    pub(crate) title: Entity<TextInput>,
    pub(crate) editor: Entity<TextInput>,
    pub(crate) pane: Pane,
    /// The open menu and what it is for. Its owner draws it under the button that opened it.
    pub(crate) menu: Option<(MenuKind, Entity<Menu>)>,
    menu_subscription: Option<Subscription>,
    pub(crate) pieces: Option<(String, Vec<preview::Piece>)>,
    pub(crate) save_task: Option<Task<()>>,
    pub(crate) retry_task: Option<Task<()>>,
    pub(crate) draft: Option<NoteDraft>,
    pub(crate) note_input: Entity<TextInput>,
    pub(crate) assignee_picker: Option<Entity<crate::people_picker::PeoplePicker>>,
    pub(crate) reply_input: Entity<TextInput>,
    pub(crate) reply_to: Option<NotationId>,
    pub(crate) notes_scope: NotationScope,
    pub(crate) notes_completed: bool,
    pub(crate) board: board::BoardUi,
    stops: HashMap<String, FocusHandle>,
    pub(crate) zone: jiff::tz::TimeZone,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<DocsEvent> for DocsView {}

impl DocsView {
    pub fn new(model: Entity<AppModel>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let search = cx.new(|cx| {
            TextInput::new(
                TextInputOptions::new(text::SEARCH_PLACEHOLDER)
                    .accessible_label("Search pages and boards")
                    .single_line(true)
                    .pass_through_navigation(true)
                    .max_bytes(400),
                cx,
            )
        });
        // The title is large on a page and ordinary on a board, so the open page sets its style
        // (see `page::style_title`) instead of the theme doing it.
        let title = cx.new(|cx| {
            TextInput::new(
                TextInputOptions::new(text::TITLE_PLACEHOLDER)
                    .accessible_label("Title")
                    .single_line(true)
                    .themed(false)
                    .max_bytes(lazy_domain::page::TITLE_MAX_CHARS * 4),
                cx,
            )
        });
        let editor = page::body_editor(cx);
        let note_input = cx.new(|cx| {
            TextInput::new(
                TextInputOptions::new(text::NOTE_PLACEHOLDER)
                    .accessible_label("Note")
                    .max_lines(5)
                    .max_bytes(lazy_domain::page::NOTATION_BODY_MAX_BYTES)
                    .submit_chord(crate::text_input::SubmitChord::ModifierEnter),
                cx,
            )
        });
        let reply_input = cx.new(|cx| {
            TextInput::new(
                TextInputOptions::new(text::REPLY_PLACEHOLDER)
                    .accessible_label("Reply to the note")
                    .max_lines(4)
                    .max_bytes(lazy_domain::page::NOTATION_BODY_MAX_BYTES)
                    .submit_chord(crate::text_input::SubmitChord::ModifierEnter),
                cx,
            )
        });
        let mut subscriptions = Vec::new();
        subscriptions.push(cx.subscribe_in(
            &search,
            window,
            |this, input, event: &TextInputEvent, window, cx| match event {
                TextInputEvent::Changed => {
                    this.active = 0;
                    let text = input.read(cx).text().to_owned();
                    this.search_changed(text, cx);
                }
                TextInputEvent::Submit => this.open_row(this.active, window, cx),
                TextInputEvent::Tab { backwards } => {
                    if *backwards {
                        window.focus_prev(cx);
                    } else {
                        window.focus_next(cx);
                    }
                }
                _ => {}
            },
        ));
        subscriptions.push(cx.subscribe_in(
            &title,
            window,
            |this, input, event: &TextInputEvent, window, cx| match event {
                TextInputEvent::Changed => {
                    let text = input.read(cx).text().to_owned();
                    this.title_edited(text, cx);
                }
                TextInputEvent::Submit => this.focus_body(window, cx),
                TextInputEvent::Tab { backwards } => {
                    if *backwards {
                        window.focus_prev(cx);
                    } else {
                        window.focus_next(cx);
                    }
                }
                _ => {}
            },
        ));
        subscriptions.push(cx.subscribe_in(
            &editor,
            window,
            |this, input, event: &TextInputEvent, _window, cx| {
                if matches!(event, TextInputEvent::Changed) {
                    let text = input.read(cx).text().to_owned();
                    this.body_edited(text, cx);
                }
            },
        ));
        subscriptions.push(cx.subscribe_in(
            &note_input,
            window,
            |this, _input, event: &TextInputEvent, window, cx| {
                if matches!(event, TextInputEvent::Submit) {
                    this.submit_note(window, cx);
                }
            },
        ));
        subscriptions.push(cx.subscribe_in(
            &reply_input,
            window,
            |this, _input, event: &TextInputEvent, window, cx| {
                if matches!(event, TextInputEvent::Submit) {
                    this.submit_reply(window, cx);
                }
            },
        ));
        subscriptions.push(cx.subscribe_in(
            &model,
            window,
            |this, _model, event: &AppEvent, window, cx| {
                let AppEvent::Changed(changes) = event;
                this.apply(changes, window, cx);
            },
        ));
        let board = board::BoardUi::new(window, cx);
        subscriptions.push(cx.subscribe_in(
            board.input(),
            window,
            |this, _input, event: &TextInputEvent, window, cx| match event {
                TextInputEvent::Submit => this.commit_edit(window, cx),
                TextInputEvent::Tab { .. } => this.commit_edit(window, cx),
                _ => {}
            },
        ));
        Self {
            model,
            target: None,
            screen: Screen::List,
            came_from: Screen::List,
            state: PageState::Live,
            search,
            active: 0,
            list_scroll: UniformListScrollHandle::new(),
            creating: None,
            title,
            editor,
            pane: Pane::None,
            menu: None,
            menu_subscription: None,
            pieces: None,
            save_task: None,
            retry_task: None,
            draft: None,
            note_input,
            assignee_picker: None,
            reply_input,
            reply_to: None,
            notes_scope: NotationScope::Assigned,
            notes_completed: false,
            board,
            stops: HashMap::new(),
            zone: jiff::tz::TimeZone::system(),
            _subscriptions: subscriptions,
        }
    }

    // ----- For the shell and tests -----

    /// Whether docs are showing.
    pub fn is_open(&self) -> bool {
        self.target.is_some()
    }

    pub fn screen(&self) -> Screen {
        self.screen
    }

    pub fn pane(&self) -> Pane {
        self.pane
    }

    pub fn target(&self) -> Option<Target> {
        self.target
    }

    /// The conversation whose pages are listed.
    pub fn conversation(&self) -> Option<ConversationId> {
        self.target.and_then(|target| target.conversation)
    }

    pub fn search(&self) -> &Entity<TextInput> {
        &self.search
    }

    pub fn title_input(&self) -> &Entity<TextInput> {
        &self.title
    }

    pub fn editor(&self) -> &Entity<TextInput> {
        &self.editor
    }

    pub fn note_input(&self) -> &Entity<TextInput> {
        &self.note_input
    }

    pub fn reply_input(&self) -> &Entity<TextInput> {
        &self.reply_input
    }

    pub fn list_state(&self) -> PageState {
        self.state
    }

    pub fn board_state(&self) -> &board::BoardUi {
        &self.board
    }

    /// Whether a menu is open, and which.
    pub fn menu_open(&self) -> bool {
        self.menu.is_some()
    }

    /// Opens docs on one conversation's pages, with the keyboard in the search box.
    pub fn open(
        &mut self,
        workspace: WorkspaceId,
        conversation: ConversationId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_list(
            workspace,
            Some(conversation),
            Some(conversation),
            window,
            cx,
        );
    }

    /// Opens docs on every page and board of the workspace the person can read. `home` is the
    /// conversation that was open: a new page goes there unless another is chosen.
    pub fn open_workspace(
        &mut self,
        workspace: WorkspaceId,
        home: Option<ConversationId>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_list(workspace, None, home, window, cx);
    }

    fn open_list(
        &mut self,
        workspace: WorkspaceId,
        conversation: Option<ConversationId>,
        home: Option<ConversationId>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.reset_page(cx);
        self.target = Some(Target {
            workspace,
            conversation,
            home,
        });
        self.screen = Screen::List;
        self.state = PageState::Live;
        self.active = 0;
        self.search.update(cx, |input, cx| input.clear(cx));
        self.docs(
            DocsInput::SearchPages {
                workspace,
                text: String::new(),
            },
            cx,
        );
        self.load_list(cx);
        let focus = self.search.focus_handle(cx);
        window.focus(&focus, cx);
        cx.notify();
    }

    /// Opens the person's notes across the workspace.
    pub fn open_notes(
        &mut self,
        workspace: WorkspaceId,
        conversation: Option<ConversationId>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.reset_page(cx);
        self.target = Some(Target {
            workspace,
            conversation: None,
            home: conversation,
        });
        self.screen = Screen::Notes;
        self.notes_scope = NotationScope::Assigned;
        self.notes_completed = false;
        self.load_notes(cx);
        window.focus(&self.stop("notes-scope", cx), cx);
        cx.notify();
    }

    /// Leaves docs: saves what is open (the core does it in the background) and forgets the
    /// screens.
    pub fn close(&mut self, cx: &mut Context<Self>) {
        if self.target.is_none() {
            return;
        }
        self.reset_page(cx);
        if self.screen == Screen::Notes {
            self.docs(DocsInput::CloseNotationInbox, cx);
        }
        self.target = None;
        cx.notify();
    }

    /// Switches the list to the trash (or back to the pages).
    pub fn show_trash(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        self.set_state(PageState::Trashed, cx);
    }

    /// Opens the "New" menu of the list, as a click on its button does.
    pub fn show_new_menu(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.open_new_menu(window, cx);
    }

    /// Opens the conversation menu of the list.
    pub fn show_place_menu(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.open_place_menu(window, cx);
    }

    /// Opens the "…" menu of the open page.
    pub fn show_more_menu(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let editable = self.page_editable(cx);
        self.open_more_menu(editable, window, cx);
    }

    /// Opens the history pane.
    pub fn show_history(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.pane != Pane::History {
            self.toggle_pane(Pane::History, window, cx);
        }
    }

    /// Opens the notes pane.
    pub fn show_notes(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.pane != Pane::Notes {
            self.toggle_pane(Pane::Notes, window, cx);
        }
    }

    // ----- Plumbing -----

    pub(crate) fn docs(&self, input: DocsInput, cx: &mut Context<Self>) {
        self.model
            .update(cx, |model, cx| model.dispatch(Input::Docs(input), cx));
    }

    /// The tab stop called `name`, made on first use.
    pub(crate) fn stop(&mut self, name: &str, cx: &mut Context<Self>) -> FocusHandle {
        self.stops
            .entry(name.to_owned())
            .or_insert_with(|| cx.focus_handle().tab_stop(true))
            .clone()
    }

    /// A compact button that is a tab stop and runs `act` on this view.
    // Every button on these screens is made here, with the same eight things said about it; a
    // struct for them would be read once and filled in a hundred times.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn button(
        &mut self,
        theme: &Theme,
        id: impl Into<SharedString>,
        label: impl Into<SharedString>,
        kind: ButtonKind,
        enabled: bool,
        cx: &mut Context<Self>,
        act: impl Fn(&mut Self, &mut Window, &mut Context<Self>) + 'static,
    ) -> AnyElement {
        let id: SharedString = id.into();
        let focus = self.stop(&id, cx);
        let this = cx.entity().downgrade();
        form::compact_button(
            theme,
            id,
            label,
            kind,
            enabled,
            &focus,
            move |window, cx| {
                this.update(cx, |view, cx| act(view, window, cx)).ok();
            },
        )
        .into_any_element()
    }

    /// A square glyph button with a tooltip that runs `act` on this view.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn icon_button(
        &mut self,
        theme: &Theme,
        id: impl Into<SharedString>,
        glyph: &'static str,
        label: &'static str,
        enabled: bool,
        cx: &mut Context<Self>,
        act: impl Fn(&mut Self, &mut Window, &mut Context<Self>) + 'static,
    ) -> AnyElement {
        let id: SharedString = id.into();
        let focus = self.stop(&id, cx);
        let this = cx.entity().downgrade();
        form::icon_button(
            theme,
            id,
            glyph,
            label,
            enabled,
            &focus,
            move |window, cx| {
                this.update(cx, |view, cx| act(view, window, cx)).ok();
            },
        )
        .into_any_element()
    }

    /// Opens `items` as a menu of `kind`, which has the keyboard until it is done. Items are
    /// found by `{label}-{id}`.
    pub(crate) fn open_menu(
        &mut self,
        kind: MenuKind,
        label: &'static str,
        items: Vec<MenuItem>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let menu = cx.new(|cx| Menu::new(label, items, cx));
        self.menu_subscription = Some(cx.subscribe_in(
            &menu,
            window,
            move |this, _menu, event: &MenuEvent, window, cx| {
                this.menu = None;
                if let MenuEvent::Select(id) = event {
                    this.menu_chosen(kind, id.as_ref(), window, cx);
                }
                cx.notify();
            },
        ));
        menu.update(cx, |menu, cx| menu.open(window, cx));
        self.menu = Some((kind, menu));
        cx.notify();
    }

    /// `trigger` with the open menu of `kind` (if that is the open one) hanging under it.
    pub(crate) fn with_menu(&self, kind: MenuKind, trigger: AnyElement, right: bool) -> AnyElement {
        let menu = self
            .menu
            .as_ref()
            .filter(|(open, _)| *open == kind)
            .map(|(_, menu)| menu.clone());
        div()
            .relative()
            .flex_none()
            .child(trigger)
            .children(menu.map(|menu| {
                gpui::deferred(
                    div()
                        .absolute()
                        .top(px(34.))
                        .when(right, |d| d.right_0())
                        .when(!right, |d| d.left_0())
                        .child(menu),
                )
            }))
            .into_any_element()
    }

    fn menu_chosen(
        &mut self,
        kind: MenuKind,
        id: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match (kind, id) {
            (MenuKind::New, "new-page") => self.create(PageKind::Page, cx),
            (MenuKind::New, "new-board") => self.create(PageKind::Board, cx),
            (MenuKind::Place, "all") => self.set_place(None, cx),
            (MenuKind::Place, id) => {
                if let Ok(conversation) = id.parse::<ConversationId>() {
                    self.set_place(Some(conversation), cx);
                }
            }
            (MenuKind::More, "add-note") => self.start_note(window, cx),
            (MenuKind::More, "attach") => self.attach_files(cx),
            (MenuKind::More, "move") => self.choose_place_for_open_page(cx),
            (MenuKind::More, "trash") => self.trash_open_page(window, cx),
            _ => {}
        }
    }

    /// The conversation a new page goes to: the one the list is narrowed to, else the one that
    /// was open, else the default channel, `#general`, or the first channel, whichever the
    /// person can write in.
    pub(crate) fn new_place(&self, cx: &gpui::App) -> Option<ConversationId> {
        let core = self.model.read(cx).core();
        let mut candidates: Vec<ConversationId> = Vec::new();
        candidates.extend(self.conversation());
        candidates.extend(self.target.and_then(|target| target.home));
        candidates.extend(
            core.conversations()
                .iter()
                .filter(|c| c.is_default)
                .map(|c| c.id),
        );
        candidates.extend(
            core.conversations()
                .iter()
                .filter(|c| c.name.as_ref().is_some_and(|n| n.as_str() == "general"))
                .map(|c| c.id),
        );
        candidates.extend(core.channels());
        candidates.into_iter().find(|id| self.can_post(*id, cx))
    }

    pub(crate) fn can_post(&self, conversation: ConversationId, cx: &gpui::App) -> bool {
        let core = self.model.read(cx).core();
        core.user_state(conversation).is_some()
            && core
                .conversation(conversation)
                .is_some_and(|conversation| !conversation.archived)
    }

    /// Forgets everything about the open page: the screen state, not the core's copy (which the
    /// core saves and keeps).
    fn reset_page(&mut self, cx: &mut Context<Self>) {
        if self.screen == Screen::Page {
            self.docs(DocsInput::ClosePage, cx);
        }
        self.pane = Pane::None;
        self.menu = None;
        self.menu_subscription = None;
        self.pieces = None;
        self.save_task = None;
        self.retry_task = None;
        self.draft = None;
        self.reply_to = None;
        self.creating = None;
        self.board.reset();
        for input in [
            &self.title,
            &self.editor,
            &self.note_input,
            &self.reply_input,
        ] {
            input.update(cx, |input, cx| input.clear(cx));
        }
    }

    /// The core changed: finish what this view was waiting for, and bring the text fields in line
    /// with the working copy.
    fn apply(&mut self, changes: &[Change], window: &mut Window, cx: &mut Context<Self>) {
        if self.target.is_none() {
            return;
        }
        for change in changes {
            if let Change::PageCreated { page } = change
                && self.creating == Some(*page)
            {
                self.creating = None;
                self.open_page(*page, window, cx);
                // A page just made starts with the keyboard in its title, all of it selected, so
                // typing replaces "Untitled".
                let len = self.title.read(cx).text().len();
                self.title
                    .update(cx, |input, cx| input.set_selection(0, len, cx));
                let focus = self.title.focus_handle(cx);
                window.focus(&focus, cx);
            }
        }
        if self.screen == Screen::Page {
            self.sync_inputs(cx);
            self.retry_if_failed(cx);
            self.board.sync(self.model.read(cx).core().open_page());
        }
        cx.notify();
    }

    // ----- Moving between screens -----

    /// Opens a page or board from a list or a note.
    pub fn open_page(&mut self, page: PageId, window: &mut Window, cx: &mut Context<Self>) {
        if self.screen != Screen::Page {
            self.came_from = self.screen;
        }
        self.reset_page(cx);
        self.screen = Screen::Page;
        self.docs(DocsInput::OpenPage(page), cx);
        self.sync_inputs(cx);
        window.focus(&self.stop("page-back", cx), cx);
        cx.notify();
    }

    /// Back to the list. The core saves unsaved edits in the background.
    pub fn close_page(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.screen != Screen::Page {
            return;
        }
        self.reset_page(cx);
        self.screen = match self.came_from {
            Screen::Notes => Screen::Notes,
            _ => Screen::List,
        };
        if self.screen == Screen::Notes {
            self.load_notes(cx);
        }
        let focus = match self.screen {
            Screen::List => self.search.focus_handle(cx),
            _ => self.stop("notes-scope", cx),
        };
        window.focus(&focus, cx);
        cx.notify();
    }

    /// Escape: one step back.
    pub(crate) fn back(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if crate::text_input::any_input_composing(cx) {
            return;
        }
        match self.screen {
            _ if self.menu.is_some() => {
                // The menu closes itself on Escape and gives the keyboard back; this only makes
                // sure the view lets go of it.
                self.menu = None;
                self.menu_subscription = None;
                cx.notify();
            }
            Screen::Page => {
                if self.board.is_editing() {
                    self.cancel_edit(window, cx);
                } else if self.board.cancel_gesture() {
                    cx.notify();
                } else if self.draft.is_some() {
                    self.draft = None;
                    cx.notify();
                } else if self
                    .model
                    .read(cx)
                    .core()
                    .open_page()
                    .is_some_and(|open| open.versions.preview.is_some())
                {
                    self.docs(DocsInput::ClosePreview, cx);
                } else if self.pane != Pane::None {
                    self.pane = Pane::None;
                    self.focus_body(window, cx);
                    cx.notify();
                } else {
                    self.close_page(window, cx);
                }
            }
            Screen::List => {
                if !self.search.read(cx).is_empty() {
                    self.search.update(cx, |input, cx| input.clear(cx));
                    self.search_changed(String::new(), cx);
                } else {
                    cx.emit(DocsEvent::Close);
                }
            }
            Screen::Notes => cx.emit(DocsEvent::Close),
        }
    }

    /// The keyboard goes to the text (or the board) of the open page.
    pub fn focus_body(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let is_board = self
            .model
            .read(cx)
            .core()
            .open_page()
            .and_then(|open| open.base.as_ref())
            .is_some_and(|base| base.summary.kind == PageKind::Board);
        let focus = if is_board {
            self.board.focus_handle().clone()
        } else {
            self.editor.focus_handle(cx)
        };
        window.focus(&focus, cx);
    }

    // ----- Editing the open page -----

    fn title_edited(&mut self, text: String, cx: &mut Context<Self>) {
        let unchanged = self
            .model
            .read(cx)
            .core()
            .open_page()
            .is_none_or(|open| open.title == text);
        if unchanged {
            return;
        }
        self.docs(
            DocsInput::EditPage {
                title: Some(text),
                content: None,
            },
            cx,
        );
        self.schedule_save(cx);
    }

    fn body_edited(&mut self, text: String, cx: &mut Context<Self>) {
        let unchanged = self.model.read(cx).core().open_page().is_none_or(|open| {
            !matches!(
                &open.content,
                Some(lazy_domain::page::PageContent::Page { body }) if *body != text
            )
        });
        if unchanged {
            return;
        }
        self.docs(
            DocsInput::EditPage {
                title: None,
                content: Some(lazy_domain::page::PageContent::Page { body: text }),
            },
            cx,
        );
        self.schedule_save(cx);
    }

    /// Asks for a save after a pause in editing.
    pub(crate) fn schedule_save(&mut self, cx: &mut Context<Self>) {
        self.save_task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(AUTOSAVE_AFTER).await;
            this.update(cx, |this, cx| {
                this.save_task = None;
                if this
                    .model
                    .read(cx)
                    .core()
                    .open_page()
                    .is_some_and(|open| matches!(open.save, lazy_client_core::SaveState::Dirty))
                {
                    this.docs(DocsInput::SavePage, cx);
                }
            })
            .ok();
        }));
    }

    /// After a failed save, tries again once the pause is over.
    fn retry_if_failed(&mut self, cx: &mut Context<Self>) {
        let failed = self
            .model
            .read(cx)
            .core()
            .open_page()
            .is_some_and(|open| matches!(open.save, lazy_client_core::SaveState::Failed(_)));
        if !failed {
            self.retry_task = None;
            return;
        }
        if self.retry_task.is_some() {
            return;
        }
        self.retry_task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(RETRY_AFTER).await;
            this.update(cx, |this, cx| {
                this.retry_task = None;
                this.docs(DocsInput::SavePage, cx);
            })
            .ok();
        }));
    }

    /// Puts the core's working copy in the text fields when it differs (a page just loaded, their
    /// version taken, a version restored).
    fn sync_inputs(&mut self, cx: &mut Context<Self>) {
        let Some((title, body)) = self.model.read(cx).core().open_page().map(|open| {
            (
                open.title.clone(),
                match &open.content {
                    Some(lazy_domain::page::PageContent::Page { body }) => Some(body.clone()),
                    _ => None,
                },
            )
        }) else {
            return;
        };
        if self.title.read(cx).text() != title && !self.title.read(cx).is_composing() {
            self.title
                .update(cx, |input, cx| input.set_text(&title, cx));
        }
        if let Some(body) = body
            && self.editor.read(cx).text() != body
            && !self.editor.read(cx).is_composing()
        {
            self.editor
                .update(cx, |input, cx| input.set_text(&body, cx));
        }
        let editable = self.page_editable(cx);
        for input in [&self.title, &self.editor] {
            if input.read(cx).is_enabled() != editable {
                input.update(cx, |input, cx| input.set_enabled(editable, cx));
            }
        }
    }

    /// Whether the open page can be changed by this person now.
    pub(crate) fn page_editable(&self, cx: &gpui::App) -> bool {
        let core = self.model.read(cx).core();
        let Some(open) = core.open_page() else {
            return false;
        };
        let Some(base) = &open.base else {
            return false;
        };
        !open.gone
            && !open.is_trashed()
            && open.versions.preview.is_none()
            && self.can_post(base.summary.conversation_id, cx)
    }

    // ----- Search -----

    fn search_changed(&mut self, text: String, cx: &mut Context<Self>) {
        let Some(target) = self.target else { return };
        self.docs(
            DocsInput::SearchPages {
                workspace: target.workspace,
                text,
            },
            cx,
        );
        cx.notify();
    }
}

impl Focusable for DocsView {
    fn focus_handle(&self, cx: &gpui::App) -> FocusHandle {
        match self.screen {
            Screen::List => self.search.focus_handle(cx),
            Screen::Page => self.editor.focus_handle(cx),
            Screen::Notes => self.editor.focus_handle(cx),
        }
    }
}

impl Render for DocsView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = *Theme::get(cx);
        let body = match self.screen {
            Screen::List => self.render_list(&theme, window, cx),
            Screen::Page => self.render_page(&theme, window, cx),
            Screen::Notes => self.render_notes_screen(&theme, window, cx),
        };
        div()
            .id("docs-view")
            .debug_selector(|| "docs-view".into())
            .size_full()
            .flex()
            .flex_col()
            .bg(theme.colors.surface)
            .on_key_down(cx.listener(|this, event: &gpui::KeyDownEvent, window, cx| {
                // Command-Option-M (Control-Alt-M elsewhere) starts a note on the selected text.
                let modifiers = &event.keystroke.modifiers;
                if event.keystroke.key == "m" && modifiers.alt && modifiers.secondary(cx) {
                    if this.screen == Screen::Page
                        && this.page_editable(cx)
                        && this.has_selection(cx)
                    {
                        this.start_note(window, cx);
                        cx.stop_propagation();
                    }
                    return;
                }
                if modifiers.modified() || event.keystroke.key != "escape" {
                    return;
                }
                this.back(window, cx);
                cx.stop_propagation();
            }))
            .child(body)
    }
}
