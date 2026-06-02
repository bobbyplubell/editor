//! Slug `editor-widget-click-regions`: a texture-backed `BlockWidget` can
//! expose clickable sub-regions in normalized widget coords. The painter maps
//! each region through the SAME aspect-preserving letterbox transform it uses
//! to blit the texture and emits a per-region `WidgetClick(region.id)` zone, in
//! addition to the whole-widget `WidgetClick(widget_id)` zone. A widget that
//! doesn't override `click_regions` keeps exactly today's single-zone behavior.

use std::sync::Arc;

use editor_core::decoration::{BlockSide, BlockWidget, Decoration, WidgetClickRegion, WidgetPixels};
use editor_core::rangeset::RangeSet;
use editor_core::state::Editor as EditorState;
use editor_egui::widget::Widget as EditorWidget;
use editor_view::viewport::{ClickAction, ViewState};

const WIDGET_ID: u64 = 0xB10C_C0DE;
const REGION_A: u64 = 0xA;
const REGION_B: u64 = 0xB;

/// A pixel block widget standing in for a rendered diagram. It supplies a tiny
/// RGBA buffer and (optionally) two clickable sub-regions — e.g. two nodes of a
/// graph — placed at known fractions of the widget box.
struct DiagramWidget {
    rgba: Vec<u8>,
    w: u32,
    h: u32,
    with_regions: bool,
}

impl DiagramWidget {
    fn new(w: u32, h: u32, with_regions: bool) -> Self {
        Self { rgba: vec![0xFF; (w as usize) * (h as usize) * 4], w, h, with_regions }
    }
}

impl BlockWidget for DiagramWidget {
    fn measure(&self, _font_size: f32, _width: f32) -> f32 {
        // Fixed block height so the heightmap reserves a predictable zone.
        80.0
    }
    fn handles_click(&self) -> bool {
        true
    }
    fn widget_id(&self) -> u64 {
        WIDGET_ID
    }
    fn pixels(&self) -> Option<WidgetPixels<'_>> {
        Some(WidgetPixels { rgba: &self.rgba, width: self.w, height: self.h })
    }
    fn click_regions(&self, _font_size: f32, _width: f32) -> Vec<WidgetClickRegion> {
        if !self.with_regions {
            return Vec::new();
        }
        vec![
            // Top-left quadrant.
            WidgetClickRegion { x: 0.0, y: 0.0, w: 0.5, h: 0.5, id: REGION_A },
            // Bottom-right quadrant.
            WidgetClickRegion { x: 0.5, y: 0.5, w: 0.5, h: 0.5, id: REGION_B },
        ]
    }
}

fn run_with_widget(w: u32, h: u32, with_regions: bool) -> ViewState {
    let mut state = EditorState::new("first line\nsecond line\n");
    let mut view = ViewState::default();

    let widget: Arc<dyn BlockWidget> = Arc::new(DiagramWidget::new(w, h, with_regions));
    let deco = Decoration::BlockWidget { side: BlockSide::Above, widget };
    // Anchor the block above line 1 (byte range starts at that line's start).
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

fn region_zone(view: &ViewState, id: u64) -> Option<&editor_view::viewport::ClickZone> {
    view.click_zones
        .iter()
        .find(|z| z.action == ClickAction::WidgetClick(id))
}

#[test]
fn emits_region_zones_plus_whole_widget_zone() {
    // Square texture so the letterbox math is easy to reason about.
    let view = run_with_widget(40, 40, true);

    // Exactly three WidgetClick zones: region A, region B, and the
    // whole-widget zone keyed on widget_id.
    let widget_zones: Vec<_> = view
        .click_zones
        .iter()
        .filter(|z| matches!(z.action, ClickAction::WidgetClick(_)))
        .collect();
    assert_eq!(
        widget_zones.len(),
        3,
        "expected two region zones + one whole-widget zone, got {:?}",
        widget_zones.iter().map(|z| &z.action).collect::<Vec<_>>()
    );

    let za = region_zone(&view, REGION_A).expect("region A zone missing");
    let zb = region_zone(&view, REGION_B).expect("region B zone missing");
    let whole = region_zone(&view, WIDGET_ID).expect("whole-widget zone missing");

    // The painted (letterboxed) box: a 40x40 square fit into the block zone.
    // The texture letterboxes into the CONTENT box (text column, gutter
    // excluded), not the full row, so a diagram clears the line-number gutter.
    // The zone is 80px tall, so the square scales to 80x80, centered
    // horizontally in the content box and vertically in the full block zone.
    // The whole-widget zone covers the FULL block rect, so it contains the box.
    let painted_w = 80.0;
    let painted_h = 80.0;
    let content_x_min = whole.rect.x_min + view.content_origin_x();
    let content_w = whole.rect.x_max - content_x_min;
    let painted_x_min = content_x_min + (content_w - painted_w) * 0.5;
    let painted_y_min = whole.rect.y_min + (whole.rect.y_max - whole.rect.y_min - painted_h) * 0.5;
    // Regression: the painted box starts right of the gutter (it used to center
    // in the full row, spilling into the gutter on a wide diagram).
    assert!(
        painted_x_min > whole.rect.x_min + 0.5,
        "diagram must clear the line-number gutter"
    );

    // Region A is the top-left quadrant of the painted box.
    assert!((za.rect.x_min - painted_x_min).abs() < 0.5, "A x_min off");
    assert!((za.rect.y_min - painted_y_min).abs() < 0.5, "A y_min off");
    assert!((za.rect.x_max - (painted_x_min + painted_w * 0.5)).abs() < 0.5, "A x_max off");
    assert!((za.rect.y_max - (painted_y_min + painted_h * 0.5)).abs() < 0.5, "A y_max off");

    // Region B is the bottom-right quadrant of the painted box.
    assert!((zb.rect.x_min - (painted_x_min + painted_w * 0.5)).abs() < 0.5, "B x_min off");
    assert!((zb.rect.y_min - (painted_y_min + painted_h * 0.5)).abs() < 0.5, "B y_min off");
    assert!((zb.rect.x_max - (painted_x_min + painted_w)).abs() < 0.5, "B x_max off");
    assert!((zb.rect.y_max - (painted_y_min + painted_h)).abs() < 0.5, "B y_max off");

    // Each region zone falls fully within the whole-widget zone (the full
    // block rect), confirming alignment with the painted region.
    for z in [za, zb] {
        assert!(z.rect.x_min >= whole.rect.x_min - 0.5);
        assert!(z.rect.y_min >= whole.rect.y_min - 0.5);
        assert!(z.rect.x_max <= whole.rect.x_max + 0.5);
        assert!(z.rect.y_max <= whole.rect.y_max + 0.5);
    }
}

#[test]
fn no_regions_emits_only_whole_widget_zone() {
    // A widget that doesn't override click_regions keeps today's behavior:
    // exactly one whole-widget WidgetClick zone, no extras.
    let view = run_with_widget(40, 40, false);
    let widget_zones: Vec<_> = view
        .click_zones
        .iter()
        .filter(|z| matches!(z.action, ClickAction::WidgetClick(_)))
        .collect();
    assert_eq!(widget_zones.len(), 1, "default (no regions) must emit one zone");
    assert!(
        region_zone(&view, WIDGET_ID).is_some(),
        "the single zone must be the whole-widget zone keyed on widget_id"
    );
}
