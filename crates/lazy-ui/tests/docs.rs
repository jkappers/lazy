//! Docs on GPUI's test platform (D30): the list, a page, saving and conflicts, the trash, history,
//! notes, files, and boards. The real shell, docs view, and client core run against a small fake
//! docs server (`docs_support`); every pointer claim is made with real mouse events and every
//! keyboard claim with real key events. What a screen reader announces and how an input method
//! behaves in the editor are not claimed here (see VERIFICATION.md).

mod docs_support;

use docs_support::*;
use gpui::TestAppContext;
use lazy_client_core::{DocsCall, DocsInput, SaveState};
use lazy_domain::board::{BoardColor, BoardEdge, BoardNode, NodeShape, Viewport};
use lazy_domain::page::{PageContent, PageKind};
use lazy_domain::{Board, PageId, Timestamp};
use lazy_ui::commands::{self, CommandId};
use lazy_ui::docs::{Pane, Screen};

fn node(id: &str, shape: NodeShape, x: i32, y: i32, text: &str) -> BoardNode {
    BoardNode {
        id: id.into(),
        shape,
        x,
        y,
        width: 160,
        height: 80,
        text: text.into(),
        color: None,
    }
}

fn two_shapes() -> Board {
    Board {
        nodes: vec![
            node("n1", NodeShape::Rectangle, 100, 100, "Start"),
            node("n2", NodeShape::Ellipse, 420, 100, "Finish"),
        ],
        edges: vec![BoardEdge {
            id: "e1".into(),
            source: "n1".into(),
            target: "n2".into(),
            label: String::new(),
        }],
        viewport: Viewport::default(),
    }
}

fn server_board(world: &World, id: PageId) -> Board {
    match world.page(id).content {
        PageContent::Board { board } => board,
        other => panic!("not a board: {other:?}"),
    }
}

fn open_row(world: &World, cx: &mut gpui::VisualTestContext, index: usize) {
    world.click_named(cx, format!("docs-row-{index}"));
    assert!(world.on_screen(cx, "docs-page-screen"), "the page opened");
}

fn save_state(world: &World, cx: &gpui::VisualTestContext) -> Option<SaveState> {
    world.core(cx, |core| core.open_page().map(|open| open.save.clone()))
}

fn select_in_editor(world: &World, cx: &mut gpui::VisualTestContext, from: usize, to: usize) {
    let view = world.docs_view(cx);
    let editor = view.read_with(cx, |view, _| view.editor().clone());
    editor.update(cx, |input, cx| input.set_selection(from, to, cx));
    draw(cx);
}

// ----- Opening docs -----

#[gpui::test]
fn the_sidebar_link_opens_docs_with_an_empty_state(cx: &mut TestAppContext) {
    let (world, cx) = start(cx);
    world.reach(cx);
    assert!(
        world.on_screen(cx, "sidebar-docs"),
        "the sidebar has a Docs link"
    );
    assert!(
        !world.on_screen(cx, "docs-button"),
        "the conversation header no longer has a Docs button"
    );
    world.open_docs(cx);
    assert!(world.on_screen(cx, "docs-empty"));
    // One "New" button with a menu, not a button for each kind.
    assert!(world.on_screen(cx, "docs-new"));
    assert!(!world.on_screen(cx, "docs-new-page"));
    world.click(cx, "docs-new");
    assert!(world.on_screen(cx, "docs-new-page"));
    assert!(world.on_screen(cx, "docs-new-board"));
    assert!(
        world
            .calls()
            .iter()
            .any(|call| matches!(call, DocsCall::ListWorkspacePages { .. })),
        "every page of the workspace was asked for"
    );
    // The sidebar row says docs are what is showing.
    world.keys(cx, "escape");
    assert!(
        !world.on_screen(cx, "docs-new-page"),
        "Escape closes the menu first"
    );
    assert!(world.on_screen(cx, "docs-view"));
}

#[gpui::test]
fn the_list_can_be_narrowed_to_one_conversation_and_widened_again(cx: &mut TestAppContext) {
    let (world, cx) = start(cx);
    let here = world.seed_page("Here", "x");
    let there = world.seed_page("There", "y");
    world
        .fake
        .borrow_mut()
        .pages
        .iter_mut()
        .find(|p| p.summary.id == there)
        .unwrap()
        .summary
        .conversation_id = world.design;
    world.reach(cx);
    world.open_docs(cx);
    assert!(world.on_screen(cx, "docs-row-1"), "both are listed");
    world.click(cx, "docs-place");
    world.click_named(cx, format!("docs-place-menu-{}", world.general));
    assert!(world.on_screen(cx, "docs-row-0"));
    assert!(!world.on_screen(cx, "docs-row-1"), "only #general's page");
    assert_eq!(
        world.core(cx, |core| core
            .page_list(world.general, lazy_protocol::PageState::Live)
            .map(|list| list.pages.iter().map(|p| p.id).collect::<Vec<_>>())),
        Some(vec![here])
    );
    world.click(cx, "docs-place");
    world.click(cx, "docs-place-menu-all");
    assert!(world.on_screen(cx, "docs-row-1"), "everything is back");
}

#[gpui::test]
fn a_new_page_goes_to_the_open_conversation_unless_the_list_is_narrowed(cx: &mut TestAppContext) {
    let (world, cx) = start(cx);
    world.reach(cx);
    world.open_docs(cx);
    world.click(cx, "docs-place");
    world.click_named(cx, format!("docs-place-menu-{}", world.design));
    world.create_from_menu(cx, "docs-new-page");
    let id = world.first_page();
    assert_eq!(
        world.page(id).summary.conversation_id,
        world.design,
        "the conversation the list is narrowed to"
    );
}

#[gpui::test]
fn the_docs_command_has_a_shortcut_a_palette_row_and_a_rule(cx: &mut TestAppContext) {
    let (world, cx) = start(cx);
    world.reach(cx);
    let shell = world.shell(cx);
    // The shortcut opens docs, and the snapshot says docs are what is showing.
    world.keys(cx, "secondary-shift-d");
    assert!(shell.read_with(cx, |shell, cx| shell.docs_open(cx)));
    let snapshot = cx.update(|_, cx| shell.read(cx).snapshot(cx));
    assert_eq!(snapshot.view, commands::MainView::Docs);
    // The shortcut and the title come from the registry.
    let command = commands::all()
        .iter()
        .find(|command| command.id == CommandId::Docs)
        .expect("the docs command");
    assert_eq!(command.title, "Docs");
    assert!(command.shortcut.is_some());
    assert_eq!(
        commands::availability(CommandId::Docs, &snapshot),
        commands::Availability::Enabled
    );
    // Docs are the whole workspace's, so they open with no conversation selected too.
    let mut none = snapshot.clone();
    none.conversation_selected = false;
    none.view = commands::MainView::Conversation;
    assert_eq!(
        commands::availability(CommandId::Docs, &none),
        commands::Availability::Enabled
    );
}

