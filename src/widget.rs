//! Widget composition: measure, lay out, paint.
//!
//! This is `bar_item_draw()` (src/bar_item.c:717) generalised. The original
//! hard-codes the order background -> icon -> label -> one widget. Here a
//! widget is an ordered list of elements and three layout modes:
//!
//!   row     icon | text | graph, laid out left to right
//!   column  stacked top to bottom
//!   stack   all elements occupy the same box (icon behind text, etc.)
//!
//! Measurement happens before painting so an auto-sized widget (frame.w/h == 0)
//! can ask for its intrinsic size, exactly like `bar_item_get_height()`
//! (src/bar_item.c:522) feeding `bar_calculate_bounds()`.

use crate::canvas::Canvas;
use crate::graph;
use crate::icon::IconCache;
use crate::model::{
    Align, Background, Element, Frame, GraphSpec, IconSpec, Layout, Rgba, TextKind, TextSpec,
    Values, WidgetSpec,
};
use crate::text::TextEngine;

pub struct Ctx<'a> {
    pub text: &'a TextEngine,
    pub icons: &'a mut IconCache,
    pub scale: f32,
}

/// Intrinsic size of an element in points, with the values already resolved.
struct Measured {
    w: f32,
    h: f32,
}

pub fn measure(widget: &WidgetSpec, ctx: &Ctx, values: &Values, avail: (f32, f32)) -> (f32, f32) {
    let pad = widget.padding_px();
    let mut sizes = Vec::with_capacity(widget.elements.len());
    for el in &widget.elements {
        sizes.push(measure_element(el, ctx, values, avail));
    }

    let (mut content_w, mut content_h) = (0.0f32, 0.0f32);
    match widget.layout {
        Layout::Row => {
            for m in &sizes {
                content_w += m.w;
                content_h = content_h.max(m.h);
            }
            if sizes.len() > 1 {
                content_w += widget.gap * (sizes.len() - 1) as f32;
            }
        }
        Layout::Column => {
            for m in &sizes {
                content_h += m.h;
                content_w = content_w.max(m.w);
            }
            if sizes.len() > 1 {
                content_h += widget.gap * (sizes.len() - 1) as f32;
            }
        }
        Layout::Stack => {
            for m in &sizes {
                content_w = content_w.max(m.w);
                content_h = content_h.max(m.h);
            }
        }
    }
    (content_w + pad.horizontal(), content_h + pad.vertical())
}

fn measure_element(el: &Element, ctx: &Ctx, values: &Values, avail: (f32, f32)) -> Measured {
    match el {
        Element::Text(t) => {
            let text = resolve_text(t, values);
            let pad = t.padding.0;
            let w = ctx.text.measure(&t.font, t.size, &text, ctx.scale) + pad.horizontal();
            let h = ctx.text.line_height(&t.font, t.size, ctx.scale) + pad.vertical();
            Measured { w, h }
        }
        Element::Icon(i) => {
            let pad = i.padding.0;
            let (w, h) = icon_box(i, avail);
            Measured {
                w: w + pad.horizontal(),
                h: h + pad.vertical(),
            }
        }
        Element::Graph(g) => {
            let w = if g.width > 0.0 {
                g.width
            } else {
                avail.0.max(40.0)
            };
            let h = if g.height > 0.0 { g.height } else { 20.0 };
            Measured { w, h }
        }
    }
}

fn icon_box(spec: &IconSpec, avail: (f32, f32)) -> (f32, f32) {
    let s = if spec.size > 0.0 {
        spec.size
    } else {
        avail.1.clamp(12.0, 24.0)
    };
    (s, s)
}

/// Resolve the effective string for a text element (`{{key}}` expansion, and
/// `value_key` override for dynamic text).
pub fn resolve_text(spec: &TextSpec, values: &Values) -> String {
    let raw = match (&spec.value_key, spec.kind) {
        (Some(k), _) if !k.is_empty() => values.get(k).unwrap_or(&spec.value).to_string(),
        (_, TextKind::Dynamic) => values.expand(&spec.value),
        (_, TextKind::Static) => spec.value.clone(),
    };
    TextEngine::truncate(&raw, spec.max_chars)
}

/// Paint a widget into `canvas` whose origin is the widget's own top-left.
pub fn draw(widget: &WidgetSpec, canvas: &mut Canvas, ctx: &mut Ctx, values: &Values) {
    draw_with_series(
        widget,
        canvas,
        ctx,
        values,
        &std::collections::HashMap::new(),
    );
}

