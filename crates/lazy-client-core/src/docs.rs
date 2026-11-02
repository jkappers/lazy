//! Docs (D30): the types a host sees. The state machine is in `core/docs.rs`.
//!
//! Pages and boards belong to a conversation. The core keeps their lists on demand (nothing is
//! cached on disk), one open page with a working copy the person edits, the page's versions and
//! notations, a search, and the person's notation inbox. Every change to a page is a request with
//! an operation id; a create or a save is journaled before it is sent.

use std::fmt;

use lazy_domain::page::PageContent;
use lazy_domain::{
    AttachmentId, ConversationId, NotationId, NotationReplyId, OperationId, PageId, PageKind,
    Timestamp, UserId, WorkspaceId,
};
use lazy_protocol::{
    Attachment, CreateNotationReplyRequest, CreateNotationRequest, CreatePageRequest, Notation,
    NotationActionRequest, NotationListParams, NotationListResponse, NotationResponse,
    NotationScope, PageActionRequest, PageDetail, PageListParams, PageListResponse, PageResponse,
    PageState, PageSummary, PageVersionDetail, PageVersionResponse, PageVersionSummary,
    PageVersionsResponse, RestoreVersionRequest, SetNotationCompletedRequest,
    UpdateNotationRequest, UpdatePageRequest,
};
use serde::{Deserialize, Serialize};

use crate::attachments::FileSource;
use crate::notify::LoadStatus;
use crate::redact::{Count, Named, Text};

/// Pages asked for at a time.
pub const DOCS_PAGE: u32 = 50;
/// How many times an unanswered mutation is sent again with the same operation id before the
/// person is asked to try again.
pub const MAX_DOC_RESENDS: u32 = 3;
/// Most unsaved pages kept after they are closed.
pub const MAX_RETAINED_PAGES: usize = 20;

/// A page list: the pages of one conversation, live or trashed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PageList {
    pub pages: Vec<PageSummary>,
    /// More follow; ask for them with `DocsInput::LoadMorePages`.
    pub has_more: bool,
    pub load: LoadStatus,
}

impl PageList {
    pub(crate) fn empty() -> Self {
        Self {
            pages: Vec::new(),
            has_more: false,
            load: LoadStatus::Idle,
        }
    }
}

/// Where the open page's working copy stands against the server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SaveState {
    /// The working copy is what the server holds.
    Saved,
    /// There are edits not yet sent. Send them with `DocsInput::SavePage` (after a pause in
    /// typing, and before closing).
    Dirty,
    /// A save is journaled or in flight.
    Saving,
    /// The last save failed. `DocsInput::SavePage` tries again (the same operation if its outcome
    /// is unknown).
    Failed(String),
    /// Someone saved a newer revision first. `ClientCore::open_page` holds it as `conflict`;
    /// settle it with `DocsInput::ResolveConflict`.
    Conflict,
}

/// How to settle a conflict.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConflictChoice {
    /// Save the working copy over the newer revision. The newer revision stays in the history.
    KeepMine,
    /// Replace the working copy with the newer revision.
    TakeTheirs,
}

/// A page's versions and the one being previewed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VersionsView {
    /// Newest first.
    pub versions: Vec<PageVersionSummary>,
    pub load: LoadStatus,
    pub preview: Option<PageVersionDetail>,
    pub preview_load: LoadStatus,
    /// A restore is on its way.
    pub restoring: bool,
}

impl VersionsView {
    pub(crate) fn new() -> Self {
        Self {
            versions: Vec::new(),
            load: LoadStatus::Idle,
            preview: None,
            preview_load: LoadStatus::Idle,
            restoring: false,
        }
    }
}

/// The page being read or edited.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenPage {
    pub id: PageId,
    pub load: LoadStatus,
    /// The page as the server last told this client, with its content and files. Edits go
    /// against this revision.
    pub base: Option<PageDetail>,
    /// The title being edited.
    pub title: String,
    /// The content being edited (a page's text or a board).
    pub content: Option<PageContent>,
    pub save: SaveState,
    /// The newer revision found by a failed save.
    pub conflict: Option<PageDetail>,
    /// Another person changed the page at this revision, and this client has not loaded it.
    pub newer_revision: Option<u32>,
    /// The page was removed for good, or the person lost access to it.
    pub gone: bool,
    pub versions: VersionsView,
    /// Oldest first.
    pub notations: Vec<Notation>,
    pub notations_load: LoadStatus,
}

