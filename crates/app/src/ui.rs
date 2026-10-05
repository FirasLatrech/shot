//! The small AppKit toolkit every window is built from:
//! - [`Target`]: an action target that runs a Rust closure.
//! - [`View`]: a flipped NSView that forwards drawing and input to a [`ViewDelegate`].
//! - [`KeyWindow`]: a window that can become key even when borderless.

use std::{
    cell::RefCell,
    path::PathBuf,
    rc::{Rc, Weak},
};

use objc2::{
    define_class, msg_send,
    rc::Retained,
    runtime::{AnyObject, ProtocolObject},
    sel, AnyThread, DefinedClass, MainThreadMarker, MainThreadOnly, Message,
};
use objc2_app_kit::{
    NSApplication, NSBackingStoreType, NSBox, NSButton, NSColor, NSDragOperation, NSDraggingContext, NSDraggingInfo,
    NSDraggingItem, NSDraggingSession, NSDraggingSource, NSEvent, NSEventModifierFlags, NSGraphicsContext, NSImage,
    NSMenu, NSMenuItem, NSPanel, NSPasteboard, NSPasteboardTypeFileURL, NSPasteboardTypePNG, NSPasteboardTypeString,
    NSStackView, NSTextField, NSTrackingArea, NSTrackingAreaOptions, NSUserInterfaceLayoutOrientation, NSView,
    NSVisualEffectBlendingMode, NSVisualEffectMaterial, NSVisualEffectState, NSVisualEffectView, NSWindow,
    NSWindowDelegate, NSWindowStyleMask,
};
use objc2_core_foundation::{CGPoint, CGSize};
use objc2_core_graphics::CGContext;
use objc2_foundation::{
    NSArray, NSData, NSNotification, NSObject, NSObjectProtocol, NSPoint, NSRect, NSSize, NSString, NSURL,
};
use shot_core::{Pt, Rect};

// ---------------------------------------------------------------- Target

type Action = Box<dyn Fn(Option<&AnyObject>)>;

define_class!(
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[ivars = Action]
    pub struct Target;

    impl Target {
        #[unsafe(method(fire:))]
        fn fire(&self, sender: Option<&AnyObject>) {
            (self.ivars())(sender);
        }
    }

    unsafe impl NSObjectProtocol for Target {}
);

impl Target {
    pub fn new(mtm: MainThreadMarker, f: impl Fn(Option<&AnyObject>) + 'static) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(Box::new(f));
        unsafe { msg_send![super(this), init] }
    }
}

/// Controls hold their target weakly; whoever builds a window keeps its
/// targets alive in one of these.
#[derive(Default)]
pub struct Targets(RefCell<Vec<Retained<Target>>>);

impl Targets {
    pub fn add(&self, mtm: MainThreadMarker, f: impl Fn(Option<&AnyObject>) + 'static) -> Retained<Target> {
        let t = Target::new(mtm, f);
        self.0.borrow_mut().push(t.clone());
        t
    }
}

/// The control that sent an action, if it is a `T`.
pub fn sender<T: objc2::DowncastTarget>(sender: Option<&AnyObject>) -> Option<&T> {
    sender?.downcast_ref::<T>()
}

// ---------------------------------------------------------------- View

/// Input and drawing callbacks; points are in the view's flipped coordinates.
#[allow(unused_variables)]
pub trait ViewDelegate {
    fn draw(&self, view: &View, cg: &CGContext) {}
    fn mouse_down(&self, view: &View, p: Pt, ev: &NSEvent) {}
    fn mouse_dragged(&self, view: &View, p: Pt, ev: &NSEvent) {}
    fn mouse_up(&self, view: &View, p: Pt, ev: &NSEvent) {}
    fn mouse_moved(&self, view: &View, p: Pt) {}
    fn mouse_entered(&self, view: &View) {}
    fn mouse_exited(&self, view: &View) {}
    fn right_mouse_down(&self, view: &View, p: Pt, ev: &NSEvent) {}
    fn scroll(&self, view: &View, ev: &NSEvent) {}
    fn magnify(&self, view: &View, ev: &NSEvent) {}
    /// Return `true` when handled; unhandled keys go up the responder chain.
    fn key_down(&self, view: &View, ev: &NSEvent) -> bool {
        false
    }
    fn flags_changed(&self, view: &View, ev: &NSEvent) {}
    /// Files dropped onto the view; return `true` to accept.
    fn drop_files(&self, view: &View, paths: Vec<PathBuf>, at: Pt) -> bool {
        false
    }
}

