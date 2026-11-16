//! The persistent data model.
//!
//! This is the "what", mirroring sketchybar's C structs (`struct bar_item`,
//! `struct text`, `struct background`, `struct graph`) but expressed as a serde
//! spec so it can come from TOML, JSON or a script patch. Nothing in this file
//! knows how to draw.

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::collections::HashMap;
use std::path::PathBuf;

// ---------------------------------------------------------------- colour ----

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Rgba {
    pub r: u8,
    pub g: u8,
    pub b: u8,
    pub a: u8,
}

impl Rgba {
    pub const TRANSPARENT: Rgba = Rgba {
        r: 0,
        g: 0,
        b: 0,
        a: 0,
    };
    pub const WHITE: Rgba = Rgba {
        r: 255,
        g: 255,
        b: 255,
        a: 255,
    };
    pub const BLACK: Rgba = Rgba {
        r: 0,
        g: 0,
        b: 0,
        a: 255,
    };

    pub const fn rgb(r: u8, g: u8, b: u8) -> Self {
        Rgba { r, g, b, a: 255 }
    }
    pub const fn with_alpha(self, a: u8) -> Self {
        Rgba { a, ..self }
    }

    /// Accepts `#rgb`, `#rrggbb`, `#rrggbbaa`, `0xrrggbb`, `0xrrggbbaa`.
    pub fn parse(s: &str) -> Result<Rgba, String> {
        let raw = s.trim();
        let t = raw.trim_start_matches('#');
        let t = t
            .strip_prefix("0x")
            .or_else(|| t.strip_prefix("0X"))
            .unwrap_or(t);
        let hex = |i: usize| -> Result<u8, String> {
            u8::from_str_radix(&t[i..i + 2], 16).map_err(|_| format!("bad hex colour {raw:?}"))
        };
        match t.len() {
            6 => Ok(Rgba {
                r: hex(0)?,
                g: hex(2)?,
                b: hex(4)?,
                a: 255,
            }),
            8 => Ok(Rgba {
                r: hex(0)?,
                g: hex(2)?,
                b: hex(4)?,
                a: hex(6)?,
            }),
            3 => {
                let d = |i: usize| -> Result<u8, String> {
                    let one = &t[i..i + 1];
                    u8::from_str_radix(&format!("{one}{one}"), 16)
                        .map_err(|_| format!("bad hex colour {raw:?}"))
                };
                Ok(Rgba {
                    r: d(0)?,
                    g: d(1)?,
                    b: d(2)?,
                    a: 255,
                })
            }
            _ => Err(format!(
                "bad hex colour {raw:?} (expected #rgb, #rrggbb or #rrggbbaa)"
            )),
        }
    }

    pub fn to_hex(self) -> String {
        format!("#{:02x}{:02x}{:02x}{:02x}", self.r, self.g, self.b, self.a)
    }

    /// Premultiplied (r,g,b,a) bytes, which is the form both tiny-skia and
    /// kCGImageAlphaPremultipliedFirst want.
    pub fn premultiplied(self) -> [u8; 4] {
        let a = self.a as u32;
        let f = |c: u8| ((c as u32 * a + 127) / 255) as u8;
        [f(self.r), f(self.g), f(self.b), self.a]
    }
}

impl Default for Rgba {
    fn default() -> Self {
        Rgba::TRANSPARENT
    }
}

impl Serialize for Rgba {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.to_hex())
    }
}

impl<'de> Deserialize<'de> for Rgba {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        Rgba::parse(&s).map_err(serde::de::Error::custom)
    }
}

// ----------------------------------------------------------------- frame ----

/// Logical (point) rectangle. Origin is top-left, y grows downwards — the same
/// convention as a Core Graphics bitmap context after the flip in `context.c`,
/// and what you want for laying out across a whole screen.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq)]
pub struct Frame {
    #[serde(default)]
    pub x: f32,
    #[serde(default)]
    pub y: f32,
    #[serde(default)]
    pub w: f32,
    #[serde(default)]
    pub h: f32,
}

