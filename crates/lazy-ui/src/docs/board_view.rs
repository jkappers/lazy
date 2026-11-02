//! Drawing a board and handling the pointer and the keyboard on it.
//!
//! The canvas is one tab stop. Inside it the arrow keys, Tab, and a few letters work on the shapes
//! (see [`DocsView::board_key`]); the pointer drags, resizes, connects, and pans. Every change to
//! the board goes through `lazy_client_core::board_edit` and `DocsInput::EditPage`, so the board
//! the screen draws is always one the server accepts.

use gpui::{
    AnyElement, Bounds, Context, FontWeight, Hsla, IntoElement, KeyDownEvent, MouseButton,
    MouseDownEvent, MouseMoveEvent, MouseUpEvent, ParentElement, PathBuilder, Pixels, Point, Role,
    ScrollEvent, Styled, Window, canvas, div, point, prelude::*, px, size,
};
use lazy_client_core::DocsInput;
use lazy_client_core::board_edit::{self, BoardEditError};
use lazy_domain::Board;
use lazy_domain::board::{BoardColor, BoardNode, NodeShape};
use lazy_domain::page::PageContent;

use super::board::{
    self, Corner, Editing, Gesture, HANDLE, NUDGE, PAN_STEP, Selected, View, arrow_head,
    boundary_point, edge_line, ellipse_points, fit_viewport, next_selection, step_zoom, zoom_about,
};
use super::{DocsView, text};
use crate::form::ButtonKind;
use crate::keyboard::{FocusNextControl, FocusPrevControl};
use crate::theme::Theme;

/// What a shape is drawn with.
struct Look {
    fill: Option<Hsla>,
    stroke: Option<Hsla>,
}

/// The colors of a shape, from the theme so light and dark both read well.
fn look(theme: &Theme, node: &BoardNode) -> Look {
    let c = theme.colors;
    if node.shape == NodeShape::Text {
        return Look {
            fill: None,
            stroke: None,
        };
    }
    let (fill, stroke) = match node.color {
        None if node.shape == NodeShape::Note => (c.warning_surface, c.warning),
        None | Some(BoardColor::Neutral) => (c.surface_raised, c.border_strong),
        Some(BoardColor::Red) => (c.danger_surface, c.danger),
        Some(BoardColor::Orange) => (c.warning_surface, c.warning),
        Some(BoardColor::Yellow) => (c.highlight.opacity(0.35), c.highlight),
        Some(BoardColor::Green) => (c.success.opacity(0.16), c.success),
        Some(BoardColor::Blue) => (c.accent_surface, c.accent),
        Some(BoardColor::Purple) => (c.mention_surface, c.mention_text),
    };
    Look {
        fill: Some(fill),
        stroke: Some(stroke),
    }
}

/// The name of a color, for the swatch's label.
pub fn color_name(color: Option<BoardColor>) -> &'static str {
    match color {
        None => "No color",
        Some(BoardColor::Neutral) => "Neutral",
        Some(BoardColor::Red) => "Red",
        Some(BoardColor::Orange) => "Orange",
        Some(BoardColor::Yellow) => "Yellow",
        Some(BoardColor::Green) => "Green",
        Some(BoardColor::Blue) => "Blue",
        Some(BoardColor::Purple) => "Purple",
    }
}

const COLORS: [Option<BoardColor>; 8] = [
    None,
    Some(BoardColor::Neutral),
    Some(BoardColor::Red),
    Some(BoardColor::Orange),
    Some(BoardColor::Yellow),
    Some(BoardColor::Green),
    Some(BoardColor::Blue),
    Some(BoardColor::Purple),
];

fn pt(origin: Point<Pixels>, x: f32, y: f32) -> Point<Pixels> {
    point(origin.x + px(x), origin.y + px(y))
}

impl DocsView {
    // ----- Reading the board -----

    fn board_now(&self, cx: &gpui::App) -> Option<Board> {
        match self.model.read(cx).core().open_page()?.content.as_ref()? {
            PageContent::Board { board } => Some(board.clone()),
            _ => None,
        }
    }

    // ----- Changing the board -----

    /// Applies `edit` to the working board and asks for a save. Returns whether it applied.
    fn edit_board(
        &mut self,
        edit: impl FnOnce(&Board) -> Result<Board, BoardEditError>,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(board) = self.board_now(cx) else {
            return false;
        };
        match edit(&board) {
            Ok(next) if next != board => {
                self.docs(
                    DocsInput::EditPage {
                        title: None,
                        content: Some(PageContent::Board { board: next }),
                    },
                    cx,
                );
                self.schedule_save(cx);
                cx.notify();
                true
            }
            Ok(_) => true,
            Err(_) => false,
        }
    }

    /// Adds a shape at the middle of what is on screen and selects it.
    pub(crate) fn add_shape(&mut self, shape: NodeShape, cx: &mut Context<Self>) {
        let Some(board) = self.board_now(cx) else {
            return;
        };
        let id = board_edit::fresh_id(&board, "n");
        let area = self.board.bounds.get();
        let view = self.board.view();
        let (cx_b, cy_b) = view.to_board(
            f32::from(area.size.width) / 2.0,
            f32::from(area.size.height) / 2.0,
        );
        // Each new shape steps aside from the last so they do not stack exactly.
        let stagger = (board.nodes.len() % 8) as i32 * 16;
        let (w, h) = board_edit::DEFAULT_NODE_SIZE;
        let (x, y) = (cx_b as i32 - w / 2 + stagger, cy_b as i32 - h / 2 + stagger);
        let id_for_select = id.clone();
        if self.edit_board(move |b| board_edit::add_node(b, &id, shape, x, y), cx) {
            self.board.selection = Some(Selected::Node(id_for_select));
        }
    }

