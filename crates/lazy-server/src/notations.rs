//! Notations (llm-docs/DECISIONS.md, D30): notes anchored to text on a page, with an assignee,
//! replies, and a done state.
//!
//! A notation is about a page's text, so only pages take them, not boards. Everything follows the
//! page: it is read by whoever can read the page's conversation, and every change needs the
//! ability to post there. The author or the assignee may edit and complete a notation; the
//! author, or an admin or owner of the workspace, may delete it. Replies are allowed while the
//! notation is open and deleted by their author or an admin.
//!
//! # Locks
//!
//! A notation change takes the page's locks first (its conversation `FOR SHARE`, the page row
//! `FOR UPDATE`, see [`crate::pages`]), then the notation row, then writes, then the workspace row
//! for the event. A page's trash, move, and purge therefore never interleave with its notations.

use lazy_domain::page::{NOTATION_BODY_MAX_BYTES, validate_anchor, validate_notation_body};
use lazy_domain::{
    ConversationId, NotationId, NotationReplyId, OperationId, PageId, Timestamp, UserId,
    WorkspaceId, WorkspaceRole,
};
use lazy_protocol::{
    CreateNotationReplyRequest, CreateNotationRequest, ErrorCode, Event, Notation,
    NotationListParams, NotationListResponse, NotationReply, NotationScope,
    SetNotationCompletedRequest, UpdateNotationRequest,
};
use sqlx::{PgConnection, PgPool, Postgres, Transaction};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::conversations::{self, Loaded};
use crate::crypto::command_fingerprint;
use crate::error::AppError;
use crate::events::{self, Audience, PendingEvent};
use crate::pages::{self, Wanted};
use crate::receipts::{self, ReceiptKind, ReceiptTarget};

/// Most notations a workspace list returns.
pub const LIST_LIMIT: i64 = 200;

fn notation_not_found() -> AppError {
    AppError::not_found("notation not found")
}

fn invalid(error: lazy_domain::page::PageError) -> AppError {
    AppError::invalid(error.to_string())
}

// --- Rows ------------------------------------------------------------------------------------

#[derive(sqlx::FromRow)]
struct NotationRow {
    id: Uuid,
    page_id: Uuid,
    workspace_id: Uuid,
    conversation_id: Uuid,
    author_id: Uuid,
    assignee_id: Uuid,
    quote: String,
    from_pos: i32,
    to_pos: i32,
    body: String,
    created_at: OffsetDateTime,
    updated_at: OffsetDateTime,
    completed_at: Option<OffsetDateTime>,
    resolved_by: Option<Uuid>,
}

macro_rules! notation_select {
    ($tail:literal) => {
        concat!(
            "SELECT n.id, n.page_id, p.workspace_id, p.conversation_id, n.author_id, n.assignee_id, \
                    n.quote, n.from_pos, n.to_pos, n.body, n.created_at, n.updated_at, \
                    n.completed_at, n.resolved_by \
             FROM notations n JOIN pages p ON p.id = n.page_id ",
            $tail
        )
    };
}

#[derive(sqlx::FromRow)]
struct ReplyRow {
    id: Uuid,
    notation_id: Uuid,
    author_id: Uuid,
    body: String,
    created_at: OffsetDateTime,
}

impl ReplyRow {
    fn to_reply(&self) -> NotationReply {
        NotationReply {
            id: NotationReplyId::from_uuid(self.id),
            notation_id: NotationId::from_uuid(self.notation_id),
            author_id: UserId::from_uuid(self.author_id),
            body: self.body.clone(),
            created_at: Timestamp::from(self.created_at),
        }
    }
}

