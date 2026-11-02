//! Pages, boards, versions, trash, notations, and personal access tokens
//! (llm-docs/DECISIONS.md, D30 and D31; llm-docs/DOCS.md).
//!
//! A page or board belongs to one conversation. Everything the server tells a client about one is
//! a [`PageSummary`] (small, no content, safe to put in an event) or a [`PageDetail`] (the summary
//! with the content and files, fetched with `GET /v1/pages/{id}`). Events carry only summaries, so
//! a client that sees a revision it does not hold fetches the page.

use lazy_domain::page::{PageContent, PageKind, VersionReason};
use lazy_domain::{
    AccessTokenId, ConversationId, NotationId, NotationReplyId, OperationId, PageId, Timestamp,
    UserId, WorkspaceId,
};
use serde::{Deserialize, Serialize};

use crate::api::Attachment;

/// Route templates and builders. Kept apart from [`crate::api::paths`] so the docs feature can be
/// read as one piece.
pub mod doc_paths {
    use lazy_domain::{ConversationId, NotationId, NotationReplyId, PageId, WorkspaceId};

    /// `POST` creates a page or board in a conversation, `GET` lists its pages.
    pub const CONVERSATION_PAGES_ROUTE: &str = "/v1/conversations/{conversation_id}/pages";
    /// `GET` lists and searches the pages the caller can read in a workspace.
    pub const WORKSPACE_PAGES_ROUTE: &str = "/v1/workspaces/{workspace_id}/pages";
    /// `GET` reads a page, `PATCH` edits or moves it, `DELETE` moves it to the trash.
    pub const PAGE_ROUTE: &str = "/v1/pages/{page_id}";
    /// `POST` takes a page out of the trash.
    pub const PAGE_RESTORE_ROUTE: &str = "/v1/pages/{page_id}/restore";
    /// `POST` removes a trashed page for good.
    pub const PAGE_PURGE_ROUTE: &str = "/v1/pages/{page_id}/purge";
    /// `GET` lists a page's versions, newest first.
    pub const PAGE_VERSIONS_ROUTE: &str = "/v1/pages/{page_id}/versions";
    /// `GET` reads one version.
    pub const PAGE_VERSION_ROUTE: &str = "/v1/pages/{page_id}/versions/{version}";
    /// `POST` restores a version as the page's current content.
    pub const PAGE_VERSION_RESTORE_ROUTE: &str = "/v1/pages/{page_id}/versions/{version}/restore";
    /// `PUT` adds a file to a page. The body is the file's bytes; headers as for message files.
    pub const PAGE_FILE_ROUTE: &str = "/v1/pages/{page_id}/files/{attachment_id}";
    /// `GET` lists a page's notations, `POST` makes one.
    pub const PAGE_NOTATIONS_ROUTE: &str = "/v1/pages/{page_id}/notations";
    /// `GET` lists the notations assigned to, or written by, the caller across a workspace.
    pub const WORKSPACE_NOTATIONS_ROUTE: &str = "/v1/workspaces/{workspace_id}/notations";
    /// `PATCH` edits a notation, `DELETE` removes it.
    pub const NOTATION_ROUTE: &str = "/v1/notations/{notation_id}";
    /// `PUT` completes a notation or reopens it.
    pub const NOTATION_COMPLETED_ROUTE: &str = "/v1/notations/{notation_id}/completed";
    /// `POST` replies under a notation.
    pub const NOTATION_REPLIES_ROUTE: &str = "/v1/notations/{notation_id}/replies";
    /// `DELETE` removes a reply.
    pub const NOTATION_REPLY_ROUTE: &str = "/v1/notation-replies/{reply_id}";
    /// `POST` makes a personal access token, `GET` lists the caller's.
    pub const TOKENS_ROUTE: &str = "/v1/tokens";
    /// `DELETE` revokes a token.
    pub const TOKEN_ROUTE: &str = "/v1/tokens/{token_id}";
    /// `GET` the OpenAPI 3.1 description of the routes a token can reach. Public.
    pub const OPENAPI_ROUTE: &str = "/v1/openapi.json";
    /// `POST` speaks the Model Context Protocol (JSON-RPC 2.0) to a token holder.
    pub const MCP_ROUTE: &str = "/mcp";

