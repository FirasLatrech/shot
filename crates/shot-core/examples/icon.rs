//! Renders the 1024×1024 app icon: `cargo run -p shot-core --example icon -- out.png`.

use shot_core::{effects, logo, text::round_rect_path, Rect};
use tiny_skia::{Color, FillRule, GradientStop, LinearGradient, Paint, Pixmap, Point, SpreadMode, Stroke, Transform};

fn main() {
    let mut pm = Pixmap::new(1024, 1024).unwrap();
    // macOS icon grid: an 824pt squircle centered on the 1024 canvas.
    let body = Rect::new(100.0, 100.0, 824.0, 824.0);
    let path = round_rect_path(body, 185.0).unwrap();

    // Soft drop shadow.
    let mut shadow = Pixmap::new(1024, 1024).unwrap();
    let mut p = Paint::default();
    p.set_color(Color::from_rgba8(0, 0, 0, 110));
    shadow.fill_path(&path, &p, FillRule::Winding, Transform::from_translate(0.0, 14.0), None);
    effects::blur(&mut shadow, None, 18);
    pm.draw_pixmap(0, 0, shadow.as_ref(), &Default::default(), Transform::identity(), None);

    let grad = |a: Color, b: Color, y0: f32, y1: f32| {
        LinearGradient::new(
            Point::from_xy(512.0, y0),
            Point::from_xy(512.0, y1),
            vec![GradientStop::new(0.0, a), GradientStop::new(1.0, b)],
            SpreadMode::Pad,
            Transform::identity(),
        )
        .unwrap()
    };
    // Near-black body, a touch lighter at the top.
    let fill = Paint {
        shader: grad(Color::from_rgba8(38, 38, 44, 255), Color::from_rgba8(8, 8, 10, 255), 100.0, 924.0),
        anti_alias: true,
        ..Default::default()
    };
    pm.fill_path(&path, &fill, FillRule::Winding, Transform::identity(), None);
    // Hairline rim that catches the light at the top edge.
    let rim = Paint {
        shader: grad(Color::from_rgba8(255, 255, 255, 60), Color::from_rgba8(255, 255, 255, 8), 100.0, 924.0),
        anti_alias: true,
        ..Default::default()
    };
    let inner = round_rect_path(body.inflate(-2.0), 183.0).unwrap();
    pm.stroke_path(&inner, &rim, &Stroke { width: 3.0, ..Default::default() }, Transform::identity(), None);

    // The mark, optically centered.
    let mark = logo::centered(1024.0, 1024.0, 330.0);
    let white = Paint {
        shader: grad(Color::WHITE, Color::from_rgba8(225, 225, 232, 255), 347.0, 677.0),
        anti_alias: true,
        ..Default::default()
    };
    pm.fill_path(&mark, &white, FillRule::Winding, Transform::identity(), None);

    pm.save_png(std::env::args().nth(1).expect("output path")).unwrap();
}