impl NotationRow {
    fn to_notation(&self, replies: Vec<NotationReply>) -> Notation {
        Notation {
            id: NotationId::from_uuid(self.id),
            page_id: PageId::from_uuid(self.page_id),
            conversation_id: ConversationId::from_uuid(self.conversation_id),
            author_id: UserId::from_uuid(self.author_id),
            assignee_id: UserId::from_uuid(self.assignee_id),
            quote: self.quote.clone(),
            from: u32::try_from(self.from_pos).unwrap_or(0),
            to: u32::try_from(self.to_pos).unwrap_or(0),
            body: self.body.clone(),
            created_at: Timestamp::from(self.created_at),
            updated_at: Timestamp::from(self.updated_at),
            completed_at: self.completed_at.map(Timestamp::from),
            resolved_by: self.resolved_by.map(UserId::from_uuid),
            replies,
        }
    }
}

/// The notations for `rows`, each with its replies, using one query for all the replies.
async fn with_replies(
    conn: &mut PgConnection,
    rows: Vec<NotationRow>,
) -> Result<Vec<Notation>, AppError> {
    if rows.is_empty() {
        return Ok(Vec::new());
    }
    let ids: Vec<Uuid> = rows.iter().map(|row| row.id).collect();
    let replies: Vec<ReplyRow> = sqlx::query_as(
        "SELECT id, notation_id, author_id, body, created_at FROM notation_replies \
         WHERE notation_id = ANY($1) ORDER BY created_at, id",
    )
    .bind(&ids)
    .fetch_all(conn)
    .await?;
    Ok(rows
        .iter()
        .map(|row| {
            let own = replies
                .iter()
                .filter(|reply| reply.notation_id == row.id)
                .map(ReplyRow::to_reply)
                .collect();
            row.to_notation(own)
        })
        .collect())
}

async fn find_row(conn: &mut PgConnection, id: Uuid) -> Result<Option<NotationRow>, AppError> {
    Ok(sqlx::query_as(notation_select!("WHERE n.id = $1"))
        .bind(id)
        .fetch_optional(conn)
        .await?)
}

async fn fetch(conn: &mut PgConnection, id: Uuid) -> Result<Notation, AppError> {
    let row = find_row(conn, id).await?.ok_or_else(notation_not_found)?;
    let mut all = with_replies(conn, vec![row]).await?;
    all.pop().ok_or_else(notation_not_found)
}

async fn open_count(conn: &mut PgConnection, page: Uuid) -> Result<u32, AppError> {
    let count: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM notations WHERE page_id = $1 AND completed_at IS NULL",
    )
    .bind(page)
    .fetch_one(conn)
    .await?;
    Ok(u32::try_from(count).unwrap_or(u32::MAX))
}

// --- Events and receipts ---------------------------------------------------------------------

async fn announce_changed(
    conn: &mut PgConnection,
    notation: Uuid,
    operation: OperationId,
) -> Result<(), AppError> {
    let row = find_row(conn, notation)
        .await?
        .ok_or_else(notation_not_found)?;
    let (workspace, conversation, page) = (row.workspace_id, row.conversation_id, row.page_id);
    let notation = fetch(conn, notation).await?;
    let open_notations = open_count(conn, page).await?;
    events::append_all(
        conn,
        WorkspaceId::from_uuid(workspace),
        vec![PendingEvent {
            kind: "notation_changed",
            conversation_id: Some(ConversationId::from_uuid(conversation)),
            operation_id: Some(operation),
            audience: Audience::Members,
            event: Event::NotationChanged {
                notation,
                open_notations,
            },
        }],
    )
    .await?;
    Ok(())
}

async fn claim(
    tx: &mut Transaction<'static, Postgres>,
    user: UserId,
    operation: OperationId,
    kind: ReceiptKind,
    fp: &[u8; 32],
    target: Uuid,
) -> Result<bool, AppError> {
    match receipts::claim(tx, user, operation, kind, fp, ReceiptTarget::Docs(target)).await? {
        None => Ok(false),
        Some(existing) => {
            existing.ensure_same(kind, fp)?;
            Ok(true)
        }
    }
}

