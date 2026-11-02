//! The HTTP routes of the docs feature (llm-docs/DECISIONS.md, D30). Handlers only translate
//! between HTTP and [`crate::pages`] and [`crate::notations`].

use axum::Json;
use axum::Router;
use axum::extract::{DefaultBodyLimit, State};
use axum::http::{HeaderMap, StatusCode};
use axum::routing::{get, post, put};
use lazy_domain::{AttachmentId, ConversationId, NotationId, NotationReplyId, PageId, WorkspaceId};
use lazy_protocol::docs::doc_paths;
use lazy_protocol::{
    AttachmentResponse, CreateNotationReplyRequest, CreateNotationRequest, CreatePageRequest,
    NotationActionRequest, NotationListParams, NotationListResponse, NotationResponse,
    PageActionRequest, PageListParams, PageListResponse, PageResponse, PageVersionResponse,
    PageVersionsResponse, RestoreVersionRequest, SetNotationCompletedRequest,
    UpdateNotationRequest, UpdatePageRequest,
};

use crate::app::AppState;
use crate::error::AppError;
use crate::http::{ApiJson, ApiPath, ApiQuery, Auth};
use crate::{attachments, notations, pages};

/// Largest request body on the docs routes: a page of 1,000,000 bytes, which JSON can double
/// when it is full of characters that need escaping.
pub const DOCS_BODY_LIMIT: usize = 4 * 1024 * 1024;

/// Every docs route except the file upload (which has the longer file timeout; see
/// [`file_routes`]), with the larger body limit.
pub(crate) fn routes() -> Router<AppState> {
    Router::new()
        .route(
            doc_paths::CONVERSATION_PAGES_ROUTE,
            get(list_conversation_pages).post(create_page),
        )
        .route(doc_paths::WORKSPACE_PAGES_ROUTE, get(list_workspace_pages))
        .route(
            doc_paths::PAGE_ROUTE,
            get(get_page).patch(update_page).delete(trash_page),
        )
        .route(doc_paths::PAGE_RESTORE_ROUTE, post(restore_page))
        .route(doc_paths::PAGE_PURGE_ROUTE, post(purge_page))
        .route(doc_paths::PAGE_VERSIONS_ROUTE, get(list_versions))
        .route(doc_paths::PAGE_VERSION_ROUTE, get(get_version))
        .route(doc_paths::PAGE_VERSION_RESTORE_ROUTE, post(restore_version))
        .route(
            doc_paths::PAGE_NOTATIONS_ROUTE,
            get(list_page_notations).post(create_notation),
        )
        .route(
            doc_paths::WORKSPACE_NOTATIONS_ROUTE,
            get(list_workspace_notations),
        )
        .route(
            doc_paths::NOTATION_ROUTE,
            axum::routing::patch(update_notation).delete(delete_notation),
        )
        // Model Context Protocol (D31). It carries page text, so it has the docs body limit; it
        // authenticates itself (see `mcp`) and answers in JSON-RPC.
        .route(
            doc_paths::MCP_ROUTE,
            post(crate::mcp::post)
                .get(crate::mcp::not_allowed)
                .delete(crate::mcp::not_allowed),
        )
        .route(doc_paths::NOTATION_COMPLETED_ROUTE, put(set_completed))
        .route(doc_paths::NOTATION_REPLIES_ROUTE, post(create_reply))
        .route(
            doc_paths::NOTATION_REPLY_ROUTE,
            axum::routing::delete(delete_reply),
        )
        .layer(DefaultBodyLimit::max(DOCS_BODY_LIMIT))
}

/// The upload of a file to a page.
pub(crate) fn file_routes() -> Router<AppState> {
    Router::new().route(doc_paths::PAGE_FILE_ROUTE, put(upload_page_file))
}

async fn create_page(
    State(state): State<AppState>,
    Auth(principal): Auth,
    ApiPath(conversation): ApiPath<ConversationId>,
    ApiJson(request): ApiJson<CreatePageRequest>,
) -> Result<Json<PageResponse>, AppError> {
    let page = pages::create(&state.pool, principal.user_id, conversation, request).await?;
    Ok(Json(PageResponse { page }))
}

async fn list_conversation_pages(
    State(state): State<AppState>,
    Auth(principal): Auth,
    ApiPath(conversation): ApiPath<ConversationId>,
    ApiQuery(params): ApiQuery<PageListParams>,
) -> Result<Json<PageListResponse>, AppError> {
    pages::list_conversation(&state.pool, principal.user_id, conversation, params)
        .await
        .map(Json)
}

async fn list_workspace_pages(
    State(state): State<AppState>,
    Auth(principal): Auth,
    ApiPath(workspace): ApiPath<WorkspaceId>,
    ApiQuery(params): ApiQuery<PageListParams>,
) -> Result<Json<PageListResponse>, AppError> {
    pages::list_workspace(&state.pool, principal.user_id, workspace, params)
        .await
        .map(Json)
}

async fn get_page(
    State(state): State<AppState>,
    Auth(principal): Auth,
    ApiPath(id): ApiPath<PageId>,
) -> Result<Json<PageResponse>, AppError> {
    let page = pages::get(&state.pool, principal.user_id, id).await?;
    Ok(Json(PageResponse { page }))
}

async fn update_page(
    State(state): State<AppState>,
    Auth(principal): Auth,
    ApiPath(id): ApiPath<PageId>,
    ApiJson(request): ApiJson<UpdatePageRequest>,
) -> Result<Json<PageResponse>, AppError> {
    let page = pages::update(&state.pool, principal.user_id, id, request).await?;
    Ok(Json(PageResponse { page }))
}

