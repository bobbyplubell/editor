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

/// The byte range of a line's trailing ` ^blockid` marker — the whitespace
/// separator and the `^id` token together — when `line` (one logical line,
/// no trailing newline) carries a well-formed block marker, else `None`.
///
/// A marker is the final token on the line, separated from preceding text by
/// whitespace, with id charset `[A-Za-z0-9-]` (`Some prose. ^abc123`). A bare
/// `^id` line (no preceding text), an `^id` glued to a word (`word^id`), or an
/// incidental `^` mid-line (`2^10`, `a ^ b`) is NOT a marker. This is the exact
/// predicate the read side uses (`core::wikilink::line_block_id` /
/// `block_anchor_id`); the two MUST agree, so the rule is mirrored here rather
/// than depended on across the editor's separate workspace (the same posture
/// `is_external_link_dest` takes toward `core::url::classify`).
///
/// The returned range starts at the whitespace before the `^` and ends at the
/// line's end, so concealing it also swallows the separating space — the
/// rendered prose reads `Some prose.` with no dangling whitespace.
///
/// status: wikilink-block-marker-conceal
#[must_use]
pub fn trailing_block_marker(line: &str) -> Option<std::ops::Range<usize>> {
    // The marker token is the last whitespace-separated token; everything
    // before the final whitespace run must be non-empty (a bare `^id` line is
    // not a marker — it tags nothing).
    let trimmed_end = line.trim_end();
    let ws_then_token = trimmed_end.rfind(char::is_whitespace)?;
    let token = &trimmed_end[ws_then_token..].trim_start();
    let head = &trimmed_end[..ws_then_token];
    if head.trim().is_empty() {
        return None;
    }
    let id = token.strip_prefix('^')?;
    if id.is_empty() || !id.bytes().all(is_block_id_byte) {
        return None;
    }
    // Conceal from the whitespace before the `^` to the line's logical end.
    Some(ws_then_token..line.len())
}

/// True for a byte allowed in a block id (`[A-Za-z0-9-]`). Mirrors
/// `core::wikilink::is_block_id_byte`; see `trailing_block_marker`.
const fn is_block_id_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'-'
}

/// Emit a `Replace` decoration concealing the trailing ` ^blockid` marker on
/// every off-cursor line that carries one. The marker is an explicit handle for
/// block-anchor links (`wikilink-block-anchors`), not prose, so it reads as
/// noise once authored; hiding it off the cursor line and revealing the raw
/// marker on the cursor line is the same live-preview reveal every other
/// markup uses. Fenced code blocks are skipped so a `^id` token inside a ```
/// fence is never concealed — matching the read side's `find_block_byte`.
///
/// status: wikilink-block-marker-conceal
fn block_marker_decorations(
    entries: &mut Vec<(std::ops::Range<usize>, Decoration)>,
    text: &str,
    line_of: &impl Fn(usize) -> usize,
    cursor_line: usize,
) {
    let mut offset = 0usize;
    let mut in_fence = false;
    for raw in text.split_inclusive('\n') {
        let line = raw.strip_suffix('\n').unwrap_or(raw);
        let trimmed = line.trim_start().trim_end();
        if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
            in_fence = !in_fence;
        } else if !in_fence
            && let Some(marker) = trailing_block_marker(line)
        {
            let start = offset + marker.start;
            let end = offset + marker.end;
            // Reveal (no conceal) when the cursor is on the marker's line.
            if line_of(start) != cursor_line {
                entries.push((start..end, Decoration::Replace { display: None }));
            }
        }
        offset += raw.len();
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
            let inner_start = i + 2;
            // A `code:`-namespaced body may carry nested `[`…`]` groups and
            // backtick spans (impl monikers: `impl#[`Builder<'a>`]method`), so
            // it gets the depth-aware close matcher. Everything else keeps the
            // flat rule below, byte-for-byte: a stray `]` still rejects.
            // status: wikilink-code-nested-brackets
            let close_start = if text[inner_start..].starts_with("code:") {
                match code_body_close(&text, inner_start) {
                    Some(close_start) => close_start,
                    None => {
                        i += 1;
                        continue;
                    }
                }
            } else {
                // Find closing `]]` on the same logical span (no newlines inside).
                let mut j = inner_start;
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
                let inner = &text[inner_start..close_start];
                if inner.is_empty() || inner.contains(']') {
                    i = close_start + 2;
                    continue;
                }
                close_start
            };
            let inner_end = close_start;
            let full_end = close_start + 2;
            let inner = &text[inner_start..inner_end];

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

    // Conceal trailing ` ^blockid` block markers off the cursor line. Runs over
    // the whole document (not viewport-scoped) so a marker scrolled to the edge
    // still hides; the work is one line scan, cheap next to the link pass above.
    // status: wikilink-block-marker-conceal
    block_marker_decorations(&mut entries, &text, &line_of, cursor_line);

    RangeSet::from_iter(entries)
}

