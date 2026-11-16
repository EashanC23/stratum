//! The raster layer: one offscreen pixmap per widget, plus the handful of
//! primitives sketchybar draws with.
//!
//! Mapping from the original (see HANDOFF.md):
//!   CGBitmapContextCreate + CGContextScaleCTM(2.0)  -> Canvas::new(w, h, 2.0)
//!   CGContextClearRect                              -> Canvas::clear
//!   CGPathAddRoundedRect + kCGPathFillStroke        -> fill_round_rect / stroke_round_rect
//!   CGContextSetRGBFillColor / StrokeColor          -> the Rgba argument
//!   CGBitmapContextCreateImage + layer.contents     -> Canvas::pixmap()
//!
//! All public coordinates are POINTS. Device pixels are an implementation
//! detail handled by `px()`, so a caller never has to know the scale — this is
//! what `CGContextScaleCTM` bought sketchybar, without the transform subtleties
//! of tiny-skia's `Transform` (stroke widths would not scale with it).

use crate::model::{Frame, Rgba};
use tiny_skia::{
    FillRule, Paint, Path, PathBuilder, Pixmap, PixmapPaint, Shader, Stroke, Transform,
};

/// Bezier circle constant. Small enough to inline; exported for the graph
/// module's arcs.
pub const KAPPA: f32 = 0.552_284_8;

pub struct Canvas {
    pixmap: Pixmap,
    scale: f32,
}

impl Canvas {
    pub fn new(width_pt: f32, height_pt: f32, scale: f32) -> Option<Self> {
        let w = (width_pt * scale).ceil().max(1.0) as u32;
        let h = (height_pt * scale).ceil().max(1.0) as u32;
        Some(Canvas {
            pixmap: Pixmap::new(w, h)?,
            scale,
        })
    }

    pub fn from_pixmap(pixmap: Pixmap) -> Self {
        Canvas { pixmap, scale: 1.0 }
    }

    pub fn scale(&self) -> f32 {
        self.scale
    }

    /// Points -> device pixels.
    pub fn px(&self, pts: f32) -> f32 {
        pts * self.scale
    }

    pub fn width_pt(&self) -> f32 {
        self.pixmap.width() as f32 / self.scale
    }

    pub fn height_pt(&self) -> f32 {
        self.pixmap.height() as f32 / self.scale
    }

    pub fn pixmap(&self) -> &Pixmap {
        &self.pixmap
    }

    pub fn pixmap_mut(&mut self) -> &mut Pixmap {
        &mut self.pixmap
    }

    pub fn into_pixmap(self) -> Pixmap {
        self.pixmap
    }

    // ---------------------------------------------------------- clearing ----

    pub fn clear(&mut self) {
        self.pixmap.fill(tiny_skia::Color::TRANSPARENT);
    }

    pub fn clear_rect(&mut self, rect: Frame) {
        // tiny-skia has no "erase" blend mode, so paint with a punch-out:
        // fill a path with Src blend by drawing over via mask is overkill.
        // Instead we fill with transparent using the Source blend mode through
        // a manual loop — cheap, and erase is rare (see window_clear_background
        // in src/window.c for why it matters).
        let (x0, y0, x1, y1) = self.clamp_rect(rect);
        let w = self.pixmap.width() as usize;
        let data = self.pixmap.data_mut();
        for y in y0..y1 {
            for x in x0..x1 {
                let i = (y * w + x) * 4;
                data[i..i + 4].fill(0);
            }
        }
    }

    // ------------------------------------------------------------- paths ----

    /// Rounded rectangle path in DEVICE pixels.
    pub fn round_rect_path(&self, rect: Frame, radius: f32) -> Option<Path> {
        round_rect_path(
            self.px(rect.x),
            self.px(rect.y),
            self.px(rect.w),
            self.px(rect.h),
            self.px(radius),
        )
    }

    pub fn fill_round_rect(&mut self, rect: Frame, radius: f32, color: Rgba) {
        if let Some(path) = self.round_rect_path(rect, radius) {
            self.fill_path(&path, color);
        }
    }

    pub fn stroke_round_rect(&mut self, rect: Frame, radius: f32, width: f32, color: Rgba) {
        if let Some(path) = self.round_rect_path(rect, radius) {
            self.stroke_path(&path, width, color);
        }
    }

    pub fn fill_path(&mut self, path: &Path, color: Rgba) {
        if color.a == 0 {
            return;
        }
        let paint = Paint {
            shader: Shader::SolidColor(to_skia(color)),
            anti_alias: true,
            ..Default::default()
        };
        self.pixmap
            .fill_path(path, &paint, FillRule::Winding, Transform::identity(), None);
    }

    pub fn stroke_path(&mut self, path: &Path, width: f32, color: Rgba) {
        if color.a == 0 {
            return;
        }
        let paint = Paint {
            shader: Shader::SolidColor(to_skia(color)),
            anti_alias: true,
            ..Default::default()
        };
        let stroke = Stroke {
            width: self.px(width).max(1.0),
            ..Default::default()
        };
        self.pixmap
            .stroke_path(path, &paint, &stroke, Transform::identity(), None);
    }

    // ------------------------------------------------------------ images ----

