# stratum — handoff

A desktop widget runtime: SketchyBar's drawing/rendering model, lifted out of the
menu bar and generalized so a widget can live anywhere on any display, and so a
widget is a composition of three atomic element types rather than one hardcoded
item.

Status: **working prototype, verified**. It builds, rasterizes, runs scripts and
writes real pixels today. It does not yet put those pixels on screen — the
windowed presenter is the one big piece left (Milestone 5).

    $ cargo run -- fixture
    $ cargo run -- render examples/widgets.toml
    config examples/widgets.toml  6 widget(s)  screen 1512x982 @2x
      host             118.17969x36 at (24, 18)
      disk             74.25391x42 at (1413.7461, 18)
      load             264x70 at (624, 18)
      net              236x76.95508 at (24, 881.0449)
      battery          110.74121x34.132813 at (1377.2588, 923.8672)
      now              260x64 at (626, 459)
    rendered in 1746ms -> out
      composite: out/composite.png

    $ cargo test
    49 unit + 1 binary + 9 CLI + 2 golden passed; 0 failed

    $ stratum init && stratum config
    config      ~/.config/stratum/config.toml
    widgets     2
    files
      ~/.config/stratum/config.toml
      ~/.config/stratum/widgets/clock.toml
      ~/.config/stratum/widgets/load.toml
    widgets:
      clock        desktop  ~/.config/stratum/scripts/clock.sh
      load         desktop  ~/.config/stratum/scripts/load-rs/src/main.rs

    $ stratum live --seconds 5          # no path: XDG discovery
    tick 0: painted clock, load
    tick 24: painted clock
    26 ticks, 2 window(s), 8 present(s)

    $ cargo run -- run examples/widgets.toml --seconds 2 --fps 5
    tick 0: painted host, disk, load, net, battery, now, toggle
    tick 1: painted disk, load, net, battery, now, toggle
    9 ticks, 2 repaint pass(es), 1 script(s) still in flight

    $ cargo run -- click examples/widgets.toml 750 940
    (750, 940) -> widget toggle
      last_event=click
      state=off

    $ cargo run --release -- live examples/widgets.toml --seconds 4
    live: 7 widget(s) on 1440x900 @2x, level 25 (ctrl-c to quit)
    tick 0: painted host, disk, load, net, battery, now, toggle
    23 ticks, 7 window(s), 13 present(s)

    release build renders every widget + composite PNG in ~110ms wall
    (including six `sh -c` script runs and a 3024x1964 PNG encode)

---

## 1. What exists

| file | lines | role |
|---|---|---|
| `src/model.rs` | 639 | widget/element/atom specs, colors, anchors, `Values` template expansion |
| `src/canvas.rs` | 269 | one offscreen RGBA pixmap per widget + primitives (rounded rect, arc, clip, blit) |
| `src/widget.rs` | 282 | measure + layout (row/column/stack) + paint order |
| `src/graph.rs` | 205 | donut / line / bar charts as pure functions |
| `src/icon.rs` | 194 | SVG (resvg) + PNG sources, size-normalized, tinted, cached by (path, mtime, size, tint) |
| `src/text.rs` | 185 | fontdue glyph raster + coverage blending, measurement, truncation |
| `src/nowplaying.rs` | 300 | MediaRemote FFI: the system Now Playing dictionary, no third-party CLI |
| `src/runtime.rs` | 552 | async tick loop, worker dispatch, hit-testing/click, dirty tracking |
| `src/script.rs` | 384 | script execution, timeout, parsing, scheduler, worker pool |
| `src/config.rs` | 168 | TOML/JSON loading + validation |
| `src/present.rs` | 104 | `Presenter` trait + headless PNG implementation |
| `src/main.rs` | 322 | `render` / `run` / `click` / `fixture` / `dump` CLI |
| `examples/widgets.toml` | — | seven widgets exercising every atom, layout mode and the click path |
| `scripts/check_docs.py` | 175 | links, anchors and every fenced config block (via `stratum dump`) |
| `.github/workflows/ci.yml` | — | fmt · clippy · tests · goldens · docs · audit, macOS arm64 |
| `.github/workflows/release.yml` | — | tag → tested binary, `.tar.gz` + sha256, GitHub release |
| `.github/dependabot.yml` | — | weekly cargo/action updates; `tiny-skia`/`fontdue` held back |

Dependencies (deliberately small, no GPU, no windowing): `tiny-skia`, `fontdue`,
`resvg`+`usvg`, `serde`, `serde_json`, `toml`.

Verified visually end to end: donut ring with a hole and correct 63/37 split,
three-bar chart, line chart with filled area, tinted SVG star, tinted PNG
diamond, live values from `sysctl`, static vs dynamic text, `max_chars` clipping
with an ellipsis, auto-sizing, and per-widget PNG output plus a screen composite.

---

## 2. The architecture, and what it replaces

SketchyBar's render path, in the original:

    window.c:51      window_open        borderless, non-activating SLS window
    surface.c:26     surface_create     SLS surface bound to a CAContext id
    context.c:5      context_create     CGBitmapContext at 2x, CTM scaled
    layer.m:13       layer_create       CALayer, anchor/position {0,0}
    bar.c:151        bar_draw           clear + background + every item
    bar_item.c:717   bar_item_draw      background → icon → label → widget
    background.c:196 background_draw    CGPathAddRoundedRect + fill/stroke
    text.c:318       text_draw          CTLineDraw at a text position
    image.c:254      image_draw         CGContextDrawImage, rounded clip
    graph.c:47       graph_draw         the one chart type
    surface.c:82     surface_flush      bitmap → CGImage → layer.contents → flush
    animation.c:4    CVDisplayLink      the clock that drives refreshes

stratum keeps the *shape* of that and swaps the parts that don't generalize:

| SketchyBar | stratum | why |
|---|---|---|
| one `bar_item` = background + icon + label + 1 widget | `WidgetSpec.elements: Vec<Element>` | a widget is any ordered composition |
| fixed paint order | declaration order, three layout modes | composability without new code per combination |
| position from bar geometry (`bar.calculate_bounds`) | `Anchor` resolved against `screen` + optional fixed frame | widgets anywhere on any display |
| `CGImage` only icons | SVG + bitmap, tintable, cached | vector icons at 2x, themeable without duplicate assets |
| one graph (sparkline) | donut / line / bar | requested |
| `CTLineDraw` | fontdue raster + manual coverage blend | no CoreText dependency; text stays a plain buffer |
| `CGBitmapContext` | `tiny_skia::Pixmap` | same model, portable, testable offscreen |
| SLS window per item | `Presenter` trait (PNG now, CALayer later) | keeps windows out of the drawing code |
| `name=value` mach messages | `sh -c` scripts, `key=value` or JSONL on stdout | no IPC to invent; anything that prints works |
| dirty flags (`bar_needs_update`) | value-diff + due-time per widget (`runtime::tick`) | a widget repaints only when its data changed |