define_class!(
    #[unsafe(super(NSView))]
    #[thread_kind = MainThreadOnly]
    #[ivars = RefCell<Option<Weak<dyn ViewDelegate>>>]
    pub struct View;

    impl View {
        #[unsafe(method(isFlipped))]
        fn is_flipped(&self) -> bool {
            true
        }

        #[unsafe(method(acceptsFirstResponder))]
        fn accepts_first_responder(&self) -> bool {
            true
        }

        #[unsafe(method(acceptsFirstMouse:))]
        fn accepts_first_mouse(&self, _ev: Option<&NSEvent>) -> bool {
            true
        }

        #[unsafe(method(drawRect:))]
        fn draw_rect(&self, _dirty: NSRect) {
            let (Some(d), Some(ctx)) = (self.delegate(), NSGraphicsContext::currentContext()) else { return };
            d.draw(self, &ctx.CGContext());
        }

        #[unsafe(method(mouseDown:))]
        fn mouse_down(&self, ev: &NSEvent) {
            if let Some(d) = self.delegate() {
                d.mouse_down(self, self.point(ev), ev);
            }
        }

        #[unsafe(method(mouseDragged:))]
        fn mouse_dragged(&self, ev: &NSEvent) {
            if let Some(d) = self.delegate() {
                d.mouse_dragged(self, self.point(ev), ev);
            }
        }

        #[unsafe(method(mouseUp:))]
        fn mouse_up(&self, ev: &NSEvent) {
            if let Some(d) = self.delegate() {
                d.mouse_up(self, self.point(ev), ev);
            }
        }

        #[unsafe(method(mouseMoved:))]
        fn mouse_moved(&self, ev: &NSEvent) {
            if let Some(d) = self.delegate() {
                d.mouse_moved(self, self.point(ev));
            }
        }

        #[unsafe(method(mouseEntered:))]
        fn mouse_entered(&self, _ev: &NSEvent) {
            if let Some(d) = self.delegate() {
                d.mouse_entered(self);
            }
        }

        #[unsafe(method(mouseExited:))]
        fn mouse_exited(&self, _ev: &NSEvent) {
            if let Some(d) = self.delegate() {
                d.mouse_exited(self);
            }
        }

        #[unsafe(method(rightMouseDown:))]
        fn right_mouse_down(&self, ev: &NSEvent) {
            if let Some(d) = self.delegate() {
                d.right_mouse_down(self, self.point(ev), ev);
            }
        }

        #[unsafe(method(scrollWheel:))]
        fn scroll_wheel(&self, ev: &NSEvent) {
            if let Some(d) = self.delegate() {
                d.scroll(self, ev);
            }
        }

        #[unsafe(method(magnifyWithEvent:))]
        fn magnify_with_event(&self, ev: &NSEvent) {
            if let Some(d) = self.delegate() {
                d.magnify(self, ev);
            }
        }

        #[unsafe(method(keyDown:))]
        fn key_down(&self, ev: &NSEvent) {
            let handled = self.delegate().is_some_and(|d| d.key_down(self, ev));
            if !handled {
                unsafe { msg_send![super(self), keyDown: ev] }
            }
        }

        #[unsafe(method(flagsChanged:))]
        fn flags_changed(&self, ev: &NSEvent) {
            if let Some(d) = self.delegate() {
                d.flags_changed(self, ev);
            }
        }

        #[unsafe(method(draggingEntered:))]
        fn dragging_entered(&self, _info: &ProtocolObject<dyn NSDraggingInfo>) -> NSDragOperation {
            NSDragOperation::Copy
        }

        #[unsafe(method(performDragOperation:))]
        fn perform_drag_operation(&self, info: &ProtocolObject<dyn NSDraggingInfo>) -> bool {
            let at = self.convertPoint_fromView(info.draggingLocation(), None);
            let paths = file_urls(&info.draggingPasteboard());
            self.delegate().is_some_and(|d| d.drop_files(self, paths, Pt::new(at.x as f32, at.y as f32)))
        }
    }

    unsafe impl NSObjectProtocol for View {}

    unsafe impl NSDraggingSource for View {
        #[unsafe(method(draggingSession:sourceOperationMaskForDraggingContext:))]
        fn source_operation_mask(&self, _s: &NSDraggingSession, _c: NSDraggingContext) -> NSDragOperation {
            NSDragOperation::Copy
        }
    }
);

