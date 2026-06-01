//! Math detection + decorations.
//!
//! Detects inline math `$...$` and block math `$$...$$` (viewport-scoped). The
//! raw span scan ([`math_spans`]) is the renderer-facing API: it reports the
//! byte ranges + kind so the `app` layer can render each span to a widget
//! (`widget-render-providers`). This crate stays renderer-unaware — it never
//! depends on `hiker-render` or egui.
//!
//! [`math_decorations`] is the cursor-in / render-off fallback: it consumes the
//! same spans and emits the tinted source view —
//!
//! - Inline `$...$`: `Mark` with monospace font + light bg + violet fg.
//! - Block `$$...$$`: per-line `Line` decoration with a tinted bg covering
//!   every line in the span (including the delimiter lines).

use editor_core::decoration::Color;

use editor_core::decoration::Decoration;

use editor_core::decoration::Set as DecorationSet;
use editor_core::state::Editor as EditorState;
use editor_core::decoration::LineStyle;

use editor_core::decoration::MarkStyle;

use editor_core::rangeset::RangeSet;

use editor_core::theme::Theme;
pub const COLOR_MATH_FG: Color = Color::rgb(170, 120, 220);
pub const COLOR_MATH_BG: Color = Color::rgba(170, 120, 220, 25);

/// Which LaTeX style a detected math span renders in.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum MathKind {
    /// `$…$` — compact, baseline-aligned inline widget.
    Inline,
    /// `$$…$$` — full-size, centered block widget.
    Display,
}

/// A detected math span: the full source byte range (delimiters inclusive),
/// the kind, and the inner range (the LaTeX source between the delimiters).
///
/// This is the renderer-facing detection output (`widget-render-providers`):
/// the `app` layer turns each span into an `InlineWidget` / `BlockWidget`
/// decoration. `editor-md` reports spans only; it does not render.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MathSpan {
    pub byte_range: std::ops::Range<usize>,
    pub kind: MathKind,
    pub inner_range: std::ops::Range<usize>,
}

/// Scan the document (viewport-scoped) for inline `$…$` and block `$$…$$`
/// math spans. Block math is detected first so a `$$` opener isn't mistaken
/// for two adjacent inline delimiters. status: widget-render-providers
pub fn math_spans(
    state: &EditorState,
    viewport: Option<&std::ops::Range<usize>>,
) -> Vec<MathSpan> {
    let text = state.doc.to_string();
    let block = scan_block_spans(&text, viewport);
    let block_ranges: Vec<std::ops::Range<usize>> =
        block.iter().map(|s| s.byte_range.clone()).collect();
    let mut spans = block;
    spans.extend(scan_inline_spans(&text, viewport, &block_ranges));
    spans
}

/// Find `$$ … $$` spans (which may cross multiple lines). The closing `$$` is
/// searched across the whole document (not just the viewport) so a block that
/// opens on-screen but closes below the fold is still detected.
fn scan_block_spans(
    text: &str,
    viewport: Option<&std::ops::Range<usize>>,
) -> Vec<MathSpan> {
    let bytes = text.as_bytes();
    let (scan_start, scan_end) = scan_bounds(bytes.len(), viewport);
    let mut spans = Vec::new();
    let mut i = scan_start;
    while i + 1 < scan_end {
        if bytes[i] == b'$' && bytes[i + 1] == b'$' {
            let mut j = i + 2;
            let mut closed = None;
            while j + 1 < bytes.len() {
                if bytes[j] == b'$' && bytes[j + 1] == b'$' {
                    closed = Some(j);
                    break;
                }
                j += 1;
            }
            let Some(close_start) = closed else { break };
            let full_end = close_start + 2;
            spans.push(MathSpan {
                byte_range: i..full_end,
                kind: MathKind::Display,
                inner_range: i + 2..close_start,
            });
            i = full_end;
            continue;
        }
        i += 1;
    }
    spans
}

/// Find single-line `$ … $` spans, skipping any byte inside a block span.
fn scan_inline_spans(
    text: &str,
    viewport: Option<&std::ops::Range<usize>>,
    block_ranges: &[std::ops::Range<usize>],
) -> Vec<MathSpan> {
    let bytes = text.as_bytes();
    let in_block = |off: usize| -> bool {
        block_ranges.iter().any(|r| off >= r.start && off < r.end)
    };
    let (scan_start, scan_end) = scan_bounds(bytes.len(), viewport);
    let mut spans = Vec::new();
    let mut i = scan_start;
    while i < scan_end {
        if bytes[i] == b'$' && !in_block(i) {
            // Skip `$$` (a block opener).
            if i + 1 < bytes.len() && bytes[i + 1] == b'$' {
                i += 2;
                continue;
            }
            let mut j = i + 1;
            let mut closed = None;
            while j < bytes.len() {
                if bytes[j] == b'\n' {
                    break;
                }
                if bytes[j] == b'$' {
                    closed = Some(j);
                    break;
                }
                j += 1;
            }
            let Some(close) = closed else {
                i += 1;
                continue;
            };
            if close == i + 1 {
                i = close + 1;
                continue;
            }
            let full_end = close + 1;
            spans.push(MathSpan {
                byte_range: i..full_end,
                kind: MathKind::Inline,
                inner_range: i + 1..close,
            });
            i = full_end;
            continue;
        }
        i += 1;
    }
    spans
}

