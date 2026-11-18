//! The example trees are documentation, so they rot unless something runs them.
//!
//! Every check here drives the real binary against the files in `examples/`:
//! a config the program rejects is the worst kind of documentation bug, and a
//! widget file that stops parsing is just as bad.
//!
//! Run just these:  cargo test --test examples

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_stratum")
}

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).to_path_buf()
}

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("stratum-examples-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch dir");
    dir
}

fn run(args: &[&str]) -> Output {
    Command::new(bin())
        .args(args)
        .output()
        .expect("spawn stratum binary")
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

fn pngs(dir: &Path) -> Vec<String> {
    let mut found: Vec<String> = std::fs::read_dir(dir)
        .expect("output dir")
        .filter_map(Result::ok)
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.ends_with(".png"))
        .collect();
    found.sort();
    found
}

#[test]
fn the_single_file_tour_parses_and_renders() {
    let out = scratch("tour");
    let cfg = repo().join("examples/widgets.toml");
    let res = run(&[
        "render",
        cfg.to_str().expect("path"),
        "-o",
        out.to_str().expect("path"),
        "--no-scripts",
    ]);
    assert!(res.status.success(), "render failed: {}", stderr(&res));
    let files = pngs(&out);
    // Seven widgets + the screen composite.
    assert_eq!(
        files.len(),
        8,
        "expected 7 widget PNGs + composite, got {files:?}"
    );
    assert!(files.contains(&"composite.png".to_string()));
    // Eight widget blocks, one of them `enabled = false`: it is parsed, and it is
    // not on the screen.
    assert!(
        !files.contains(&"off-demo.png".to_string()),
        "a disabled widget must not be written, got {files:?}"
    );
    let _ = std::fs::remove_dir_all(&out);
}

#[test]
fn the_real_tree_loads_every_widget() {
    let cfg = repo().join("examples/desktop/config.toml");
    let res = run(&["config", cfg.to_str().expect("path"), "--search"]);
    assert!(res.status.success(), "config failed: {}", stderr(&res));
    let stdout = String::from_utf8_lossy(&res.stdout);
    assert!(
        stdout.contains("widgets     6"),
        "expected the six example widgets, got:\n{stdout}"
    );
}

#[test]
fn the_real_tree_renders_without_starting_any_script() {
    // `--no-scripts` keeps this offline and instant: the scripts here call
    // Last.fm and OpenRouter, and CI has no credentials.
    let out = scratch("desktop");
    let cfg = repo().join("examples/desktop/config.toml");
    let res = run(&[
        "render",
        cfg.to_str().expect("path"),
        "-o",
        out.to_str().expect("path"),
        "--no-scripts",
    ]);
    assert!(res.status.success(), "render failed: {}", stderr(&res));

    let files = pngs(&out);
    assert_eq!(
        files.len(),
        7,
        "expected 6 widget PNGs + composite, got {files:?}"
    );
    for id in ["clock", "load", "credits", "lastfm", "nowplaying", "stock"] {
        assert!(
            files.iter().any(|f| f == &format!("{id}.png")),
            "{id}.png missing from {files:?}"
        );
    }
    let _ = std::fs::remove_dir_all(&out);
}

#[test]
fn every_example_widget_file_parses_on_its_own() {
    // A widget file is the unit people copy; each must stand alone.
    let dir = repo().join("examples/desktop/widgets");
    let mut checked = 0;
    for entry in std::fs::read_dir(&dir).expect("widgets dir") {
        let path = entry.expect("entry").path();
        if path.extension().and_then(|e| e.to_str()) != Some("toml") {
            continue;
        }
        let res = run(&["dump", path.to_str().expect("path")]);
        assert!(
            res.status.success(),
            "{} does not parse: {}",
            path.display(),
            stderr(&res)
        );
        checked += 1;
    }
    assert_eq!(checked, 6, "expected six example widget files");
}
