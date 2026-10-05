//! Turns a [`Document`] into pixels. The editor uses [`render_canvas`] for
//! the live view; exports go through [`render`], which also crops, resizes
//! and applies the background.

use image::RgbaImage;
use tiny_skia::{FillRule, LineCap, LineJoin, Mask, Paint, PathBuilder, Pixmap, PixmapPaint, Stroke, Transform};

use crate::{
    annot::{counter_radius, curve_points, Annotation, ArrowStyle, Shape},
    doc::Document,
    effects,
    geom::{Color, Pt},
    text::{self, paint, Fonts},
};

/// Full export pipeline: canvas → crop → resize → background.
pub fn render(doc: &Document, fonts: &Fonts) -> RgbaImage {
    let canvas = doc.canvas_bounds();
    let mut pm = render_canvas(doc, fonts);
    if let Some(crop) = doc.crop {
        if let Some((x, y, w, h)) = crop.translate(-canvas.x, -canvas.y).clamp_px(pm.width(), pm.height()) {
            pm = sub_pixmap(&pm, x, y, w, h);
        }
    }
    let mut img = to_image(&pm);
    let max = crate::doc::MAX_PX;
    if let Some((w, h)) =
        doc.output_size.filter(|&(w, h)| w > 0 && h > 0 && w <= max && h <= max && (w, h) != img.dimensions())
    {
        img = image::imageops::resize(&img, w, h, image::imageops::FilterType::Lanczos3);
    }
    match &doc.background {
        Some(bg) => to_image(&bg.apply(&to_pixmap(&img))),
        None => img,
    }
}

/// Layers plus annotations over the whole canvas, uncropped.
pub fn render_canvas(doc: &Document, fonts: &Fonts) -> Pixmap {
    let canvas = doc.canvas_bounds();
    let max = crate::doc::MAX_PX as f32;
    let (w, h) = (canvas.w.clamp(1.0, max) as u32, canvas.h.clamp(1.0, max) as u32);
    let Some(mut pm) = Pixmap::new(w, h).or_else(|| Pixmap::new(1, 1)) else { unreachable!("1×1 pixmap") };
    for layer in &doc.layers {
        pm.draw_pixmap(
            (layer.at.x - canvas.x) as i32,
            (layer.at.y - canvas.y) as i32,
            to_pixmap(&layer.image).as_ref(),
            &PixmapPaint::default(),
            Transform::identity(),
            None,
        );
    }
    let origin = Pt::new(canvas.x, canvas.y);
    // Spotlight dims the screenshot only, so annotations stay vivid.
    if doc.annotations.iter().any(|a| matches!(a.shape, Shape::Spotlight { .. })) {
        spotlight(&mut pm, &doc.annotations, origin);
    }
    for a in &doc.annotations {
        draw_annotation(&mut pm, a, origin, fonts);
    }
    pm
}