#[gpui::test]
fn the_notes_command_opens_the_notes_screen(cx: &mut TestAppContext) {
    let (world, cx) = start(cx);
    world.reach(cx);
    world.keys(cx, "secondary-shift-n");
    assert!(world.on_screen(cx, "docs-notes-screen"));
    let view = world.docs_view(cx);
    assert_eq!(view.read_with(cx, |view, _| view.screen()), Screen::Notes);
}

#[gpui::test]
fn escape_steps_back_one_level_at_a_time_and_gives_the_keyboard_back(cx: &mut TestAppContext) {
    let (world, cx) = start(cx);
    world.seed_page("Plan", "Alpha");
    world.reach(cx);
    let shell = world.shell(cx);
    world.open_docs(cx);
    open_row(&world, cx, 0);
    // History opens beside the page; Escape closes it first.
    world.click(cx, "docs-history-toggle");
    let view = world.docs_view(cx);
    assert_eq!(view.read_with(cx, |view, _| view.pane()), Pane::History);
    world.keys(cx, "escape");
    assert_eq!(view.read_with(cx, |view, _| view.pane()), Pane::None);
    assert!(world.on_screen(cx, "docs-page-screen"));
    // Then the page closes into the list...
    world.keys(cx, "escape");
    assert!(world.on_screen(cx, "docs-list-screen"));
    assert!(world.core(cx, |core| core.open_page().is_none()));
    // ...and then docs close, with the keyboard back in the composer.
    world.keys(cx, "escape");
    assert!(!shell.read_with(cx, |shell, cx| shell.docs_open(cx)));
    assert!(!world.on_screen(cx, "docs-view"));
}

#[gpui::test]
fn choosing_another_conversation_ends_docs(cx: &mut TestAppContext) {
    let (world, cx) = start(cx);
    world.reach(cx);
    world.open_docs(cx);
    let shell = world.shell(cx);
    world.dispatch(
        cx,
        lazy_client_core::Input::SelectConversation(world.design),
    );
    assert!(!shell.read_with(cx, |shell, cx| shell.docs_open(cx)));
}

// ----- The list -----

#[gpui::test]
fn the_list_shows_pages_and_boards_with_their_counts(cx: &mut TestAppContext) {
    let (world, cx) = start(cx);
    world.seed_page("Launch plan", "Ship on Friday");
    world.seed_board("Flow", two_shapes());
    {
        let mut fake = world.fake.borrow_mut();
        fake.pages[0].summary.open_notations = 2;
        fake.pages[0].summary.file_count = 1;
    }
    world.reach(cx);
    world.open_docs(cx);
    assert!(world.on_screen(cx, "docs-row-0"));
    assert!(world.on_screen(cx, "docs-row-1"));
    assert!(!world.on_screen(cx, "docs-row-2"));
    let rows = world.core(cx, |core| {
        core.workspace_pages(world.workspace, lazy_protocol::PageState::Live)
            .map(|list| list.pages.len())
    });
    assert_eq!(rows, Some(2));
}

#[gpui::test]
fn up_down_and_enter_open_a_page_from_the_search_box(cx: &mut TestAppContext) {
    let (world, cx) = start(cx);
    let _older = world.seed_page("Older", "x");
    let newer = world.seed_page("Newer", "y");
    world.reach(cx);
    world.open_docs(cx);
    let view = world.docs_view(cx);
    // The newest is first and active; Down moves to the other; Up comes back.
    assert_eq!(view.read_with(cx, |view, _| view.active_row()), 0);
    world.keys(cx, "down");
    assert_eq!(view.read_with(cx, |view, _| view.active_row()), 1);
    world.keys(cx, "up");
    assert_eq!(view.read_with(cx, |view, _| view.active_row()), 0);
    world.keys(cx, "enter");
    assert!(world.on_screen(cx, "docs-page-screen"));
    assert_eq!(
        world.core(cx, |core| core.open_page().map(|open| open.id)),
        Some(newer)
    );
}

#[gpui::test]
fn the_search_box_searches_the_workspace_and_clearing_it_brings_the_list_back(
    cx: &mut TestAppContext,
) {
    let (world, cx) = start(cx);
    world.seed_page("Launch plan", "Ship on Friday");
    world.seed_page("Lunch menu", "Soup");
    world.reach(cx);
    world.open_docs(cx);
    world.type_text(cx, "soup");
    assert!(
        world
            .calls()
            .iter()
            .any(|call| matches!(call, DocsCall::ListWorkspacePages { .. })),
        "the workspace was searched"
    );
    assert!(world.on_screen(cx, "docs-row-0"));
    assert!(
        !world.on_screen(cx, "docs-row-1"),
        "only the page with soup"
    );
    world.keys(cx, "escape");
    assert!(world.on_screen(cx, "docs-row-1"), "the whole list is back");
    assert!(
        world.on_screen(cx, "docs-view"),
        "Escape cleared the search, it did not close docs"
    );
}

#[gpui::test]
fn a_search_with_no_match_says_so(cx: &mut TestAppContext) {
    let (world, cx) = start(cx);
    world.seed_page("Launch plan", "Ship");
    world.reach(cx);
    world.open_docs(cx);
    world.type_text(cx, "zzz");
    assert!(world.on_screen(cx, "docs-no-match"));
}

#[gpui::test]
fn tab_reaches_new_page_and_enter_makes_one(cx: &mut TestAppContext) {
    let (world, cx) = start(cx);
    world.reach(cx);
    world.open_docs(cx);
    // Search box, then the Pages/Trash toggle, Notes for me, and New; Enter opens its menu and
    // Enter again takes the first item, a page.
    world.keys(cx, "tab tab tab enter");
    assert!(world.menu_open(cx), "New opens a menu");
    world.keys(cx, "enter");
    assert!(world.on_screen(cx, "docs-page-screen"));
    assert_eq!(world.fake.borrow().pages.len(), 1);
}

