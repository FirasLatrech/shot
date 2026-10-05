//! Floating Screenshots: images pinned above all windows. Drag to move,
//! scroll/pinch to resize, ⌥-scroll for opacity, arrow keys to nudge,
//! Lock Mode to click through to the apps underneath.

use std::{
    cell::{Cell, RefCell},
    rc::{Rc, Weak},
    sync::Arc,
};

use image::RgbaImage;
use objc2::rc::Retained;
use objc2_app_kit::{NSEvent, NSEventModifierFlags, NSMenu, NSWindow, NSWindowCollectionBehavior};
use objc2_core_foundation::CFRetained;
use objc2_core_graphics::{CGContext, CGImage};
use objc2_foundation::{MainThreadMarker, NSPoint, NSRect, NSSize};
use shot_core::{Document, Pt, Rect};

use crate::{
    app::app,
    cg, gfx,
    ui::{self, key, Targets, View, ViewDelegate},
};

pub struct Pin {
    window: Retained<NSWindow>,
    view: Retained<View>,
    image: Arc<RgbaImage>,
    cg: CFRetained<CGImage>,
    base: NSSize,
    scale: Cell<f64>,
    locked: Cell<bool>,
    /// Targets of the most recent popup menu; replaced by the next one.
    menu_targets: RefCell<Targets>,
}

pub fn show(image: Arc<RgbaImage>, at: Option<NSRect>) {
    let app = app();
    let mtm = app.mtm;
    let screen = objc2_app_kit::NSScreen::mainScreen(mtm).expect("a screen");
    let vf = screen.visibleFrame();
    let backing = screen.backingScaleFactor();
    // Natural size in points, shrunk to fit 60% of the screen.
    let (w, h) = (image.width() as f64 / backing, image.height() as f64 / backing);
    let fit = (vf.size.width * 0.6 / w).min(vf.size.height * 0.6 / h).min(1.0);
    let base = NSSize::new(w * fit, h * fit);
    let frame = at.unwrap_or_else(|| {
        NSRect::new(
            NSPoint::new(
                vf.origin.x + (vf.size.width - base.width) / 2.0,
                vf.origin.y + (vf.size.height - base.height) / 2.0,
            ),
            base,
        )
    });

    let window = ui::overlay_window(mtm, frame);
    window.setLevel(3); // floating: above normal windows
    window.setHasShadow(true);
    window.setCollectionBehavior(
        NSWindowCollectionBehavior::CanJoinAllSpaces | NSWindowCollectionBehavior::FullScreenAuxiliary,
    );

    let pin = Rc::new_cyclic(|weak: &Weak<Pin>| {
        let delegate: Weak<dyn ViewDelegate> = weak.clone();
        let view = View::new(mtm, NSRect::new(NSPoint::ZERO, frame.size), delegate);
        view.setAutoresizingMask(
            objc2_app_kit::NSAutoresizingMaskOptions::ViewWidthSizable
                | objc2_app_kit::NSAutoresizingMaskOptions::ViewHeightSizable,
        );
        Pin {
            cg: cg::image_from_rgba8(&image),
            window,
            view,
            image,
            base,
            scale: Cell::new(frame.size.width / base.width),
            locked: Cell::new(false),
            menu_targets: RefCell::default(),
        }
    });
    pin.window.setContentView(Some(&pin.view));
    app.pins.borrow_mut().push(pin.clone());
    if !app.overlays_hidden.get() {
        pin.window.makeKeyAndOrderFront(None);
    }
}

impl Pin {
    pub fn set_hidden(&self, hidden: bool) {
        if hidden {
            self.window.orderOut(None);
        } else {
            self.window.orderFrontRegardless();
        }
    }

    /// Lock Mode: clicks pass through to the apps underneath.
    pub fn set_locked(&self, locked: bool) {
        self.locked.set(locked);
        self.window.setIgnoresMouseEvents(locked);
        self.view.redraw();
        if locked {
            crate::app::toast("Pin locked — unlock it from the menu bar");
        }
    }

    fn close(&self) {
        self.window.orderOut(None);
        // Release on the next tick: we may be inside one of the pin's own events.
        let me = self as *const Pin;
        crate::app::after(0.0, move || app().pins.borrow_mut().retain(|p| !std::ptr::eq(Rc::as_ptr(p), me)));
    }

    /// Resizes around the window's center.
    fn set_scale(&self, k: f64) {
        let k = k.clamp(0.1, 4.0);
        self.scale.set(k);
        let f = self.window.frame();
        let size = NSSize::new(self.base.width * k, self.base.height * k);
        let origin = NSPoint::new(
            f.origin.x + (f.size.width - size.width) / 2.0,
            f.origin.y + (f.size.height - size.height) / 2.0,
        );
        self.window.setFrame_display(NSRect::new(origin, size), true);
    }

