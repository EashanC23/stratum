//! Scripting: the config-and-script contract, without the mach port.
//!
//! sketchybar's contract is: a plugin script prints `name=value` lines and
//! sketchybar parses them into item state (`bar_item_parse_set_message`,
//! src/bar_item.c). Identical idea here — a widget's script writes
//! `key=value` (or JSON) lines on stdout and those values are the widget's
//! variables. Text templates read them with `{{key}}`, graphs read them with
//! `data_key`.
//!
//! Nothing about this requires a daemon in-process: the script may be a shell
//! pipeline, a Python one-liner, or a binary. That is the whole point of the
//! design — the renderer owns pixels, the script owns truth.

use crate::model::{ScriptFormat, ScriptSpec, Values};
use std::collections::{HashMap, HashSet};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};

/// Hard ceiling on what a script may print. A runaway script that dumps a file
/// to stdout should not be able to grow this process's memory.
pub const MAX_OUTPUT_BYTES: u64 = 1 << 20; // 1 MiB

/// Children spawned by one owner (a `ScriptPool`), so that shutdown kills exactly
/// its own processes. A global registry looks simpler but is wrong: two runtimes
/// in one process (tests, for instance) would kill each other's scripts.
#[derive(Clone, Default)]
pub struct ChildRegistry(std::sync::Arc<std::sync::Mutex<std::collections::HashSet<u32>>>);

impl ChildRegistry {
    pub fn register(&self, pid: u32) {
        if let Ok(mut set) = self.0.lock() {
            set.insert(pid);
        }
    }

    pub fn unregister(&self, pid: u32) {
        if let Ok(mut set) = self.0.lock() {
            set.remove(&pid);
        }
    }

    /// SIGTERM everything still running. Returns how many were signalled.
    pub fn kill_all(&self) -> usize {
        let pids: Vec<u32> = self
            .0
            .lock()
            .map(|mut set| set.drain().collect())
            .unwrap_or_default();
        let mut killed = 0;
        for pid in pids {
            #[cfg(unix)]
            // SAFETY: `pid` came from `Child::id()` for a child this registry
            // spawned and has not reaped; sending SIGTERM to it is the documented
            // shutdown contract. `kill` is async-signal-safe and takes no pointers.
            unsafe {
                if libc_kill(pid as i32, 15) == 0 {
                    killed += 1;
                }
            }
        }
        killed
    }
}

#[cfg(unix)]
unsafe extern "C" {
    #[link_name = "kill"]
    fn libc_kill(pid: i32, sig: i32) -> i32;
}

/// Outcome of one script run.
#[derive(Debug, Default)]
pub struct ScriptRun {
    pub values: Values,
    pub stderr: String,
    pub elapsed_ms: u128,
    pub timed_out: bool,
    pub exit_ok: bool,
}

