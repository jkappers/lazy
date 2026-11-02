//! A world for the docs tests: the real shell and the real client core, a recording effect sink,
//! and a small fake docs server that answers the core's requests the way the real one does (pages,
//! versions, trash, notations), so a test reads as what the person does and sees.
#![allow(dead_code)]

use std::cell::RefCell;
use std::rc::Rc;

use gpui::{
    AppContext as _, Entity, Modifiers, TestAppContext, VisualTestContext, WindowAppearance,
};
use lazy_client_core::{
    DocsAnswer, DocsCall, DocsEffect, DocsInput, Effect, Input, RequestError, StartupState,
};
use lazy_domain::notification::NotificationSettings;
use lazy_domain::page::{PageContent, PageKind, VersionReason};
use lazy_domain::{
    ChannelName, ChannelVisibility, ConversationId, ConversationKind, DisplayName, EventSeq,
    MessageDocument, MessageId, NotationId, PageId, Timestamp, UserId, Username, WorkspaceId,
    WorkspaceRole,
};
use lazy_protocol::{
    ApiError, Attachment, BootstrapResponse, Conversation, ConversationUserState, ErrorCode,
    HistoryPage, Message, Notation, NotationListResponse, NotationResponse, NotationScope,
    PROTOCOL_VERSION, PageDetail, PageListResponse, PageResponse, PageState, PageSummary,
    PageVersionDetail, PageVersionResponse, PageVersionSummary, PageVersionsResponse, User,
    Workspace, WorkspaceBootstrap,
};
use lazy_ui::app_model::{AppModel, EffectSink};
use lazy_ui::docs::DocsView;
use lazy_ui::root_view::RootView;
use lazy_ui::shell::ShellView;

#[derive(Default)]
pub struct Recorder(pub RefCell<Vec<Effect>>);

impl EffectSink for Recorder {
    fn execute(&self, effects: Vec<Effect>) {
        self.0.borrow_mut().extend(effects);
    }
}

/// The server's side of the docs routes.
#[derive(Default)]
pub struct Fake {
    pub pages: Vec<PageDetail>,
    pub versions: Vec<(PageId, PageVersionDetail)>,
    pub notations: Vec<Notation>,
    /// Every request the core made, in order.
    pub calls: Vec<DocsCall>,
    pub tick: i64,
    /// Answer the next mutation as an unreachable server would.
    pub fail_next: Option<RequestError>,
}

pub struct World {
    pub model: Entity<AppModel>,
    pub effects: Rc<Recorder>,
    pub root: Entity<RootView>,
    pub me: User,
    pub bob: User,
    pub workspace: WorkspaceId,
    pub general: ConversationId,
    pub design: ConversationId,
    pub fake: RefCell<Fake>,
    pub seq: std::cell::Cell<i64>,
}

pub fn person(name: &str, display: &str) -> User {
    User {
        is_bot: false,
        deactivated: false,
        avatar_version: 0,
        status: None,
        id: UserId::generate(),
        username: Username::parse(name).unwrap(),
        display_name: DisplayName::parse(display).unwrap(),
    }
}

pub fn at(offset: i64) -> Timestamp {
    Timestamp::from_offset_date_time(
        time::OffsetDateTime::now_utc() - time::Duration::minutes(60)
            + time::Duration::seconds(offset),
    )
}

pub fn start(cx: &mut TestAppContext) -> (World, &mut VisualTestContext) {
    cx.update(|cx| lazy_ui::init(cx, WindowAppearance::Light));
    let effects = Rc::new(Recorder::default());
    let model = cx.new(|_| AppModel::new(effects.clone()));
    let (root, cx) = cx.add_window_view({
        let model = model.clone();
        move |window, cx| RootView::new(model, window, cx)
    });
    let world = World {
        model,
        effects,
        root,
        me: person("myself", "Me Myself"),
        bob: person("bob", "Bob Okafor"),
        workspace: WorkspaceId::generate(),
        general: ConversationId::generate(),
        design: ConversationId::generate(),
        fake: RefCell::new(Fake::default()),
        seq: std::cell::Cell::new(0),
    };
    (world, cx)
}

