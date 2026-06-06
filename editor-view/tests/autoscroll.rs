//! Autoscroll during selection. SPEC.md §9.24 / §2.2 / §7.3.
//!
//! While the user drag-selects and the pointer reaches (or passes) the top or
//! bottom edge of the viewport, the view scrolls so the selection can extend
//! beyond what's currently on screen. Verifies:
//! - the velocity curve: dead zone in the middle, sign by edge, capped past it,
//! - a held bottom-edge drag scrolls the view down and grows the selection,
//! - a held top-edge drag (when already scrolled) scrolls back up,
//! - a drag in the dead zone neither scrolls nor flags `autoscroll_active`,
//! - the scroll (and the repaint flag) stop once clamped at the document end.

use editor_core::state::Editor as EditorState;
use editor_view::command::handle_mouse_with_mods;
use editor_view::command::selection_autoscroll_velocity;
use editor_view::command::Action;
use editor_view::events::Modifiers;
use editor_view::events::MouseButton;
use editor_view::events::MouseEvent;
use editor_view::viewport::DragState;
use editor_view::viewport::ViewState;

/// A view ~11 text lines tall (height 200, line_height 18), gutter off so the
/// x→byte mapping is a simple monospace approximation.
fn make_view() -> ViewState {
    ViewState { gutter_width: 0.0, font_size: 14.0, line_height: 18.0, width: 400.0, height: 200.0, ..ViewState::default() }
}

/// A document with `n` short numbered lines, taller than the viewport so there
/// is room to scroll.
fn doc(n: usize) -> EditorState {
    let text: String = (0..n).map(|i| format!("line {i}\n")).collect();
    EditorState::new(&text)
}

fn apply(action: Action, state: &mut EditorState) {
    if let Action::Replace { state: next, .. } = action {
        *state = next;
    }
}

#[test]
fn velocity_is_zero_in_dead_zone() {
    let view = make_view();
    // Anywhere comfortably between the top and bottom bands.
    assert_eq!(selection_autoscroll_velocity(&view, 100.0), 0.0);
    assert_eq!(selection_autoscroll_velocity(&view, view.height / 2.0), 0.0);
}

#[test]
fn velocity_signs_point_outward() {
    let view = make_view();
    // Near the top edge → scroll up (negative).
    assert!(selection_autoscroll_velocity(&view, 2.0) < 0.0);
    // Near the bottom edge → scroll down (positive).
    assert!(selection_autoscroll_velocity(&view, view.height - 2.0) > 0.0);
}

#[test]
fn velocity_grows_past_edge_then_caps() {
    let view = make_view();
    let at_edge = selection_autoscroll_velocity(&view, view.height); // y == bottom edge
    let past_edge = selection_autoscroll_velocity(&view, view.height + 200.0);
    assert!(past_edge >= at_edge, "further past the edge must not be slower");
    // Cap is AUTOSCROLL_MAX_LINES (1.25) * line_height.
    let cap = 1.25 * view.line_height;
    assert!(past_edge <= cap + f32::EPSILON, "speed must be capped");
    assert!((past_edge - cap).abs() < 0.01, "far past the edge should hit the cap");
}

#[test]
fn velocity_is_zero_without_height() {
    let mut view = make_view();
    view.height = 0.0;
    assert_eq!(selection_autoscroll_velocity(&view, -50.0), 0.0);
}

#[test]
fn bottom_edge_drag_scrolls_down_and_extends_selection() {
    let mut state = doc(100);
    let mut view = make_view();
    view.sync_to(&state);
    let mods = Modifiers::default();

    // Press near the top to start a selection on the first line.
    let down = MouseEvent::Down { button: MouseButton::Left, x: 5.0, y: 4.0, click_count: 1 };
    let act = handle_mouse_with_mods(&state, &mut view, &down, mods);
    apply(act, &mut state);
    assert!(matches!(view.drag, DragState::MaybeSelecting { .. }));

    let start_end = state.selection.main().end();

    // Hold the drag past the bottom edge for several frames (the egui adapter
    // re-emits the held Drag each frame; we simulate that here).
    let drag = MouseEvent::Drag { x: 5.0, y: view.height + 40.0, button: MouseButton::Left };
    let mut prev_scroll = view.scroll_y;
    for _ in 0..30 {
        let act = handle_mouse_with_mods(&state, &mut view, &drag, mods);
        apply(act, &mut state);
        assert!(view.scroll_y >= prev_scroll);
        prev_scroll = view.scroll_y;
    }

    assert!(view.scroll_y > 0.0, "view should have scrolled down");
    assert!(view.autoscroll_active, "still autoscrolling at the edge");
    assert!(
        state.selection.main().end() > start_end,
        "selection should have extended onto newly revealed lines"
    );
}

