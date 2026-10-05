//! Sidebar for the Background tool: backdrops, padding, corners, shadow,
//! alignment, aspect ratio, Auto Balance and saved presets.

use std::rc::Rc;

use objc2::{rc::Retained, sel, MainThreadOnly};
use objc2_app_kit::{NSButton, NSOpenPanel, NSPopUpButton, NSSlider, NSView};
use objc2_foundation::{MainThreadMarker, NSPoint, NSRect, NSSize, NSString};
use shot_core::{
    background::{Align, Fill, PRESETS},
    render::to_image,
    Background,
};

use crate::{
    app::app,
    editor::{Editor, SIDEBAR_W},
    ui,
};

const ALIGNS: [(&str, Align); 9] = [
    ("Top Left", Align::TopLeft),
    ("Top", Align::Top),
    ("Top Right", Align::TopRight),
    ("Left", Align::Left),
    ("Center", Align::Center),
    ("Right", Align::Right),
    ("Bottom Left", Align::BottomLeft),
    ("Bottom", Align::Bottom),
    ("Bottom Right", Align::BottomRight),
];
const ASPECTS: [(&str, Option<(u32, u32)>); 8] = [
    ("Auto", None),
    ("1:1", Some((1, 1))),
    ("4:3", Some((4, 3))),
    ("3:2", Some((3, 2))),
    ("16:9", Some((16, 9))),
    ("4:5 (Instagram)", Some((4, 5))),
    ("9:16 (Stories)", Some((9, 16))),
    ("2:1 (Twitter)", Some((2, 1))),
];

type Handler = Box<dyn Fn(&Editor, Option<&objc2::runtime::AnyObject>)>;

/// Controls whose values follow the document's background.
pub struct Panel {
    enabled: Retained<NSButton>,
    padding: Retained<NSSlider>,
    radius: Retained<NSSlider>,
    shadow: Retained<NSSlider>,
    align: Retained<NSPopUpButton>,
    aspect: Retained<NSPopUpButton>,
    balance: Retained<NSButton>,
    presets: Retained<NSPopUpButton>,
}

/// Edits the current background (creating the default one if needed).
fn update(ed: &Editor, f: impl FnOnce(&mut Background)) {
    let mut bg = ed.document().background.clone().unwrap_or_default();
    f(&mut bg);
    ed.set_background(Some(bg));
    sync(ed);
}

