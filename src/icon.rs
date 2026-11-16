//! Icons: raster + vector sources, cached by (path, mtime, target size, tint).
//!
//! sketchybar's equivalent is `image.c` + `image_draw` (src/image.c:254) which
//! only ever deals with CGImage. Here the model accepts two sources:
//!
//! - `svg`: rasterised with resvg at the requested pixel size (crisp at 2x, and
//!   re-rasterised when the size changes rather than upscaled)
//! - `bitmap`: PNG decoded once; other formats need another decoder (see
//!   HANDOFF.md — add `image` or `zune-jpeg` for JPEG)
//!
//! `tint` multiplies coverage, which is how a single monochrome SVG becomes a
//! themed icon without shipping one file per colour.

use crate::model::Rgba;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::SystemTime;
use tiny_skia::Pixmap;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct Key {
    path: PathBuf,
    mtime: Option<SystemTime>,
    /// Requested height in device pixels; 0 = intrinsic.
    height_px: u32,
    tint: Option<Rgba>,
}

pub struct IconCache {
    entries: HashMap<Key, Pixmap>,
    /// Last mtime we saw per path, refreshed at most once a second. `get` runs
    /// once per icon per repaint; a `stat` syscall there is pure waste.
    stats: HashMap<PathBuf, (Option<SystemTime>, std::time::Instant)>,
    hits: u64,
    misses: u64,
}

impl IconCache {
    pub fn new() -> Self {
        IconCache {
            entries: HashMap::new(),
            stats: HashMap::new(),
            hits: 0,
            misses: 0,
        }
    }

    pub fn stats(&self) -> (u64, u64) {
        (self.hits, self.misses)
    }

    /// Load (or fetch from cache) an icon, scaled so its height is `height_px`
    /// device pixels. Aspect ratio is preserved.
    pub fn get(
        &mut self,
        path: &Path,
        height_px: u32,
        tint: Option<Rgba>,
        scale: f32,
    ) -> Result<&Pixmap, String> {
        let mtime = self.mtime(path);
        let key = Key {
            path: path.to_path_buf(),
            mtime,
            height_px,
            tint,
        };
        // Entry API: one lookup, and the borrow from `insert` is the one we return
        // — no `expect("just inserted")` and no double hashing.
        match self.entries.entry(key) {
            std::collections::hash_map::Entry::Occupied(e) => {
                self.hits += 1;
                Ok(e.into_mut())
            }
            std::collections::hash_map::Entry::Vacant(e) => {
                self.misses += 1;
                let pm = load(path, height_px, tint, scale)?;
                Ok(e.insert(pm))
            }
        }
    }

    pub fn clear(&mut self) {
        self.entries.clear();
        self.stats.clear();
    }

    /// mtime with a one-second memo, so repaints do not syscall per icon.
    fn mtime(&mut self, path: &Path) -> Option<SystemTime> {
        let fresh = self
            .stats
            .get(path)
            .filter(|(_, at)| at.elapsed() < std::time::Duration::from_secs(1))
            .map(|(t, _)| *t);
        match fresh {
            Some(t) => t,
            None => {
                let t = std::fs::metadata(path).ok().and_then(|m| m.modified().ok());
                self.stats
                    .insert(path.to_path_buf(), (t, std::time::Instant::now()));
                t
            }
        }
    }
}

impl Default for IconCache {
    fn default() -> Self {
        Self::new()
    }
}

fn load(path: &Path, height_px: u32, tint: Option<Rgba>, scale: f32) -> Result<Pixmap, String> {
    let is_svg = path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.eq_ignore_ascii_case("svg"))
        .unwrap_or(false);

    let mut pm = if is_svg {
        render_svg(path, height_px, scale)?
    } else {
        load_bitmap(path)?
    };

    // Normalise to the requested pixel height. SVG is rendered at size already;
    // bitmaps are resampled once here instead of per frame.
    if height_px > 0 && pm.height() != height_px {
        pm = resize_to_height(&pm, height_px)?;
    }

    if let Some(t) = tint {
        apply_tint(&mut pm, t);
    }
    Ok(pm)
}

fn render_svg(path: &Path, height_px: u32, scale: f32) -> Result<Pixmap, String> {
    let data = std::fs::read(path).map_err(|e| format!("svg {}: {e}", path.display()))?;
    let opt = usvg::Options::default();
    let tree =
        usvg::Tree::from_data(&data, &opt).map_err(|e| format!("svg {}: {e}", path.display()))?;

    let size = tree.size();
    let (iw, ih) = (size.width(), size.height());
    if iw <= 0.0 || ih <= 0.0 {
        return Err(format!("svg {}: zero size", path.display()));
    }
    let target_h = if height_px > 0 {
        height_px as f32
    } else {
        (ih * scale).ceil()
    };
    let k = target_h / ih;
    let w = (iw * k).ceil().max(1.0) as u32;
    let h = (ih * k).ceil().max(1.0) as u32;

    let mut pm = Pixmap::new(w, h).ok_or_else(|| "svg: failed to allocate pixmap".to_string())?;
    resvg::render(
        &tree,
        tiny_skia::Transform::from_scale(k, k),
        &mut pm.as_mut(),
    );
    Ok(pm)
}

fn load_bitmap(path: &Path) -> Result<Pixmap, String> {
    let data = std::fs::read(path).map_err(|e| format!("icon {}: {e}", path.display()))?;
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    match ext.as_str() {
        "png" => Pixmap::decode_png(&data).map_err(|e| format!("png {}: {e}", path.display())),
        "jpg" | "jpeg" => Err(format!(
            "jpeg {}: decoder not wired up yet — add the `image` crate (HANDOFF.md, milestone 5)",
            path.display()
        )),
        other => Err(format!(
            "icon {}: unsupported format {other:?} (png and svg are built in)",
            path.display()
        )),
    }
}

fn resize_to_height(src: &Pixmap, height_px: u32) -> Result<Pixmap, String> {
    let k = height_px as f32 / src.height() as f32;
    let w = ((src.width() as f32 * k).ceil() as u32).max(1);
    let mut dst = Pixmap::new(w, height_px).ok_or("icon resize: allocation failed")?;
    dst.draw_pixmap(
        0,
        0,
        src.as_ref(),
        &tiny_skia::PixmapPaint {
            quality: tiny_skia::FilterQuality::Bilinear,
            ..Default::default()
        },
        tiny_skia::Transform::from_scale(
            w as f32 / src.width() as f32,
            height_px as f32 / src.height() as f32,
        ),
        None,
    );
    Ok(dst)
}

/// Recolour: keep the pixel's coverage (alpha), replace its colour with `tint`.
/// Output stays premultiplied, which is what the compositor wants.
fn apply_tint(pm: &mut Pixmap, tint: Rgba) {
    let data = pm.data_mut();
    for px in data.chunks_exact_mut(4) {
        let a = px[3] as u32 * tint.a as u32 / 255;
        if a == 0 {
            px.fill(0);
            continue;
        }
        px[0] = (tint.r as u32 * a / 255) as u8;
        px[1] = (tint.g as u32 * a / 255) as u8;
        px[2] = (tint.b as u32 * a / 255) as u8;
        px[3] = a as u8;
    }
}
