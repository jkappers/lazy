//! Docs (D30): pages and boards, versions, trash, search, moving, access, events, and files.
//! Notations are in `notations.rs`.

mod common;

use std::time::Duration;

use common::*;
use lazy_domain::NotationId;
use lazy_domain::page::{PageContent, PageKind};
use lazy_domain::{AttachmentId, ConversationId, OperationId, PageId};
use lazy_protocol::docs::doc_paths;
use lazy_protocol::{
    ErrorCode, Event, EventEnvelope, Notation, NotationListResponse, NotationResponse, PageDetail,
    PageListResponse, PageResponse, PageVersionResponse, PageVersionsResponse,
};
use lazy_server::attachments::cleanup;
use lazy_server::attachments::store::FileStore;
use reqwest::Method;
use serde_json::{Value, json};
use sqlx::PgPool;

async fn denied(response: reqwest::Response, code: ErrorCode) {
    let (status, error) = api_error(response).await;
    assert_error(status, &error, code);
}

/// An owner (first to register), and alice and bob, all in #general.
struct Docs {
    app: App,
    owner: Account,
    alice: Account,
    bob: Account,
}

async fn docs(pool: PgPool) -> Docs {
    let app = App::start(pool).await;
    let owner = app.register("owner").await;
    let alice = app.register("alice").await;
    let bob = app.register("bob").await;
    Docs {
        app,
        owner,
        alice,
        bob,
    }
}

async fn finish(docs: Docs) {
    docs.app.server.shutdown().await.unwrap();
}

fn op() -> OperationId {
    OperationId::generate()
}

impl Docs {
    async fn create_raw(
        &self,
        who: &Account,
        conversation: ConversationId,
        page: PageId,
        operation: OperationId,
        body: Value,
    ) -> reqwest::Response {
        let mut request = json!({
            "operation_id": operation, "page_id": page, "kind": "page", "title": "",
        });
        for (key, value) in body.as_object().unwrap() {
            request[key] = value.clone();
        }
        self.app
            .call(
                Method::POST,
                &doc_paths::conversation_pages(conversation),
                who,
                Some(request),
            )
            .await
    }

    async fn page_from(response: reqwest::Response) -> PageDetail {
        assert!(response.status().is_success(), "{}", response.status());
        response.json::<PageResponse>().await.unwrap().page
    }

    /// Creates a page with a title and a body in #general.
    async fn page(&self, who: &Account, title: &str, body: &str) -> PageDetail {
        self.page_in(who, self.app.general, title, body).await
    }

    async fn page_in(
        &self,
        who: &Account,
        conversation: ConversationId,
        title: &str,
        body: &str,
    ) -> PageDetail {
        let response = self
            .create_raw(
                who,
                conversation,
                PageId::generate(),
                op(),
                json!({"title": title, "content": {"kind": "page", "body": body}}),
            )
            .await;
        Self::page_from(response).await
    }

    async fn board(&self, who: &Account, title: &str, board: Value) -> PageDetail {
        let response = self
            .create_raw(
                who,
                self.app.general,
                PageId::generate(),
                op(),
                json!({"title": title, "kind": "board", "content": {"kind": "board", "board": board}}),
            )
            .await;
        Self::page_from(response).await
    }

    async fn edit_raw(&self, who: &Account, page: &PageDetail, body: Value) -> reqwest::Response {
        let mut request = json!({
            "operation_id": op(), "base_revision": page.summary.revision,
        });
        for (key, value) in body.as_object().unwrap() {
            request[key] = value.clone();
        }
        self.app
            .call(
                Method::PATCH,
                &doc_paths::page(page.summary.id),
                who,
                Some(request),
            )
            .await
    }

    async fn edit(&self, who: &Account, page: &PageDetail, text: &str) -> PageDetail {
        let response = self
            .edit_raw(
                who,
                page,
                json!({"content": {"kind": "page", "body": text}}),
            )
            .await;
        Self::page_from(response).await
    }

    async fn get_raw(&self, who: &Account, page: PageId) -> reqwest::Response {
        self.app
            .call(Method::GET, &doc_paths::page(page), who, None)
            .await
    }

    async fn get(&self, who: &Account, page: PageId) -> PageDetail {
        Self::page_from(self.get_raw(who, page).await).await
    }

    async fn action_raw(
        &self,
        who: &Account,
        method: Method,
        path: &str,
        operation: OperationId,
    ) -> reqwest::Response {
        self.app
            .call(method, path, who, Some(json!({"operation_id": operation})))
            .await
    }

    async fn trash(&self, who: &Account, page: PageId) -> PageDetail {
        let response = self
            .action_raw(who, Method::DELETE, &doc_paths::page(page), op())
            .await;
        Self::page_from(response).await
    }

    async fn restore(&self, who: &Account, page: PageId) -> PageDetail {
        let response = self
            .action_raw(who, Method::POST, &doc_paths::page_restore(page), op())
            .await;
        Self::page_from(response).await
    }

    async fn list_raw(&self, who: &Account, path: &str, query: &str) -> reqwest::Response {
        self.app
            .call(Method::GET, &format!("{path}?{query}"), who, None)
            .await
    }

    async fn list(&self, who: &Account, conversation: ConversationId, query: &str) -> Vec<String> {
        let response = self
            .list_raw(who, &doc_paths::conversation_pages(conversation), query)
            .await;
        assert!(response.status().is_success(), "{}", response.status());
        titles(response.json().await.unwrap())
    }

    async fn find(&self, who: &Account, query: &str) -> Vec<String> {
        let response = self
            .list_raw(
                who,
                &doc_paths::workspace_pages(self.app.workspace_id),
                query,
            )
            .await;
        assert!(response.status().is_success(), "{}", response.status());
        titles(response.json().await.unwrap())
    }

    async fn versions(&self, who: &Account, page: PageId) -> PageVersionsResponse {
        let response = self
            .app
            .call(Method::GET, &doc_paths::page_versions(page), who, None)
            .await;
        assert!(response.status().is_success(), "{}", response.status());
        response.json().await.unwrap()
    }

    async fn backdate_versions(&self, page: PageId, minutes: i32) {
        sqlx::query(
            "UPDATE page_versions SET created_at = created_at - make_interval(mins => $2) \
             WHERE page_id = $1",
        )
        .bind(page.as_uuid())
        .bind(minutes)
        .execute(&self.app.pool)
        .await
        .unwrap();
    }

    async fn page_events(&self, page: PageId) -> Vec<String> {
        let kinds: Vec<(String,)> = sqlx::query_as(
            "SELECT kind FROM events WHERE payload->'page'->>'id' = $1 \
                OR payload->>'page_id' = $1 ORDER BY seq",
        )
        .bind(page.to_string())
        .fetch_all(&self.app.pool)
        .await
        .unwrap();
        kinds.into_iter().map(|(kind,)| kind).collect()
    }

    /// A private channel alice created, with bob in it; owner is outside.
    async fn secret(&self) -> ConversationId {
        let channel = self
            .app
            .create_channel(&self.alice, "secret", "private")
            .await;
        let response = self
            .app
            .add_members(&self.alice, channel.id, &[&self.bob])
            .await;
        assert!(response.status().is_success());
        channel.id
    }
}

fn titles(list: PageListResponse) -> Vec<String> {
    list.pages.into_iter().map(|page| page.title).collect()
}

fn body_of(page: &PageDetail) -> &str {
    match &page.content {
        PageContent::Page { body } => body,
        PageContent::Board { .. } => panic!("a board"),
    }
}

// --- Creating ----------------------------------------------------------------------------------