    /// Draw a premultiplied RGBA pixmap at a point-space position, scaled to
    /// `w`x`h` points.
    pub fn draw_rgba(&mut self, src: &Pixmap, at: (f32, f32), size: (f32, f32), opacity: f32) {
        let sw = self.px(size.0).max(1.0);
        let sh = self.px(size.1).max(1.0);
        let sx = sw / src.width() as f32;
        let sy = sh / src.height() as f32;
        let paint = PixmapPaint {
            opacity: opacity.clamp(0.0, 1.0),
            ..Default::default()
        };
        self.pixmap.draw_pixmap(
            self.px(at.0).round() as i32,
            self.px(at.1).round() as i32,
            src.as_ref(),
            &paint,
            Transform::from_scale(sx, sy),
            None,
        );
    }

    /// `CGContextClip(rounded rect)` equivalent: run `f` and keep only what
    /// lands inside `rect`.
    pub fn with_clip<F: FnOnce(&mut Canvas)>(&mut self, rect: Frame, radius: f32, f: F) {
        let Some(path) = self.round_rect_path(rect, radius) else {
            return;
        };
        let Some(mut mask) = tiny_skia::Mask::new(self.pixmap.width(), self.pixmap.height()) else {
            return;
        };
        mask.fill_path(&path, FillRule::Winding, true, Transform::identity());

        // Render into a scratch canvas, then composite through the mask.
        let Some(mut scratch) = Canvas::new(self.width_pt(), self.height_pt(), self.scale) else {
            return;
        };
        f(&mut scratch);
        let mut copy = scratch.pixmap().clone();
        copy.apply_mask(&mask);
        self.pixmap.draw_pixmap(
            0,
            0,
            copy.as_ref(),
            &PixmapPaint::default(),
            Transform::identity(),
            None,
        );
    }

    // ------------------------------------------------------------ helpers ----

    fn clamp_rect(&self, rect: Frame) -> (usize, usize, usize, usize) {
        let w = self.pixmap.width() as i64;
        let h = self.pixmap.height() as i64;
        let x0 = self.px(rect.x).floor() as i64;
        let y0 = self.px(rect.y).floor() as i64;
        let x1 = self.px(rect.x + rect.w).ceil() as i64;
        let y1 = self.px(rect.y + rect.h).ceil() as i64;
        (
            x0.clamp(0, w) as usize,
            y0.clamp(0, h) as usize,
            x1.clamp(0, w) as usize,
            y1.clamp(0, h) as usize,
        )
    }
}

pub fn to_skia(c: Rgba) -> tiny_skia::Color {
    tiny_skia::Color::from_rgba8(c.r, c.g, c.b, c.a)
}

/// Rounded rectangle as an explicit path (move/line/cubic/lines), which is what
/// `CGPathAddRoundedRect` builds internally. Coordinates are device pixels.
pub fn round_rect_path(x: f32, y: f32, w: f32, h: f32, r: f32) -> Option<Path> {
    if w <= 0.0 || h <= 0.0 {
        return None;
    }
    let r = r.min(w / 2.0).min(h / 2.0).max(0.0);
    let mut pb = PathBuilder::new();
    if r <= 0.5 {
        pb.push_rect(tiny_skia::Rect::from_xywh(x, y, w, h)?);
        return pb.finish();
    }
    let k = r * (1.0 - KAPPA);

    pb.move_to(x + r, y);
    pb.line_to(x + w - r, y);
    pb.cubic_to(x + w - k, y, x + w, y + k, x + w, y + r);
    pb.line_to(x + w, y + h - r);
    pb.cubic_to(x + w, y + h - k, x + w - k, y + h, x + w - r, y + h);
    pb.line_to(x + r, y + h);
    pb.cubic_to(x + k, y + h, x, y + h - k, x, y + h - r);
    pb.line_to(x, y + r);
    pb.cubic_to(x, y + k, x + k, y, x + r, y);
    pb.close();
    pb.finish()
}

/// Circular arc from `start` to `end` radians (0 = 3 o'clock, y-down clockwise
/// positive), as cubic segments. Used by the donut chart.
pub fn arc(pb: &mut PathBuilder, cx: f32, cy: f32, r: f32, start: f32, end: f32) {
    let mut a0 = start;
    let sweep = end - start;
    if sweep.abs() < 1e-4 {
        return;
    }
    // Split into <= 90 degree chunks so the cubic approximation stays tight.
    let steps = (sweep.abs() / std::f32::consts::FRAC_PI_2).ceil() as usize;
    let step = sweep / steps as f32;
    let alpha = (4.0 / 3.0) * (step / 4.0).tan();

    for _ in 0..steps {
        let a1 = a0 + step;
        let (x0, y0) = (cx + r * a0.cos(), cy + r * a0.sin());
        let (x1, y1) = (cx + r * a1.cos(), cy + r * a1.sin());
        let c0 = (x0 - r * alpha * a0.sin(), y0 + r * alpha * a0.cos());
        let c1 = (x1 + r * alpha * a1.sin(), y1 - r * alpha * a1.cos());
        pb.cubic_to(c0.0, c0.1, c1.0, c1.1, x1, y1);
        a0 = a1;
    }
}
