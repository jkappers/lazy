//! Docs (D30): pages, boards, versions, trash, and notations.
//!
//! # What the core keeps
//!
//! Nothing here is saved to disk except the journal of creates and saves. Lists, the open page,
//! versions, notations, the search, and the notation inbox are fetched when asked for and kept in
//! memory; a restart starts them empty. (The content cache holds messages, and a page's body can
//! be a megabyte, so caching pages is a later decision.)
//!
//! # Concurrency rules
//!
//! * Every mutation has an operation id. A request whose outcome is unknown (`RequestError::Unknown`)
//!   is sent again with the **same** call, so the same operation id, up to [`MAX_DOC_RESENDS`]
//!   times; the server returns the original result. It is never replaced by a new operation.
//! * A create or a save is journaled (`DocsEffect::Persist`) before its request is sent, and
//!   settled after a definitive answer. After a restart the journal is sent again the same way.
//! * Edits are made against a working copy. One save is in flight per page; edits made meanwhile
//!   go in the next save, sent as soon as the first is answered.
//! * A save is sent with `base_revision`. `stale_revision` fetches the newer page and puts the
//!   page in [`SaveState::Conflict`] until the person keeps theirs or takes the other.
//! * Pages change on the server in a total order given by `revision`; a summary is applied only
//!   if its revision is newer than the one held. A trash or restore changes no revision, so
//!   an *event* (which arrives in log order) is always applied, while a *response* is applied only
//!   if no event touched the page since the request was made. This is the `clock` below.

use std::collections::{HashMap, HashSet, VecDeque};

use lazy_domain::page::{self, PageContent, PageKind};
use lazy_domain::{
    ConversationId, NotationId, OperationId, PageId, Timestamp, UserId, WorkspaceId,
};
use lazy_protocol::{
    CreateNotationReplyRequest, CreateNotationRequest, CreatePageRequest, ErrorCode, Notation,
    NotationActionRequest, NotationListParams, NotationScope, PageActionRequest, PageDetail,
    PageListParams, PageSort, PageState, PageSummary, RestoreVersionRequest,
    SetNotationCompletedRequest, UpdateNotationRequest, UpdatePageRequest,
};

use crate::change::{Change, Output};
use crate::docs::{
    ConflictChoice, CreateState, DOCS_PAGE, DocJournalAction, DocJournalEntry, DocsAnswer,
    DocsCall, DocsEffect, DocsInput, DocsSearchView, MAX_DOC_RESENDS, NotationInboxView, OpenPage,
    PageList, RecoveredEdit, SaveState,
};
use crate::effect::Effect;
use crate::model::{RequestError, SessionPhase};
use crate::notify::LoadStatus;

use super::ClientCore;

/// Most operation ids remembered to recognize our own changes when they come back as events.
const MY_OPS: usize = 64;

/// Which list a slot holds: one conversation's pages, or every page of a workspace the person
/// can read.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum ListKey {
    Conversation(ConversationId, PageState),
    Workspace(WorkspaceId, PageState),
}

impl ListKey {
    fn state(self) -> PageState {
        match self {
            Self::Conversation(_, state) | Self::Workspace(_, state) => state,
        }
    }

    fn change(self) -> Change {
        match self {
            Self::Conversation(conversation, _) => Change::PageListChanged { conversation },
            Self::Workspace(workspace, _) => Change::WorkspacePagesChanged { workspace },
        }
    }

    /// Whether `summary` belongs in this list.
    fn holds(self, summary: &PageSummary) -> bool {
        let trashed = summary.trashed_at.is_some();
        match self {
            Self::Conversation(conversation, state) => {
                summary.conversation_id == conversation && trashed == (state == PageState::Trashed)
            }
            Self::Workspace(workspace, state) => {
                summary.workspace_id == workspace && trashed == (state == PageState::Trashed)
            }
        }
    }
}

/// Where a summary came from.
#[derive(Clone, Copy)]
enum Source {
    /// A realtime event: in log order, always applied if not older.
    Event,
    /// A response to a request made when the clock read `since`.
    Response { since: u64 },
}

#[derive(Debug)]
enum SaveStage {
    /// Waiting for the journal write.
    Persisting,
    /// The request is out (possibly sent again).
    Sending,
    /// Resent too often without an answer: the outcome is unknown. `SavePage` sends it again.
    Unsettled,
}

#[derive(Debug)]
struct SaveOp {
    operation: OperationId,
    request: UpdatePageRequest,
    stage: SaveStage,
    /// Recovered from the journal after a restart, with no open page to return to.
    recovered: bool,
    submitted_at: Timestamp,
}

#[derive(Debug)]
struct PageSlot {
    view: OpenPage,
    save: Option<SaveOp>,
}

#[derive(Debug)]
struct CreateEntry {
    operation: OperationId,
    conversation: ConversationId,
    request: CreatePageRequest,
    stage: SaveStage,
    /// The server definitively refused it: a retry is a new operation.
    refused: bool,
    state: CreateState,
}

#[derive(Debug)]
struct ListSlot {
    list: PageList,
    generation: u64,
}

#[derive(Debug)]
struct SearchSlot {
    view: DocsSearchView,
    generation: u64,
}

#[derive(Debug)]
struct InboxSlot {
    view: NotationInboxView,
    generation: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Refresh {
    /// A save was refused as stale: show the newer page as a conflict.
    Conflict,
    /// Another person changed the page: load it.
    Reload,
}

/// What a request in flight is for.
#[derive(Debug)]
enum Pending {
    List {
        key: ListKey,
        generation: u64,
        offset: usize,
        since: u64,
    },
    Search {
        generation: u64,
        offset: usize,
        since: u64,
    },
    Inbox {
        generation: u64,
    },
    Open {
        page: PageId,
    },
    Refresh {
        page: PageId,
        purpose: Refresh,
    },
    Create {
        page: PageId,
        since: u64,
    },
    /// An unknown outcome of a create is settled by asking whether the page exists: the status
    /// route does not know page operations. `create` is sent again, as the same operation, only if
    /// the page is not there.
    CreateCheck {
        page: PageId,
        since: u64,
        create: Box<DocsCall>,
    },
    Save {
        page: PageId,
        since: u64,
    },
    /// Trash, restore, move, purge.
    Change {
        page: PageId,
        since: u64,
    },
    Versions {
        page: PageId,
    },
    Version {
        page: PageId,
    },
    RestoreVersion {
        page: PageId,
        since: u64,
    },
    Notations {
        page: PageId,
    },
    /// Any change to a notation or its replies.
    NotationChange {
        page: Option<PageId>,
    },
    File {
        page: PageId,
    },
}

#[derive(Debug)]
struct Request {
    what: Pending,
    call: DocsCall,
    resends: u32,
}

/// What the core keeps about docs.
#[derive(Debug, Default)]
pub(super) struct Docs {
    lists: HashMap<ListKey, ListSlot>,
    pages: HashMap<PageId, PageSlot>,
    open: Option<PageId>,
    search: Option<SearchSlot>,
    inbox: Option<InboxSlot>,
    creates: HashMap<PageId, CreateEntry>,
    /// The page just created, so opening it needs no wait.
    fresh: Option<PageDetail>,
    recovered_edits: Vec<RecoveredEdit>,
    /// Journal entries from a previous run that are not yet sent.
    journal: Vec<DocJournalEntry>,
    pending: HashMap<u64, Request>,
    /// Mutations that went unanswered too often; sent again after any answer arrives.
    stalled: Vec<Request>,
    next_request: u64,
    next_generation: u64,
    /// Counts events applied; `epochs` says when each page last had one.
    clock: u64,
    epochs: HashMap<PageId, u64>,
    my_ops: VecDeque<OperationId>,
    notice: Option<String>,
}

impl Docs {
    fn remember_op(&mut self, operation: OperationId) {
        if self.my_ops.contains(&operation) {
            return;
        }
        if self.my_ops.len() == MY_OPS {
            self.my_ops.pop_front();
        }
        self.my_ops.push_back(operation);
    }
}

fn newest_first(pages: &mut [PageSummary]) {
    pages.sort_by(|a, b| {
        b.updated_at
            .cmp(&a.updated_at)
            .then_with(|| b.id.cmp(&a.id))
    });
}

fn failed(error: &RequestError) -> LoadStatus {
    LoadStatus::Failed {
        reason: error.message(),
        can_retry: matches!(error, RequestError::Unknown(_)),
    }
}

fn is_code(error: &RequestError, code: ErrorCode) -> bool {
    matches!(error, RequestError::Rejected(e) if e.code == code)
}

impl ClientCore {
    // ----- Read model -----

    /// The pages of a conversation, live or trashed, once `DocsInput::ListPages` asked for them.
    pub fn page_list(&self, conversation: ConversationId, state: PageState) -> Option<&PageList> {
        self.docs
            .lists
            .get(&ListKey::Conversation(conversation, state))
            .map(|s| &s.list)
    }

    /// Every page of a workspace the person can read, live or trashed, once
    /// `DocsInput::ListWorkspacePages` asked for them. Newest change first.
    pub fn workspace_pages(&self, workspace: WorkspaceId, state: PageState) -> Option<&PageList> {
        self.docs
            .lists
            .get(&ListKey::Workspace(workspace, state))
            .map(|s| &s.list)
    }

    /// The page being read or edited.
    pub fn open_page(&self) -> Option<&OpenPage> {
        let id = self.docs.open?;
        self.docs.pages.get(&id).map(|slot| &slot.view)
    }

