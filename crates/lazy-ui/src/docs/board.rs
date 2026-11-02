//! The board canvas's state and its geometry.
//!
//! Everything in the board itself (shapes, connectors, text) is the core's working copy and is
//! changed only through `lazy_client_core::board_edit`. What lives here is what the *screen* adds:
//! which shape is selected, a drag or a pan in progress, the connector being drawn, the shape
//! whose text is being edited, and where the canvas is looking. The viewport is the screen's
//! alone (panning and zooming do not make a new revision of the board), starting from the one the
//! board was saved with.
//!
//! # Coordinates
//!
//! A shape's place is in board pixels. On screen, `screen = board * zoom + offset`, where `offset`
//! is in screen pixels and `zoom` is the viewport's percent over 100. The geometry below is pure
//! and tested without a window.

use std::cell::Cell;
use std::rc::Rc;

use gpui::{
    AppContext as _, Bounds, Context, Entity, FocusHandle, Pixels, Point, Size, Window, point, px,
    size,
};
use lazy_client_core::OpenPage;
use lazy_domain::Board;
use lazy_domain::board::{BoardNode, MAX_ZOOM_PERCENT, MIN_ZOOM_PERCENT, NodeShape, Viewport};
use lazy_domain::page::PageContent;

use super::DocsView;
use crate::text_input::{TextInput, TextInputOptions};

/// How far an arrow key nudges a shape, in board pixels.
pub const NUDGE: i32 = 10;
/// How far an arrow key pans the canvas with nothing selected, in screen pixels.
pub const PAN_STEP: f32 = 48.0;
/// The size of a resize handle on screen.
pub const HANDLE: f32 = 10.0;
/// A zoom step, in percent.
pub const ZOOM_STEP: u16 = 10;
/// Pointer movement under this many pixels is a click, not a drag.
pub const DRAG_SLOP: f32 = 3.0;

/// What is selected.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Selected {
    Node(String),
    Edge(String),
}

/// Which corner of a shape a resize drag holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Corner {
    NorthWest,
    NorthEast,
    SouthWest,
    SouthEast,
}

/// A pointer gesture in progress.
#[derive(Debug, Clone, PartialEq)]
pub enum Gesture {
    /// Moving a shape.
    Move {
        id: String,
        start: Point<Pixels>,
        moved: bool,
    },
    /// Resizing a shape from a corner.
    Resize {
        id: String,
        corner: Corner,
        start: Point<Pixels>,
    },
    /// Panning the canvas.
    Pan {
        start: Point<Pixels>,
        from: (f32, f32),
        moved: bool,
    },
}

/// What the text field over the canvas is editing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Editing {
    NodeText(String),
    EdgeLabel(String),
}

/// The screen state of one board.
pub struct BoardUi {
    pub(crate) focus: FocusHandle,
    pub selection: Option<Selected>,
    /// The shape a connector is being drawn from.
    pub connecting: Option<String>,
    pub(crate) gesture: Option<Gesture>,
    /// Where the pointer is now, during a gesture.
    pub(crate) pointer: Point<Pixels>,
    pub editing: Option<Editing>,
    pub(crate) input: Entity<TextInput>,
    /// The canvas's place in the window, from the last paint.
    pub(crate) bounds: Rc<Cell<Bounds<Pixels>>>,
    /// Where the canvas looks: offset in screen pixels, and zoom in percent.
    pub(crate) offset: (f32, f32),
    pub(crate) zoom: u16,
    /// The page the viewport was taken from.
    page: Option<lazy_domain::PageId>,
}

impl BoardUi {
    pub fn new(_window: &mut Window, cx: &mut Context<DocsView>) -> Self {
        let input = cx.new(|cx| {
            TextInput::new(
                TextInputOptions::new("Text")
                    .accessible_label("Text of the shape")
                    .single_line(true)
                    .max_bytes(lazy_domain::board::MAX_NODE_TEXT_BYTES),
                cx,
            )
        });
        Self {
            focus: cx.focus_handle().tab_stop(true),
            selection: None,
            connecting: None,
            gesture: None,
            pointer: point(px(0.), px(0.)),
            editing: None,
            input,
            bounds: Rc::new(Cell::new(Bounds::default())),
            offset: (0., 0.),
            zoom: 100,
            page: None,
        }
    }

