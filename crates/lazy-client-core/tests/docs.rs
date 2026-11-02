//! Docs (D30): lists, the open page, saving and conflicts, creating, trash, versions, notations,
//! the journal, and the order in which events and answers may arrive.

mod common;

use common::{Driver, World};
use lazy_client_core::{
    Change, ConflictChoice, CreateState, DocJournalAction, DocJournalEntry, DocsAnswer, DocsCall,
    DocsEffect, DocsInput, Effect, Input, LoadStatus, Output, RequestError, SaveState,
    StartupState,
};
use lazy_domain::page::VersionReason;
use lazy_domain::page::{PageContent, PageKind};
use lazy_domain::{
    ConversationId, NotationId, NotationReplyId, OperationId, PageId, Timestamp, UserId,
};
use lazy_protocol::{
    ApiError, ErrorCode, Event, Notation, NotationListResponse, NotationResponse, NotationScope,
    PageDetail, PageListResponse, PageResponse, PageState, PageSummary, PageVersionDetail,
    PageVersionResponse, PageVersionSummary, PageVersionsResponse,
};

// ----- Helpers -----

struct Fx {
    world: World,
    driver: Driver,
    general: ConversationId,
    me: UserId,
}

fn setup() -> Fx {
    let world = World::new();
    let general = world.channel.id;
    let me = world.me.id;
    let mut driver = Driver::ready(&world);
    driver.load_history(general, vec![], false);
    Fx {
        world,
        driver,
        general,
        me,
    }
}

impl Fx {
    fn docs(&mut self, input: DocsInput) -> Output {
        docs(self, input)
    }
    fn reply(&mut self, id: u64, answer: DocsAnswer) -> Output {
        reply(self, id, answer)
    }
    fn reply_err(&mut self, id: u64, error: RequestError) -> Output {
        reply_err(self, id, error)
    }
    fn load_list(&mut self, pages: Vec<PageSummary>) {
        load_list(self, pages)
    }
    fn open(&mut self, detail: &PageDetail) {
        open(self, detail)
    }
    fn edit_body(&mut self, body: &str) {
        edit_body(self, body)
    }
    fn save(&mut self) -> (u64, lazy_protocol::UpdatePageRequest) {
        save(self)
    }
}

fn at(offset: i64) -> Timestamp {
    Timestamp::from_offset_date_time(
        time::OffsetDateTime::from_unix_timestamp(1_700_000_000 + offset).unwrap(),
    )
}

fn summary(fx: &Fx, id: PageId, title: &str, revision: u32) -> PageSummary {
    PageSummary {
        id,
        workspace_id: fx.world.workspace.id,
        conversation_id: fx.general,
        kind: PageKind::Page,
        title: title.to_owned(),
        excerpt: String::new(),
        created_by: fx.me,
        created_at: at(0),
        updated_by: fx.me,
        updated_at: at(i64::from(revision)),
        revision,
        trashed_at: None,
        purge_at: None,
        open_notations: 0,
        file_count: 0,
    }
}

fn detail(fx: &Fx, id: PageId, title: &str, revision: u32, body: &str) -> PageDetail {
    PageDetail {
        summary: summary(fx, id, title, revision),
        content: PageContent::Page { body: body.into() },
        files: Vec::new(),
    }
}

fn trashed(mut summary: PageSummary) -> PageSummary {
    summary.trashed_at = Some(at(100));
    summary.purge_at = Some(at(200));
    summary
}

fn requests(out: &Output) -> Vec<(u64, DocsCall)> {
    out.effects
        .iter()
        .filter_map(|effect| match effect {
            Effect::Docs(DocsEffect::Request { id, call }) => Some((*id, call.clone())),
            _ => None,
        })
        .collect()
}

fn only_request(out: &Output) -> (u64, DocsCall) {
    let mut found = requests(out);
    assert_eq!(found.len(), 1, "one request expected, got {found:?}");
    found.remove(0)
}

fn persisted(out: &Output) -> Option<DocJournalEntry> {
    out.effects.iter().find_map(|effect| match effect {
        Effect::Docs(DocsEffect::Persist(entry)) => Some(entry.clone()),
        _ => None,
    })
}

fn settled(out: &Output) -> Vec<OperationId> {
    out.effects
        .iter()
        .filter_map(|effect| match effect {
            Effect::Docs(DocsEffect::Settle { operation }) => Some(*operation),
            _ => None,
        })
        .collect()
}

fn docs(fx: &mut Fx, input: DocsInput) -> Output {
    fx.driver.send(Input::Docs(input))
}

fn reply(fx: &mut Fx, id: u64, answer: DocsAnswer) -> Output {
    fx.driver.send(Input::DocsReply {
        id,
        result: Ok(answer),
    })
}

fn reply_err(fx: &mut Fx, id: u64, error: RequestError) -> Output {
    fx.driver.send(Input::DocsReply {
        id,
        result: Err(error),
    })
}

fn unknown() -> RequestError {
    RequestError::Unknown("timed out".into())
}

fn rejected(code: ErrorCode) -> RequestError {
    RequestError::Rejected(ApiError::new(code, "no"))
}

fn page_answer(detail: &PageDetail) -> DocsAnswer {
    DocsAnswer::Page(PageResponse {
        page: detail.clone(),
    })
}

fn list_answer(pages: Vec<PageSummary>, has_more: bool) -> DocsAnswer {
    DocsAnswer::Pages(PageListResponse { pages, has_more })
}

fn event(fx: &Fx, at_seq: i64, summary: PageSummary, operation: Option<OperationId>) -> Input {
    fx.world.event_at(
        10 + at_seq,
        Event::PageChanged {
            page: summary,
            operation_id: operation,
        },
    )
}

fn list_titles(fx: &Fx, state: PageState) -> Vec<String> {
    fx.driver
        .core
        .page_list(fx.general, state)
        .map(|list| list.pages.iter().map(|p| p.title.clone()).collect())
        .unwrap_or_default()
}

/// Lists the live pages of the general channel and answers with `pages`.
fn load_list(fx: &mut Fx, pages: Vec<PageSummary>) {
    let out = docs(
        fx,
        DocsInput::ListPages {
            conversation: fx.general,
            state: PageState::Live,
        },
    );
    let (id, call) = only_request(&out);
    assert!(matches!(call, DocsCall::ListPages { .. }));
    reply(fx, id, list_answer(pages, false));
}

/// Opens a page and answers its load (and the notations request that follows).
fn open(fx: &mut Fx, detail: &PageDetail) {
    let out = docs(fx, DocsInput::OpenPage(detail.summary.id));
    let (id, call) = only_request(&out);
    assert_eq!(call, DocsCall::GetPage(detail.summary.id));
    let out = reply(fx, id, page_answer(detail));
    if detail.content.kind() == PageKind::Page {
        let (id, call) = only_request(&out);
        assert_eq!(call, DocsCall::ListNotations(detail.summary.id));
        reply(
            fx,
            id,
            DocsAnswer::Notations(NotationListResponse {
                notations: Vec::new(),
            }),
        );
    }
}

fn edit_body(fx: &mut Fx, body: &str) {
    docs(
        fx,
        DocsInput::EditPage {
            title: None,
            content: Some(PageContent::Page { body: body.into() }),
        },
    );
}

/// Saves the open page and takes it through the journal; returns the request that goes out.
fn save(fx: &mut Fx) -> (u64, lazy_protocol::UpdatePageRequest) {
    let out = docs(fx, DocsInput::SavePage);
    assert!(
        requests(&out).is_empty(),
        "nothing is sent before the journal write"
    );
    let entry = persisted(&out).expect("the save is journaled first");
    let out = fx.driver.send(Input::DocsPersisted {
        operation: entry.operation_id,
    });
    let (id, call) = only_request(&out);
    match call {
        DocsCall::UpdatePage { request, .. } => (id, request),
        other => panic!("expected an update, got {other:?}"),
    }
}

fn open_state(fx: &Fx) -> &lazy_client_core::OpenPage {
    fx.driver.core.open_page().expect("a page is open")
}

fn notation(fx: &Fx, page: PageId, id: NotationId, quote: &str, from: u32, n: i64) -> Notation {
    Notation {
        id,
        page_id: page,
        conversation_id: fx.general,
        author_id: fx.me,
        assignee_id: fx.me,
        quote: quote.to_owned(),
        from,
        to: from + quote.chars().count() as u32,
        body: "check".into(),
        created_at: at(n),
        updated_at: at(n),
        completed_at: None,
        resolved_by: None,
        replies: Vec::new(),
    }
}

// ----- Lists -----

