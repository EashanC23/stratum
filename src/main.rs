//! stratum — a compositor-backed desktop widget runtime.
//!
//! `stratum render <config>` rasterises every widget once and writes PNGs
//! (headless presenter). `stratum fixture <dir>` writes the demo assets
//! (a bitmap icon and an SVG icon) so the examples run anywhere.
//!
//! The real windowed presenter is not in this crate yet: see HANDOFF.md §6.

use stratum::config::Config;
use stratum::present::{PngPresenter, Presenter};
use stratum::runtime;

use std::path::{Path, PathBuf};

fn usage() {
    println!(
        "stratum {}

usage:
  stratum [command] [config.toml]      # config is optional: XDG discovery is used
  stratum config                       show the resolved config and its search path
  stratum init [--dir DIR]             write a starter config tree
  stratum service install|uninstall|status   run widgets at login (LaunchAgent)
  stratum render [config.toml] [-o <dir>] [--scale N] [--no-scripts] [--event NAME]
  stratum run    [config.toml] [--seconds N] [--fps N] [-o <dir>]
  stratum click  [config.toml] <x> <y> [--event NAME] [-o <dir>]
  stratum live   [config.toml] [--seconds N] [--fps N] [--level L] [--snapshot DIR]
  stratum golden [config.toml] [-o DIR]   render the golden corpus (default tests/golden)
  stratum nowplaying                   print the current track as key=value lines
  stratum fixture [<dir>]        write examples/star.svg + a demo PNG icon
  stratum dump   <config.toml>   print the parsed config back out (round-trip check)

  --level L     level for widgets with always_on_top = true (default 25 = above
                the menu bar; 3 = floating). Everything else sits at desktop level.
  --inject-click X Y   TEST HOOK: push a synthetic click through the mouse queue
  --snapshot D  `live`: write each window's composited pixels to D, then exit
  --event NAME  fire that named event before rendering (must be in a script's `on`)
  --seconds N   run the loop for N seconds instead of forever
  --fps N       loop rate (default 10)
  --scale N     override the device scale from the config
  --no-scripts  do not execute widget scripts; inline data only
  --no-composite  skip the full-screen composite PNG (its 2x canvas is the
                largest allocation in a headless render: ~48 MB at 1512x982)

scripts see STRATUM_EVENT, STRATUM_WIDGET and, on a click, STRATUM_CLICK_X/Y
",
        env!("CARGO_PKG_VERSION")
    );
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some(cmd) = args.first().cloned() else {
        usage();
        std::process::exit(2);
    };

    let result = match cmd.as_str() {
        "-h" | "--help" | "help" => {
            usage();
            return;
        }
        "render" => cmd_render(&args[1..]),
        "run" => cmd_run(&args[1..]),
        "click" => cmd_click(&args[1..]),
        "live" => cmd_live(&args[1..]),
        "golden" => cmd_golden(&args[1..]),
        "nowplaying" => {
            cmd_nowplaying(args.get(1).map(String::as_str));
            Ok(())
        }
        "config" => cmd_config(&args[1..]),
        "init" => cmd_init(&args[1..]),
        "service" => cmd_service(&args[1..]),
        "fixture" => cmd_fixture(&args[1..]),
        "dump" => cmd_dump(&args[1..]),
        other => Err(format!("unknown command {other:?} (try --help)")),
    };

    if let Err(e) = result {
        eprintln!("error: {e}");
        std::process::exit(1);
    }
}

/// Shared CLI options for the commands that render.
#[derive(Clone)]
struct Opts {
    cfg_path: Option<PathBuf>,
    out_dir: Option<String>,
    scale: Option<f32>,
    no_scripts: bool,
    no_composite: bool,
    event: Option<String>,
    seconds: Option<f64>,
    fps: f64,
    level: Option<isize>,
    snapshot: Option<String>,
    search: bool,
    inject_click: Option<(f32, f32)>,
    free: Vec<String>,
}

fn parse_opts(args: &[String]) -> Result<Opts, String> {
    let mut o = Opts {
        cfg_path: None,
        out_dir: None,
        scale: None,
        no_scripts: false,
        no_composite: false,
        event: None,
        seconds: None,
        fps: 10.0,
        level: None,
        snapshot: None,
        search: false,
        inject_click: None,
        free: Vec::new(),
    };
    let mut cfg: Option<PathBuf> = None;
    let mut i = 0;
    let num = |v: Option<&String>, what: &str| -> Result<f64, String> {
        v.ok_or(format!("{what} needs a number"))?
            .parse()
            .map_err(|_| format!("{what} expects a number"))
    };
    while i < args.len() {
        match args[i].as_str() {
            "-o" | "--out" => {
                o.out_dir = Some(args.get(i + 1).ok_or("-o needs a directory")?.clone());
                i += 2;
            }
            "--scale" => {
                o.scale = Some(num(args.get(i + 1), "--scale")? as f32);
                i += 2;
            }
            "--seconds" => {
                o.seconds = Some(num(args.get(i + 1), "--seconds")?);
                i += 2;
            }
            "--fps" => {
                o.fps = num(args.get(i + 1), "--fps")?.clamp(0.5, 240.0);
                i += 2;
            }
            "--level" => {
                o.level = Some(num(args.get(i + 1), "--level")? as isize);
                i += 2;
            }
            "--snapshot" => {
                o.snapshot = Some(
                    args.get(i + 1)
                        .ok_or("--snapshot needs a directory")?
                        .clone(),
                );
                i += 2;
            }
            "--event" => {
                o.event = Some(args.get(i + 1).ok_or("--event needs a name")?.clone());
                i += 2;
            }
            "--search" => {
                o.search = true;
                i += 1;
            }
            "--inject-click" => {
                let x = args.get(i + 1).ok_or("--inject-click needs X Y")?;
                let y = args.get(i + 2).ok_or("--inject-click needs X Y")?;
                o.inject_click = Some((
                    x.parse().map_err(|_| "X must be a number".to_string())?,
                    y.parse().map_err(|_| "Y must be a number".to_string())?,
                ));
                i += 3;
            }
            "--no-scripts" => {
                o.no_scripts = true;
                i += 1;
            }
            "--no-composite" => {
                o.no_composite = true;
                i += 1;
            }
            other if !other.starts_with('-') && cfg.is_none() => {
                cfg = Some(PathBuf::from(other));
                i += 1;
            }
            other if !other.starts_with('-') => {
                o.free.push(other.to_string());
                i += 1;
            }
            other => return Err(format!("unexpected argument {other:?}")),
        }
    }
    o.cfg_path = cfg;
    Ok(o)
}

fn load(opts: &Opts) -> Result<(Config, String), String> {
    let path = match &opts.cfg_path {
        Some(p) => p.clone(),
        None => stratum::config::discover(None)?,
    };
    let mut cfg = Config::load(&path)?;
    if let Some(s) = opts.scale {
        cfg.scale = s;
    }
    if opts.no_composite {
        cfg.output.composite = false;
    }
    // `render` takes a picture, not a movie: with animation on, the loop would
    // repaint each widget for `animation_ms` and write the file a dozen times.
    cfg.animation_ms = 0.0;
    // Output dir: `-o` wins, else the config's. `~` is expanded and a relative
    // path is taken from the config file's directory, like every other path.
    let base = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let raw_dir = opts
        .out_dir
        .clone()
        .unwrap_or_else(|| cfg.output.dir.clone());
    let dir = stratum::config::expand_path(&raw_dir, base)
        .display()
        .to_string();
    println!(
        "config {}  {} widget(s) from {} file(s)  screen {}x{} @{}x",
        path.display(),
        cfg.widgets.len(),
        cfg.loaded_files.len(),
        cfg.screen.w,
        cfg.screen.h,
        cfg.scale
    );
    Ok((cfg, dir))
}

fn print_frames(painted: &[(String, stratum::model::Frame)]) {
    for (id, f) in painted {
        println!("  {:<16} {}x{} at ({}, {})", id, f.w, f.h, f.x, f.y);
    }
}

fn cmd_render(args: &[String]) -> Result<(), String> {
    let opts = parse_opts(args)?;
    let (cfg, dir) = load(&opts)?;

    let mut rt = runtime::Runtime::new(cfg.clone(), !opts.no_scripts)?;
    if let Some(ev) = &opts.event {
        rt.emit(ev);
    }
    let mut presenter =
        PngPresenter::new(Path::new(&dir), cfg.screen, cfg.scale, cfg.output.composite);

    let start = std::time::Instant::now();
    let painted = rt.tick_blocking(&mut presenter, std::time::Duration::from_secs(10))?;
    print_frames(&painted);

    let extra = presenter.finish()?;
    println!(
        "rendered {} widget(s) in {}ms -> {dir}",
        rt.config().widgets.len(),
        start.elapsed().as_millis()
    );
    for p in extra {
        println!("  composite: {}", p.display());
    }
    Ok(())
}

/// Long-running loop. Repaints only what changed; prints a line per repaint so
/// the worker/event behaviour is visible from a terminal.
fn cmd_run(args: &[String]) -> Result<(), String> {
    let opts = parse_opts(args)?;
    let (cfg, dir) = load(&opts)?;

    let mut rt = runtime::Runtime::new(cfg.clone(), !opts.no_scripts)?;
    if let Some(ev) = &opts.event {
        rt.emit(ev);
    }
    let mut presenter =
        PngPresenter::new(Path::new(&dir), cfg.screen, cfg.scale, cfg.output.composite);

    let period = std::time::Duration::from_secs_f64(1.0 / opts.fps);
    let deadline = opts
        .seconds
        .map(|s| std::time::Instant::now() + std::time::Duration::from_secs_f64(s));
    let mut ticks = 0u64;
    let mut repaints = 0u64;
    // `cfg` is not needed after this point; move it rather than cloning.
    let mut effective = cfg;
    let mut watcher = Watcher::new(&effective);
    if let Some((x, y)) = opts.inject_click {
        stratum::live::inject_click(x, y);
        println!("injected click at ({x}, {y})");
    }
    let cfg_path = opts
        .cfg_path
        .clone()
        .or_else(|| stratum::config::discover(None).ok());

    loop {
        let cycle_start = std::time::Instant::now();
        let painted = rt.tick(&mut presenter)?;
        if !painted.is_empty() {
            repaints += 1;
            let names: Vec<&str> = painted.iter().map(|(id, _)| id.as_str()).collect();
            println!("tick {ticks}: painted {}", names.join(", "));
        }
        ticks += 1;

        // Hot reload: pick up edits without a restart.
        if watcher.changed(&effective)
            && let Some(path) = &cfg_path
        {
            match Config::load(path) {
                Ok(fresh) => {
                    let removed = rt.reload(fresh);
                    if !removed.is_empty() {
                        println!("  removed: {}", removed.join(", "));
                    }
                    // reload moved the config into the runtime; take it back for
                    // the watcher (one clone per reload, not per tick).
                    effective = rt.config().clone();
                    watcher = Watcher::new(&effective);
                    println!("reloaded config ({} widget(s))", effective.widgets.len());
                }
                Err(e) => eprintln!("reload failed, keeping the old config: {e}"),
            }
        }

        if deadline.is_some_and(|d| std::time::Instant::now() >= d) {
            break;
        }
        let spent = cycle_start.elapsed();
        if spent < period {
            std::thread::sleep(period - spent);
        }
    }

    let _ = presenter.finish()?;
    println!(
        "{ticks} ticks, {repaints} repaint pass(es), {} script(s) still in flight",
        rt.scripts_inflight()
    );
    Ok(())
}

/// Hit-test one point and deliver a click, then drain the resulting script run.
fn cmd_click(args: &[String]) -> Result<(), String> {
    let opts = parse_opts(args)?;

    // Positionals are ambiguous by nature: `click [config] x y`. Take the last
    // two as coordinates and anything before them as the config path.
    let mut pos: Vec<String> = Vec::new();
    if let Some(p) = &opts.cfg_path {
        pos.push(p.display().to_string());
    }
    pos.extend(opts.free.clone());
    if pos.len() < 2 {
        return Err("click needs <x> <y>".into());
    }
    let y: f32 = pos
        .pop()
        .ok_or("click needs <x> <y>")?
        .parse()
        .map_err(|_| "y must be a number")?;
    let x: f32 = pos
        .pop()
        .ok_or("click needs <x> <y>")?
        .parse()
        .map_err(|_| "x must be a number")?;

    let mut opts = opts;
    opts.cfg_path = pos.pop().map(PathBuf::from);
    let (cfg, dir) = load(&opts)?;

    let mut rt = runtime::Runtime::new(cfg.clone(), !opts.no_scripts)?;
    let mut presenter = PngPresenter::new(Path::new(&dir), cfg.screen, cfg.scale, false);
    // Layout first: hit-testing needs resolved frames.
    rt.tick_blocking(&mut presenter, std::time::Duration::from_secs(15))?;

    let clicked = match rt.click(x, y) {
        Some(id) => {
            println!("({x}, {y}) -> widget {id}");
            id
        }
        None => {
            println!("({x}, {y}) -> no widget");
            return Ok(());
        }
    };
    rt.tick_blocking(&mut presenter, std::time::Duration::from_secs(5))?;
    if let Some(v) = rt.values(&clicked) {
        let mut keys: Vec<_> = v.0.iter().collect();
        keys.sort();
        for (k, val) in keys {
            println!("  {k}={val}");
        }
    }
    Ok(())
}

fn cmd_dump(args: &[String]) -> Result<(), String> {
    let path = args.first().ok_or("dump needs a config path")?;
    let cfg = Config::load(Path::new(path))?;
    let s = toml::to_string_pretty(&cfg).map_err(|e| format!("serialise: {e}"))?;
    print!("{s}");
    Ok(())
}

// --------------------------------------------------------------- launchd ----

const SERVICE_LABEL: &str = "dev.stratum.widgets";

fn launch_agents_dir() -> Result<PathBuf, String> {
    let home = std::env::var("HOME").map_err(|_| "no $HOME".to_string())?;
    Ok(PathBuf::from(home).join("Library/LaunchAgents"))
}

fn user_uid() -> Result<String, String> {
    let out = std::process::Command::new("id")
        .arg("-u")
        .output()
        .map_err(|e| format!("id -u: {e}"))?;
    let uid = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if uid.is_empty() {
        Err("could not determine uid".into())
    } else {
        Ok(uid)
    }
}

/// Escape text for XML. Paths can legally contain `&`, `<`, `"`; inserting them
/// raw into a plist would corrupt it and, in the worst case, let a crafted path
/// inject plist keys into a file launchd executes.
fn xml_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            other => out.push(other),
        }
    }
    out
}