    pub fn conversation_pages(conversation: ConversationId) -> String {
        CONVERSATION_PAGES_ROUTE.replace("{conversation_id}", &conversation.to_string())
    }
    pub fn workspace_pages(workspace: WorkspaceId) -> String {
        WORKSPACE_PAGES_ROUTE.replace("{workspace_id}", &workspace.to_string())
    }
    pub fn page(page: PageId) -> String {
        PAGE_ROUTE.replace("{page_id}", &page.to_string())
    }
    pub fn page_restore(page: PageId) -> String {
        PAGE_RESTORE_ROUTE.replace("{page_id}", &page.to_string())
    }
    pub fn page_purge(page: PageId) -> String {
        PAGE_PURGE_ROUTE.replace("{page_id}", &page.to_string())
    }
    pub fn page_versions(page: PageId) -> String {
        PAGE_VERSIONS_ROUTE.replace("{page_id}", &page.to_string())
    }
    pub fn page_version(page: PageId, version: u32) -> String {
        PAGE_VERSION_ROUTE
            .replace("{page_id}", &page.to_string())
            .replace("{version}", &version.to_string())
    }
    pub fn page_version_restore(page: PageId, version: u32) -> String {
        PAGE_VERSION_RESTORE_ROUTE
            .replace("{page_id}", &page.to_string())
            .replace("{version}", &version.to_string())
    }
    pub fn page_file(page: PageId, attachment: lazy_domain::AttachmentId) -> String {
        PAGE_FILE_ROUTE
            .replace("{page_id}", &page.to_string())
            .replace("{attachment_id}", &attachment.to_string())
    }
    pub fn page_notations(page: PageId) -> String {
        PAGE_NOTATIONS_ROUTE.replace("{page_id}", &page.to_string())
    }
    pub fn workspace_notations(workspace: WorkspaceId) -> String {
        WORKSPACE_NOTATIONS_ROUTE.replace("{workspace_id}", &workspace.to_string())
    }
    pub fn notation(notation: NotationId) -> String {
        NOTATION_ROUTE.replace("{notation_id}", &notation.to_string())
    }
    pub fn notation_completed(notation: NotationId) -> String {
        NOTATION_COMPLETED_ROUTE.replace("{notation_id}", &notation.to_string())
    }
    pub fn notation_replies(notation: NotationId) -> String {
        NOTATION_REPLIES_ROUTE.replace("{notation_id}", &notation.to_string())
    }
    pub fn notation_reply(reply: NotationReplyId) -> String {
        NOTATION_REPLY_ROUTE.replace("{reply_id}", &reply.to_string())
    }
    pub fn token(token: lazy_domain::AccessTokenId) -> String {
        TOKEN_ROUTE.replace("{token_id}", &token.to_string())
    }
}

/// Everything about a page except its content. Small enough for an event and a list row.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PageSummary {
    pub id: PageId,
    pub workspace_id: WorkspaceId,
    pub conversation_id: ConversationId,
    pub kind: PageKind,
    pub title: String,
    /// The first 160 characters of the text, on one line. Empty for a board with no text.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub excerpt: String,
    pub created_by: UserId,
    pub created_at: Timestamp,
    /// Who last changed the content or title, and when.
    pub updated_by: UserId,
    pub updated_at: Timestamp,
    /// Starts at 1 and rises by one with every change to the title, content, or conversation. A
    /// client keeps the highest it has seen and sends it back as `base_revision`.
    pub revision: u32,
    /// Set while the page is in the trash.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trashed_at: Option<Timestamp>,
    /// When a trashed page will be removed for good.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub purge_at: Option<Timestamp>,
    /// Notations on the page that are not completed.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub open_notations: u32,
    /// Files added to the page.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub file_count: u32,
}

fn is_zero(n: &u32) -> bool {
    *n == 0
}

/// A page with its content and files.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PageDetail {
    #[serde(flatten)]
    pub summary: PageSummary,
    pub content: PageContent,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub files: Vec<Attachment>,
}

/// Response to creating, editing, moving, trashing, or restoring a page.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PageResponse {
    pub page: PageDetail,
}