    /// The search of pages, if one is shown.
    pub fn page_search(&self) -> Option<&DocsSearchView> {
        self.docs.search.as_ref().map(|s| &s.view)
    }

    /// The person's notations across a workspace, once loaded.
    pub fn notation_inbox(&self) -> Option<&NotationInboxView> {
        self.docs.inbox.as_ref().map(|s| &s.view)
    }

    /// Progress of making a page; `None` once it is made (see `Change::PageCreated`) or when
    /// none was asked for.
    pub fn create_state(&self, page: PageId) -> Option<&CreateState> {
        self.docs.creates.get(&page).map(|entry| &entry.state)
    }

    /// Edits kept because a restart found them already out of date. Show them so the text is not
    /// lost, then dismiss them.
    pub fn recovered_edits(&self) -> &[RecoveredEdit] {
        &self.docs.recovered_edits
    }

    /// The last thing that went wrong with a docs request that has nowhere better to show.
    pub fn docs_notice(&self) -> Option<&str> {
        self.docs.notice.as_deref()
    }

    /// Where a notation's quote is now in the open page's text (characters), or `None` when the
    /// quote is gone (the note is detached) or the page is a board.
    pub fn notation_anchor(&self, notation: &Notation) -> Option<(u32, u32)> {
        match self.open_page()?.content.as_ref()? {
            PageContent::Page { body } => {
                page::relocate_anchor(body, &notation.quote, notation.from, notation.to)
            }
            PageContent::Board { .. } => None,
        }
    }

    // ----- Plumbing -----

    fn docs_ready(&self) -> bool {
        matches!(self.phase, SessionPhase::Ready)
    }

    fn docs_me(&self) -> Option<UserId> {
        self.me.as_ref().map(|me| me.id)
    }

    fn docs_send(&mut self, what: Pending, call: DocsCall, out: &mut Output) {
        self.docs_send_request(
            Request {
                what,
                call,
                resends: 0,
            },
            out,
        );
    }

    fn docs_send_request(&mut self, request: Request, out: &mut Output) {
        self.docs.next_request += 1;
        let id = self.docs.next_request;
        out.effects.push(Effect::Docs(DocsEffect::Request {
            id,
            call: request.call.clone(),
        }));
        self.docs.pending.insert(id, request);
    }

    fn docs_notice_set(&mut self, notice: Option<String>, out: &mut Output) {
        if self.docs.notice != notice {
            self.docs.notice = notice;
            out.changes.push(Change::DocsStateChanged);
        }
    }

    fn docs_generation(&mut self) -> u64 {
        self.docs.next_generation += 1;
        self.docs.next_generation
    }

    fn docs_op(&mut self) -> OperationId {
        let operation = OperationId::generate();
        self.docs.remember_op(operation);
        operation
    }

    // ----- Inputs -----

    pub(super) fn docs_input(&mut self, input: DocsInput, out: &mut Output) {
        if !self.docs_ready() {
            return;
        }
        match input {
            DocsInput::ListPages {
                conversation,
                state,
            } => self.docs_list(ListKey::Conversation(conversation, state), false, out),
            DocsInput::LoadMorePages {
                conversation,
                state,
            } => self.docs_list(ListKey::Conversation(conversation, state), true, out),
            DocsInput::ListWorkspacePages { workspace, state } => {
                self.docs_list(ListKey::Workspace(workspace, state), false, out)
            }
            DocsInput::LoadMoreWorkspacePages { workspace, state } => {
                self.docs_list(ListKey::Workspace(workspace, state), true, out)
            }
            DocsInput::OpenPage(page) => self.docs_open(page, out),
            DocsInput::ClosePage => self.docs_close(out),
            DocsInput::CreatePage {
                page,
                conversation,
                kind,
                title,
                content,
            } => self.docs_create(page, conversation, kind, title, content, out),
            DocsInput::RetryCreate(page) => self.docs_retry_create(page, out),
            DocsInput::EditPage { title, content } => self.docs_edit(title, content, out),
            DocsInput::SavePage => {
                if let Some(page) = self.docs.open {
                    self.docs_save(page, out);
                }
            }
            DocsInput::ResolveConflict(choice) => self.docs_resolve(choice, out),
            DocsInput::ReloadPage => {
                if let Some(page) = self.docs.open {
                    self.docs_fetch_open(page, out);
                }
            }
            DocsInput::MovePage { page, conversation } => self.docs_move(page, conversation, out),
            DocsInput::TrashPage(page) => self.docs_change_state(page, Change2::Trash, out),
            DocsInput::RestorePage(page) => self.docs_change_state(page, Change2::Restore, out),
            DocsInput::PurgePage(page) => self.docs_change_state(page, Change2::Purge, out),
            DocsInput::LoadVersions => self.docs_load_versions(out),
            DocsInput::PreviewVersion(version) => self.docs_preview_version(version, out),
            DocsInput::ClosePreview => {
                if let Some(slot) = self.docs_open_slot() {
                    slot.view.versions.preview = None;
                    slot.view.versions.preview_load = LoadStatus::Idle;
                    out.changes.push(Change::OpenPageChanged);
                }
            }
            DocsInput::RestoreVersion(version) => self.docs_restore_version(version, out),
            DocsInput::SearchPages { workspace, text } => self.docs_search(workspace, text, out),
            DocsInput::LoadMorePageSearch => self.docs_search_more(out),
            DocsInput::CreateNotation {
                notation,
                quote,
                from,
                to,
                body,
                assignee,
            } => self.docs_create_notation(notation, quote, from, to, body, assignee, out),
            DocsInput::UpdateNotation {
                notation,
                body,
                assignee,
            } => {
                let request = UpdateNotationRequest {
                    operation_id: self.docs_op(),
                    body,
                    assignee_id: assignee,
                };
                let page = self.docs_page_of_notation(notation);
                self.docs_send(
                    Pending::NotationChange { page },
                    DocsCall::UpdateNotation { notation, request },
                    out,
                );
            }
            DocsInput::SetNotationCompleted {
                notation,
                completed,
            } => {
                let request = SetNotationCompletedRequest {
                    operation_id: self.docs_op(),
                    completed,
                };
                let page = self.docs_page_of_notation(notation);
                self.docs_send(
                    Pending::NotationChange { page },
                    DocsCall::SetNotationCompleted { notation, request },
                    out,
                );
            }
            DocsInput::DeleteNotation(notation) => {
                let request = NotationActionRequest {
                    operation_id: self.docs_op(),
                };
                let page = self.docs_page_of_notation(notation);
                self.docs_send(
                    Pending::NotationChange { page },
                    DocsCall::DeleteNotation { notation, request },
                    out,
                );
            }
            DocsInput::ReplyToNotation {
                notation,
                reply,
                body,
            } => {
                if let Err(error) = page::validate_notation_body(&body) {
                    self.docs_notice_set(Some(error.to_string()), out);
                    return;
                }
                let request = CreateNotationReplyRequest {
                    operation_id: self.docs_op(),
                    reply_id: reply,
                    body,
                };
                let page = self.docs_page_of_notation(notation);
                self.docs_send(
                    Pending::NotationChange { page },
                    DocsCall::ReplyToNotation { notation, request },
                    out,
                );
            }
            DocsInput::DeleteNotationReply { notation, reply } => {
                let request = NotationActionRequest {
                    operation_id: self.docs_op(),
                };
                let page = self.docs_page_of_notation(notation);
                self.docs_send(
                    Pending::NotationChange { page },
                    DocsCall::DeleteNotationReply { reply, request },
                    out,
                );
            }
            DocsInput::LoadNotationInbox {
                workspace,
                scope,
                completed,
            } => self.docs_load_inbox(workspace, scope, completed, out),
            DocsInput::CloseNotationInbox => {
                if self.docs.inbox.take().is_some() {
                    out.changes.push(Change::NotationInboxChanged);
                }
            }
            DocsInput::AddPageFile { attachment, source } => {
                let Some(page) = self.docs.open else { return };
                self.docs_send(
                    Pending::File { page },
                    DocsCall::UploadPageFile {
                        page,
                        attachment,
                        source,
                    },
                    out,
                );
            }
            DocsInput::DismissRecoveredEdit(operation) => {
                let before = self.docs.recovered_edits.len();
                self.docs
                    .recovered_edits
                    .retain(|edit| edit.operation != operation);
                if self.docs.recovered_edits.len() != before {
                    out.changes.push(Change::DocsStateChanged);
                }
            }
            DocsInput::DismissDocsNotice => self.docs_notice_set(None, out),
        }
    }

    fn docs_open_slot(&mut self) -> Option<&mut PageSlot> {
        let id = self.docs.open?;
        self.docs.pages.get_mut(&id)
    }

    fn docs_page_of_notation(&self, notation: NotationId) -> Option<PageId> {
        if let Some(slot) = self.open_page()
            && slot.notations.iter().any(|n| n.id == notation)
        {
            return Some(slot.id);
        }
        self.docs
            .inbox
            .as_ref()
            .and_then(|inbox| inbox.view.notations.iter().find(|n| n.id == notation))
            .map(|n| n.page_id)
    }

    // ----- Lists and search -----

