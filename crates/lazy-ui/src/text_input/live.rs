//! Live markdown: the pure part.
//!
//! A page body stays a plain `String`. This module reads it line by line and says how each piece
//! should look (a heading is large, `**x**` is bold, `- ` is a bullet), and builds the *display
//! text* the input actually shapes: the same text, except that on every line the caret is not on
//! the syntax markers are removed (`# `, `**`, backticks, the URL of a link) or replaced (`- ` by
//! a bullet). The caret's line shows its markers, dimmed, so the text can be edited as typed.
//!
//! Because the model never changes, selection, IME, undo, clipboard, the size limit, and anything
//! that points into the text by byte offset (notes) work as before. [`Display`] converts offsets in
//! both directions.
//!
//! The dialect is the one `lazy-domain`'s `markdown.rs` parses (`*bold*` or `**bold**`, `_italic_`,
//! `~strike~`, backtick code, `[text](url)`, bare addresses, `> `, `- `, `1. `, fences), plus what
//! the docs preview adds: `#` headings, `- [ ]` tasks, and `---` rules. A marker opens only at the
//! start of a word and closes only at the end of one, as there.
//!
//! Nothing here depends on GPUI.

use std::ops::Range;

/// Longest text that is styled. Above this the input is plain: styling re-reads the whole text.
pub const LIVE_MAX_BYTES: usize = 200_000;

/// A line longer than this gets no inline styling, which bounds the work a pathological line
/// can cost.
const MAX_STYLED_LINE_BYTES: usize = 4_000;

/// Inline formatting in force over a piece of text.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct Flags {
    pub bold: bool,
    pub italic: bool,
    pub strike: bool,
    pub code: bool,
    pub link: bool,
}

impl Flags {
    const CODE: Flags = Flags {
        bold: false,
        italic: false,
        strike: false,
        code: true,
        link: false,
    };
}

/// What kind of line this is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Block {
    Plain,
    Heading(u8),
    Bullet,
    Numbered,
    Quote,
    Task {
        checked: bool,
    },
    /// A line of three or more backticks that opens or closes a code block.
    Fence,
    /// A line inside a code block.
    Code,
    /// `---`.
    Rule,
}

impl Block {
    /// Font size relative to body text. Matches the sizes the docs preview draws.
    pub fn scale(self) -> f32 {
        match self {
            Block::Heading(1) => 2.0,
            Block::Heading(2) => 1.7,
            Block::Heading(3) => 1.45,
            Block::Heading(_) => 1.1,
            Block::Fence | Block::Code => 0.93,
            _ => 1.0,
        }
    }

    /// Line height relative to body text. Code keeps the body's, so a block reads as one surface.
    pub fn line_scale(self) -> f32 {
        match self {
            Block::Heading(_) => self.scale(),
            _ => 1.0,
        }
    }

    pub fn is_code(self) -> bool {
        matches!(self, Block::Fence | Block::Code)
    }
}

/// How a piece of a line is shown.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    /// Content.
    Text,
    /// Syntax that stays visible, dimmed (the `1. ` of a numbered item, a code fence).
    Marker,
    /// Syntax that is removed when the caret is elsewhere and dimmed when it is on the line.
    Hide,
    /// Syntax that is replaced by this text when the caret is elsewhere (`- ` becomes a bullet).
    Replace(&'static str),
}

/// A piece of one line: a byte range of the model text and how to show it. The pieces of a line
/// tile it exactly.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Seg {
    pub range: Range<usize>,
    pub role: Role,
    pub flags: Flags,
}

/// What follows the marker of a list item or quote, for continuing it on Enter.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LeadKind {
    Bullet(char),
    Task(char),
    Numbered { number: u32, delimiter: char },
    Quote,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Lead {
    pub kind: LeadKind,
    /// Bytes of leading spaces.
    pub indent: usize,
    /// Byte offset just past the marker (and the space after it).
    pub marker_end: usize,
}

/// One logical line of the model text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StyledLine {
    /// Bytes of the line, not counting its line break.
    pub range: Range<usize>,
    pub block: Block,
    pub segs: Vec<Seg>,
    pub lead: Option<Lead>,
    /// For a task, the byte offset of the space or `x` between the brackets.
    pub task_char: Option<usize>,
}

// ---------------------------------------------------------------------------------------------
// Analysis
// ---------------------------------------------------------------------------------------------

/// Reads `text` line by line. The result has one entry per line (an empty text has one empty
/// line), in order.
pub fn analyze(text: &str) -> Vec<StyledLine> {
    let mut lines = Vec::with_capacity(text.len() / 32 + 1);
    let mut start = 0;
    let mut fence: Option<usize> = None;
    for line in text.split('\n') {
        lines.push(style_line(line, start, &mut fence));
        start += line.len() + 1;
    }
    lines
}

/// Lines that contain any byte of `from..=to` (inclusive, so a caret at a line's end belongs to
/// that line), as first and last indices.
pub fn active_lines(lines: &[StyledLine], from: usize, to: usize) -> (usize, usize) {
    if lines.is_empty() {
        return (0, 0);
    }
    let last_ix = lines.len() - 1;
    let first = lines.partition_point(|l| l.range.end < from).min(last_ix);
    let last = lines
        .partition_point(|l| l.range.start <= to)
        .saturating_sub(1)
        .max(first);
    (first, last.min(last_ix))
}

#[derive(Default)]
struct Segs {
    out: Vec<Seg>,
}