#[test]
fn a_list_loads_pages_appends_the_next_page_and_ignores_duplicates() {
    let mut fx = setup();
    let (a, b, c) = (PageId::generate(), PageId::generate(), PageId::generate());
    let out = fx.docs(DocsInput::ListPages {
        conversation: fx.general,
        state: PageState::Live,
    });
    assert!(out.changes.contains(&Change::PageListChanged {
        conversation: fx.general
    }));
    let (id, call) = only_request(&out);
    let DocsCall::ListPages { params, .. } = call else {
        panic!("a list request");
    };
    assert_eq!(params.offset, Some(0));
    assert_eq!(
        fx.driver
            .core
            .page_list(fx.general, PageState::Live)
            .unwrap()
            .load,
        LoadStatus::Loading
    );
    fx.reply(
        id,
        list_answer(vec![summary(&fx, a, "A", 3), summary(&fx, b, "B", 2)], true),
    );
    assert_eq!(list_titles(&fx, PageState::Live), ["A", "B"]);
    assert!(
        fx.driver
            .core
            .page_list(fx.general, PageState::Live)
            .unwrap()
            .has_more
    );

    let out = fx.docs(DocsInput::LoadMorePages {
        conversation: fx.general,
        state: PageState::Live,
    });
    let (id, call) = only_request(&out);
    let DocsCall::ListPages { params, .. } = call else {
        panic!("a list request");
    };
    assert_eq!(params.offset, Some(2));
    // The page shifted while it was out: B comes again.
    fx.reply(
        id,
        list_answer(
            vec![summary(&fx, b, "B", 2), summary(&fx, c, "C", 1)],
            false,
        ),
    );
    assert_eq!(list_titles(&fx, PageState::Live), ["A", "B", "C"]);
    assert!(
        !fx.driver
            .core
            .page_list(fx.general, PageState::Live)
            .unwrap()
            .has_more
    );
}

#[test]
fn an_answer_to_a_list_asked_for_again_is_ignored() {
    let mut fx = setup();
    let a = PageId::generate();
    let ask = |fx: &mut Fx| {
        let out = docs(
            fx,
            DocsInput::ListPages {
                conversation: fx.general,
                state: PageState::Live,
            },
        );
        only_request(&out).0
    };
    let first = ask(&mut fx);
    let second = ask(&mut fx);
    fx.reply(first, list_answer(vec![summary(&fx, a, "Old", 1)], false));
    assert!(list_titles(&fx, PageState::Live).is_empty());
    fx.reply(second, list_answer(vec![summary(&fx, a, "New", 2)], false));
    assert_eq!(list_titles(&fx, PageState::Live), ["New"]);
}

#[test]
fn a_failed_list_says_so_and_keeps_what_it_held() {
    let mut fx = setup();
    let a = PageId::generate();
    fx.load_list(vec![summary(&fx, a, "A", 1)]);
    let out = fx.docs(DocsInput::ListPages {
        conversation: fx.general,
        state: PageState::Live,
    });
    let (id, _) = only_request(&out);
    fx.reply_err(id, unknown());
    let list = fx
        .driver
        .core
        .page_list(fx.general, PageState::Live)
        .unwrap();
    assert!(matches!(
        list.load,
        LoadStatus::Failed {
            can_retry: true,
            ..
        }
    ));
    assert_eq!(list.pages.len(), 1);
}

// ----- Events -----

#[test]
fn events_keep_a_loaded_list_current_and_order_it_by_change() {
    let mut fx = setup();
    let (a, b) = (PageId::generate(), PageId::generate());
    fx.load_list(vec![summary(&fx, a, "A", 1)]);
    // A new page appears, and sorts by its update time.
    let ev = event(&fx, 2, summary(&fx, b, "B", 5), None);
    let out = fx.driver.send(ev);
    assert!(out.changes.contains(&Change::PageListChanged {
        conversation: fx.general
    }));
    assert_eq!(list_titles(&fx, PageState::Live), ["B", "A"]);
    // An edit to A.
    let ev = event(&fx, 3, summary(&fx, a, "A2", 9), None);
    fx.driver.send(ev);
    assert_eq!(list_titles(&fx, PageState::Live), ["A2", "B"]);
    // A late, older copy and a duplicate change nothing.
    let ev = event(&fx, 4, summary(&fx, a, "A1", 1), None);
    fx.driver.send(ev);
    let ev = event(&fx, 5, summary(&fx, a, "A2", 9), None);
    fx.driver.send(ev);
    assert_eq!(list_titles(&fx, PageState::Live), ["A2", "B"]);
}

#[test]
fn a_page_moves_between_the_live_list_and_the_trash_list() {
    let mut fx = setup();
    let a = PageId::generate();
    fx.load_list(vec![summary(&fx, a, "A", 4)]);
    let out = fx.docs(DocsInput::ListPages {
        conversation: fx.general,
        state: PageState::Trashed,
    });
    let (id, _) = only_request(&out);
    fx.reply(id, list_answer(vec![], false));

    // Trashing changes no revision: the event is still applied.
    let ev = event(&fx, 2, trashed(summary(&fx, a, "A", 4)), None);
    fx.driver.send(ev);
    assert!(list_titles(&fx, PageState::Live).is_empty());
    assert_eq!(list_titles(&fx, PageState::Trashed), ["A"]);
    let ev = event(&fx, 3, summary(&fx, a, "A", 4), None);
    fx.driver.send(ev);
    assert_eq!(list_titles(&fx, PageState::Live), ["A"]);
    assert!(list_titles(&fx, PageState::Trashed).is_empty());
}

#[test]
fn a_page_moved_to_another_conversation_leaves_the_list() {
    let mut fx = setup();
    let a = PageId::generate();
    fx.load_list(vec![summary(&fx, a, "A", 1)]);
    let mut moved = summary(&fx, a, "A", 2);
    moved.conversation_id = fx.world.second_channel.id;
    let ev = event(&fx, 2, moved, None);
    fx.driver.send(ev);
    assert!(list_titles(&fx, PageState::Live).is_empty());
}

#[test]
fn a_purged_page_leaves_every_list_and_marks_the_open_page_gone() {
    let mut fx = setup();
    let a = PageId::generate();
    fx.load_list(vec![summary(&fx, a, "A", 1)]);
    fx.open(&detail(&fx, a, "A", 1, "text"));
    let ev = fx.world.event_at(
        10 + 2,
        Event::PageRemoved {
            conversation_id: fx.general,
            page_id: a,
        },
    );
    fx.driver.send(ev);
    assert!(list_titles(&fx, PageState::Live).is_empty());
    assert!(open_state(&fx).gone);
}

#[test]
fn losing_a_conversation_drops_its_pages() {
    let mut fx = setup();
    let a = PageId::generate();
    fx.load_list(vec![summary(&fx, a, "A", 1)]);
    fx.open(&detail(&fx, a, "A", 1, "text"));
    let ev = fx.world.event_at(
        10 + 2,
        Event::MemberRemoved {
            conversation_id: fx.general,
            user_id: fx.me,
        },
    );
    fx.driver.send(ev);
    assert!(
        fx.driver
            .core
            .page_list(fx.general, PageState::Live)
            .is_none()
    );
    assert!(open_state(&fx).gone);
}

// ----- Workspace lists -----

fn workspace_titles(fx: &Fx, state: PageState) -> Vec<String> {
    fx.driver
        .core
        .workspace_pages(fx.world.workspace.id, state)
        .map(|list| list.pages.iter().map(|p| p.title.clone()).collect())
        .unwrap_or_default()
}

fn load_workspace_list(fx: &mut Fx, pages: Vec<PageSummary>, has_more: bool) {
    let out = fx.docs(DocsInput::ListWorkspacePages {
        workspace: fx.world.workspace.id,
        state: PageState::Live,
    });
    assert!(out.changes.contains(&Change::WorkspacePagesChanged {
        workspace: fx.world.workspace.id
    }));
    let (id, call) = only_request(&out);
    let DocsCall::ListWorkspacePages { workspace, params } = call else {
        panic!("a workspace list request");
    };
    assert_eq!(workspace, fx.world.workspace.id);
    assert_eq!(params.state, PageState::Live);
    assert_eq!(params.q, None);
    assert_eq!(params.offset, Some(0));
    fx.reply(id, list_answer(pages, has_more));
}

#[test]
fn a_workspace_list_loads_pages_from_every_conversation_and_pages_on() {
    let mut fx = setup();
    let (a, b, c) = (PageId::generate(), PageId::generate(), PageId::generate());
    let mut elsewhere = summary(&fx, b, "B", 2);
    elsewhere.conversation_id = fx.world.second_channel.id;
    let first = summary(&fx, a, "A", 3);
    load_workspace_list(&mut fx, vec![first, elsewhere], true);
    assert_eq!(workspace_titles(&fx, PageState::Live), ["A", "B"]);
    // A conversation list was never asked for, so none exists.
    assert!(
        fx.driver
            .core
            .page_list(fx.general, PageState::Live)
            .is_none()
    );

    let out = fx.docs(DocsInput::LoadMoreWorkspacePages {
        workspace: fx.world.workspace.id,
        state: PageState::Live,
    });
    let (id, call) = only_request(&out);
    let DocsCall::ListWorkspacePages { params, .. } = call else {
        panic!("a workspace list request");
    };
    assert_eq!(params.offset, Some(2));
    fx.reply(
        id,
        list_answer(
            vec![summary(&fx, b, "B", 2), summary(&fx, c, "C", 1)],
            false,
        ),
    );
    assert_eq!(workspace_titles(&fx, PageState::Live), ["A", "B", "C"]);
    assert!(
        !fx.driver
            .core
            .workspace_pages(fx.world.workspace.id, PageState::Live)
            .unwrap()
            .has_more
    );
}