impl View {
    pub fn new(mtm: MainThreadMarker, frame: NSRect, delegate: Weak<dyn ViewDelegate>) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(RefCell::new(Some(delegate)));
        let view: Retained<Self> = unsafe { msg_send![super(this), initWithFrame: frame] };
        let opts = NSTrackingAreaOptions::MouseEnteredAndExited
            | NSTrackingAreaOptions::MouseMoved
            | NSTrackingAreaOptions::ActiveAlways
            | NSTrackingAreaOptions::InVisibleRect;
        let area = unsafe {
            NSTrackingArea::initWithRect_options_owner_userInfo(
                NSTrackingArea::alloc(),
                NSRect::ZERO,
                opts,
                Some(&view),
                None,
            )
        };
        view.addTrackingArea(&area);
        view
    }

    fn delegate(&self) -> Option<Rc<dyn ViewDelegate>> {
        self.ivars().borrow().as_ref()?.upgrade()
    }

    fn point(&self, ev: &NSEvent) -> Pt {
        let p = self.convertPoint_fromView(ev.locationInWindow(), None);
        Pt::new(p.x as f32, p.y as f32)
    }

    pub fn redraw(&self) {
        self.setNeedsDisplay(true);
    }

    pub fn accept_file_drops(&self) {
        self.registerForDraggedTypes(&NSArray::from_slice(&[unsafe { NSPasteboardTypeFileURL }]));
    }

    /// Starts dragging `file` out of the view (to Finder, Slack, …).
    pub fn drag_file(&self, file: &std::path::Path, preview: &NSImage, frame: NSRect, ev: &NSEvent) {
        let url = NSURL::fileURLWithPath(&NSString::from_str(&file.to_string_lossy()));
        let item = NSDraggingItem::initWithPasteboardWriter(NSDraggingItem::alloc(), ProtocolObject::from_ref(&*url));
        unsafe { item.setDraggingFrame_contents(frame, Some(preview)) };
        self.beginDraggingSessionWithItems_event_source(
            &NSArray::from_retained_slice(&[item]),
            ev,
            ProtocolObject::from_ref(self),
        );
    }
}

fn file_urls(pb: &NSPasteboard) -> Vec<PathBuf> {
    let Some(items) = pb.pasteboardItems() else { return vec![] };
    items
        .iter()
        .filter_map(|item| {
            let s = item.stringForType(unsafe { NSPasteboardTypeFileURL })?;
            let url = NSURL::URLWithString(&s)?;
            Some(PathBuf::from(url.path()?.to_string()))
        })
        .collect()
}

// ---------------------------------------------------------------- Windows

define_class!(
    /// Borderless windows can't become key by default; overlays, pins and
    /// the capture UI need keyboard input, so they use this subclass.
    #[unsafe(super(NSWindow))]
    #[thread_kind = MainThreadOnly]
    pub struct KeyWindow;

    impl KeyWindow {
        #[unsafe(method(canBecomeKeyWindow))]
        fn can_become_key(&self) -> bool {
            true
        }

        #[unsafe(method(canBecomeMainWindow))]
        fn can_become_main(&self) -> bool {
            true
        }
    }
);

define_class!(
    /// Panel variant for overlays, pins and the capture UI: it can be
    /// non-activating, so clicking it doesn't steal focus from other apps.
    #[unsafe(super(NSPanel, NSWindow))]
    #[thread_kind = MainThreadOnly]
    pub struct KeyPanel;

    impl KeyPanel {
        #[unsafe(method(canBecomeKeyWindow))]
        fn can_become_key(&self) -> bool {
            true
        }
    }
);

