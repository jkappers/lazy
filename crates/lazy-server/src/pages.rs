//! Pages and boards (llm-docs/DECISIONS.md, D30): documents that belong to a conversation.
//!
//! A page holds markdown, a board holds shapes and connectors, and both share a title, a
//! revision, versions, and the trash. Who may read or write follows the conversation
//! ([`lazy_domain::PageAccess`]): a page in a conversation the caller cannot read does not exist
//! for them.
//!
//! # Locks
//!
//! Every write takes the conversation row `FOR SHARE` (so it never overlaps an archive or a
//! membership change), then the page row `FOR UPDATE`, then does its other writes, and takes the
//! workspace row (for the event) last, as everywhere else. A move also takes the destination
//! conversation `FOR SHARE`. Creating, restoring, and moving into a conversation take
//! `pages:{conversation}` (advisory) so the limit on live pages is exact.
//!
//! # Revisions
//!
//! `revision` rises by one with every change to the title, the content, or the conversation. The
//! trash, files, and notations do not change it; a summary at the same revision can therefore
//! differ in `trashed_at`, `file_count`, and `open_notations`, and a client applies it when any of
//! them does.

use lazy_domain::page::{
    MAX_PAGES_PER_CONVERSATION, MAX_VERSIONS_PER_PAGE, PageContent, PageKind, TRASH_RETENTION_DAYS,
    VersionReason, excerpt, normalize_title, should_snapshot,
};
use lazy_domain::{
    Board, ConversationId, OperationId, PageAccess, PageId, Timestamp, UserId, WorkspaceId,
};
use lazy_protocol::{
    CreatePageRequest, ErrorCode, Event, PageDetail, PageListParams, PageListResponse, PageSort,
    PageState, PageSummary, PageVersionDetail, PageVersionSummary, PageVersionsResponse,
    RestoreVersionRequest, UpdatePageRequest,
};
use sqlx::types::Json;
use sqlx::{PgConnection, PgPool, Postgres, Transaction};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::attachments;
use crate::conversations::{self, Loaded, Lock};
use crate::crypto::command_fingerprint;
use crate::error::AppError;
use crate::events::{self, Audience, PendingEvent};
use crate::receipts::{self, Receipt, ReceiptKind, ReceiptTarget};

/// Most files on one page.
pub const MAX_FILES_PER_PAGE: i64 = 100;
/// Default and largest page of a list.
pub const DEFAULT_LIST_LIMIT: u32 = 50;
pub const MAX_LIST_LIMIT: u32 = 100;

pub(crate) fn page_not_found() -> AppError {
    AppError::not_found("page not found")
}

fn cannot_edit() -> AppError {
    AppError::forbidden("you cannot change pages in this conversation")
}

// --- Rows ------------------------------------------------------------------------------------

#[derive(sqlx::FromRow)]
pub(crate) struct PageRow {
    pub id: Uuid,
    pub workspace_id: Uuid,
    pub conversation_id: Uuid,
    pub kind: String,
    pub title: String,
    pub excerpt: String,
    pub revision: i32,
    pub created_by: Uuid,
    pub created_at: OffsetDateTime,
    pub updated_by: Uuid,
    pub updated_at: OffsetDateTime,
    pub trashed_at: Option<OffsetDateTime>,
    pub version_head: i32,
    pub open_notations: i64,
    pub file_count: i64,
}

macro_rules! page_select {
    ($tail:literal) => {
        concat!(
            "SELECT p.id, p.workspace_id, p.conversation_id, p.kind, p.title, p.excerpt, p.revision, \
                    p.created_by, p.created_at, p.updated_by, p.updated_at, p.trashed_at, p.version_head, \
                    (SELECT count(*) FROM notations n WHERE n.page_id = p.id AND n.completed_at IS NULL) \
                        AS open_notations, \
                    (SELECT count(*) FROM attachments a WHERE a.page_id = p.id AND a.deleted_at IS NULL) \
                        AS file_count \
             FROM pages p ",
            $tail
        )
    };
}

fn kind_from_db(text: &str) -> Result<PageKind, AppError> {
    match text {
        "page" => Ok(PageKind::Page),
        "board" => Ok(PageKind::Board),
        other => Err(AppError::internal(format!(
            "unknown page kind {other:?} in database"
        ))),
    }
}

fn kind_to_db(kind: PageKind) -> &'static str {
    match kind {
        PageKind::Page => "page",
        PageKind::Board => "board",
    }
}

