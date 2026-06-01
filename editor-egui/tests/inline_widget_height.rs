//! Slug `widget-inline-math-baseline`: a visual line's height must grow to fit
//! the tallest inline widget on it (a tall fraction / large operator), so the
//! widget paints full-height instead of being clipped to the text line. A line
//! with no inline widget keeps its base height, and the caret on a grown row
//! still spans the grown height (so cursor rect / hit-testing stay correct).

use std::sync::Arc;

use editor_core::decoration::{Decoration, InlineWidget};
use editor_core::rangeset::RangeSet;
use editor_core::state::Editor as EditorState;
use editor_egui::widget::Widget as EditorWidget;
use editor_view::viewport::ViewState;

/// A tall inline widget: `mult`× the font size in height, baseline near its
/// bottom. Stands in for an inline `$\frac{a}{b}$` whose box exceeds the text
/// line height.
struct TallWidget {
    mult: f32,
}

impl InlineWidget for TallWidget {
    fn measure(&self, font_size: f32) -> (f32, f32) {
        (font_size * 4.0, font_size * self.mult)
    }
    fn widget_id(&self) -> u64 {
        0xBADF00D
    }
    fn baseline(&self) -> Option<f32> {
        // Baseline a quarter up from the bottom of the box.
        Some(self.measure(14.0).1 * 0.75)
    }
}

fn run_with_widget(text: &str, range: std::ops::Range<usize>, mult: f32) -> ViewState {
    let mut state = EditorState::new(text);
    let mut view = ViewState::default();

    let widget: Arc<dyn InlineWidget> = Arc::new(TallWidget { mult });
    let deco = Decoration::InlineWidget { widget, atomic: true };
    let set = RangeSet::from_iter([(range, deco)]);
    // The inline-widget layer must be height-tracked for the row-growth pass to
    // scan it — exactly how the app routes the math-widget layer
    // (`push_with_heights`).
    view.decorations.push_with_heights(set);

    {
        let mut harness = egui_kittest::Harness::builder()
            .with_size(egui::vec2(400.0, 300.0))
            .build_ui(|ui| {
                EditorWidget::new(&mut state, &mut view).show(ui);
            });
        harness.run();
    }
    view
}

#[test]
fn tall_inline_widget_grows_its_row() {
    // Widget on line 0 is 3× the font size tall — well above the text line.
    let view = run_with_widget("x=ab\nplain line\n", 2..4, 3.0);
    let base = view.line_height;
    let grown = view.height_map.text_height(0);
    assert!(
        grown >= view.font_size * 3.0 - 0.5,
        "row with a 3x-tall inline widget should grow to >= 3x font ({}), got {grown}",
        view.font_size * 3.0
    );
    assert!(
        grown > base,
        "grown row height {grown} should exceed the base line height {base}"
    );
}

#[test]
fn line_without_inline_widget_is_unchanged() {
    // Line 1 ("plain line") carries no widget; it must keep the base height.
    let view = run_with_widget("x=ab\nplain line\n", 2..4, 3.0);
    let base = view.line_height;
    let plain = view.height_map.text_height(1);
    assert!(
        (plain - base).abs() < 0.5,
        "a line with no inline widget must stay at base height {base}, got {plain}"
    );
}

#[test]
fn short_inline_widget_does_not_shrink_row() {
    // A widget shorter than the text line must not shrink the row below base.
    let view = run_with_widget("x=ab\n", 2..4, 0.5);
    let base = view.line_height;
    let h = view.height_map.text_height(0);
    assert!(
        h >= base - 0.5,
        "a sub-line-height inline widget must not shrink the row below base {base}, got {h}"
    );
}

#[test]
fn caret_on_grown_row_spans_grown_height() {
    // Place the caret at the start of the grown row and confirm the row band
    // the caret/cursor rect is built from reflects the grown height — i.e. the
    // height map (which feeds line_top_y, cursor rect, hit-testing, scroll)
    // grew. The next line's top must sit a full grown row below line 0's top.
    let view = run_with_widget("x=ab\nplain line\n", 2..4, 3.0);
    let grown = view.height_map.text_height(0);
    let top0 = view.height_map.y_at_row_top(0);
    let top1 = view.height_map.y_at_row_top(1);
    assert!(
        (top1 - top0 - grown).abs() < 0.5,
        "line 1 should start a full grown row ({grown}) below line 0 (top0={top0}, top1={top1})"
    );
}