#[test]
fn events_keep_a_workspace_list_current_across_conversations_and_the_trash() {
    let mut fx = setup();
    let (a, b) = (PageId::generate(), PageId::generate());
    let first = summary(&fx, a, "A", 1);
    load_workspace_list(&mut fx, vec![first], false);
    let out = fx.docs(DocsInput::ListWorkspacePages {
        workspace: fx.world.workspace.id,
        state: PageState::Trashed,
    });
    let (id, _) = only_request(&out);
    fx.reply(id, list_answer(vec![], false));

    // A page made in another conversation appears, and the change is announced.
    let mut other = summary(&fx, b, "B", 5);
    other.conversation_id = fx.world.second_channel.id;
    let ev = event(&fx, 2, other.clone(), None);
    let out = fx.driver.send(ev);
    assert!(out.changes.contains(&Change::WorkspacePagesChanged {
        workspace: fx.world.workspace.id
    }));
    assert_eq!(workspace_titles(&fx, PageState::Live), ["B", "A"]);

    // Moving it keeps it in the workspace list, with its new place.
    let mut moved = summary(&fx, b, "B", 6);
    moved.conversation_id = fx.general;
    let ev = event(&fx, 3, moved, None);
    fx.driver.send(ev);
    let held = fx
        .driver
        .core
        .workspace_pages(fx.world.workspace.id, PageState::Live)
        .unwrap();
    assert_eq!(held.pages[0].conversation_id, fx.general);

    // Trashing moves it to the trash list, and a purge removes it from both.
    let ev = event(&fx, 4, trashed(summary(&fx, b, "B", 6)), None);
    fx.driver.send(ev);
    assert_eq!(workspace_titles(&fx, PageState::Live), ["A"]);
    assert_eq!(workspace_titles(&fx, PageState::Trashed), ["B"]);
    let ev = fx.world.event_at(
        10 + 5,
        Event::PageRemoved {
            conversation_id: fx.general,
            page_id: b,
        },
    );
    fx.driver.send(ev);
    assert!(workspace_titles(&fx, PageState::Trashed).is_empty());
}

#[test]
fn a_workspace_list_answer_older_than_an_event_does_not_bring_back_a_removed_page() {
    let mut fx = setup();
    let (a, b) = (PageId::generate(), PageId::generate());
    let out = fx.docs(DocsInput::ListWorkspacePages {
        workspace: fx.world.workspace.id,
        state: PageState::Live,
    });
    let (id, _) = only_request(&out);
    // While the answer is out, B is purged.
    let ev = fx.world.event_at(
        10 + 2,
        Event::PageRemoved {
            conversation_id: fx.general,
            page_id: b,
        },
    );
    fx.driver.send(ev);
    fx.reply(
        id,
        list_answer(
            vec![summary(&fx, a, "A", 1), summary(&fx, b, "B", 1)],
            false,
        ),
    );
    assert_eq!(workspace_titles(&fx, PageState::Live), ["A"]);
}

#[test]
fn losing_a_conversation_removes_its_pages_from_the_workspace_list() {
    let mut fx = setup();
    let (a, b) = (PageId::generate(), PageId::generate());
    let mut elsewhere = summary(&fx, b, "B", 1);
    elsewhere.conversation_id = fx.world.second_channel.id;
    let first = summary(&fx, a, "A", 2);
    load_workspace_list(&mut fx, vec![first, elsewhere], false);
    let ev = fx.world.event_at(
        10 + 2,
        Event::MemberRemoved {
            conversation_id: fx.general,
            user_id: fx.me,
        },
    );
    fx.driver.send(ev);
    assert_eq!(workspace_titles(&fx, PageState::Live), ["B"]);
}

// ----- Opening and saving -----

#[test]
fn opening_loads_the_page_then_its_notations_and_boards_have_none() {
    let mut fx = setup();
    let a = PageId::generate();
    let d = detail(&fx, a, "Plan", 3, "hello");
    fx.open(&d);
    let page = open_state(&fx);
    assert_eq!(page.title, "Plan");
    assert_eq!(page.content, Some(d.content.clone()));
    assert_eq!(page.save, SaveState::Saved);
    assert_eq!(page.load, LoadStatus::Ready);
    assert_eq!(page.notations_load, LoadStatus::Ready);

    let board = PageId::generate();
    let mut b = detail(&fx, board, "Map", 1, "");
    b.summary.kind = PageKind::Board;
    b.content = PageContent::empty(PageKind::Board);
    let out = fx.docs(DocsInput::OpenPage(board));
    let (id, _) = only_request(&out);
    let out = fx.reply(id, page_answer(&b));
    assert!(requests(&out).is_empty(), "boards take no notations");
}

#[test]
fn a_missing_page_is_gone_without_a_retry() {
    let mut fx = setup();
    let a = PageId::generate();
    let out = fx.docs(DocsInput::OpenPage(a));
    let (id, _) = only_request(&out);
    fx.reply_err(id, rejected(ErrorCode::NotFound));
    let page = open_state(&fx);
    assert!(page.gone);
    assert!(matches!(
        page.load,
        LoadStatus::Failed {
            can_retry: false,
            ..
        }
    ));
}

#[test]
fn a_save_is_journaled_before_it_is_sent_and_sends_only_what_changed() {
    let mut fx = setup();
    let a = PageId::generate();
    fx.open(&detail(&fx, a, "Plan", 3, "hello"));
    fx.edit_body("hello world");
    assert_eq!(open_state(&fx).save, SaveState::Dirty);

    let out = fx.docs(DocsInput::SavePage);
    assert!(requests(&out).is_empty());
    let entry = persisted(&out).expect("journaled");
    let DocJournalAction::Update { page, request } = &entry.action else {
        panic!("an update entry");
    };
    assert_eq!(*page, a);
    assert_eq!(request.base_revision, 3);
    assert_eq!(request.title, None);
    assert_eq!(
        request.content,
        Some(PageContent::Page {
            body: "hello world".into()
        })
    );
    assert_eq!(open_state(&fx).save, SaveState::Saving);

    let out = fx.driver.send(Input::DocsPersisted {
        operation: entry.operation_id,
    });
    let (id, call) = only_request(&out);
    assert!(matches!(call, DocsCall::UpdatePage { .. }));
    let saved = detail(&fx, a, "Plan", 4, "hello world");
    let out = fx.reply(id, page_answer(&saved));
    assert_eq!(settled(&out), [entry.operation_id]);
    let page = open_state(&fx);
    assert_eq!(page.save, SaveState::Saved);
    assert_eq!(page.base.as_ref().unwrap().summary.revision, 4);
}

#[test]
fn saving_with_no_changes_sends_nothing() {
    let mut fx = setup();
    let a = PageId::generate();
    fx.open(&detail(&fx, a, "Plan", 3, "hello"));
    fx.edit_body("changed");
    fx.edit_body("hello");
    assert_eq!(open_state(&fx).save, SaveState::Saved);
    let out = fx.docs(DocsInput::SavePage);
    assert!(out.effects.is_empty());
}

#[test]
fn edits_made_while_a_save_is_out_go_in_the_next_save_with_the_new_revision() {
    let mut fx = setup();
    let a = PageId::generate();
    fx.open(&detail(&fx, a, "Plan", 3, "one"));
    fx.edit_body("two");
    let (first, request) = fx.save();
    assert_eq!(request.base_revision, 3);
    // More typing, and a second save request, while the first is out: nothing else is sent.
    fx.edit_body("three");
    let out = fx.docs(DocsInput::SavePage);
    assert!(out.effects.is_empty());
    assert_eq!(open_state(&fx).save, SaveState::Saving);

    let out = fx.reply(first, page_answer(&detail(&fx, a, "Plan", 4, "two")));
    // The first save is answered; what was typed since is journaled for the next one.
    let entry = persisted(&out).expect("a second save");
    let DocJournalAction::Update { request, .. } = entry.action else {
        panic!("an update");
    };
    assert_eq!(request.base_revision, 4);
    assert_eq!(
        request.content,
        Some(PageContent::Page {
            body: "three".into()
        })
    );
}

#[test]
fn a_title_is_normalized_and_the_working_title_follows_the_server() {
    let mut fx = setup();
    let a = PageId::generate();
    fx.open(&detail(&fx, a, "Plan", 1, "x"));
    fx.docs(DocsInput::EditPage {
        title: Some("  ".into()),
        content: None,
    });
    let (id, request) = fx.save();
    assert_eq!(request.title.as_deref(), Some("Untitled"));
    fx.reply(id, page_answer(&detail(&fx, a, "Untitled", 2, "x")));
    let page = open_state(&fx);
    assert_eq!(page.title, "Untitled");
    assert_eq!(page.save, SaveState::Saved);
}