#[sqlx::test]
async fn a_page_is_created_once_however_often_the_request_is_sent(pool: PgPool) {
    let d = docs(pool).await;
    let (page, operation) = (PageId::generate(), op());
    let body = json!({"title": "  Plan  ", "content": {"kind": "page", "body": "# Hi"}});
    let first = Docs::page_from(
        d.create_raw(&d.alice, d.app.general, page, operation, body.clone())
            .await,
    )
    .await;
    assert_eq!(first.summary.title, "Plan");
    assert_eq!(first.summary.revision, 1);
    assert_eq!(first.summary.excerpt, "# Hi");
    assert_eq!(first.summary.created_by, d.alice.user.id);
    assert_eq!(body_of(&first), "# Hi");

    let again = Docs::page_from(
        d.create_raw(&d.alice, d.app.general, page, operation, body.clone())
            .await,
    )
    .await;
    assert_eq!(again, first);
    assert_eq!(d.page_events(page).await, ["page_changed"]);

    // The same operation with other content, and the same page id under a new operation: conflicts.
    let other = json!({"title": "Plan", "content": {"kind": "page", "body": "different"}});
    denied(
        d.create_raw(&d.alice, d.app.general, page, operation, other)
            .await,
        ErrorCode::Conflict,
    )
    .await;
    denied(
        d.create_raw(&d.alice, d.app.general, page, op(), body)
            .await,
        ErrorCode::Conflict,
    )
    .await;
    assert_eq!(d.page_events(page).await.len(), 1);
    finish(d).await;
}

#[sqlx::test]
async fn an_empty_title_is_untitled_and_content_must_match_the_kind(pool: PgPool) {
    let d = docs(pool).await;
    let empty = Docs::page_from(
        d.create_raw(&d.alice, d.app.general, PageId::generate(), op(), json!({}))
            .await,
    )
    .await;
    assert_eq!(empty.summary.title, "Untitled");
    assert_eq!(
        empty.content,
        PageContent::Page {
            body: String::new()
        }
    );

    let wrong = d
        .create_raw(
            &d.alice,
            d.app.general,
            PageId::generate(),
            op(),
            json!({"kind": "board", "content": {"kind": "page", "body": "x"}}),
        )
        .await;
    denied(wrong, ErrorCode::InvalidRequest).await;
    let long = d
        .create_raw(
            &d.alice,
            d.app.general,
            PageId::generate(),
            op(),
            json!({"title": "x".repeat(256)}),
        )
        .await;
    denied(long, ErrorCode::InvalidRequest).await;
    finish(d).await;
}

#[sqlx::test]
async fn a_page_of_the_largest_size_is_accepted_and_one_byte_more_is_not(pool: PgPool) {
    let d = docs(pool).await;
    let max = lazy_domain::page::PAGE_BODY_MAX_BYTES;
    let page = d.page(&d.alice, "big", &"x".repeat(max)).await;
    assert_eq!(body_of(&page).len(), max);
    // Its excerpt stays short, and listing does not carry the body.
    assert_eq!(page.summary.excerpt.len(), 160);
    let response = d
        .create_raw(
            &d.alice,
            d.app.general,
            PageId::generate(),
            op(),
            json!({"content": {"kind": "page", "body": "x".repeat(max + 1)}}),
        )
        .await;
    denied(response, ErrorCode::InvalidRequest).await;
    finish(d).await;
}

#[sqlx::test]
async fn a_conversation_holds_a_bounded_number_of_live_pages(pool: PgPool) {
    let d = docs(pool).await;
    let first = d.page(&d.alice, "first", "").await;
    // Fill the rest directly, as a thousand requests would.
    sqlx::query(
        "INSERT INTO pages (id, workspace_id, conversation_id, kind, title, body, created_by, \
                            created_at, updated_by, updated_at) \
         SELECT gen_random_uuid(), workspace_id, conversation_id, 'page', 'filler ' || n, '', \
                created_by, now(), created_by, now() \
         FROM pages, generate_series(1, 999) n WHERE id = $1",
    )
    .bind(first.summary.id.as_uuid())
    .execute(&d.app.pool)
    .await
    .unwrap();
    let response = d
        .create_raw(&d.alice, d.app.general, PageId::generate(), op(), json!({}))
        .await;
    denied(response, ErrorCode::LimitReached).await;
    // The trash does not count: trashing one makes room, and restoring it then has none.
    d.trash(&d.alice, first.summary.id).await;
    let room = d.page(&d.alice, "room", "").await;
    let response = d
        .action_raw(
            &d.alice,
            Method::POST,
            &doc_paths::page_restore(first.summary.id),
            op(),
        )
        .await;
    denied(response, ErrorCode::LimitReached).await;
    assert_eq!(room.summary.revision, 1);
    finish(d).await;
}

// --- Editing ---------------------------------------------------------------------------------

#[sqlx::test]
async fn an_edit_raises_the_revision_and_a_stale_one_is_refused(pool: PgPool) {
    let d = docs(pool).await;
    let page = d.page(&d.alice, "Plan", "one").await;
    let edited = d.edit(&d.bob, &page, "two").await;
    assert_eq!(edited.summary.revision, 2);
    assert_eq!(edited.summary.updated_by, d.bob.user.id);
    assert_eq!(edited.summary.created_by, d.alice.user.id);
    assert_eq!(body_of(&edited), "two");
    // Alice still holds revision 1.
    denied(
        d.edit_raw(
            &d.alice,
            &page,
            json!({"content": {"kind": "page", "body": "mine"}}),
        )
        .await,
        ErrorCode::StaleRevision,
    )
    .await;
    assert_eq!(body_of(&d.get(&d.alice, page.summary.id).await), "two");
    // A title alone is an edit too.
    let renamed = d
        .edit_raw(&d.alice, &edited, json!({"title": "Roadmap"}))
        .await;
    let renamed = Docs::page_from(renamed).await;
    assert_eq!(renamed.summary.title, "Roadmap");
    assert_eq!(renamed.summary.revision, 3);
    assert_eq!(body_of(&renamed), "two");
    finish(d).await;
}

#[sqlx::test]
async fn an_edit_that_changes_nothing_commits_nothing(pool: PgPool) {
    let d = docs(pool).await;
    let page = d.page(&d.alice, "Plan", "same").await;
    let head = d.app.head().await;
    let same = d.edit(&d.alice, &page, "same").await;
    assert_eq!(same.summary.revision, 1);
    assert_eq!(d.app.head().await, head);
    assert!(
        d.versions(&d.alice, page.summary.id)
            .await
            .versions
            .is_empty()
    );
    finish(d).await;
}

#[sqlx::test]
async fn a_repeated_edit_returns_its_result_even_after_the_page_moved_on(pool: PgPool) {
    let d = docs(pool).await;
    let page = d.page(&d.alice, "Plan", "one").await;
    let request = json!({
        "operation_id": op(), "base_revision": 1,
        "content": {"kind": "page", "body": "two"},
    });
    let path = doc_paths::page(page.summary.id);
    let first = d
        .app
        .call(Method::PATCH, &path, &d.alice, Some(request.clone()))
        .await;
    assert!(first.status().is_success());
    let head = d.app.head().await;
    d.edit(
        &d.bob,
        &Docs::page_from(d.get_raw(&d.bob, page.summary.id).await).await,
        "three",
    )
    .await;
    let repeat = d
        .app
        .call(Method::PATCH, &path, &d.alice, Some(request.clone()))
        .await;
    assert!(repeat.status().is_success(), "{}", repeat.status());
    assert!(d.app.head().await > head);
    assert_eq!(
        d.page_events(page.summary.id).await.len(),
        3,
        "created, edited twice; the repeat added none"
    );
    // The same operation id for a different edit is a conflict.
    let mut other = request;
    other["content"] = json!({"kind": "page", "body": "other"});
    denied(
        d.app
            .call(Method::PATCH, &path, &d.alice, Some(other))
            .await,
        ErrorCode::Conflict,
    )
    .await;
    finish(d).await;
}

