//! Pages, boards, and notations: the pure rules (llm-docs/DECISIONS.md, D30).
//!
//! A page belongs to one conversation. Whoever can read the conversation can read its pages, and
//! whoever can post in it can write, trash, and restore them ([`PageAccess`]). A *page* holds
//! markdown text; a *board* holds a [`Board`]. Neither is nested in the other, and they share
//! everything else: a title, a revision, versions, and trash.

use serde::{Deserialize, Serialize};

use crate::authz::ChannelAccess;
use crate::board::{Board, BoardError};

/// What a page holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PageKind {
    /// Markdown text.
    Page,
    /// Shapes, connectors, and text on a canvas.
    Board,
}

/// The title a page gets when it is made, or edited, with none.
pub const UNTITLED: &str = "Untitled";
/// Longest title, in characters.
pub const TITLE_MAX_CHARS: usize = 255;
/// Most UTF-8 bytes of markdown in one page.
pub const PAGE_BODY_MAX_BYTES: usize = 1_000_000;
/// Days a trashed page waits before it is removed for good.
pub const TRASH_RETENTION_DAYS: i64 = 30;
/// Minutes between automatic snapshots of a page's earlier state.
pub const VERSION_INTERVAL_MINUTES: i64 = 5;
/// Versions kept per page; the oldest are removed.
pub const MAX_VERSIONS_PER_PAGE: usize = 100;
/// Most live pages and boards in one conversation.
pub const MAX_PAGES_PER_CONVERSATION: usize = 1_000;
/// Longest excerpt of a page's text in a list, in characters.
pub const EXCERPT_MAX_CHARS: usize = 160;

/// Longest text a notation quotes, in bytes.
pub const NOTATION_QUOTE_MAX_BYTES: usize = 2_000;
/// Longest notation or reply body, in bytes.
pub const NOTATION_BODY_MAX_BYTES: usize = 5_000;

/// Why page content was refused.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PageError {
    #[error("a title is at most {TITLE_MAX_CHARS} characters")]
    TitleTooLong,
    #[error("a page is at most {PAGE_BODY_MAX_BYTES} bytes")]
    BodyTooLong,
    #[error("a page holds text, not a board")]
    PageHasNoBoard,
    #[error("a board holds shapes, not text")]
    BoardHasNoBody,
    #[error("a notation quotes 1 to {NOTATION_QUOTE_MAX_BYTES} bytes of text")]
    BadQuote,
    #[error("a notation or reply is 1 to {NOTATION_BODY_MAX_BYTES} bytes")]
    BadNotationBody,
    #[error("a notation anchors a range of the page's text: start before end, inside the page")]
    BadAnchor,
    #[error(transparent)]
    Board(#[from] BoardError),
}

/// A page's title with the rules applied: surrounding space removed, control characters refused,
/// and [`UNTITLED`] when nothing is left.
pub fn normalize_title(title: &str) -> Result<String, PageError> {
    let title = title.trim();
    if title.is_empty() {
        return Ok(UNTITLED.to_owned());
    }
    if title.chars().count() > TITLE_MAX_CHARS {
        return Err(PageError::TitleTooLong);
    }
    // A title is one line.
    Ok(title
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect())
}

/// What a page holds: exactly one of text or a board, matching its [`PageKind`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PageContent {
    Page { body: String },
    Board { board: Board },
}

impl PageContent {
    /// The empty content for a kind.
    pub fn empty(kind: PageKind) -> Self {
        match kind {
            PageKind::Page => Self::Page {
                body: String::new(),
            },
            PageKind::Board => Self::Board {
                board: Board::empty(),
            },
        }
    }

    pub fn kind(&self) -> PageKind {
        match self {
            Self::Page { .. } => PageKind::Page,
            Self::Board { .. } => PageKind::Board,
        }
    }

    /// Checks the size and shape rules.
    pub fn validate(&self) -> Result<(), PageError> {
        match self {
            Self::Page { body } if body.len() > PAGE_BODY_MAX_BYTES => Err(PageError::BodyTooLong),
            Self::Page { .. } => Ok(()),
            Self::Board { board } => Ok(board.validate()?),
        }
    }

    /// The text search and excerpts read: the markdown, or the text on the board.
    pub fn plain_text(&self) -> String {
        match self {
            Self::Page { body } => body.clone(),
            Self::Board { board } => board.plain_text(),
        }
    }

    /// Whether `other` is the same kind, so one can replace the other.
    pub fn same_kind(&self, other: &Self) -> bool {
        self.kind() == other.kind()
    }
}