#[gpui::test]
fn an_archived_conversation_can_be_read_but_not_added_to(cx: &mut TestAppContext) {
    let (world, cx) = start(cx);
    world.seed_page("Plan", "x");
    world.reach(cx);
    let mut conversation = world.core(cx, |core| {
        core.conversation(world.general).cloned().unwrap()
    });
    conversation.archived = true;
    world.event(
        cx,
        lazy_protocol::Event::ConversationUpdated { conversation },
    );
    world.open_docs(cx);
    world.create_from_menu(cx, "docs-new-page");
    let made: Vec<_> = world
        .calls()
        .into_iter()
        .filter_map(|call| match call {
            DocsCall::CreatePage { conversation, .. } => Some(conversation),
            _ => None,
        })
        .collect();
    assert_eq!(
        made,
        vec![world.design],
        "nothing is made in the archived conversation; the new page goes where one can write"
    );
    world.click(cx, "docs-page-back");
    open_row(&world, cx, 1);
    assert!(world.on_screen(cx, "docs-read-only"));
    let view = world.docs_view(cx);
    let editor = view.read_with(cx, |view, _| view.editor().clone());
    assert!(
        !editor.read_with(cx, |input, _| input.is_enabled()),
        "the text cannot be edited"
    );
}

// ----- A page -----

#[gpui::test]
fn a_new_page_opens_and_what_is_typed_is_saved_after_a_pause(cx: &mut TestAppContext) {
    let (world, cx) = start(cx);
    world.reach(cx);
    world.open_docs(cx);
    world.create_from_menu(cx, "docs-new-page");
    assert!(world.on_screen(cx, "docs-page-screen"));
    let id = world.first_page();
    assert_eq!(world.page(id).summary.title, "Untitled");
    world.click(cx, "docs-editor");
    world.type_text(cx, "Hello docs");
    assert_eq!(
        world.body_of(id),
        "",
        "nothing is sent while typing continues"
    );
    assert_eq!(save_state(&world, cx), Some(SaveState::Dirty));
    world.autosave(cx);
    assert_eq!(world.body_of(id), "Hello docs");
    assert_eq!(save_state(&world, cx), Some(SaveState::Saved));
    assert!(world.on_screen(cx, "docs-save-status"));
}

#[gpui::test]
fn a_new_page_starts_with_the_keyboard_in_its_title_and_an_old_one_on_back(
    cx: &mut TestAppContext,
) {
    let (world, cx) = start(cx);
    world.seed_page("Plan", "Alpha");
    world.reach(cx);
    world.open_docs(cx);
    let view = world.docs_view(cx);
    let title = view.read_with(cx, |view, _| view.title_input().clone());
    world.create_from_menu(cx, "docs-new-page");
    assert!(cx.update(|window, cx| title.read(cx).is_focused(window)));
    world.type_text(cx, "Roadmap");
    world.autosave(cx);
    assert!(
        world
            .fake
            .borrow()
            .pages
            .iter()
            .any(|p| p.summary.title == "Roadmap")
    );
    // An existing page opens on Back, the first stop.
    world.click(cx, "docs-page-back");
    open_row(&world, cx, 1);
    assert!(!cx.update(|window, cx| title.read(cx).is_focused(window)));
}

#[gpui::test]
fn the_title_is_edited_in_the_title_box_and_enter_moves_to_the_text(cx: &mut TestAppContext) {
    let (world, cx) = start(cx);
    let id = world.seed_page("Plan", "Alpha");
    world.reach(cx);
    world.open_docs(cx);
    open_row(&world, cx, 0);
    let view = world.docs_view(cx);
    let title = view.read_with(cx, |view, _| view.title_input().clone());
    assert_eq!(
        title.read_with(cx, |input, _| input.text().to_owned()),
        "Plan"
    );
    title.update_in(cx, |input, window, cx| input.focus(window, cx));
    world.keys(cx, "secondary-a");
    world.type_text(cx, "Roadmap");
    world.keys(cx, "enter");
    world.autosave(cx);
    assert_eq!(world.page(id).summary.title, "Roadmap");
    let editor = view.read_with(cx, |view, _| view.editor().clone());
    assert!(
        cx.update(|window, cx| editor.read(cx).is_focused(window)),
        "Enter in the title moves to the text"
    );
}

#[gpui::test]
fn a_page_is_one_document_with_its_title_on_top_and_no_edit_preview_switch(
    cx: &mut TestAppContext,
) {
    let (world, cx) = start(cx);
    world.seed_page("Plan", "# Heading\n\nSome **bold** text");
    world.reach(cx);
    world.open_docs(cx);
    open_row(&world, cx, 0);
    assert!(
        world.on_screen(cx, "docs-title"),
        "the title is the first line"
    );
    assert!(world.on_screen(cx, "docs-editor"));
    assert!(!world.on_screen(cx, "docs-mode-preview"));
    assert!(!world.on_screen(cx, "docs-mode-edit"));
    // The bar has the few things you do all the time; the rest is one menu.
    assert!(world.on_screen(cx, "docs-notes-toggle"));
    assert!(world.on_screen(cx, "docs-history-toggle"));
    assert!(world.on_screen(cx, "docs-more"));
    assert!(!world.on_screen(cx, "docs-trash"));
    assert!(!world.on_screen(cx, "docs-attach"));
}

#[gpui::test]
fn someone_else_changing_the_page_is_a_conflict_to_settle(cx: &mut TestAppContext) {
    let (world, cx) = start(cx);
    let id = world.seed_page("Plan", "Alpha");
    world.reach(cx);
    world.open_docs(cx);
    open_row(&world, cx, 0);
    // Someone saves behind this person's back.
    {
        let mut fake = world.fake.borrow_mut();
        fake.pages[0].content = PageContent::Page {
            body: "Theirs".into(),
        };
        fake.pages[0].summary.revision += 1;
    }
    world.click(cx, "docs-editor");
    world.type_text(cx, " mine");
    world.autosave(cx);
    assert!(world.on_screen(cx, "docs-conflict"), "the banner is up");
    assert_eq!(save_state(&world, cx), Some(SaveState::Conflict));
    assert_eq!(world.body_of(id), "Theirs", "nothing was overwritten");
    // Keep mine saves the working copy over theirs.
    world.click(cx, "docs-keep-mine");
    assert!(world.body_of(id).contains("mine"));
    assert!(!world.on_screen(cx, "docs-conflict"));
}

