//! The custom GPUI element that shapes, lays out, and paints the text.
//!
//! Layout runs inside GPUI's measure callback because the number of wrapped rows, and so the
//! input's height, depends on the width the parent grants. Shaping is cached by [`LayoutKey`], so
//! frames that change nothing do no shaping work.
//!
//! A plain input shapes its text in one call. A live-markdown input (see [`super::live`]) shapes
//! the display text one logical line at a time, because a heading has its own font size, and
//! reuses the lines of the previous layout that did not change.

use std::cell::RefCell;
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::rc::Rc;

use gpui::{
    AvailableSpace, Bounds, ContentMask, Element, ElementId, ElementInputHandler, Entity, Font,
    FontFeatures, FontStyle, FontWeight, GlobalElementId, Hsla, InspectorElementId, IntoElement,
    LayoutId, Pixels, SharedString, Size, StrikethroughStyle, Style, TextAlign, TextRun, TextStyle,
    UnderlineStyle, Window, WrappedLine, fill, font, hsla, point, px, relative, size,
};

use super::layout::{LayoutKey, LayoutState, LiveColors, LiveKey};
use super::live::{self, Block, DisplayLine, RunKind, StyledLine};
use super::view::TextInput;
use crate::fonts::MONO_FAMILY;
use crate::theme::Theme;

pub(crate) struct TextElement {
    pub input: Entity<TextInput>,
}

impl IntoElement for TextElement {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for TextElement {
    type RequestLayoutState = ();
    type PrepaintState = ();

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut gpui::App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        let input = self.input.clone();
        let cell = input.read(cx).layout.clone();
        let text_style = window.text_style();
        let mut style = Style::default();
        style.size.width = relative(1.).into();
        let layout_id =
            window.request_measured_layout(style, move |known, available, window, cx| {
                measure(&input, &cell, &text_style, known, available, window, cx)
            });
        (layout_id, ())
    }

    fn prepaint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        window: &mut Window,
        cx: &mut gpui::App,
    ) -> Self::PrepaintState {
        let notify = self.input.update(cx, |input, _| {
            input.resolve_scroll(bounds);
            std::mem::take(&mut input.platform_dirty)
        });
        if notify {
            let handle = self.input.read(cx).focus_handle.clone();
            window.notify_text_input_changed(&handle);
        }
    }

    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        _prepaint: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut gpui::App,
    ) {
        let input = self.input.read(cx);
        let focus_handle = input.focus_handle.clone();
        let cell = input.layout.clone();
        let style = input.style.clone();
        let scroll = input.scroll;
        let revision = input.model.revision();
        let editable = input.is_enabled();
        let selection = input.model.selection();
        // The offsets are read before the layout is borrowed mutably below: the conversion
        // consults the layout's display map.
        let selection_display = input.display_offset(selection.range().start)
            ..input.display_offset(selection.range().end);
        let caret_display = input.display_offset(selection.head);

        window.handle_input(
            &focus_handle,
            ElementInputHandler::new(bounds, self.input.clone()),
            cx,
        );

        let mut layout = cell.borrow_mut();
        let Some(layout) = layout.as_mut() else {
            return;
        };
        layout.bounds = Some(bounds);
        if layout.revision != revision {
            // The text changed after this layout was shaped; the next frame lays it out again.
            return;
        }

        let focused = focus_handle.is_focused(window);
        let origin = bounds.origin - scroll;
        let colors = layout.key.live.as_ref().map(|live| live.colors);
        let view_top = scroll.y;
        let view_bottom = scroll.y + bounds.size.height;
        window.with_content_mask(Some(ContentMask { bounds }), |window| {
            if let (Some(display), Some(colors)) = (&layout.display, colors) {
                for (ix, line) in display.lines.iter().enumerate() {
                    let top = layout.line_tops[ix];
                    let bottom = layout.line_bottom(ix);
                    if bottom < view_top || top > view_bottom {
                        continue;
                    }
                    match line.block {
                        Block::Code | Block::Fence => {
                            let surface = Bounds::new(
                                origin + point(px(0.), top),
                                size(bounds.size.width, bottom - top),
                            );
                            window.paint_quad(fill(surface, colors.code_surface));
                        }
                        Block::Rule if !line.active => {
                            let middle = top + (bottom - top) / 2.;
                            let rule = Bounds::new(
                                origin + point(px(0.), middle),
                                size(bounds.size.width, px(1.)),
                            );
                            window.paint_quad(fill(rule, colors.rule));
                        }
                        _ => {}
                    }
                }
            }
            if focused && !selection.is_empty() {
                for rect in layout.selection_rects(selection_display) {
                    let rect = Bounds::new(origin + rect.origin, rect.size);
                    window.paint_quad(fill(rect, style.selection_color));
                }
            }
            for (ix, (line, top)) in layout.lines.iter().zip(&layout.line_tops).enumerate() {
                if layout.line_bottom(ix) < view_top || *top > view_bottom {
                    continue;
                }
                let at = origin + point(px(0.), *top);
                if let Err(error) = line.paint(
                    at,
                    layout.row_heights[ix],
                    TextAlign::Left,
                    None,
                    window,
                    cx,
                ) {
                    tracing::warn!(%error, "failed to paint a line of text input");
                }
            }
            if focused && editable && selection.is_empty() {
                let caret = layout.point_for_offset(caret_display);
                let rect = Bounds::new(
                    origin + caret,
                    gpui::size(style.caret_width, layout.height_for_offset(caret_display)),
                );
                window.paint_quad(fill(rect, style.caret_color));
            }
        });
    }
}