Layer boundaries, which are the thing to preserve:

    model (data)  →  layout (measure/place, no pixels)  →  raster (own pixmap)
                  →  atoms (text | icon | graph)        →  presenter (I/O)

Drawing code never sees a window; a window never sees a font. Get this wrong and
you end up with a monolith you can't port.

---

## 3. The model

A widget is data. Nothing about it is compiled in:

```toml
[[widget]]
id = "disk"
anchor = "top-right"           # top-left | top-center | top-right | center* | bottom-*
frame = { x = 24, y = 18 }     # w/h = 0 → auto-size from content
padding = 10                   # scalar or { left, right, top, bottom }
gap = 10                       # space between elements
layout = "row"                 # row | column | stack
background = { enabled = true, color = "#0e0e12e6", corner_radius = 9 }

[[widget.elements]]            # order == paint order
type = "graph"
kind = "donut"
data_key = "parts"             # "used,free" from the script
thickness = 0.62               # donut: hole ratio
width = 22
height = 22
colors = ["#ff8f6b", "#2b2b33"]

[[widget.elements]]
type = "text"
value = "{{pct}}%"
kind = "dynamic"

[widget.script]
command = "echo parts=63,37; echo pct=63"
interval = 2.0
```

Element types (exactly three):

Widget-level placement, in one place because it is the thing people ask about:

    anchor / frame      where it sits on the desktop
    always_on_top       false (default) = belongs to the desktop, app windows cover it
                        true            = floats above applications and the menu bar
    click_through       true (default)  = clicks reach whatever is underneath
    layer               z-order among widgets that share a level

Element types (exactly three):

- `text` — `value`, `kind = "static" | "dynamic"`, `value_key`, `font`, `size`,
  `color`, `align`, `padding`, `max_chars`, `background`.
  - `static` = immutable label: the string is fixed, measured once, never
    re-shaped. Use for captions.
  - `dynamic` = mutable: `{{key}}` placeholders are re-expanded from script
    values each tick; `value_key` replaces the whole string with one value.
- `icon` — `svg` | `bitmap`, `size` (0 = intrinsic), `tint`, `padding`.
- `graph` — `kind = "donut" | "line" | "bar"`, `data` (inline) or `data_key`
  (script series), `colors[]`, `width`/`height`, `thickness`, `weight`, `filled`.

  `weight` (donuts only, default 1.0) is the fraction of the circle the ring
  occupies: 1.0 is a full ring, 0.5 a semicircle, 0.8 a ring with a 20% gap. The
  missing arc is split evenly either side of the bottom, so a partial ring hangs
  off its bottom edge like a dial. Consequence worth knowing: slice boundaries are
  anchored at that bottom point, so a full ring's first slice starts at 6 o'clock
  (it used to start at 12) — the goldens for `donut` were regenerated for exactly
  that reason.

The three `thickness` meanings are documented in `src/graph.rs`: donut = hole
ratio, bar = gap in points, line = stroke width. It's one field because it is
"the one number that changes the shape", but if that reads as overloading when
you extend it, split it (`hole_ratio`, `gap`, `line_width`) — the config schema
is the public API, so decide before you ship a config format.


App-level keys worth knowing:

| key | meaning |
|---|---|
| `screen` | logical rect used for display 0 when no real display is known |
| `screens` | optional list of display rects in one shared top-left space; the live presenter fills this in from `NSScreen` |
| `scale` | device pixels per point (2.0 for Retina) |
| `animation_ms` | graph tween duration; 0 disables animation |
| `widget_dirs` / `script_dirs` | where widget files and scripts live |
| `output.dir` | where the headless presenter writes |

A widget's `display = 1` resolves its `anchor`/`frame` against `screens[1]`, so
"bottom-right of the second monitor" is one line. Out-of-range indices clamp to
the last display instead of making the widget vanish.

### Configuration layout and discovery (XDG)

stratum reads its config from the XDG location, on macOS too:

```text
$XDG_CONFIG_HOME/stratum/config.toml        # else ~/.config/stratum/config.toml
  |- app settings: scale, screen, fonts, output, widget_dirs, script_dirs
  |- widget_dirs = ["widgets", "~/dotfiles/widgets"]
  `- script_dirs = ["scripts"]