fn launchctl(args: &[&str]) -> std::process::Output {
    std::process::Command::new("launchctl")
        .args(args)
        .output()
        .unwrap_or_else(|e| {
            eprintln!("launchctl {:?}: {e}", args);
            std::process::Output {
                status: Default::default(),
                stdout: Vec::new(),
                stderr: Vec::new(),
            }
        })
}

/// `stratum service install|uninstall|status` — run widgets at login and keep
/// them alive, via a per-user LaunchAgent.
///
/// The agent runs the same `live` command you would type, so a config edit is
/// picked up by the hot reload; only a binary upgrade needs a reinstall.
fn cmd_service(args: &[String]) -> Result<(), String> {
    let action = args.first().map(|s| s.as_str()).unwrap_or("status");
    let confirm = args.iter().any(|a| a == "--yes");
    let plist = launch_agents_dir()?.join(format!("{SERVICE_LABEL}.plist"));
    let uid = user_uid()?;
    let domain = format!("gui/{uid}");

    match action {
        "status" => {
            println!("plist    {}", plist.display());
            println!("written  {}", if plist.is_file() { "yes" } else { "no" });
            let out = launchctl(&["print", &format!("{domain}/{SERVICE_LABEL}")]);
            let loaded = out.status.success();
            println!("loaded   {loaded}");
            if loaded {
                let text = String::from_utf8_lossy(&out.stdout);
                for line in text.lines() {
                    let t = line.trim();
                    if t.starts_with("state =")
                        || t.starts_with("pid =")
                        || t.starts_with("last exit")
                    {
                        println!("  {t}");
                    }
                }
            }
            Ok(())
        }
        "install" => {
            if plist.is_file() && !confirm {
                return Err(format!(
                    "{} already exists — `stratum service uninstall` first, or pass --yes to replace",
                    plist.display()
                ));
            }
            let exe = std::env::current_exe().map_err(|e| format!("current_exe: {e}"))?;
            let log_dir = std::env::var("HOME")
                .map(|h| PathBuf::from(h).join(".local/state/stratum"))
                .map_err(|_| "no $HOME".to_string())?;
            std::fs::create_dir_all(&log_dir).map_err(|e| format!("{}: {e}", log_dir.display()))?;
            let agents = launch_agents_dir()?;
            std::fs::create_dir_all(&agents).map_err(|e| format!("{}: {e}", agents.display()))?;

            let xml = format!(
                r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>{SERVICE_LABEL}</string>
    <key>ProgramArguments</key>
    <array>
        <string>{exe}</string>
        <string>live</string>
    </array>
    <key>RunAtLoad</key>
    <true/>
    <key>KeepAlive</key>
    <true/>
    <key>ThrottleInterval</key>
    <integer>10</integer>
    <key>ProcessType</key>
    <string>Interactive</string>
    <key>StandardOutPath</key>
    <string>{log_dir}/widgets.log</string>
    <key>StandardErrorPath</key>
    <string>{log_dir}/widgets.log</string>
</dict>
</plist>
"#,
                exe = xml_escape(&exe.display().to_string()),
                log_dir = xml_escape(&log_dir.display().to_string())
            );
            std::fs::write(&plist, xml).map_err(|e| format!("{}: {e}", plist.display()))?;
            println!("wrote {}", plist.display());

            // Replacing a running agent: bootout is asynchronous, and bootstrap
            // right after it fails with EIO ("Bootstrap failed: 5"). Give the
            // domain a moment, then fall back to the legacy `load -w`.
            let _ = launchctl(&["bootout", &format!("{domain}/{SERVICE_LABEL}")]);
            std::thread::sleep(std::time::Duration::from_millis(300));
            let mut out = launchctl(&["bootstrap", &domain, &plist.to_string_lossy()]);
            if !out.status.success() {
                std::thread::sleep(std::time::Duration::from_millis(500));
                out = launchctl(&["bootstrap", &domain, &plist.to_string_lossy()]);
            }
            if !out.status.success() {
                eprintln!(
                    "bootstrap failed ({}); falling back to `launchctl load -w`",
                    String::from_utf8_lossy(&out.stderr).trim()
                );
                out = launchctl(&["load", "-w", &plist.to_string_lossy()]);
            }
            if !out.status.success() {
                let err = String::from_utf8_lossy(&out.stderr);
                return Err(format!(
                    "could not load the agent: {}\n  the plist is in place; load it manually with\n    launchctl bootstrap {domain} {}",
                    err.trim(),
                    plist.display()
                ));
            }
            println!(
                "loaded as {SERVICE_LABEL} (log: {}/widgets.log)",
                log_dir.display()
            );
            Ok(())
        }
        "uninstall" => {
            let _ = launchctl(&["bootout", &format!("{domain}/{SERVICE_LABEL}")]);
            match std::fs::remove_file(&plist) {
                Ok(()) => println!("removed {}", plist.display()),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                    println!("not installed ({})", plist.display());
                }
                Err(e) => return Err(format!("{}: {e}", plist.display())),
            }
            Ok(())
        }
        other => Err(format!(
            "unknown service action {other:?} (install | uninstall | status)"
        )),
    }
}