    fn docs_list(&mut self, key: ListKey, more: bool, out: &mut Output) {
        let state = key.state();
        let generation = self.docs_generation();
        let since = self.docs.clock;
        let slot = self.docs.lists.entry(key).or_insert_with(|| ListSlot {
            list: PageList::empty(),
            generation: 0,
        });
        if more && (!slot.list.has_more || slot.list.load == LoadStatus::Loading) {
            return;
        }
        let offset = if more { slot.list.pages.len() } else { 0 };
        slot.generation = generation;
        slot.list.load = LoadStatus::Loading;
        if !more {
            // What is held stays until the first page replaces it, so the list does not flash.
            slot.list.has_more = false;
        }
        let params = PageListParams {
            state,
            q: None,
            kind: None,
            sort: PageSort::Updated,
            limit: Some(DOCS_PAGE),
            offset: Some(offset as u32),
        };
        self.docs_send(
            Pending::List {
                key,
                generation,
                offset,
                since,
            },
            match key {
                ListKey::Conversation(conversation, _) => DocsCall::ListPages {
                    conversation,
                    params,
                },
                ListKey::Workspace(workspace, _) => {
                    DocsCall::ListWorkspacePages { workspace, params }
                }
            },
            out,
        );
        out.changes.push(key.change());
    }

    fn docs_search(&mut self, workspace: WorkspaceId, text: String, out: &mut Output) {
        let text = text.trim().to_owned();
        if text.is_empty() {
            if self.docs.search.take().is_some() {
                out.changes.push(Change::PageSearchChanged);
            }
            return;
        }
        let generation = self.docs_generation();
        let since = self.docs.clock;
        let keep = self
            .docs
            .search
            .as_ref()
            .filter(|s| s.view.workspace == workspace)
            .map(|s| s.view.results.clone())
            .unwrap_or_default();
        self.docs.search = Some(SearchSlot {
            view: DocsSearchView {
                workspace,
                query: text.clone(),
                results: keep,
                has_more: false,
                load: LoadStatus::Loading,
            },
            generation,
        });
        let params = PageListParams {
            q: Some(text),
            limit: Some(DOCS_PAGE),
            offset: Some(0),
            ..PageListParams::default()
        };
        self.docs_send(
            Pending::Search {
                generation,
                offset: 0,
                since,
            },
            DocsCall::ListWorkspacePages { workspace, params },
            out,
        );
        out.changes.push(Change::PageSearchChanged);
    }

    fn docs_search_more(&mut self, out: &mut Output) {
        let generation = self.docs_generation();
        let since = self.docs.clock;
        let Some(slot) = self.docs.search.as_mut() else {
            return;
        };
        if !slot.view.has_more || slot.view.load == LoadStatus::Loading {
            return;
        }
        slot.generation = generation;
        slot.view.load = LoadStatus::Loading;
        let offset = slot.view.results.len();
        let workspace = slot.view.workspace;
        let params = PageListParams {
            q: Some(slot.view.query.clone()),
            limit: Some(DOCS_PAGE),
            offset: Some(offset as u32),
            ..PageListParams::default()
        };
        self.docs_send(
            Pending::Search {
                generation,
                offset,
                since,
            },
            DocsCall::ListWorkspacePages { workspace, params },
            out,
        );
        out.changes.push(Change::PageSearchChanged);
    }

    fn docs_load_inbox(
        &mut self,
        workspace: WorkspaceId,
        scope: NotationScope,
        completed: bool,
        out: &mut Output,
    ) {
        let generation = self.docs_generation();
        let keep = self
            .docs
            .inbox
            .as_ref()
            .filter(|s| s.view.workspace == workspace && s.view.scope == scope)
            .map(|s| s.view.notations.clone())
            .unwrap_or_default();
        self.docs.inbox = Some(InboxSlot {
            view: NotationInboxView {
                workspace,
                scope,
                completed,
                notations: keep,
                load: LoadStatus::Loading,
            },
            generation,
        });
        self.docs_send(
            Pending::Inbox { generation },
            DocsCall::ListWorkspaceNotations {
                workspace,
                params: NotationListParams {
                    scope,
                    completed,
                    q: None,
                },
            },
            out,
        );
        out.changes.push(Change::NotationInboxChanged);
    }

    // ----- Opening, closing, loading -----

    fn docs_open(&mut self, page: PageId, out: &mut Output) {
        if self.docs.open == Some(page) {
            return;
        }
        self.docs_close(out);
        let fresh = self
            .docs
            .fresh
            .take()
            .filter(|detail| detail.summary.id == page);
        let slot = self.docs.pages.entry(page).or_insert_with(|| PageSlot {
            view: OpenPage::new(page),
            save: None,
        });
        // A page kept with unsaved edits comes back with them.
        if slot.view.base.is_none()
            && let Some(detail) = fresh
        {
            slot.view.title = detail.summary.title.clone();
            slot.view.content = Some(detail.content.clone());
            slot.view.base = Some(detail);
            slot.view.load = LoadStatus::Ready;
        }
        slot.view.gone = false;
        self.docs.open = Some(page);
        self.docs_fetch_open(page, out);
        out.changes.push(Change::OpenPageChanged);
    }

    fn docs_fetch_open(&mut self, page: PageId, out: &mut Output) {
        if let Some(slot) = self.docs.pages.get_mut(&page) {
            if slot.view.base.is_none() {
                slot.view.load = LoadStatus::Loading;
            }
            slot.view.newer_revision = None;
        }
        self.docs_send(Pending::Open { page }, DocsCall::GetPage(page), out);
        out.changes.push(Change::OpenPageChanged);
    }

    fn docs_close(&mut self, out: &mut Output) {
        let Some(page) = self.docs.open.take() else {
            return;
        };
        let has_edits = self
            .docs
            .pages
            .get(&page)
            .is_some_and(|slot| slot.view.has_edits() && slot.view.save != SaveState::Conflict);
        if has_edits {
            // The page stays in memory until the save settles.
            self.docs_save(page, out);
        }
        self.docs_collect(page);
        out.changes.push(Change::OpenPageChanged);
    }

    /// Drops a closed page's state once nothing in it needs to be kept.
    fn docs_collect(&mut self, page: PageId) {
        if self.docs.open == Some(page) {
            return;
        }
        let keep = self.docs.pages.get(&page).is_some_and(|slot| {
            slot.save.is_some()
                || slot.view.has_edits()
                || matches!(slot.view.save, SaveState::Conflict | SaveState::Failed(_))
        });
        if !keep {
            self.docs.pages.remove(&page);
        }
        // Bound what is retained for closed pages.
        if self.docs.pages.len() > crate::docs::MAX_RETAINED_PAGES {
            let open = self.docs.open;
            let drop: Vec<PageId> = self
                .docs
                .pages
                .iter()
                .filter(|(id, slot)| Some(**id) != open && slot.save.is_none())
                .map(|(id, _)| *id)
                .take(self.docs.pages.len() - crate::docs::MAX_RETAINED_PAGES)
                .collect();
            for id in drop {
                self.docs.pages.remove(&id);
            }
        }
    }

    // ----- Creating -----

    fn docs_create(
        &mut self,
        page_id: PageId,
        conversation: ConversationId,
        kind: PageKind,
        title: String,
        content: Option<PageContent>,
        out: &mut Output,
    ) {
        if self.docs.creates.contains_key(&page_id) {
            return;
        }
        let Some(me) = self.docs_me() else { return };
        let title = match page::normalize_title(&title) {
            Ok(title) => title,
            Err(error) => return self.docs_notice_set(Some(error.to_string()), out),
        };
        if let Some(content) = &content
            && (content.kind() != kind || content.validate().is_err())
        {
            let message = content.validate().err().map_or_else(
                || "That content doesn't fit this kind.".to_owned(),
                |e| e.to_string(),
            );
            return self.docs_notice_set(Some(message), out);
        }
        let operation = self.docs_op();
        let request = CreatePageRequest {
            operation_id: operation,
            page_id,
            kind,
            title,
            content,
        };
        let submitted_at = Timestamp::now();
        self.docs.creates.insert(
            page_id,
            CreateEntry {
                operation,
                conversation,
                request: request.clone(),
                stage: SaveStage::Persisting,
                refused: false,
                state: CreateState::InFlight,
            },
        );
        out.effects
            .push(Effect::Docs(DocsEffect::Persist(DocJournalEntry {
                operation_id: operation,
                user_id: me,
                submitted_at,
                action: DocJournalAction::Create {
                    conversation,
                    request,
                },
            })));
        out.changes.push(Change::DocsStateChanged);
    }

    fn docs_retry_create(&mut self, page: PageId, out: &mut Output) {
        let Some(entry) = self.docs.creates.get_mut(&page) else {
            return;
        };
        if entry.refused {
            // The server definitively refused it: this is a new operation for the same page.
            let request = entry.request.clone();
            let conversation = entry.conversation;
            self.docs.creates.remove(&page);
            self.docs_create(
                page,
                conversation,
                request.kind,
                request.title,
                request.content,
                out,
            );
            return;
        }
        if matches!(entry.stage, SaveStage::Unsettled) {
            entry.stage = SaveStage::Sending;
            entry.state = CreateState::InFlight;
            let call = DocsCall::CreatePage {
                conversation: entry.conversation,
                request: entry.request.clone(),
            };
            let since = self.docs.clock;
            self.docs_send(Pending::Create { page, since }, call, out);
            out.changes.push(Change::DocsStateChanged);
        }
    }

    // ----- Editing and saving -----