/// Draws one annotation; `origin` is the canvas point at pixmap (0, 0).
pub fn draw_annotation(pm: &mut Pixmap, a: &Annotation, origin: Pt, fonts: &Fonts) {
    let t = Transform::from_translate(-origin.x, -origin.y);
    let w = a.width.max(1.0);
    let fill = paint(a.color);
    let stroke =
        |width: f32| Stroke { width, line_cap: LineCap::Round, line_join: LineJoin::Round, ..Default::default() };
    let px_region = |r: &crate::Rect| r.translate(-origin.x, -origin.y).clamp_px(pm.width(), pm.height());

    match &a.shape {
        Shape::Arrow { from, to, style, ctrl } => draw_arrow(pm, *from, *to, *style, *ctrl, w, &fill, t),
        Shape::Line { from, to } => {
            let mut pb = PathBuilder::new();
            pb.move_to(from.x, from.y);
            pb.line_to(to.x, to.y);
            if let Some(p) = pb.finish() {
                pm.stroke_path(&p, &fill, &stroke(w), t, None);
            }
        }
        Shape::Rect { rect, filled } => {
            let Some(path) = text::round_rect_path(*rect, w.min(6.0)) else { return };
            if *filled {
                pm.fill_path(&path, &fill, FillRule::Winding, t, None);
            } else {
                pm.stroke_path(&path, &fill, &stroke(w), t, None);
            }
        }
        Shape::Ellipse { rect, filled } => {
            let Some(r) = tiny_skia::Rect::from_xywh(rect.x, rect.y, rect.w.max(1.0), rect.h.max(1.0)) else { return };
            let Some(path) = PathBuilder::from_oval(r) else { return };
            if *filled {
                pm.fill_path(&path, &fill, FillRule::Winding, t, None);
            } else {
                pm.stroke_path(&path, &fill, &stroke(w), t, None);
            }
        }
        Shape::Pencil { points } => stroke_points(pm, points, &fill, &stroke(w), t),
        Shape::Highlighter { points } => {
            let mut p = paint(a.color.with_alpha(110));
            p.blend_mode = tiny_skia::BlendMode::Multiply;
            let s =
                Stroke { width: w * 4.0, line_cap: LineCap::Square, line_join: LineJoin::Round, ..Default::default() };
            stroke_points(pm, points, &p, &s, t);
        }
        Shape::Text { at, text, style, size } => {
            text::draw(pm, fonts, at.x - origin.x, at.y - origin.y, text, *size, *style, a.color)
        }
        Shape::Counter { at, n } => {
            let r = counter_radius(w);
            if let Some(c) = PathBuilder::from_circle(at.x, at.y, r) {
                pm.fill_path(&c, &fill, FillRule::Winding, t, None);
                pm.stroke_path(&c, &paint(Color::WHITE.with_alpha(220)), &stroke(r / 8.0), t, None);
            }
            let fg = if a.color.luma() > 0.6 { Color::BLACK } else { Color::WHITE };
            text::draw_centered(pm, fonts, at.x - origin.x, at.y - origin.y, &n.to_string(), r * 1.15, fg);
        }
        Shape::Blur { rect, secure } => {
            if let Some(region) = px_region(rect) {
                let radius = (w * 3.0).max(6.0) as u32;
                if *secure {
                    effects::secure_blur(pm, region, radius, seed(rect));
                } else {
                    effects::blur(pm, Some(region), radius);
                }
            }
        }
        Shape::Pixelate { rect } => {
            if let Some(region) = px_region(rect) {
                effects::pixelate(pm, region, (w * 3.0).max(8.0) as u32, seed(rect));
            }
        }
        // Spotlights are combined across the document; see `spotlight`.
        Shape::Spotlight { .. } => {}
    }
}

/// Dims everything outside the union of all spotlight shapes.
fn spotlight(pm: &mut Pixmap, all: &[Annotation], origin: Pt) {
    let Some(mut mask) = Mask::new(pm.width(), pm.height()) else { return };
    let t = Transform::from_translate(-origin.x, -origin.y);
    for a in all {
        let Shape::Spotlight { rect, ellipse } = &a.shape else { continue };
        let path = if *ellipse {
            tiny_skia::Rect::from_xywh(rect.x, rect.y, rect.w.max(1.0), rect.h.max(1.0))
                .and_then(PathBuilder::from_oval)
        } else {
            text::round_rect_path(*rect, 8.0)
        };
        if let Some(p) = path {
            mask.fill_path(&p, FillRule::Winding, true, t);
        }
    }
    mask.data_mut().iter_mut().for_each(|v| *v = 255 - *v);
    let dim = paint(Color::BLACK.with_alpha(140));
    let full = tiny_skia::Rect::from_xywh(0.0, 0.0, pm.width() as f32, pm.height() as f32).unwrap();
    pm.fill_rect(full, &dim, Transform::identity(), Some(&mask));
}

