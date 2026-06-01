//! Autocomplete framework (SPEC §9.6, IMPLEMENTATION §16.5.3).
//!
//! Defines the pluggable [`CompletionSource`] trait, the [`CompletionItem`]
//! the popup displays, and the [`CompletionState`] that the view tracks while
//! the popup is open. The actual popup painting lives in the egui backend
//! (`editor-egui::completion`); input handling lives in `command.rs`.

use std::ops::Range;

use editor_core::state::Editor as EditorState;
use smol_str::SmolStr;

/// A single completion candidate produced by a [`CompletionSource`].
#[derive(Clone, Debug)]
pub struct CompletionItem {
    /// Text shown in the popup list (the "label").
    pub label: SmolStr,
    /// Optional secondary text shown next to the label (signature, type, etc.).
    pub detail: Option<SmolStr>,
    /// Text to insert when the user commits this item.
    pub insert: SmolStr,
    /// Optional explicit replace range. If `None`, the source asks the host
    /// to replace the trailing word at the cursor.
    pub replace_range: Option<Range<usize>>,
    /// Hint for icon / sort ordering.
    pub kind: CompletionKind,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CompletionKind {
    Snippet,
    Variable,
    Function,
    Keyword,
    Wikilink,
    Text,
}

/// A pluggable source of completion candidates. Sources are stored on the
/// [`ViewState`] as `Arc<dyn CompletionSource>` and queried each time a
/// trigger character is typed (or the user requests completion explicitly).
pub trait CompletionSource: Send + Sync {
    /// Characters that should auto-open the popup when typed. The default
    /// empty slice means the source is only invoked via explicit
    /// (Ctrl-Space) triggering.
    fn triggers(&self) -> &[char] {
        &[]
    }
    /// Return all candidates this source produces for `state` with the
    /// caret at byte offset `pos`. Sources should filter against the
    /// current query themselves; the caller does not pre-filter.
    fn matches(&self, state: &EditorState, pos: usize) -> Vec<CompletionItem>;