impl OpenPage {
    pub(crate) fn new(id: PageId) -> Self {
        Self {
            id,
            load: LoadStatus::Idle,
            base: None,
            title: String::new(),
            content: None,
            save: SaveState::Saved,
            conflict: None,
            newer_revision: None,
            gone: false,
            versions: VersionsView::new(),
            notations: Vec::new(),
            notations_load: LoadStatus::Idle,
        }
    }

    /// Whether the working copy differs from the page as loaded.
    pub fn has_edits(&self) -> bool {
        match &self.base {
            Some(base) => {
                self.title != base.summary.title || self.content.as_ref() != Some(&base.content)
            }
            None => false,
        }
    }

    /// Whether the page is in the trash (it can be read, not edited).
    pub fn is_trashed(&self) -> bool {
        self.base
            .as_ref()
            .is_some_and(|base| base.summary.trashed_at.is_some())
    }
}

/// A search of pages across a workspace.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DocsSearchView {
    pub workspace: WorkspaceId,
    pub query: String,
    pub results: Vec<PageSummary>,
    pub has_more: bool,
    pub load: LoadStatus,
}

/// The person's notations across a workspace.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NotationInboxView {
    pub workspace: WorkspaceId,
    pub scope: NotationScope,
    pub completed: bool,
    pub notations: Vec<Notation>,
    pub load: LoadStatus,
}

/// Progress of creating a page.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CreateState {
    InFlight,
    /// Refused or given up on. `DocsInput::RetryCreate` tries again with the same operation if
    /// its outcome is unknown; creating again with a new page id is a new page.
    Failed(String),
}

/// An edit that was journaled, sent, and refused as stale after a restart: the person's text is
/// kept so it is not lost.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecoveredEdit {
    pub operation: OperationId,
    pub page: PageId,
    pub title: Option<String>,
    pub content: Option<PageContent>,
    pub submitted_at: Timestamp,
}

/// A create or a save, recorded durably before it is sent, so an interrupted one is finished
/// after a restart. Sending it again with the same operation id returns the original result.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DocJournalEntry {
    pub operation_id: OperationId,
    pub user_id: UserId,
    pub submitted_at: Timestamp,
    pub action: DocJournalAction,
}

/// What a journal entry will send.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum DocJournalAction {
    Create {
        conversation: ConversationId,
        request: CreatePageRequest,
    },
    Update {
        page: PageId,
        request: UpdatePageRequest,
    },
}

impl fmt::Debug for DocJournalEntry {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DocJournalEntry")
            .field("operation_id", &self.operation_id)
            .field("action", &self.action)
            .finish()
    }
}

impl fmt::Debug for DocJournalAction {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Create { conversation, .. } => f
                .debug_struct("Create")
                .field("conversation", conversation)
                .finish(),
            Self::Update { page, .. } => f.debug_struct("Update").field("page", page).finish(),
        }
    }
}