// ------------------------------------------------------------- hot reload ----

/// Watches the config and every file it pulled in (plus the widget directories,
/// whose mtime changes when a file is added or removed). Polled once a second;
/// no filesystem-notification crate needed for a handful of paths.
struct Watcher {
    paths: Vec<(PathBuf, Option<std::time::SystemTime>)>,
    last_check: std::time::Instant,
}

impl Watcher {
    fn new(cfg: &Config) -> Self {
        Watcher {
            paths: snapshot(cfg),
            last_check: std::time::Instant::now(),
        }
    }

    /// True at most once per second, when something on disk actually changed.
    fn changed(&mut self, cfg: &Config) -> bool {
        if self.last_check.elapsed() < std::time::Duration::from_secs(1) {
            return false;
        }
        self.last_check = std::time::Instant::now();
        let now = snapshot(cfg);
        if now == self.paths {
            return false;
        }
        self.paths = now;
        true
    }
}

fn watched_paths(cfg: &Config) -> Vec<PathBuf> {
    let mut v = cfg.loaded_files.clone();
    if let Some(s) = &cfg.source
        && !v.contains(s)
    {
        v.push(s.clone());
    }
    v.extend(cfg.resolved_widget_dirs.iter().cloned());
    v.sort();
    v.dedup();
    v
}

