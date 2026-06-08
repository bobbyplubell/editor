//! Decorations: styling and replacement applied to ranges of the document.
//!
//! Decorations are *not* part of the document. They're produced by extensions
//! (markdown live preview, syntax highlighting, diff hunks, occurrence
//! highlight) and consumed by the view layer at paint time.

use std::fmt;
use std::sync::Arc;

use smol_str::SmolStr;

use crate::rangeset::{HeightAffecting, RangeSet};

/// Severity of a [`Diagnostic`], ordered from most to least critical.
///
/// Drives the decoration provider's underline color and gutter marker for a
/// diagnostic; see [`GutterMarker::Diagnostic`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum Severity {
    Error,
    Warning,
    Info,
    Hint,
}

/// A structured lint/error report attached to a byte range of the document.
///
/// Diagnostics are produced by hosts (LSP, tree-sitter queries, custom
/// linters) and rendered by the diagnostic decoration provider as
/// wavy-underlined marks plus per-line gutter markers. See SPEC §9.7 and
/// IMPLEMENTATION §16.5.1.
#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Diagnostic {
    /// Byte range in the document this diagnostic applies to.
    pub range: std::ops::Range<usize>,
    pub severity: Severity,
    pub message: SmolStr,
    /// Producer identifier, e.g. "rustc", "clippy", "tree-sitter".
    pub source: SmolStr,
    /// Optional machine-readable code, e.g. "E0308".
    pub code: Option<SmolStr>,
}

/// A borrowed, tightly-packed RGBA8 pixel buffer supplied by a pixel widget.
///
/// `rgba` is tightly packed (no row padding) and `len() == width * height * 4`.
/// Pixels are already scaled to **physical** px: the widget bakes the device
/// pixel ratio (DPR) in, so the egui adapter blits 1:1 into the reserved rect
/// without rescaling.
///
/// This type is deliberately egui-free — `editor-core` never names a GPU
/// texture. The egui adapter (`editor-egui`) is the only layer that uploads
/// these bytes; see slug `widget-painter-texture-blit`.
pub struct WidgetPixels<'a> {
    pub rgba: &'a [u8],
    pub width: u32,
    pub height: u32,
}

/// Data-side trait for inline widgets embedded in a line of text.
///
/// Widgets are introduced as decorations via [`Decoration::InlineWidget`]. The
/// painter measures the widget at the current font size and reserves an
/// equivalently-sized region in the line layout. A widget supplies its visual
/// either as text (via [`display`](InlineWidget::display)) or as raw pixels
/// (via [`pixels`](InlineWidget::pixels)); a widget that supplies neither
/// renders as a bordered placeholder rect.
pub trait InlineWidget: Send + Sync {
    /// Pixel size when laid out at the given font size.
    fn measure(&self, font_size: f32) -> (f32, f32);
    /// True if clicks land on the widget; default false = passthrough to cursor.
    fn handles_click(&self) -> bool {
        false
    }
    /// Stable identity for diffing; defaults to 0.
    ///
    /// This is also the texture-cache key (slug `widget-painter-texture-blit`).
    /// A widget that supplies [`pixels`](InlineWidget::pixels) MUST return a
    /// stable, content-derived id (a hash of source + style + size + dpr +
    /// theme) so the texture cache invalidates whenever the rendered bytes —
    /// including the widget's size — change.
    fn widget_id(&self) -> u64 {
        0
    }
    /// When `Some`, the inline-widget painter renders the returned text
    /// with the supplied colors instead of the bordered "widget"
    /// placeholder. Used by hosts that want a small textual insertion
    /// at a byte position (patch-review intraline `new_str` rendering,
    /// inline diagnostics, etc.) without introducing a new decoration
    /// variant. Default `None` keeps the placeholder behavior for
    /// non-textual widgets.
    fn display(&self) -> Option<InlineWidgetDisplay> {
        None
    }
    /// When `Some`, the inline-widget painter uploads the returned RGBA8 buffer
    /// to a texture, caches it by [`widget_id`](InlineWidget::widget_id), and
    /// blits it into the reserved rect (slug `widget-painter-texture-blit`).
    /// [`display`](InlineWidget::display) takes precedence: a textual widget
    /// renders as text and never reaches this pixel path. Default `None` keeps
    /// the placeholder behavior for widgets without pixels.
    fn pixels(&self) -> Option<WidgetPixels<'_>> {
        None
    }
    /// Distance in physical px from the top of the widget box down to the text
    /// baseline. Used to vertically align an inline pixel widget (e.g. inline
    /// math) on the surrounding text's baseline rather than centering it.
    /// `None` centers the widget in its reserved rect.
    fn baseline(&self) -> Option<f32> {
        None
    }
}