    fn set_opacity(&self, a: f64) {
        self.window.setAlphaValue(a.clamp(0.1, 1.0));
    }

    fn nudge(&self, dx: f64, dy: f64) {
        let f = self.window.frame();
        self.window.setFrameOrigin(NSPoint::new(f.origin.x + dx, f.origin.y + dy));
    }

    fn me(&self) -> Option<Rc<Pin>> {
        let me = self as *const Pin;
        app().pins.borrow().iter().find(|p| std::ptr::eq(Rc::as_ptr(p), me)).cloned()
    }

    fn context_menu(&self) -> Retained<NSMenu> {
        let mtm = MainThreadMarker::new().unwrap();
        let me = self.me().map(|p| Rc::downgrade(&p)).unwrap_or_default();
        let act = |f: fn(&Pin)| -> Box<dyn Fn()> {
            let me = me.clone();
            Box::new(move || {
                if let Some(p) = me.upgrade() {
                    f(&p)
                }
            })
        };
        let targets = Targets::default();
        let menu = ui::menu(
            mtm,
            &targets,
            vec![
                (
                    "Copy",
                    act(|p| {
                        ui::copy_image(&p.image);
                        crate::app::toast("Copied to clipboard");
                    }),
                ),
                (
                    "Save",
                    act(|p| {
                        app().save(&p.image);
                    }),
                ),
                ("Annotate", act(|p| crate::editor::open(Document::new((*p.image).clone())))),
                ("Copy Text (OCR)", act(|p| app().ocr(&p.image))),
                ("-", Box::new(|| {})),
                ("Opacity 100%", act(|p| p.set_opacity(1.0))),
                ("Opacity 75%", act(|p| p.set_opacity(0.75))),
                ("Opacity 50%", act(|p| p.set_opacity(0.5))),
                ("Opacity 25%", act(|p| p.set_opacity(0.25))),
                ("Actual Size", act(|p| p.set_scale(1.0))),
                ("-", Box::new(|| {})),
                ("Lock (click through)", act(|p| p.set_locked(true))),
                ("Close", act(|p| p.close())),
            ],
        );
        *self.menu_targets.borrow_mut() = targets;
        menu
    }
}

impl ViewDelegate for Pin {
    fn draw(&self, view: &View, c: &CGContext) {
        let b = view.bounds();
        let r = Rect::new(0.0, 0.0, b.size.width as f32, b.size.height as f32);
        gfx::image(c, &self.cg, r, true);
        if self.locked.get() {
            gfx::stroke(c, r.inflate(-1.0), 2.0, (1.0, 0.6, 0.0, 0.8));
        }
    }

    fn mouse_down(&self, _view: &View, _p: Pt, ev: &NSEvent) {
        if ev.clickCount() == 2 {
            return self.close();
        }
        self.window.performWindowDragWithEvent(ev);
    }

    fn right_mouse_down(&self, view: &View, _p: Pt, ev: &NSEvent) {
        NSMenu::popUpContextMenu_withEvent_forView(&self.context_menu(), ev, view);
    }

    fn scroll(&self, _view: &View, ev: &NSEvent) {
        let dy = ev.scrollingDeltaY() * if ev.hasPreciseScrollingDeltas() { 0.005 } else { 0.05 };
        if ui::has_mod(ev, NSEventModifierFlags::Option) {
            self.set_opacity(self.window.alphaValue() + dy);
        } else {
            self.set_scale(self.scale.get() * (1.0 + dy));
        }
    }

    fn magnify(&self, _view: &View, ev: &NSEvent) {
        self.set_scale(self.scale.get() * (1.0 + ev.magnification()));
    }

    fn key_down(&self, _view: &View, ev: &NSEvent) -> bool {
        let step = if ui::has_mod(ev, NSEventModifierFlags::Shift) { 10.0 } else { 1.0 };
        let cmd = ui::has_mod(ev, NSEventModifierFlags::Command);
        match (ev.keyCode(), ui::key_chars(ev).as_str()) {
            (key::ESCAPE, _) => self.close(),
            (_, "w") if cmd => self.close(),
            (_, "c") if cmd => ui::copy_image(&self.image),
            (key::LEFT, _) => self.nudge(-step, 0.0),
            (key::RIGHT, _) => self.nudge(step, 0.0),
            (key::UP, _) => self.nudge(0.0, step),
            (key::DOWN, _) => self.nudge(0.0, -step),
            (_, "=" | "+") => self.set_scale(self.scale.get() * 1.1),
            (_, "-") => self.set_scale(self.scale.get() / 1.1),
            (_, "0") => self.set_scale(1.0),
            (_, "l") => self.set_locked(true),
            _ => return false,
        }
        true
    }
}