/// How a `ScriptSpec` turns into a process.
///
/// `command` is the old path (`sh -c`). `file` is the new one: an executable is
/// run directly, otherwise the extension picks an interpreter. `.rs` compiles
/// through a sibling `Cargo.toml` — that is the honest way to run Rust without
/// pretending a source file is a program.
fn invocation(spec: &ScriptSpec) -> Result<Invocation, String> {
    if let Some(file) = &spec.file {
        // A config loaded from a relative path leaves `file` relative. The child
        // runs with `cwd` set to the script's own directory, so a relative
        // program path would be re-resolved against that directory and the spawn
        // would fail with ENOENT ("spawn failed: No such file or directory").
        // Absolutise once, here, and every derived path (program, arg, cargo
        // manifest) is correct from any working directory.
        let file = &std::path::absolute(file).unwrap_or_else(|_| file.clone());
        if !file.is_file() {
            return Err(format!(
                "script file {} not found (looked beside the widget, in script_dirs, and relative to the working directory)",
                file.display()
            ));
        }
        if let Some(interp) = &spec.interpreter {
            return Ok(Invocation {
                program: interp.into(),
                args: vec![path_arg(file)],
                cwd: file.parent().map(|p| p.to_path_buf()),
                env: Vec::new(),
            });
        }
        if is_executable(file) {
            return Ok(Invocation {
                program: path_arg(file),
                args: Vec::new(),
                cwd: file.parent().map(|p| p.to_path_buf()),
                env: Vec::new(),
            });
        }
        let ext = file
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        let interpreter = match ext.as_str() {
            "sh" => Some("sh"),
            "bash" => Some("bash"),
            "zsh" => Some("zsh"),
            "py" => Some("python3"),
            "rb" => Some("ruby"),
            "js" | "mjs" => Some("node"),
            "lua" => Some("lua"),
            "pl" => Some("perl"),
            "rs" => {
                // A Rust "script" is a cargo project: `src/main.rs` sits next to
                // (or a level or two below) a Cargo.toml. Walk up to find it.
                let start = file.parent().unwrap_or(Path::new("."));
                let manifest = start
                    .ancestors()
                    .take(4)
                    .map(|d| d.join("Cargo.toml"))
                    .find(|p| p.is_file());
                let Some(manifest) = manifest else {
                    return Err(format!(
                        "{}: a .rs script needs a Cargo.toml above it (or set `interpreter`)",
                        file.display()
                    ));
                };
                let dir = manifest.parent().unwrap_or(start).to_path_buf();
                let target = cargo_target_dir(&manifest);
                return Ok(Invocation {
                    program: "cargo".into(),
                    args: vec![
                        "run".into(),
                        "--quiet".into(),
                        "--manifest-path".into(),
                        path_arg(&manifest),
                    ],
                    cwd: Some(dir),
                    env: vec![("CARGO_TARGET_DIR".into(), target.display().to_string())],
                });
            }
            _ => None,
        };
        return match interpreter {
            Some(p) => Ok(Invocation {
                program: p.into(),
                args: vec![path_arg(file)],
                cwd: file.parent().map(|p| p.to_path_buf()),
                env: Vec::new(),
            }),
            None => Err(format!(
                "{}: not executable and no interpreter for {}. \n  chmod +x it, or set `interpreter = \"...\"`",
                file.display(),
                if ext.is_empty() {
                    "this file".to_string()
                } else {
                    format!(".{ext}")
                }
            )),
        };
    }

    Ok(Invocation {
        program: "sh".into(),
        args: vec!["-c".into(), spec.command.clone().into()],
        cwd: None,
        env: Vec::new(),
    })
}

#[derive(Debug, Clone)]
struct Invocation {
    program: std::ffi::OsString,
    args: Vec<std::ffi::OsString>,
    cwd: Option<std::path::PathBuf>,
    env: Vec<(String, String)>,
}

/// Build artifacts must not land in the config tree — `~/.config` is for
/// configuration, not for hundreds of megabytes of `.o` files.
/// PATH for script children.
///
/// A LaunchAgent gets `/usr/bin:/bin:/usr/sbin:/sbin` and nothing else, so a
/// script that needs `cargo`, `python3` from Homebrew, or anything in
/// `~/.cargo/bin` fails with "spawn failed: No such file or directory" — which
/// looks like a missing script, not a missing PATH. Inherit what we have, then
/// append the usual install locations.
fn child_path() -> String {
    let mut parts: Vec<String> = std::env::var("PATH")
        .unwrap_or_default()
        .split(':')
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
        .collect();
    let mut extra: Vec<String> = vec![
        "/opt/homebrew/bin".into(),
        "/usr/local/bin".into(),
        "/usr/bin".into(),
        "/bin".into(),
    ];
    if let Ok(home) = std::env::var("HOME") {
        extra.insert(0, format!("{home}/.cargo/bin"));
        extra.push(format!("{home}/.local/bin"));
    }
    for dir in extra {
        if !parts.iter().any(|p| p == &dir) {
            parts.push(dir);
        }
    }
    parts.join(":")
}

