//! Diagram code-block detection + decorations (Mermaid, WaveDrom).
//!
//! Detects fenced code blocks tagged ```` ```mermaid ```` and ```` ```wavedrom
//! ````. The raw span scans ([`mermaid_spans`] / [`wavedrom_spans`]) are the
//! renderer-facing API: each reports the fence's byte range + the inner diagram
//! source so the `app` layer can render the block to a `BlockWidget`
//! (`widget-mermaid-render`, `widget-wavedrom-render`). This crate stays
//! renderer-unaware — it never depends on `hiker-render` or egui. Both diagram
//! kinds share one language-parameterized fence scan ([`scan_fences`]).
//!
//! [`mermaid_decorations`] / [`wavedrom_decorations`] are the cursor-in /
//! render-off fallback: they consume the same fence scan and apply a tinted
//! per-line background across the entire block (including the fence lines), so
//! users see where their diagram blocks live when the rendered widget is
//! suppressed.

use editor_core::decoration::Color;

use editor_core::decoration::Decoration;

use editor_core::decoration::Set as DecorationSet;
use editor_core::state::Editor as EditorState;
use editor_core::decoration::LineStyle;

use editor_core::rangeset::RangeSet;

use editor_core::theme::Theme;
pub const COLOR_MERMAID_BG: Color = Color::rgba(120, 200, 180, 28);
pub const COLOR_WAVEDROM_BG: Color = Color::rgba(200, 170, 120, 28);

/// A detected ```` ```mermaid ```` fenced block: the full source byte range
/// (fence lines inclusive) and the inner range (the diagram source between the
/// open and close fences).
///
/// This is the renderer-facing detection output (`widget-mermaid-render`): the
/// `app` layer turns each span into a `BlockWidget` decoration. `editor-md`
/// reports spans only; it does not render.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MermaidSpan {
    pub byte_range: std::ops::Range<usize>,
    pub inner_range: std::ops::Range<usize>,
}

/// A detected ```` ```wavedrom ```` fenced block — same shape as [`MermaidSpan`]
/// (full byte range + inner WaveJSON source range). The renderer-facing output
/// for the WaveDrom widget (`widget-wavedrom-render`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WaveDromSpan {
    pub byte_range: std::ops::Range<usize>,
    pub inner_range: std::ops::Range<usize>,
}

/// One scanned diagram fence: the open/close *line* indices (`close_line` is the
/// matching close fence, or the last document line when the fence is
/// unterminated). The `*_spans` and `*_decorations` builders all share this
/// single language-parameterized line-level scan so fence-parsing lives in one
/// place.
struct Fence {
    open_line: usize,
    close_line: usize,
}

/// Whether `line_text` opens a ```` ```<lang> ```` fence for `lang`
/// (case-insensitive, info string matched exactly after the triple-backtick).
fn is_lang_fence_open(line_text: &str, lang: &str) -> bool {
    let trimmed = line_text.trim_start();
    let Some(rest) = trimmed.strip_prefix("```") else {
        return false;
    };
    rest.trim().eq_ignore_ascii_case(lang)
}

/// Whether `line_text` is a bare closing fence (``` ``` ``` with no info string).
fn is_fence_close(line_text: &str) -> bool {
    let trimmed = line_text.trim_start();
    let Some(rest) = trimmed.strip_prefix("```") else {
        return false;
    };
    rest.trim().is_empty()
}