impl Segs {
    fn push(&mut self, range: Range<usize>, role: Role, flags: Flags) {
        if range.is_empty() {
            return;
        }
        if matches!(role, Role::Text | Role::Marker)
            && let Some(last) = self.out.last_mut()
            && last.role == role
            && last.flags == flags
            && last.range.end == range.start
        {
            last.range.end = range.end;
            return;
        }
        self.out.push(Seg { range, role, flags });
    }
}

fn leading_spaces(line: &str) -> usize {
    line.bytes().take_while(|b| *b == b' ').count()
}

/// The backtick count if `line` opens a fence (as the message parser reads it).
fn fence_open(line: &str) -> Option<usize> {
    let rest = line.trim_start();
    let ticks = rest.bytes().take_while(|b| *b == b'`').count();
    if ticks < 3 || rest[ticks..].contains("```") {
        return None;
    }
    Some(ticks)
}

fn is_fence_close(line: &str, ticks: usize) -> bool {
    let trimmed = line.trim();
    trimmed.len() >= ticks && trimmed.bytes().all(|b| b == b'`')
}

/// `#` to `######` followed by a space or tab: the level and the bytes of the marker.
fn heading(line: &str) -> Option<(u8, usize)> {
    let hashes = line.bytes().take_while(|b| *b == b'#').count();
    if !(1..=6).contains(&hashes) {
        return None;
    }
    let after = line[hashes..].chars().next()?;
    matches!(after, ' ' | '\t').then_some((hashes as u8, hashes + 1))
}

fn is_rule(line: &str) -> bool {
    let trimmed = line.trim();
    trimmed.len() >= 3 && trimmed.bytes().all(|b| b == b'-')
}

fn style_line(line: &str, base: usize, fence: &mut Option<usize>) -> StyledLine {
    let range = base..base + line.len();
    let mut segs = Segs::default();
    let mut lead = None;
    let mut task_char = None;
    let block;

    if let Some(ticks) = *fence {
        if is_fence_close(line, ticks) {
            *fence = None;
            block = Block::Fence;
            segs.push(range.clone(), Role::Marker, Flags::CODE);
        } else {
            block = Block::Code;
            segs.push(range.clone(), Role::Text, Flags::CODE);
        }
    } else if let Some(ticks) = fence_open(line) {
        *fence = Some(ticks);
        block = Block::Fence;
        segs.push(range.clone(), Role::Marker, Flags::CODE);
    } else if let Some((level, marker)) = heading(line) {
        block = Block::Heading(level);
        segs.push(base..base + marker, Role::Hide, Flags::default());
        inline(&line[marker..], base + marker, &mut segs);
    } else if is_rule(line) {
        block = Block::Rule;
        segs.push(range.clone(), Role::Hide, Flags::default());
    } else if let Some((quote, marker_end)) = quote_marker(line) {
        block = Block::Quote;
        segs.push(
            base..base + marker_end,
            Role::Replace("▎ "),
            Flags::default(),
        );
        lead = Some(Lead {
            kind: LeadKind::Quote,
            indent: quote,
            marker_end: base + marker_end,
        });
        inline(&line[marker_end..], base + marker_end, &mut segs);
    } else if let Some(item) = list_item(line) {
        segs.push(base..base + item.indent, Role::Text, Flags::default());
        match item.kind {
            ItemKind::Bullet(c) => {
                block = Block::Bullet;
                segs.push(
                    base + item.indent..base + item.marker_end,
                    Role::Replace("• "),
                    Flags::default(),
                );
                lead = Some(Lead {
                    kind: LeadKind::Bullet(c),
                    indent: item.indent,
                    marker_end: base + item.marker_end,
                });
            }
            ItemKind::Task(c, checked, at) => {
                block = Block::Task { checked };
                segs.push(
                    base + item.indent..base + item.marker_end,
                    Role::Replace(if checked { "☑ " } else { "☐ " }),
                    Flags::default(),
                );
                task_char = Some(base + at);
                lead = Some(Lead {
                    kind: LeadKind::Task(c),
                    indent: item.indent,
                    marker_end: base + item.marker_end,
                });
            }
            ItemKind::Numbered(number, delimiter) => {
                block = Block::Numbered;
                segs.push(
                    base + item.indent..base + item.marker_end,
                    Role::Marker,
                    Flags::default(),
                );
                lead = Some(Lead {
                    kind: LeadKind::Numbered { number, delimiter },
                    indent: item.indent,
                    marker_end: base + item.marker_end,
                });
            }
        }
        inline(&line[item.marker_end..], base + item.marker_end, &mut segs);
    } else {
        block = Block::Plain;
        inline(line, base, &mut segs);
    }

    StyledLine {
        range,
        block,
        segs: segs.out,
        lead,
        task_char,
    }
}

/// Bytes of leading spaces and the bytes through `>` and one optional space.
fn quote_marker(line: &str) -> Option<(usize, usize)> {
    let trimmed = line.trim_start();
    let indent = line.len() - trimmed.len();
    let rest = trimmed.strip_prefix('>')?;
    let space = usize::from(rest.starts_with(' '));
    Some((indent, indent + 1 + space))
}

enum ItemKind {
    Bullet(char),
    /// Marker character, checked, and the byte offset of the character between the brackets.
    Task(char, bool, usize),
    Numbered(u32, char),
}

struct Item {
    kind: ItemKind,
    indent: usize,
    marker_end: usize,
}

