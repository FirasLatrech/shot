use shot_core::{annot::*, background::*, geom::*, *};
fn main() {
    let fonts = Fonts::system().unwrap();
    let mut img = image::RgbaImage::from_pixel(900, 560, image::Rgba([250, 250, 252, 255]));
    for y in 0..560 {
        for x in 0..900 {
            if (x / 40 + y / 40) % 2 == 0 {
                img.put_pixel(x, y, image::Rgba([225, 230, 240, 255]));
            }
        }
    }
    let mut d = Document::new(img);
    let a = |s, c| Annotation::new(s, c, 5.0);
    let p = Pt::new;
    let mut v = vec![
        a(Shape::Arrow { from: p(40., 40.), to: p(220., 140.), style: ArrowStyle::Standard, ctrl: None }, Color::RED),
        a(Shape::Arrow { from: p(40., 200.), to: p(220., 240.), style: ArrowStyle::Thin, ctrl: None }, PALETTE[4]),
        a(Shape::Arrow { from: p(40., 300.), to: p(220., 300.), style: ArrowStyle::Double, ctrl: None }, PALETTE[3]),
        a(Shape::Arrow { from: p(40., 420.), to: p(240., 380.), style: ArrowStyle::Curved, ctrl: None }, PALETTE[5]),
        a(Shape::Rect { rect: Rect::new(280., 30., 140., 90.), filled: false }, Color::RED),
        a(Shape::Ellipse { rect: Rect::new(440., 30., 140., 90.), filled: false }, PALETTE[4]),
        a(Shape::Rect { rect: Rect::new(600., 30., 140., 90.), filled: true }, PALETTE[1]),
        a(
            Shape::Pencil {
                points: smooth(&[p(280., 160.), p(320., 200.), p(360., 150.), p(400., 210.), p(440., 160.)], 3),
            },
            PALETTE[5],
        ),
        a(Shape::Highlighter { points: vec![p(470., 180.), p(720., 180.)] }, PALETTE[2]),
        a(Shape::Counter { at: p(780., 60.), n: 1 }, Color::RED),
        a(Shape::Counter { at: p(840., 60.), n: 2 }, PALETTE[4]),
        a(Shape::Pixelate { rect: Rect::new(600., 240., 120., 80.) }, Color::RED),
        a(Shape::Blur { rect: Rect::new(740., 240., 120., 80.), secure: false }, Color::RED),
        a(Shape::Spotlight { rect: Rect::new(560., 400., 300., 130.), ellipse: false }, Color::RED),
    ];
    for (i, s) in TextStyle::ALL.into_iter().enumerate() {
        v.push(a(
            Shape::Text {
                at: p(290. + (i % 2) as f32 * 140., 250. + (i / 2) as f32 * 70.),
                text: format!("{s:?}"),
                style: s,
                size: 26.,
            },
            if i % 2 == 0 { Color::RED } else { PALETTE[4] },
        ));
    }
    d.annotations = v;
    d.background = Some(Background { fill: background::PRESETS[1].clone(), ..Default::default() });
    render(&d, &fonts).save(std::env::args().nth(1).unwrap()).unwrap();
}