$XDG_CONFIG_HOME/stratum/widgets/clock.toml # one widget file (any *.toml)
$XDG_CONFIG_HOME/stratum/scripts/clock.sh   # anything that prints key=value
```

`config.toml` may also be called `.config` or `config` — the dotfile spelling is
searched because people ask for it. Lookup order:

1. an explicit path argument (`stratum render ~/some.toml`);
2. `$STRATUM_CONFIG` (must exist — a typo there is an error, not a fallback);
3. `$XDG_CONFIG_HOME/stratum/{config.toml,.config,config}`;
4. `~/.config/stratum/{config.toml,.config,config}`.

If nothing is found the error lists every path it tried and points at
`stratum init`.

`stratum init` writes a complete starter tree (app config + two widget files +
one shell script + one Rust script + an asset), refuses to overwrite, and is what
the error message suggests. `stratum config [--search]` prints the resolved file,
the dirs, every contributing file, and each widget with its script.

**Path resolution** — relative paths belong to the file that declared them:

| field | resolved against |
|---|---|
| `widget_dirs`, `script_dirs` | the top-level config file's directory |
| a widget's `script.file` | the widget file's directory, then `script_dirs`, then cwd |
| icon `svg` / `bitmap` | the widget file's directory, then `script_dirs`, then cwd |
| `output.dir` | the config file's directory (`~` expanded) |

That is what makes a widget directory movable: `widgets/clock.toml` can say
`file = "clock.sh"` and `svg = "../assets/star.svg"` and it works wherever the
tree lives.

TOML trap worth knowing: bare keys must come **before** the first `[table]` in a
file. Writing `widget_dirs` after `[output]` silently files it under `output`.
stratum rejects unknown keys in `OutputCfg` so that mistake is an error rather
than a config that loads zero widgets.

### Adding a widget (the runbook)

1. `~/.config/stratum/widgets/<id>.toml` — one file, one `[[widget]]`, unique `id`.
2. Place it: `anchor` + `frame` (offset from that corner). Omit `w`/`h` to
   auto-size; give them when the slot must not jitter.
3. Declare elements in paint order — `text`, `icon`, `graph` — under a `layout`
   (`row` / `column` / `stack`), with `padding` and `gap`.
4. Write the data source: any program printing `key=value` lines. Run it by hand
   first — a widget cannot show a value the script never printed.
5. Point `script.file` at it (resolved beside the widget file, then
   `script_dirs`), and give `interval` seconds, or `interval = 0` plus
   `on = ["click"]` for a purely event-driven widget.
6. `stratum render <cfg>` writes the PNGs. `stratum config <cfg> --search` when a
   path does not resolve; `stratum click <cfg> x y` for hit-testing.
7. `stratum live`, then `stratum service install` once it is right.
8. When in doubt, copy one that works: `examples/desktop/widgets/` — five
   complete widgets, one per pattern (smallest, partial donut, Rust script, icon
   + label, two-value donut).

Two path bugs this runbook caught, both fixed and guarded now:

- **Relative script paths broke the spawn.** `invocation()` runs the child with
  `cwd` set to the script's own directory, so a relative program path was
  re-resolved against that directory and every script failed with
  `spawn failed: No such file or directory`. It only appeared when the config was
  loaded from a relative path — exactly `stratum render examples/desktop/config.toml`.
  `std::path::absolute` is now applied when paths resolve (config) and again in
  `invocation()` (defensive), so a script runs from any working directory.
- **The composite stacked every past paint of a widget.** `PngPresenter::present`
  pushed a new entry per call, and a widget is presented at least twice (an empty
  first paint, then again once its script answers). The composite drew the stale
  copy under the fresh one: ghosted text, and an icon displaced into the middle of
  a label. `present` now replaces the entry for that id;
  `composite_keeps_only_the_newest_paint_of_a_widget` is the guard.

---

## 4. Layout rules

`widget::measure` gives intrinsic size; `Anchor::resolve` places it.

- `row`: sizes summed on the main axis, max on the cross axis, `gap` between.
- `column`: the transpose.
- `stack`: every element centred in the content box, except graphs which fill it.
  Declaration order still governs paint order, so "chart behind a label" is
  `graph` then `text` in a `stack` widget.

Auto-size (`frame.w/h == 0`) is what makes a widget content-driven; a fixed
frame is what you use when the widget anchors to something (a corner, a slot) and
must not jitter as its text changes.

Known gap: no text wrapping and no ellipsis-to-box (only `max_chars`). If you add
it, do it in `measure` so layout stays the single source of truth.

---

## 5. Scripting contract

The renderer owns pixels; the script owns truth.

- `command` runs through `sh -c`, so pipelines work, and it may be any binary.
- stdout is parsed as `key=value` lines (blank lines and `#` comments ignored) or
  one JSON object per line (`format = "json-lines"`). **One key per line** — the
  parser is line-oriented, exactly like the original's message format.
- `interval` seconds between runs; `0` = never poll (run once, then only on
  events). `timeout_ms` kills a hung script.
- `on = ["click", "space-change", ...]` re-runs the script when that event fires.
  Events carry context in the environment, not in argv, so one command line
  serves both polling and interaction:
      STRATUM_WIDGET        the widget id
      STRATUM_EVENT         interval | click | hover | space-change | ...
      STRATUM_CLICK_X/Y     screen-space point, on click
- Values reach the model by `{{key}}` in text, `value_key` for whole-string
  overrides, and `data_key` for graph series (`"1 2 3"` or `"1,2,3"`).
- **Scripts run on a worker pool**, never on the render thread
  (`script::ScriptPool`, default one worker per CPU capped at 4). `tick()` only
  dispatches and collects; a widget with a run in flight is not dispatched twice;
  a 400ms script does not delay anyone's frame (there is a test for this).
- `Runtime::tick_blocking` is the one-shot wrapper: drive the async loop until
  nothing is in flight or a deadline hits. `stratum render` uses it; the daemon
  uses `tick()` on a fixed cadence.
- Hit-testing lives in the runtime (`Runtime::hit_test`), because it owns the
  resolved frames; a presenter only has to forward a coordinate. Topmost widget
  wins by `layer`.
- A widget repaints only when its values changed, so a 1 Hz script on a widget
  whose output is stable costs one comparison per tick.

Interactive example (`examples/widgets.toml`, widget `toggle`): `interval = 0`,
`on = ["click"]`, and the script keeps its own state in `/tmp` — the renderer
holds nothing for it, which is the point.

Still missing from scripting (M3 remainder):

1. **Hover and scroll** — `Runtime::click` is the pattern; add `hover(x, y)` on
   pointer motion and `scroll(x, y, dy)` with the same subscription check.
   Needs a presenter that reports pointer events.
2. **Hot config reload** — watch the config file (`notify`/FSEvents) and rebuild
   only changed widgets, mirroring sketchybar's `hotload.c`.
3. **A push channel** — script → widget is solved (stdout). Widget → script is
   solved for discrete events (env + re-run). Continuous bidirectional traffic
   still wants a socket: hand the script a `STRATUM_SOCKET` path and let
   long-running scripts push updates without re-spawning.

---

## 6. Presentation

`present::Presenter` is the seam between "we have pixels" and "the pixels are
somewhere". Two implementations exist; a third is a plausible upgrade.

```rust
pub trait Presenter {
    fn present(&mut self, id: &str, frame: Frame, image: &Pixmap) -> Result<(), String>;
    fn reposition(&mut self, id: &str, frame: Frame) -> Result<(), String> { Ok(()) }
    fn destroy(&mut self, id: &str) -> Result<(), String> { Ok(()) }
}
```

### 6a. `MacPresenter` — LIVE, public AppKit path (done)

`src/live.rs`. One borderless `NSWindow` per widget, each with a layer-backed
`NSView` whose `CALayer.contents` is a `CGImage` built from the widget's pixmap.
This is sketchybar's window→surface→layer stack built without a single private
symbol.

What it does:

- **Clear, chrome-less, shadowless** window: `setOpaque(false)`,
  `setBackgroundColor(clearColor)`, `setHasShadow(false)`, `Borderless` style.
  Anything opaque here paints a rectangle where the rounded corners should be.
- **Desktop level by default**, which is the whole point of a desktop widget:
  `always_on_top = false` puts the window at `kCGDesktopIconWindowLevel`
  (-2147483603) — above the wallpaper and the desktop icons, *below every app
  window*. Open Safari over a widget and Safari covers it.
- **Opt-in floating** for the ones that must stay visible: `always_on_top = true`
  raises the window to `NSStatusWindowLevel` (25, above the menu bar). `--level L`
  changes that opt-in level (3 = floating under the menu bar).
- Constants live in `src/live.rs`: `DESKTOP_LEVEL` (-2147483623, behind even the
  desktop icons), `DESKTOP_ICON_LEVEL` (default), `STATUS_LEVEL` (25).