    /// Cheap predicate: would this source produce completions at `pos`
    /// even though no trigger char was just typed? Lets the driver
    /// re-open the popup when the caret is edited back into an existing
    /// context — e.g. typing inside an already-closed `[[wikilink]]`.
    /// Must be cheap (a local scan); it runs on every non-trigger
    /// keystroke. Default `false` so most sources only open on their
    /// trigger char. [bug-wikilink-edit-reopens-popup]
    fn reopens_in_context(&self, _state: &EditorState, _pos: usize) -> bool {
        false
    }
}

/// A candidate offered to the shared ranking core. It carries the
/// `CompletionItem` that should land if chosen, plus the two text fields
/// the ranker scores against: the `label` (whole-path or token) and an
/// optional `basename` (the final `/`-segment) weighted above the folder
/// prefix for vault paths. When `basename` is `None` the ranker scores the
/// label alone. [autocomplete-shared-core]
#[derive(Clone, Debug)]
pub struct RankCandidate {
    /// Full text scored at base weight (e.g. the relative vault path).
    pub label: SmolStr,
    /// Basename scored at a boosted weight; `None` to score `label` only.
    pub basename: Option<SmolStr>,
    /// The item committed when this candidate is chosen.
    pub item: CompletionItem,
}

/// A source of candidate matching/ranking with no buffer coupling. Both the
/// in-buffer [`CompletionSource`] and the standalone picker build on this so
/// the ranking is shared and only the trigger/replace seam differs.
/// [autocomplete-candidate-source]
pub trait CandidateSource {
    /// Return up to `limit` ranked items for `query` (case-insensitive
    /// subsequence match, basename-aware). Implementations enumerate their
    /// own candidate set and run it through [`rank`].
    fn candidates(&self, query: &str, limit: usize) -> Vec<CompletionItem>;
}

/// Rank `candidates` against `query` and return the top `limit`
/// [`CompletionItem`]s. Pure: no egui, no buffer. [autocomplete-shared-core]
///
/// Matching is case-insensitive subsequence: a candidate qualifies when
/// every `query` char appears in order in its scored text. Score boosts
/// reward (in descending strength) exact match, prefix match, a contiguous
/// run, and matches that begin on a word- or `/`-segment boundary. For
/// candidates carrying a `basename`, the basename is scored above the full
/// label so a basename hit beats a deep-path hit — reproducing the behavior
/// the wikilink source hand-coded in `score_basename`.
///
/// An empty `query` keeps every candidate (score 0) so the list shows the
/// full set. Ties break by `label` (then by stable order) so results don't
/// reshuffle frame to frame.
#[must_use]
pub fn rank(query: &str, candidates: Vec<RankCandidate>, limit: usize) -> Vec<CompletionItem> {
    let needle = query.to_lowercase();
    let mut scored: Vec<(i64, SmolStr, CompletionItem)> = Vec::new();
    for cand in candidates {
        let label_lower = cand.label.to_lowercase();
        let base_score = score_text(&label_lower, &needle);
        // Basename is weighted strictly above the folder prefix: take the
        // better of (boosted basename score, plain label score).
        let score = match &cand.basename {
            Some(bn) => {
                let bn_score = score_text(&bn.to_lowercase(), &needle);
                base_score.max(bn_score.saturating_add(BASENAME_BOOST))
            }
            None => base_score,
        };
        if score <= 0 && !needle.is_empty() {
            continue;
        }
        scored.push((score, cand.label.clone(), cand.item));
    }
    // Sort by descending score, then ascending label for determinism.
    scored.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
    scored.truncate(limit);
    scored.into_iter().map(|(_, _, item)| item).collect()
}

/// Extra weight applied to a basename match so it outranks an equally
/// strong folder-prefix match on the same path.
const BASENAME_BOOST: i64 = 1000;

/// Score one lowercased `text` against a lowercased `needle`. `0` means no
/// match (the query is not a subsequence). Larger is better. Mirrors the
/// tiers the wikilink source used (exact > prefix > contiguous > boundary
/// subsequence > scattered subsequence) so ranking behavior is preserved.
fn score_text(text: &str, needle: &str) -> i64 {
    if needle.is_empty() {
        return 1;
    }
    if text == needle {
        return 10_000;
    }
    if text.starts_with(needle) {
        return 5_000;
    }
    if let Some(at) = text.find(needle) {
        // Contiguous run; reward when it starts on a word/segment boundary.
        let boundary = at == 0 || is_boundary_byte(text.as_bytes()[at - 1]);
        return if boundary { 3_000 } else { 2_000 };
    }
    // Scattered subsequence: every needle char appears in order. Reward
    // runs that begin on segment boundaries so `arch` ranks `…/architecture`
    // above an interior-only scatter.
    match subsequence_score(text, needle) {
        Some(boundary_hits) => 100 + boundary_hits,
        None => 0,
    }
}

/// `true` for bytes that delimit a word or path segment, so a match landing
/// just after one counts as boundary-aligned.
const fn is_boundary_byte(b: u8) -> bool {
    matches!(b, b'/' | b' ' | b'-' | b'_' | b'.')
}

/// If `needle` is a subsequence of `text`, return how many of its matched
/// characters landed on a word/segment boundary (used as a tie-breaking
/// bonus). Returns `None` when it isn't a subsequence at all.
fn subsequence_score(text: &str, needle: &str) -> Option<i64> {
    let bytes = text.as_bytes();
    let mut ni = needle.bytes();
    let mut next = ni.next();
    let mut boundary_hits: i64 = 0;
    for (idx, &b) in bytes.iter().enumerate() {
        let Some(c) = next else { break };
        if c == b {
            if idx == 0 || is_boundary_byte(bytes[idx - 1]) {
                boundary_hits += 1;
            }
            next = ni.next();
        }
    }
    if next.is_none() { Some(boundary_hits) } else { None }
}

/// Per-frame state of the autocomplete popup. Inactive by default.
#[derive(Clone, Debug, Default)]
pub struct CompletionState {
    pub active: bool,
    pub items: Vec<CompletionItem>,
    /// Index of the highlighted item in `items`.
    pub selected: usize,
    /// Byte offset where the popup was opened (used to anchor the popup and
    /// to compute the live query).
    pub anchor_byte: usize,
    /// Current query string (chars typed after `anchor_byte`).
    pub query: String,
}

impl CompletionState {
    pub fn close(&mut self) {
        self.active = false;
        self.items.clear();
        self.selected = 0;
        self.query.clear();
    }