fn list_item(line: &str) -> Option<Item> {
    let indent = leading_spaces(line);
    if indent > 3 {
        return None;
    }
    let rest = &line[indent..];
    let mut chars = rest.chars();
    let first = chars.next()?;
    if first == '-' || first == '*' {
        let space = chars.next().filter(|c| c.is_whitespace())?;
        let marker = indent + 1 + space.len_utf8();
        let after = &line[marker..];
        for (open, checked) in [("[ ]", false), ("[x]", true), ("[X]", true)] {
            if let Some(tail) = after.strip_prefix(open) {
                let gap = tail.chars().next();
                if gap.is_none() || gap.is_some_and(char::is_whitespace) {
                    let end = marker + 3 + gap.map_or(0, char::len_utf8);
                    return Some(Item {
                        kind: ItemKind::Task(first, checked, marker + 1),
                        indent,
                        marker_end: end,
                    });
                }
            }
        }
        return Some(Item {
            kind: ItemKind::Bullet(first),
            indent,
            marker_end: marker,
        });
    }
    let digits = rest.bytes().take_while(u8::is_ascii_digit).count();
    if !(1..=9).contains(&digits) {
        return None;
    }
    let delimiter = rest[digits..]
        .chars()
        .next()
        .filter(|c| matches!(c, '.' | ')'))?;
    let space = rest[digits + 1..]
        .chars()
        .next()
        .filter(|c| c.is_whitespace())?;
    Some(Item {
        kind: ItemKind::Numbered(rest[..digits].parse().unwrap_or(1), delimiter),
        indent,
        marker_end: indent + digits + 1 + space.len_utf8(),
    })
}

// ---------------------------------------------------------------------------------------------
// Inline scanning
// ---------------------------------------------------------------------------------------------

fn is_word(c: char) -> bool {
    c.is_alphanumeric()
}

fn inline(text: &str, base: usize, out: &mut Segs) {
    if text.is_empty() {
        return;
    }
    if text.len() > MAX_STYLED_LINE_BYTES {
        out.push(base..base + text.len(), Role::Text, Flags::default());
        return;
    }
    let chars: Vec<(usize, char)> = text.char_indices().collect();
    let hi = chars.len();
    let mut scanner = Scanner {
        text,
        chars,
        base,
        out,
    };
    scanner.scan(0, hi, Flags::default());
}

/// Positions already known to have no partner, so unmatched markers cost one scan, not one each.
#[derive(Default)]
struct Memo {
    no_closer: [bool; 6],
    no_code_run: [bool; 8],
    no_bracket: bool,
    /// The first `]` found by an earlier scan, valid for any later `[` before it.
    bracket_at: Option<usize>,
}

struct Scanner<'a> {
    text: &'a str,
    chars: Vec<(usize, char)>,
    base: usize,
    out: &'a mut Segs,
}

