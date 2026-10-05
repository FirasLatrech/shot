//! Text layout and rasterization for the text tool and counters.

use ab_glyph::{point, Font, FontArc, Glyph, PxScale, ScaleFont};
use tiny_skia::{FillRule, Paint, PathBuilder, Pixmap, PixmapPaint, Transform};

use crate::{
    annot::TextStyle,
    effects,
    geom::{Color, Rect},
};

#[derive(Clone)]
pub struct Fonts {
    pub sans: FontArc,
    pub mono: FontArc,
}

impl Fonts {
    pub fn new(sans: Vec<u8>, mono: Vec<u8>) -> Option<Self> {
        Some(Self { sans: FontArc::try_from_vec(sans).ok()?, mono: FontArc::try_from_vec(mono).ok()? })
    }

    /// Loads the macOS system fonts, falling back to Helvetica/Menlo.
    pub fn system() -> Option<Self> {
        let read = |paths: &[&str]| paths.iter().find_map(|p| std::fs::read(p).ok());
        Self::new(
            read(&["/System/Library/Fonts/SFNS.ttf", "/System/Library/Fonts/Helvetica.ttc"])?,
            read(&["/System/Library/Fonts/SFNSMono.ttf", "/System/Library/Fonts/Menlo.ttc"])?,
        )
    }

    fn for_style(&self, style: TextStyle) -> &FontArc {
        if style == TextStyle::Mono {
            &self.mono
        } else {
            &self.sans
        }
    }
}

struct Layout {
    glyphs: Vec<Glyph>,
    w: f32,
    h: f32,
}

fn layout(font: &FontArc, size: f32, text: &str) -> Layout {
    let scaled = font.as_scaled(PxScale::from(size));
    let line_h = scaled.height() + scaled.line_gap();
    let mut glyphs = Vec::new();
    let mut w: f32 = 0.0;
    let lines: Vec<&str> = text.split('\n').collect();
    for (i, line) in lines.iter().enumerate() {
        let mut x = 0.0;
        let mut prev = None;
        for c in line.chars() {
            let id = scaled.glyph_id(c);
            if let Some(p) = prev {
                x += scaled.kern(p, id);
            }
            glyphs.push(id.with_scale_and_position(size, point(x, scaled.ascent() + i as f32 * line_h)));
            x += scaled.h_advance(id);
            prev = Some(id);
        }
        w = w.max(x);
    }
    Layout { glyphs, w: w.max(size * 0.3), h: lines.len() as f32 * line_h }
}

/// Paints the glyph coverage of `lay` in `color` into a pixmap the size of the text.
fn rasterize(font: &FontArc, lay: &Layout, color: Color) -> Option<Pixmap> {
    let mut pm = Pixmap::new(lay.w.ceil().max(1.0) as u32 + 2, lay.h.ceil().max(1.0) as u32 + 2)?;
    let (pw, ph) = (pm.width() as i32, pm.height() as i32);
    let data = pm.data_mut();
    for g in &lay.glyphs {
        let Some(og) = font.outline_glyph(g.clone()) else { continue };
        let b = og.px_bounds();
        og.draw(|x, y, c| {
            let (px, py) = (b.min.x as i32 + x as i32 + 1, b.min.y as i32 + y as i32 + 1);
            if px < 0 || py < 0 || px >= pw || py >= ph {
                return;
            }
            let i = (py * pw + px) as usize * 4;
            let a = (c.min(1.0) * color.a as f32) as u8;
            if a > data[i + 3] {
                let pm = |v: u8| (v as u32 * a as u32 / 255) as u8;
                data[i..i + 4].copy_from_slice(&[pm(color.r), pm(color.g), pm(color.b), a]);
            }
        });
    }
    Some(pm)
}

/// Font sizes outside this range are either invisible or absurdly large.
fn clamp_size(size: f32) -> f32 {
    if size.is_finite() {
        size.clamp(1.0, 1000.0)
    } else {
        16.0
    }
}

/// Size of the box a text annotation occupies, background included.
pub fn measure(fonts: &Fonts, text: &str, size: f32, style: TextStyle) -> Rect {
    let size = clamp_size(size);
    let lay = layout(fonts.for_style(style), size, text);
    let pad = padding(style, size);
    Rect::new(-pad, -pad, lay.w + 2.0 * pad, lay.h + 2.0 * pad)
}

fn padding(style: TextStyle, size: f32) -> f32 {
    match style {
        TextStyle::Plain | TextStyle::Outline | TextStyle::Shadow => size * 0.1,
        _ => size * 0.35,
    }
}

/// Text color that stays readable on top of `bg`.
fn on(bg: Color) -> Color {
    if bg.luma() > 0.6 {
        Color::BLACK
    } else {
        Color::WHITE
    }
}