/// The first [`EXCERPT_MAX_CHARS`] characters of the text, on one line.
pub fn excerpt(text: &str) -> String {
    let mut out = String::new();
    let mut count = 0;
    let mut last_space = true;
    for c in text.chars() {
        let c = if c.is_whitespace() { ' ' } else { c };
        if c == ' ' && last_space {
            continue;
        }
        last_space = c == ' ';
        out.push(c);
        count += 1;
        if count == EXCERPT_MAX_CHARS {
            break;
        }
    }
    out.trim_end().to_owned()
}

/// Why a version was kept.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VersionReason {
    /// The state before an edit, kept at most every [`VERSION_INTERVAL_MINUTES`].
    Autosave,
    /// The state before a restore replaced it, so a restore can be undone.
    PreRestore,
}

impl VersionReason {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Autosave => "autosave",
            Self::PreRestore => "pre_restore",
        }
    }

    pub fn parse(text: &str) -> Option<Self> {
        match text {
            "autosave" => Some(Self::Autosave),
            "pre_restore" => Some(Self::PreRestore),
            _ => None,
        }
    }
}

/// Whether an edit should keep the page's earlier state as a version.
///
/// `seconds_since_latest` is the age of the newest version (`None` when there is none) and
/// `same_as_latest` says the earlier state equals it. An automatic snapshot waits out the
/// interval; any snapshot is skipped when it would repeat the newest version.
pub fn should_snapshot(
    reason: VersionReason,
    seconds_since_latest: Option<i64>,
    same_as_latest: bool,
) -> bool {
    if same_as_latest {
        return false;
    }
    match (reason, seconds_since_latest) {
        (VersionReason::Autosave, Some(age)) => age >= VERSION_INTERVAL_MINUTES * 60,
        _ => true,
    }
}

/// What one person may do with one page, derived from their access to its conversation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PageAccess {
    pub read: bool,
    pub write: bool,
}

impl PageAccess {
    pub fn of(channel: &ChannelAccess) -> Self {
        Self {
            read: channel.can_read(),
            write: channel.can_post(),
        }
    }

    /// Viewing, listing, history, and downloading files.
    pub fn can_view(self) -> bool {
        self.read
    }

    /// Creating, editing, moving, trashing, restoring, deleting for good, and adding files.
    pub fn can_edit(self) -> bool {
        self.write
    }
}

/// Checks the text a notation or reply carries.
pub fn validate_notation_body(body: &str) -> Result<(), PageError> {
    if body.trim().is_empty() || body.len() > NOTATION_BODY_MAX_BYTES {
        return Err(PageError::BadNotationBody);
    }
    Ok(())
}

/// Checks a notation's anchor: the quoted text and the character range it covered in the body when
/// the note was made. `body_chars` is the length of the page's body in characters.
pub fn validate_anchor(
    quote: &str,
    from: u32,
    to: u32,
    body_chars: usize,
) -> Result<(), PageError> {
    if quote.trim().is_empty() || quote.len() > NOTATION_QUOTE_MAX_BYTES {
        return Err(PageError::BadQuote);
    }
    if from >= to || to as usize > body_chars {
        return Err(PageError::BadAnchor);
    }
    Ok(())
}