impl Scanner<'_> {
    fn byte(&self, i: usize) -> usize {
        self.chars.get(i).map_or(self.text.len(), |c| c.0)
    }

    fn ch(&self, i: usize) -> char {
        self.chars[i].1
    }

    fn emit(&mut self, lo: usize, hi: usize, role: Role, flags: Flags) {
        let range = self.base + self.byte(lo)..self.base + self.byte(hi);
        self.out.push(range, role, flags);
    }

    fn run_len(&self, at: usize, hi: usize) -> usize {
        let c = self.ch(at);
        (at..hi).take_while(|j| self.ch(*j) == c).count()
    }

    fn scan(&mut self, lo: usize, hi: usize, flags: Flags) {
        let mut memo = Memo::default();
        let mut i = lo;
        while i < hi {
            let c = self.ch(i);
            match c {
                '\\' if i + 1 < hi && self.ch(i + 1).is_ascii_punctuation() => {
                    self.emit(i, i + 1, Role::Hide, flags);
                    self.emit(i + 1, i + 2, Role::Text, flags);
                    i += 2;
                }
                '`' => {
                    let n = self.run_len(i, hi);
                    if let Some(close) = self.code_span(i, n, hi, &mut memo) {
                        self.emit(i, i + n, Role::Hide, flags);
                        self.emit(
                            i + n,
                            close,
                            Role::Text,
                            Flags {
                                code: true,
                                ..flags
                            },
                        );
                        self.emit(close, close + n, Role::Hide, flags);
                        i = close + n;
                    } else {
                        self.emit(i, i + n, Role::Text, flags);
                        i += n;
                    }
                }
                '[' => {
                    if let Some((close, end)) = self.link(i, hi, &mut memo) {
                        self.emit(i, i + 1, Role::Hide, flags);
                        self.scan(
                            i + 1,
                            close,
                            Flags {
                                link: true,
                                ..flags
                            },
                        );
                        self.emit(close, end, Role::Hide, flags);
                        i = end;
                    } else {
                        self.emit(i, i + 1, Role::Text, flags);
                        i += 1;
                    }
                }
                '*' | '_' | '~' => {
                    let run = self.run_len(i, hi);
                    let kind = match c {
                        '*' => 0,
                        '_' => 1,
                        _ => 2,
                    };
                    let free = match kind {
                        0 => !flags.bold,
                        1 => !flags.italic,
                        _ => !flags.strike,
                    };
                    let opens = run <= 2
                        && free
                        && (i == lo || !is_word(self.ch(i - 1)))
                        && i + run < hi
                        && !self.ch(i + run).is_whitespace();
                    let close = if opens {
                        self.find_closer(i + run, hi, c, run, kind, &mut memo)
                    } else {
                        None
                    };
                    if let Some(close) = close {
                        let inner = match kind {
                            0 => Flags {
                                bold: true,
                                ..flags
                            },
                            1 => Flags {
                                italic: true,
                                ..flags
                            },
                            _ => Flags {
                                strike: true,
                                ..flags
                            },
                        };
                        self.emit(i, i + run, Role::Hide, flags);
                        self.scan(i + run, close, inner);
                        self.emit(close, close + run, Role::Hide, flags);
                        i = close + run;
                    } else {
                        self.emit(i, i + run, Role::Text, flags);
                        i += run;
                    }
                }
                'h' | 'l' if i == lo || !is_word(self.ch(i - 1)) => {
                    if let Some(end) = self.bare_link(i, hi) {
                        self.emit(
                            i,
                            end,
                            Role::Text,
                            Flags {
                                link: true,
                                ..flags
                            },
                        );
                        i = end;
                    } else {
                        self.emit(i, i + 1, Role::Text, flags);
                        i += 1;
                    }
                }
                _ => {
                    self.emit(i, i + 1, Role::Text, flags);
                    i += 1;
                }
            }
        }
    }

    /// The index of the run of exactly `n` backticks that closes a span opened at `at`.
    fn code_span(&self, at: usize, n: usize, hi: usize, memo: &mut Memo) -> Option<usize> {
        let slot = n.min(7);
        if memo.no_code_run[slot] {
            return None;
        }
        let mut j = at + n;
        while j < hi {
            if self.ch(j) == '`' {
                let run = self.run_len(j, hi);
                if run == n {
                    return Some(j);
                }
                j += run;
            } else {
                j += 1;
            }
        }
        memo.no_code_run[slot] = true;
        None
    }

    /// For `[text](url)` opening at `at`: the index of the `]` and the index after the `)`.
    fn link(&self, at: usize, hi: usize, memo: &mut Memo) -> Option<(usize, usize)> {
        if memo.no_bracket {
            return None;
        }
        let close = match memo.bracket_at {
            Some(k) if k > at => k,
            _ => match (at + 1..hi).find(|j| self.ch(*j) == ']') {
                Some(k) => {
                    memo.bracket_at = Some(k);
                    k
                }
                None => {
                    memo.no_bracket = true;
                    return None;
                }
            },
        };
        if close == at + 1 || close + 2 >= hi || self.ch(close + 1) != '(' {
            return None;
        }
        let mut j = close + 2;
        while j < hi {
            let c = self.ch(j);
            if c == ')' {
                return (j > close + 2).then_some((close, j + 1));
            }
            if c.is_whitespace() {
                return None;
            }
            j += 1;
        }
        None
    }

    /// The end of a bare address (`http://`, `https://`, `lazy://`) starting at `at`.
    fn bare_link(&self, at: usize, hi: usize) -> Option<usize> {
        let rest = &self.text[self.byte(at)..self.byte(hi)];
        let prefix = ["https://", "http://", "lazy://"]
            .into_iter()
            .find(|p| rest.starts_with(p))?;
        let mut end = at + prefix.len();
        if end >= hi || self.ch(end).is_whitespace() {
            return None;
        }
        while end < hi && !self.ch(end).is_whitespace() {
            end += 1;
        }
        while end > at + prefix.len()
            && matches!(
                self.ch(end - 1),
                '.' | ',' | ';' | ':' | '!' | '?' | ')' | ']' | '}' | '\'' | '"' | '*' | '_' | '~'
            )
        {
            end -= 1;
        }
        (end > at + prefix.len()).then_some(end)
    }

    /// The token a closing-marker search steps over, as the parser would consume it.
    fn skip_atom(&self, j: usize, hi: usize, memo: &mut Memo) -> Option<usize> {
        match self.ch(j) {
            '\\' if j + 1 < hi && self.ch(j + 1).is_ascii_punctuation() => Some(j + 2),
            '`' => {
                let n = self.run_len(j, hi);
                Some(
                    self.code_span(j, n, hi, memo)
                        .map_or(j + n, |close| close + n),
                )
            }
            '[' => self.link(j, hi, memo).map(|(_, end)| end),
            'h' | 'l' if j == 0 || !is_word(self.ch(j - 1)) => self.bare_link(j, hi),
            _ => None,
        }
    }

    fn find_closer(
        &self,
        from: usize,
        hi: usize,
        c: char,
        len: usize,
        kind: usize,
        memo: &mut Memo,
    ) -> Option<usize> {
        let slot = kind * 2 + (len - 1);
        if memo.no_closer[slot] {
            return None;
        }
        let mut scratch = Memo::default();
        let mut j = from;
        while j < hi {
            if let Some(after) = self.skip_atom(j, hi, &mut scratch) {
                j = after;
                continue;
            }
            if self.ch(j) == c {
                let run = self.run_len(j, hi);
                let end = j + run;
                if run == len
                    && j > from
                    && !self.ch(j - 1).is_whitespace()
                    && (end >= hi || !is_word(self.ch(end)))
                {
                    return Some(j);
                }
                j = end;
            } else {
                j += 1;
            }
        }
        memo.no_closer[slot] = true;
        None
    }
}

// ---------------------------------------------------------------------------------------------
// Display text
// ---------------------------------------------------------------------------------------------

/// How a run of the display text is drawn.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RunKind {
    Text,
    /// Syntax shown because the caret is on its line.
    Dim,
    /// A bullet, checkbox, or quote bar standing in for syntax.
    Glyph,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Run {
    pub len: usize,
    pub kind: RunKind,
    pub flags: Flags,
}

/// A checkbox that can be clicked: where it is drawn and which model byte holds its state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskBox {
    pub display: Range<usize>,
    pub model_char: usize,
    pub checked: bool,
}