#[derive(Clone, Debug)]
pub struct InlineWidgetDisplay {
    pub text: SmolStr,
    pub bg: Option<Color>,
    pub fg: Option<Color>,
    pub strikethrough: bool,
}

/// A clickable sub-region of a block widget, expressed in NORMALIZED widget
/// coordinates: `x`/`y`/`w`/`h` are fractions in `0.0..1.0` of the widget's
/// painted box. The painter maps them through the SAME aspect-preserving
/// letterbox transform it uses to blit the widget's texture, so a region lines
/// up exactly with what's drawn regardless of scaling or device pixel ratio.
///
/// `id` is host-defined and surfaces back as
/// [`ClickAction::WidgetClick`](../../editor_view/viewport/enum.ClickAction.html)'s
/// payload when the region is clicked, so the host can map the click to an
/// action (e.g. the node id of a rendered diagram). Deliberately egui-free —
/// plain `f32`s only, keeping `editor-core` free of any GPU/UI types.
#[derive(Clone, Copy, Debug, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct WidgetClickRegion {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
    pub id: u64,
}

/// Horizontal alignment of a positioned text run in a [`BlockPaint::Text`].
///
/// `x` in the run is the anchor: `Left` draws the text starting at `x`,
/// `Center` centers it on `x`, `Right` ends it at `x`. The painter resolves the
/// run's pixel width itself (it owns the font), so the data side only states
/// intent. Used by the table widget to honor per-column alignment.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum TextAlign {
    Left,
    Center,
    Right,
}

/// One styled inline run within a [`BlockPaint::RichText`]: a contiguous slice
/// of text plus the per-run style flags the painter turns into an egui
/// `TextFormat` section. The text carries no markup — the inline markdown
/// markers (`**`, `*`, `` ` ``, `~~`) are already stripped by the producer, so
/// the painter renders exactly `text` with the requested style.
///
/// Deliberately egui-free (plain fields + [`Color`]): `editor-core` never names
/// a font or `TextFormat`. The host (`editor-egui`) maps each run to a format
/// section of a single wrapping `LayoutJob`. Used by the table widget to render
/// inline markdown inside cells (`widget-table-render`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StyledRun {
    pub text: SmolStr,
    pub color: Color,
    pub bold: bool,
    pub italic: bool,
    pub strike: bool,
    pub underline: bool,
    /// Render in a monospace family with a faint background box (inline code).
    pub code: bool,
    /// Optional run background (e.g. the inline-code box). `None` = transparent.
    pub bg: Option<Color>,
}

impl StyledRun {
    /// A plain (unstyled) run carrying just `text` in `color`. Convenience for
    /// the common case; flags default to off.
    pub fn plain(text: impl Into<SmolStr>, color: Color) -> Self {
        Self {
            text: text.into(),
            color,
            bold: false,
            italic: false,
            strike: false,
            underline: false,
            code: false,
            bg: None,
        }
    }
}

/// One retained native-paint primitive: plain geometry + style in **logical
/// points**, no egui or GPU types. A [`BlockWidget`] returns a list of these
/// from [`paint_list`](BlockWidget::paint_list) and the `editor-egui` painter
/// replays them into the widget's reserved rect (translating point coords to
/// screen). Coordinates are relative to the widget box's top-left.
///
/// This is the egui-free half of the `widget-block-native-paint` hook: the data
/// side describes *what* to draw (it cannot name a `Painter`), the host decides
/// *how*. Mirrors the discipline of [`WidgetPixels`] / [`WidgetClickRegion`] /
/// [`BlockTextLine`]. Used by tables (`widget-table-render`); generic enough
/// that any later structured native-painted block reuses it.
#[derive(Clone, Debug, PartialEq)]
pub enum BlockPaint {
    /// A filled rectangle (header / cell backgrounds).
    Rect { x: f32, y: f32, w: f32, h: f32, color: Color },
    /// A straight line (grid rules, cell borders). `width` is the stroke width
    /// in logical points.
    Line { from: (f32, f32), to: (f32, f32), width: f32, color: Color },
    /// A positioned text run. `(x, y)` is the top-left anchor of the line box
    /// (the painter vertically lays the glyph baseline within it); `align`
    /// resolves `x` against the run's painter-measured width. `font_scale` is a
    /// multiplier on the widget's base font size (1.0 == body size).
    Text {
        x: f32,
        y: f32,
        text: SmolStr,
        color: Color,
        font_scale: f32,
        align: TextAlign,
    },
    /// A wrapping rich-text block: a sequence of [`StyledRun`]s laid out as a
    /// single multi-format galley wrapped to `max_width` (logical points), the
    /// whole block anchored at `(x, y)` (top-left) and horizontally aligned by
    /// `align` against `max_width`. The host builds one egui `LayoutJob` with a
    /// format section per run (so a `**bold** *italic*` cell wraps as one
    /// paragraph with per-run style). Used by table cells (`widget-table-render`).
    RichText {
        x: f32,
        y: f32,
        runs: Vec<StyledRun>,
        max_width: f32,
        align: TextAlign,
    },
}