fn snapshot(cfg: &Config) -> Vec<(PathBuf, Option<std::time::SystemTime>)> {
    watched_paths(cfg)
        .into_iter()
        .map(|p| {
            let t = std::fs::metadata(&p).ok().and_then(|m| m.modified().ok());
            (p, t)
        })
        .collect()
}

/// `stratum config` — what would run, and where it came from.
fn cmd_config(args: &[String]) -> Result<(), String> {
    let opts = parse_opts(args)?;
    let path = match &opts.cfg_path {
        Some(p) => p.clone(),
        None => stratum::config::discover(None)?,
    };
    let cfg = Config::load(&path)?;

    println!("config      {}", path.display());
    println!(
        "config dir  {}",
        stratum::config::config_dir()
            .map(|d| d.display().to_string())
            .unwrap_or_else(|| "(no $XDG_CONFIG_HOME and no $HOME)".into())
    );
    println!("scale       {}x", cfg.scale);
    println!("screen      {}x{}", cfg.screen.w, cfg.screen.h);
    println!("output      {}", cfg.output.dir);
    println!("widgets     {}", cfg.widgets.len());
    if !cfg.resolved_widget_dirs.is_empty() {
        println!("widget_dirs");
        for d in &cfg.resolved_widget_dirs {
            println!(
                "  {}{}",
                d.display(),
                if d.is_dir() { "" } else { "  (missing)" }
            );
        }
    }
    if !cfg.resolved_script_dirs.is_empty() {
        println!("script_dirs");
        for d in &cfg.resolved_script_dirs {
            println!(
                "  {}{}",
                d.display(),
                if d.is_dir() { "" } else { "  (missing)" }
            );
        }
    }
    println!("files");
    for f in &cfg.loaded_files {
        println!("  {}", f.display());
    }
    println!("widgets:");
    for w in &cfg.widgets {
        let stack = if w.always_on_top { "on-top" } else { "desktop" };
        let script = match &w.script {
            Some(s) => match (&s.file, s.command.is_empty()) {
                (Some(f), _) => f.display().to_string(),
                (None, false) => format!("inline: {}", truncate(&s.command, 40)),
                (None, true) => "(empty)".into(),
            },
            None => "-".into(),
        };
        println!("  {:<12} {:<8} {}", w.id, stack, script);
    }
    if opts.search {
        println!("search path");
        for p in stratum::config::search_paths(None) {
            println!(
                "  {} {}",
                p.display(),
                if p.is_file() { "[found]" } else { "" }
            );
        }
    }
    Ok(())
}

