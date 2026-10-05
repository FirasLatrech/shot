//! Capture History: a filterable grid of recent captures that can be
//! restored, annotated, pinned, dragged out or deleted.

use std::{
    cell::{Cell, RefCell},
    collections::HashMap,
    rc::{Rc, Weak},
    sync::Arc,
};

use objc2::{rc::Retained, sel, MainThreadOnly};
use objc2_app_kit::{NSAutoresizingMaskOptions, NSEvent, NSMenu, NSPopUpButton, NSScrollView, NSTextField, NSWindow};
use objc2_core_foundation::CFRetained;
use objc2_core_graphics::{CGContext, CGImage};
use objc2_foundation::{MainThreadMarker, NSDate, NSDateFormatter, NSPoint, NSRect, NSSize, NSString};
use shot_core::{
    history::{CaptureKind, Entry},
    Document, Pt, Rect,
};

use crate::{
    app::{after, app, Capture},
    cg, gfx,
    ui::{self, key, Targets, View, ViewDelegate, WindowObserver},
};

const CELL_W: f32 = 200.0;
const CELL_H: f32 = 160.0;
const THUMB_H: f32 = 120.0;
const PAD: f32 = 14.0;
const FILTERS: [(&str, Option<CaptureKind>); 4] = [
    ("All Captures", None),
    ("Area", Some(CaptureKind::Area)),
    ("Window", Some(CaptureKind::Window)),
    ("Fullscreen", Some(CaptureKind::Fullscreen)),
];

pub struct HistoryWindow {
    window: Retained<NSWindow>,
    scroll: Retained<NSScrollView>,
    grid: Retained<View>,
    count: Retained<NSTextField>,
    entries: RefCell<Vec<Entry>>,
    filter: Cell<Option<CaptureKind>>,
    selected: Cell<Option<usize>>,
    thumbs: RefCell<HashMap<String, CFRetained<CGImage>>>,
    loading: RefCell<std::collections::HashSet<String>>,
    press: Cell<Option<Pt>>,
    observer: RefCell<Option<Retained<WindowObserver>>>,
    targets: Targets,
    /// Targets of the most recent popup menu; replaced by the next one.
    menu_targets: RefCell<Targets>,
}

thread_local! {
    static OPEN: RefCell<Option<Rc<HistoryWindow>>> = const { RefCell::new(None) };
}

