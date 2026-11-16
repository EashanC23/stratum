//! Presentation: where a finished widget image goes.
//!
//! The whole point of keeping this behind a trait is that the raster layer
//! never learns about windows. Two implementations exist and one is planned:
//!
//! 1. `PngPresenter` — headless, for tests and for eyeballing output. In this crate.
//! 2. `MacPresenter` (src/live.rs) — the real one: a borderless, non-activating
//!    window per widget, layer-backed, the pixmap copied into `CALayer.contents`
//!    as a CGImage. Public AppKit only; see HANDOFF.md §6 for what the private
//!    SkyLight path would add.
//! 3. `WgpuPresenter` — optional later: upload the pixmap as a wgpu texture and
//!    let the GPU composite, for effects (backdrop blur, moving-surface clips)
//!    that would otherwise be CPU-bound.
//!
//! Contract: `present` receives a widget id, the screen-space frame it occupies,
//! and a premultiplied RGBA pixmap at `scale`. It must not assume the pixmap's
//! dimensions relate to the frame by anything other than `frame * scale`.

use crate::model::Frame;
use std::path::{Path, PathBuf};
use tiny_skia::Pixmap;

pub trait Presenter {
    fn present(&mut self, id: &str, frame: Frame, image: &Pixmap) -> Result<(), String>;
    /// Called when a widget's frame or visibility changed but its pixels did
    /// not; a window-backed presenter moves the window here instead of
    /// re-uploading contents.
    fn reposition(&mut self, _id: &str, _frame: Frame) -> Result<(), String> {
        Ok(())
    }
    fn destroy(&mut self, _id: &str) -> Result<(), String> {
        Ok(())
    }
}

/// Writes `<dir>/<id>.png` per widget and `<dir>/composite.png` with every
/// widget placed on the screen.
pub struct PngPresenter {
    dir: PathBuf,
    screen: Frame,
    scale: f32,
    composite: bool,
    written: Vec<(String, Frame, Pixmap)>,
}

impl PngPresenter {
    pub fn new(dir: &Path, screen: Frame, scale: f32, composite: bool) -> Self {
        PngPresenter {
            dir: dir.to_path_buf(),
            screen,
            scale,
            composite,
            written: Vec::new(),
        }
    }

    pub fn finish(&mut self) -> Result<Vec<PathBuf>, String> {
        if !self.composite || self.written.is_empty() {
            return Ok(Vec::new());
        }
        let mut canvas = crate::canvas::Canvas::new(self.screen.w, self.screen.h, self.scale)
            .ok_or("composite: allocation failed")?;
        canvas.clear();

        // Paint in layer order; stable sort keeps config order for ties.
        let mut order: Vec<usize> = (0..self.written.len()).collect();
        order.sort_by_key(|i| self.written[*i].1.y as i32);
        for i in order {
            let (_, frame, pm) = &self.written[i];
            canvas.draw_rgba(pm, (frame.x, frame.y), (frame.w, frame.h), 1.0);
        }

        let path = self.dir.join("composite.png");
        canvas
            .into_pixmap()
            .save_png(&path)
            .map_err(|e| format!("composite {}: {e}", path.display()))?;
        Ok(vec![path])
    }
}

impl Presenter for PngPresenter {
    fn present(&mut self, id: &str, frame: Frame, image: &Pixmap) -> Result<(), String> {
        std::fs::create_dir_all(&self.dir)
            .map_err(|e| format!("output dir {}: {e}", self.dir.display()))?;
        let path = self.dir.join(format!("{id}.png"));
        image
            .save_png(&path)
            .map_err(|e| format!("{}: {e}", path.display()))?;
        self.written.push((id.to_string(), frame, image.clone()));
        Ok(())
    }

    fn destroy(&mut self, id: &str) -> Result<(), String> {
        self.written.retain(|(i, _, _)| i != id);
        Ok(())
    }
}
