//! The capture UI: one full-screen window per display that freezes the
//! screen, shows a crosshair and magnifier, and lets the user drag an area or
//! pick a window. All-In-One adds an adjustable selection with a toolbar.

use std::{
    cell::{Cell, RefCell},
    rc::{Rc, Weak},
    sync::Arc,
};

use image::RgbaImage;
use objc2::{rc::Retained, sel, MainThreadOnly};
use objc2_app_kit::{
    NSCursor, NSEvent, NSEventModifierFlags, NSFont, NSPopUpButton, NSStackView, NSTextField,
    NSUserInterfaceLayoutOrientation, NSView, NSWindow, NSWindowCollectionBehavior,
};
use objc2_core_foundation::{CFRetained, CGFloat};
use objc2_core_graphics::{CGContext, CGImage};
use objc2_foundation::{MainThreadMarker, NSArray, NSPoint, NSRect, NSSize, NSString};
use shot_core::{history::CaptureKind, Pt, Rect};

use crate::{
    app::{after, app},
    capture::{self, Display, WindowInfo},
    cg, gfx,
    ui::{self, key, Targets, View, ViewDelegate},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Area,
    Window,
    AllInOne,
    /// Area selection that runs OCR instead of saving an image.
    Text,
    /// Area selection, then a countdown, then a live capture.
    SelfTimer,
}

/// What the finished selection should turn into.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Output {
    Capture,
    Copy,
    Save,
    Pin,
    Annotate,
    Text,
}

/// All-In-One controls under the selection.
struct Toolbar {
    bar: Retained<NSStackView>,
    width: Retained<NSTextField>,
    height: Retained<NSTextField>,
}

struct Screen {
    display: Display,
    window: Retained<NSWindow>,
    view: Retained<View>,
    /// The frozen frame and its CGImage, when "freeze screen" is on.
    frozen: Option<(RgbaImage, CFRetained<CGImage>)>,
}

#[derive(Clone, Copy, PartialEq)]
enum Drag {
    New(Pt),
    Move(Pt, Rect),
    /// Resizing from the corner/edge `handle` (0..8, clockwise from top-left).
    Resize(usize, Rect),
}

#[derive(Default)]
struct State {
    cursor: Pt,
    rect: Option<Rect>,
    drag: Option<Drag>,
    hover: Option<usize>,
    aspect: Option<f32>,
}

pub struct Selection {
    mode: Cell<Mode>,
    screens: RefCell<Vec<Screen>>,
    windows: Vec<WindowInfo>,
    state: RefCell<State>,
    toolbar: RefCell<Option<Toolbar>>,
    targets: Targets,
}

const HANDLE: f32 = 8.0;
/// NSPopUpMenuWindowLevel is 101; stay just under it.
const OVERLAY_LEVEL: isize = 100;
const ASPECTS: [(&str, Option<f32>); 7] = [
    ("Freeform", None),
    ("1:1", Some(1.0)),
    ("4:3", Some(4.0 / 3.0)),
    ("3:2", Some(1.5)),
    ("16:9", Some(16.0 / 9.0)),
    ("16:10", Some(1.6)),
    ("9:16", Some(9.0 / 16.0)),
];