/// `POST /v1/conversations/{id}/pages`. The id is the client's, so a retry is the same creation:
/// the same id with the same request returns the page, and the same id with a different request
/// is `conflict`. Whoever can post in the conversation may create.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CreatePageRequest {
    pub operation_id: OperationId,
    pub page_id: PageId,
    pub kind: PageKind,
    /// Empty means "Untitled".
    #[serde(default)]
    pub title: String,
    /// Absent means empty content of `kind`; present, it must be of `kind`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content: Option<PageContent>,
}

/// `PATCH /v1/pages/{id}`. The edit commits only if `base_revision` is still current; otherwise
/// `stale_revision`, and the client fetches the page and lets the person reconcile. Repeating an
/// operation returns its original result. An edit that changes nothing commits nothing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UpdatePageRequest {
    pub operation_id: OperationId,
    pub base_revision: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// Replaces the content; it must be of the page's kind.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content: Option<PageContent>,
    /// Text added after the page's text, with a blank line between. Only for a page, and not with
    /// `content`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub append: Option<String>,
    /// Move the page to another conversation of the same workspace. The caller must be able to
    /// post in both.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub conversation_id: Option<ConversationId>,
}

/// Body of the trash, restore, and purge routes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PageActionRequest {
    pub operation_id: OperationId,
}

/// Whether a list shows live pages or the trash.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PageState {
    #[default]
    Live,
    Trashed,
}

/// How a list is ordered.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PageSort {
    /// Most recently changed first.
    #[default]
    Updated,
    /// By title, A to Z, ignoring case.
    Title,
}

/// Query of the page lists. Every field is optional.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct PageListParams {
    #[serde(default)]
    pub state: PageState,
    /// Words to find in titles and text.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub q: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<PageKind>,
    #[serde(default)]
    pub sort: PageSort,
    /// At most 100; the default is 50.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub offset: Option<u32>,
}

/// A page of a list.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PageListResponse {
    pub pages: Vec<PageSummary>,
    /// Whether more follow at `offset + pages.len()`.
    pub has_more: bool,
}

/// One saved state of a page.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PageVersionSummary {
    pub page_id: PageId,
    /// 1 for the oldest the page has had; never reused.
    pub version: u32,
    pub title: String,
    pub reason: VersionReason,
    /// Who made the change that this version preceded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_by: Option<UserId>,
    pub created_at: Timestamp,
    /// The page's `revision` when this state was current.
    pub revision: u32,
}

/// A version with its content.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PageVersionDetail {
    #[serde(flatten)]
    pub summary: PageVersionSummary,
    pub content: PageContent,
}

/// Response of `GET /v1/pages/{id}/versions`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PageVersionsResponse {
    pub versions: Vec<PageVersionSummary>,
}

/// Response of `GET /v1/pages/{id}/versions/{version}`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PageVersionResponse {
    pub version: PageVersionDetail,
}

/// `POST /v1/pages/{id}/versions/{version}/restore`. The page's current state is first kept as a
/// `pre_restore` version, so a restore can be undone.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RestoreVersionRequest {
    pub operation_id: OperationId,
    pub base_revision: u32,
}

/// A reply under a notation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NotationReply {
    pub id: NotationReplyId,
    pub notation_id: NotationId,
    pub author_id: UserId,
    pub body: String,
    pub created_at: Timestamp,
}

/// A note anchored to text on a page, assigned to someone, with replies.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Notation {
    pub id: NotationId,
    pub page_id: PageId,
    pub conversation_id: ConversationId,
    pub author_id: UserId,
    pub assignee_id: UserId,
    /// The text the note was made on.
    pub quote: String,
    /// Where the quote was, in characters, when the note was made. A client finds the quote again
    /// in the current text with [`lazy_domain::page::relocate_anchor`].
    pub from: u32,
    pub to: u32,
    pub body: String,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completed_at: Option<Timestamp>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolved_by: Option<UserId>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub replies: Vec<NotationReply>,
}

/// `POST /v1/pages/{id}/notations`. Only pages take notations, not boards. The assignee defaults
/// to the author and must be able to read the page.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CreateNotationRequest {
    pub operation_id: OperationId,
    pub notation_id: NotationId,
    pub quote: String,
    pub from: u32,
    pub to: u32,
    pub body: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub assignee_id: Option<UserId>,
}