impl PageRow {
    pub(crate) fn to_summary(&self) -> Result<PageSummary, AppError> {
        let trashed = self.trashed_at.map(Timestamp::from);
        Ok(PageSummary {
            id: PageId::from_uuid(self.id),
            workspace_id: WorkspaceId::from_uuid(self.workspace_id),
            conversation_id: ConversationId::from_uuid(self.conversation_id),
            kind: kind_from_db(&self.kind)?,
            title: self.title.clone(),
            excerpt: self.excerpt.clone(),
            created_by: UserId::from_uuid(self.created_by),
            created_at: Timestamp::from(self.created_at),
            updated_by: UserId::from_uuid(self.updated_by),
            updated_at: Timestamp::from(self.updated_at),
            revision: u32::try_from(self.revision).unwrap_or(1),
            trashed_at: trashed,
            purge_at: self
                .trashed_at
                .map(|at| Timestamp::from(at + time::Duration::days(TRASH_RETENTION_DAYS))),
            open_notations: u32::try_from(self.open_notations).unwrap_or(u32::MAX),
            file_count: u32::try_from(self.file_count).unwrap_or(u32::MAX),
        })
    }
}

pub(crate) async fn find_row(
    conn: &mut PgConnection,
    id: PageId,
) -> Result<Option<PageRow>, AppError> {
    Ok(sqlx::query_as(page_select!("WHERE p.id = $1"))
        .bind(id.as_uuid())
        .fetch_optional(conn)
        .await?)
}

#[derive(sqlx::FromRow)]
struct ContentRow {
    title: String,
    body: Option<String>,
    board: Option<Json<Board>>,
}

impl ContentRow {
    fn content(self) -> Result<PageContent, AppError> {
        match (self.body, self.board) {
            (Some(body), None) => Ok(PageContent::Page { body }),
            (None, Some(Json(board))) => Ok(PageContent::Board { board }),
            _ => Err(AppError::internal(
                "a page has both or neither of text and board",
            )),
        }
    }
}

async fn content_of(conn: &mut PgConnection, id: Uuid) -> Result<(String, PageContent), AppError> {
    let row: ContentRow = sqlx::query_as("SELECT title, body, board FROM pages WHERE id = $1")
        .bind(id)
        .fetch_one(conn)
        .await?;
    let title = row.title.clone();
    Ok((title, row.content()?))
}

fn parts(content: &PageContent) -> (Option<&str>, Option<Json<&Board>>) {
    match content {
        PageContent::Page { body } => (Some(body), None),
        PageContent::Board { board } => (None, Some(Json(board))),
    }
}

/// The page as a client reads it: summary, content, files.
pub(crate) async fn detail(conn: &mut PgConnection, id: PageId) -> Result<PageDetail, AppError> {
    let row = find_row(conn, id).await?.ok_or_else(page_not_found)?;
    let (_, content) = content_of(conn, id.as_uuid()).await?;
    let files = attachments::for_page(conn, id.as_uuid()).await?;
    Ok(PageDetail {
        summary: row.to_summary()?,
        content,
        files,
    })
}

// --- Access ----------------------------------------------------------------------------------

/// A page and the caller's relationship to its conversation, for reading. `not_found` when the
/// page does not exist or the caller cannot read its conversation.
pub(crate) async fn load_readable(
    conn: &mut PgConnection,
    user: UserId,
    id: PageId,
) -> Result<(PageRow, Loaded), AppError> {
    let row = find_row(conn, id).await?.ok_or_else(page_not_found)?;
    let loaded = conversations::load(conn, user, ConversationId::from_uuid(row.conversation_id))
        .await?
        .filter(Loaded::can_read)
        .ok_or_else(page_not_found)?;
    Ok((row, loaded))
}

/// Which trash state an operation needs.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Wanted {
    Live,
    Trashed,
    Any,
}

/// Starts a write: the page's conversation `FOR SHARE`, the page row `FOR UPDATE`, and the page as
/// it is now. The caller must be able to post in the conversation.
pub(crate) async fn begin_write(
    pool: &PgPool,
    user: UserId,
    id: PageId,
    wanted: Wanted,
) -> Result<(Transaction<'static, Postgres>, Loaded, PageRow), AppError> {
    // A page can move between finding its conversation and locking it; look again if it did.
    for _ in 0..5 {
        let conversation: Option<Uuid> =
            sqlx::query_scalar("SELECT conversation_id FROM pages WHERE id = $1")
                .bind(id.as_uuid())
                .fetch_optional(pool)
                .await?;
        let conversation = conversation.ok_or_else(page_not_found)?;
        let (mut tx, loaded) = conversations::begin_visible(
            pool,
            user,
            ConversationId::from_uuid(conversation),
            Lock::Shared,
        )
        .await
        .map_err(|error| match &error {
            AppError::Api(api) if api.code == ErrorCode::NotFound => page_not_found(),
            _ => error,
        })?;
        let locked: Option<Uuid> =
            sqlx::query_scalar("SELECT conversation_id FROM pages WHERE id = $1 FOR UPDATE")
                .bind(id.as_uuid())
                .fetch_optional(&mut *tx)
                .await?;
        match locked {
            None => return Err(page_not_found()),
            Some(now) if now != conversation => continue,
            Some(_) => {}
        }
        if !PageAccess::of(&loaded.access()).can_edit() {
            return Err(cannot_edit());
        }
        let row = find_row(&mut tx, id).await?.ok_or_else(page_not_found)?;
        match (wanted, row.trashed_at.is_some()) {
            (Wanted::Live, true) => return Err(page_not_found()),
            (Wanted::Trashed, false) => {
                return Err(AppError::invalid("only a trashed page can be purged"));
            }
            _ => {}
        }
        return Ok((tx, loaded, row));
    }
    Err(AppError::conflict("the page keeps moving; try again"))
}