pub fn show() {
    if let Some(w) = OPEN.with(|o| o.borrow().clone()) {
        w.reload();
        ui::activate(w.window.mtm());
        return w.window.makeKeyAndOrderFront(None);
    }
    let mtm = MainThreadMarker::new().unwrap();
    let window = ui::app_window(mtm, "Capture History", 900.0, 600.0);
    let content = window.contentView().unwrap();
    let b = content.bounds();
    let bar_h = 44.0;

    let hw = Rc::new_cyclic(|weak: &Weak<HistoryWindow>| {
        let delegate: Weak<dyn ViewDelegate> = weak.clone();
        let scroll = NSScrollView::initWithFrame(
            NSScrollView::alloc(mtm),
            NSRect::new(NSPoint::ZERO, NSSize::new(b.size.width, b.size.height - bar_h)),
        );
        scroll.setHasVerticalScroller(true);
        scroll.setAutohidesScrollers(true);
        scroll.setAutoresizingMask(
            NSAutoresizingMaskOptions::ViewWidthSizable | NSAutoresizingMaskOptions::ViewHeightSizable,
        );
        let grid = View::new(mtm, NSRect::new(NSPoint::ZERO, NSSize::new(b.size.width, 10.0)), delegate);
        grid.setAutoresizingMask(NSAutoresizingMaskOptions::ViewWidthSizable);
        scroll.setDocumentView(Some(&grid));
        HistoryWindow {
            window,
            scroll,
            grid,
            count: ui::label(""),
            entries: RefCell::default(),
            filter: Cell::new(None),
            selected: Cell::new(None),
            thumbs: RefCell::default(),
            loading: RefCell::default(),
            press: Cell::new(None),
            observer: RefCell::default(),
            targets: Targets::default(),
            menu_targets: RefCell::default(),
        }
    });

    let filter = NSPopUpButton::initWithFrame_pullsDown(NSPopUpButton::alloc(mtm), NSRect::ZERO, false);
    for (name, _) in FILTERS {
        filter.addItemWithTitle(&NSString::from_str(name));
    }
    let me = Rc::downgrade(&hw);
    let t = hw.targets.add(mtm, move |sender| {
        let Some(hw) = me.upgrade() else { return };
        let Some(p) = ui::sender::<NSPopUpButton>(sender) else { return };
        hw.filter.set(FILTERS.get(p.indexOfSelectedItem().max(0) as usize).and_then(|f| f.1));
        hw.reload();
    });
    unsafe {
        filter.setTarget(Some(&t));
        filter.setAction(Some(sel!(fire:)));
    }
    let me = Rc::downgrade(&hw);
    let clear = ui::text_button(mtm, &hw.targets, "Clear All…", move || {
        let Some(hw) = me.upgrade() else { return };
        let alert = objc2_app_kit::NSAlert::new(MainThreadMarker::new().unwrap());
        alert.setMessageText(&NSString::from_str("Delete all captures from history?"));
        alert.addButtonWithTitle(&NSString::from_str("Delete All"));
        alert.addButtonWithTitle(&NSString::from_str("Cancel"));
        if alert.runModal() == 1000 {
            let _ = app().history.borrow_mut().clear();
            hw.thumbs.borrow_mut().clear();
            hw.reload();
        }
    });
    let folder = ui::text_button(mtm, &hw.targets, "Show in Finder", || {
        if let Some(e) = app().history.borrow().entries().next() {
            ui::reveal_in_finder(&app().history.borrow().path(e));
        }
    });
    let keep = ui::label(&format!("Captures are kept for {} days", app().settings.borrow().history_days));
    keep.setTextColor(Some(&objc2_app_kit::NSColor::secondaryLabelColor()));
    hw.count.setTextColor(Some(&objc2_app_kit::NSColor::secondaryLabelColor()));
    let bar = ui::bar(
        &[
            ui::view(&filter),
            ui::view(&hw.count),
            ui::flex_space(),
            ui::view(&keep),
            ui::view(&folder),
            ui::view(&clear),
        ],
        10.0,
        objc2_foundation::NSEdgeInsets { top: 0.0, left: 14.0, bottom: 0.0, right: 14.0 },
        false,
    );
    content.addSubview(&hw.scroll);
    ui::pin_bar(&bar, &content, true, bar_h);

    *hw.observer.borrow_mut() = Some(ui::on_close(&hw.window, || {
        after(0.0, || OPEN.with(|o| o.borrow_mut().take()).map_or((), drop));
    }));
    OPEN.with(|o| *o.borrow_mut() = Some(hw.clone()));
    hw.reload();
    ui::activate(mtm);
    hw.window.makeKeyAndOrderFront(None);
    hw.window.makeFirstResponder(Some(&hw.grid));
}

impl HistoryWindow {
    fn reload(&self) {
        let filter = self.filter.get();
        let entries: Vec<Entry> =
            app().history.borrow().entries().filter(|e| filter.is_none_or(|k| e.kind == k)).cloned().collect();
        self.count.setStringValue(&NSString::from_str(&format!("{} captures", entries.len())));
        *self.entries.borrow_mut() = entries;
        self.selected.set(None);
        self.relayout();
    }

    fn columns(&self) -> usize {
        let w = self.scroll.contentView().bounds().size.width as f32;
        (((w - PAD) / (CELL_W + PAD)) as usize).max(1)
    }

    fn relayout(&self) {
        let rows = self.entries.borrow().len().div_ceil(self.columns());
        let h = (rows as f32 * (CELL_H + PAD) + PAD) as f64;
        let w = self.scroll.contentView().bounds().size.width;
        let min_h = self.scroll.contentView().bounds().size.height;
        self.grid.setFrameSize(NSSize::new(w, h.max(min_h)));
        self.grid.redraw();
    }

    fn cell(&self, i: usize) -> Rect {
        let cols = self.columns();
        let w = self.grid.bounds().size.width as f32;
        // Center the grid horizontally.
        let x0 = (w - cols as f32 * (CELL_W + PAD) + PAD) / 2.0;
        Rect::new(x0 + (i % cols) as f32 * (CELL_W + PAD), PAD + (i / cols) as f32 * (CELL_H + PAD), CELL_W, CELL_H)
    }

    fn index_at(&self, p: Pt) -> Option<usize> {
        (0..self.entries.borrow().len()).find(|&i| self.cell(i).contains(p))
    }