/// A window at `frame` (AppKit screen coordinates).
pub fn window(mtm: MainThreadMarker, frame: NSRect, style: NSWindowStyleMask) -> Retained<NSWindow> {
    let w: Retained<KeyWindow> = unsafe {
        msg_send![KeyWindow::alloc(mtm), initWithContentRect: frame, styleMask: style, backing: NSBackingStoreType::Buffered, defer: false]
    };
    unsafe { w.setReleasedWhenClosed(false) };
    Retained::into_super(w)
}

/// A transparent, shadowless, borderless panel that stays out of screenshots.
pub fn overlay_window(mtm: MainThreadMarker, frame: NSRect) -> Retained<NSWindow> {
    let style = NSWindowStyleMask::Borderless | NSWindowStyleMask::NonactivatingPanel;
    let p: Retained<KeyPanel> = unsafe {
        msg_send![KeyPanel::alloc(mtm), initWithContentRect: frame, styleMask: style, backing: NSBackingStoreType::Buffered, defer: false]
    };
    unsafe { p.setReleasedWhenClosed(false) };
    p.setHidesOnDeactivate(false);
    let w: Retained<NSWindow> = Retained::into_super(Retained::into_super(p));
    // Appear instantly: the default "zoom in" animation reads as a blob on full-screen overlays.
    w.setAnimationBehavior(objc2_app_kit::NSWindowAnimationBehavior::None);
    w.setOpaque(false);
    w.setBackgroundColor(Some(&NSColor::clearColor()));
    w.setHasShadow(false);
    // Hidden from screen captures; SHOT_DEV_VISIBLE=1 shows them for UI work.
    if std::env::var_os("SHOT_DEV_VISIBLE").is_none() {
        w.setSharingType(objc2_app_kit::NSWindowSharingType::None);
    }
    w
}

pub fn activate(mtm: MainThreadMarker) {
    #[allow(deprecated)]
    NSApplication::sharedApplication(mtm).activateIgnoringOtherApps(true);
}

define_class!(
    /// Runs a closure when its window closes.
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[ivars = Box<dyn Fn()>]
    pub struct WindowObserver;

    unsafe impl NSObjectProtocol for WindowObserver {}

    unsafe impl NSWindowDelegate for WindowObserver {
        #[unsafe(method(windowWillClose:))]
        fn window_will_close(&self, _n: &NSNotification) {
            (self.ivars())();
        }
    }
);

/// Calls `f` when `window` closes. The window holds its delegate weakly, so
/// keep the returned observer alive as long as the window.
pub fn on_close(window: &NSWindow, f: impl Fn() + 'static) -> Retained<WindowObserver> {
    let mtm = window.mtm();
    let obs = WindowObserver::alloc(mtm).set_ivars(Box::new(f) as Box<dyn Fn()>);
    let obs: Retained<WindowObserver> = unsafe { msg_send![super(obs), init] };
    window.setDelegate(Some(ProtocolObject::from_ref(&*obs)));
    obs
}

/// A standard titled, resizable app window, centered on screen.
pub fn app_window(mtm: MainThreadMarker, title: &str, w: f64, h: f64) -> Retained<NSWindow> {
    let style = NSWindowStyleMask::Titled
        | NSWindowStyleMask::Closable
        | NSWindowStyleMask::Miniaturizable
        | NSWindowStyleMask::Resizable;
    let win = window(mtm, NSRect::new(NSPoint::ZERO, NSSize::new(w, h)), style);
    win.setTitle(&NSString::from_str(title));
    win.center();
    win
}

/// Pins a view's width; stack views ignore plain frame sizes.
pub fn set_width(v: &NSView, w: f64) {
    v.widthAnchor().constraintEqualToConstant(w).setActive(true);
}

/// Adds `bar` to `parent` as a full-width strip of `height` along its top or bottom edge.
pub fn pin_bar(bar: &NSView, parent: &NSView, top: bool, height: f64) {
    parent.addSubview(bar);
    bar.setTranslatesAutoresizingMaskIntoConstraints(false);
    bar.leadingAnchor().constraintEqualToAnchor(&parent.leadingAnchor()).setActive(true);
    bar.trailingAnchor().constraintEqualToAnchor(&parent.trailingAnchor()).setActive(true);
    bar.heightAnchor().constraintEqualToConstant(height).setActive(true);
    if top {
        bar.topAnchor().constraintEqualToAnchor(&parent.topAnchor()).setActive(true);
    } else {
        bar.bottomAnchor().constraintEqualToAnchor(&parent.bottomAnchor()).setActive(true);
    }
}

