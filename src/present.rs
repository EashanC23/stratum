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
        // Replace rather than push: a widget is presented again whenever its
        // pixels change, and keeping every version would draw the stale ones
        // underneath the current one (ghosted text, an icon in the wrong place).
        let entry = (id.to_string(), frame, image.clone());
        match self.written.iter_mut().find(|(i, _, _)| i == id) {
            Some(slot) => *slot = entry,
            None => self.written.push(entry),
        }
        Ok(())
    }

    fn destroy(&mut self, id: &str) -> Result<(), String> {
        self.written.retain(|(i, _, _)| i != id);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tiny_skia::{Color, Pixmap};

    fn solid(w: u32, h: u32) -> Pixmap {
        let mut pm = Pixmap::new(w, h).expect("pixmap");
        pm.fill(Color::WHITE);
        pm
    }

    #[test]
    fn composite_keeps_only_the_newest_paint_of_a_widget() {
        let dir = std::env::temp_dir().join(format!("stratum-composite-{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();

        let mut p = PngPresenter::new(&dir, Frame::new(0.0, 0.0, 40.0, 20.0), 1.0, true);
        // First paint: no data yet, so the widget is small and sits at the left.
        p.present("a", Frame::new(0.0, 0.0, 10.0, 10.0), &solid(10, 10))
            .expect("first present");
        // Second paint: values arrived, the widget grew and moved right.
        p.present("a", Frame::new(20.0, 0.0, 10.0, 10.0), &solid(10, 10))
            .expect("second present");

        let files = p.finish().expect("composite");
        let pm = Pixmap::load_png(&files[0]).expect("png");
        let alpha = |x: u32, y: u32| pm.pixel(x, y).expect("pixel").alpha();

        assert_eq!(alpha(5, 5), 0, "the stale copy must not be composited");
        assert_eq!(alpha(25, 5), 255, "the newest copy must be composited");
        std::fs::remove_dir_all(&dir).ok();
    }
}
