//! Config loading and discovery.
//!
//! Layout this is built for (the XDG one, on macOS too):
//!
//! ```text
//! $XDG_CONFIG_HOME/stratum/config.toml     # or ~/.config/stratum/config.toml
//!   |- app-level settings: scale, screen, fonts, output, dirs
//!   |- widget_dirs = ["widgets", "~/dotfiles/widgets"]
//!   `- script_dirs = ["scripts"]
//! $XDG_CONFIG_HOME/stratum/widgets/clock.toml
//! $XDG_CONFIG_HOME/stratum/scripts/clock.sh
//! ```
//!
//! `config.toml` may also be called `.config` or `config` — the dotfile spelling
//! is searched because people ask for it. `$STRATUM_CONFIG` overrides everything.
//!
//! A widget file is an ordinary config fragment: it may contain `[[widget]]`
//! blocks and nothing else. Relative `script.file` and icon paths resolve
//! against the file that declared them, so a widget directory is movable.

use crate::model::{Element, Frame, WidgetSpec};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    /// The logical screen used to resolve `anchor` for display 0 when no real
    /// display information is available (headless rendering). `stratum live`
    /// fills `screens` from the actual displays instead.
    #[serde(default = "default_screen")]
    pub screen: Frame,
    /// Display rects in one shared top-left-origin space, index = `WidgetSpec`
    /// `display`. Empty means "just `screen`". Filled automatically by the live
    /// presenter; can be set by hand for headless multi-display renders.
    #[serde(default)]
    pub screens: Vec<Frame>,
    /// Tween duration for graph values, in milliseconds. 0 disables animation.
    #[serde(default = "default_animation_ms")]
    pub animation_ms: f64,
    /// Device pixels per point. 2.0 matches sketchybar's hardcoded
    /// `contentsScale` / `SLSSetSurfaceResolution(2.0)`.
    #[serde(default = "default_scale")]
    pub scale: f32,
    /// Named font aliases: `[fonts] default = "/System/Library/Fonts/SFNS.ttf"`.
    #[serde(default)]
    pub fonts: BTreeMap<String, String>,
    /// Widgets declared inline in this file.
    #[serde(default, rename = "widget")]
    pub widgets: Vec<WidgetSpec>,
    /// Directories scanned for `*.toml` widget files. Relative entries resolve
    /// against this config file's directory; `~` is expanded.
    #[serde(default)]
    pub widget_dirs: Vec<String>,
    /// Where a widget's `script.file` is looked up when it is not beside the
    /// widget file. Relative entries resolve against this config's directory.
    #[serde(default)]
    pub script_dirs: Vec<String>,
    /// Output settings for the headless presenter.
    #[serde(default)]
    pub output: OutputCfg,
    /// Where this config was loaded from (not serialised).
    #[serde(skip)]
    pub source: Option<PathBuf>,
    /// Every file that contributed widgets, in load order (not serialised).
    #[serde(skip)]
    pub loaded_files: Vec<PathBuf>,
    /// `widget_dirs` / `script_dirs` after expansion (not serialised).
    #[serde(skip)]
    pub resolved_widget_dirs: Vec<PathBuf>,
    #[serde(skip)]
    pub resolved_script_dirs: Vec<PathBuf>,
}

/// Unknown keys are rejected: TOML silently reparents a bare key that follows a
/// `[table]` header, so `widget_dirs = [...]` written below `[output]` would
/// otherwise be swallowed without a word. This error names the offender.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OutputCfg {
    #[serde(default = "default_out_dir")]
    pub dir: String,
    /// Also write a full-screen composite with every widget placed, which is
    /// the closest thing to a screenshot this headless presenter can give.
    #[serde(default = "default_true")]
    pub composite: bool,
}

fn default_true() -> bool {
    true
}
fn default_out_dir() -> String {
    "out".to_string()
}
fn default_screen() -> Frame {
    Frame::new(0.0, 0.0, 1512.0, 982.0)
}
fn default_scale() -> f32 {
    2.0
}
fn default_animation_ms() -> f64 {
    180.0
}

