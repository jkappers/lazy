//! The docs scenes of the `screens` example: the list, a page, a conflict, a board, history,
//! notes, and the trash, each drawn from a small pretend server so the core is in the same state
//! it would be in after real answers.

use gpui::{Entity, HeadlessAppContext, WindowHandle};
use lazy_client_core::{DocsAnswer, DocsCall, DocsEffect, DocsInput, Effect, Input, RequestError};
use lazy_domain::board::{BoardColor, BoardEdge, BoardNode, NodeShape, Viewport};
use lazy_domain::page::{PageContent, PageKind, VersionReason};
use lazy_domain::{Board, NotationId, NotationReplyId, PageId, Timestamp};
use lazy_protocol::{
    ApiError, ErrorCode, Notation, NotationListResponse, NotationReply, PageDetail,
    PageListResponse, PageResponse, PageSummary, PageVersionDetail, PageVersionResponse,
    PageVersionSummary, PageVersionsResponse,
};
use lazy_ui::app_model::AppModel;
use lazy_ui::root_view::RootView;

use super::{Cast, Recorder, dispatch, draw, press, window_shell_do};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    List,
    Empty,
    Trash,
    Page,
    Conflict,
    Board,
    BoardEditing,
    History,
    Notes,
    NotesScreen,
    /// The list with its "New" menu open.
    NewMenu,
    /// The list with the conversation menu open.
    PlaceMenu,
    /// An open page with the "…" menu open.
    MoreMenu,
}

fn minutes_ago(n: i64) -> Timestamp {
    Timestamp::from_offset_date_time(time::OffsetDateTime::now_utc() - time::Duration::minutes(n))
}

struct Store {
    pages: Vec<PageDetail>,
    versions: Vec<PageVersionDetail>,
    notations: Vec<Notation>,
    /// Make the server's copy of this page newer than the client's.
    newer: Option<PageId>,
}

const BODY: &str = "# Launch plan\n\nWe ship the **docs** to everyone on Friday. The checklist:\n\n- Write the announcement\n- Record a short demo\n- Update the *help centre*\n\n> Nothing goes out until the demo is signed off.\n\n## Risks\n\nThe board export is the riskiest part; see `export.rs`.\n\n```\ncargo test -p lazy-server\n```\n";

fn summary(cast: &Cast, title: &str, kind: PageKind, minutes: i64, excerpt: &str) -> PageSummary {
    PageSummary {
        id: PageId::generate(),
        workspace_id: cast.workspace,
        conversation_id: cast.general,
        kind,
        title: title.to_owned(),
        excerpt: excerpt.to_owned(),
        created_by: cast.bob.id,
        created_at: minutes_ago(minutes + 600),
        updated_by: if minutes % 2 == 0 {
            cast.me.id
        } else {
            cast.bob.id
        },
        updated_at: minutes_ago(minutes),
        revision: 3,
        trashed_at: None,
        purge_at: None,
        open_notations: 0,
        file_count: 0,
    }
}

fn board() -> Board {
    let node = |id: &str, shape, x, y, w, h, text: &str, color| BoardNode {
        id: id.into(),
        shape,
        x,
        y,
        width: w,
        height: h,
        text: text.into(),
        color,
    };
    Board {
        nodes: vec![
            node(
                "n1",
                NodeShape::Ellipse,
                40,
                60,
                130,
                70,
                "Idea",
                Some(BoardColor::Blue),
            ),
            node(
                "n2",
                NodeShape::Rectangle,
                250,
                60,
                170,
                70,
                "Write the spec",
                None,
            ),
            node(
                "n3",
                NodeShape::Diamond,
                500,
                40,
                150,
                110,
                "Approved?",
                Some(BoardColor::Orange),
            ),
            node(
                "n4",
                NodeShape::Rectangle,
                740,
                60,
                150,
                70,
                "Ship it",
                Some(BoardColor::Green),
            ),
            node(
                "n5",
                NodeShape::Note,
                250,
                220,
                190,
                90,
                "Ask design about the empty state",
                None,
            ),
            node(
                "n6",
                NodeShape::Text,
                520,
                230,
                220,
                40,
                "Risks and open questions",
                None,
            ),
            node(
                "n7",
                NodeShape::Rectangle,
                520,
                300,
                160,
                60,
                "Rework",
                Some(BoardColor::Red),
            ),
        ],
        edges: vec![
            edge("e1", "n1", "n2", ""),
            edge("e2", "n2", "n3", ""),
            edge("e3", "n3", "n4", "yes"),
            edge("e4", "n3", "n7", "no"),
            edge("e5", "n7", "n2", ""),
        ],
        viewport: Viewport {
            x: 40,
            y: 30,
            zoom_percent: 100,
        },
    }
}