/// Whether `operation` already did the removal it names (the thing is gone, so the lookup that
/// started the command failed): the receipt says it was this caller's identical command.
async fn removal_already_done(
    pool: &PgPool,
    user: UserId,
    operation: OperationId,
    fp: &[u8; 32],
    target: Uuid,
) -> Result<bool, AppError> {
    let mut conn = pool.acquire().await?;
    Ok(receipts::find(&mut conn, user, operation)
        .await?
        .is_some_and(|receipt| {
            receipt.ensure_same(ReceiptKind::ChangeNotation, fp).is_ok()
                && receipt.target_id == Some(target)
        }))
}

// --- Access ----------------------------------------------------------------------------------

/// Starts a change to an existing notation: the page's locks (the page must be live and the
/// caller able to post), then the notation row.
async fn begin_change(
    pool: &PgPool,
    user: UserId,
    notation: Uuid,
) -> Result<(Transaction<'static, Postgres>, Loaded, NotationRow), AppError> {
    let page: Option<Uuid> = sqlx::query_scalar("SELECT page_id FROM notations WHERE id = $1")
        .bind(notation)
        .fetch_optional(pool)
        .await?;
    let page = page.ok_or_else(notation_not_found)?;
    let (mut tx, loaded, _page) =
        pages::begin_write(pool, user, PageId::from_uuid(page), Wanted::Live)
            .await
            .map_err(|error| match &error {
                AppError::Api(api) if api.code == ErrorCode::NotFound => notation_not_found(),
                _ => error,
            })?;
    let locked: Option<Uuid> =
        sqlx::query_scalar("SELECT id FROM notations WHERE id = $1 FOR UPDATE")
            .bind(notation)
            .fetch_optional(&mut *tx)
            .await?;
    locked.ok_or_else(notation_not_found)?;
    let row = find_row(&mut tx, notation)
        .await?
        .ok_or_else(notation_not_found)?;
    Ok((tx, loaded, row))
}

fn involves(row: &NotationRow, user: UserId) -> bool {
    row.author_id == user.as_uuid() || row.assignee_id == user.as_uuid()
}

fn is_admin(loaded: &Loaded) -> bool {
    matches!(
        loaded.facts.workspace_role,
        Some(WorkspaceRole::Owner | WorkspaceRole::Admin)
    )
}

/// The assignee must be able to read the page: an active member who may read its conversation.
async fn ensure_assignee_can_read(
    conn: &mut PgConnection,
    assignee: UserId,
    conversation: ConversationId,
) -> Result<(), AppError> {
    let readable = conversations::load(conn, assignee, conversation)
        .await?
        .is_some_and(|loaded| loaded.can_read());
    if readable {
        Ok(())
    } else {
        Err(AppError::invalid(
            "the assignee cannot read this page's conversation",
        ))
    }
}

// --- Creating --------------------------------------------------------------------------------

