//! The Annotate window: toolbar, canvas, background sidebar and export bar.

use std::{
    cell::{Cell, RefCell},
    rc::{Rc, Weak},
    sync::Arc,
};

use image::RgbaImage;
use objc2::{rc::Retained, sel, AnyThread, MainThreadOnly};
use objc2_app_kit::{
    NSAutoresizingMaskOptions, NSBezelStyle, NSButton, NSButtonType, NSColorPanel, NSColorSpace, NSEvent,
    NSEventModifierFlags, NSFont, NSImage, NSPopUpButton, NSSegmentSwitchTracking, NSSegmentedControl,
    NSSharingServicePicker, NSTextField, NSView, NSWindow, NSWindowStyleMask, NSWindowTitleVisibility,
};
use objc2_core_foundation::CFRetained;
use objc2_core_graphics::{CGContext, CGImage};
use objc2_foundation::{MainThreadMarker, NSArray, NSPoint, NSRect, NSRectEdge, NSSize, NSString, NSURL};
use shot_core::{
    annot::{self, ArrowStyle, Shape, TextStyle},
    doc::{Layer, UndoStack, PROJECT_EXT},
    geom::PALETTE,
    render::{self, draw_annotation, render_canvas},
    text, Annotation, Background, Color, Document, Pt, Rect,
};
use tiny_skia::Pixmap;