fn cargo_target_dir(manifest: &Path) -> PathBuf {
    let cache = std::env::var("XDG_CACHE_HOME")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var("HOME")
                .ok()
                .map(|h| PathBuf::from(h).join(".cache"))
        })
        .unwrap_or_else(|| PathBuf::from("."));
    let name = manifest
        .parent()
        .and_then(|p| p.file_name())
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "script".into());
    cache.join("stratum").join("cargo-target").join(name)
}

fn path_arg(p: &std::path::Path) -> std::ffi::OsString {
    p.as_os_str().to_os_string()
}

#[cfg(unix)]
fn is_executable(path: &std::path::Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path)
        .map(|m| m.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

#[cfg(not(unix))]
fn is_executable(_path: &std::path::Path) -> bool {
    false
}

pub fn run(spec: &ScriptSpec) -> ScriptRun {
    run_with_env(spec, &[])
}

/// Run with a registry that tracks the child, so a shutdown can kill it.
pub fn run_tracked(
    spec: &ScriptSpec,
    vars: &[(String, String)],
    kids: &ChildRegistry,
) -> ScriptRun {
    run_inner(spec, vars, Some(kids))
}

/// Same as `run`, but exports `vars` into the child's environment. That is how
/// a script learns what happened: `STRATUM_EVENT=click`, the widget id, the
/// click coordinates. Chosen over argv so an interval script and a click script
/// can share one command line.
pub fn run_with_env(spec: &ScriptSpec, vars: &[(String, String)]) -> ScriptRun {
    run_inner(spec, vars, None)
}

fn run_inner(
    spec: &ScriptSpec,
    vars: &[(String, String)],
    kids: Option<&ChildRegistry>,
) -> ScriptRun {
    let started = Instant::now();
    let inv = match invocation(spec) {
        Ok(inv) => inv,
        Err(e) => {
            return ScriptRun {
                stderr: e,
                ..Default::default()
            };
        }
    };
    let mut cmd = Command::new(&inv.program);
    cmd.args(&inv.args);
    if let Some(dir) = &inv.cwd {
        cmd.current_dir(dir);
    }
    cmd.env("PATH", child_path());
    for (k, v) in &inv.env {
        cmd.env(k, v);
    }
    for (k, v) in vars {
        cmd.env(k, v);
    }
    let mut child = match cmd
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .stdin(Stdio::null())
        .spawn()
    {
        Ok(c) => c,
        Err(e) => {
            return ScriptRun {
                stderr: format!("spawn failed: {e}"),
                ..Default::default()
            };
        }
    };

    let pid: u32 = child.id();
    let Some(pid) = Some(pid) else {
        let _ = child.wait();
        return ScriptRun {
            stderr: "spawn: no pid".into(),
            ..Default::default()
        };
    };
    if let Some(kids) = kids {
        kids.register(pid);
    }

    // Drain stdout on a thread so a chatty script cannot deadlock on a full pipe
    // while we sit in the timeout loop; cap how much we keep.
    let mut stdout = child.stdout.take();
    let reader = std::thread::spawn(move || {
        let mut buf = String::new();
        if let Some(s) = stdout.as_mut() {
            let _ = s.take(MAX_OUTPUT_BYTES).read_to_string(&mut buf);
        }
        buf
    });

    let deadline = Duration::from_millis(spec.timeout_ms.max(50));
    let mut timed_out = false;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) => {
                if started.elapsed() > deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    timed_out = true;
                    break;
                }
                std::thread::sleep(Duration::from_millis(5));
            }
            Err(_) => break,
        }
    }

    let exit_ok = child
        .try_wait()
        .ok()
        .flatten()
        .map(|s| s.success())
        .unwrap_or(false);
    if let Some(kids) = kids {
        kids.unregister(pid);
    }
    let out = reader.join().unwrap_or_default();
    let stderr = child
        .stderr
        .take()
        .and_then(|mut e| {
            let mut s = String::new();
            e.read_to_string(&mut s).ok().map(|_| s)
        })
        .unwrap_or_default();

    let values = parse(&out, spec.format);
    ScriptRun {
        values,
        stderr,
        elapsed_ms: started.elapsed().as_millis(),
        timed_out,
        exit_ok: exit_ok && !timed_out,
    }
}