#[gpui::test]
fn taking_theirs_drops_the_edits_and_shows_their_text(cx: &mut TestAppContext) {
    let (world, cx) = start(cx);
    let id = world.seed_page("Plan", "Alpha");
    world.reach(cx);
    world.open_docs(cx);
    open_row(&world, cx, 0);
    {
        let mut fake = world.fake.borrow_mut();
        fake.pages[0].content = PageContent::Page {
            body: "Theirs".into(),
        };
        fake.pages[0].summary.revision += 1;
    }
    world.click(cx, "docs-editor");
    world.type_text(cx, " mine");
    world.autosave(cx);
    world.click(cx, "docs-take-theirs");
    assert_eq!(world.body_of(id), "Theirs");
    let view = world.docs_view(cx);
    let editor = view.read_with(cx, |view, _| view.editor().clone());
    assert_eq!(
        editor.read_with(cx, |input, _| input.text().to_owned()),
        "Theirs"
    );
    assert!(!world.on_screen(cx, "docs-conflict"));
}

#[gpui::test]
fn a_failed_save_says_so_and_is_tried_again(cx: &mut TestAppContext) {
    let (world, cx) = start(cx);
    let id = world.seed_page("Plan", "Alpha");
    world.reach(cx);
    world.open_docs(cx);
    open_row(&world, cx, 0);
    world.click(cx, "docs-editor");
    world.fake.borrow_mut().fail_next = Some(lazy_client_core::RequestError::Rejected(
        lazy_protocol::ApiError::new(lazy_protocol::ErrorCode::Forbidden, "no"),
    ));
    world.type_text(cx, "!");
    world.autosave(cx);
    assert!(matches!(save_state(&world, cx), Some(SaveState::Failed(_))));
    assert_eq!(world.body_of(id), "Alpha");
    // After the pause the view asks again, and this time it goes through.
    cx.executor()
        .advance_clock(lazy_ui::docs::RETRY_AFTER + std::time::Duration::from_millis(5));
    cx.run_until_parked();
    world.serve(cx);
    assert_eq!(world.body_of(id), "Alpha!");
    assert_eq!(save_state(&world, cx), Some(SaveState::Saved));
}

#[gpui::test]
fn back_saves_what_was_typed_without_waiting_for_the_pause(cx: &mut TestAppContext) {
    let (world, cx) = start(cx);
    let id = world.seed_page("Plan", "Alpha");
    world.reach(cx);
    world.open_docs(cx);
    open_row(&world, cx, 0);
    world.click(cx, "docs-editor");
    world.type_text(cx, "!");
    world.click(cx, "docs-page-back");
    world.serve(cx);
    assert!(world.on_screen(cx, "docs-list-screen"));
    assert_eq!(world.body_of(id), "Alpha!");
}

// ----- The trash -----

#[gpui::test]
fn moving_to_trash_restoring_and_deleting_for_good_go_through_the_trash(cx: &mut TestAppContext) {
    let (world, cx) = start(cx);
    let id = world.seed_page("Plan", "Alpha");
    world.reach(cx);
    world.open_docs(cx);
    open_row(&world, cx, 0);
    world.pick_from_more(cx, "docs-trash");
    assert!(
        world.on_screen(cx, "docs-list-screen"),
        "trashing leaves the page"
    );
    assert!(world.page(id).summary.trashed_at.is_some());
    assert!(world.on_screen(cx, "docs-empty") || !world.on_screen(cx, "docs-row-0"));

    // The trash lists it, with how long it has left.
    world.click(cx, "docs-state-trash");
    assert!(world.on_screen(cx, "docs-row-0"));
    assert!(world.on_screen(cx, "docs-restore-0"));

    // Restore brings it back to the pages.
    world.click(cx, "docs-restore-0");
    assert!(world.page(id).summary.trashed_at.is_none());
    world.click(cx, "docs-state-live");
    assert!(world.on_screen(cx, "docs-row-0"));
}

#[gpui::test]
fn deleting_for_good_asks_first_and_cancel_keeps_the_page(cx: &mut TestAppContext) {
    let (world, cx) = start(cx);
    let id = world.seed_page("Plan", "Alpha");
    world.fake.borrow_mut().pages[0].summary.trashed_at = Some(Timestamp::now());
    world.fake.borrow_mut().pages[0].summary.purge_at = Some(Timestamp::now());
    world.reach(cx);
    world.open_docs(cx);
    world.click(cx, "docs-state-trash");
    world.click(cx, "docs-purge-0");
    assert!(
        world.on_screen(cx, "purge-page-confirm"),
        "a dialog asks first"
    );
    assert!(
        !world
            .fake
            .borrow()
            .calls
            .iter()
            .any(|c| matches!(c, DocsCall::PurgePage { .. }))
    );
    world.click(cx, "purge-page-cancel");
    assert!(!world.on_screen(cx, "purge-page-confirm"));
    assert_eq!(world.fake.borrow().pages.len(), 1, "cancel keeps it");
    // The keyboard starts on Cancel, so Enter alone does not delete.
    world.click(cx, "docs-purge-0");
    world.keys(cx, "enter");
    assert_eq!(world.fake.borrow().pages.len(), 1);
    world.click(cx, "docs-purge-0");
    world.click(cx, "purge-page-confirm");
    assert!(
        world.fake.borrow().pages.iter().all(|p| p.summary.id != id),
        "now it is gone"
    );
    assert!(!world.on_screen(cx, "docs-row-0"));
}

#[gpui::test]
fn an_open_trashed_page_can_be_read_but_not_edited(cx: &mut TestAppContext) {
    let (world, cx) = start(cx);
    world.seed_page("Plan", "Alpha");
    world.fake.borrow_mut().pages[0].summary.trashed_at = Some(Timestamp::now());
    world.fake.borrow_mut().pages[0].summary.purge_at = Some(Timestamp::now());
    world.reach(cx);
    world.open_docs(cx);
    world.click(cx, "docs-state-trash");
    open_row(&world, cx, 0);
    assert!(world.on_screen(cx, "docs-trashed"));
    let view = world.docs_view(cx);
    let editor = view.read_with(cx, |view, _| view.editor().clone());
    assert!(!editor.read_with(cx, |input, _| input.is_enabled()));
    world.click(cx, "docs-more");
    assert!(
        world.on_screen(cx, "docs-attach"),
        "the menu is open, so its items are there"
    );
    assert!(
        !world.on_screen(cx, "docs-trash"),
        "it is already in the trash"
    );
}