    pub fn focus_handle(&self) -> &FocusHandle {
        &self.focus
    }

    pub fn input(&self) -> &Entity<TextInput> {
        &self.input
    }

    pub fn zoom_percent(&self) -> u16 {
        self.zoom
    }

    pub fn offset(&self) -> (f32, f32) {
        self.offset
    }

    /// Forgets the selection, gesture, and viewport (another page is opening).
    pub fn reset(&mut self) {
        self.selection = None;
        self.connecting = None;
        self.gesture = None;
        self.editing = None;
        self.offset = (0., 0.);
        self.zoom = 100;
        self.page = None;
    }

    /// Follows the core: takes the saved viewport when a different board opens, and drops a
    /// selection or edit whose shape is gone.
    pub fn sync(&mut self, open: Option<&OpenPage>) {
        let Some(open) = open else { return };
        let Some(PageContent::Board { board }) = &open.content else {
            return;
        };
        if self.page != Some(open.id) {
            self.page = Some(open.id);
            self.offset = (board.viewport.x as f32, board.viewport.y as f32);
            self.zoom = board.viewport.zoom_percent;
        }
        let exists = |selected: &Selected| match selected {
            Selected::Node(id) => board.nodes.iter().any(|node| node.id == *id),
            Selected::Edge(id) => board.edges.iter().any(|edge| edge.id == *id),
        };
        if self.selection.as_ref().is_some_and(|s| !exists(s)) {
            self.selection = None;
            self.editing = None;
        }
        if self
            .connecting
            .as_ref()
            .is_some_and(|id| !board.nodes.iter().any(|node| node.id == *id))
        {
            self.connecting = None;
        }
    }

    /// Escape: drops whatever is in progress. Returns whether anything was.
    pub fn cancel_gesture(&mut self) -> bool {
        let busy = self.gesture.is_some() || self.connecting.is_some() || self.editing.is_some();
        self.gesture = None;
        self.connecting = None;
        self.editing = None;
        busy
    }

    pub fn is_editing(&self) -> bool {
        self.editing.is_some()
    }

    /// The selected shape's id.
    pub fn selected_node(&self) -> Option<&str> {
        match &self.selection {
            Some(Selected::Node(id)) => Some(id),
            _ => None,
        }
    }

    pub fn selected_edge(&self) -> Option<&str> {
        match &self.selection {
            Some(Selected::Edge(id)) => Some(id),
            _ => None,
        }
    }

    /// The zoom as a factor.
    pub fn scale(&self) -> f32 {
        f32::from(self.zoom) / 100.0
    }

    /// The canvas as it looks now, for painting and hit testing.
    pub fn view(&self) -> View {
        View {
            offset: self.offset,
            scale: self.scale(),
        }
    }
}

/// Where the canvas looks.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct View {
    pub offset: (f32, f32),
    pub scale: f32,
}

impl View {
    /// Board point to canvas point.
    pub fn to_screen(self, x: f32, y: f32) -> (f32, f32) {
        (
            x * self.scale + self.offset.0,
            y * self.scale + self.offset.1,
        )
    }

    /// Canvas point to board point.
    pub fn to_board(self, x: f32, y: f32) -> (f32, f32) {
        (
            (x - self.offset.0) / self.scale,
            (y - self.offset.1) / self.scale,
        )
    }
}

/// The zoom after one step in or out, kept in range.
pub fn step_zoom(zoom: u16, in_: bool) -> u16 {
    if in_ {
        (zoom + ZOOM_STEP).min(MAX_ZOOM_PERCENT)
    } else {
        zoom.saturating_sub(ZOOM_STEP).max(MIN_ZOOM_PERCENT)
    }
}

