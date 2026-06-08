//! Wikilink decorations: `[[Target]]`.
//!
//! When the cursor is off the link's line the whole `[[…]]` span collapses to
//! a styled link pill; with the cursor on the line the raw markdown stays
//! visible for editing (the standard live-preview reveal).
//!
//! Under the path-form (`wikilink-path-form`) the body is the target verbatim
//! — a bare basename (`Name`) or a workspace-relative path without the `.md`
//! extension (`folder/sub/Name`). There is no `|display` alias half.
//!
//! A host may pass a `resolve` closure mapping a link target to the note's
//! *current* title (its basename, or frontmatter `title` once that's wired).
//! When present, links render as clickable [`WikilinkWidget`] pills carrying
//! the live title — a click emits a `WidgetClick` tagged with
//! [`WIKILINK_WIDGET_TAG`] so the host can resolve the target and open it.
//! A target the resolver can't place renders in a distinct unresolved style.
//! Without a resolver (read-only previews, the standalone example) links
//! fall back to a plain non-clickable `Replace`.

use editor_core::decoration::Color;
use editor_core::decoration::Decoration;
use editor_core::decoration::InlineWidget;
use editor_core::decoration::InlineWidgetDisplay;
use editor_core::decoration::MarkStyle;
use editor_core::decoration::Set as DecorationSet;
use editor_core::rangeset::RangeSet;
use editor_core::state::Editor as EditorState;
use editor_core::theme::Theme;
use smol_str::SmolStr;
use std::sync::Arc;

/// Wikilink color — blue-ish, distinct from the markdown link color.
pub const COLOR_WIKILINK: Color = Color::rgb(86, 156, 214);
/// Unresolved-link color — a muted red, so a dangling ULID or unmatched name
/// reads as broken rather than as a normal link. [wikilink-unresolved]
pub const COLOR_WIKILINK_UNRESOLVED: Color = Color::rgb(224, 108, 117);

/// Tag bit OR-ed into a wikilink widget's `widget_id`. The low bits carry the
/// link's full-span start byte so the host can re-parse the link at that
/// offset on click. The tag lets the buffer panel tell wikilink `WidgetClick`s
/// apart from other inline-widget click consumers (patch-review, diff hunks).
pub const WIKILINK_WIDGET_TAG: u64 = 1 << 62;

/// Resolver closure: target (a workspace path or bare basename) → the note's
/// current display title, or `None` when the target can't be resolved.
pub type TitleResolver<'a> = dyn Fn(&str) -> Option<String> + 'a;

/// Clickable inline pill for a resolved/unresolved wikilink. Renders as
/// textual inline content (`display()` is `Some`) and reports clicks so the
/// host can open the target. status: wikilink-render-live-title
struct WikilinkWidget {
    text: SmolStr,
    fg: Color,
    bg: Option<Color>,
    id: u64,
}

impl InlineWidget for WikilinkWidget {
    fn measure(&self, _font_size: f32) -> (f32, f32) {
        // Width comes from the rendered title galley (the layout takes the max
        // of this and the galley width); 0 lets the text drive the advance.
        (0.0, 0.0)
    }
    fn handles_click(&self) -> bool {
        true
    }
    fn widget_id(&self) -> u64 {
        self.id
    }
    fn display(&self) -> Option<InlineWidgetDisplay> {
        Some(InlineWidgetDisplay {
            text: self.text.clone(),
            bg: self.bg,
            fg: Some(self.fg),
            strikethrough: false,
        })
    }
}