    /// The cached thumbnail, or `None` while it's decoded in the background
    /// (full-size PNG decodes would stall drawing and scrolling).
    fn thumb(&self, e: &Entry) -> Option<CFRetained<CGImage>> {
        if let Some(t) = self.thumbs.borrow().get(&e.id) {
            return Some(t.clone());
        }
        if !self.loading.borrow_mut().insert(e.id.clone()) {
            return None;
        }
        let (id, path) = (e.id.clone(), app().history.borrow().path(e));
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let thumb = image::open(path).ok().map(|i| i.thumbnail(CELL_W as u32 * 2, THUMB_H as u32 * 2).into_rgba8());
            let _ = tx.send(thumb);
        });
        crate::app::poll(rx, move |thumb| {
            let Some(hw) = OPEN.with(|o| o.borrow().clone()) else { return };
            hw.loading.borrow_mut().remove(&id);
            if let Some(img) = thumb {
                let mut thumbs = hw.thumbs.borrow_mut();
                // Thumbnails are cheap to rebuild; keep memory bounded.
                if thumbs.len() > 400 {
                    thumbs.clear();
                }
                thumbs.insert(id, cg::image_from_rgba8(&img));
            }
            hw.grid.redraw();
        });
        None
    }

    fn capture(&self, i: usize) -> Option<Capture> {
        let e = self.entries.borrow().get(i)?.clone();
        let path = app().history.borrow().path(&e);
        let img = image::open(&path).ok()?.into_rgba8();
        Some(Capture { image: Arc::new(img), file: Some(path) })
    }

    fn delete(&self, i: usize) {
        let Some(e) = self.entries.borrow().get(i).cloned() else { return };
        let _ = app().history.borrow_mut().remove(&e.id);
        self.thumbs.borrow_mut().remove(&e.id);
        self.reload();
    }

    fn menu_for(&self, i: usize) -> Retained<NSMenu> {
        let mtm = MainThreadMarker::new().unwrap();
        let me = OPEN.with(|o| o.borrow().as_ref().map(Rc::downgrade)).unwrap_or_default();
        let act = |f: fn(&HistoryWindow, usize)| -> Box<dyn Fn()> {
            let me = me.clone();
            Box::new(move || {
                if let Some(hw) = me.upgrade() {
                    f(&hw, i)
                }
            })
        };
        let targets = Targets::default();
        let menu = ui::menu(
            mtm,
            &targets,
            vec![
                ("Open in Quick Access", act(|hw, i| hw.capture(i).map_or((), crate::overlay::show))),
                (
                    "Annotate",
                    act(|hw, i| {
                        if let Some(c) = hw.capture(i) {
                            crate::editor::open(Document::new((*c.image).clone()));
                        }
                    }),
                ),
                (
                    "Copy",
                    act(|hw, i| {
                        if let Some(c) = hw.capture(i) {
                            ui::copy_image(&c.image);
                            crate::app::toast("Copied to clipboard");
                        }
                    }),
                ),
                ("Pin to Screen", act(|hw, i| hw.capture(i).map_or((), |c| crate::pin::show(c.image, None)))),
                ("Copy Text (OCR)", act(|hw, i| hw.capture(i).map_or((), |c| app().ocr(&c.image)))),
                (
                    "Show in Finder",
                    act(|hw, i| hw.capture(i).and_then(|c| c.file).map_or((), |f| ui::reveal_in_finder(&f))),
                ),
                ("-", Box::new(|| {})),
                ("Delete", act(|hw, i| hw.delete(i))),
            ],
        );
        *self.menu_targets.borrow_mut() = targets;
        menu
    }
}

fn date_label(secs: u64) -> String {
    let f = NSDateFormatter::new();
    f.setDateStyle(objc2_foundation::NSDateFormatterStyle::MediumStyle);
    f.setTimeStyle(objc2_foundation::NSDateFormatterStyle::ShortStyle);
    f.setDoesRelativeDateFormatting(true);
    f.stringFromDate(&NSDate::dateWithTimeIntervalSince1970(secs as f64)).to_string()
}