/// The offset that keeps the canvas point `focus` over the same board point when the zoom changes
/// from `old` to `new` percent.
pub fn zoom_about(offset: (f32, f32), focus: (f32, f32), old: u16, new: u16) -> (f32, f32) {
    let ratio = f32::from(new) / f32::from(old);
    (
        focus.0 - (focus.0 - offset.0) * ratio,
        focus.1 - (focus.1 - offset.1) * ratio,
    )
}

/// The viewport that shows the whole board in `area`, with a margin; for previews.
pub fn fit_viewport(board: &Board, area: Size<Pixels>) -> Viewport {
    let (w, h) = (
        f32::from(area.width).max(1.0),
        f32::from(area.height).max(1.0),
    );
    let Some((min_x, min_y, max_x, max_y)) = extent(board) else {
        return Viewport::default();
    };
    let margin = 24.0;
    let (bw, bh) = ((max_x - min_x).max(1.0), (max_y - min_y).max(1.0));
    let scale = ((w - 2.0 * margin) / bw)
        .min((h - 2.0 * margin) / bh)
        .clamp(
            f32::from(MIN_ZOOM_PERCENT) / 100.0,
            f32::from(MAX_ZOOM_PERCENT) / 100.0,
        )
        .min(1.0);
    let zoom = ((scale * 100.0).floor() as u16).clamp(MIN_ZOOM_PERCENT, MAX_ZOOM_PERCENT);
    let scale = f32::from(zoom) / 100.0;
    Viewport {
        x: ((w - bw * scale) / 2.0 - min_x * scale).round() as i32,
        y: ((h - bh * scale) / 2.0 - min_y * scale).round() as i32,
        zoom_percent: zoom,
    }
}

/// The smallest rectangle holding every shape: (left, top, right, bottom).
pub fn extent(board: &Board) -> Option<(f32, f32, f32, f32)> {
    let mut nodes = board.nodes.iter();
    let first = nodes.next()?;
    let mut out = (
        first.x as f32,
        first.y as f32,
        (first.x + first.width) as f32,
        (first.y + first.height) as f32,
    );
    for node in nodes {
        out.0 = out.0.min(node.x as f32);
        out.1 = out.1.min(node.y as f32);
        out.2 = out.2.max((node.x + node.width) as f32);
        out.3 = out.3.max((node.y + node.height) as f32);
    }
    Some(out)
}

/// Where the line from a shape's center towards `target` leaves the shape's outline.
pub fn boundary_point(node: &BoardNode, target: (f32, f32)) -> (f32, f32) {
    let center = (
        node.x as f32 + node.width as f32 / 2.0,
        node.y as f32 + node.height as f32 / 2.0,
    );
    let (hw, hh) = (node.width as f32 / 2.0, node.height as f32 / 2.0);
    let (dx, dy) = (target.0 - center.0, target.1 - center.1);
    if dx.abs() < f32::EPSILON && dy.abs() < f32::EPSILON {
        return center;
    }
    let t = match node.shape {
        NodeShape::Ellipse => 1.0 / ((dx / hw).powi(2) + (dy / hh).powi(2)).sqrt(),
        NodeShape::Diamond => 1.0 / (dx.abs() / hw + dy.abs() / hh),
        _ => {
            let tx = if dx.abs() > f32::EPSILON {
                hw / dx.abs()
            } else {
                f32::INFINITY
            };
            let ty = if dy.abs() > f32::EPSILON {
                hh / dy.abs()
            } else {
                f32::INFINITY
            };
            tx.min(ty)
        }
    };
    (center.0 + dx * t, center.1 + dy * t)
}

/// The middle of a shape, in board pixels.
pub fn center(node: &BoardNode) -> (f32, f32) {
    (
        node.x as f32 + node.width as f32 / 2.0,
        node.y as f32 + node.height as f32 / 2.0,
    )
}

