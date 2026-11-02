//! The routes a personal access token can reach (llm-docs/DECISIONS.md, D31), and the scope each
//! one needs. **Default deny**: a route that is not in [`TOKEN_ROUTES`] answers a token with
//! `forbidden`, however many scopes it holds. The same table feeds the OpenAPI description
//! ([`crate::openapi`]), and `tests/openapi.rs` walks every route the server registers and fails
//! if a route is not one of three things: in this table, on the short list of routes that take no
//! credentials at all ([`PUBLIC_ROUTES`]), or a route that refuses a token with `forbidden`.

use axum::http::Method;
use lazy_protocol::TokenScope;
use lazy_protocol::api::paths;
use lazy_protocol::docs::doc_paths;

/// One route a token can reach.
pub struct TokenRoute {
    pub method: &'static str,
    /// The route template as registered, with `{placeholders}`.
    pub path: &'static str,
    pub scope: TokenScope,
    /// A stable name for the operation (OpenAPI `operationId`; also what a client generator names
    /// the call).
    pub operation_id: &'static str,
    pub summary: &'static str,
    /// Name of the request body schema in `components.schemas`, if the route takes a JSON body.
    pub request: Option<&'static str>,
    /// Name of the success response schema, if the body is JSON. `None` with `status` 204 is an
    /// empty body.
    pub response: Option<&'static str>,
    /// The success status.
    pub status: u16,
    /// Name of the query parameters schema, if the route takes any.
    pub query: Option<&'static str>,
}

const fn route(
    method: &'static str,
    path: &'static str,
    scope: TokenScope,
    operation_id: &'static str,
    summary: &'static str,
    request: Option<&'static str>,
    response: Option<&'static str>,
) -> TokenRoute {
    TokenRoute {
        method,
        path,
        scope,
        operation_id,
        summary,
        request,
        response,
        status: 200,
        query: None,
    }
}

impl TokenRoute {
    const fn query(mut self, schema: &'static str) -> Self {
        self.query = Some(schema);
        self
    }

    const fn no_content(mut self) -> Self {
        self.status = 204;
        self
    }
}

use TokenScope::{
    ConversationsRead, McpUse, NotationsRead, NotationsWrite, PagesRead, PagesWrite, ProfileRead,
};