/// What the person does to docs. Wrapped in `Input::Docs`.
#[derive(Clone, PartialEq, Eq)]
pub enum DocsInput {
    /// Load the first page of a conversation's pages (live or trashed), replacing what was held.
    ListPages {
        conversation: ConversationId,
        state: PageState,
    },
    /// Load the next page of a list that has more.
    LoadMorePages {
        conversation: ConversationId,
        state: PageState,
    },
    /// Load the first page of every page in a workspace the person can read (live or trashed),
    /// replacing what was held. Read it with `ClientCore::workspace_pages`.
    ListWorkspacePages {
        workspace: WorkspaceId,
        state: PageState,
    },
    /// Load the next page of a workspace list that has more.
    LoadMoreWorkspacePages {
        workspace: WorkspaceId,
        state: PageState,
    },
    /// Open a page or board. Loads it and, for a page, its notations.
    OpenPage(PageId),
    /// Close the open page. Unsaved edits are saved first, in the background.
    ClosePage,
    /// Make a page or board. `page` is the id the host chose (`PageId::generate()`), so a retry
    /// is the same creation. `content` of `None` means empty.
    CreatePage {
        page: PageId,
        conversation: ConversationId,
        kind: PageKind,
        title: String,
        content: Option<PageContent>,
    },
    /// Try again a creation that failed with an unknown outcome.
    RetryCreate(PageId),
    /// Change the open page's working copy. Nothing is sent until `SavePage`.
    EditPage {
        title: Option<String>,
        content: Option<PageContent>,
    },
    /// Send the open page's edits (journaled first). Safe to send often: one save is in flight at
    /// a time and edits made meanwhile go in the next.
    SavePage,
    /// Settle a conflict found by a save.
    ResolveConflict(ConflictChoice),
    /// Load the open page again (after `newer_revision`, or a failed load).
    ReloadPage,
    /// Move a page to another conversation of the workspace.
    MovePage {
        page: PageId,
        conversation: ConversationId,
    },
    TrashPage(PageId),
    RestorePage(PageId),
    /// Remove a trashed page for good.
    PurgePage(PageId),
    /// Load the open page's versions.
    LoadVersions,
    /// Load one version to look at it.
    PreviewVersion(u32),
    ClosePreview,
    /// Make a version the page's content. The current content is kept as a version first.
    RestoreVersion(u32),
    /// Search the workspace's pages. Empty text clears the search.
    SearchPages {
        workspace: WorkspaceId,
        text: String,
    },
    LoadMorePageSearch,
    /// Anchor a note on the open page's text.
    CreateNotation {
        notation: NotationId,
        quote: String,
        from: u32,
        to: u32,
        body: String,
        assignee: Option<UserId>,
    },
    UpdateNotation {
        notation: NotationId,
        body: Option<String>,
        assignee: Option<UserId>,
    },
    SetNotationCompleted {
        notation: NotationId,
        completed: bool,
    },
    DeleteNotation(NotationId),
    ReplyToNotation {
        notation: NotationId,
        reply: NotationReplyId,
        body: String,
    },
    DeleteNotationReply {
        notation: NotationId,
        reply: NotationReplyId,
    },
    /// Load the person's notations in a workspace.
    LoadNotationInbox {
        workspace: WorkspaceId,
        scope: NotationScope,
        completed: bool,
    },
    CloseNotationInbox,
    /// Add a file to the open page. The runtime reads and uploads it in one request.
    AddPageFile {
        attachment: AttachmentId,
        source: FileSource,
    },
    /// Forget an edit kept by a restart (see [`RecoveredEdit`]).
    DismissRecoveredEdit(OperationId),
    DismissDocsNotice,
}

