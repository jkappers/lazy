//! Asks before deleting a page from the trash for good. The text says what goes with it and that
//! it cannot be undone, and the keyboard starts on Cancel.

use gpui::{
    Context, Entity, EventEmitter, FocusHandle, IntoElement, ParentElement, Render, Styled, Window,
    div, prelude::*,
};
use lazy_client_core::{DocsInput, Input};
use lazy_domain::PageId;

use crate::app_model::AppModel;
use crate::docs::text;
use crate::form::{self, ButtonKind};
use crate::modal::{DialogDismiss, DialogEvent, DialogNext, DialogPrev, ModalTrap, dialog_overlay};
use crate::theme::Theme;

pub struct PurgePageDialog {
    model: Entity<AppModel>,
    page: PageId,
    title: String,
    trap: ModalTrap,
    cancel_focus: FocusHandle,
    delete_focus: FocusHandle,
}

impl EventEmitter<DialogEvent> for PurgePageDialog {}

impl PurgePageDialog {
    pub fn new(
        model: Entity<AppModel>,
        page: PageId,
        title: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let mut trap = ModalTrap::new(cx);
        let cancel_focus = cx.focus_handle().tab_stop(true);
        trap.open(&cancel_focus, window, cx);
        Self {
            model,
            page,
            title,
            trap,
            cancel_focus,
            delete_focus: cx.focus_handle().tab_stop(true),
        }
    }

    fn delete(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let page = self.page;
        self.model.update(cx, |model, cx| {
            model.dispatch(Input::Docs(DocsInput::PurgePage(page)), cx)
        });
        self.dismiss(window, cx);
    }

    fn dismiss(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.trap.close(window, cx);
        cx.emit(DialogEvent::Dismiss);
    }
}

impl Render for PurgePageDialog {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = *Theme::get(cx);
        let colors = theme.colors;
        let this = cx.entity().downgrade();
        let close_to = this.clone();
        let cancel_to = this.clone();
        let delete_to = this;
        let body = div()
            .flex()
            .flex_col()
            .gap_3()
            .child(
                div()
                    .text_size(theme.type_scale.body)
                    .text_color(colors.text)
                    .child(text::PURGE_DETAIL),
            )
            .child(
                div()
                    .flex()
                    .justify_end()
                    .gap_2()
                    .child(form::button(
                        &theme,
                        "purge-page-cancel",
                        "Cancel",
                        ButtonKind::Secondary,
                        true,
                        &self.cancel_focus,
                        move |window, cx| {
                            cancel_to
                                .update(cx, |dialog, cx| dialog.dismiss(window, cx))
                                .ok();
                        },
                    ))
                    .child(form::button(
                        &theme,
                        "purge-page-confirm",
                        text::DELETE_FOREVER,
                        ButtonKind::Danger,
                        true,
                        &self.delete_focus,
                        move |window, cx| {
                            delete_to
                                .update(cx, |dialog, cx| dialog.delete(window, cx))
                                .ok();
                        },
                    )),
            )
            .into_any_element();
        dialog_overlay(
            &theme,
            text::purge_question(&self.title),
            460.,
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