pub fn start(mode: Mode) {
    let app = app();
    if app.selection.borrow().is_some() {
        return;
    }
    let mtm = app.mtm;
    let freeze = app.settings.borrow().freeze_screen && mode != Mode::SelfTimer;
    let displays = capture::displays(mtm);
    let frozen: Vec<_> = displays
        .iter()
        .map(|d| {
            freeze.then(|| capture::region(d.bounds)).flatten().map(|img| {
                let cg = cg::image_from_rgba8(&img);
                (img, cg)
            })
        })
        .collect();

    let sel = Rc::new(Selection {
        mode: Cell::new(mode),
        screens: RefCell::default(),
        windows: capture::windows(),
        state: RefCell::new(State {
            rect: (mode == Mode::AllInOne).then(|| app.settings.borrow().last_selection).flatten(),
            ..Default::default()
        }),
        toolbar: RefCell::default(),
        targets: Targets::default(),
    });
    let weak: Weak<dyn ViewDelegate> = Rc::<Selection>::downgrade(&sel);
    let main_h = capture::main_height(mtm);
    let screens = displays
        .into_iter()
        .zip(frozen)
        .map(|(display, frozen)| {
            let frame = ui::cg_to_screen(display.bounds, main_h);
            let window = ui::overlay_window(mtm, frame);
            // Above the menu bar and Dock (and full-screen apps, via the collection
            // behavior) but below pop-up menus, so the All-In-One menus still show.
            window.setLevel(OVERLAY_LEVEL);
            window.setCollectionBehavior(
                NSWindowCollectionBehavior::CanJoinAllSpaces | NSWindowCollectionBehavior::FullScreenAuxiliary,
            );
            window.setAcceptsMouseMovedEvents(true);
            let view = View::new(mtm, NSRect::new(NSPoint::ZERO, frame.size), weak.clone());
            window.setContentView(Some(&view));
            Screen { display, window, view, frozen }
        })
        .collect();
    *sel.screens.borrow_mut() = screens;
    *app.selection.borrow_mut() = Some(sel.clone());

    ui::activate(mtm);
    let mouse = NSEvent::mouseLocation();
    sel.state.borrow_mut().cursor = Pt::new(mouse.x as f32, (main_h - mouse.y) as f32);
    for s in sel.screens.borrow().iter() {
        s.window.makeKeyAndOrderFront(None);
    }
    // The screen under the mouse gets keyboard focus.
    if let Some(s) = sel.screen_at(sel.state.borrow().cursor) {
        let screens = sel.screens.borrow();
        screens[s].window.makeKeyAndOrderFront(None);
        screens[s].window.makeFirstResponder(Some(&screens[s].view));
    }
    sel.update_hover();
    sel.sync_toolbar();
    NSCursor::crosshairCursor().set();
    sel.redraw();
}

impl Selection {
    fn screen_at(&self, p: Pt) -> Option<usize> {
        self.screens.borrow().iter().position(|s| s.display.bounds.contains(p))
    }

    fn screen_of(&self, view: &View) -> Option<usize> {
        self.screens.borrow().iter().position(|s| std::ptr::eq(&*s.view, view))
    }

    /// View point → global CG point.
    fn global(&self, view: &View, p: Pt) -> Pt {
        let i = self.screen_of(view).unwrap_or(0);
        let b = self.screens.borrow()[i].display.bounds;
        Pt::new(b.x + p.x, b.y + p.y)
    }

    fn redraw(&self) {
        for s in self.screens.borrow().iter() {
            s.view.redraw();
        }
    }

    fn update_hover(&self) {
        let mut st = self.state.borrow_mut();
        let c = st.cursor;
        st.hover = self.windows.iter().position(|w| w.bounds.contains(c));
    }

    fn close(&self) {
        for s in self.screens.borrow().iter() {
            s.window.orderOut(None);
        }
        NSCursor::arrowCursor().set();
        // Defer the drop: we're usually inside one of our own views' callbacks.
        after(0.0, || drop(app().selection.borrow_mut().take()));
    }

    fn cancel(&self) {
        self.close();
    }

    /// Crops the frozen frame, or captures live once our windows are gone.
    fn grab(&self, rect: Rect, then: impl FnOnce(RgbaImage) + 'static) {
        let screens = self.screens.borrow();
        // The frozen frame only covers one display; a selection that spans two
        // is captured live instead of being cut at the display edge.
        let inside =
            |b: Rect| rect.x >= b.x && rect.y >= b.y && rect.right() <= b.right() && rect.bottom() <= b.bottom();
        let frozen = screens.iter().find(|s| inside(s.display.bounds)).and_then(|s| {
            let (img, _) = s.frozen.as_ref()?;
            let b = s.display.bounds;
            let k = s.display.scale as f32;
            let px = Rect::new((rect.x - b.x) * k, (rect.y - b.y) * k, rect.w * k, rect.h * k);
            let (x, y, w, h) = px.clamp_px(img.width(), img.height())?;
            Some(image::imageops::crop_imm(img, x, y, w, h).to_image())
        });
        drop(screens);
        self.close();
        match frozen {
            Some(img) => then(img),
            // Let the window server remove our overlay before grabbing the screen.
            None => after(0.15, move || {
                if let Some(img) = capture::region(rect) {
                    then(img)
                }
            }),
        }
    }

    fn finish_area(&self, rect: Rect, output: Output) {
        if rect.w < 2.0 || rect.h < 2.0 {
            return self.cancel();
        }
        {
            let app = app();
            app.settings.borrow_mut().last_selection = Some(rect);
            app.save_settings();
        }
        if self.mode.get() == Mode::SelfTimer {
            self.close();
            return countdown(rect);
        }
        let output = if self.mode.get() == Mode::Text { Output::Text } else { output };
        self.grab(rect, move |img| deliver(img, CaptureKind::Area, output));
    }