impl fmt::Debug for DocsInput {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ListPages {
                conversation,
                state,
            } => f
                .debug_struct("ListPages")
                .field("conversation", conversation)
                .field("state", state)
                .finish(),
            Self::LoadMorePages {
                conversation,
                state,
            } => f
                .debug_struct("LoadMorePages")
                .field("conversation", conversation)
                .field("state", state)
                .finish(),
            Self::ListWorkspacePages { workspace, state } => f
                .debug_struct("ListWorkspacePages")
                .field("workspace", workspace)
                .field("state", state)
                .finish(),
            Self::LoadMoreWorkspacePages { workspace, state } => f
                .debug_struct("LoadMoreWorkspacePages")
                .field("workspace", workspace)
                .field("state", state)
                .finish(),
            Self::OpenPage(page) => f.debug_tuple("OpenPage").field(page).finish(),
            Self::ClosePage => f.write_str("ClosePage"),
            Self::CreatePage {
                page,
                conversation,
                kind,
                title,
                content,
            } => f
                .debug_struct("CreatePage")
                .field("page", page)
                .field("conversation", conversation)
                .field("kind", kind)
                .field("title", &Text(title))
                .field("content", &content.as_ref().map(|_| Named("content")))
                .finish(),
            Self::RetryCreate(page) => f.debug_tuple("RetryCreate").field(page).finish(),
            Self::EditPage { title, content } => f
                .debug_struct("EditPage")
                .field("title", &title.as_ref().map(|t| Text(t)))
                .field("content", &content.as_ref().map(|_| Named("content")))
                .finish(),
            Self::SavePage => f.write_str("SavePage"),
            Self::ResolveConflict(choice) => {
                f.debug_tuple("ResolveConflict").field(choice).finish()
            }
            Self::ReloadPage => f.write_str("ReloadPage"),
            Self::MovePage { page, conversation } => f
                .debug_struct("MovePage")
                .field("page", page)
                .field("conversation", conversation)
                .finish(),
            Self::TrashPage(page) => f.debug_tuple("TrashPage").field(page).finish(),
            Self::RestorePage(page) => f.debug_tuple("RestorePage").field(page).finish(),
            Self::PurgePage(page) => f.debug_tuple("PurgePage").field(page).finish(),
            Self::LoadVersions => f.write_str("LoadVersions"),
            Self::PreviewVersion(n) => f.debug_tuple("PreviewVersion").field(n).finish(),
            Self::ClosePreview => f.write_str("ClosePreview"),
            Self::RestoreVersion(n) => f.debug_tuple("RestoreVersion").field(n).finish(),
            Self::SearchPages { workspace, text } => f
                .debug_struct("SearchPages")
                .field("workspace", workspace)
                .field("text", &Text(text))
                .finish(),
            Self::LoadMorePageSearch => f.write_str("LoadMorePageSearch"),
            Self::CreateNotation {
                notation,
                quote,
                from,
                to,
                body,
                assignee,
            } => f
                .debug_struct("CreateNotation")
                .field("notation", notation)
                .field("quote", &Text(quote))
                .field("from", from)
                .field("to", to)
                .field("body", &Text(body))
                .field("assignee", assignee)
                .finish(),
            Self::UpdateNotation {
                notation,
                body,
                assignee,
            } => f
                .debug_struct("UpdateNotation")
                .field("notation", notation)
                .field("body", &body.as_ref().map(|b| Text(b)))
                .field("assignee", assignee)
                .finish(),
            Self::SetNotationCompleted {
                notation,
                completed,
            } => f
                .debug_struct("SetNotationCompleted")
                .field("notation", notation)
                .field("completed", completed)
                .finish(),
            Self::DeleteNotation(n) => f.debug_tuple("DeleteNotation").field(n).finish(),
            Self::ReplyToNotation {
                notation,
                reply,
                body,
            } => f
                .debug_struct("ReplyToNotation")
                .field("notation", notation)
                .field("reply", reply)
                .field("body", &Text(body))
                .finish(),
            Self::DeleteNotationReply { notation, reply } => f
                .debug_struct("DeleteNotationReply")
                .field("notation", notation)
                .field("reply", reply)
                .finish(),
            Self::LoadNotationInbox {
                workspace,
                scope,
                completed,
            } => f
                .debug_struct("LoadNotationInbox")
                .field("workspace", workspace)
                .field("scope", scope)
                .field("completed", completed)
                .finish(),
            Self::CloseNotationInbox => f.write_str("CloseNotationInbox"),
            Self::AddPageFile { attachment, source } => f
                .debug_struct("AddPageFile")
                .field("attachment", attachment)
                .field("source", source)
                .finish(),
            Self::DismissRecoveredEdit(op) => {
                f.debug_tuple("DismissRecoveredEdit").field(op).finish()
            }
            Self::DismissDocsNotice => f.write_str("DismissDocsNotice"),
        }
    }
}