    fn docs_edit(&mut self, title: Option<String>, content: Option<PageContent>, out: &mut Output) {
        let Some(slot) = self.docs_open_slot() else {
            return;
        };
        if slot.view.base.is_none() || slot.view.is_trashed() || slot.view.gone {
            return;
        }
        if let Some(title) = title {
            slot.view.title = title;
        }
        if let Some(content) = content
            && slot
                .view
                .base
                .as_ref()
                .is_some_and(|base| base.content.same_kind(&content))
        {
            slot.view.content = Some(content);
        }
        if matches!(
            slot.view.save,
            SaveState::Saved | SaveState::Dirty | SaveState::Failed(_)
        ) {
            slot.view.save = if slot.view.has_edits() {
                SaveState::Dirty
            } else {
                SaveState::Saved
            };
        }
        out.changes.push(Change::OpenPageChanged);
    }

    fn docs_save(&mut self, page: PageId, out: &mut Output) {
        let Some(me) = self.docs_me() else { return };
        let since = self.docs.clock;
        let Some(slot) = self.docs.pages.get_mut(&page) else {
            return;
        };
        if slot.view.save == SaveState::Conflict {
            return;
        }
        if let Some(op) = slot.save.as_mut() {
            // One save at a time. An unsettled one is sent again as it was; otherwise the edits
            // made meanwhile go in the next save, started when this one is answered.
            if matches!(op.stage, SaveStage::Unsettled) {
                op.stage = SaveStage::Sending;
                slot.view.save = SaveState::Saving;
                let call = DocsCall::UpdatePage {
                    page,
                    request: op.request.clone(),
                };
                self.docs_send(Pending::Save { page, since }, call, out);
                out.changes.push(Change::OpenPageChanged);
            }
            return;
        }
        let Some(base) = slot.view.base.clone() else {
            return;
        };
        if base.summary.trashed_at.is_some() {
            slot.view.save =
                SaveState::Failed("Restore the page from the trash to edit it.".into());
            out.changes.push(Change::OpenPageChanged);
            return;
        }
        let title = match page::normalize_title(&slot.view.title) {
            Ok(title) => title,
            Err(error) => {
                slot.view.save = SaveState::Failed(error.to_string());
                out.changes.push(Change::OpenPageChanged);
                return;
            }
        };
        let new_title = (title != base.summary.title).then_some(title.clone());
        let new_content = slot
            .view
            .content
            .clone()
            .filter(|content| *content != base.content);
        if new_title.is_none() && new_content.is_none() {
            // Nothing but the spelling of the title differed.
            slot.view.title = base.summary.title.clone();
            slot.view.save = SaveState::Saved;
            out.changes.push(Change::OpenPageChanged);
            self.docs_collect(page);
            return;
        }
        if let Some(content) = &new_content
            && let Err(error) = content.validate()
        {
            slot.view.save = SaveState::Failed(error.to_string());
            out.changes.push(Change::OpenPageChanged);
            return;
        }
        let operation = OperationId::generate();
        let request = UpdatePageRequest {
            operation_id: operation,
            base_revision: base.summary.revision,
            title: new_title,
            content: new_content,
            append: None,
            conversation_id: None,
        };
        let submitted_at = Timestamp::now();
        slot.save = Some(SaveOp {
            operation,
            request: request.clone(),
            stage: SaveStage::Persisting,
            recovered: false,
            submitted_at,
        });
        slot.view.save = SaveState::Saving;
        self.docs.remember_op(operation);
        out.effects
            .push(Effect::Docs(DocsEffect::Persist(DocJournalEntry {
                operation_id: operation,
                user_id: me,
                submitted_at,
                action: DocJournalAction::Update { page, request },
            })));
        out.changes.push(Change::OpenPageChanged);
    }

    pub(super) fn docs_persisted(&mut self, operation: OperationId, out: &mut Output) {
        let since = self.docs.clock;
        if let Some((page, entry)) = self
            .docs
            .creates
            .iter_mut()
            .find(|(_, e)| e.operation == operation)
        {
            let page = *page;
            entry.stage = SaveStage::Sending;
            let call = DocsCall::CreatePage {
                conversation: entry.conversation,
                request: entry.request.clone(),
            };
            self.docs_send(Pending::Create { page, since }, call, out);
            return;
        }
        let found = self.docs.pages.iter_mut().find_map(|(page, slot)| {
            let op = slot.save.as_mut()?;
            (op.operation == operation).then(|| {
                op.stage = SaveStage::Sending;
                (
                    *page,
                    DocsCall::UpdatePage {
                        page: *page,
                        request: op.request.clone(),
                    },
                )
            })
        });
        if let Some((page, call)) = found {
            self.docs_send(Pending::Save { page, since }, call, out);
        }
    }

    pub(super) fn docs_persist_failed(
        &mut self,
        operation: OperationId,
        reason: String,
        out: &mut Output,
    ) {
        let message = format!("Couldn't keep a safe copy first ({reason}). Nothing was sent.");
        if let Some(entry) = self
            .docs
            .creates
            .values_mut()
            .find(|e| e.operation == operation)
        {
            entry.refused = true;
            entry.state = CreateState::Failed(message);
            out.changes.push(Change::DocsStateChanged);
            return;
        }
        let page = self.docs.pages.iter().find_map(|(page, slot)| {
            slot.save
                .as_ref()
                .is_some_and(|op| op.operation == operation)
                .then_some(*page)
        });
        if let Some(page) = page
            && let Some(slot) = self.docs.pages.get_mut(&page)
        {
            slot.save = None;
            slot.view.save = SaveState::Failed(message);
            out.changes.push(Change::OpenPageChanged);
        }
    }

    fn docs_resolve(&mut self, choice: ConflictChoice, out: &mut Output) {
        let Some(page) = self.docs.open else { return };
        let Some(slot) = self.docs.pages.get_mut(&page) else {
            return;
        };
        let Some(theirs) = slot.view.conflict.take() else {
            return;
        };
        slot.view.base = Some(theirs.clone());
        slot.view.newer_revision = None;
        match choice {
            ConflictChoice::TakeTheirs => {
                slot.view.title = theirs.summary.title.clone();
                slot.view.content = Some(theirs.content);
                slot.view.save = SaveState::Saved;
            }
            ConflictChoice::KeepMine => {
                slot.view.save = if slot.view.has_edits() {
                    SaveState::Dirty
                } else {
                    SaveState::Saved
                };
            }
        }
        let dirty = slot.view.save == SaveState::Dirty;
        out.changes.push(Change::OpenPageChanged);
        if dirty {
            self.docs_save(page, out);
        }
    }

    // ----- Trash, restore, purge, move -----

    fn docs_change_state(&mut self, page: PageId, change: Change2, out: &mut Output) {
        let request = PageActionRequest {
            operation_id: self.docs_op(),
        };
        let since = self.docs.clock;
        let call = match change {
            Change2::Trash => DocsCall::TrashPage { page, request },
            Change2::Restore => DocsCall::RestorePage { page, request },
            Change2::Purge => DocsCall::PurgePage { page, request },
        };
        self.docs_send(Pending::Change { page, since }, call, out);
    }

    fn docs_move(&mut self, page: PageId, conversation: ConversationId, out: &mut Output) {
        let Some(revision) = self.docs_known_summary(page).map(|s| s.revision) else {
            return;
        };
        let request = UpdatePageRequest {
            operation_id: self.docs_op(),
            base_revision: revision,
            title: None,
            content: None,
            append: None,
            conversation_id: Some(conversation),
        };
        let since = self.docs.clock;
        self.docs_send(
            Pending::Change { page, since },
            DocsCall::UpdatePage { page, request },
            out,
        );
    }

    /// The best-known summary of a page, from the open page, a list, or the search.
    fn docs_known_summary(&self, page: PageId) -> Option<&PageSummary> {
        if let Some(base) = self
            .docs
            .pages
            .get(&page)
            .and_then(|s| s.view.base.as_ref())
        {
            return Some(&base.summary);
        }
        self.docs
            .lists
            .values()
            .flat_map(|s| s.list.pages.iter())
            .chain(self.docs.search.iter().flat_map(|s| s.view.results.iter()))
            .find(|p| p.id == page)
    }

    // ----- Versions -----

    fn docs_load_versions(&mut self, out: &mut Output) {
        let Some(page) = self.docs.open else { return };
        if let Some(slot) = self.docs.pages.get_mut(&page) {
            slot.view.versions.load = LoadStatus::Loading;
        }
        self.docs_send(
            Pending::Versions { page },
            DocsCall::ListVersions(page),
            out,
        );
        out.changes.push(Change::OpenPageChanged);
    }

    fn docs_preview_version(&mut self, version: u32, out: &mut Output) {
        let Some(page) = self.docs.open else { return };
        if let Some(slot) = self.docs.pages.get_mut(&page) {
            slot.view.versions.preview_load = LoadStatus::Loading;
        }
        self.docs_send(
            Pending::Version { page },
            DocsCall::GetVersion { page, version },
            out,
        );
        out.changes.push(Change::OpenPageChanged);
    }

    fn docs_restore_version(&mut self, version: u32, out: &mut Output) {
        let Some(page) = self.docs.open else { return };
        let Some(slot) = self.docs.pages.get_mut(&page) else {
            return;
        };
        let Some(base) = slot.view.base.as_ref() else {
            return;
        };
        if slot.view.has_edits() || slot.save.is_some() {
            let notice = "Save or discard your edits before restoring a version.".to_owned();
            self.docs_notice_set(Some(notice), out);
            return;
        }
        let request = RestoreVersionRequest {
            operation_id: OperationId::generate(),
            base_revision: base.summary.revision,
        };
        slot.view.versions.restoring = true;
        self.docs.remember_op(request.operation_id);
        let since = self.docs.clock;
        self.docs_send(
            Pending::RestoreVersion { page, since },
            DocsCall::RestoreVersion {
                page,
                version,
                request,
            },
            out,
        );
        out.changes.push(Change::OpenPageChanged);
    }