/// Same as `draw`, but graph elements listed in `series` use the given values
/// instead of resolving them from `values`. This is the seam animation uses:
/// the runtime passes interpolated series, the drawing code stays unchanged.
pub fn draw_with_series(
    widget: &WidgetSpec,
    canvas: &mut Canvas,
    ctx: &mut Ctx,
    values: &Values,
    series: &std::collections::HashMap<usize, Vec<f32>>,
) {
    let size = (canvas.width_pt(), canvas.height_pt());

    // 1. plate (background_draw, src/background.c:196)
    draw_background(
        &widget.background,
        canvas,
        Frame::new(0.0, 0.0, size.0, size.1),
    );

    // 2. elements, in declaration order — that order IS the paint order, which
    //    is how a widget composes any mix of text/icon/graph.
    let pad = widget.padding_px();
    let avail = (size.0 - pad.horizontal(), size.1 - pad.vertical());

    let measured: Vec<Measured> = widget
        .elements
        .iter()
        .map(|e| measure_element(e, ctx, values, avail))
        .collect();

    let mut cursor_main = 0.0f32;
    for (index, (el, m)) in widget.elements.iter().zip(measured.iter()).enumerate() {
        let slot = match widget.layout {
            Layout::Row => {
                let h = avail.1;
                let r = Frame::new(pad.left + cursor_main, pad.top, m.w, h);
                cursor_main += m.w + widget.gap;
                r
            }
            Layout::Column => {
                let w = avail.0;
                let r = Frame::new(pad.left, pad.top + cursor_main, w, m.h);
                cursor_main += m.h + widget.gap;
                r
            }
            Layout::Stack => {
                // Centre each element in the content box unless it is a graph,
                // which fills it.
                match el {
                    Element::Graph(_) => Frame::new(pad.left, pad.top, avail.0, avail.1),
                    _ => Frame::new(
                        pad.left + (avail.0 - m.w) / 2.0,
                        pad.top + (avail.1 - m.h) / 2.0,
                        m.w,
                        m.h,
                    ),
                }
            }
        };
        draw_element(el, canvas, ctx, values, slot, series.get(&index));
    }
}

fn draw_element(
    el: &Element,
    canvas: &mut Canvas,
    ctx: &mut Ctx,
    values: &Values,
    slot: Frame,
    series_override: Option<&Vec<f32>>,
) {
    match el {
        Element::Text(spec) => {
            let pad = spec.padding.0;
            draw_background(&spec.background, canvas, slot);

            let text = resolve_text(spec, values);
            if text.is_empty() {
                return;
            }
            let inner_w = slot.w - pad.horizontal();
            ctx.text.draw(
                canvas,
                &spec.font,
                spec.size,
                &text,
                (slot.x + pad.left, slot.y + pad.top),
                spec.color,
                spec.align,
                inner_w,
            );
        }
        Element::Icon(spec) => {
            let pad = spec.padding.0;
            let path = match (&spec.svg, &spec.bitmap) {
                (Some(p), _) => p.clone(),
                (_, Some(p)) => p.clone(),
                _ => return,
            };
            let target_h = (slot.h - pad.vertical()).max(1.0);
            let height_px = (target_h * ctx.scale).round().max(1.0) as u32;
            match ctx.icons.get(&path, height_px, spec.tint, ctx.scale) {
                Ok(pm) => {
                    let aspect = pm.width() as f32 / pm.height().max(1) as f32;
                    let w = target_h * aspect;
                    let x = slot.x + pad.left + (slot.w - pad.horizontal() - w) / 2.0;
                    canvas.draw_rgba(pm, (x, slot.y + pad.top), (w, target_h), 1.0);
                }
                Err(e) => {
                    eprintln!("[icon] {e}");
                }
            }
        }
        Element::Graph(spec) => {
            let data = match series_override {
                Some(v) => v.clone(),
                None => resolve_series(spec, values),
            };
            let rect = Frame::new(
                slot.x,
                slot.y,
                if spec.width > 0.0 { spec.width } else { slot.w },
                if spec.height > 0.0 {
                    spec.height
                } else {
                    slot.h
                },
            );
            graph::draw(canvas, rect, spec, &data);
        }
    }
}