impl Default for OutputCfg {
    fn default() -> Self {
        OutputCfg {
            dir: default_out_dir(),
            composite: true,
        }
    }
}

impl Default for Config {
    fn default() -> Self {
        Config {
            screen: default_screen(),
            screens: Vec::new(),
            animation_ms: default_animation_ms(),
            scale: default_scale(),
            fonts: BTreeMap::new(),
            widgets: Vec::new(),
            widget_dirs: Vec::new(),
            script_dirs: Vec::new(),
            output: OutputCfg::default(),
            source: None,
            loaded_files: Vec::new(),
            resolved_widget_dirs: Vec::new(),
            resolved_script_dirs: Vec::new(),
        }
    }
}

// ----------------------------------------------------------------- discovery ----

/// File names tried inside the config directory, in order.
const CONFIG_NAMES: &[&str] = &["config.toml", ".config", "config"];

/// Config directory: `$XDG_CONFIG_HOME/stratum`, else `~/.config/stratum`.
pub fn config_dir() -> Option<PathBuf> {
    if let Ok(xdg) = std::env::var("XDG_CONFIG_HOME")
        && !xdg.trim().is_empty()
    {
        return Some(PathBuf::from(xdg).join("stratum"));
    }
    std::env::var("HOME")
        .ok()
        .filter(|h| !h.trim().is_empty())
        .map(|h| PathBuf::from(h).join(".config").join("stratum"))
}

/// Every path that would be considered, in order, for an optional explicit path.
pub fn search_paths(explicit: Option<&Path>) -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Some(p) = explicit {
        out.push(p.to_path_buf());
    } else if let Some(dir) = config_dir() {
        for name in CONFIG_NAMES {
            out.push(dir.join(name));
        }
    }
    out
}

/// Resolve the config to use.
///
/// Order: `$STRATUM_CONFIG` (must exist), the explicit argument, then the XDG
/// directory. An explicit path that does not exist is an error — silently
/// falling back would hide a typo.
pub fn discover(explicit: Option<&Path>) -> Result<PathBuf, String> {
    if let Some(p) = explicit {
        return if p.is_file() {
            Ok(p.to_path_buf())
        } else {
            Err(format!("config {}: no such file", p.display()))
        };
    }

    if let Ok(from_env) = std::env::var("STRATUM_CONFIG")
        && !from_env.trim().is_empty()
    {
        let p = PathBuf::from(from_env);
        return if p.is_file() {
            Ok(p)
        } else {
            Err(format!(
                "$STRATUM_CONFIG points at {}, which does not exist",
                p.display()
            ))
        };
    }

    let candidates = search_paths(None);
    for c in &candidates {
        if c.is_file() {
            return Ok(c.clone());
        }
    }

    let listed = if candidates.is_empty() {
        "(no $XDG_CONFIG_HOME and no $HOME — cannot guess a config directory)".to_string()
    } else {
        candidates
            .iter()
            .map(|p| format!("  {}", p.display()))
            .collect::<Vec<_>>()
            .join("\n")
    };
    Err(format!(
        "no config found. Looked in:\n{listed}\n\
         Create a starter tree with `stratum init`, or pass a config path."
    ))
}

// -------------------------------------------------------------------- loading ----

impl Config {
    /// Load a config file and everything it points at.
    pub fn load(path: &Path) -> Result<Config, String> {
        let mut cfg = parse_file(path)?;
        let base = path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(Path::new("."))
            .to_path_buf();
        cfg.source = Some(path.to_path_buf());
        cfg.loaded_files = vec![path.to_path_buf()];
        cfg.expand(&base)?;
        cfg.validate()?;
        Ok(cfg)
    }