#[sqlx::test]
async fn append_adds_text_after_the_page_and_only_to_pages(pool: PgPool) {
    let d = docs(pool).await;
    let page = d.page(&d.alice, "Log", "first\n").await;
    let appended = Docs::page_from(
        d.edit_raw(&d.alice, &page, json!({"append": "second"}))
            .await,
    )
    .await;
    assert_eq!(body_of(&appended), "first\n\nsecond");
    let empty = d.page(&d.alice, "Empty", "").await;
    let appended = Docs::page_from(
        d.edit_raw(&d.alice, &empty, json!({"append": "only"}))
            .await,
    )
    .await;
    assert_eq!(body_of(&appended), "only");
    denied(
        d.edit_raw(
            &d.alice,
            &page,
            json!({"append": "x", "content": {"kind": "page", "body": "y"}}),
        )
        .await,
        ErrorCode::InvalidRequest,
    )
    .await;
    let board = d.board(&d.alice, "B", json!({})).await;
    denied(
        d.edit_raw(&d.alice, &board, json!({"append": "x"})).await,
        ErrorCode::InvalidRequest,
    )
    .await;
    // Content of the other kind is refused.
    denied(
        d.edit_raw(
            &d.alice,
            &board,
            json!({"content": {"kind": "page", "body": "x"}}),
        )
        .await,
        ErrorCode::InvalidRequest,
    )
    .await;
    finish(d).await;
}

// --- Boards ----------------------------------------------------------------------------------

fn board_json() -> Value {
    json!({
        "nodes": [
            {"id": "a", "shape": "rectangle", "x": 0, "y": 0, "width": 120, "height": 60, "text": "Start"},
            {"id": "b", "shape": "ellipse", "x": 200, "y": 0, "width": 120, "height": 60, "text": "Finish"},
        ],
        "edges": [{"id": "e", "source": "a", "target": "b", "label": "then"}],
        "viewport": {"x": 0, "y": 0, "zoom_percent": 100},
    })
}

#[sqlx::test]
async fn a_board_is_stored_checked_and_found_by_its_text(pool: PgPool) {
    let d = docs(pool).await;
    let board = d.board(&d.alice, "Flow", board_json()).await;
    assert_eq!(board.summary.kind, PageKind::Board);
    assert_eq!(board.summary.excerpt, "Start Finish then");
    let PageContent::Board { board: stored } = &board.content else {
        panic!("a board");
    };
    assert_eq!(stored.nodes.len(), 2);
    assert_eq!(d.find(&d.alice, "q=finish").await, ["Flow"]);
    assert_eq!(d.find(&d.alice, "q=then").await, ["Flow"]);

    let mut dangling = board_json();
    dangling["edges"][0]["target"] = json!("ghost");
    let response = d
        .edit_raw(
            &d.alice,
            &board,
            json!({"content": {"kind": "board", "board": dangling}}),
        )
        .await;
    denied(response, ErrorCode::InvalidRequest).await;

    let mut more = board_json();
    more["nodes"][0]["x"] = json!(40);
    let moved = Docs::page_from(
        d.edit_raw(
            &d.alice,
            &board,
            json!({"content": {"kind": "board", "board": more}}),
        )
        .await,
    )
    .await;
    assert_eq!(moved.summary.revision, 2);
    finish(d).await;
}

// --- Versions --------------------------------------------------------------------------------

#[sqlx::test]
async fn versions_keep_the_state_before_a_change_at_most_every_five_minutes(pool: PgPool) {
    let d = docs(pool).await;
    let page = d.page(&d.alice, "Plan", "one").await;
    let id = page.summary.id;
    let second = d.edit(&d.alice, &page, "two").await;
    // The first edit kept the state it replaced.
    let list = d.versions(&d.alice, id).await.versions;
    assert_eq!(list.len(), 1);
    assert_eq!(list[0].version, 1);
    assert_eq!(list[0].revision, 1);
    assert_eq!(list[0].created_by, Some(d.alice.user.id));
    let one = d
        .app
        .call(Method::GET, &doc_paths::page_version(id, 1), &d.alice, None)
        .await
        .json::<PageVersionResponse>()
        .await
        .unwrap()
        .version;
    assert_eq!(one.content, PageContent::Page { body: "one".into() });

    // A second edit right away keeps nothing; one after the interval does.
    let third = d.edit(&d.bob, &second, "three").await;
    assert_eq!(d.versions(&d.alice, id).await.versions.len(), 1);
    d.backdate_versions(id, 6).await;
    d.edit(&d.bob, &third, "four").await;
    let list = d.versions(&d.alice, id).await.versions;
    assert_eq!(
        list.iter().map(|v| v.version).collect::<Vec<_>>(),
        [2, 1],
        "newest first"
    );
    assert_eq!(list[0].created_by, Some(d.bob.user.id));
    denied(
        d.app
            .call(Method::GET, &doc_paths::page_version(id, 9), &d.alice, None)
            .await,
        ErrorCode::NotFound,
    )
    .await;
    finish(d).await;
}

#[sqlx::test]
async fn restoring_a_version_keeps_the_page_as_it_was_so_the_restore_can_be_undone(pool: PgPool) {
    let d = docs(pool).await;
    let page = d.page(&d.alice, "Plan", "one").await;
    let id = page.summary.id;
    let two = d.edit(&d.alice, &page, "two").await;

    let restore = |version: u32, base: u32| {
        let d = &d;
        async move {
            d.app
                .call(
                    Method::POST,
                    &doc_paths::page_version_restore(id, version),
                    &d.alice,
                    Some(json!({"operation_id": op(), "base_revision": base})),
                )
                .await
        }
    };
    denied(restore(1, 1).await, ErrorCode::StaleRevision).await;
    let restored = Docs::page_from(restore(1, two.summary.revision).await).await;
    assert_eq!(body_of(&restored), "one");
    assert_eq!(restored.summary.revision, 3);
    let list = d.versions(&d.alice, id).await.versions;
    assert_eq!(list.len(), 2);
    assert_eq!(list[0].reason, lazy_domain::VersionReason::PreRestore);
    // The state before the restore is the newest version: restoring it undoes the restore.
    let undone = Docs::page_from(restore(list[0].version, 3).await).await;
    assert_eq!(body_of(&undone), "two");
    denied(restore(77, 4).await, ErrorCode::NotFound).await;
    finish(d).await;
}

#[sqlx::test]
async fn a_page_keeps_its_newest_hundred_versions_and_numbers_are_never_reused(pool: PgPool) {
    let d = docs(pool).await;
    let page = d.page(&d.alice, "Plan", "start").await;
    let id = page.summary.id;
    // Give it ninety-nine versions already, then edit past the limit.
    sqlx::query(
        "INSERT INTO page_versions (page_id, version, title, body, reason, created_at, revision) \
         SELECT $1, n, 'Plan', 'v' || n, 'autosave', now() - interval '1 day', 1 \
         FROM generate_series(1, 100) n",
    )
    .bind(id.as_uuid())
    .execute(&d.app.pool)
    .await
    .unwrap();
    sqlx::query("UPDATE pages SET version_head = 100 WHERE id = $1")
        .bind(id.as_uuid())
        .execute(&d.app.pool)
        .await
        .unwrap();
    let edited = d.edit(&d.alice, &page, "next").await;
    let list = d.versions(&d.alice, id).await.versions;
    assert_eq!(list.len(), 100);
    assert_eq!(list[0].version, 101);
    assert_eq!(list[99].version, 2, "version 1 was removed");
    d.backdate_versions(id, 10).await;
    d.edit(&d.alice, &edited, "later").await;
    let list = d.versions(&d.alice, id).await.versions;
    assert_eq!(list.len(), 100);
    assert_eq!((list[0].version, list[99].version), (102, 3));
    finish(d).await;
}

// --- Trash -----------------------------------------------------------------------------------

