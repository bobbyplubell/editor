//! Soft-wrap integration tests.

use editor_core::decoration::Decoration;
use editor_core::rangeset::RangeSet;
use editor_core::state::Editor as EditorState;
use editor_egui::widget::Widget as EditorWidget;
use editor_view::viewport::ViewState;

const fn long_line() -> &'static str {
    // ~150 chars of one buffer line. Wrap at ~80px wide widget = several VLines.
    "the quick brown fox jumps over the lazy dog the quick brown fox jumps over the lazy dog the quick brown fox jumps over the lazy dog\n"
}

#[test]
fn wrap_off_gives_single_vline_per_buffer_line() {
    let mut state = EditorState::new(long_line());
    let mut view = ViewState::default();
    // wrap disabled by default
    {
        let mut harness = egui_kittest::Harness::builder()
            .with_size(egui::vec2(400.0, 200.0))
            .build_ui(|ui| {
                EditorWidget::new(&mut state, &mut view).show(ui);
            });
        harness.run();
    }
    // Without wrap, the wrap map should report 1 visual line (or be empty —
    // either is fine since the painter wouldn't iterate vlines).
    if let Some(w) = view.wrap_map.peek(0) {
        assert_eq!(w.visual_count(), 1, "wrap-off: should be 1 vline, got {}", w.visual_count());
    }
}

#[test]
fn wrap_on_produces_multiple_vlines_for_long_line() {
    let mut state = EditorState::new(long_line());
    let mut view = ViewState::default();
    view.wrap_map.set_enabled(true);

    {
        let mut harness = egui_kittest::Harness::builder()
            .with_size(egui::vec2(400.0, 200.0))
            .build_ui(|ui| {
                EditorWidget::new(&mut state, &mut view).show(ui);
            });
        harness.run();
    }
    let w = view.wrap_map.peek(0).expect("wrap entry for line 0");
    assert!(w.visual_count() > 1, "long line should wrap: got {} vlines", w.visual_count());
}

/// Regression: pressing Enter while wrap is on emptied/shifted a visible line
/// in the same frame as paint. `measure()` took its stale (edit-frame) early
/// return, which skipped re-wrapping, so the painter sliced the now-empty line
/// text with the previous frame's longer vline range and panicked with
/// "end byte index N is out of bounds of ``". The fix keeps the wrap cache in
/// sync every frame, before that early return.
#[test]
fn enter_in_wrapped_list_does_not_panic() {
    // A list item; line 0 is 8 bytes ("- abcdef"), line 1 empty.
    let mut state = EditorState::new("- abcdef\n");
    let mut view = ViewState::default();
    view.wrap_map.set_enabled(true);

    {
    let mut harness = egui_kittest::Harness::builder()
        .with_size(egui::vec2(400.0, 200.0))
        .build_ui(|ui| {
            EditorWidget::new(&mut state, &mut view).show(ui);
        });
    // Frame 1: click near the start of line 0 to grant focus and put the caret
    // a few bytes in, while caching line 0's vline range against the full
    // 8-byte text. Splitting anywhere before byte 8 (next frame) leaves line 0
    // shorter than that cached range.
    harness.input_mut().events.push(egui::Event::PointerButton {
        pos: egui::pos2(70.0, 10.0),
        button: egui::PointerButton::Primary,
        pressed: true,
        modifiers: Default::default(),
    });
    harness.input_mut().events.push(egui::Event::PointerButton {
        pos: egui::pos2(70.0, 10.0),
        button: egui::PointerButton::Primary,
        pressed: false,
        modifiers: Default::default(),
    });
    harness.run();

    // Frame 2: Enter is applied during this frame's input handling, so it is an
    // "edit frame" (pre_doc_id != doc_id) — exactly the path that panicked.
    harness.input_mut().events.push(egui::Event::Key {
        key: egui::Key::Enter,
        physical_key: None,
        pressed: true,
        repeat: false,
        modifiers: Default::default(),
    });
    harness.input_mut().events.push(egui::Event::Key {
        key: egui::Key::Enter,
        physical_key: None,
        pressed: false,
        repeat: false,
        modifiers: Default::default(),
    });
    harness.run();
    }

    // Reaching here without a panic is the assertion; sanity-check the split.
    assert_eq!(state.doc.len_lines(), 3, "Enter should have added a line");
}