/// Scan `state` (viewport-scoped) for ```` ```<lang> ```` fenced blocks,
/// returning each fence's open/close line indices. The close fence is searched
/// across the whole document (not just the viewport) so a block that opens
/// on-screen but closes below the fold is still detected.
fn scan_fences(
    state: &EditorState,
    viewport: Option<&std::ops::Range<usize>>,
    lang: &str,
) -> Vec<Fence> {
    let text = state.doc.to_string();
    let total_lines = state.doc.len_lines();
    let doc_len = state.doc.len_bytes();
    let line_byte_end = |line: usize| -> usize {
        if line + 1 < total_lines {
            state.doc.line_to_byte(line + 1)
        } else {
            doc_len
        }
    };
    let line_str = |line: usize| -> &str {
        let s = state.doc.line_to_byte(line);
        let e = line_byte_end(line);
        let raw = &text[s..e];
        raw.strip_suffix('\n').unwrap_or(raw)
    };

    let line_range = match viewport {
        Some(vp) => editor_view::viewport_lines(&state.doc, vp),
        None => 0..total_lines,
    };
    let scan_end_line = line_range.end;
    let mut fences = Vec::new();
    let mut line = line_range.start;
    while line < scan_end_line {
        if is_lang_fence_open(line_str(line), lang) {
            let open_line = line;
            let mut close_line = open_line;
            let mut found_close = false;
            let mut probe = open_line + 1;
            while probe < total_lines {
                if is_fence_close(line_str(probe)) {
                    close_line = probe;
                    found_close = true;
                    break;
                }
                probe += 1;
            }
            if !found_close {
                close_line = total_lines.saturating_sub(1);
            }
            fences.push(Fence { open_line, close_line });
            line = close_line + 1;
            continue;
        }
        line += 1;
    }
    fences
}

/// Scan the document (viewport-scoped) for ```` ```<lang> ```` fenced blocks,
/// reporting each as `(byte_range, inner_range)`: the full fence byte range and
/// the inner diagram source range (lines between the open and close fences).
/// Shared by [`mermaid_spans`] / [`wavedrom_spans`].
fn fence_spans(
    state: &EditorState,
    viewport: Option<&std::ops::Range<usize>>,
    lang: &str,
) -> Vec<(std::ops::Range<usize>, std::ops::Range<usize>)> {
    let total_lines = state.doc.len_lines();
    let doc_len = state.doc.len_bytes();
    let line_byte_end = |line: usize| -> usize {
        if line + 1 < total_lines {
            state.doc.line_to_byte(line + 1)
        } else {
            doc_len
        }
    };
    scan_fences(state, viewport, lang)
        .into_iter()
        .map(|f| {
            let block_start = state.doc.line_to_byte(f.open_line);
            let block_end = line_byte_end(f.close_line);
            // Inner source spans the lines between the open and close fences.
            // When the fence is unterminated (`close_line == open_line`) or the
            // block is empty, the inner range collapses to an empty slice.
            let inner_start = line_byte_end(f.open_line).min(block_end);
            let inner_end = if f.close_line > f.open_line {
                state.doc.line_to_byte(f.close_line)
            } else {
                inner_start
            };
            (block_start..block_end, inner_start..inner_end.max(inner_start))
        })
        .collect()
}

/// Scan the document (viewport-scoped) for ```` ```mermaid ```` fenced blocks,
/// reporting each fence's byte range and the inner diagram source range so the
/// `app` layer can render it to a `BlockWidget`. status: widget-mermaid-render
pub fn mermaid_spans(
    state: &EditorState,
    viewport: Option<&std::ops::Range<usize>>,
) -> Vec<MermaidSpan> {
    fence_spans(state, viewport, "mermaid")
        .into_iter()
        .map(|(byte_range, inner_range)| MermaidSpan { byte_range, inner_range })
        .collect()
}

/// Scan the document (viewport-scoped) for ```` ```wavedrom ```` fenced blocks,
/// reporting each fence's byte range and the inner WaveJSON source range so the
/// `app` layer can render it to a `BlockWidget`. status: widget-wavedrom-render
pub fn wavedrom_spans(
    state: &EditorState,
    viewport: Option<&std::ops::Range<usize>>,
) -> Vec<WaveDromSpan> {
    fence_spans(state, viewport, "wavedrom")
        .into_iter()
        .map(|(byte_range, inner_range)| WaveDromSpan { byte_range, inner_range })
        .collect()
}

