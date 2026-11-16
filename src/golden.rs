//! Golden-image regression support.
//!
//! The idea: render widgets deterministically and compare the result to a
//! committed PNG, so any change in rasterisation, layout, colour handling or
//! text placement fails a test instead of being noticed (or not) by eye.
//!
//! Determinism comes from three things, all of which matter:
//!   1. a pinned font under `tests/golden/fonts/` (system fonts change with
//!      macOS releases and would make every golden machine-specific);
//!   2. inline data and `static` text — no scripts, no clock, no sysctl;
//!   3. pinned crates (`Cargo.lock`), because antialiasing lives in tiny-skia.
//!
//! Comparison is tolerant on purpose: `Tolerance::default()` allows a couple of
//! values of channel drift on a small fraction of pixels. That absorbs
//! insignificant build variance without hiding a real regression — a shifted
//! glyph, a wrong colour or a changed radius moves far more than that.

use crate::canvas::Canvas;
use crate::icon::IconCache;
use crate::model::{Values, WidgetSpec};
use crate::text::TextEngine;
use crate::widget;
use std::path::Path;
use tiny_skia::Pixmap;

/// How much difference is not a failure.
///
/// Two independent limits, because one is not enough:
///   - `strong_delta`: a channel jump this big is a real change *on any single
///     pixel* (a magenta speck, a missing glyph, a shifted edge). No fraction
///     allowance — one is a failure.
///   - `weak_delta` + `max_weak_fraction`: drift this small is tolerated, but
///     only on a small share of pixels (build-to-build rounding).
///
/// A single-limit design either lets a one-pixel artifact through (fraction
/// threshold) or fails on harmless rounding (exact match).
#[derive(Debug, Clone, Copy)]
pub struct Tolerance {
    pub weak_delta: u8,
    pub max_weak_fraction: f32,
    pub strong_delta: u8,
}

impl Default for Tolerance {
    fn default() -> Self {
        Tolerance {
            weak_delta: 2,
            max_weak_fraction: 0.002,
            strong_delta: 24,
        }
    }
}

impl Tolerance {
    pub const EXACT: Tolerance = Tolerance {
        weak_delta: 0,
        max_weak_fraction: 0.0,
        strong_delta: 1,
    };
}

#[derive(Debug)]
pub struct Diff {
    pub expected: (u32, u32),
    pub actual: (u32, u32),
    /// Pixels beyond `weak_delta`.
    pub differing: usize,
    /// Pixels beyond `strong_delta` — any of these fails outright.
    pub strong: usize,
    pub total: usize,
    pub worst_delta: u8,
    pub worst_at: (u32, u32),
    pub tol: Tolerance,
}

impl Diff {
    pub fn differing_fraction(&self) -> f32 {
        if self.total == 0 {
            0.0
        } else {
            self.differing as f32 / self.total as f32
        }
    }

    /// The single answer: does this pair pass?
    pub fn passes(&self) -> bool {
        self.expected == self.actual
            && self.strong == 0
            && self.differing_fraction() <= self.tol.max_weak_fraction
    }

    /// Human/agent-readable failure message.
    pub fn report(&self, name: &str) -> String {
        if self.expected != self.actual {
            return format!(
                "{name}: size changed, expected {}x{}, got {}x{}",
                self.expected.0, self.expected.1, self.actual.0, self.actual.1
            );
        }
        format!(
            "{name}: {} of {} pixels differ >{} ({:.2}%), {} differ >{} (not allowed), worst channel delta {} at ({}, {})",
            self.differing,
            self.total,
            self.tol.weak_delta,
            self.differing_fraction() * 100.0,
            self.strong,
            self.tol.strong_delta,
            self.worst_delta,
            self.worst_at.0,
            self.worst_at.1
        )
    }
}

