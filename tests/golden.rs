//! Golden-image regression tests (M4).
//!
//! Renders `tests/golden/widgets.toml` at 1x and 2x and compares each widget
//! against the committed PNG of the same name.
//!
//!     cargo test --test golden                      # compare (what CI runs)
//!     cargo test --test golden -- --ignored update_goldens   # regenerate
//!
//! Failure messages say how many pixels differ and where, so "the donut arc
//! moved" is a two-second diagnosis rather than a bisect.
//!
//! Determinism: the config pins its font to tests/golden/fonts/ and uses only
//! inline data and static text, so the same input renders the same bytes on any
//! machine with the same Cargo.lock.

use std::path::{Path, PathBuf};
use stratum::config::Config;
use stratum::golden::{self, Tolerance};
use stratum::icon::IconCache;
use stratum::model::Values;
use stratum::text::TextEngine;

fn golden_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/golden")
}

fn load() -> (Config, TextEngine) {
    let cfg = Config::load(&golden_dir().join("widgets.toml")).expect("golden config parses");
    let mut text = TextEngine::new();
    for (name, path) in &cfg.fonts {
        text.load(name, path)
            .unwrap_or_else(|e| panic!("font {name} at {path}: {e}"));
    }
    assert!(
        text.has("mono"),
        "the golden config must pin a font, not fall back to a system one"
    );
    (cfg, text)
}

/// Compare, or regenerate when `update_goldens` is explicitly requested.
fn check_or_update(
    id: &str,
    scale: f32,
    pm: &tiny_skia::Pixmap,
    update: bool,
) -> Result<(), String> {
    let path = golden_dir().join(golden::file_name(id, scale));
    if update {
        pm.save_png(&path)
            .map_err(|e| format!("{}: {e}", path.display()))?;
        eprintln!("updated {}", path.display());
        return Ok(());
    }
    golden::check_file(&path, pm, Tolerance::default())
}

#[test]
fn goldens_match() {
    let (cfg, text) = load();
    let mut icons = IconCache::new();
    let values = Values::default();

    let mut failures = Vec::new();
    let mut compared = 0usize;

    for widget in &cfg.widgets {
        for &scale in golden::SCALES {
            let pm = golden::render_widget(widget, &values, &text, &mut icons, scale)
                .unwrap_or_else(|e| panic!("render {}: {e}", widget.id));
            compared += 1;
            if let Err(e) = check_or_update(&widget.id, scale, &pm, false) {
                failures.push(e);
            }
        }
    }

    assert!(
        compared >= 16,
        "expected at least 8 widgets x 2 scales, rendered {compared}"
    );
    assert!(
        failures.is_empty(),
        "{} of {compared} golden image(s) drifted:\n\n{}",
        failures.len(),
        failures.join("\n\n")
    );
}

/// Regenerate every golden. Run explicitly:
///     cargo test --test golden -- --ignored update_goldens
///
/// Kept `#[ignore]` so a normal `cargo test` can never silently rewrite the
/// expectations it is supposed to be checking.
#[test]
#[ignore = "writes the committed golden PNGs; run deliberately"]
fn update_goldens() {
    let (cfg, text) = load();
    let mut icons = IconCache::new();
    let values = Values::default();
    let dir = golden_dir();

    let mut n = 0;
    for widget in &cfg.widgets {
        for &scale in golden::SCALES {
            let pm = golden::render_widget(widget, &values, &text, &mut icons, scale)
                .unwrap_or_else(|e| panic!("render {}: {e}", widget.id));
            let path = dir.join(golden::file_name(&widget.id, scale));
            pm.save_png(&path)
                .unwrap_or_else(|e| panic!("{}: {e}", path.display()));
            eprintln!("wrote {} ({}x{})", path.display(), pm.width(), pm.height());
            n += 1;
        }
    }
    assert!(n >= 16, "only wrote {n} goldens");
}

/// A golden harness that silently passes is worse than none: prove the
/// comparison actually rejects a changed pixel by corrupting an in-memory copy.
#[test]
fn comparison_rejects_a_tampered_image() {
    let (cfg, text) = load();
    let mut icons = IconCache::new();
    let values = Values::default();
    let widget = cfg
        .widgets
        .iter()
        .find(|w| w.id == "plate")
        .expect("plate widget");

    let pm = golden::render_widget(widget, &values, &text, &mut icons, 1.0).expect("render");
    let mut tampered = pm.clone();
    let stride = tampered.width() as usize;
    let idx = (stride * 3) + 5;
    let px = tampered.pixels_mut()[idx];
    tampered.pixels_mut()[idx] =
        tiny_skia::PremultipliedColorU8::from_rgba(255, 0, 255, 255).expect("colour");
    let _ = px;

    let diff = golden::compare(&pm, &tampered, Tolerance::default());
    assert!(
        !diff.passes(),
        "a single magenta pixel was not detected: {diff:?}"
    );
    assert_eq!(
        diff.strong, 1,
        "exactly the tampered pixel should be strong"
    );
}