fn truncate(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        s.to_string()
    } else {
        s.chars().take(n).collect::<String>() + "…"
    }
}

/// `stratum init` — a starter tree in the XDG config dir (or `--dir`).
///
/// Deliberately small but *complete*: app settings, one widget file, one shell
/// script, and one Rust script, so all three reference styles exist to copy.
fn cmd_init(args: &[String]) -> Result<(), String> {
    let mut dir: Option<PathBuf> = None;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--dir" | "-d" => {
                dir = Some(PathBuf::from(
                    args.get(i + 1).ok_or("--dir needs a path")?.clone(),
                ));
                i += 2;
            }
            other => return Err(format!("unexpected argument {other:?}")),
        }
    }
    let dir = dir
        .or_else(stratum::config::config_dir)
        .ok_or("cannot guess a config directory; pass --dir")?;

    let cfg = dir.join("config.toml");
    if cfg.exists() {
        return Err(format!(
            "{} already exists — refusing to overwrite (move it away first)",
            cfg.display()
        ));
    }

    let widgets = dir.join("widgets");
    let scripts = dir.join("scripts");
    let rust = scripts.join("load-rs");
    let assets = dir.join("assets");
    for d in [&widgets, &scripts, &rust, &rust.join("src"), &assets] {
        std::fs::create_dir_all(d).map_err(|e| format!("{}: {e}", d.display()))?;
    }

    let write = |path: &Path, body: &str| -> Result<(), String> {
        std::fs::write(path, body).map_err(|e| format!("{}: {e}", path.display()))
    };

    write(
        &cfg,
        &format!(
            r##"# stratum — app-level settings.
#
# Everything here applies to every widget. Widgets themselves live in
# `widget_dirs`, one file each (any *.toml, subdirectories are walked).

scale = {scale}
# Omit `screen` to use the real display in `stratum live`.
# screen = {{ x = 0, y = 0, w = 1440, h = 900 }}

# Bare keys MUST come before any [table] below — TOML assigns everything after
# a [header] to that table, so `widget_dirs` written under [output] would
# silently belong to it (stratum rejects unknown keys there, at least).

# Where widget files are looked up. Relative paths resolve against this file.
widget_dirs = ["widgets"]

# Where a widget's `script.file` is looked up when it is not beside the widget.
script_dirs = ["scripts"]

[output]
dir = "{out}"
composite = true

[fonts]
# Pin a font per alias; `default` is used by anything that does not name one.
# default = "/System/Library/Fonts/SFNS.ttf"
"##,
            scale = 2.0,
            out = "~/.local/state/stratum/frames",
        ),
    )?;

    write(
        &widgets.join("clock.toml"),
        r##"# One widget per file keeps diffs small and ownership obvious.
#
# script.file may be a .sh, .py, .rb, .js, .lua, .pl, or a .rs with a sibling
# Cargo.toml. It runs on `interval` seconds and on any event in `on`.

[[widget]]
id = "clock"
anchor = "top-right"
frame = { x = 24, y = 18 }
padding = 10
gap = 6
layout = "row"
background = { enabled = true, color = "#0e0e12e6", corner_radius = 9 }

[[widget.elements]]
type = "icon"
svg = "../assets/star.svg"
size = 13
tint = "#8f8fa0"

[[widget.elements]]
type = "text"
value = "{{time}}"
kind = "dynamic"
font = "mono"
size = 13
color = "#e6e6f0"

[widget.script]
file = "clock.sh"
interval = 1.0
"##,
    )?;

    write(
        &widgets.join("load.toml"),
        r##"# The same thing in Rust: `scripts/load-rs` is a tiny cargo project that
# prints `key=value` lines. It is compiled on first run (cargo caches it).

[[widget]]
id = "load"
anchor = "bottom-left"
frame = { x = 24, y = 24 }
padding = 10
layout = "column"
gap = 4
background = { enabled = true, color = "#0e0e12cc", corner_radius = 9 }

[[widget.elements]]
type = "graph"
kind = "line"
data_key = "load"
width = 200
height = 40
thickness = 1.5
colors = ["#5ad1a0"]

[[widget.elements]]
type = "text"
value = "load {{load_text}}"
kind = "dynamic"
font = "mono"
size = 11
color = "#5ad1a0"

[widget.script]
file = "load-rs/src/main.rs"
interval = 5.0
"##,
    )?;

    write(
        &scripts.join("clock.sh"),
        r##"#!/bin/sh
# A stratum script: print `key=value` lines. One key per line, no spaces around
# the `=`. Whatever is printed becomes available to the widget's templates.
date '+time=%H:%M:%S'
date '+date=%a %d %b'
"##,
    )?;

    write(
        &rust.join("Cargo.toml"),
        r#"[package]
name = "load-rs"
version = "0.1.0"
edition = "2021"

[[bin]]
name = "load-rs"
path = "src/main.rs"
"#,
    )?;

    write(
        &rust.join("src/main.rs"),
        r#"//! A stratum script in Rust: print `key=value` lines on stdout.
//!
//! Run by the runtime as `cargo run --quiet --manifest-path <this dir>/Cargo.toml`,
//! so it compiles once and is cached. A release build or a prebuilt binary works
//! too — point `file` at the executable and it is run directly.

fn main() {
    let loads = load_average();
    let series = loads
        .iter()
        .map(|v| format!("{v:.2}"))
        .collect::<Vec<_>>()
        .join(",");

    // One key per line. `load` feeds the line graph, `load_text` the label.
    println!("load={series}");
    println!("load_text={:.2}", loads[0]);
}

/// 1/5/15 minute load averages from `getloadavg(3)`.
fn load_average() -> [f64; 3] {
    let mut loads = [0.0f64; 3];
    let n = unsafe { getloadavg(loads.as_mut_ptr(), 3) };
    if n <= 0 { [0.0; 3] } else { loads }
}

unsafe extern "C" {
    fn getloadavg(loadavg: *mut f64, nelem: std::ffi::c_int) -> std::ffi::c_int;
}
"#,
    )?;

    write(
        &assets.join("star.svg"),
        r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" width="24" height="24">
  <path fill="#ffffff" d="M12 1.6l3.2 6.5 7.2 1-5.2 5 1.2 7.1L12 17.9 5.6 21.2l1.2-7.1-5.2-5 7.2-1z"/>
</svg>
"##,
    )?;

    // The shell script should be runnable on its own, which is the point of
    // supporting executables.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let p = scripts.join("clock.sh");
        let mut perms = std::fs::metadata(&p)
            .map_err(|e| e.to_string())?
            .permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&p, perms).map_err(|e| e.to_string())?;
    }

    println!("wrote {}/", dir.display());
    println!("  config.toml");
    println!("  widgets/clock.toml      (script: scripts/clock.sh)");
    println!("  widgets/load.toml       (script: scripts/load-rs/src/main.rs)");
    println!("  scripts/clock.sh");
    println!("  scripts/load-rs/        (tiny cargo project)");
    println!("  assets/star.svg");
    println!();
    println!("try: stratum config && stratum render");
    Ok(())
}