/// Two frames: what is laid out in one is what the mouse can hit in the next.
pub fn draw(cx: &mut VisualTestContext) {
    for _ in 0..2 {
        cx.update(|window, _| window.refresh());
        cx.run_until_parked();
    }
}

impl World {
    fn channel(&self, id: ConversationId, name: &str) -> Conversation {
        Conversation {
            id,
            workspace_id: self.workspace,
            kind: ConversationKind::Channel,
            name: Some(ChannelName::parse(name).unwrap()),
            topic: None,
            description: None,
            visibility: ChannelVisibility::Public,
            archived: false,
            is_default: name == "general",
            created_by: None,
            participants: vec![],
            member_count: 2,
        }
    }

    fn bootstrap(&self) -> BootstrapResponse {
        let state = |id: ConversationId| ConversationUserState {
            conversation_id: id,
            favorite: false,
            last_read: EventSeq::ZERO,
            unread_count: 0,
            mention_count: 0,
        };
        BootstrapResponse {
            protocol_version: PROTOCOL_VERSION,
            user: self.me.clone(),
            workspaces: vec![WorkspaceBootstrap {
                saved: Vec::new(),
                notification_settings: NotificationSettings::default(),
                conversation_notification_prefs: Vec::new(),
                custom_emoji: Vec::new(),
                threads: Vec::new(),
                groups: Vec::new(),
                workspace: Workspace {
                    id: self.workspace,
                    name: "Lazy Labs".into(),
                    settings: Default::default(),
                },
                role: WorkspaceRole::Member,
                conversations: vec![
                    self.channel(self.general, "general"),
                    self.channel(self.design, "design"),
                ],
                user_state: vec![state(self.general), state(self.design)],
                members: vec![self.me.clone(), self.bob.clone()],
                head: EventSeq::ZERO,
            }],
        }
    }

    pub fn dispatch(&self, cx: &mut VisualTestContext, input: Input) {
        self.model.update(cx, |model, cx| model.dispatch(input, cx));
        cx.run_until_parked();
    }

    pub fn docs(&self, cx: &mut VisualTestContext, input: DocsInput) {
        self.dispatch(cx, Input::Docs(input));
        self.serve(cx);
    }

    pub fn core<R>(
        &self,
        cx: &VisualTestContext,
        read: impl FnOnce(&lazy_client_core::ClientCore) -> R,
    ) -> R {
        self.model.read_with(cx, |model, _| read(model.core()))
    }

    /// Signs in with #general open and its history empty.
    pub fn reach(&self, cx: &mut VisualTestContext) {
        cx.simulate_resize(gpui::size(gpui::px(1280.), gpui::px(900.)));
        self.dispatch(
            cx,
            Input::Started(StartupState {
                session_restored: true,
                ..Default::default()
            }),
        );
        self.dispatch(cx, Input::BootstrapLoaded(Box::new(self.bootstrap())));
        self.dispatch(cx, Input::Clock(Timestamp::now()));
        let generation = self
            .effects
            .0
            .borrow()
            .iter()
            .find_map(|effect| match effect {
                Effect::FetchHistory { generation, .. } => Some(*generation),
                _ => None,
            })
            .expect("loading #general fetches its history");
        self.effects.0.borrow_mut().clear();
        self.dispatch(
            cx,
            Input::HistoryLoaded {
                conversation: self.general,
                generation,
                result: Ok(HistoryPage {
                    messages: vec![Message {
                        attachments: vec![],
                        mentions: Default::default(),
                        thread_root: None,
                        in_channel: false,
                        thread: None,
                        id: MessageId::generate(),
                        conversation_id: self.general,
                        author_id: self.bob.id,
                        document: MessageDocument::from_markdown("hello").unwrap(),
                        created_at: at(0),
                        revision: 1,
                        seq: EventSeq::new(1).unwrap(),
                        edited_at: None,
                        reactions: vec![],
                        reactions_as_of: EventSeq::ZERO,
                        deleted: false,
                    }],
                    has_more: false,
                }),
            },
        );
        draw(cx);
    }