async fn lock_capacity(conn: &mut PgConnection, conversation: Uuid) -> Result<(), AppError> {
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1, 0))")
        .bind(format!("pages:{conversation}"))
        .execute(conn)
        .await?;
    Ok(())
}

/// Refuses when the conversation already has the most live pages. Call after [`lock_capacity`].
async fn ensure_room(conn: &mut PgConnection, conversation: Uuid) -> Result<(), AppError> {
    let live: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM pages WHERE conversation_id = $1 AND trashed_at IS NULL",
    )
    .bind(conversation)
    .fetch_one(conn)
    .await?;
    if live >= MAX_PAGES_PER_CONVERSATION as i64 {
        return Err(AppError::new(
            ErrorCode::LimitReached,
            format!("a conversation has at most {MAX_PAGES_PER_CONVERSATION} pages and boards"),
        ));
    }
    Ok(())
}

// --- Events ----------------------------------------------------------------------------------

async fn announce_changed(
    conn: &mut PgConnection,
    page: PageId,
    operation: Option<OperationId>,
    removed_from: Option<ConversationId>,
) -> Result<(), AppError> {
    let row = find_row(conn, page).await?.ok_or_else(page_not_found)?;
    let summary = row.to_summary()?;
    let (workspace, conversation) = (summary.workspace_id, summary.conversation_id);
    let mut pending = Vec::with_capacity(2);
    if let Some(old) = removed_from {
        pending.push(PendingEvent {
            kind: "page_removed",
            conversation_id: Some(old),
            operation_id: None,
            audience: Audience::Members,
            event: Event::PageRemoved {
                conversation_id: old,
                page_id: page,
            },
        });
    }
    pending.push(PendingEvent {
        kind: "page_changed",
        conversation_id: Some(conversation),
        operation_id: operation,
        audience: Audience::Members,
        event: Event::PageChanged {
            page: summary,
            operation_id: operation,
        },
    });
    events::append_all(conn, workspace, pending).await?;
    Ok(())
}

/// Announces a change to a page's files or notations count (no revision change).
pub(crate) async fn announce_counts(conn: &mut PgConnection, page: PageId) -> Result<(), AppError> {
    announce_changed(conn, page, None, None).await
}

// --- Receipts --------------------------------------------------------------------------------

fn fingerprint(tag: &str, parts: &[&[u8]]) -> [u8; 32] {
    command_fingerprint(tag, parts)
}

/// Claims `operation` for a docs command. `Some(page)` means the command was already done: the
/// page as it is now, for the caller to return.
async fn claim(
    tx: &mut Transaction<'static, Postgres>,
    user: UserId,
    operation: OperationId,
    kind: ReceiptKind,
    fp: &[u8; 32],
    target: Uuid,
) -> Result<Option<Receipt>, AppError> {
    match receipts::claim(tx, user, operation, kind, fp, ReceiptTarget::Docs(target)).await? {
        None => Ok(None),
        Some(existing) => {
            existing.ensure_same(kind, fp)?;
            Ok(Some(existing))
        }
    }
}

/// The result of a repeated command: the page as it is now, if the caller can still read it.
async fn repeat_result(
    tx: &mut Transaction<'static, Postgres>,
    user: UserId,
    page: PageId,
) -> Result<PageDetail, AppError> {
    load_readable(tx, user, page).await?;
    detail(tx, page).await
}

// --- Creating --------------------------------------------------------------------------------

fn content_error(error: lazy_domain::page::PageError) -> AppError {
    AppError::invalid(error.to_string())
}