/// `stratum nowplaying` — the system's Now Playing info as `key=value`, ready to
/// be a widget script. Uses MediaRemote (see src/nowplaying.rs), so no
/// third-party binary is involved.
#[cfg(target_os = "macos")]
fn cmd_nowplaying(arg: Option<&str>) {
    use stratum::nowplaying;
    if arg == Some("--why") {
        let d = nowplaying::diagnose();
        println!("api_present={}", d.api_present);
        println!("clients={}", d.clients);
        println!("dict_present={}", d.dict_present);
        println!("playing={}", d.playing);
        println!("verdict={}", d.verdict());
        return;
    }
    match nowplaying::current() {
        Ok(Some(t)) => {
            println!("state={}", if t.playing { "playing" } else { "paused" });
            println!("title={}", t.title);
            println!("artist={}", t.artist);
            println!("album={}", t.album);
            println!("elapsed={:.0}", t.elapsed);
            println!("duration={:.0}", t.duration);
            println!("remaining={:.0}", t.remaining());
            println!("progress={:.0}", t.progress_percent());
            println!("label={}", t.one_line());
            // The play/pause glyph the mono system font can actually draw.
            println!("mark={}", if t.playing { ">" } else { "||" });
        }
        Ok(None) => {
            println!("state=idle");
            println!("label=nothing playing");
            println!("mark=");
        }
        Err(e) => {
            println!("state=error");
            println!("label={e}");
            eprintln!("nowplaying: {e}");
        }
    }
}

/// Non-macOS builds have no MediaRemote.
#[cfg(not(target_os = "macos"))]
fn cmd_nowplaying(_arg: Option<&str>) {
    println!("state=error");
    println!("label=nowplaying is macOS-only (MediaRemote)");
}