    // ----- Notations -----

    #[allow(clippy::too_many_arguments)]
    fn docs_create_notation(
        &mut self,
        notation: NotationId,
        quote: String,
        from: u32,
        to: u32,
        body: String,
        assignee: Option<UserId>,
        out: &mut Output,
    ) {
        let Some(page) = self.docs.open else { return };
        let body_chars = match self.open_page().and_then(|p| p.content.as_ref()) {
            Some(PageContent::Page { body }) => body.chars().count(),
            _ => {
                let notice = "Notes can be added to pages, not boards.".to_owned();
                return self.docs_notice_set(Some(notice), out);
            }
        };
        if let Err(error) = page::validate_notation_body(&body)
            .and_then(|()| page::validate_anchor(&quote, from, to, body_chars))
        {
            return self.docs_notice_set(Some(error.to_string()), out);
        }
        let request = CreateNotationRequest {
            operation_id: self.docs_op(),
            notation_id: notation,
            quote,
            from,
            to,
            body,
            assignee_id: assignee,
        };
        self.docs_send(
            Pending::NotationChange { page: Some(page) },
            DocsCall::CreateNotation { page, request },
            out,
        );
    }

    // ----- Answers -----

    pub(super) fn docs_reply(
        &mut self,
        id: u64,
        result: Result<DocsAnswer, RequestError>,
        out: &mut Output,
    ) {
        let Some(mut request) = self.docs.pending.remove(&id) else {
            return;
        };
        if let Err(error) = &result {
            if error.is_unauthenticated() {
                return self.session_expired(out);
            }
            if matches!(error, RequestError::Unknown(_)) {
                match request.what {
                    // Ask whether the page exists before anything is sent again.
                    Pending::Create { page, since } if request.resends < MAX_DOC_RESENDS => {
                        let create = Box::new(request.call);
                        return self.docs_send_request(
                            Request {
                                what: Pending::CreateCheck {
                                    page,
                                    since,
                                    create,
                                },
                                call: DocsCall::GetPage(page),
                                resends: request.resends + 1,
                            },
                            out,
                        );
                    }
                    Pending::CreateCheck {
                        page,
                        since,
                        create,
                    } => {
                        let request = if request.resends < MAX_DOC_RESENDS {
                            Request {
                                what: Pending::CreateCheck {
                                    page,
                                    since,
                                    create,
                                },
                                call: request.call,
                                resends: request.resends + 1,
                            }
                        } else {
                            Request {
                                what: Pending::Create { page, since },
                                call: *create,
                                resends: request.resends,
                            }
                        };
                        return if matches!(request.what, Pending::CreateCheck { .. }) {
                            self.docs_send_request(request, out)
                        } else {
                            self.docs_stall(request, out)
                        };
                    }
                    _ => {}
                }
                if request.call.is_mutation() {
                    if request.resends < MAX_DOC_RESENDS {
                        request.resends += 1;
                        return self.docs_send_request(request, out);
                    }
                    return self.docs_stall(request, out);
                }
            }
        } else {
            self.docs_flush_stalled(out);
        }
        match request.what {
            Pending::List {
                key,
                generation,
                offset,
                since,
            } => self.docs_list_answer(key, generation, offset, since, result, out),
            Pending::Search {
                generation,
                offset,
                since,
            } => self.docs_search_answer(generation, offset, since, result, out),
            Pending::Inbox { generation } => self.docs_inbox_answer(generation, result, out),
            Pending::Open { page } => self.docs_open_answer(page, result, out),
            Pending::Refresh { page, purpose } => {
                self.docs_refresh_answer(page, purpose, result, out)
            }
            Pending::Create { page, since } => self.docs_create_answer(page, since, result, out),
            Pending::CreateCheck {
                page,
                since,
                create,
            } => {
                if matches!(&result, Err(error) if is_code(error, ErrorCode::NotFound)) {
                    // Not there: the create never committed. Send it again, as the same operation.
                    let request = Request {
                        what: Pending::Create { page, since },
                        call: *create,
                        resends: request.resends,
                    };
                    return self.docs_send_request(request, out);
                }
                self.docs_create_answer(page, since, result, out)
            }
            Pending::Save { page, since } => self.docs_save_answer(page, since, result, out),
            Pending::Change { page, since } => self.docs_change_answer(page, since, result, out),
            Pending::Versions { page } => self.docs_versions_answer(page, result, out),
            Pending::Version { page } => self.docs_version_answer(page, result, out),
            Pending::RestoreVersion { page, since } => {
                self.docs_restore_answer(page, since, result, out)
            }
            Pending::Notations { page } => self.docs_notations_answer(page, result, out),
            Pending::NotationChange { page } => self.docs_notation_change_answer(page, result, out),
            Pending::File { page } => self.docs_file_answer(page, result, out),
        }
    }

    /// A mutation went unanswered too often. Creates and saves wait for the person's retry;
    /// others wait for the server to answer anything.
    fn docs_stall(&mut self, request: Request, out: &mut Output) {
        match request.what {
            Pending::Create { page, .. } => {
                if let Some(entry) = self.docs.creates.get_mut(&page) {
                    entry.stage = SaveStage::Unsettled;
                    entry.state = CreateState::Failed(
                        "Couldn't reach the server. Try again; nothing is created twice.".into(),
                    );
                }
                out.changes.push(Change::DocsStateChanged);
            }
            Pending::Save { page, .. } => {
                if let Some(slot) = self.docs.pages.get_mut(&page) {
                    if let Some(op) = slot.save.as_mut() {
                        op.stage = SaveStage::Unsettled;
                    }
                    slot.view.save = SaveState::Failed(
                        "Couldn't reach the server. Your edits are kept; try saving again.".into(),
                    );
                }
                out.changes.push(Change::OpenPageChanged);
            }
            _ => {
                self.docs.stalled.push(request);
                let notice =
                    "Couldn't reach the server. A change is waiting to be sent.".to_owned();
                self.docs_notice_set(Some(notice), out);
            }
        }
    }

    fn docs_flush_stalled(&mut self, out: &mut Output) {
        for mut request in std::mem::take(&mut self.docs.stalled) {
            request.resends = 0;
            self.docs_send_request(request, out);
        }
    }

    fn docs_list_answer(
        &mut self,
        key: ListKey,
        generation: u64,
        offset: usize,
        since: u64,
        result: Result<DocsAnswer, RequestError>,
        out: &mut Output,
    ) {
        match result {
            Ok(DocsAnswer::Pages(response)) => {
                let fresh: Vec<PageSummary> = response
                    .pages
                    .into_iter()
                    .filter(|p| self.docs.epochs.get(&p.id).is_none_or(|&at| at <= since))
                    .collect();
                let Some(slot) = self.docs.lists.get_mut(&key) else {
                    return;
                };
                if slot.generation != generation {
                    return;
                }
                if offset == 0 {
                    // Pages an event added meanwhile are not lost: they are newer than the answer.
                    let known: HashSet<PageId> = fresh.iter().map(|p| p.id).collect();
                    let kept: Vec<PageSummary> = slot
                        .list
                        .pages
                        .iter()
                        .filter(|p| {
                            !known.contains(&p.id)
                                && self.docs.epochs.get(&p.id).is_some_and(|&at| at > since)
                        })
                        .cloned()
                        .collect();
                    slot.list.pages = fresh;
                    slot.list.pages.extend(kept);
                } else {
                    for page in fresh {
                        if !slot.list.pages.iter().any(|p| p.id == page.id) {
                            slot.list.pages.push(page);
                        }
                    }
                }
                newest_first(&mut slot.list.pages);
                slot.list.has_more = response.has_more;
                slot.list.load = LoadStatus::Ready;
            }
            Ok(_) => return,
            Err(error) => {
                let Some(slot) = self.docs.lists.get_mut(&key) else {
                    return;
                };
                if slot.generation != generation {
                    return;
                }
                slot.list.load = failed(&error);
            }
        }
        out.changes.push(key.change());
    }

    fn docs_search_answer(
        &mut self,
        generation: u64,
        offset: usize,
        since: u64,
        result: Result<DocsAnswer, RequestError>,
        out: &mut Output,
    ) {
        let Some(slot) = self.docs.search.as_mut() else {
            return;
        };
        if slot.generation != generation {
            return;
        }
        match result {
            Ok(DocsAnswer::Pages(response)) => {
                let epochs = &self.docs.epochs;
                let pages = response
                    .pages
                    .into_iter()
                    .filter(|p| epochs.get(&p.id).is_none_or(|&at| at <= since));
                if offset == 0 {
                    slot.view.results = pages.collect();
                } else {
                    for page in pages {
                        if !slot.view.results.iter().any(|p| p.id == page.id) {
                            slot.view.results.push(page);
                        }
                    }
                }
                slot.view.has_more = response.has_more;
                slot.view.load = LoadStatus::Ready;
            }
            Ok(_) => return,
            Err(error) => slot.view.load = failed(&error),
        }
        out.changes.push(Change::PageSearchChanged);
    }

    fn docs_inbox_answer(
        &mut self,
        generation: u64,
        result: Result<DocsAnswer, RequestError>,
        out: &mut Output,
    ) {
        let Some(slot) = self.docs.inbox.as_mut() else {
            return;
        };
        if slot.generation != generation {
            return;
        }
        match result {
            Ok(DocsAnswer::Notations(response)) => {
                slot.view.notations = response.notations;
                sort_notations_newest(&mut slot.view.notations);
                slot.view.load = LoadStatus::Ready;
            }
            Ok(_) => return,
            Err(error) => slot.view.load = failed(&error),
        }
        out.changes.push(Change::NotationInboxChanged);
    }

