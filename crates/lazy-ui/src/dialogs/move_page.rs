//! Chooses the conversation a page or board moves to. It lists the conversations the person is in
//! and can post to; the page's notes, files, and history go with it.

use gpui::{
    Context, Entity, EventEmitter, FocusHandle, IntoElement, ParentElement, Render, Styled, Window,
    div, prelude::*, px,
};
use lazy_client_core::{DocsInput, Input};
use lazy_domain::{ConversationId, PageId};

use crate::app_model::AppModel;
use crate::form::{self, ButtonKind};
use crate::modal::{DialogDismiss, DialogEvent, DialogNext, DialogPrev, ModalTrap, dialog_overlay};
use crate::theme::Theme;

pub struct MovePageDialog {
    model: Entity<AppModel>,
    page: PageId,
    choices: Vec<(ConversationId, String)>,
    handles: Vec<FocusHandle>,
    trap: ModalTrap,
    cancel_focus: FocusHandle,
}

impl EventEmitter<DialogEvent> for MovePageDialog {}

impl MovePageDialog {
    pub fn new(
        model: Entity<AppModel>,
        page: PageId,
        from: ConversationId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let choices: Vec<(ConversationId, String)> = {
            let core = model.read(cx).core();
            let sidebar = core.sidebar();
            sidebar
                .favorites
                .iter()
                .chain(&sidebar.channels)
                .chain(&sidebar.direct_and_group)
                .copied()
                .filter(|id| *id != from)
                .filter(|id| core.conversation(*id).is_some_and(|c| !c.archived))
                .map(|id| (id, core.conversation_title(id)))
                .collect()
        };
        let handles: Vec<FocusHandle> = choices
            .iter()
            .map(|_| cx.focus_handle().tab_stop(true))
            .collect();
        let mut trap = ModalTrap::new(cx);
        let cancel_focus = cx.focus_handle().tab_stop(true);
        match handles.first() {
            Some(first) => trap.open(first, window, cx),
            None => trap.open(&cancel_focus, window, cx),
        }
        Self {
            model,
            page,
            choices,
            handles,
            trap,
            cancel_focus,
        }
    }

    fn choose(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some((conversation, _)) = self.choices.get(index).cloned() else {
            return;
        };
        let page = self.page;
        self.model.update(cx, |model, cx| {
            model.dispatch(Input::Docs(DocsInput::MovePage { page, conversation }), cx)
        });
        self.dismiss(window, cx);
    }

    fn dismiss(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.trap.close(window, cx);
        cx.emit(DialogEvent::Dismiss);
    }
}

impl Render for MovePageDialog {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = *Theme::get(cx);
        let colors = theme.colors;
        let this = cx.entity().downgrade();
        let close_to = this.clone();
        let cancel_to = this.clone();
        let list = div()
            .id("move-page-list")
            .debug_selector(|| "move-page-list".into())
            .max_h(px(280.))
            .overflow_y_scroll()
            .flex()
            .flex_col()
            .gap_1()
            .children(self.choices.iter().enumerate().map(|(index, (_, name))| {
                let to = this.clone();
                form::button(
                    &theme,
                    format!("move-page-choice-{index}"),
                    name.clone(),
                    ButtonKind::Secondary,
                    true,
                    &self.handles[index],
                    move |window, cx| {
                        to.update(cx, |dialog, cx| dialog.choose(index, window, cx))
                            .ok();
                    },
                )
            }));
        let body = div()
            .flex()
            .flex_col()
            .gap_3()
            .child(
                div()
                    .text_size(theme.type_scale.body)
                    .text_color(colors.text_muted)
                    .child(if self.choices.is_empty() {
                        "You're not in another conversation that can hold it."
                    } else {
                        "Its notes, files, and history move with it. Everyone in the new conversation can read it."
                    }),
            )
            .children((!self.choices.is_empty()).then_some(list))
            .child(
                div().flex().justify_end().child(form::button(
                    &theme,
                    "move-page-cancel",
                    "Cancel",
                    ButtonKind::Secondary,
                    true,
                    &self.cancel_focus,
                    move |window, cx| {
                        cancel_to
                            .update(cx, |dialog, cx| dialog.dismiss(window, cx))
                            .ok();
                    },
                )),
            )
            .into_any_element();
        dialog_overlay(
            &theme,
            "Move to another conversation",
            440.,
            &self.trap,
            body,
            "Close",
            move |window, cx| {
                close_to
                    .update(cx, |dialog, cx| dialog.dismiss(window, cx))
                    .ok();
            },
        )
        .on_action(cx.listener(|this, _: &DialogDismiss, window, cx| this.dismiss(window, cx)))
        .on_action(cx.listener(|this, _: &DialogNext, window, cx| this.trap.tab(false, window, cx)))
        .on_action(cx.listener(|this, _: &DialogPrev, window, cx| this.trap.tab(true, window, cx)))
    }
}