#[test]
fn a_save_whose_outcome_is_unknown_is_sent_again_with_the_same_operation() {
    let mut fx = setup();
    let a = PageId::generate();
    fx.open(&detail(&fx, a, "Plan", 3, "one"));
    fx.edit_body("two");
    let (mut id, request) = fx.save();
    let mut attempts = 0;
    loop {
        let out = fx.reply_err(id, unknown());
        match requests(&out).pop() {
            Some((next, DocsCall::UpdatePage { request: again, .. })) => {
                assert_eq!(again.operation_id, request.operation_id);
                assert_eq!(again, request);
                id = next;
                attempts += 1;
            }
            Some(other) => panic!("unexpected {other:?}"),
            None => break,
        }
    }
    assert_eq!(attempts, lazy_client_core::MAX_DOC_RESENDS);
    assert!(matches!(open_state(&fx).save, SaveState::Failed(_)));
    // The person tries again: the same operation, not a new one.
    let out = fx.docs(DocsInput::SavePage);
    assert!(persisted(&out).is_none(), "the entry is already journaled");
    let (id, call) = only_request(&out);
    let DocsCall::UpdatePage { request: again, .. } = call else {
        panic!("an update");
    };
    assert_eq!(again.operation_id, request.operation_id);
    // This time the first attempt turns out to have committed: the server returns its result.
    fx.reply(id, page_answer(&detail(&fx, a, "Plan", 4, "two")));
    assert_eq!(open_state(&fx).save, SaveState::Saved);
}

#[test]
fn a_refused_save_settles_the_journal_and_says_why() {
    let mut fx = setup();
    let a = PageId::generate();
    fx.open(&detail(&fx, a, "Plan", 3, "one"));
    fx.edit_body("two");
    let (id, request) = fx.save();
    let out = fx.reply_err(id, rejected(ErrorCode::Forbidden));
    assert_eq!(settled(&out), [request.operation_id]);
    assert!(matches!(open_state(&fx).save, SaveState::Failed(_)));
    // A new save is a new operation.
    let out = fx.docs(DocsInput::SavePage);
    let entry = persisted(&out).expect("journaled again");
    assert_ne!(entry.operation_id, request.operation_id);
}

#[test]
fn an_unjournaled_save_is_not_sent() {
    let mut fx = setup();
    let a = PageId::generate();
    fx.open(&detail(&fx, a, "Plan", 3, "one"));
    fx.edit_body("two");
    let out = fx.docs(DocsInput::SavePage);
    let entry = persisted(&out).unwrap();
    let out = fx.driver.send(Input::DocsPersistFailed {
        operation: entry.operation_id,
        reason: "disk full".into(),
    });
    assert!(requests(&out).is_empty());
    assert!(matches!(open_state(&fx).save, SaveState::Failed(_)));
    assert!(open_state(&fx).has_edits(), "the edits are kept");
}

#[test]
fn a_trashed_page_cannot_be_edited() {
    let mut fx = setup();
    let a = PageId::generate();
    let mut d = detail(&fx, a, "Plan", 3, "one");
    d.summary = trashed(d.summary);
    fx.open(&d);
    fx.edit_body("two");
    assert_eq!(open_state(&fx).save, SaveState::Saved);
    assert!(!open_state(&fx).has_edits());
}

// ----- Conflicts -----

fn conflict_fixture() -> (Fx, PageId) {
    let mut fx = setup();
    let a = PageId::generate();
    fx.open(&detail(&fx, a, "Plan", 3, "mine base"));
    fx.edit_body("my edit");
    let (id, request) = fx.save();
    let out = fx.reply_err(id, rejected(ErrorCode::StaleRevision));
    assert_eq!(settled(&out), [request.operation_id]);
    let (id, call) = only_request(&out);
    assert_eq!(call, DocsCall::GetPage(a));
    fx.reply(id, page_answer(&detail(&fx, a, "Plan", 5, "their edit")));
    (fx, a)
}

#[test]
fn a_stale_save_fetches_the_newer_page_and_waits_for_a_choice() {
    let (fx, _) = conflict_fixture();
    let page = open_state(&fx);
    assert_eq!(page.save, SaveState::Conflict);
    assert_eq!(page.conflict.as_ref().unwrap().summary.revision, 5);
    assert_eq!(
        page.content,
        Some(PageContent::Page {
            body: "my edit".into()
        })
    );
}

#[test]
fn taking_theirs_replaces_the_working_copy() {
    let (mut fx, _) = conflict_fixture();
    let out = fx.docs(DocsInput::ResolveConflict(ConflictChoice::TakeTheirs));
    assert!(out.effects.is_empty());
    let page = open_state(&fx);
    assert_eq!(page.save, SaveState::Saved);
    assert_eq!(
        page.content,
        Some(PageContent::Page {
            body: "their edit".into()
        })
    );
    assert_eq!(page.base.as_ref().unwrap().summary.revision, 5);
}

#[test]
fn keeping_mine_saves_over_the_newer_revision() {
    let (mut fx, a) = conflict_fixture();
    let out = fx.docs(DocsInput::ResolveConflict(ConflictChoice::KeepMine));
    let entry = persisted(&out).expect("a new save");
    let DocJournalAction::Update { page, request } = entry.action else {
        panic!("an update");
    };
    assert_eq!(page, a);
    assert_eq!(request.base_revision, 5);
    assert_eq!(
        request.content,
        Some(PageContent::Page {
            body: "my edit".into()
        })
    );
}

#[test]
fn a_repeated_save_answered_with_a_page_that_moved_on_is_a_conflict_not_an_overwrite() {
    let mut fx = setup();
    let a = PageId::generate();
    fx.open(&detail(&fx, a, "Plan", 3, "one"));
    fx.edit_body("mine");
    let (id, _) = fx.save();
    // The first attempt committed (revision 4) and someone then edited (revision 5); the repeat
    // is answered with the page as it is now.
    fx.reply(
        id,
        page_answer(&detail(&fx, a, "Plan", 5, "theirs after mine")),
    );
    let page = open_state(&fx);
    assert_eq!(page.save, SaveState::Conflict);
    assert_eq!(page.conflict.as_ref().unwrap().summary.revision, 5);
    assert_eq!(
        page.content,
        Some(PageContent::Page {
            body: "mine".into()
        })
    );
}

#[test]
fn a_save_answered_with_a_later_revision_of_the_same_text_is_just_saved() {
    let mut fx = setup();
    let a = PageId::generate();
    fx.open(&detail(&fx, a, "Plan", 3, "one"));
    fx.edit_body("mine");
    let (id, _) = fx.save();
    // The text is ours; the revision is whatever it is now (a file or a move came after).
    let out = fx.reply(id, page_answer(&detail(&fx, a, "Plan", 6, "mine")));
    assert!(requests(&out).is_empty());
    let page = open_state(&fx);
    assert_eq!(page.save, SaveState::Saved);
    assert_eq!(page.base.as_ref().unwrap().summary.revision, 6);
}

#[test]
fn a_field_nobody_typed_in_follows_the_page_after_a_save() {
    let mut fx = setup();
    let a = PageId::generate();
    fx.open(&detail(&fx, a, "Plan", 3, "one"));
    fx.edit_body("mine");
    let (id, _) = fx.save();
    // Someone else renamed the page; the answer carries our text and their title.
    fx.reply(id, page_answer(&detail(&fx, a, "Renamed", 4, "mine")));
    let page = open_state(&fx);
    assert_eq!(page.title, "Renamed");
    assert_eq!(page.save, SaveState::Saved);
}

#[test]
fn a_file_added_by_someone_else_changes_no_revision_but_is_picked_up() {
    let mut fx = setup();
    let a = PageId::generate();
    fx.open(&detail(&fx, a, "Plan", 3, "one"));
    fx.edit_body("typing");
    let mut with_file = summary(&fx, a, "Plan", 3);
    with_file.file_count = 1;
    let ev = event(&fx, 2, with_file.clone(), None);
    let out = fx.driver.send(ev);
    // The count shows at once; the file list is fetched.
    assert_eq!(open_state(&fx).base.as_ref().unwrap().summary.file_count, 1);
    let (id, call) = only_request(&out);
    assert_eq!(call, DocsCall::GetPage(a));
    let mut d = detail(&fx, a, "Plan", 3, "one");
    d.summary = with_file;
    d.files = vec![lazy_protocol::Attachment {
        id: lazy_domain::AttachmentId::generate(),
        name: "a.txt".into(),
        media_type: "text/plain".into(),
        size: 3,
        sha256: "00".into(),
        width: None,
        height: None,
        has_thumbnail: false,
    }];
    fx.reply(id, page_answer(&d));
    let page = open_state(&fx);
    assert_eq!(page.base.as_ref().unwrap().files.len(), 1);
    // What was being typed is untouched.
    assert_eq!(
        page.content,
        Some(PageContent::Page {
            body: "typing".into()
        })
    );
    assert_eq!(page.save, SaveState::Dirty);
}

