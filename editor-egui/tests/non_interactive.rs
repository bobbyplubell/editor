//! A non-interactive (display-only) widget must not consume pointer input: it
//! allocates a hover-only response, never takes focus, and leaves selection /
//! caret untouched when clicked or dragged over. This is what lets a host
//! surface above the widget (the canvas interaction surface) own all pointer
//! input while the widget paints the read-only content beneath it.

use editor_core::state::Editor as EditorState;
use editor_egui::widget::Widget as EditorWidget;
use editor_view::viewport::ViewState;

#[test]
fn non_interactive_widget_ignores_clicks_and_keeps_selection() {
    let mut state = EditorState::new("hello world\nsecond line\n");
    let mut view = ViewState::default();
    // Caret byte position we expect to survive a click over the body.
    let before = state.selection.main().head.offset();

    // Scope the harness so its closure's `&mut state` borrow ends before we
    // read `state` for the assertion below.
    {
        let mut harness = egui_kittest::Harness::builder()
            .with_size(egui::vec2(800.0, 600.0))
            .build_ui(|ui| {
                EditorWidget::new(&mut state, &mut view)
                    .interactive(false)
                    .show(ui);
            });
        harness.run();

        // A press well inside the text body would, on an interactive widget,
        // grant focus and move the caret. On a non-interactive one it must do
        // neither.
        let pos = egui::pos2(90.0, 10.0);
        harness.input_mut().events.push(egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed: true,
            modifiers: Default::default(),
        });
        harness.input_mut().events.push(egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed: false,
            modifiers: Default::default(),
        });
        harness.run();
    }

    assert_eq!(
        state.selection.main().head.offset(),
        before,
        "a non-interactive widget must not move the caret / selection on click",
    );
}

#[test]
fn interactive_widget_moves_caret_on_click() {
    // The complement: with the default (interactive) widget, the same click
    // does move the caret, so the non-interactive assertion above is meaningful.
    let mut state = EditorState::new("hello world\nsecond line\n");
    let mut view = ViewState::default();
    let before = state.selection.main().head.offset();

    {
        let mut harness = egui_kittest::Harness::builder()
            .with_size(egui::vec2(800.0, 600.0))
            .build_ui(|ui| {
                EditorWidget::new(&mut state, &mut view).show(ui);
            });
        harness.run();

        let pos = egui::pos2(90.0, 30.0);
        harness.input_mut().events.push(egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed: true,
            modifiers: Default::default(),
        });
        harness.input_mut().events.push(egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed: false,
            modifiers: Default::default(),
        });
        harness.run();
    }

    assert_ne!(
        state.selection.main().head.offset(),
        before,
        "an interactive widget should move the caret on a click into the body",
    );
}