/// Rasterise a widget into a standalone pixmap — no presenter, no window.
///
/// The scale is explicit here (rather than coming from a config) so goldens can
/// exercise 1x and 2x from the same spec.
pub fn render_widget(
    widget: &WidgetSpec,
    values: &Values,
    text: &TextEngine,
    icons: &mut IconCache,
    scale: f32,
) -> Result<Pixmap, String> {
    let mut ctx = widget::Ctx { text, icons, scale };
    let avail = (
        if widget.frame.w > 0.0 {
            widget.frame.w
        } else {
            1024.0
        },
        if widget.frame.h > 0.0 {
            widget.frame.h
        } else {
            1024.0
        },
    );
    let (w, h) = widget::measure(widget, &ctx, values, avail);
    let (w, h) = (w.max(1.0), h.max(1.0));

    let mut canvas = Canvas::new(w, h, scale)
        .ok_or_else(|| format!("{}: canvas allocation failed", widget.id))?;
    canvas.clear();
    widget::draw(widget, &mut canvas, &mut ctx, values);
    Ok(canvas.into_pixmap())
}

/// `clock` @ 2x -> `clock@2x.png`.
pub fn file_name(id: &str, scale: f32) -> String {
    let s = if (scale.fract()).abs() < f32::EPSILON {
        format!("{}", scale as i32)
    } else {
        format!("{scale}")
    };
    format!("{id}@{s}x.png")
}

pub const SCALES: &[f32] = &[1.0, 2.0];

/// Build a text engine for a config: explicit `[fonts]` entries, then the system
/// stack for anything missing. Shared by the runtime and the `golden` command so
/// a golden renders exactly like the daemon does.
pub fn text_engine_for(cfg: &crate::config::Config) -> Result<TextEngine, String> {
    let mut text = TextEngine::new();
    for (name, path) in &cfg.fonts {
        text.load(name, path)?;
    }

    // Load the macOS font chain only for names a widget actually uses and that
    // are still missing. A config that pins its fonts never touches the system
    // files — worth ~20 MB of resident memory (SFNS + Helvetica + Menlo, parsed).
    let used = font_names_used(cfg);
    let missing: Vec<&String> = used.iter().filter(|n| !text.has(n.as_str())).collect();
    if !missing.is_empty() || text.loaded_names().is_empty() {
        text.load_system_defaults()?;
    }
    Ok(text)
}

/// Every font name any widget could ask for. `default` is included when a text
/// element leaves `font` out (that is the model's default alias).
pub fn font_names_used(cfg: &crate::config::Config) -> std::collections::BTreeSet<String> {
    let mut used = std::collections::BTreeSet::new();
    for w in &cfg.widgets {
        for el in &w.elements {
            if let crate::model::Element::Text(t) = el {
                used.insert(t.font.clone());
            }
        }
    }
    used
}

/// Render every widget at every golden scale into `dir`.
/// Returns the files written.
pub fn write_all(
    cfg: &crate::config::Config,
    dir: &Path,
    text: &TextEngine,
    icons: &mut IconCache,
) -> Result<Vec<std::path::PathBuf>, String> {
    std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let mut written = Vec::new();
    let values = Values::default();

    for widget in &cfg.widgets {
        for &scale in SCALES {
            let pm = render_widget(widget, &values, text, icons, scale)?;
            let path = dir.join(file_name(&widget.id, scale));
            pm.save_png(&path)
                .map_err(|e| format!("{}: {e}", path.display()))?;
            written.push(path);
        }
    }
    Ok(written)
}

/// Compare a rendered pixmap against a committed PNG.
pub fn check_file(expected: &Path, actual: &Pixmap, tol: Tolerance) -> Result<(), String> {
    let bytes = std::fs::read(expected).map_err(|e| {
        format!(
            "{}: {e}\n  (regenerate with: cargo test --test golden -- --ignored update_goldens)",
            expected.display()
        )
    })?;
    let exp = Pixmap::decode_png(&bytes)
        .map_err(|e| format!("{}: not a decodable PNG: {e}", expected.display()))?;
    let diff = compare(&exp, actual, tol);
    let name = expected.display().to_string();
    if !diff.passes() {
        return Err(format!(
            "{}\n  if this change is intended: cargo test --test golden -- --ignored update_goldens",
            diff.report(&name)
        ));
    }
    Ok(())
}