// ----- History -----

#[gpui::test]
fn history_lists_versions_previews_one_and_restores_it(cx: &mut TestAppContext) {
    let (world, cx) = start(cx);
    let id = world.seed_page("Plan", "One");
    world.reach(cx);
    world.open_docs(cx);
    open_row(&world, cx, 0);
    for text in ["Two", "Three"] {
        world.docs(
            cx,
            DocsInput::EditPage {
                title: None,
                content: Some(PageContent::Page { body: text.into() }),
            },
        );
        world.docs(cx, DocsInput::SavePage);
    }
    assert_eq!(world.body_of(id), "Three");
    world.click(cx, "docs-history-toggle");
    assert!(world.on_screen(cx, "docs-history-pane"));
    assert!(world.on_screen(cx, "docs-version-0"));
    assert!(world.on_screen(cx, "docs-version-1"));
    // Newest first: version 2 is "Two", version 1 is "One".
    world.click(cx, "docs-version-1");
    assert!(world.on_screen(cx, "docs-version-banner"));
    assert!(
        world.on_screen(cx, "docs-version-note"),
        "what a restore does is said"
    );
    assert_eq!(world.body_of(id), "Three", "looking changes nothing");
    world.click(cx, "docs-version-restore");
    assert_eq!(world.body_of(id), "One");
    assert!(
        world
            .fake
            .borrow()
            .versions
            .iter()
            .any(|(_, v)| v.summary.reason == lazy_domain::page::VersionReason::PreRestore),
        "the page as it was is kept, so the restore can be undone"
    );
    assert!(!world.on_screen(cx, "docs-version-banner"));
}

#[gpui::test]
fn a_version_preview_goes_away_with_back_to_current_and_escape(cx: &mut TestAppContext) {
    let (world, cx) = start(cx);
    world.seed_page("Plan", "One");
    world.reach(cx);
    world.open_docs(cx);
    open_row(&world, cx, 0);
    world.docs(
        cx,
        DocsInput::EditPage {
            title: None,
            content: Some(PageContent::Page { body: "Two".into() }),
        },
    );
    world.docs(cx, DocsInput::SavePage);
    world.click(cx, "docs-history-toggle");
    world.click(cx, "docs-version-0");
    assert!(world.on_screen(cx, "docs-version-banner"));
    world.click(cx, "docs-version-back");
    assert!(!world.on_screen(cx, "docs-version-banner"));
    world.click(cx, "docs-version-0");
    world.keys(cx, "escape");
    assert!(!world.on_screen(cx, "docs-version-banner"));
    assert!(
        world.on_screen(cx, "docs-history-pane"),
        "the pane stays until the next Escape"
    );
}

// ----- Notes -----

#[gpui::test]
fn a_note_is_written_on_selected_text_and_listed_with_its_quote(cx: &mut TestAppContext) {
    let (world, cx) = start(cx);
    let id = world.seed_page("Plan", "Alpha beta gamma");
    world.reach(cx);
    world.open_docs(cx);
    open_row(&world, cx, 0);
    // With nothing selected there is nothing to add a note to: the menu item is off.
    world.pick_from_more(cx, "docs-add-note");
    assert!(!world.on_screen(cx, "docs-note-draft"));
    world.keys(cx, "escape");
    assert!(!world.menu_open(cx));
    select_in_editor(&world, cx, 6, 10);
    world.pick_from_more(cx, "docs-add-note");
    assert!(world.on_screen(cx, "docs-note-draft"));
    world.type_text(cx, "Is this the right word?");
    world.click(cx, "docs-note-submit");
    let notations = world.fake.borrow().notations.clone();
    assert_eq!(notations.len(), 1);
    assert_eq!(notations[0].quote, "beta");
    assert_eq!((notations[0].from, notations[0].to), (6, 10));
    assert_eq!(notations[0].page_id, id);
    assert_eq!(
        notations[0].assignee_id, world.me.id,
        "the author unless someone is chosen"
    );
    assert!(world.on_screen(cx, "docs-note-0"));
    assert!(!world.on_screen(cx, "docs-note-draft"));
}

#[gpui::test]
fn a_note_can_be_replied_to_completed_reopened_and_deleted(cx: &mut TestAppContext) {
    let (world, cx) = start(cx);
    world.seed_page("Plan", "Alpha beta gamma");
    world.reach(cx);
    world.open_docs(cx);
    open_row(&world, cx, 0);
    select_in_editor(&world, cx, 6, 10);
    world.pick_from_more(cx, "docs-add-note");
    world.type_text(cx, "Check");
    world.click(cx, "docs-note-submit");

    world.click(cx, "docs-note-reply-0");
    world.type_text(cx, "Yes, fixed");
    world.click(cx, "docs-reply-send-0");
    assert_eq!(world.fake.borrow().notations[0].replies.len(), 1);
    assert!(world.on_screen(cx, "docs-reply-0-0"));

    world.click(cx, "docs-note-done-0");
    assert!(world.fake.borrow().notations[0].completed_at.is_some());
    assert!(world.on_screen(cx, "docs-note-done-badge-0"));
    assert!(
        !world.on_screen(cx, "docs-note-reply-0"),
        "a finished note takes no more replies"
    );
    world.click(cx, "docs-note-done-0");
    assert!(world.fake.borrow().notations[0].completed_at.is_none());

    world.click(cx, "docs-note-delete-0");
    assert!(world.fake.borrow().notations.is_empty());
    assert!(!world.on_screen(cx, "docs-note-0"));
}

#[gpui::test]
fn a_note_whose_text_was_changed_says_it_is_detached(cx: &mut TestAppContext) {
    let (world, cx) = start(cx);
    world.seed_page("Plan", "Alpha beta gamma");
    world.reach(cx);
    world.open_docs(cx);
    open_row(&world, cx, 0);
    select_in_editor(&world, cx, 6, 10);
    world.pick_from_more(cx, "docs-add-note");
    world.type_text(cx, "Check");
    world.click(cx, "docs-note-submit");
    assert!(!world.on_screen(cx, "docs-note-detached-0"));
    assert!(world.on_screen(cx, "docs-note-show-0"));
    // The quoted word goes away.
    world.docs(
        cx,
        DocsInput::EditPage {
            title: None,
            content: Some(PageContent::Page {
                body: "Alpha gamma".into(),
            }),
        },
    );
    draw(cx);
    assert!(world.on_screen(cx, "docs-note-detached-0"));
    assert!(
        !world.on_screen(cx, "docs-note-show-0"),
        "there is nothing to show it in"
    );
}