// ----- Other people's changes -----

#[test]
fn another_persons_change_to_a_clean_open_page_loads_it() {
    let mut fx = setup();
    let a = PageId::generate();
    fx.open(&detail(&fx, a, "Plan", 3, "one"));
    let ev = event(
        &fx,
        2,
        summary(&fx, a, "Plan", 4),
        Some(OperationId::generate()),
    );
    let out = fx.driver.send(ev);
    let (id, call) = only_request(&out);
    assert_eq!(call, DocsCall::GetPage(a));
    assert_eq!(open_state(&fx).newer_revision, Some(4));
    fx.reply(id, page_answer(&detail(&fx, a, "Plan", 4, "one and two")));
    let page = open_state(&fx);
    assert_eq!(page.newer_revision, None);
    assert_eq!(
        page.content,
        Some(PageContent::Page {
            body: "one and two".into()
        })
    );
}

#[test]
fn another_persons_change_does_not_overwrite_unsaved_edits() {
    let mut fx = setup();
    let a = PageId::generate();
    fx.open(&detail(&fx, a, "Plan", 3, "one"));
    fx.edit_body("mine");
    let ev = event(
        &fx,
        2,
        summary(&fx, a, "Plan", 4),
        Some(OperationId::generate()),
    );
    let out = fx.driver.send(ev);
    assert!(requests(&out).is_empty());
    let page = open_state(&fx);
    assert_eq!(page.newer_revision, Some(4));
    assert_eq!(
        page.content,
        Some(PageContent::Page {
            body: "mine".into()
        })
    );
    assert_eq!(page.base.as_ref().unwrap().summary.revision, 3);
}

#[test]
fn the_echo_of_our_own_save_does_not_reload_the_page() {
    let mut fx = setup();
    let a = PageId::generate();
    fx.open(&detail(&fx, a, "Plan", 3, "one"));
    fx.edit_body("two");
    let (id, request) = fx.save();
    // The event can arrive before the answer.
    let ev = event(
        &fx,
        2,
        summary(&fx, a, "Plan", 4),
        Some(request.operation_id),
    );
    let out = fx.driver.send(ev);
    assert!(requests(&out).is_empty(), "no refetch for our own change");
    fx.reply(id, page_answer(&detail(&fx, a, "Plan", 4, "two")));
    assert_eq!(open_state(&fx).save, SaveState::Saved);
}

// ----- Trash and the order of answers -----

#[test]
fn a_late_trash_answer_does_not_undo_a_restore_that_came_by_event() {
    let mut fx = setup();
    let a = PageId::generate();
    fx.load_list(vec![summary(&fx, a, "A", 4)]);
    let out = fx.docs(DocsInput::TrashPage(a));
    let (trash_id, _) = only_request(&out);
    // Someone trashed it and someone restored it, in log order, before our answer arrived.
    let ev = event(&fx, 2, trashed(summary(&fx, a, "A", 4)), None);
    fx.driver.send(ev);
    let ev = event(&fx, 3, summary(&fx, a, "A", 4), None);
    fx.driver.send(ev);
    assert_eq!(list_titles(&fx, PageState::Live), ["A"]);
    // The answer to our trash is older than the restore.
    let answer = PageDetail {
        summary: trashed(summary(&fx, a, "A", 4)),
        content: PageContent::Page {
            body: String::new(),
        },
        files: Vec::new(),
    };
    fx.reply(trash_id, page_answer(&answer));
    assert_eq!(list_titles(&fx, PageState::Live), ["A"]);
}

#[test]
fn a_trash_answer_with_no_events_meanwhile_is_applied() {
    let mut fx = setup();
    let a = PageId::generate();
    fx.load_list(vec![summary(&fx, a, "A", 4)]);
    let out = fx.docs(DocsInput::TrashPage(a));
    let (id, call) = only_request(&out);
    assert!(matches!(call, DocsCall::TrashPage { .. }));
    let answer = PageDetail {
        summary: trashed(summary(&fx, a, "A", 4)),
        content: PageContent::Page {
            body: String::new(),
        },
        files: Vec::new(),
    };
    fx.reply(id, page_answer(&answer));
    assert!(list_titles(&fx, PageState::Live).is_empty());
}

#[test]
fn a_list_answer_older_than_an_event_does_not_bring_back_what_the_event_removed() {
    let mut fx = setup();
    let (a, b) = (PageId::generate(), PageId::generate());
    let out = fx.docs(DocsInput::ListPages {
        conversation: fx.general,
        state: PageState::Live,
    });
    let (id, _) = only_request(&out);
    // While the list is out, B is purged and A changes.
    let ev = fx.world.event_at(
        10 + 2,
        Event::PageRemoved {
            conversation_id: fx.general,
            page_id: b,
        },
    );
    fx.driver.send(ev);
    let ev = event(&fx, 3, summary(&fx, a, "A new", 9), None);
    fx.driver.send(ev);
    fx.reply(
        id,
        list_answer(
            vec![summary(&fx, a, "A old", 1), summary(&fx, b, "B", 1)],
            false,
        ),
    );
    assert_eq!(list_titles(&fx, PageState::Live), ["A new"]);
}

#[test]
fn a_purge_answer_removes_the_page() {
    let mut fx = setup();
    let a = PageId::generate();
    fx.load_list(vec![summary(&fx, a, "A", 1)]);
    let out = fx.docs(DocsInput::PurgePage(a));
    let (id, call) = only_request(&out);
    assert!(matches!(call, DocsCall::PurgePage { .. }));
    fx.reply(id, DocsAnswer::Done);
    assert!(list_titles(&fx, PageState::Live).is_empty());
}

#[test]
fn an_unanswered_trash_is_sent_again_and_then_waits_for_the_server() {
    let mut fx = setup();
    let a = PageId::generate();
    fx.load_list(vec![summary(&fx, a, "A", 1)]);
    let out = fx.docs(DocsInput::TrashPage(a));
    let (mut id, first) = only_request(&out);
    let DocsCall::TrashPage {
        request: first_request,
        ..
    } = first
    else {
        panic!("a trash");
    };
    for _ in 0..lazy_client_core::MAX_DOC_RESENDS {
        let out = fx.reply_err(id, unknown());
        let (next, call) = only_request(&out);
        let DocsCall::TrashPage { request, .. } = call else {
            panic!("a trash");
        };
        assert_eq!(request.operation_id, first_request.operation_id);
        id = next;
    }
    let out = fx.reply_err(id, unknown());
    assert!(requests(&out).is_empty());
    assert!(fx.driver.core.docs_notice().is_some());
    // Any answer from the server sends the waiting change again, as it was.
    let out = fx.docs(DocsInput::ListPages {
        conversation: fx.general,
        state: PageState::Trashed,
    });
    let (list_id, _) = only_request(&out);
    let out = fx.reply(list_id, list_answer(vec![], false));
    let (_, call) = only_request(&out);
    let DocsCall::TrashPage { request, .. } = call else {
        panic!("a trash");
    };
    assert_eq!(request.operation_id, first_request.operation_id);
}

// ----- Creating -----

#[test]
fn a_create_is_journaled_first_then_listed_and_ready_to_open() {
    let mut fx = setup();
    let a = PageId::generate();
    fx.load_list(vec![]);
    let out = fx.docs(DocsInput::CreatePage {
        page: a,
        conversation: fx.general,
        kind: PageKind::Page,
        title: "  Notes ".into(),
        content: None,
    });
    assert!(requests(&out).is_empty());
    let entry = persisted(&out).expect("journaled");
    let DocJournalAction::Create { request, .. } = &entry.action else {
        panic!("a create");
    };
    assert_eq!(request.title, "Notes");
    assert_eq!(request.page_id, a);
    assert_eq!(fx.driver.core.create_state(a), Some(&CreateState::InFlight));

    let out = fx.driver.send(Input::DocsPersisted {
        operation: entry.operation_id,
    });
    let (id, call) = only_request(&out);
    assert!(matches!(call, DocsCall::CreatePage { .. }));
    let made = detail(&fx, a, "Notes", 1, "");
    let out = fx.reply(id, page_answer(&made));
    assert_eq!(settled(&out), [entry.operation_id]);
    assert!(out.changes.contains(&Change::PageCreated { page: a }));
    assert_eq!(fx.driver.core.create_state(a), None);
    assert_eq!(list_titles(&fx, PageState::Live), ["Notes"]);

    // Opening it shows the page at once; the load only confirms it.
    fx.docs(DocsInput::OpenPage(a));
    assert_eq!(open_state(&fx).load, LoadStatus::Ready);
    assert_eq!(open_state(&fx).title, "Notes");
}