/// A translucent bar (title-bar material) holding `content` inset by `insets`,
/// with a hairline separator on its inner edge.
pub fn bar(
    content: &[Retained<NSView>],
    spacing: f64,
    insets: objc2_foundation::NSEdgeInsets,
    separator_on_top: bool,
) -> Retained<NSView> {
    let mtm = MainThreadMarker::new().expect("main thread");
    let fx = NSVisualEffectView::initWithFrame(NSVisualEffectView::alloc(mtm), NSRect::ZERO);
    fx.setMaterial(NSVisualEffectMaterial::Titlebar);
    fx.setBlendingMode(NSVisualEffectBlendingMode::WithinWindow);
    fx.setState(NSVisualEffectState::FollowsWindowActiveState);
    let row = stack(content, true, spacing);
    row.setEdgeInsets(insets);
    fx.addSubview(&row);
    row.setTranslatesAutoresizingMaskIntoConstraints(false);
    row.leadingAnchor().constraintEqualToAnchor(&fx.leadingAnchor()).setActive(true);
    row.trailingAnchor().constraintEqualToAnchor(&fx.trailingAnchor()).setActive(true);
    row.topAnchor().constraintEqualToAnchor(&fx.topAnchor()).setActive(true);
    row.bottomAnchor().constraintEqualToAnchor(&fx.bottomAnchor()).setActive(true);

    let line = NSBox::initWithFrame(NSBox::alloc(mtm), NSRect::ZERO);
    line.setBoxType(objc2_app_kit::NSBoxType::Separator);
    fx.addSubview(&line);
    line.setTranslatesAutoresizingMaskIntoConstraints(false);
    line.leadingAnchor().constraintEqualToAnchor(&fx.leadingAnchor()).setActive(true);
    line.trailingAnchor().constraintEqualToAnchor(&fx.trailingAnchor()).setActive(true);
    line.heightAnchor().constraintEqualToConstant(1.0).setActive(true);
    if separator_on_top {
        line.topAnchor().constraintEqualToAnchor(&fx.topAnchor()).setActive(true);
    } else {
        line.bottomAnchor().constraintEqualToAnchor(&fx.bottomAnchor()).setActive(true);
    }
    Retained::into_super(fx)
}

/// An empty view that soaks up extra space in a stack.
pub fn flex_space() -> Retained<NSView> {
    let v = NSView::initWithFrame(NSView::alloc(MainThreadMarker::new().expect("main thread")), NSRect::ZERO);
    v.setContentHuggingPriority_forOrientation(1.0, objc2_app_kit::NSLayoutConstraintOrientation::Horizontal);
    v
}

/// A borderless icon button that opens `menu` underneath itself.
pub fn menu_button(
    mtm: MainThreadMarker,
    targets: &Targets,
    symbol_name: &str,
    tip: &str,
    menu: Retained<NSMenu>,
) -> Retained<NSButton> {
    let b = icon_button(mtm, targets, symbol_name, tip, || {});
    let anchor = b.clone();
    let t = targets.add(mtm, move |_| {
        let h = anchor.bounds().size.height;
        menu.popUpMenuPositioningItem_atLocation_inView(None, NSPoint::new(0.0, h + 4.0), Some(&anchor));
    });
    unsafe { b.setTarget(Some(&t)) };
    b
}

/// The Shot mark as a template image `height` points tall; macOS tints it
/// for light and dark menu bars.
pub fn logo_template(height: f64) -> Retained<NSImage> {
    let k = 3.0; // supersample for crisp edges at any scale
    let w = height * shot_core::logo::ASPECT as f64;
    let mut pm = tiny_skia::Pixmap::new((w * k).ceil() as u32, (height * k).ceil() as u32).expect("non-zero");
    let mut p = tiny_skia::Paint::default();
    p.set_color(tiny_skia::Color::BLACK);
    p.anti_alias = true;
    let path = shot_core::logo::path(0.0, 0.0, (height * k) as f32);
    pm.fill_path(&path, &p, tiny_skia::FillRule::Winding, tiny_skia::Transform::identity(), None);
    let img = nsimage(&shot_core::render::to_image(&pm));
    img.setSize(NSSize::new(w, height));
    img.setTemplate(true);
    img
}

