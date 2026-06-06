//! Agent-overlay geometry: where the per-hunk Accept/Reject (or Restore)
//! action row anchors relative to a `diff(working, proposal)` hunk.
//!
//! Split out of the app's `diff_overlay.rs` so the placement math is a pure
//! function of the two texts (no editor / op-log state) — unit-testable, and
//! reused by the headless `diff-overlay-snapshot` tool that renders the
//! overlay across scenarios.
//!
//! The green addition for an `Added` hunk is rendered by
//! [`crate::view::proposal_decorations`] as a `BlockSide::Above` block at the
//! working line `left_lines.start`. The action row must land *adjacent* to
//! that block — see [`anchor_and_side`].

use editor_core::decoration::BlockSide;
use editor_core::diff::{Hunk, HunkKind};
use editor_core::rope::Rope;

/// A `diff(working, proposal)` change hunk projected into byte coordinates.
/// `byte_*` is the span in the editable buffer (`working`, the diff's left
/// side) where the hunk attaches — a pure insertion is a zero-width range at
/// its site. `op_*` is the span in the `proposal` (right side) where the
/// pending-op content lives (an insertion has width there), used to resolve
/// which pending ops a hunk covers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentHunk {
    pub byte_start: usize,
    pub byte_end: usize,
    pub op_start: usize,
    pub op_end: usize,
    pub kind: HunkKind,
}

impl AgentHunk {
    /// A pure insertion has no footprint in the working buffer (zero-width).
    pub const fn is_pure_insertion(&self) -> bool {
        self.byte_start == self.byte_end
    }
}

/// Byte offset of the start of each physical line in `text`, with a trailing
/// entry equal to `text.len()`.
fn line_byte_offsets(text: &str) -> Vec<usize> {
    let mut offsets = vec![0usize];
    let mut acc = 0usize;
    for line in text.split_inclusive('\n') {
        acc += line.len();
        offsets.push(acc);
    }
    offsets
}

/// Project `diff(working, proposal)` change hunks into [`AgentHunk`] byte
/// spans. `working` is the editable buffer (diff left/old side); `proposal` is
/// the agent's review materialization (right/new side).
pub fn agent_hunks(working: &Rope, proposal: &str, hunks: &[Hunk]) -> Vec<AgentHunk> {
    let buf_byte = |line: usize| -> usize {
        if line >= working.len_lines() {
            working.len_bytes()
        } else {
            working.line_to_byte(line)
        }
    };
    let prop_offsets = line_byte_offsets(proposal);
    let prop_byte = |line: usize| -> usize { prop_offsets.get(line).copied().unwrap_or(proposal.len()) };

    let mut out = Vec::new();
    for h in hunks {
        if matches!(h.kind, HunkKind::Context) {
            continue;
        }
        let byte_start = buf_byte(h.left_lines.start);
        let byte_end = buf_byte(h.left_lines.end).max(byte_start);
        let op_start = prop_byte(h.right_lines.start);
        let op_end = prop_byte(h.right_lines.end).max(op_start);
        out.push(AgentHunk { byte_start, byte_end, op_start, op_end, kind: h.kind.clone() });
    }
    out
}

/// The byte position and block side to anchor a per-hunk action row at.
///
/// - **Modify / delete** (the changed lines exist in the buffer, so the hunk
///   has width): anchor at the hunk's last line, rendered `Below` it — the row
///   sits beneath the change.
/// - **Pure insertion** (zero-width in the buffer): the green addition renders
///   `Above` the working line at the insertion site, so the action row must
///   also be `Above` that same line to stay adjacent to the addition. Anchoring
///   it `Below` (the old behavior) placed the buttons one line *past* the
///   following context line — the off-by-one the snapshot tool surfaced.
pub fn anchor_and_side(working: &Rope, byte_start: usize, byte_end: usize) -> (usize, BlockSide) {
    let line_to_byte = |line: usize| -> usize {
        if line >= working.len_lines() {
            working.len_bytes()
        } else {
            working.line_to_byte(line)
        }
    };
    if byte_end > byte_start {
        let last_line_end = byte_end.saturating_sub(1).min(working.len_bytes());
        let line = working.byte_to_line(last_line_end);
        (line_to_byte(line), BlockSide::Below)
    } else {
        let pos = byte_start.min(working.len_bytes());
        let line = working.byte_to_line(pos);
        (line_to_byte(line), BlockSide::Above)
    }
}