/// `POST /v1/pages/{id}/notations`.
pub async fn create(
    pool: &PgPool,
    user: UserId,
    page: PageId,
    request: CreateNotationRequest,
) -> Result<Notation, AppError> {
    validate_notation_body(&request.body).map_err(invalid)?;
    let assignee = request.assignee_id.unwrap_or(user);
    let fp = command_fingerprint(
        "create_notation",
        &[
            page.as_uuid().as_bytes(),
            request.notation_id.as_uuid().as_bytes(),
            request.quote.as_bytes(),
            &request.from.to_be_bytes(),
            &request.to.to_be_bytes(),
            request.body.as_bytes(),
            assignee.as_uuid().as_bytes(),
        ],
    );
    let (mut tx, loaded, row) = pages::begin_write(pool, user, page, Wanted::Live).await?;
    if claim(
        &mut tx,
        user,
        request.operation_id,
        ReceiptKind::CreateNotation,
        &fp,
        request.notation_id.as_uuid(),
    )
    .await?
    {
        return repeat(&mut tx, user, request.notation_id.as_uuid()).await;
    }
    if row.kind != "page" {
        return Err(AppError::invalid(
            "only a page takes notations, not a board",
        ));
    }
    let body_chars: i64 =
        sqlx::query_scalar("SELECT char_length(body)::bigint FROM pages WHERE id = $1")
            .bind(page.as_uuid())
            .fetch_one(&mut *tx)
            .await?;
    validate_anchor(
        &request.quote,
        request.from,
        request.to,
        usize::try_from(body_chars).unwrap_or(0),
    )
    .map_err(invalid)?;
    let taken: Option<bool> = sqlx::query_scalar("SELECT true FROM notations WHERE id = $1")
        .bind(request.notation_id.as_uuid())
        .fetch_optional(&mut *tx)
        .await?;
    if taken.is_some() {
        return Err(AppError::conflict(
            "that notation id is already used for something else",
        ));
    }
    ensure_assignee_can_read(&mut tx, assignee, loaded.id).await?;
    let now: OffsetDateTime = sqlx::query_scalar("SELECT clock_timestamp()")
        .fetch_one(&mut *tx)
        .await?;
    sqlx::query(
        "INSERT INTO notations (id, page_id, author_id, assignee_id, quote, from_pos, to_pos, body, \
                                created_at, updated_at) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $9)",
    )
    .bind(request.notation_id.as_uuid())
    .bind(page.as_uuid())
    .bind(user.as_uuid())
    .bind(assignee.as_uuid())
    .bind(&request.quote)
    .bind(i32::try_from(request.from).unwrap_or(i32::MAX))
    .bind(i32::try_from(request.to).unwrap_or(i32::MAX))
    .bind(&request.body)
    .bind(now)
    .execute(&mut *tx)
    .await?;
    announce_changed(&mut tx, request.notation_id.as_uuid(), request.operation_id).await?;
    let notation = fetch(&mut tx, request.notation_id.as_uuid()).await?;
    tx.commit().await?;
    Ok(notation)
}

/// A repeated command's result: the notation as it is now, if the caller can still read it.
async fn repeat(
    tx: &mut Transaction<'static, Postgres>,
    user: UserId,
    notation: Uuid,
) -> Result<Notation, AppError> {
    let row = find_row(tx, notation)
        .await?
        .ok_or_else(notation_not_found)?;
    pages::load_readable(tx, user, PageId::from_uuid(row.page_id))
        .await
        .map_err(|_| notation_not_found())?;
    fetch(tx, notation).await
}

// --- Changing --------------------------------------------------------------------------------

/// `PATCH /v1/notations/{id}`: the author or the assignee changes the body or the assignee.
pub async fn update(
    pool: &PgPool,
    user: UserId,
    id: NotationId,
    request: UpdateNotationRequest,
) -> Result<Notation, AppError> {
    if let Some(body) = &request.body {
        validate_notation_body(body).map_err(invalid)?;
    }
    let fp = command_fingerprint(
        "update_notation",
        &[
            id.as_uuid().as_bytes(),
            request.body.as_deref().unwrap_or("\u{0}").as_bytes(),
            request
                .assignee_id
                .map(|a| a.as_uuid().into_bytes())
                .unwrap_or_default()
                .as_slice(),
        ],
    );
    let (mut tx, _loaded, row) = begin_change(pool, user, id.as_uuid()).await?;
    if claim(
        &mut tx,
        user,
        request.operation_id,
        ReceiptKind::ChangeNotation,
        &fp,
        id.as_uuid(),
    )
    .await?
    {
        return repeat(&mut tx, user, id.as_uuid()).await;
    }
    if !involves(&row, user) {
        return Err(AppError::forbidden(
            "only the author or the assignee can change this notation",
        ));
    }
    let body = request.body.clone().unwrap_or_else(|| row.body.clone());
    let assignee = request
        .assignee_id
        .map_or(row.assignee_id, |assignee| assignee.as_uuid());
    if body == row.body && assignee == row.assignee_id {
        let notation = fetch(&mut tx, id.as_uuid()).await?;
        tx.commit().await?;
        return Ok(notation);
    }
    if assignee != row.assignee_id {
        ensure_assignee_can_read(
            &mut tx,
            UserId::from_uuid(assignee),
            ConversationId::from_uuid(row.conversation_id),
        )
        .await?;
    }
    sqlx::query(
        "UPDATE notations SET body = $2, assignee_id = $3, updated_at = clock_timestamp() \
         WHERE id = $1",
    )
    .bind(id.as_uuid())
    .bind(&body)
    .bind(assignee)
    .execute(&mut *tx)
    .await?;
    announce_changed(&mut tx, id.as_uuid(), request.operation_id).await?;
    let notation = fetch(&mut tx, id.as_uuid()).await?;
    tx.commit().await?;
    Ok(notation)
}