/// `POST /v1/conversations/{id}/pages`.
pub async fn create(
    pool: &PgPool,
    user: UserId,
    conversation: ConversationId,
    request: CreatePageRequest,
) -> Result<PageDetail, AppError> {
    let title = normalize_title(&request.title).map_err(content_error)?;
    let content = match request.content {
        Some(content) => {
            if content.kind() != request.kind {
                return Err(AppError::invalid("the content is not of the page's kind"));
            }
            content
        }
        None => PageContent::empty(request.kind),
    };
    content.validate().map_err(content_error)?;
    let content_json = serde_json::to_vec(&content)
        .map_err(|e| AppError::internal(format!("cannot serialize content: {e}")))?;
    let fp = fingerprint(
        "create_page",
        &[
            conversation.as_uuid().as_bytes(),
            request.page_id.as_uuid().as_bytes(),
            title.as_bytes(),
            &content_json,
        ],
    );

    let (mut tx, loaded) =
        conversations::begin_visible(pool, user, conversation, Lock::Shared).await?;
    if !PageAccess::of(&loaded.access()).can_edit() {
        return Err(cannot_edit());
    }
    if let Some(_existing) = claim(
        &mut tx,
        user,
        request.operation_id,
        ReceiptKind::CreatePage,
        &fp,
        request.page_id.as_uuid(),
    )
    .await?
    {
        return repeat_result(&mut tx, user, request.page_id).await;
    }
    let taken: Option<bool> = sqlx::query_scalar("SELECT true FROM pages WHERE id = $1")
        .bind(request.page_id.as_uuid())
        .fetch_optional(&mut *tx)
        .await?;
    if taken.is_some() {
        return Err(AppError::conflict(
            "that page id is already used for something else",
        ));
    }
    lock_capacity(&mut tx, conversation.as_uuid()).await?;
    ensure_room(&mut tx, conversation.as_uuid()).await?;

    let now: OffsetDateTime = sqlx::query_scalar("SELECT clock_timestamp()")
        .fetch_one(&mut *tx)
        .await?;
    let (body, board) = parts(&content);
    sqlx::query(
        "INSERT INTO pages (id, workspace_id, conversation_id, kind, title, body, board, excerpt, \
                            created_by, created_at, updated_by, updated_at) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $9, $10)",
    )
    .bind(request.page_id.as_uuid())
    .bind(loaded.workspace_id.as_uuid())
    .bind(conversation.as_uuid())
    .bind(kind_to_db(request.kind))
    .bind(&title)
    .bind(body)
    .bind(board)
    .bind(excerpt(&content.plain_text()))
    .bind(user.as_uuid())
    .bind(now)
    .execute(&mut *tx)
    .await?;
    announce_changed(&mut tx, request.page_id, Some(request.operation_id), None).await?;
    let page = detail(&mut tx, request.page_id).await?;
    tx.commit().await?;
    Ok(page)
}

// --- Reading ---------------------------------------------------------------------------------

/// `GET /v1/pages/{id}`. A trashed page can be read (to look at it before restoring it).
pub async fn get(pool: &PgPool, user: UserId, id: PageId) -> Result<PageDetail, AppError> {
    let mut conn = pool.acquire().await?;
    load_readable(&mut conn, user, id).await?;
    detail(&mut conn, id).await
}

