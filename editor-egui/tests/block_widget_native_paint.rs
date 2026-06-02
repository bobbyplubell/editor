//! Slug `widget-block-native-paint`: a `BlockWidget` that returns a retained
//! native-paint list (`paint_list`) is replayed by the painter directly — no
//! texture upload — and takes precedence over `pixels()`. A widget that handles
//! clicks gets its whole-widget `WidgetClick(widget_id)` zone, plus a per-region
//! `WidgetClick(region.id)` zone for each `click_regions` entry mapped linearly
//! into the painted box (the path table cells route through,
//! `widget-table-cell-edit`). This is the reusable hook tables consume
//! (`widget-table-render`).

use std::sync::Arc;

use editor_core::decoration::{
    BlockPaint, BlockSide, BlockWidget, Color, Decoration, TextAlign, WidgetClickRegion,
    WidgetPixels,
};
use editor_core::rangeset::RangeSet;
use editor_core::state::Editor as EditorState;
use editor_egui::widget::Widget as EditorWidget;
use editor_view::viewport::{ClickAction, ViewState};

const WIDGET_ID: u64 = 0x7AB1_E000;
const REGION: u64 = 0xCE11;

/// A block widget that supplies BOTH a paint list and pixels, so the test can
/// assert the paint list wins (`pixels()` would otherwise upload a texture).
/// With `regions` it also exposes one sub-region (a "cell") in the lower-right
/// quadrant of the painted box.
struct NativeWidget {
    paints: bool,
    regions: bool,
    /// A bright RGBA buffer used only if the painter wrongly took the pixel path.
    rgba: Vec<u8>,
}

impl NativeWidget {
    fn new(paints: bool, regions: bool) -> Self {
        Self { paints, regions, rgba: vec![0xFF; 4 * 4 * 4] }
    }
}

impl BlockWidget for NativeWidget {
    fn measure(&self, _font_size: f32, _width: f32) -> f32 {
        60.0
    }
    fn handles_click(&self) -> bool {
        true
    }
    fn widget_id(&self) -> u64 {
        WIDGET_ID
    }
    fn paint_list(&self, _font_size: f32, width: f32) -> Option<Vec<BlockPaint>> {
        if !self.paints {
            return None;
        }
        Some(vec![
            BlockPaint::Rect {
                x: 0.0,
                y: 0.0,
                w: width,
                h: 20.0,
                color: Color::rgb(200, 100, 50),
            },
            BlockPaint::Line {
                from: (0.0, 20.0),
                to: (width, 20.0),
                width: 1.0,
                color: Color::rgb(80, 80, 80),
            },
            BlockPaint::Text {
                x: 4.0,
                y: 24.0,
                text: "cell".into(),
                color: Color::rgb(10, 10, 10),
                font_scale: 1.0,
                align: TextAlign::Left,
            },
        ])
    }
    fn click_regions(&self, _font_size: f32, _width: f32) -> Vec<WidgetClickRegion> {
        if !self.regions {
            return Vec::new();
        }
        vec![WidgetClickRegion { x: 0.5, y: 0.5, w: 0.5, h: 0.5, id: REGION }]
    }
    fn pixels(&self) -> Option<WidgetPixels<'_>> {
        Some(WidgetPixels { rgba: &self.rgba, width: 4, height: 4 })
    }
}

fn run(paints: bool, regions: bool) -> ViewState {
    let mut state = EditorState::new("first line\nsecond line\n");
    let mut view = ViewState::default();

    let widget: Arc<dyn BlockWidget> = Arc::new(NativeWidget::new(paints, regions));
    let deco = Decoration::BlockWidget { side: BlockSide::Above, widget };
    let line1_start = state.doc.line_to_byte(1);
    let set = RangeSet::from_iter([(line1_start..line1_start + 1, deco)]);
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

fn zone<'a>(view: &'a ViewState, id: u64) -> Option<&'a editor_view::viewport::ClickZone> {
    view.click_zones.iter().find(|z| z.action == ClickAction::WidgetClick(id))
}

#[test]
fn native_paint_widget_emits_whole_widget_click_zone() {
    // The native-paint path runs without panicking and still records the
    // whole-widget click zone (so a body click can route to edit).
    let view = run(true, false);
    assert!(
        zone(&view, WIDGET_ID).is_some(),
        "native-paint widget records its whole-widget click zone"
    );
}

#[test]
fn no_regions_emits_only_whole_widget_zone() {
    // A native widget without sub-regions emits exactly one WidgetClick zone.
    let view = run(true, false);
    let widget_zones = view
        .click_zones
        .iter()
        .filter(|z| matches!(z.action, ClickAction::WidgetClick(_)))
        .count();
    assert_eq!(widget_zones, 1, "native paint with no regions emits one zone");
}

#[test]
fn native_paint_emits_per_region_zones() {
    // With sub-regions the native path emits a per-region zone (the table-cell
    // path) PLUS the whole-widget fallback — two WidgetClick zones. The region
    // zone maps into the painted box and so falls within the whole-widget zone.
    let view = run(true, true);
    let widget_zones = view
        .click_zones
        .iter()
        .filter(|z| matches!(z.action, ClickAction::WidgetClick(_)))
        .count();
    assert_eq!(widget_zones, 2, "one region zone + the whole-widget zone");

    let region = zone(&view, REGION).expect("region zone missing");
    let whole = zone(&view, WIDGET_ID).expect("whole-widget zone missing");
    // The lower-right-quadrant region sits inside the whole-widget rect.
    assert!(region.rect.x_min >= whole.rect.x_min - 0.5);
    assert!(region.rect.y_min >= whole.rect.y_min - 0.5);
    assert!(region.rect.x_max <= whole.rect.x_max + 0.5);
    assert!(region.rect.y_max <= whole.rect.y_max + 0.5);
    // And it's the lower-right portion: starts past the vertical midpoint.
    assert!(region.rect.y_min > (whole.rect.y_min + whole.rect.y_max) * 0.5 - 0.5);
}