    pub fn shell(&self, cx: &mut VisualTestContext) -> Entity<ShellView> {
        self.root
            .read_with(cx, |root, _| root.shell().expect("the shell").clone())
    }

    pub fn docs_view(&self, cx: &mut VisualTestContext) -> Entity<DocsView> {
        let shell = self.shell(cx);
        shell.read_with(cx, |shell, _| shell.docs_view().clone())
    }

    pub fn on_screen(&self, cx: &mut VisualTestContext, selector: &'static str) -> bool {
        draw(cx);
        cx.debug_bounds(selector).is_some()
    }

    pub fn click(&self, cx: &mut VisualTestContext, selector: &'static str) {
        draw(cx);
        let bounds = cx
            .debug_bounds(selector)
            .unwrap_or_else(|| panic!("nothing is labelled {selector:?} on screen"));
        cx.simulate_click(bounds.center(), Modifiers::default());
        cx.run_until_parked();
        self.serve(cx);
        draw(cx);
    }

    pub fn click_named(&self, cx: &mut VisualTestContext, selector: String) {
        draw(cx);
        // The selector table wants a static string; a test leaks the few it names.
        let selector: &'static str = Box::leak(selector.into_boxed_str());
        let bounds = cx
            .debug_bounds(selector)
            .unwrap_or_else(|| panic!("nothing is labelled {selector:?} on screen"));
        cx.simulate_click(bounds.center(), Modifiers::default());
        cx.run_until_parked();
        self.serve(cx);
        draw(cx);
    }

    pub fn keys(&self, cx: &mut VisualTestContext, keys: &str) {
        cx.simulate_keystrokes(keys);
        cx.run_until_parked();
        self.serve(cx);
        draw(cx);
    }

    pub fn type_text(&self, cx: &mut VisualTestContext, text: &str) {
        cx.simulate_input(text);
        cx.run_until_parked();
        self.serve(cx);
        draw(cx);
    }

    /// Opens docs with the "Docs" link in the sidebar: every page of the workspace.
    pub fn open_docs(&self, cx: &mut VisualTestContext) {
        self.click(cx, "sidebar-docs");
        assert!(self.on_screen(cx, "docs-view"), "docs opened");
    }

    /// Whether a docs menu is open.
    pub fn menu_open(&self, cx: &mut VisualTestContext) -> bool {
        let view = self.docs_view(cx);
        view.read_with(cx, |view, _| view.menu_open())
    }

    /// Opens the "New" menu of the list and picks `item` (`docs-new-page` or `docs-new-board`).
    pub fn create_from_menu(&self, cx: &mut VisualTestContext, item: &'static str) {
        self.click(cx, "docs-new");
        self.click(cx, item);
    }

    /// Opens the "…" menu of an open page and picks `item` (`docs-trash`, `docs-move`,
    /// `docs-attach`, or `docs-add-note`).
    pub fn pick_from_more(&self, cx: &mut VisualTestContext, item: &'static str) {
        self.click(cx, "docs-more");
        self.click(cx, item);
    }

    // ----- The fake server -----

    pub fn seed_page(&self, title: &str, body: &str) -> PageId {
        let mut fake = self.fake.borrow_mut();
        fake.tick += 1;
        let id = PageId::generate();
        let tick = fake.tick;
        fake.pages.push(PageDetail {
            summary: summary(self, id, self.general, PageKind::Page, title, 1, tick),
            content: PageContent::Page { body: body.into() },
            files: Vec::new(),
        });
        id
    }

    pub fn seed_board(&self, title: &str, board: lazy_domain::Board) -> PageId {
        let mut fake = self.fake.borrow_mut();
        fake.tick += 1;
        let id = PageId::generate();
        let tick = fake.tick;
        fake.pages.push(PageDetail {
            summary: summary(self, id, self.general, PageKind::Board, title, 1, tick),
            content: PageContent::Board { board },
            files: Vec::new(),
        });
        id
    }

    pub fn page(&self, id: PageId) -> PageDetail {
        self.fake
            .borrow()
            .pages
            .iter()
            .find(|p| p.summary.id == id)
            .cloned()
            .expect("the page exists on the fake server")
    }