use crate::{
    app::app,
    background_panel, cg, gfx,
    ui::{self, key, Targets, View, ViewDelegate, WindowObserver},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tool {
    Select,
    Crop,
    Rect,
    FilledRect,
    Ellipse,
    Line,
    Arrow,
    Text,
    Pencil,
    Highlighter,
    Counter,
    Blur,
    Pixelate,
    Spotlight,
}

/// Tool, SF Symbol, tooltip, keyboard shortcut.
const TOOLS: [(Tool, &str, &str, char); 14] = [
    (Tool::Select, "cursorarrow", "Select & Move (V)", 'v'),
    (Tool::Crop, "crop", "Crop (C)", 'c'),
    (Tool::Rect, "rectangle", "Rectangle (R)", 'r'),
    (Tool::FilledRect, "rectangle.fill", "Filled Rectangle (F)", 'f'),
    (Tool::Ellipse, "circle", "Ellipse (O)", 'o'),
    (Tool::Line, "line.diagonal", "Line (L)", 'l'),
    (Tool::Arrow, "arrow.up.right", "Arrow (A)", 'a'),
    (Tool::Text, "textformat", "Text (T)", 't'),
    (Tool::Pencil, "pencil", "Pencil (P)", 'p'),
    (Tool::Highlighter, "highlighter", "Highlighter (H)", 'h'),
    (Tool::Counter, "1.circle", "Counter (N)", 'n'),
    (Tool::Blur, "drop", "Blur (B)", 'b'),
    (Tool::Pixelate, "square.grid.3x3", "Pixelate (X)", 'x'),
    (Tool::Spotlight, "flashlight.on.fill", "Spotlight (S)", 's'),
];

const ARROW_STYLES: [(&str, ArrowStyle); 4] = [
    ("Standard", ArrowStyle::Standard),
    ("Thin", ArrowStyle::Thin),
    ("Double", ArrowStyle::Double),
    ("Curved", ArrowStyle::Curved),
];
const TEXT_SIZES: [f32; 8] = [14.0, 18.0, 24.0, 32.0, 40.0, 56.0, 72.0, 96.0];
const CROP_ASPECTS: [(&str, Option<f32>); 7] = [
    ("Freeform", None),
    ("1:1", Some(1.0)),
    ("4:3", Some(4.0 / 3.0)),
    ("3:2", Some(1.5)),
    ("16:9", Some(16.0 / 9.0)),
    ("9:16", Some(9.0 / 16.0)),
    ("Original", Some(-1.0)),
];

const TOOLBAR_H: f64 = 52.0;
const BOTTOM_H: f64 = 46.0;
const WIDTHS: [f32; 7] = [1.0, 2.0, 3.0, 5.0, 8.0, 12.0, 16.0];
pub const SIDEBAR_W: f64 = 260.0;
const HANDLE: f32 = 9.0;

#[derive(Clone, Copy, PartialEq)]
enum Selected {
    Annot(usize),
    Layer(usize),
}

#[derive(Clone, PartialEq)]
enum Drag {
    Create(Pt),
    Move {
        start: Pt,
        index: usize,
        orig: Annotation,
    },
    MoveLayer {
        start: Pt,
        index: usize,
        orig: Pt,
    },
    /// Arrow/line endpoint 0 or 1, or 2 for the curve control point.
    Point {
        index: usize,
        which: u8,
    },
    CropNew(Pt),
    CropMove(Pt, Rect),
    CropResize(usize, Rect),
}

/// An in-progress text annotation and the field it's typed into.
struct TextEdit {
    field: Retained<NSTextField>,
    at: Pt,
    style: TextStyle,
    size: f32,
    color: Color,
}

/// A custom-drawn control: its view plus the delegate that keeps it alive.
type Drawn = (Retained<View>, Rc<dyn ViewDelegate>);

/// Committed annotations rendered once; reused while the user drags.
struct Cache {
    pm: Pixmap,
    cg: CFRetained<CGImage>,
}

pub struct Editor {
    pub window: Retained<NSWindow>,
    canvas: Retained<View>,
    sidebar: Retained<NSView>,
    observer: RefCell<Option<Retained<WindowObserver>>>,
    pub targets: Targets,
    /// Targets of the most recent popup menu; replaced by the next one.
    menu_targets: RefCell<Targets>,

    doc: RefCell<Document>,
    undo: RefCell<UndoStack>,

    tool: Cell<Tool>,
    color: Cell<Color>,
    width: Cell<f32>,
    arrow_style: Cell<ArrowStyle>,
    text_style: Cell<TextStyle>,
    text_size: Cell<f32>,
    blur_secure: Cell<bool>,
    spot_ellipse: Cell<bool>,
    crop_aspect: Cell<Option<f32>>,

    selected: Cell<Option<Selected>>,
    drag: RefCell<Option<Drag>>,
    /// Shape being drawn, not yet in the document.
    active: RefCell<Option<Annotation>>,
    /// Annotation being moved; drawn live instead of from the cache.
    live: Cell<Option<usize>>,
    crop_draft: Cell<Option<Rect>>,
    text_edit: RefCell<Option<TextEdit>>,
    cache: RefCell<Option<Cache>>,
    preview: RefCell<Option<CFRetained<CGImage>>>,
    pub show_background: Cell<bool>,
    zoom: Cell<f32>,
    pan: Cell<Pt>,

    tools_control: RefCell<Option<Retained<NSSegmentedControl>>>,
    style_popup: RefCell<Option<Retained<NSPopUpButton>>>,
    size_popup: RefCell<Option<Retained<NSPopUpButton>>>,
    width_popup: RefCell<Option<Retained<NSPopUpButton>>>,
    /// The color button and the delegate that draws it.
    color_dot: RefCell<Option<Drawn>>,
    bg_button: RefCell<Option<Retained<NSButton>>>,
    size_label: RefCell<Option<Retained<NSTextField>>>,
    drag_me: RefCell<Option<Rc<dyn ViewDelegate>>>,
    pub panel: RefCell<Option<background_panel::Panel>>,
}

pub fn open(doc: Document) {
    let app = app();
    let mtm = app.mtm;
    let canvas_px = doc.visible_bounds();
    let scale = objc2_app_kit::NSScreen::mainScreen(mtm).map_or(2.0, |s| s.backingScaleFactor()) as f32;
    let w = (canvas_px.w / scale).clamp(980.0, 1400.0) as f64 + 80.0;
    let h = (canvas_px.h / scale).clamp(420.0, 860.0) as f64 + TOOLBAR_H + BOTTOM_H + 64.0;
    let window = ui::app_window(mtm, "Annotate", w, h);
    window.setStyleMask(window.styleMask() | NSWindowStyleMask::FullSizeContentView);
    window.setTitlebarAppearsTransparent(true);
    window.setTitleVisibility(NSWindowTitleVisibility::Hidden);
    window.setContentMinSize(NSSize::new(1060.0, 480.0));

    let ed = Rc::new_cyclic(|weak: &Weak<Editor>| {
        let delegate: Weak<dyn ViewDelegate> = weak.clone();
        let canvas = View::new(mtm, NSRect::ZERO, delegate);
        canvas.accept_file_drops();
        let sidebar = NSView::initWithFrame(NSView::alloc(mtm), NSRect::ZERO);
        Editor {
            window,
            canvas,
            sidebar,
            observer: RefCell::default(),
            targets: Targets::default(),
            menu_targets: RefCell::default(),
            doc: RefCell::new(doc),
            undo: RefCell::default(),
            tool: Cell::new(Tool::Arrow),
            color: Cell::new(Color::RED),
            width: Cell::new(5.0),
            arrow_style: Cell::new(ArrowStyle::Standard),
            text_style: Cell::new(TextStyle::Outline),
            text_size: Cell::new(32.0),
            blur_secure: Cell::new(true),
            spot_ellipse: Cell::new(false),
            crop_aspect: Cell::new(None),
            selected: Cell::new(None),
            drag: RefCell::default(),
            active: RefCell::default(),
            live: Cell::new(None),
            crop_draft: Cell::new(None),
            text_edit: RefCell::default(),
            cache: RefCell::default(),
            preview: RefCell::default(),
            show_background: Cell::new(false),
            zoom: Cell::new(1.0),
            pan: Cell::new(Pt::default()),
            tools_control: RefCell::default(),
            style_popup: RefCell::default(),
            size_popup: RefCell::default(),
            width_popup: RefCell::default(),
            color_dot: RefCell::default(),
            bg_button: RefCell::default(),
            size_label: RefCell::default(),
            drag_me: RefCell::default(),
            panel: RefCell::default(),
        }
    });
    ed.build_ui();
    let weak = Rc::downgrade(&ed);
    *ed.observer.borrow_mut() = Some(ui::on_close(&ed.window, move || {
        if let Some(ed) = weak.upgrade() {
            ed.commit_text();
            // The shared color panel doesn't retain its target; unhook ours.
            let mtm = MainThreadMarker::new().unwrap();
            if NSColorPanel::sharedColorPanelExists(mtm) {
                let panel = NSColorPanel::sharedColorPanel(mtm);
                unsafe {
                    panel.setTarget(None);
                    panel.setAction(None);
                }
            }
            // Release on the next tick: this closure is owned by the editor.
            let me = Rc::as_ptr(&ed);
            crate::app::after(0.0, move || {
                crate::app::app().editors.borrow_mut().retain(|e| !std::ptr::eq(Rc::as_ptr(e), me))
            });
        }
    }));
    ed.set_tool(Tool::Arrow);
    ed.doc_changed();
    app.editors.borrow_mut().push(ed.clone());
    ui::activate(mtm);
    ed.window.makeKeyAndOrderFront(None);
    ed.window.makeFirstResponder(Some(&ed.canvas));
}

impl Editor {
    // ------------------------------------------------------------ UI construction

    fn build_ui(self: &Rc<Self>) {
        let mtm = MainThreadMarker::new().unwrap();
        let content = self.window.contentView().expect("content view");
        let bounds = content.bounds();
        let (w, h) = (bounds.size.width, bounds.size.height);

        self.canvas.setFrame(NSRect::new(NSPoint::new(0.0, BOTTOM_H), NSSize::new(w, h - TOOLBAR_H - BOTTOM_H)));
        self.canvas.setAutoresizingMask(
            NSAutoresizingMaskOptions::ViewWidthSizable | NSAutoresizingMaskOptions::ViewHeightSizable,
        );
        content.addSubview(&self.canvas);

        self.sidebar.setFrame(NSRect::new(
            NSPoint::new(w - SIDEBAR_W, BOTTOM_H),
            NSSize::new(SIDEBAR_W, h - TOOLBAR_H - BOTTOM_H),
        ));
        self.sidebar.setAutoresizingMask(
            NSAutoresizingMaskOptions::ViewMinXMargin | NSAutoresizingMaskOptions::ViewHeightSizable,
        );
        self.sidebar.setHidden(true);
        content.addSubview(&self.sidebar);
        background_panel::build(self, &self.sidebar);

        // The top bar sits in the (transparent) title bar, right of the traffic lights.
        ui::pin_bar(&self.build_toolbar(mtm), &content, true, TOOLBAR_H);
        ui::pin_bar(&self.build_bottom_bar(mtm), &content, false, BOTTOM_H);
    }

    /// Wraps an editor method as a control action that doesn't keep the editor alive.
    fn act(self: &Rc<Self>, f: fn(&Editor)) -> impl Fn() + 'static {
        let me = Rc::downgrade(self);
        move || {
            if let Some(ed) = me.upgrade() {
                f(&ed)
            }
        }
    }

    fn build_toolbar(self: &Rc<Self>, mtm: MainThreadMarker) -> Retained<NSView> {
        // Tools: one native segmented control.
        let images: Vec<Retained<NSImage>> = TOOLS.iter().map(|t| ui::symbol(t.1).unwrap_or_default()).collect();
        let me = Rc::downgrade(self);
        let t = self.targets.add(mtm, move |sender| {
            let Some(ed) = me.upgrade() else { return };
            let Some(c) = ui::sender::<NSSegmentedControl>(sender) else { return };
            if let Some(t) = TOOLS.get(c.selectedSegment().max(0) as usize) {
                ed.set_tool(t.0);
            }
        });
        let tools = unsafe {
            NSSegmentedControl::segmentedControlWithImages_trackingMode_target_action(
                &NSArray::from_retained_slice(&images),
                NSSegmentSwitchTracking::SelectOne,
                Some(&t),
                Some(sel!(fire:)),
                mtm,
            )
        };
        for (i, t) in TOOLS.iter().enumerate() {
            tools.setToolTip_forSegment(Some(&NSString::from_str(t.2)), i as isize);
            tools.setWidth_forSegment(30.0, i as isize);
        }
        *self.tools_control.borrow_mut() = Some(tools.clone());

        // Color: a dot showing the current color; click for the palette.
        let dot_delegate: Rc<dyn ViewDelegate> = Rc::new(ColorDot(Rc::downgrade(self)));
        let dot = View::new(mtm, NSRect::new(NSPoint::ZERO, NSSize::new(24.0, 24.0)), Rc::downgrade(&dot_delegate));
        ui::set_width(&dot, 24.0);
        dot.heightAnchor().constraintEqualToConstant(24.0).setActive(true);
        dot.setToolTip(Some(&NSString::from_str("Color")));
        *self.color_dot.borrow_mut() = Some((dot.clone(), dot_delegate));

        // Stroke width.
        let width = NSPopUpButton::initWithFrame_pullsDown(NSPopUpButton::alloc(mtm), NSRect::ZERO, false);
        for wpx in WIDTHS {
            width.addItemWithTitle(&NSString::from_str(&format!("{wpx}")));
            if let Some(item) = width.lastItem() {
                item.setImage(Some(&ui::line_image(wpx)));
            }
        }
        let me = Rc::downgrade(self);
        let t = self.targets.add(mtm, move |sender| {
            let Some(ed) = me.upgrade() else { return };
            let Some(p) = ui::sender::<NSPopUpButton>(sender) else { return };
            ed.set_width(WIDTHS[p.indexOfSelectedItem().max(0) as usize]);
        });
        unsafe {
            width.setTarget(Some(&t));
            width.setAction(Some(sel!(fire:)));
        }
        width.setToolTip(Some(&NSString::from_str("Stroke width")));
        *self.width_popup.borrow_mut() = Some(width.clone());

        let style = NSPopUpButton::initWithFrame_pullsDown(NSPopUpButton::alloc(mtm), NSRect::ZERO, false);
        let me = Rc::downgrade(self);
        let t = self.targets.add(mtm, move |sender| {
            let Some(ed) = me.upgrade() else { return };
            let Some(p) = ui::sender::<NSPopUpButton>(sender) else { return };
            ed.set_style(p.indexOfSelectedItem() as usize);
        });
        unsafe {
            style.setTarget(Some(&t));
            style.setAction(Some(sel!(fire:)));
        }
        *self.style_popup.borrow_mut() = Some(style.clone());

        let size = NSPopUpButton::initWithFrame_pullsDown(NSPopUpButton::alloc(mtm), NSRect::ZERO, false);
        for s in TEXT_SIZES {
            size.addItemWithTitle(&NSString::from_str(&format!("{s} pt")));
        }
        size.selectItemAtIndex(3);
        let me = Rc::downgrade(self);
        let t = self.targets.add(mtm, move |sender| {
            let Some(ed) = me.upgrade() else { return };
            let Some(p) = ui::sender::<NSPopUpButton>(sender) else { return };
            ed.set_text_size(TEXT_SIZES[p.indexOfSelectedItem().max(0) as usize]);
        });
        unsafe {
            size.setTarget(Some(&t));
            size.setAction(Some(sel!(fire:)));
        }
        *self.size_popup.borrow_mut() = Some(size.clone());

        // Undo / redo.
        let history_images = [
            ui::symbol("arrow.uturn.backward").unwrap_or_default(),
            ui::symbol("arrow.uturn.forward").unwrap_or_default(),
        ];
        let me = Rc::downgrade(self);
        let t = self.targets.add(mtm, move |sender| {
            let Some(ed) = me.upgrade() else { return };
            let Some(c) = ui::sender::<NSSegmentedControl>(sender) else { return };
            if c.selectedSegment() == 0 {
                ed.undo()
            } else {
                ed.redo()
            }
        });
        let undo_redo = unsafe {
            NSSegmentedControl::segmentedControlWithImages_trackingMode_target_action(
                &NSArray::from_retained_slice(&history_images),
                NSSegmentSwitchTracking::Momentary,
                Some(&t),
                Some(sel!(fire:)),
                mtm,
            )
        };
        undo_redo.setToolTip_forSegment(Some(&NSString::from_str("Undo (⌘Z)")), 0);
        undo_redo.setToolTip_forSegment(Some(&NSString::from_str("Redo (⇧⌘Z)")), 1);

        let transform = ui::menu(
            mtm,
            &self.targets,
            vec![
                ("Rotate Right", Box::new(self.act(|e| e.edit(|d| d.rotate_cw())))),
                ("Flip Horizontally", Box::new(self.act(|e| e.edit(|d| d.flip(true))))),
                ("Flip Vertically", Box::new(self.act(|e| e.edit(|d| d.flip(false))))),
            ],
        );
        let transform = ui::menu_button(mtm, &self.targets, "rotate.right", "Rotate & Flip", transform);

        let bg = ui::icon_button(
            mtm,
            &self.targets,
            "photo.on.rectangle.angled",
            "Background",
            self.act(|e| e.toggle_background()),
        );
        bg.setTitle(&NSString::from_str("Background"));
        bg.setImagePosition(objc2_app_kit::NSCellImagePosition::ImageLeading);
        bg.setButtonType(NSButtonType::PushOnPushOff);
        bg.setBezelStyle(NSBezelStyle::Toolbar);
        *self.bg_button.borrow_mut() = Some(bg.clone());

        ui::bar(
            &[
                ui::view(&tools),
                ui::view(&dot),
                ui::view(&width),
                ui::view(&style),
                ui::view(&size),
                ui::flex_space(),
                ui::view(&undo_redo),
                ui::view(&transform),
                ui::view(&bg),
            ],
            10.0,
            objc2_foundation::NSEdgeInsets { top: 0.0, left: 84.0, bottom: 0.0, right: 12.0 },
            false,
        )
    }

    fn build_bottom_bar(self: &Rc<Self>, mtm: MainThreadMarker) -> Retained<NSView> {
        let drag_delegate: Rc<dyn ViewDelegate> = Rc::new(DragMe(Rc::downgrade(self)));
        let drag = View::new(mtm, NSRect::new(NSPoint::ZERO, NSSize::new(92.0, 26.0)), Rc::downgrade(&drag_delegate));
        ui::set_width(&drag, 92.0);
        drag.heightAnchor().constraintEqualToConstant(26.0).setActive(true);
        *self.drag_me.borrow_mut() = Some(drag_delegate);
        drag.setToolTip(Some(&NSString::from_str("Drag the result into any app")));

        let label = ui::label("");
        label.setFont(Some(&NSFont::monospacedDigitSystemFontOfSize_weight(11.0, 0.0)));
        label.setTextColor(Some(&objc2_app_kit::NSColor::secondaryLabelColor()));
        *self.size_label.borrow_mut() = Some(label.clone());

        let icon = |name: &str, tip: &str, f: fn(&Editor)| {
            ui::view(&ui::icon_button(mtm, &self.targets, name, tip, self.act(f)))
        };
        let more = ui::menu(
            mtm,
            &self.targets,
            vec![
                (
                    "Save As…",
                    Box::new(self.act(|e| {
                        app().save_as(&e.final_image());
                    })),
                ),
                ("Save Project…", Box::new(self.act(|e| e.save_project()))),
                ("-", Box::new(|| {})),
                ("Copy Text (OCR)", Box::new(self.act(|e| app().ocr(&e.final_image())))),
            ],
        );
        let save = ui::text_button(
            mtm,
            &self.targets,
            "Save",
            self.act(|e| {
                app().save(&e.final_image());
            }),
        );
        let copy = ui::text_button(mtm, &self.targets, "Copy", self.act(|e| e.copy()));
        copy.setBezelColor(Some(&objc2_app_kit::NSColor::controlAccentColor()));
        ui::set_width(&save, 72.0);
        ui::set_width(&copy, 72.0);

        ui::bar(
            &[
                ui::view(&drag),
                ui::view(&label),
                ui::flex_space(),
                icon("pin", "Pin to Screen", |e| crate::pin::show(Arc::new(e.final_image()), None)),
                icon("square.and.arrow.up", "Share", |e| e.share()),
                ui::view(&ui::menu_button(mtm, &self.targets, "ellipsis.circle", "More", more)),
                ui::view(&save),
                ui::view(&copy),
            ],
            10.0,
            objc2_foundation::NSEdgeInsets { top: 0.0, left: 12.0, bottom: 0.0, right: 12.0 },
            true,
        )
    }

    /// The palette menu behind the color dot.
    fn color_menu(self: &Rc<Self>) -> Retained<objc2_app_kit::NSMenu> {
        let mtm = MainThreadMarker::new().unwrap();
        let menu = objc2_app_kit::NSMenu::new(mtm);
        let targets = Targets::default();
        let add = |title: &str, image: Option<Retained<NSImage>>, checked: bool, f: Box<dyn Fn()>| {
            let item = ui::menu_item(mtm, &targets, title, f);
            item.setImage(image.as_deref());
            item.setState(isize::from(checked));
            menu.addItem(&item);
        };
        let names = ["Red", "Orange", "Yellow", "Green", "Blue", "Purple", "Black", "White"];
        let custom = app().settings.borrow().custom_colors.clone();
        let colors = PALETTE
            .iter()
            .zip(names.map(String::from))
            .map(|(c, n)| (*c, n))
            .chain(custom.into_iter().map(|c| (c, c.hex())));
        for (c, name) in colors {
            let me = Rc::downgrade(self);
            add(
                &name,
                Some(ui::color_dot(c, 14)),
                c == self.color.get(),
                Box::new(move || {
                    if let Some(ed) = me.upgrade() {
                        ed.set_color(c)
                    }
                }),
            );
        }
        menu.addItem(&objc2_app_kit::NSMenuItem::separatorItem(mtm));
        let me = Rc::downgrade(self);
        add(
            "Custom Color…",
            ui::symbol("eyedropper"),
            false,
            Box::new(move || {
                if let Some(ed) = me.upgrade() {
                    ed.open_color_panel()
                }
            }),
        );
        add("Add Current Color to Palette", ui::symbol("plus.circle"), false, Box::new(self.act(|e| e.save_color())));
        *self.menu_targets.borrow_mut() = targets;
        menu
    }

    fn open_color_panel(self: &Rc<Self>) {
        let mtm = MainThreadMarker::new().unwrap();
        let panel = NSColorPanel::sharedColorPanel(mtm);
        let me = Rc::downgrade(self);
        let t = self.targets.add(mtm, move |sender| {
            let Some(ed) = me.upgrade() else { return };
            let Some(p) = ui::sender::<NSColorPanel>(sender) else { return };
            if let Some(c) = from_ns(&p.color()) {
                ed.set_color(c);
            }
        });
        unsafe {
            panel.setTarget(Some(&t));
            panel.setAction(Some(sel!(fire:)));
        }
        panel.setShowsAlpha(true);
        panel.setColor(&gfx::ns_color(rgba(self.color.get())));
        panel.orderFront(None);
    }

    fn save_color(&self) {
        let app = app();
        let c = self.color.get();
        let mut s = app.settings.borrow_mut();
        if !s.custom_colors.contains(&c) && !PALETTE.contains(&c) {
            s.custom_colors.push(c);
        }
        drop(s);
        app.save_settings();
        crate::app::toast("Color added to the palette");
    }

    // ------------------------------------------------------------ state changes

    fn set_tool(&self, tool: Tool) {
        self.commit_text();
        if self.tool.get() == Tool::Crop && tool != Tool::Crop {
            self.crop_draft.set(None);
        }
        if tool == Tool::Crop {
            self.crop_draft.set(self.doc.borrow().crop);
            self.selected.set(None);
        }
        self.tool.set(tool);
        if let Some(c) = self.tools_control.borrow().as_ref() {
            c.setSelectedSegment(TOOLS.iter().position(|t| t.0 == tool).map_or(-1, |i| i as isize));
        }
        if tool != Tool::Select {
            self.selected.set(None);
        }
        // Each tool keeps the last width that suits it.
        let width = match tool {
            Tool::Highlighter => 6.0,
            Tool::Blur | Tool::Pixelate => 5.0,
            _ => self.width.get(),
        };
        self.width.set(width);
        self.sync_width_popup();
        self.sync_style_popup();
        self.invalidate();
    }

    fn sync_style_popup(&self) {
        let Some(popup) = self.style_popup.borrow().clone() else { return };
        popup.removeAllItems();
        let (items, selected): (Vec<String>, usize) = match self.tool.get() {
            Tool::Arrow => (
                ARROW_STYLES.iter().map(|s| s.0.to_string()).collect(),
                ARROW_STYLES.iter().position(|s| s.1 == self.arrow_style.get()).unwrap_or(0),
            ),
            Tool::Text => (
                TextStyle::ALL.iter().map(|s| format!("{s:?}")).collect(),
                TextStyle::ALL.iter().position(|s| *s == self.text_style.get()).unwrap_or(0),
            ),
            Tool::Blur => (vec!["Secure".into(), "Smooth".into()], usize::from(!self.blur_secure.get())),
            Tool::Spotlight => (vec!["Rectangle".into(), "Ellipse".into()], usize::from(self.spot_ellipse.get())),
            Tool::Crop => (
                CROP_ASPECTS.iter().map(|a| a.0.to_string()).collect(),
                CROP_ASPECTS.iter().position(|a| a.1 == self.crop_aspect.get()).unwrap_or(0),
            ),
            _ => (vec![], 0),
        };
        popup.setHidden(items.is_empty());
        for i in &items {
            popup.addItemWithTitle(&NSString::from_str(i));
        }
        if !items.is_empty() {
            popup.selectItemAtIndex(selected as isize);
        }
        if let Some(size) = self.size_popup.borrow().as_ref() {
            size.setHidden(self.tool.get() != Tool::Text);
        }
    }

    fn set_style(&self, i: usize) {
        match self.tool.get() {
            Tool::Arrow => self.arrow_style.set(ARROW_STYLES.get(i).map_or(ArrowStyle::Standard, |s| s.1)),
            Tool::Text => self.text_style.set(TextStyle::ALL.get(i).copied().unwrap_or_default()),
            Tool::Blur => self.blur_secure.set(i == 0),
            Tool::Spotlight => self.spot_ellipse.set(i == 1),
            Tool::Crop => {
                let mut aspect = CROP_ASPECTS.get(i).and_then(|a| a.1);
                if aspect == Some(-1.0) {
                    let b = self.doc.borrow().canvas_bounds();
                    aspect = Some(b.w / b.h);
                }
                self.crop_aspect.set(aspect);
                if let (Some(a), Some(r)) = (aspect, self.crop_draft.get()) {
                    self.crop_draft.set(Some(Rect::new(r.x, r.y, r.w, r.w / a)));
                }
            }
            _ => {}
        }
        self.canvas.redraw();
    }

    fn set_color(&self, c: Color) {
        self.color.set(c);
        if let Some((dot, _)) = self.color_dot.borrow().as_ref() {
            dot.redraw();
        }
        self.modify_selected(|a| a.color = c);
    }

    fn sync_width_popup(&self) {
        if let Some(p) = self.width_popup.borrow().as_ref() {
            let w = self.width.get();
            let i = WIDTHS
                .iter()
                .enumerate()
                .min_by(|a, b| (a.1 - w).abs().total_cmp(&(b.1 - w).abs()))
                .map_or(0, |(i, _)| i);
            p.selectItemAtIndex(i as isize);
        }
    }

    fn set_width(&self, w: f32) {
        self.width.set(w);
        self.modify_selected(|a| a.width = w);
    }

    fn set_text_size(&self, s: f32) {
        self.text_size.set(s);
        self.modify_selected(|a| {
            if let Shape::Text { size, .. } = &mut a.shape {
                *size = s;
            }
        });
    }

    fn modify_selected(&self, f: impl FnOnce(&mut Annotation)) {
        if let Some(Selected::Annot(i)) = self.selected.get() {
            self.edit(|d| {
                if let Some(a) = d.annotations.get_mut(i) {
                    f(a);
                }
            });
        }
    }

    /// Applies an undoable change to the document.
    fn edit(&self, f: impl FnOnce(&mut Document)) {
        self.undo.borrow_mut().checkpoint(&self.doc.borrow());
        f(&mut self.doc.borrow_mut());
        self.doc_changed();
    }

    pub fn doc_changed(&self) {
        let sel_valid = match self.selected.get() {
            Some(Selected::Annot(i)) => i < self.doc.borrow().annotations.len(),
            Some(Selected::Layer(i)) => i < self.doc.borrow().layers.len(),
            None => true,
        };
        if !sel_valid {
            self.selected.set(None);
        }
        self.invalidate();
        let size = self.final_size();
        if let Some(l) = self.size_label.borrow().as_ref() {
            l.setStringValue(&NSString::from_str(&format!("{} × {} px", size.0, size.1)));
        }
        self.window.setTitle(&NSString::from_str(&format!("Annotate — {} × {}", size.0, size.1)));
    }

    fn invalidate(&self) {
        self.cache.replace(None);
        self.preview.replace(None);
        self.canvas.redraw();
    }

    fn undo(&self) {
        self.commit_text();
        let changed = self.undo.borrow_mut().undo(&mut self.doc.borrow_mut());
        if changed {
            self.doc_changed();
        }
    }

    fn redo(&self) {
        let changed = self.undo.borrow_mut().redo(&mut self.doc.borrow_mut());
        if changed {
            self.doc_changed();
        }
    }

    pub fn document(&self) -> std::cell::Ref<'_, Document> {
        self.doc.borrow()
    }

    pub fn set_background(&self, bg: Option<Background>) {
        let same = self.doc.borrow().background == bg;
        if !same {
            self.edit(|d| d.background = bg);
        }
    }

    fn toggle_background(&self) {
        let on = !self.show_background.get();
        self.show_background.set(on);
        if let Some(b) = self.bg_button.borrow().as_ref() {
            b.setState(isize::from(on));
        }
        self.sidebar.setHidden(!on);
        let content = self.window.contentView().unwrap().bounds();
        let w = content.size.width - if on { SIDEBAR_W } else { 0.0 };
        let mut f = self.canvas.frame();
        f.size.width = w;
        self.canvas.setFrame(f);
        if on && self.doc.borrow().background.is_none() {
            let bg = app().settings.borrow().background_presets.first().map(|p| p.1.clone()).unwrap_or_default();
            self.set_background(Some(bg));
            background_panel::sync(self);
        }
        self.invalidate();
    }

    // ------------------------------------------------------------ export

    fn final_size(&self) -> (u32, u32) {
        let doc = self.doc.borrow();
        if let Some(s) = doc.output_size {
            return s;
        }
        let r = doc.visible_bounds();
        (r.w.round() as u32, r.h.round() as u32)
    }

    pub fn final_image(&self) -> RgbaImage {
        self.commit_text();
        render::render(&self.doc.borrow(), &app().fonts)
    }

    fn copy(&self) {
        ui::copy_image(&self.final_image());
        crate::app::toast("Copied to clipboard");
    }

    fn share(&self) {
        let Some(path) = crate::app::temp_png(&self.final_image()) else { return };
        let url = NSURL::fileURLWithPath(&NSString::from_str(&path.to_string_lossy()));
        let item: Retained<objc2::runtime::AnyObject> = Retained::into_super(Retained::into_super(url));
        let items = NSArray::from_retained_slice(&[item]);
        let picker = unsafe { NSSharingServicePicker::initWithItems(NSSharingServicePicker::alloc(), &items) };
        let b = self.canvas.bounds();
        picker.showRelativeToRect_ofView_preferredEdge(
            NSRect::new(NSPoint::new(b.size.width - 40.0, b.size.height - 4.0), NSSize::new(1.0, 1.0)),
            &self.canvas,
            NSRectEdge::MaxY,
        );
    }

    fn save_project(&self) {
        self.commit_text();
        let mtm = MainThreadMarker::new().unwrap();
        let panel = objc2_app_kit::NSSavePanel::savePanel(mtm);
        panel.setNameFieldStringValue(&NSString::from_str(&format!("Screenshot.{PROJECT_EXT}")));
        if panel.runModal() != 1 {
            return;
        }
        let Some(path) = panel.URL().and_then(|u| u.path()) else { return };
        let mut path = std::path::PathBuf::from(path.to_string());
        path.set_extension(PROJECT_EXT);
        match self.doc.borrow().save_project(&path) {
            Ok(()) => crate::app::toast("Project saved"),
            Err(e) => crate::app::alert("Couldn't save the project", &e.to_string()),
        }
    }

    // ------------------------------------------------------------ geometry

    /// The document region on screen, its top-left in the view, and points per pixel.
    fn layout(&self) -> (Rect, Pt, f32) {
        let doc = self.doc.borrow();
        let region = if self.tool.get() == Tool::Crop { doc.canvas_bounds() } else { doc.visible_bounds() };
        let b = self.canvas.bounds();
        let (vw, vh) = (b.size.width as f32, b.size.height as f32);
        let backing = self.window.backingScaleFactor() as f32;
        let fit = ((vw - 48.0) / region.w).min((vh - 48.0) / region.h).min(1.0 / backing).max(0.01);
        let scale = fit * self.zoom.get();
        let pan = self.pan.get();
        let offset = Pt::new((vw - region.w * scale) / 2.0 + pan.x, (vh - region.h * scale) / 2.0 + pan.y);
        (region, offset, scale)
    }

    fn to_doc(&self, p: Pt) -> Pt {
        let (region, offset, scale) = self.layout();
        Pt::new(region.x + (p.x - offset.x) / scale, region.y + (p.y - offset.y) / scale)
    }

    fn to_view(&self, r: Rect) -> Rect {
        let (region, offset, scale) = self.layout();
        Rect::new(offset.x + (r.x - region.x) * scale, offset.y + (r.y - region.y) * scale, r.w * scale, r.h * scale)
    }

    fn to_view_pt(&self, p: Pt) -> Pt {
        let r = self.to_view(Rect::new(p.x, p.y, 0.0, 0.0));
        Pt::new(r.x, r.y)
    }

    fn handle_tol(&self) -> f32 {
        HANDLE / self.layout().2
    }

    // ------------------------------------------------------------ rendering

    fn ensure_cache(&self) {
        if self.cache.borrow().is_some() {
            return;
        }
        let pm = {
            let doc = self.doc.borrow();
            match self.live.get() {
                Some(i) => {
                    let mut d = doc.clone();
                    if i < d.annotations.len() {
                        d.annotations.remove(i);
                    }
                    render_canvas(&d, &app().fonts)
                }
                None => render_canvas(&doc, &app().fonts),
            }
        };
        let cg = cg::image_from_pixmap(&pm);
        *self.cache.borrow_mut() = Some(Cache { pm, cg });
    }

    /// The committed canvas, rendered once per document change.
    fn frame_image(&self) -> CFRetained<CGImage> {
        self.ensure_cache();
        self.cache.borrow().as_ref().expect("cache just built").cg.clone()
    }

    /// Whatever is being drawn or dragged right now, rendered into a small
    /// patch of the canvas (with the pixels beneath it, so blur and pixelate
    /// preview correctly). Returns the patch and where it goes, in canvas
    /// coordinates. Rendering only this patch keeps dragging smooth on 6K images.
    fn motion_patch(&self) -> Option<(CFRetained<CGImage>, Rect)> {
        let live = self.live.get().and_then(|i| self.doc.borrow().annotations.get(i).cloned());
        let moving: Vec<Annotation> = live.into_iter().chain(self.active.borrow().clone()).collect();
        let area =
            moving.iter().map(|a| self.measured_bounds(a).inflate(a.width * 4.0 + 24.0)).reduce(|a, b| a.union(&b))?;
        let cache = self.cache.borrow();
        let cache = cache.as_ref()?;
        let canvas = self.doc.borrow().canvas_bounds();
        let (x, y, w, h) = area.translate(-canvas.x, -canvas.y).clamp_px(cache.pm.width(), cache.pm.height())?;
        let mut patch = render::sub_pixmap(&cache.pm, x, y, w, h);
        let origin = Pt::new(canvas.x + x as f32, canvas.y + y as f32);
        for a in &moving {
            draw_annotation(&mut patch, a, origin, &app().fonts);
        }
        Some((cg::image_from_pixmap(&patch), Rect::new(origin.x, origin.y, w as f32, h as f32)))
    }

    fn draw_canvas(&self, c: &CGContext) {
        let b = self.canvas.bounds();
        let bounds = Rect::new(0.0, 0.0, b.size.width as f32, b.size.height as f32);
        gfx::fill_ns(bounds, &objc2_app_kit::NSColor::underPageBackgroundColor());

        if self.show_background.get() {
            let cached = self.preview.borrow().clone();
            let img = match cached {
                Some(img) => img,
                None => {
                    let img = cg::image_from_rgba8(&render::render(&self.doc.borrow(), &app().fonts));
                    *self.preview.borrow_mut() = Some(img.clone());
                    img
                }
            };
            let (iw, ih) = (CGImage::width(Some(&img)) as f32, CGImage::height(Some(&img)) as f32);
            let k =
                ((bounds.w - 48.0) / iw).min((bounds.h - 48.0) / ih).min(1.0 / self.window.backingScaleFactor() as f32);
            let (w, h) = (iw * k, ih * k);
            let r = Rect::new((bounds.w - w) / 2.0, (bounds.h - h) / 2.0, w, h);
            gfx::with_shadow(c, 18.0, 0.25, || gfx::image(c, &img, r, true));
            return;
        }

        let img = self.frame_image();
        let canvas = self.doc.borrow().canvas_bounds();
        let (region, _, _) = self.layout();
        let dest = self.to_view(region);
        // Crop to the visible region without copying pixels.
        let src =
            cg::rect((region.x - canvas.x) as f64, (region.y - canvas.y) as f64, region.w as f64, region.h as f64);
        gfx::with_shadow(c, 18.0, 0.25, || checker(c, dest));
        if let Some(sub) = CGImage::with_image_in_rect(Some(&img), src) {
            gfx::image(c, &sub, dest, true);
        }
        if let Some((patch, at)) = self.motion_patch() {
            gfx::clipped(c, dest, || gfx::image(c, &patch, self.to_view(at), true));
        }
        // Spotlight dims the whole canvas, so preview it as an outline + dim.
        if let Some(Annotation { shape: Shape::Spotlight { rect, .. }, .. }) = self.active.borrow().as_ref() {
            gfx::dim_except(c, dest, Some(self.to_view(*rect)), (0.0, 0.0, 0.0, 0.45));
        }

        if self.tool.get() == Tool::Crop {
            let crop = self.crop_draft.get().map(|r| self.to_view(r));
            gfx::dim_except(c, dest, crop, (0.0, 0.0, 0.0, 0.5));
            if let (Some(r), Some(doc_r)) = (crop, self.crop_draft.get()) {
                gfx::stroke(c, r, 1.0, gfx::WHITE);
                for i in 1..3 {
                    let f = i as f32 / 3.0;
                    gfx::line(c, r.x + r.w * f, r.y, r.x + r.w * f, r.bottom(), 0.5, (1.0, 1.0, 1.0, 0.5));
                    gfx::line(c, r.x, r.y + r.h * f, r.right(), r.y + r.h * f, 0.5, (1.0, 1.0, 1.0, 0.5));
                }
                for h in r.handles(HANDLE) {
                    gfx::fill(c, h, gfx::WHITE);
                }
                gfx::pill(
                    &format!("{} × {}  ⏎ to apply", doc_r.w.round(), doc_r.h.round()),
                    r.x,
                    (r.y - 26.0).max(4.0),
                    11.0,
                );
            }
            return;
        }

        match self.selected.get() {
            Some(Selected::Annot(i)) => {
                let doc = self.doc.borrow();
                if let Some(a) = doc.annotations.get(i) {
                    let r = self.to_view(self.measured_bounds(a)).inflate(4.0);
                    gfx::stroke(c, r, 1.0, gfx::ACCENT);
                    for p in edit_points(a) {
                        let v = self.to_view_pt(p);
                        let h = Rect::new(v.x - HANDLE / 2.0, v.y - HANDLE / 2.0, HANDLE, HANDLE);
                        gfx::fill_ellipse(c, h, gfx::WHITE);
                        gfx::stroke_ellipse(c, h, 1.5, gfx::ACCENT);
                    }
                }
            }
            Some(Selected::Layer(i)) => {
                if let Some(l) = self.doc.borrow().layers.get(i) {
                    gfx::stroke(c, self.to_view(l.rect()).inflate(2.0), 2.0, gfx::ACCENT);
                }
            }
            None => {}
        }
    }

    /// Bounds with real text metrics (the core falls back to an estimate).
    fn measured_bounds(&self, a: &Annotation) -> Rect {
        match &a.shape {
            Shape::Text { at, text: t, style, size } => {
                text::measure(&app().fonts, t, *size, *style).translate(at.x, at.y)
            }
            _ => a.bounds(),
        }
    }

    // ------------------------------------------------------------ text editing

    fn begin_text(&self, at: Pt, existing: Option<(String, TextStyle, f32, Color)>) {
        self.commit_text();
        let mtm = MainThreadMarker::new().unwrap();
        let (initial, style, size, color) =
            existing.unwrap_or((String::new(), self.text_style.get(), self.text_size.get(), self.color.get()));
        let (_, _, scale) = self.layout();
        let v = self.to_view_pt(at);
        let field = NSTextField::textFieldWithString(&NSString::from_str(&initial), mtm);
        field.setFont(Some(&NSFont::systemFontOfSize((size * scale).max(9.0) as f64)));
        field.setTextColor(Some(&gfx::ns_color(rgba(color))));
        field.setBezeled(false);
        field.setDrawsBackground(true);
        field.setBackgroundColor(Some(&gfx::ns_color((1.0, 1.0, 1.0, 0.85))));
        field.setPlaceholderString(Some(&NSString::from_str("Type… (⌥⏎ new line)")));
        let h = (size * scale * 1.4).max(18.0) as f64;
        field.setFrame(NSRect::new(NSPoint::new(v.x as f64, v.y as f64), NSSize::new(320.0_f64.max(h * 6.0), h)));
        let me_ptr = self as *const Editor;
        let t = self.targets.add(mtm, move |_| {
            if let Some(ed) = app().editors.borrow().iter().find(|e| std::ptr::eq(Rc::as_ptr(e), me_ptr)).cloned() {
                ed.commit_text();
            }
        });
        unsafe {
            field.setTarget(Some(&t));
            field.setAction(Some(sel!(fire:)));
        }
        self.canvas.addSubview(&field);
        self.window.makeFirstResponder(Some(&field));
        *self.text_edit.borrow_mut() = Some(TextEdit { field, at, style, size, color });
    }

    fn commit_text(&self) {
        let Some(TextEdit { field, at, style, size, color }) = self.text_edit.borrow_mut().take() else { return };
        let text = field.stringValue().to_string();
        self.window.makeFirstResponder(Some(&self.canvas));
        // Remove on the next tick: this often runs inside the field's own action.
        crate::app::after(0.0, move || field.removeFromSuperview());
        if !text.trim().is_empty() {
            self.edit(|d| d.annotations.push(Annotation::new(Shape::Text { at, text, style, size }, color, 2.0)));
        }
    }

    // ------------------------------------------------------------ creating shapes

    fn new_shape(&self, start: Pt, end: Pt, constrain: bool) -> Option<Annotation> {
        let (w, c) = (self.width.get(), self.color.get());
        let end = if constrain { constrain_pt(self.tool.get(), start, end) } else { end };
        let rect = Rect::from_pts(start, end);
        let shape = match self.tool.get() {
            Tool::Rect => Shape::Rect { rect, filled: false },
            Tool::FilledRect => Shape::Rect { rect, filled: true },
            Tool::Ellipse => Shape::Ellipse { rect, filled: false },
            Tool::Line => Shape::Line { from: start, to: end },
            Tool::Arrow => {
                let style = self.arrow_style.get();
                let ctrl = (style == ArrowStyle::Curved).then(|| render::default_ctrl(start, end));
                Shape::Arrow { from: start, to: end, style, ctrl }
            }
            Tool::Blur => Shape::Blur { rect, secure: self.blur_secure.get() },
            Tool::Pixelate => Shape::Pixelate { rect },
            Tool::Spotlight => Shape::Spotlight { rect, ellipse: self.spot_ellipse.get() },
            _ => return None,
        };
        Some(Annotation::new(shape, c, w))
    }

    fn crop_snap(&self, p: Pt) -> Pt {
        let b = self.doc.borrow().canvas_bounds();
        let tol = 8.0 / self.layout().2;
        let snap = |v: f32, lo: f32, hi: f32| {
            if (v - lo).abs() < tol {
                lo
            } else if (v - hi).abs() < tol {
                hi
            } else {
                v.clamp(lo, hi)
            }
        };
        Pt::new(snap(p.x, b.x, b.right()), snap(p.y, b.y, b.bottom()))
    }

    fn apply_crop(&self) {
        let draft = self.crop_draft.get();
        let canvas = self.doc.borrow().canvas_bounds();
        let crop = draft.filter(|r| r.w >= 2.0 && r.h >= 2.0 && *r != canvas);
        self.edit(|d| d.crop = crop);
        self.set_tool(Tool::Select);
    }
}