#[allow(clippy::too_many_arguments)]
fn draw_arrow(
    pm: &mut Pixmap,
    from: Pt,
    to: Pt,
    style: ArrowStyle,
    ctrl: Option<Pt>,
    w: f32,
    fill: &Paint,
    t: Transform,
) {
    let len = from.dist(to);
    if len < 1.0 {
        return;
    }
    let head_len = (w * 3.5 + 8.0).min(len * 0.6);
    let head_w = head_len * 0.65;
    let line = Stroke { width: w, line_cap: LineCap::Round, line_join: LineJoin::Round, ..Default::default() };
    // Solid triangular head at `tip`, pointing away from `back`.
    let head = |pm: &mut Pixmap, back: Pt, tip: Pt| {
        let d = back.dist(tip).max(f32::EPSILON);
        let (ux, uy) = ((tip.x - back.x) / d, (tip.y - back.y) / d);
        let base = tip.offset(-ux * head_len, -uy * head_len);
        let mut pb = PathBuilder::new();
        pb.move_to(tip.x, tip.y);
        pb.line_to(base.x - uy * head_w, base.y + ux * head_w);
        pb.line_to(base.x + uy * head_w, base.y - ux * head_w);
        pb.close();
        if let Some(p) = pb.finish() {
            pm.fill_path(&p, fill, FillRule::Winding, t, None);
            // A thin stroke rounds the head's sharp corners.
            pm.stroke_path(
                &p,
                fill,
                &Stroke { width: w * 0.4, line_join: LineJoin::Round, ..Default::default() },
                t,
                None,
            );
        }
    };
    let (ux, uy) = ((to.x - from.x) / len, (to.y - from.y) / len);
    let (nx, ny) = (-uy, ux);
    let back = |p: Pt, by: f32| p.offset(-ux * by, -uy * by);

    match style {
        ArrowStyle::Standard => {
            let base = back(to, head_len * 0.8);
            let (tw, bw) = (w * 0.3, w * 0.75);
            let mut pb = PathBuilder::new();
            pb.move_to(from.x + nx * tw, from.y + ny * tw);
            pb.line_to(base.x + nx * bw, base.y + ny * bw);
            pb.line_to(base.x - nx * bw, base.y - ny * bw);
            pb.line_to(from.x - nx * tw, from.y - ny * tw);
            pb.close();
            if let Some(p) = pb.finish() {
                pm.fill_path(&p, fill, FillRule::Winding, t, None);
            }
            head(pm, from, to);
        }
        ArrowStyle::Thin => {
            let base = back(to, head_len);
            let mut pb = PathBuilder::new();
            pb.move_to(from.x, from.y);
            pb.line_to(to.x, to.y);
            pb.move_to(base.x + nx * head_w, base.y + ny * head_w);
            pb.line_to(to.x, to.y);
            pb.line_to(base.x - nx * head_w, base.y - ny * head_w);
            if let Some(p) = pb.finish() {
                pm.stroke_path(&p, fill, &line, t, None);
            }
        }
        ArrowStyle::Double => {
            let (a, b) = (from.offset(ux * head_len * 0.8, uy * head_len * 0.8), back(to, head_len * 0.8));
            let mut pb = PathBuilder::new();
            pb.move_to(a.x, a.y);
            pb.line_to(b.x, b.y);
            if let Some(p) = pb.finish() {
                pm.stroke_path(&p, fill, &line, t, None);
            }
            head(pm, to, from);
            head(pm, from, to);
        }
        ArrowStyle::Curved => {
            let c = ctrl.unwrap_or_else(|| default_ctrl(from, to));
            let pts = curve_points(from, c, to, 32);
            // Stop the line where the head starts so the tip stays sharp.
            let cut = pts.iter().rposition(|p| p.dist(to) >= head_len * 0.8).unwrap_or(0);
            stroke_points(pm, &pts[..=cut], fill, &line, t);
            head(pm, pts[cut.saturating_sub(1)], to);
        }
    }
}

/// Bend used for a fresh curved arrow: a quarter of its length to the left.
pub fn default_ctrl(from: Pt, to: Pt) -> Pt {
    let m = from.lerp(to, 0.5);
    m.offset((from.y - to.y) * 0.25, (to.x - from.x) * 0.25)
}

fn stroke_points(pm: &mut Pixmap, points: &[Pt], paint: &Paint, stroke: &Stroke, t: Transform) {
    let Some(first) = points.first() else { return };
    let mut pb = PathBuilder::new();
    pb.move_to(first.x, first.y);
    if points.len() == 1 {
        // A click without movement still leaves a dot.
        pb.line_to(first.x + 0.01, first.y);
    }
    for p in &points[1..] {
        pb.line_to(p.x, p.y);
    }
    if let Some(path) = pb.finish() {
        pm.stroke_path(&path, paint, stroke, t, None);
    }
}

fn seed(r: &crate::Rect) -> u64 {
    (r.x.to_bits() as u64) << 32 ^ r.y.to_bits() as u64 ^ (r.w.to_bits() as u64).rotate_left(17)
}

pub fn sub_pixmap(pm: &Pixmap, x: u32, y: u32, w: u32, h: u32) -> Pixmap {
    let mut out = Pixmap::new(w, h).expect("non-zero crop");
    out.draw_pixmap(-(x as i32), -(y as i32), pm.as_ref(), &PixmapPaint::default(), Transform::identity(), None);
    out
}