fn edge(id: &str, source: &str, target: &str, label: &str) -> BoardEdge {
    BoardEdge {
        id: id.into(),
        source: source.into(),
        target: target.into(),
        label: label.into(),
    }
}

fn store(cast: &Cast, kind: Kind) -> (Store, PageId) {
    let mut pages = Vec::new();
    let mut add = |title: &str, kind: PageKind, minutes: i64, content: PageContent| {
        let mut s = summary(
            cast,
            title,
            kind,
            minutes,
            &lazy_domain::page::excerpt(&content.plain_text()),
        );
        s.file_count = 0;
        let id = s.id;
        pages.push(PageDetail {
            summary: s,
            content,
            files: Vec::new(),
        });
        id
    };
    let open = match kind {
        Kind::Empty => PageId::generate(),
        Kind::Board | Kind::BoardEditing => {
            add(
                "Release flow",
                PageKind::Board,
                3,
                PageContent::Board { board: board() },
            );
            add(
                "Launch plan",
                PageKind::Page,
                12,
                PageContent::Page { body: BODY.into() },
            );
            pages[0].summary.id
        }
        _ => {
            let id = add(
                "Launch plan",
                PageKind::Page,
                4,
                PageContent::Page { body: BODY.into() },
            );
            let release = add(
                "Release flow",
                PageKind::Board,
                35,
                PageContent::Board { board: board() },
            );
            add(
                "Meeting notes, week 42",
                PageKind::Page,
                140,
                PageContent::Page {
                    body: "Agenda\n\nDemo on Friday.".into(),
                },
            );
            let onboarding = add(
                "Onboarding checklist",
                PageKind::Page,
                60 * 30,
                PageContent::Page {
                    body: "Day one: laptop, accounts, a welcome message in #general.".into(),
                },
            );
            let colours = add(
                "Brand colours",
                PageKind::Board,
                60 * 50,
                PageContent::Board {
                    board: Board::empty(),
                },
            );
            // The workspace list shows where each one lives; a few are not in #general.
            for (page, place) in [
                (release, cast.engineering),
                (onboarding, cast.engineering),
                (colours, cast.design),
            ] {
                if let Some(held) = pages.iter_mut().find(|p| p.summary.id == page) {
                    held.summary.conversation_id = place;
                }
            }
            id
        }
    };
    if kind == Kind::Trash {
        for page in &mut pages {
            if page.summary.title != "Launch plan" {
                page.summary.trashed_at = Some(minutes_ago(60 * 24 * 3));
                page.summary.purge_at = Some(Timestamp::from_offset_date_time(
                    time::OffsetDateTime::now_utc() + time::Duration::days(27),
                ));
            }
        }
    }
    if let Some(first) = pages.first_mut()
        && matches!(kind, Kind::List | Kind::Page | Kind::Notes | Kind::History)
    {
        first.summary.open_notations = 2;
        first.summary.file_count = 1;
    }
    let mut versions = Vec::new();
    for (n, (minutes, reason, text)) in [
        (
            300,
            VersionReason::Autosave,
            "# Launch plan\n\nWe ship on Friday.",
        ),
        (
            90,
            VersionReason::Autosave,
            "# Launch plan\n\nWe ship the docs on Friday.\n\n- Write the announcement",
        ),
        (30, VersionReason::PreRestore, "# Launch plan\n\nDraft"),
    ]
    .into_iter()
    .enumerate()
    {
        versions.push(PageVersionDetail {
            summary: PageVersionSummary {
                page_id: open,
                version: n as u32 + 1,
                title: "Launch plan".into(),
                reason,
                created_by: Some(if n % 2 == 0 { cast.bob.id } else { cast.me.id }),
                created_at: minutes_ago(minutes),
                revision: n as u32 + 1,
            },
            content: PageContent::Page { body: text.into() },
        });
    }
    let notation = |quote: &str,
                    from: u32,
                    body: &str,
                    mine: bool,
                    done: bool,
                    replies: Vec<NotationReply>| Notation {
        id: NotationId::generate(),
        page_id: open,
        conversation_id: cast.general,
        author_id: if mine { cast.me.id } else { cast.bob.id },
        assignee_id: cast.me.id,
        quote: quote.into(),
        from,
        to: from + quote.chars().count() as u32,
        body: body.into(),
        created_at: minutes_ago(50),
        updated_at: minutes_ago(20),
        completed_at: done.then(|| minutes_ago(10)),
        resolved_by: done.then_some(cast.me.id),
        replies,
    };
    let first_id = NotationId::generate();
    let mut first = notation(
        "Nothing goes out until the demo is signed off.",
        0,
        "Who signs it off? Put a name here.",
        false,
        false,
        vec![NotationReply {
            id: NotationReplyId::generate(),
            notation_id: first_id,
            author_id: cast.me.id,
            body: "Zoe, I'll ask her today.".into(),
            created_at: minutes_ago(15),
        }],
    );
    first.id = first_id;
    first.quote = "ship the **docs**".into();
    first.from = BODY
        .find("ship the **docs**")
        .map_or(0, |b| BODY[..b].chars().count() as u32);
    first.to = first.from + first.quote.chars().count() as u32;
    let second = notation(
        "export.rs",
        0,
        "Is this still the right file?",
        true,
        false,
        Vec::new(),
    );
    let gone = notation(
        "a sentence that was cut",
        4,
        "This one no longer applies.",
        false,
        true,
        Vec::new(),
    );
    (
        Store {
            pages,
            versions,
            notations: vec![first, second, gone],
            newer: (kind == Kind::Conflict).then_some(open),
        },
        open,
    )
}