// ---------------------------------------------------------------- input

impl ViewDelegate for Editor {
    fn draw(&self, _view: &View, cg: &CGContext) {
        self.draw_canvas(cg);
    }

    fn mouse_down(&self, _view: &View, vp: Pt, ev: &NSEvent) {
        if self.show_background.get() {
            return;
        }
        self.window.makeFirstResponder(Some(&self.canvas));
        let p = self.to_doc(vp);
        let tol = self.handle_tol();
        match self.tool.get() {
            Tool::Select => {
                // Handles of the selected shape first, then shapes, then layers.
                if let Some(Selected::Annot(i)) = self.selected.get() {
                    let a = self.doc.borrow().annotations.get(i).cloned();
                    if let Some(which) = a.as_ref().and_then(|a| edit_points(a).iter().position(|h| h.dist(p) <= tol)) {
                        self.undo.borrow_mut().checkpoint(&self.doc.borrow());
                        self.live.set(Some(i));
                        self.cache.replace(None);
                        *self.drag.borrow_mut() = Some(Drag::Point { index: i, which: which as u8 });
                        return;
                    }
                }
                let hit = self.doc.borrow().hit(p, tol.max(4.0));
                if let Some(i) = hit {
                    let a = self.doc.borrow().annotations[i].clone();
                    if ev.clickCount() == 2 {
                        if let Shape::Text { at, text, style, size } = &a.shape {
                            self.edit(|d| {
                                d.annotations.remove(i);
                            });
                            self.selected.set(None);
                            return self.begin_text(*at, Some((text.clone(), *style, *size, a.color)));
                        }
                    }
                    self.selected.set(Some(Selected::Annot(i)));
                    self.undo.borrow_mut().checkpoint(&self.doc.borrow());
                    self.live.set(Some(i));
                    self.cache.replace(None);
                    *self.drag.borrow_mut() = Some(Drag::Move { start: p, index: i, orig: a });
                } else if let Some(i) = self.doc.borrow().hit_layer(p) {
                    self.selected.set(Some(Selected::Layer(i)));
                    let orig = self.doc.borrow().layers[i].at;
                    *self.drag.borrow_mut() = Some(Drag::MoveLayer { start: p, index: i, orig });
                } else {
                    self.selected.set(None);
                }
                self.canvas.redraw();
            }
            Tool::Crop => {
                let p = self.crop_snap(p);
                let drag = match self.crop_draft.get() {
                    Some(r) => match r.handles(HANDLE).iter().position(|h| h.inflate(tol).contains(p)) {
                        Some(h) => Drag::CropResize(h, r),
                        None if r.contains(p) => Drag::CropMove(p, r),
                        None => Drag::CropNew(p),
                    },
                    None => Drag::CropNew(p),
                };
                *self.drag.borrow_mut() = Some(drag);
            }
            Tool::Text => {
                let hit = self.doc.borrow().hit(p, 4.0);
                if let Some(i) = hit {
                    let a = self.doc.borrow().annotations[i].clone();
                    if let Shape::Text { at, text, style, size } = &a.shape {
                        self.edit(|d| {
                            d.annotations.remove(i);
                        });
                        return self.begin_text(*at, Some((text.clone(), *style, *size, a.color)));
                    }
                }
                if self.text_edit.borrow().is_some() {
                    self.commit_text();
                } else {
                    self.begin_text(p, None);
                }
            }
            Tool::Counter => {
                let n = self.doc.borrow().next_counter();
                let (c, w) = (self.color.get(), self.width.get());
                self.edit(|d| d.annotations.push(Annotation::new(Shape::Counter { at: p, n }, c, w)));
            }
            Tool::Pencil | Tool::Highlighter => {
                let shape = if self.tool.get() == Tool::Pencil {
                    Shape::Pencil { points: vec![p] }
                } else {
                    Shape::Highlighter { points: vec![p] }
                };
                *self.active.borrow_mut() = Some(Annotation::new(shape, self.color.get(), self.width.get()));
                *self.drag.borrow_mut() = Some(Drag::Create(p));
                self.canvas.redraw();
            }
            _ => {
                *self.drag.borrow_mut() = Some(Drag::Create(p));
            }
        }
    }