    pub fn body_of(&self, id: PageId) -> String {
        match self.page(id).content {
            PageContent::Page { body } => body,
            other => panic!("not a page: {other:?}"),
        }
    }

    /// A frame from the server's realtime connection.
    pub fn event(&self, cx: &mut VisualTestContext, event: lazy_protocol::Event) {
        let seq = self.seq.get() + 1;
        self.seq.set(seq);
        self.dispatch(
            cx,
            Input::Frame(lazy_protocol::ServerFrame::Event(
                lazy_protocol::EventEnvelope {
                    workspace_id: self.workspace,
                    seq: EventSeq::new(seq).unwrap(),
                    occurred_at: Timestamp::now(),
                    event,
                },
            )),
        );
        self.serve(cx);
        draw(cx);
    }

    /// Lets the pause after typing pass, so the autosave runs and the server answers.
    pub fn autosave(&self, cx: &mut VisualTestContext) {
        cx.executor()
            .advance_clock(lazy_ui::docs::AUTOSAVE_AFTER + std::time::Duration::from_millis(5));
        cx.run_until_parked();
        self.serve(cx);
        draw(cx);
    }

    pub fn drag(
        &self,
        cx: &mut VisualTestContext,
        from: gpui::Point<gpui::Pixels>,
        to: gpui::Point<gpui::Pixels>,
    ) {
        let held = Some(gpui::MouseButton::Left);
        cx.simulate_mouse_down(from, gpui::MouseButton::Left, Modifiers::default());
        let middle = gpui::point((from.x + to.x) / 2.0, (from.y + to.y) / 2.0);
        cx.simulate_mouse_move(middle, held, Modifiers::default());
        cx.simulate_mouse_move(to, held, Modifiers::default());
        cx.simulate_mouse_up(to, gpui::MouseButton::Left, Modifiers::default());
        cx.run_until_parked();
        draw(cx);
    }

    pub fn center(
        &self,
        cx: &mut VisualTestContext,
        selector: &'static str,
    ) -> gpui::Point<gpui::Pixels> {
        draw(cx);
        cx.debug_bounds(selector)
            .unwrap_or_else(|| panic!("nothing is labelled {selector:?} on screen"))
            .center()
    }

    pub fn first_page(&self) -> PageId {
        self.fake.borrow().pages[0].summary.id
    }

    pub fn calls(&self) -> Vec<DocsCall> {
        self.fake.borrow().calls.clone()
    }

    /// Answers everything the core has asked for, until it asks nothing more.
    pub fn serve(&self, cx: &mut VisualTestContext) {
        for _ in 0..40 {
            let effects = std::mem::take(&mut *self.effects.0.borrow_mut());
            let mut replies: Vec<Input> = Vec::new();
            for effect in effects {
                let Effect::Docs(effect) = effect else {
                    continue;
                };
                match effect {
                    DocsEffect::Persist(entry) => replies.push(Input::DocsPersisted {
                        operation: entry.operation_id,
                    }),
                    DocsEffect::Settle { .. } => {}
                    DocsEffect::Request { id, call } => {
                        let result = self.answer(call);
                        replies.push(Input::DocsReply { id, result });
                    }
                }
            }
            if replies.is_empty() {
                return;
            }
            for reply in replies {
                self.dispatch(cx, reply);
            }
        }
        panic!("the core kept asking");
    }