/// Data-side trait for block widgets injected in the vertical gap above /
/// below a line. A widget supplies its visual either as a retained native-paint
/// list via [`paint_list`](BlockWidget::paint_list) (replayed by the host with
/// no texture — used by tables) or as raw pixels via
/// [`pixels`](BlockWidget::pixels); a widget without either renders as a colored
/// placeholder rect.
pub trait BlockWidget: Send + Sync {
    /// Returns the laid-out height (pixels) for the given font size and
    /// available width.
    fn measure(&self, font_size: f32, width: f32) -> f32;
    fn handles_click(&self) -> bool {
        false
    }
    /// Stable identity for diffing; defaults to 0.
    ///
    /// This is also the texture-cache key (slug `widget-painter-texture-blit`).
    /// A widget that supplies [`pixels`](BlockWidget::pixels) MUST return a
    /// stable, content-derived id (a hash of source + style + size + dpr +
    /// theme) so the texture cache invalidates whenever the rendered bytes —
    /// including the widget's size — change.
    fn widget_id(&self) -> u64 {
        0
    }
    /// When `Some`, the host replays the returned [`BlockPaint`] primitives
    /// natively into the widget's reserved rect — no texture, no raster
    /// (slug `widget-block-native-paint`). Takes precedence over
    /// [`pixels`](BlockWidget::pixels): a widget that supplies a paint list is
    /// drawn from it directly. `font_size` is the editor body font size and
    /// `width` the available widget width (both logical points), matching
    /// [`measure`](BlockWidget::measure) so the list and the reserved height
    /// agree. Default `None` keeps the texture / placeholder behavior.
    ///
    /// Used by the table widget; the hook is generic, so any structured
    /// native-painted block reuses it.
    fn paint_list(&self, font_size: f32, width: f32) -> Option<Vec<BlockPaint>> {
        let _ = (font_size, width);
        None
    }
    /// When `Some`, the block-widget painter uploads the returned RGBA8 buffer
    /// to a texture, caches it by [`widget_id`](BlockWidget::widget_id), and
    /// blits it into the reserved rect (slug `widget-painter-texture-blit`).
    /// [`paint_list`](BlockWidget::paint_list) takes precedence when present.
    /// Default `None` keeps the placeholder behavior for widgets without pixels.
    fn pixels(&self) -> Option<WidgetPixels<'_>> {
        None
    }
    /// Clickable sub-regions in NORMALIZED widget coords (0.0..1.0 of the
    /// widget's painted box), each with a host-defined id. Default: none.
    /// `font_size` and `width` are the same layout inputs passed to
    /// [`measure`](BlockWidget::measure) / [`paint_list`](BlockWidget::paint_list),
    /// so a widget whose sub-regions depend on its laid-out geometry (e.g. a
    /// table's per-cell rects) can compute them; widgets with resolution-
    /// independent regions (e.g. a diagram's normalized hit-boxes) ignore them.
    ///
    /// Regions fire on BOTH paint paths. On the texture path
    /// ([`pixels`](BlockWidget::pixels) is `Some`) the painter maps each region
    /// through the same letterbox transform as the texture; on the native path
    /// ([`paint_list`](BlockWidget::paint_list) is `Some`) it maps them linearly
    /// into the painted box (no letterbox). Either way it emits a per-region
    /// [`WidgetClick(id)`] click zone in addition to the whole-widget zone keyed
    /// on [`widget_id`](BlockWidget::widget_id); the host distinguishes the two
    /// by id.
    fn click_regions(&self, font_size: f32, width: f32) -> Vec<WidgetClickRegion> {
        let _ = (font_size, width);
        Vec::new()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Color {
    pub r: u8,
    pub g: u8,
    pub b: u8,
    pub a: u8,
}

impl Color {
    pub const TRANSPARENT: Color = Color { r: 0, g: 0, b: 0, a: 0 };
    pub const fn rgb(r: u8, g: u8, b: u8) -> Self {
        Self { r, g, b, a: 255 }
    }
    pub const fn rgba(r: u8, g: u8, b: u8, a: u8) -> Self {
        Self { r, g, b, a }
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct MarkStyle {
    pub bold: bool,
    pub italic: bool,
    pub strikethrough: bool,
    pub underline: bool,
    pub fg: Option<Color>,
    pub bg: Option<Color>,
    /// 1.0 == base font size. Used for heading scaling.
    pub font_scale: Option<f32>,
    pub monospace: bool,
    /// When true, cursor motion treats the marked range as a single
    /// indivisible unit: motion commands that would land inside the range
    /// snap to the appropriate boundary in the direction of motion.
    /// `Decoration::Replace` is implicitly atomic regardless of this field.
    pub atomic: bool,
}

#[derive(Clone, Debug, Default, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct LineStyle {
    pub bg: Option<Color>,
    /// 1.0 == base line height. > 1.0 makes the line taller.
    pub height_scale: Option<f32>,
    /// Left indent in pixels (host-resolved).
    pub indent: Option<f32>,
    pub gutter_marker: Option<GutterMarker>,
    /// When true, the line is hidden entirely (height = 0, no text, no gutter,
    /// no selection, no cursor rendering). Used by folds.
    pub hide: bool,
    /// Show a clickable fold chevron in the gutter on this line.
    pub fold_chevron: Option<FoldChevron>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct FoldChevron {
    pub id: u64,
    pub collapsed: bool,
}

#[derive(Clone, Debug, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum GutterMarker {
    DiffAdded,
    DiffRemoved,
    DiffModified,
    Bookmark,
    Diagnostic(Severity),
    Custom(SmolStr),
}

#[derive(Clone)]
pub enum Decoration {
    /// Inline styling applied to a byte range.
    Mark(MarkStyle),
    /// Whole-line styling. The range should cover one line (rope's
    /// `line_to_byte` boundaries).
    Line(LineStyle),
    /// Hide the underlying text and (optionally) render `display` instead.
    Replace { display: Option<SmolStr> },
    /// Inject vertical space above or below the line at `range.start`. The
    /// underlying model is *not* modified — line numbers stay correct, cursor
    /// positions still map to source. Equivalent to CodeMirror's "block
    /// widget" and VSCode's "view zone".
    Block(BlockDeco),
    /// Trait-object inline widget. The painter reserves a region sized via
    /// `widget.measure(font_size)`. v1 limitation: the egui adapter renders a
    /// styled placeholder rect with a small "widget" label rather than calling
    /// into the widget for painting.
    InlineWidget {
        widget: Arc<dyn InlineWidget>,
        /// If true, the inline widget is treated as a single indivisible unit
        /// for cursor motion (equivalent to `MarkStyle { atomic: true }`).
        atomic: bool,
    },
    /// Trait-object block widget. The painter draws it from the widget's
    /// retained native-paint list ([`BlockWidget::paint_list`],
    /// slug `widget-block-native-paint`) when present, else blits its texture
    /// ([`BlockWidget::pixels`]), else falls back to a colored placeholder rect.
    BlockWidget {
        side: BlockSide,
        widget: Arc<dyn BlockWidget>,
    },
}

impl fmt::Debug for Decoration {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Decoration::Mark(s) => f.debug_tuple("Mark").field(s).finish(),
            Decoration::Line(s) => f.debug_tuple("Line").field(s).finish(),
            Decoration::Replace { display } => {
                f.debug_struct("Replace").field("display", display).finish()
            }
            Decoration::Block(b) => f.debug_tuple("Block").field(b).finish(),
            Decoration::InlineWidget { atomic, widget } => f
                .debug_struct("InlineWidget")
                .field("atomic", atomic)
                .field("widget_id", &widget.widget_id())
                .finish(),
            Decoration::BlockWidget { side, widget } => f
                .debug_struct("BlockWidget")
                .field("side", side)
                .field("widget_id", &widget.widget_id())
                .finish(),
        }
    }
}

