//! Runtime: config -> values -> per-widget raster -> presenter.
//!
//! One canvas per widget, exactly like sketchybar gives every bar item its own
//! window+surface+bitmap context (src/bar_item.c:543, src/surface.c:26). That
//! is what makes a widget cheap to move or repaint independently, and it is the
//! property worth keeping when you spread widgets across the desktop.
//!
//! The loop is non-blocking by design:
//!
//!   tick()  dispatch due/triggered scripts -> collect finished ones -> repaint
//!           only widgets whose values changed
//!
//! Scripts run on a worker pool (`script::ScriptPool`), never on the caller's
//! thread, so one slow command cannot stall the whole desktop. `tick_blocking`
//! is the convenience wrapper for one-shot renders and tests.

use crate::canvas::Canvas;
use crate::config::Config;
use crate::icon::IconCache;
use crate::model::{Frame, Values, WidgetSpec};
use crate::present::Presenter;
use crate::script::{Scheduler, ScriptPool};
use crate::text::TextEngine;
use crate::widget;
use std::collections::{HashMap, HashSet};
use std::time::Instant;

/// One animated graph series: where it came from, where it is going, and when.
#[derive(Debug, Clone)]
pub struct Tween {
    pub from: Vec<f32>,
    pub to: Vec<f32>,
    pub started: Instant,
}

impl Tween {
    /// Value at `now`, eased with smoothstep. Snaps when the series length
    /// changes (interpolating between different-length series is meaningless)
    /// and when animation is disabled.
    pub fn sample(&self, now: Instant, animation_ms: f64) -> Vec<f32> {
        if animation_ms <= 0.0 || self.from.len() != self.to.len() {
            return self.to.clone();
        }
        let elapsed = now.saturating_duration_since(self.started).as_secs_f64() * 1000.0;
        if elapsed >= animation_ms {
            return self.to.clone();
        }
        let t = (elapsed / animation_ms) as f32;
        let e = t * t * (3.0 - 2.0 * t); // smoothstep
        self.from
            .iter()
            .zip(self.to.iter())
            .map(|(a, b)| a + (b - a) * e)
            .collect()
    }

    pub fn finished(&self, now: Instant, animation_ms: f64) -> bool {
        animation_ms <= 0.0
            || self.from.len() != self.to.len()
            || now.saturating_duration_since(self.started).as_secs_f64() * 1000.0 >= animation_ms
    }
}

/// Environment variables handed to a widget's script. Part of the public
/// contract: a script reads `STRATUM_EVENT` to know why it was run.
pub const ENV_EVENT: &str = "STRATUM_EVENT";
pub const ENV_WIDGET: &str = "STRATUM_WIDGET";
pub const ENV_CLICK_X: &str = "STRATUM_CLICK_X";
pub const ENV_CLICK_Y: &str = "STRATUM_CLICK_Y";

/// An input event queued for a widget's script.
#[derive(Debug, Clone)]
pub struct Event {
    pub name: String,
    pub x: Option<f32>,
    pub y: Option<f32>,
}

impl Event {
    pub fn new(name: &str) -> Self {
        Event {
            name: name.to_string(),
            x: None,
            y: None,
        }
    }

    fn vars(&self, widget: &str) -> Vec<(String, String)> {
        let mut v = vec![
            (ENV_WIDGET.to_string(), widget.to_string()),
            (ENV_EVENT.to_string(), self.name.clone()),
        ];
        if let (Some(x), Some(y)) = (self.x, self.y) {
            v.push((ENV_CLICK_X.to_string(), format!("{x:.1}")));
            v.push((ENV_CLICK_Y.to_string(), format!("{y:.1}")));
        }
        v
    }
}

pub struct Runtime {
    cfg: Config,
    text: TextEngine,
    icons: IconCache,
    values: HashMap<String, Values>,
    /// Screen-space frame each widget currently occupies (for hit-testing, and
    /// for presenters that move a window without repainting it).
    frames: HashMap<String, Frame>,
    scheduler: Scheduler,
    pool: ScriptPool,
    /// Widgets needing a repaint on the next tick.
    dirty: HashSet<String>,
    /// Events waiting to be delivered to a widget's script.
    pending: HashMap<String, Event>,
    /// Consecutive failed runs per widget, for exponential backoff.
    failures: HashMap<String, u32>,
    /// Live tweens, keyed by (widget id, element index).
    tweens: HashMap<(String, usize), Tween>,
    run_scripts: bool,
    last_painted: HashSet<String>,
}