/// One request to the server. Wrapped in [`DocsEffect::Request`].
#[derive(Clone, PartialEq, Eq)]
pub enum DocsCall {
    ListPages {
        conversation: ConversationId,
        params: PageListParams,
    },
    ListWorkspacePages {
        workspace: WorkspaceId,
        params: PageListParams,
    },
    GetPage(PageId),
    CreatePage {
        conversation: ConversationId,
        request: CreatePageRequest,
    },
    UpdatePage {
        page: PageId,
        request: UpdatePageRequest,
    },
    TrashPage {
        page: PageId,
        request: PageActionRequest,
    },
    RestorePage {
        page: PageId,
        request: PageActionRequest,
    },
    PurgePage {
        page: PageId,
        request: PageActionRequest,
    },
    ListVersions(PageId),
    GetVersion {
        page: PageId,
        version: u32,
    },
    RestoreVersion {
        page: PageId,
        version: u32,
        request: RestoreVersionRequest,
    },
    ListNotations(PageId),
    CreateNotation {
        page: PageId,
        request: CreateNotationRequest,
    },
    UpdateNotation {
        notation: NotationId,
        request: UpdateNotationRequest,
    },
    SetNotationCompleted {
        notation: NotationId,
        request: SetNotationCompletedRequest,
    },
    DeleteNotation {
        notation: NotationId,
        request: NotationActionRequest,
    },
    ReplyToNotation {
        notation: NotationId,
        request: CreateNotationReplyRequest,
    },
    DeleteNotationReply {
        reply: NotationReplyId,
        request: NotationActionRequest,
    },
    ListWorkspaceNotations {
        workspace: WorkspaceId,
        params: NotationListParams,
    },
    /// Read the file and upload it with a single `PUT`. The same attachment id is the same upload.
    UploadPageFile {
        page: PageId,
        attachment: AttachmentId,
        source: FileSource,
    },
}

impl DocsCall {
    /// Whether the call changes something (so its outcome must be settled, never replaced by a
    /// request with a new operation id).
    pub fn is_mutation(&self) -> bool {
        !matches!(
            self,
            Self::ListPages { .. }
                | Self::ListWorkspacePages { .. }
                | Self::GetPage(_)
                | Self::ListVersions(_)
                | Self::GetVersion { .. }
                | Self::ListNotations(_)
                | Self::ListWorkspaceNotations { .. }
        )
    }
}

impl fmt::Debug for DocsCall {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ListPages { conversation, .. } => f
                .debug_struct("ListPages")
                .field("conversation", conversation)
                .finish(),
            Self::ListWorkspacePages { workspace, .. } => f
                .debug_struct("ListWorkspacePages")
                .field("workspace", workspace)
                .field("params", &Named("params"))
                .finish(),
            Self::GetPage(p) => f.debug_tuple("GetPage").field(p).finish(),
            Self::CreatePage {
                conversation,
                request,
            } => f
                .debug_struct("CreatePage")
                .field("conversation", conversation)
                .field("page", &request.page_id)
                .field("operation", &request.operation_id)
                .finish(),
            Self::UpdatePage { page, request } => f
                .debug_struct("UpdatePage")
                .field("page", page)
                .field("operation", &request.operation_id)
                .field("base_revision", &request.base_revision)
                .finish(),
            Self::TrashPage { page, request } => f
                .debug_struct("TrashPage")
                .field("page", page)
                .field("operation", &request.operation_id)
                .finish(),
            Self::RestorePage { page, request } => f
                .debug_struct("RestorePage")
                .field("page", page)
                .field("operation", &request.operation_id)
                .finish(),
            Self::PurgePage { page, request } => f
                .debug_struct("PurgePage")
                .field("page", page)
                .field("operation", &request.operation_id)
                .finish(),
            Self::ListVersions(p) => f.debug_tuple("ListVersions").field(p).finish(),
            Self::GetVersion { page, version } => f
                .debug_struct("GetVersion")
                .field("page", page)
                .field("version", version)
                .finish(),
            Self::RestoreVersion {
                page,
                version,
                request,
            } => f
                .debug_struct("RestoreVersion")
                .field("page", page)
                .field("version", version)
                .field("operation", &request.operation_id)
                .finish(),
            Self::ListNotations(p) => f.debug_tuple("ListNotations").field(p).finish(),
            Self::CreateNotation { page, request } => f
                .debug_struct("CreateNotation")
                .field("page", page)
                .field("notation", &request.notation_id)
                .field("body", &Text(&request.body))
                .finish(),
            Self::UpdateNotation { notation, request } => f
                .debug_struct("UpdateNotation")
                .field("notation", notation)
                .field("operation", &request.operation_id)
                .finish(),
            Self::SetNotationCompleted { notation, request } => f
                .debug_struct("SetNotationCompleted")
                .field("notation", notation)
                .field("completed", &request.completed)
                .finish(),
            Self::DeleteNotation { notation, .. } => f
                .debug_struct("DeleteNotation")
                .field("notation", notation)
                .finish(),
            Self::ReplyToNotation { notation, request } => f
                .debug_struct("ReplyToNotation")
                .field("notation", notation)
                .field("reply", &request.reply_id)
                .field("body", &Text(&request.body))
                .finish(),
            Self::DeleteNotationReply { reply, .. } => f
                .debug_struct("DeleteNotationReply")
                .field("reply", reply)
                .finish(),
            Self::ListWorkspaceNotations { workspace, .. } => f
                .debug_struct("ListWorkspaceNotations")
                .field("workspace", workspace)
                .finish(),
            Self::UploadPageFile {
                page,
                attachment,
                source,
            } => f
                .debug_struct("UploadPageFile")
                .field("page", page)
                .field("attachment", attachment)
                .field("source", source)
                .finish(),
        }
    }
}