    pub(crate) fn delete_selection(&mut self, cx: &mut Context<Self>) {
        match self.board.selection.clone() {
            Some(Selected::Node(id)) => {
                self.edit_board(move |b| board_edit::delete_node(b, &id), cx);
            }
            Some(Selected::Edge(id)) => {
                self.edit_board(move |b| board_edit::delete_edge(b, &id), cx);
            }
            None => return,
        }
        self.board.selection = None;
        cx.notify();
    }

    pub(crate) fn color_selection(&mut self, color: Option<BoardColor>, cx: &mut Context<Self>) {
        if let Some(id) = self.board.selected_node().map(str::to_owned) {
            self.edit_board(move |b| board_edit::set_node_color(b, &id, color), cx);
        }
    }

    pub(crate) fn toggle_connect(&mut self, cx: &mut Context<Self>) {
        self.board.connecting = match (&self.board.connecting, self.board.selected_node()) {
            (Some(_), _) => None,
            (None, Some(id)) => Some(id.to_owned()),
            (None, None) => None,
        };
        cx.notify();
    }

    fn finish_connect(&mut self, target: &str, cx: &mut Context<Self>) {
        let Some(source) = self.board.connecting.take() else {
            return;
        };
        if source != target {
            let Some(board) = self.board_now(cx) else {
                return;
            };
            let id = board_edit::fresh_id(&board, "e");
            let (source, target, keep) = (source, target.to_owned(), id.clone());
            if self.edit_board(move |b| board_edit::connect(b, &id, &source, &target), cx) {
                self.board.selection = Some(Selected::Edge(keep));
            }
        }
        cx.notify();
    }

    pub(crate) fn zoom_by(&mut self, in_: bool, focus: Option<(f32, f32)>, cx: &mut Context<Self>) {
        let old = self.board.zoom;
        let new = step_zoom(old, in_);
        if new == old {
            return;
        }
        let area = self.board.bounds.get();
        let focus = focus.unwrap_or((
            f32::from(area.size.width) / 2.0,
            f32::from(area.size.height) / 2.0,
        ));
        self.board.offset = zoom_about(self.board.offset, focus, old, new);
        self.board.zoom = new;
        cx.notify();
    }

    /// Shows the whole board.
    pub(crate) fn fit_board(&mut self, cx: &mut Context<Self>) {
        let Some(board) = self.board_now(cx) else {
            return;
        };
        let area = self.board.bounds.get().size;
        let viewport = fit_viewport(&board, area);
        self.board.offset = (viewport.x as f32, viewport.y as f32);
        self.board.zoom = viewport.zoom_percent;
        cx.notify();
    }

    pub(crate) fn reset_zoom(&mut self, cx: &mut Context<Self>) {
        let old = self.board.zoom;
        let area = self.board.bounds.get();
        let focus = (
            f32::from(area.size.width) / 2.0,
            f32::from(area.size.height) / 2.0,
        );
        self.board.offset = zoom_about(self.board.offset, focus, old, 100);
        self.board.zoom = 100;
        cx.notify();
    }

    // ----- Editing text on a shape -----