/// `PATCH /v1/notations/{id}`. The author or the assignee may change the body or the assignee.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UpdateNotationRequest {
    pub operation_id: OperationId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub assignee_id: Option<UserId>,
}

/// `PUT /v1/notations/{id}/completed`. The author or the assignee may complete or reopen.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SetNotationCompletedRequest {
    pub operation_id: OperationId,
    pub completed: bool,
}

/// `POST /v1/notations/{id}/replies`. Anyone who can post in the conversation may reply while the
/// notation is open.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CreateNotationReplyRequest {
    pub operation_id: OperationId,
    pub reply_id: NotationReplyId,
    pub body: String,
}

/// Body of the notation delete routes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NotationActionRequest {
    pub operation_id: OperationId,
}

/// Response to a notation change.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NotationResponse {
    pub notation: Notation,
}

/// Response of the notation lists.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NotationListResponse {
    pub notations: Vec<Notation>,
}

/// Whose notations the workspace list shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NotationScope {
    /// Assigned to the caller.
    #[default]
    Assigned,
    /// Written by the caller.
    Created,
}

/// Query of `GET /v1/workspaces/{id}/notations`.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct NotationListParams {
    #[serde(default)]
    pub scope: NotationScope,
    /// Show completed notations instead of open ones.
    #[serde(default)]
    pub completed: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub q: Option<String>,
}

/// What a personal access token may do. A token can never do more than its owner.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub enum TokenScope {
    #[serde(rename = "profile:read")]
    ProfileRead,
    #[serde(rename = "pages:read")]
    PagesRead,
    #[serde(rename = "pages:write")]
    PagesWrite,
    #[serde(rename = "notations:read")]
    NotationsRead,
    #[serde(rename = "notations:write")]
    NotationsWrite,
    #[serde(rename = "conversations:read")]
    ConversationsRead,
    /// Lets the token speak MCP at `/mcp`. Still limited by the other scopes it holds.
    #[serde(rename = "mcp:use")]
    McpUse,
}

impl TokenScope {
    pub const ALL: [TokenScope; 7] = [
        Self::ProfileRead,
        Self::PagesRead,
        Self::PagesWrite,
        Self::NotationsRead,
        Self::NotationsWrite,
        Self::ConversationsRead,
        Self::McpUse,
    ];

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ProfileRead => "profile:read",
            Self::PagesRead => "pages:read",
            Self::PagesWrite => "pages:write",
            Self::NotationsRead => "notations:read",
            Self::NotationsWrite => "notations:write",
            Self::ConversationsRead => "conversations:read",
            Self::McpUse => "mcp:use",
        }
    }

    pub fn parse(text: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|scope| scope.as_str() == text)
    }
}

/// Most tokens one person may hold.
pub const MAX_TOKENS_PER_USER: usize = 25;
/// Longest token name, in characters.
pub const TOKEN_NAME_MAX_CHARS: usize = 64;
/// Prefix of every token secret, so a leaked one is recognizable.
pub const TOKEN_PREFIX: &str = "lzp_";

/// A personal access token, without its secret.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccessToken {
    pub id: AccessTokenId,
    pub name: String,
    pub scopes: Vec<TokenScope>,
    pub created_at: Timestamp,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_used_at: Option<Timestamp>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<Timestamp>,
}

/// `POST /v1/tokens`. Needs the caller's password again; a token cannot make tokens.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CreateTokenRequest {
    pub password: crate::Password,
    pub name: String,
    pub scopes: Vec<TokenScope>,
    /// Days until it stops working; absent means it does not expire.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_in_days: Option<u32>,
}

/// The new token and its secret, which the server keeps only as a hash and never shows again.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CreateTokenResponse {
    pub token: AccessToken,
    pub secret: crate::SessionToken,
}

/// Response of `GET /v1/workspaces/{id}/conversations`: the conversations of the workspace that the
/// caller is a member of (for a token, the way to find the conversations that hold pages).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConversationListResponse {
    pub conversations: Vec<crate::api::Conversation>,
}