- **On every Space and over full-screen apps**: `CanJoinAllSpaces | Stationary |
  FullScreenAuxiliary | IgnoresCycle`.
- **Never steals focus**: `NSApplicationActivationPolicy::Accessory` and we never
  call `activate`. No Dock icon, no menu bar takeover, no key-window fight.
- **Click-through by default** (`setIgnoresMouseEvents(true)`), because a widget
  that eats clicks with no click handler is a nuisance. `click_through = false`
  per widget opts in, ready for when the presenter forwards mouse events.
- **Coordinates flipped once**: config frames are top-left origin, AppKit is
  bottom-left; the conversion lives in `appkit_rect` and nowhere else.
- **No implicit animation**: every `layer.contents` swap is wrapped in
  `CATransaction.setDisableActions(true)` — without it each update animates for
  ~250ms and the widget ghosts between values.
- **Run loop pumped** between ticks (`NSRunLoop.runUntilDate`), which is the live
  equivalent of `SLSFlushSurface`.
- **Mouse events reach the runtime.** Each window's view is a `MouseView`
  (`define_class!` subclass of `NSView`) that implements `mouseDown:`/`mouseUp:`
  and `mouseEntered:`/`mouseExited:`, pushing into a queue the loop drains each
  tick. The window never becomes key, so the tracking area uses `ActiveAlways` —
  without it hover would only fire once the app was activated, which never
  happens. `always_on_top`/`click_through` decide whether the window is hit at
  all. `live --inject-click X Y` pushes a synthetic event through the same queue,
  so the whole path is testable without a mouse.
- **Hover is enter/exit, not motion**: one `hover` event per entry, so a script is
  not re-run per pixel.
- **Displays are polled once a second.** Undock, change resolution or wake from
  sleep and the layout is re-read, widgets re-resolved against new rects, and the
  presenter converts through a single constant (the main display's height).
  Verified for placement by a two-screen test; the polling itself is a one-second
  `NSScreen` call.
- **Hot reload.** The config and every file it pulled in (plus the widget
  directories, whose mtime changes when a file is added) are fingerprinted once a
  second. A change re-reads the config, keeps runtime state for surviving widget
  ids, closes windows for removed ones, and repaints. Verified by editing a widget
  and adding another *while* `run` was going.
- **Runs at login** via `stratum service install` (see below).

Run it:

    cargo run --release -- live examples/widgets.toml            # ctrl-c to quit
    cargo run --release -- live examples/widgets.toml --seconds 5 --fps 10
    cargo run --release -- live examples/widgets.toml --level 3  # under the menu bar
    cargo run --release -- live examples/widgets.toml --snapshot shots/

`--snapshot DIR` writes, per widget, a PNG of what AppKit composited plus a table
of the window's frame/level/window number, then exits. That is how this was
verified without Screen Recording permission: `screencapture` refuses from a CLI
process, but `NSView.cacheDisplayInRect:toBitmapImageRep:` composites the layer
tree in-process. Committed evidence: `docs/live-snapshots/*.png` (donut with a
real hole, bar chart with its label, filled line chart, tinted SVG star, tinted
PNG diamond, live `sysctl` values, truncated title).

Measured: 7 windows, 23 ticks in 4s at 10 fps (release), 13 presents. With one
widget opting in (`disk`), `live --snapshot` reports:

    widget          x      y     w      h         level   window   vis
    battery      1305    842   111     34   -2147483603    16489  true
    disk         1342     18    74     42            25    16486  true
    host           24     18   118     36   -2147483603    16485  true
    ... (the rest at desktop level)

Anything below level 0 is composited under normal windows, so "apps cover the
widgets" follows from the level itself. `--snapshot` deliberately does *not*
report occlusion state: `NSWindow.occlusionState` reads false for every window in
a short-lived Accessory process (measured, even at level 25), so it would be a
misleading column.

**What the public path cannot do** (this is the honest gap, not a bug):

| capability | public (shipped) | private SLS |
|---|---|---|
| borderless transparent window | yes | yes |
| desktop-level (apps cover it) | yes (`kCGDesktopIconWindowLevel`) | yes |
| above apps / the menu bar | yes (`level 25`, opt-in per widget) | yes |
| never activates | yes (`Accessory`) | yes (`kCGSPreventsActivationTagBit`) |
| every Space, over full-screen | yes (`CanJoinAllSpaces`) | yes, and a true sticky Space via `SLSSpaceCreate` |
| blurred backdrop behind a widget | no | no — needs `CABackdropLayer`/private |
| per-widget CGS surface | no (one shared CGS window each, still fine) | yes (`SLSAddSurface`/`SLSBindSurface`) |
| App Store / sandbox | yes | never |
| survives OS updates | yes | breaks occasionally |

So the remaining private-API work is **optional**, and buys mainly exotic
capabilities (true sticky Spaces, backdrop blur, per-widget surfaces), not basic
"widgets on the desktop".

### 6a-bis. Running at login

    stratum service status      # plist path, whether it is written, whether it is loaded
    stratum service install     # write ~/Library/LaunchAgents/dev.stratum.widgets.plist + bootstrap
    stratum service uninstall   # bootout + remove

The agent runs `<current_exe> live` with `RunAtLoad` and `KeepAlive`, logging to
`~/.local/state/stratum/widgets.log`. `bootstrap` right after a `bootout` can fail
with `Bootstrap failed: 5` because the domain has not settled — the installer
retries after a short sleep and falls back to `launchctl load -w`. Reinstall after
moving or rebuilding the binary, since the plist records its absolute path.

### 6b. If you do go private

The call sequence is `SLSNewWindowWithOpaqueShapeAndContext` →
`SLSSetWindowResolution(2.0)` → `CAContext contextWithCGSConnection:cid` (private
class, hand-declare it as `src/layer.m:5` does) → set its `layer` →
`SLSAddSurface` / `SLSBindSurface(..., contextId)` / `SLSSetSurfaceBounds` /
`SLSSetSurfaceResolution` / `SLSSetSurfaceColorSpace` / `SLSOrderSurface` →
per frame `layer.contents = CGImage` under `setDisableActions:YES` →
`SLSFlushSurface`. In Rust: `libloading` on
`/System/Library/PrivateFrameworks/SkyLight.framework`, `objc2` for the ObjC
side. Batch moves/reshapes in `SLSTransactionCreate`/`Commit`; on macOS 26+ defer
the layer bounds update into `SBSLSTransactionAddPostDecodeAction`
(`src/window.c:205`, `window_defer_update` `src/window.c:192`).

Keep it behind a presenter flag. Nothing else in the crate needs to know.

### 6c. wgpu — optional

Upload each pixmap as a texture and composite on the GPU for backdrop blur,
rounded clipping of moving surfaces, and GPU-side position/opacity animation. If
you go here, consider `vello` for the vector atoms and dropping the CPU raster.
Do not start here: the CPU path is what makes the whole thing testable.

### 6d. Multi-display

`WidgetSpec.display` is parsed and still advisory; the live presenter resolves
against the main display's frame (and warns when the config's `screen` disagrees,
which it did during development: config said 1512x982, the Mac is 1440x900). The
work: enumerate screens (`NSScreen.screens`), resolve anchors per display, and
key windows by (widget, display).