    pub(crate) fn begin_edit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(board) = self.board_now(cx) else {
            return;
        };
        let (editing, current) = match &self.board.selection {
            Some(Selected::Node(id)) => (
                Editing::NodeText(id.clone()),
                board
                    .nodes
                    .iter()
                    .find(|n| n.id == *id)
                    .map(|n| n.text.clone()),
            ),
            Some(Selected::Edge(id)) => (
                Editing::EdgeLabel(id.clone()),
                board
                    .edges
                    .iter()
                    .find(|e| e.id == *id)
                    .map(|e| e.label.clone()),
            ),
            None => return,
        };
        let Some(current) = current else { return };
        self.board.editing = Some(editing);
        self.board
            .input
            .update(cx, |input, cx| input.set_text(&current, cx));
        let focus = self.board.input.read(cx).focus_handle.clone();
        window.focus(&focus, cx);
        cx.notify();
    }

    pub(crate) fn commit_edit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(editing) = self.board.editing.take() else {
            return;
        };
        let value = self.board.input.read(cx).text().to_owned();
        match editing {
            Editing::NodeText(id) => {
                self.edit_board(move |b| board_edit::set_node_text(b, &id, &value), cx);
            }
            Editing::EdgeLabel(id) => {
                self.edit_board(move |b| board_edit::set_edge_label(b, &id, &value), cx);
            }
        }
        let focus = self.board.focus.clone();
        window.focus(&focus, cx);
        cx.notify();
    }

    pub(crate) fn cancel_edit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.board.editing.take().is_some() {
            let focus = self.board.focus.clone();
            window.focus(&focus, cx);
            cx.notify();
        }
    }

    // ----- The pointer -----

    fn pointer_down_node(
        &mut self,
        id: String,
        event: &MouseDownEvent,
        editable: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let focus = self.board.focus.clone();
        window.focus(&focus, cx);
        if self.board.editing.is_some() {
            self.commit_edit(window, cx);
        }
        if editable && self.board.connecting.is_some() {
            self.finish_connect(&id, cx);
            return;
        }
        self.board.selection = Some(Selected::Node(id.clone()));
        if !editable {
            cx.notify();
            return;
        }
        if event.click_count >= 2 {
            self.begin_edit(window, cx);
            return;
        }
        self.board.pointer = event.position;
        self.board.gesture = Some(Gesture::Move {
            id,
            start: event.position,
            moved: false,
        });
        cx.notify();
    }

    fn pointer_down_edge(
        &mut self,
        id: String,
        event: &MouseDownEvent,
        editable: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let focus = self.board.focus.clone();
        window.focus(&focus, cx);
        self.board.selection = Some(Selected::Edge(id));
        if editable && event.click_count >= 2 {
            self.begin_edit(window, cx);
            return;
        }
        cx.notify();
    }

    fn pointer_down_background(
        &mut self,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let focus = self.board.focus.clone();
        window.focus(&focus, cx);
        if self.board.editing.is_some() {
            self.commit_edit(window, cx);
        }
        self.board.selection = None;
        self.board.pointer = event.position;
        self.board.gesture = Some(Gesture::Pan {
            start: event.position,
            from: self.board.offset,
            moved: false,
        });
        cx.notify();
    }

    fn pointer_down_handle(
        &mut self,
        id: String,
        corner: Corner,
        event: &MouseDownEvent,
        cx: &mut Context<Self>,
    ) {
        self.board.pointer = event.position;
        self.board.gesture = Some(Gesture::Resize {
            id,
            corner,
            start: event.position,
        });
        cx.notify();
    }

    fn pointer_move(&mut self, event: &MouseMoveEvent, cx: &mut Context<Self>) {
        let Some(gesture) = self.board.gesture.clone() else {
            return;
        };
        if event.pressed_button != Some(MouseButton::Left) {
            // The button was let go somewhere we did not hear about.
            self.board.gesture = None;
            cx.notify();
            return;
        }
        self.board.pointer = event.position;
        let distance = |a: Point<Pixels>, b: Point<Pixels>| {
            let (dx, dy) = (f32::from(a.x - b.x), f32::from(a.y - b.y));
            (dx * dx + dy * dy).sqrt()
        };
        match gesture {
            Gesture::Move { id, start, moved } => {
                let moved = moved || distance(start, event.position) > board::DRAG_SLOP;
                self.board.gesture = Some(Gesture::Move { id, start, moved });
            }
            Gesture::Pan { start, from, moved } => {
                let moved = moved || distance(start, event.position) > board::DRAG_SLOP;
                if moved {
                    self.board.offset = (
                        from.0 + f32::from(event.position.x - start.x),
                        from.1 + f32::from(event.position.y - start.y),
                    );
                }
                self.board.gesture = Some(Gesture::Pan { start, from, moved });
            }
            Gesture::Resize { .. } => {}
        }
        cx.notify();
    }

    fn pointer_up(&mut self, cx: &mut Context<Self>) {
        let Some(gesture) = self.board.gesture.take() else {
            return;
        };
        let scale = self.board.scale();
        let pointer = self.board.pointer;
        let delta = |start: Point<Pixels>| {
            (
                (f32::from(pointer.x - start.x) / scale).round() as i32,
                (f32::from(pointer.y - start.y) / scale).round() as i32,
            )
        };
        match gesture {
            Gesture::Move {
                id,
                start,
                moved: true,
            } => {
                let (dx, dy) = delta(start);
                self.edit_board(move |b| board_edit::move_node(b, &id, dx, dy), cx);
            }
            Gesture::Resize { id, corner, start } => {
                let (dx, dy) = delta(start);
                self.edit_board(
                    move |b| {
                        let node = b
                            .nodes
                            .iter()
                            .find(|n| n.id == id)
                            .ok_or_else(|| BoardEditError::NoSuchNode(id.clone()))?;
                        let (x, y, w, h) = board::resized(node, corner, dx, dy);
                        let (nx, ny) = (node.x, node.y);
                        let moved = board_edit::move_node(b, &id, x - nx, y - ny)?;
                        board_edit::resize_node(&moved, &id, w, h)
                    },
                    cx,
                );
            }
            _ => {}
        }
        cx.notify();
    }

    fn wheel(&mut self, event: &ScrollEvent, cx: &mut Context<Self>) {
        let delta = event.delta.pixel_delta(px(20.));
        if event.modifiers.control || event.modifiers.platform {
            let origin = self.board.bounds.get().origin;
            let focus = (
                f32::from(event.position.x - origin.x),
                f32::from(event.position.y - origin.y),
            );
            self.zoom_by(f32::from(delta.y) > 0.0, Some(focus), cx);
        } else {
            self.board.offset.0 += f32::from(delta.x);
            self.board.offset.1 += f32::from(delta.y);
            cx.notify();
        }
    }

    // ----- The keyboard -----

    /// Letters, Enter, Delete, and the arrows, on the canvas. Anything with Ctrl, Cmd, or Alt is
    /// left to the shell's shortcuts. Returns whether the key was used.
    pub(crate) fn board_key(
        &mut self,
        event: &KeyDownEvent,
        editable: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let m = event.keystroke.modifiers;
        if m.control || m.platform || m.alt || self.board.editing.is_some() {
            return false;
        }
        let key = event.keystroke.key.as_str();
        match key {
            "left" | "right" | "up" | "down" => {
                let step = if m.shift { 1 } else { NUDGE };
                let (dx, dy) = match key {
                    "left" => (-step, 0),
                    "right" => (step, 0),
                    "up" => (0, -step),
                    _ => (0, step),
                };
                if editable && let Some(id) = self.board.selected_node().map(str::to_owned) {
                    self.edit_board(move |b| board_edit::move_node(b, &id, dx, dy), cx);
                } else {
                    self.board.offset.0 -= dx as f32 * PAN_STEP / NUDGE as f32;
                    self.board.offset.1 -= dy as f32 * PAN_STEP / NUDGE as f32;
                    cx.notify();
                }
                true
            }
            "enter" if !event.is_held => {
                if editable
                    && self.board.connecting.is_some()
                    && let Some(id) = self.board.selected_node().map(str::to_owned)
                {
                    self.finish_connect(&id, cx);
                } else if editable && self.board.selection.is_some() {
                    self.begin_edit(window, cx);
                }
                true
            }
            "delete" | "backspace" if editable && self.board.selection.is_some() => {
                self.delete_selection(cx);
                true
            }
            "r" | "e" | "d" | "s" | "t" if editable => {
                self.add_shape(
                    match key {
                        "r" => NodeShape::Rectangle,
                        "e" => NodeShape::Ellipse,
                        "d" => NodeShape::Diamond,
                        "s" => NodeShape::Note,
                        _ => NodeShape::Text,
                    },
                    cx,
                );
                true
            }
            "c" if editable => {
                self.toggle_connect(cx);
                true
            }
            "+" | "=" => {
                self.zoom_by(true, None, cx);
                true
            }
            "-" => {
                self.zoom_by(false, None, cx);
                true
            }
            digit if editable && digit.len() == 1 && digit.as_bytes()[0].is_ascii_digit() => {
                let index = usize::from(digit.as_bytes()[0] - b'0');
                if let Some(color) = COLORS.get(index) {
                    self.color_selection(*color, cx);
                    true
                } else {
                    false
                }
            }
            _ => false,
        }
    }

    /// Tab and Shift+Tab walk the shapes and connectors, then leave the canvas.
    fn board_tab(&mut self, backwards: bool, window: &mut Window, cx: &mut Context<Self>) {
        let next = self
            .board_now(cx)
            .and_then(|board| next_selection(&board, self.board.selection.as_ref(), backwards));
        match next {
            Some(selection) => {
                self.board.selection = Some(selection);
                cx.notify();
            }
            None => {
                self.board.selection = None;
                if backwards {
                    window.focus_prev(cx);
                } else {
                    window.focus_next(cx);
                }
                cx.notify();
            }
        }
    }

    // ----- Drawing -----

    /// The tools above the canvas: shapes, connect, delete, and color.
    pub(crate) fn board_tools(
        &mut self,
        theme: &Theme,
        editable: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let colors = theme.colors;
        let has_selection = self.board.selection.is_some();
        let has_node = self.board.selected_node().is_some();
        let shapes = [
            ("board-add-rectangle", "Box", NodeShape::Rectangle),
            ("board-add-ellipse", "Oval", NodeShape::Ellipse),
            ("board-add-diamond", "Diamond", NodeShape::Diamond),
            ("board-add-note", "Sticky", NodeShape::Note),
            ("board-add-text", "Text", NodeShape::Text),
        ];
        let mut items: Vec<AnyElement> = shapes
            .into_iter()
            .map(|(id, label, shape)| {
                self.button(
                    theme,
                    id,
                    label,
                    ButtonKind::Quiet,
                    editable,
                    cx,
                    move |view, _, cx| view.add_shape(shape, cx),
                )
            })
            .collect();
        items.push(self.button(
            theme,
            "board-connect",
            if self.board.connecting.is_some() {
                "Pick the other end"
            } else {
                "Connect"
            },
            if self.board.connecting.is_some() {
                ButtonKind::Primary
            } else {
                ButtonKind::Quiet
            },
            editable && (has_node || self.board.connecting.is_some()),
            cx,
            |view, _, cx| view.toggle_connect(cx),
        ));
        items.push(self.button(
            theme,
            "board-delete",
            "Delete",
            ButtonKind::Quiet,
            editable && has_selection,
            cx,
            |view, _, cx| view.delete_selection(cx),
        ));
        let swatches: Vec<AnyElement> = COLORS
            .iter()
            .enumerate()
            .map(|(index, color)| self.swatch(theme, index, *color, editable && has_node, cx))
            .collect();
        div()
            .debug_selector(|| "board-tools".into())
            .flex()
            .flex_wrap()
            .items_center()
            .gap_2()
            .children(items)
            .child(div().w(px(1.)).h(px(20.)).bg(colors.border))
            .children(swatches)
            .into_any_element()
    }

    fn swatch(
        &mut self,
        theme: &Theme,
        index: usize,
        color: Option<BoardColor>,
        enabled: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let c = theme.colors;
        let focus = self.stop(&format!("board-color-{index}"), cx);
        let this = cx.entity().downgrade();
        let key_to = this.clone();
        let probe = BoardNode {
            id: String::new(),
            shape: NodeShape::Rectangle,
            x: 0,
            y: 0,
            width: 1,
            height: 1,
            text: String::new(),
            color,
        };
        let look = look(theme, &probe);
        let chosen = self
            .board_now(cx)
            .and_then(|board| {
                self.board
                    .selected_node()
                    .and_then(|id| board.nodes.iter().find(|n| n.id == id).map(|n| n.color))
            })
            .is_some_and(|current| current == color);
        let ring = c.focus_ring;
        div()
            .id(("board-color", index))
            .debug_selector(move || format!("board-color-{index}"))
            .role(Role::Button)
            .aria_label(format!("Color: {}", color_name(color)))
            .track_focus(&focus)
            .size(px(22.))
            .flex_none()
            .rounded(theme.radii.pill)
            .border_2()
            .border_color(if chosen {
                c.text
            } else {
                look.stroke.unwrap_or(c.border_strong)
            })
            .bg(look.fill.unwrap_or(c.surface))
            .focus_visible(move |s| s.border_color(ring))
            .when(enabled, |d| d.cursor_pointer())
            .when(!enabled, |d| d.opacity(0.4))
            .when(color.is_none(), |d| {
                d.flex()
                    .items_center()
                    .justify_center()
                    .text_color(c.text_muted)
                    .text_size(px(12.))
                    .child("×")
            })
            .on_key_down(move |event: &KeyDownEvent, _, cx| {
                if enabled && crate::keyboard::is_activation(event) {
                    key_to
                        .update(cx, |view, cx| view.color_selection(color, cx))
                        .ok();
                    cx.stop_propagation();
                }
            })
            .on_click(move |_, _, cx| {
                if enabled {
                    this.update(cx, |view, cx| view.color_selection(color, cx))
                        .ok();
                }
            })
            .into_any_element()
    }

    /// A board that cannot be changed, fitted to the room there is (a version preview).
    pub(crate) fn read_only_board(&self, theme: &Theme, board: &Board) -> AnyElement {
        let area = self.board.bounds.get().size;
        let area = if f32::from(area.width) < 50.0 {
            size(px(800.), px(500.))
        } else {
            area
        };
        let viewport = fit_viewport(board, area);
        let view = View {
            offset: (viewport.x as f32, viewport.y as f32),
            scale: f32::from(viewport.zoom_percent) / 100.0,
        };
        let layers = draw_layers(theme, board, view, None, &self.board.bounds, None);
        div()
            .id("docs-version-board")
            .debug_selector(|| "docs-version-board".into())
            .relative()
            .size_full()
            .overflow_hidden()
            .bg(theme.colors.surface)
            .children(layers)
            .into_any_element()
    }

    /// The interactive canvas.
    pub(crate) fn render_board(
        &mut self,
        theme: &Theme,
        working: Option<&PageContent>,
        editable: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let colors = theme.colors;
        let board = match working {
            Some(PageContent::Board { board }) => board.clone(),
            _ => Board::empty(),
        };
        let view = self.board.view();
        let preview = self.gesture_preview(&board);
        let layers = draw_layers(
            theme,
            &board,
            view,
            Some(&self.board),
            &self.board.bounds,
            preview.as_ref(),
        );
        let origin = self.board.bounds.get().origin;
        let this = cx.entity().downgrade();
        let mut children: Vec<AnyElement> = layers;

        // The shapes: the hit targets, the text, and the selection.
        for node in &board.nodes {
            let id = node.id.clone();
            let (x, y, w, h) = match &preview {
                Some(p) if p.id == node.id => (p.x, p.y, p.w, p.h),
                _ => (node.x, node.y, node.width, node.height),
            };
            let (sx, sy) = view.to_screen(x as f32, y as f32);
            let (sw, sh) = (w as f32 * view.scale, h as f32 * view.scale);
            let selected = self.board.selected_node() == Some(node.id.as_str());
            let source = self.board.connecting.as_deref() == Some(node.id.as_str());
            let editing_here =
                matches!(&self.board.editing, Some(Editing::NodeText(e)) if *e == node.id);
            let to = this.clone();
            let handle_to = this.clone();
            let ring = colors.focus_ring;
            let text_size = (theme.type_scale.body * view.scale.clamp(0.6, 2.0)).max(px(9.));
            let node_id = node.id.clone();
            let body = node.text.clone();
            let shape = node.shape;
            let mut element = div()
                .id(("board-node", children.len()))
                .debug_selector({
                    let id = node_id.clone();
                    move || format!("board-node-{id}")
                })
                .role(Role::Group)
                .aria_label(if body.is_empty() {
                    "Empty shape".to_owned()
                } else {
                    body.clone()
                })
                .absolute()
                .left(px(sx))
                .top(px(sy))
                .w(px(sw))
                .h(px(sh))
                .flex()
                .items_center()
                .justify_center()
                .px(px(6. * view.scale))
                .text_size(text_size)
                .text_color(colors.text)
                .when(
                    shape == NodeShape::Rectangle || shape == NodeShape::Note,
                    |d| {
                        let look = look(theme, node);
                        d.rounded(if shape == NodeShape::Note {
                            px(2.)
                        } else {
                            theme.radii.small
                        })
                        .border_1()
                        .border_color(look.stroke.unwrap_or(colors.border_strong))
                        .bg(look.fill.unwrap_or(colors.surface_raised))
                    },
                )
                .when(shape == NodeShape::Text, |d| {
                    d.border_1().border_color(gpui::transparent_black())
                })
                .when(selected, |d| d.border_2().border_color(ring))
                .when(source, |d| d.border_2().border_color(colors.accent))
                .when(editable, |d| d.cursor_pointer())
                .on_mouse_down(MouseButton::Left, {
                    let id = id.clone();
                    move |event, window, cx| {
                        let id = id.clone();
                        to.update(cx, |view, cx| {
                            view.pointer_down_node(id, event, editable, window, cx)
                        })
                        .ok();
                        cx.stop_propagation();
                    }
                })
                .child(
                    div()
                        .max_w_full()
                        .max_h_full()
                        .overflow_hidden()
                        .text_center()
                        .when(editing_here, |d| d.invisible())
                        .child(body),
                );
            if selected && editable && self.board.editing.is_none() {
                for corner in [
                    Corner::NorthWest,
                    Corner::NorthEast,
                    Corner::SouthWest,
                    Corner::SouthEast,
                ] {
                    let to = handle_to.clone();
                    let id = id.clone();
                    let (left, top) = match corner {
                        Corner::NorthWest => (-HANDLE / 2.0, -HANDLE / 2.0),
                        Corner::NorthEast => (sw - HANDLE / 2.0, -HANDLE / 2.0),
                        Corner::SouthWest => (-HANDLE / 2.0, sh - HANDLE / 2.0),
                        Corner::SouthEast => (sw - HANDLE / 2.0, sh - HANDLE / 2.0),
                    };
                    let name = format!("board-handle-{corner:?}");
                    element = element.child(
                        div()
                            .debug_selector(move || name.clone())
                            .absolute()
                            .left(px(left))
                            .top(px(top))
                            .size(px(HANDLE))
                            .rounded(px(2.))
                            .bg(colors.focus_ring)
                            .border_1()
                            .border_color(colors.surface)
                            .cursor_pointer()
                            .on_mouse_down(MouseButton::Left, move |event, _, cx| {
                                let id = id.clone();
                                to.update(cx, |view, cx| {
                                    view.pointer_down_handle(id, corner, event, cx)
                                })
                                .ok();
                                cx.stop_propagation();
                            }),
                    );
                }
            }
            children.push(element.into_any_element());
        }

        // The connectors' handles: a dot in the middle, with the label beside it.
        for edge in &board.edges {
            let Some(((x1, y1), (x2, y2))) = edge_line(&board, edge) else {
                continue;
            };
            let (mx, my) = view.to_screen((x1 + x2) / 2.0, (y1 + y2) / 2.0);
            let selected = self.board.selected_edge() == Some(edge.id.as_str());
            let editing_here =
                matches!(&self.board.editing, Some(Editing::EdgeLabel(e)) if *e == edge.id);
            let to = this.clone();
            let id = edge.id.clone();
            let name = edge.id.clone();
            let label = edge.label.clone();
            children.push(
                div()
                    .id(("board-edge", children.len()))
                    .debug_selector(move || format!("board-edge-{name}"))
                    .role(Role::Button)
                    .aria_label(if label.is_empty() {
                        "Connector".to_owned()
                    } else {
                        format!("Connector: {label}")
                    })
                    .absolute()
                    .left(px(mx - 9.))
                    .top(px(my - 9.))
                    .min_w(px(18.))
                    .h(px(18.))
                    .px_1()
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(theme.radii.pill)
                    .border_1()
                    .border_color(if selected {
                        colors.focus_ring
                    } else {
                        colors.border_strong
                    })
                    .bg(colors.surface)
                    .text_size(px(11.))
                    .text_color(colors.text_muted)
                    .cursor_pointer()
                    .when(selected, |d| d.border_2())
                    .on_mouse_down(MouseButton::Left, move |event, window, cx| {
                        let id = id.clone();
                        to.update(cx, |view, cx| {
                            view.pointer_down_edge(id, event, editable, window, cx)
                        })
                        .ok();
                        cx.stop_propagation();
                    })
                    .child(if editing_here {
                        String::new()
                    } else {
                        edge.label.clone()
                    })
                    .into_any_element(),
            );
        }

        // The text field over the shape or connector being edited.
        if let Some(editing) = &self.board.editing {
            let (left, top, width) = match editing {
                Editing::NodeText(id) => board.nodes.iter().find(|n| n.id == *id).map(|n| {
                    let (x, y) = view.to_screen(n.x as f32, n.y as f32);
                    (
                        x,
                        y + n.height as f32 * view.scale / 2.0 - 16.0,
                        (n.width as f32 * view.scale).max(140.),
                    )
                }),
                Editing::EdgeLabel(id) => board.edges.iter().find(|e| e.id == *id).and_then(|e| {
                    edge_line(&board, e).map(|((x1, y1), (x2, y2))| {
                        let (mx, my) = view.to_screen((x1 + x2) / 2.0, (y1 + y2) / 2.0);
                        (mx - 70., my + 14., 140.)
                    })
                }),
            }
            .unwrap_or((8., 8., 140.));
            children.push(
                div()
                    .debug_selector(|| "board-text-edit".into())
                    .absolute()
                    .left(px(left))
                    .top(px(top))
                    .w(px(width))
                    .px_2()
                    .py_1()
                    .rounded(theme.radii.small)
                    .bg(colors.surface_raised)
                    .border_2()
                    .border_color(colors.focus_ring)
                    .child(self.board.input.clone())
                    .into_any_element(),
            );
        }

        // Zoom controls in the corner.
        let zoom_out = self.button(
            theme,
            "board-zoom-out",
            "−",
            ButtonKind::Quiet,
            true,
            cx,
            |view, _, cx| view.zoom_by(false, None, cx),
        );
        let zoom_in = self.button(
            theme,
            "board-zoom-in",
            "+",
            ButtonKind::Quiet,
            true,
            cx,
            |view, _, cx| view.zoom_by(true, None, cx),
        );
        let zoom_reset = self.button(
            theme,
            "board-zoom-reset",
            format!("{}%", self.board.zoom),
            ButtonKind::Quiet,
            true,
            cx,
            |view, _, cx| view.reset_zoom(cx),
        );
        let fit = self.button(
            theme,
            "board-fit",
            "Fit",
            ButtonKind::Quiet,
            true,
            cx,
            |view, _, cx| view.fit_board(cx),
        );
        children.push(
            div()
                .absolute()
                .right(px(12.))
                .bottom(px(12.))
                .flex()
                .items_center()
                .gap_1()
                .child(fit)
                .child(zoom_out)
                .child(zoom_reset)
                .child(zoom_in)
                .into_any_element(),
        );
        if board.nodes.is_empty() {
            children.push(
                div()
                    .debug_selector(|| "board-empty".into())
                    .absolute()
                    .inset_0()
                    .flex()
                    .items_center()
                    .justify_center()
                    .text_color(colors.text_faint)
                    .child(if editable {
                        "An empty board. Add a shape from the toolbar, or press R."
                    } else {
                        "An empty board."
                    })
                    .into_any_element(),
            );
        }

        let _ = origin;
        let down_to = this.clone();
        let move_to = this.clone();
        let up_to = this.clone();
        let up_out_to = this.clone();
        let wheel_to = this.clone();
        let key_to = this.clone();
        let focused = self.board.focus.is_focused(window);
        div()
            .id("docs-board")
            .debug_selector(|| "docs-board".into())
            .role(Role::Group)
            .aria_label(
                "Board canvas. Tab walks the shapes, arrow keys move one, Enter edits its text.",
            )
            .track_focus(&self.board.focus)
            .relative()
            .size_full()
            .overflow_hidden()
            .bg(colors.surface)
            .border_2()
            .border_color(if focused {
                colors.focus_ring
            } else {
                gpui::transparent_black()
            })
            .on_action(cx.listener(|this, _: &FocusNextControl, window, cx| {
                if this.board.editing.is_some() {
                    window.focus_next(cx);
                } else {
                    this.board_tab(false, window, cx);
                }
            }))
            .on_action(cx.listener(|this, _: &FocusPrevControl, window, cx| {
                if this.board.editing.is_some() {
                    window.focus_prev(cx);
                } else {
                    this.board_tab(true, window, cx);
                }
            }))
            .on_mouse_down(MouseButton::Left, move |event, window, cx| {
                down_to
                    .update(cx, |view, cx| {
                        view.pointer_down_background(event, window, cx)
                    })
                    .ok();
            })
            .on_mouse_move(move |event, _, cx| {
                move_to
                    .update(cx, |view, cx| view.pointer_move(event, cx))
                    .ok();
            })
            .on_mouse_up(MouseButton::Left, move |_: &MouseUpEvent, _, cx| {
                up_to.update(cx, |view, cx| view.pointer_up(cx)).ok();
            })
            .on_mouse_up_out(MouseButton::Left, move |_: &MouseUpEvent, _, cx| {
                up_out_to.update(cx, |view, cx| view.pointer_up(cx)).ok();
            })
            .on_scroll(move |event, _, cx| {
                wheel_to.update(cx, |view, cx| view.wheel(event, cx)).ok();
            })
            .on_key_down(move |event: &KeyDownEvent, window, cx| {
                let used = key_to
                    .update(cx, |view, cx| view.board_key(event, editable, window, cx))
                    .unwrap_or(false);
                if used {
                    cx.stop_propagation();
                }
            })
            .children(children)
            .into_any_element()
    }

    /// Where the shape being dragged or resized is drawn now.
    fn gesture_preview(&self, board: &Board) -> Option<Preview> {
        let scale = self.board.scale();
        let pointer = self.board.pointer;
        let delta = |start: Point<Pixels>| {
            (
                (f32::from(pointer.x - start.x) / scale).round() as i32,
                (f32::from(pointer.y - start.y) / scale).round() as i32,
            )
        };
        match self.board.gesture.as_ref()? {
            Gesture::Move {
                id,
                start,
                moved: true,
            } => {
                let node = board.nodes.iter().find(|n| n.id == *id)?;
                let (dx, dy) = delta(*start);
                Some(Preview {
                    id: id.clone(),
                    x: node.x + dx,
                    y: node.y + dy,
                    w: node.width,
                    h: node.height,
                })
            }
            Gesture::Resize { id, corner, start } => {
                let node = board.nodes.iter().find(|n| n.id == *id)?;
                let (dx, dy) = delta(*start);
                let (x, y, w, h) = board::resized(node, *corner, dx, dy);
                Some(Preview {
                    id: id.clone(),
                    x,
                    y,
                    w,
                    h,
                })
            }
            _ => None,
        }
    }
}

