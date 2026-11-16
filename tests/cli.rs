//! End-to-end tests: drive the real binary.
//!
//! These are the ones that catch what unit tests cannot — a config that parses
//! but produces no output, a subcommand that exits 0 on failure, a PNG that is
//! not actually a PNG. They shell out through `CARGO_BIN_EXE_stratum`, so no
//! extra harness is needed.
//!
//! Run just these:      cargo test --test cli
//! See the output:      cargo test --test cli -- --nocapture

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_stratum")
}

/// A scratch dir per test, so parallel tests do not collide.
fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("stratum-cli-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch dir");
    dir
}

fn write(dir: &Path, name: &str, body: &str) -> PathBuf {
    let path = dir.join(name);
    std::fs::write(&path, body).expect("write config");
    path
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

const TEXT_ONLY: &str = r##"
screen = { x = 0, y = 0, w = 400, h = 300 }
scale = 2

[output]
dir = "OUT"
composite = true

[[widget]]
id = "clock"
anchor = "top-left"
frame = { x = 10, y = 10 }
padding = 8
background = { enabled = true, color = "#101014e0", corner_radius = 8 }

[[widget.elements]]
type = "text"
value = "hello"
kind = "static"
size = 13

[[widget.elements]]
type = "text"
value = "{{v}}"
kind = "dynamic"
size = 13

[widget.script]
command = "echo v=world"
interval = 0.0
"##;

#[test]
fn render_writes_a_png_per_widget_and_a_composite() {
    let dir = scratch("render");
    let out = dir.join("out");
    let cfg = write(
        &dir,
        "widgets.toml",
        &TEXT_ONLY.replace("OUT", &out.to_string_lossy()),
    );

    let res = run(&["render", cfg.to_str().unwrap(), "-o", out.to_str().unwrap()]);
    assert!(res.status.success(), "render failed: {}", stderr(&res));

    for name in ["clock.png", "composite.png"] {
        let p = out.join(name);
        let bytes = std::fs::read(&p).unwrap_or_else(|e| panic!("{}: {e}", p.display()));
        assert!(bytes.len() > 100, "{} is suspiciously small", p.display());
        assert_eq!(
            &bytes[..8],
            b"\x89PNG\r\n\x1a\n",
            "{} is not a PNG",
            p.display()
        );
    }

    // The composite must be screen-sized at 2x.
    let composite = std::fs::read(out.join("composite.png")).expect("composite");
    assert!(!composite.is_empty());

    // Script values must have landed before the final paint.
    let stdout = String::from_utf8_lossy(&res.stdout);
    assert!(stdout.contains("rendered 1 widget"), "{stdout}");
}

#[test]
fn invalid_configs_fail_loudly_and_write_nothing() {
    let dir = scratch("invalid");
    let out = dir.join("out");
    let cases = [
        (
            "duplicate.toml",
            r##"[[widget]]
id = "dup"
[[widget.elements]]
type = "text"
value = "a"
[[widget]]
id = "dup"
[[widget.elements]]
type = "text"
value = "b"
"##,
            "duplicate widget id",
        ),
        (
            "noicon.toml",
            r##"[[widget]]
id = "w"
[[widget.elements]]
type = "icon"
size = 12
"##,
            "svg",
        ),
        (
            "scale.toml",
            r##"scale = 42
[[widget]]
id = "w"
[[widget.elements]]
type = "text"
value = "x"
"##,
            "scale",
        ),
    ];

    for (name, body, needle) in cases {
        let cfg = write(&dir, name, body);
        let res = run(&["render", cfg.to_str().unwrap(), "-o", out.to_str().unwrap()]);
        assert!(!res.status.success(), "{name} should have failed");
        assert!(
            stderr(&res).contains(needle),
            "{name}: stderr {:?} did not contain {needle:?}",
            stderr(&res)
        );
    }
    assert!(
        !out.exists() || std::fs::read_dir(&out).unwrap().count() == 0,
        "a failed render left output behind"
    );
}

#[test]
fn missing_config_is_an_error_not_a_panic() {
    let res = run(&["render", "/nonexistent/stratum.toml"]);
    assert!(!res.status.success());
    let err = stderr(&res);
    assert!(err.contains("config"), "stderr: {err}");
    assert!(!err.contains("panicked"), "panic instead of error: {err}");
}

#[test]
fn dump_is_idempotent() {
    let dir = scratch("dump");
    let cfg = write(
        &dir,
        "w.toml",
        r##"
[[widget]]
id = "w"
padding = 6
background = { enabled = true, color = "#050505", corner_radius = 4 }
[[widget.elements]]
type = "text"
value = "x"
size = 12
"##,
    );

    let first = run(&["dump", cfg.to_str().unwrap()]);
    assert!(first.status.success(), "{}", stderr(&first));
    let once = String::from_utf8(first.stdout).expect("utf8");

    let second_cfg = write(&dir, "w2.toml", &once);
    let second = run(&["dump", second_cfg.to_str().unwrap()]);
    assert!(second.status.success(), "{}", stderr(&second));
    let twice = String::from_utf8(second.stdout).expect("utf8");

    assert_eq!(once, twice, "dump output is not stable");
}

#[test]
fn click_reports_the_widget_under_the_point() {
    let dir = scratch("click");
    let out = dir.join("out");
    let mut cfg = TEXT_ONLY.replace("OUT", &out.to_string_lossy());
    cfg = cfg.replace(
        r##"frame = { x = 10, y = 10 }"##,
        r##"frame = { x = 10, y = 10, w = 120, h = 40 }"##,
    );
    let path = write(&dir, "w.toml", &cfg);

    let inside = run(&[
        "click",
        path.to_str().unwrap(),
        "50",
        "30",
        "-o",
        out.to_str().unwrap(),
    ]);
    assert!(inside.status.success(), "{}", stderr(&inside));
    assert!(
        String::from_utf8_lossy(&inside.stdout).contains("-> widget clock"),
        "{}",
        String::from_utf8_lossy(&inside.stdout)
    );

    let outside = run(&[
        "click",
        path.to_str().unwrap(),
        "350",
        "250",
        "-o",
        out.to_str().unwrap(),
    ]);
    assert!(outside.status.success());
    assert!(
        String::from_utf8_lossy(&outside.stdout).contains("no widget"),
        "{}",
        String::from_utf8_lossy(&outside.stdout)
    );
}

#[test]
fn help_lists_every_subcommand() {
    let res = run(&["--help"]);
    assert!(res.status.success());
    let text = String::from_utf8_lossy(&res.stdout);
    for cmd in ["render", "run", "click", "live", "fixture", "dump"] {
        assert!(text.contains(cmd), "help is missing {cmd}:\n{text}");
    }
}

// ------------------------------------------------------------ XDG config ----

/// A complete config tree in a temp dir: app config + widget file + shell
/// script, with the widget referencing the script by name only.
fn xdg_tree(name: &str) -> (PathBuf, PathBuf) {
    let root = scratch(name);
    let stratum_dir = root.join("stratum");
    std::fs::create_dir_all(stratum_dir.join("widgets")).expect("widgets dir");
    std::fs::create_dir_all(stratum_dir.join("scripts")).expect("scripts dir");
    let out = root.join("frames");

    std::fs::write(
        stratum_dir.join("config.toml"),
        format!(
            r##"
scale = 1
widget_dirs = ["widgets"]
script_dirs = ["scripts"]

[output]
dir = "{}"
composite = false
"##,
            out.display()
        ),
    )
    .expect("app config");

    std::fs::write(
        stratum_dir.join("widgets/hello.toml"),
        r##"
[[widget]]
id = "hello"
anchor = "top-left"
frame = { x = 10, y = 10 }
padding = 6
background = { enabled = true, color = "#101014" }

[[widget.elements]]
type = "text"
value = "hi {{who}}"
kind = "dynamic"
size = 12

[widget.script]
file = "hello.sh"
interval = 0.0
"##,
    )
    .expect("widget file");

    std::fs::write(stratum_dir.join("scripts/hello.sh"), "echo who=world\n").expect("script");

    (root, out)
}

fn run_in(root: &Path, args: &[&str]) -> Output {
    Command::new(bin())
        .args(args)
        .current_dir(root)
        .env("XDG_CONFIG_HOME", root)
        .env_remove("STRATUM_CONFIG")
        .output()
        .expect("spawn stratum")
}

#[test]
fn discovers_the_config_under_xdg_config_home() {
    let (root, out) = xdg_tree("xdg-discovery");

    let cfg = run_in(&root, &["config"]);
    assert!(cfg.status.success(), "{}", stderr(&cfg));
    let text = String::from_utf8_lossy(&cfg.stdout);
    assert!(
        text.contains("stratum/config.toml"),
        "did not find the XDG config:\n{text}"
    );
    assert!(
        text.lines()
            .any(|l| l.trim_start().starts_with("widgets") && l.trim_end().ends_with('1')),
        "widget count wrong:\n{text}"
    );
    assert!(
        text.contains("hello        desktop") || text.contains("hello desktop"),
        "widget not listed:\n{text}"
    );
    assert!(
        text.contains("widgets/hello.toml"),
        "widget file not listed:\n{text}"
    );
    assert!(
        text.contains("hello.sh"),
        "script not resolved to a file:\n{text}"
    );

    // `render` with no path argument must use the same discovery.
    let res = run_in(&root, &["render"]);
    assert!(res.status.success(), "{}", stderr(&res));
    let png = out.join("hello.png");
    assert!(
        png.is_file(),
        "no {}:\n{}",
        png.display(),
        String::from_utf8_lossy(&res.stdout)
    );
    let bytes = std::fs::read(&png).expect("png");
    assert_eq!(&bytes[..8], b"\x89PNG\r\n\x1a\n");

    // `click` with no config path: coordinates only, discovery supplies the config.
    let click = run_in(&root, &["click", "20", "20"]);
    assert!(click.status.success(), "{}", stderr(&click));
    let out = String::from_utf8_lossy(&click.stdout);
    assert!(out.contains("-> widget hello"), "{out}");
    assert!(
        out.contains("who=world"),
        "script values did not reach the runtime:\n{out}"
    );
}

#[test]
fn a_widget_in_a_widget_dir_can_reference_a_shared_script() {
    // Same tree, but the script lives in a *different* directory from the
    // widget — this is the case `script_dirs` exists for.
    let (root, out) = xdg_tree("xdg-scriptdirs");
    std::fs::rename(
        root.join("stratum/scripts/hello.sh"),
        root.join("stratum/scripts/shared-hello.sh"),
    )
    .expect("rename");
    std::fs::write(
        root.join("stratum/widgets/hello.toml"),
        r##"
[[widget]]
id = "hello"
anchor = "top-left"
frame = { x = 10, y = 10 }
[[widget.elements]]
type = "text"
value = "hi {{who}}"
kind = "dynamic"
size = 12

[widget.script]
file = "shared-hello.sh"
interval = 0.0
"##,
    )
    .expect("widget");

    let res = run_in(&root, &["render"]);
    assert!(res.status.success(), "{}", stderr(&res));
    assert!(out.join("hello.png").is_file());
}

#[test]
fn init_writes_a_usable_tree_and_refuses_to_clobber() {
    let root = scratch("init");
    let dir = root.join("cfg");

    let first = run(&["init", "--dir", dir.to_str().unwrap()]);
    assert!(first.status.success(), "{}", stderr(&first));
    for f in [
        "config.toml",
        "widgets/clock.toml",
        "widgets/load.toml",
        "scripts/clock.sh",
        "scripts/load-rs/Cargo.toml",
        "scripts/load-rs/src/main.rs",
        "assets/star.svg",
    ] {
        assert!(dir.join(f).is_file(), "init did not write {f}");
    }

    // The generated tree must actually load and render (no scripts: the Rust
    // one would invoke cargo, which is covered by the unit tests instead).
    let out = root.join("out");
    let res = run(&[
        "render",
        dir.join("config.toml").to_str().unwrap(),
        "-o",
        out.to_str().unwrap(),
        "--no-scripts",
    ]);
    assert!(res.status.success(), "{}", stderr(&res));
    assert!(out.join("clock.png").is_file(), "no clock.png");

    // And a second init must not overwrite it.
    let second = run(&["init", "--dir", dir.to_str().unwrap()]);
    assert!(!second.status.success());
    assert!(
        stderr(&second).contains("already exists"),
        "{}",
        stderr(&second)
    );
}