async fn trash_page(
    State(state): State<AppState>,
    Auth(principal): Auth,
    ApiPath(id): ApiPath<PageId>,
    ApiJson(request): ApiJson<PageActionRequest>,
) -> Result<Json<PageResponse>, AppError> {
    let page = pages::trash(&state.pool, principal.user_id, id, request.operation_id).await?;
    Ok(Json(PageResponse { page }))
}

async fn restore_page(
    State(state): State<AppState>,
    Auth(principal): Auth,
    ApiPath(id): ApiPath<PageId>,
    ApiJson(request): ApiJson<PageActionRequest>,
) -> Result<Json<PageResponse>, AppError> {
    let page = pages::restore(&state.pool, principal.user_id, id, request.operation_id).await?;
    Ok(Json(PageResponse { page }))
}

async fn purge_page(
    State(state): State<AppState>,
    Auth(principal): Auth,
    ApiPath(id): ApiPath<PageId>,
    ApiJson(request): ApiJson<PageActionRequest>,
) -> Result<StatusCode, AppError> {
    pages::purge(&state.pool, principal.user_id, id, request.operation_id).await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn list_versions(
    State(state): State<AppState>,
    Auth(principal): Auth,
    ApiPath(id): ApiPath<PageId>,
) -> Result<Json<PageVersionsResponse>, AppError> {
    pages::versions(&state.pool, principal.user_id, id)
        .await
        .map(Json)
}

async fn get_version(
    State(state): State<AppState>,
    Auth(principal): Auth,
    ApiPath((id, version)): ApiPath<(PageId, u32)>,
) -> Result<Json<PageVersionResponse>, AppError> {
    let version = pages::get_version(&state.pool, principal.user_id, id, version).await?;
    Ok(Json(PageVersionResponse { version }))
}

async fn restore_version(
    State(state): State<AppState>,
    Auth(principal): Auth,
    ApiPath((id, version)): ApiPath<(PageId, u32)>,
    ApiJson(request): ApiJson<RestoreVersionRequest>,
) -> Result<Json<PageResponse>, AppError> {
    let page = pages::restore_version(&state.pool, principal.user_id, id, version, request).await?;
    Ok(Json(PageResponse { page }))
}

async fn upload_page_file(
    State(state): State<AppState>,
    Auth(principal): Auth,
    ApiPath((page, attachment)): ApiPath<(PageId, AttachmentId)>,
    headers: HeaderMap,
    body: axum::body::Body,
) -> Result<Json<AttachmentResponse>, AppError> {
    let attachment =
        attachments::upload_page_file(&state, principal.user_id, page, attachment, &headers, body)
            .await?;
    Ok(Json(AttachmentResponse { attachment }))
}

async fn list_page_notations(
    State(state): State<AppState>,
    Auth(principal): Auth,
    ApiPath(id): ApiPath<PageId>,
) -> Result<Json<NotationListResponse>, AppError> {
    notations::list_page(&state.pool, principal.user_id, id)
        .await
        .map(Json)
}

async fn list_workspace_notations(
    State(state): State<AppState>,
    Auth(principal): Auth,
    ApiPath(workspace): ApiPath<WorkspaceId>,
    ApiQuery(params): ApiQuery<NotationListParams>,
) -> Result<Json<NotationListResponse>, AppError> {
    notations::list_workspace(&state.pool, principal.user_id, workspace, params)
        .await
        .map(Json)
}

async fn create_notation(
    State(state): State<AppState>,
    Auth(principal): Auth,
    ApiPath(page): ApiPath<PageId>,
    ApiJson(request): ApiJson<CreateNotationRequest>,
) -> Result<Json<NotationResponse>, AppError> {
    let notation = notations::create(&state.pool, principal.user_id, page, request).await?;
    Ok(Json(NotationResponse { notation }))
}

async fn update_notation(
    State(state): State<AppState>,
    Auth(principal): Auth,
    ApiPath(id): ApiPath<NotationId>,
    ApiJson(request): ApiJson<UpdateNotationRequest>,
) -> Result<Json<NotationResponse>, AppError> {
    let notation = notations::update(&state.pool, principal.user_id, id, request).await?;
    Ok(Json(NotationResponse { notation }))
}

async fn set_completed(
    State(state): State<AppState>,
    Auth(principal): Auth,
    ApiPath(id): ApiPath<NotationId>,
    ApiJson(request): ApiJson<SetNotationCompletedRequest>,
) -> Result<Json<NotationResponse>, AppError> {
    let notation = notations::set_completed(&state.pool, principal.user_id, id, request).await?;
    Ok(Json(NotationResponse { notation }))
}

async fn delete_notation(
    State(state): State<AppState>,
    Auth(principal): Auth,
    ApiPath(id): ApiPath<NotationId>,
    ApiJson(request): ApiJson<NotationActionRequest>,
) -> Result<StatusCode, AppError> {
    notations::delete(&state.pool, principal.user_id, id, request.operation_id).await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn create_reply(
    State(state): State<AppState>,
    Auth(principal): Auth,
    ApiPath(id): ApiPath<NotationId>,
    ApiJson(request): ApiJson<CreateNotationReplyRequest>,
) -> Result<Json<NotationResponse>, AppError> {
    let notation = notations::reply(&state.pool, principal.user_id, id, request).await?;
    Ok(Json(NotationResponse { notation }))
}

async fn delete_reply(
    State(state): State<AppState>,
    Auth(principal): Auth,
    ApiPath(id): ApiPath<NotationReplyId>,
    ApiJson(request): ApiJson<NotationActionRequest>,
) -> Result<Json<NotationResponse>, AppError> {
    let notation =
        notations::delete_reply(&state.pool, principal.user_id, id, request.operation_id).await?;
    Ok(Json(NotationResponse { notation }))
}
