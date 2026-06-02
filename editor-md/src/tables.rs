//! GFM pipe-table detection.
//!
//! Detects pipe tables (`| a | b |` with a `|---|---|` rule row) by walking the
//! same pulldown-cmark parse the mark-only providers use (GFM tables already
//! parse via [`Options::ENABLE_TABLES`]). The raw span scan ([`table_spans`]) is
//! the renderer-facing API: it reports each table block's byte range plus the
//! per-column alignments (cheap to surface from the parse) so the `app` layer
//! can render each table to a natively-painted `BlockWidget`
//! (`widget-table-render`). Full cell parsing lives app-side.
//!
//! This crate stays renderer-unaware — it never depends on `hiker-render` or
//! egui, mirroring [`crate::diagrams::mermaid_spans`] /
//! [`crate::equations::math_spans`].

use editor_core::state::Editor as EditorState;
use pulldown_cmark::{Alignment, Event, Options, Parser, Tag, TagEnd};

/// Per-column horizontal alignment of a detected pipe table, taken straight
/// from the GFM delimiter row (`:--`, `:-:`, `--:`). `None` means the column
/// had no explicit alignment marker (renderer's default, left).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ColumnAlign {
    None,
    Left,
    Center,
    Right,
}

impl From<Alignment> for ColumnAlign {
    fn from(a: Alignment) -> Self {
        match a {
            Alignment::None => ColumnAlign::None,
            Alignment::Left => ColumnAlign::Left,
            Alignment::Center => ColumnAlign::Center,
            Alignment::Right => ColumnAlign::Right,
        }
    }
}

/// A detected pipe-table block: the full source byte range (header, rule, and
/// body rows inclusive) and the per-column alignments.
///
/// This is the renderer-facing detection output (`widget-table-render`): the
/// `app` layer turns each span into a natively-painted `BlockWidget`. The byte
/// range slices out the raw pipe-and-dash source the app provider parses into
/// rows / cells; `aligns` carries the column count and alignment so the app
/// need not re-parse the delimiter row. A malformed table (no rule row) is not
/// a GFM table at all and is never reported — the source stays as tinted
/// markdown (`widget-render-error-fallback`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TableSpan {
    pub byte_range: std::ops::Range<usize>,
    pub aligns: Vec<ColumnAlign>,
}

/// Scan the document (viewport-scoped) for GFM pipe-table blocks, reporting each
/// table's byte range + column alignments so the `app` layer can render it to a
/// `BlockWidget`. Tables that overlap the viewport are reported in full (a table
/// straddling the visible band still renders). status: widget-table-render
pub fn table_spans(
    state: &EditorState,
    viewport: Option<&std::ops::Range<usize>>,
) -> Vec<TableSpan> {
    let text = state.doc.to_string();
    let mut opts = Options::empty();
    opts.insert(Options::ENABLE_STRIKETHROUGH);
    opts.insert(Options::ENABLE_TABLES);
    opts.insert(Options::ENABLE_TASKLISTS);
    opts.insert(Options::ENABLE_GFM);
    let parser = Parser::new_ext(&text, opts).into_offset_iter();

    let mut spans = Vec::new();
    let mut pending: Option<Vec<ColumnAlign>> = None;
    for (event, byte_range) in parser {
        match event {
            Event::Start(Tag::Table(aligns)) => {
                pending = Some(aligns.into_iter().map(ColumnAlign::from).collect());
            }
            Event::End(TagEnd::Table) => {
                if let Some(aligns) = pending.take() {
                    let overlaps = viewport
                        .map(|vp| byte_range.start < vp.end && byte_range.end > vp.start)
                        .unwrap_or(true);
                    if overlaps {
                        spans.push(TableSpan { byte_range, aligns });
                    }
                }
            }
            _ => {}
        }
    }
    spans
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_simple_table() {
        let src = "intro\n\n| a | b |\n|---|---|\n| 1 | 2 |\n\nmore\n";
        let state = EditorState::new(src);
        let spans = table_spans(&state, None);
        assert_eq!(spans.len(), 1, "one table block");
        let span = &spans[0];
        let block = &src[span.byte_range.clone()];
        assert!(block.starts_with("| a | b |"), "span covers the header row");
        assert!(block.contains("| 1 | 2 |"), "span covers the body row");
        assert_eq!(span.aligns.len(), 2, "two columns");
    }

    #[test]
    fn surfaces_column_alignments() {
        let src = "| l | c | r |\n|:--|:-:|--:|\n| 1 | 2 | 3 |\n";
        let state = EditorState::new(src);
        let spans = table_spans(&state, None);
        assert_eq!(spans.len(), 1);
        assert_eq!(
            spans[0].aligns,
            vec![ColumnAlign::Left, ColumnAlign::Center, ColumnAlign::Right]
        );
    }

    #[test]
    fn malformed_table_without_rule_row_ignored() {
        // No `|---|` delimiter row → not a GFM table → no span (the source
        // stays as tinted markdown, `widget-render-error-fallback`).
        let src = "| a | b |\n| 1 | 2 |\n";
        let state = EditorState::new(src);
        assert!(
            table_spans(&state, None).is_empty(),
            "a table missing its rule row is not detected"
        );
    }

    #[test]
    fn detects_multiple_tables() {
        let src = "| a |\n|---|\n| 1 |\n\ntext\n\n| b |\n|---|\n| 2 |\n";
        let state = EditorState::new(src);
        assert_eq!(table_spans(&state, None).len(), 2, "two separate tables");
    }

    #[test]
    fn viewport_scopes_detection() {
        // A table well below a large viewport-excluded region is skipped when
        // the viewport doesn't reach it.
        let src = "| a |\n|---|\n| 1 |\n";
        let state = EditorState::new(src);
        let past_end = src.len()..src.len();
        assert!(
            table_spans(&state, Some(&past_end)).is_empty(),
            "a table outside the viewport is not reported"
        );
        let whole = 0..src.len();
        assert_eq!(table_spans(&state, Some(&whole)).len(), 1);
    }
}