    fn docs_open_answer(
        &mut self,
        page: PageId,
        result: Result<DocsAnswer, RequestError>,
        out: &mut Output,
    ) {
        let Some(slot) = self.docs.pages.get_mut(&page) else {
            return;
        };
        match result {
            Ok(DocsAnswer::Page(response)) => {
                let detail = response.page;
                let held = slot.view.base.as_ref().map_or(0, |b| b.summary.revision);
                if detail.summary.revision < held {
                    // An older answer than what is held (a save finished meanwhile).
                    return;
                }
                let had_edits = slot.view.has_edits();
                let summary = detail.summary.clone();
                slot.view.load = LoadStatus::Ready;
                slot.view.gone = false;
                slot.view.base = Some(detail.clone());
                if !had_edits && slot.view.save != SaveState::Saving {
                    slot.view.title = detail.summary.title.clone();
                    slot.view.content = Some(detail.content.clone());
                    slot.view.save = SaveState::Saved;
                }
                let kind = detail.content.kind();
                let since = self.docs.clock;
                self.docs_upsert(summary, Source::Response { since }, out);
                if kind == PageKind::Page && self.docs.open == Some(page) {
                    if let Some(slot) = self.docs.pages.get_mut(&page) {
                        slot.view.notations_load = LoadStatus::Loading;
                    }
                    self.docs_send(
                        Pending::Notations { page },
                        DocsCall::ListNotations(page),
                        out,
                    );
                }
            }
            Ok(_) => return,
            Err(error) => {
                if is_code(&error, ErrorCode::NotFound) {
                    slot.view.gone = true;
                    slot.view.load = LoadStatus::Failed {
                        reason: "This page was removed, or you can't see it any more.".into(),
                        can_retry: false,
                    };
                } else if slot.view.base.is_none() {
                    slot.view.load = failed(&error);
                }
            }
        }
        out.changes.push(Change::OpenPageChanged);
    }

    fn docs_refresh_answer(
        &mut self,
        page: PageId,
        purpose: Refresh,
        result: Result<DocsAnswer, RequestError>,
        out: &mut Output,
    ) {
        let Some(slot) = self.docs.pages.get_mut(&page) else {
            return;
        };
        match (result, purpose) {
            (Ok(DocsAnswer::Page(response)), Refresh::Conflict) => {
                slot.view.conflict = Some(response.page);
                slot.view.save = SaveState::Conflict;
            }
            (Ok(DocsAnswer::Page(response)), Refresh::Reload) => {
                let detail = response.page;
                let held = slot.view.base.as_ref().map_or(0, |b| b.summary.revision);
                if detail.summary.revision < held {
                    return;
                }
                if detail.summary.revision == held {
                    // A file was added or removed (that changes no revision): take the files and
                    // counts, leave the text and the working copy alone.
                    if slot.view.base.as_ref() == Some(&detail) {
                        return;
                    }
                    slot.view.base = Some(detail);
                    slot.view.newer_revision = None;
                    out.changes.push(Change::OpenPageChanged);
                    return;
                }
                let had_edits = slot.view.has_edits();
                slot.view.base = Some(detail.clone());
                slot.view.newer_revision = None;
                if !had_edits && slot.save.is_none() {
                    slot.view.title = detail.summary.title.clone();
                    slot.view.content = Some(detail.content);
                    slot.view.save = SaveState::Saved;
                }
            }
            (Ok(_), _) => return,
            (Err(error), Refresh::Conflict) => {
                slot.view.save = SaveState::Failed(format!(
                    "Someone else changed this page, and the new version couldn't be loaded. {}",
                    error.message()
                ));
            }
            (Err(_), Refresh::Reload) => return,
        }
        out.changes.push(Change::OpenPageChanged);
    }

    fn docs_create_answer(
        &mut self,
        page: PageId,
        since: u64,
        result: Result<DocsAnswer, RequestError>,
        out: &mut Output,
    ) {
        let Some(entry) = self.docs.creates.get(&page) else {
            return;
        };
        let operation = entry.operation;
        match result {
            Ok(DocsAnswer::Page(response)) => {
                self.docs.creates.remove(&page);
                out.effects
                    .push(Effect::Docs(DocsEffect::Settle { operation }));
                let detail = response.page;
                self.docs_upsert(detail.summary.clone(), Source::Response { since }, out);
                self.docs.fresh = Some(detail);
                out.changes.push(Change::PageCreated { page });
            }
            Ok(_) => {}
            Err(error) => {
                out.effects
                    .push(Effect::Docs(DocsEffect::Settle { operation }));
                if let Some(entry) = self.docs.creates.get_mut(&page) {
                    entry.refused = true;
                    entry.state = CreateState::Failed(error.message());
                }
                out.changes.push(Change::DocsStateChanged);
            }
        }
    }

    fn docs_save_answer(
        &mut self,
        page: PageId,
        since: u64,
        result: Result<DocsAnswer, RequestError>,
        out: &mut Output,
    ) {
        let Some(slot) = self.docs.pages.get_mut(&page) else {
            return;
        };
        let Some(op) = slot.save.take() else {
            return;
        };
        let operation = op.operation;
        let recovered = op.recovered;
        let sent_title = op.request.title.clone();
        let sent_content = op.request.content.clone();
        let (submitted_at, request) = (op.submitted_at, op.request);
        match result {
            Ok(DocsAnswer::Page(response)) => {
                out.effects
                    .push(Effect::Docs(DocsEffect::Settle { operation }));
                let detail = response.page;
                // A repeated request is answered with the page as it is now, not as the first
                // attempt left it. If what we sent is not what the page holds, someone changed it
                // after our change committed (or ours never did): the person decides.
                let diverged = sent_title
                    .as_ref()
                    .is_some_and(|t| *t != detail.summary.title)
                    || sent_content.as_ref().is_some_and(|c| *c != detail.content);
                if diverged && slot.view.base.is_some() {
                    slot.view.conflict = Some(detail);
                    slot.view.save = SaveState::Conflict;
                    out.changes.push(Change::OpenPageChanged);
                    return;
                }
                // What was typed since stays; a field nobody typed in follows the page.
                let old_base = slot.view.base.clone();
                let normalized = page::normalize_title(&slot.view.title)
                    .unwrap_or_else(|_| slot.view.title.clone());
                let reference_title = sent_title
                    .clone()
                    .or_else(|| old_base.as_ref().map(|b| b.summary.title.clone()));
                if reference_title.is_none_or(|t| t == normalized) {
                    slot.view.title = detail.summary.title.clone();
                }
                let reference_content = sent_content
                    .clone()
                    .or_else(|| old_base.as_ref().map(|b| b.content.clone()));
                if slot.view.content.is_none() || slot.view.content == reference_content {
                    slot.view.content = Some(detail.content.clone());
                }
                if slot
                    .view
                    .base
                    .as_ref()
                    .is_none_or(|b| b.summary.revision <= detail.summary.revision)
                {
                    slot.view.base = Some(detail.clone());
                }
                slot.view.conflict = None;
                let again = slot.view.has_edits();
                slot.view.save = if again {
                    SaveState::Dirty
                } else {
                    SaveState::Saved
                };
                self.docs_upsert(detail.summary, Source::Response { since }, out);
                out.changes.push(Change::OpenPageChanged);
                // Edits made while the save was out go in the next one, open page or not.
                if again {
                    self.docs_save(page, out);
                } else {
                    self.docs_collect(page);
                }
            }
            Ok(_) => {}
            Err(error) => {
                out.effects
                    .push(Effect::Docs(DocsEffect::Settle { operation }));
                if is_code(&error, ErrorCode::StaleRevision) {
                    if recovered || slot.view.base.is_none() {
                        self.docs.recovered_edits.push(RecoveredEdit {
                            operation,
                            page,
                            title: request.title,
                            content: request.content,
                            submitted_at,
                        });
                        self.docs.pages.remove(&page);
                        let notice = "A change you made before closing Lazy was out of date. Your text is kept.".to_owned();
                        self.docs_notice_set(Some(notice), out);
                    } else {
                        slot.view.save = SaveState::Saving;
                        self.docs_send(
                            Pending::Refresh {
                                page,
                                purpose: Refresh::Conflict,
                            },
                            DocsCall::GetPage(page),
                            out,
                        );
                    }
                } else if is_code(&error, ErrorCode::NotFound) {
                    slot.view.gone = true;
                    slot.view.save = SaveState::Failed("This page was removed.".into());
                } else {
                    slot.view.save = SaveState::Failed(error.message());
                }
                out.changes.push(Change::OpenPageChanged);
            }
        }
    }