/// Tinted-source fallback view for a diagram `lang`: a per-line background tint
/// across the whole fenced block (fence lines inclusive). Shared by
/// [`mermaid_decorations`] / [`wavedrom_decorations`].
fn diagram_decorations(
    state: &EditorState,
    viewport: Option<&std::ops::Range<usize>>,
    lang: &str,
    bg: Color,
) -> DecorationSet {
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
    for fence in scan_fences(state, viewport, lang) {
        for l in fence.open_line..=fence.close_line {
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

    RangeSet::from_iter(entries)
}

/// Tinted-source fallback view: a per-line background tint across the whole
/// ```` ```mermaid ```` block (fence lines inclusive). Used when rendering is
/// off (`widget-render-toggle`) or the cursor is inside the span
/// (`widget-reveal-block`).
pub fn mermaid_decorations(
    state: &EditorState,
    theme: Option<&Theme>,
    viewport: Option<&std::ops::Range<usize>>,
) -> DecorationSet {
    let bg = theme.map(|t| t.markdown.code_bg).unwrap_or(COLOR_MERMAID_BG);
    diagram_decorations(state, viewport, "mermaid", bg)
}

/// Tinted-source fallback view for ```` ```wavedrom ```` blocks, mirroring
/// [`mermaid_decorations`]. status: widget-wavedrom-render
pub fn wavedrom_decorations(
    state: &EditorState,
    theme: Option<&Theme>,
    viewport: Option<&std::ops::Range<usize>>,
) -> DecorationSet {
    let bg = theme.map(|t| t.markdown.code_bg).unwrap_or(COLOR_WAVEDROM_BG);
    diagram_decorations(state, viewport, "wavedrom", bg)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_mermaid_fence_span() {
        let src = "intro\n\n```mermaid\ngraph TD; A-->B\n```\n\nmore\n";
        let state = EditorState::new(src);
        let spans = mermaid_spans(&state, None);
        assert_eq!(spans.len(), 1, "one mermaid block");
        let span = &spans[0];
        // The full span covers the fences; the inner range is the diagram body.
        assert!(src[span.byte_range.clone()].starts_with("```mermaid"));
        assert_eq!(&src[span.inner_range.clone()], "graph TD; A-->B\n");
    }

    #[test]
    fn unterminated_fence_still_reports_a_span() {
        let src = "```mermaid\ngraph TD; A-->B\n";
        let state = EditorState::new(src);
        let spans = mermaid_spans(&state, None);
        assert_eq!(spans.len(), 1, "an unterminated fence still reports a span");
    }

    #[test]
    fn decorations_still_emitted() {
        let src = "```mermaid\ngraph TD; A-->B\n```\n";
        let state = EditorState::new(src);
        let set = mermaid_decorations(&state, None, None);
        assert!(set.iter_all().count() > 0, "mermaid block tint emitted");
    }

    #[test]
    fn non_mermaid_fence_ignored() {
        let src = "```rust\nfn main() {}\n```\n";
        let state = EditorState::new(src);
        assert!(mermaid_spans(&state, None).is_empty(), "rust fence is not mermaid");
    }

    #[test]
    fn detects_wavedrom_fence_span() {
        let src = "intro\n\n```wavedrom\n{ signal: [{ name: 'clk', wave: 'p..' }] }\n```\n\nmore\n";
        let state = EditorState::new(src);
        let spans = wavedrom_spans(&state, None);
        assert_eq!(spans.len(), 1, "one wavedrom block");
        let span = &spans[0];
        assert!(src[span.byte_range.clone()].starts_with("```wavedrom"));
        assert_eq!(
            &src[span.inner_range.clone()],
            "{ signal: [{ name: 'clk', wave: 'p..' }] }\n"
        );
    }

    #[test]
    fn wavedrom_and_mermaid_dont_cross_detect() {
        let src = "```mermaid\ngraph TD; A-->B\n```\n\n```wavedrom\n{ signal: [] }\n```\n";
        let state = EditorState::new(src);
        assert_eq!(mermaid_spans(&state, None).len(), 1, "only the mermaid fence");
        assert_eq!(wavedrom_spans(&state, None).len(), 1, "only the wavedrom fence");
    }

    #[test]
    fn unterminated_wavedrom_fence_still_reports_a_span() {
        let src = "```wavedrom\n{ signal: [] }\n";
        let state = EditorState::new(src);
        assert_eq!(wavedrom_spans(&state, None).len(), 1, "unterminated still reports");
    }

    #[test]
    fn wavedrom_decorations_emitted() {
        let src = "```wavedrom\n{ signal: [] }\n```\n";
        let state = EditorState::new(src);
        let set = wavedrom_decorations(&state, None, None);
        assert!(set.iter_all().count() > 0, "wavedrom block tint emitted");
    }
}