impl Runtime {
    pub fn new(cfg: Config, run_scripts: bool) -> Result<Self, String> {
        Self::with_threads(cfg, run_scripts, 0)
    }

    pub fn with_threads(cfg: Config, run_scripts: bool, threads: usize) -> Result<Self, String> {
        // Drop switched-off widgets here, once: every later pass (layout, hit
        // test, script scheduling, presenters) then sees only live widgets, and
        // `dump` / `config --search` still report the disabled ones because they
        // read the file, not the runtime.
        let mut cfg = cfg;
        cfg.widgets.retain(|w| w.enabled);
        let text = crate::golden::text_engine_for(&cfg)?;
        Ok(Runtime {
            cfg,
            text,
            icons: IconCache::new(),
            values: HashMap::new(),
            frames: HashMap::new(),
            scheduler: Scheduler::new(),
            pool: ScriptPool::new(threads)?,
            dirty: HashSet::new(),
            pending: HashMap::new(),
            failures: HashMap::new(),
            tweens: HashMap::new(),
            run_scripts,
            last_painted: HashSet::new(),
        })
    }

    pub fn config(&self) -> &Config {
        &self.cfg
    }

    pub fn text(&self) -> &TextEngine {
        &self.text
    }

    pub fn values(&self, id: &str) -> Option<&Values> {
        self.values.get(id)
    }

    /// Swap in a re-read config without losing runtime state.
    ///
    /// Values and tweens for widgets whose id survives are kept, so an edited
    /// widget keeps its data and a live tween is not reset. Everything is marked
    /// dirty, and the ids that no longer exist are returned so the presenter can
    /// close their windows.
    pub fn reload(&mut self, cfg: crate::config::Config) -> Vec<String> {
        // Same filter as construction, so toggling `enabled` off closes the
        // window: the id is absent from `keep`, so it comes back in `removed`.
        let mut cfg = cfg;
        cfg.widgets.retain(|w| w.enabled);
        let keep: HashSet<String> = cfg.widgets.iter().map(|w| w.id.clone()).collect();
        let removed: Vec<String> = self
            .cfg
            .widgets
            .iter()
            .map(|w| w.id.clone())
            .filter(|id| !keep.contains(id))
            .collect();

        self.values.retain(|id, _| keep.contains(id));
        self.tweens.retain(|(id, _), _| keep.contains(id));
        self.failures.retain(|id, _| keep.contains(id));
        self.pending.retain(|id, _| keep.contains(id));
        self.dirty.extend(keep.iter().cloned());
        self.cfg = cfg;
        removed
    }

    /// Replace the display layout (a monitor was plugged in, resolution changed).
    /// Every widget is re-resolved on the next tick.
    pub fn set_screens(&mut self, screens: Vec<Frame>) {
        if screens.is_empty() {
            return;
        }
        self.cfg.screens = screens;
        self.cfg.screen = self.cfg.screens[0];
        self.dirty
            .extend(self.cfg.widgets.iter().map(|w| w.id.clone()));
    }

    pub fn frame_of(&self, id: &str) -> Option<Frame> {
        self.frames.get(id).copied()
    }

    /// Test hook: install values as if a script had just returned them.
    #[cfg(test)]
    pub fn inject_values(&mut self, id: &str, values: Values) {
        self.values.insert(id.to_string(), values);
        self.dirty.insert(id.to_string());
    }

    pub fn scripts_inflight(&self) -> usize {
        self.pool.inflight_count()
    }

    /// True while any graph tween is still moving.
    pub fn animating(&self) -> bool {
        let now = Instant::now();
        let ms = self.cfg.animation_ms;
        self.tweens.values().any(|t| !t.finished(now, ms))
    }

    /// Widget whose frame contains a screen point, topmost layer first.
    pub fn hit_test(&self, x: f32, y: f32) -> Option<&str> {
        let mut hits: Vec<(&str, i32)> = self
            .cfg
            .widgets
            .iter()
            .filter(|w| {
                self.frames
                    .get(&w.id)
                    .map(|f| f.contains(x, y))
                    .unwrap_or(false)
            })
            .map(|w| (w.id.as_str(), w.layer))
            .collect();
        hits.sort_by_key(|(_, layer)| -layer);
        hits.first().map(|(id, _)| *id)
    }