    fn docs_change_answer(
        &mut self,
        page: PageId,
        since: u64,
        result: Result<DocsAnswer, RequestError>,
        out: &mut Output,
    ) {
        match result {
            Ok(DocsAnswer::Page(response)) => {
                let detail = response.page;
                self.docs_upsert(detail.summary.clone(), Source::Response { since }, out);
                let events_since = self.docs.epochs.get(&page).is_some_and(|&at| at > since);
                if let Some(slot) = self.docs.pages.get_mut(&page)
                    && !events_since
                {
                    let had_edits = slot.view.has_edits();
                    slot.view.base = Some(detail.clone());
                    if !had_edits {
                        slot.view.title = detail.summary.title;
                        slot.view.content = Some(detail.content);
                    }
                    out.changes.push(Change::OpenPageChanged);
                }
            }
            Ok(DocsAnswer::Done) => {
                // A purge: the page is gone.
                self.docs_forget_page(page, out);
            }
            Ok(_) => {}
            Err(error) => {
                if is_code(&error, ErrorCode::StaleRevision) {
                    let notice = "The page changed while you were moving it. Try again.".to_owned();
                    self.docs_notice_set(Some(notice), out);
                } else {
                    self.docs_notice_set(Some(error.message()), out);
                }
            }
        }
    }

    fn docs_versions_answer(
        &mut self,
        page: PageId,
        result: Result<DocsAnswer, RequestError>,
        out: &mut Output,
    ) {
        let Some(slot) = self.docs.pages.get_mut(&page) else {
            return;
        };
        match result {
            Ok(DocsAnswer::Versions(response)) => {
                slot.view.versions.versions = response.versions;
                slot.view.versions.load = LoadStatus::Ready;
            }
            Ok(_) => return,
            Err(error) => slot.view.versions.load = failed(&error),
        }
        out.changes.push(Change::OpenPageChanged);
    }

    fn docs_version_answer(
        &mut self,
        page: PageId,
        result: Result<DocsAnswer, RequestError>,
        out: &mut Output,
    ) {
        let Some(slot) = self.docs.pages.get_mut(&page) else {
            return;
        };
        match result {
            Ok(DocsAnswer::Version(response)) => {
                slot.view.versions.preview = Some(response.version);
                slot.view.versions.preview_load = LoadStatus::Ready;
            }
            Ok(_) => return,
            Err(error) => slot.view.versions.preview_load = failed(&error),
        }
        out.changes.push(Change::OpenPageChanged);
    }

    fn docs_restore_answer(
        &mut self,
        page: PageId,
        since: u64,
        result: Result<DocsAnswer, RequestError>,
        out: &mut Output,
    ) {
        if let Some(slot) = self.docs.pages.get_mut(&page) {
            slot.view.versions.restoring = false;
        }
        match result {
            Ok(DocsAnswer::Page(response)) => {
                let detail = response.page;
                self.docs_upsert(detail.summary.clone(), Source::Response { since }, out);
                if let Some(slot) = self.docs.pages.get_mut(&page) {
                    slot.view.title = detail.summary.title.clone();
                    slot.view.content = Some(detail.content.clone());
                    slot.view.base = Some(detail);
                    slot.view.save = SaveState::Saved;
                    slot.view.conflict = None;
                    slot.view.versions.preview = None;
                    slot.view.versions.preview_load = LoadStatus::Idle;
                }
                // The restore added a version of the state it replaced.
                if self.docs.open == Some(page) {
                    self.docs_send(
                        Pending::Versions { page },
                        DocsCall::ListVersions(page),
                        out,
                    );
                }
            }
            Ok(_) => {}
            Err(error) => {
                let notice = if is_code(&error, ErrorCode::StaleRevision) {
                    "The page changed first. Reload it and try again.".to_owned()
                } else {
                    error.message()
                };
                self.docs_notice_set(Some(notice), out);
            }
        }
        out.changes.push(Change::OpenPageChanged);
    }

    fn docs_notations_answer(
        &mut self,
        page: PageId,
        result: Result<DocsAnswer, RequestError>,
        out: &mut Output,
    ) {
        let Some(slot) = self.docs.pages.get_mut(&page) else {
            return;
        };
        match result {
            Ok(DocsAnswer::Notations(response)) => {
                // A notation an event brought meanwhile is kept if it is newer than the answer's.
                let mut merged = response.notations;
                for held in std::mem::take(&mut slot.view.notations) {
                    match merged.iter().position(|n| n.id == held.id) {
                        Some(i) if merged[i].updated_at < held.updated_at => merged[i] = held,
                        Some(_) => {}
                        None => {}
                    }
                }
                sort_notations_oldest(&mut merged);
                slot.view.notations = merged;
                slot.view.notations_load = LoadStatus::Ready;
            }
            Ok(_) => return,
            Err(error) => slot.view.notations_load = failed(&error),
        }
        out.changes.push(Change::OpenPageChanged);
    }

    fn docs_notation_change_answer(
        &mut self,
        page: Option<PageId>,
        result: Result<DocsAnswer, RequestError>,
        out: &mut Output,
    ) {
        match result {
            Ok(DocsAnswer::Notation(response)) => {
                self.docs_upsert_notation(response.notation, None, out);
            }
            Ok(DocsAnswer::Done) => {
                // A delete: the event says what is gone; reload the page's list to be sure.
                if let Some(page) = page
                    && self.docs.open == Some(page)
                {
                    self.docs_send(
                        Pending::Notations { page },
                        DocsCall::ListNotations(page),
                        out,
                    );
                }
            }
            Ok(_) => {}
            Err(error) => self.docs_notice_set(Some(error.message()), out),
        }
    }

    fn docs_file_answer(
        &mut self,
        page: PageId,
        result: Result<DocsAnswer, RequestError>,
        out: &mut Output,
    ) {
        match result {
            Ok(DocsAnswer::File(file)) => {
                if let Some(base) = self
                    .docs
                    .pages
                    .get_mut(&page)
                    .and_then(|slot| slot.view.base.as_mut())
                    && !base.files.iter().any(|f| f.id == file.id)
                {
                    base.files.push(file);
                    base.summary.file_count = base.files.len() as u32;
                    out.changes.push(Change::OpenPageChanged);
                }
            }
            Ok(_) => {}
            Err(error) => self.docs_notice_set(Some(error.message()), out),
        }
    }

    // ----- Summaries -----

    /// Applies a page summary to every list, the search, and the open page.
    fn docs_upsert(&mut self, summary: PageSummary, source: Source, out: &mut Output) {
        let id = summary.id;
        let epoch = self.docs.epochs.get(&id).copied().unwrap_or(0);
        let held_revision = self.docs_known_summary(id).map(|s| s.revision);
        let accept = match (held_revision, source) {
            (Some(held), _) if summary.revision > held => true,
            (Some(held), Source::Event) => summary.revision == held,
            (Some(held), Source::Response { since }) => summary.revision == held && epoch <= since,
            (None, Source::Event) => true,
            (None, Source::Response { since }) => epoch <= since,
        };
        if !accept {
            return;
        }
        let trashed = summary.trashed_at.is_some();
        let keys: Vec<ListKey> = self.docs.lists.keys().copied().collect();
        for key in keys {
            let belongs = key.holds(&summary);
            let Some(slot) = self.docs.lists.get_mut(&key) else {
                continue;
            };
            let position = slot.list.pages.iter().position(|p| p.id == id);
            let changed = match (position, belongs) {
                (Some(i), true) => {
                    slot.list.pages[i] = summary.clone();
                    true
                }
                (Some(i), false) => {
                    slot.list.pages.remove(i);
                    true
                }
                (None, true) if slot.list.load != LoadStatus::Idle => {
                    slot.list.pages.push(summary.clone());
                    true
                }
                _ => false,
            };
            if changed {
                newest_first(&mut slot.list.pages);
                out.changes.push(key.change());
            }
        }
        if let Some(search) = self.docs.search.as_mut()
            && let Some(i) = search.view.results.iter().position(|p| p.id == id)
        {
            if trashed {
                search.view.results.remove(i);
            } else {
                search.view.results[i] = summary.clone();
            }
            out.changes.push(Change::PageSearchChanged);
        }
    }

    /// What an open or retained page does when a summary of it arrives from the server.
    fn docs_slot_saw(
        &mut self,
        summary: &PageSummary,
        operation: Option<OperationId>,
        out: &mut Output,
    ) {
        let own = operation.is_some_and(|op| self.docs.my_ops.contains(&op));
        let id = summary.id;
        let Some(slot) = self.docs.pages.get_mut(&id) else {
            return;
        };
        let Some(base) = slot.view.base.as_mut() else {
            return;
        };
        if summary.revision > base.summary.revision {
            if own {
                return;
            }
            slot.view.newer_revision = Some(summary.revision);
            let reload = !slot.view.has_edits() && slot.save.is_none();
            if reload {
                self.docs_send(
                    Pending::Refresh {
                        page: id,
                        purpose: Refresh::Reload,
                    },
                    DocsCall::GetPage(id),
                    out,
                );
            }
        } else if summary.revision == base.summary.revision {
            // A trash, a count, or a file: no new revision, but the page differs.
            let files_changed = !own && summary.file_count as usize != base.files.len();
            base.summary = summary.clone();
            if files_changed {
                self.docs_send(
                    Pending::Refresh {
                        page: id,
                        purpose: Refresh::Reload,
                    },
                    DocsCall::GetPage(id),
                    out,
                );
            }
        }
        out.changes.push(Change::OpenPageChanged);
    }

    fn docs_forget_page(&mut self, page: PageId, out: &mut Output) {
        for (key, slot) in self.docs.lists.iter_mut() {
            let before = slot.list.pages.len();
            slot.list.pages.retain(|p| p.id != page);
            if slot.list.pages.len() != before {
                out.changes.push(key.change());
            }
        }
        if let Some(search) = self.docs.search.as_mut() {
            let before = search.view.results.len();
            search.view.results.retain(|p| p.id != page);
            if search.view.results.len() != before {
                out.changes.push(Change::PageSearchChanged);
            }
        }
        if let Some(slot) = self.docs.pages.get_mut(&page) {
            slot.view.gone = true;
            slot.save = None;
            out.changes.push(Change::OpenPageChanged);
        }
        if self.docs.open != Some(page) {
            self.docs.pages.remove(&page);
        }
    }