/// Both ends of a connector, on the two outlines, in board pixels. `None` if an end is missing.
pub fn edge_line(board: &Board, edge: &lazy_domain::BoardEdge) -> Option<((f32, f32), (f32, f32))> {
    let source = board.nodes.iter().find(|n| n.id == edge.source)?;
    let target = board.nodes.iter().find(|n| n.id == edge.target)?;
    Some((
        boundary_point(source, center(target)),
        boundary_point(target, center(source)),
    ))
}

/// The shape after `selection` in tab order (shapes, then connectors), or `None` past the end (so
/// Tab can leave the canvas). `backwards` walks the other way.
pub fn next_selection(
    board: &Board,
    selection: Option<&Selected>,
    backwards: bool,
) -> Option<Selected> {
    let order: Vec<Selected> = board
        .nodes
        .iter()
        .map(|n| Selected::Node(n.id.clone()))
        .chain(board.edges.iter().map(|e| Selected::Edge(e.id.clone())))
        .collect();
    if order.is_empty() {
        return None;
    }
    let position = selection.and_then(|s| order.iter().position(|o| o == s));
    match (position, backwards) {
        (None, false) => order.first().cloned(),
        (None, true) => order.last().cloned(),
        (Some(i), false) => order.get(i + 1).cloned(),
        (Some(0), true) => None,
        (Some(i), true) => order.get(i - 1).cloned(),
    }
}

/// The corner of the shape at `size` on screen that `point` (relative to the shape) is within
/// [`HANDLE`] of, if any.
pub fn corner_of(size: (f32, f32), at: (f32, f32)) -> Option<Corner> {
    let near = |a: f32, b: f32| (a - b).abs() <= HANDLE;
    match (
        near(at.0, 0.0),
        near(at.0, size.0),
        near(at.1, 0.0),
        near(at.1, size.1),
    ) {
        (true, _, true, _) => Some(Corner::NorthWest),
        (_, true, true, _) => Some(Corner::NorthEast),
        (true, _, _, true) => Some(Corner::SouthWest),
        (_, true, _, true) => Some(Corner::SouthEast),
        _ => None,
    }
}

/// The new place and size of a shape resized from `corner` by (`dx`, `dy`) board pixels, never
/// smaller than the minimum.
pub fn resized(node: &BoardNode, corner: Corner, dx: i32, dy: i32) -> (i32, i32, i32, i32) {
    use lazy_client_core::board_edit::MIN_NODE_EDGE;
    let (mut x, mut y, mut w, mut h) = (node.x, node.y, node.width, node.height);
    match corner {
        Corner::NorthWest => {
            x += dx;
            y += dy;
            w -= dx;
            h -= dy;
        }
        Corner::NorthEast => {
            y += dy;
            w += dx;
            h -= dy;
        }
        Corner::SouthWest => {
            x += dx;
            w -= dx;
            h += dy;
        }
        Corner::SouthEast => {
            w += dx;
            h += dy;
        }
    }
    if w < MIN_NODE_EDGE {
        if matches!(corner, Corner::NorthWest | Corner::SouthWest) {
            x -= MIN_NODE_EDGE - w;
        }
        w = MIN_NODE_EDGE;
    }
    if h < MIN_NODE_EDGE {
        if matches!(corner, Corner::NorthWest | Corner::NorthEast) {
            y -= MIN_NODE_EDGE - h;
        }
        h = MIN_NODE_EDGE;
    }
    (x, y, w, h)
}

/// A sized point for the polygon of an ellipse: `segments` points around it.
pub fn ellipse_points(cx: f32, cy: f32, rx: f32, ry: f32, segments: usize) -> Vec<(f32, f32)> {
    (0..segments)
        .map(|i| {
            let angle = std::f32::consts::TAU * i as f32 / segments as f32;
            (cx + rx * angle.cos(), cy + ry * angle.sin())
        })
        .collect()
}

