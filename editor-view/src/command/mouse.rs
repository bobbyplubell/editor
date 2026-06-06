//! Pointer→buffer command handling: the `Cmd` mouse-button state machine
//! (press / drag / release) that drives caret placement, word/line and
//! rectangle selections, drag-to-move text, and selection-drag autoscroll, plus
//! the two click-pattern helpers it relies on. Split out of `command.rs` as a
//! continuation of the `Cmd` impl so that file stays within its per-file line
//! budget; every item here is a method or associated fn on [`super::Cmd`].

use editor_core::change::Set as ChangeSet;
use editor_core::selection::{SelRange, Selection};
use editor_core::state::Editor as EditorState;
use editor_core::transaction::{EditType, Transaction};

use super::{
    apply_selection, apply_selection_autoscroll, view_to_buffer, view_to_buffer_at_line, Action,
    Cmd,
};
use crate::events::Modifiers;
use crate::multicursor;
use crate::viewport::DragState;

impl Cmd<'_> {
    pub(super) fn mouse_down(
        &mut self,
        x: f32,
        y: f32,
        click_count: u8,
        mods: Modifiers,
    ) -> Action {
        let state = self.state;
        let view = &mut *self.view;
        view.touch();
        // Check for a clickable decoration first.
        if let Some(zone) = view.click_zones.iter().find(|z| z.rect.contains(x, y)) {
            return Action::Click(zone.action.clone());
        }
        let pos = view_to_buffer(state, view, x, y);

        // Plain click (no modifiers, no multi-click) inside an existing
        // non-empty selection arms a possible text drag — we don't move
        // the caret yet, we wait to see whether the user drags or releases.
        if click_count == 1
            && !mods.shift
            && !mods.alt
            && !mods.primary()
            && Self::pos_in_any_nonempty_range(state, pos)
        {
            // 10px threshold matches CodeMirror 6's drag-to-move default. With
            // a smaller threshold, micro-jitter inside an existing selection
            // would mis-trigger a text drag rather than letting the user
            // click-collapse the selection and begin a new one.
            view.drag = DragState::MaybeDraggingSelection { start: (x, y), threshold: 10.0 };
            return Action::None;
        }

        // Alt-only Down (and not inside an existing selection) starts a
        // rectangular/column selection — place a single caret at `pos` and
        // arm `RectangleSelecting`. Alt+Shift retains the existing
        // multicursor-add semantics below.
        if click_count == 1
            && mods.alt
            && !mods.shift
            && !mods.primary()
            && !Self::pos_in_any_nonempty_range(state, pos)
        {
            view.drag = DragState::RectangleSelecting { start_xy: (x, y) };
            return Action::state_only(apply_selection(state, Selection::single(pos)));
        }

        let sel = match click_count {
            2 => {
                // Single code path: run `view.double_click_re` against the
                // clicked line's content. The default (`\w+`) reproduces the
                // historic Unicode-word behavior; users override via
                // `editor.double_click_pattern`. No match → plain caret.
                let line = state.doc.byte_to_line(pos);
                let line_start = state.doc.line_to_byte(line);
                let text = state.doc.line_str(line);
                let local = pos - line_start;
                match Self::pattern_span_at(&text, local, &view.double_click_re) {
                    Some((s, e)) => {
                        Selection::from_range(SelRange::new(line_start + s, line_start + e))
                    }
                    None => Selection::single(pos),
                }
            }
            3 => {
                // Single code path: run `view.triple_click_re` against the
                // clicked line **including** its trailing newline (the slice
                // `line_start..line_end_with_newline`), so the default `.*\n?`
                // reproduces the previous whole-line-incl-newline behavior.
                let line = state.doc.byte_to_line(pos);
                let line_start = state.doc.line_to_byte(line);
                let line_end = if line + 1 < state.doc.len_lines() {
                    state.doc.line_to_byte(line + 1)
                } else {
                    state.doc.len_bytes()
                };
                let text = state.doc.slice(line_start..line_end).to_string();
                let local = pos - line_start;
                match Self::pattern_span_at(&text, local, &view.triple_click_re) {
                    Some((s, e)) => {
                        Selection::from_range(SelRange::new(line_start + s, line_start + e))
                    }
                    None => Selection::single(pos),
                }
            }
            _ if mods.alt || (mods.primary() && !mods.shift) => {
                multicursor::add_cursor(state, pos)
            }
            _ if mods.shift => {
                let anchor = state.selection.main().anchor.offset();
                Selection::from_range(SelRange::new(anchor, pos))
            }
            _ => Selection::single(pos),
        };
        // Arm a drag from the selection we just made: a plain click anchors a
        // point (lo == hi), a double/triple click anchors the whole word/line so a
        // subsequent drag (or a no-motion stray Drag — see `mouse_drag`) extends
        // rather than collapses it.
        let main = sel.main();
        view.drag = DragState::MaybeSelecting { lo: main.start(), hi: main.end() };
        Action::state_only(apply_selection(state, sel))
    }

    pub(super) fn mouse_drag(&mut self, x: f32, y: f32) -> Action {
        let state = self.state;
        let view = &mut *self.view;
        match view.drag {
            DragState::MaybeSelecting { lo, hi } => {
                view.touch();
                // Scroll first so a drag held at (or past) a viewport edge keeps
                // revealing lines; `view_to_buffer` then maps the pointer against
                // the updated `scroll_y`, extending the head onto the new line.
                apply_selection_autoscroll(view, y);
                let head = view_to_buffer(state, view, x, y);
                // Union the anchored range with the pointer: while the pointer
                // stays within [lo, hi] the selection is exactly that range (so a
                // double-click word survives jitter and the no-motion stray Drag
                // the translate layer emits each held frame). Dragging past either
                // edge extends from the far edge, with the head on the moving side.
                let start = lo.min(head);
                let end = hi.max(head);
                let range = if head < lo {
                    SelRange::new(end, start)
                } else {
                    SelRange::new(start, end)
                };
                Action::state_only(apply_selection(state, Selection::from_range(range)))
            }
            DragState::MaybeDraggingSelection { start, threshold } => {
                let dx = x - start.0;
                let dy = y - start.1;
                if (dx * dx + dy * dy).sqrt() > threshold {
                    let drop_caret = view_to_buffer(state, view, x, y);
                    view.drag = DragState::DraggingSelection { drop_caret };
                    view.touch();
                }
                Action::None
            }
            DragState::DraggingSelection { .. } => {
                let drop_caret = view_to_buffer(state, view, x, y);
                view.drag = DragState::DraggingSelection { drop_caret };
                view.touch();
                Action::None
            }
            DragState::RectangleSelecting { start_xy } => {
                view.touch();
                // Same edge autoscroll as the linear case, before the y→line lookups
                // below read `scroll_y`, so a column selection can grow off-screen.
                apply_selection_autoscroll(view, y);
                // Build a multi-range Selection covering one SelRange per buffer
                // line intersecting the vertical span `[start_xy.1, y]`, each
                // spanning from x→byte(min_x) to x→byte(max_x) on its own line.
                // The main range is the line the pointer is currently on.
                let (sx, sy) = start_xy;
                let (cx, cy) = (x, y);
                let y_lo = sy.min(cy);
                let y_hi = sy.max(cy);
                let x_lo = sx.min(cx);
                let x_hi = sx.max(cx);
                let line_lo = view
                    .height_map
                    .line_at_y(y_lo + view.scroll_y)
                    .min(state.doc.len_lines().saturating_sub(1));
                let line_hi = view
                    .height_map
                    .line_at_y(y_hi + view.scroll_y)
                    .min(state.doc.len_lines().saturating_sub(1));
                let mut ranges: Vec<SelRange> = Vec::with_capacity(line_hi - line_lo + 1);
                for line in line_lo..=line_hi {
                    let a = view_to_buffer_at_line(state, view, x_lo, line);
                    let b = view_to_buffer_at_line(state, view, x_hi, line);
                    ranges.push(SelRange::new(a, b));
                }
                let cur_line = view
                    .height_map
                    .line_at_y(cy + view.scroll_y)
                    .min(state.doc.len_lines().saturating_sub(1));
                let main = cur_line.saturating_sub(line_lo).min(ranges.len() - 1);
                let sel = Selection::from_ranges(ranges, main);
                Action::state_only(apply_selection(state, sel))
            }
            DragState::Idle => Action::None,
        }
    }

    pub(super) fn mouse_up(&mut self, x: f32, y: f32) -> Action {
        let state = self.state;
        let view = &mut *self.view;
        let prev = view.drag;
        view.drag = DragState::Idle;
        // The drag is over — stop any edge autoscroll repaint loop.
        view.autoscroll_active = false;
        match prev {
            DragState::DraggingSelection { drop_caret } => {
                // Apply a text drag: remove the main selection range and
                // reinsert it at `drop_caret`. If the drop falls inside the
                // original range, cancel.
                let src = state.selection.main().range();
                if drop_caret >= src.start && drop_caret <= src.end {
                    view.touch();
                    return Action::None;
                }
                let text = state.doc.slice(src.clone()).to_string();
                let len = text.len();
                let mut edits: Vec<(std::ops::Range<usize>, String)> = if drop_caret > src.end {
                    vec![(drop_caret..drop_caret, text), (src.clone(), String::new())]
                } else {
                    vec![(src.clone(), String::new()), (drop_caret..drop_caret, text)]
                };
                edits.sort_by_key(|(r, _)| r.start);
                let changes = ChangeSet::of(state.doc.len_bytes(), edits);
                let new_start = if drop_caret > src.end {
                    drop_caret - (src.end - src.start)
                } else {
                    drop_caret
                };
                let new_sel = Selection::from_range(SelRange::new(new_start, new_start + len));
                let tx = Transaction::new(changes)
                    .with_edit_type(EditType::Other)
                    .with_selection(new_sel);
                view.touch();
                Action::doc(state.apply(tx.clone()), tx)
            }
            DragState::MaybeDraggingSelection { .. } => {
                // No drag occurred — treat as a plain click: collapse the
                // selection to a single caret at the clicked position.
                let pos = view_to_buffer(state, view, x, y);
                view.touch();
                Action::state_only(apply_selection(state, Selection::single(pos)))
            }
            _ => Action::None,
        }
    }

    fn pos_in_any_nonempty_range(state: &EditorState, pos: usize) -> bool {
        state.selection.ranges().iter().any(|r| !r.is_empty() && pos >= r.start() && pos < r.end())
    }

    /// Byte span (relative to `text`) of the first regex match that contains the
    /// click column `local`. `None` when no match covers it, so the caller falls
    /// back to its built-in behavior. End-inclusive (`local <= end`) so a click at
    /// a match's trailing edge still selects it, mirroring the Unicode-word path.
    /// Used by double/triple-click when `editor.{double,triple}_click_pattern` is
    /// set. status: click-select-pattern
    fn pattern_span_at(text: &str, local: usize, re: &regex::Regex) -> Option<(usize, usize)> {
        re.find_iter(text).map(|m| (m.start(), m.end())).find(|&(s, e)| local >= s && local <= e)
    }
}