/// GPUI's measure callback: shapes the text for the granted width (if the cached layout does not
/// already match) and reports the size of the visible area.
fn measure(
    input: &Entity<TextInput>,
    cell: &Rc<RefCell<Option<LayoutState>>>,
    text_style: &TextStyle,
    known: Size<Option<Pixels>>,
    available: Size<AvailableSpace>,
    window: &mut Window,
    cx: &mut gpui::App,
) -> Size<Pixels> {
    let this = input.read(cx);
    let wrap_width = if this.single_line {
        None
    } else {
        known.width.or(match available.width {
            AvailableSpace::Definite(width) => Some(width),
            _ => None,
        })
    };
    let font = text_style.font();
    let live_lines = this.live_lines();
    let live_key = live_lines.as_ref().map(|lines| {
        let range = this.active_range();
        LiveKey {
            active: live::active_lines(lines, range.start, range.end),
            marked: this.model.marked_range(),
            colors: live_colors(&this.style, cx.try_global::<Theme>()),
        }
    });
    let key = LayoutKey {
        revision: this.model.revision(),
        wrap_width,
        font: font.clone(),
        font_size: this.style.font_size,
        line_height: this.style.line_height,
        text_color: this.style.text_color,
        placeholder_color: this.style.placeholder_color,
        placeholder: this.placeholder.clone(),
        masked: this.masked,
        live: live_key.clone(),
    };

    let up_to_date = cell.borrow().as_ref().is_some_and(|state| state.key == key);
    if !up_to_date {
        let live_state = match (&live_lines, &live_key) {
            (Some(lines), Some(live_key)) => {
                shape_live(this, cell, lines, live_key, &key, &font, window)
            }
            _ => None,
        };
        let state = live_state.unwrap_or_else(|| shape_plain(this, key, &font, wrap_width, window));
        *cell.borrow_mut() = Some(state);
    }

    let guard = cell.borrow();
    let Some(state) = guard.as_ref() else {
        return Size::default();
    };
    let height = if state.display.is_some() {
        // Rows differ in height: the content, between one body row and the limit.
        state
            .content_size
            .height
            .min(state.line_height * this.max_lines as f32)
            .max(state.line_height)
    } else {
        let rows = if this.single_line || state.showing_placeholder {
            1
        } else {
            state.rows.len().clamp(1, this.max_lines)
        };
        state.line_height * rows as f32
    };
    Size {
        width: known.width.unwrap_or(state.content_size.width),
        height,
    }
}

/// Shapes the whole text in one call, every row the body line height.
fn shape_plain(
    this: &TextInput,
    key: LayoutKey,
    font: &Font,
    wrap_width: Option<Pixels>,
    window: &mut Window,
) -> LayoutState {
    let showing_placeholder = this.model.is_empty();
    let text = this.display_text();
    // Emoji are drawn by the bundled color emoji font, not left to the system's fallback.
    let runs = crate::emoji_font::with_emoji_font(
        &text,
        runs_for(this, text.len(), font, showing_placeholder),
    );
    let lines: Vec<WrappedLine> =
        match window
            .text_system()
            .shape_text(text, this.style.font_size, &runs, wrap_width, None)
        {
            Ok(lines) => lines.into_iter().collect(),
            Err(error) => {
                tracing::warn!(%error, "failed to shape text input content");
                Vec::new()
            }
        };
    let heights = vec![this.style.line_height; lines.len()];
    LayoutState::new(
        key,
        lines,
        heights,
        this.style.line_height,
        showing_placeholder,
    )
}