    fn mouse_dragged(&self, _view: &View, vp: Pt, ev: &NSEvent) {
        let p = self.to_doc(vp);
        let shift = ui::has_mod(ev, NSEventModifierFlags::Shift);
        let Some(drag) = self.drag.borrow().clone() else { return };
        match drag {
            Drag::Create(start) => {
                let mut active = self.active.borrow_mut();
                match active.as_mut().map(|a| &mut a.shape) {
                    Some(Shape::Pencil { points }) => points.push(p),
                    Some(Shape::Highlighter { points }) if shift => *points = vec![start, Pt::new(p.x, start.y)],
                    Some(Shape::Highlighter { points }) => points.push(p),
                    _ => *active = self.new_shape(start, p, shift),
                }
            }
            Drag::Move { start, index, orig } => {
                let mut a = orig.clone();
                a.translate(p.x - start.x, p.y - start.y);
                if let Some(slot) = self.doc.borrow_mut().annotations.get_mut(index) {
                    *slot = a;
                }
            }
            Drag::MoveLayer { start, index, orig } => {
                if let Some(l) = self.doc.borrow_mut().layers.get_mut(index) {
                    l.at = orig.offset(p.x - start.x, p.y - start.y);
                }
                self.cache.replace(None);
            }
            Drag::Point { index, which } => {
                if let Some(a) = self.doc.borrow_mut().annotations.get_mut(index) {
                    move_point(a, which, p);
                }
            }
            Drag::CropNew(start) => {
                let p = self.crop_snap(p);
                self.crop_draft.set(Some(Rect::with_aspect(start, p, self.crop_aspect.get())));
            }
            Drag::CropMove(start, r) => {
                let b = self.doc.borrow().canvas_bounds();
                let x = (r.x + p.x - start.x).clamp(b.x, b.right() - r.w);
                let y = (r.y + p.y - start.y).clamp(b.y, b.bottom() - r.h);
                self.crop_draft.set(Some(Rect::new(x, y, r.w, r.h)));
            }
            Drag::CropResize(h, r) => {
                let p = self.crop_snap(p);
                self.crop_draft.set(Some(r.resize_handle(h, p, self.crop_aspect.get())));
            }
        }
        self.canvas.redraw();
    }