    fn docs_set_open_notations(&mut self, page: PageId, count: u32, out: &mut Output) {
        for (key, slot) in self.docs.lists.iter_mut() {
            if let Some(p) = slot.list.pages.iter_mut().find(|p| p.id == page)
                && p.open_notations != count
            {
                p.open_notations = count;
                out.changes.push(key.change());
            }
        }
        if let Some(slot) = self.docs.pages.get_mut(&page)
            && let Some(base) = slot.view.base.as_mut()
            && base.summary.open_notations != count
        {
            base.summary.open_notations = count;
            out.changes.push(Change::OpenPageChanged);
        }
    }

    // ----- Notations -----

    fn docs_upsert_notation(&mut self, notation: Notation, open: Option<u32>, out: &mut Output) {
        let page = notation.page_id;
        if let Some(slot) = self.docs.pages.get_mut(&page) {
            let notations = &mut slot.view.notations;
            match notations.iter().position(|n| n.id == notation.id) {
                Some(i) if notations[i].updated_at > notation.updated_at => {}
                Some(i) => {
                    notations[i] = notation.clone();
                    out.changes.push(Change::OpenPageChanged);
                }
                None if slot.view.notations_load != LoadStatus::Idle => {
                    notations.push(notation.clone());
                    sort_notations_oldest(notations);
                    out.changes.push(Change::OpenPageChanged);
                }
                None => {}
            }
        }
        self.docs_inbox_apply(&notation, out);
        if let Some(count) = open {
            self.docs_set_open_notations(page, count, out);
        }
    }

    /// Keeps the inbox right: a notation belongs there if it is assigned to (or written by) the
    /// person, in this workspace, and its completed state matches the view.
    fn docs_inbox_apply(&mut self, notation: &Notation, out: &mut Output) {
        let Some(me) = self.docs_me() else { return };
        let Some(workspace) = self.docs.inbox.as_ref().map(|i| i.view.workspace) else {
            return;
        };
        let in_workspace = self
            .scope_conversations(workspace)
            .iter()
            .any(|c| c.id == notation.conversation_id);
        let Some(inbox) = self.docs.inbox.as_mut() else {
            return;
        };
        let mine = match inbox.view.scope {
            NotationScope::Assigned => notation.assignee_id == me,
            NotationScope::Created => notation.author_id == me,
        };
        let belongs =
            in_workspace && mine && notation.completed_at.is_some() == inbox.view.completed;
        let list = &mut inbox.view.notations;
        let position = list.iter().position(|n| n.id == notation.id);
        let changed = match (position, belongs) {
            (Some(i), true) if list[i].updated_at > notation.updated_at => false,
            (Some(i), true) => {
                list[i] = notation.clone();
                true
            }
            (Some(i), false) => {
                list.remove(i);
                true
            }
            (None, true) if inbox.view.load != LoadStatus::Idle => {
                list.push(notation.clone());
                true
            }
            _ => false,
        };
        if changed {
            sort_notations_newest(&mut inbox.view.notations);
            out.changes.push(Change::NotationInboxChanged);
        }
    }

    // ----- Events -----

    pub(super) fn docs_page_changed(
        &mut self,
        page: PageSummary,
        operation: Option<OperationId>,
        out: &mut Output,
    ) {
        self.docs.clock += 1;
        let clock = self.docs.clock;
        self.docs.epochs.insert(page.id, clock);
        self.docs_upsert(page.clone(), Source::Event, out);
        self.docs_slot_saw(&page, operation, out);
    }

    pub(super) fn docs_page_removed(&mut self, page: PageId, out: &mut Output) {
        self.docs.clock += 1;
        let clock = self.docs.clock;
        self.docs.epochs.insert(page, clock);
        self.docs_forget_page(page, out);
    }

    pub(super) fn docs_notation_changed(
        &mut self,
        notation: Notation,
        open_notations: u32,
        out: &mut Output,
    ) {
        self.docs_upsert_notation(notation, Some(open_notations), out);
    }

    pub(super) fn docs_notation_removed(
        &mut self,
        page: PageId,
        notation: NotationId,
        open_notations: u32,
        out: &mut Output,
    ) {
        if let Some(slot) = self.docs.pages.get_mut(&page) {
            let before = slot.view.notations.len();
            slot.view.notations.retain(|n| n.id != notation);
            if slot.view.notations.len() != before {
                out.changes.push(Change::OpenPageChanged);
            }
        }
        if let Some(inbox) = self.docs.inbox.as_mut() {
            let before = inbox.view.notations.len();
            inbox.view.notations.retain(|n| n.id != notation);
            if inbox.view.notations.len() != before {
                out.changes.push(Change::NotationInboxChanged);
            }
        }
        self.docs_set_open_notations(page, open_notations, out);
    }

    /// The person lost a conversation: its pages go with it.
    pub(super) fn docs_conversation_lost(
        &mut self,
        conversation: ConversationId,
        out: &mut Output,
    ) {
        let keys: Vec<_> = self
            .docs
            .lists
            .keys()
            .filter(|key| matches!(key, ListKey::Conversation(c, _) if *c == conversation))
            .copied()
            .collect();
        let mut gone: Vec<PageId> = Vec::new();
        for key in keys {
            if let Some(slot) = self.docs.lists.remove(&key) {
                gone.extend(slot.list.pages.iter().map(|p| p.id));
            }
            out.changes.push(Change::PageListChanged { conversation });
        }
        // The workspace lists hold them too; `docs_forget_page` takes them out.
        for (key, slot) in &self.docs.lists {
            if matches!(key, ListKey::Workspace(..)) {
                gone.extend(
                    slot.list
                        .pages
                        .iter()
                        .filter(|p| p.conversation_id == conversation)
                        .map(|p| p.id),
                );
            }
        }
        for (id, slot) in &self.docs.pages {
            if slot
                .view
                .base
                .as_ref()
                .is_some_and(|b| b.summary.conversation_id == conversation)
            {
                gone.push(*id);
            }
        }
        for page in gone {
            self.docs_forget_page(page, out);
        }
    }

    // ----- Recovery -----

    pub(super) fn docs_restore_journal(&mut self, journal: Vec<DocJournalEntry>) {
        self.docs.journal = journal;
    }

    pub(super) fn docs_clear_journal(&mut self) {
        self.docs.journal.clear();
    }

    /// Sends what a previous run journaled and never settled. The server returns the original
    /// result for an operation it committed, and commits it now if it never saw it.
    pub(super) fn docs_recover_journal(&mut self, me: UserId, out: &mut Output) {
        let (mine, others): (Vec<_>, Vec<_>) = std::mem::take(&mut self.docs.journal)
            .into_iter()
            .partition(|entry| entry.user_id == me);
        self.docs.journal = others;
        let mut mine = mine;
        mine.sort_by_key(|entry| entry.submitted_at);
        let since = self.docs.clock;
        for entry in mine {
            self.docs.remember_op(entry.operation_id);
            match entry.action {
                DocJournalAction::Create {
                    conversation,
                    request,
                } => {
                    let page = request.page_id;
                    if self.docs.creates.contains_key(&page) {
                        continue;
                    }
                    self.docs.creates.insert(
                        page,
                        CreateEntry {
                            operation: entry.operation_id,
                            conversation,
                            request: request.clone(),
                            stage: SaveStage::Sending,
                            refused: false,
                            state: CreateState::InFlight,
                        },
                    );
                    self.docs_send(
                        Pending::Create { page, since },
                        DocsCall::CreatePage {
                            conversation,
                            request,
                        },
                        out,
                    );
                }
                DocJournalAction::Update { page, request } => {
                    if self
                        .docs
                        .pages
                        .get(&page)
                        .is_some_and(|slot| slot.save.is_some())
                    {
                        continue;
                    }
                    let slot = self.docs.pages.entry(page).or_insert_with(|| PageSlot {
                        view: OpenPage::new(page),
                        save: None,
                    });
                    slot.view.save = SaveState::Saving;
                    slot.save = Some(SaveOp {
                        operation: entry.operation_id,
                        request: request.clone(),
                        stage: SaveStage::Sending,
                        recovered: true,
                        submitted_at: entry.submitted_at,
                    });
                    self.docs_send(
                        Pending::Save { page, since },
                        DocsCall::UpdatePage { page, request },
                        out,
                    );
                }
            }
        }
        out.changes.push(Change::DocsStateChanged);
    }

    /// Forgets everything held for the signed-in person (not the journal).
    pub(super) fn docs_reset(&mut self) {
        let journal = std::mem::take(&mut self.docs.journal);
        self.docs = Docs::default();
        self.docs.journal = journal;
    }
}

/// The three changes of a page's place that need only its id.
#[derive(Clone, Copy)]
enum Change2 {
    Trash,
    Restore,
    Purge,
}

fn sort_notations_oldest(notations: &mut [Notation]) {
    notations.sort_by(|a, b| {
        a.created_at
            .cmp(&b.created_at)
            .then_with(|| a.id.cmp(&b.id))
    });
}

fn sort_notations_newest(notations: &mut [Notation]) {
    notations.sort_by(|a, b| {
        b.updated_at
            .cmp(&a.updated_at)
            .then_with(|| b.id.cmp(&a.id))
    });
}