/// The colors for live markdown: the theme's if one is installed, else ones derived from the
/// input's own style.
fn live_colors(style: &super::style::TextInputStyle, theme: Option<&Theme>) -> LiveColors {
    match theme {
        Some(theme) => LiveColors {
            dim: theme.colors.text_faint,
            glyph: theme.colors.accent,
            link: theme.colors.accent,
            muted: theme.colors.text_muted,
            code_surface: theme.colors.code_surface,
            rule: theme.colors.border_strong,
        },
        None => LiveColors {
            dim: style.placeholder_color,
            glyph: style.text_color,
            link: style.text_color,
            muted: style.placeholder_color,
            code_surface: hsla(0., 0., 0.5, 0.15),
            rule: style.placeholder_color,
        },
    }
}

fn hash_color(hasher: &mut impl Hasher, color: Hsla) {
    for part in [color.h, color.s, color.l, color.a] {
        part.to_bits().hash(hasher);
    }
}

/// Shapes live markdown one logical line at a time. Lines identical to ones in the previous layout
/// are taken from it. `None` if the text system fails, so the caller shapes plainly instead.
fn shape_live(
    this: &TextInput,
    cell: &Rc<RefCell<Option<LayoutState>>>,
    lines: &[StyledLine],
    live: &LiveKey,
    key: &LayoutKey,
    font: &Font,
    window: &mut Window,
) -> Option<LayoutState> {
    let display = Rc::new(live::build(this.model.text(), lines, live.active));
    let marked = live
        .marked
        .as_ref()
        .map(|range| display.to_display(range.start)..display.to_display(range.end));

    // Lines of the previous layout by signature; each can be taken over once.
    let mut previous: HashMap<u64, Vec<WrappedLine>> = HashMap::new();
    if let Some(old) = cell.borrow_mut().as_mut() {
        let old_lines = std::mem::take(&mut old.lines);
        for (sig, line) in std::mem::take(&mut old.sigs).into_iter().zip(old_lines) {
            previous.entry(sig).or_default().push(line);
        }
    }

    let mut style_hasher = std::collections::hash_map::DefaultHasher::new();
    format!("{font:?}").hash(&mut style_hasher);
    key.wrap_width
        .map(|w| f32::from(w).to_bits())
        .hash(&mut style_hasher);
    f32::from(key.font_size).to_bits().hash(&mut style_hasher);
    f32::from(key.line_height).to_bits().hash(&mut style_hasher);
    hash_color(&mut style_hasher, key.text_color);
    for color in [
        live.colors.dim,
        live.colors.glyph,
        live.colors.link,
        live.colors.muted,
        live.colors.code_surface,
    ] {
        hash_color(&mut style_hasher, color);
    }
    let style_sig = style_hasher.finish();

    let mut shaped = Vec::with_capacity(display.lines.len());
    let mut heights = Vec::with_capacity(display.lines.len());
    let mut sigs = Vec::with_capacity(display.lines.len());
    for line in &display.lines {
        let text = &display.text[line.start..line.start + line.len];
        let underline = marked.as_ref().and_then(|range| {
            let from = range.start.max(line.start);
            let to = range.end.min(line.start + line.len);
            (from < to).then(|| from - line.start..to - line.start)
        });

        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        style_sig.hash(&mut hasher);
        text.hash(&mut hasher);
        line.block.hash(&mut hasher);
        line.runs.hash(&mut hasher);
        underline.hash(&mut hasher);
        let sig = hasher.finish();

        let size = this.style.font_size * line.block.scale();
        heights.push(this.style.line_height * line.block.line_scale());
        sigs.push(sig);
        if let Some(reused) = previous.get_mut(&sig).and_then(Vec::pop)
            && reused.text.as_ref() == text
        {
            shaped.push(reused);
            continue;
        }

        let mut runs = line_runs(line, font, &live.colors, key.text_color);
        if let Some(range) = underline {
            underline_range(&mut runs, range);
        }
        let runs = crate::emoji_font::with_emoji_font(text, runs);
        match window.text_system().shape_text(
            SharedString::from(text.to_owned()),
            size,
            &runs,
            key.wrap_width,
            None,
        ) {
            Ok(mut lines) if lines.len() == 1 => shaped.push(lines.remove(0)),
            Ok(_) | Err(_) => {
                tracing::warn!("failed to shape a line of live markdown");
                return None;
            }
        }
    }

    let mut state = LayoutState::new(key.clone(), shaped, heights, this.style.line_height, false);
    state.display = Some(display);
    state.sigs = sigs;
    Some(state)
}