    fn finish_window(&self, i: usize) {
        let Some(w) = self.windows.get(i).cloned() else { return };
        self.close();
        let app = app();
        let (shadow, background) = {
            let s = app.settings.borrow();
            (s.window_shadow, s.window_background.clone())
        };
        let Some(mut img) = capture::window(w.id, shadow) else { return };
        if let Some(bg) = background {
            img = shot_core::render::to_image(&bg.apply(&shot_core::render::to_pixmap(&img)));
        }
        deliver(img, CaptureKind::Window, Output::Capture);
    }

    fn handle_at(rect: Rect, p: Pt) -> Option<usize> {
        rect.handles(HANDLE).iter().position(|h| h.inflate(4.0).contains(p))
    }

    // ------------------------------------------------------------ All-In-One toolbar

    fn sync_toolbar(self: &Rc<Self>) {
        if self.mode.get() != Mode::AllInOne {
            return;
        }
        let rect = self.state.borrow().rect;
        let Some(rect) = rect.filter(|_| self.state.borrow().drag.is_none()) else {
            if let Some(t) = self.toolbar.borrow().as_ref() {
                t.bar.setHidden(true);
            }
            return;
        };
        if self.toolbar.borrow().is_none() {
            let bar = self.build_toolbar();
            *self.toolbar.borrow_mut() = Some(bar);
        }
        let toolbar = self.toolbar.borrow();
        let Toolbar { bar, width: wf, height: hf } = toolbar.as_ref().unwrap();
        let Some(si) = self.screen_at(rect.center()).or(self.screen_at(Pt::new(rect.x, rect.y))) else { return };
        let screens = self.screens.borrow();
        let s = &screens[si];
        if unsafe { bar.superview() }.is_none_or(|v| !std::ptr::eq(&*v, &**s.view as &NSView)) {
            bar.removeFromSuperview();
            s.view.addSubview(bar);
        }
        let k = s.display.scale as f32;
        wf.setStringValue(&NSString::from_str(&format!("{}", (rect.w * k).round())));
        hf.setStringValue(&NSString::from_str(&format!("{}", (rect.h * k).round())));
        let size = bar.fittingSize();
        let b = s.display.bounds;
        let local = rect.translate(-b.x, -b.y);
        let mut y = local.bottom() + 12.0;
        if y + size.height as f32 > b.h - 8.0 {
            y = (local.y - size.height as f32 - 12.0).max(8.0);
        }
        let x = (local.center().x - size.width as f32 / 2.0).clamp(8.0, (b.w - size.width as f32 - 8.0).max(8.0));
        bar.setFrame(NSRect::new(NSPoint::new(x as f64, y as f64), size));
        bar.setHidden(false);
    }