impl Frame {
    pub const fn new(x: f32, y: f32, w: f32, h: f32) -> Self {
        Frame { x, y, w, h }
    }
    pub fn contains(&self, x: f32, y: f32) -> bool {
        x >= self.x && y >= self.y && x < self.x + self.w && y < self.y + self.h
    }
    pub fn right(&self) -> f32 {
        self.x + self.w
    }
    pub fn bottom(&self) -> f32 {
        self.y + self.h
    }
}

/// Which corner of the screen the widget's `x`/`y` are measured from. This is
/// the whole reason a widget can live anywhere on the desktop instead of being
/// pinned to a bar: the anchor resolves against whichever display the widget
/// is assigned to.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Anchor {
    #[default]
    TopLeft,
    TopCenter,
    TopRight,
    CenterLeft,
    Center,
    CenterRight,
    BottomLeft,
    BottomCenter,
    BottomRight,
}

impl Anchor {
    /// Resolve a widget frame against a screen frame. `frame.w`/`frame.h` of 0
    /// mean "use the measured (intrinsic) size".
    // A 1-byte enum: taking `self` by reference costs an indirection for nothing,
    // but `anchor.resolve(...)` reads better than `Anchor::resolve(*self, ...)`.
    #[expect(
        clippy::trivially_copy_pass_by_ref,
        reason = "method-call syntax on a Copy enum; a by-value receiver would be `*self` at every call site"
    )]
    pub fn resolve(&self, screen: Frame, frame: Frame, measured: (f32, f32)) -> Frame {
        let w = if frame.w > 0.0 { frame.w } else { measured.0 };
        let h = if frame.h > 0.0 { frame.h } else { measured.1 };
        // Offsets are relative to the SCREEN's own origin, so this is correct for
        // a display that is not at (0,0) — a second monitor to the right or left.
        let (ox, oy) = (screen.x, screen.y);
        let (x, y) = match self {
            Anchor::TopLeft => (ox + frame.x, oy + frame.y),
            Anchor::TopCenter => (ox + screen.w / 2.0 + frame.x - w / 2.0, oy + frame.y),
            Anchor::TopRight => (ox + screen.w - w - frame.x, oy + frame.y),
            Anchor::CenterLeft => (ox + frame.x, oy + screen.h / 2.0 + frame.y - h / 2.0),
            Anchor::Center => (
                ox + screen.w / 2.0 + frame.x - w / 2.0,
                oy + screen.h / 2.0 + frame.y - h / 2.0,
            ),
            Anchor::CenterRight => (
                ox + screen.w - w - frame.x,
                oy + screen.h / 2.0 + frame.y - h / 2.0,
            ),
            Anchor::BottomLeft => (ox + frame.x, oy + screen.h - h - frame.y),
            Anchor::BottomCenter => (
                ox + screen.w / 2.0 + frame.x - w / 2.0,
                oy + screen.h - h - frame.y,
            ),
            Anchor::BottomRight => (ox + screen.w - w - frame.x, oy + screen.h - h - frame.y),
        };
        Frame::new(x, y, w, h)
    }
}

/// Per-side padding, in points.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq)]
pub struct Padding {
    #[serde(default)]
    pub left: f32,
    #[serde(default)]
    pub right: f32,
    #[serde(default)]
    pub top: f32,
    #[serde(default)]
    pub bottom: f32,
}

impl Padding {
    pub const fn all(v: f32) -> Self {
        Padding {
            left: v,
            right: v,
            top: v,
            bottom: v,
        }
    }

    pub fn horizontal(&self) -> f32 {
        self.left + self.right
    }
    pub fn vertical(&self) -> f32 {
        self.top + self.bottom
    }
}

/// Serde-friendly shorthand so config can write `padding = 6` instead of
/// `padding = { left = 6, right = 6, ... }`.
#[derive(Debug, Clone, Copy, Serialize)]
#[serde(transparent)]
#[derive(Default)]
pub struct PaddingShorthand(pub Padding);