/// The style runs of one display line.
fn line_runs(
    line: &DisplayLine,
    base: &Font,
    colors: &LiveColors,
    text_color: Hsla,
) -> Vec<TextRun> {
    let heading = matches!(line.block, Block::Heading(_));
    let line_color = match line.block {
        Block::Quote | Block::Task { checked: true } => colors.muted,
        _ => text_color,
    };
    line.runs
        .iter()
        .map(|run| {
            let code = run.flags.code || line.block.is_code();
            let mut face = if code {
                let mut face = font(MONO_FAMILY);
                // Code is shown as typed: no joined arrows or comparison signs.
                face.features = FontFeatures::disable_ligatures();
                face
            } else {
                base.clone()
            };
            face.weight = if heading {
                FontWeight::BOLD
            } else if run.flags.bold {
                if code {
                    FontWeight::BOLD
                } else {
                    FontWeight::SEMIBOLD
                }
            } else {
                FontWeight::NORMAL
            };
            face.style = if run.flags.italic {
                FontStyle::Italic
            } else {
                FontStyle::Normal
            };
            let color = match run.kind {
                RunKind::Dim => colors.dim,
                RunKind::Glyph => colors.glyph,
                RunKind::Text if run.flags.link => colors.link,
                RunKind::Text => line_color,
            };
            TextRun {
                len: run.len,
                font: face,
                color,
                background_color: (run.flags.code
                    && !line.block.is_code()
                    && run.kind == RunKind::Text)
                    .then_some(colors.code_surface),
                underline: (run.flags.link && run.kind == RunKind::Text).then_some(
                    UnderlineStyle {
                        thickness: px(1.),
                        color: Some(color),
                        wavy: false,
                    },
                ),
                strikethrough: run.flags.strike.then_some(StrikethroughStyle {
                    thickness: px(1.),
                    color: Some(color),
                }),
            }
        })
        .collect()
}

/// Underlines `range` (bytes of the line), the input method's composition, splitting runs at its
/// edges.
fn underline_range(runs: &mut Vec<TextRun>, range: std::ops::Range<usize>) {
    let mut out = Vec::with_capacity(runs.len() + 2);
    let mut at = 0;
    for run in runs.drain(..) {
        let end = at + run.len;
        let pieces = [
            (at, range.start.clamp(at, end), false),
            (range.start.clamp(at, end), range.end.clamp(at, end), true),
            (range.end.clamp(at, end), end, false),
        ];
        for (from, to, marked) in pieces {
            if to > from {
                let mut piece = run.clone();
                piece.len = to - from;
                if marked {
                    piece.underline = Some(UnderlineStyle {
                        color: Some(piece.color),
                        thickness: px(1.),
                        wavy: false,
                    });
                }
                out.push(piece);
            }
        }
        at = end;
    }
    *runs = out;
}

/// Text runs covering the whole display text, underlining the IME preedit range.
fn runs_for(
    this: &TextInput,
    display_len: usize,
    font: &gpui::Font,
    showing_placeholder: bool,
) -> Vec<TextRun> {
    let color = if showing_placeholder {
        this.style.placeholder_color
    } else {
        this.style.text_color
    };
    let run = |len: usize, underline: Option<UnderlineStyle>| TextRun {
        len,
        font: font.clone(),
        color,
        background_color: None,
        underline,
        strikethrough: None,
    };
    let marked = if showing_placeholder {
        None
    } else {
        this.model.marked_range()
    };
    let Some(marked) = marked else {
        return vec![run(display_len, None)];
    };
    let start = this.display_offset(marked.start).min(display_len);
    let end = this.display_offset(marked.end).clamp(start, display_len);
    let underline = UnderlineStyle {
        color: Some(color),
        thickness: px(1.),
        wavy: false,
    };
    [
        run(start, None),
        run(end - start, Some(underline)),
        run(display_len - end, None),
    ]
    .into_iter()
    .filter(|run| run.len > 0)
    .collect()
}