    /// Resolve dirs, pull in widget files, and make script/icon paths absolute.
    fn expand(&mut self, base: &Path) -> Result<(), String> {
        self.resolved_widget_dirs = self
            .widget_dirs
            .iter()
            .map(|d| expand_path(d, base))
            .collect();
        self.resolved_script_dirs = self
            .script_dirs
            .iter()
            .map(|d| expand_path(d, base))
            .collect();

        // Widgets declared in the root file resolve against the root.
        for w in &mut self.widgets {
            resolve_widget_paths(w, base, &self.resolved_script_dirs);
        }

        // Widget files, in a stable order so `dump` and errors are reproducible.
        let mut files = Vec::new();
        for dir in &self.resolved_widget_dirs {
            if !dir.is_dir() {
                return Err(format!(
                    "widget_dirs entry {} is not a directory",
                    dir.display()
                ));
            }
            files.extend(toml_files(dir)?);
        }
        files.sort();

        let mut from_files = Vec::new();
        for file in files {
            let mut sub = parse_file(&file)?;
            if !sub.widget_dirs.is_empty() || !sub.script_dirs.is_empty() {
                return Err(format!(
                    "{}: a widget file may not declare widget_dirs or script_dirs \
                     (put those in the top-level config)",
                    file.display()
                ));
            }
            let dir = file.parent().unwrap_or(Path::new(".")).to_path_buf();
            for w in &mut sub.widgets {
                resolve_widget_paths(w, &dir, &self.resolved_script_dirs);
            }
            if sub.widgets.is_empty() {
                return Err(format!("{}: contains no [[widget]] blocks", file.display()));
            }
            from_files.extend(sub.widgets);
            self.loaded_files.push(file);
        }
        self.widgets.extend(from_files);
        Ok(())
    }

    /// Display rects to resolve against, index = `display`.
    pub fn screen_list(&self) -> Vec<Frame> {
        if self.screens.is_empty() {
            vec![self.screen]
        } else {
            self.screens.clone()
        }
    }

    /// Screen for a widget's `display`, clamped to what exists so a typo shows
    /// the widget on the last display instead of making it disappear.
    pub fn screen_for(&self, display: u32) -> Frame {
        // Indexed, no Vec clone: this is called once per widget per repaint.
        if self.screens.is_empty() {
            return self.screen;
        }
        let i = (display as usize).min(self.screens.len() - 1);
        self.screens[i]
    }

    pub fn validate(&self) -> Result<(), String> {
        let mut seen = std::collections::HashSet::new();
        for w in &self.widgets {
            if !seen.insert(w.id.clone()) {
                return Err(format!(
                    "duplicate widget id {:?} (check {} and the widget files)",
                    w.id,
                    self.source
                        .as_ref()
                        .map(|p| p.display().to_string())
                        .unwrap_or_else(|| "the config".into())
                ));
            }
            if w.id.trim().is_empty() {
                return Err("a widget needs a non-empty `id`".into());
            }
            if w.elements.is_empty() {
                return Err(format!("widget {:?} has no elements", w.id));
            }
            for el in &w.elements {
                if let Element::Graph(g) = el
                    && !(0.0..=1.0).contains(&g.weight)
                    && g.weight != 1.0
                {
                    let hint = if g.weight > 1.0 && g.weight <= 100.0 {
                        format!(" — did you mean {}?", g.weight / 100.0)
                    } else {
                        String::new()
                    };
                    return Err(format!(
                        "widget {:?}: donut weight {} out of range 0.0..=1.0{hint}",
                        w.id, g.weight
                    ));
                }
                if let Element::Icon(i) = el
                    && i.svg.is_none()
                    && i.bitmap.is_none()
                {
                    return Err(format!(
                        "widget {:?}: icon element needs `svg` or `bitmap`",
                        w.id
                    ));
                }
            }
        }
        if !(0.0..=8.0).contains(&self.scale) {
            return Err(format!("scale {} out of range 0..=8", self.scale));
        }
        Ok(())
    }
}

/// A config is hand-written; anything this large is not a config. Refusing early
/// keeps a swapped-in `/dev/zero` or a runaway generated file from eating memory
/// (the daemon re-reads configs on every hot reload).
const MAX_CONFIG_BYTES: u64 = 4 * 1024 * 1024;