/// Answers every request the core has made, from the pretend server.
fn serve(
    cx: &mut HeadlessAppContext,
    model: &Entity<AppModel>,
    rec: &Recorder,
    store: &mut Store,
    cast: &Cast,
) {
    for _ in 0..40 {
        let effects = std::mem::take(&mut *rec.0.borrow_mut());
        let mut replies = Vec::new();
        for effect in effects {
            let Effect::Docs(effect) = effect else {
                continue;
            };
            match effect {
                DocsEffect::Persist(entry) => replies.push(Input::DocsPersisted {
                    operation: entry.operation_id,
                }),
                DocsEffect::Settle { .. } => {}
                DocsEffect::Request { id, call } => replies.push(Input::DocsReply {
                    id,
                    result: answer(store, cast, call),
                }),
            }
        }
        if replies.is_empty() {
            return;
        }
        for reply in replies {
            dispatch(cx, model, reply);
        }
    }
}

fn answer(store: &mut Store, cast: &Cast, call: DocsCall) -> Result<DocsAnswer, RequestError> {
    let missing = || {
        Err(RequestError::Rejected(ApiError::new(
            ErrorCode::NotFound,
            "no such page",
        )))
    };
    match call {
        DocsCall::ListPages {
            conversation,
            params,
        } => {
            let trashed = params.state == lazy_protocol::PageState::Trashed;
            Ok(DocsAnswer::Pages(PageListResponse {
                pages: store
                    .pages
                    .iter()
                    .filter(|p| p.summary.conversation_id == conversation)
                    .filter(|p| p.summary.trashed_at.is_some() == trashed)
                    .map(|p| p.summary.clone())
                    .collect(),
                has_more: false,
            }))
        }
        DocsCall::ListWorkspacePages { params, .. } => {
            let trashed = params.state == lazy_protocol::PageState::Trashed;
            Ok(DocsAnswer::Pages(PageListResponse {
                pages: store
                    .pages
                    .iter()
                    .filter(|p| p.summary.trashed_at.is_some() == trashed)
                    .map(|p| p.summary.clone())
                    .collect(),
                has_more: false,
            }))
        }
        DocsCall::GetPage(id) => match store.pages.iter_mut().find(|p| p.summary.id == id) {
            Some(page) => {
                if store.newer == Some(id) {
                    page.summary.revision += 1;
                    page.summary.updated_by = cast.bob.id;
                    page.content = PageContent::Page {
                        body: BODY.replace("Friday", "Thursday"),
                    };
                    store.newer = None;
                    // The first load is the client's; the next is the newer one.
                    store.newer = Some(id);
                }
                Ok(DocsAnswer::Page(PageResponse { page: page.clone() }))
            }
            None => missing(),
        },
        DocsCall::UpdatePage { page, request } => {
            let Some(entry) = store.pages.iter_mut().find(|p| p.summary.id == page) else {
                return missing();
            };
            if store.newer == Some(page) || entry.summary.revision != request.base_revision {
                entry.summary.revision = request.base_revision + 1;
                entry.summary.updated_by = cast.bob.id;
                entry.content = PageContent::Page {
                    body: BODY.replace("Friday", "Thursday"),
                };
                return Err(RequestError::Rejected(ApiError::new(
                    ErrorCode::StaleRevision,
                    "changed",
                )));
            }
            Ok(DocsAnswer::Page(PageResponse {
                page: entry.clone(),
            }))
        }
        DocsCall::ListVersions(page) => Ok(DocsAnswer::Versions(PageVersionsResponse {
            versions: store
                .versions
                .iter()
                .rev()
                .filter(|v| v.summary.page_id == page)
                .map(|v| v.summary.clone())
                .collect(),
        })),
        DocsCall::GetVersion { page, version } => match store
            .versions
            .iter()
            .find(|v| v.summary.page_id == page && v.summary.version == version)
        {
            Some(v) => Ok(DocsAnswer::Version(PageVersionResponse {
                version: v.clone(),
            })),
            None => missing(),
        },
        DocsCall::ListNotations(page) => Ok(DocsAnswer::Notations(NotationListResponse {
            notations: store
                .notations
                .iter()
                .filter(|n| n.page_id == page)
                .cloned()
                .collect(),
        })),
        DocsCall::ListWorkspaceNotations { params, .. } => {
            Ok(DocsAnswer::Notations(NotationListResponse {
                notations: store
                    .notations
                    .iter()
                    .filter(|n| n.completed_at.is_some() == params.completed)
                    .filter(|n| match params.scope {
                        lazy_protocol::NotationScope::Assigned => n.assignee_id == cast.me.id,
                        lazy_protocol::NotationScope::Created => n.author_id == cast.me.id,
                    })
                    .cloned()
                    .collect(),
            }))
        }
        _ => Err(RequestError::Unknown("not in a scene".into())),
    }
}