impl ViewDelegate for HistoryWindow {
    fn draw(&self, view: &View, c: &CGContext) {
        let visible = view.visibleRect();
        let vis = Rect::new(
            visible.origin.x as f32,
            visible.origin.y as f32,
            visible.size.width as f32,
            visible.size.height as f32,
        );
        let entries = self.entries.borrow().clone();
        if entries.is_empty() {
            let msg = "No captures yet — they'll show up here.";
            let s = gfx::text_size(msg, 14.0, false);
            gfx::text(
                msg,
                (vis.w - s.width as f32) / 2.0,
                vis.y + vis.h / 2.0 - 10.0,
                14.0,
                false,
                (0.5, 0.5, 0.5, 1.0),
            );
            return;
        }
        // Width changed since the last layout: fix the height on the next tick.
        let rows = entries.len().div_ceil(self.columns());
        let want = (rows as f32 * (CELL_H + PAD) + PAD).max(self.scroll.contentView().bounds().size.height as f32);
        if (view.bounds().size.height as f32 - want).abs() > 1.0 {
            let me = OPEN.with(|o| o.borrow().as_ref().map(Rc::downgrade)).unwrap_or_default();
            after(0.0, move || me.upgrade().map_or((), |hw| hw.relayout()));
        }
        for (i, e) in entries.iter().enumerate() {
            let cell = self.cell(i);
            if cell.intersect(&vis).is_none() {
                continue;
            }
            let selected = self.selected.get() == Some(i);
            let thumb_box = Rect::new(cell.x, cell.y, cell.w, THUMB_H);
            gfx::fill_rounded_ns(thumb_box, 10.0, &objc2_app_kit::NSColor::quaternarySystemFillColor());
            if let Some(t) = self.thumb(e) {
                let (tw, th) = (CGImage::width(Some(&t)) as f32, CGImage::height(Some(&t)) as f32);
                let inner = thumb_box.inflate(-8.0);
                let k = (inner.w / tw).min(inner.h / th);
                let (w, h) = (tw * k, th * k);
                let r = Rect::new(inner.x + (inner.w - w) / 2.0, inner.y + (inner.h - h) / 2.0, w, h);
                gfx::with_shadow(c, 6.0, 0.2, || gfx::image(c, &t, r, true));
            }
            if selected {
                let ring = objc2_app_kit::NSBezierPath::bezierPathWithRoundedRect_xRadius_yRadius(
                    ui::ns_rect(thumb_box.inflate(1.5)),
                    11.5,
                    11.5,
                );
                ring.setLineWidth(3.0);
                objc2_app_kit::NSColor::controlAccentColor().setStroke();
                ring.stroke();
            }
            let icon = match e.kind {
                CaptureKind::Area => "crop",
                CaptureKind::Window => "macwindow",
                CaptureKind::Fullscreen => "display",
                CaptureKind::Scrolling => "arrow.up.and.down.text.horizontal",
            };
            let secondary = (0.55, 0.55, 0.6, 1.0);
            gfx::symbol(icon, Rect::new(cell.x, cell.y + THUMB_H + 6.0, 14.0, 14.0), 10.0, secondary);
            let label = format!("{}  ·  {} × {}", e.kind.label(), e.width, e.height);
            gfx::text_in(
                &label,
                cell.x + 18.0,
                cell.y + THUMB_H + 5.0,
                11.0,
                true,
                &objc2_app_kit::NSColor::labelColor(),
            );
            gfx::text(&date_label(e.created), cell.x + 18.0, cell.y + THUMB_H + 21.0, 10.0, false, secondary);
        }
    }

    fn mouse_down(&self, view: &View, p: Pt, ev: &NSEvent) {
        let i = self.index_at(p);
        self.selected.set(i);
        self.press.set(Some(p));
        view.redraw();
        if let (Some(i), 2) = (i, ev.clickCount()) {
            if let Some(c) = self.capture(i) {
                crate::overlay::show(c);
            }
        }
    }

    fn mouse_dragged(&self, view: &View, p: Pt, ev: &NSEvent) {
        let Some(start) = self.press.get().filter(|s| s.dist(p) > 4.0) else { return };
        self.press.set(None);
        let Some(i) = self.index_at(start) else { return };
        if let Some(c) = self.capture(i) {
            if let Some(file) = &c.file {
                view.drag_file(file, &ui::nsimage(&c.image), ui::ns_rect(self.cell(i)), ev);
            }
        }
    }

    fn right_mouse_down(&self, view: &View, p: Pt, ev: &NSEvent) {
        if let Some(i) = self.index_at(p) {
            self.selected.set(Some(i));
            view.redraw();
            NSMenu::popUpContextMenu_withEvent_forView(&self.menu_for(i), ev, view);
        }
    }

    fn key_down(&self, _view: &View, ev: &NSEvent) -> bool {
        match (ev.keyCode(), self.selected.get()) {
            (key::DELETE | key::FORWARD_DELETE, Some(i)) => self.delete(i),
            (key::RETURN | key::ENTER, Some(i)) => self.capture(i).map_or((), crate::overlay::show),
            _ => return false,
        }
        true
    }
}