/// Render the golden corpus (1x and 2x per widget) into a directory.
///
/// This is the "look at the expectations" command; `cargo test --test golden`
/// is the one that enforces them, and `cargo test --test golden -- --ignored
/// update_goldens` rewrites them.
fn cmd_golden(args: &[String]) -> Result<(), String> {
    let opts = parse_opts(args)?;
    let (cfg, _) = load(&opts)?;
    // Goldens live with the tests, not with the demo output.
    let dir = opts.out_dir.unwrap_or_else(|| "tests/golden".into());
    let text = stratum::golden::text_engine_for(&cfg)?;
    let mut icons = stratum::icon::IconCache::new();
    let files = stratum::golden::write_all(&cfg, Path::new(&dir), &text, &mut icons)?;
    for f in &files {
        let bytes = std::fs::metadata(f).map(|m| m.len()).unwrap_or(0);
        println!("  {:<32} {} bytes", f.display(), bytes);
    }
    println!("{} image(s) in {dir}", files.len());
    Ok(())
}

/// Writes a tiny bitmap icon so the PNG path is exercised end to end, plus an
/// SVG so the vector path is too.
fn cmd_fixture(args: &[String]) -> Result<(), String> {
    let dir = PathBuf::from(args.first().cloned().unwrap_or_else(|| "examples".into()));
    std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;

    // 32x32 device pixels, transparent background, a filled diamond.
    let mut pm = tiny_skia::Pixmap::new(32, 32).ok_or("fixture: allocation failed")?;
    let mut pb = tiny_skia::PathBuilder::new();
    pb.move_to(16.0, 2.0);
    pb.line_to(30.0, 16.0);
    pb.line_to(16.0, 30.0);
    pb.line_to(2.0, 16.0);
    pb.close();
    let path = pb.finish().ok_or("fixture: bad path")?;
    let paint = tiny_skia::Paint {
        shader: tiny_skia::Shader::SolidColor(tiny_skia::Color::from_rgba8(0x6c, 0xc6, 0xff, 0xff)),
        anti_alias: true,
        ..Default::default()
    };
    pm.fill_path(
        &path,
        &paint,
        tiny_skia::FillRule::Winding,
        tiny_skia::Transform::identity(),
        None,
    );
    let png = dir.join("diamond.png");
    pm.save_png(&png)
        .map_err(|e| format!("{}: {e}", png.display()))?;

    let svg = dir.join("star.svg");
    std::fs::write(
        &svg,
        r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" width="24" height="24">
  <path fill="#ffffff" d="M12 1.6l3.2 6.5 7.2 1-5.2 5 1.2 7.1L12 17.9 5.6 21.2l1.2-7.1-5.2-5 7.2-1z"/>
</svg>
"##,
    )
    .map_err(|e| format!("{}: {e}", svg.display()))?;

    println!("wrote {} and {}", png.display(), svg.display());
    Ok(())
}