/// Parse script output into values. `key=value` is the default wire format;
/// blank lines and `#` comments are ignored, which makes shell scripts readable.
pub fn parse(out: &str, format: ScriptFormat) -> Values {
    match format {
        ScriptFormat::KeyValue => {
            let mut m = HashMap::new();
            for line in out.lines() {
                let line = line.trim();
                if line.is_empty() || line.starts_with('#') {
                    continue;
                }
                if let Some((k, v)) = line.split_once('=') {
                    m.insert(k.trim().to_string(), v.trim().to_string());
                }
            }
            Values(m)
        }
        ScriptFormat::JsonLines => {
            let mut m = HashMap::new();
            for line in out.lines() {
                let line = line.trim();
                if line.is_empty() {
                    continue;
                }
                if let Ok(serde_json::Value::Object(obj)) = serde_json::from_str(line) {
                    for (k, v) in obj {
                        let s = match v {
                            serde_json::Value::String(s) => s,
                            other => other.to_string(),
                        };
                        m.insert(k, s);
                    }
                }
            }
            Values(m)
        }
    }
}

/// When each widget is due next. A widget with a script and `interval > 0`
/// re-runs on its own clock; the renderer only repaints widgets whose values
/// actually changed (the `needs_update` flag from src/bar_item.c).
pub struct Scheduler {
    next: HashMap<String, Instant>,
}

impl Scheduler {
    pub fn new() -> Self {
        Scheduler {
            next: HashMap::new(),
        }
    }

    pub fn due(&self, id: &str) -> bool {
        self.next
            .get(id)
            .map(|t| Instant::now() >= *t)
            .unwrap_or(true)
    }

    /// Record that the script just ran. `interval <= 0` means "run once": the
    /// widget is pushed far into the future so it is never polled again, and
    /// only an event can re-run it.
    pub fn mark(&mut self, id: &str, interval: f64) {
        let when = if interval > 0.0 {
            Instant::now() + Duration::from_secs_f64(interval)
        } else {
            // ~100 years: a sentinel, not a magic number you should tune.
            Instant::now() + Duration::from_secs(60 * 60 * 24 * 365 * 100)
        };
        self.next.insert(id.to_string(), when);
    }

    pub fn forget(&mut self, id: &str) {
        self.next.remove(id);
    }
}

impl Default for Scheduler {
    fn default() -> Self {
        Self::new()
    }
}

// ------------------------------------------------------------- worker pool ----

/// A fixed pool running widget scripts OFF the render thread.
///
/// Why this exists: `script::run` blocks for as long as the command takes. One
/// `curl` with a 2s timeout in a widget would stall every other widget's frame.
/// The render loop dispatches a job and keeps painting; results are collected on
/// a later tick. A widget whose script is still running is not dispatched twice.
pub struct ScriptPool {
    tx: mpsc::Sender<Job>,
    rx: mpsc::Receiver<Outcome>,
    inflight: HashSet<String>,
    /// Children this pool spawned, killed when the pool is dropped.
    children: ChildRegistry,
}

struct Job {
    widget: String,
    spec: ScriptSpec,
    vars: Vec<(String, String)>,
    children: ChildRegistry,
}

struct Outcome {
    widget: String,
    run: ScriptRun,
}