    /// Trigger a named event on every widget subscribed to it in `on`.
    pub fn emit(&mut self, event: &str) {
        for w in &self.cfg.widgets {
            if self.subscribed(w, event) {
                self.pending.insert(w.id.clone(), Event::new(event));
            }
        }
    }

    /// Pointer entered a widget: queue `hover` for it if it subscribes.
    /// Fires once per entry (tracking areas report enter/exit, not motion), so a
    /// script is not re-run on every pixel of movement.
    pub fn hover(&mut self, x: f32, y: f32) -> Option<String> {
        let id = self.hit_test(x, y)?.to_string();
        let subscribed = self
            .cfg
            .widgets
            .iter()
            .find(|w| w.id == id)
            .map(|w| self.subscribed(w, "hover"))
            .unwrap_or(false);
        if subscribed {
            self.pending.insert(
                id.clone(),
                Event {
                    name: "hover".into(),
                    x: Some(x),
                    y: Some(y),
                },
            );
        }
        Some(id)
    }

    /// Deliver a click at a screen point: hit-test, then queue `click` for that
    /// widget if it subscribes. Returns the widget under the cursor.
    pub fn click(&mut self, x: f32, y: f32) -> Option<String> {
        let id = self.hit_test(x, y)?.to_string();
        let subscribed = self
            .cfg
            .widgets
            .iter()
            .find(|w| w.id == id)
            .map(|w| self.subscribed(w, "click"))
            .unwrap_or(false);
        if subscribed {
            self.pending.insert(
                id.clone(),
                Event {
                    name: "click".into(),
                    x: Some(x),
                    y: Some(y),
                },
            );
        }
        Some(id)
    }

    fn subscribed(&self, w: &WidgetSpec, event: &str) -> bool {
        w.script
            .as_ref()
            .map(|s| s.on.iter().any(|e| e == event))
            .unwrap_or(false)
    }

    /// Widgets painted by the previous tick.
    pub fn last_painted(&self) -> &HashSet<String> {
        &self.last_painted
    }

    /// One loop iteration. Never blocks on a script.
    pub fn tick(&mut self, presenter: &mut dyn Presenter) -> Result<Vec<(String, Frame)>, String> {
        self.collect_results();

        // Queue work: explicit events first, then the polling clocks.
        // Only the id and the (small) script spec are copied per widget — the
        // specs themselves are borrowed. Cloning `widgets` here costs a deep copy
        // of every element and string, twice a tick.
        if self.run_scripts {
            for i in 0..self.cfg.widgets.len() {
                let (id, spec) = {
                    let w = &self.cfg.widgets[i];
                    match &w.script {
                        Some(spec) => (w.id.clone(), spec.clone()),
                        None => continue,
                    }
                };
                let event = self.pending.get(&id).cloned();
                let due = self.scheduler.due(&id);
                if event.is_none() && !due {
                    continue;
                }
                let vars = match &event {
                    Some(ev) => ev.vars(&id),
                    None => vec![
                        (ENV_WIDGET.to_string(), id.clone()),
                        (ENV_EVENT.to_string(), "interval".to_string()),
                    ],
                };
                if self.pool.dispatch(&id, &spec, vars) {
                    let failures = self.failures.get(&id).copied().unwrap_or(0);
                    self.scheduler
                        .mark(&id, backoff_interval(spec.interval, failures));
                    self.pending.remove(&id);
                }
            }
        }

        // A tween needs a repaint every tick until it lands.
        let now = Instant::now();
        let anim_ms = self.cfg.animation_ms;
        let animating: Vec<String> = self
            .tweens
            .iter()
            .filter(|(_, t)| !t.finished(now, anim_ms))
            .map(|((id, _), _)| id.clone())
            .collect();
        self.dirty.extend(animating);

        // Paint whatever is dirty or was never painted. `mem::take` moves the
        // spec out for the duration of the render, so nothing is deep-copied.
        let mut painted = Vec::new();
        for i in 0..self.cfg.widgets.len() {
            let id = self.cfg.widgets[i].id.clone();
            let never = !self.frames.contains_key(&id);
            if !never && !self.dirty.contains(&id) {
                continue;
            }
            let widget = std::mem::take(&mut self.cfg.widgets[i]);
            let rendered = self.render_widget(&widget, presenter);
            self.cfg.widgets[i] = widget;
            let frame = rendered?;
            self.dirty.remove(&id);
            painted.push((id, frame));
        }
        self.last_painted = painted.iter().map(|(id, _)| id.clone()).collect();
        Ok(painted)
    }