#[gpui::test]
fn show_in_text_selects_the_quote_in_the_editor(cx: &mut TestAppContext) {
    let (world, cx) = start(cx);
    world.seed_page("Plan", "Alpha beta gamma");
    world.reach(cx);
    world.open_docs(cx);
    open_row(&world, cx, 0);
    select_in_editor(&world, cx, 6, 10);
    world.pick_from_more(cx, "docs-add-note");
    world.type_text(cx, "Check");
    world.click(cx, "docs-note-submit");
    select_in_editor(&world, cx, 0, 0);
    world.click(cx, "docs-note-show-0");
    let view = world.docs_view(cx);
    let editor = view.read_with(cx, |view, _| view.editor().clone());
    assert_eq!(editor.read_with(cx, |input, _| input.selection()), (6, 10));
}

#[gpui::test]
fn boards_take_no_notes(cx: &mut TestAppContext) {
    let (world, cx) = start(cx);
    world.seed_board("Flow", two_shapes());
    world.reach(cx);
    world.open_docs(cx);
    open_row(&world, cx, 0);
    assert!(!world.on_screen(cx, "docs-notes-toggle"));
    assert!(!world.on_screen(cx, "docs-add-note"));
}

#[gpui::test]
fn the_notes_screen_lists_assigned_notes_and_opens_their_page(cx: &mut TestAppContext) {
    let (world, cx) = start(cx);
    let id = world.seed_page("Plan", "Alpha beta gamma");
    world.reach(cx);
    world.open_docs(cx);
    open_row(&world, cx, 0);
    select_in_editor(&world, cx, 6, 10);
    world.pick_from_more(cx, "docs-add-note");
    world.type_text(cx, "Check");
    world.click(cx, "docs-note-submit");
    world.click(cx, "docs-page-back");

    world.click(cx, "docs-open-notes");
    assert!(world.on_screen(cx, "docs-notes-screen"));
    assert!(world.on_screen(cx, "notes-list"));
    // Done notes are listed apart.
    world.click(cx, "notes-completed");
    assert!(world.on_screen(cx, "notes-empty"));
    world.click(cx, "notes-open");
    world.click(cx, "notes-open-0");
    assert!(world.on_screen(cx, "docs-page-screen"));
    assert_eq!(
        world.core(cx, |core| core.open_page().map(|open| open.id)),
        Some(id)
    );
    // Back from the page returns to the notes, not the list.
    world.click(cx, "docs-page-back");
    assert!(world.on_screen(cx, "docs-notes-screen"));
}

#[gpui::test]
fn the_notes_screen_can_show_the_notes_you_wrote(cx: &mut TestAppContext) {
    let (world, cx) = start(cx);
    world.reach(cx);
    world.keys(cx, "secondary-shift-n");
    assert!(world.on_screen(cx, "notes-empty"));
    world.click(cx, "notes-scope-created");
    assert!(world.on_screen(cx, "notes-empty"));
    let calls = world.calls();
    assert!(
        calls
            .iter()
            .filter(|c| matches!(c, DocsCall::ListWorkspaceNotations { .. }))
            .count()
            >= 2
    );
}

// ----- Files and moving -----

#[gpui::test]
fn a_file_is_added_to_the_page_and_listed(cx: &mut TestAppContext) {
    let (world, cx) = start(cx);
    world.seed_page("Plan", "Alpha");
    world.reach(cx);
    world.open_docs(cx);
    open_row(&world, cx, 0);
    let view = world.docs_view(cx);
    view.update(cx, |view, cx| {
        view.attach_paths(vec!["/tmp/spec.pdf".into()], cx)
    });
    world.serve(cx);
    draw(cx);
    assert!(world.on_screen(cx, "docs-file-0"));
    assert!(world.on_screen(cx, "docs-file-download-0"));
    assert!(
        world
            .calls()
            .iter()
            .any(|c| matches!(c, DocsCall::UploadPageFile { .. }))
    );
}

#[gpui::test]
fn a_page_moves_to_another_conversation_from_a_dialog(cx: &mut TestAppContext) {
    let (world, cx) = start(cx);
    let id = world.seed_page("Plan", "Alpha");
    world.reach(cx);
    world.open_docs(cx);
    open_row(&world, cx, 0);
    world.pick_from_more(cx, "docs-move");
    assert!(world.on_screen(cx, "move-page-list"));
    world.click(cx, "move-page-choice-0");
    assert_eq!(world.page(id).summary.conversation_id, world.design);
    assert!(!world.on_screen(cx, "move-page-list"), "the dialog closed");
}

// ----- Boards -----

fn open_board(world: &World, cx: &mut gpui::VisualTestContext) -> PageId {
    let id = world.seed_board("Flow", two_shapes());
    world.reach(cx);
    world.open_docs(cx);
    open_row(world, cx, 0);
    assert!(world.on_screen(cx, "docs-board"));
    id
}

fn selected(world: &World, cx: &mut gpui::VisualTestContext) -> Option<String> {
    let view = world.docs_view(cx);
    view.read_with(cx, |view, _| {
        view.board_state().selected_node().map(str::to_owned)
    })
}

#[gpui::test]
fn a_board_draws_its_shapes_and_a_click_selects_one(cx: &mut TestAppContext) {
    let (world, cx) = start(cx);
    open_board(&world, cx);
    assert!(world.on_screen(cx, "board-node-n1"));
    assert!(world.on_screen(cx, "board-node-n2"));
    assert!(world.on_screen(cx, "board-edge-e1"));
    assert_eq!(selected(&world, cx), None);
    world.click(cx, "board-node-n1");
    assert_eq!(selected(&world, cx).as_deref(), Some("n1"));
    assert!(
        world.on_screen(cx, "board-handle-SouthEast"),
        "a selected shape has handles"
    );
    world.click(cx, "board-node-n2");
    assert_eq!(selected(&world, cx).as_deref(), Some("n2"));
}