    fn mouse_up(&self, _view: &View, _vp: Pt, _ev: &NSEvent) {
        let drag = self.drag.borrow_mut().take();
        match drag {
            Some(Drag::Create(_)) => {
                let Some(mut a) = self.active.borrow_mut().take() else { return };
                let tol = 1.5 / self.layout().2;
                if let Shape::Pencil { points } = &mut a.shape {
                    *points = annot::smooth(&annot::simplify(points, tol), 2);
                }
                let tiny = a.bounds().w < 3.0 && a.bounds().h < 3.0 && !matches!(a.shape, Shape::Pencil { .. });
                if !tiny {
                    self.edit(|d| d.annotations.push(a));
                    // Switch to selecting the new shape when it's an editable arrow.
                    if matches!(
                        self.doc.borrow().annotations.last().map(|a| &a.shape),
                        Some(Shape::Arrow { style: ArrowStyle::Curved, .. })
                    ) {
                        self.selected.set(Some(Selected::Annot(self.doc.borrow().annotations.len() - 1)));
                    }
                }
                self.canvas.redraw();
            }
            Some(Drag::Move { .. } | Drag::Point { .. }) => {
                self.live.set(None);
                self.doc_changed();
            }
            Some(Drag::MoveLayer { index, orig, .. }) => {
                // Record the move as one undo step.
                let now = self.doc.borrow().layers.get(index).map(|l| l.at);
                if let Some(now) = now.filter(|n| *n != orig) {
                    self.doc.borrow_mut().layers[index].at = orig;
                    self.edit(|d| d.layers[index].at = now);
                }
            }
            _ => self.canvas.redraw(),
        }
    }