/// Creates a page and takes it through the journal; returns its operation and the create request.
fn start_create(fx: &mut Fx, page: PageId) -> (OperationId, u64) {
    let out = fx.docs(DocsInput::CreatePage {
        page,
        conversation: fx.general,
        kind: PageKind::Page,
        title: "Notes".into(),
        content: None,
    });
    let entry = persisted(&out).unwrap();
    let out = fx.driver.send(Input::DocsPersisted {
        operation: entry.operation_id,
    });
    (entry.operation_id, only_request(&out).0)
}

#[test]
fn an_unknown_create_is_settled_by_asking_whether_the_page_exists() {
    let mut fx = setup();
    let a = PageId::generate();
    load_list(&mut fx, vec![]);
    let (operation, id) = start_create(&mut fx, a);
    // The answer was lost: the core asks for the page, not for the status of the operation.
    let out = fx.reply_err(id, unknown());
    let (check, call) = only_request(&out);
    assert_eq!(call, DocsCall::GetPage(a));
    // It committed: the page is there, and the journal is settled with no second create.
    let out = fx.reply(check, page_answer(&detail(&fx, a, "Notes", 1, "")));
    assert_eq!(settled(&out), [operation]);
    assert!(out.changes.contains(&Change::PageCreated { page: a }));
    assert_eq!(list_titles(&fx, PageState::Live), ["Notes"]);
    assert_eq!(fx.driver.core.create_state(a), None);
}

#[test]
fn an_unknown_create_whose_page_is_not_there_is_sent_again_as_the_same_operation() {
    let mut fx = setup();
    let a = PageId::generate();
    let (operation, id) = start_create(&mut fx, a);
    let out = fx.reply_err(id, unknown());
    let (check, _) = only_request(&out);
    let out = fx.reply_err(check, rejected(ErrorCode::NotFound));
    let (_, call) = only_request(&out);
    let DocsCall::CreatePage { request, .. } = call else {
        panic!("the create goes again");
    };
    assert_eq!(request.operation_id, operation);
    assert_eq!(request.page_id, a);
}

#[test]
fn a_create_that_cannot_be_settled_waits_for_the_person_and_retries_the_same_operation() {
    let mut fx = setup();
    let a = PageId::generate();
    let (operation, mut id) = start_create(&mut fx, a);
    // The server stays unreachable: create, check, check, ... until the core gives up.
    let mut asked = 0;
    loop {
        let out = fx.reply_err(id, unknown());
        match requests(&out).pop() {
            Some((next, _)) => {
                id = next;
                asked += 1;
            }
            None => break,
        }
    }
    assert_eq!(asked, lazy_client_core::MAX_DOC_RESENDS);
    assert!(matches!(
        fx.driver.core.create_state(a),
        Some(CreateState::Failed(_))
    ));
    let out = fx.docs(DocsInput::RetryCreate(a));
    let (_, call) = only_request(&out);
    let DocsCall::CreatePage { request, .. } = call else {
        panic!("a create");
    };
    assert_eq!(request.operation_id, operation);
    assert!(persisted(&out).is_none(), "already journaled");
}

#[test]
fn a_refused_create_is_settled_and_a_retry_is_a_new_operation() {
    let mut fx = setup();
    let a = PageId::generate();
    let out = fx.docs(DocsInput::CreatePage {
        page: a,
        conversation: fx.general,
        kind: PageKind::Board,
        title: "Map".into(),
        content: None,
    });
    let entry = persisted(&out).unwrap();
    let out = fx.driver.send(Input::DocsPersisted {
        operation: entry.operation_id,
    });
    let (id, _) = only_request(&out);
    let out = fx.reply_err(id, rejected(ErrorCode::Forbidden));
    assert_eq!(settled(&out), [entry.operation_id]);
    assert!(matches!(
        fx.driver.core.create_state(a),
        Some(CreateState::Failed(_))
    ));
    let out = fx.docs(DocsInput::RetryCreate(a));
    let again = persisted(&out).expect("journaled anew");
    assert_ne!(again.operation_id, entry.operation_id);
}

#[test]
fn content_of_the_wrong_kind_is_refused_before_anything_is_journaled() {
    let mut fx = setup();
    let out = fx.docs(DocsInput::CreatePage {
        page: PageId::generate(),
        conversation: fx.general,
        kind: PageKind::Board,
        title: "Map".into(),
        content: Some(PageContent::Page { body: "x".into() }),
    });
    assert!(out.effects.is_empty());
    assert!(fx.driver.core.docs_notice().is_some());
}

// ----- Versions -----

#[test]
fn versions_are_listed_previewed_and_restored() {
    let mut fx = setup();
    let a = PageId::generate();
    fx.open(&detail(&fx, a, "Plan", 5, "now"));
    let out = fx.docs(DocsInput::LoadVersions);
    let (id, call) = only_request(&out);
    assert_eq!(call, DocsCall::ListVersions(a));
    let me = fx.me;
    let v = move |n: u32| PageVersionSummary {
        page_id: a,
        version: n,
        title: "Plan".into(),
        reason: VersionReason::Autosave,
        created_by: Some(me),
        created_at: at(i64::from(n)),
        revision: n,
    };
    fx.reply(
        id,
        DocsAnswer::Versions(PageVersionsResponse {
            versions: vec![v(2), v(1)],
        }),
    );
    assert_eq!(open_state(&fx).versions.versions.len(), 2);

    let out = fx.docs(DocsInput::PreviewVersion(1));
    let (id, _) = only_request(&out);
    fx.reply(
        id,
        DocsAnswer::Version(PageVersionResponse {
            version: PageVersionDetail {
                summary: v(1),
                content: PageContent::Page {
                    body: "then".into(),
                },
            },
        }),
    );
    assert!(open_state(&fx).versions.preview.is_some());
    fx.docs(DocsInput::ClosePreview);
    assert!(open_state(&fx).versions.preview.is_none());

    let out = fx.docs(DocsInput::RestoreVersion(1));
    let (id, call) = only_request(&out);
    let DocsCall::RestoreVersion {
        version, request, ..
    } = call
    else {
        panic!("a restore");
    };
    assert_eq!((version, request.base_revision), (1, 5));
    let out = fx.reply(id, page_answer(&detail(&fx, a, "Plan", 6, "then")));
    assert_eq!(
        open_state(&fx).content,
        Some(PageContent::Page {
            body: "then".into()
        })
    );
    // The restore added a version, so the list is asked for again.
    assert_eq!(only_request(&out).1, DocsCall::ListVersions(a));
}

#[test]
fn a_restore_is_refused_while_there_are_unsaved_edits() {
    let mut fx = setup();
    let a = PageId::generate();
    fx.open(&detail(&fx, a, "Plan", 5, "now"));
    fx.edit_body("typing");
    let out = fx.docs(DocsInput::RestoreVersion(1));
    assert!(requests(&out).is_empty());
    assert!(fx.driver.core.docs_notice().is_some());
}

// ----- Search -----

#[test]
fn a_search_replaces_its_results_and_ignores_a_superseded_answer() {
    let mut fx = setup();
    let (a, b) = (PageId::generate(), PageId::generate());
    let ws = fx.world.workspace.id;
    let out = fx.docs(DocsInput::SearchPages {
        workspace: ws,
        text: "road".into(),
    });
    let (first, call) = only_request(&out);
    let DocsCall::ListWorkspacePages { params, .. } = call else {
        panic!("a workspace list");
    };
    assert_eq!(params.q.as_deref(), Some("road"));
    let out = fx.docs(DocsInput::SearchPages {
        workspace: ws,
        text: "roadmap".into(),
    });
    let (second, _) = only_request(&out);
    fx.reply(first, list_answer(vec![summary(&fx, a, "Old", 1)], false));
    assert!(fx.driver.core.page_search().unwrap().results.is_empty());
    fx.reply(
        second,
        list_answer(vec![summary(&fx, b, "Roadmap", 1)], false),
    );
    assert_eq!(
        fx.driver.core.page_search().unwrap().results[0].title,
        "Roadmap"
    );
    // A trashed page leaves the results.
    let ev = event(&fx, 2, trashed(summary(&fx, b, "Roadmap", 1)), None);
    fx.driver.send(ev);
    assert!(fx.driver.core.page_search().unwrap().results.is_empty());
    // Empty text clears the search.
    fx.docs(DocsInput::SearchPages {
        workspace: ws,
        text: " ".into(),
    });
    assert!(fx.driver.core.page_search().is_none());
}

// ----- Notations -----

