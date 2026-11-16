//! The graph element: donut, line, bar.
//!
//! sketchybar has exactly one (`graph_draw`, src/graph.c:47 — a filled
//! sparkline). These are pure functions over (canvas, rect, data, style) so the
//! widget layer stays dumb and the charts stay unit-testable: given the same
//! numbers and rect they produce the same pixels, no state carried between
//! frames.

use crate::canvas::{Canvas, arc, round_rect_path};
use crate::model::{Frame, GraphKind, GraphSpec, Rgba};
use tiny_skia::PathBuilder;

/// Colour for index `i`, repeating the last one.
fn color_at(colors: &[Rgba], i: usize) -> Rgba {
    match colors.len() {
        0 => Rgba::rgb(0x88, 0xcc, 0xff),
        n => colors[i.min(n - 1)],
    }
}

/// Draw `values` into `rect` following `spec`. Returns false when there was
/// nothing to draw (empty data, zero area) so the caller can skip the dirty
/// flag churn.
pub fn draw(canvas: &mut Canvas, rect: Frame, spec: &GraphSpec, values: &[f32]) -> bool {
    if values.is_empty() || rect.w <= 0.0 || rect.h <= 0.0 {
        return false;
    }
    match spec.kind {
        GraphKind::Donut => draw_donut(canvas, rect, spec, values),
        GraphKind::Line => draw_line(canvas, rect, spec, values),
        GraphKind::Bar => draw_bar(canvas, rect, spec, values),
    }
}

// ----------------------------------------------------------------- donut ----

/// Angular extent of a donut ring for `weight` in 0.0..=1.0.
///
/// Returns `(start_angle, sweep)`. The ring is centred on the top of the widget,
/// so the missing arc is split evenly either side of the bottom — "270°" in the
/// usual y-up description, `+pi/2` in this y-down canvas. At `weight = 1.0` the
/// sweep is a full turn; at `0.5` it is the top semicircle, from 9 o'clock over
/// the top to 3 o'clock.
pub fn donut_span(weight: f32) -> (f32, f32) {
    let w = weight.clamp(0.0, 1.0);
    let start = std::f32::consts::FRAC_PI_2 + (1.0 - w) * std::f32::consts::PI;
    (start, w * std::f32::consts::TAU)
}

fn draw_donut(canvas: &mut Canvas, rect: Frame, spec: &GraphSpec, values: &[f32]) -> bool {
    let radius = (rect.w.min(rect.h) / 2.0).max(1.0);
    let cx = rect.x + rect.w / 2.0;
    let cy = rect.y + rect.h / 2.0;

    // `thickness` is reused as the hole ratio here: 0.6 means the inner radius
    // is 60% of the outer one.
    let hole = if spec.thickness <= 0.0 {
        0.6
    } else {
        spec.thickness.clamp(0.1, 0.95)
    };
    let outer = radius;
    let inner = radius * hole;

    let total: f32 = values.iter().filter(|v| **v > 0.0).sum();
    if total <= 0.0 {
        return false;
    }

    let (start, ring_sweep) = donut_span(spec.weight);
    if ring_sweep <= 0.0 {
        return false;
    }

    let mut angle = start;
    let gap = 0.02; // radians between slices, keeps boundaries legible
    for (i, v) in values.iter().enumerate() {
        if *v <= 0.0 {
            continue;
        }
        let sweep = (*v / total) * ring_sweep;
        let start = angle + gap / 2.0;
        let end = angle + sweep - gap / 2.0;
        if end > start
            && let Some(path) = ring_slice(cx, cy, inner, outer, start, end, canvas.scale())
        {
            canvas.fill_path(&path, color_at(&spec.colors, i));
        }
        angle += sweep;
    }
    true
}

/// One donut slice: outer arc forward, inner arc backward, closed.
fn ring_slice(
    cx: f32,
    cy: f32,
    inner: f32,
    outer: f32,
    start: f32,
    end: f32,
    scale: f32,
) -> Option<tiny_skia::Path> {
    let (cx, cy, inner, outer) = (cx * scale, cy * scale, inner * scale, outer * scale);
    let mut pb = PathBuilder::new();

    let (sx, sy) = (cx + outer * start.cos(), cy + outer * start.sin());
    pb.move_to(sx, sy);
    arc(&mut pb, cx, cy, outer, start, end);

    let (ix, iy) = (cx + inner * end.cos(), cy + inner * end.sin());
    pb.line_to(ix, iy);
    arc(&mut pb, cx, cy, inner, end, start);
    pb.close();
    pb.finish()
}

// ------------------------------------------------------------------ line ----