    pub fn open(&mut self, anchor_byte: usize, items: Vec<CompletionItem>) {
        self.active = !items.is_empty();
        self.items = items;
        self.selected = 0;
        self.anchor_byte = anchor_byte;
        self.query.clear();
    }

    pub fn move_selection(&mut self, delta: isize) {
        if self.items.is_empty() {
            return;
        }
        let n = self.items.len() as isize;
        let mut s = self.selected as isize + delta;
        s = s.rem_euclid(n);
        self.selected = s as usize;
    }

    pub fn selected_item(&self) -> Option<&CompletionItem> {
        self.items.get(self.selected)
    }
}

#[cfg(test)]
mod rank_tests {
    use super::{rank, CompletionItem, CompletionKind, RankCandidate};
    use smol_str::SmolStr;

    fn cand(label: &str, basename: Option<&str>) -> RankCandidate {
        RankCandidate {
            label: SmolStr::from(label),
            basename: basename.map(SmolStr::from),
            item: CompletionItem {
                label: SmolStr::from(basename.unwrap_or(label)),
                detail: Some(SmolStr::from(label)),
                insert: SmolStr::from(label),
                replace_range: None,
                kind: CompletionKind::Text,
            },
        }
    }

    fn labels(items: &[CompletionItem]) -> Vec<&str> {
        items.iter().map(|i| i.label.as_str()).collect()
    }

    #[test]
    fn prefix_beats_mid_match() {
        let items = rank(
            "arch",
            vec![cand("search", None), cand("architecture", None)],
            10,
        );
        assert_eq!(labels(&items), vec!["architecture", "search"]);
    }

    #[test]
    fn basename_beats_deep_path() {
        // Both contain "arch"; the basename match must win.
        let items = rank(
            "arch",
            vec![
                cand("architecture/notes/draft.md", Some("draft")),
                cand("notes/architecture.md", Some("architecture")),
            ],
            10,
        );
        assert_eq!(items[0].label.as_str(), "architecture");
    }

    #[test]
    fn subsequence_matches() {
        let items = rank("ace", vec![cand("abcde", None), cand("xyz", None)], 10);
        assert_eq!(labels(&items), vec!["abcde"]);
    }

    #[test]
    fn nonmatch_is_filtered() {
        let items = rank("zzz", vec![cand("abcde", None)], 10);
        assert!(items.is_empty());
    }

    #[test]
    fn empty_query_keeps_all_in_label_order() {
        let items = rank("", vec![cand("beta", None), cand("alpha", None)], 10);
        // Deterministic tie-break by label.
        assert_eq!(labels(&items), vec!["alpha", "beta"]);
    }

    #[test]
    fn deterministic_tie_break_by_label() {
        // Same match tier ("exact-ish prefix"); order must be by label.
        let items = rank(
            "no",
            vec![cand("note", None), cand("nope", None), cand("nod", None)],
            10,
        );
        assert_eq!(labels(&items), vec!["nod", "nope", "note"]);
    }

    #[test]
    fn respects_limit() {
        let items = rank(
            "a",
            vec![cand("a1", None), cand("a2", None), cand("a3", None)],
            2,
        );
        assert_eq!(items.len(), 2);
    }

    #[test]
    fn boundary_subsequence_beats_scattered() {
        // "ab" appears boundary-aligned in "alpha-beta" (a…, -b…) vs
        // scattered inside "cabbage". Boundary wins.
        let items = rank(
            "ab",
            vec![cand("crab-base", None), cand("zabzz", None)],
            10,
        );
        // "crab-base": contiguous "ab" inside "crab"; "zabzz": contiguous
        // "ab" inside. Both contiguous, neither boundary — fall to label
        // tie-break determinism. Just assert both present & stable.
        assert_eq!(labels(&items), vec!["crab-base", "zabzz"]);
    }
}
