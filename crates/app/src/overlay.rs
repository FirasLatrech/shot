//! Quick Access Overlay: the thumbnail card shown after each capture, with
//! copy / save / annotate / pin / OCR, drag & drop, swipe-to-dismiss and auto-close.

use std::{
    cell::{Cell, RefCell},
    rc::Rc,
};

use objc2::rc::Retained;
use objc2_app_kit::{NSBezierPath, NSEvent, NSEventPhase, NSMenu, NSScreen, NSWindow, NSWindowCollectionBehavior};
use objc2_core_foundation::CFRetained;
use objc2_core_graphics::{CGContext, CGImage};
use objc2_foundation::{MainThreadMarker, NSPoint, NSRect, NSSize};
use shot_core::{settings::Corner, Document, Pt, Rect};

use crate::{
    app::{after, app, Capture},
    cg, gfx,
    ui::{self, Targets, View, ViewDelegate},
};

const BASE_W: f64 = 220.0;
const MARGIN: f64 = 16.0;
const GAP: f64 = 12.0;

/// The card's hover controls.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Ctl {
    Copy,
    Save,
    Close,
    Annotate,
    Text,
    Pin,
}

/// Where each control sits on a card of size `w`×`h`.
fn controls(w: f32, h: f32) -> [(Ctl, Rect); 6] {
    let (pw, ph, d) = (88.0, 28.0, 26.0);
    let cx = (w - pw) / 2.0;
    [
        (Ctl::Copy, Rect::new(cx, h / 2.0 - ph - 4.0, pw, ph)),
        (Ctl::Save, Rect::new(cx, h / 2.0 + 4.0, pw, ph)),
        (Ctl::Close, Rect::new(8.0, 8.0, d, d)),
        (Ctl::Annotate, Rect::new(w - d - 8.0, 8.0, d, d)),
        (Ctl::Text, Rect::new(8.0, h - d - 8.0, d, d)),
        (Ctl::Pin, Rect::new(w - d - 8.0, h - d - 8.0, d, d)),
    ]
}

pub struct QuickOverlay {
    pub capture: Capture,
    window: Retained<NSWindow>,
    view: RefCell<Option<Retained<View>>>,
    cg: CFRetained<CGImage>,
    hovered: Cell<bool>,
    hot: Cell<Option<Ctl>>,
    press: Cell<Option<Pt>>,
    swipe: Cell<f64>,
    closed: Cell<bool>,
    screen: Retained<NSScreen>,
    /// Targets of the most recent popup menu; replaced by the next one.
    menu_targets: RefCell<Targets>,
}

pub fn show(capture: Capture) {
    let app = app();
    let mtm = app.mtm;
    let size = card_size(&capture, app.settings.borrow().overlay_scale as f64);
    let screen = screen_under_mouse(mtm);
    let window = ui::overlay_window(mtm, NSRect::new(NSPoint::ZERO, size));
    window.setLevel(25);
    window.setHasShadow(true);
    window.setCollectionBehavior(
        NSWindowCollectionBehavior::CanJoinAllSpaces | NSWindowCollectionBehavior::FullScreenAuxiliary,
    );

    let o = Rc::new(QuickOverlay {
        cg: cg::image_from_rgba8(&capture.image),
        capture,
        window,
        view: RefCell::default(),
        hovered: Cell::new(false),
        hot: Cell::new(None),
        press: Cell::new(None),
        swipe: Cell::new(0.0),
        closed: Cell::new(false),
        screen,
        menu_targets: RefCell::default(),
    });
    let weak: std::rc::Weak<dyn ViewDelegate> = Rc::<QuickOverlay>::downgrade(&o);
    let view = View::new(mtm, NSRect::new(NSPoint::ZERO, size), weak);
    ui::round_corners(&view, 12.0);
    o.window.setContentView(Some(&view));
    *o.view.borrow_mut() = Some(view);

    app.overlays.borrow_mut().push(o.clone());
    layout();
    if app.overlays_hidden.get() {
        o.window.orderOut(None);
    } else {
        o.window.orderFrontRegardless();
    }

    let auto = app.settings.borrow().overlay_auto_close;
    if let Some(secs) = auto {
        auto_close(Rc::downgrade(&o), secs as f64);
    }
}