    fn build_toolbar(self: &Rc<Self>) -> Toolbar {
        let mtm = MainThreadMarker::new().unwrap();
        let weak = Rc::downgrade(self);
        let act = |out: Output| {
            let weak = weak.clone();
            move || {
                if let Some(s) = weak.upgrade() {
                    let rect = s.state.borrow().rect;
                    if let Some(r) = rect {
                        s.finish_area(r, out);
                    }
                }
            }
        };

        let aspect = NSPopUpButton::initWithFrame_pullsDown(NSPopUpButton::alloc(mtm), NSRect::ZERO, false);
        for (name, _) in ASPECTS {
            aspect.addItemWithTitle(&NSString::from_str(name));
        }
        let w2 = weak.clone();
        let t = self.targets.add(mtm, move |sender| {
            let Some(s) = w2.upgrade() else { return };
            let Some(popup) = ui::sender::<NSPopUpButton>(sender) else { return };
            let ratio = ASPECTS.get(popup.indexOfSelectedItem() as usize).and_then(|a| a.1);
            let mut st = s.state.borrow_mut();
            st.aspect = ratio;
            if let (Some(ratio), Some(r)) = (ratio, st.rect) {
                st.rect = Some(Rect::new(r.x, r.y, r.w, r.w / ratio));
            }
            drop(st);
            s.sync_toolbar();
            s.redraw();
        });
        unsafe {
            aspect.setTarget(Some(&t));
            aspect.setAction(Some(sel!(fire:)));
        }

        let field = |placeholder: &str| {
            let f = NSTextField::textFieldWithString(&NSString::from_str(""), mtm);
            f.setPlaceholderString(Some(&NSString::from_str(placeholder)));
            f.setFont(Some(&NSFont::monospacedDigitSystemFontOfSize_weight(12.0, 0.0)));
            ui::set_width(&f, 64.0);
            f
        };
        let (wf, hf) = (field("W"), field("H"));
        for f in [&wf, &hf] {
            let w3 = weak.clone();
            let t = self.targets.add(mtm, move |_| {
                if let Some(s) = w3.upgrade() {
                    s.apply_size_fields();
                }
            });
            unsafe {
                f.setTarget(Some(&t));
                f.setAction(Some(sel!(fire:)));
            }
        }

        let views: Vec<Retained<NSView>> = vec![
            Retained::into_super(Retained::into_super(Retained::into_super(aspect))),
            Retained::into_super(Retained::into_super(wf.clone())),
            Retained::into_super(Retained::into_super(NSTextField::labelWithString(&NSString::from_str("×"), mtm))),
            Retained::into_super(Retained::into_super(hf.clone())),
            Retained::into_super(Retained::into_super(ui::icon_button(
                mtm,
                &self.targets,
                "text.viewfinder",
                "Capture Text",
                act(Output::Text),
            ))),
            Retained::into_super(Retained::into_super(ui::icon_button(
                mtm,
                &self.targets,
                "pin",
                "Pin",
                act(Output::Pin),
            ))),
            Retained::into_super(Retained::into_super(ui::icon_button(
                mtm,
                &self.targets,
                "pencil.tip.crop.circle",
                "Annotate",
                act(Output::Annotate),
            ))),
            Retained::into_super(Retained::into_super(ui::icon_button(
                mtm,
                &self.targets,
                "square.and.arrow.down",
                "Save",
                act(Output::Save),
            ))),
            Retained::into_super(Retained::into_super(ui::icon_button(
                mtm,
                &self.targets,
                "doc.on.doc",
                "Copy",
                act(Output::Copy),
            ))),
            Retained::into_super(Retained::into_super(ui::text_button(
                mtm,
                &self.targets,
                "Capture ⏎",
                act(Output::Capture),
            ))),
        ];
        let bar = NSStackView::stackViewWithViews(&NSArray::from_retained_slice(&views), mtm);
        bar.setOrientation(NSUserInterfaceLayoutOrientation::Horizontal);
        bar.setSpacing(6.0);
        bar.setEdgeInsets(objc2_foundation::NSEdgeInsets { top: 6.0, left: 10.0, bottom: 6.0, right: 10.0 });
        bar.setWantsLayer(true);
        if let Some(layer) = bar.layer() {
            layer.setCornerRadius(10.0);
            let bg = gfx::ns_color((0.12, 0.12, 0.14, 0.92));
            layer.setBackgroundColor(Some(&bg.CGColor()));
        }
        objc2_app_kit::NSAppearanceCustomization::setAppearance(
            &*bar,
            objc2_app_kit::NSAppearance::appearanceNamed(unsafe { objc2_app_kit::NSAppearanceNameDarkAqua }).as_deref(),
        );
        Toolbar { bar, width: wf, height: hf }
    }

    /// Applies the W×H fields (in pixels) to the current selection.
    fn apply_size_fields(self: &Rc<Self>) {
        let toolbar = self.toolbar.borrow();
        let Some(t) = toolbar.as_ref() else { return };
        let (w, h) = (t.width.doubleValue() as f32, t.height.doubleValue() as f32);
        drop(toolbar);
        let mut st = self.state.borrow_mut();
        let Some(r) = st.rect else { return };
        let k = self.screen_at(r.center()).map_or(1.0, |i| self.screens.borrow()[i].display.scale as f32);
        if w > 0.0 && h > 0.0 {
            st.rect = Some(Rect::new(r.x, r.y, w / k, h / k));
        }
        drop(st);
        self.sync_toolbar();
        self.redraw();
    }

    // ------------------------------------------------------------ drawing

