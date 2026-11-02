//! Editing a board (D30): small pure functions from one [`Board`] to the next.
//!
//! The canvas calls these as the person draws; the result is what `DocsInput::EditPage` carries.
//! Each refuses (returns the error, leaving the board as it was) rather than make a board that
//! [`Board::validate`] would refuse, so a board the editor holds is always one the server accepts.

use lazy_domain::Board;
use lazy_domain::board::{
    BoardColor, BoardEdge, BoardError, BoardNode, MAX_BOARD_EDGES, MAX_BOARD_EXTENT,
    MAX_BOARD_ID_BYTES, MAX_BOARD_NODES, MAX_EDGE_LABEL_BYTES, MAX_NODE_TEXT_BYTES, NodeShape,
    Viewport,
};

/// Smallest width or height a shape can be resized to, in pixels.
pub const MIN_NODE_EDGE: i32 = 16;
/// The size a shape is made at.
pub const DEFAULT_NODE_SIZE: (i32, i32) = (160, 80);

/// Why an edit was not made.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum BoardEditError {
    #[error("there is no shape {0:?}")]
    NoSuchNode(String),
    #[error("there is no connector {0:?}")]
    NoSuchEdge(String),
    #[error(transparent)]
    Invalid(#[from] BoardError),
}

fn clamp_extent(value: i32) -> i32 {
    value.clamp(-MAX_BOARD_EXTENT, MAX_BOARD_EXTENT)
}

fn truncate(text: &str, max: usize) -> String {
    if text.len() <= max {
        return text.to_owned();
    }
    let mut end = max;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    text[..end].to_owned()
}

fn node_mut<'a>(board: &'a mut Board, id: &str) -> Result<&'a mut BoardNode, BoardEditError> {
    board
        .nodes
        .iter_mut()
        .find(|node| node.id == id)
        .ok_or_else(|| BoardEditError::NoSuchNode(id.to_owned()))
}

/// Adds a shape of the default size with its top-left corner at (`x`, `y`). `id` must be new.
pub fn add_node(
    board: &Board,
    id: &str,
    shape: NodeShape,
    x: i32,
    y: i32,
) -> Result<Board, BoardEditError> {
    if board.nodes.len() >= MAX_BOARD_NODES {
        return Err(BoardError::TooManyNodes.into());
    }
    if id.is_empty() || id.len() > MAX_BOARD_ID_BYTES {
        return Err(BoardError::BadId.into());
    }
    let mut next = board.clone();
    next.nodes.push(BoardNode {
        id: id.to_owned(),
        shape,
        x: clamp_extent(x),
        y: clamp_extent(y),
        width: DEFAULT_NODE_SIZE.0,
        height: DEFAULT_NODE_SIZE.1,
        text: String::new(),
        color: None,
    });
    next.validate()?;
    Ok(next)
}

/// Moves a shape by (`dx`, `dy`), keeping it on the canvas.
pub fn move_node(board: &Board, id: &str, dx: i32, dy: i32) -> Result<Board, BoardEditError> {
    let mut next = board.clone();
    let node = node_mut(&mut next, id)?;
    node.x = clamp_extent(node.x.saturating_add(dx));
    node.y = clamp_extent(node.y.saturating_add(dy));
    next.validate()?;
    Ok(next)
}

/// Sets a shape's size, at least [`MIN_NODE_EDGE`] on each side.
pub fn resize_node(
    board: &Board,
    id: &str,
    width: i32,
    height: i32,
) -> Result<Board, BoardEditError> {
    let mut next = board.clone();
    let node = node_mut(&mut next, id)?;
    node.width = width.clamp(MIN_NODE_EDGE, MAX_BOARD_EXTENT);
    node.height = height.clamp(MIN_NODE_EDGE, MAX_BOARD_EXTENT);
    next.validate()?;
    Ok(next)
}

/// Sets a shape's text, cut to the limit.
pub fn set_node_text(board: &Board, id: &str, text: &str) -> Result<Board, BoardEditError> {
    let mut next = board.clone();
    node_mut(&mut next, id)?.text = truncate(text, MAX_NODE_TEXT_BYTES);
    next.validate()?;
    Ok(next)
}

/// Sets, or with `None` clears, a shape's color.
pub fn set_node_color(
    board: &Board,
    id: &str,
    color: Option<BoardColor>,
) -> Result<Board, BoardEditError> {
    let mut next = board.clone();
    node_mut(&mut next, id)?.color = color;
    next.validate()?;
    Ok(next)
}

/// Removes a shape and every connector that touched it.
pub fn delete_node(board: &Board, id: &str) -> Result<Board, BoardEditError> {
    if !board.nodes.iter().any(|node| node.id == id) {
        return Err(BoardEditError::NoSuchNode(id.to_owned()));
    }
    let mut next = board.clone();
    next.nodes.retain(|node| node.id != id);
    next.edges
        .retain(|edge| edge.source != id && edge.target != id);
    next.validate()?;
    Ok(next)
}

/// Joins two shapes with a connector. `id` must be new; the shapes must differ and exist.
pub fn connect(
    board: &Board,
    id: &str,
    source: &str,
    target: &str,
) -> Result<Board, BoardEditError> {
    if board.edges.len() >= MAX_BOARD_EDGES {
        return Err(BoardError::TooManyEdges.into());
    }
    let mut next = board.clone();
    next.edges.push(BoardEdge {
        id: id.to_owned(),
        source: source.to_owned(),
        target: target.to_owned(),
        label: String::new(),
    });
    next.validate()?;
    Ok(next)
}