    fn key_down(&self, _view: &View, ev: &NSEvent) -> bool {
        let cmd = ui::has_mod(ev, NSEventModifierFlags::Command);
        let shift = ui::has_mod(ev, NSEventModifierFlags::Shift);
        let chars = ui::key_chars(ev).to_lowercase();
        if cmd {
            match chars.as_str() {
                "z" if shift => self.redo(),
                "z" => self.undo(),
                "c" => self.copy(),
                "s" if shift => {
                    app().save_as(&self.final_image());
                }
                "s" => {
                    app().save(&self.final_image());
                }
                "v" => self.paste(),
                "w" => self.window.close(),
                "0" => {
                    self.zoom.set(1.0);
                    self.pan.set(Pt::default());
                    self.canvas.redraw();
                }
                "=" | "+" => self.zoom_by(1.25),
                "-" => self.zoom_by(0.8),
                _ => return false,
            }
            return true;
        }
        match ev.keyCode() {
            key::ESCAPE => {
                if self.tool.get() == Tool::Crop {
                    self.set_tool(Tool::Select);
                } else {
                    self.selected.set(None);
                    self.canvas.redraw();
                }
            }
            key::RETURN | key::ENTER if self.tool.get() == Tool::Crop => self.apply_crop(),
            key::DELETE | key::FORWARD_DELETE => match self.selected.get() {
                Some(Selected::Annot(i)) => {
                    self.selected.set(None);
                    self.edit(|d| {
                        d.annotations.remove(i);
                    });
                }
                Some(Selected::Layer(i)) => {
                    self.selected.set(None);
                    self.edit(|d| {
                        d.layers.remove(i);
                    });
                }
                None => {}
            },
            k @ (key::LEFT | key::RIGHT | key::UP | key::DOWN) => {
                let step = if shift { 10.0 } else { 1.0 };
                let (dx, dy) = match k {
                    key::LEFT => (-step, 0.0),
                    key::RIGHT => (step, 0.0),
                    key::UP => (0.0, -step),
                    _ => (0.0, step),
                };
                match self.selected.get() {
                    Some(Selected::Annot(i)) => self.edit(|d| d.annotations[i].translate(dx, dy)),
                    Some(Selected::Layer(i)) => self.edit(|d| d.layers[i].at = d.layers[i].at.offset(dx, dy)),
                    None => return false,
                }
            }
            _ => {
                let Some(c) = chars.chars().next() else { return false };
                let Some((tool, ..)) = TOOLS.iter().find(|t| t.3 == c) else { return false };
                self.set_tool(*tool);
            }
        }
        true
    }

