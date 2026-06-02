//! Texture cache for pixel widgets (slug `widget-painter-texture-blit`).
//!
//! A pixel widget (inline or block) hands the adapter a tightly-packed RGBA8
//! buffer already scaled to physical px (the widget bakes DPR in). We upload it
//! once to an egui texture, keep it in a cache keyed by `(widget_id, width,
//! height)`, and reuse the upload on later frames. Entries not requested during
//! a frame are evicted at frame end, so closed buffers don't leak GPU textures
//! — the same offscreen-texture discipline `minimap` uses for its strip.

use std::collections::HashMap;
use std::collections::hash_map::Entry as MapEntry;

use editor_core::decoration::WidgetPixels;
use egui::{Color32, ColorImage, Painter, Rect, TextureHandle, TextureOptions, Ui};

/// Cache key: the widget's stable id plus its physical pixel size. The size is
/// part of the key so a re-render at a new size (which a well-behaved widget
/// also reflects in `widget_id` and its buffer dims) always misses and
/// re-uploads instead of stretching a stale texture.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct TextureKey {
    pub widget_id: u64,
    pub width: u32,
    pub height: u32,
}

struct Entry {
    handle: TextureHandle,
    /// Frame index this entry was last requested on; drives eviction.
    last_used: u64,
}

/// Per-widget texture cache. Lives on the host-owned [`PaintCache`] alongside
/// the per-row layout cache, so it persists across frames and is evicted on the
/// same cadence (see `widget.rs`).
///
/// [`PaintCache`]: crate::widget::PaintCache
#[derive(Default)]
pub struct TextureCache {
    entries: HashMap<TextureKey, Entry>,
    frame: u64,
}

impl TextureCache {
    /// Advance to a new frame. Called once per paint pass before any
    /// [`blit`](Self::blit) so [`evict_unused`](Self::evict_unused) can tell
    /// stale entries from ones touched this frame.
    pub const fn begin_frame(&mut self) {
        self.frame = self.frame.wrapping_add(1);
    }

    /// Number of cached textures (inspection / test helper).
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Upload (on a cache miss) and blit a pixel widget into `rect`,
    /// letterboxing so the texture keeps its aspect ratio if `rect`'s shape
    /// differs from the buffer's. The rect normally already comes from the
    /// widget's `measure`, so letterboxing is a safety net for size drift.
    ///
    /// `ui` is used only to upload the texture (`ui.ctx().load_texture`); the
    /// image is *drawn* with the caller-supplied `painter`, which the editor
    /// clips to its body rect — so a tall/wide diagram is clipped to the editor
    /// instead of bleeding over the toolbar / into the gutter (the old path drew
    /// with the unclipped `ui.painter()`).
    ///
    /// Returns `true` if it painted; `false` if the buffer was malformed
    /// (`rgba.len() != width * height * 4`) so the caller can fall back to a
    /// placeholder. A malformed buffer is never cached.
    pub fn blit(
        &mut self,
        ui: &Ui,
        painter: &Painter,
        widget_id: u64,
        pixels: &WidgetPixels<'_>,
        rect: Rect,
    ) -> bool {
        let key = TextureKey { widget_id, width: pixels.width, height: pixels.height };
        let expected = (key.width as usize) * (key.height as usize) * 4;
        if pixels.rgba.len() != expected || expected == 0 {
            return false;
        }
        let frame = self.frame;
        let handle_id = match self.entries.entry(key) {
            MapEntry::Occupied(e) => {
                let e = e.into_mut();
                e.last_used = frame;
                e.handle.id()
            }
            MapEntry::Vacant(slot) => {
                let image = ColorImage::from_rgba_unmultiplied(
                    [key.width as usize, key.height as usize],
                    pixels.rgba,
                );
                let handle = ui.ctx().load_texture(
                    format!("widget-{:016x}", key.widget_id),
                    image,
                    TextureOptions::LINEAR,
                );
                let id = handle.id();
                slot.insert(Entry { handle, last_used: frame });
                id
            }
        };
        let target = letterbox(rect, pixels.width as f32, pixels.height as f32);
        painter.image(
            handle_id,
            target,
            Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
            Color32::WHITE,
        );
        true
    }

    /// Drop every entry not requested during the current frame.
    pub fn evict_unused(&mut self) {
        let frame = self.frame;
        self.entries.retain(|_, e| e.last_used == frame);
    }
}

/// Fit a `tex_w` x `tex_h` texture inside `rect`, preserving aspect ratio and
/// centering the result (letterbox). Degenerate sizes fall back to `rect`.
///
/// Exposed to the block-widget painter (`super::blocks`) so per-region click
/// zones map through the EXACT same transform [`TextureCache::blit`] uses,
/// keeping a clickable sub-region aligned with the pixels it covers.
pub(super) fn letterbox(rect: Rect, tex_w: f32, tex_h: f32) -> Rect {
    if tex_w <= 0.0 || tex_h <= 0.0 || rect.width() <= 0.0 || rect.height() <= 0.0 {
        return rect;
    }
    let scale = (rect.width() / tex_w).min(rect.height() / tex_h);
    Rect::from_center_size(rect.center(), egui::vec2(tex_w * scale, tex_h * scale))
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::pos2;

    #[test]
    fn letterbox_preserves_aspect_and_centers() {
        // A square texture into a wide rect -> pillarboxed square, centered.
        let rect = Rect::from_min_max(pos2(0.0, 0.0), pos2(200.0, 100.0));
        let fit = letterbox(rect, 50.0, 50.0);
        assert!((fit.width() - 100.0).abs() < 1e-3);
        assert!((fit.height() - 100.0).abs() < 1e-3);
        assert!((fit.center() - rect.center()).length() < 1e-3);
    }

    #[test]
    fn letterbox_degenerate_falls_back_to_rect() {
        let rect = Rect::from_min_max(pos2(0.0, 0.0), pos2(10.0, 10.0));
        assert_eq!(letterbox(rect, 0.0, 5.0), rect);
        assert_eq!(letterbox(rect, 5.0, 0.0), rect);
    }
}