/// Put widgets on the real desktop as live windows (macOS).
///
/// Public-API path: borderless, clear, above-the-menu-bar NSWindow per widget
/// with a layer-backed view whose `contents` is the widget's pixmap as a
/// CGImage. See src/live.rs and HANDOFF §6a for what the private-API version
/// adds on top.
#[cfg(target_os = "macos")]
fn cmd_live(args: &[String]) -> Result<(), String> {
    let opts = parse_opts(args)?;
    // Under launchd a config error would exit and be restarted immediately, in a
    // tight loop that fills the log. Wait for the file to be fixed instead.
    let (cfg, _dir) = match load(&opts) {
        Ok(v) => v,
        Err(e) if std::env::var_os("STRATUM_NO_WAIT").is_some() => return Err(e),
        Err(e) => {
            eprintln!("{e}");
            eprintln!("waiting for the config to be fixed (ctrl-c to give up)…");
            loop {
                std::thread::sleep(std::time::Duration::from_secs(2));
                match load(&opts) {
                    Ok(v) => break v,
                    Err(e) => eprintln!("{e}"),
                }
            }
        }
    };

    // Real display layout supersedes the config's logical screen: `screens[0]`
    // is the main display, the rest follow NSScreen's order.
    let mut effective = cfg.clone();
    let real = stratum::live::displays();
    if !real.is_empty() {
        if (real[0].w - cfg.screen.w).abs() > 1.0 || (real[0].h - cfg.screen.h).abs() > 1.0 {
            println!(
                "note: config screen {}x{} but the main display is {}x{} — using the display",
                cfg.screen.w, cfg.screen.h, real[0].w, real[0].h
            );
        }
        effective.screen = real[0];
        effective.screens = real.clone();
        if real.len() > 1 {
            println!("displays: {}", real.len());
            for (i, f) in real.iter().enumerate() {
                println!("  [{i}] {}x{} at ({}, {})", f.w, f.h, f.x, f.y);
            }
        }
    }

    // 25 = NSStatusWindowLevel (above the menu bar); 3 = floating under it.
    // Opt-in level for `always_on_top` widgets; desktop level is the default.
    let level = opts.level.unwrap_or(stratum::live::STATUS_LEVEL);
    let mut presenter =
        stratum::live::MacPresenter::new(effective.screens.clone(), effective.scale, level)?;
    for w in &effective.widgets {
        presenter.set_click_through(&w.id, w.click_through);
        presenter.set_always_on_top(&w.id, w.always_on_top);
    }
    let mut rt = runtime::Runtime::new(effective.clone(), !opts.no_scripts)?;
    if let Some(ev) = &opts.event {
        rt.emit(ev);
    }

    println!(
        "live: {} widget(s) on {}x{} @{}x, desktop level {} / on-top level {level} (ctrl-c to quit)",
        effective.widgets.len(),
        effective.screen.w,
        effective.screen.h,
        effective.scale,
        stratum::live::DESKTOP_ICON_LEVEL
    );

    let period = std::time::Duration::from_secs_f64(1.0 / opts.fps);
    let deadline = opts
        .seconds
        .map(|s| std::time::Instant::now() + std::time::Duration::from_secs_f64(s));
    let mut ticks = 0u64;
    let mut snapshotted = false;
    let mut last_display_check = std::time::Instant::now();
    let mut known_screens = effective.screens.clone();
    let mut watcher = Watcher::new(&effective);
    if let Some((x, y)) = opts.inject_click {
        stratum::live::inject_click(x, y);
        println!("injected click at ({x}, {y})");
    }
    let cfg_path = opts
        .cfg_path
        .clone()
        .or_else(|| stratum::config::discover(None).ok());

    loop {
        let painted = rt.tick(&mut presenter)?;
        if !painted.is_empty() {
            let names: Vec<&str> = painted.iter().map(|(id, _)| id.as_str()).collect();
            println!("tick {ticks}: painted {}", names.join(", "));
        }
        ticks += 1;

        // Let AppKit draw. This is the live equivalent of surface_flush().
        presenter.pump(period.as_secs_f64().max(0.008));

        // Mouse: whatever the views captured since the last tick.
        for (id, kind, mx, my) in presenter.poll_mouse() {
            match kind {
                stratum::live::MouseKind::Down => {
                    let hit = rt.click(mx, my);
                    println!("click ({mx:.0}, {my:.0}) -> {:?}", hit);
                }
                stratum::live::MouseKind::Entered => {
                    let hit = rt.hover(mx, my);
                    println!("hover ({mx:.0}, {my:.0}) -> {:?}", hit);
                }
                _ => {}
            }
            let _ = id;
        }

        // Config edits: re-read, keep state, close windows for removed widgets.
        if watcher.changed(&effective)
            && let Some(path) = &cfg_path
        {
            match Config::load(path) {
                Ok(mut fresh) => {
                    if !real.is_empty() {
                        fresh.screens = known_screens.clone();
                        fresh.screen = known_screens[0];
                    }
                    let removed = rt.reload(fresh.clone());
                    for id in &removed {
                        let _ = presenter.destroy(id);
                        eprintln!("[live] widget {id} removed");
                    }
                    for w in &fresh.widgets {
                        presenter.set_click_through(&w.id, w.click_through);
                        presenter.set_always_on_top(&w.id, w.always_on_top);
                    }
                    effective = fresh;
                    watcher = Watcher::new(&effective);
                    eprintln!(
                        "[live] config reloaded ({} widget(s))",
                        effective.widgets.len()
                    );
                }
                Err(e) => eprintln!("[live] config reload failed, keeping the old one: {e}"),
            }
        }

        // Displays come and go (undock, resolution change, wake from sleep).
        // Polling once a second costs an NSScreen call and needs no notifications.
        if last_display_check.elapsed() >= std::time::Duration::from_secs(1) {
            last_display_check = std::time::Instant::now();
            let now = stratum::live::displays();
            if !now.is_empty() && now != known_screens {
                eprintln!(
                    "[live] display layout changed ({} display(s)) — re-placing widgets",
                    now.len()
                );
                known_screens = now.clone();
                effective.screens = now.clone();
                effective.screen = now[0];
                presenter.set_screens(now);
                rt.set_screens(effective.screens.clone());
            }
        }

        if let (Some(dir), false) = (&opts.snapshot, snapshotted) {
            // Wait for the state to settle: scripts land on worker threads, so
            // the first frame is usually the unresolved template.
            let settled = rt.scripts_inflight() == 0 && ticks > 1;
            if settled && !painted.is_empty() {
                presenter.pump(0.25);
                let shots = presenter.snapshot(std::path::Path::new(dir))?;
                println!(
                    "{:<10} {:>6} {:>6} {:>5} {:>6} {:>13} {:>8} {:>5}  pixels",
                    "widget", "x", "y", "w", "h", "level", "window", "vis"
                );
                for (info, png) in &shots {
                    println!(
                        "{:<10} {:>6.0} {:>6.0} {:>5.0} {:>6.0} {:>13} {:>8} {:>5}  {}",
                        info.id,
                        info.frame.x,
                        info.frame.y,
                        info.frame.w,
                        info.frame.h,
                        info.level,
                        info.window_number,
                        info.visible,
                        png
                    );
                }
                snapshotted = true;
                if opts.seconds.is_none() {
                    break;
                }
            }
        }

        if deadline.is_some_and(|d| std::time::Instant::now() >= d) {
            break;
        }
    }

    println!(
        "{} ticks, {} window(s), {} present(s)",
        ticks,
        presenter.window_count(),
        presenter.presented()
    );
    Ok(())
}

/// Non-macOS builds have no live presenter yet.
#[cfg(not(target_os = "macos"))]
fn cmd_live(_args: &[String]) -> Result<(), String> {
    Err("the live presenter is macOS-only for now (see HANDOFF §6)".into())
}

#[cfg(test)]
mod escape_tests {
    use super::xml_escape;

    #[test]
    fn plist_values_are_escaped() {
        assert_eq!(xml_escape("plain/path"), "plain/path");
        // A path that would otherwise close the <string> element or inject a key.
        assert_eq!(
            xml_escape("/tmp/a&b/</string><key>x</key>"),
            "/tmp/a&amp;b/&lt;/string&gt;&lt;key&gt;x&lt;/key&gt;"
        );
        assert_eq!(xml_escape("q\"q'q"), "q&quot;q&apos;q");
    }
}