#[sqlx::test]
async fn a_trashed_page_leaves_the_list_until_it_is_restored(pool: PgPool) {
    let d = docs(pool).await;
    let page = d.page(&d.alice, "Plan", "text").await;
    let id = page.summary.id;
    let trashed = d.trash(&d.bob, id).await;
    assert!(trashed.summary.trashed_at.is_some());
    assert!(trashed.summary.purge_at > trashed.summary.trashed_at);
    assert_eq!(trashed.summary.revision, 1, "trashing is not an edit");
    assert!(d.list(&d.alice, d.app.general, "").await.is_empty());
    assert_eq!(
        d.list(&d.alice, d.app.general, "state=trashed").await,
        ["Plan"]
    );
    assert!(d.find(&d.alice, "q=text").await.is_empty());
    assert_eq!(d.find(&d.alice, "q=text&state=trashed").await, ["Plan"]);
    // It can still be read, but not edited or given files or notations.
    assert!(d.get(&d.alice, id).await.summary.trashed_at.is_some());
    denied(
        d.edit_raw(&d.alice, &trashed, json!({"title": "x"})).await,
        ErrorCode::NotFound,
    )
    .await;
    // Trashing again is a no-op that answers the same way.
    let head = d.app.head().await;
    assert_eq!(
        d.trash(&d.alice, id).await.summary.trashed_at,
        trashed.summary.trashed_at
    );
    assert_eq!(d.app.head().await, head);

    let back = d.restore(&d.alice, id).await;
    assert!(back.summary.trashed_at.is_none());
    assert_eq!(d.list(&d.alice, d.app.general, "").await, ["Plan"]);
    assert_eq!(
        d.restore(&d.alice, id).await.summary.revision,
        1,
        "restoring twice is harmless"
    );
    assert_eq!(
        d.page_events(id).await,
        ["page_changed", "page_changed", "page_changed"],
        "created, trashed, restored"
    );
    finish(d).await;
}

#[sqlx::test]
async fn only_a_trashed_page_can_be_purged_and_a_purge_can_be_repeated(pool: PgPool) {
    let d = docs(pool).await;
    let page = d.page(&d.alice, "Plan", "text").await;
    let id = page.summary.id;
    let purge = |operation: OperationId| {
        let d = &d;
        async move {
            d.action_raw(
                &d.alice,
                Method::POST,
                &doc_paths::page_purge(id),
                operation,
            )
            .await
        }
    };
    denied(purge(op()).await, ErrorCode::InvalidRequest).await;
    d.trash(&d.alice, id).await;
    let operation = op();
    assert_eq!(purge(operation).await.status(), 204);
    denied(d.get_raw(&d.alice, id).await, ErrorCode::NotFound).await;
    let head = d.app.head().await;
    assert_eq!(
        purge(operation).await.status(),
        204,
        "the same operation again"
    );
    assert_eq!(d.app.head().await, head);
    denied(purge(op()).await, ErrorCode::NotFound).await;
    assert_eq!(d.page_events(id).await.last().unwrap(), "page_removed");
    finish(d).await;
}

#[sqlx::test]
async fn the_retention_job_removes_trash_older_than_thirty_days_with_its_files(pool: PgPool) {
    let d = docs(pool).await;
    let dir = d.app.server.attachments_dir().to_owned();
    let old = d.page(&d.alice, "Old", "x").await;
    let recent = d.page(&d.alice, "Recent", "x").await;
    let kept = d.page(&d.alice, "Kept", "x").await;
    let file = upload_file(&d, old.summary.id, b"bytes").await;
    d.trash(&d.alice, old.summary.id).await;
    d.trash(&d.alice, recent.summary.id).await;
    sqlx::query("UPDATE pages SET trashed_at = now() - interval '31 days' WHERE id = $1")
        .bind(old.summary.id.as_uuid())
        .execute(&d.app.pool)
        .await
        .unwrap();
    sqlx::query("UPDATE pages SET trashed_at = now() - interval '29 days' WHERE id = $1")
        .bind(recent.summary.id.as_uuid())
        .execute(&d.app.pool)
        .await
        .unwrap();

    let removed = lazy_server::pages::purge_expired(&d.app.pool, 30)
        .await
        .unwrap();
    assert_eq!(removed, 1);
    denied(
        d.get_raw(&d.alice, old.summary.id).await,
        ErrorCode::NotFound,
    )
    .await;
    assert!(
        d.get(&d.alice, recent.summary.id)
            .await
            .summary
            .trashed_at
            .is_some()
    );
    assert!(
        d.get(&d.alice, kept.summary.id)
            .await
            .summary
            .trashed_at
            .is_none()
    );
    assert_eq!(
        d.page_events(old.summary.id).await.last().unwrap(),
        "page_removed"
    );

    // Its file answers nothing at once, and the cleanup job takes the bytes and the row.
    denied(
        d.app.download_raw(&d.alice, file, false, false, &[]).await,
        ErrorCode::NotFound,
    )
    .await;
    let store = FileStore::open_dir(&dir).unwrap();
    let report = cleanup(&d.app.pool, &store, Duration::from_secs(24 * 3600))
        .await
        .unwrap();
    assert_eq!(report.removed, 1);
    let rows: i64 = sqlx::query_scalar("SELECT count(*) FROM attachments WHERE id = $1")
        .bind(file.as_uuid())
        .fetch_one(&d.app.pool)
        .await
        .unwrap();
    assert_eq!(rows, 0);
    finish(d).await;
}

// --- Search and lists --------------------------------------------------------------------------

#[sqlx::test]
async fn pages_are_found_by_title_and_text_and_listed_in_order(pool: PgPool) {
    let d = docs(pool).await;
    let a = d
        .page(&d.alice, "Quarterly roadmap", "ship the editor")
        .await;
    d.page(&d.alice, "Notes", "the roadmap is long").await;
    d.page(&d.alice, "Zebra", "stripes").await;
    d.board(&d.alice, "Whiteboard", board_json()).await;

    assert_eq!(
        d.find(&d.alice, "q=roadmap&sort=title").await,
        ["Notes", "Quarterly roadmap"]
    );
    assert_eq!(d.find(&d.alice, "q=editor").await, ["Quarterly roadmap"]);
    // A title match works for a part of a word, the text index for whole words.
    assert_eq!(d.find(&d.alice, "q=quarter").await, ["Quarterly roadmap"]);
    assert_eq!(d.find(&d.alice, "kind=board").await, ["Whiteboard"]);
    assert_eq!(
        d.find(&d.alice, "kind=page&sort=title").await,
        ["Notes", "Quarterly roadmap", "Zebra"]
    );
    // Newest change first: editing moves a page to the top.
    d.edit(&d.alice, &a, "ship the editor now").await;
    assert_eq!(d.find(&d.alice, "kind=page").await[0], "Quarterly roadmap");
    // LIKE wildcards in the query are plain characters.
    assert!(d.find(&d.alice, "q=%25").await.is_empty());

    let response = d
        .list_raw(
            &d.alice,
            &doc_paths::workspace_pages(d.app.workspace_id),
            "limit=2",
        )
        .await;
    let first: PageListResponse = response.json().await.unwrap();
    assert_eq!((first.pages.len(), first.has_more), (2, true));
    let response = d
        .list_raw(
            &d.alice,
            &doc_paths::workspace_pages(d.app.workspace_id),
            "limit=2&offset=2",
        )
        .await;
    let second: PageListResponse = response.json().await.unwrap();
    assert_eq!((second.pages.len(), second.has_more), (2, false));
    assert!(
        first
            .pages
            .iter()
            .all(|p| !second.pages.iter().any(|q| q.id == p.id))
    );
    finish(d).await;
}

#[sqlx::test]
async fn lists_and_search_show_only_what_the_caller_can_read(pool: PgPool) {
    let d = docs(pool).await;
    let secret = d.secret().await;
    d.page_in(&d.alice, secret, "Hidden plan", "classified")
        .await;
    d.page(&d.alice, "Open plan", "public").await;

    assert_eq!(
        d.find(&d.bob, "sort=title").await,
        ["Hidden plan", "Open plan"]
    );
    assert_eq!(d.find(&d.owner, "sort=title").await, ["Open plan"]);
    assert!(d.find(&d.owner, "q=classified").await.is_empty());
    denied(
        d.list_raw(&d.owner, &doc_paths::conversation_pages(secret), "")
            .await,
        ErrorCode::NotFound,
    )
    .await;
    // Someone who is no longer in the channel loses its pages at once.
    d.app.remove_from_conversation(secret, &d.bob).await;
    assert_eq!(d.find(&d.bob, "").await, ["Open plan"]);
    finish(d).await;
}

// --- Access ----------------------------------------------------------------------------------

