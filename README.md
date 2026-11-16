# stratum

Compositor-backed desktop widgets: SketchyBar's drawing model, generalized so a
widget can sit anywhere on any display and compose three atomic element types —
text (static or dynamic), icon (SVG or bitmap), graph (donut / line / bar).

Config lives in the XDG directory — `~/.config/stratum/config.toml`, with widget
files in `widgets/` and scripts (shell, Python, Ruby, JS, Lua, Perl, or a Rust
cargo project) in `scripts/`. `stratum init` writes a working tree; `stratum
config` tells you exactly what would run and where it came from.

    stratum init && stratum live

Read [HANDOFF.md](HANDOFF.md) first — it documents the architecture, the mapping
to the original C implementation, what is built, what is not, and the decisions
left to make (chiefly: private SkyLight windows vs public NSWindow).

Quick start:

    cargo run --release -- live examples/widgets.toml   # real widgets on the desktop
    cargo run --release -- render --no-composite        # headless, smallest footprint
    cargo run -- fixture                        # demo assets
    cargo run -- render examples/widgets.toml   # -> out/*.png + out/composite.png
    cargo run -- run examples/widgets.toml --seconds 5   # non-blocking loop
    cargo run -- click examples/widgets.toml 750 940     # hit-test + click + run
    cargo test                                  # 61 tests (49 unit, 9 CLI, 2 golden, 1 binary)

macOS puts them on the real desktop (`stratum live`): desktop-level by default,
so app windows cover them like they cover the wallpaper. Add
`always_on_top = true` to a widget to float it above applications instead. `stratum render` writes PNGs for tests/CI. Evidence of
the live path: docs/live-snapshots/. Rendering is locked down by golden images in
tests/golden/ (8 widgets, 1x and 2x, pinned font):
`cargo test --test golden` compares, `-- --ignored update_goldens` rewrites.