impl<'de> Deserialize<'de> for PaddingShorthand {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct V;
        impl<'de> serde::de::Visitor<'de> for V {
            type Value = PaddingShorthand;
            fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                f.write_str("a number or a table of left/right/top/bottom")
            }
            fn visit_f64<E: serde::de::Error>(self, v: f64) -> Result<Self::Value, E> {
                Ok(PaddingShorthand(Padding::all(v as f32)))
            }
            fn visit_i64<E: serde::de::Error>(self, v: i64) -> Result<Self::Value, E> {
                Ok(PaddingShorthand(Padding::all(v as f32)))
            }
            fn visit_map<A: serde::de::MapAccess<'de>>(
                self,
                mut m: A,
            ) -> Result<Self::Value, A::Error> {
                let mut p = Padding::default();
                while let Some(k) = m.next_key::<String>()? {
                    let v: f32 = m.next_value()?;
                    match k.as_str() {
                        "left" => p.left = v,
                        "right" => p.right = v,
                        "top" => p.top = v,
                        "bottom" => p.bottom = v,
                        other => {
                            return Err(serde::de::Error::custom(format!(
                                "unknown padding field {other:?}"
                            )));
                        }
                    }
                }
                Ok(PaddingShorthand(p))
            }
        }
        d.deserialize_any(V)
    }
}

// ------------------------------------------------------------ background ----

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct Shadow {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_shadow_color")]
    pub color: Rgba,
    #[serde(default)]
    pub radius: f32,
    #[serde(default)]
    pub y_offset: f32,
}

fn default_shadow_color() -> Rgba {
    Rgba {
        r: 0,
        g: 0,
        b: 0,
        a: 90,
    }
}

impl Default for Shadow {
    fn default() -> Self {
        Shadow {
            enabled: false,
            color: default_shadow_color(),
            radius: 0.0,
            y_offset: 0.0,
        }
    }
}

/// The rounded-rect plate behind a widget or an element. Direct port of
/// `struct background` + `background_draw()` (src/background.c:196).
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct Background {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub color: Rgba,
    #[serde(default)]
    pub corner_radius: f32,
    #[serde(default)]
    pub border_width: f32,
    #[serde(default)]
    pub border_color: Rgba,
    #[serde(default)]
    pub shadow: Shadow,
}

impl Default for Background {
    fn default() -> Self {
        Background {
            enabled: false,
            color: Rgba::TRANSPARENT,
            corner_radius: 0.0,
            border_width: 0.0,
            border_color: Rgba::TRANSPARENT,
            shadow: Shadow::default(),
        }
    }
}

// ------------------------------------------------------------------ text ----

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Align {
    #[default]
    Left,
    Center,
    Right,
}

/// Immutable vs mutable text, the distinction the widget model needs and a
/// status bar does not: a `static` label is measured once and never re-shaped,
/// a `dynamic` value is re-read from the script and re-shaped every tick.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum TextKind {
    #[default]
    Dynamic,
    Static,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TextSpec {
    /// Literal text, or a template with `{{key}}` placeholders filled from the
    /// widget's script values.
    #[serde(default)]
    pub value: String,
    #[serde(default)]
    pub kind: TextKind,
    /// Key in the script's value map that overrides `value` when `dynamic`.
    #[serde(default)]
    pub value_key: Option<String>,
    #[serde(default = "default_font")]
    pub font: String,
    #[serde(default = "default_font_size")]
    pub size: f32,
    #[serde(default = "default_fg")]
    pub color: Rgba,
    #[serde(default)]
    pub align: Align,
    #[serde(default)]
    pub padding: PaddingShorthand,
    /// Truncate to this many characters (renders an ellipsis), like
    /// `text->max_chars` in src/text.c.
    #[serde(default)]
    pub max_chars: Option<usize>,
    #[serde(default)]
    pub background: Background,
}

fn default_font() -> String {
    "default".to_string()
}
fn default_font_size() -> f32 {
    12.0
}
fn default_fg() -> Rgba {
    Rgba::WHITE
}