/// Closes after `secs`, but waits while the pointer is over the card.
fn auto_close(weak: std::rc::Weak<QuickOverlay>, secs: f64) {
    after(secs, move || {
        if let Some(o) = weak.upgrade() {
            if o.hovered.get() {
                auto_close(Rc::downgrade(&o), 1.0);
            } else {
                o.close();
            }
        }
    });
}

fn card_size(c: &Capture, scale: f64) -> NSSize {
    let w = BASE_W * scale.clamp(0.5, 2.0);
    let aspect = c.image.width() as f64 / c.image.height().max(1) as f64;
    NSSize::new(w, (w / aspect).clamp(w * 0.45, w * 1.3))
}

fn screen_under_mouse(mtm: MainThreadMarker) -> Retained<NSScreen> {
    let m = NSEvent::mouseLocation();
    let screens = NSScreen::screens(mtm);
    let found = screens.iter().find(|s| {
        let f = s.frame();
        m.x >= f.origin.x && m.x <= f.origin.x + f.size.width && m.y >= f.origin.y && m.y <= f.origin.y + f.size.height
    });
    found.or_else(|| NSScreen::mainScreen(mtm)).expect("a screen")
}

/// Stacks the cards from the configured corner; newest closest to it.
fn layout() {
    let app = app();
    let corner = app.settings.borrow().overlay_corner;
    // Cards stack per screen: cards on one display don't push the others.
    let mut offsets: Vec<(*const NSScreen, f64)> = vec![];
    for o in app.overlays.borrow().iter().rev() {
        let key = Retained::as_ptr(&o.screen);
        let i = offsets.iter().position(|(k, _)| *k == key).unwrap_or_else(|| {
            offsets.push((key, 0.0));
            offsets.len() - 1
        });
        let offset = offsets[i].1;
        let vf = o.screen.visibleFrame();
        let size = o.window.frame().size;
        let x = match corner {
            Corner::BottomLeft | Corner::TopLeft => vf.origin.x + MARGIN,
            _ => vf.origin.x + vf.size.width - size.width - MARGIN,
        };
        let y = match corner {
            Corner::BottomLeft | Corner::BottomRight => vf.origin.y + MARGIN + offset,
            _ => vf.origin.y + vf.size.height - MARGIN - size.height - offset,
        };
        o.window.setFrame_display(NSRect::new(NSPoint::new(x, y), size), true);
        offsets[i].1 += size.height + GAP;
    }
}

impl QuickOverlay {
    pub fn set_hidden(&self, hidden: bool) {
        if hidden {
            self.window.orderOut(None);
        } else if !self.closed.get() {
            self.window.orderFrontRegardless();
        }
    }

    pub fn close(&self) {
        if self.closed.replace(true) {
            return;
        }
        self.window.orderOut(None);
        {
            // Keep only the last few: each one holds a full-size image.
            let app = app();
            let mut closed = app.recently_closed.borrow_mut();
            closed.push(self.capture.clone());
            let excess = closed.len().saturating_sub(10);
            closed.drain(..excess);
        }
        // Release on the next tick: we may be inside one of the card's own events.
        let me = self as *const QuickOverlay;
        after(0.0, move || {
            app().overlays.borrow_mut().retain(|o| !std::ptr::eq(Rc::as_ptr(o), me));
            layout();
        });
    }

    fn copy(&self) {
        ui::copy_image(&self.capture.image);
        crate::app::toast("Copied to clipboard");
        self.close();
    }

    fn save(&self) {
        if app().save(&self.capture.image).is_some() {
            self.close();
        }
    }

    fn annotate(&self) {
        crate::editor::open(Document::new((*self.capture.image).clone()));
        self.close();
    }