#[sqlx::test]
async fn pages_follow_the_access_of_their_conversation(pool: PgPool) {
    let d = docs(pool).await;
    let secret = d.secret().await;
    let hidden = d.page_in(&d.alice, secret, "Hidden", "x").await;
    let open = d.page(&d.alice, "Open", "x").await;
    let id = hidden.summary.id;

    // Outside a private channel nothing exists: not even that it exists.
    denied(d.get_raw(&d.owner, id).await, ErrorCode::NotFound).await;
    denied(
        d.edit_raw(&d.owner, &hidden, json!({"title": "x"})).await,
        ErrorCode::NotFound,
    )
    .await;
    denied(
        d.action_raw(&d.owner, Method::DELETE, &doc_paths::page(id), op())
            .await,
        ErrorCode::NotFound,
    )
    .await;
    denied(
        d.action_raw(&d.owner, Method::POST, &doc_paths::page_restore(id), op())
            .await,
        ErrorCode::NotFound,
    )
    .await;
    denied(
        d.app
            .call(Method::GET, &doc_paths::page_versions(id), &d.owner, None)
            .await,
        ErrorCode::NotFound,
    )
    .await;
    denied(
        d.create_raw(&d.owner, secret, PageId::generate(), op(), json!({}))
            .await,
        ErrorCode::NotFound,
    )
    .await;

    // A workspace member who has not joined a public channel reads but does not write. (Owner is
    // in #general through registration, so take them out of it first.)
    d.app
        .remove_from_conversation(d.app.general, &d.owner)
        .await;
    assert_eq!(d.get(&d.owner, open.summary.id).await.summary.title, "Open");
    denied(
        d.edit_raw(&d.owner, &open, json!({"title": "x"})).await,
        ErrorCode::Forbidden,
    )
    .await;
    denied(
        d.action_raw(
            &d.owner,
            Method::DELETE,
            &doc_paths::page(open.summary.id),
            op(),
        )
        .await,
        ErrorCode::Forbidden,
    )
    .await;
    denied(
        d.create_raw(&d.owner, d.app.general, PageId::generate(), op(), json!({}))
            .await,
        ErrorCode::Forbidden,
    )
    .await;

    // An archived channel is read-only.
    sqlx::query("UPDATE conversations SET archived = true WHERE id = $1")
        .bind(d.app.general.as_uuid())
        .execute(&d.app.pool)
        .await
        .unwrap();
    assert_eq!(d.get(&d.alice, open.summary.id).await.summary.title, "Open");
    denied(
        d.edit_raw(&d.alice, &open, json!({"title": "x"})).await,
        ErrorCode::Forbidden,
    )
    .await;
    denied(
        d.create_raw(&d.alice, d.app.general, PageId::generate(), op(), json!({}))
            .await,
        ErrorCode::Forbidden,
    )
    .await;
    finish(d).await;
}

#[sqlx::test]
async fn a_page_moves_with_its_files_only_between_conversations_the_caller_can_post_in(
    pool: PgPool,
) {
    let d = docs(pool).await;
    let secret = d.secret().await;
    let page = d.page(&d.alice, "Roving", "text").await;
    let file = upload_file(&d, page.summary.id, b"hello").await;
    let elsewhere = d.app.create_channel(&d.owner, "owners", "private").await;

    // Alice cannot see the destination, bob can post in it but not in the source's twin.
    denied(
        d.edit_raw(&d.alice, &page, json!({"conversation_id": elsewhere.id}))
            .await,
        ErrorCode::NotFound,
    )
    .await;
    let moved = Docs::page_from(
        d.edit_raw(&d.alice, &page, json!({"conversation_id": secret}))
            .await,
    )
    .await;
    assert_eq!(moved.summary.conversation_id, secret);
    assert_eq!(moved.summary.revision, 2);
    assert_eq!(moved.files.len(), 1);
    // It left #general (the owner no longer sees it) and its file followed it.
    denied(
        d.get_raw(&d.owner, page.summary.id).await,
        ErrorCode::NotFound,
    )
    .await;
    denied(
        d.app.download_raw(&d.owner, file, false, false, &[]).await,
        ErrorCode::NotFound,
    )
    .await;
    assert_eq!(
        d.app
            .download_raw(&d.bob, file, false, false, &[])
            .await
            .status(),
        200
    );
    assert_eq!(
        d.page_events(page.summary.id).await,
        [
            "page_changed",
            "page_changed",
            "page_removed",
            "page_changed"
        ]
    );
    // Moving it back needs post access to both ends.
    denied(
        d.edit_raw(&d.owner, &moved, json!({"conversation_id": d.app.general}))
            .await,
        ErrorCode::NotFound,
    )
    .await;
    finish(d).await;
}

// --- Events ----------------------------------------------------------------------------------

async fn subscribed(d: &Docs, who: &Account) -> Ws {
    let mut ws = d.app.connect_ws(who).await;
    ws.subscribe(d.app.workspace_id, d.app.head().await).await;
    ws.replay().await;
    ws
}

fn docs_events(events: &[EventEnvelope]) -> Vec<(&'static str, Option<OperationId>)> {
    events
        .iter()
        .filter_map(|envelope| match &envelope.event {
            Event::PageChanged { operation_id, .. } => Some(("changed", *operation_id)),
            Event::PageRemoved { .. } => Some(("removed", None)),
            _ => None,
        })
        .collect()
}

#[sqlx::test]
async fn members_are_told_of_changes_and_nobody_else_is(pool: PgPool) {
    let d = docs(pool).await;
    let secret = d.secret().await;
    let mut bob = subscribed(&d, &d.bob).await;
    let mut owner = subscribed(&d, &d.owner).await;

    let operation = op();
    let page = Docs::page_from(
        d.create_raw(
            &d.alice,
            secret,
            PageId::generate(),
            operation,
            json!({"title": "Plan"}),
        )
        .await,
    )
    .await;
    let edited = d.edit(&d.alice, &page, "text").await;
    d.trash(&d.alice, page.summary.id).await;
    d.restore(&d.alice, page.summary.id).await;
    d.action_raw(
        &d.alice,
        Method::DELETE,
        &doc_paths::page(page.summary.id),
        op(),
    )
    .await;
    d.action_raw(
        &d.alice,
        Method::POST,
        &doc_paths::page_purge(page.summary.id),
        op(),
    )
    .await;
    let marker = d.app.register("marker").await;

    let seen = bob.events_until_marker(marker.user.id).await;
    let events = docs_events(&seen);
    assert_eq!(events.len(), 6);
    assert_eq!(events[0], ("changed", Some(operation)));
    assert_eq!(events[5], ("removed", None));
    // Each carries a complete summary at the revision it made.
    let revisions: Vec<u32> = seen
        .iter()
        .filter_map(|e| match &e.event {
            Event::PageChanged { page, .. } => Some(page.revision),
            _ => None,
        })
        .collect();
    assert_eq!(revisions, [1, edited.summary.revision, 2, 2, 2]);
    assert!(seen.iter().any(|e| matches!(&e.event,
        Event::PageChanged { page, .. } if page.trashed_at.is_some())));
    // The owner is not in the private channel: nothing arrived.
    let seen = owner.events_until_marker(marker.user.id).await;
    assert!(docs_events(&seen).is_empty());
    finish(d).await;
}

#[sqlx::test]
async fn positions_stay_gapless_across_docs_writes(pool: PgPool) {
    let d = docs(pool).await;
    let page = d.page(&d.alice, "Plan", "one").await;
    let second = d.edit(&d.alice, &page, "two").await;
    d.trash(&d.alice, page.summary.id).await;
    d.restore(&d.alice, page.summary.id).await;
    d.edit(
        &d.bob,
        &Docs::page_from(d.get_raw(&d.bob, second.summary.id).await).await,
        "three",
    )
    .await;
    let positions = d.app.log_positions_after(lazy_domain::EventSeq::ZERO).await;
    let expected: Vec<i64> = (positions[0]..positions[0] + positions.len() as i64).collect();
    assert_eq!(positions, expected);
    finish(d).await;
}

// --- Files -----------------------------------------------------------------------------------

async fn upload_file(d: &Docs, page: PageId, bytes: &[u8]) -> AttachmentId {
    let id = AttachmentId::generate();
    let response = put_file(d, &d.alice, page, id, bytes).await;
    assert!(
        response.status().is_success(),
        "upload: {}",
        response.status()
    );
    id
}