/// One logical line of the display text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DisplayLine {
    pub block: Block,
    /// Offset of the line's first byte in [`Display::text`].
    pub start: usize,
    pub len: usize,
    /// Whether the line shows its syntax because the caret (or selection) touches it.
    pub active: bool,
    pub runs: Vec<Run>,
    pub task: Option<TaskBox>,
}

#[derive(Debug, Clone, Copy)]
struct Piece {
    model: usize,
    model_len: usize,
    display: usize,
    display_len: usize,
    /// The display bytes are the model bytes.
    identity: bool,
}

/// The text to shape, its lines and styles, and the map between it and the model text.
#[derive(Debug, Clone)]
pub struct Display {
    pub text: String,
    pub lines: Vec<DisplayLine>,
    pieces: Vec<Piece>,
}

/// Builds the display text for `lines` (from [`analyze`] of `text`), showing the syntax of the
/// lines `active.0..=active.1` and hiding it elsewhere.
pub fn build(text: &str, lines: &[StyledLine], active: (usize, usize)) -> Display {
    let mut out = String::with_capacity(text.len());
    let mut pieces: Vec<Piece> = Vec::new();
    let mut display_lines = Vec::with_capacity(lines.len());

    let push_piece = |pieces: &mut Vec<Piece>, piece: Piece| {
        if piece.identity
            && let Some(last) = pieces.last_mut()
            && last.identity
            && last.model + last.model_len == piece.model
            && last.display + last.display_len == piece.display
        {
            last.model_len += piece.model_len;
            last.display_len += piece.display_len;
            return;
        }
        pieces.push(piece);
    };

    for (n, line) in lines.iter().enumerate() {
        if n > 0 {
            push_piece(
                &mut pieces,
                Piece {
                    model: line.range.start - 1,
                    model_len: 1,
                    display: out.len(),
                    display_len: 1,
                    identity: true,
                },
            );
            out.push('\n');
        }
        let is_active = n >= active.0 && n <= active.1;
        let start = out.len();
        let mut runs: Vec<Run> = Vec::new();
        let mut task = None;
        for seg in &line.segs {
            let source = &text[seg.range.clone()];
            let (shown, kind, identity) = match seg.role {
                Role::Text => (source, RunKind::Text, true),
                Role::Marker => (source, RunKind::Dim, true),
                Role::Hide | Role::Replace(_) if is_active => (source, RunKind::Dim, true),
                Role::Hide => ("", RunKind::Dim, false),
                Role::Replace(glyph) => (glyph, RunKind::Glyph, false),
            };
            let at = out.len();
            if let (Role::Replace(_), false, Block::Task { checked }) =
                (seg.role, is_active, line.block)
                && let Some(model_char) = line.task_char
            {
                let width = shown.chars().next().map_or(0, char::len_utf8);
                task = Some(TaskBox {
                    display: at..at + width,
                    model_char,
                    checked,
                });
            }
            push_piece(
                &mut pieces,
                Piece {
                    model: seg.range.start,
                    model_len: seg.range.len(),
                    display: at,
                    display_len: shown.len(),
                    identity,
                },
            );
            out.push_str(shown);
            if !shown.is_empty() {
                let run = Run {
                    len: shown.len(),
                    kind,
                    flags: seg.flags,
                };
                match runs.last_mut() {
                    Some(last) if last.kind == run.kind && last.flags == run.flags => {
                        last.len += run.len
                    }
                    _ => runs.push(run),
                }
            }
        }
        display_lines.push(DisplayLine {
            block: line.block,
            start,
            len: out.len() - start,
            active: is_active,
            runs,
            task,
        });
    }

    Display {
        text: out,
        lines: display_lines,
        pieces,
    }
}

impl Display {
    /// The display offset of a model offset. An offset inside removed syntax maps to the end of
    /// what replaced it.
    pub fn to_display(&self, model: usize) -> usize {
        let Some(ix) = self
            .pieces
            .partition_point(|p| p.model <= model)
            .checked_sub(1)
        else {
            return 0;
        };
        let piece = self.pieces[ix];
        if model >= piece.model + piece.model_len {
            return piece.display + piece.display_len;
        }
        if piece.identity {
            piece.display + (model - piece.model)
        } else if model == piece.model {
            piece.display
        } else {
            piece.display + piece.display_len
        }
    }

    /// The model offset of a display offset. Where removed syntax sits at that offset, the result
    /// is after it, so the caret lands beside the text.
    pub fn to_model(&self, display: usize) -> usize {
        let Some(ix) = self
            .pieces
            .partition_point(|p| p.display <= display)
            .checked_sub(1)
        else {
            return 0;
        };
        let piece = self.pieces[ix];
        if display >= piece.display + piece.display_len {
            return piece.model + piece.model_len;
        }
        if piece.identity {
            piece.model + (display - piece.display)
        } else if display == piece.display {
            piece.model
        } else {
            piece.model + piece.model_len
        }
    }

    /// The index of the display line containing display offset `offset`.
    pub fn line_at(&self, offset: usize) -> usize {
        self.lines
            .partition_point(|l| l.start <= offset)
            .saturating_sub(1)
    }
}

// ---------------------------------------------------------------------------------------------
// Editing rules
// ---------------------------------------------------------------------------------------------

/// What pressing Enter at the end of a list item, task, or quote does.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnterEdit {
    /// The bytes to replace; the caret ends after `insert`.
    pub range: Range<usize>,
    pub insert: String,
}