/// A small filled circle, used for color swatches in menus.
pub fn color_dot(c: shot_core::Color, size: u32) -> Retained<NSImage> {
    let k = 2; // draw at 2× for Retina
    let mut pm = tiny_skia::Pixmap::new(size * k, size * k).expect("non-zero");
    let r = (size * k) as f32 / 2.0;
    if let Some(path) = tiny_skia::PathBuilder::from_circle(r, r, r - 1.0) {
        let mut p = tiny_skia::Paint::default();
        p.set_color(c.to_skia());
        p.anti_alias = true;
        pm.fill_path(&path, &p, tiny_skia::FillRule::Winding, tiny_skia::Transform::identity(), None);
        p.set_color(tiny_skia::Color::from_rgba8(0, 0, 0, 50));
        let stroke = tiny_skia::Stroke { width: 1.5, ..Default::default() };
        pm.stroke_path(&path, &p, &stroke, tiny_skia::Transform::identity(), None);
    }
    let img = nsimage(&shot_core::render::to_image(&pm));
    img.setSize(NSSize::new(size as f64, size as f64));
    img
}

/// A horizontal line of `width` px as a template image, for stroke-width menus.
pub fn line_image(width: f32) -> Retained<NSImage> {
    let mut pm = tiny_skia::Pixmap::new(56, 24).expect("non-zero");
    let rect = tiny_skia::Rect::from_xywh(4.0, 12.0 - width / 2.0, 48.0, width.max(1.0)).expect("valid");
    let mut p = tiny_skia::Paint::default();
    p.set_color(tiny_skia::Color::BLACK);
    pm.fill_rect(rect, &p, tiny_skia::Transform::identity(), None);
    let img = nsimage(&shot_core::render::to_image(&pm));
    img.setSize(NSSize::new(28.0, 12.0));
    img.setTemplate(true);
    img
}

pub fn view(v: &NSView) -> Retained<NSView> {
    v.retain()
}

pub fn stack(views: &[Retained<NSView>], horizontal: bool, spacing: f64) -> Retained<NSStackView> {
    let mtm = MainThreadMarker::new().expect("main thread");
    let s = NSStackView::stackViewWithViews(&NSArray::from_retained_slice(views), mtm);
    s.setOrientation(if horizontal {
        NSUserInterfaceLayoutOrientation::Horizontal
    } else {
        NSUserInterfaceLayoutOrientation::Vertical
    });
    s.setSpacing(spacing);
    s
}

pub fn label(text: &str) -> Retained<NSTextField> {
    NSTextField::labelWithString(&NSString::from_str(text), MainThreadMarker::new().expect("main thread"))
}

// ---------------------------------------------------------------- Helpers

pub fn ns_rect(r: Rect) -> NSRect {
    NSRect::new(NSPoint::new(r.x as f64, r.y as f64), NSSize::new(r.w as f64, r.h as f64))
}

/// Converts a rect in global CG coordinates (top-left origin) to AppKit
/// screen coordinates (bottom-left origin).
pub fn cg_to_screen(r: Rect, main_height: f64) -> NSRect {
    NSRect::new(CGPoint::new(r.x as f64, main_height - r.y as f64 - r.h as f64), CGSize::new(r.w as f64, r.h as f64))
}

pub fn round_corners(view: &NSView, radius: f64) {
    view.setWantsLayer(true);
    if let Some(layer) = view.layer() {
        layer.setCornerRadius(radius);
        layer.setMasksToBounds(true);
    }
}

pub fn symbol(name: &str) -> Option<Retained<NSImage>> {
    NSImage::imageWithSystemSymbolName_accessibilityDescription(&NSString::from_str(name), None)
}

/// A borderless SF Symbol button that calls `action`.
pub fn icon_button(
    mtm: MainThreadMarker,
    targets: &Targets,
    name: &str,
    tip: &str,
    action: impl Fn() + 'static,
) -> Retained<NSButton> {
    let t = targets.add(mtm, move |_| action());
    let img = symbol(name).unwrap_or_default();
    let b = unsafe { NSButton::buttonWithImage_target_action(&img, Some(&t), Some(sel!(fire:)), mtm) };
    b.setToolTip(Some(&NSString::from_str(tip)));
    b
}