fn draw_line(canvas: &mut Canvas, rect: Frame, spec: &GraphSpec, values: &[f32]) -> bool {
    let (min, max) = bounds(values);
    let span = (max - min).max(f32::EPSILON);
    let n = values.len();
    let step = if n > 1 { rect.w / (n - 1) as f32 } else { 0.0 };

    let y_of = |v: f32| rect.y + rect.h - ((v - min) / span) * rect.h;

    // Path in device pixels.
    let s = canvas.scale();
    let mut pb = PathBuilder::new();
    pb.move_to(rect.x * s, y_of(values[0]) * s);
    for (i, v) in values.iter().enumerate().skip(1) {
        let x = rect.x + step * i as f32;
        pb.line_to(x * s, y_of(*v) * s);
    }
    let Some(line) = pb.finish() else {
        return false;
    };

    if spec.filled {
        // Close down to the baseline and fill with the line colour at low alpha
        // (sketchybar's graph is a filled polygon; this is the same idea).
        let mut fill_pb = PathBuilder::new();
        fill_pb.move_to(rect.x * s, (rect.y + rect.h) * s);
        fill_pb.line_to(rect.x * s, y_of(values[0]) * s);
        for (i, v) in values.iter().enumerate().skip(1) {
            let x = rect.x + step * i as f32;
            fill_pb.line_to(x * s, y_of(*v) * s);
        }
        fill_pb.line_to((rect.x + rect.w) * s, (rect.y + rect.h) * s);
        fill_pb.close();
        if let Some(fill) = fill_pb.finish() {
            let c = color_at(&spec.colors, 0);
            canvas.fill_path(
                &fill,
                Rgba {
                    a: (c.a as f32 * 0.25) as u8,
                    ..c
                },
            );
        }
    }

    canvas.stroke_path(&line, spec.thickness.max(1.0), color_at(&spec.colors, 0));
    true
}

// ------------------------------------------------------------------- bar ----

fn draw_bar(canvas: &mut Canvas, rect: Frame, spec: &GraphSpec, values: &[f32]) -> bool {
    let max = values
        .iter()
        .cloned()
        .fold(0.0f32, f32::max)
        .max(f32::EPSILON);
    let n = values.len() as f32;
    // `thickness` is reused as the gap in points between bars.
    let gap = spec.thickness.max(0.0);
    let bar_w = ((rect.w - gap * (n - 1.0)) / n).max(1.0);

    for (i, v) in values.iter().enumerate() {
        let h = ((*v / max) * rect.h).max(1.0);
        let x = rect.x + i as f32 * (bar_w + gap);
        let y = rect.y + rect.h - h;
        let radius = (bar_w / 4.0).min(4.0);
        canvas.fill_round_rect(
            Frame::new(x, y, bar_w, h),
            radius,
            color_at(&spec.colors, i),
        );
    }
    true
}

fn bounds(values: &[f32]) -> (f32, f32) {
    let mut min = f32::MAX;
    let mut max = f32::MIN;
    for v in values {
        min = min.min(*v);
        max = max.max(*v);
    }
    if !min.is_finite() || !max.is_finite() {
        return (0.0, 1.0);
    }
    // Pad a flat series so the line does not sit exactly on an edge.
    if (max - min).abs() < 1e-6 {
        return (min - 0.5, max + 0.5);
    }
    (min, max)
}

/// Convenience for the widget layer: draw a rounded plate behind a chart.
pub fn plate(canvas: &mut Canvas, rect: Frame, radius: f32, color: Rgba) {
    if let Some(path) = round_rect_path(
        rect.x * canvas.scale(),
        rect.y * canvas.scale(),
        rect.w * canvas.scale(),
        rect.h * canvas.scale(),
        radius * canvas.scale(),
    ) {
        canvas.fill_path(&path, color);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::f32::consts::{FRAC_PI_2, PI, TAU};

    /// The ring is centred on the top, so the gap is symmetric about the bottom.
    #[test]
    fn weight_maps_to_a_sweep_around_the_top() {
        let full = donut_span(1.0);
        assert!((full.1 - TAU).abs() < 1e-5, "full weight is a whole turn");

        let half = donut_span(0.5);
        assert!((half.1 - PI).abs() < 1e-5, "half weight is a semicircle");
        // Begins at 9 o'clock (pi in this y-down space) and ends at 3 o'clock.
        assert!((half.0 - PI).abs() < 1e-5, "starts at {:?}", half.0);
        assert!((half.0 + half.1 - TAU).abs() < 1e-5, "ends at 3 o'clock");

        // A quarter hangs off the bottom: from 10:30 to 1:30.
        let quarter = donut_span(0.25);
        assert!((quarter.1 - TAU / 4.0).abs() < 1e-5);
        assert!((quarter.0 - (FRAC_PI_2 + 0.75 * PI)).abs() < 1e-5);
    }

    /// The gap must be centred exactly on the bottom (pi/2 here) for any weight:
    /// the arc's midpoint is the top, its endpoints are the same distance either
    /// side of the bottom.
    #[test]
    fn the_gap_is_symmetric_about_the_bottom() {
        for w in [0.1f32, 0.25, 0.5, 0.66, 0.8, 0.95, 1.0] {
            let (start, sweep) = donut_span(w);
            let end = start + sweep;
            let from_bottom_start = (start - FRAC_PI_2).rem_euclid(TAU);
            let from_bottom_end = (FRAC_PI_2 + TAU - end).rem_euclid(TAU);
            assert!(
                (from_bottom_start - from_bottom_end).abs() < 1e-4,
                "weight {w}: gap is lopsided ({from_bottom_start} vs {from_bottom_end})"
            );
        }
    }

    #[test]
    fn degenerate_weights_are_clamped() {
        assert_eq!(donut_span(-1.0).1, 0.0);
        assert!((donut_span(2.0).1 - TAU).abs() < 1e-5);
    }
}
