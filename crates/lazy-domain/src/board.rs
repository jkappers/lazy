//! The content of a board: shapes, connectors, and text on a canvas (llm-docs/DECISIONS.md, D30).
//!
//! A board is stored and sent as this value, not as a client library's state, so the desktop app,
//! a phone, and the API read the same thing. Coordinates are whole pixels (so a board compares for
//! equality and survives a round trip through JSON unchanged), and the zoom is a whole percent.
//! [`Board::validate`] is the only gate: the server refuses a board that fails it, and a client
//! checks before it sends.

use std::collections::HashSet;

use serde::{Deserialize, Serialize};

/// Most shapes on one board.
pub const MAX_BOARD_NODES: usize = 500;
/// Most connectors on one board.
pub const MAX_BOARD_EDGES: usize = 1_000;
/// Longest shape id or connector id, in bytes.
pub const MAX_BOARD_ID_BYTES: usize = 64;
/// Most UTF-8 bytes of text in one shape.
pub const MAX_NODE_TEXT_BYTES: usize = 2_000;
/// Most UTF-8 bytes of text on one connector.
pub const MAX_EDGE_LABEL_BYTES: usize = 200;
/// Largest coordinate or size magnitude, in pixels.
pub const MAX_BOARD_EXTENT: i32 = 100_000;
/// Smallest and largest zoom, in percent.
pub const MIN_ZOOM_PERCENT: u16 = 10;
pub const MAX_ZOOM_PERCENT: u16 = 400;

/// What a shape looks like.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NodeShape {
    Rectangle,
    Ellipse,
    Diamond,
    /// A sticky note.
    Note,
    /// Text with no outline.
    Text,
}

/// A shape's color, as a name the app maps to its theme (so light and dark both read well).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BoardColor {
    Neutral,
    Red,
    Orange,
    Yellow,
    Green,
    Blue,
    Purple,
}

/// One shape on the canvas.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BoardNode {
    /// Unique on the board; chosen by the client that made the shape.
    pub id: String,
    pub shape: NodeShape,
    /// Top-left corner.
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<BoardColor>,
}

/// A connector from one shape to another.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BoardEdge {
    pub id: String,
    /// The `id` of the shape it leaves.
    pub source: String,
    /// The `id` of the shape it arrives at.
    pub target: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub label: String,
}

/// Where the canvas is looking.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Viewport {
    pub x: i32,
    pub y: i32,
    pub zoom_percent: u16,
}

impl Default for Viewport {
    fn default() -> Self {
        Self {
            x: 0,
            y: 0,
            zoom_percent: 100,
        }
    }
}

/// Why a board was refused.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum BoardError {
    #[error("a board has at most {MAX_BOARD_NODES} shapes")]
    TooManyNodes,
    #[error("a board has at most {MAX_BOARD_EDGES} connectors")]
    TooManyEdges,
    #[error("a shape or connector id must be 1 to {MAX_BOARD_ID_BYTES} bytes")]
    BadId,
    #[error("two shapes or two connectors share the id {0:?}")]
    DuplicateId(String),
    #[error("shape text is at most {MAX_NODE_TEXT_BYTES} bytes")]
    NodeTextTooLong,
    #[error("a connector label is at most {MAX_EDGE_LABEL_BYTES} bytes")]
    EdgeLabelTooLong,
    #[error("a shape needs a positive width and height and must stay within the canvas")]
    BadGeometry,
    #[error("connector {0:?} names a shape that is not on the board")]
    DanglingEdge(String),
    #[error("connector {0:?} starts and ends on the same shape")]
    SelfEdge(String),
    #[error("zoom is {MIN_ZOOM_PERCENT} to {MAX_ZOOM_PERCENT} percent")]
    BadZoom,
}

/// A board's whole content.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Board {
    #[serde(default)]
    pub nodes: Vec<BoardNode>,
    #[serde(default)]
    pub edges: Vec<BoardEdge>,
    #[serde(default)]
    pub viewport: Viewport,
}

fn id_is_valid(id: &str) -> bool {
    !id.is_empty() && id.len() <= MAX_BOARD_ID_BYTES && !id.chars().any(char::is_control)
}

fn within(value: i32) -> bool {
    value.unsigned_abs() <= MAX_BOARD_EXTENT as u32
}

impl Board {
    /// A board with nothing on it.
    pub fn empty() -> Self {
        Self::default()
    }

    /// Checks every rule above. A board that passes can be stored and drawn.
    pub fn validate(&self) -> Result<(), BoardError> {
        if self.nodes.len() > MAX_BOARD_NODES {
            return Err(BoardError::TooManyNodes);
        }
        if self.edges.len() > MAX_BOARD_EDGES {
            return Err(BoardError::TooManyEdges);
        }
        if !(MIN_ZOOM_PERCENT..=MAX_ZOOM_PERCENT).contains(&self.viewport.zoom_percent)
            || !within(self.viewport.x)
            || !within(self.viewport.y)
        {
            return Err(BoardError::BadZoom);
        }
        let mut node_ids = HashSet::with_capacity(self.nodes.len());
        for node in &self.nodes {
            if !id_is_valid(&node.id) {
                return Err(BoardError::BadId);
            }
            if !node_ids.insert(node.id.as_str()) {
                return Err(BoardError::DuplicateId(node.id.clone()));
            }
            if node.text.len() > MAX_NODE_TEXT_BYTES {
                return Err(BoardError::NodeTextTooLong);
            }
            if node.width <= 0
                || node.height <= 0
                || !within(node.x)
                || !within(node.y)
                || !within(node.width)
                || !within(node.height)
            {
                return Err(BoardError::BadGeometry);
            }
        }
        let mut edge_ids = HashSet::with_capacity(self.edges.len());
        for edge in &self.edges {
            if !id_is_valid(&edge.id) {
                return Err(BoardError::BadId);
            }
            if !edge_ids.insert(edge.id.as_str()) {
                return Err(BoardError::DuplicateId(edge.id.clone()));
            }
            if edge.label.len() > MAX_EDGE_LABEL_BYTES {
                return Err(BoardError::EdgeLabelTooLong);
            }
            if !node_ids.contains(edge.source.as_str()) || !node_ids.contains(edge.target.as_str())
            {
                return Err(BoardError::DanglingEdge(edge.id.clone()));
            }
            if edge.source == edge.target {
                return Err(BoardError::SelfEdge(edge.id.clone()));
            }
        }
        Ok(())
    }