    fn pin(&self) {
        crate::pin::show(self.capture.image.clone(), None);
        self.close();
    }

    fn set_hover(&self, on: bool) {
        self.hovered.set(on);
        if !on {
            self.hot.set(None);
        }
        if let Some(v) = self.view.borrow().as_ref() {
            v.redraw();
        }
    }

    fn run(&self, c: Ctl) {
        match c {
            Ctl::Copy => self.copy(),
            Ctl::Save => self.save(),
            Ctl::Close => self.close(),
            Ctl::Annotate => self.annotate(),
            Ctl::Text => app().ocr(&self.capture.image),
            Ctl::Pin => self.pin(),
        }
    }

    fn context_menu(self: &Rc<Self>, view: &View, ev: &NSEvent) {
        let me = Rc::downgrade(self);
        let act = |f: fn(&QuickOverlay)| -> Box<dyn Fn()> {
            let me = me.clone();
            Box::new(move || {
                if let Some(o) = me.upgrade() {
                    f(&o)
                }
            })
        };
        let targets = Targets::default();
        let menu = ui::menu(
            MainThreadMarker::new().unwrap(),
            &targets,
            vec![
                ("Copy", act(|o| o.copy())),
                ("Save", act(|o| o.save())),
                (
                    "Save As…",
                    act(|o| {
                        if app().save_as(&o.capture.image).is_some() {
                            o.close()
                        }
                    }),
                ),
                ("Annotate", act(|o| o.annotate())),
                ("Pin to Screen", act(|o| o.pin())),
                ("Copy Text (OCR)", act(|o| app().ocr(&o.capture.image))),
                ("-", Box::new(|| {})),
                ("Close", act(|o| o.close())),
            ],
        );
        *self.menu_targets.borrow_mut() = targets;
        NSMenu::popUpContextMenu_withEvent_forView(&menu, ev, view);
    }

    fn me(&self) -> Option<Rc<QuickOverlay>> {
        let me = self as *const QuickOverlay;
        app().overlays.borrow().iter().find(|o| std::ptr::eq(Rc::as_ptr(o), me)).cloned()
    }
}

impl ViewDelegate for QuickOverlay {
    fn draw(&self, view: &View, c: &CGContext) {
        let b = view.bounds();
        let bounds = Rect::new(0.0, 0.0, b.size.width as f32, b.size.height as f32);
        gfx::fill(c, bounds, (0.11, 0.11, 0.13, 1.0));
        let (iw, ih) = (self.capture.image.width() as f32, self.capture.image.height() as f32);
        let k = (bounds.w / iw).min(bounds.h / ih);
        let (w, h) = (iw * k, ih * k);
        gfx::image(c, &self.cg, Rect::new((bounds.w - w) / 2.0, (bounds.h - h) / 2.0, w, h), true);

        if self.hovered.get() {
            gfx::fill(c, bounds, (0.0, 0.0, 0.0, 0.42));
            for (ctl, r) in controls(bounds.w, bounds.h) {
                let hot = self.hot.get() == Some(ctl);
                match ctl {
                    Ctl::Copy | Ctl::Save => {
                        let bg = if hot { (1.0, 1.0, 1.0, 1.0) } else { (1.0, 1.0, 1.0, 0.88) };
                        gfx::round_rect(r, r.h as f64 / 2.0, bg);
                        let label = if ctl == Ctl::Copy { "Copy" } else { "Save" };
                        let s = gfx::text_size(label, 13.0, true);
                        gfx::text(
                            label,
                            r.center().x - s.width as f32 / 2.0,
                            r.center().y - s.height as f32 / 2.0,
                            13.0,
                            true,
                            (0.1, 0.1, 0.12, 1.0),
                        );
                    }
                    _ => {
                        let alpha = if hot { 0.85 } else { 0.55 };
                        gfx::fill_ellipse(c, r, (0.0, 0.0, 0.0, alpha));
                        gfx::stroke_ellipse(c, r.inflate(-0.5), 1.0, (1.0, 1.0, 1.0, 0.25));
                        let name = match ctl {
                            Ctl::Close => "xmark",
                            Ctl::Annotate => "pencil",
                            Ctl::Text => "text.viewfinder",
                            _ => "pin.fill",
                        };
                        gfx::symbol(name, r, 11.0, gfx::WHITE);
                    }
                }
            }
            let label = format!("{} × {}", iw as u32, ih as u32);
            let s = gfx::text_size(&label, 10.0, true);
            gfx::text(&label, (bounds.w - s.width as f32) / 2.0, bounds.h - 22.0, 10.0, true, (1.0, 1.0, 1.0, 0.75));
        }
        // Hairline border so light screenshots don't blend into light desktops.
        let path =
            NSBezierPath::bezierPathWithRoundedRect_xRadius_yRadius(ui::ns_rect(bounds.inflate(-0.5)), 12.0, 12.0);
        gfx::ns_color((1.0, 1.0, 1.0, 0.2)).setStroke();
        path.stroke();
    }