async fn put_file(
    d: &Docs,
    who: &Account,
    page: PageId,
    id: AttachmentId,
    bytes: &[u8],
) -> reqwest::Response {
    d.app
        .http
        .put(d.app.url(&doc_paths::page_file(page, id)))
        .bearer_auth(&who.token)
        .header("content-type", "application/octet-stream")
        .header("x-attachment-name", "notes.bin")
        .body(bytes.to_vec())
        .send()
        .await
        .unwrap()
}

#[sqlx::test]
async fn a_file_on_a_page_is_listed_read_by_readers_and_never_pending(pool: PgPool) {
    let d = docs(pool).await;
    let secret = d.secret().await;
    let page = d.page_in(&d.alice, secret, "Files", "x").await;
    let id = page.summary.id;
    let mut bob = subscribed(&d, &d.bob).await;

    let file = AttachmentId::generate();
    let response = put_file(&d, &d.alice, id, file, b"hello").await;
    assert!(response.status().is_success());
    // The same upload again is the same file and announces nothing more.
    let again = put_file(&d, &d.alice, id, file, b"hello").await;
    assert!(again.status().is_success());
    denied(
        put_file(&d, &d.alice, id, file, b"other").await,
        ErrorCode::Conflict,
    )
    .await;

    let detail = d.get(&d.bob, id).await;
    assert_eq!(detail.files.len(), 1);
    assert_eq!(detail.files[0].id, file);
    assert_eq!((detail.files[0].size, detail.summary.file_count), (5, 1));
    assert_eq!(detail.summary.revision, 1, "a file is not an edit");
    let download = d.app.download_raw(&d.bob, file, false, false, &[]).await;
    assert_eq!(download.status(), 200);
    assert_eq!(download.bytes().await.unwrap().as_ref(), b"hello");
    denied(
        d.app.download_raw(&d.owner, file, false, false, &[]).await,
        ErrorCode::NotFound,
    )
    .await;

    // It is not one of the uploader's pending files, so it never counts against them or expires.
    sqlx::query("UPDATE attachments SET created_at = now() - interval '3 days' WHERE id = $1")
        .bind(file.as_uuid())
        .execute(&d.app.pool)
        .await
        .unwrap();
    let store = FileStore::open_dir(d.app.server.attachments_dir()).unwrap();
    let report = cleanup(&d.app.pool, &store, Duration::from_secs(3600))
        .await
        .unwrap();
    assert_eq!(report.expired, 0);
    assert_eq!(
        d.app
            .download_raw(&d.bob, file, false, false, &[])
            .await
            .status(),
        200
    );
    // And it cannot be put on a message.
    let sent = d
        .app
        .send_with_files_raw(&d.alice, secret, op(), "hi", &[file], None, false)
        .await;
    denied(sent, ErrorCode::NotFound).await;

    let marker = d.app.register("marker").await;
    let seen = bob.events_until_marker(marker.user.id).await;
    let counts: Vec<u32> = seen
        .iter()
        .filter_map(|e| match &e.event {
            Event::PageChanged { page, .. } => Some(page.file_count),
            _ => None,
        })
        .collect();
    assert_eq!(counts, [1], "one announcement, for the one file");
    finish(d).await;
}

#[sqlx::test]
async fn only_someone_who_can_edit_a_live_page_can_add_files(pool: PgPool) {
    let d = docs(pool).await;
    let secret = d.secret().await;
    let page = d.page_in(&d.alice, secret, "Files", "x").await;
    let id = page.summary.id;
    denied(
        put_file(&d, &d.owner, id, AttachmentId::generate(), b"x").await,
        ErrorCode::NotFound,
    )
    .await;
    d.trash(&d.alice, id).await;
    denied(
        put_file(&d, &d.alice, id, AttachmentId::generate(), b"x").await,
        ErrorCode::NotFound,
    )
    .await;
    d.restore(&d.alice, id).await;
    let missing = PageId::generate();
    denied(
        put_file(&d, &d.alice, missing, AttachmentId::generate(), b"x").await,
        ErrorCode::NotFound,
    )
    .await;
    assert!(
        put_file(&d, &d.alice, id, AttachmentId::generate(), b"x")
            .await
            .status()
            .is_success()
    );
    // A page holds a bounded number of files.
    sqlx::query(
        "INSERT INTO attachments (id, workspace_id, conversation_id, uploader_id, name, media_type, \
                                  size, sha256, page_id) \
         SELECT gen_random_uuid(), workspace_id, conversation_id, created_by, 'f', \
                'application/octet-stream', 1, decode(repeat('00', 32), 'hex'), id \
         FROM pages, generate_series(1, 99) WHERE id = $1",
    )
    .bind(id.as_uuid())
    .execute(&d.app.pool)
    .await
    .unwrap();
    denied(
        put_file(&d, &d.alice, id, AttachmentId::generate(), b"x").await,
        ErrorCode::LimitReached,
    )
    .await;
    finish(d).await;
}

// --- Receipts and the wire -----------------------------------------------------------------------

#[sqlx::test]
async fn docs_operations_are_not_confused_with_other_receipts(pool: PgPool) {
    let d = docs(pool).await;
    // An operation id used for a page cannot be reused for a message, or the other way round.
    let operation = op();
    let page = PageId::generate();
    Docs::page_from(
        d.create_raw(&d.alice, d.app.general, page, operation, json!({}))
            .await,
    )
    .await;
    let status = d.app.operation(&d.alice, operation).await;
    assert!(
        matches!(status, lazy_protocol::OperationStatus::NotFound),
        "a page's operation is reconciled by fetching the page: {status:?}"
    );
    let sent = d
        .app
        .send_raw(&d.alice, d.app.general, operation, "hello")
        .await;
    denied(sent, ErrorCode::Conflict).await;
    // Another person's receipts are separate: the same id is theirs to use.
    assert!(
        d.create_raw(
            &d.bob,
            d.app.general,
            PageId::generate(),
            operation,
            json!({})
        )
        .await
        .status()
        .is_success()
    );
    finish(d).await;
}

// --- Notations -------------------------------------------------------------------------------

impl Docs {
    async fn notate_raw(
        &self,
        who: &Account,
        page: PageId,
        notation: NotationId,
        operation: OperationId,
        body: Value,
    ) -> reqwest::Response {
        let mut request = json!({
            "operation_id": operation, "notation_id": notation,
            "quote": "beta", "from": 6, "to": 10, "body": "check this",
        });
        for (key, value) in body.as_object().unwrap() {
            request[key] = value.clone();
        }
        self.app
            .call(
                Method::POST,
                &doc_paths::page_notations(page),
                who,
                Some(request),
            )
            .await
    }

    async fn notation_from(response: reqwest::Response) -> Notation {
        assert!(response.status().is_success(), "{}", response.status());
        response.json::<NotationResponse>().await.unwrap().notation
    }

    /// A notation on "beta" in a page reading "alpha beta gamma".
    async fn notate(&self, who: &Account, page: PageId, assignee: Option<&Account>) -> Notation {
        let body = match assignee {
            Some(who) => json!({"assignee_id": who.user.id}),
            None => json!({}),
        };
        Self::notation_from(
            self.notate_raw(who, page, NotationId::generate(), op(), body)
                .await,
        )
        .await
    }

    async fn notation_call(
        &self,
        who: &Account,
        method: Method,
        path: &str,
        body: Value,
    ) -> reqwest::Response {
        let mut request = json!({"operation_id": op()});
        for (key, value) in body.as_object().unwrap() {
            request[key] = value.clone();
        }
        self.app.call(method, path, who, Some(request)).await
    }

    async fn complete(&self, who: &Account, n: &Notation, completed: bool) -> reqwest::Response {
        self.notation_call(
            who,
            Method::PUT,
            &doc_paths::notation_completed(n.id),
            json!({"completed": completed}),
        )
        .await
    }

