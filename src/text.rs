//! Text: Core Text's job in sketchybar (`CTLineDraw`, src/text.c:318).
//!
//! fontdue gives us the two things we actually need — advance widths for
//! measurement and coverage bitmaps for drawing — and we blend those into the
//! canvas ourselves. No shaping/ligatures: that is the one honest regression
//! against `CTLineDraw`, and `rustybuzz` slots in here if it matters.
//!
//! Two caches keep the hot path allocation-free:
//!   - fonts live in a `Vec` so a glyph key can hold an index, not a name;
//!   - rasterised, coloured glyphs are cached by (font, pixel size, char, colour).
//!     Re-rasterising every glyph of every text element on every repaint is the
//!     biggest per-frame cost in a widget bar: a 1 Hz clock would reshape the
//!     same eight glyphs a thousand times an hour for no reason.
//!
//! Static vs dynamic (`TextKind`) is still enforced by the caller: a static spec
//! is measured once, a dynamic one every tick — but neither re-rasterises.
//!
//! Set `STRATUM_TRACE_TEXT=1` to dump per-glyph pen positions.

use crate::canvas::Canvas;
use crate::model::{Align, Rgba};
use fontdue::{Font, FontSettings};
use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::Arc;
use tiny_skia::{Pixmap, PixmapPaint, Transform};

/// Cache key: the font is an index, so the key stays `Copy` (no per-glyph
/// `String` allocation on the hot path).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct GlyphKey {
    font: usize,
    /// `f32::to_bits` of the pixel size — f32 is neither `Eq` nor `Hash`.
    px: u32,
    ch: char,
    /// Premultiplied RGBA.
    color: [u8; 4],
}

pub struct TextEngine {
    fonts: Vec<(String, Font)>,
    /// Index into `fonts` used for unknown names.
    fallback: Option<usize>,
    glyphs: RefCell<HashMap<GlyphKey, Arc<Pixmap>>>,
}

impl TextEngine {
    pub fn new() -> Self {
        TextEngine {
            fonts: Vec::new(),
            fallback: None,
            glyphs: RefCell::new(HashMap::new()),
        }
    }

    /// Load a TTF/OTF (or index 0 of a TTC) under `name`. The first font loaded
    /// becomes the fallback for unknown names.
    pub fn load(&mut self, name: &str, path: &str) -> Result<(), String> {
        let bytes = std::fs::read(path).map_err(|e| format!("font {path}: {e}"))?;
        let font = Font::from_bytes(bytes, FontSettings::default())
            .map_err(|e| format!("font {path}: {e}"))?;
        if let Some(slot) = self.fonts.iter_mut().find(|(n, _)| n == name) {
            slot.1 = font;
        } else {
            self.fonts.push((name.to_string(), font));
            if self.fallback.is_none() {
                self.fallback = Some(self.fonts.len() - 1);
            }
        }
        Ok(())
    }

    /// Load the usual macOS stack under `default` / `mono` aliases.
    pub fn load_system_defaults(&mut self) -> Result<(), String> {
        const CANDIDATES: &[(&str, &str)] = &[
            ("default", "/System/Library/Fonts/SFNS.ttf"),
            ("default", "/System/Library/Fonts/Helvetica.ttc"),
            ("default", "/System/Library/Fonts/Supplemental/Arial.ttf"),
            ("mono", "/System/Library/Fonts/SFNSMono.ttf"),
            ("mono", "/System/Library/Fonts/Menlo.ttc"),
        ];
        for (name, path) in CANDIDATES {
            if self.has(name) || !std::path::Path::new(path).exists() {
                continue;
            }
            let _ = self.load(name, path);
        }
        if self.fonts.is_empty() {
            return Err("no usable system font found".into());
        }
        Ok(())
    }

    pub fn has(&self, name: &str) -> bool {
        self.fonts.iter().any(|(n, _)| n == name)
    }

    /// Names currently loaded — used to decide whether the system fallback chain
    /// has to be loaded at all.
    pub fn loaded_names(&self) -> Vec<&str> {
        self.fonts.iter().map(|(n, _)| n.as_str()).collect()
    }

    fn index(&self, name: &str) -> Option<usize> {
        self.fonts
            .iter()
            .position(|(n, _)| n == name)
            .or(self.fallback)
    }

    fn font(&self, index: usize) -> &Font {
        &self.fonts[index].1
    }