/// Straight RGBA → premultiplied pixmap. An empty image becomes 1×1 transparent.
pub fn to_pixmap(img: &RgbaImage) -> Pixmap {
    if img.width() == 0 || img.height() == 0 {
        return Pixmap::new(1, 1).expect("1×1 pixmap");
    }
    let mut data = img.as_raw().clone();
    for px in data.chunks_exact_mut(4) {
        let a = px[3] as u32;
        if a < 255 {
            (0..3).for_each(|c| px[c] = ((px[c] as u32 * a + 127) / 255) as u8);
        }
    }
    Pixmap::from_vec(data, tiny_skia::IntSize::from_wh(img.width(), img.height()).expect("non-zero image"))
        .expect("buffer matches size")
}

/// Premultiplied pixmap → straight RGBA.
pub fn to_image(pm: &Pixmap) -> RgbaImage {
    let mut data = pm.data().to_vec();
    for px in data.chunks_exact_mut(4) {
        let a = px[3] as u32;
        if a > 0 && a < 255 {
            (0..3).for_each(|c| px[c] = ((px[c] as u32 * 255 + a / 2) / a).min(255) as u8);
        }
    }
    RgbaImage::from_raw(pm.width(), pm.height(), data).expect("buffer matches size")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{geom::Rect, TextStyle};

    fn white(w: u32, h: u32) -> Document {
        Document::new(RgbaImage::from_pixel(w, h, image::Rgba([255, 255, 255, 255])))
    }

    #[test]
    fn rect_annotation_and_crop() {
        let fonts = Fonts::system().expect("system fonts");
        let mut doc = white(100, 80);
        doc.annotations.push(Annotation::new(
            Shape::Rect { rect: Rect::new(10.0, 10.0, 30.0, 30.0), filled: true },
            Color::RED,
            2.0,
        ));
        doc.crop = Some(Rect::new(10.0, 10.0, 50.0, 40.0));
        let img = render(&doc, &fonts);
        assert_eq!(img.dimensions(), (50, 40));
        assert_eq!(img.get_pixel(15, 15).0, [255, 59, 48, 255]);
        assert_eq!(img.get_pixel(45, 35).0, [255, 255, 255, 255]);
    }

    #[test]
    fn every_shape_renders_without_panicking() {
        let fonts = Fonts::system().expect("system fonts");
        let mut doc = white(200, 200);
        let r = Rect::new(20.0, 20.0, 80.0, 60.0);
        let (a, b) = (Pt::new(10.0, 10.0), Pt::new(150.0, 120.0));
        let mut shapes = vec![
            Shape::Line { from: a, to: b },
            Shape::Rect { rect: r, filled: false },
            Shape::Ellipse { rect: r, filled: true },
            Shape::Pencil { points: vec![a, b, Pt::new(30.0, 190.0)] },
            Shape::Highlighter { points: vec![a, b] },
            Shape::Counter { at: b, n: 12 },
            Shape::Blur { rect: r, secure: true },
            Shape::Blur { rect: r.translate(-50.0, 150.0), secure: false },
            Shape::Pixelate { rect: r },
            Shape::Spotlight { rect: r, ellipse: true },
        ];
        for style in [ArrowStyle::Standard, ArrowStyle::Thin, ArrowStyle::Double, ArrowStyle::Curved] {
            shapes.push(Shape::Arrow { from: a, to: b, style, ctrl: None });
        }
        for style in TextStyle::ALL {
            shapes.push(Shape::Text { at: a, text: "Hello\nworld".into(), style, size: 24.0 });
        }
        doc.annotations = shapes.into_iter().map(|s| Annotation::new(s, Color::RED, 4.0)).collect();
        let img = render(&doc, &fonts);
        assert_eq!(img.dimensions(), (200, 200));
    }

    #[test]
    fn spotlight_dims_outside_only() {
        let fonts = Fonts::system().expect("system fonts");
        let mut doc = white(100, 100);
        doc.annotations.push(Annotation::new(
            Shape::Spotlight { rect: Rect::new(20.0, 20.0, 60.0, 60.0), ellipse: false },
            Color::RED,
            2.0,
        ));
        let img = render(&doc, &fonts);
        assert_eq!(img.get_pixel(50, 50).0, [255, 255, 255, 255]);
        assert!(img.get_pixel(5, 5).0[0] < 200);
    }

    #[test]
    fn premultiply_roundtrip() {
        let img = RgbaImage::from_pixel(2, 2, image::Rgba([200, 100, 50, 128]));
        let back = to_image(&to_pixmap(&img));
        for (a, b) in img.pixels().zip(back.pixels()) {
            assert!(a.0.iter().zip(b.0).all(|(x, y)| x.abs_diff(y) <= 2), "{a:?} {b:?}");
        }
    }
}