/// Where a notation's quote is now, after the page was edited: the range the note was made on if
/// the quote is still there, otherwise the nearest occurrence at or after it, otherwise the
/// nearest before it. `None` when the quote is gone, and the note is shown as detached.
///
/// Offsets are in characters, as the editor counts them.
pub fn relocate_anchor(body: &str, quote: &str, from: u32, to: u32) -> Option<(u32, u32)> {
    if quote.is_empty() {
        return None;
    }
    let chars: Vec<char> = body.chars().collect();
    let quote_chars: Vec<char> = quote.chars().collect();
    let len = quote_chars.len();
    if len > chars.len() {
        return None;
    }
    let matches_at = |at: usize| chars[at..at + len] == quote_chars[..];
    let from = from as usize;
    if (to as usize).saturating_sub(from) == len && from + len <= chars.len() && matches_at(from) {
        return Some((from as u32, (from + len) as u32));
    }
    let last = chars.len() - len;
    let after = (from.min(last)..=last).find(|&at| matches_at(at));
    let before = (0..from.min(last + 1)).rev().find(|&at| matches_at(at));
    after.or(before).map(|at| (at as u32, (at + len) as u32))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::authz::{ChannelVisibility, WorkspaceRole};

    #[test]
    fn titles_are_trimmed_defaulted_and_bounded() {
        assert_eq!(normalize_title("  Plan  ").unwrap(), "Plan");
        assert_eq!(normalize_title("   ").unwrap(), UNTITLED);
        assert_eq!(normalize_title("a\nb").unwrap(), "a b");
        assert_eq!(
            normalize_title(&"x".repeat(TITLE_MAX_CHARS + 1)),
            Err(PageError::TitleTooLong)
        );
        assert!(normalize_title(&"é".repeat(TITLE_MAX_CHARS)).is_ok());
    }

    #[test]
    fn content_matches_its_kind_and_is_bounded() {
        assert_eq!(PageContent::empty(PageKind::Page).kind(), PageKind::Page);
        assert_eq!(PageContent::empty(PageKind::Board).kind(), PageKind::Board);
        let big = PageContent::Page {
            body: "x".repeat(PAGE_BODY_MAX_BYTES + 1),
        };
        assert_eq!(big.validate(), Err(PageError::BodyTooLong));
        assert!(
            !PageContent::empty(PageKind::Page).same_kind(&PageContent::empty(PageKind::Board))
        );
    }

    #[test]
    fn content_round_trips_through_json_tagged_by_kind() {
        let content = PageContent::Page { body: "hi".into() };
        let json = serde_json::to_string(&content).unwrap();
        assert_eq!(json, r#"{"kind":"page","body":"hi"}"#);
        assert_eq!(serde_json::from_str::<PageContent>(&json).unwrap(), content);
        let board = PageContent::empty(PageKind::Board);
        let json = serde_json::to_string(&board).unwrap();
        assert_eq!(serde_json::from_str::<PageContent>(&json).unwrap(), board);
    }

    #[test]
    fn excerpts_collapse_space_and_stop_at_the_limit() {
        assert_eq!(excerpt("# Title\n\n  some   text "), "# Title some text");
        assert_eq!(excerpt(&"a".repeat(500)).chars().count(), EXCERPT_MAX_CHARS);
        assert_eq!(excerpt(""), "");
    }

    #[test]
    fn snapshots_wait_out_the_interval_and_skip_repeats() {
        use VersionReason::*;
        assert!(should_snapshot(Autosave, None, false));
        assert!(!should_snapshot(Autosave, Some(60), false));
        assert!(should_snapshot(Autosave, Some(300), false));
        assert!(!should_snapshot(Autosave, Some(10_000), true));
        // Keeping the state before a restore ignores the interval, but not a repeat.
        assert!(should_snapshot(PreRestore, Some(1), false));
        assert!(!should_snapshot(PreRestore, Some(1), true));
    }

    #[test]
    fn version_reasons_round_trip_through_their_stored_text() {
        for reason in [VersionReason::Autosave, VersionReason::PreRestore] {
            assert_eq!(VersionReason::parse(reason.as_str()), Some(reason));
        }
        assert_eq!(VersionReason::parse("other"), None);
    }

    #[test]
    fn page_access_follows_the_conversation() {
        let member = ChannelAccess {
            workspace_role: Some(WorkspaceRole::Member),
            is_channel_member: true,
            visibility: ChannelVisibility::Private,
            archived: false,
        };
        let access = PageAccess::of(&member);
        assert!(access.can_view() && access.can_edit());
        let archived = ChannelAccess {
            archived: true,
            ..member
        };
        let access = PageAccess::of(&archived);
        assert!(access.can_view() && !access.can_edit());
        let outsider = ChannelAccess {
            is_channel_member: false,
            ..member
        };
        let access = PageAccess::of(&outsider);
        assert!(!access.can_view() && !access.can_edit());
    }

    #[test]
    fn notation_text_and_anchors_are_checked() {
        assert!(validate_notation_body("ok").is_ok());
        assert_eq!(
            validate_notation_body("  "),
            Err(PageError::BadNotationBody)
        );
        assert_eq!(
            validate_notation_body(&"x".repeat(NOTATION_BODY_MAX_BYTES + 1)),
            Err(PageError::BadNotationBody)
        );
        assert!(validate_anchor("word", 0, 4, 10).is_ok());
        assert_eq!(validate_anchor("", 0, 4, 10), Err(PageError::BadQuote));
        assert_eq!(validate_anchor("word", 4, 4, 10), Err(PageError::BadAnchor));
        assert_eq!(
            validate_anchor("word", 8, 12, 10),
            Err(PageError::BadAnchor)
        );
    }

    #[test]
    fn an_anchor_follows_its_quote_through_edits() {
        let body = "alpha beta gamma";
        // Unchanged.
        assert_eq!(relocate_anchor(body, "beta", 6, 10), Some((6, 10)));
        // Text inserted before it.
        assert_eq!(
            relocate_anchor("new alpha beta gamma", "beta", 6, 10),
            Some((10, 14))
        );
        // Text removed before it.
        assert_eq!(relocate_anchor("beta gamma", "beta", 6, 10), Some((0, 4)));
        // Gone.
        assert_eq!(relocate_anchor("alpha gamma", "beta", 6, 10), None);
        // Counts characters, not bytes.
        assert_eq!(relocate_anchor("é beta", "beta", 0, 4), Some((2, 6)));
        // A quote longer than the page, and an empty one.
        assert_eq!(relocate_anchor("ab", "abcd", 0, 4), None);
        assert_eq!(relocate_anchor("ab", "", 0, 0), None);
    }
}