impl Default for TextSpec {
    fn default() -> Self {
        TextSpec {
            value: String::new(),
            kind: TextKind::Dynamic,
            value_key: None,
            font: default_font(),
            size: default_font_size(),
            color: default_fg(),
            align: Align::Left,
            padding: PaddingShorthand(Padding::default()),
            max_chars: None,
            background: Background::default(),
        }
    }
}

// ------------------------------------------------------------------ icon ----

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct IconSpec {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub svg: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bitmap: Option<PathBuf>,
    /// Box size in points. 0 = intrinsic (aspect preserved).
    #[serde(default)]
    pub size: f32,
    /// Override painted colour (multiplies coverage) — this is how a symbolic
    /// SVG becomes a themed icon without shipping one file per colour.
    #[serde(default)]
    pub tint: Option<Rgba>,
    #[serde(default)]
    pub padding: PaddingShorthand,
}

// ----------------------------------------------------------------- graph ----

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
#[derive(Default)]
pub enum GraphKind {
    Donut,
    #[default]
    Line,
    Bar,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GraphSpec {
    #[serde(default)]
    pub kind: GraphKind,
    /// Inline data.
    #[serde(default)]
    pub data: Vec<f32>,
    /// Script key holding comma/space separated numbers; overrides `data`.
    #[serde(default)]
    pub data_key: Option<String>,
    /// One colour per slice/bar/point; the last colour repeats.
    #[serde(default)]
    pub colors: Vec<Rgba>,
    /// Fixed box. 0 = take the space the widget layout gives it.
    #[serde(default)]
    pub width: f32,
    #[serde(default)]
    pub height: f32,
    /// Line width / bar gap / donut hole ratio depending on `kind`.
    #[serde(default = "default_graph_thickness")]
    pub thickness: f32,
    /// Donuts only: how much of the circle the ring occupies, as a decimal.
    /// `1.0` is a full ring, `0.5` a semicircle. The missing part is split
    /// evenly on either side of the bottom, so a partial ring hangs off its
    /// bottom edge like a dial. Ignored by line and bar.
    #[serde(default = "default_weight")]
    pub weight: f32,
    #[serde(default = "default_true")]
    pub filled: bool,
    #[serde(default)]
    pub show_value: bool,
}

fn default_graph_thickness() -> f32 {
    2.0
}
fn default_weight() -> f32 {
    1.0
}
fn default_true() -> bool {
    true
}

impl Default for GraphSpec {
    fn default() -> Self {
        GraphSpec {
            kind: GraphKind::Line,
            data: Vec::new(),
            data_key: None,
            colors: vec![Rgba::rgb(0x88, 0xcc, 0xff)],
            width: 0.0,
            height: 0.0,
            thickness: default_graph_thickness(),
            weight: default_weight(),
            filled: true,
            show_value: false,
        }
    }
}

// --------------------------------------------------------------- element ----

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "kebab-case")]
pub enum Element {
    Text(TextSpec),
    Icon(IconSpec),
    Graph(GraphSpec),
}