/// True when a markdown-link destination leaves the vault — `http(s)://`,
/// `mailto:`, or a `zim://` archive reference. Such links keep the standard
/// markdown decoration (styled label, OS-open handled elsewhere) and are NOT
/// turned into clickable note pills here. Vault-shaped destinations (a bare
/// name, a relative path, or one with a `#section` anchor) fall through and
/// become clickable pills resolved against the index. Mirrors the precedence
/// in `core::url::classify` but stays a pure local check so `editor-md` keeps
/// no dependency on `hiker-core`. status: markdown-link-vault-nav
#[must_use]
pub fn is_external_link_dest(dest: &str) -> bool {
    let lower = dest.trim().to_ascii_lowercase();
    lower.starts_with("http://")
        || lower.starts_with("https://")
        || lower.starts_with("mailto:")
        || lower.starts_with("zim://")
}

pub fn wikilink_decorations(
    state: &EditorState,
    theme: Option<&Theme>,
    viewport: Option<&std::ops::Range<usize>>,
    resolve: Option<&TitleResolver<'_>>,
) -> DecorationSet {
    let link_color = theme.map(|t| t.markdown.link).unwrap_or(COLOR_WIKILINK);
    let text = state.doc.to_string();
    let doc_len = text.len();
    let cursor = state.selection.main().head.offset();
    let cursor_line = state.doc.byte_to_line(cursor.min(doc_len));
    let line_of = |b: usize| state.doc.byte_to_line(b.min(doc_len));

    let mut entries: Vec<(std::ops::Range<usize>, Decoration)> = Vec::new();

    let bytes = text.as_bytes();
    let (scan_start, scan_end) = match viewport {
        Some(vp) => (vp.start.min(bytes.len()), vp.end.min(bytes.len())),
        None => (0, bytes.len()),
    };
    let mut i = scan_start;
    while i + 1 < scan_end {
        // Markdown link `[text](dest)` whose dest is a vault target becomes a
        // clickable note pill, sharing the wikilink click path. An external
        // dest is left to the standard markdown decoration. A `[[` opens the
        // wikilink branch below, so require the next byte to NOT be `[`.
        if bytes[i] == b'['
            && bytes[i + 1] != b'['
            && (i == 0 || bytes[i - 1] != b'[')
            && let Some(md) = parse_md_link(&text, i)
            && !is_external_link_dest(&md.dest)
        {
            let span_line_start = line_of(i);
            let span_line_end = line_of(md.full_end.saturating_sub(1).max(i));
            let on_cursor = cursor_line >= span_line_start && cursor_line <= span_line_end;
            if !on_cursor {
                emit_md_link_pill(&mut entries, &md, i, link_color, resolve);
            }
            i = md.full_end;
            continue;
        }
        if bytes[i] == b'[' && bytes[i + 1] == b'[' {
            // Find closing `]]` on the same logical span (no newlines inside).
            let mut j = i + 2;
            let mut closed = None;
            while j + 1 < bytes.len() {
                if bytes[j] == b'\n' {
                    break;
                }
                if bytes[j] == b']' && bytes[j + 1] == b']' {
                    closed = Some(j);
                    break;
                }
                j += 1;
            }
            let Some(close_start) = closed else {
                i += 1;
                continue;
            };
            let inner_start = i + 2;
            let inner_end = close_start;
            let full_end = close_start + 2;
            let inner = &text[inner_start..inner_end];
            if inner.is_empty() || inner.contains(']') {
                i = full_end;
                continue;
            }

            // Path-form (`wikilink-path-form`): the entire body is the target;
            // no `|alias` half. The display falls back to the target verbatim
            // when no resolver provides a title.
            let target = inner.trim();
            let display_text: &str = target;
            // Range of the styled text inside the brackets — used for the
            // alias Mark below; under path-form it's the whole body.
            let alias_range_in_inner: std::ops::Range<usize> = 0..inner.len();

            let span_line_start = line_of(i);
            let span_line_end = line_of(full_end.saturating_sub(1).max(i));
            let on_cursor =
                cursor_line >= span_line_start && cursor_line <= span_line_end;

            if !on_cursor {
                match resolve {
                    Some(resolve) => {
                        // Live-title: prefer the resolver's current title, then
                        // the stored display, then the raw target. A `None`
                        // result renders unresolved. [wikilink-render-live-title]
                        let resolved = resolve(target);
                        let (label, fg, bg) = match &resolved {
                            Some(title) => (
                                title.as_str(),
                                link_color,
                                Some(pill_bg(link_color)),
                            ),
                            None => (display_text, COLOR_WIKILINK_UNRESOLVED, None),
                        };
                        let label = if label.is_empty() { target } else { label };
                        entries.push((
                            i..full_end,
                            Decoration::InlineWidget {
                                widget: Arc::new(WikilinkWidget {
                                    text: SmolStr::from(label),
                                    fg,
                                    bg,
                                    id: WIKILINK_WIDGET_TAG | i as u64,
                                }),
                                atomic: true,
                            },
                        ));
                    }
                    None => {
                        entries.push((
                            i..full_end,
                            Decoration::Replace {
                                display: Some(SmolStr::from(display_text)),
                            },
                        ));
                    }
                }
            }

            // Always emit a Mark on the alias-or-target text inside the span,
            // so when revealed (cursor on line) the displayed text is styled.
            let alias_byte_range = (inner_start + alias_range_in_inner.start)
                ..(inner_start + alias_range_in_inner.end);
            entries.push((
                alias_byte_range,
                Decoration::Mark(MarkStyle {
                    fg: Some(link_color),
                    underline: true,
                    ..MarkStyle::default()
                }),
            ));

            i = full_end;
            continue;
        }
        i += 1;
    }

    RangeSet::from_iter(entries)
}