/// Every route a token can reach, in the order the description lists them.
pub const TOKEN_ROUTES: &[TokenRoute] = &[
    route(
        "GET",
        paths::ME_ROUTE,
        ProfileRead,
        "getMe",
        "The token owner's profile",
        None,
        Some("ProfileResponse"),
    ),
    route(
        "GET",
        paths::OPEN_CONVERSATION_ROUTE,
        ConversationsRead,
        "listConversations",
        "Conversations of a workspace that the token owner is a member of",
        None,
        Some("ConversationListResponse"),
    ),
    route(
        "GET",
        paths::BROWSE_CHANNELS_ROUTE,
        ConversationsRead,
        "browseChannels",
        "Public channels of a workspace, joined or not",
        None,
        Some("BrowseChannelsResponse"),
    ),
    route(
        "GET",
        doc_paths::CONVERSATION_PAGES_ROUTE,
        PagesRead,
        "listConversationPages",
        "List the pages and boards of a conversation",
        None,
        Some("PageListResponse"),
    )
    .query("PageListParams"),
    route(
        "POST",
        doc_paths::CONVERSATION_PAGES_ROUTE,
        PagesWrite,
        "createPage",
        "Create a page or board in a conversation",
        Some("CreatePageRequest"),
        Some("PageResponse"),
    ),
    route(
        "GET",
        doc_paths::WORKSPACE_PAGES_ROUTE,
        PagesRead,
        "listWorkspacePages",
        "List and search the pages the token owner can read in a workspace",
        None,
        Some("PageListResponse"),
    )
    .query("PageListParams"),
    route(
        "GET",
        doc_paths::PAGE_ROUTE,
        PagesRead,
        "getPage",
        "Read a page with its content and files",
        None,
        Some("PageResponse"),
    ),
    route(
        "PATCH",
        doc_paths::PAGE_ROUTE,
        PagesWrite,
        "updatePage",
        "Edit or move a page; refused with stale_revision if base_revision is not current",
        Some("UpdatePageRequest"),
        Some("PageResponse"),
    ),
    route(
        "DELETE",
        doc_paths::PAGE_ROUTE,
        PagesWrite,
        "trashPage",
        "Move a page to the trash",
        Some("PageActionRequest"),
        Some("PageResponse"),
    ),
    route(
        "POST",
        doc_paths::PAGE_RESTORE_ROUTE,
        PagesWrite,
        "restorePage",
        "Take a page out of the trash",
        Some("PageActionRequest"),
        Some("PageResponse"),
    ),
    route(
        "POST",
        doc_paths::PAGE_PURGE_ROUTE,
        PagesWrite,
        "purgePage",
        "Remove a trashed page for good",
        Some("PageActionRequest"),
        None,
    )
    .no_content(),
    route(
        "GET",
        doc_paths::PAGE_VERSIONS_ROUTE,
        PagesRead,
        "listPageVersions",
        "List a page's versions, newest first",
        None,
        Some("PageVersionsResponse"),
    ),
    route(
        "GET",
        doc_paths::PAGE_VERSION_ROUTE,
        PagesRead,
        "getPageVersion",
        "Read one version of a page",
        None,
        Some("PageVersionResponse"),
    ),
    route(
        "POST",
        doc_paths::PAGE_VERSION_RESTORE_ROUTE,
        PagesWrite,
        "restorePageVersion",
        "Make a version the page's current content",
        Some("RestoreVersionRequest"),
        Some("PageResponse"),
    ),
    route(
        "PUT",
        doc_paths::PAGE_FILE_ROUTE,
        PagesWrite,
        "uploadPageFile",
        "Add a file to a page; the body is the file's bytes",
        None,
        Some("AttachmentResponse"),
    ),
    route(
        "GET",
        paths::ATTACHMENT_ROUTE,
        PagesRead,
        "downloadPageFile",
        "Download a page's file (a file of a message is not available to a token)",
        None,
        None,
    ),
    route(
        "GET",
        paths::ATTACHMENT_THUMBNAIL_ROUTE,
        PagesRead,
        "downloadPageFileThumbnail",
        "Download the thumbnail of an image on a page",
        None,
        None,
    ),
    route(
        "GET",
        doc_paths::PAGE_NOTATIONS_ROUTE,
        NotationsRead,
        "listPageNotations",
        "List a page's notations",
        None,
        Some("NotationListResponse"),
    ),
    route(
        "POST",
        doc_paths::PAGE_NOTATIONS_ROUTE,
        NotationsWrite,
        "createNotation",
        "Anchor a note to text on a page",
        Some("CreateNotationRequest"),
        Some("NotationResponse"),
    ),
    route(
        "GET",
        doc_paths::WORKSPACE_NOTATIONS_ROUTE,
        NotationsRead,
        "listWorkspaceNotations",
        "Notations assigned to, or written by, the token owner",
        None,
        Some("NotationListResponse"),
    )
    .query("NotationListParams"),
    route(
        "PATCH",
        doc_paths::NOTATION_ROUTE,
        NotationsWrite,
        "updateNotation",
        "Change a notation's body or assignee",
        Some("UpdateNotationRequest"),
        Some("NotationResponse"),
    ),
    route(
        "DELETE",
        doc_paths::NOTATION_ROUTE,
        NotationsWrite,
        "deleteNotation",
        "Delete a notation",
        Some("NotationActionRequest"),
        None,
    )
    .no_content(),
    route(
        "PUT",
        doc_paths::NOTATION_COMPLETED_ROUTE,
        NotationsWrite,
        "setNotationCompleted",
        "Complete a notation or reopen it",
        Some("SetNotationCompletedRequest"),
        Some("NotationResponse"),
    ),
    route(
        "POST",
        doc_paths::NOTATION_REPLIES_ROUTE,
        NotationsWrite,
        "createNotationReply",
        "Reply under an open notation",
        Some("CreateNotationReplyRequest"),
        Some("NotationResponse"),
    ),
    route(
        "DELETE",
        doc_paths::NOTATION_REPLY_ROUTE,
        NotationsWrite,
        "deleteNotationReply",
        "Delete a reply",
        Some("NotationActionRequest"),
        Some("NotationResponse"),
    ),
    route(
        "POST",
        doc_paths::MCP_ROUTE,
        McpUse,
        "mcp",
        "Model Context Protocol endpoint (JSON-RPC 2.0 over Streamable HTTP)",
        None,
        None,
    ),
];

/// Routes that take no credentials at all. A token is not needed and not looked at.
pub const PUBLIC_ROUTES: &[(&str, &str)] = &[
    ("GET", paths::HEALTH),
    ("GET", paths::SERVER_INFO),
    ("POST", paths::PREVIEW_INVITATION),
    ("POST", paths::REGISTER),
    ("POST", paths::LOGIN),
    ("POST", paths::HOOK_ROUTE),
    ("GET", doc_paths::OPENAPI_ROUTE),
];

/// The scope a token needs for a request, or `None` if tokens cannot reach it.
pub fn required_scope(method: &Method, matched_path: &str) -> Option<TokenScope> {
    TOKEN_ROUTES
        .iter()
        .find(|route| route.method == method.as_str() && route.path == matched_path)
        .map(|route| route.scope)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn no_route_is_listed_twice_and_every_operation_has_its_own_id() {
        let mut routes = HashSet::new();
        let mut operations = HashSet::new();
        for route in TOKEN_ROUTES {
            assert!(
                routes.insert((route.method, route.path)),
                "{} {} twice",
                route.method,
                route.path
            );
            assert!(
                operations.insert(route.operation_id),
                "{} twice",
                route.operation_id
            );
        }
    }

    #[test]
    fn a_route_not_in_the_table_needs_no_scope_because_a_token_cannot_reach_it() {
        assert_eq!(required_scope(&Method::GET, paths::BOOTSTRAP), None);
        assert_eq!(required_scope(&Method::POST, paths::LOGOUT), None);
        assert_eq!(required_scope(&Method::POST, doc_paths::TOKENS_ROUTE), None);
        assert_eq!(
            required_scope(&Method::GET, doc_paths::PAGE_ROUTE),
            Some(TokenScope::PagesRead)
        );
        assert_eq!(
            required_scope(&Method::DELETE, doc_paths::PAGE_ROUTE),
            Some(TokenScope::PagesWrite)
        );
        // The method counts: a table entry for GET says nothing about PATCH on /v1/me.
        assert_eq!(required_scope(&Method::PATCH, paths::ME_ROUTE), None);
    }
}