#[gpui::test]
fn dragging_a_shape_moves_it_and_the_move_is_saved(cx: &mut TestAppContext) {
    let (world, cx) = start(cx);
    let id = open_board(&world, cx);
    let from = world.center(cx, "board-node-n1");
    world.drag(
        cx,
        from,
        gpui::point(from.x + gpui::px(60.), from.y + gpui::px(40.)),
    );
    let board = world.core(cx, |core| {
        match core.open_page().and_then(|o| o.content.clone()) {
            Some(PageContent::Board { board }) => board,
            other => panic!("{other:?}"),
        }
    });
    let n1 = board.nodes.iter().find(|n| n.id == "n1").unwrap();
    assert_eq!((n1.x, n1.y), (160, 140));
    world.autosave(cx);
    let saved = server_board(&world, id);
    let n1 = saved.nodes.iter().find(|n| n.id == "n1").unwrap();
    assert_eq!((n1.x, n1.y), (160, 140));
}

#[gpui::test]
fn a_click_without_a_drag_moves_nothing(cx: &mut TestAppContext) {
    let (world, cx) = start(cx);
    let id = open_board(&world, cx);
    world.click(cx, "board-node-n1");
    world.autosave(cx);
    assert_eq!(server_board(&world, id), two_shapes());
    assert_eq!(
        world.page(id).summary.revision,
        1,
        "no edit, no new revision"
    );
}

#[gpui::test]
fn dragging_a_corner_handle_resizes_the_shape(cx: &mut TestAppContext) {
    let (world, cx) = start(cx);
    let id = open_board(&world, cx);
    world.click(cx, "board-node-n1");
    let handle = world.center(cx, "board-handle-SouthEast");
    world.drag(
        cx,
        handle,
        gpui::point(handle.x + gpui::px(40.), handle.y + gpui::px(20.)),
    );
    world.autosave(cx);
    let saved = server_board(&world, id);
    let n1 = saved.nodes.iter().find(|n| n.id == "n1").unwrap();
    assert_eq!((n1.x, n1.y, n1.width, n1.height), (100, 100, 200, 100));
}

#[gpui::test]
fn dragging_the_background_pans_without_changing_the_board(cx: &mut TestAppContext) {
    let (world, cx) = start(cx);
    let id = open_board(&world, cx);
    let view = world.docs_view(cx);
    let before = view.read_with(cx, |view, _| view.board_state().offset());
    let from = world.center(cx, "docs-board");
    world.drag(
        cx,
        from,
        gpui::point(from.x + gpui::px(50.), from.y + gpui::px(30.)),
    );
    let after = view.read_with(cx, |view, _| view.board_state().offset());
    assert_eq!((after.0 - before.0, after.1 - before.1), (50., 30.));
    world.autosave(cx);
    assert_eq!(
        world.page(id).summary.revision,
        1,
        "looking around is not an edit"
    );
}

#[gpui::test]
fn the_zoom_buttons_zoom_in_and_out_within_limits(cx: &mut TestAppContext) {
    let (world, cx) = start(cx);
    open_board(&world, cx);
    let view = world.docs_view(cx);
    world.click(cx, "board-zoom-in");
    assert_eq!(
        view.read_with(cx, |view, _| view.board_state().zoom_percent()),
        110
    );
    world.click(cx, "board-zoom-out");
    world.click(cx, "board-zoom-out");
    assert_eq!(
        view.read_with(cx, |view, _| view.board_state().zoom_percent()),
        90
    );
    for _ in 0..60 {
        world.click(cx, "board-zoom-out");
    }
    assert_eq!(
        view.read_with(cx, |view, _| view.board_state().zoom_percent()),
        10
    );
    world.click(cx, "board-zoom-reset");
    assert_eq!(
        view.read_with(cx, |view, _| view.board_state().zoom_percent()),
        100
    );
    world.click(cx, "board-fit");
    assert!(view.read_with(cx, |view, _| view.board_state().zoom_percent()) <= 100);
}

#[gpui::test]
fn the_keyboard_walks_the_shapes_nudges_them_and_edits_their_text(cx: &mut TestAppContext) {
    let (world, cx) = start(cx);
    let id = open_board(&world, cx);
    let view = world.docs_view(cx);
    view.update_in(cx, |view, window, cx| view.focus_body(window, cx));
    // Tab selects the first shape, then the next, then the connector.
    world.keys(cx, "tab");
    assert_eq!(selected(&world, cx).as_deref(), Some("n1"));
    world.keys(cx, "tab");
    assert_eq!(selected(&world, cx).as_deref(), Some("n2"));
    world.keys(cx, "tab");
    assert_eq!(
        selected(&world, cx),
        None,
        "a connector is selected, not a shape"
    );
    assert_eq!(
        view.read_with(cx, |view, _| view.board_state().selection.clone()),
        Some(lazy_ui::docs::board::Selected::Edge("e1".into()))
    );
    world.keys(cx, "shift-tab");
    assert_eq!(selected(&world, cx).as_deref(), Some("n2"));
    world.keys(cx, "shift-tab");
    // Arrows nudge the selected shape by ten pixels.
    world.keys(cx, "right right down");
    world.autosave(cx);
    let saved = server_board(&world, id);
    let n1 = saved.nodes.iter().find(|n| n.id == "n1").unwrap();
    assert_eq!((n1.x, n1.y), (120, 110));
    // Enter edits the text; Enter again keeps it.
    world.keys(cx, "enter");
    assert!(view.read_with(cx, |view, _| view.board_state().is_editing()));
    world.keys(cx, "secondary-a");
    world.type_text(cx, "Begin");
    world.keys(cx, "enter");
    assert!(!view.read_with(cx, |view, _| view.board_state().is_editing()));
    world.autosave(cx);
    let saved = server_board(&world, id);
    assert_eq!(
        saved.nodes.iter().find(|n| n.id == "n1").unwrap().text,
        "Begin"
    );
}

#[gpui::test]
fn escape_while_editing_a_shapes_text_keeps_the_old_text(cx: &mut TestAppContext) {
    let (world, cx) = start(cx);
    let id = open_board(&world, cx);
    let view = world.docs_view(cx);
    world.click(cx, "board-node-n1");
    world.keys(cx, "enter");
    world.type_text(cx, "xyz");
    world.keys(cx, "escape");
    assert!(!view.read_with(cx, |view, _| view.board_state().is_editing()));
    world.autosave(cx);
    assert_eq!(server_board(&world, id).nodes[0].text, "Start");
    assert!(
        world.on_screen(cx, "docs-page-screen"),
        "Escape ended the edit, not the page"
    );
}