#[test]
fn notations_load_with_the_page_and_follow_events() {
    let mut fx = setup();
    let a = PageId::generate();
    let d = detail(&fx, a, "Plan", 1, "alpha beta gamma");
    let out = fx.docs(DocsInput::OpenPage(a));
    let (id, _) = only_request(&out);
    let out = fx.reply(id, page_answer(&d));
    let (nid, _) = only_request(&out);
    let n1 = notation(&fx, a, NotationId::generate(), "beta", 6, 1);
    fx.reply(
        nid,
        DocsAnswer::Notations(NotationListResponse {
            notations: vec![n1.clone()],
        }),
    );
    assert_eq!(open_state(&fx).notations.len(), 1);

    // A reply arrives by event: the notation is replaced, and the count is the server's.
    let mut with_reply = n1.clone();
    with_reply.updated_at = at(9);
    with_reply.body = "edited".into();
    let ev = fx.world.event_at(
        10 + 2,
        Event::NotationChanged {
            notation: with_reply.clone(),
            open_notations: 1,
        },
    );
    fx.driver.send(ev);
    assert_eq!(open_state(&fx).notations[0].body, "edited");
    // An older copy does not replace it.
    let ev = fx.world.event_at(
        10 + 3,
        Event::NotationChanged {
            notation: n1.clone(),
            open_notations: 1,
        },
    );
    fx.driver.send(ev);
    assert_eq!(open_state(&fx).notations[0].body, "edited");
    assert_eq!(
        open_state(&fx)
            .base
            .as_ref()
            .unwrap()
            .summary
            .open_notations,
        1
    );

    // Where the quote is now, after text was added in front of it.
    fx.docs(DocsInput::EditPage {
        title: None,
        content: Some(PageContent::Page {
            body: "new alpha beta gamma".into(),
        }),
    });
    let n = open_state(&fx).notations[0].clone();
    assert_eq!(fx.driver.core.notation_anchor(&n), Some((10, 14)));
    fx.docs(DocsInput::EditPage {
        title: None,
        content: Some(PageContent::Page {
            body: "alpha".into(),
        }),
    });
    assert_eq!(fx.driver.core.notation_anchor(&n), None);

    // Removed.
    let ev = fx.world.event_at(
        10 + 4,
        Event::NotationRemoved {
            conversation_id: fx.general,
            page_id: a,
            notation_id: n.id,
            open_notations: 0,
        },
    );
    fx.driver.send(ev);
    assert!(open_state(&fx).notations.is_empty());
    assert_eq!(
        open_state(&fx)
            .base
            .as_ref()
            .unwrap()
            .summary
            .open_notations,
        0
    );
}

#[test]
fn notation_actions_send_one_operation_each_and_apply_the_answer() {
    let mut fx = setup();
    let a = PageId::generate();
    fx.open(&detail(&fx, a, "Plan", 1, "alpha beta gamma"));
    let id = NotationId::generate();
    let out = fx.docs(DocsInput::CreateNotation {
        notation: id,
        quote: "beta".into(),
        from: 6,
        to: 10,
        body: "look".into(),
        assignee: None,
    });
    let (req, call) = only_request(&out);
    assert!(matches!(call, DocsCall::CreateNotation { .. }));
    let made = notation(&fx, a, id, "beta", 6, 1);
    fx.reply(
        req,
        DocsAnswer::Notation(NotationResponse {
            notation: made.clone(),
        }),
    );
    assert_eq!(open_state(&fx).notations.len(), 1);

    let out = fx.docs(DocsInput::SetNotationCompleted {
        notation: id,
        completed: true,
    });
    let (req, call) = only_request(&out);
    assert!(matches!(call, DocsCall::SetNotationCompleted { .. }));
    let mut done = made.clone();
    done.completed_at = Some(at(5));
    done.updated_at = at(5);
    fx.reply(
        req,
        DocsAnswer::Notation(NotationResponse { notation: done }),
    );
    assert!(open_state(&fx).notations[0].completed_at.is_some());

    let out = fx.docs(DocsInput::ReplyToNotation {
        notation: id,
        reply: NotationReplyId::generate(),
        body: "  ".into(),
    });
    assert!(out.effects.is_empty(), "an empty reply is not sent");
}

#[test]
fn a_bad_anchor_or_a_board_is_refused_before_it_is_sent() {
    let mut fx = setup();
    let a = PageId::generate();
    fx.open(&detail(&fx, a, "Plan", 1, "short"));
    let out = fx.docs(DocsInput::CreateNotation {
        notation: NotationId::generate(),
        quote: "short".into(),
        from: 0,
        to: 99,
        body: "x".into(),
        assignee: None,
    });
    assert!(out.effects.is_empty());
    assert!(fx.driver.core.docs_notice().is_some());
}

#[test]
fn the_inbox_keeps_the_person_s_notations_current() {
    let mut fx = setup();
    let a = PageId::generate();
    let ws = fx.world.workspace.id;
    let mine = notation(&fx, a, NotationId::generate(), "x", 0, 1);
    let out = fx.docs(DocsInput::LoadNotationInbox {
        workspace: ws,
        scope: NotationScope::Assigned,
        completed: false,
    });
    let (id, _) = only_request(&out);
    fx.reply(
        id,
        DocsAnswer::Notations(NotationListResponse {
            notations: vec![mine.clone()],
        }),
    );
    assert_eq!(fx.driver.core.notation_inbox().unwrap().notations.len(), 1);

    // A new one assigned to me arrives.
    let other = notation(&fx, a, NotationId::generate(), "y", 2, 2);
    let ev = fx.world.event_at(
        10 + 2,
        Event::NotationChanged {
            notation: other.clone(),
            open_notations: 2,
        },
    );
    fx.driver.send(ev);
    assert_eq!(fx.driver.core.notation_inbox().unwrap().notations.len(), 2);
    // One assigned to someone else is not mine.
    let mut theirs = notation(&fx, a, NotationId::generate(), "z", 4, 3);
    theirs.assignee_id = fx.world.other.id;
    let ev = fx.world.event_at(
        10 + 3,
        Event::NotationChanged {
            notation: theirs,
            open_notations: 3,
        },
    );
    fx.driver.send(ev);
    assert_eq!(fx.driver.core.notation_inbox().unwrap().notations.len(), 2);
    // Completing one takes it out of the open list.
    let mut done = other.clone();
    done.completed_at = Some(at(9));
    done.updated_at = at(9);
    let ev = fx.world.event_at(
        10 + 4,
        Event::NotationChanged {
            notation: done,
            open_notations: 2,
        },
    );
    fx.driver.send(ev);
    assert_eq!(fx.driver.core.notation_inbox().unwrap().notations.len(), 1);
}

// ----- Files -----

#[test]
fn a_file_added_to_the_open_page_is_listed_when_uploaded() {
    use lazy_client_core::{FileBytes, FileSource};
    let mut fx = setup();
    let a = PageId::generate();
    fx.open(&detail(&fx, a, "Plan", 1, "x"));
    let file = lazy_protocol::Attachment {
        id: lazy_domain::AttachmentId::generate(),
        name: "a.txt".into(),
        media_type: "text/plain".into(),
        size: 3,
        sha256: "00".into(),
        width: None,
        height: None,
        has_thumbnail: false,
    };
    let out = fx.docs(DocsInput::AddPageFile {
        attachment: file.id,
        source: FileSource::Bytes(FileBytes::new(b"abc".to_vec())),
    });
    let (id, call) = only_request(&out);
    assert!(matches!(call, DocsCall::UploadPageFile { .. }));
    fx.reply(id, DocsAnswer::File(file.clone()));
    let base = open_state(&fx).base.clone().unwrap();
    assert_eq!(base.files, vec![file]);
    assert_eq!(base.summary.file_count, 1);
}

// ----- The journal -----

fn start_with_journal(world: &World, journal: Vec<DocJournalEntry>) -> Driver {
    let mut driver = Driver::new();
    driver.send(Input::Started(StartupState {
        session_restored: true,
        docs_journal: journal,
        ..Default::default()
    }));
    driver.send(Input::BootstrapLoaded(Box::new(world.bootstrap())));
    driver
}