/// Faint pill background tinted from the link color (low alpha).
const fn pill_bg(c: Color) -> Color {
    Color::rgba(c.r, c.g, c.b, 36)
}

/// A parsed `[label](dest)` markdown link on a single line. Byte offsets are
/// absolute in the document; `dest` is the destination text (a vault path or
/// name, possibly with a `#section` anchor) verbatim.
struct MdLink {
    /// Display text between the `[` and `]`.
    label: String,
    /// Destination between the `(` and `)`.
    dest: String,
    /// Byte offset just past the closing `)`.
    full_end: usize,
}

/// Parse a `[label](dest)` markdown link whose opening `[` is at byte `start`.
/// Returns `None` when the bytes there are not a well-formed inline link on one
/// line (no nested `]`/`)`, no newline inside, label and dest both present).
fn parse_md_link(text: &str, start: usize) -> Option<MdLink> {
    let rest = &text[start..];
    let bytes = rest.as_bytes();
    // `[` … `]` label (no newline, no nested `]`).
    if bytes.first() != Some(&b'[') {
        return None;
    }
    let mut k = 1;
    while k < bytes.len() && bytes[k] != b']' && bytes[k] != b'\n' {
        k += 1;
    }
    if k >= bytes.len() || bytes[k] != b']' {
        return None;
    }
    let label = &rest[1..k];
    // Immediately followed by `(` … `)` dest.
    if bytes.get(k + 1) != Some(&b'(') {
        return None;
    }
    let dest_start = k + 2;
    let mut m = dest_start;
    while m < bytes.len() && bytes[m] != b')' && bytes[m] != b'\n' {
        m += 1;
    }
    if m >= bytes.len() || bytes[m] != b')' {
        return None;
    }
    let dest = &rest[dest_start..m];
    if dest.trim().is_empty() {
        return None;
    }
    Some(MdLink {
        label: label.to_string(),
        dest: dest.to_string(),
        full_end: start + m + 1,
    })
}

