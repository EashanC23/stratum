//! stratum — compositor-backed desktop widgets.
//!
//! Layering (bottom to top), which is the whole architecture:
//!
//!   model    (src/model.rs)   widget specs, colors, anchors — pure data
//!   layout   (src/widget.rs)  measure + place elements, no pixels
//!   raster   (src/canvas.rs)  one offscreen RGBA pixmap per widget
//!   atoms    (src/text.rs, src/icon.rs, src/graph.rs) — the only three things
//!            a widget can draw
//!   present  (src/present.rs) where the finished pixmap goes
//!
//! A widget never talks to a window and a window never talks to a font. Keep
//! that boundary and the presenter becomes swappable (PNG now, CALayer/wgpu
//! later) without touching drawing code.

pub mod canvas;
pub mod config;
pub mod golden;
pub mod graph;
pub mod icon;
#[cfg(target_os = "macos")]
pub mod live;
pub mod model;
pub mod present;
pub mod runtime;
pub mod script;
pub mod text;
pub mod widget;

pub use config::Config;
pub use model::*;
pub use present::Presenter;
pub use runtime::Runtime;