/// A standard push button with a title.
pub fn text_button(
    mtm: MainThreadMarker,
    targets: &Targets,
    title: &str,
    action: impl Fn() + 'static,
) -> Retained<NSButton> {
    let t = targets.add(mtm, move |_| action());
    unsafe { NSButton::buttonWithTitle_target_action(&NSString::from_str(title), Some(&t), Some(sel!(fire:)), mtm) }
}

pub type MenuItem<'a> = (&'a str, Box<dyn Fn()>);

/// A menu of `(title, action)` items; a title of `"-"` is a separator.
pub fn menu(mtm: MainThreadMarker, targets: &Targets, items: Vec<MenuItem>) -> Retained<NSMenu> {
    let menu = NSMenu::new(mtm);
    for (title, f) in items {
        if title == "-" {
            menu.addItem(&NSMenuItem::separatorItem(mtm));
        } else {
            menu.addItem(&menu_item(mtm, targets, title, f));
        }
    }
    menu
}

pub fn menu_item(mtm: MainThreadMarker, targets: &Targets, title: &str, f: Box<dyn Fn()>) -> Retained<NSMenuItem> {
    let t = targets.add(mtm, move |_| f());
    let item = unsafe {
        NSMenuItem::initWithTitle_action_keyEquivalent(
            NSMenuItem::alloc(mtm),
            &NSString::from_str(title),
            Some(sel!(fire:)),
            &NSString::new(),
        )
    };
    unsafe { item.setTarget(Some(&t)) };
    item
}

pub fn reveal_in_finder(path: &std::path::Path) {
    let url = NSURL::fileURLWithPath(&NSString::from_str(&path.to_string_lossy()));
    objc2_app_kit::NSWorkspace::sharedWorkspace()
        .activateFileViewerSelectingURLs(&NSArray::from_retained_slice(&[url]));
}

pub fn nsimage(img: &image::RgbaImage) -> Retained<NSImage> {
    let cg = crate::cg::image_from_rgba8(img);
    NSImage::initWithCGImage_size(NSImage::alloc(), &cg, NSSize::ZERO)
}

pub fn copy_image(img: &image::RgbaImage) {
    copy_png(&shot_core::doc::encode_png(img));
}

/// Puts already-encoded PNG bytes on the clipboard.
pub fn copy_png(png: &[u8]) {
    let pb = NSPasteboard::generalPasteboard();
    pb.clearContents();
    pb.setData_forType(Some(&NSData::with_bytes(png)), unsafe { NSPasteboardTypePNG });
}

pub fn copy_text(text: &str) {
    let pb = NSPasteboard::generalPasteboard();
    pb.clearContents();
    pb.setString_forType(&NSString::from_str(text), unsafe { NSPasteboardTypeString });
}

/// First image on the pasteboard, if any.
pub fn pasted_image() -> Option<image::RgbaImage> {
    let pb = NSPasteboard::generalPasteboard();
    for ty in ["public.png", "public.tiff"] {
        if let Some(data) = pb.dataForType(&NSString::from_str(ty)) {
            if let Ok(img) = image::load_from_memory(&data.to_vec()) {
                return Some(img.into_rgba8());
            }
        }
    }
    None
}

pub fn has_mod(ev: &NSEvent, m: NSEventModifierFlags) -> bool {
    ev.modifierFlags().contains(m)
}

pub fn key_chars(ev: &NSEvent) -> String {
    ev.charactersIgnoringModifiers().map(|s| s.to_string()).unwrap_or_default()
}

/// Hardware key codes for keys that have no useful character.
pub mod key {
    pub const ESCAPE: u16 = 53;
    pub const RETURN: u16 = 36;
    pub const ENTER: u16 = 76;
    pub const DELETE: u16 = 51;
    pub const FORWARD_DELETE: u16 = 117;
    pub const SPACE: u16 = 49;
    pub const LEFT: u16 = 123;
    pub const RIGHT: u16 = 124;
    pub const DOWN: u16 = 125;
    pub const UP: u16 = 126;
}