/// Emit a clickable pill replacing a vault-target markdown link's whole span.
/// The pill's label is the link's own `[label]` text (markdown links carry
/// their display text directly, unlike wikilinks which resolve a title).
/// The click id is `WIKILINK_WIDGET_TAG | start`, so the shared wikilink click
/// handler re-parses the link at `start` and resolves the dest.
/// status: markdown-link-vault-nav
fn emit_md_link_pill(
    entries: &mut Vec<(std::ops::Range<usize>, Decoration)>,
    md: &MdLink,
    start: usize,
    link_color: Color,
    resolve: Option<&TitleResolver<'_>>,
) {
    let label = md.label.trim();
    let label = if label.is_empty() { md.dest.trim() } else { label };
    // Resolution drives only the color: a dest the index can't place renders in
    // the unresolved style, matching wikilink behavior. The page part (before
    // any `#section`) is what the resolver checks.
    let page = md.dest.split('#').next().unwrap_or(&md.dest).trim();
    let resolved = resolve.is_none_or(|r| r(page).is_some());
    let (fg, bg) = if resolved {
        (link_color, Some(pill_bg(link_color)))
    } else {
        (COLOR_WIKILINK_UNRESOLVED, None)
    };
    entries.push((
        start..md.full_end,
        Decoration::InlineWidget {
            widget: Arc::new(WikilinkWidget {
                text: SmolStr::from(label),
                fg,
                bg,
                id: WIKILINK_WIDGET_TAG | start as u64,
            }),
            atomic: true,
        },
    ));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn external_dests_are_recognized() {
        assert!(is_external_link_dest("http://example.com"));
        assert!(is_external_link_dest("HTTPS://Example.com"));
        assert!(is_external_link_dest("mailto:a@b.c"));
        assert!(is_external_link_dest("zim://zim/C/Foo"));
        assert!(!is_external_link_dest("Note"));
        assert!(!is_external_link_dest("folder/Note.md"));
        assert!(!is_external_link_dest("Note#Heading"));
        assert!(!is_external_link_dest("#Section"));
    }

    #[test]
    fn parses_inline_markdown_link() {
        let text = "see [Doc](folder/Doc#Heading) end";
        let start = text.find('[').unwrap();
        let md = parse_md_link(text, start).expect("link parses");
        assert_eq!(md.label, "Doc");
        assert_eq!(md.dest, "folder/Doc#Heading");
        assert_eq!(&text[start..md.full_end], "[Doc](folder/Doc#Heading)");
    }

    #[test]
    fn rejects_malformed_markdown_link() {
        assert!(parse_md_link("[Doc] (x)", 0).is_none());
        assert!(parse_md_link("[Doc]\n(x)", 0).is_none());
        assert!(parse_md_link("[Doc]()", 0).is_none());
        assert!(parse_md_link("[Doc](no\nclose", 0).is_none());
    }

    /// Decorations produced with the caret at offset 0, so off-cursor links
    /// collapse to pills. The resolver maps every page to a title (resolved).
    fn pill_targets(src: &str) -> Vec<u64> {
        let state = EditorState::new(src);
        let resolve = |_: &str| Some("Title".to_string());
        let set = wikilink_decorations(&state, None, None, Some(&resolve));
        set.iter_all()
            .filter_map(|(_, d)| match d {
                Decoration::InlineWidget { widget, .. } => Some(widget.widget_id()),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn vault_markdown_link_becomes_clickable_pill() {
        // Cursor at 0; the link is on its own (later) line so it's off-cursor.
        let src = "x\n[Doc](folder/Doc#Heading)\n";
        let ids = pill_targets(src);
        assert_eq!(ids.len(), 1, "one pill emitted for the vault link");
        assert_ne!(ids[0] & WIKILINK_WIDGET_TAG, 0, "tagged as a wikilink-bucket click");
    }

    #[test]
    fn external_markdown_link_is_not_a_pill() {
        let src = "x\n[Site](https://example.com)\n";
        assert!(pill_targets(src).is_empty(), "external link stays a plain markdown link");
    }

    #[test]
    fn wikilink_still_emits_its_pill() {
        let src = "x\n[[Other#Heading]]\n";
        let ids = pill_targets(src);
        assert_eq!(ids.len(), 1);
        assert_ne!(ids[0] & WIKILINK_WIDGET_TAG, 0);
    }
}
