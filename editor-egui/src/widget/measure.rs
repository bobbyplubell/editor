//! The widget's geometry / measure pass: the metrics fingerprint that gates the
//! measure cache, the soft-wrap pre-pass that keeps the wrap map current for the
//! visible band, the per-line height-decoration derivation (headings, hides,
//! block/inline widgets, soft-wrap multiplier), and the visible-wrap-mismatch
//! probe that catches a scroll-in wrap flip. Split out of `widget.rs` as a
//! continuation of the `Widget` impl so that file stays within its per-file line
//! budget; every item here is a method or associated fn on
//! [`super::Widget`].

use editor_core::decoration::{BlockDeco, BlockSide, Decoration, LineStyle};
use editor_view::viewport::ViewState;
use editor_view::wrapping::VisualSpan;

use super::Widget;

impl Widget<'_> {
    /// Stable fingerprint over the metrics that, if changed, invalidate the
    /// heightmap or wrap cache (size, font, gutter, wrap settings).
    pub(super) fn compute_metrics_fingerprint(view: &ViewState) -> u64 {
        let bits = [
            view.width.to_bits() as u64,
            view.height.to_bits() as u64,
            view.font_size.to_bits() as u64,
            view.line_height.to_bits() as u64,
            view.gutter_width.to_bits() as u64,
            view.wrap_map.width().to_bits() as u64,
            view.wrap_map.enabled() as u64,
        ];
        let mut acc: u64 = 0xA076_1D64_78BD_642F;
        for b in bits {
            acc ^= b.wrapping_mul(0x9E37_79B9_7F4A_7C15);
            acc = acc.rotate_left(27);
        }
        acc
    }

    /// Whether any currently-visible line's live soft-wrap row count differs
    /// from the count the height map reserved for it at the last derivation
    /// (recorded via [`HeightMap::set_wrap_count`]). This catches a line whose
    /// wrap flipped when a viewport-scoped decoration began covering it on
    /// scroll-in — the one geometry input that changes without touching the
    /// doc / metrics / height-decoration signatures. Comparing against the
    /// baked count (rather than last frame's visible set) means a line newly
    /// entering the viewport is checked too, not just lines already on screen.
    /// O(visible lines); only runs on a frame that already passed the
    /// measure-cache gate.
    pub(super) fn visible_wrap_mismatch(&self) -> bool {
        if !self.view.wrap_map.enabled() {
            return false;
        }
        self.view.visible_lines().any(|line| {
            let live = self.view.wrap_map.peek(line).map_or(1, |w| w.visual_count());
            live != self.view.height_map.wrap_count(line)
        })
    }

    pub(super) fn apply_line_height_decorations(&mut self) {
        let view = &mut *self.view;
        let state = &*self.state;
        let base = view.line_height;
        let total_lines = state.doc.len_lines();
        // O(K) reset over existing overrides rather than O(N) over every
        // line in the doc. apply runs every scroll frame (viewport change
        // invalidates measure cache), so the loop body cost matters a lot
        // on long files.
        view.height_map.reset_text_heights();
        view.height_map.clear_blocks();
        let has_height_layers = !view.decorations.height_indices.is_empty();
        if !has_height_layers && !view.wrap_map.enabled() {
            // Fast path: nothing to apply; prefix needs to reflect the base-height
            // reset above.
            view.height_map.recompute();
            return;
        }
        let doc_len = state.doc.len_bytes();
        // Per-line tallest inline widget (logical points), accumulated during the
        // scan below and applied as a row-height floor afterwards. An inline math
        // formula (a fraction, a `\sum` with limits) measures taller than the text
        // line; its visual row must grow to that height so the blit paints at full
        // resolution rather than being clipped to the text line (slug
        // `widget-inline-math-baseline`). Kept separate from the main pass because
        // `set_line_height` is absolute — we want a `max`, applied once per line
        // after headings/hides have set the text height.
        let mut inline_widget_heights: std::collections::BTreeMap<usize, f32> =
            std::collections::BTreeMap::new();
        // Only scan layers flagged as height-affecting. The painter still walks
        // every layer separately for marks/replace/widgets.
        for layer in view.decorations.height_layers() {
            for (range, deco) in layer.iter_overlapping(0..doc_len + 1) {
                match deco {
                    Decoration::Line(LineStyle { hide: true, .. }) => {
                        let line = state.doc.byte_to_line(range.start.min(doc_len));
                        view.height_map.set_line_height(line, 0.0);
                    }
                    Decoration::Line(LineStyle { height_scale: Some(scale), .. }) => {
                        let line = state.doc.byte_to_line(range.start);
                        view.height_map.set_line_height(line, base * scale);
                    }
                    Decoration::Block(BlockDeco { side, height, .. }) => {
                        let line = state.doc.byte_to_line(range.start.min(doc_len));
                        match side {
                            BlockSide::Above => view.height_map.add_block_above(line, *height),
                            BlockSide::Below => view.height_map.add_block_below(line, *height),
                        }
                    }
                    Decoration::BlockWidget { side, widget } => {
                        let line = state.doc.byte_to_line(range.start.min(doc_len));
                        // Pass the CONTENT width (text column, gutter excluded) —
                        // the same box `paint_block_widget_placeholder` letterboxes
                        // into — so a widget that scales to fit width reserves a
                        // matching height (no excess vertical band).
                        let content_w = (view.width - view.content_origin_x()).max(0.0);
                        let h = widget.measure(view.font_size, content_w);
                        match side {
                            BlockSide::Above => view.height_map.add_block_above(line, h),
                            BlockSide::Below => view.height_map.add_block_below(line, h),
                        }
                    }
                    Decoration::InlineWidget { widget, .. } => {
                        let line = state.doc.byte_to_line(range.start.min(doc_len));
                        let h = widget.measure(view.font_size).1;
                        let slot = inline_widget_heights.entry(line).or_insert(0.0);
                        if h > *slot {
                            *slot = h;
                        }
                    }
                    _ => {}
                }
            }
        }
        // Grow each line's text row to fit its tallest inline widget. Only lines
        // that are visible (not hidden, text_height > 0) participate — a hidden
        // line stays collapsed. Applied before the soft-wrap multiplier so a grown
        // base row is multiplied per visual row consistently with headings.
        for (&line, &widget_h) in &inline_widget_heights {
            let text_h = view.height_map.text_height(line);
            if text_h > 0.0 && widget_h > text_h {
                view.height_map.set_line_height(line, widget_h);
            }
        }
        // Apply soft-wrap multiplier: a line with N visual rows is N× taller
        // (unless hidden, height==0).
        if view.wrap_map.enabled() {
            for line in 0..total_lines {
                if let Some(w) = view.wrap_map.peek(line) {
                    let vc = w.visual_count();
                    if vc > 1 {
                        let h = view.height_map.text_height(line);
                        if h > 0.0 {
                            view.height_map.set_line_height(line, h * vc as f32);
                            // Record the row count this allocation was built for, so
                            // measure can detect a later viewport-scoped wrap change
                            // on this line (scroll-in reveal) and re-derive.
                            view.height_map.set_wrap_count(line, vc);
                        }
                    }
                }
            }
        }
        view.height_map.recompute();
    }

    pub(super) fn prewrap_visible(&mut self) {
        let view = &mut *self.view;
        let state = &*self.state;
        if !view.wrap_map.enabled() {
            return;
        }
        let total = state.doc.len_lines();
        // Decide the line range to rescan. On a pure scroll (full-doc geometry
        // inputs unchanged), only lines whose viewport-scoped decoration coverage
        // could have shifted need work — the union of last frame's and this
        // frame's visible band. Lines outside that union were covered by the same
        // layers in both frames, so their cached wraps remain valid. On any
        // geometry change (doc edit, full-doc layer churn, width/char/enabled
        // change, line-count change) we fall back to walking the whole document.
        //
        // The "always walk all lines" approach we kept here for a while masked an
        // off-screen-stale bug after edits by paying O(N) per scroll frame; the
        // partition between geometry-affecting and viewport-scoped layers
        // (`DecorationLayers::geometry_epoch` vs `signature`) gives us the same
        // correctness without the per-line cost.
        //
        // Per-line `font_scale` (heading promotion etc.) and visual spans are
        // folded into the wrap calc so a heading whose decorated text is e.g.
        // 1.6× the base monospace cell wraps at the right column, and a hidden
        // marker (`<span …>` tag, wikilink target) doesn't eat wrap budget it
        // never paints. Probes one byte past EOL so decorations anchored at the
        // line break still register (range queries are half-open).
        let geo_key = {
            use std::hash::{Hash, Hasher};
            let mut h = std::collections::hash_map::DefaultHasher::new();
            state.doc.content_id().hash(&mut h);
            view.decorations.geometry_epoch.hash(&mut h);
            view.wrap_map.width().to_bits().hash(&mut h);
            view.wrap_map.char_width().to_bits().hash(&mut h);
            view.wrap_map.enabled().hash(&mut h);
            h.finish()
        };
        let vp = view.visible_lines();
        let walk = view.wrap_map.walk_range(geo_key, total, (vp.start, vp.end));
        let mut spans: Vec<VisualSpan> = Vec::new();
        for line in walk {
            let start = state.doc.line_to_byte(line);
            let line_text = state.doc.line_str(line);
            let line_len = line_text.len();
            let probe_end = (start + line_len).max(start + 1);
            let snap = |b: usize| -> usize {
                let mut b = b.min(line_len);
                while b > 0 && !line_text.is_char_boundary(b) {
                    b -= 1;
                }
                b
            };
            let mut max_scale: f32 = 1.0;
            spans.clear();
            for layer in &view.decorations.layers {
                for (range, deco) in layer.iter_overlapping(start..probe_end) {
                    match deco {
                        Decoration::Mark(ms) => {
                            if let Some(s) = ms.font_scale
                                && s > max_scale
                            {
                                max_scale = s;
                            }
                        }
                        Decoration::Replace { display } => {
                            let s = snap(range.start.saturating_sub(start));
                            let e = snap(range.end.saturating_sub(start));
                            if e > s {
                                let cols =
                                    display.as_ref().map_or(0, |d| d.chars().count()) as u32;
                                spans.push(VisualSpan { start: s as u32, end: e as u32, cols });
                            }
                        }
                        _ => {}
                    }
                }
            }
            spans.sort_by_key(|s| s.start);
            view.wrap_map.get_or_compute(line, &line_text, max_scale, &spans);
        }
    }
}
