//! Inline-widget painting for the editor's `PaintCtx` (slugs
//! `widget-painter-texture-blit`, `editor-inline-widget-display`,
//! `widget-inline-math-baseline`).
//!
//! An inline decoration widget occupies a measured box on a text row. This
//! module owns the three ways that box gets painted — a baseline-aligned
//! texture blit for pixel widgets, a textual variant that reads as ordinary
//! inline text, and a bordered placeholder — plus the geometry (`SegSpan`)
//! and baseline metric (`galley_ascent`) those paths share. It is a paint
//! concern lifted off the main `widget.rs` row loop, which constructs a
//! `SegSpan` per inline segment and calls into the methods here.

use std::sync::Arc;

use editor_core::decoration::InlineWidget;
use editor_view::viewport::ClickAction;
use editor_view::viewport::ClickRect;
use editor_view::viewport::ClickZone;
use egui::{Color32, Pos2, Rect, Stroke};

use super::to_egui_color;
use super::PaintCtx;

/// Geometry of one inline segment's placeholder box: its horizontal
/// extent (`x`, `width`), the row band it sits in (`top_y`, `height`),
/// the top `label_y` for any galley drawn inside, and the absolute y of
/// the surrounding text's baseline in this row (`baseline_y`) so an inline
/// pixel widget sits on the same baseline as the glyphs around it. `Copy`.
#[derive(Clone, Copy)]
pub(crate) struct SegSpan {
    pub(crate) x: f32,
    pub(crate) width: f32,
    pub(crate) top_y: f32,
    pub(crate) height: f32,
    pub(crate) label_y: f32,
    pub(crate) baseline_y: f32,
}

impl PaintCtx<'_> {
    /// v1 placeholder render for an inline widget decoration: a styled rect of
    /// the widget's measured size plus a tiny "widget" label. Real per-widget
    /// painting is deferred to a future trait method.
    pub(crate) fn paint_inline_widget_placeholder(
        &mut self,
        widget: &Arc<dyn InlineWidget>,
        span: SegSpan,
        label_galley: &Arc<egui::Galley>,
    ) {
        let SegSpan { x: seg_x, width: seg_w, top_y: line_top_y, height: row_height, label_y, .. } = span;
        let visuals = self.ui.style().visuals.clone();
        let rect = Rect::from_min_max(
            Pos2::new(seg_x, line_top_y),
            Pos2::new(seg_x + seg_w, line_top_y + row_height),
        );

        // status: widget-painter-texture-blit — a pixel widget blits its
        // cached texture; the textual path below (`editor-inline-widget-display`)
        // is unchanged and takes precedence over pixels per the trait contract.
        if widget.display().is_none() && self.blit_inline_pixels(widget, span) {
            if widget.handles_click() {
                self.push_inline_click_zone(widget, span);
            }
            return;
        }

        if let Some(display) = widget.display() {
            // Textual variant — host wants this widget to read as ordinary
            // inline text with a colored background (patch-review intraline
            // insertion, future inline diagnostics, etc.). Skip the bordered
            // placeholder entirely and just paint a bg fill + the galley.
            if let Some(bg) = display.bg {
                self.painter.rect_filled(rect, 0.0, to_egui_color(bg));
            }
            let fg = display
                .fg
                .map(to_egui_color)
                .unwrap_or_else(|| visuals.text_color());
            self.painter
                .galley(Pos2::new(seg_x, label_y), label_galley.clone(), fg);
            if display.strikethrough {
                let mid_y = line_top_y + row_height * 0.5;
                self.painter.line_segment(
                    [Pos2::new(seg_x, mid_y), Pos2::new(seg_x + seg_w, mid_y)],
                    Stroke::new(1.0, fg),
                );
            }
        } else {
            let bg = if visuals.dark_mode {
                Color32::from_rgba_unmultiplied(70, 80, 110, 80)
            } else {
                Color32::from_rgba_unmultiplied(210, 220, 240, 220)
            };
            let border = visuals.weak_text_color().gamma_multiply(0.5);
            self.painter.rect_filled(rect, 3.0, bg);
            self.painter
                .rect_stroke(rect, 3.0, Stroke::new(0.5, border), egui::StrokeKind::Inside);
            self.painter
                .galley(Pos2::new(seg_x + 2.0, label_y), label_galley.clone(), border);
        }

        if widget.handles_click() {
            self.push_inline_click_zone(widget, span);
        }
    }

    /// Record a widget-local click zone over the inline segment's box,
    /// dispatching to the widget's `widget_id`. Shared by the texture-blit,
    /// textual, and placeholder inline paths.
    fn push_inline_click_zone(&mut self, widget: &Arc<dyn InlineWidget>, span: SegSpan) {
        let SegSpan { x: seg_x, width: seg_w, top_y: line_top_y, height: row_height, .. } = span;
        self.view.click_zones.push(ClickZone {
            rect: ClickRect {
                x_min: seg_x - self.rect.min.x,
                y_min: line_top_y - self.rect.min.y,
                x_max: seg_x + seg_w - self.rect.min.x,
                y_max: line_top_y + row_height - self.rect.min.y,
            },
            action: ClickAction::WidgetClick(widget.widget_id()),
        });
    }

    /// Blit an inline pixel widget's cached texture at its true measured size,
    /// baseline-aligned via `InlineWidget::baseline()` when present so the
    /// formula sits on the surrounding text's baseline (`span.baseline_y`).
    /// The visual row was grown in `apply_line_height_decorations` to fit the
    /// widget's measured height, so the texture paints full-resolution rather
    /// than clipped to the text line. Returns `false` when the widget has no
    /// pixels or its buffer is malformed, so the caller paints a placeholder.
    ///
    /// status: widget-painter-texture-blit, widget-inline-math-baseline
    fn blit_inline_pixels(&mut self, widget: &Arc<dyn InlineWidget>, span: SegSpan) -> bool {
        let Some(pixels) = widget.pixels() else {
            return false;
        };
        // `pixels` are physical px; `measure` / `baseline` are logical points
        // (the widget divides out the device pixel ratio). The box is sized in
        // *points* so HiDPI doesn't render the texture at the raw pixel size —
        // `blit` letterboxes the physical texture into this rect — and at the
        // widget's full measured height (the row grew to fit it).
        let (w, h) = widget.measure(self.view.font_size);
        let target = match widget.baseline() {
            // Place the box so its baseline (b points below its top) lands on
            // the surrounding text's baseline, left-anchored at the reserved x.
            Some(b) if h > 0.0 => Rect::from_min_size(
                Pos2::new(span.x, span.baseline_y - b),
                egui::vec2(w, h),
            ),
            // No baseline: vertically center the box in the row band, left-
            // anchored at the reserved x.
            _ => {
                let top = span.top_y + (span.height - h) * 0.5;
                Rect::from_min_size(Pos2::new(span.x, top), egui::vec2(w, h))
            }
        };
        self.cache
            .textures
            .blit(self.ui, &self.painter, widget.widget_id(), &pixels, target)
    }
}

/// Distance in points from a single-line galley's top to its text baseline.
/// Read from the first glyph's `font_ascent` (the metric egui itself draws
/// text against); falls back to a ratio of `line_height` for an empty galley
/// so an inline widget on a blank-ish run still aligns sensibly.
pub(crate) fn galley_ascent(galley: &egui::Galley, line_height: f32) -> f32 {
    galley
        .rows
        .first()
        .and_then(|r| r.glyphs.first())
        .map(|g| g.font_ascent)
        .unwrap_or(line_height * 0.8)
}