/// A shape's place while it is being dragged or resized.
struct Preview {
    id: String,
    x: i32,
    y: i32,
    w: i32,
    h: i32,
}

/// The painted layer under the shapes: outlines of ovals and diamonds, and the connectors.
fn draw_layers(
    theme: &Theme,
    board: &Board,
    view: View,
    ui: Option<&board::BoardUi>,
    bounds_cell: &std::rc::Rc<std::cell::Cell<Bounds<Pixels>>>,
    preview: Option<&Preview>,
) -> Vec<AnyElement> {
    let colors = theme.colors;
    let mut nodes: Vec<BoardNode> = board.nodes.clone();
    if let Some(p) = preview
        && let Some(node) = nodes.iter_mut().find(|n| n.id == p.id)
    {
        node.x = p.x;
        node.y = p.y;
        node.width = p.w;
        node.height = p.h;
    }
    let drawn = Board {
        nodes: nodes.clone(),
        edges: board.edges.clone(),
        viewport: board.viewport,
    };
    let looks: Vec<(BoardNode, Option<Hsla>, Option<Hsla>)> = nodes
        .into_iter()
        .map(|node| {
            let l = look(theme, &node);
            (node, l.fill, l.stroke)
        })
        .collect();
    let selected_edge = ui.and_then(|ui| ui.selected_edge().map(str::to_owned));
    let line_color = colors.text_muted;
    let selected_color = colors.focus_ring;
    let cell = bounds_cell.clone();
    let layer = canvas(
        move |bounds, _window, _cx| {
            cell.set(bounds);
            bounds
        },
        move |_, bounds, window, _cx| {
            let o = bounds.origin;
            // Connectors first, so shapes sit on them.
            for edge in &drawn.edges {
                let Some(((x1, y1), (x2, y2))) = edge_line(&drawn, edge) else {
                    continue;
                };
                let color = if selected_edge.as_deref() == Some(edge.id.as_str()) {
                    selected_color
                } else {
                    line_color
                };
                let (a, b) = (view.to_screen(x1, y1), view.to_screen(x2, y2));
                let mut line =
                    PathBuilder::stroke(px(if color == selected_color { 2.5 } else { 1.5 }));
                line.move_to(pt(o, a.0, a.1));
                line.line_to(pt(o, b.0, b.1));
                if let Ok(path) = line.build() {
                    window.paint_path(path, color);
                }
                let head = arrow_head(a, b, 12.0);
                let mut tri = PathBuilder::fill();
                tri.move_to(pt(o, head[0].0, head[0].1));
                tri.line_to(pt(o, head[1].0, head[1].1));
                tri.line_to(pt(o, head[2].0, head[2].1));
                tri.close();
                if let Ok(path) = tri.build() {
                    window.paint_path(path, color);
                }
            }
            for (node, fill, stroke) in &looks {
                let outline: Vec<(f32, f32)> = match node.shape {
                    NodeShape::Ellipse => {
                        let (cx, cy) = board::center(node);
                        ellipse_points(
                            cx,
                            cy,
                            node.width as f32 / 2.0,
                            node.height as f32 / 2.0,
                            48,
                        )
                    }
                    NodeShape::Diamond => {
                        let (cx, cy) = board::center(node);
                        let (hw, hh) = (node.width as f32 / 2.0, node.height as f32 / 2.0);
                        vec![(cx, cy - hh), (cx + hw, cy), (cx, cy + hh), (cx - hw, cy)]
                    }
                    _ => continue,
                };
                let points: Vec<(f32, f32)> = outline
                    .iter()
                    .map(|(x, y)| view.to_screen(*x, *y))
                    .collect();
                if let Some(fill) = fill {
                    let mut path = PathBuilder::fill();
                    for (i, (x, y)) in points.iter().enumerate() {
                        if i == 0 {
                            path.move_to(pt(o, *x, *y));
                        } else {
                            path.line_to(pt(o, *x, *y));
                        }
                    }
                    path.close();
                    if let Ok(path) = path.build() {
                        window.paint_path(path, *fill);
                    }
                }
                if let Some(stroke) = stroke {
                    let mut path = PathBuilder::stroke(px(1.5));
                    for (i, (x, y)) in points.iter().enumerate() {
                        if i == 0 {
                            path.move_to(pt(o, *x, *y));
                        } else {
                            path.line_to(pt(o, *x, *y));
                        }
                    }
                    path.close();
                    if let Ok(path) = path.build() {
                        window.paint_path(path, *stroke);
                    }
                }
            }
        },
    )
    .absolute()
    .size_full();
    let mut out = vec![layer.into_any_element()];
    if ui.is_none() {
        // A read-only board draws its rectangles and text itself: there are no handlers to hang
        // them on, so plain boxes do.
        for node in &board.nodes {
            let l = look(theme, node);
            let (sx, sy) = view.to_screen(node.x as f32, node.y as f32);
            let (sw, sh) = (
                node.width as f32 * view.scale,
                node.height as f32 * view.scale,
            );
            out.push(
                div()
                    .absolute()
                    .left(px(sx))
                    .top(px(sy))
                    .w(px(sw))
                    .h(px(sh))
                    .flex()
                    .items_center()
                    .justify_center()
                    .px(px(4.))
                    .text_size((theme.type_scale.small * view.scale.clamp(0.7, 1.5)).max(px(8.)))
                    .text_color(colors.text)
                    .overflow_hidden()
                    .when(
                        matches!(node.shape, NodeShape::Rectangle | NodeShape::Note),
                        |d| {
                            d.rounded(theme.radii.small)
                                .border_1()
                                .border_color(l.stroke.unwrap_or(colors.border_strong))
                                .bg(l.fill.unwrap_or(colors.surface_raised))
                        },
                    )
                    .child(
                        div()
                            .text_center()
                            .font_weight(FontWeight::NORMAL)
                            .child(node.text.clone()),
                    )
                    .into_any_element(),
            );
        }
    }
    let _ = (text::BACK, boundary_point);
    out
}