fn escape_like(term: &str) -> String {
    let mut out = String::with_capacity(term.len() + 2);
    for c in term.chars() {
        if matches!(c, '%' | '_' | '\\') {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

/// Pages of a workspace the caller can read, optionally of one conversation.
async fn list_pages(
    pool: &PgPool,
    user: UserId,
    workspace: WorkspaceId,
    conversation: Option<ConversationId>,
    params: PageListParams,
) -> Result<PageListResponse, AppError> {
    let limit = params
        .limit
        .unwrap_or(DEFAULT_LIST_LIMIT)
        .clamp(1, MAX_LIST_LIMIT);
    let offset = params.offset.unwrap_or(0);
    let q = params
        .q
        .as_deref()
        .map(str::trim)
        .filter(|q| !q.is_empty())
        .map(str::to_owned);
    let like = q.as_deref().map(|q| format!("%{}%", escape_like(q)));
    let rows: Vec<PageRow> = sqlx::query_as(page_select!(
        "JOIN conversations c ON c.id = p.conversation_id \
         WHERE p.workspace_id = $1 AND ($2::uuid IS NULL OR p.conversation_id = $2) \
           AND (c.visibility = 'public' OR EXISTS ( \
                SELECT 1 FROM conversation_members cm \
                WHERE cm.conversation_id = c.id AND cm.user_id = $3)) \
           AND (p.trashed_at IS NOT NULL) = $4 \
           AND ($5::text IS NULL OR p.kind = $5) \
           AND ($6::text IS NULL OR p.search_vector @@ plainto_tsquery('simple', $6) \
                OR p.title ILIKE $7) \
         ORDER BY (CASE WHEN $8 THEN lower(p.title) END) ASC, \
                  (CASE WHEN NOT $8 AND $4 THEN p.trashed_at END) DESC, \
                  (CASE WHEN NOT $8 AND NOT $4 THEN p.updated_at END) DESC, \
                  p.id DESC \
         LIMIT $9 OFFSET $10"
    ))
    .bind(workspace.as_uuid())
    .bind(conversation.map(|id| id.as_uuid()))
    .bind(user.as_uuid())
    .bind(params.state == PageState::Trashed)
    .bind(params.kind.map(kind_to_db))
    .bind(q.as_deref())
    .bind(like.as_deref())
    .bind(params.sort == PageSort::Title)
    .bind(i64::from(limit) + 1)
    .bind(i64::from(offset))
    .fetch_all(pool)
    .await?;
    let has_more = rows.len() > limit as usize;
    let pages = rows
        .iter()
        .take(limit as usize)
        .map(PageRow::to_summary)
        .collect::<Result<Vec<_>, _>>()?;
    Ok(PageListResponse { pages, has_more })
}

/// `GET /v1/conversations/{id}/pages`.
pub async fn list_conversation(
    pool: &PgPool,
    user: UserId,
    conversation: ConversationId,
    params: PageListParams,
) -> Result<PageListResponse, AppError> {
    let mut conn = pool.acquire().await?;
    let loaded = conversations::load(&mut conn, user, conversation)
        .await?
        .filter(Loaded::can_read)
        .ok_or_else(|| AppError::not_found("conversation not found"))?;
    drop(conn);
    list_pages(pool, user, loaded.workspace_id, Some(conversation), params).await
}

/// `GET /v1/workspaces/{id}/pages`.
pub async fn list_workspace(
    pool: &PgPool,
    user: UserId,
    workspace: WorkspaceId,
    params: PageListParams,
) -> Result<PageListResponse, AppError> {
    let mut conn = pool.acquire().await?;
    crate::access::require_member(&mut conn, workspace, user).await?;
    drop(conn);
    list_pages(pool, user, workspace, None, params).await
}

// --- Changing --------------------------------------------------------------------------------

/// Keeps the page's current state as a version, if the rules say to.
async fn snapshot(
    tx: &mut Transaction<'static, Postgres>,
    row: &PageRow,
    user: UserId,
    reason: VersionReason,
) -> Result<(), AppError> {
    #[derive(sqlx::FromRow)]
    struct Latest {
        title: String,
        body: Option<String>,
        board: Option<Json<Board>>,
        age: f64,
    }
    let latest: Option<Latest> = sqlx::query_as(
        "SELECT title, body, board, EXTRACT(EPOCH FROM clock_timestamp() - created_at)::float8 AS age \
         FROM page_versions WHERE page_id = $1 ORDER BY version DESC LIMIT 1",
    )
    .bind(row.id)
    .fetch_optional(&mut **tx)
    .await?;
    let (title, content) = content_of(tx, row.id).await?;
    let same = latest.as_ref().is_some_and(|latest| {
        latest.title == title
            && match (&content, &latest.body, &latest.board) {
                (PageContent::Page { body }, Some(latest_body), None) => body == latest_body,
                (PageContent::Board { board }, None, Some(Json(latest_board))) => {
                    board == latest_board
                }
                _ => false,
            }
    });
    let age = latest.as_ref().map(|latest| latest.age as i64);
    if !should_snapshot(reason, age, same) {
        return Ok(());
    }
    let version = row.version_head + 1;
    let (body, board) = parts(&content);
    sqlx::query(
        "INSERT INTO page_versions (page_id, version, title, body, board, reason, created_by, \
                                    created_at, revision) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, clock_timestamp(), $8)",
    )
    .bind(row.id)
    .bind(version)
    .bind(&title)
    .bind(body)
    .bind(board)
    .bind(reason.as_str())
    .bind(user.as_uuid())
    .bind(row.revision)
    .execute(&mut **tx)
    .await?;
    sqlx::query("UPDATE pages SET version_head = $2 WHERE id = $1")
        .bind(row.id)
        .bind(version)
        .execute(&mut **tx)
        .await?;
    // Numbers are contiguous and only the oldest are removed, so the cut is arithmetic.
    sqlx::query("DELETE FROM page_versions WHERE page_id = $1 AND version <= $2")
        .bind(row.id)
        .bind(version - MAX_VERSIONS_PER_PAGE as i32)
        .execute(&mut **tx)
        .await?;
    Ok(())
}

/// What a change does to a page.
struct Change {
    title: String,
    content: PageContent,
    destination: Option<Loaded>,
}

/// Applies `change` to the locked page. Returns whether anything changed (nothing is written, and
/// no event appended, when not).
async fn apply(
    tx: &mut Transaction<'static, Postgres>,
    user: UserId,
    row: &PageRow,
    change: Change,
    reason: VersionReason,
    operation: OperationId,
) -> Result<bool, AppError> {
    let (title, content) = content_of(tx, row.id).await?;
    let content_changes = change.content != content || change.title != title;
    let moves = change
        .destination
        .as_ref()
        .is_some_and(|dest| dest.id.as_uuid() != row.conversation_id);
    if !content_changes && !moves {
        return Ok(false);
    }
    if content_changes {
        snapshot(tx, row, user, reason).await?;
    }
    if let Some(dest) = change.destination.as_ref().filter(|_| moves) {
        lock_capacity(tx, dest.id.as_uuid()).await?;
        if row.trashed_at.is_none() {
            ensure_room(tx, dest.id.as_uuid()).await?;
        }
    }
    let (body, board) = parts(&change.content);
    let destination = change
        .destination
        .as_ref()
        .filter(|_| moves)
        .map_or(row.conversation_id, |dest| dest.id.as_uuid());
    let now: OffsetDateTime = sqlx::query_scalar("SELECT clock_timestamp()")
        .fetch_one(&mut **tx)
        .await?;
    sqlx::query(
        "UPDATE pages SET title = $2, body = $3, board = $4, excerpt = $5, revision = revision + 1, \
                          updated_by = $6, updated_at = $7, conversation_id = $8 \
         WHERE id = $1",
    )
    .bind(row.id)
    .bind(&change.title)
    .bind(body)
    .bind(board)
    .bind(excerpt(&change.content.plain_text()))
    .bind(user.as_uuid())
    .bind(now)
    .bind(destination)
    .execute(&mut **tx)
    .await?;
    if moves {
        // Its files follow it, so they stay readable by the same people as the page.
        sqlx::query("UPDATE attachments SET conversation_id = $2 WHERE page_id = $1")
            .bind(row.id)
            .bind(destination)
            .execute(&mut **tx)
            .await?;
    }
    announce_changed(
        tx,
        PageId::from_uuid(row.id),
        Some(operation),
        moves.then(|| ConversationId::from_uuid(row.conversation_id)),
    )
    .await?;
    Ok(true)
}

fn stale() -> AppError {
    AppError::new(
        ErrorCode::StaleRevision,
        "the page changed since you opened it; fetch it and reconcile",
    )
}

/// `PATCH /v1/pages/{id}`.
pub async fn update(
    pool: &PgPool,
    user: UserId,
    id: PageId,
    request: UpdatePageRequest,
) -> Result<PageDetail, AppError> {
    if request.content.is_some() && request.append.is_some() {
        return Err(AppError::invalid("give content or append, not both"));
    }
    let title = request
        .title
        .as_deref()
        .map(normalize_title)
        .transpose()
        .map_err(content_error)?;
    if let Some(content) = &request.content {
        content.validate().map_err(content_error)?;
    }
    let content_json = serde_json::to_vec(&request.content)
        .map_err(|e| AppError::internal(format!("cannot serialize content: {e}")))?;
    let destination_bytes = request
        .conversation_id
        .map(|c| c.as_uuid().into_bytes())
        .unwrap_or_default();
    let revision = request.base_revision.to_be_bytes();
    let fp = fingerprint(
        "update_page",
        &[
            id.as_uuid().as_bytes(),
            &revision,
            title.as_deref().unwrap_or("\u{0}").as_bytes(),
            &content_json,
            request.append.as_deref().unwrap_or("\u{0}").as_bytes(),
            &destination_bytes,
        ],
    );

    let (mut tx, loaded, row) = begin_write(pool, user, id, Wanted::Live).await?;
    if claim(
        &mut tx,
        user,
        request.operation_id,
        ReceiptKind::ChangePage,
        &fp,
        id.as_uuid(),
    )
    .await?
    .is_some()
    {
        return repeat_result(&mut tx, user, id).await;
    }
    if u32::try_from(row.revision).ok() != Some(request.base_revision) {
        return Err(stale());
    }
    let kind = kind_from_db(&row.kind)?;
    let (current_title, current) = content_of(&mut tx, row.id).await?;
    let content = match (request.content, request.append) {
        (Some(content), _) => {
            if content.kind() != kind {
                return Err(AppError::invalid("the content is not of the page's kind"));
            }
            content
        }
        (None, Some(text)) => match current {
            PageContent::Page { body } => {
                let joined = if body.trim().is_empty() {
                    text
                } else {
                    format!("{}\n\n{text}", body.trim_end())
                };
                let content = PageContent::Page { body: joined };
                content.validate().map_err(content_error)?;
                content
            }
            PageContent::Board { .. } => {
                return Err(AppError::invalid("only a page takes appended text"));
            }
        },
        (None, None) => current,
    };
    let destination = match request.conversation_id {
        Some(target) if target.as_uuid() != row.conversation_id => {
            let dest = conversations::join_visible(&mut tx, user, target, Lock::Shared)
                .await
                .map_err(|error| match &error {
                    AppError::Api(api) if api.code == ErrorCode::NotFound => {
                        AppError::not_found("conversation not found")
                    }
                    _ => error,
                })?;
            if dest.workspace_id != loaded.workspace_id {
                return Err(AppError::not_found("conversation not found"));
            }
            if !PageAccess::of(&dest.access()).can_edit() {
                return Err(cannot_edit());
            }
            Some(dest)
        }
        _ => None,
    };
    apply(
        &mut tx,
        user,
        &row,
        Change {
            title: title.unwrap_or(current_title),
            content,
            destination,
        },
        VersionReason::Autosave,
        request.operation_id,
    )
    .await?;
    let page = detail(&mut tx, id).await?;
    tx.commit().await?;
    Ok(page)
}

// --- Trash -----------------------------------------------------------------------------------

/// `DELETE /v1/pages/{id}`: moves the page to the trash. A trashed page stays for
/// [`TRASH_RETENTION_DAYS`] days, then the retention job removes it.
pub async fn trash(
    pool: &PgPool,
    user: UserId,
    id: PageId,
    operation: OperationId,
) -> Result<PageDetail, AppError> {
    let fp = fingerprint("trash_page", &[id.as_uuid().as_bytes()]);
    let (mut tx, _loaded, row) = begin_write(pool, user, id, Wanted::Any).await?;
    if claim(
        &mut tx,
        user,
        operation,
        ReceiptKind::ChangePage,
        &fp,
        id.as_uuid(),
    )
    .await?
    .is_some()
        || row.trashed_at.is_some()
    {
        let page = repeat_result(&mut tx, user, id).await?;
        tx.commit().await?;
        return Ok(page);
    }
    sqlx::query("UPDATE pages SET trashed_at = clock_timestamp() WHERE id = $1")
        .bind(row.id)
        .execute(&mut *tx)
        .await?;
    announce_changed(&mut tx, id, Some(operation), None).await?;
    let page = detail(&mut tx, id).await?;
    tx.commit().await?;
    Ok(page)
}

/// `POST /v1/pages/{id}/restore`: takes a page out of the trash. A page that is not in the trash
/// is returned as it is.
pub async fn restore(
    pool: &PgPool,
    user: UserId,
    id: PageId,
    operation: OperationId,
) -> Result<PageDetail, AppError> {
    let fp = fingerprint("restore_page", &[id.as_uuid().as_bytes()]);
    let (mut tx, _loaded, row) = begin_write(pool, user, id, Wanted::Any).await?;
    if claim(
        &mut tx,
        user,
        operation,
        ReceiptKind::ChangePage,
        &fp,
        id.as_uuid(),
    )
    .await?
    .is_some()
        || row.trashed_at.is_none()
    {
        let page = repeat_result(&mut tx, user, id).await?;
        tx.commit().await?;
        return Ok(page);
    }
    lock_capacity(&mut tx, row.conversation_id).await?;
    ensure_room(&mut tx, row.conversation_id).await?;
    sqlx::query("UPDATE pages SET trashed_at = NULL WHERE id = $1")
        .bind(row.id)
        .execute(&mut *tx)
        .await?;
    announce_changed(&mut tx, id, Some(operation), None).await?;
    let page = detail(&mut tx, id).await?;
    tx.commit().await?;
    Ok(page)
}

/// Removes a locked page for good: its files are marked deleted (the cleanup job removes their
/// bytes), the row goes with its versions and notations, and members are told.
async fn remove(tx: &mut Transaction<'static, Postgres>, row: &PageRow) -> Result<(), AppError> {
    sqlx::query(
        "UPDATE attachments SET deleted_at = now() WHERE page_id = $1 AND deleted_at IS NULL",
    )
    .bind(row.id)
    .execute(&mut **tx)
    .await?;
    sqlx::query("DELETE FROM pages WHERE id = $1")
        .bind(row.id)
        .execute(&mut **tx)
        .await?;
    let conversation = ConversationId::from_uuid(row.conversation_id);
    events::append_all(
        tx,
        WorkspaceId::from_uuid(row.workspace_id),
        vec![PendingEvent {
            kind: "page_removed",
            conversation_id: Some(conversation),
            operation_id: None,
            audience: Audience::Members,
            event: Event::PageRemoved {
                conversation_id: conversation,
                page_id: PageId::from_uuid(row.id),
            },
        }],
    )
    .await?;
    Ok(())
}

/// `POST /v1/pages/{id}/purge`: removes a trashed page for good. Repeating the operation after it
/// succeeded succeeds again.
pub async fn purge(
    pool: &PgPool,
    user: UserId,
    id: PageId,
    operation: OperationId,
) -> Result<(), AppError> {
    let fp = fingerprint("purge_page", &[id.as_uuid().as_bytes()]);
    let (mut tx, _loaded, row) = match begin_write(pool, user, id, Wanted::Trashed).await {
        Ok(started) => started,
        Err(error) => {
            // Gone already: it is a repeat if this operation removed it.
            let mut conn = pool.acquire().await?;
            if let Some(receipt) = receipts::find(&mut conn, user, operation).await?
                && receipt.ensure_same(ReceiptKind::ChangePage, &fp).is_ok()
                && receipt.target_id == Some(id.as_uuid())
                && find_row(&mut conn, id).await?.is_none()
            {
                return Ok(());
            }
            return Err(error);
        }
    };
    if claim(
        &mut tx,
        user,
        operation,
        ReceiptKind::ChangePage,
        &fp,
        id.as_uuid(),
    )
    .await?
    .is_some()
    {
        return Ok(());
    }
    remove(&mut tx, &row).await?;
    tx.commit().await?;
    Ok(())
}

/// The retention job: removes pages that have been in the trash longer than `days`. Returns how
/// many it removed. Each page is its own transaction, so a busy workspace is never held long.
pub async fn purge_expired(pool: &PgPool, days: i64) -> Result<usize, AppError> {
    let mut removed = 0;
    loop {
        let mut tx = pool.begin().await?;
        // Skipping locked rows: a page being restored right now is not expired.
        let candidate: Option<Uuid> = sqlx::query_scalar(
            "SELECT id FROM pages WHERE trashed_at IS NOT NULL \
               AND trashed_at < now() - make_interval(days => $1::int) \
             ORDER BY trashed_at, id LIMIT 1 FOR UPDATE SKIP LOCKED",
        )
        .bind(i32::try_from(days).unwrap_or(i32::MAX))
        .fetch_optional(&mut *tx)
        .await?;
        let Some(candidate) = candidate else {
            return Ok(removed);
        };
        let row = find_row(&mut tx, PageId::from_uuid(candidate))
            .await?
            .ok_or_else(page_not_found)?;
        remove(&mut tx, &row).await?;
        tx.commit().await?;
        removed += 1;
    }
}

// --- Versions --------------------------------------------------------------------------------

#[derive(sqlx::FromRow)]
struct VersionRow {
    version: i32,
    title: String,
    body: Option<String>,
    board: Option<Json<Board>>,
    reason: String,
    created_by: Option<Uuid>,
    created_at: OffsetDateTime,
    revision: i32,
}

impl VersionRow {
    fn summary(&self, page: PageId) -> Result<PageVersionSummary, AppError> {
        Ok(PageVersionSummary {
            page_id: page,
            version: u32::try_from(self.version).unwrap_or(0),
            title: self.title.clone(),
            reason: VersionReason::parse(&self.reason)
                .ok_or_else(|| AppError::internal("unknown version reason in database"))?,
            created_by: self.created_by.map(UserId::from_uuid),
            created_at: Timestamp::from(self.created_at),
            revision: u32::try_from(self.revision).unwrap_or(0),
        })
    }
}

macro_rules! version_select {
    ($tail:literal) => {
        concat!(
            "SELECT version, title, body, board, reason, created_by, created_at, revision \
             FROM page_versions WHERE page_id = $1 ",
            $tail
        )
    };
}

/// `GET /v1/pages/{id}/versions`, newest first.
pub async fn versions(
    pool: &PgPool,
    user: UserId,
    id: PageId,
) -> Result<PageVersionsResponse, AppError> {
    let mut conn = pool.acquire().await?;
    load_readable(&mut conn, user, id).await?;
    let rows: Vec<VersionRow> = sqlx::query_as(version_select!("ORDER BY version DESC"))
        .bind(id.as_uuid())
        .fetch_all(&mut *conn)
        .await?;
    Ok(PageVersionsResponse {
        versions: rows
            .iter()
            .map(|row| row.summary(id))
            .collect::<Result<_, _>>()?,
    })
}

async fn version_row(
    conn: &mut PgConnection,
    id: PageId,
    version: u32,
) -> Result<VersionRow, AppError> {
    sqlx::query_as(version_select!("AND version = $2"))
        .bind(id.as_uuid())
        .bind(i32::try_from(version).unwrap_or(i32::MAX))
        .fetch_optional(conn)
        .await?
        .ok_or_else(|| AppError::not_found("version not found"))
}

/// `GET /v1/pages/{id}/versions/{version}`.
pub async fn get_version(
    pool: &PgPool,
    user: UserId,
    id: PageId,
    version: u32,
) -> Result<PageVersionDetail, AppError> {
    let mut conn = pool.acquire().await?;
    load_readable(&mut conn, user, id).await?;
    let row = version_row(&mut conn, id, version).await?;
    let summary = row.summary(id)?;
    let content = ContentRow {
        title: row.title,
        body: row.body,
        board: row.board,
    }
    .content()?;
    Ok(PageVersionDetail { summary, content })
}

/// `POST /v1/pages/{id}/versions/{version}/restore`: the page's current state is kept as a
/// `pre_restore` version, then the version's title and content become current.
pub async fn restore_version(
    pool: &PgPool,
    user: UserId,
    id: PageId,
    version: u32,
    request: RestoreVersionRequest,
) -> Result<PageDetail, AppError> {
    let fp = fingerprint(
        "restore_version",
        &[
            id.as_uuid().as_bytes(),
            &version.to_be_bytes(),
            &request.base_revision.to_be_bytes(),
        ],
    );
    let (mut tx, _loaded, row) = begin_write(pool, user, id, Wanted::Live).await?;
    if claim(
        &mut tx,
        user,
        request.operation_id,
        ReceiptKind::ChangePage,
        &fp,
        id.as_uuid(),
    )
    .await?
    .is_some()
    {
        return repeat_result(&mut tx, user, id).await;
    }
    if u32::try_from(row.revision).ok() != Some(request.base_revision) {
        return Err(stale());
    }
    let old = version_row(&mut tx, id, version).await?;
    let title = old.title.clone();
    let content = ContentRow {
        title: old.title,
        body: old.body,
        board: old.board,
    }
    .content()?;
    if content.kind() != kind_from_db(&row.kind)? {
        return Err(AppError::invalid("that version is of another kind"));
    }
    apply(
        &mut tx,
        user,
        &row,
        Change {
            title,
            content,
            destination: None,
        },
        VersionReason::PreRestore,
        request.operation_id,
    )
    .await?;
    let page = detail(&mut tx, id).await?;
    tx.commit().await?;
    Ok(page)
}