pub fn build(ed: &Rc<Editor>, sidebar: &NSView) {
    let mtm = MainThreadMarker::new().unwrap();
    let targets = &ed.targets;
    let me = Rc::downgrade(ed);
    // Wraps an editor callback so controls never keep the editor alive.
    let on = |f: Handler| {
        let me = me.clone();
        targets.add(mtm, move |sender| {
            if let Some(ed) = me.upgrade() {
                f(&ed, sender)
            }
        })
    };
    let mut views: Vec<Retained<NSView>> = vec![];

    let title = ui::label("Background");
    title.setFont(Some(&objc2_app_kit::NSFont::boldSystemFontOfSize(13.0)));
    views.push(ui::view(&title));

    let t = on(Box::new(|ed, sender| {
        let Some(b) = ui::sender::<NSButton>(sender) else { return };
        if b.state() == 1 {
            update(ed, |_| {});
        } else {
            ed.set_background(None);
        }
    }));
    let enabled = unsafe {
        NSButton::checkboxWithTitle_target_action(
            &NSString::from_str("Add background"),
            Some(&t),
            Some(sel!(fire:)),
            mtm,
        )
    };
    views.push(ui::view(&enabled));

    // 20 swatches, 5 per row.
    for row in PRESETS.chunks(5) {
        let mut swatches: Vec<Retained<NSView>> = vec![];
        for fill in row {
            let chosen = fill.clone();
            let t = on(Box::new(move |ed, _| {
                let fill = chosen.clone();
                update(ed, move |bg| bg.fill = fill)
            }));
            let b = unsafe { NSButton::buttonWithImage_target_action(&swatch(fill), Some(&t), Some(sel!(fire:)), mtm) };
            b.setBordered(false);
            ui::set_width(&b, 40.0);
            swatches.push(ui::view(&b));
        }
        views.push(ui::view(&ui::stack(&swatches, true, 6.0)));
    }

    let t = on(Box::new(|ed, _| {
        let mtm = MainThreadMarker::new().unwrap();
        let panel = NSOpenPanel::openPanel(mtm);
        if panel.runModal() == 1 {
            if let Some(path) = panel.URL().and_then(|u| u.path()) {
                update(ed, |bg| bg.fill = Fill::Image(path.to_string()));
            }
        }
    }));
    let custom = unsafe {
        NSButton::buttonWithTitle_target_action(&NSString::from_str("Custom Image…"), Some(&t), Some(sel!(fire:)), mtm)
    };
    views.push(ui::view(&custom));

    let slider = |label: &str, max: f64, f: fn(&mut Background, f32)| {
        let t = on(Box::new(move |ed, sender| {
            let Some(s) = ui::sender::<NSSlider>(sender) else { return };
            let v = s.doubleValue() as f32;
            update(ed, |bg| f(bg, v));
        }));
        let s = unsafe {
            NSSlider::sliderWithValue_minValue_maxValue_target_action(0.0, 0.0, max, Some(&t), Some(sel!(fire:)), mtm)
        };
        // Update on release only: every change re-renders the full image.
        s.setContinuous(false);
        ui::set_width(&s, SIDEBAR_W - 40.0);
        (ui::view(&ui::label(label)), s)
    };
    let (l, padding) = slider("Padding", 240.0, |bg, v| bg.padding = v);
    views.extend([l, ui::view(&padding)]);
    let (l, radius) = slider("Corner radius", 40.0, |bg, v| bg.radius = v);
    views.extend([l, ui::view(&radius)]);
    let (l, shadow) = slider("Shadow", 1.0, |bg, v| bg.shadow = v);
    views.extend([l, ui::view(&shadow)]);

    let popup = |items: &[&str], f: fn(&mut Background, usize)| {
        let p = NSPopUpButton::initWithFrame_pullsDown(NSPopUpButton::alloc(mtm), NSRect::ZERO, false);
        for i in items {
            p.addItemWithTitle(&NSString::from_str(i));
        }
        let t = on(Box::new(move |ed, sender| {
            let Some(p) = ui::sender::<NSPopUpButton>(sender) else { return };
            let i = p.indexOfSelectedItem().max(0) as usize;
            update(ed, |bg| f(bg, i));
        }));
        unsafe {
            p.setTarget(Some(&t));
            p.setAction(Some(sel!(fire:)));
        }
        p
    };
    views.push(ui::view(&ui::label("Alignment")));
    let align = popup(&ALIGNS.map(|a| a.0), |bg, i| bg.align = ALIGNS[i].1);
    views.push(ui::view(&align));
    views.push(ui::view(&ui::label("Aspect ratio")));
    let aspect = popup(&ASPECTS.map(|a| a.0), |bg, i| bg.aspect = ASPECTS[i].1);
    views.push(ui::view(&aspect));

    let t = on(Box::new(|ed, sender| {
        let Some(b) = ui::sender::<NSButton>(sender) else { return };
        let on = b.state() == 1;
        update(ed, |bg| bg.auto_balance = on);
    }));
    let balance = unsafe {
        NSButton::checkboxWithTitle_target_action(&NSString::from_str("Auto Balance"), Some(&t), Some(sel!(fire:)), mtm)
    };
    balance.setToolTip(Some(&NSString::from_str("Evens out the space around the content of your screenshot")));
    views.push(ui::view(&balance));

    views.push(ui::view(&ui::label("Presets")));
    let presets = NSPopUpButton::initWithFrame_pullsDown(NSPopUpButton::alloc(mtm), NSRect::ZERO, false);
    let t = on(Box::new(|ed, sender| {
        let Some(p) = ui::sender::<NSPopUpButton>(sender) else { return };
        let i = p.indexOfSelectedItem();
        let preset = app().settings.borrow().background_presets.get((i - 1).max(0) as usize).map(|p| p.1.clone());
        if let (true, Some(bg)) = (i > 0, preset) {
            ed.set_background(Some(bg));
            sync(ed);
        }
    }));
    unsafe {
        presets.setTarget(Some(&t));
        presets.setAction(Some(sel!(fire:)));
    }
    views.push(ui::view(&presets));
    let t = on(Box::new(|ed, _| {
        let Some(bg) = ed.document().background.clone() else { return };
        let app = app();
        let name = format!("Preset {}", app.settings.borrow().background_presets.len() + 1);
        app.settings.borrow_mut().background_presets.push((name, bg));
        app.save_settings();
        sync(ed);
        crate::app::toast("Background preset saved");
    }));
    let save = unsafe {
        NSButton::buttonWithTitle_target_action(&NSString::from_str("Save as Preset"), Some(&t), Some(sel!(fire:)), mtm)
    };
    views.push(ui::view(&save));

    let stack = ui::stack(&views, false, 8.0);
    stack.setAlignment(objc2_app_kit::NSLayoutAttribute::Leading);
    stack.setEdgeInsets(objc2_foundation::NSEdgeInsets { top: 14.0, left: 16.0, bottom: 14.0, right: 16.0 });
    let size = stack.fittingSize();
    let h = sidebar.frame().size.height.max(size.height);
    stack.setFrame(NSRect::new(NSPoint::new(0.0, h - size.height), NSSize::new(SIDEBAR_W, size.height)));
    stack.setAutoresizingMask(objc2_app_kit::NSAutoresizingMaskOptions::ViewMinYMargin);
    sidebar.addSubview(&stack);

    *ed.panel.borrow_mut() = Some(Panel { enabled, padding, radius, shadow, align, aspect, balance, presets });
    sync(ed);
}

