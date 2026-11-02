//! The history pane: an open page's earlier versions, newest first.

use gpui::{
    AnyElement, Context, FontWeight, IntoElement, ParentElement, Role, Styled, div, prelude::*, px,
};
use jiff::Zoned;
use lazy_client_core::{DocsInput, LoadStatus};
use lazy_protocol::PageVersionSummary;

use super::{DocsView, text};
use crate::form::ButtonKind;
use crate::theme::Theme;
use crate::time_fmt::{ago_label, to_zoned};

use super::page::PANE_WIDTH;

/// What the pane needs from the core.
struct History {
    versions: Vec<PageVersionSummary>,
    load: LoadStatus,
    previewing: Option<u32>,
}

impl DocsView {
    fn history(&self, cx: &gpui::App) -> History {
        match self.model.read(cx).core().open_page() {
            Some(open) => History {
                versions: open.versions.versions.clone(),
                load: open.versions.load.clone(),
                previewing: open.versions.preview.as_ref().map(|v| v.summary.version),
            },
            None => History {
                versions: Vec::new(),
                load: LoadStatus::Idle,
                previewing: None,
            },
        }
    }

    pub(crate) fn history_pane(&mut self, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let colors = theme.colors;
        let history = self.history(cx);
        let now = Zoned::now();
        let rows: Vec<AnyElement> = history
            .versions
            .iter()
            .enumerate()
            .map(|(index, version)| {
                let number = version.version;
                let selected = history.previewing == Some(number);
                let who = version
                    .created_by
                    .map(|user| self.model.read(cx).core().display_name(user).to_owned())
                    .unwrap_or_else(|| "Someone".to_owned());
                let when = ago_label(&to_zoned(version.created_at, &self.zone), &now);
                let this = cx.entity().downgrade();
                let focus = self.stop(&format!("docs-version-{index}"), cx);
                div()
                    .id(("docs-version", index))
                    .debug_selector(move || format!("docs-version-{index}"))
                    .role(Role::ListItem)
                    .aria_label(format!(
                        "{}, {who}, {when}",
                        text::version_label(number, version.reason)
                    ))
                    .track_focus(&focus)
                    .px_3()
                    .py_2()
                    .rounded(theme.radii.medium)
                    .border_1()
                    .border_color(if selected {
                        colors.accent
                    } else {
                        gpui::transparent_black()
                    })
                    .when(selected, |d| d.bg(colors.selection))
                    .when(!selected, |d| d.hover(move |s| s.bg(colors.surface_hover)))
                    .focus_visible(move |s| s.border_color(colors.focus_ring))
                    .cursor_pointer()
                    .on_key_down({
                        let this = this.clone();
                        move |event: &gpui::KeyDownEvent, _, cx| {
                            if matches!(event.keystroke.key.as_str(), "enter" | "space") {
                                this.update(cx, |view, cx| {
                                    view.docs(DocsInput::PreviewVersion(number), cx)
                                })
                                .ok();
                                cx.stop_propagation();
                            }
                        }
                    })
                    .on_click(move |_, _, cx| {
                        this.update(cx, |view, cx| {
                            view.docs(DocsInput::PreviewVersion(number), cx)
                        })
                        .ok();
                    })
                    .child(
                        div()
                            .text_size(theme.type_scale.body)
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(colors.text)
                            .child(text::version_label(number, version.reason)),
                    )
                    .child(
                        div()
                            .text_size(theme.type_scale.small)
                            .text_color(colors.text_muted)
                            .child(format!("{who} · {when}")),
                    )
                    .into_any_element()
            })
            .collect();
        let body = match (&history.load, rows.is_empty()) {
            (LoadStatus::Failed { reason, can_retry }, true) => {
                let retry = can_retry.then(|| {
                    self.button(
                        theme,
                        "docs-history-retry",
                        crate::states::TRY_AGAIN,
                        ButtonKind::Secondary,
                        true,
                        cx,
                        |view, _, cx| view.docs(DocsInput::LoadVersions, cx),
                    )
                });
                div()
                    .debug_selector(|| "docs-history-failed".into())
                    .flex()
                    .flex_col()
                    .gap_2()
                    .p_3()
                    .child(div().text_color(colors.danger).child(format!(
                        "{}: {}",
                        text::HISTORY_FAILED,
                        reason
                    )))
                    .children(retry)
                    .into_any_element()
            }
            (LoadStatus::Loading | LoadStatus::Idle, true) => div()
                .debug_selector(|| "docs-history-loading".into())
                .p_3()
                .text_color(colors.text_muted)
                .child(text::LOADING)
                .into_any_element(),
            (_, true) => div()
                .debug_selector(|| "docs-history-empty".into())
                .p_3()
                .text_size(theme.type_scale.small)
                .text_color(colors.text_muted)
                .child(text::HISTORY_EMPTY)
                .into_any_element(),
            _ => div()
                .id("docs-history-list")
                .debug_selector(|| "docs-history-list".into())
                .role(Role::List)
                .aria_label("Versions")
                .flex_1()
                .min_h_0()
                .overflow_y_scroll()
                .p_2()
                .flex()
                .flex_col()
                .gap_1()
                .children(rows)
                .into_any_element(),
        };
        pane_frame(theme, "docs-history-pane", text::HISTORY, body)
    }
}

/// The frame of a side pane: a title over a scrolling body.
pub(crate) fn pane_frame(
    theme: &Theme,
    selector: &'static str,
    title: &'static str,
    body: AnyElement,
) -> AnyElement {
    let colors = theme.colors;
    div()
        .id(selector)
        .debug_selector(move || selector.into())
        .role(Role::Complementary)
        .aria_label(title)
        .flex_none()
        .w(px(PANE_WIDTH))
        .h_full()
        .flex()
        .flex_col()
        .border_l_1()
        .border_color(colors.border)
        .bg(colors.surface)
        .child(
            div()
                .flex_none()
                .px_4()
                .py_2()
                .border_b_1()
                .border_color(colors.border)
                .font_weight(FontWeight::SEMIBOLD)
                .text_size(theme.type_scale.body)
                .text_color(colors.text)
                .child(title),
        )
        .child(div().flex_1().min_h_0().flex().flex_col().child(body))
        .into_any_element()
}
