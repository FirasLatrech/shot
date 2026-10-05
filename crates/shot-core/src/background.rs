//! The Background tool: puts a screenshot on a backdrop with padding,
//! alignment, aspect ratio, rounded corners, a shadow, and Auto Balance.

use serde::{Deserialize, Serialize};
use tiny_skia::{FillRule, GradientStop, LinearGradient, Paint, Pattern, Pixmap, PixmapPaint, SpreadMode, Transform};

use crate::{
    effects,
    geom::{Color, Rect},
    render::{sub_pixmap, to_pixmap},
    text::round_rect_path,
};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Fill {
    Solid(Color),
    /// Linear gradient at `angle` degrees (0 = left→right).
    Gradient {
        from: Color,
        to: Color,
        angle: f32,
    },
    /// Path to a user-supplied image, scaled to cover.
    Image(String),
    /// No backdrop: transparent padding, keeps only the shadow.
    Transparent,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum Align {
    TopLeft,
    Top,
    TopRight,
    Left,
    #[default]
    Center,
    Right,
    BottomLeft,
    Bottom,
    BottomRight,
}

impl Align {
    /// Fractions (0, 0.5, 1) along each axis.
    fn factors(self) -> (f32, f32) {
        let i = self as usize;
        ((i % 3) as f32 / 2.0, (i / 3) as f32 / 2.0)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Background {
    pub fill: Fill,
    /// Space around the screenshot, in pixels.
    pub padding: f32,
    pub radius: f32,
    /// Shadow strength in 0..=1.
    pub shadow: f32,
    pub align: Align,
    /// Output aspect ratio as (w, h); `None` follows the screenshot.
    pub aspect: Option<(u32, u32)>,
    /// Trims uneven solid margins inside the screenshot so content sits centered.
    pub auto_balance: bool,
}

impl Default for Background {
    fn default() -> Self {
        Self {
            fill: PRESETS[0].clone(),
            padding: 64.0,
            radius: 12.0,
            shadow: 0.5,
            align: Align::Center,
            aspect: None,
            auto_balance: false,
        }
    }
}

const fn grad(from: Color, to: Color, angle: f32) -> Fill {
    Fill::Gradient { from, to, angle }
}

/// The 20 built-in backdrops.
pub const PRESETS: [Fill; 20] = [
    grad(Color::rgb(255, 94, 98), Color::rgb(255, 153, 102), 135.0),
    grad(Color::rgb(102, 126, 234), Color::rgb(118, 75, 162), 135.0),
    grad(Color::rgb(67, 233, 123), Color::rgb(56, 249, 215), 135.0),
    grad(Color::rgb(250, 112, 154), Color::rgb(254, 225, 64), 135.0),
    grad(Color::rgb(48, 207, 208), Color::rgb(51, 8, 103), 135.0),
    grad(Color::rgb(168, 237, 234), Color::rgb(254, 214, 227), 135.0),
    grad(Color::rgb(255, 154, 158), Color::rgb(254, 207, 239), 90.0),
    grad(Color::rgb(161, 140, 209), Color::rgb(251, 194, 235), 90.0),
    grad(Color::rgb(132, 250, 176), Color::rgb(143, 211, 244), 90.0),
    grad(Color::rgb(252, 203, 144), Color::rgb(213, 126, 235), 135.0),
    grad(Color::rgb(224, 195, 252), Color::rgb(142, 197, 252), 135.0),
    grad(Color::rgb(240, 147, 251), Color::rgb(245, 87, 108), 135.0),
    grad(Color::rgb(79, 172, 254), Color::rgb(0, 242, 254), 90.0),
    grad(Color::rgb(15, 32, 39), Color::rgb(44, 83, 100), 135.0),
    grad(Color::rgb(35, 37, 38), Color::rgb(65, 67, 69), 90.0),
    grad(Color::rgb(255, 216, 155), Color::rgb(25, 84, 123), 135.0),
    grad(Color::rgb(238, 156, 167), Color::rgb(255, 221, 225), 135.0),
    Fill::Solid(Color::rgb(242, 242, 247)),
    Fill::Solid(Color::rgb(28, 28, 30)),
    Fill::Transparent,
];

impl Background {
    /// Clamps values to what the UI offers, for backgrounds read from files.
    pub fn sanitize(&mut self) {
        let fin = |v: f32, lo: f32, hi: f32| if v.is_finite() { v.clamp(lo, hi) } else { lo };
        self.padding = fin(self.padding, 0.0, 2000.0);
        self.radius = fin(self.radius, 0.0, 1000.0);
        self.shadow = fin(self.shadow, 0.0, 1.0);
        self.aspect =
            self.aspect.filter(|&(w, h)| w > 0 && h > 0 && (1.0 / 20.0..=20.0).contains(&(w as f32 / h as f32)));
    }

    pub fn apply(&self, shot: &Pixmap) -> Pixmap {
        let mut bg = self.clone();
        bg.sanitize();
        bg.apply_sane(shot)
    }

    fn apply_sane(&self, shot: &Pixmap) -> Pixmap {
        let trimmed;
        let shot = if self.auto_balance {
            trimmed = auto_balance(shot);
            &trimmed
        } else {
            shot
        };
        let (sw, sh) = (shot.width() as f32, shot.height() as f32);
        let mut w = sw + 2.0 * self.padding;
        let mut h = sh + 2.0 * self.padding;
        if let Some((aw, ah)) = self.aspect.filter(|(a, b)| *a > 0 && *b > 0) {
            let ratio = aw as f32 / ah as f32;
            if w / h < ratio {
                w = h * ratio;
            } else {
                h = w / ratio;
            }
        }
        let max = crate::doc::MAX_PX as f32;
        let Some(mut out) = Pixmap::new(w.round().min(max) as u32, h.round().min(max) as u32) else {
            return shot.clone();
        };
        self.paint_fill(&mut out);

        let (fx, fy) = self.align.factors();
        let x = (self.padding + (w - sw - 2.0 * self.padding) * fx).round();
        let y = (self.padding + (h - sh - 2.0 * self.padding) * fy).round();
        let frame = Rect::new(x, y, sw, sh);

        if self.shadow > 0.0 {
            draw_shadow(&mut out, frame, self.radius, self.shadow);
        }
        if let Some(path) = round_rect_path(frame, self.radius) {
            let pattern = Pattern::new(
                shot.as_ref(),
                SpreadMode::Pad,
                tiny_skia::FilterQuality::Nearest,
                1.0,
                Transform::from_translate(x, y),
            );
            let paint = Paint { shader: pattern, anti_alias: true, ..Default::default() };
            out.fill_path(&path, &paint, FillRule::Winding, Transform::identity(), None);
        }
        out
    }

    fn paint_fill(&self, out: &mut Pixmap) {
        let (w, h) = (out.width() as f32, out.height() as f32);
        match &self.fill {
            Fill::Transparent => {}
            Fill::Solid(c) => out.fill(c.to_skia()),
            Fill::Gradient { from, to, angle } => {
                let (s, c) = angle.to_radians().sin_cos();
                // Project the rect's half-diagonal onto the direction so the
                // gradient spans corner to corner at any angle.
                let half = (w * c.abs() + h * s.abs()) / 2.0;
                let (cx, cy) = (w / 2.0, h / 2.0);
                let shader = LinearGradient::new(
                    tiny_skia::Point::from_xy(cx - c * half, cy - s * half),
                    tiny_skia::Point::from_xy(cx + c * half, cy + s * half),
                    vec![GradientStop::new(0.0, from.to_skia()), GradientStop::new(1.0, to.to_skia())],
                    SpreadMode::Pad,
                    Transform::identity(),
                );
                if let Some(shader) = shader {
                    fill_all(out, Paint { shader, ..Default::default() });
                }
            }
            Fill::Image(path) => {
                let Ok(img) = image::open(path) else { return out.fill(tiny_skia::Color::WHITE) };
                let img = img.resize_to_fill(w as u32, h as u32, image::imageops::FilterType::Triangle).into_rgba8();
                out.draw_pixmap(0, 0, to_pixmap(&img).as_ref(), &PixmapPaint::default(), Transform::identity(), None);
            }
        }
    }
}

fn fill_all(out: &mut Pixmap, paint: Paint) {
    let r = tiny_skia::Rect::from_xywh(0.0, 0.0, out.width() as f32, out.height() as f32).unwrap();
    out.fill_rect(r, &paint, Transform::identity(), None);
}

fn draw_shadow(out: &mut Pixmap, frame: Rect, radius: f32, strength: f32) {
    // The shadow is soft, so render and blur it at quarter resolution and
    // scale it up: same look, ~16× less blurring work on big screenshots.
    const K: f32 = 4.0;
    let blur = (frame.w.min(frame.h) * 0.04).clamp(8.0, 40.0);
    let spread = blur * 2.5;
    let (w, h) = (((frame.w + spread * 2.0) / K).ceil() as u32, ((frame.h + spread * 2.0) / K).ceil() as u32);
    let Some(mut sh) = Pixmap::new(w.max(1), h.max(1)) else { return };
    let shape = Rect::new(spread / K, spread / K, frame.w / K, frame.h / K);
    if let Some(path) = round_rect_path(shape, radius / K) {
        let alpha = (strength.clamp(0.0, 1.0) * 160.0) as u8;
        sh.fill_path(
            &path,
            &crate::text::paint(Color::BLACK.with_alpha(alpha)),
            FillRule::Winding,
            Transform::identity(),
            None,
        );
    }
    effects::blur(&mut sh, None, ((blur / 2.0 / K) as u32).max(1));
    let paint = PixmapPaint { quality: tiny_skia::FilterQuality::Bilinear, ..Default::default() };
    let t = Transform::from_scale(K, K).post_translate(frame.x - spread, frame.y - spread + blur * 0.5);
    out.draw_pixmap(0, 0, sh.as_ref(), &paint, t, None);
}

/// Equalizes solid-color margins: finds the content box (pixels that differ
/// from the corner color) and re-pads it by the smallest margin on every side.
pub fn auto_balance(pm: &Pixmap) -> Pixmap {
    let (w, h) = (pm.width(), pm.height());
    let bg = pm.pixel(0, 0).expect("non-empty");
    let differs = |x: u32, y: u32| {
        let p = pm.pixel(x, y).unwrap();
        [
            p.red().abs_diff(bg.red()),
            p.green().abs_diff(bg.green()),
            p.blue().abs_diff(bg.blue()),
            p.alpha().abs_diff(bg.alpha()),
        ]
        .into_iter()
        .any(|d| d > 12)
    };
    let row = |y: u32| (0..w).any(|x| differs(x, y));
    let col = |x: u32| (0..h).any(|y| differs(x, y));
    let Some(top) = (0..h).find(|&y| row(y)) else { return pm.clone() };
    let bottom = (0..h).rev().find(|&y| row(y)).unwrap();
    let left = (0..w).find(|&x| col(x)).unwrap();
    let right = (0..w).rev().find(|&x| col(x)).unwrap();

    let margin = top.min(h - 1 - bottom).min(left).min(w - 1 - right);
    let content = sub_pixmap(pm, left, top, right - left + 1, bottom - top + 1);
    let mut out = Pixmap::new(content.width() + 2 * margin, content.height() + 2 * margin).unwrap();
    out.fill(tiny_skia::Color::from_rgba8(bg.red(), bg.green(), bg.blue(), bg.alpha()).premultiply().demultiply());
    out.draw_pixmap(
        margin as i32,
        margin as i32,
        content.as_ref(),
        &PixmapPaint::default(),
        Transform::identity(),
        None,
    );
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn padding_and_aspect() {
        let shot = Pixmap::new(100, 50).unwrap();
        let bg = Background { padding: 10.0, aspect: Some((1, 1)), ..Default::default() };
        let out = bg.apply(&shot);
        assert_eq!((out.width(), out.height()), (120, 120));
    }

    #[test]
    fn auto_balance_equalizes_margins() {
        let mut pm = Pixmap::new(100, 60).unwrap();
        pm.fill(tiny_skia::Color::WHITE);
        // Content at x 10..30, y 20..40: margins left 10, right 70, top 20, bottom 20.
        let r = tiny_skia::Rect::from_xywh(10.0, 20.0, 20.0, 20.0).unwrap();
        pm.fill_rect(r, &crate::text::paint(Color::BLACK), Transform::identity(), None);
        let out = auto_balance(&pm);
        assert_eq!((out.width(), out.height()), (40, 40));
    }
}