/// Draws `text` with its top-left at (`x`, `y`) in pixmap coordinates.
#[allow(clippy::too_many_arguments)]
pub fn draw(pm: &mut Pixmap, fonts: &Fonts, x: f32, y: f32, text: &str, size: f32, style: TextStyle, color: Color) {
    let size = clamp_size(size);
    let font = fonts.for_style(style);
    let lay = layout(font, size, text);
    let pad = padding(style, size);
    let bg = Rect::new(x - pad, y - pad, lay.w + 2.0 * pad, lay.h + 2.0 * pad);
    let stamp = |pm: &mut Pixmap, glyphs: &Pixmap, dx: f32, dy: f32| {
        pm.draw_pixmap(
            (x + dx - 1.0) as i32,
            (y + dy - 1.0) as i32,
            glyphs.as_ref(),
            &PixmapPaint::default(),
            Transform::identity(),
            None,
        );
    };

    let fg = match style {
        TextStyle::Plain => color,
        TextStyle::Outline => {
            let Some(ring) = rasterize(font, &lay, on(color)) else { return };
            let r = (size / 14.0).max(1.5);
            for i in 0..12 {
                let t = i as f32 * std::f32::consts::TAU / 12.0;
                stamp(pm, &ring, r * t.cos(), r * t.sin());
            }
            color
        }
        TextStyle::Shadow => {
            let Some(mut shadow) = rasterize(font, &lay, Color::BLACK.with_alpha(150)) else { return };
            let pad = (size / 4.0) as u32;
            let Some(mut padded) = Pixmap::new(shadow.width() + pad * 2, shadow.height() + pad * 2) else { return };
            padded.draw_pixmap(
                pad as i32,
                pad as i32,
                shadow.as_ref(),
                &PixmapPaint::default(),
                Transform::identity(),
                None,
            );
            effects::blur(&mut padded, None, (size / 10.0).max(1.0) as u32);
            shadow = padded;
            let off = size / 16.0;
            stamp(pm, &shadow, off - pad as f32, off * 1.5 - pad as f32);
            color
        }
        TextStyle::Pill => {
            fill_round_rect(pm, bg, bg.h / 2.0, color);
            on(color)
        }
        TextStyle::Box => {
            fill_round_rect(pm, bg, size * 0.25, Color::WHITE);
            stroke_round_rect(pm, bg, size * 0.25, color, (size / 12.0).max(1.5));
            color
        }
        TextStyle::Highlight => {
            fill_round_rect(pm, bg, size * 0.15, color.with_alpha(90));
            on(Color::WHITE)
        }
        TextStyle::Mono => {
            fill_round_rect(pm, bg, size * 0.25, Color::rgb(30, 30, 34));
            if color.luma() < 0.3 {
                Color::WHITE
            } else {
                color
            }
        }
    };
    if let Some(glyphs) = rasterize(font, &lay, fg) {
        stamp(pm, &glyphs, 0.0, 0.0);
    }
}

/// Draws `text` centered on (`cx`, `cy`); used for counter numbers.
pub fn draw_centered(pm: &mut Pixmap, fonts: &Fonts, cx: f32, cy: f32, text: &str, size: f32, color: Color) {
    let lay = layout(&fonts.sans, clamp_size(size), text);
    let Some(glyphs) = rasterize(&fonts.sans, &lay, color) else { return };
    pm.draw_pixmap(
        (cx - lay.w / 2.0 - 1.0).round() as i32,
        (cy - lay.h / 2.0 - 1.0).round() as i32,
        glyphs.as_ref(),
        &PixmapPaint::default(),
        Transform::identity(),
        None,
    );
}

pub fn round_rect_path(r: Rect, radius: f32) -> Option<tiny_skia::Path> {
    let rad = radius.min(r.w / 2.0).min(r.h / 2.0).max(0.0);
    let k = 0.552_284_8 * rad;
    let (l, t, rt, b) = (r.x, r.y, r.right(), r.bottom());
    let mut pb = PathBuilder::new();
    pb.move_to(l + rad, t);
    pb.line_to(rt - rad, t);
    pb.cubic_to(rt - rad + k, t, rt, t + rad - k, rt, t + rad);
    pb.line_to(rt, b - rad);
    pb.cubic_to(rt, b - rad + k, rt - rad + k, b, rt - rad, b);
    pb.line_to(l + rad, b);
    pb.cubic_to(l + rad - k, b, l, b - rad + k, l, b - rad);
    pb.line_to(l, t + rad);
    pb.cubic_to(l, t + rad - k, l + rad - k, t, l + rad, t);
    pb.close();
    pb.finish()
}

pub fn paint(color: Color) -> Paint<'static> {
    let mut p = Paint::default();
    p.set_color(color.to_skia());
    p.anti_alias = true;
    p
}

pub fn fill_round_rect(pm: &mut Pixmap, r: Rect, radius: f32, color: Color) {
    if let Some(path) = round_rect_path(r, radius) {
        pm.fill_path(&path, &paint(color), FillRule::Winding, Transform::identity(), None);
    }
}

fn stroke_round_rect(pm: &mut Pixmap, r: Rect, radius: f32, color: Color, width: f32) {
    if let Some(path) = round_rect_path(r, radius) {
        let stroke = tiny_skia::Stroke { width, ..Default::default() };
        pm.stroke_path(&path, &paint(color), &stroke, Transform::identity(), None);
    }
}
