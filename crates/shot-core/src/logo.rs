//! The Shot mark: three panels in perspective, each with a short left edge
//! and a tall right edge, growing wider from left to right.

use tiny_skia::{Path, PathBuilder};

/// Panels as (left x, right x) in units where the mark is 1.0 tall.
const PANELS: [(f32, f32); 3] = [(0.0, 0.14), (0.288, 0.577), (0.721, 1.162)];
/// Height of each panel's short left edge, relative to its right edge.
const LEFT_EDGE: f32 = 0.72;
/// Width of the mark relative to its height.
pub const ASPECT: f32 = 1.162;

/// The mark with its top-left at (`x`, `y`), `height` tall.
pub fn path(x: f32, y: f32, height: f32) -> Path {
    let mut pb = PathBuilder::new();
    let inset = height * (1.0 - LEFT_EDGE) / 2.0;
    for (l, r) in PANELS {
        let (l, r) = (x + l * height, x + r * height);
        pb.move_to(l, y + inset);
        pb.line_to(r, y);
        pb.line_to(r, y + height);
        pb.line_to(l, y + height - inset);
        pb.close();
    }
    pb.finish().expect("non-empty mark")
}

/// The mark centered in a `w`×`h` box, `height` tall.
pub fn centered(w: f32, h: f32, height: f32) -> Path {
    path((w - height * ASPECT) / 2.0, (h - height) / 2.0, height)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mark_fills_its_box() {
        let b = path(10.0, 20.0, 100.0).bounds();
        assert_eq!((b.left(), b.top()), (10.0, 20.0));
        assert!((b.width() - ASPECT * 100.0).abs() < 1e-3);
        assert!((b.height() - 100.0).abs() < 1e-3);
    }
}