    /// Tick until no script is in flight and nothing is queued, or the deadline
    /// passes. This is the one-shot path (`stratum render`): the same async
    /// loop, with a bound on how long you are willing to wait.
    pub fn tick_blocking(
        &mut self,
        presenter: &mut dyn Presenter,
        max_wait: std::time::Duration,
    ) -> Result<Vec<(String, Frame)>, String> {
        let deadline = std::time::Instant::now() + max_wait;
        let mut painted = Vec::new();
        loop {
            painted.extend(self.tick(presenter)?);
            if self.pool.inflight_count() == 0 && self.pending.is_empty() && !self.animating() {
                break;
            }
            if std::time::Instant::now() >= deadline {
                eprintln!(
                    "[runtime] deadline hit with {} script(s) still running",
                    self.pool.inflight_count()
                );
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        // One more tick: values that landed after the last paint.
        painted.extend(self.tick(presenter)?);
        Ok(painted)
    }

    /// Move each of a widget's graph elements a step closer to its target and
    /// return the series to draw this frame.
    fn advance_tweens(&mut self, widget: &WidgetSpec, values: &Values) -> HashMap<usize, Vec<f32>> {
        let now = Instant::now();
        let anim_ms = self.cfg.animation_ms;
        let mut out = HashMap::new();

        for (index, el) in widget.elements.iter().enumerate() {
            let crate::model::Element::Graph(spec) = el else {
                continue;
            };
            let target = widget::resolve_series(spec, values);
            if target.is_empty() {
                continue;
            }
            let key = (widget.id.clone(), index);

            let next = match self.tweens.get(&key) {
                // First sight: no tween, appear at the value.
                None => Tween {
                    from: target.clone(),
                    to: target.clone(),
                    started: now,
                },
                Some(t) if t.to != target => {
                    // New data arrived mid-flight: re-anchor at the current
                    // position so the motion is continuous, not a jump.
                    Tween {
                        from: t.sample(now, anim_ms),
                        to: target.clone(),
                        started: now,
                    }
                }
                Some(t) => t.clone(),
            };
            out.insert(index, next.sample(now, anim_ms));
            self.tweens.insert(key, next);
        }
        out
    }

    fn collect_results(&mut self) {
        for (id, run) in self.pool.poll() {
            if run.timed_out {
                eprintln!("[script {id}] timed out");
            } else if !run.exit_ok && !run.stderr.is_empty() {
                eprintln!("[script {id}] {}", run.stderr.trim());
            }
            // Back off on repeated failure: a script that is broken (bad path,
            // missing dependency, network down) must not be retried every second
            // forever, spamming the log.
            let fails = self.failures.entry(id.clone()).or_insert(0);
            if run.exit_ok {
                *fails = 0;
            } else {
                *fails = fails.saturating_add(1);
            }
            let changed = match self.values.get(&id) {
                Some(prev) => prev.0 != run.values.0,
                None => true,
            };
            self.values.insert(id.clone(), run.values);
            if changed {
                self.dirty.insert(id);
            }
        }
    }

    /// Rasterise one widget and present it. Returns its resolved screen frame.
    pub fn render_widget(
        &mut self,
        widget: &WidgetSpec,
        presenter: &mut dyn Presenter,
    ) -> Result<Frame, String> {
        // Move the value map out for the render instead of cloning it, and put it
        // back on every exit path. A `HashMap<String, String>` per repaint is real
        // work at 10 fps.
        let values = self.values.remove(&widget.id).unwrap_or_default();
        let rendered = self.render_widget_inner(widget, presenter, &values);
        self.values.insert(widget.id.clone(), values);
        rendered
    }

    fn render_widget_inner(
        &mut self,
        widget: &WidgetSpec,
        presenter: &mut dyn Presenter,
        values: &Values,
    ) -> Result<Frame, String> {
        // Advance tweens first: it needs `&mut self`, and `ctx` borrows self.icons
        // for the rest of the function.
        let series = self.advance_tweens(widget, values);

        let mut ctx = widget::Ctx {
            text: &self.text,
            icons: &mut self.icons,
            scale: self.cfg.scale,
        };

        // Measure first: an auto-sized widget needs its intrinsic size before a
        // canvas can be allocated. Resolution is against the widget's OWN
        // display, so `display = 1` sits on the second monitor.
        let screen = self.cfg.screen_for(widget.display);
        let avail = (
            if widget.frame.w > 0.0 {
                widget.frame.w
            } else {
                screen.w
            },
            if widget.frame.h > 0.0 {
                widget.frame.h
            } else {
                screen.h
            },
        );
        let measured = widget::measure(widget, &ctx, values, avail);
        let frame = widget.anchor.resolve(screen, widget.frame, measured);

        let mut canvas = Canvas::new(frame.w, frame.h, self.cfg.scale).ok_or_else(|| {
            format!(
                "widget {}: allocation failed ({}x{})",
                widget.id, frame.w, frame.h
            )
        })?;
        canvas.clear();
        widget::draw_with_series(widget, &mut canvas, &mut ctx, values, &series);

        presenter.present(&widget.id, frame, canvas.pixmap())?;
        self.frames.insert(widget.id.clone(), frame);
        Ok(frame)
    }
}

/// Delay before retrying a widget's script after `failures` consecutive
/// failures: the configured interval doubled per failure, capped at 5 minutes.
/// An interval of 0 ("run once") stays 0 — a failed one-shot is not retried by
/// the clock at all, only by an event.
pub fn backoff_interval(base: f64, failures: u32) -> f64 {
    if base <= 0.0 || failures == 0 {
        return base;
    }
    let factor = 2f64.powi(failures.min(6) as i32);
    (base * factor).min(300.0)
}

impl Drop for Runtime {
    fn drop(&mut self) {
        // Do not leave `sleep`, `cargo run`, `curl` … behind when the daemon exits.
        let n = self.pool.kill_children();
        if n > 0 {
            eprintln!("[runtime] stopped {n} in-flight script process(es)");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::*;
    use crate::present::PngPresenter;
    use std::time::{Duration, Instant};

    fn sink() -> PngPresenter {
        PngPresenter::new(
            std::path::Path::new("/tmp/stratum-test"),
            Frame::new(0.0, 0.0, 400.0, 300.0),
            1.0,
            false,
        )
    }

    fn text_widget(id: &str) -> WidgetSpec {
        WidgetSpec {
            id: id.into(),
            display: 0,
            anchor: Anchor::TopLeft,
            frame: Frame::default(),
            layout: Layout::Row,
            padding: PaddingShorthand(Padding::all(6.0)),
            gap: 4.0,
            background: Background {
                enabled: true,
                color: Rgba::rgb(0x10, 0x10, 0x14),
                corner_radius: 8.0,
                ..Default::default()
            },
            click_through: true,
            enabled: true,
            always_on_top: false,
            elements: vec![Element::Text(TextSpec {
                value: "hello".into(),
                kind: TextKind::Static,
                size: 14.0,
                ..Default::default()
            })],
            script: None,
            layer: 0,
        }
    }

    fn script_spec(command: &str, interval: f64, on: &[&str]) -> ScriptSpec {
        ScriptSpec {
            command: command.into(),
            file: None,
            interpreter: None,
            interval,
            format: ScriptFormat::KeyValue,
            timeout_ms: 5000,
            on: on.iter().map(|s| s.to_string()).collect(),
        }
    }

    fn cfg_with(mut w: WidgetSpec, script: Option<ScriptSpec>) -> Config {
        w.script = script;
        Config {
            widgets: vec![w],
            ..Default::default()
        }
    }

    #[test]
    fn auto_size_grows_with_text() {
        let mut rt = Runtime::new(Config::default(), false).expect("runtime");
        let w = text_widget("t");
        let v = Values::default();
        let (w_px, h_px) = {
            let Runtime { text, icons, .. } = &mut rt;
            let ctx = widget::Ctx {
                text: &*text,
                icons,
                scale: 2.0,
            };
            widget::measure(&w, &ctx, &v, (1000.0, 1000.0))
        };
        assert!(w_px > 30.0, "width {w_px}");
        assert!(h_px > 15.0, "height {h_px}");
    }

    #[test]
    fn larger_text_measures_wider() {
        let mut rt = Runtime::new(Config::default(), false).expect("runtime");
        let v = Values::default();
        let (wa, wb) = {
            let Runtime { text, icons, .. } = &mut rt;
            let ctx = widget::Ctx {
                text: &*text,
                icons,
                scale: 2.0,
            };
            let a = text_widget("a");
            let mut b = text_widget("b");
            if let Element::Text(t) = &mut b.elements[0] {
                t.value = "iiiiiiiiii".into();
            }
            (
                widget::measure(&a, &ctx, &v, (1000.0, 1000.0)).0,
                widget::measure(&b, &ctx, &v, (1000.0, 1000.0)).0,
            )
        };
        assert!(wb > wa, "{wb} vs {wa}");
    }

    #[test]
    fn slow_script_does_not_block_tick() {
        let cfg = cfg_with(
            text_widget("t"),
            Some(script_spec("sleep 0.4; echo v=late", 60.0, &[])),
        );
        let mut rt = Runtime::with_threads(cfg, true, 2).expect("runtime");
        let mut s = sink();
        let t0 = Instant::now();
        rt.tick(&mut s).expect("tick");
        let first = t0.elapsed();
        // Dispatching must return immediately, not after the 400ms script.
        assert!(
            first < Duration::from_millis(250),
            "tick blocked for {first:?}"
        );
        // Nothing landed yet, but the widget still painted its first frame.
        assert!(rt.frames.contains_key("t"));
        assert!(rt.scripts_inflight() == 1, "script should be in flight");
    }

    #[test]
    fn async_values_land_on_a_later_tick() {
        let cfg = cfg_with(
            text_widget("t"),
            Some(script_spec("sleep 0.05; echo v=1", 60.0, &[])),
        );
        let mut rt = Runtime::with_threads(cfg, true, 2).expect("runtime");
        let mut s = sink();
        let painted = rt
            .tick_blocking(&mut s, Duration::from_secs(3))
            .expect("blocking tick");
        assert_eq!(rt.values("t").and_then(|v| v.get("v")), Some("1"));
        // Two paints: the empty first frame, then the value arriving.
        assert!(painted.len() >= 2, "painted {painted:?}");
    }

    #[test]
    fn a_widget_can_be_switched_off_and_back_on() {
        let cfg = Config {
            screen: Frame::new(0.0, 0.0, 400.0, 300.0),
            ..Default::default()
        };
        let with = |off: bool| {
            let mut cfg = cfg.clone();
            let mut a = text_widget("a");
            a.frame = Frame::new(10.0, 10.0, 100.0, 40.0);
            let mut b = text_widget("b");
            b.frame = Frame::new(10.0, 200.0, 100.0, 40.0);
            b.enabled = !off;
            cfg.widgets = vec![a, b];
            cfg
        };

        let mut rt = Runtime::with_threads(with(false), false, 1).expect("runtime");
        assert_eq!(rt.config().widgets.len(), 2, "both are on to start");

        // Switch "b" off: it disappears from the runtime and is reported as
        // removed, which is what closes its window in the presenter.
        let removed = rt.reload(with(true));
        assert_eq!(removed, vec!["b".to_string()]);
        assert_eq!(rt.config().widgets.len(), 1);
        let ids: Vec<&str> = rt.config().widgets.iter().map(|w| w.id.as_str()).collect();
        assert_eq!(ids, vec!["a"]);

        // And back on: a reload adds it again with no restart.
        assert!(rt.reload(with(false)).is_empty());
        assert_eq!(rt.config().widgets.len(), 2);
    }

    #[test]
    fn click_hits_the_right_widget() {
        let mut cfg = Config {
            screen: Frame::new(0.0, 0.0, 400.0, 300.0),
            ..Default::default()
        };
        let mut a = text_widget("left");
        a.frame = Frame::new(10.0, 10.0, 100.0, 40.0);
        let mut b = text_widget("right");
        b.frame = Frame::new(200.0, 10.0, 100.0, 40.0);
        b.layer = 2;
        cfg.widgets = vec![a, b];

        let mut rt = Runtime::with_threads(cfg, true, 1).expect("runtime");
        let mut s = sink();
        rt.tick_blocking(&mut s, Duration::from_secs(1))
            .expect("tick");

        assert_eq!(rt.hit_test(250.0, 20.0), Some("right"));
        assert_eq!(rt.hit_test(50.0, 20.0), Some("left"));
        assert_eq!(rt.hit_test(150.0, 200.0), None);
        assert_eq!(rt.click(250.0, 20.0), Some("right".to_string()));
    }

    #[test]
    fn click_exports_coordinates_to_the_script() {
        let cfg = cfg_with(
            text_widget("t"),
            Some(script_spec(
                // One key per line: the parser is line-oriented, like the original.
                "echo ev=$STRATUM_EVENT; echo at=$STRATUM_CLICK_X,$STRATUM_CLICK_Y",
                0.0,
                &["click"],
            )),
        );
        let mut rt = Runtime::with_threads(cfg, true, 1).expect("runtime");
        let mut s = sink();
        rt.tick_blocking(&mut s, Duration::from_secs(1))
            .expect("tick");

        // Click inside the resolved frame (auto-sized widgets have no config frame).
        let f = rt.frame_of("t").expect("frame");
        let (x, y) = (f.x + f.w / 2.0, f.y + f.h / 2.0);
        assert_eq!(rt.click(x, y), Some("t".to_string()));
        rt.tick_blocking(&mut s, Duration::from_secs(2))
            .expect("tick");
        let v = rt.values("t").expect("values");
        assert_eq!(v.get("ev"), Some("click"));
        assert_eq!(v.get("at"), Some(format!("{x:.1},{y:.1}").as_str()));
    }

    #[test]
    fn events_only_reach_subscribers() {
        let a = text_widget("a");
        let mut b = text_widget("b");
        b.id = "b".into();
        let mut cfg = Config::default();
        let mut aw = a;
        aw.script = Some(script_spec("echo v=1", 0.0, &["space-change"]));
        let mut bw = b;
        bw.script = Some(script_spec("echo v=1", 0.0, &[]));
        cfg.widgets = vec![aw, bw];

        let mut rt = Runtime::with_threads(cfg, true, 1).expect("runtime");
        rt.emit("space-change");
        assert!(rt.pending.contains_key("a"));
        assert!(!rt.pending.contains_key("b"));
    }

    #[test]
    fn widgets_resolve_against_their_own_display() {
        let mut cfg = Config {
            screen: Frame::new(0.0, 0.0, 1440.0, 900.0),
            // Second display sits to the right of the main one, in the shared space.
            screens: vec![
                Frame::new(0.0, 0.0, 1440.0, 900.0),
                Frame::new(1440.0, 0.0, 1920.0, 1080.0),
            ],
            ..Default::default()
        };
        let mut a = text_widget("on-main");
        a.display = 0;
        a.frame = Frame::new(10.0, 10.0, 100.0, 30.0);
        let mut b = text_widget("on-second");
        b.display = 1;
        b.frame = Frame::new(10.0, 10.0, 100.0, 30.0);
        // A typo in `display` must not make the widget vanish.
        let mut c = text_widget("out-of-range");
        c.display = 7;
        c.frame = Frame::new(10.0, 500.0, 100.0, 30.0);
        cfg.widgets = vec![a, b, c];

        assert_eq!(cfg.screen_for(0), Frame::new(0.0, 0.0, 1440.0, 900.0));
        assert_eq!(cfg.screen_for(1).x, 1440.0);
        assert_eq!(cfg.screen_for(9).x, 1440.0, "clamped to the last display");

        let mut rt = Runtime::with_threads(cfg, false, 1).expect("runtime");
        let mut s = sink();
        rt.tick_blocking(&mut s, Duration::from_secs(1))
            .expect("tick");

        assert_eq!(rt.frame_of("on-main").unwrap().x, 10.0);
        assert_eq!(rt.frame_of("on-second").unwrap().x, 1450.0);
        assert_eq!(rt.frame_of("out-of-range").unwrap().x, 1450.0);

        // Hit-testing is in the same shared space, so the second display works.
        assert_eq!(rt.hit_test(1455.0, 20.0), Some("on-second"));
        assert_eq!(rt.hit_test(15.0, 20.0), Some("on-main"));
    }

    fn graph_widget(id: &str, animation_ms: f64) -> Config {
        let mut w = text_widget(id);
        w.elements = vec![Element::Graph(GraphSpec {
            kind: GraphKind::Line,
            data_key: Some("load".into()),
            width: 40.0,
            height: 20.0,
            ..Default::default()
        })];
        Config {
            animation_ms,
            widgets: vec![w],
            ..Default::default()
        }
    }

    fn values_with(key: &str, value: &str) -> Values {
        let mut m = std::collections::HashMap::new();
        m.insert(key.to_string(), value.to_string());
        Values(m)
    }

    #[test]
    fn a_tween_eases_from_old_to_new() {
        let now = Instant::now();
        let t = Tween {
            from: vec![0.0, 0.0],
            to: vec![10.0, 20.0],
            started: now,
        };
        // At t=0 it is at the old values; far past the duration it is at the new.
        assert_eq!(t.sample(now, 100.0), vec![0.0, 0.0]);
        assert_eq!(
            t.sample(now + Duration::from_millis(100), 100.0),
            vec![10.0, 20.0]
        );
        assert_eq!(
            t.sample(now + Duration::from_secs(5), 100.0),
            vec![10.0, 20.0]
        );
        // Halfway: between the two, and monotone.
        let mid = t.sample(now + Duration::from_millis(50), 100.0);
        assert!(mid[0] > 0.0 && mid[0] < 10.0, "mid was {mid:?}");
        // Animation off, or a length change, snaps.
        assert_eq!(t.sample(now, 0.0), vec![10.0, 20.0]);
        let odd = Tween {
            from: vec![0.0],
            to: vec![10.0, 20.0],
            started: now,
        };
        assert_eq!(odd.sample(now, 100.0), vec![10.0, 20.0]);
    }

    #[test]
    fn animating_widgets_repaint_until_they_land() {
        let cfg = graph_widget("g", 60.0);
        let mut rt = Runtime::with_threads(cfg, false, 1).expect("runtime");
        let mut s = sink();

        rt.inject_values("g", values_with("load", "0,0,0"));
        rt.tick_blocking(&mut s, Duration::from_secs(1))
            .expect("tick");

        // New data starts a tween; the following ticks must keep painting.
        rt.inject_values("g", values_with("load", "10,10,10"));
        let first = rt.tick(&mut s).expect("tick");
        assert!(!first.is_empty(), "the frame with new data must paint");

        let second = rt.tick(&mut s).expect("tick");
        assert!(
            !second.is_empty(),
            "a widget mid-tween must repaint even though nothing changed"
        );

        std::thread::sleep(Duration::from_millis(80));
        rt.tick(&mut s).expect("tick");
        let after = rt.tick(&mut s).expect("tick");
        assert!(
            after.is_empty(),
            "once the tween lands it must stop repainting"
        );
    }

    #[test]
    fn animation_off_means_one_frame() {
        let cfg = graph_widget("g", 0.0);
        let mut rt = Runtime::with_threads(cfg, false, 1).expect("runtime");
        let mut s = sink();
        rt.inject_values("g", values_with("load", "0,0,0"));
        rt.tick(&mut s).expect("tick");
        rt.inject_values("g", values_with("load", "10,10,10"));
        rt.tick(&mut s).expect("tick");
        let after = rt.tick(&mut s).expect("tick");
        assert!(after.is_empty(), "no animation, no extra frames");
    }

    #[test]
    fn backoff_grows_and_caps() {
        assert_eq!(backoff_interval(1.0, 0), 1.0);
        assert_eq!(backoff_interval(1.0, 1), 2.0);
        assert_eq!(backoff_interval(1.0, 3), 8.0);
        assert_eq!(backoff_interval(1.0, 20), 64.0, "2^6 of a 1s base");
        assert_eq!(backoff_interval(10.0, 6), 300.0, "capped at five minutes");
        assert_eq!(backoff_interval(0.0, 4), 0.0, "run-once stays run-once");
    }

    #[test]
    fn unchanged_values_do_not_repaint() {
        let cfg = cfg_with(
            text_widget("t"),
            Some(script_spec("echo v=stable", 0.0, &[])),
        );
        let mut rt = Runtime::with_threads(cfg, true, 1).expect("runtime");
        let mut s = sink();
        rt.tick_blocking(&mut s, Duration::from_secs(2))
            .expect("tick");
        let second = rt.tick(&mut s).expect("tick");
        assert!(second.is_empty(), "repainted {second:?}");
    }
}