/// `PUT /v1/notations/{id}/completed`: the author or the assignee completes or reopens.
pub async fn set_completed(
    pool: &PgPool,
    user: UserId,
    id: NotationId,
    request: SetNotationCompletedRequest,
) -> Result<Notation, AppError> {
    let fp = command_fingerprint(
        "complete_notation",
        &[id.as_uuid().as_bytes(), &[u8::from(request.completed)]],
    );
    let (mut tx, _loaded, row) = begin_change(pool, user, id.as_uuid()).await?;
    if claim(
        &mut tx,
        user,
        request.operation_id,
        ReceiptKind::ChangeNotation,
        &fp,
        id.as_uuid(),
    )
    .await?
    {
        return repeat(&mut tx, user, id.as_uuid()).await;
    }
    if !involves(&row, user) {
        return Err(AppError::forbidden(
            "only the author or the assignee can complete this notation",
        ));
    }
    if row.completed_at.is_some() == request.completed {
        let notation = fetch(&mut tx, id.as_uuid()).await?;
        tx.commit().await?;
        return Ok(notation);
    }
    sqlx::query(
        "UPDATE notations SET \
             completed_at = CASE WHEN $2 THEN clock_timestamp() END, \
             resolved_by = CASE WHEN $2 THEN $3::uuid END, \
             updated_at = clock_timestamp() \
         WHERE id = $1",
    )
    .bind(id.as_uuid())
    .bind(request.completed)
    .bind(user.as_uuid())
    .execute(&mut *tx)
    .await?;
    announce_changed(&mut tx, id.as_uuid(), request.operation_id).await?;
    let notation = fetch(&mut tx, id.as_uuid()).await?;
    tx.commit().await?;
    Ok(notation)
}

/// `DELETE /v1/notations/{id}`: the author, or an admin or owner, removes it with its replies.
pub async fn delete(
    pool: &PgPool,
    user: UserId,
    id: NotationId,
    operation: OperationId,
) -> Result<(), AppError> {
    let fp = command_fingerprint("delete_notation", &[id.as_uuid().as_bytes()]);
    let (mut tx, loaded, row) = match begin_change(pool, user, id.as_uuid()).await {
        Ok(started) => started,
        Err(error) => {
            return if removal_already_done(pool, user, operation, &fp, id.as_uuid()).await? {
                Ok(())
            } else {
                Err(error)
            };
        }
    };
    if claim(
        &mut tx,
        user,
        operation,
        ReceiptKind::ChangeNotation,
        &fp,
        id.as_uuid(),
    )
    .await?
    {
        return Ok(());
    }
    if row.author_id != user.as_uuid() && !is_admin(&loaded) {
        return Err(AppError::forbidden(
            "only the author can delete this notation",
        ));
    }
    sqlx::query("DELETE FROM notations WHERE id = $1")
        .bind(id.as_uuid())
        .execute(&mut *tx)
        .await?;
    let open_notations = open_count(&mut tx, row.page_id).await?;
    let conversation = ConversationId::from_uuid(row.conversation_id);
    events::append_all(
        &mut tx,
        WorkspaceId::from_uuid(row.workspace_id),
        vec![PendingEvent {
            kind: "notation_removed",
            conversation_id: Some(conversation),
            operation_id: Some(operation),
            audience: Audience::Members,
            event: Event::NotationRemoved {
                conversation_id: conversation,
                page_id: PageId::from_uuid(row.page_id),
                notation_id: id,
                open_notations,
            },
        }],
    )
    .await?;
    tx.commit().await?;
    Ok(())
}