/// Sets a connector's label, cut to the limit.
pub fn set_edge_label(board: &Board, id: &str, label: &str) -> Result<Board, BoardEditError> {
    let mut next = board.clone();
    next.edges
        .iter_mut()
        .find(|edge| edge.id == id)
        .ok_or_else(|| BoardEditError::NoSuchEdge(id.to_owned()))?
        .label = truncate(label, MAX_EDGE_LABEL_BYTES);
    next.validate()?;
    Ok(next)
}

/// Removes a connector.
pub fn delete_edge(board: &Board, id: &str) -> Result<Board, BoardEditError> {
    if !board.edges.iter().any(|edge| edge.id == id) {
        return Err(BoardEditError::NoSuchEdge(id.to_owned()));
    }
    let mut next = board.clone();
    next.edges.retain(|edge| edge.id != id);
    next.validate()?;
    Ok(next)
}

/// Points the canvas somewhere. The zoom is clamped to the allowed range.
pub fn set_viewport(board: &Board, x: i32, y: i32, zoom_percent: u16) -> Board {
    use lazy_domain::board::{MAX_ZOOM_PERCENT, MIN_ZOOM_PERCENT};
    let mut next = board.clone();
    next.viewport = Viewport {
        x: clamp_extent(x),
        y: clamp_extent(y),
        zoom_percent: zoom_percent.clamp(MIN_ZOOM_PERCENT, MAX_ZOOM_PERCENT),
    };
    next
}

/// An id for a new shape or connector that no existing one uses: `n1`, `n2`, ... for shapes and
/// `e1`, `e2`, ... for connectors, chosen by the caller's `prefix`.
pub fn fresh_id(board: &Board, prefix: &str) -> String {
    let taken = |candidate: &str| {
        board.nodes.iter().any(|node| node.id == candidate)
            || board.edges.iter().any(|edge| edge.id == candidate)
    };
    (1..)
        .map(|n| format!("{prefix}{n}"))
        .find(|candidate| !taken(candidate))
        .expect("an unused id exists")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn two_shapes() -> Board {
        let board = add_node(&Board::empty(), "a", NodeShape::Rectangle, 0, 0).unwrap();
        add_node(&board, "b", NodeShape::Ellipse, 300, 40).unwrap()
    }

    #[test]
    fn shapes_are_added_moved_resized_and_given_text_and_color() {
        let board = two_shapes();
        let board = move_node(&board, "a", 10, -5).unwrap();
        let board = resize_node(&board, "a", 4, 500).unwrap();
        let board = set_node_text(&board, "a", "Start").unwrap();
        let board = set_node_color(&board, "a", Some(BoardColor::Green)).unwrap();
        let a = &board.nodes[0];
        assert_eq!((a.x, a.y), (10, -5));
        assert_eq!((a.width, a.height), (MIN_NODE_EDGE, 500));
        assert_eq!(a.text, "Start");
        assert_eq!(a.color, Some(BoardColor::Green));
        board.validate().unwrap();
    }

    #[test]
    fn an_edit_that_would_make_an_invalid_board_changes_nothing() {
        let board = two_shapes();
        assert_eq!(
            add_node(&board, "a", NodeShape::Note, 0, 0),
            Err(BoardError::DuplicateId("a".into()).into())
        );
        assert_eq!(
            add_node(&board, "", NodeShape::Note, 0, 0),
            Err(BoardError::BadId.into())
        );
        assert_eq!(
            connect(&board, "e", "a", "a"),
            Err(BoardError::SelfEdge("e".into()).into())
        );
        assert_eq!(
            connect(&board, "e", "a", "ghost"),
            Err(BoardError::DanglingEdge("e".into()).into())
        );
        assert_eq!(
            move_node(&board, "ghost", 1, 1),
            Err(BoardEditError::NoSuchNode("ghost".into()))
        );
    }

    #[test]
    fn deleting_a_shape_takes_its_connectors_with_it() {
        let board = connect(&two_shapes(), "e", "a", "b").unwrap();
        let board = set_edge_label(&board, "e", "then").unwrap();
        assert_eq!(board.edges[0].label, "then");
        let gone = delete_node(&board, "b").unwrap();
        assert_eq!(gone.nodes.len(), 1);
        assert!(gone.edges.is_empty());
        let kept = delete_edge(&board, "e").unwrap();
        assert_eq!(kept.nodes.len(), 2);
        assert!(kept.edges.is_empty());
        assert_eq!(
            delete_edge(&board, "nope"),
            Err(BoardEditError::NoSuchEdge("nope".into()))
        );
    }

    #[test]
    fn text_is_cut_on_a_character_boundary_and_the_viewport_is_clamped() {
        let board = two_shapes();
        let long = "é".repeat(MAX_NODE_TEXT_BYTES);
        let board = set_node_text(&board, "a", &long).unwrap();
        assert!(board.nodes[0].text.len() <= MAX_NODE_TEXT_BYTES);
        assert!(board.nodes[0].text.chars().all(|c| c == 'é'));
        let view = set_viewport(&board, 5, 6, 9_000);
        assert_eq!(view.viewport.zoom_percent, 400);
        view.validate().unwrap();
    }

    #[test]
    fn moving_far_stays_on_the_canvas() {
        let board = move_node(&two_shapes(), "a", i32::MAX, i32::MIN).unwrap();
        board.validate().unwrap();
        assert_eq!(board.nodes[0].x, MAX_BOARD_EXTENT);
    }

    #[test]
    fn fresh_ids_skip_the_ones_in_use() {
        let board = add_node(&Board::empty(), "n1", NodeShape::Note, 0, 0).unwrap();
        assert_eq!(fresh_id(&board, "n"), "n2");
        assert_eq!(fresh_id(&board, "e"), "e1");
    }
}