fn parse_file(path: &Path) -> Result<Config, String> {
    let len = std::fs::metadata(path)
        .map_err(|e| format!("config {}: {e}", path.display()))?
        .len();
    if len > MAX_CONFIG_BYTES {
        return Err(format!(
            "config {}: {len} bytes is larger than the {MAX_CONFIG_BYTES}-byte limit",
            path.display()
        ));
    }
    let text =
        std::fs::read_to_string(path).map_err(|e| format!("config {}: {e}", path.display()))?;
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("toml")
        .to_ascii_lowercase();
    let msg = if ext == "json" {
        match serde_json::from_str::<Config>(&text) {
            Ok(c) => return Ok(c),
            Err(e) => e.to_string(),
        }
    } else {
        match toml::from_str::<Config>(&text) {
            Ok(c) => return Ok(c),
            Err(e) => e.to_string(),
        }
    };
    // The most common config mistake: commenting out every line of an element
    // block but keeping the `[[widget.elements]]` header.
    let hint = if msg.contains("missing field `type`") {
        "\n  hint: a [[widget.elements]] block needs a `type` — an empty block (every line commented out) fails to parse"
    } else {
        ""
    };
    Err(format!("config {}: {msg}{hint}", path.display()))
}

/// `*.toml` in a directory, one level deep (subdirectories are walked too, so
/// `widgets/desk/clock.toml` works).
fn toml_files(dir: &Path) -> Result<Vec<PathBuf>, String> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        let entries = std::fs::read_dir(&d).map_err(|e| format!("{}: {e}", d.display()))?;
        for entry in entries {
            let entry = entry.map_err(|e| format!("{}: {e}", d.display()))?;
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if path
                .extension()
                .and_then(|e| e.to_str())
                .is_some_and(|e| e.eq_ignore_ascii_case("toml"))
            {
                out.push(path);
            }
        }
    }
    Ok(out)
}

/// Expand `~` and make relative paths absolute against `base`.
pub fn expand_path(raw: &str, base: &Path) -> PathBuf {
    let trimmed = raw.trim();
    if let Some(rest) = trimmed.strip_prefix("~/")
        && let Ok(home) = std::env::var("HOME")
    {
        return PathBuf::from(home).join(rest);
    }
    if trimmed == "~"
        && let Ok(home) = std::env::var("HOME")
    {
        return PathBuf::from(home);
    }
    let p = PathBuf::from(trimmed);
    if p.is_absolute() { p } else { base.join(p) }
}