/// Pure comparison, no I/O — this is what the unit tests exercise.
pub fn compare(expected: &Pixmap, actual: &Pixmap, tol: Tolerance) -> Diff {
    let (ew, eh) = (expected.width(), expected.height());
    let (aw, ah) = (actual.width(), actual.height());
    let mut diff = Diff {
        expected: (ew, eh),
        actual: (aw, ah),
        differing: 0,
        strong: 0,
        total: (ew * eh) as usize,
        worst_delta: 0,
        worst_at: (0, 0),
        tol,
    };
    if (ew, eh) != (aw, ah) {
        return diff;
    }

    let e = expected.data();
    let a = actual.data();
    for (i, (ep, ap)) in e.chunks_exact(4).zip(a.chunks_exact(4)).enumerate() {
        let delta = (0..4).map(|c| ep[c].abs_diff(ap[c])).max().unwrap_or(0);
        if delta > 0 {
            let (x, y) = ((i as u32) % ew, (i as u32) / ew);
            if delta > diff.worst_delta {
                diff.worst_delta = delta;
                diff.worst_at = (x, y);
            }
            if delta > tol.weak_delta {
                diff.differing += 1;
            }
            if delta > tol.strong_delta {
                diff.strong += 1;
            }
        }
    }
    diff
}

#[cfg(test)]
mod tests {
    use super::*;
    use tiny_skia::Color;

    fn solid(w: u32, h: u32, rgba: [u8; 4]) -> Pixmap {
        let mut pm = Pixmap::new(w, h).expect("pixmap");
        pm.fill(Color::from_rgba8(rgba[0], rgba[1], rgba[2], rgba[3]));
        pm
    }

    #[test]
    fn identical_images_have_no_difference() {
        let a = solid(8, 8, [10, 20, 30, 255]);
        let b = solid(8, 8, [10, 20, 30, 255]);
        let d = compare(&a, &b, Tolerance::default());
        assert_eq!(d.differing, 0);
        assert_eq!(d.worst_delta, 0);
        assert!(d.passes());
    }

    #[test]
    fn tiny_drift_is_tolerated_but_a_real_change_is_not() {
        let a = solid(10, 10, [100, 100, 100, 255]);
        let mut b = solid(10, 10, [100, 100, 100, 255]);
        // One pixel drifts by 1: inside tolerance.
        b.pixels_mut()[0] = tiny_skia::PremultipliedColorU8::from_rgba(101, 100, 100, 255).unwrap();
        let d = compare(&a, &b, Tolerance::default());
        assert_eq!(d.worst_delta, 1);
        assert!(d.passes(), "1/255 drift on one pixel should pass");

        // A whole half of the image changes colour: not tolerated.
        let mut c = solid(10, 10, [100, 100, 100, 255]);
        for y in 0..10u32 {
            for x in 0..5u32 {
                c.pixels_mut()[(y * 10 + x) as usize] =
                    tiny_skia::PremultipliedColorU8::from_rgba(255, 0, 0, 255).unwrap();
            }
        }
        let d = compare(&a, &c, Tolerance::default());
        assert!(!d.passes(), "half the image changing colour must fail");
        assert!(d.strong > 0);
    }

    #[test]
    fn size_change_is_reported_as_such() {
        let a = solid(8, 8, [0, 0, 0, 255]);
        let b = solid(8, 9, [0, 0, 0, 255]);
        let d = compare(&a, &b, Tolerance::default());
        let msg = d.report("widget");
        assert!(msg.contains("size changed"), "{msg}");
        assert!(msg.contains("8x8") && msg.contains("8x9"), "{msg}");
    }

    #[test]
    fn golden_file_names_are_stable() {
        assert_eq!(file_name("disk", 1.0), "disk@1x.png");
        assert_eq!(file_name("disk", 2.0), "disk@2x.png");
    }

    #[test]
    fn a_fully_pinned_config_does_not_load_system_fonts() {
        let cfg = crate::config::Config {
            fonts: [(
                "mono".to_string(),
                "tests/golden/fonts/JetBrainsMono-Regular.ttf".to_string(),
            )]
            .into_iter()
            .collect(),
            widgets: vec![crate::model::WidgetSpec {
                id: "t".into(),
                elements: vec![crate::model::Element::Text(crate::model::TextSpec {
                    font: "mono".into(),
                    ..Default::default()
                })],
                ..Default::default()
            }],
            ..Default::default()
        };
        let te = text_engine_for(&cfg).expect("engine");
        assert_eq!(
            te.loaded_names(),
            vec!["mono"],
            "only the pinned font may be parsed when a config pins everything it uses"
        );
    }
}