/// The three points of an arrowhead at `tip`, for a line arriving from `from`.
pub fn arrow_head(from: (f32, f32), tip: (f32, f32), length: f32) -> [(f32, f32); 3] {
    let (dx, dy) = (tip.0 - from.0, tip.1 - from.1);
    let len = (dx * dx + dy * dy).sqrt().max(f32::EPSILON);
    let (ux, uy) = (dx / len, dy / len);
    let base = (tip.0 - ux * length, tip.1 - uy * length);
    let half = length * 0.5;
    [
        tip,
        (base.0 - uy * half, base.1 + ux * half),
        (base.0 + uy * half, base.1 - ux * half),
    ]
}

/// Convenience for building a [`Size`] of pixels.
pub fn px_size(w: f32, h: f32) -> Size<Pixels> {
    size(px(w), px(h))
}

#[cfg(test)]
mod tests {
    use super::*;
    use lazy_domain::board::BoardEdge;

    fn node(id: &str, x: i32, y: i32, shape: NodeShape) -> BoardNode {
        BoardNode {
            id: id.into(),
            shape,
            x,
            y,
            width: 100,
            height: 60,
            text: String::new(),
            color: None,
        }
    }

    #[test]
    fn the_view_maps_both_ways() {
        let view = View {
            offset: (20., -10.),
            scale: 1.5,
        };
        let (sx, sy) = view.to_screen(100., 40.);
        assert_eq!((sx, sy), (170., 50.));
        let (bx, by) = view.to_board(sx, sy);
        assert!((bx - 100.).abs() < 1e-3 && (by - 40.).abs() < 1e-3);
    }

    #[test]
    fn zoom_steps_stay_in_range() {
        assert_eq!(step_zoom(100, true), 110);
        assert_eq!(step_zoom(MAX_ZOOM_PERCENT, true), MAX_ZOOM_PERCENT);
        assert_eq!(step_zoom(MIN_ZOOM_PERCENT, false), MIN_ZOOM_PERCENT);
        assert_eq!(step_zoom(100, false), 90);
    }

    #[test]
    fn zooming_about_a_point_keeps_it_still() {
        let before = View {
            offset: (10., 20.),
            scale: 1.0,
        };
        let focus = (300., 200.);
        let under = before.to_board(focus.0, focus.1);
        let offset = zoom_about(before.offset, focus, 100, 200);
        let after = View { offset, scale: 2.0 };
        let (sx, sy) = after.to_screen(under.0, under.1);
        assert!((sx - focus.0).abs() < 1e-3 && (sy - focus.1).abs() < 1e-3);
    }

    #[test]
    fn connectors_leave_each_outline_toward_the_other_shape() {
        let a = node("a", 0, 0, NodeShape::Rectangle);
        let b = node("b", 300, 0, NodeShape::Rectangle);
        let board = Board {
            nodes: vec![a, b],
            edges: vec![BoardEdge {
                id: "e".into(),
                source: "a".into(),
                target: "b".into(),
                label: String::new(),
            }],
            viewport: Viewport::default(),
        };
        let ((x1, y1), (x2, y2)) = edge_line(&board, &board.edges[0]).unwrap();
        assert!((x1 - 100.).abs() < 1e-3 && (y1 - 30.).abs() < 1e-3);
        assert!((x2 - 300.).abs() < 1e-3 && (y2 - 30.).abs() < 1e-3);
    }

    #[test]
    fn outlines_differ_by_shape() {
        let towards = (1000., 30.);
        let rect = boundary_point(&node("r", 0, 0, NodeShape::Rectangle), towards);
        let oval = boundary_point(&node("o", 0, 0, NodeShape::Ellipse), towards);
        let diamond = boundary_point(&node("d", 0, 0, NodeShape::Diamond), towards);
        assert!((rect.0 - 100.).abs() < 1e-2);
        assert!((oval.0 - 100.).abs() < 1e-2);
        assert!((diamond.0 - 100.).abs() < 1e-2);
        // On the diagonal the diamond's edge is much nearer the middle than the rectangle's.
        let diagonal = (1000., 1000.);
        let rect = boundary_point(&node("r", 0, 0, NodeShape::Rectangle), diagonal);
        let diamond = boundary_point(&node("d", 0, 0, NodeShape::Diamond), diagonal);
        assert!(diamond.0 < rect.0);
    }