    fn draw_screen(&self, s: &Screen, c: &CGContext) {
        let b = s.display.bounds;
        let local = Rect::new(0.0, 0.0, b.w, b.h);
        if let Some((_, img)) = &s.frozen {
            gfx::image(c, img, local, false);
        }
        let st = self.state.borrow();
        let settings = app().settings.borrow().clone();
        let dim = (0.0, 0.0, 0.0, 0.35);
        let cursor_here = b.contains(st.cursor);
        let cur = Pt::new(st.cursor.x - b.x, st.cursor.y - b.y);

        if self.mode.get() == Mode::Window {
            let hover = st.hover.map(|i| self.windows[i].bounds.translate(-b.x, -b.y));
            gfx::dim_except(c, local, hover, dim);
            if let Some(h) = hover {
                gfx::fill(c, h, (0.0, 0.48, 1.0, 0.18));
                gfx::stroke(c, h, 3.0, gfx::ACCENT);
                let w = &self.windows[st.hover.unwrap()];
                let label = if w.title.is_empty() { w.owner.clone() } else { format!("{} — {}", w.owner, w.title) };
                let label: String = label.chars().take(60).collect();
                gfx::pill(&label, h.x + 8.0, h.y + 8.0, 12.0);
            }
            hint(local, "Click a window to capture • Space: area mode • Esc: cancel");
            return;
        }

        let sel = st.rect.map(|r| r.translate(-b.x, -b.y));
        gfx::dim_except(c, local, sel, dim);
        if let Some(r) = sel.filter(|r| r.intersect(&local).is_some()) {
            gfx::stroke(c, r.inflate(0.5), 1.0, gfx::WHITE);
            let k = s.display.scale as f32;
            let label = format!("{} × {}", (r.w * k).round(), (r.h * k).round());
            if st.drag.is_some() || self.mode.get() != Mode::AllInOne {
                gfx::pill(&label, r.x, (r.y - 26.0).max(4.0), 11.0);
            }
            if self.mode.get() == Mode::AllInOne && st.drag.is_none() {
                for h in r.handles(HANDLE) {
                    gfx::fill_ellipse(c, h, gfx::WHITE);
                    gfx::stroke_ellipse(c, h, 1.0, (0.0, 0.0, 0.0, 0.4));
                }
            }
        }

        if cursor_here && st.drag.is_none_or(|d| matches!(d, Drag::New(_))) {
            if settings.show_crosshair {
                let col = (1.0, 1.0, 1.0, 0.55);
                gfx::line(c, 0.0, cur.y, b.w, cur.y, 1.0, col);
                gfx::line(c, cur.x, 0.0, cur.x, b.h, 1.0, col);
            }
            if settings.show_magnifier {
                if let Some((img, cgimg)) = &s.frozen {
                    magnifier(c, img, cgimg, cur, s.display.scale as f32, local);
                }
            }
        }
        if cursor_here && st.rect.is_none() {
            let text = match self.mode.get() {
                Mode::Text => "Drag over text to copy it • Esc: cancel",
                Mode::SelfTimer => "Drag the area for the self-timer • Esc: cancel",
                Mode::AllInOne => "Drag to select • Enter: capture • Esc: cancel",
                _ => "Drag to capture • Click: window • Space: window mode • Esc: cancel",
            };
            hint(local, text);
        }
    }
}

impl ViewDelegate for Selection {
    fn draw(&self, view: &View, cg: &CGContext) {
        if let Some(i) = self.screen_of(view) {
            let screens = self.screens.borrow();
            self.draw_screen(&screens[i], cg);
        }
    }

    fn mouse_moved(&self, view: &View, p: Pt) {
        self.state.borrow_mut().cursor = self.global(view, p);
        self.update_hover();
        self.redraw();
    }

    fn mouse_down(&self, view: &View, p: Pt, _ev: &NSEvent) {
        let g = self.global(view, p);
        if self.mode.get() == Mode::Window {
            return;
        }
        let mut st = self.state.borrow_mut();
        st.cursor = g;
        st.drag = Some(match st.rect.filter(|_| self.mode.get() == Mode::AllInOne) {
            Some(r) => match Self::handle_at(r, g) {
                Some(h) => Drag::Resize(h, r),
                None if r.contains(g) => Drag::Move(g, r),
                None => Drag::New(g),
            },
            None => Drag::New(g),
        });
        if matches!(st.drag, Some(Drag::New(_))) {
            st.rect = None;
        }
        drop(st);
        if let Some(t) = self.toolbar.borrow().as_ref() {
            t.bar.setHidden(true);
        }
        self.redraw();
    }

