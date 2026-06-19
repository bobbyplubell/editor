//! Public-API smoke tests for `editor-ts`.
//!
//! Real end-to-end parse/highlight tests need a concrete tree-sitter
//! grammar, which this crate intentionally does not depend on (see
//! `languages.rs`). For now we assert the public surface compiles and the
//! type signatures match what the spec requires; once a `lang-*` feature
//! is wired with its corresponding `tree-sitter-<lang>` dep, add a real
//! parse test guarded by `#[cfg(feature = "lang-json")]` (or similar).

use editor_core::change::Set as ChangeSet;
use editor_core::state::Editor as EditorState;
use editor_core::rope::Rope;

use editor_core::theme::light_default;
use editor_ts::parsing::TsLanguage;
use editor_ts::parsing::TsState;
use editor_ts::parsing::changeset_to_edits;
use editor_ts::parsing::parse;
use editor_ts::parsing::reparse;
use editor_ts::highlight::ts_decorations;
#[test]
fn public_api_surface_exists() {
    // Type-level assertions only — make sure the function signatures
    // line up with what the spec calls for. We cannot actually invoke
    // `parse` without a real `tree_sitter::Language`, so this is a
    // compile-time check expressed as `let _: fn(...) -> ...`.
    let _: fn(&TsLanguage, &str) -> TsState = parse;
    let _: fn(&TsLanguage, &str, &TsState, &[tree_sitter::InputEdit]) -> TsState = reparse;
    let _: fn(&EditorState, &TsState, Option<&editor_core::theme::Theme>) -> editor_core::decoration::Set =
        ts_decorations;
}

#[test]
fn changeset_to_edits_is_pure() {
    // This helper does not touch tree-sitter at all and is safe to test
    // without a real grammar.
    let before = Rope::from_str("fn main() {}");
    let cs = ChangeSet::of(
        before.len_bytes(),
        [(3..7, "foo".to_string())],
    );
    let edits = changeset_to_edits(&before, &cs);
    assert_eq!(edits.len(), 1);
    assert_eq!(edits[0].start_byte, 3);
    assert_eq!(edits[0].old_end_byte, 7);
    assert_eq!(edits[0].new_end_byte, 3 + "foo".len());
}

#[test]
fn theme_is_consumable_by_ts_decorations_signature() {
    // We can't construct a TsState without a parser, but we can prove
    // the theme type plumbs through.
    let theme = light_default();
    let _tokens = theme.tokens.len();
}

#[cfg(feature = "lang-rust")]
#[test]
fn rust_end_to_end_parse_and_highlight() {
    use editor_ts::languages::{bundle, Language};
    let lang = bundle(Language::Rust);
    let src = "/// docs\nfn main() { let s = \"hi\"; }\n";
    let ts = parse(&lang, src);
    let tags: Vec<&str> = ts.highlights.iter().map(|(_, t)| t.as_str()).collect();
    assert!(tags.iter().any(|t| t.starts_with("keyword")), "fn/let captured: {tags:?}");
    assert!(tags.iter().any(|t| t.starts_with("string")), "string literal captured");
    assert!(tags.iter().any(|t| t.starts_with("comment")), "doc comment captured");

    let state = EditorState::new(src);
    let decos = ts_decorations(&state, &ts, Some(&light_default()));
    assert!(decos.iter_all().next().is_some(), "highlights become Mark decorations");
}

#[cfg(feature = "lang-python")]
#[test]
fn python_end_to_end_parse_and_highlight() {
    use editor_ts::languages::{bundle, Language};
    let lang = bundle(Language::Python);
    let src = "def f(x):\n    # note\n    return 'hi'\n";
    let ts = parse(&lang, src);
    let tags: Vec<&str> = ts.highlights.iter().map(|(_, t)| t.as_str()).collect();
    assert!(tags.iter().any(|t| t.starts_with("keyword")), "def/return captured: {tags:?}");
    assert!(tags.iter().any(|t| t.starts_with("string")), "string literal captured");
    assert!(tags.iter().any(|t| t.starts_with("comment")), "comment captured");

    let state = EditorState::new(src);
    let decos = ts_decorations(&state, &ts, Some(&light_default()));
    assert!(decos.iter_all().next().is_some(), "highlights become Mark decorations");
}