    fn magnify(&self, _view: &View, ev: &NSEvent) {
        self.zoom_by(1.0 + ev.magnification() as f32);
    }

    fn scroll(&self, _view: &View, ev: &NSEvent) {
        if ui::has_mod(ev, NSEventModifierFlags::Command) {
            return self.zoom_by(1.0 + ev.scrollingDeltaY() as f32 * 0.01);
        }
        if self.zoom.get() > 1.0 {
            let p = self.pan.get();
            self.pan.set(p.offset(ev.scrollingDeltaX() as f32, ev.scrollingDeltaY() as f32));
            self.canvas.redraw();
        }
    }

    /// Dropping images onto the canvas combines them into one picture.
    fn drop_files(&self, _view: &View, paths: Vec<std::path::PathBuf>, at: Pt) -> bool {
        let p = self.to_doc(at);
        let images: Vec<RgbaImage> = paths.iter().filter_map(|f| image::open(f).ok()).map(|i| i.into_rgba8()).collect();
        if images.is_empty() {
            return false;
        }
        self.edit(|d| {
            for (i, img) in images.into_iter().enumerate() {
                let off = i as f32 * 24.0;
                let at = Pt::new(p.x - img.width() as f32 / 2.0 + off, p.y - img.height() as f32 / 2.0 + off);
                d.layers.push(Layer { image: Arc::new(img), at });
            }
        });
        self.set_tool(Tool::Select);
        self.selected.set(Some(Selected::Layer(self.doc.borrow().layers.len() - 1)));
        true
    }
}