/// Point a widget's relative script and icon paths at real files.
///
/// Search order: the widget's own directory, the config's `script_dirs`, then
/// the current directory (so configs written before this existed keep working).
fn resolve_widget_paths(w: &mut WidgetSpec, base: &Path, script_dirs: &[PathBuf]) {
    if let Some(spec) = &mut w.script
        && let Some(file) = spec.file.as_ref()
        && !file.is_absolute()
    {
        let mut tried = vec![base.join(file)];
        tried.extend(script_dirs.iter().map(|d| d.join(file)));
        tried.push(file.to_path_buf());
        if let Some(found) = tried.iter().find(|p| p.is_file()) {
            // Absolute, so a script still runs when the child's cwd is its own
            // directory. A relative config path would otherwise break the spawn.
            spec.file = Some(std::path::absolute(found).unwrap_or_else(|_| found.clone()));
        }
        // Not found: leave it relative so the error names what was asked for.
    }

    for el in &mut w.elements {
        let Element::Icon(icon) = el else { continue };
        for slot in [&mut icon.svg, &mut icon.bitmap] {
            let Some(path) = slot.as_ref() else { continue };
            if path.is_absolute() {
                continue;
            }
            let mut tried = vec![base.join(path)];
            tried.extend(script_dirs.iter().map(|d| d.join(path)));
            tried.push(path.clone());
            if let Some(found) = tried.iter().find(|p| p.is_file()) {
                *slot = Some(std::path::absolute(found).unwrap_or_else(|_| found.clone()));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn minimal_config_parses() {
        let src = r##"
screen = { x = 0, y = 0, w = 1440, h = 900 }
scale = 2

[[widget]]
id = "clock"
anchor = "top-right"
frame = { x = 20, y = 20 }
padding = 8
gap = 6
layout = "row"
background = { enabled = true, color = "#101014e0", corner_radius = 10 }

[[widget.elements]]
type = "icon"
svg = "examples/star.svg"
size = 14
tint = "#ffffff"

[[widget.elements]]
type = "text"
value = "{{time}}"
kind = "dynamic"
size = 13
"##;
        let cfg: Config = toml::from_str(src).expect("parse");
        cfg.validate().expect("validate");
        assert_eq!(cfg.widgets.len(), 1);
        assert_eq!(cfg.widgets[0].elements.len(), 2);
        assert_eq!(cfg.widgets[0].padding_px().left, 8.0);
    }

    /// Restore a captured env var. Tests here mutate `XDG_CONFIG_HOME` and put it
    /// back before returning; they are the only threads touching it, so the
    /// single-threaded discipline `set_var` wants is satisfied.
    fn restore_env(saved: Option<String>) {
        // SAFETY: called from the same test that set the variable, with the value
        // it captured; no other thread reads or writes this variable.
        unsafe {
            match saved {
                Some(v) => std::env::set_var("XDG_CONFIG_HOME", v),
                None => std::env::remove_var("XDG_CONFIG_HOME"),
            }
        }
    }

    fn cfg_with(src: &str) -> Result<Config, String> {
        let c: Config = toml::from_str(src).map_err(|e| e.to_string())?;
        c.validate()?;
        Ok(c)
    }

    #[test]
    fn rejects_duplicate_widget_ids() {
        let err = cfg_with(
            r##"
[[widget]]
id = "same"
[[widget.elements]]
type = "text"
value = "a"

[[widget]]
id = "same"
[[widget.elements]]
type = "text"
value = "b"
"##,
        )
        .unwrap_err();
        assert!(err.contains("duplicate widget id"), "{err}");
    }

    #[test]
    fn rejects_widget_without_elements() {
        let err = cfg_with(
            r##"
[[widget]]
id = "empty"
"##,
        )
        .unwrap_err();
        assert!(err.contains("no elements"), "{err}");
    }

    #[test]
    fn rejects_icon_without_source() {
        let err = cfg_with(
            r##"
[[widget]]
id = "w"
[[widget.elements]]
type = "icon"
size = 12
"##,
        )
        .unwrap_err();
        assert!(err.contains("svg") && err.contains("bitmap"), "{err}");
    }

    #[test]
    fn rejects_out_of_range_scale() {
        let err = cfg_with(
            r##"
scale = 99
[[widget]]
id = "w"
[[widget.elements]]
type = "text"
value = "x"
"##,
        )
        .unwrap_err();
        assert!(err.contains("scale"), "{err}");
    }

    #[test]
    fn widgets_belong_to_the_desktop_unless_they_opt_in() {
        let c = cfg_with(
            r##"
[[widget]]
id = "behind"
[[widget.elements]]
type = "text"
value = "x"

[[widget]]
id = "above"
always_on_top = true
[[widget.elements]]
type = "text"
value = "y"
"##,
        )
        .expect("config");
        assert!(!c.widgets[0].always_on_top, "default must be desktop level");
        assert!(c.widgets[0].click_through, "default must be click-through");
        assert!(c.widgets[1].always_on_top);
    }

    #[test]
    fn accepts_every_colour_syntax() {
        let c = cfg_with(
            r##"
[[widget]]
id = "w"
background = { enabled = true, color = "#abc" }
[[widget.elements]]
type = "text"
value = "x"
color = "#112233"
background = { enabled = true, color = "0x11223344" }
"##,
        )
        .expect("colours parse");
        assert_eq!(c.widgets[0].elements.len(), 1);
    }

    #[test]
    fn round_trips_through_toml() {
        let src = std::fs::read_to_string("examples/widgets.toml").expect("example config");
        let first: Config = toml::from_str(&src).expect("parse");
        first.validate().expect("validate");
        let text = toml::to_string_pretty(&first).expect("serialise");
        let second: Config = toml::from_str(&text).expect("re-parse");
        assert_eq!(first.widgets.len(), second.widgets.len());
        assert_eq!(second.widgets.len(), 7);
        let text2 = toml::to_string_pretty(&second).expect("serialise twice");
        assert_eq!(text, text2);
    }

    #[test]
    fn anchor_resolution() {
        use crate::model::Anchor;
        let screen = Frame::new(0.0, 0.0, 1000.0, 500.0);
        let f = Anchor::TopRight.resolve(screen, Frame::new(20.0, 20.0, 0.0, 0.0), (100.0, 30.0));
        assert_eq!(f.x, 880.0);
        assert_eq!(f.y, 20.0);
        let f =
            Anchor::BottomCenter.resolve(screen, Frame::new(0.0, 10.0, 0.0, 0.0), (200.0, 40.0));
        assert_eq!(f.x, 400.0);
        assert_eq!(f.y, 450.0);
    }

    // ------------------------------------------------------- discovery ----

    #[test]
    fn searches_the_xdg_dir_then_the_dot_config_spellings() {
        let tmp = std::env::temp_dir().join(format!("stratum-discover-{}", std::process::id()));
        let dir = tmp.join("stratum");
        std::fs::create_dir_all(&dir).expect("mkdir");
        // `.config` is searched after `config.toml`, so with only `.config`
        // present that is what discovery must pick.
        std::fs::write(dir.join(".config"), "[[widget]]\nid='x'\n").expect("write");

        let saved = std::env::var("XDG_CONFIG_HOME").ok();
        // SAFETY: this test mutates a process-global and restores it below. Tests
        // that read XDG_CONFIG_HOME are serialised by running in one thread; the
        // variable is put back before the test returns.
        unsafe { std::env::set_var("XDG_CONFIG_HOME", &tmp) };
        let found = discover(None).expect("discover");
        assert_eq!(found, dir.join(".config"));
        restore_env(saved);
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn an_explicit_path_wins_and_must_exist() {
        let tmp = std::env::temp_dir().join(format!("stratum-explicit-{}", std::process::id()));
        std::fs::create_dir_all(&tmp).expect("mkdir");
        let cfg = tmp.join("mine.toml");
        std::fs::write(&cfg, "").expect("write");

        assert_eq!(discover(Some(&cfg)).expect("found"), cfg);
        let err = discover(Some(&tmp.join("nope.toml"))).unwrap_err();
        assert!(err.contains("no such file"), "{err}");
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn missing_config_lists_what_was_searched() {
        let tmp = std::env::temp_dir().join(format!("stratum-missing-{}", std::process::id()));
        std::fs::create_dir_all(&tmp).expect("mkdir");
        let saved = std::env::var("XDG_CONFIG_HOME").ok();
        // SAFETY: this test mutates a process-global and restores it below. Tests
        // that read XDG_CONFIG_HOME are serialised by running in one thread; the
        // variable is put back before the test returns.
        unsafe { std::env::set_var("XDG_CONFIG_HOME", &tmp) };
        let err = discover(None).unwrap_err();
        restore_env(saved);
        assert!(err.contains("config.toml"), "{err}");
        assert!(err.contains(&tmp.display().to_string()), "{err}");
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn refuses_an_oversized_config() {
        let dir = std::env::temp_dir().join(format!("stratum-bigcfg-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("mkdir");
        let path = dir.join("big.toml");
        // One comment line past the limit: cheap to create, huge to parse.
        let pad = "x".repeat((MAX_CONFIG_BYTES as usize) + 32);
        std::fs::write(&path, format!("# {pad}\n")).expect("write");

        let err = Config::load(&path).unwrap_err();
        assert!(err.contains("larger than"), "{err}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn expands_tilde_and_relative_paths() {
        let base = Path::new("/tmp/base");
        assert_eq!(
            expand_path("widgets", base),
            PathBuf::from("/tmp/base/widgets")
        );
        assert_eq!(expand_path("/abs/x", base), PathBuf::from("/abs/x"));
        if let Ok(home) = std::env::var("HOME") {
            assert_eq!(expand_path("~/w", base), PathBuf::from(home).join("w"));
        }
    }
}