/// Regression: a line whose soft-wrap row count changes when a *viewport-scoped*
/// decoration begins covering it on scroll-in must have its height map row
/// re-derived to match — otherwise the painter stacks a different number of
/// visual rows than the row reserves, producing the overlapping / jumbled text
/// seen near the bottom of scrolled canvas cards.
///
/// The wrap cache is refreshed for the visible band every frame, but the height
/// map was only re-derived on a doc / metrics / height-decoration change. A
/// viewport-scoped `Replace` (wikilink hide, inline-math substitution, …) only
/// covers a line once it scrolls into view, so it flips the line's wrap count
/// without touching any of those signatures. The fix records the wrap count each
/// derivation reserved and re-derives when a visible line's live count diverges.
#[test]
fn scroll_in_viewport_replace_rederives_wrapped_row_height() {
    // 60 short lines, except line 30 which is long enough to wrap to several
    // visual rows. The long line carries spaces so it breaks cleanly.
    let long = "the quick brown fox jumps over the lazy dog and then the quick brown \
                fox jumps over the lazy dog once more for good measure indeed yes";
    let mut doc = String::new();
    for i in 0..60 {
        if i == 30 {
            doc.push_str(long);
        } else {
            doc.push('x');
        }
        doc.push('\n');
    }
    let target = 30usize;

    let mut state = EditorState::new(&doc);
    let mut view = ViewState::default();
    view.wrap_map.set_enabled(true);

    // Host decoration hook: once `target` is visible, hide all but its last few
    // bytes with a viewport-scoped `Replace`, collapsing its wrap to a single
    // visual row. Off-screen the line is never covered, so it wraps in full.
    let mut rebuild = move |st: &EditorState, vw: &mut ViewState| {
        vw.decorations.clear();
        if vw.visible_lines().contains(&target) {
            let ls = st.doc.line_to_byte(target);
            let le = ls + st.doc.line_str(target).len();
            let hide = ls..le.saturating_sub(4);
            let set = RangeSet::from_iter([(hide, Decoration::Replace { display: None })]);
            vw.decorations.push_viewport_scoped(set);
        }
    };

    // Frame 1: scrolled to the top. `target` is off-screen, so it is wrapped to
    // its full multi-row height and the height map reserves that many rows.
    view.scroll_y = 0.0;
    {
        let mut harness = egui_kittest::Harness::builder()
            .with_size(egui::vec2(360.0, 200.0))
            .build_ui(|ui| {
                EditorWidget::new(&mut state, &mut view)
                    .interactive(false)
                    .with_decoration_rebuild(&mut rebuild)
                    .show(ui);
            });
        harness.run();
    }
    let base = view.line_height;
    let rows_offscreen = view.wrap_map.peek(target).map_or(1, editor_view::wrapping::WrappedLine::visual_count);
    assert!(rows_offscreen > 1, "long line should wrap off-screen: got {rows_offscreen}");
    assert!(
        view.height_map.text_height(target) > base * 1.5,
        "off-screen the height map should reserve the full multi-row height"
    );

    // Frame 2: scroll the long line to the top of the viewport. The hook now
    // covers it with the hiding `Replace`, collapsing its wrap to one row.
    view.scroll_y = base * target as f32;
    {
        let mut harness = egui_kittest::Harness::builder()
            .with_size(egui::vec2(360.0, 200.0))
            .build_ui(|ui| {
                EditorWidget::new(&mut state, &mut view)
                    .interactive(false)
                    .with_decoration_rebuild(&mut rebuild)
                    .show(ui);
            });
        harness.run();
    }

    let rows_onscreen = view.wrap_map.peek(target).map_or(1, editor_view::wrapping::WrappedLine::visual_count);
    assert_eq!(rows_onscreen, 1, "the hiding Replace should collapse the line to one row");
    let h = view.height_map.text_height(target);
    assert!(
        (h - base).abs() < 1.0,
        "after the wrap collapsed, the height map must reserve exactly one row \
         (base {base}), got {h} — a stale multi-row allocation is the overlap bug"
    );
}

#[test]
fn wrap_reflows_when_width_changes() {
    let mut state = EditorState::new(long_line());
    let mut view = ViewState::default();
    view.wrap_map.set_enabled(true);

    {
        let mut harness = egui_kittest::Harness::builder()
            .with_size(egui::vec2(300.0, 200.0))
            .build_ui(|ui| {
                EditorWidget::new(&mut state, &mut view).show(ui);
            });
        harness.run();
    }
    let initial_vlines = view.wrap_map.peek(0).map(editor_view::wrapping::WrappedLine::visual_count).unwrap_or(1);
    {
        let mut harness = egui_kittest::Harness::builder()
            .with_size(egui::vec2(800.0, 200.0))
            .build_ui(|ui| {
                EditorWidget::new(&mut state, &mut view).show(ui);
            });
        harness.run();
    }
    let after = view.wrap_map.peek(0).map(editor_view::wrapping::WrappedLine::visual_count).unwrap_or(1);
    assert!(
        after < initial_vlines,
        "wider widget should reduce vline count: {initial_vlines} → {after}"
    );
}
