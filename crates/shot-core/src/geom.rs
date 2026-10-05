use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
pub struct Pt {
    pub x: f32,
    pub y: f32,
}

impl Pt {
    pub const fn new(x: f32, y: f32) -> Self {
        Self { x, y }
    }
    pub fn dist(self, o: Pt) -> f32 {
        (self.x - o.x).hypot(self.y - o.y)
    }
    pub fn lerp(self, o: Pt, t: f32) -> Pt {
        Pt::new(self.x + (o.x - self.x) * t, self.y + (o.y - self.y) * t)
    }
    pub fn offset(self, dx: f32, dy: f32) -> Pt {
        Pt::new(self.x + dx, self.y + dy)
    }
}

/// Axis-aligned rectangle; `w`/`h` are always non-negative after [`Rect::from_pts`].
#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl Rect {
    pub const fn new(x: f32, y: f32, w: f32, h: f32) -> Self {
        Self { x, y, w, h }
    }
    pub fn from_pts(a: Pt, b: Pt) -> Self {
        Self::new(a.x.min(b.x), a.y.min(b.y), (a.x - b.x).abs(), (a.y - b.y).abs())
    }
    pub fn right(&self) -> f32 {
        self.x + self.w
    }
    pub fn bottom(&self) -> f32 {
        self.y + self.h
    }
    pub fn center(&self) -> Pt {
        Pt::new(self.x + self.w / 2.0, self.y + self.h / 2.0)
    }
    pub fn contains(&self, p: Pt) -> bool {
        p.x >= self.x && p.x <= self.right() && p.y >= self.y && p.y <= self.bottom()
    }
    pub fn inflate(&self, d: f32) -> Rect {
        Rect::new(self.x - d, self.y - d, self.w + 2.0 * d, self.h + 2.0 * d)
    }
    pub fn union(&self, o: &Rect) -> Rect {
        let x = self.x.min(o.x);
        let y = self.y.min(o.y);
        Rect::new(x, y, self.right().max(o.right()) - x, self.bottom().max(o.bottom()) - y)
    }
    pub fn intersect(&self, o: &Rect) -> Option<Rect> {
        let x = self.x.max(o.x);
        let y = self.y.max(o.y);
        let r = self.right().min(o.right());
        let b = self.bottom().min(o.bottom());
        (r > x && b > y).then(|| Rect::new(x, y, r - x, b - y))
    }
    pub fn translate(&self, dx: f32, dy: f32) -> Rect {
        Rect::new(self.x + dx, self.y + dy, self.w, self.h)
    }
    /// Integer pixel bounds clamped to a `width`×`height` image.
    pub fn clamp_px(&self, width: u32, height: u32) -> Option<(u32, u32, u32, u32)> {
        let r = self.intersect(&Rect::new(0.0, 0.0, width as f32, height as f32))?;
        let (x0, y0) = (r.x.floor() as u32, r.y.floor() as u32);
        let (x1, y1) = (r.right().ceil() as u32, r.bottom().ceil() as u32);
        (x1 > x0 && y1 > y0).then_some((x0, y0, x1 - x0, y1 - y0))
    }
    /// The 8 resize handles (squares of `size`), clockwise from top-left.
    pub fn handles(&self, size: f32) -> [Rect; 8] {
        let (x0, xm, x1) = (self.x, self.center().x, self.right());
        let (y0, ym, y1) = (self.y, self.center().y, self.bottom());
        let h = |x: f32, y: f32| Rect::new(x - size / 2.0, y - size / 2.0, size, size);
        [h(x0, y0), h(xm, y0), h(x1, y0), h(x1, ym), h(x1, y1), h(xm, y1), h(x0, y1), h(x0, ym)]
    }

    /// Drags handle `h` (see [`Rect::handles`]) to `p`; the opposite side
    /// stays put. With `aspect` (w/h), the other dimension follows.
    pub fn resize_handle(&self, h: usize, p: Pt, aspect: Option<f32>) -> Rect {
        let (mut x0, mut y0, mut x1, mut y1) = (self.x, self.y, self.right(), self.bottom());
        match h {
            0 => (x0, y0) = (p.x, p.y),
            1 => y0 = p.y,
            2 => (x1, y0) = (p.x, p.y),
            3 => x1 = p.x,
            4 => (x1, y1) = (p.x, p.y),
            5 => y1 = p.y,
            6 => (x0, y1) = (p.x, p.y),
            _ => x0 = p.x,
        }
        let out = Rect::from_pts(Pt::new(x0, y0), Pt::new(x1, y1));
        let Some(a) = aspect.filter(|a| a.is_finite() && *a > 0.0) else { return out };
        // Top/bottom edges drive the height; everything else drives the width.
        let (w, ht) = if h == 1 || h == 5 { (out.h * a, out.h) } else { (out.w, out.w / a) };
        // Keep the side (or center line) opposite the dragged handle fixed.
        let x = match h {
            0 | 6 | 7 => self.right() - w,
            1 | 5 => self.center().x - w / 2.0,
            _ => self.x,
        };
        let y = match h {
            0..=2 => self.bottom() - ht,
            3 | 7 => self.center().y - ht / 2.0,
            _ => self.y,
        };
        Rect::new(x, y, w, ht)
    }

    /// Grow the rect from anchor `a` to `b`, constrained to `ratio` (w/h) when given.
    pub fn with_aspect(a: Pt, b: Pt, ratio: Option<f32>) -> Rect {
        let Some(ratio) = ratio.filter(|r| *r > 0.0) else {
            return Rect::from_pts(a, b);
        };
        let (dx, dy) = (b.x - a.x, b.y - a.y);
        let w = dx.abs().max(dy.abs() * ratio);
        let h = w / ratio;
        Rect::from_pts(a, Pt::new(a.x + w.copysign(dx), a.y + h.copysign(dy)))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Color {
    pub r: u8,
    pub g: u8,
    pub b: u8,
    pub a: u8,
}

impl Color {
    pub const fn rgb(r: u8, g: u8, b: u8) -> Self {
        Self { r, g, b, a: 255 }
    }
    pub const fn rgba(r: u8, g: u8, b: u8, a: u8) -> Self {
        Self { r, g, b, a }
    }
    pub const WHITE: Color = Color::rgb(255, 255, 255);
    pub const BLACK: Color = Color::rgb(0, 0, 0);
    pub const RED: Color = Color::rgb(255, 59, 48);

    pub fn with_alpha(self, a: u8) -> Self {
        Self { a, ..self }
    }
    pub fn to_skia(self) -> tiny_skia::Color {
        tiny_skia::Color::from_rgba8(self.r, self.g, self.b, self.a)
    }
    /// Perceived luminance in 0..=1, used to pick readable text on top of a color.
    pub fn luma(self) -> f32 {
        (0.299 * self.r as f32 + 0.587 * self.g as f32 + 0.114 * self.b as f32) / 255.0
    }
    pub fn hex(self) -> String {
        format!("#{:02X}{:02X}{:02X}", self.r, self.g, self.b)
    }
    pub fn parse_hex(s: &str) -> Option<Color> {
        let s = s.trim_start_matches('#');
        let v = u32::from_str_radix(s, 16).ok()?;
        match s.len() {
            6 => Some(Color::rgb((v >> 16) as u8, (v >> 8) as u8, v as u8)),
            8 => Some(Color::rgba((v >> 24) as u8, (v >> 16) as u8, (v >> 8) as u8, v as u8)),
            _ => None,
        }
    }
}

/// The default palette shown in the editor; users add their own on top.
pub const PALETTE: [Color; 8] = [
    Color::RED,
    Color::rgb(255, 149, 0),
    Color::rgb(255, 204, 0),
    Color::rgb(52, 199, 89),
    Color::rgb(0, 122, 255),
    Color::rgb(175, 82, 222),
    Color::BLACK,
    Color::WHITE,
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn aspect_rect_keeps_ratio_in_all_quadrants() {
        let a = Pt::new(100.0, 100.0);
        for b in [Pt::new(160.0, 110.0), Pt::new(40.0, 30.0), Pt::new(90.0, 300.0)] {
            let r = Rect::with_aspect(a, b, Some(16.0 / 9.0));
            assert!((r.w / r.h - 16.0 / 9.0).abs() < 1e-3, "{r:?}");
        }
    }

    #[test]
    fn resize_handles() {
        let r = Rect::new(10.0, 10.0, 100.0, 50.0);
        assert_eq!(r.resize_handle(4, Pt::new(210.0, 110.0), None), Rect::new(10.0, 10.0, 200.0, 100.0));
        assert_eq!(r.resize_handle(0, Pt::new(0.0, 0.0), None), Rect::new(0.0, 0.0, 110.0, 60.0));
        let sq = r.resize_handle(3, Pt::new(60.0, 0.0), Some(1.0));
        assert_eq!(sq, Rect::new(10.0, 10.0, 50.0, 50.0));
        // Dragging the top-left corner keeps the bottom-right corner fixed.
        let tl = r.resize_handle(0, Pt::new(0.0, 0.0), Some(2.0));
        assert_eq!((tl.right(), tl.bottom()), (110.0, 60.0));
        assert_eq!(r.resize_handle(4, Pt::new(0.0, 0.0), Some(0.0)), r.resize_handle(4, Pt::new(0.0, 0.0), None));
        assert!(r.handles(8.0)[4].contains(Pt::new(110.0, 60.0)));
    }

    #[test]
    fn hex_roundtrip() {
        let c = Color::rgb(18, 52, 86);
        assert_eq!(Color::parse_hex(&c.hex()), Some(c));
    }

    #[test]
    fn clamp_px_clips_to_image() {
        assert_eq!(Rect::new(-5.0, -5.0, 20.0, 20.0).clamp_px(10, 10), Some((0, 0, 10, 10)));
        assert_eq!(Rect::new(20.0, 20.0, 5.0, 5.0).clamp_px(10, 10), None);
    }
}