// ---------------------------------------------------------------- widget ----

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Layout {
    #[default]
    Row,
    Column,
    /// Everything overlapping in the same box — icon under text, sparkline
    /// behind a label, etc.
    Stack,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ScriptFormat {
    /// sketchybar's own wire format: `key=value` lines on stdout.
    #[default]
    KeyValue,
    /// One JSON object per line.
    JsonLines,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScriptSpec {
    /// Run through `sh -c`, so shell pipelines work. Ignored when `file` is set.
    #[serde(default)]
    pub command: String,
    /// Path to a script file. Taken as executable if it has the exec bit,
    /// otherwise run through an interpreter chosen from the extension
    /// (`.sh`, `.py`, `.rb`, `.js`, `.lua`, `.pl`, `.rs` via a sibling
    /// Cargo.toml). Relative paths resolve against the widget file, then the
    /// config's `script_dirs`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file: Option<PathBuf>,
    /// Force an interpreter: `interpreter = "python3"`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub interpreter: Option<String>,
    /// Seconds between runs. <= 0 means "run once, then only on demand".
    #[serde(default = "default_interval")]
    pub interval: f64,
    #[serde(default)]
    pub format: ScriptFormat,
    #[serde(default = "default_timeout_ms")]
    pub timeout_ms: u64,
    /// Events that re-run this script immediately, in addition to `interval`.
    /// Built-ins: `start`, `click`, `hover`, `scroll`, `space-change`,
    /// `display-change`, `bar-hidden`. The event name is exported to the script
    /// as `STRATUM_EVENT`; the widget id as `STRATUM_WIDGET`.
    #[serde(default)]
    pub on: Vec<String>,
}

fn default_interval() -> f64 {
    1.0
}
fn default_timeout_ms() -> u64 {
    // Generous on purpose: a `.rs` script compiles through cargo on first run,
    // which is slower than any sane runtime. Scripts run off the render thread,
    // so a slow first tick costs a repaint, not a stalled desktop.
    10_000
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct WidgetSpec {
    #[serde(default)]
    pub id: String,
    /// Which display this widget belongs to (0 = main). Purely advisory until
    /// the presenter layer learns about multiple displays.
    #[serde(default)]
    pub display: u32,
    #[serde(default)]
    pub anchor: Anchor,
    #[serde(default)]
    pub frame: Frame,
    #[serde(default)]
    pub layout: Layout,
    #[serde(default)]
    pub padding: PaddingShorthand,
    #[serde(default)]
    pub gap: f32,
    #[serde(default)]
    pub background: Background,
    #[serde(default)]
    pub elements: Vec<Element>,
    #[serde(default)]
    pub script: Option<ScriptSpec>,
    /// Draw order across widgets; higher is on top.
    #[serde(default)]
    pub layer: i32,
    /// Let clicks pass through to whatever is underneath. Default true: a
    /// widget that eats clicks with no click handler is a nuisance. Set false
    /// for interactive widgets once the presenter forwards mouse events.
    #[serde(default = "default_true")]
    pub click_through: bool,
    /// Float above application windows. Default FALSE: a widget belongs to the
    /// desktop and app windows cover it, like a wallpaper with data on it. Set
    /// true for the ones that must stay visible while you work.
    #[serde(default)]
    pub always_on_top: bool,
}

impl WidgetSpec {
    pub fn padding_px(&self) -> Padding {
        self.padding.0
    }
}

// ------------------------------------------------------------- script I/O ----

/// Values a widget has learned from its script this cycle.
#[derive(Debug, Clone, Default)]
pub struct Values(pub HashMap<String, String>);

impl Values {
    pub fn get(&self, k: &str) -> Option<&str> {
        self.0.get(k).map(|s| s.as_str())
    }

    /// Parse a value as a numeric series (`"1 2 3"`, `"1,2,3"`).
    pub fn series(&self, k: &str) -> Option<Vec<f32>> {
        let raw = self.get(k)?;
        let v: Vec<f32> = raw
            .split(|c: char| c == ',' || c.is_whitespace())
            .filter(|s| !s.is_empty())
            .filter_map(|s| s.parse().ok())
            .collect();
        (!v.is_empty()).then_some(v)
    }

    /// `{{key}}` substitution. Missing keys expand to the empty string rather
    /// than failing, so a widget keeps painting while its script is broken.
    pub fn expand(&self, template: &str) -> String {
        let mut out = String::with_capacity(template.len());
        let mut rest = template;
        while let Some(start) = rest.find("{{") {
            out.push_str(&rest[..start]);
            let after = &rest[start + 2..];
            match after.find("}}") {
                Some(end) => {
                    let key = after[..end].trim();
                    out.push_str(self.get(key).unwrap_or(""));
                    rest = &after[end + 2..];
                }
                None => {
                    out.push_str(&rest[start..]);
                    return out;
                }
            }
        }
        out.push_str(rest);
        out
    }
}