// --- Replies ---------------------------------------------------------------------------------

/// `POST /v1/notations/{id}/replies`: while the notation is open.
pub async fn reply(
    pool: &PgPool,
    user: UserId,
    id: NotationId,
    request: CreateNotationReplyRequest,
) -> Result<Notation, AppError> {
    if request.body.trim().is_empty() || request.body.len() > NOTATION_BODY_MAX_BYTES {
        return Err(AppError::invalid(format!(
            "a reply is 1 to {NOTATION_BODY_MAX_BYTES} bytes"
        )));
    }
    let fp = command_fingerprint(
        "reply_notation",
        &[
            id.as_uuid().as_bytes(),
            request.reply_id.as_uuid().as_bytes(),
            request.body.as_bytes(),
        ],
    );
    let (mut tx, _loaded, row) = begin_change(pool, user, id.as_uuid()).await?;
    if claim(
        &mut tx,
        user,
        request.operation_id,
        ReceiptKind::ChangeNotation,
        &fp,
        id.as_uuid(),
    )
    .await?
    {
        return repeat(&mut tx, user, id.as_uuid()).await;
    }
    if row.completed_at.is_some() {
        return Err(AppError::forbidden(
            "this notation is completed; reopen it to reply",
        ));
    }
    let taken: Option<bool> = sqlx::query_scalar("SELECT true FROM notation_replies WHERE id = $1")
        .bind(request.reply_id.as_uuid())
        .fetch_optional(&mut *tx)
        .await?;
    if taken.is_some() {
        return Err(AppError::conflict(
            "that reply id is already used for something else",
        ));
    }
    sqlx::query(
        "INSERT INTO notation_replies (id, notation_id, author_id, body, created_at) \
         VALUES ($1, $2, $3, $4, clock_timestamp())",
    )
    .bind(request.reply_id.as_uuid())
    .bind(id.as_uuid())
    .bind(user.as_uuid())
    .bind(&request.body)
    .execute(&mut *tx)
    .await?;
    sqlx::query("UPDATE notations SET updated_at = clock_timestamp() WHERE id = $1")
        .bind(id.as_uuid())
        .execute(&mut *tx)
        .await?;
    announce_changed(&mut tx, id.as_uuid(), request.operation_id).await?;
    let notation = fetch(&mut tx, id.as_uuid()).await?;
    tx.commit().await?;
    Ok(notation)
}

/// `DELETE /v1/notation-replies/{id}`: the reply's author, or an admin or owner.
pub async fn delete_reply(
    pool: &PgPool,
    user: UserId,
    reply: NotationReplyId,
    operation: OperationId,
) -> Result<Notation, AppError> {
    let fp = command_fingerprint("delete_reply", &[reply.as_uuid().as_bytes()]);
    let found: Option<(Uuid, Uuid)> =
        sqlx::query_as("SELECT notation_id, author_id FROM notation_replies WHERE id = $1")
            .bind(reply.as_uuid())
            .fetch_optional(pool)
            .await?;
    let Some((notation, _author)) = found else {
        // Gone already: a repeat of this very command, if it was this caller's.
        let mut conn = pool.acquire().await?;
        if let Some(receipt) = receipts::find(&mut conn, user, operation).await?
            && receipt
                .ensure_same(ReceiptKind::ChangeNotation, &fp)
                .is_ok()
            && let Some(target) = receipt.target_id
            && find_row(&mut conn, target).await?.is_some()
        {
            return fetch(&mut conn, target).await;
        }
        return Err(AppError::not_found("reply not found"));
    };
    let (mut tx, loaded, _row) = begin_change(pool, user, notation).await?;
    if claim(
        &mut tx,
        user,
        operation,
        ReceiptKind::ChangeNotation,
        &fp,
        notation,
    )
    .await?
    {
        return repeat(&mut tx, user, notation).await;
    }
    let author: Option<Uuid> =
        sqlx::query_scalar("SELECT author_id FROM notation_replies WHERE id = $1 FOR UPDATE")
            .bind(reply.as_uuid())
            .fetch_optional(&mut *tx)
            .await?;
    let author = author.ok_or_else(|| AppError::not_found("reply not found"))?;
    if author != user.as_uuid() && !is_admin(&loaded) {
        return Err(AppError::forbidden("only the author can delete this reply"));
    }
    sqlx::query("DELETE FROM notation_replies WHERE id = $1")
        .bind(reply.as_uuid())
        .execute(&mut *tx)
        .await?;
    sqlx::query("UPDATE notations SET updated_at = clock_timestamp() WHERE id = $1")
        .bind(notation)
        .execute(&mut *tx)
        .await?;
    announce_changed(&mut tx, notation, operation).await?;
    let result = fetch(&mut tx, notation).await?;
    tx.commit().await?;
    Ok(result)
}