fn docs(
    cx: &mut HeadlessAppContext,
    model: &Entity<AppModel>,
    rec: &Recorder,
    store: &mut Store,
    cast: &Cast,
    input: DocsInput,
) {
    dispatch(cx, model, Input::Docs(input));
    serve(cx, model, rec, store, cast);
}

/// Brings the shell to #general and draws the docs scene `kind`.
pub fn scene(
    cx: &mut HeadlessAppContext,
    model: &Entity<AppModel>,
    cast: &Cast,
    rec: &Recorder,
    win: &WindowHandle<RootView>,
    kind: Kind,
) {
    super::reach_chat(cx, model, cast, rec, true);
    rec.0.borrow_mut().clear();
    let (mut store, open) = store(cast, kind);
    draw(cx, win);
    if kind == Kind::NotesScreen {
        window_shell_do(cx, win, |shell, window, cx| {
            shell.open_notes_for_me(window, cx)
        });
        serve(cx, model, rec, &mut store, cast);
        draw(cx, win);
        return;
    }
    window_shell_do(cx, win, |shell, window, cx| shell.open_docs(window, cx));
    serve(cx, model, rec, &mut store, cast);
    draw(cx, win);
    if kind == Kind::Trash {
        window_shell_do(cx, win, |shell, window, cx| {
            shell
                .docs_view()
                .update(cx, |docs, cx| docs.show_trash(window, cx))
        });
        serve(cx, model, rec, &mut store, cast);
        draw(cx, win);
        return;
    }
    if matches!(kind, Kind::NewMenu | Kind::PlaceMenu) {
        window_shell_do(cx, win, |shell, window, cx| {
            shell.docs_view().update(cx, |docs, cx| {
                if kind == Kind::NewMenu {
                    docs.show_new_menu(window, cx)
                } else {
                    docs.show_place_menu(window, cx)
                }
            })
        });
        draw(cx, win);
        return;
    }
    if matches!(kind, Kind::List | Kind::Empty) {
        return;
    }
    window_shell_do(cx, win, |shell, window, cx| {
        shell
            .docs_view()
            .update(cx, |docs, cx| docs.open_page(open, window, cx))
    });
    serve(cx, model, rec, &mut store, cast);
    draw(cx, win);
    match kind {
        Kind::MoreMenu => {
            window_shell_do(cx, win, |shell, window, cx| {
                shell
                    .docs_view()
                    .update(cx, |docs, cx| docs.show_more_menu(window, cx))
            });
        }
        Kind::Conflict => {
            docs(
                cx,
                model,
                rec,
                &mut store,
                cast,
                DocsInput::EditPage {
                    title: None,
                    content: Some(PageContent::Page {
                        body: BODY.replace("Friday", "Wednesday"),
                    }),
                },
            );
            docs(cx, model, rec, &mut store, cast, DocsInput::SavePage);
        }
        Kind::Board => {
            window_shell_do(cx, win, |shell, window, cx| {
                shell
                    .docs_view()
                    .update(cx, |docs, cx| docs.focus_body(window, cx))
            });
            press(cx, win, "tab tab");
        }
        Kind::BoardEditing => {
            window_shell_do(cx, win, |shell, window, cx| {
                shell
                    .docs_view()
                    .update(cx, |docs, cx| docs.focus_body(window, cx))
            });
            press(cx, win, "tab enter");
        }
        Kind::History => {
            window_shell_do(cx, win, |shell, window, cx| {
                shell
                    .docs_view()
                    .update(cx, |docs, cx| docs.show_history(window, cx))
            });
            serve(cx, model, rec, &mut store, cast);
            docs(
                cx,
                model,
                rec,
                &mut store,
                cast,
                DocsInput::PreviewVersion(2),
            );
        }
        Kind::Notes => {
            window_shell_do(cx, win, |shell, window, cx| {
                shell
                    .docs_view()
                    .update(cx, |docs, cx| docs.show_notes(window, cx))
            });
        }
        _ => {}
    }
    draw(cx, win);
}