impl ScriptPool {
    /// `threads` worker threads; `0` means "one per CPU, capped at 4".
    pub fn new(threads: usize) -> Result<Self, String> {
        let n = if threads == 0 {
            std::thread::available_parallelism()
                .map(|p| p.get().min(4))
                .unwrap_or(2)
        } else {
            threads
        };
        let (tx, job_rx) = mpsc::channel::<Job>();
        let (out_tx, rx) = mpsc::channel::<Outcome>();
        let job_rx = Arc::new(Mutex::new(job_rx));
        let children = ChildRegistry::default();

        for i in 0..n {
            let job_rx = Arc::clone(&job_rx);
            let out_tx = out_tx.clone();
            std::thread::Builder::new()
                .name(format!("stratum-script-{i}"))
                .spawn(move || {
                    loop {
                        let job = match job_rx.lock() {
                            Ok(guard) => match guard.recv() {
                                Ok(j) => j,
                                Err(_) => return, // pool dropped
                            },
                            Err(_) => return,
                        };
                        let run = run_tracked(&job.spec, &job.vars, &job.children);
                        if out_tx
                            .send(Outcome {
                                widget: job.widget,
                                run,
                            })
                            .is_err()
                        {
                            return;
                        }
                    }
                })
                .map_err(|e| format!("spawn script worker: {e}"))?;
        }

        Ok(ScriptPool {
            tx,
            rx,
            inflight: HashSet::new(),
            children,
        })
    }

    /// Queue a run. Returns false when this widget already has one in flight.
    pub fn dispatch(
        &mut self,
        widget: &str,
        spec: &ScriptSpec,
        vars: Vec<(String, String)>,
    ) -> bool {
        if self.inflight.contains(widget) {
            return false;
        }
        let job = Job {
            widget: widget.to_string(),
            spec: spec.clone(),
            vars,
            children: self.children.clone(),
        };
        match self.tx.send(job) {
            Ok(()) => {
                self.inflight.insert(widget.to_string());
                true
            }
            Err(_) => false,
        }
    }

    /// Collect every finished run without blocking.
    pub fn poll(&mut self) -> Vec<(String, ScriptRun)> {
        let mut out = Vec::new();
        loop {
            match self.rx.try_recv() {
                Ok(o) => {
                    self.inflight.remove(&o.widget);
                    out.push((o.widget, o.run));
                }
                Err(mpsc::TryRecvError::Empty) => break,
                Err(mpsc::TryRecvError::Disconnected) => break,
            }
        }
        out
    }

    pub fn is_inflight(&self, widget: &str) -> bool {
        self.inflight.contains(widget)
    }

    pub fn inflight_count(&self) -> usize {
        self.inflight.len()
    }