    #[test]
    fn tab_order_walks_shapes_then_connectors_and_then_leaves() {
        let board = Board {
            nodes: vec![
                node("a", 0, 0, NodeShape::Rectangle),
                node("b", 200, 0, NodeShape::Rectangle),
            ],
            edges: vec![BoardEdge {
                id: "e".into(),
                source: "a".into(),
                target: "b".into(),
                label: String::new(),
            }],
            viewport: Viewport::default(),
        };
        let first = next_selection(&board, None, false).unwrap();
        assert_eq!(first, Selected::Node("a".into()));
        let second = next_selection(&board, Some(&first), false).unwrap();
        assert_eq!(second, Selected::Node("b".into()));
        let third = next_selection(&board, Some(&second), false).unwrap();
        assert_eq!(third, Selected::Edge("e".into()));
        assert_eq!(next_selection(&board, Some(&third), false), None);
        assert_eq!(next_selection(&board, Some(&first), true), None);
        assert_eq!(next_selection(&board, None, true), Some(third));
        assert_eq!(next_selection(&Board::empty(), None, false), None);
    }

    #[test]
    fn resizing_from_a_corner_moves_the_opposite_one_nowhere() {
        let n = node("a", 100, 100, NodeShape::Rectangle);
        assert_eq!(resized(&n, Corner::SouthEast, 20, 10), (100, 100, 120, 70));
        assert_eq!(resized(&n, Corner::NorthWest, 20, 10), (120, 110, 80, 50));
        // Never smaller than the minimum, and the held corner stays put.
        let (x, y, w, h) = resized(&n, Corner::NorthWest, 500, 500);
        assert_eq!((w, h), (16, 16));
        assert_eq!((x + w, y + h), (200, 160));
    }

    #[test]
    fn corners_are_found_near_the_corners_only() {
        assert_eq!(corner_of((100., 60.), (2., 3.)), Some(Corner::NorthWest));
        assert_eq!(corner_of((100., 60.), (99., 58.)), Some(Corner::SouthEast));
        assert_eq!(corner_of((100., 60.), (50., 30.)), None);
        assert_eq!(corner_of((100., 60.), (50., 2.)), None);
    }

    #[test]
    fn fitting_shows_the_whole_board() {
        let board = Board {
            nodes: vec![
                node("a", 0, 0, NodeShape::Rectangle),
                node("b", 1900, 900, NodeShape::Rectangle),
            ],
            ..Board::empty()
        };
        let viewport = fit_viewport(&board, px_size(800., 500.));
        let view = View {
            offset: (viewport.x as f32, viewport.y as f32),
            scale: f32::from(viewport.zoom_percent) / 100.0,
        };
        let (x1, y1) = view.to_screen(0., 0.);
        let (x2, y2) = view.to_screen(2000., 960.);
        assert!(x1 >= 0. && y1 >= 0. && x2 <= 800. && y2 <= 500.);
        assert_eq!(
            fit_viewport(&Board::empty(), px_size(800., 500.)),
            Viewport::default()
        );
    }

    #[test]
    fn an_arrowhead_points_at_its_tip() {
        let [tip, a, b] = arrow_head((0., 0.), (100., 0.), 12.);
        assert_eq!(tip, (100., 0.));
        assert!((a.0 - 88.).abs() < 1e-3 && (b.0 - 88.).abs() < 1e-3);
        assert!((a.1 + b.1).abs() < 1e-3);
    }

    #[test]
    fn an_ellipse_polygon_stays_on_its_ellipse() {
        for (x, y) in ellipse_points(50., 30., 50., 30., 24) {
            let value = ((x - 50.) / 50.).powi(2) + ((y - 30.) / 30.).powi(2);
            assert!((value - 1.0).abs() < 1e-3);
        }
    }
}