/// Byte offset of the closing `]]` of a `code:`-namespaced wikilink body starting at `from`
/// (the byte after `[[`), or `None` when the body never closes on its line. Unlike the flat
/// rule in `wikilink_decorations`, a code body may carry nested `[`…`]` groups and backtick
/// spans — the canonical short-sym moniker form qualifies impl methods as
/// `impl#[`Builder<'a>`]method` — so the matcher tracks bracket depth, treats backtick spans
/// as opaque, and closes on the first `]]` at depth zero outside backticks. A stray `]` at
/// depth zero that isn't the closer is malformed (no parse), matching the flat rule's
/// strictness for everything non-nested. This is the exact matcher the app-side parser uses
/// (`core::wikilink::code_body_close`); the two scanners MUST agree on what is a link, so the
/// rule is mirrored here rather than depended on across the editor's separate workspace (the
/// same posture `trailing_block_marker` takes toward `core::wikilink::line_block_id`).
/// status: wikilink-code-nested-brackets
fn code_body_close(text: &str, from: usize) -> Option<usize> {
    let bytes = text.as_bytes();
    let mut depth = 0usize;
    let mut in_backtick = false;
    let mut j = from;
    while j < bytes.len() {
        match bytes[j] {
            b'\n' => return None,
            b'`' => in_backtick = !in_backtick,
            b'[' if !in_backtick => depth += 1,
            b']' if !in_backtick => {
                if depth > 0 {
                    depth -= 1;
                } else if bytes.get(j + 1) == Some(&b']') {
                    return Some(j);
                } else {
                    return None; // stray `]` at depth zero — malformed body
                }
            }
            _ => {}
        }
        j += 1;
    }
    None
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

    #[test]
    fn block_anchor_wikilink_emits_a_pill() {
        // A `#^block` anchor rides the same pill path as a heading anchor; the
        // anchor split + block resolution happen in the nav layer.
        for src in ["x\n[[Other#^abc123]]\n", "x\n[[#^abc123]]\n"] {
            let ids = pill_targets(src);
            assert_eq!(ids.len(), 1, "one pill for {src:?}");
            assert_ne!(ids[0] & WIKILINK_WIDGET_TAG, 0);
        }
    }

    #[test]
    fn block_anchor_markdown_link_emits_a_pill() {
        let src = "x\n[Doc](other#^abc123)\n";
        let ids = pill_targets(src);
        assert_eq!(ids.len(), 1, "vault md link with a block anchor is a pill");
        assert_ne!(ids[0] & WIKILINK_WIDGET_TAG, 0);
    }

    /// Pill spans produced with the caret at offset 0, so off-cursor links
    /// collapse; the resolver maps every target to a title (resolved).
    fn pill_spans(src: &str) -> Vec<std::ops::Range<usize>> {
        let state = EditorState::new(src);
        let resolve = |_: &str| Some("Title".to_string());
        let set = wikilink_decorations(&state, None, None, Some(&resolve));
        set.iter_all()
            .filter_map(|(r, d)| match d {
                Decoration::InlineWidget { .. } => Some(r),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn code_wikilinks_with_nested_brackets_become_pills() {
        // The four standing impl-qualified bodies from
        // bug-editor-code-pill-scanner-flat (clustering.md): nested `[`/`]`
        // plus backtick spans inside `[[code:…]]`. Mirrors
        // `core::wikilink::parses_code_links_with_nested_brackets_and_backticks`.
        let bodies = [
            "code:hiker/cluster/build/impl#[`Builder<'a>`]top_level_split",
            "code:hiker/cluster/build/impl#[`Builder<'a>`]build_top_level_nodes",
            "code:hiker/cluster/build/impl#[`Builder<'a>`]split_branch_ctx",
            "code:hiker/cluster/build/impl#[`SplitBranchCtx<'a>`]split_top_level_groups",
        ];
        for body in bodies {
            let src = format!("x\nimplements:: [[{body}]], [[code:hiker/x/y]]\n");
            let spans = pill_spans(&src);
            assert_eq!(spans.len(), 2, "{body}");
            assert_eq!(&src[spans[0].clone()], format!("[[{body}]]"));
            assert_eq!(
                &src[spans[1].clone()],
                "[[code:hiker/x/y]]",
                "scan resumes after the close",
            );
        }
        // Bare bracket group (no backticks) and a two-group (self type + trait) qualifier.
        let src = "x\n[[code:hiker/tab/impl#[TabKind]git_diff_preview]] and \
                   [[code:hiker/canvas_activity/impl#[CanvasActivity][`Activity<dyn AppCtx + 'static>`]id]]\n";
        let spans = pill_spans(src);
        assert_eq!(spans.len(), 2);
        assert_eq!(&src[spans[0].clone()], "[[code:hiker/tab/impl#[TabKind]git_diff_preview]]");
        assert_eq!(
            &src[spans[1].clone()],
            "[[code:hiker/canvas_activity/impl#[CanvasActivity][`Activity<dyn AppCtx + 'static>`]id]]",
        );
    }

    #[test]
    fn malformed_code_bodies_emit_no_pill() {
        // Unterminated / multi-line / unclosed-backtick / stray-`]` code bodies
        // stay rejected. Mirrors `core::wikilink::malformed_code_bodies_do_not_parse`.
        for src in [
            "x\n[[code:hiker/impl#[`X`]m\n",     // no close
            "x\n[[code:hiker/impl#[`X\n`]m]]\n", // newline inside
            "x\n[[code:hiker/a/[b]]\n",          // unclosed bracket eats the close
            "x\n[[code:hiker/a`b]]\n",           // unclosed backtick swallows the close
            "x\n[[code:hiker/a]b]]\n",           // stray `]` at depth zero
        ] {
            assert!(pill_spans(src).is_empty(), "{src:?}");
        }
    }

    #[test]
    fn ordinary_body_with_stray_bracket_still_rejects() {
        // The flat rule for non-`code:` bodies is unchanged byte-for-byte.
        assert!(pill_spans("x\n[[a]b]]\n").is_empty());
    }

    #[test]
    fn nested_code_body_reaches_the_resolver_verbatim() {
        // The pill's label comes from the host's title resolver, keyed by the
        // verbatim body — the nested form must arrive intact so the host can
        // supply the pretty label (`Builder::top_level_split`).
        let src = "x\n[[code:hiker/cluster/build/impl#[`Builder<'a>`]top_level_split]]\n";
        let state = EditorState::new(src);
        let seen = std::cell::RefCell::new(Vec::new());
        let resolve = |t: &str| {
            seen.borrow_mut().push(t.to_string());
            Some("Builder::top_level_split".to_string())
        };
        let set = wikilink_decorations(&state, None, None, Some(&resolve));
        let labels: Vec<String> = set
            .iter_all()
            .filter_map(|(_, d)| match d {
                Decoration::InlineWidget { widget, .. } => {
                    widget.display().map(|disp| disp.text.to_string())
                }
                _ => None,
            })
            .collect();
        assert_eq!(labels, vec!["Builder::top_level_split"]);
        assert_eq!(
            seen.borrow().as_slice(),
            ["code:hiker/cluster/build/impl#[`Builder<'a>`]top_level_split"],
        );
    }

    #[test]
    fn trailing_block_marker_classifies_real_markers() {
        // A real marker: whitespace-preceded `^id` at the end of a non-empty
        // line. The concealed range starts at the separating space.
        let line = "Some paragraph text. ^abc123";
        let r = trailing_block_marker(line).expect("real marker");
        assert_eq!(&line[r.clone()], " ^abc123");
        // Hyphenated id is valid.
        let line = "item ^a-b-c";
        assert_eq!(trailing_block_marker(line).map(|r| &line[r]), Some(" ^a-b-c"));
    }

    #[test]
    fn trailing_block_marker_rejects_incidental_carets() {
        // Math / mid-line carets are NOT markers.
        assert_eq!(trailing_block_marker("2^10"), None);
        assert_eq!(trailing_block_marker("a ^ b"), None);
        assert_eq!(trailing_block_marker("x ^id more words"), None);
        // A caret glued to a word (no preceding whitespace) is not a marker.
        assert_eq!(trailing_block_marker("word^abc"), None);
        // A bare `^id` line (nothing before it) tags no block.
        assert_eq!(trailing_block_marker("^abc"), None);
        assert_eq!(trailing_block_marker("  ^abc"), None);
        // Out-of-charset id (underscore) is malformed.
        assert_eq!(trailing_block_marker("note ^under_score"), None);
        // A bare caret is malformed.
        assert_eq!(trailing_block_marker("note ^"), None);
    }

    /// Concealed-marker spans produced with the caret at offset 0, so the marker
    /// line is off-cursor and the marker hides.
    fn concealed_spans(src: &str) -> Vec<std::ops::Range<usize>> {
        let state = EditorState::new(src);
        let set = wikilink_decorations(&state, None, None, None);
        set.iter_all()
            .filter_map(|(r, d)| match d {
                Decoration::Replace { display: None } => Some(r),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn block_marker_concealed_off_cursor_line() {
        // Caret at 0 (line 0); the marker on line 2 conceals.
        let src = "intro\n\nA tagged paragraph. ^abc123\n";
        let spans = concealed_spans(src);
        let marker = src.find(" ^abc123").unwrap();
        assert!(
            spans.iter().any(|r| r.start == marker && r.end == marker + " ^abc123".len()),
            "off-cursor marker concealed, got {spans:?}",
        );
    }

    #[test]
    fn block_marker_revealed_on_cursor_line() {
        // Place the caret on the marker's line; nothing conceals there.
        let src = "A tagged paragraph. ^abc123\nmore\n";
        let mut state = EditorState::new(src);
        let on_line = src.find("tagged").unwrap();
        state.selection = editor_core::selection::Selection::single(on_line);
        let set = wikilink_decorations(&state, None, None, None);
        let any_conceal = set
            .iter_all()
            .any(|(_, d)| matches!(d, Decoration::Replace { display: None }));
        assert!(!any_conceal, "marker on the cursor line reveals (no conceal)");
    }

    #[test]
    fn block_marker_in_fence_not_concealed() {
        // A `^id` token inside a fenced code block is never a marker.
        let src = "before\n```\ncode line ^infence\n```\n";
        assert!(concealed_spans(src).is_empty(), "fenced `^id` is not concealed");
    }

    #[test]
    fn incidental_caret_not_concealed() {
        // `2^10` in prose must never conceal.
        let src = "the value 2^10 is large\nmore\n";
        assert!(concealed_spans(src).is_empty(), "math caret not concealed");
    }
}