    /// The text on the board, one line per shape and connector label, for search and for readers
    /// that cannot draw a canvas.
    pub fn plain_text(&self) -> String {
        let mut lines: Vec<&str> = Vec::new();
        for node in &self.nodes {
            if !node.text.is_empty() {
                lines.push(&node.text);
            }
        }
        for edge in &self.edges {
            if !edge.label.is_empty() {
                lines.push(&edge.label);
            }
        }
        lines.join("\n")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node(id: &str) -> BoardNode {
        BoardNode {
            id: id.into(),
            shape: NodeShape::Rectangle,
            x: 0,
            y: 0,
            width: 120,
            height: 60,
            text: String::new(),
            color: None,
        }
    }

    fn edge(id: &str, source: &str, target: &str) -> BoardEdge {
        BoardEdge {
            id: id.into(),
            source: source.into(),
            target: target.into(),
            label: String::new(),
        }
    }

    #[test]
    fn an_empty_board_is_valid_and_reads_from_an_empty_object() {
        Board::empty().validate().unwrap();
        let board: Board = serde_json::from_str("{}").unwrap();
        assert_eq!(board, Board::empty());
    }

    #[test]
    fn a_board_round_trips_through_json_unchanged() {
        let mut a = node("a");
        a.text = "Start".into();
        a.color = Some(BoardColor::Green);
        let board = Board {
            nodes: vec![a, node("b")],
            edges: vec![BoardEdge {
                label: "then".into(),
                ..edge("e1", "a", "b")
            }],
            viewport: Viewport {
                x: -40,
                y: 12,
                zoom_percent: 150,
            },
        };
        board.validate().unwrap();
        let json = serde_json::to_string(&board).unwrap();
        assert_eq!(serde_json::from_str::<Board>(&json).unwrap(), board);
    }

    #[test]
    fn duplicate_ids_are_refused() {
        let board = Board {
            nodes: vec![node("a"), node("a")],
            ..Board::empty()
        };
        assert_eq!(board.validate(), Err(BoardError::DuplicateId("a".into())));
    }

    #[test]
    fn connectors_must_join_two_different_shapes_on_the_board() {
        let dangling = Board {
            nodes: vec![node("a")],
            edges: vec![edge("e", "a", "ghost")],
            ..Board::empty()
        };
        assert_eq!(
            dangling.validate(),
            Err(BoardError::DanglingEdge("e".into()))
        );
        let looped = Board {
            nodes: vec![node("a")],
            edges: vec![edge("e", "a", "a")],
            ..Board::empty()
        };
        assert_eq!(looped.validate(), Err(BoardError::SelfEdge("e".into())));
    }

    #[test]
    fn geometry_zoom_ids_and_sizes_are_bounded() {
        let mut flat = node("a");
        flat.height = 0;
        let board = Board {
            nodes: vec![flat],
            ..Board::empty()
        };
        assert_eq!(board.validate(), Err(BoardError::BadGeometry));

        let mut far = node("a");
        far.x = MAX_BOARD_EXTENT + 1;
        let board = Board {
            nodes: vec![far],
            ..Board::empty()
        };
        assert_eq!(board.validate(), Err(BoardError::BadGeometry));

        let board = Board {
            viewport: Viewport {
                zoom_percent: 5,
                ..Viewport::default()
            },
            ..Board::empty()
        };
        assert_eq!(board.validate(), Err(BoardError::BadZoom));

        let board = Board {
            nodes: vec![node("")],
            ..Board::empty()
        };
        assert_eq!(board.validate(), Err(BoardError::BadId));

        let mut wordy = node("a");
        wordy.text = "x".repeat(MAX_NODE_TEXT_BYTES + 1);
        let board = Board {
            nodes: vec![wordy],
            ..Board::empty()
        };
        assert_eq!(board.validate(), Err(BoardError::NodeTextTooLong));
    }

    #[test]
    fn counts_are_bounded() {
        let board = Board {
            nodes: (0..=MAX_BOARD_NODES)
                .map(|i| node(&i.to_string()))
                .collect(),
            ..Board::empty()
        };
        assert_eq!(board.validate(), Err(BoardError::TooManyNodes));
    }

    #[test]
    fn plain_text_lists_shape_text_then_labels() {
        let mut a = node("a");
        a.text = "One".into();
        let mut e = edge("e", "a", "b");
        e.label = "leads to".into();
        let board = Board {
            nodes: vec![a, node("b")],
            edges: vec![e],
            ..Board::empty()
        };
        assert_eq!(board.plain_text(), "One\nleads to");
    }
}
