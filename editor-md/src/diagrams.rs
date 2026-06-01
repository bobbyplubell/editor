//! Mermaid code-block detection + decorations.
//!
//! Detects fenced code blocks tagged ```` ```mermaid ````. The raw span scan
//! ([`mermaid_spans`]) is the renderer-facing API: it reports the fence's byte
//! range + the inner diagram source so the `app` layer can render each block to
//! a `BlockWidget` (`widget-mermaid-render`). This crate stays renderer-unaware
//! — it never depends on `hiker-render` or egui.
//!
//! [`mermaid_decorations`] is the cursor-in / render-off fallback: it consumes
//! the same fence scan and applies a tinted per-line background across the
//! entire block (including the fence lines), so users see where their mermaid
//! blocks live when the rendered widget is suppressed.

use editor_core::decoration::Color;

use editor_core::decoration::Decoration;

use editor_core::decoration::Set as DecorationSet;
use editor_core::state::Editor as EditorState;
use editor_core::decoration::LineStyle;

use editor_core::rangeset::RangeSet;

use editor_core::theme::Theme;
pub const COLOR_MERMAID_BG: Color = Color::rgba(120, 200, 180, 28);

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

/// One scanned ```` ```mermaid ```` fence: the open/close *line* indices
/// (`close_line` is the matching close fence, or the last document line when
/// the fence is unterminated). Both `mermaid_spans` and `mermaid_decorations`
/// build from this single line-level scan so fence-parsing lives in one place.
struct MermaidFence {
    open_line: usize,
    close_line: usize,
}

/// Whether `line_text` opens a ```` ```mermaid ```` fence.
fn is_mermaid_fence_open(line_text: &str) -> bool {
    let trimmed = line_text.trim_start();
    let Some(rest) = trimmed.strip_prefix("```") else {
        return false;
    };
    rest.trim().eq_ignore_ascii_case("mermaid")
}

/// Whether `line_text` is a bare closing fence (``` ``` ``` with no info string).
fn is_fence_close(line_text: &str) -> bool {
    let trimmed = line_text.trim_start();
    let Some(rest) = trimmed.strip_prefix("```") else {
        return false;
    };
    rest.trim().is_empty()
}

/// Scan `state` (viewport-scoped) for ```` ```mermaid ```` fenced blocks,
/// returning each fence's open/close line indices. The close fence is searched
/// across the whole document (not just the viewport) so a block that opens
/// on-screen but closes below the fold is still detected.
fn scan_fences(
    state: &EditorState,
    viewport: Option<&std::ops::Range<usize>>,
) -> Vec<MermaidFence> {
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
        if is_mermaid_fence_open(line_str(line)) {
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
            fences.push(MermaidFence { open_line, close_line });
            line = close_line + 1;
            continue;
        }
        line += 1;
    }
    fences
}

/// Scan the document (viewport-scoped) for ```` ```mermaid ```` fenced blocks,
/// reporting each fence's byte range and the inner diagram source range so the
/// `app` layer can render it to a `BlockWidget`. status: widget-mermaid-render
pub fn mermaid_spans(
    state: &EditorState,
    viewport: Option<&std::ops::Range<usize>>,
) -> Vec<MermaidSpan> {
    let total_lines = state.doc.len_lines();
    let doc_len = state.doc.len_bytes();
    let line_byte_end = |line: usize| -> usize {
        if line + 1 < total_lines {
            state.doc.line_to_byte(line + 1)
        } else {
            doc_len
        }
    };
    scan_fences(state, viewport)
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
            MermaidSpan {
                byte_range: block_start..block_end,
                inner_range: inner_start..inner_end.max(inner_start),
            }
        })
        .collect()
}

/// Tinted-source fallback view: a per-line background tint across the whole
/// fenced block (fence lines inclusive). Used when rendering is off
/// (`widget-render-toggle`) or the cursor is inside the span
/// (`widget-reveal-block`).
pub fn mermaid_decorations(
    state: &EditorState,
    theme: Option<&Theme>,
    viewport: Option<&std::ops::Range<usize>>,
) -> DecorationSet {
    let bg = theme.map(|t| t.markdown.code_bg).unwrap_or(COLOR_MERMAID_BG);
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
    for fence in scan_fences(state, viewport) {
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
}