// TODO(serde): `BlockDeco`/`BlockKind` and `Decoration` itself are not
// serializable because they may transitively reference `Arc<dyn Widget>`
// trait objects. Revisit when a stable widget identity scheme exists.
#[derive(Clone, Debug)]
pub struct BlockDeco {
    pub side: BlockSide,
    pub height: f32,
    pub kind: BlockKind,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum BlockSide {
    Above,
    Below,
}

#[derive(Clone, Debug)]
pub enum BlockKind {
    /// Diagonal-stripe hatched fill — used for spacer alignment in
    /// side-by-side diffs.
    Hatched(Color),
    /// Solid fill — used for plain gutters / placeholders.
    Solid(Color),
    /// Rendered text — used by the unified inline diff to show removed
    /// lines stacked above their replacement.
    Text {
        lines: Vec<BlockTextLine>,
    },
    /// Clickable bar that toggles a fold. The host receives a
    /// `ClickAction::ToggleFold(id)` and is responsible for updating its fold
    /// state and re-emitting decorations on the next frame.
    Expander {
        id: u64,
        label: SmolStr,
        /// Whether the body is currently collapsed; used to choose the
        /// chevron glyph and label tense.
        collapsed: bool,
    },
    /// Horizontal row with a label on the left and one or more clickable
    /// buttons on the right. Used by hosts that want a single-line "do
    /// something" affordance attached to a block range (patch-review
    /// per-hunk Accept/Reject, unanchored-pin rows). Each enabled button
    /// registers a `ClickAction::WidgetClick(button.id)` zone.
    ActionRow {
        label: SmolStr,
        /// Small leading glyph rendered before the label (e.g. "?" for an
        /// unanchored hunk). Empty / None when not needed.
        glyph: Option<SmolStr>,
        tone: ActionTone,
        buttons: Vec<ActionButton>,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum ActionTone {
    Normal,
    Warning,
    Conflicted,
}

#[derive(Clone, Debug)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct ActionButton {
    pub id: u64,
    pub label: SmolStr,
    pub style: ActionButtonStyle,
    pub enabled: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum ActionButtonStyle {
    Primary,
    Danger,
    Neutral,
}

#[derive(Clone, Debug)]
pub struct BlockTextLine {
    pub text: SmolStr,
    pub bg: Option<Color>,
    pub fg: Option<Color>,
    pub gutter_marker: Option<GutterMarker>,
    /// Intraline byte ranges (within `text`) to emphasize with a bg color.
    pub marks: Vec<(std::ops::Range<usize>, Color)>,
    /// Draw a horizontal strike line through the whole text row. Set on
    /// removed-diff lines so deleted content reads as struck-through in
    /// addition to its red background.
    pub strikethrough: bool,
}

impl HeightAffecting for Decoration {
    /// A decoration affects line height when it hides a line, scales its
    /// height, or injects a vertical block above/below it. Mark / Replace /
    /// InlineWidget styling stays within the existing line box and does not.
    /// This is the single source of truth the heightmap driver relies on; it
    /// must stay in sync with the variants matched in the egui widget's
    /// `apply_line_height_decorations`.
    fn affects_height(&self) -> bool {
        matches!(
            self,
            Decoration::Line(LineStyle { hide: true, .. })
                | Decoration::Line(LineStyle { height_scale: Some(_), .. })
                | Decoration::Block(_)
                | Decoration::BlockWidget { .. }
        )
    }
}

pub type Set = RangeSet<Decoration>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn height_affecting_variants() {
        assert!(Decoration::Line(LineStyle { hide: true, ..Default::default() }).affects_height());
        assert!(Decoration::Line(LineStyle {
            height_scale: Some(2.0),
            ..Default::default()
        })
        .affects_height());
        assert!(
            Decoration::Block(BlockDeco {
                side: BlockSide::Above,
                height: 10.0,
                kind: BlockKind::Solid(Color::TRANSPARENT),
            })
            .affects_height()
        );
    }

    #[test]
    fn paint_only_variants_do_not_affect_height() {
        assert!(!Decoration::Mark(MarkStyle::default()).affects_height());
        assert!(!Decoration::Replace { display: None }.affects_height());
        // A Line with only a bg / indent (no hide, no height_scale) is
        // paint-only — it stays within the existing line box.
        assert!(!Decoration::Line(LineStyle {
            bg: Some(Color::TRANSPARENT),
            ..Default::default()
        })
        .affects_height());
    }

    #[test]
    fn set_marks_affects_height_at_construction() {
        let height_set: Set = RangeSet::from_iter([(
            0..1,
            Decoration::Line(LineStyle { height_scale: Some(1.5), ..Default::default() }),
        )]);
        assert!(height_set.affects_height());

        let paint_set: Set =
            RangeSet::from_iter([(0..1, Decoration::Mark(MarkStyle { bold: true, ..Default::default() }))]);
        assert!(!paint_set.affects_height());

        // Mixed set: one height-affecting entry flips the whole set.
        let mixed: Set = RangeSet::from_iter([
            (0..1, Decoration::Mark(MarkStyle::default())),
            (2..3, Decoration::Line(LineStyle { hide: true, ..Default::default() })),
        ]);
        assert!(mixed.affects_height());
    }
}