    async fn reply(&self, who: &Account, n: &Notation, text: &str) -> reqwest::Response {
        self.notation_call(
            who,
            Method::POST,
            &doc_paths::notation_replies(n.id),
            json!({"reply_id": lazy_domain::NotationReplyId::generate(), "body": text}),
        )
        .await
    }

    async fn notations_of(&self, who: &Account, page: PageId) -> Vec<Notation> {
        let response = self
            .app
            .call(Method::GET, &doc_paths::page_notations(page), who, None)
            .await;
        assert!(response.status().is_success(), "{}", response.status());
        response
            .json::<NotationListResponse>()
            .await
            .unwrap()
            .notations
    }

    async fn inbox(&self, who: &Account, query: &str) -> Vec<Notation> {
        let response = self
            .list_raw(
                who,
                &doc_paths::workspace_notations(self.app.workspace_id),
                query,
            )
            .await;
        assert!(response.status().is_success(), "{}", response.status());
        response
            .json::<NotationListResponse>()
            .await
            .unwrap()
            .notations
    }

    async fn open_count(&self, who: &Account, page: PageId) -> u32 {
        self.get(who, page).await.summary.open_notations
    }
}

#[sqlx::test]
async fn a_notation_is_anchored_assigned_counted_and_announced(pool: PgPool) {
    let d = docs(pool).await;
    let page = d.page(&d.alice, "Plan", "alpha beta gamma").await;
    let id = page.summary.id;
    let mut alice = subscribed(&d, &d.alice).await;

    let (notation_id, operation) = (NotationId::generate(), op());
    let body = json!({"assignee_id": d.alice.user.id});
    let made = Docs::notation_from(
        d.notate_raw(&d.bob, id, notation_id, operation, body.clone())
            .await,
    )
    .await;
    assert_eq!((made.quote.as_str(), made.from, made.to), ("beta", 6, 10));
    assert_eq!(made.author_id, d.bob.user.id);
    assert_eq!(made.assignee_id, d.alice.user.id);
    assert_eq!(made.page_id, id);
    assert_eq!(made.conversation_id, d.app.general);
    assert!(made.completed_at.is_none() && made.replies.is_empty());
    assert_eq!(d.open_count(&d.owner, id).await, 1);
    assert_eq!(
        d.notations_of(&d.alice, id).await,
        std::slice::from_ref(&made)
    );

    // The same request again is the same notation; a different one under that id is a conflict.
    let again = Docs::notation_from(
        d.notate_raw(&d.bob, id, notation_id, operation, body.clone())
            .await,
    )
    .await;
    assert_eq!(again, made);
    denied(
        d.notate_raw(&d.bob, id, notation_id, operation, json!({"body": "other"}))
            .await,
        ErrorCode::Conflict,
    )
    .await;
    denied(
        d.notate_raw(&d.bob, id, notation_id, op(), body).await,
        ErrorCode::Conflict,
    )
    .await;
    // The assignee defaults to the author.
    let own = d.notate(&d.bob, id, None).await;
    assert_eq!(own.assignee_id, d.bob.user.id);

    let marker = d.app.register("marker").await;
    let seen = alice.events_until_marker(marker.user.id).await;
    let counts: Vec<u32> = seen
        .iter()
        .filter_map(|e| match &e.event {
            Event::NotationChanged { open_notations, .. } => Some(*open_notations),
            _ => None,
        })
        .collect();
    assert_eq!(counts, [1, 2]);
    finish(d).await;
}

#[sqlx::test]
async fn a_notation_needs_a_page_text_and_someone_who_can_read_it(pool: PgPool) {
    let d = docs(pool).await;
    let secret = d.secret().await;
    let page = d
        .page_in(&d.alice, secret, "Plan", "alpha beta gamma")
        .await;
    let id = page.summary.id;
    let make = |who: &Account, body: Value| {
        let (d, who) = (&d, who.clone());
        async move {
            d.notate_raw(&who, id, NotationId::generate(), op(), body)
                .await
        }
    };
    // Bad anchors and bodies.
    denied(
        make(&d.alice, json!({"quote": ""})).await,
        ErrorCode::InvalidRequest,
    )
    .await;
    denied(
        make(&d.alice, json!({"from": 10, "to": 10})).await,
        ErrorCode::InvalidRequest,
    )
    .await;
    denied(
        make(&d.alice, json!({"from": 6, "to": 99})).await,
        ErrorCode::InvalidRequest,
    )
    .await;
    denied(
        make(&d.alice, json!({"body": "  "})).await,
        ErrorCode::InvalidRequest,
    )
    .await;
    // The assignee must be able to read the private channel.
    denied(
        make(&d.alice, json!({"assignee_id": d.owner.user.id})).await,
        ErrorCode::InvalidRequest,
    )
    .await;
    // Nobody outside it can make one, or see that the page exists.
    denied(make(&d.owner, json!({})).await, ErrorCode::NotFound).await;
    // A board takes none.
    let board = d.board(&d.alice, "B", board_json()).await;
    let response = d
        .notate_raw(
            &d.alice,
            board.summary.id,
            NotationId::generate(),
            op(),
            json!({}),
        )
        .await;
    denied(response, ErrorCode::InvalidRequest).await;
    // Nor a page in the trash.
    d.trash(&d.alice, id).await;
    denied(make(&d.alice, json!({})).await, ErrorCode::NotFound).await;
    d.restore(&d.alice, id).await;
    // Reading a conversation is not enough to write in it.
    let open = d.page(&d.alice, "Open", "alpha beta gamma").await;
    d.app
        .remove_from_conversation(d.app.general, &d.owner)
        .await;
    denied(
        d.notate_raw(
            &d.owner,
            open.summary.id,
            NotationId::generate(),
            op(),
            json!({}),
        )
        .await,
        ErrorCode::Forbidden,
    )
    .await;
    assert!(d.notations_of(&d.owner, open.summary.id).await.is_empty());
    finish(d).await;
}

#[sqlx::test]
async fn the_author_and_the_assignee_change_and_complete_a_notation(pool: PgPool) {
    let d = docs(pool).await;
    let page = d.page(&d.alice, "Plan", "alpha beta gamma").await;
    let id = page.summary.id;
    let n = d.notate(&d.bob, id, Some(&d.alice)).await;
    let path = doc_paths::notation(n.id);

    // A third person may do neither.
    denied(
        d.notation_call(&d.owner, Method::PATCH, &path, json!({"body": "x"}))
            .await,
        ErrorCode::Forbidden,
    )
    .await;
    denied(d.complete(&d.owner, &n, true).await, ErrorCode::Forbidden).await;
    // The assignee edits the body; the author hands it to someone else.
    let edited = Docs::notation_from(
        d.notation_call(&d.alice, Method::PATCH, &path, json!({"body": "done soon"}))
            .await,
    )
    .await;
    assert_eq!(edited.body, "done soon");
    assert!(edited.updated_at >= n.updated_at);
    let handed = Docs::notation_from(
        d.notation_call(
            &d.bob,
            Method::PATCH,
            &path,
            json!({"assignee_id": d.owner.user.id}),
        )
        .await,
    )
    .await;
    assert_eq!(handed.assignee_id, d.owner.user.id);
    // No change commits nothing.
    let head = d.app.head().await;
    let unchanged = d
        .notation_call(&d.bob, Method::PATCH, &path, json!({"body": "done soon"}))
        .await;
    assert_eq!(
        Docs::notation_from(unchanged).await.updated_at,
        handed.updated_at
    );
    assert_eq!(d.app.head().await, head);

    // The new assignee completes it, and it leaves the open count.
    assert_eq!(d.open_count(&d.alice, id).await, 1);
    let done = Docs::notation_from(d.complete(&d.owner, &n, true).await).await;
    assert!(done.completed_at.is_some());
    assert_eq!(done.resolved_by, Some(d.owner.user.id));
    assert_eq!(d.open_count(&d.alice, id).await, 0);
    let reopened = Docs::notation_from(d.complete(&d.bob, &n, false).await).await;
    assert!(reopened.completed_at.is_none() && reopened.resolved_by.is_none());
    assert_eq!(d.open_count(&d.alice, id).await, 1);
    finish(d).await;
}