#[test]
fn journaled_creates_and_saves_are_sent_again_after_a_restart_with_their_operations() {
    let world = World::new();
    let general = world.channel.id;
    let me = world.me.id;
    let page = PageId::generate();
    let create = DocJournalEntry {
        operation_id: OperationId::generate(),
        user_id: me,
        submitted_at: at(1),
        action: DocJournalAction::Create {
            conversation: general,
            request: lazy_protocol::CreatePageRequest {
                operation_id: OperationId::generate(),
                page_id: page,
                kind: PageKind::Page,
                title: "Draft".into(),
                content: None,
            },
        },
    };
    let create = DocJournalEntry {
        operation_id: match &create.action {
            DocJournalAction::Create { request, .. } => request.operation_id,
            _ => unreachable!(),
        },
        ..create
    };
    let other_page = PageId::generate();
    let update_request = lazy_protocol::UpdatePageRequest {
        operation_id: OperationId::generate(),
        base_revision: 2,
        title: None,
        content: Some(PageContent::Page {
            body: "kept".into(),
        }),
        append: None,
        conversation_id: None,
    };
    let update = DocJournalEntry {
        operation_id: update_request.operation_id,
        user_id: me,
        submitted_at: at(2),
        action: DocJournalAction::Update {
            page: other_page,
            request: update_request.clone(),
        },
    };
    let foreign = DocJournalEntry {
        user_id: UserId::generate(),
        ..update.clone()
    };
    let mut driver = start_with_journal(&world, vec![update.clone(), create.clone(), foreign]);
    let sent: Vec<DocsCall> = driver
        .effects
        .iter()
        .filter_map(|effect| match effect {
            Effect::Docs(DocsEffect::Request { call, .. }) => Some(call.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(
        sent.len(),
        2,
        "someone else's entry is left alone: {sent:?}"
    );
    assert!(matches!(&sent[0], DocsCall::CreatePage { request, .. } if request.page_id == page));
    assert!(matches!(&sent[1], DocsCall::UpdatePage { request, .. } if *request == update_request));

    // The create committed before the restart: the server returns it, and the journal is settled.
    let id = driver
        .effects
        .iter()
        .find_map(|effect| match effect {
            Effect::Docs(DocsEffect::Request {
                id,
                call: DocsCall::CreatePage { .. },
            }) => Some(*id),
            _ => None,
        })
        .unwrap();
    let mut fx = Fx {
        world,
        driver: std::mem::replace(&mut driver, Driver::new()),
        general,
        me,
    };
    let d = detail(&fx, page, "Draft", 1, "");
    let out = fx.reply(id, page_answer(&d));
    assert_eq!(settled(&out), [create.operation_id]);
}

#[test]
fn a_journaled_save_found_stale_after_a_restart_keeps_the_text() {
    let world = World::new();
    let me = world.me.id;
    let page = PageId::generate();
    let request = lazy_protocol::UpdatePageRequest {
        operation_id: OperationId::generate(),
        base_revision: 2,
        title: Some("T".into()),
        content: Some(PageContent::Page {
            body: "kept".into(),
        }),
        append: None,
        conversation_id: None,
    };
    let entry = DocJournalEntry {
        operation_id: request.operation_id,
        user_id: me,
        submitted_at: at(2),
        action: DocJournalAction::Update { page, request },
    };
    let driver = start_with_journal(&world, vec![entry.clone()]);
    let id = driver
        .effects
        .iter()
        .find_map(|effect| match effect {
            Effect::Docs(DocsEffect::Request { id, .. }) => Some(*id),
            _ => None,
        })
        .unwrap();
    let mut fx = Fx {
        general: world.channel.id,
        me,
        world,
        driver,
    };
    let out = fx.reply_err(id, rejected(ErrorCode::StaleRevision));
    assert_eq!(settled(&out), [entry.operation_id]);
    let edits = fx.driver.core.recovered_edits();
    assert_eq!(edits.len(), 1);
    assert_eq!(edits[0].page, page);
    assert_eq!(
        edits[0].content,
        Some(PageContent::Page {
            body: "kept".into()
        })
    );
    fx.docs(DocsInput::DismissRecoveredEdit(entry.operation_id));
    assert!(fx.driver.core.recovered_edits().is_empty());
}

// ----- Closing, signing out, redaction -----

#[test]
fn closing_a_page_with_edits_saves_them_and_keeps_the_page_until_it_settles() {
    let mut fx = setup();
    let a = PageId::generate();
    fx.open(&detail(&fx, a, "Plan", 1, "one"));
    fx.edit_body("two");
    let out = fx.docs(DocsInput::ClosePage);
    let entry = persisted(&out).expect("saved on close");
    assert!(fx.driver.core.open_page().is_none());
    let out = fx.driver.send(Input::DocsPersisted {
        operation: entry.operation_id,
    });
    let (id, _) = only_request(&out);
    let out = fx.reply(id, page_answer(&detail(&fx, a, "Plan", 2, "two")));
    assert_eq!(settled(&out), [entry.operation_id]);
    // Reopening shows the saved page.
    fx.docs(DocsInput::OpenPage(a));
    assert_eq!(open_state(&fx).save, SaveState::Saved);
}

#[test]
fn unsaved_edits_survive_closing_and_reopening_a_page() {
    let mut fx = setup();
    let a = PageId::generate();
    fx.open(&detail(&fx, a, "Plan", 1, "one"));
    // A conflicting page keeps its working copy when closed.
    fx.edit_body("two");
    let (id, _) = fx.save();
    let out = fx.reply_err(id, rejected(ErrorCode::StaleRevision));
    let (get, _) = only_request(&out);
    fx.reply(get, page_answer(&detail(&fx, a, "Plan", 3, "theirs")));
    fx.docs(DocsInput::ClosePage);
    fx.docs(DocsInput::OpenPage(a));
    let page = open_state(&fx);
    assert_eq!(page.save, SaveState::Conflict);
    assert_eq!(page.content, Some(PageContent::Page { body: "two".into() }));
}

#[test]
fn signing_out_forgets_every_page_and_never_prints_content() {
    let mut fx = setup();
    let a = PageId::generate();
    fx.load_list(vec![summary(&fx, a, "Secret title", 1)]);
    fx.open(&detail(&fx, a, "Secret title", 1, "secret body"));
    fx.edit_body("secret edit");
    let out = fx.docs(DocsInput::SavePage);
    let printed = format!(
        "{:?} {:?}",
        out.effects,
        Input::Docs(DocsInput::EditPage {
            title: Some("Secret title".into()),
            content: Some(PageContent::Page {
                body: "secret edit".into()
            }),
        })
    );
    assert!(!printed.contains("secret"), "{printed}");
    fx.driver.send(Input::SignOut);
    assert!(fx.driver.core.open_page().is_none());
    assert!(
        fx.driver
            .core
            .page_list(fx.general, PageState::Live)
            .is_none()
    );
}

#[test]
fn nothing_happens_before_the_session_is_ready() {
    let mut driver = Driver::new();
    let out = driver.send(Input::Docs(DocsInput::OpenPage(PageId::generate())));
    assert!(out.is_empty());
}

// ----- Randomized interleavings -----

/// Events and answers for one page arrive in every order the transport allows (answers in the
/// order the requests were made, events in log order, but the two streams interleaved freely, and
/// each event possibly delivered twice). Whatever the interleaving, the page ends at the newest
/// revision the server committed, in the right list.
#[test]
fn any_interleaving_of_events_answers_and_duplicates_ends_at_the_newest_revision() {
    // A small deterministic generator, so a failure names its seed.
    struct Rng(u64);
    impl Rng {
        fn next(&mut self) -> u64 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            self.0
        }
        fn below(&mut self, n: u64) -> u64 {
            self.next() % n
        }
    }
    for seed in 1..=300u64 {
        let mut rng = Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1);
        let mut fx = setup();
        let a = PageId::generate();
        let out = fx.docs(DocsInput::ListPages {
            conversation: fx.general,
            state: PageState::Live,
        });
        let (list_id, _) = only_request(&out);
        // The server's history of the page: revisions 1..=n, trashed from `trash_at` on.
        let n = 2 + rng.below(5) as u32;
        let trash_at = rng.below(u64::from(n) + 2) as u32; // n+1 means never
        let proto = summary(&fx, a, "", 1);
        let state_at = |revision: u32| {
            let mut s = PageSummary {
                title: format!("v{revision}"),
                revision,
                updated_at: at(i64::from(revision)),
                ..proto.clone()
            };
            if trash_at != 0 && revision >= trash_at && trash_at <= n {
                s = trashed(s);
            }
            s
        };
        let mut events: Vec<(i64, PageSummary)> =
            (1..=n).map(|r| (i64::from(r), state_at(r))).collect();
        // The list answer was read at some revision.
        let read_at = 1 + rng.below(u64::from(n)) as u32;
        let mut answer = Some((list_id, state_at(read_at)));
        let mut sent = 0usize;
        let mut last = Vec::new();
        while sent < events.len() || answer.is_some() {
            let deliver_answer = answer.is_some() && (sent == events.len() || rng.below(3) == 0);
            if deliver_answer {
                let (id, s) = answer.take().unwrap();
                // An answer read at revision r can only arrive after events up to r were
                // committed, and events up to some later point may already have arrived.
                let pages = vec![s];
                fx.reply(id, list_answer(pages, false));
            } else {
                let (at_seq, s) = events[sent].clone();
                sent += 1;
                let ev = event(&fx, at_seq, s.clone(), None);
                fx.driver.send(ev);
                if rng.below(4) == 0 {
                    let again = event(&fx, at_seq, s, None);
                    fx.driver.send(again);
                }
                last = events[..sent].to_vec();
            }
        }
        let _ = (&mut events, last);
        let final_state = state_at(n);
        let live: Vec<_> = list_titles(&fx, PageState::Live);
        if final_state.trashed_at.is_some() {
            assert!(
                live.is_empty(),
                "seed {seed}: trashed page listed live: {live:?}"
            );
        } else {
            assert_eq!(live, [format!("v{n}")], "seed {seed}");
        }
    }
}