    fn mouse_dragged(&self, view: &View, p: Pt, ev: &NSEvent) {
        let g = self.global(view, p);
        let mut st = self.state.borrow_mut();
        st.cursor = g;
        let square = ui::has_mod(ev, NSEventModifierFlags::Shift).then_some(1.0);
        let aspect = square.or(st.aspect);
        st.rect = match st.drag {
            Some(Drag::New(start)) => {
                // Keep the selection on the display where it started.
                let screen =
                    self.screens.borrow().iter().find(|s| s.display.bounds.contains(start)).map(|s| s.display.bounds);
                let end = screen.map_or(g, |b| Pt::new(g.x.clamp(b.x, b.right()), g.y.clamp(b.y, b.bottom())));
                Some(Rect::with_aspect(start, end, aspect))
            }
            Some(Drag::Move(start, r)) => Some(r.translate(g.x - start.x, g.y - start.y)),
            Some(Drag::Resize(h, r)) => Some(r.resize_handle(h, g, aspect)),
            None => st.rect,
        };
        drop(st);
        self.redraw();
    }

    fn mouse_up(&self, view: &View, p: Pt, _ev: &NSEvent) {
        let g = self.global(view, p);
        let mode = self.mode.get();
        if mode == Mode::Window {
            let hover = self.state.borrow().hover;
            if let Some(i) = hover {
                self.finish_window(i);
            }
            return;
        }
        let (drag, rect) = {
            let mut st = self.state.borrow_mut();
            (st.drag.take(), st.rect)
        };
        let tiny = rect.is_none_or(|r| r.w < 4.0 || r.h < 4.0);
        if tiny && matches!(drag, Some(Drag::New(_))) {
            // A plain click: capture the window under the cursor.
            if mode == Mode::Area {
                if let Some(i) = self.windows.iter().position(|w| w.bounds.contains(g)) {
                    return self.finish_window(i);
                }
            }
            self.state.borrow_mut().rect = None;
            self.redraw();
            return;
        }
        match (mode, rect) {
            (Mode::AllInOne, _) => {
                // Keep the selection adjustable until the user confirms.
                if let Some(rc) = app().selection.borrow().clone() {
                    rc.sync_toolbar();
                }
                self.redraw();
            }
            (_, Some(r)) => self.finish_area(r, Output::Capture),
            _ => {}
        }
    }

    fn key_down(&self, _view: &View, ev: &NSEvent) -> bool {
        match ev.keyCode() {
            key::ESCAPE => self.cancel(),
            key::SPACE if matches!(self.mode.get(), Mode::Area | Mode::Window) => {
                self.mode.set(if self.mode.get() == Mode::Window { Mode::Area } else { Mode::Window });
                self.redraw();
            }
            key::RETURN | key::ENTER => {
                let rect = self.state.borrow().rect;
                if let Some(r) = rect {
                    self.finish_area(r, Output::Capture);
                }
            }
            k @ (key::LEFT | key::RIGHT | key::UP | key::DOWN) => {
                let step = if ui::has_mod(ev, NSEventModifierFlags::Shift) { 10.0 } else { 1.0 };
                let (dx, dy) = match k {
                    key::LEFT => (-step, 0.0),
                    key::RIGHT => (step, 0.0),
                    key::UP => (0.0, -step),
                    _ => (0.0, step),
                };
                let mut st = self.state.borrow_mut();
                st.rect = st.rect.map(|r| r.translate(dx, dy));
                drop(st);
                if let Some(rc) = app().selection.borrow().clone() {
                    rc.sync_toolbar();
                }
                self.redraw();
            }
            _ => return false,
        }
        true
    }
}

/// Sends a finished capture to wherever the user asked.
fn deliver(img: RgbaImage, kind: CaptureKind, output: Output) {
    let app = app();
    match output {
        Output::Capture => app.captured(img, kind),
        Output::Copy => {
            ui::copy_image(&img);
            crate::app::toast("Copied to clipboard");
        }
        Output::Save => {
            app.save(&img);
        }
        Output::Pin => crate::pin::show(Arc::new(img), None),
        Output::Annotate => crate::editor::open(shot_core::Document::new(img)),
        Output::Text => app.ocr(&img),
    }
}

fn hint(bounds: Rect, text: &str) {
    let s = gfx::text_size(text, 12.0, true);
    gfx::pill(text, bounds.center().x - s.width as f32 / 2.0 - 7.0, bounds.bottom() - 64.0, 12.0);
}