/// Response of `GET /v1/tokens`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TokenListResponse {
    pub tokens: Vec<AccessToken>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use lazy_domain::page::PageKind;

    fn summary() -> PageSummary {
        PageSummary {
            id: PageId::generate(),
            workspace_id: WorkspaceId::generate(),
            conversation_id: ConversationId::generate(),
            kind: PageKind::Page,
            title: "Plan".into(),
            excerpt: String::new(),
            created_by: UserId::generate(),
            created_at: Timestamp::now(),
            updated_by: UserId::generate(),
            updated_at: Timestamp::now(),
            revision: 1,
            trashed_at: None,
            purge_at: None,
            open_notations: 0,
            file_count: 0,
        }
    }

    #[test]
    fn a_summary_leaves_out_what_is_empty() {
        let json = serde_json::to_value(summary()).unwrap();
        for key in [
            "excerpt",
            "trashed_at",
            "purge_at",
            "open_notations",
            "file_count",
        ] {
            assert!(json.get(key).is_none(), "{key} should be left out");
        }
        let summary = PageSummary {
            open_notations: 2,
            ..summary()
        };
        let json = serde_json::to_string(&summary).unwrap();
        assert_eq!(serde_json::from_str::<PageSummary>(&json).unwrap(), summary);
    }

    #[test]
    fn a_detail_flattens_its_summary_beside_the_content() {
        let detail = PageDetail {
            summary: summary(),
            content: PageContent::Page { body: "hi".into() },
            files: Vec::new(),
        };
        let json = serde_json::to_value(&detail).unwrap();
        assert_eq!(json["title"], "Plan");
        assert_eq!(json["content"]["body"], "hi");
        let back: PageDetail = serde_json::from_value(json).unwrap();
        assert_eq!(back, detail);
    }

    #[test]
    fn routes_fill_their_placeholders() {
        let (page, conv) = (PageId::generate(), ConversationId::generate());
        assert_eq!(
            doc_paths::conversation_pages(conv),
            format!("/v1/conversations/{conv}/pages")
        );
        assert_eq!(doc_paths::page(page), format!("/v1/pages/{page}"));
        assert_eq!(
            doc_paths::page_version_restore(page, 3),
            format!("/v1/pages/{page}/versions/3/restore")
        );
        assert!(!doc_paths::notation(NotationId::generate()).contains('{'));
    }

    #[test]
    fn list_params_default_to_live_pages_by_recent_change() {
        let params: PageListParams = serde_json::from_str("{}").unwrap();
        assert_eq!(params, PageListParams::default());
        assert_eq!(params.state, PageState::Live);
        assert_eq!(params.sort, PageSort::Updated);
        let params: PageListParams =
            serde_json::from_str(r#"{"state":"trashed","kind":"board","sort":"title"}"#).unwrap();
        assert_eq!(params.state, PageState::Trashed);
        assert_eq!(params.kind, Some(PageKind::Board));
    }

    #[test]
    fn token_scopes_use_their_colon_names_on_the_wire() {
        for scope in TokenScope::ALL {
            let json = serde_json::to_string(&scope).unwrap();
            assert_eq!(json, format!("\"{}\"", scope.as_str()));
            assert_eq!(TokenScope::parse(scope.as_str()), Some(scope));
        }
        assert_eq!(TokenScope::parse("admin:write"), None);
    }

    #[test]
    fn a_token_secret_never_prints_in_debug() {
        let response = CreateTokenResponse {
            token: AccessToken {
                id: AccessTokenId::generate(),
                name: "ci".into(),
                scopes: vec![TokenScope::PagesRead],
                created_at: Timestamp::now(),
                last_used_at: None,
                expires_at: None,
            },
            secret: crate::SessionToken::new("lzp_supersecret"),
        };
        assert!(!format!("{response:?}").contains("supersecret"));
    }

    #[test]
    fn notations_round_trip() {
        let notation = Notation {
            id: NotationId::generate(),
            page_id: PageId::generate(),
            conversation_id: ConversationId::generate(),
            author_id: UserId::generate(),
            assignee_id: UserId::generate(),
            quote: "word".into(),
            from: 3,
            to: 7,
            body: "check this".into(),
            created_at: Timestamp::now(),
            updated_at: Timestamp::now(),
            completed_at: None,
            resolved_by: None,
            replies: Vec::new(),
        };
        let json = serde_json::to_string(&notation).unwrap();
        assert!(!json.contains("replies"));
        assert_eq!(serde_json::from_str::<Notation>(&json).unwrap(), notation);
    }
}