#[test]
fn dead_zone_drag_does_not_scroll() {
    let mut state = doc(100);
    let mut view = make_view();
    view.sync_to(&state);
    let mods = Modifiers::default();

    let down = MouseEvent::Down { button: MouseButton::Left, x: 5.0, y: 4.0, click_count: 1 };
    apply(handle_mouse_with_mods(&state, &mut view, &down, mods), &mut state);

    // Drag to the middle of the viewport — no autoscroll.
    let drag = MouseEvent::Drag { x: 30.0, y: 100.0, button: MouseButton::Left };
    apply(handle_mouse_with_mods(&state, &mut view, &drag, mods), &mut state);

    assert_eq!(view.scroll_y, 0.0);
    assert!(!view.autoscroll_active);
}

#[test]
fn top_edge_drag_scrolls_back_up() {
    let mut state = doc(100);
    let mut view = make_view();
    view.sync_to(&state);
    view.scroll_y = 500.0; // start partway down
    let mods = Modifiers::default();

    let down = MouseEvent::Down { button: MouseButton::Left, x: 5.0, y: 100.0, click_count: 1 };
    apply(handle_mouse_with_mods(&state, &mut view, &down, mods), &mut state);

    let drag = MouseEvent::Drag { x: 5.0, y: -20.0, button: MouseButton::Left };
    let before = view.scroll_y;
    apply(handle_mouse_with_mods(&state, &mut view, &drag, mods), &mut state);

    assert!(view.scroll_y < before, "dragging above the top edge scrolls up");
    assert!(view.autoscroll_active);
}

#[test]
fn autoscroll_stops_when_clamped_at_end() {
    let mut state = doc(100);
    let mut view = make_view();
    view.sync_to(&state);
    // Park at the very bottom: total - height.
    view.scroll_y = view.height_map.total_height() - view.height;
    let mods = Modifiers::default();

    let down = MouseEvent::Down { button: MouseButton::Left, x: 5.0, y: 100.0, click_count: 1 };
    apply(handle_mouse_with_mods(&state, &mut view, &down, mods), &mut state);

    let at_max = view.scroll_y;
    let drag = MouseEvent::Drag { x: 5.0, y: view.height + 40.0, button: MouseButton::Left };
    apply(handle_mouse_with_mods(&state, &mut view, &drag, mods), &mut state);

    assert_eq!(view.scroll_y, at_max, "already at the end — nothing to reveal");
    assert!(!view.autoscroll_active, "clamped scroll must not keep forcing repaints");
}

#[test]
fn mouse_up_clears_autoscroll_flag() {
    let mut state = doc(100);
    let mut view = make_view();
    view.sync_to(&state);
    let mods = Modifiers::default();

    let down = MouseEvent::Down { button: MouseButton::Left, x: 5.0, y: 4.0, click_count: 1 };
    apply(handle_mouse_with_mods(&state, &mut view, &down, mods), &mut state);
    let drag = MouseEvent::Drag { x: 5.0, y: view.height + 40.0, button: MouseButton::Left };
    apply(handle_mouse_with_mods(&state, &mut view, &drag, mods), &mut state);
    assert!(view.autoscroll_active);

    let up = MouseEvent::Up { button: MouseButton::Left, x: 5.0, y: view.height + 40.0 };
    handle_mouse_with_mods(&state, &mut view, &up, mods);
    assert!(!view.autoscroll_active);
    assert_eq!(view.drag, DragState::Idle);
}