    fn answer(&self, call: DocsCall) -> Result<DocsAnswer, RequestError> {
        let mut fake = self.fake.borrow_mut();
        fake.calls.push(call.clone());
        if call.is_mutation()
            && let Some(error) = fake.fail_next.take()
        {
            return Err(error);
        }
        fake.tick += 1;
        let tick = fake.tick;
        let not_found = || {
            Err(RequestError::Rejected(ApiError::new(
                ErrorCode::NotFound,
                "no such page",
            )))
        };
        let me = self.me.id;
        match call {
            DocsCall::ListPages {
                conversation,
                params,
            } => {
                let mut pages: Vec<PageSummary> = fake
                    .pages
                    .iter()
                    .filter(|p| p.summary.conversation_id == conversation)
                    .filter(|p| {
                        (p.summary.trashed_at.is_some()) == (params.state == PageState::Trashed)
                    })
                    .filter(|p| params.kind.is_none_or(|kind| p.summary.kind == kind))
                    .map(|p| p.summary.clone())
                    .collect();
                pages.sort_by_key(|page| std::cmp::Reverse(page.updated_at));
                let offset = params.offset.unwrap_or(0) as usize;
                let limit = params.limit.unwrap_or(50) as usize;
                let has_more = pages.len() > offset + limit;
                let pages = pages.into_iter().skip(offset).take(limit).collect();
                Ok(DocsAnswer::Pages(PageListResponse { pages, has_more }))
            }
            DocsCall::ListWorkspacePages { params, .. } => {
                let q = params.q.unwrap_or_default().to_lowercase();
                let trashed = params.state == PageState::Trashed;
                let mut pages: Vec<PageSummary> = fake
                    .pages
                    .iter()
                    .filter(|p| p.summary.trashed_at.is_some() == trashed)
                    .filter(|p| {
                        p.summary.title.to_lowercase().contains(&q)
                            || p.content.plain_text().to_lowercase().contains(&q)
                    })
                    .map(|p| p.summary.clone())
                    .collect();
                pages.sort_by_key(|page| std::cmp::Reverse(page.updated_at));
                let offset = params.offset.unwrap_or(0) as usize;
                let limit = params.limit.unwrap_or(50) as usize;
                let has_more = pages.len() > offset + limit;
                let pages = pages.into_iter().skip(offset).take(limit).collect();
                Ok(DocsAnswer::Pages(PageListResponse { pages, has_more }))
            }
            DocsCall::GetPage(id) => match fake.pages.iter().find(|p| p.summary.id == id) {
                Some(page) => Ok(DocsAnswer::Page(PageResponse { page: page.clone() })),
                None => not_found(),
            },
            DocsCall::CreatePage {
                conversation,
                request,
            } => {
                let content = request
                    .content
                    .clone()
                    .unwrap_or_else(|| PageContent::empty(request.kind));
                let title = if request.title.trim().is_empty() {
                    "Untitled".to_owned()
                } else {
                    request.title.clone()
                };
                let mut detail = PageDetail {
                    summary: summary(
                        self,
                        request.page_id,
                        conversation,
                        request.kind,
                        &title,
                        1,
                        tick,
                    ),
                    content,
                    files: Vec::new(),
                };
                detail.summary.excerpt = lazy_domain::page::excerpt(&detail.content.plain_text());
                if !fake.pages.iter().any(|p| p.summary.id == request.page_id) {
                    fake.pages.push(detail.clone());
                }
                Ok(DocsAnswer::Page(PageResponse { page: detail }))
            }
            DocsCall::UpdatePage { page, request } => {
                let Some(index) = fake.pages.iter().position(|p| p.summary.id == page) else {
                    return not_found();
                };
                if fake.pages[index].summary.revision != request.base_revision {
                    return Err(RequestError::Rejected(ApiError::new(
                        ErrorCode::StaleRevision,
                        "changed",
                    )));
                }
                let before = fake.pages[index].clone();
                let n = fake.versions.iter().filter(|(p, _)| *p == page).count() as u32 + 1;
                fake.versions.push((
                    page,
                    PageVersionDetail {
                        summary: PageVersionSummary {
                            page_id: page,
                            version: n,
                            title: before.summary.title.clone(),
                            reason: VersionReason::Autosave,
                            created_by: Some(me),
                            created_at: at(i64::from(n)),
                            revision: before.summary.revision,
                        },
                        content: before.content.clone(),
                    },
                ));
                let entry = &mut fake.pages[index];
                if let Some(title) = &request.title {
                    entry.summary.title = if title.trim().is_empty() {
                        "Untitled".into()
                    } else {
                        title.clone()
                    };
                }
                if let Some(content) = &request.content {
                    entry.content = content.clone();
                }
                if let Some(append) = &request.append
                    && let PageContent::Page { body } = &mut entry.content
                {
                    body.push_str("\n\n");
                    body.push_str(append);
                }
                if let Some(conversation) = request.conversation_id {
                    entry.summary.conversation_id = conversation;
                }
                entry.summary.revision += 1;
                entry.summary.updated_at = at(tick);
                entry.summary.updated_by = me;
                entry.summary.excerpt = lazy_domain::page::excerpt(&entry.content.plain_text());
                Ok(DocsAnswer::Page(PageResponse {
                    page: entry.clone(),
                }))
            }
            DocsCall::TrashPage { page, .. } | DocsCall::RestorePage { page, .. } => {
                let trash = matches!(call, DocsCall::TrashPage { .. });
                let Some(entry) = fake.pages.iter_mut().find(|p| p.summary.id == page) else {
                    return not_found();
                };
                if trash {
                    entry.summary.trashed_at = Some(Timestamp::now());
                    entry.summary.purge_at = Some(Timestamp::from_offset_date_time(
                        time::OffsetDateTime::now_utc() + time::Duration::days(30),
                    ));
                } else {
                    entry.summary.trashed_at = None;
                    entry.summary.purge_at = None;
                }
                Ok(DocsAnswer::Page(PageResponse {
                    page: entry.clone(),
                }))
            }
            DocsCall::PurgePage { page, .. } => {
                fake.pages.retain(|p| p.summary.id != page);
                Ok(DocsAnswer::Done)
            }
            DocsCall::ListVersions(page) => {
                let mut versions: Vec<PageVersionSummary> = fake
                    .versions
                    .iter()
                    .filter(|(p, _)| *p == page)
                    .map(|(_, v)| v.summary.clone())
                    .collect();
                versions.sort_by_key(|v| std::cmp::Reverse(v.version));
                Ok(DocsAnswer::Versions(PageVersionsResponse { versions }))
            }
            DocsCall::GetVersion { page, version } => {
                match fake
                    .versions
                    .iter()
                    .find(|(p, v)| *p == page && v.summary.version == version)
                {
                    Some((_, v)) => Ok(DocsAnswer::Version(PageVersionResponse {
                        version: v.clone(),
                    })),
                    None => not_found(),
                }
            }
            DocsCall::RestoreVersion { page, version, .. } => {
                let Some((_, old)) = fake
                    .versions
                    .iter()
                    .find(|(p, v)| *p == page && v.summary.version == version)
                    .cloned()
                else {
                    return not_found();
                };
                let Some(index) = fake.pages.iter().position(|p| p.summary.id == page) else {
                    return not_found();
                };
                let before = fake.pages[index].clone();
                let n = fake.versions.iter().filter(|(p, _)| *p == page).count() as u32 + 1;
                fake.versions.push((
                    page,
                    PageVersionDetail {
                        summary: PageVersionSummary {
                            page_id: page,
                            version: n,
                            title: before.summary.title.clone(),
                            reason: VersionReason::PreRestore,
                            created_by: Some(me),
                            created_at: at(i64::from(n)),
                            revision: before.summary.revision,
                        },
                        content: before.content.clone(),
                    },
                ));
                let entry = &mut fake.pages[index];
                entry.content = old.content.clone();
                entry.summary.title = old.summary.title.clone();
                entry.summary.revision += 1;
                entry.summary.excerpt = lazy_domain::page::excerpt(&entry.content.plain_text());
                Ok(DocsAnswer::Page(PageResponse {
                    page: entry.clone(),
                }))
            }
            DocsCall::ListNotations(page) => Ok(DocsAnswer::Notations(NotationListResponse {
                notations: fake
                    .notations
                    .iter()
                    .filter(|n| n.page_id == page)
                    .cloned()
                    .collect(),
            })),
            DocsCall::CreateNotation { page, request } => {
                let conversation = fake
                    .pages
                    .iter()
                    .find(|p| p.summary.id == page)
                    .map_or(self.general, |p| p.summary.conversation_id);
                let notation = Notation {
                    id: request.notation_id,
                    page_id: page,
                    conversation_id: conversation,
                    author_id: me,
                    assignee_id: request.assignee_id.unwrap_or(me),
                    quote: request.quote,
                    from: request.from,
                    to: request.to,
                    body: request.body,
                    created_at: at(tick),
                    updated_at: at(tick),
                    completed_at: None,
                    resolved_by: None,
                    replies: Vec::new(),
                };
                fake.notations.push(notation.clone());
                Ok(DocsAnswer::Notation(NotationResponse { notation }))
            }
            DocsCall::UpdateNotation { notation, request } => {
                let Some(entry) = fake.notations.iter_mut().find(|n| n.id == notation) else {
                    return not_found();
                };
                if let Some(body) = request.body {
                    entry.body = body;
                }
                if let Some(assignee) = request.assignee_id {
                    entry.assignee_id = assignee;
                }
                entry.updated_at = at(tick);
                Ok(DocsAnswer::Notation(NotationResponse {
                    notation: entry.clone(),
                }))
            }
            DocsCall::SetNotationCompleted { notation, request } => {
                let Some(entry) = fake.notations.iter_mut().find(|n| n.id == notation) else {
                    return not_found();
                };
                entry.completed_at = request.completed.then(|| at(tick));
                entry.resolved_by = request.completed.then_some(me);
                entry.updated_at = at(tick);
                Ok(DocsAnswer::Notation(NotationResponse {
                    notation: entry.clone(),
                }))
            }
            DocsCall::DeleteNotation { notation, .. } => {
                fake.notations.retain(|n| n.id != notation);
                Ok(DocsAnswer::Done)
            }
            DocsCall::ReplyToNotation { notation, request } => {
                let Some(entry) = fake.notations.iter_mut().find(|n| n.id == notation) else {
                    return not_found();
                };
                entry.replies.push(lazy_protocol::NotationReply {
                    id: request.reply_id,
                    notation_id: notation,
                    author_id: me,
                    body: request.body,
                    created_at: at(tick),
                });
                entry.updated_at = at(tick);
                Ok(DocsAnswer::Notation(NotationResponse {
                    notation: entry.clone(),
                }))
            }
            DocsCall::DeleteNotationReply { reply, .. } => {
                for notation in &mut fake.notations {
                    notation.replies.retain(|r| r.id != reply);
                }
                Ok(DocsAnswer::Done)
            }
            DocsCall::ListWorkspaceNotations { params, .. } => {
                let notations = fake
                    .notations
                    .iter()
                    .filter(|n| match params.scope {
                        NotationScope::Assigned => n.assignee_id == me,
                        NotationScope::Created => n.author_id == me,
                    })
                    .filter(|n| n.completed_at.is_some() == params.completed)
                    .cloned()
                    .collect();
                Ok(DocsAnswer::Notations(NotationListResponse { notations }))
            }
            DocsCall::UploadPageFile {
                page, attachment, ..
            } => {
                let file = Attachment {
                    id: attachment,
                    name: "spec.pdf".into(),
                    media_type: "application/pdf".into(),
                    size: 2048,
                    sha256: "00".into(),
                    width: None,
                    height: None,
                    has_thumbnail: false,
                };
                if let Some(entry) = fake.pages.iter_mut().find(|p| p.summary.id == page) {
                    entry.files.push(file.clone());
                    entry.summary.file_count += 1;
                }
                Ok(DocsAnswer::File(file))
            }
        }
    }
}

pub fn summary(
    world: &World,
    id: PageId,
    conversation: ConversationId,
    kind: PageKind,
    title: &str,
    revision: u32,
    tick: i64,
) -> PageSummary {
    PageSummary {
        id,
        workspace_id: world.workspace,
        conversation_id: conversation,
        kind,
        title: title.to_owned(),
        excerpt: String::new(),
        created_by: world.me.id,
        created_at: at(0),
        updated_by: world.me.id,
        updated_at: at(tick),
        revision,
        trashed_at: None,
        purge_at: None,
        open_notations: 0,
        file_count: 0,
    }
}

pub fn fresh_notation_id() -> NotationId {
    NotationId::generate()
}
