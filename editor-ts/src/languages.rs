//! Per-language `TsLanguage` bundles, gated by cargo features.
//!
//! [`bundle`] returns a fully-populated [`crate::TsLanguage`] for one
//! [`Language`]. Rust and Python are wired (optional deps pulled in by their
//! `lang-*` feature); the rest are stubs — the remaining
//! `tree-sitter-{javascript,…}` crates are intentionally **not** depended on,
//! because pulling them all in roughly doubles CI build time and most
//! consumers want only one or two.
//!
//! # Wiring a real grammar (host instructions)
//!
//! In the host's `Cargo.toml`:
//!
//! ```toml
//! [dependencies]
//! editor-ts = { version = "…", features = ["lang-rust"] }
//! tree-sitter-rust = "0.21"
//! ```
//!
//! Then in this file replace the `Language::Rust` arm's panic with the real
//! bundle:
//!
//! ```ignore
//! Language::Rust => TsLanguage {
//!     language: tree_sitter_rust::LANGUAGE.into(),
//!     highlights_query: tree_sitter_rust::HIGHLIGHTS_QUERY.to_string(),
//!     injections_query: Some(tree_sitter_rust::INJECTIONS_QUERY.to_string()),
//!     indent_query: None,
//! },
//! ```
//!
//! Hosts may also bypass this helper entirely and construct a
//! [`crate::TsLanguage`] by hand — useful when a grammar lives in a
//! private crate or when the highlights query is customized.

use crate::parsing::TsLanguage;

/// A built-in language whose grammar bundle is gated behind a cargo feature.
///
/// Each variant is only present when its `lang-*` feature is enabled, so the
/// match in [`bundle`] stays exhaustive against the configured feature set.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Language {
    #[cfg(feature = "lang-rust")]
    Rust,
    #[cfg(feature = "lang-python")]
    Python,
    #[cfg(feature = "lang-javascript")]
    Javascript,
    #[cfg(feature = "lang-typescript")]
    Typescript,
    #[cfg(feature = "lang-bash")]
    Bash,
    #[cfg(feature = "lang-go")]
    Go,
    #[cfg(feature = "lang-json")]
    Json,
    #[cfg(feature = "lang-yaml")]
    Yaml,
    #[cfg(feature = "lang-toml")]
    Toml,
    #[cfg(feature = "lang-html")]
    Html,
    #[cfg(feature = "lang-css")]
    Css,
}

/// Return the grammar bundle for a feature-gated built-in [`Language`].
///
/// Rust and Python are wired to their upstream grammar crates; the remaining
/// arms panic with a TODO until a host wires in the real `tree-sitter-<lang>`
/// crate (see the module docs for the procedure). When no `lang-*` feature is
/// enabled, [`Language`] is uninhabited and this function cannot be called.
pub fn bundle(language: Language) -> TsLanguage {
    match language {
        #[cfg(feature = "lang-rust")]
        Language::Rust => TsLanguage {
            language: tree_sitter_rust::LANGUAGE.into(),
            highlights_query: tree_sitter_rust::HIGHLIGHTS_QUERY.to_string(),
            injections_query: Some(tree_sitter_rust::INJECTIONS_QUERY.to_string()),
            indent_query: None,
        },
        #[cfg(feature = "lang-python")]
        Language::Python => TsLanguage {
            language: tree_sitter_python::LANGUAGE.into(),
            highlights_query: tree_sitter_python::HIGHLIGHTS_QUERY.to_string(),
            injections_query: None, // upstream grammar ships no injections.scm
            indent_query: None,
        },
        #[cfg(feature = "lang-javascript")]
        Language::Javascript => panic!("editor-ts: `javascript` bundle stub; wire `tree-sitter-javascript` in languages.rs"),
        #[cfg(feature = "lang-typescript")]
        Language::Typescript => panic!("editor-ts: `typescript` bundle stub; wire `tree-sitter-typescript` in languages.rs"),
        #[cfg(feature = "lang-bash")]
        Language::Bash => panic!("editor-ts: `bash` bundle stub; wire `tree-sitter-bash` in languages.rs"),
        #[cfg(feature = "lang-go")]
        Language::Go => panic!("editor-ts: `go` bundle stub; wire `tree-sitter-go` in languages.rs"),
        #[cfg(feature = "lang-json")]
        Language::Json => panic!("editor-ts: `json` bundle stub; wire `tree-sitter-json` in languages.rs"),
        #[cfg(feature = "lang-yaml")]
        Language::Yaml => panic!("editor-ts: `yaml` bundle stub; wire `tree-sitter-yaml` in languages.rs"),
        #[cfg(feature = "lang-toml")]
        Language::Toml => panic!("editor-ts: `toml` bundle stub; wire `tree-sitter-toml` in languages.rs"),
        #[cfg(feature = "lang-html")]
        Language::Html => panic!("editor-ts: `html` bundle stub; wire `tree-sitter-html` in languages.rs"),
        #[cfg(feature = "lang-css")]
        Language::Css => panic!("editor-ts: `css` bundle stub; wire `tree-sitter-css` in languages.rs"),
    }
}