/// Pushes the document's background into the controls.
pub fn sync(ed: &Editor) {
    let panel = ed.panel.borrow();
    let Some(p) = panel.as_ref() else { return };
    let bg = ed.document().background.clone();
    p.enabled.setState(isize::from(bg.is_some()));
    let bg = bg.unwrap_or_default();
    p.padding.setDoubleValue(bg.padding as f64);
    p.radius.setDoubleValue(bg.radius as f64);
    p.shadow.setDoubleValue(bg.shadow as f64);
    p.align.selectItemAtIndex(ALIGNS.iter().position(|a| a.1 == bg.align).unwrap_or(4) as isize);
    p.aspect.selectItemAtIndex(ASPECTS.iter().position(|a| a.1 == bg.aspect).unwrap_or(0) as isize);
    p.balance.setState(isize::from(bg.auto_balance));
    p.presets.removeAllItems();
    p.presets.addItemWithTitle(&NSString::from_str("Apply preset…"));
    for (name, _) in app().settings.borrow().background_presets.iter() {
        p.presets.addItemWithTitle(&NSString::from_str(name));
    }
}

/// A 40×28 preview of a backdrop.
fn swatch(fill: &Fill) -> Retained<objc2_app_kit::NSImage> {
    let (w, h) = (80, 56);
    let mut img = match fill {
        Fill::Transparent => image::RgbaImage::from_fn(w, h, |x, y| {
            let v = if (x / 8 + y / 8) % 2 == 0 { 220 } else { 180 };
            image::Rgba([v, v, v, 255])
        }),
        _ => {
            let bg = Background { fill: fill.clone(), padding: 0.0, radius: 0.0, shadow: 0.0, ..Default::default() };
            to_image(&bg.apply(&tiny_skia::Pixmap::new(w, h).unwrap()))
        }
    };
    for (x, y, px) in img.enumerate_pixels_mut() {
        let (cx, cy) = (x.min(w - 1 - x) as f32, y.min(h - 1 - y) as f32);
        if cx < 8.0 && cy < 8.0 && (8.0 - cx).hypot(8.0 - cy) > 8.0 {
            px.0[3] = 0;
        }
    }
    let ns = ui::nsimage(&img);
    ns.setSize(NSSize::new(40.0, 28.0));
    ns
}