// --- Reading ---------------------------------------------------------------------------------

/// `GET /v1/pages/{id}/notations`, oldest first.
pub async fn list_page(
    pool: &PgPool,
    user: UserId,
    page: PageId,
) -> Result<NotationListResponse, AppError> {
    let mut conn = pool.acquire().await?;
    let (row, _loaded) = pages::load_readable(&mut conn, user, page).await?;
    let rows: Vec<NotationRow> = sqlx::query_as(notation_select!(
        "WHERE n.page_id = $1 ORDER BY n.created_at, n.id"
    ))
    .bind(row.id)
    .fetch_all(&mut *conn)
    .await?;
    Ok(NotationListResponse {
        notations: with_replies(&mut conn, rows).await?,
    })
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

/// `GET /v1/workspaces/{id}/notations`: assigned to the caller, or written by them, open or
/// completed, newest first, on live pages in conversations they can read.
pub async fn list_workspace(
    pool: &PgPool,
    user: UserId,
    workspace: WorkspaceId,
    params: NotationListParams,
) -> Result<NotationListResponse, AppError> {
    let mut conn = pool.acquire().await?;
    crate::access::require_member(&mut conn, workspace, user).await?;
    let q = params
        .q
        .as_deref()
        .map(str::trim)
        .filter(|q| !q.is_empty())
        .map(|q| format!("%{}%", escape_like(q)));
    let rows: Vec<NotationRow> = sqlx::query_as(notation_select!(
        "JOIN conversations c ON c.id = p.conversation_id \
         WHERE p.workspace_id = $1 AND p.trashed_at IS NULL \
           AND (CASE WHEN $2 THEN n.author_id ELSE n.assignee_id END) = $3 \
           AND (n.completed_at IS NOT NULL) = $4 \
           AND (c.visibility = 'public' OR EXISTS ( \
                SELECT 1 FROM conversation_members cm \
                WHERE cm.conversation_id = c.id AND cm.user_id = $3)) \
           AND ($5::text IS NULL OR n.body ILIKE $5 OR n.quote ILIKE $5 OR p.title ILIKE $5) \
         ORDER BY n.created_at DESC, n.id DESC LIMIT $6"
    ))
    .bind(workspace.as_uuid())
    .bind(params.scope == NotationScope::Created)
    .bind(user.as_uuid())
    .bind(params.completed)
    .bind(q.as_deref())
    .bind(LIST_LIMIT)
    .fetch_all(&mut *conn)
    .await?;
    Ok(NotationListResponse {
        notations: with_replies(&mut conn, rows).await?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn like_patterns_escape_their_wildcards() {
        assert_eq!(escape_like("50%_off\\"), "50\\%\\_off\\\\");
        assert_eq!(escape_like("plain"), "plain");
    }
}