---

## 7. Testing

Four tiers, cheapest first.

### Tier 0 — the example trees, `tests/examples.rs`

Documentation is executed, not just read: both example trees render with
`--no-scripts` (no network, no credentials), the XDG tree reports its five
widgets, and every `examples/desktop/widgets/*.toml` parses on its own through
`dump`. A widget file people copy that stops parsing is a documentation bug.


### Script runtime guarantees

- **Off the render thread** (`ScriptPool`), so a 10s `cargo` compile costs a
  repaint, not a frozen desktop.
- **Bounded output**: at most 1 MiB of stdout is kept (`MAX_OUTPUT_BYTES`); a
  runaway script cannot grow the process.
- **Backoff**: a failing script's interval doubles per consecutive failure, capped
  at 5 minutes, and resets on success (`backoff_interval`, tested). A broken
  script stops flooding the log.
- **No orphans**: each pool registers its children and SIGTERMs them on shutdown
  (`ChildRegistry`). Per-pool, not global — a global registry kills a *different*
  runtime's scripts, which is exactly what parallel tests exposed.
- **Sane PATH**: children get the inherited PATH plus `~/.cargo/bin`,
  `~/.local/bin`, `/opt/homebrew/bin`, `/usr/local/bin`, `/usr/bin`, `/bin`.
  Without this a LaunchAgent (minimal PATH) reports
  `spawn failed: No such file or directory` for `cargo`, which reads like a
  missing script rather than a missing PATH. Found by running the real service.

### Secrets in scripts

Never put a token in a widget file: that file is meant to be shareable. The
convention used by the shipped `credits` widget (OpenRouter balance):

- the script reads `$OPENROUTER_API_KEY`, else
  `~/.config/stratum/secrets/openrouter.key` (directory mode 700, file 600);
- it hands the token to `curl` through **stdin** as a `--config` document
  (`printf 'header = "Authorization: Bearer %s"' | curl --config - "$URL"`), so it
  never appears in `ps` the way `-H "Authorization: …"` would;
- it always exits 0 and prints a complete `label=` for failure states
  (`no API key`, `credits unreachable`, `bad response`) so the widget degrades to
  readable text instead of an empty template.

Because a script's stdout is the only contract, a widget can show an API-backed
value without the runtime knowing anything about HTTP, JSON, or auth.

### Now playing, and why it is a chain

`src/nowplaying.rs` calls `MRMediaRemoteGetNowPlayingInfo` — MediaRemote is the
same source Control Centre's Now Playing panel uses, and it needs no third-party
CLI. On macOS 26 the daemon answers *unentitled CLI callers* with a NULL
dictionary and zero clients even while audio is playing (verified with a raw C
probe against playing video: `clients=0`, `MRMediaRemoteGetNowPlayingApplicationIsPlaying`
= NO). `stratum nowplaying --why` reports exactly that:

```text
api_present=true
clients=0
dict_present=false
playing=false
verdict=no media client is registered with the daemon; macOS does not expose
now-playing to this unentitled process …
```

So the practical widget is a chain, and every hop is labelled with `source=`:

1. **media** — MediaRemote, when the OS serves it (signed app, or a future macOS).
2. **music / spotify** — that app's AppleScript, still no third-party binary.
3. **lastfm** — the API's `nowplaying` flag, which sees browser playback too.
4. **cache** — the last track, marked `state=last`.

That ordering is what makes a "now playing" widget honest on macOS 26: the
inbuilt source is tried first, and the fallback says which one answered. The
script writes the winning hop to `~/.cache/stratum/nowplaying.source`, so
"which source answered?" is a `cat` away instead of a guess.

Two hazards in that AppleScript hop, both paid for:

- **Never split metadata on whitespace.** `set -- $(… | tr '\t' '\n')` splits on
  spaces *and* newlines, so "Side N**** Anthem" by "Mike Sherm" became
  `title=Side`, `artist=N****`, `album=Anthem` — and a `*` in a title could even
  glob-expand. Fields are joined with ASCII 31 (unit separator), which cannot
  occur in a track name, and read back with `awk -F'\037'`. Verified with a fake
  `osascript` on PATH that returns a space-laden title.
- **Bound the Apple Event.** Music can take two minutes to answer an AE
  (`-1712`); every call is wrapped in `with timeout of 3 seconds` so a stuck
  player costs 3 s and falls through to the next hop instead of stalling the
  widget.

Browser playback is the other half of the picture: Chromium-based browsers
(Vivaldi, Chrome) register **no** media client with `mediaremoted`, so both the
MediaRemote hop and any app-level read miss them — that case only resolves
through Last.fm. Safari and Music do register (Music is readable by AppleScript
even when MediaRemote refuses the CLI).

Four shell-level traps cost real debugging on that widget, and any script that
parses app metadata will meet them:

1. **`set -- $(...)` splits on spaces, not just newlines.** `"Mike Sherm"` becomes
   two fields and every later field shifts by one — the title shows up truncated
   and the artist is the rest of the title. Join fields with a byte metadata
   cannot contain (ASCII 31, unit separator) and split on that byte.
2. **`$(( 7 > 6 ? 7 - 6 : 0 ))` uses constants, not `$7`/`$6`.** The parameters
   need the `$`. Symptom: a donut reporting 85% for a track that is 20% through.
3. **An AppleEvent to a process without Automation permission *hangs*** until the
   2-minute default timeout, so a stuck player costs a widget two minutes. Bound
   every call with `with timeout of 3 seconds`, and let the chain move on.
4. **TCC attributes AppleEvents to the responsible process.** `stratum` has
   `com.apple.Music` granted (visible in the TCC database), while a shell without
   that grant waits on a prompt it cannot display — the same script behaves
   differently under launchd and in a terminal. Test the daemon path through the
   daemon.

Also: a fixture (`STRATUM_NOWPLAYING_JSON`) must never write the "last track"
cache, or the widget keeps showing a made-up track as history.

### Script files

`command` is an inline `sh -c` line. `file` points at a script instead, and is
what a config tree wants:

```toml
[[widget]]
id = "clock"
[[widget.elements]]
type = "text"
value = "{{time}}"
kind = "dynamic"

[widget.script]
file = "clock.sh"        # resolved as described in §3
interval = 1.0
# interpreter = "python3"   # optional: force one
```

Resolution: if the file has the exec bit it is run directly; otherwise the
extension picks an interpreter (`.sh`/`.bash`/`.zsh` → the shell, `.py` →
`python3`, `.rb` → `ruby`, `.js` → `node`, `.lua` → `lua`, `.pl` → `perl`).
`.rs` is treated as a cargo project: the nearest `Cargo.toml` within three
directories above the file is used, and the binary is run with
`cargo run --quiet --manifest-path …` (compiled once, cached by cargo).

Anything that prints `key=value` lines to stdout works, in any language — the
runtime does not care what produced the bytes. For `.rs` the build artifacts are
redirected with `CARGO_TARGET_DIR` into `$XDG_CACHE_HOME/stratum/cargo-target/`,
so `~/.config` stays configuration instead of growing a `target/` tree. `timeout_ms` defaults to 10s
because a Rust script's first run includes a compile; scripts run off the render
thread, so a slow first tick costs a repaint, not a stalled desktop.

### Tier 1 — unit tests, in-module (54)

They live beside the code they test (`#[cfg(test)] mod tests`), so adding a case
is one function in the file you are already editing.

| module | what is asserted |
|---|---|
| `model` | — (covered through `config`) |
| `config` | minimal config parses; every colour syntax; anchor math; **rejects** duplicate ids / no elements / icon without source / out-of-range scale; TOML round-trip is idempotent on `examples/widgets.toml` |
| `script` | `key=value` parsing, comments/blank lines, `{{key}}` templates with missing keys, series parsing (`1,2,3` and `1 2 3`), timeout kills a hung script, stdout capture |
| `runtime` | auto-size from text; slow script does **not** block `tick()`; async values land on a later tick; unchanged values do not repaint; hit-testing; click exports coordinates; events reach only subscribers |
| `widget` | `text_fits_its_measured_box` — measure, render into exactly that box, assert the rightmost ink pixel is inside it (the text-units guard) |
| `live` | CGImage round-trips byte-for-byte against the source pixmap; `write_info_matrix` writes one PNG per bitmapInfo candidate (the empirical record of why `1` is correct) |

### Tier 2 — CLI integration, `tests/cli.rs` (9)

Drives the real binary via `env!("CARGO_BIN_EXE_stratum")`; no harness needed.

- `render` writes `<id>.png` + `composite.png`, both with a real PNG signature
- invalid configs exit non-zero, say why, and leave **no output behind**
- a missing config errors instead of panicking
- `dump` is idempotent (parse → serialise → parse → serialise is stable)
- `click` reports the widget under a point, and `no widget` outside one
- `--help` lists every subcommand

### Tier 3 — golden images, `tests/golden/` (8 widgets x 2 scales)

Renders `tests/golden/widgets.toml` at 1x and 2x and compares each widget pixel
for pixel against the committed PNG of the same name. This is the tier that
catches what assertions cannot: a moved arc, a wrong radius, a colour that lost a
channel, text that drifted a pixel.

    cargo test --test golden                              # compare (what CI runs)
    cargo test --test golden -- --ignored update_goldens  # regenerate, deliberately
    cargo run -- golden tests/golden/widgets.toml         # write them to eyeball
    cargo run -- golden examples/widgets.toml -o /tmp/g  # goldens for any config

Coverage — one widget per atom, plus the compositions that have bitten before:

| golden | what it locks down |
|---|---|
| `plate` | rounded corners, 1pt border, offset-plate shadow, colour parsing |
| `text` | left/centre/right alignment, per-side padding, `max_chars` ellipsis, text background |
| `icons` | SVG rasterised at target size + PNG resampled, both tinted |
| `donut` | three-slice ring: arc cubics, hole ratio, gap between slices |
| `line` | polyline + filled area + min/max normalisation |
| `bar` | rounded bars, gap from `thickness`, colour cycling |
| `stack` | paint order (graph under text) and `Layout::Stack` centring |
| `dynamic` | `{{missing}}` collapses to empty; the plate shrinks to fit (the first-frame state) |

**Determinism** is the whole game, so it is enforced three ways:

1. The font is *pinned* — `tests/golden/fonts/JetBrainsMono-Regular.ttf` (SIL OFL,
   licence beside it). System fonts change across macOS releases and would make
   every golden machine-specific. `load()` in the test asserts a font is pinned
   rather than silently falling back.
2. No scripts, no clock, no sysctl: inline data, `static` text, `Values::default()`.
3. `Cargo.lock` — antialiasing lives in tiny-skia, so the crate versions are part
   of the expectation.

**Tolerance has two limits**, because one is not enough:

- `strong_delta = 24` — a channel jump this big fails on *any single pixel*.
  One magenta speck counts.
- `weak_delta = 2`, `max_weak_fraction = 0.2%` — rounding drift is tolerated, but
  only in small quantity.

A single-limit design either waves through a one-pixel artifact (fraction
threshold) or fails on harmless rounding (exact match). The first version of this
had only the fraction limit and a tampered pixel passed — see
`comparison_rejects_a_tampered_image`, which corrupts one pixel and asserts the
comparison rejects it. A golden harness that cannot fail is worse than none.

Failure messages are meant to be diagnostic, not just red:

    donut@2x.png: 743 of 14976 pixels differ >2 (4.96%), 717 differ >24 (not
    allowed), worst channel delta 241 at (49, 29)

That exact output came from a deliberate mutation (donut `thickness` 0.6 -> 0.4):
2 of 16 goldens failed, the other 14 stayed green. Reverting made it green again.
Reproduce with any one-line change to the golden config.

Regeneration is `#[ignore]`d on purpose, so a normal `cargo test` can never
rewrite the expectations it is checking.

Not covered by goldens: the live presenter itself (window creation, layer
compositing, run-loop pumping). Those need a display; they are verified by
`live --snapshot` against `docs/live-snapshots/`, which is manual by nature.

### In CI

`.github/workflows/ci.yml` (macOS arm64, on every push and PR) runs the gates plus
two things a laptop usually skips:

- `python3 scripts/check_docs.py README.md HANDOFF.md --bin target/release/stratum`
  — links, anchors, and every ` ```toml ` block fed to `stratum dump`, so a broken
  config example fails the build. This caught a fragment in §5 that was not a
  valid widget.
- `cargo audit` — vulnerabilities fail; the one unmaintained-crate warning is
  printed and allowed.

`release.yml` publishes on a `v*` tag after re-running the suite; `dependabot.yml`
holds `tiny-skia` and `fontdue` back because their upgrades can shift antialiasing.

### Running them

    cargo test                      # everything: 61 tests (49 unit + 1 binary + 9 CLI + 2 golden)
    cargo test --lib                # tier 1 only (fast, milliseconds)
    cargo test --test cli           # tier 2 only
    cargo test --test golden        # tier 3 only
    cargo test --test golden -- --ignored update_goldens   # rewrite expectations
    cargo test config               # one module, by name
    cargo test rejects_             # by test-name prefix
    cargo test -- --nocapture       # see eprintln! from failing tests
    STRATUM_TRACE_TEXT=1 cargo test --lib widget -- --nocapture   # per-glyph trace

### Adding a config test

`Config::validate` is where config rules belong; test it through a TOML string so
the test reads like a config:

```rust
#[test]
fn rejects_duplicate_widget_ids() {
    let src = r##"[[widget]]
id = "same"
[[widget.elements]]
type = "text"
value = "a"
"##; // add a second widget with the same id
    let err = cfg_with(src).unwrap_err();
    assert!(err.contains("duplicate widget id"), "{err}");
}
```

`cfg_with` (in `src/config.rs` tests) parses *and* validates. For a new rule, add
the check to `validate()` and assert on the message text — the messages are the
user-facing contract (`error: duplicate widget id "dup"`).

Config *schema* changes also get caught by `round_trips_through_toml`, which
serialises the real example config: adding a field without `#[serde(default)]`
(or a `Deserialize`) breaks it immediately rather than at runtime.

---

## 8. Milestones

- **M0 — scaffold.** done. model, layout, atoms, raster, PNG presenter, CLI.
- **M1 — library split.** done. `lib.rs` + thin `main.rs`; everything public.
- **M2 — atoms complete.** text wrapping/ellipsis-to-box; JPEG via `image`;
  graph axis labels and `show_value` (the field exists, unused); gradient fills
  (a `shader` enum on the plate: solid / linear / radial).
- **M3 — scripting v2.** mostly done: worker pool (off-thread scripts, no
  double-dispatch, `tick_blocking` for one-shots), named events with env-var
  context, runtime hit-testing and click delivery. Remaining: hover/scroll,
  config hot reload, optional `STRATUM_SOCKET` push channel.
- **M4 — golden-image tests.** done. 8 widgets x 1x/2x in `tests/golden/`, a
  pinned OFL font, a two-limit tolerance, an anti-theater test, and an
  `#[ignore]`d regenerator. See §7 tier 3.
- **M5 — live presenter.** done on the public AppKit path (`src/live.rs`):
  per-widget borderless clear windows above the menu bar, layer-backed views,
  CGImage contents, run-loop pumping, `--snapshot` verification. Remaining and
  optional: private SkyLight for sticky Spaces/backdrop blur/per-widget surfaces
  (§6b).
- **M6 — display + Spaces.** multi-monitor done (per-widget `display`, shared
  top-left space, one-second re-poll); Spaces work through
  `CanJoinAllSpaces`; per-display scale is still one global `scale`.
- **M7 — ergonomics.** animation (done: `animation_ms`, smoothstep on graph
  series), hot reload (done), login item (done: `stratum service`), a widget
  "block" system (reusable compositions), theme
  tokens (colors/radii/fonts by name, referenced from widgets), `stratum validate`
  for CI.

---

## 9. Honest limitations of the current code

- **No text shaping.** fontdue is glyph-per-char: no ligatures, no kerning
  pairs, no bidi, no complex-script shaping. Text is laid out per glyph, so
  `text_fits_its_measured_box` (src/widget.rs) is the guard that keeps
  measurement and rasterisation in the same units. Swap in `rustybuzz` +
  `cosmic-text` at the `TextEngine` boundary when this matters — nothing else has
  to change.
- **JPEG icons are rejected** with an explicit error rather than silently
  ignored (`src/icon.rs`). PNG and SVG work.
- **Shadows are faked** as an offset plate, not blurred; the doc comment says to
  let the compositor do it (`CALayer.shadowRadius`). On the PNG presenter you get
  the offset only.
- **No damage tracking.** A widget that repaints clears and redraws its whole
  bitmap. Fine at widget sizes; would matter for a full-screen overlay.
- **Scripts are off-thread, but spawn-heavy:** every run is a fresh `sh -c`
  (~5-10ms). A long-lived script should use the socket channel (M3 remainder)
  rather than being spawned on every tick.
- **Scale is per-config, not per-display.** `contentsScale` is 2.0 for
  everything, exactly like the original, which is wrong on a 1x external display.
- **`Frame` is the screen's top-left origin**, y-down. If you ever mix with
  AppKit's bottom-left coords, flip in the presenter, not in the model.

---

## 10. Open decisions

1. Per-widget windows vs one overlay per display (recommendation above).
2. Private SkyLight APIs vs staying on public NSWindow and losing
   above-menu-bar/sticky-Spaces. This changes the product, not just the code.
3. Process model: one daemon per user (needed for scripting and hot reload) vs
   a library an app embeds. Recommend the daemon, with the library underneath.
4. Config format lock-in: TOML now; decide whether widget definitions may come
   from scripts too (SketchyBar allows `sketchybar --add item ...` at runtime).
   Dynamic widget creation is the single biggest API-shaping question.
5. Click/hover delivery: socket vs stdin, and whether hit-testing lives in the
   presenter (window-level) or the runtime (frame-level). Recommend presenter:
   it already knows window coordinates.

---

## 11. Security

Threat model: stratum is a local, single-user desktop tool acting on your behalf.
No network listener, no setuid, no IPC socket, no privileged operations. The
trust boundary is the **config tree and the scripts it names** — a config is code,
exactly as it is in SketchyBar. Only run configs you would run yourself.

Controls in place, and why:

| # | Control | Where |
|---|---|---|
| 1 | Scripts are named explicitly (`command` or `file`); a file runs by path, argv-style, never through a shell. `sh -c` exists only for the inline form the user writes. | `script.rs` |
| 2 | Secrets never live in a widget file: env var or a 0600 file under `~/.config/stratum/secrets/`. The OpenRouter token goes to curl via **stdin** `--config`, so it is not in `ps`. | `scripts/credits.sh` |
| 3 | Bounded inputs: script stdout capped at 1 MiB, config at 4 MiB, per-script timeout 10 s by default, exponential backoff on repeated failure. | `script.rs`, `config.rs`, `runtime.rs` |
| 4 | No orphaned processes: each pool SIGTERMs its own children on shutdown (per-owner registry, not a global one). | `script.rs` |
| 5 | LaunchAgent plist values are XML-escaped, so a path containing `<`, `&` or quotes cannot inject keys into a file launchd executes. | `main.rs` (`xml_escape`) |
| 6 | `unsafe` is confined to `src/live.rs` FFI/AppKit plus one `kill(2)` call; every block carries a `// SAFETY:` justification. No manual `Send`/`Sync`, no `mem::zeroed`, no `uninitialized`, no raw pointer escapes into safe code. | `live.rs`, `script.rs` |
| 7 | Child `PATH` is set explicitly (inherited + `~/.cargo/bin`, `~/.local/bin`, Homebrew, `/usr/local/bin`), so a bare `cargo`/`python3` never resolves through a relative/attacker-controlled entry in a daemon's minimal environment. | `script.rs` (`child_path`) |
| 8 | Dependency advisories: `cargo audit` (see the run log in §7). | CI |
| 9 | Production code contains **zero** `unwrap()`/`expect()` outside `#[cfg(test)]`; every fallible step returns `Result` with a message naming the file. | all of `src/` |

Residual risks (stated rather than hidden):

- A malicious config can run arbitrary commands as you. That is the product, not a
  bug; the mitigation is that configs are yours and version-controlled.
- SVG/PNG decoding is memory-safe Rust but not sandboxed; a pathological SVG can
  be slow, though `timeout_ms` does not cover parse-time (it happens on the render
  thread). Keep icon assets local and trusted.
- The API token passes through a pipe to `curl`; anything with ptrace rights on
  your user could read it.
- The binary is unsigned/not notarised; launchd runs whatever absolute path the
  plist records — reinstall after moving it.
- `--inject-click` only synthesises events for this process.
- The objc2/CoreGraphics FFI cannot run under Miri, so those 18 blocks are
  reviewed manually (SAFETY comments) rather than machine-checked. Everything
  else is ordinary safe Rust covered by the test suite.

## 12. Performance and resource profile

Measured on the development machine (arm64, `cargo build --release` with
`lto = "fat"`, `codegen-units = 1`, `strip = "symbols"`). Numbers are from
`/usr/bin/time -l`, `ps`, and `vmmap -summary`, not estimates.

| metric | value |
|---|---|
| release binary | 3.70 MB (5.31 MB before LTO + strip) |
| daemon physical footprint / RSS | 27.4 MB / 28 MB, 9 threads |
| daemon CPU (1 Hz clock + 128 px donut, 10 fps tick) | 0.08 s CPU per 20 s wall = **0.4 % of one core** |
| headless render, 7 widgets @2x | 0.08–0.12 s |
| headless render peak RSS, `--no-composite` | 38.0 MB |
| headless render peak RSS, with composite | 83.2 MB — the full-screen 2× PNG is ~48 MB of it |
| glyph draw, cached vs re-rasterised | 29.4 µs vs 48.0 µs per draw (1.6×) |
| config that pins its fonts vs system chain | 20.6 MB vs 41.2 MB peak (**−50 %**) |

Where the memory goes, in the headless path: binary + dyld floor ≈ 9 MB, the 2×
composite canvas ≈ 24 MB, its PNG encode ≈ 24 MB, fonts 8–21 MB depending on
whether the system chain is loaded, widget canvases a few hundred KB each. The
daemon does none of the composite work, which is why it sits at ~27 MB.

What was changed to get there (each with a measurement or a test behind it):

- **Glyph cache** keyed by (font index, pixel size, char, premultiplied colour).
  Before: every glyph of every text element was re-rasterised and re-blended on
  every repaint. `cargo test --release --lib bench_glyph -- --ignored --nocapture`
  reproduces the 1.6× figure; `text::tests::repeats_of_the_same_glyph_hit_the_cache`
  asserts the mechanism.
- **No deep clone per tick.** `tick()` used to `.clone()` the whole widget list
  twice a tick (elements, strings, backgrounds). It now borrows for dispatch and
  `mem::take`s each spec for the duration of its render.
- **Values moved, not cloned.** A `HashMap<String, String>` was cloned per
  repaint; it is now taken out of the map and put back on every exit path.
- **`screen_for` allocates nothing** (it cloned the screens `Vec` per widget per
  repaint).
- **Icon mtime memoised for 1 s**, so a repaint does not `stat` per icon.
- **Font chain skipped when it is not needed.** A config that pins every font it
  uses no longer parses SFNS + Helvetica + Menlo (~20 MB). Guarded by
  `golden::tests::a_fully_pinned_config_does_not_load_system_fonts`.
- **Release profile**: LTO, one codegen unit, stripped symbols → 30 % smaller
  binary, no unwinding machinery lost (`panic` left as unwind for backtraces).

Known remaining costs, stated rather than hidden:

- Idle CPU is ~0.4 % because the loop ticks at `--fps` (10) whether or not
  anything is dirty. An event-driven wake (repaint only when a script lands or
  input arrives) would take it to ~0, at the cost of a more complex loop.
- The composite PNG is the single largest allocation in a headless render; use
  `--no-composite` (or `composite = false`) when you do not need it.
- Fonts are parsed whole. ~8 MB for SFNS + SFNSMono is the floor for the default
  stack; a pinned small font is ~300 KB of file but several MB parsed.

## 13. Cheatsheet

    stratum init                               # starter tree in ~/.config/stratum
    stratum config [--search]                  # what would run, and where from
    stratum render                             # no path: XDG discovery
    stratum live --seconds 5                   # same, on the desktop
    cargo run -- fixture                       # write examples/{star.svg,diamond.png}
    cargo run -- render examples/widgets.toml  # all widgets -> out/*.png + composite
    cargo run -- render cfg.toml -o /tmp/x --scale 1 --no-scripts --event space-change
    cargo run -- run cfg.toml --seconds 5 --fps 10      # non-blocking loop
    cargo run -- click cfg.toml 750 940                 # hit-test + click + drain
    stratum nowplaying [--why]                 # current track; --why explains emptiness
    stratum service install                    # widgets at login (LaunchAgent)
    stratum live --inject-click 300 40         # test hook: synthetic click
    cargo run --release -- live examples/widgets.toml   # widgets on the desktop
    cargo run --release -- live examples/widgets.toml --snapshot shots/   # + evidence
    cargo run -- dump examples/widgets.toml    # parse + re-serialize (schema check)
    cargo test
    cargo build --release                      # ~240ms render for 7 widgets + composite

Persist this habit from the original: **one drawable per widget, cleared and
redrawn from scratch, presented once.** It is the reason arbitrary restyling is
cheap, and it is the property that will still be true on the day you swap the
rasterizer out.