/// The edit for Enter with the caret at `caret` (nothing selected), if the line is a list item
/// or quote and the caret is past its marker: a new line carrying the next marker, or, on an
/// item with no text, removal of the marker so the list ends. `None` means a plain line break.
pub fn enter_edit(lines: &[StyledLine], text: &str, caret: usize) -> Option<EnterEdit> {
    let ix = lines.partition_point(|l| l.range.end < caret);
    let line = lines.get(ix)?;
    let lead = line.lead?;
    if caret < lead.marker_end {
        return None;
    }
    if text[lead.marker_end..line.range.end].trim().is_empty() {
        return Some(EnterEdit {
            range: line.range.clone(),
            insert: String::new(),
        });
    }
    let indent = " ".repeat(lead.indent);
    let marker = match lead.kind {
        LeadKind::Bullet(c) => format!("{c} "),
        LeadKind::Task(c) => format!("{c} [ ] "),
        LeadKind::Numbered { number, delimiter } => {
            format!("{}{delimiter} ", number.saturating_add(1))
        }
        LeadKind::Quote => "> ".to_owned(),
    };
    Some(EnterEdit {
        range: caret..caret,
        insert: format!("\n{indent}{marker}"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn one(text: &str) -> StyledLine {
        let mut lines = analyze(text);
        assert_eq!(lines.len(), 1, "{text:?}");
        lines.remove(0)
    }

    fn pieces(text: &str) -> Vec<(String, Role, Flags)> {
        one(text)
            .segs
            .iter()
            .map(|s| (text[s.range.clone()].to_owned(), s.role, s.flags))
            .collect()
    }

    fn bold() -> Flags {
        Flags {
            bold: true,
            ..Flags::default()
        }
    }

    fn display(text: &str, caret: usize) -> Display {
        let lines = analyze(text);
        let active = active_lines(&lines, caret, caret);
        build(text, &lines, active)
    }

    #[test]
    fn every_line_is_tiled_by_its_segments() {
        let text = "# Title\n\n- [ ] a *b* `c` [d](e)\n1. x\n> q\n---\n```rs\nfn *x*\n```\nplain";
        for line in analyze(text) {
            let mut at = line.range.start;
            for seg in &line.segs {
                assert_eq!(seg.range.start, at);
                at = seg.range.end;
            }
            assert_eq!(at, line.range.end, "{line:?}");
        }
    }

    #[test]
    fn headings_need_a_space_and_one_to_six_hashes() {
        assert_eq!(one("# x").block, Block::Heading(1));
        assert_eq!(one("###### x").block, Block::Heading(6));
        assert_eq!(one("####### x").block, Block::Plain);
        assert_eq!(one("#x").block, Block::Plain);
        assert_eq!(one("#").block, Block::Plain);
        assert_eq!(one("# ").block, Block::Heading(1));
        let p = pieces("## Risks");
        assert_eq!(p[0].0, "## ");
        assert_eq!(p[0].1, Role::Hide);
        assert_eq!(p[1].0, "Risks");
    }

    #[test]
    fn inline_marks_hide_their_markers_and_flag_the_text() {
        let p = pieces("a **b** c");
        assert_eq!(p[0], ("a ".into(), Role::Text, Flags::default()));
        assert_eq!(p[1], ("**".into(), Role::Hide, Flags::default()));
        assert_eq!(p[2], ("b".into(), Role::Text, bold()));
        assert_eq!(p[3], ("**".into(), Role::Hide, Flags::default()));
        assert_eq!(p[4], (" c".into(), Role::Text, Flags::default()));
        // One marker is enough, as in messages.
        assert_eq!(pieces("*b*")[1].2, bold());
        assert!(pieces("_i_")[1].2.italic);
        assert!(pieces("~s~")[1].2.strike);
        assert!(pieces("~~s~~")[1].2.strike);
    }

    #[test]
    fn markers_follow_the_word_rules() {
        // Not at the start of a word, or followed by a space, or never closed: plain text.
        for plain in ["snake_case_name", "2*3*4", "a * b * c", "*open", "x*y*"] {
            let line = one(plain);
            assert!(
                line.segs.iter().all(|s| s.role == Role::Text),
                "{plain:?} stayed plain"
            );
        }
        assert!(
            one("(*b*)")
                .segs
                .iter()
                .any(|s| s.role == Role::Hide && s.range == (1..2))
        );
    }

    #[test]
    fn marks_nest_and_code_is_literal() {
        let p = pieces("**a _b_ c**");
        let italic = p.iter().find(|s| s.0 == "b").unwrap();
        assert!(italic.2.bold && italic.2.italic);
        let p = pieces("`a *b* c`");
        assert_eq!(p[1].0, "a *b* c");
        assert!(p[1].2.code && !p[1].2.bold);
        // A longer run holds a shorter one.
        let p = pieces("``a`b``");
        assert_eq!(p[1].0, "a`b");
    }

    #[test]
    fn links_hide_their_address() {
        let p = pieces("see [docs](https://x.y/z) now");
        assert_eq!(p[1], ("[".into(), Role::Hide, Flags::default()));
        assert!(p[2].2.link && p[2].0 == "docs");
        assert_eq!(
            p[3],
            ("](https://x.y/z)".into(), Role::Hide, Flags::default())
        );
        // Not a link: no address, or a space in it.
        for plain in ["[a]", "[a]()", "[a](b c)", "[](x)"] {
            assert!(
                one(plain).segs.iter().all(|s| s.role == Role::Text),
                "{plain:?}"
            );
        }
        let bare = pieces("go to https://x.y/z. ok");
        assert!(bare[1].2.link);
        assert_eq!(bare[1].0, "https://x.y/z");
    }

    #[test]
    fn escapes_hide_the_backslash() {
        let p = pieces(r"\*not bold\*");
        assert_eq!(p[0], (r"\".into(), Role::Hide, Flags::default()));
        assert!(p.iter().all(|s| !s.2.bold));
    }

    #[test]
    fn lists_quotes_tasks_and_rules() {
        assert_eq!(one("- a").block, Block::Bullet);
        assert_eq!(one("* a").block, Block::Bullet);
        assert_eq!(one("  - a").block, Block::Bullet);
        assert_eq!(
            one("    - a").block,
            Block::Plain,
            "four spaces is not a list"
        );
        assert_eq!(one("-a").block, Block::Plain);
        assert_eq!(one("1. a").block, Block::Numbered);
        assert_eq!(one("2) a").block, Block::Numbered);
        assert_eq!(one("1.a").block, Block::Plain);
        assert_eq!(one("> a").block, Block::Quote);
        assert_eq!(one(">a").block, Block::Quote);
        assert_eq!(one("- [ ] a").block, Block::Task { checked: false });
        assert_eq!(one("- [x] a").block, Block::Task { checked: true });
        assert_eq!(one("- [X] a").block, Block::Task { checked: true });
        assert_eq!(one("- [y] a").block, Block::Bullet);
        assert_eq!(one("- [ ]a").block, Block::Bullet);
        assert_eq!(one("---").block, Block::Rule);
        assert_eq!(one("------").block, Block::Rule);
        assert_eq!(one("--").block, Block::Plain);
        assert_eq!(one("- a").segs[0].role, Role::Replace("• "));
        assert_eq!(one("- [x] a").segs[0].role, Role::Replace("☑ "));
        assert_eq!(one("- [ ] a").task_char, Some(3));
    }

    #[test]
    fn fences_make_code_until_they_close_or_the_text_ends() {
        let lines = analyze("a\n```rust\n# not *a* heading\n```\n# real");
        let blocks: Vec<_> = lines.iter().map(|l| l.block).collect();
        assert_eq!(
            blocks,
            [
                Block::Plain,
                Block::Fence,
                Block::Code,
                Block::Fence,
                Block::Heading(1)
            ]
        );
        assert!(lines[2].segs.iter().all(|s| s.flags.code));
        // Unterminated: everything after is code.
        let open = analyze("```\nx\n# y");
        assert_eq!(open[2].block, Block::Code);
        // Inline triple backticks are not a fence.
        assert_eq!(one("a ```b``` c").block, Block::Plain);
        // A longer fence is closed only by an equal or longer run.
        let long = analyze("````\n```\n````");
        assert_eq!(long[1].block, Block::Code);
        assert_eq!(long[2].block, Block::Fence);
    }

    #[test]
    fn a_very_long_line_is_left_plain() {
        let text = format!("*a* {}", "x".repeat(MAX_STYLED_LINE_BYTES));
        let line = one(&text);
        assert!(line.segs.iter().all(|s| s.role == Role::Text));
    }

    #[test]
    fn unterminated_and_adversarial_input_is_fast_and_tiled() {
        for text in [
            "*a ".repeat(1_300),
            "`a ".repeat(1_300),
            "[a ".repeat(1_300),
            "[a](b ".repeat(600),
            "*_~".repeat(1_300),
            "\\".repeat(3_000),
        ] {
            let started = std::time::Instant::now();
            let lines = analyze(&text);
            assert!(
                started.elapsed() < std::time::Duration::from_millis(500),
                "{:?} took {:?}",
                &text[..8],
                started.elapsed()
            );
            let covered: usize = lines[0].segs.iter().map(|s| s.range.len()).sum();
            assert_eq!(covered, text.len());
        }
    }

    #[test]
    fn cjk_and_emoji_are_styled_without_splitting_characters() {
        let text = "**日本語** と *🎉 party* `コード`";
        let lines = analyze(text);
        for seg in &lines[0].segs {
            assert!(text.is_char_boundary(seg.range.start));
            assert!(text.is_char_boundary(seg.range.end));
        }
        let shown = display(text, text.len());
        assert!(shown.lines[0].active);
        assert_eq!(shown.text, text);
        let away = display(&format!("{text}\nx"), text.len() + 2);
        assert_eq!(
            away.text.lines().next().unwrap(),
            "日本語 と 🎉 party コード"
        );
        let model = format!("{text}\nx");
        for at in (0..=away.text.len()).filter(|a| away.text.is_char_boundary(*a)) {
            assert!(model.is_char_boundary(away.to_model(at)), "display {at}");
        }
    }

    #[test]
    fn the_caret_line_shows_its_syntax_and_the_others_do_not() {
        let text = "# Title\nsome **bold** text\n- item";
        let on_heading = display(text, 3);
        assert_eq!(
            on_heading.text, "# Title\nsome bold text\n• item",
            "only the active line keeps its markers"
        );
        let on_bold = display(text, 12);
        assert_eq!(on_bold.text, "Title\nsome **bold** text\n• item");
        let on_item = display(text, text.len());
        assert_eq!(on_item.text, "Title\nsome bold text\n- item");
        assert!(on_item.lines[2].active && !on_item.lines[0].active);
    }

    #[test]
    fn offsets_map_both_ways_around_hidden_syntax() {
        let text = "a **b** c\nnext";
        let d = display(text, text.len()); // caret on the second line: line 0 is rendered
        assert_eq!(d.text, "a b c\nnext");
        // Characters outside the markers.
        assert_eq!(d.to_display(0), 0);
        assert_eq!(d.to_display(2), 2, "start of the opening marker");
        assert_eq!(
            d.to_display(4),
            2,
            "the bold text starts where the marker was"
        );
        assert_eq!(
            d.to_display(5),
            3,
            "after the text, before the closing marker"
        );
        assert_eq!(d.to_display(7), 3);
        assert_eq!(d.to_display(9), 5, "the line break");
        assert_eq!(d.to_display(text.len()), d.text.len());
        // Back: a display offset beside hidden syntax lands after it.
        assert_eq!(d.to_model(2), 4);
        assert_eq!(d.to_model(3), 7);
        assert_eq!(
            d.to_model(5),
            9,
            "end of the line is after the closing marker"
        );
        assert_eq!(d.to_model(d.text.len()), text.len());
        // Every display offset maps to a model offset that maps back to it.
        for display_at in 0..=d.text.len() {
            if d.text.is_char_boundary(display_at) {
                let model = d.to_model(display_at);
                assert!(text.is_char_boundary(model));
                assert_eq!(d.to_display(model), display_at, "display {display_at}");
            }
        }
    }

    #[test]
    fn a_replaced_marker_maps_to_its_ends() {
        let text = "x\n- item";
        let d = display(text, 0);
        assert_eq!(d.text, "x\n• item");
        assert_eq!(d.to_display(2), 2, "before the marker");
        assert_eq!(
            d.to_display(3),
            2 + "• ".len(),
            "inside it: after the glyph"
        );
        assert_eq!(d.to_display(4), 2 + "• ".len());
        assert_eq!(d.to_model(2), 2);
        assert_eq!(d.to_model(2 + "• ".len()), 4);
    }

    #[test]
    fn selection_across_lines_keeps_all_of_them_raw() {
        let text = "# a\n**b**\nc";
        let lines = analyze(text);
        let active = active_lines(&lines, 1, 8);
        assert_eq!(active, (0, 1));
        let d = build(text, &lines, active);
        assert_eq!(d.text, "# a\n**b**\nc");
    }

    #[test]
    fn active_lines_include_a_caret_at_a_line_end_and_empty_lines() {
        let lines = analyze("ab\n\ncd");
        assert_eq!(active_lines(&lines, 2, 2), (0, 0), "end of the first line");
        assert_eq!(active_lines(&lines, 3, 3), (1, 1), "the empty line");
        assert_eq!(active_lines(&lines, 4, 4), (2, 2));
        assert_eq!(active_lines(&lines, 0, 6), (0, 2));
        assert_eq!(active_lines(&analyze(""), 0, 0), (0, 0));
    }

    #[test]
    fn a_rendered_task_exposes_its_box_and_an_active_one_does_not() {
        let text = "- [ ] a\nb";
        let away = display(text, 8);
        let task = away.lines[0].task.clone().expect("box");
        assert_eq!(&away.text[task.display.clone()], "☐");
        assert_eq!(task.model_char, 3);
        assert!(!task.checked);
        let on = display(text, 2);
        assert!(on.lines[0].task.is_none());
        assert_eq!(on.text, text);
    }

    #[test]
    fn runs_cover_each_display_line_exactly() {
        let text = "# T\n- a *b* [c](d) `e`\n```\nx\n```";
        let lines = analyze(text);
        for caret in 0..=text.len() {
            let d = build(text, &lines, active_lines(&lines, caret, caret));
            for line in &d.lines {
                assert_eq!(line.runs.iter().map(|r| r.len).sum::<usize>(), line.len);
            }
        }
    }

    fn enter(text: &str, caret: usize) -> Option<(String, usize)> {
        let lines = analyze(text);
        enter_edit(&lines, text, caret).map(|e| {
            let mut out = text.to_owned();
            out.replace_range(e.range.clone(), &e.insert);
            (out, e.range.start + e.insert.len())
        })
    }

    #[test]
    fn enter_continues_lists_tasks_and_quotes() {
        assert_eq!(enter("- a", 3), Some(("- a\n- ".into(), 6)));
        assert_eq!(enter("* a", 3), Some(("* a\n* ".into(), 6)));
        assert_eq!(enter("  - a", 5), Some(("  - a\n  - ".into(), 10)));
        assert_eq!(enter("1. a", 4), Some(("1. a\n2. ".into(), 8)));
        assert_eq!(enter("9) a", 4), Some(("9) a\n10) ".into(), 9)));
        assert_eq!(enter("- [x] a", 7), Some(("- [x] a\n- [ ] ".into(), 14)));
        assert_eq!(enter("> a", 3), Some(("> a\n> ".into(), 6)));
        // In the middle of an item the rest moves to the new item.
        assert_eq!(enter("- ab", 3), Some(("- a\n- b".into(), 6)));
    }

    #[test]
    fn enter_on_an_empty_item_ends_the_list() {
        assert_eq!(enter("- a\n- ", 6), Some(("- a\n".into(), 4)));
        assert_eq!(enter("1. ", 3), Some(("".into(), 0)));
        assert_eq!(enter("> ", 2), Some(("".into(), 0)));
        assert_eq!(enter("- [ ] ", 6), Some(("".into(), 0)));
    }

    #[test]
    fn enter_in_the_marker_or_in_plain_text_is_a_plain_break() {
        assert_eq!(enter("- a", 1), None);
        assert_eq!(enter("- a", 0), None);
        assert_eq!(enter("plain", 5), None);
        assert_eq!(enter("# h", 3), None);
        assert_eq!(enter("```\n- a\n```", 7), None, "inside a fence");
    }
}
