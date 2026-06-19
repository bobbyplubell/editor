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
pub const COLOR_CHART_BG: Color = Color::rgba(140, 170, 220, 28);

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

/// A detected ```` ```chart ```` fenced block — same shape as [`MermaidSpan`]
/// (full byte range + inner chart-block source range). The inner source is a
/// `hiker-charts` block body: a YAML config, optionally followed by a `---` line
/// and inline CSV. The renderer-facing output for the chart widget
/// (`widget-chart-render`); the `app` layer turns each span into a `BlockWidget`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChartSpan {
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

/// Scan the document (viewport-scoped) for ```` ```chart ```` fenced blocks,
/// reporting each fence's byte range and the inner chart-block source range so
/// the `app` layer can parse + render it to a `BlockWidget`. The inner range is
/// the whole block body (YAML config plus an optional `---` + inline CSV), which
/// the host hands to `hiker_charts_core::block::parse_block`. status: widget-chart-render
pub fn chart_spans(
    state: &EditorState,
    viewport: Option<&std::ops::Range<usize>>,
) -> Vec<ChartSpan> {
    fence_spans(state, viewport, "chart")
        .into_iter()
        .map(|(byte_range, inner_range)| ChartSpan { byte_range, inner_range })
        .collect()
}

/// Detect a SINGLE-LINE diagram fence in a standalone source STRING and return
/// the inner diagram source. A pipe-table cell is one logical line (a newline
/// ends the row), so a multi-line ```` ```lang … ``` ```` fenced block can't
/// live in a cell; the realistic one-line spelling is an inline fence —
/// `` ```lang <source>``` `` (triple-backtick, the `lang` info string, then the
/// diagram source up to the closing triple-backtick, all on one line). The host
/// re-runs this over a cell's source to find a nested diagram without building
/// an [`EditorState`] (mirrors [`crate::equations::math_spans_in_str`]).
///
/// Returns the inner source `&str` (trimmed of a single leading space after the
/// info string) only when the WHOLE trimmed input is exactly one such fence —
/// any prose around it yields `None`, so an ordinary text cell never matches.
/// `lang` is matched case-insensitively. status: widget-table-render
fn single_line_fence_body<'a>(text: &'a str, lang: &str) -> Option<&'a str> {
    let trimmed = text.trim();
    let rest = trimmed.strip_prefix("```")?;
    let inner_with_lang = rest.strip_suffix("```")?;
    // The info string is the run of non-space chars right after the opening
    // backticks; it must equal `lang` (case-insensitive), then a separator
    // (space or end) before the diagram source.
    let lang_len = inner_with_lang
        .find(|c: char| c.is_whitespace())
        .unwrap_or(inner_with_lang.len());
    if !inner_with_lang[..lang_len].eq_ignore_ascii_case(lang) {
        return None;
    }
    let body = inner_with_lang[lang_len..].trim();
    if body.is_empty() {
        return None;
    }
    Some(body)
}

/// The inner mermaid source of a single-line ```` ```mermaid … ``` ```` fence
/// filling the whole trimmed `text`, or `None`. The renderer-unaware primitive a
/// host re-runs over a table cell to detect an inline mermaid diagram
/// (`widget-table-render`); the `app` layer renders the returned source to a
/// texture. status: widget-table-render
pub fn mermaid_span_in_str(text: &str) -> Option<&str> {
    single_line_fence_body(text, "mermaid")
}

/// The inner WaveJSON source of a single-line ```` ```wavedrom … ``` ```` fence
/// filling the whole trimmed `text`, or `None` — the WaveDrom counterpart to
/// [`mermaid_span_in_str`]. status: widget-table-render
pub fn wavedrom_span_in_str(text: &str) -> Option<&str> {
    single_line_fence_body(text, "wavedrom")
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

/// Tinted-source fallback view for ```` ```chart ```` blocks, mirroring
/// [`mermaid_decorations`]. Shown when widget rendering is off
/// (`widget-render-toggle`) or the cursor is inside the span
/// (`widget-reveal-block`). status: widget-chart-render
pub fn chart_decorations(
    state: &EditorState,
    theme: Option<&Theme>,
    viewport: Option<&std::ops::Range<usize>>,
) -> DecorationSet {
    let bg = theme.map(|t| t.markdown.code_bg).unwrap_or(COLOR_CHART_BG);
    diagram_decorations(state, viewport, "chart", bg)
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

    #[test]
    fn detects_chart_fence_span() {
        let src = "intro\n\n```chart\nmark: bar\nx: a\ny: b\n---\na,b\n1,2\n```\n\nmore\n";
        let state = EditorState::new(src);
        let spans = chart_spans(&state, None);
        assert_eq!(spans.len(), 1, "one chart block");
        let span = &spans[0];
        assert!(src[span.byte_range.clone()].starts_with("```chart"));
        // The inner range is the whole block body (config + `---` + inline CSV).
        assert_eq!(&src[span.inner_range.clone()], "mark: bar\nx: a\ny: b\n---\na,b\n1,2\n");
    }

    #[test]
    fn chart_does_not_cross_detect_with_mermaid() {
        let src = "```chart\nmark: bar\n```\n\n```mermaid\ngraph TD; A-->B\n```\n";
        let state = EditorState::new(src);
        assert_eq!(chart_spans(&state, None).len(), 1, "only the chart fence");
        assert_eq!(mermaid_spans(&state, None).len(), 1, "only the mermaid fence");
    }

    #[test]
    fn chart_decorations_emitted() {
        let src = "```chart\nmark: bar\nx: a\ny: b\n```\n";
        let state = EditorState::new(src);
        let set = chart_decorations(&state, None, None);
        assert!(set.iter_all().count() > 0, "chart block tint emitted");
    }

    #[test]
    fn inline_mermaid_fence_detected_in_str() {
        // status: widget-table-render — a one-line ```mermaid …``` fence (the
        // only form a pipe-table cell can hold) yields its inner source.
        assert_eq!(mermaid_span_in_str("```mermaid graph TD; A-->B```"), Some("graph TD; A-->B"));
        // Surrounding whitespace is tolerated; the info string is case-insensitive.
        assert_eq!(mermaid_span_in_str("  ```Mermaid pie \"A\":1```  "), Some("pie \"A\":1"));
    }

    #[test]
    fn inline_wavedrom_fence_detected_in_str() {
        assert_eq!(
            wavedrom_span_in_str("```wavedrom { signal: [] }```"),
            Some("{ signal: [] }"),
        );
    }

    #[test]
    fn inline_fence_rejects_non_fence_cells() {
        // Plain prose, a bare info-only fence, a different language, and prose
        // around the fence all yield None (an ordinary text cell never matches).
        assert_eq!(mermaid_span_in_str("just some text"), None);
        assert_eq!(mermaid_span_in_str("```mermaid```"), None, "empty body");
        assert_eq!(mermaid_span_in_str("```rust fn main(){}```"), None, "wrong lang");
        assert_eq!(mermaid_span_in_str("see ```mermaid graph TD```"), None, "prose before");
        assert_eq!(wavedrom_span_in_str("```mermaid graph TD; A-->B```"), None, "lang mismatch");
    }
}
