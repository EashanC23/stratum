# Examples

Two sets, for two different jobs.

## `widgets.toml` — the tour, in one file

Eight widgets, every element type and layout, with **inline** `command` scripts so
there is nothing to install (one of them is deliberately switched off):

```sh
stratum fixture                              # writes star.svg + diamond.png
stratum render examples/widgets.toml         # -> examples/out/*.png
stratum render examples/widgets.toml --frames 3
```

| widget | shows |
|---|---|
| `host` | bitmap icon + a `static` (immutable) label |
| `disk` | donut from `parts=used,free`, percent template, `always_on_top = true` |
| `load` | bar chart and a caption in a `stack` layout |
| `net` | filled line chart in a `column` layout |
| `battery` | SVG icon tinted at render time + two texts in a `row` |
| `now` | fixed-size plate, `max_chars` truncation |
| `toggle` | `interval = 0` + `on = ["click"]`: event-driven, no polling |
| `off-demo` | `enabled = false`: present in the file, absent from the screen |

## `desktop/` — the configuration this project actually runs

The real XDG tree: one config file, six widget files, six scripts, three assets.
Copy it to `~/.config/stratum/` or point `stratum` at it directly:

```sh
stratum config examples/desktop/config.toml --search   # what resolved
stratum render examples/desktop/config.toml            # -> examples/desktop/out/
stratum live   examples/desktop/config.toml            # on the desktop
```

| widget | what it is | needs |
|---|---|---|
| `clock` | `{{time}}` every second, top-left, no plate | nothing |
| `load` | line chart fed by **Rust** (`scripts/load-rs`, compiled on first run) | `cargo` |
| `credits` | OpenRouter credit donut, `weight = 0.8`, no background | `OPENROUTER_API_KEY` or `~/.config/stratum/secrets/openrouter.key` |
| `lastfm` | now playing / last scrobble + "3m ago" | `LASTFM_API_KEY` + `LASTFM_USER`, or the two files in `~/.config/stratum/secrets/` |
| `stock` | filled line graph + one label, from an HTTP JSON endpoint | nothing (Yahoo Finance needs no key); `STRATUM_TICKER` picks the symbol |
| `nowplaying` | progress donut + "Artist — Title", from a source chain | nothing (MediaRemote, then Music/Spotify, then Last.fm) |

None of the scripts are required for the tree to work: `credits.sh` prints
`label=no API key`, `lastfm.sh` prints `label=lastfm: no key/user`, and
`nowplaying.sh` falls through to "nothing playing yet". That is deliberate — a
widget should read as a sentence in every state, never as half a template.

Secrets belong outside the repo, in `~/.config/stratum/secrets/` (mode `700`,
files `600`). Nothing here reads a key from a config file, and `credits.sh` hands
its token to `curl` through stdin so it never appears in `ps`.

`nowplaying.sh` is worth reading as a worked example of a hard data source: it
chains MediaRemote → Music/Spotify AppleScript → Last.fm → cache, labels each hop
with `source=`, and parses fields with a unit separator rather than whitespace
(`HANDOFF.md` §5 explains both decisions).