/// The series a graph element should draw, before any animation.
pub fn resolve_series(spec: &GraphSpec, values: &Values) -> Vec<f32> {
    if let Some(key) = &spec.data_key
        && let Some(v) = values.series(key)
    {
        return v;
    }
    spec.data.clone()
}

/// Shared plate renderer, used by widgets and by text backgrounds.
pub fn draw_background(bg: &Background, canvas: &mut Canvas, rect: Frame) {
    if !bg.enabled {
        return;
    }
    if bg.color.a == 0 && (bg.border_color.a == 0 || bg.border_width <= 0.0) && !bg.shadow.enabled {
        return;
    }

    if bg.shadow.enabled {
        // No blur pass (see HANDOFF.md): a soft shadow is the one thing this
        // rasteriser deliberately leaves to the compositor — CALayer has
        // shadowRadius/shadowOpacity, so the presenter sets it there instead of
        // burning a gaussian into the bitmap every frame.
        let off = bg.shadow.y_offset;
        let r = Frame::new(rect.x, rect.y + off, rect.w, rect.h);
        let c: Rgba = bg.shadow.color;
        canvas.fill_round_rect(r, bg.corner_radius, c);
    }

    if bg.color.a > 0 {
        canvas.fill_round_rect(rect, bg.corner_radius, bg.color);
    }
    if bg.border_width > 0.0 && bg.border_color.a > 0 {
        canvas.stroke_round_rect(rect, bg.corner_radius, bg.border_width, bg.border_color);
    }
}

/// Align helper kept public for tests.
pub fn align_offset(align: Align, box_w: f32, content_w: f32) -> f32 {
    match align {
        Align::Left => 0.0,
        Align::Center => (box_w - content_w) / 2.0,
        Align::Right => box_w - content_w,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{
        Background, Padding, PaddingShorthand, TextKind, TextSpec, Values, WidgetSpec,
    };

    fn text_widget(value: &str, size: f32) -> WidgetSpec {
        WidgetSpec {
            id: "t".into(),
            display: 0,
            anchor: crate::model::Anchor::TopLeft,
            frame: Frame::default(),
            layout: Layout::Row,
            padding: PaddingShorthand(Padding::all(10.0)),
            gap: 0.0,
            background: Background::default(),
            elements: vec![Element::Text(TextSpec {
                value: value.into(),
                kind: TextKind::Static,
                font: "mono".into(),
                size,
                ..Default::default()
            })],
            script: None,
            layer: 0,
            click_through: true,
            always_on_top: false,
        }
    }

    /// The bug this guards: text measured in points but rasterised/drawn at
    /// device pixels, so it renders wider than the box that was sized for it and
    /// gets clipped against the right edge.
    #[test]
    fn text_fits_its_measured_box() {
        let mut te = TextEngine::new();
        te.load_system_defaults().expect("fonts");
        let mut icons = IconCache::new();

        for (value, size) in [
            ("click me: off", 11.0),
            ("mac-studio", 12.0),
            ("18 MB/s", 11.0),
        ] {
            let spec = text_widget(value, size);
            let v = Values::default();
            let (w, h) = {
                let ctx = Ctx {
                    text: &te,
                    icons: &mut icons,
                    scale: 2.0,
                };
                measure(&spec, &ctx, &v, (2000.0, 2000.0))
            };
            let mut canvas = Canvas::new(w, h, 2.0).expect("canvas");
            canvas.clear();
            {
                let mut c = Ctx {
                    text: &te,
                    icons: &mut icons,
                    scale: 2.0,
                };
                draw(&spec, &mut canvas, &mut c, &v);
            }

            // Rightmost painted pixel.
            let pm = canvas.pixmap();
            let stride = pm.width() as usize;
            let mut right = 0usize;
            for (i, px) in pm.data().chunks_exact(4).enumerate() {
                if px[3] > 8 {
                    right = right.max(i % stride);
                }
            }
            assert!(
                right < pm.width() as usize,
                "{value:?} painted to x={right} in a {}-px canvas ({w}pt)",
                pm.width()
            );
            // And it should actually fill most of the box, otherwise the
            // measurement is over-estimating instead.
            let pad_px = 20.0;
            assert!(
                pm.width() as f32 <= (right as f32) + 1.0 + pad_px + 6.0,
                "{value:?} only reached x={right} of {} px — over-measured",
                pm.width()
            );
        }
    }
}