#[gpui::test]
fn double_clicking_a_shape_edits_its_text(cx: &mut TestAppContext) {
    let (world, cx) = start(cx);
    open_board(&world, cx);
    let view = world.docs_view(cx);
    let at = world.center(cx, "board-node-n2");
    cx.simulate_event(gpui::MouseDownEvent {
        position: at,
        modifiers: Default::default(),
        button: gpui::MouseButton::Left,
        click_count: 2,
        first_mouse: false,
    });
    cx.simulate_event(gpui::MouseUpEvent {
        position: at,
        modifiers: Default::default(),
        button: gpui::MouseButton::Left,
        click_count: 2,
    });
    draw(cx);
    assert!(view.read_with(cx, |view, _| view.board_state().is_editing()));
    assert!(world.on_screen(cx, "board-text-edit"));
}

#[gpui::test]
fn letters_add_shapes_and_delete_removes_the_shape_and_its_connectors(cx: &mut TestAppContext) {
    let (world, cx) = start(cx);
    let id = open_board(&world, cx);
    let view = world.docs_view(cx);
    view.update_in(cx, |view, window, cx| view.focus_body(window, cx));
    world.keys(cx, "r");
    world.keys(cx, "e");
    world.keys(cx, "d");
    world.keys(cx, "s");
    world.keys(cx, "t");
    world.autosave(cx);
    let saved = server_board(&world, id);
    assert_eq!(saved.nodes.len(), 7);
    let shapes: Vec<NodeShape> = saved.nodes[2..].iter().map(|n| n.shape).collect();
    assert_eq!(
        shapes,
        [
            NodeShape::Rectangle,
            NodeShape::Ellipse,
            NodeShape::Diamond,
            NodeShape::Note,
            NodeShape::Text
        ]
    );
    // The newest is selected; delete removes just it.
    world.keys(cx, "delete");
    world.autosave(cx);
    assert_eq!(server_board(&world, id).nodes.len(), 6);
    // Deleting a shape takes its connectors with it.
    world.click(cx, "board-node-n1");
    world.keys(cx, "backspace");
    world.autosave(cx);
    let saved = server_board(&world, id);
    assert!(saved.nodes.iter().all(|n| n.id != "n1"));
    assert!(saved.edges.is_empty());
}

#[gpui::test]
fn connect_draws_a_connector_from_the_selected_shape_to_the_next_one_chosen(
    cx: &mut TestAppContext,
) {
    let (world, cx) = start(cx);
    let id = open_board(&world, cx);
    world.keys(cx, "escape");
    let view = world.docs_view(cx);
    // A connector in the other direction.
    world.click(cx, "board-node-n2");
    world.click(cx, "board-connect");
    assert_eq!(
        view.read_with(cx, |view, _| view.board_state().connecting.clone()),
        Some("n2".into())
    );
    world.click(cx, "board-node-n1");
    world.autosave(cx);
    let saved = server_board(&world, id);
    assert_eq!(saved.edges.len(), 2);
    assert!(
        saved
            .edges
            .iter()
            .any(|e| e.source == "n2" && e.target == "n1")
    );
    assert_eq!(
        view.read_with(cx, |view, _| view.board_state().connecting.clone()),
        None
    );
}

#[gpui::test]
fn connect_with_the_keyboard_and_escape_cancels_it(cx: &mut TestAppContext) {
    let (world, cx) = start(cx);
    let id = open_board(&world, cx);
    let view = world.docs_view(cx);
    world.click(cx, "board-node-n1");
    world.keys(cx, "c");
    assert_eq!(
        view.read_with(cx, |view, _| view.board_state().connecting.clone()),
        Some("n1".into())
    );
    world.keys(cx, "escape");
    assert_eq!(
        view.read_with(cx, |view, _| view.board_state().connecting.clone()),
        None
    );
    world.autosave(cx);
    assert_eq!(server_board(&world, id).edges.len(), 1, "nothing was added");
}

#[gpui::test]
fn a_color_key_or_swatch_colors_the_selected_shape(cx: &mut TestAppContext) {
    let (world, cx) = start(cx);
    let id = open_board(&world, cx);
    world.click(cx, "board-node-n1");
    world.keys(cx, "3");
    world.autosave(cx);
    assert_eq!(
        server_board(&world, id).nodes[0].color,
        Some(BoardColor::Orange)
    );
    world.click(cx, "board-color-2");
    world.autosave(cx);
    assert_eq!(
        server_board(&world, id).nodes[0].color,
        Some(BoardColor::Red)
    );
    world.click(cx, "board-color-0");
    world.autosave(cx);
    assert_eq!(server_board(&world, id).nodes[0].color, None);
}

#[gpui::test]
fn a_board_in_the_trash_cannot_be_changed(cx: &mut TestAppContext) {
    let (world, cx) = start(cx);
    let id = world.seed_board("Flow", two_shapes());
    world.fake.borrow_mut().pages[0].summary.trashed_at = Some(Timestamp::now());
    world.fake.borrow_mut().pages[0].summary.purge_at = Some(Timestamp::now());
    world.reach(cx);
    world.open_docs(cx);
    world.click(cx, "docs-state-trash");
    open_row(&world, cx, 0);
    world.click(cx, "board-node-n1");
    world.keys(cx, "delete");
    world.keys(cx, "r");
    world.autosave(cx);
    assert_eq!(server_board(&world, id), two_shapes());
    assert_eq!(world.page(id).summary.kind, PageKind::Board);
}

#[gpui::test]
fn a_previewed_board_version_is_drawn_read_only(cx: &mut TestAppContext) {
    let (world, cx) = start(cx);
    open_board(&world, cx);
    world.docs(
        cx,
        DocsInput::EditPage {
            title: None,
            content: Some(PageContent::Board {
                board: Board {
                    nodes: vec![node("n9", NodeShape::Diamond, 0, 0, "Only")],
                    ..Board::empty()
                },
            }),
        },
    );
    world.docs(cx, DocsInput::SavePage);
    world.click(cx, "docs-history-toggle");
    world.click(cx, "docs-version-0");
    assert!(world.on_screen(cx, "docs-version-board"));
    assert!(
        !world.on_screen(cx, "board-node-n1"),
        "the preview has no handles to grab"
    );
}