impl Editor {
    fn zoom_by(&self, k: f32) {
        self.zoom.set((self.zoom.get() * k).clamp(0.25, 16.0));
        if self.zoom.get() <= 1.0 {
            self.pan.set(Pt::default());
        }
        self.canvas.redraw();
    }

    /// ⌘V: an image on the clipboard becomes a new layer.
    fn paste(&self) {
        let Some(img) = ui::pasted_image() else { return };
        let c = self.doc.borrow().visible_bounds().center();
        let at = Pt::new(c.x - img.width() as f32 / 2.0, c.y - img.height() as f32 / 2.0);
        self.edit(|d| d.layers.push(Layer { image: Arc::new(img), at }));
    }
}

/// The color button: a dot in the current color that opens the palette.
struct ColorDot(Weak<Editor>);

impl ViewDelegate for ColorDot {
    fn draw(&self, view: &View, c: &CGContext) {
        let Some(ed) = self.0.upgrade() else { return };
        let b = view.bounds();
        let r = Rect::new(2.0, 2.0, b.size.width as f32 - 4.0, b.size.height as f32 - 4.0);
        gfx::fill_ellipse(c, r, rgba(ed.color.get()));
        gfx::stroke_ellipse(c, r, 1.0, (0.0, 0.0, 0.0, 0.25));
        gfx::stroke_ellipse(c, r.inflate(-2.0), 1.5, (1.0, 1.0, 1.0, 0.7));
    }

    fn mouse_down(&self, view: &View, _p: Pt, _ev: &NSEvent) {
        let Some(ed) = self.0.upgrade() else { return };
        let menu = ed.color_menu();
        let h = view.bounds().size.height;
        menu.popUpMenuPositioningItem_atLocation_inView(None, NSPoint::new(0.0, h + 4.0), Some(view));
    }
}

/// The "Drag me" button: drags the finished image out of the editor.
struct DragMe(Weak<Editor>);

impl ViewDelegate for DragMe {
    fn draw(&self, view: &View, _c: &CGContext) {
        let b = view.bounds();
        let r = Rect::new(0.0, 0.0, b.size.width as f32, b.size.height as f32);
        let path = objc2_app_kit::NSBezierPath::bezierPathWithRoundedRect_xRadius_yRadius(
            ui::ns_rect(r.inflate(-1.0)),
            6.0,
            6.0,
        );
        gfx::ns_color((0.5, 0.5, 0.55, 0.25)).setFill();
        path.fill();
        let s = gfx::text_size("✥ Drag me", 12.0, true);
        gfx::text(
            "✥ Drag me",
            (r.w - s.width as f32) / 2.0,
            (r.h - s.height as f32) / 2.0,
            12.0,
            true,
            (0.5, 0.5, 0.55, 1.0),
        );
    }

    fn mouse_dragged(&self, view: &View, _p: Pt, ev: &NSEvent) {
        let Some(ed) = self.0.upgrade() else { return };
        let img = ed.final_image();
        if let Some(path) = crate::app::temp_png(&img) {
            view.drag_file(&path, &ui::nsimage(&img), view.bounds(), ev);
        }
    }
}

// ---------------------------------------------------------------- helpers

fn rgba(c: Color) -> gfx::Rgba {
    (c.r as f64 / 255.0, c.g as f64 / 255.0, c.b as f64 / 255.0, c.a as f64 / 255.0)
}

fn from_ns(c: &objc2_app_kit::NSColor) -> Option<Color> {
    let c = c.colorUsingColorSpace(&NSColorSpace::sRGBColorSpace())?;
    let b = |v: f64| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
    Some(Color::rgba(b(c.redComponent()), b(c.greenComponent()), b(c.blueComponent()), b(c.alphaComponent())))
}

/// Shift-drag: squares for boxes, 45° steps for lines and arrows.
fn constrain_pt(tool: Tool, start: Pt, end: Pt) -> Pt {
    let (dx, dy) = (end.x - start.x, end.y - start.y);
    match tool {
        Tool::Line | Tool::Arrow => {
            let angle = dy.atan2(dx);
            let snapped = (angle / std::f32::consts::FRAC_PI_4).round() * std::f32::consts::FRAC_PI_4;
            let len = dx.hypot(dy);
            Pt::new(start.x + len * snapped.cos(), start.y + len * snapped.sin())
        }
        _ => {
            let s = dx.abs().max(dy.abs());
            Pt::new(start.x + s.copysign(dx), start.y + s.copysign(dy))
        }
    }
}

/// Draggable points of a shape: endpoints, plus the curve control point.
fn edit_points(a: &Annotation) -> Vec<Pt> {
    match &a.shape {
        Shape::Arrow { from, to, ctrl: Some(c), style: ArrowStyle::Curved } => vec![*from, *to, *c],
        Shape::Arrow { from, to, .. } | Shape::Line { from, to } => vec![*from, *to],
        _ => vec![],
    }
}

fn move_point(a: &mut Annotation, which: u8, p: Pt) {
    match (&mut a.shape, which) {
        (Shape::Arrow { from, .. } | Shape::Line { from, .. }, 0) => *from = p,
        (Shape::Arrow { to, .. } | Shape::Line { to, .. }, 1) => *to = p,
        (Shape::Arrow { ctrl, .. }, 2) => *ctrl = Some(p),
        _ => {}
    }
}

/// Light backdrop so transparent pixels read as "empty".
fn checker(c: &CGContext, r: Rect) {
    gfx::fill(c, r, (0.88, 0.88, 0.9, 1.0));
}