    /// SIGTERM every child this pool still has running (shutdown path).
    pub fn kill_children(&self) -> usize {
        self.children.kill_all()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keyvalue_parsing() {
        let v = parse(
            "cpu=42\n# a comment\n\nmem_free=1.2 GB\n",
            ScriptFormat::KeyValue,
        );
        assert_eq!(v.get("cpu"), Some("42"));
        assert_eq!(v.get("mem_free"), Some("1.2 GB"));
    }

    #[test]
    fn templates_expand_and_default_empty() {
        let v = parse("who=world\n", ScriptFormat::KeyValue);
        assert_eq!(v.expand("hello {{who}}"), "hello world");
        assert_eq!(v.expand("hello {{missing}}!"), "hello !");
    }

    #[test]
    fn series_parsing() {
        let v = parse("load=1,2,3  4\n", ScriptFormat::KeyValue);
        assert_eq!(v.series("load"), Some(vec![1.0, 2.0, 3.0, 4.0]));
    }

    #[test]
    fn script_timeout_does_not_hang() {
        let spec = ScriptSpec {
            command: "sleep 5; echo never=1".into(),
            file: None,
            interpreter: None,
            interval: 0.0,
            format: ScriptFormat::KeyValue,
            timeout_ms: 100,
            on: vec![],
        };
        let r = run(&spec);
        assert!(r.timed_out);
        assert!(r.values.get("never").is_none());
    }

    #[test]
    fn script_captures_output() {
        let spec = ScriptSpec {
            command: "echo a=1; echo b=two".into(),
            file: None,
            interpreter: None,
            interval: 1.0,
            format: ScriptFormat::KeyValue,
            timeout_ms: 1000,
            on: vec![],
        };
        let r = run(&spec);
        assert!(r.exit_ok);
        assert_eq!(r.values.get("b"), Some("two"));
    }

    #[cfg(unix)]
    fn temp_script(name: &str, body: &str, exec: bool) -> std::path::PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let dir =
            std::env::temp_dir().join(format!("stratum-script-{}-{name}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(name);
        std::fs::write(&path, body).unwrap();
        let mut perms = std::fs::metadata(&path).unwrap().permissions();
        perms.set_mode(if exec { 0o755 } else { 0o644 });
        std::fs::set_permissions(&path, perms).unwrap();
        path
    }

    #[cfg(unix)]
    #[test]
    fn runs_a_script_file_through_its_interpreter() {
        let path = temp_script("a.sh", "echo from=file\n", false);
        let spec = ScriptSpec {
            command: "echo from=inline".into(),
            file: Some(path),
            interpreter: None,
            interval: 1.0,
            format: ScriptFormat::KeyValue,
            timeout_ms: 5000,
            on: vec![],
        };
        let r = run(&spec);
        assert!(r.exit_ok, "stderr: {}", r.stderr);
        assert_eq!(
            r.values.get("from"),
            Some("file"),
            "file must win over command"
        );
    }

    #[cfg(unix)]
    #[test]
    fn runs_an_executable_directly() {
        let path = temp_script("b.sh", "#!/bin/sh\necho exec=yes\n", true);
        let spec = ScriptSpec {
            command: String::new(),
            file: Some(path),
            interpreter: None,
            interval: 1.0,
            format: ScriptFormat::KeyValue,
            timeout_ms: 5000,
            on: vec![],
        };
        let r = run(&spec);
        assert!(r.exit_ok, "stderr: {}", r.stderr);
        assert_eq!(r.values.get("exec"), Some("yes"));
    }

    #[cfg(unix)]
    #[test]
    fn an_interpreter_override_wins() {
        // No extension at all: without the override this would be unrunnable.
        let path = temp_script("c.script", "echo interp=python\n", false);
        let spec = ScriptSpec {
            command: String::new(),
            file: Some(path),
            interpreter: Some("sh".into()),
            interval: 1.0,
            format: ScriptFormat::KeyValue,
            timeout_ms: 5000,
            on: vec![],
        };
        let r = run(&spec);
        assert!(r.exit_ok, "stderr: {}", r.stderr);
        assert_eq!(r.values.get("interp"), Some("python"));
    }

    #[cfg(unix)]
    #[test]
    fn a_missing_script_file_explains_itself() {
        let spec = ScriptSpec {
            command: String::new(),
            file: Some(std::path::PathBuf::from("/nonexistent/nope.sh")),
            interpreter: None,
            interval: 1.0,
            format: ScriptFormat::KeyValue,
            timeout_ms: 1000,
            on: vec![],
        };
        let r = run(&spec);
        assert!(!r.exit_ok);
        assert!(r.stderr.contains("not found"), "{}", r.stderr);
        assert!(r.stderr.contains("script_dirs"), "{}", r.stderr);
    }

    #[cfg(unix)]
    #[test]
    fn a_rust_script_without_a_manifest_is_rejected_with_advice() {
        let path = temp_script("lonely.rs", "fn main() {}\n", false);
        let spec = ScriptSpec {
            command: String::new(),
            file: Some(path),
            interpreter: None,
            interval: 1.0,
            format: ScriptFormat::KeyValue,
            timeout_ms: 1000,
            on: vec![],
        };
        let r = run(&spec);
        assert!(!r.exit_ok);
        assert!(r.stderr.contains("Cargo.toml"), "{}", r.stderr);
    }
}