    pub fn line_height(&self, name: &str, size: f32, scale: f32) -> f32 {
        let px = size * scale;
        match self
            .index(name)
            .and_then(|i| self.font(i).horizontal_line_metrics(px))
        {
            Some(m) => m.new_line_size / scale,
            None => size * 1.3,
        }
    }

    /// Width in points of a single line.
    ///
    /// Advances are measured at the DEVICE size (`size * scale`) and converted
    /// back to points. Measuring at `size` while drawing on a 2x canvas makes
    /// text half-size with double tracking — the classic mixed-units bug, and it
    /// looks like a font problem rather than a unit problem.
    pub fn measure(&self, name: &str, size: f32, text: &str, scale: f32) -> f32 {
        let Some(i) = self.index(name) else {
            return 0.0;
        };
        let font = self.font(i);
        let px = size * scale;
        text.chars()
            .map(|c| font.metrics(c, px).advance_width)
            .sum::<f32>()
            / scale
    }

    /// Truncate with an ellipsis so the result fits `max_chars` characters.
    pub fn truncate(text: &str, max_chars: Option<usize>) -> String {
        let Some(max) = max_chars else {
            return text.to_string();
        };
        if max == 0 {
            return String::new();
        }
        let count = text.chars().count();
        if count <= max {
            return text.to_string();
        }
        let keep = max.saturating_sub(1);
        let mut out: String = text.chars().take(keep).collect();
        out.push('…');
        out
    }

    /// The coloured glyph bitmap, rasterised once per (font, size, char, colour).
    fn glyph(&self, key: GlyphKey) -> Option<Arc<Pixmap>> {
        if let Some(pm) = self.glyphs.borrow().get(&key) {
            return Some(Arc::clone(pm));
        }
        let font = self.font(key.font);
        let px = f32::from_bits(key.px);
        let (metrics, coverage) = font.rasterize(key.ch, px);
        // A zero-sized or unallocatable glyph slot: nothing to draw.
        let mut glyph = Pixmap::new(metrics.width as u32, metrics.height as u32)?;
        let data = glyph.data_mut();
        for (i, cov) in coverage.iter().enumerate() {
            // Premultiply: coverage × colour alpha, then colour × alpha.
            let a = (*cov as u32 * key.color[3] as u32 + 127) / 255;
            if a == 0 {
                continue;
            }
            let o = i * 4;
            let scale = |c: u8| ((c as u32 * a + 127) / 255) as u8;
            data[o] = scale(key.color[0]);
            data[o + 1] = scale(key.color[1]);
            data[o + 2] = scale(key.color[2]);
            data[o + 3] = a as u8;
        }
        let glyph = Arc::new(glyph);
        self.glyphs.borrow_mut().insert(key, Arc::clone(&glyph));
        Some(glyph)
    }

    /// Drop the glyph cache (diagnostics; the benchmark uses it to measure the
    /// uncached path without re-parsing fonts).
    pub fn clear_glyph_cache(&mut self) {
        self.glyphs.borrow_mut().clear();
    }

    /// Number of cached glyph bitmaps — diagnostics and tests.
    pub fn cached_glyphs(&self) -> usize {
        self.glyphs.borrow().len()
    }