/// Work for the runtime. Wrapped in `Effect::Docs`.
#[derive(Clone, PartialEq, Eq)]
pub enum DocsEffect {
    /// Make the call and answer with `Input::DocsReply` carrying the same `id`. A mutation is a
    /// single attempt: the runtime does not repeat it (the core sends it again, with the same
    /// operation id, if the outcome is unknown). A read may be repeated.
    Request { id: u64, call: DocsCall },
    /// Durably record a create or a save. Answered by `Input::DocsPersisted` once it is on disk,
    /// or `Input::DocsPersistFailed`. The request goes out only after that.
    Persist(DocJournalEntry),
    /// The operation is settled; forget its journal entry.
    Settle { operation: OperationId },
}

impl fmt::Debug for DocsEffect {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Request { id, call } => f
                .debug_struct("Request")
                .field("id", id)
                .field("call", call)
                .finish(),
            Self::Persist(entry) => f.debug_tuple("Persist").field(entry).finish(),
            Self::Settle { operation } => f
                .debug_struct("Settle")
                .field("operation", operation)
                .finish(),
        }
    }
}

/// What a successful call returned.
#[derive(Clone, PartialEq, Eq)]
pub enum DocsAnswer {
    Pages(PageListResponse),
    Page(PageResponse),
    Versions(PageVersionsResponse),
    Version(PageVersionResponse),
    Notations(NotationListResponse),
    Notation(NotationResponse),
    File(Attachment),
    /// Succeeded with nothing to apply (purge, delete).
    Done,
}

impl fmt::Debug for DocsAnswer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Pages(r) => write!(f, "Pages({:?})", Count("pages", r.pages.len())),
            Self::Page(_) => f.write_str("Page(<page>)"),
            Self::Versions(r) => write!(f, "Versions({:?})", Count("versions", r.versions.len())),
            Self::Version(_) => f.write_str("Version(<version>)"),
            Self::Notations(r) => {
                write!(f, "Notations({:?})", Count("notations", r.notations.len()))
            }
            Self::Notation(_) => f.write_str("Notation(<notation>)"),
            Self::File(_) => f.write_str("File(<file>)"),
            Self::Done => f.write_str("Done"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debug_output_never_shows_what_was_typed() {
        let input = DocsInput::CreateNotation {
            notation: NotationId::generate(),
            quote: "the secret quote".into(),
            from: 0,
            to: 4,
            body: "a private remark".into(),
            assignee: None,
        };
        let text = format!("{input:?}");
        assert!(
            !text.contains("secret") && !text.contains("private"),
            "{text}"
        );
        let input = DocsInput::EditPage {
            title: Some("Salaries".into()),
            content: Some(PageContent::Page {
                body: "top secret".into(),
            }),
        };
        let text = format!("{input:?}");
        assert!(
            !text.contains("Salaries") && !text.contains("top secret"),
            "{text}"
        );
    }

    #[test]
    fn only_reads_are_not_mutations() {
        assert!(!DocsCall::GetPage(PageId::generate()).is_mutation());
        assert!(
            DocsCall::TrashPage {
                page: PageId::generate(),
                request: PageActionRequest {
                    operation_id: OperationId::generate()
                }
            }
            .is_mutation()
        );
    }
}