fn scan_bounds(len: usize, viewport: Option<&std::ops::Range<usize>>) -> (usize, usize) {
    match viewport {
        Some(vp) => (vp.start.min(len), vp.end.min(len)),
        None => (0, len),
    }
}

/// Tinted-source fallback view: violet-on-tint marks for inline math, a
/// per-line background tint for block math. Used when rendering is off
/// (`widget-render-toggle`) or the cursor is inside the span
/// (`widget-reveal-*`).
pub fn math_decorations(
    state: &EditorState,
    theme: Option<&Theme>,
    viewport: Option<&std::ops::Range<usize>>,
) -> DecorationSet {
    // Math has no dedicated theme slot; use code_bg if a theme is provided.
    let fg = theme.map(|t| t.palette.accent).unwrap_or(COLOR_MATH_FG);
    let bg = theme.map(|t| t.markdown.code_bg).unwrap_or(COLOR_MATH_BG);

    let total_lines = state.doc.len_lines();
    let doc_len = state.doc.len_bytes();
    let line_byte_end = |line: usize| -> usize {
        if line + 1 < total_lines {
            state.doc.line_to_byte(line + 1)
        } else {
            doc_len
        }
    };

    let mut entries: Vec<(std::ops::Range<usize>, Decoration)> = Vec::new();
    for span in math_spans(state, viewport) {
        match span.kind {
            MathKind::Display => {
                let start_line = state.doc.byte_to_line(span.byte_range.start);
                let end_line =
                    state.doc.byte_to_line(span.byte_range.end.saturating_sub(1));
                for l in start_line..=end_line {
                    if l >= total_lines {
                        break;
                    }
                    let s = state.doc.line_to_byte(l);
                    let e = line_byte_end(l);
                    entries.push((
                        s..e,
                        Decoration::Line(LineStyle {
                            bg: Some(bg),
                            ..LineStyle::default()
                        }),
                    ));
                }
            }
            MathKind::Inline => {
                entries.push((
                    span.byte_range.clone(),
                    Decoration::Mark(MarkStyle {
                        fg: Some(fg),
                        bg: Some(bg),
                        monospace: true,
                        ..MarkStyle::default()
                    }),
                ));
            }
        }
    }
    RangeSet::from_iter(entries)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_inline_and_block_spans() {
        let src = "before $x^2$ after\n\n$$\n\\int_0^1 x\\,dx\n$$\n";
        let state = EditorState::new(src);
        let spans = math_spans(&state, None);
        // One inline, one display.
        let inline: Vec<_> = spans.iter().filter(|s| s.kind == MathKind::Inline).collect();
        let display: Vec<_> = spans.iter().filter(|s| s.kind == MathKind::Display).collect();
        assert_eq!(inline.len(), 1, "one inline span");
        assert_eq!(display.len(), 1, "one display span");
        // Inner range of the inline span is the LaTeX source `x^2`.
        let inl = inline[0];
        assert_eq!(&src[inl.inner_range.clone()], "x^2");
        // Inner range of the display span is the multi-line body.
        let disp = display[0];
        assert_eq!(&src[disp.inner_range.clone()], "\n\\int_0^1 x\\,dx\n");
    }

    #[test]
    fn block_opener_not_split_into_two_inline() {
        let src = "$$a$$";
        let state = EditorState::new(src);
        let spans = math_spans(&state, None);
        assert_eq!(spans.len(), 1);
        assert_eq!(spans[0].kind, MathKind::Display);
    }

    #[test]
    fn empty_inline_is_not_a_span() {
        let src = "a $$ b"; // `$$` with no close → no span
        let state = EditorState::new(src);
        let spans = math_spans(&state, None);
        assert!(spans.is_empty());
    }

    #[test]
    fn decorations_still_emitted() {
        let src = "x $y$ z";
        let state = EditorState::new(src);
        let set = math_decorations(&state, None, None);
        assert!(set.iter_all().count() > 0, "inline math marks emitted");
    }
}