/// 8× zoom of the pixels around the cursor, with the color under it.
fn magnifier(c: &CGContext, img: &RgbaImage, cgimg: &CGImage, cur: Pt, scale: f32, bounds: Rect) {
    const CELLS: f32 = 15.0;
    const SIZE: f32 = 120.0;
    let (px, py) = ((cur.x * scale) as i64, (cur.y * scale) as i64);
    let half = (CELLS / 2.0) as i64;
    // Near an edge the source square is partly outside the image; draw only
    // the part that exists, in its true cells, so the center stays on the pointer.
    let want = Rect::new((px - half) as f32, (py - half) as f32, CELLS, CELLS);
    let full = Rect::new(0.0, 0.0, CGImage::width(Some(cgimg)) as f32, CGImage::height(Some(cgimg)) as f32);
    let Some(have) = want.intersect(&full) else { return };
    let src = cg::rect(have.x as f64, have.y as f64, have.w as f64, have.h as f64);
    let Some(sub) = CGImage::with_image_in_rect(Some(cgimg), src) else { return };

    let mut x = cur.x + 24.0;
    let mut y = cur.y + 24.0;
    if x + SIZE > bounds.right() {
        x = cur.x - 24.0 - SIZE;
    }
    if y + SIZE + 28.0 > bounds.bottom() {
        y = cur.y - 24.0 - SIZE - 28.0;
    }
    let frame = Rect::new(x, y, SIZE, SIZE);
    gfx::fill(c, frame.inflate(1.0), gfx::BLACK);
    let cell = SIZE / CELLS;
    let dest = Rect::new(x + (have.x - want.x) * cell, y + (have.y - want.y) * cell, have.w * cell, have.h * cell);
    gfx::image(c, &sub, dest, false);
    let center = Rect::new(x + cell * half as f32, y + cell * half as f32, cell, cell);
    gfx::stroke(c, center, 1.0, gfx::WHITE);
    gfx::stroke(c, frame, 2.0, gfx::WHITE);

    let color = img.get_pixel_checked(px.max(0) as u32, py.max(0) as u32).map(|p| p.0).unwrap_or_default();
    let label = format!("{}, {}  #{:02X}{:02X}{:02X}", px, py, color[0], color[1], color[2]);
    gfx::pill(&label, x, y + SIZE + 6.0, 10.0);
}

/// Self-timer: count down over the selection, then capture it live.
fn countdown(rect: Rect) {
    let mtm = MainThreadMarker::new().unwrap();
    let secs = app().settings.borrow().self_timer_secs.max(1);
    let main_h = capture::main_height(mtm);
    let size = 120.0;
    let c = rect.center();
    let frame = ui::cg_to_screen(Rect::new(c.x - size / 2.0, c.y - size / 2.0, size, size), main_h);
    let win = ui::overlay_window(mtm, frame);
    win.setLevel(OVERLAY_LEVEL);
    win.setIgnoresMouseEvents(true);
    let label = NSTextField::labelWithString(&NSString::from_str(""), mtm);
    label.setFont(Some(&NSFont::monospacedDigitSystemFontOfSize_weight(64.0, 0.3)));
    label.setTextColor(Some(&gfx::ns_color(gfx::WHITE)));
    label.setAlignment(objc2_app_kit::NSTextAlignment::Center);
    label.setFrame(NSRect::new(NSPoint::new(0.0, 22.0), NSSize::new(size as CGFloat, 80.0)));
    let bg = NSView::initWithFrame(NSView::alloc(mtm), NSRect::new(NSPoint::ZERO, frame.size));
    bg.setWantsLayer(true);
    if let Some(layer) = bg.layer() {
        layer.setCornerRadius(size as f64 / 2.0);
        layer.setBackgroundColor(Some(&gfx::ns_color((0.0, 0.0, 0.0, 0.6)).CGColor()));
    }
    bg.addSubview(&label);
    win.setContentView(Some(&bg));
    win.orderFrontRegardless();

    fn tick(win: Retained<NSWindow>, label: Retained<NSTextField>, left: u32, rect: Rect) {
        if left == 0 {
            win.orderOut(None);
            after(0.15, move || {
                if let Some(img) = capture::region(rect) {
                    app().captured(img, CaptureKind::Area);
                }
            });
            return;
        }
        label.setStringValue(&NSString::from_str(&left.to_string()));
        crate::app::play_sound("Tink");
        after(1.0, move || tick(win, label, left - 1, rect));
    }
    tick(win, label, secs, rect);
}