#[sqlx::test]
async fn replies_are_for_open_notations_and_removed_by_their_author_or_an_admin(pool: PgPool) {
    let d = docs(pool).await;
    let page = d.page(&d.alice, "Plan", "alpha beta gamma").await;
    let n = d.notate(&d.alice, page.summary.id, None).await;

    let first = Docs::notation_from(d.reply(&d.bob, &n, "on it").await).await;
    let second = Docs::notation_from(d.reply(&d.alice, &n, "thanks").await).await;
    assert_eq!(first.replies.len(), 1);
    assert_eq!(
        second
            .replies
            .iter()
            .map(|r| r.body.as_str())
            .collect::<Vec<_>>(),
        ["on it", "thanks"]
    );
    assert!(second.updated_at >= first.updated_at);
    // A reply id is used once.
    let reply = second.replies[0].id;
    denied(
        d.notation_call(
            &d.bob,
            Method::POST,
            &doc_paths::notation_replies(n.id),
            json!({"reply_id": reply, "body": "again"}),
        )
        .await,
        ErrorCode::Conflict,
    )
    .await;
    denied(d.reply(&d.bob, &n, " ").await, ErrorCode::InvalidRequest).await;

    // Completed: no more replies.
    d.complete(&d.alice, &n, true).await;
    denied(d.reply(&d.bob, &n, "late").await, ErrorCode::Forbidden).await;
    d.complete(&d.alice, &n, false).await;

    // Alice cannot remove bob's reply; the owner (an admin role) can; bob can remove his own.
    let path = doc_paths::notation_reply(reply);
    denied(
        d.notation_call(&d.alice, Method::DELETE, &path, json!({}))
            .await,
        ErrorCode::Forbidden,
    )
    .await;
    let operation = op();
    let removed = d
        .app
        .call(
            Method::DELETE,
            &path,
            &d.owner,
            Some(json!({"operation_id": operation})),
        )
        .await;
    assert_eq!(Docs::notation_from(removed).await.replies.len(), 1);
    // Repeating that deletion succeeds again with the notation as it is.
    let repeat = d
        .app
        .call(
            Method::DELETE,
            &path,
            &d.owner,
            Some(json!({"operation_id": operation})),
        )
        .await;
    assert_eq!(Docs::notation_from(repeat).await.replies.len(), 1);
    denied(
        d.notation_call(&d.owner, Method::DELETE, &path, json!({}))
            .await,
        ErrorCode::NotFound,
    )
    .await;
    finish(d).await;
}

#[sqlx::test]
async fn a_notation_is_deleted_by_its_author_or_an_admin_and_members_are_told(pool: PgPool) {
    let d = docs(pool).await;
    let page = d.page(&d.alice, "Plan", "alpha beta gamma").await;
    let id = page.summary.id;
    let mine = d.notate(&d.bob, id, None).await;
    let other = d.notate(&d.bob, id, None).await;
    let mut alice = subscribed(&d, &d.alice).await;

    let path = doc_paths::notation(mine.id);
    denied(
        d.notation_call(&d.alice, Method::DELETE, &path, json!({}))
            .await,
        ErrorCode::Forbidden,
    )
    .await;
    let operation = op();
    let call = |who: &Account| {
        let (d, who, path) = (&d, who.clone(), path.clone());
        async move {
            d.app
                .call(
                    Method::DELETE,
                    &path,
                    &who,
                    Some(json!({"operation_id": operation})),
                )
                .await
        }
    };
    assert_eq!(call(&d.bob).await.status(), 204);
    assert_eq!(call(&d.bob).await.status(), 204, "the same operation again");
    assert_eq!(d.open_count(&d.alice, id).await, 1);
    // An admin (the owner) removes someone else's.
    let gone = d
        .notation_call(
            &d.owner,
            Method::DELETE,
            &doc_paths::notation(other.id),
            json!({}),
        )
        .await;
    assert_eq!(gone.status(), 204);
    assert!(d.notations_of(&d.alice, id).await.is_empty());
    denied(
        d.notation_call(&d.bob, Method::DELETE, &path, json!({}))
            .await,
        ErrorCode::NotFound,
    )
    .await;

    let marker = d.app.register("marker").await;
    let seen = alice.events_until_marker(marker.user.id).await;
    let removed: Vec<u32> = seen
        .iter()
        .filter_map(|e| match &e.event {
            Event::NotationRemoved { open_notations, .. } => Some(*open_notations),
            _ => None,
        })
        .collect();
    assert_eq!(removed, [1, 0]);
    finish(d).await;
}

#[sqlx::test]
async fn the_notation_lists_show_what_is_assigned_or_written_on_readable_pages(pool: PgPool) {
    let d = docs(pool).await;
    let secret = d.secret().await;
    let open = d.page(&d.alice, "Roadmap", "alpha beta gamma").await;
    let hidden = d
        .page_in(&d.alice, secret, "Hidden", "alpha beta gamma")
        .await;
    let to_bob = d.notate(&d.alice, open.summary.id, Some(&d.bob)).await;
    let hidden_to_bob = d.notate(&d.alice, hidden.summary.id, Some(&d.bob)).await;
    let mine = d.notate(&d.bob, open.summary.id, None).await;

    let ids = |list: Vec<Notation>| list.into_iter().map(|n| n.id).collect::<Vec<_>>();
    // Newest first.
    assert_eq!(
        ids(d.inbox(&d.bob, "").await),
        [mine.id, hidden_to_bob.id, to_bob.id]
    );
    assert_eq!(ids(d.inbox(&d.bob, "scope=created").await), [mine.id]);
    assert_eq!(
        ids(d.inbox(&d.alice, "scope=created").await),
        [hidden_to_bob.id, to_bob.id]
    );
    assert_eq!(
        ids(d.inbox(&d.bob, "q=roadmap").await),
        [mine.id, to_bob.id]
    );
    // Completed ones are listed apart from open ones.
    d.complete(&d.bob, &to_bob, true).await;
    assert_eq!(ids(d.inbox(&d.bob, "completed=true").await), [to_bob.id]);
    assert_eq!(ids(d.inbox(&d.bob, "").await), [mine.id, hidden_to_bob.id]);
    // Someone who left the private channel no longer sees its notation, and trash hides a page's.
    d.app.remove_from_conversation(secret, &d.bob).await;
    d.trash(&d.alice, open.summary.id).await;
    assert!(d.inbox(&d.bob, "").await.is_empty());
    assert!(d.inbox(&d.owner, "").await.is_empty());
    finish(d).await;
}

#[sqlx::test]
async fn notations_go_with_their_page_and_are_removed_when_it_is_purged(pool: PgPool) {
    let d = docs(pool).await;
    let secret = d.secret().await;
    let page = d.page(&d.alice, "Plan", "alpha beta gamma").await;
    let id = page.summary.id;
    let n = d.notate(&d.alice, id, None).await;
    let moved = Docs::page_from(
        d.edit_raw(&d.alice, &page, json!({"conversation_id": secret}))
            .await,
    )
    .await;
    assert_eq!(moved.summary.open_notations, 1);
    let found = d.notations_of(&d.bob, id).await;
    assert_eq!(found[0].conversation_id, secret);
    denied(
        d.app
            .call(Method::GET, &doc_paths::page_notations(id), &d.owner, None)
            .await,
        ErrorCode::NotFound,
    )
    .await;
    d.trash(&d.alice, id).await;
    assert_eq!(
        d.action_raw(&d.alice, Method::POST, &doc_paths::page_purge(id), op())
            .await
            .status(),
        204
    );
    let rows: i64 = sqlx::query_scalar("SELECT count(*) FROM notations WHERE id = $1")
        .bind(n.id.as_uuid())
        .fetch_one(&d.app.pool)
        .await
        .unwrap();
    assert_eq!(rows, 0);
    finish(d).await;
}

#[test]
fn the_protocol_is_at_the_version_that_added_docs() {
    assert_eq!(lazy_protocol::PROTOCOL_VERSION, 15);
}

#[allow(dead_code)]
fn unused(_: ConversationId, _: PageDetail) {}