    /// Draw one line, top-left at `pos` (points), advancing right.
    /// Returns the drawn width in points.
    // A drawing call legitimately needs its text, font, colour, position and
    // alignment; grouping them into a struct for one caller would obscure more
    // than it clarifies.
    #[expect(
        clippy::too_many_arguments,
        reason = "one call site, all params distinct"
    )]
    pub fn draw(
        &self,
        canvas: &mut Canvas,
        name: &str,
        size: f32,
        text: &str,
        pos: (f32, f32),
        color: Rgba,
        align: Align,
        box_width: f32,
    ) -> f32 {
        let Some(font_index) = self.index(name) else {
            return 0.0;
        };
        let scale = canvas.scale();
        let total = self.measure(name, size, text, scale);
        let x = match align {
            Align::Left => pos.0,
            Align::Center => pos.0 + (box_width - total) / 2.0,
            Align::Right => pos.0 + box_width - total,
        };
        let line = self.line_height(name, size, scale);
        let baseline = pos.1 + line * 0.78;

        // Everything below is DEVICE pixels: glyphs are rasterised at the device
        // size and the pen advances in device pixels. Only the caller's inputs
        // and the return value are points.
        let px_size = size * scale;
        let font = self.font(font_index);
        let rgba = color.premultiplied();
        let trace = std::env::var_os("STRATUM_TRACE_TEXT").is_some();
        let mut pen = x * scale;

        for ch in text.chars() {
            let metrics = font.metrics(ch, px_size);
            if trace {
                eprintln!(
                    "[text] {ch:?} pen={pen:.2} adv={:.2} w={} h={} xmin={}",
                    metrics.advance_width, metrics.width, metrics.height, metrics.xmin
                );
            }
            if metrics.width > 0 && metrics.height > 0 {
                let Some(glyph) = self.glyph(GlyphKey {
                    font: font_index,
                    px: px_size.to_bits(),
                    ch,
                    color: rgba,
                }) else {
                    pen += metrics.advance_width;
                    continue;
                };
                // fontdue metrics are y-up relative to the baseline; our canvas
                // is y-down, so the bitmap top sits `ymin + height` above the
                // baseline. `pen` and `baseline` are already device pixels here —
                // multiplying by scale again is the bug that makes text look
                // double-spaced and clipped.
                let gx = (pen + metrics.xmin as f32).round() as i32;
                let gy = (baseline * scale - (metrics.ymin + metrics.height as i32) as f32).round()
                    as i32;
                canvas.pixmap_mut().draw_pixmap(
                    gx,
                    gy,
                    glyph.as_ref().as_ref(),
                    &PixmapPaint::default(),
                    Transform::identity(),
                    None,
                );
            }
            pen += metrics.advance_width;
        }
        total
    }
}

impl Default for TextEngine {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repeats_of_the_same_glyph_hit_the_cache() {
        let mut te = TextEngine::new();
        te.load_system_defaults().expect("fonts");
        let mut canvas = Canvas::new(40.0, 12.0, 2.0).expect("canvas");

        te.draw(
            &mut canvas,
            "mono",
            10.0,
            "aa",
            (0.0, 0.0),
            Rgba::WHITE,
            Align::Left,
            40.0,
        );
        assert_eq!(te.cached_glyphs(), 1, "two identical glyphs, one bitmap");

        // Same glyph in a different colour is a different entry.
        te.draw(
            &mut canvas,
            "mono",
            10.0,
            "a",
            (0.0, 0.0),
            Rgba::rgb(255, 0, 0),
            Align::Left,
            40.0,
        );
        assert_eq!(te.cached_glyphs(), 2);

        // Redrawing the first one is free.
        te.draw(
            &mut canvas,
            "mono",
            10.0,
            "aa",
            (0.0, 0.0),
            Rgba::WHITE,
            Align::Left,
            40.0,
        );
        assert_eq!(te.cached_glyphs(), 2);
    }

    /// Timing measurement, not a CI gate:
    /// `cargo test --release --lib bench_glyph -- --ignored --nocapture`
    #[test]
    #[ignore = "timing measurement"]
    fn bench_glyph_cache() {
        use std::time::Instant;
        let mut te = TextEngine::new();
        te.load_system_defaults().expect("fonts");
        let mut canvas = Canvas::new(300.0, 20.0, 2.0).expect("canvas");
        let text = "load average 12.34 MB/s";
        let n = 2000usize;

        // Warm: every glyph comes from the cache (the steady state of a repaint).
        let t = Instant::now();
        for _ in 0..n {
            te.draw(
                &mut canvas,
                "mono",
                11.0,
                text,
                (0.0, 0.0),
                Rgba::WHITE,
                Align::Left,
                300.0,
            );
        }
        let warm = t.elapsed();

        // Cold: cache cleared before each draw, so every glyph is re-rasterised
        // (what the code did on every repaint before the cache existed).
        let t = Instant::now();
        for _ in 0..n {
            te.clear_glyph_cache();
            te.draw(
                &mut canvas,
                "mono",
                11.0,
                text,
                (0.0, 0.0),
                Rgba::WHITE,
                Align::Left,
                300.0,
            );
        }
        let cold = t.elapsed();

        let per = |d: std::time::Duration| d.as_secs_f64() * 1e6 / n as f64;
        eprintln!(
            "{n} draws of {text:?}: cached {:.1} us/draw, uncached {:.1} us/draw, {:.1}x faster",
            per(warm),
            per(cold),
            cold.as_secs_f64() / warm.as_secs_f64()
        );
    }
}
