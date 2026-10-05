use serde::{Deserialize, Serialize};

use crate::geom::{Color, Pt, Rect};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum ArrowStyle {
    /// Tapered body with a solid head.
    #[default]
    Standard,
    /// Constant-width line with an open head.
    Thin,
    /// Heads on both ends.
    Double,
    /// Quadratic curve through `ctrl`.
    Curved,
}

/// The 7 predefined text looks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum TextStyle {
    #[default]
    Plain,
    Outline,
    Shadow,
    Pill,
    Box,
    Highlight,
    Mono,
}

impl TextStyle {
    pub const ALL: [TextStyle; 7] = [
        TextStyle::Plain,
        TextStyle::Outline,
        TextStyle::Shadow,
        TextStyle::Pill,
        TextStyle::Box,
        TextStyle::Highlight,
        TextStyle::Mono,
    ];
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Shape {
    Arrow {
        from: Pt,
        to: Pt,
        style: ArrowStyle,
        ctrl: Option<Pt>,
    },
    Line {
        from: Pt,
        to: Pt,
    },
    Rect {
        rect: Rect,
        filled: bool,
    },
    Ellipse {
        rect: Rect,
        filled: bool,
    },
    /// Freehand, smoothed when the stroke ends.
    Pencil {
        points: Vec<Pt>,
    },
    /// Translucent marker stroke, rendered multiply-like beneath text.
    Highlighter {
        points: Vec<Pt>,
    },
    Text {
        at: Pt,
        text: String,
        style: TextStyle,
        size: f32,
    },
    Counter {
        at: Pt,
        n: u32,
    },
    Blur {
        rect: Rect,
        secure: bool,
    },
    Pixelate {
        rect: Rect,
    },
    Spotlight {
        rect: Rect,
        ellipse: bool,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Annotation {
    pub shape: Shape,
    pub color: Color,
    /// Stroke width in image pixels.
    pub width: f32,
}

impl Annotation {
    pub fn new(shape: Shape, color: Color, width: f32) -> Self {
        Self { shape, color, width }
    }

    /// Effects modify the pixels beneath them instead of drawing on top.
    pub fn is_effect(&self) -> bool {
        matches!(self.shape, Shape::Blur { .. } | Shape::Pixelate { .. } | Shape::Spotlight { .. })
    }

    /// Bounding box, used for selection handles and hit-testing.
    pub fn bounds(&self) -> Rect {
        let pad = self.width;
        match &self.shape {
            Shape::Arrow { from, to, ctrl, .. } => {
                let r = Rect::from_pts(*from, *to);
                ctrl.map_or(r, |c| r.union(&Rect::new(c.x, c.y, 0.0, 0.0))).inflate(pad * 2.0)
            }
            Shape::Line { from, to } => Rect::from_pts(*from, *to).inflate(pad),
            Shape::Rect { rect, .. }
            | Shape::Ellipse { rect, .. }
            | Shape::Blur { rect, .. }
            | Shape::Pixelate { rect }
            | Shape::Spotlight { rect, .. } => *rect,
            Shape::Pencil { points } | Shape::Highlighter { points } => points_bounds(points).inflate(pad),
            Shape::Text { at, text, size, .. } => {
                // Approximation; the editor refines this with measured text when it has fonts.
                let lines = text.lines().count().max(1) as f32;
                let cols = text.lines().map(|l| l.chars().count()).max().unwrap_or(1) as f32;
                Rect::new(at.x, at.y, cols * size * 0.6, lines * size * 1.25).inflate(size * 0.4)
            }
            Shape::Counter { at, .. } => {
                let r = counter_radius(self.width);
                Rect::new(at.x - r, at.y - r, 2.0 * r, 2.0 * r)
            }
        }
    }

    /// Whether `p` touches this annotation within `tol` pixels.
    pub fn hit(&self, p: Pt, tol: f32) -> bool {
        let tol = tol + self.width / 2.0;
        match &self.shape {
            Shape::Arrow { from, to, ctrl: Some(c), style: ArrowStyle::Curved } => {
                curve_points(*from, *c, *to, 24).windows(2).any(|w| seg_dist(p, w[0], w[1]) <= tol)
            }
            Shape::Arrow { from, to, .. } | Shape::Line { from, to } => seg_dist(p, *from, *to) <= tol,
            Shape::Rect { rect, filled: false } | Shape::Ellipse { rect, filled: false } => {
                rect.inflate(tol).contains(p) && !rect.inflate(-tol).contains(p)
            }
            Shape::Pencil { points } | Shape::Highlighter { points } => match points.as_slice() {
                [only] => only.dist(p) <= tol,
                pts => pts.windows(2).any(|w| seg_dist(p, w[0], w[1]) <= tol),
            },
            _ => self.bounds().inflate(tol).contains(p),
        }
    }

    pub fn translate(&mut self, dx: f32, dy: f32) {
        let mv = |p: &mut Pt| *p = p.offset(dx, dy);
        match &mut self.shape {
            Shape::Arrow { from, to, ctrl, .. } => {
                mv(from);
                mv(to);
                if let Some(c) = ctrl {
                    mv(c);
                }
            }
            Shape::Line { from, to } => {
                mv(from);
                mv(to);
            }
            Shape::Rect { rect, .. }
            | Shape::Ellipse { rect, .. }
            | Shape::Blur { rect, .. }
            | Shape::Pixelate { rect }
            | Shape::Spotlight { rect, .. } => *rect = rect.translate(dx, dy),
            Shape::Pencil { points } | Shape::Highlighter { points } => points.iter_mut().for_each(mv),
            Shape::Text { at, .. } | Shape::Counter { at, .. } => mv(at),
        }
    }
}

pub fn counter_radius(width: f32) -> f32 {
    10.0 + width * 3.0
}

fn points_bounds(points: &[Pt]) -> Rect {
    let Some(first) = points.first() else {
        return Rect::default();
    };
    points.iter().fold(Rect::new(first.x, first.y, 0.0, 0.0), |r, p| r.union(&Rect::new(p.x, p.y, 0.0, 0.0)))
}

fn seg_dist(p: Pt, a: Pt, b: Pt) -> f32 {
    let (dx, dy) = (b.x - a.x, b.y - a.y);
    let len2 = dx * dx + dy * dy;
    if len2 == 0.0 {
        return p.dist(a);
    }
    let t = (((p.x - a.x) * dx + (p.y - a.y) * dy) / len2).clamp(0.0, 1.0);
    p.dist(a.lerp(b, t))
}

/// Samples a quadratic Bézier into `n` segments.
pub fn curve_points(a: Pt, c: Pt, b: Pt, n: usize) -> Vec<Pt> {
    let n = n.max(1);
    (0..=n)
        .map(|i| {
            let t = i as f32 / n as f32;
            a.lerp(c, t).lerp(c.lerp(b, t), t)
        })
        .collect()
}

/// Chaikin corner-cutting: turns jittery mouse input into a smooth stroke.
pub fn smooth(points: &[Pt], iterations: usize) -> Vec<Pt> {
    let mut pts = points.to_vec();
    for _ in 0..iterations {
        if pts.len() < 3 {
            break;
        }
        let mut out = Vec::with_capacity(pts.len() * 2);
        out.push(pts[0]);
        for w in pts.windows(2) {
            out.push(w[0].lerp(w[1], 0.25));
            out.push(w[0].lerp(w[1], 0.75));
        }
        out.push(*pts.last().unwrap());
        pts = out;
    }
    pts
}

/// Drops points closer than `min_dist` to the previous kept point.
pub fn simplify(points: &[Pt], min_dist: f32) -> Vec<Pt> {
    let mut out: Vec<Pt> = Vec::with_capacity(points.len());
    for &p in points {
        if out.last().is_none_or(|l| l.dist(p) >= min_dist) {
            out.push(p);
        }
    }
    if let (Some(&last), Some(&kept)) = (points.last(), out.last()) {
        if kept != last {
            out.push(last);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hit_line_and_outline_rect() {
        let line = Annotation::new(Shape::Line { from: Pt::new(0.0, 0.0), to: Pt::new(100.0, 0.0) }, Color::RED, 4.0);
        assert!(line.hit(Pt::new(50.0, 3.0), 2.0));
        assert!(!line.hit(Pt::new(50.0, 20.0), 2.0));

        let rect =
            Annotation::new(Shape::Rect { rect: Rect::new(0.0, 0.0, 100.0, 100.0), filled: false }, Color::RED, 2.0);
        assert!(rect.hit(Pt::new(0.0, 50.0), 3.0));
        assert!(!rect.hit(Pt::new(50.0, 50.0), 3.0), "inside of an outline is not a hit");
    }

    #[test]
    fn smoothing_keeps_endpoints() {
        let pts = [Pt::new(0.0, 0.0), Pt::new(10.0, 10.0), Pt::new(20.0, 0.0)];
        let s = smooth(&pts, 2);
        assert_eq!(s.first(), pts.first());
        assert_eq!(s.last(), pts.last());
        assert!(s.len() > pts.len());
    }

    #[test]
    fn translate_moves_bounds() {
        let mut a = Annotation::new(Shape::Rect { rect: Rect::new(1.0, 2.0, 3.0, 4.0), filled: true }, Color::RED, 2.0);
        a.translate(10.0, 20.0);
        assert_eq!(a.bounds(), Rect::new(11.0, 22.0, 3.0, 4.0));
    }
}