    fn mouse_moved(&self, view: &View, p: Pt) {
        let b = view.bounds();
        let hot = controls(b.size.width as f32, b.size.height as f32)
            .into_iter()
            .find(|(_, r)| r.contains(p))
            .map(|(c, _)| c);
        if hot != self.hot.get() {
            self.hot.set(hot);
            view.redraw();
        }
    }

    fn mouse_up(&self, view: &View, p: Pt, _ev: &NSEvent) {
        let b = view.bounds();
        let pressed = self.press.take();
        let hit = controls(b.size.width as f32, b.size.height as f32)
            .into_iter()
            .find(|(_, r)| r.contains(p))
            .map(|(c, _)| c);
        if let (Some(c), Some(start)) = (hit, pressed) {
            if controls(b.size.width as f32, b.size.height as f32).iter().any(|(k, r)| *k == c && r.contains(start)) {
                self.run(c);
            }
        }
    }

    fn mouse_entered(&self, _view: &View) {
        self.set_hover(true);
    }

    fn mouse_exited(&self, _view: &View) {
        self.set_hover(false);
    }

    fn mouse_down(&self, view: &View, p: Pt, ev: &NSEvent) {
        self.press.set(Some(p));
        let b = view.bounds();
        let on_control = controls(b.size.width as f32, b.size.height as f32).iter().any(|(_, r)| r.contains(p));
        if ev.clickCount() == 2 && !on_control {
            self.annotate();
        }
    }

    fn mouse_dragged(&self, view: &View, p: Pt, ev: &NSEvent) {
        let Some(start) = self.press.get() else { return };
        if start.dist(p) < 4.0 {
            return;
        }
        self.press.set(None);
        let file = self.capture.file.clone().or_else(|| crate::app::temp_png(&self.capture.image));
        if let Some(file) = file {
            let b = view.bounds();
            view.drag_file(&file, &ui::nsimage(&self.capture.image), b, ev);
        }
    }

    fn right_mouse_down(&self, view: &View, _p: Pt, ev: &NSEvent) {
        if let Some(me) = self.me() {
            me.context_menu(view, ev);
        }
    }

    /// Two-finger swipe toward the screen edge dismisses the card.
    fn scroll(&self, _view: &View, ev: &NSEvent) {
        if !ev.hasPreciseScrollingDeltas() {
            return;
        }
        if ev.phase() == NSEventPhase::Began {
            self.swipe.set(0.0);
        }
        let total = self.swipe.get() + ev.scrollingDeltaX();
        self.swipe.set(total);
        let corner = app().settings.borrow().overlay_corner;
        let toward_edge = match corner {
            Corner::BottomLeft | Corner::TopLeft => total < -40.0,
            _ => total > 40.0,
        };
        if toward_edge {
            self.swipe.set(0.0);
            self.close();
        }
    }
}
