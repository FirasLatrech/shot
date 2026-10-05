//! Settings: General, Capture, Overlay and Shortcuts tabs. Every control
//! writes straight to `Settings` and persists immediately.

use std::{cell::RefCell, path::PathBuf, ptr::NonNull, rc::Rc};

use block2::RcBlock;
use objc2::{rc::Retained, runtime::AnyObject, sel, MainThreadOnly, Message};
use objc2_app_kit::{
    NSButton, NSEvent, NSEventMask, NSEventModifierFlags, NSGridCell, NSGridCellPlacement, NSGridRowAlignment,
    NSGridView, NSOpenPanel, NSPopUpButton, NSSlider, NSTabViewController, NSTabViewControllerTabStyle, NSTabViewItem,
    NSView, NSViewController, NSWindow, NSWindowToolbarStyle,
};
use objc2_foundation::{MainThreadMarker, NSArray, NSRect, NSString};
use shot_core::settings::{Action, Corner, ImageFormat, Settings};

use crate::{
    app::{app, pretty_shortcut},
    ui::{self, Targets, WindowObserver},
};

struct SettingsWindow {
    window: Retained<NSWindow>,
    targets: Targets,
    observer: RefCell<Option<Retained<WindowObserver>>>,
    monitor: RefCell<Option<Retained<AnyObject>>>,
}

thread_local! {
    static OPEN: RefCell<Option<Rc<SettingsWindow>>> = const { RefCell::new(None) };
}

/// Applies a settings change and saves it.
fn change(f: impl FnOnce(&mut Settings)) {
    let app = app();
    f(&mut app.settings.borrow_mut());
    app.save_settings();
}

pub fn show() {
    if let Some(w) = OPEN.with(|o| o.borrow().clone()) {
        ui::activate(w.window.mtm());
        return w.window.makeKeyAndOrderFront(None);
    }
    let mtm = MainThreadMarker::new().unwrap();
    let window = ui::app_window(mtm, "Settings", 560.0, 400.0);
    window.setStyleMask(window.styleMask() & !objc2_app_kit::NSWindowStyleMask::Resizable);
    window.setToolbarStyle(NSWindowToolbarStyle::Preference);
    let sw = Rc::new(SettingsWindow {
        window,
        targets: Targets::default(),
        observer: RefCell::default(),
        monitor: RefCell::default(),
    });

    // Toolbar tabs, like Apple's own Settings windows.
    let tabs = NSTabViewController::new(mtm);
    tabs.setTabStyle(NSTabViewControllerTabStyle::Toolbar);
    for (name, symbol, view) in [
        ("General", "gearshape", general_tab(&sw)),
        ("Capture", "camera.viewfinder", capture_tab(&sw)),
        ("Overlay", "rectangle.on.rectangle", overlay_tab(&sw)),
        ("Shortcuts", "keyboard", shortcuts_tab(&sw)),
    ] {
        let vc = NSViewController::new(mtm);
        vc.setView(&view);
        vc.setTitle(Some(&NSString::from_str(name)));
        vc.setPreferredContentSize(view.fittingSize());
        let item = NSTabViewItem::tabViewItemWithViewController(&vc);
        item.setImage(ui::symbol(symbol).as_deref());
        tabs.addTabViewItem(&item);
    }
    sw.window.setContentViewController(Some(&tabs));

    let me = Rc::downgrade(&sw);
    *sw.observer.borrow_mut() = Some(ui::on_close(&sw.window, move || {
        if let Some(sw) = me.upgrade() {
            sw.stop_recording();
        }
        crate::app::after(0.0, || drop(OPEN.with(|o| o.borrow_mut().take())));
    }));
    OPEN.with(|o| *o.borrow_mut() = Some(sw.clone()));
    ui::activate(mtm);
    sw.window.center();
    sw.window.makeKeyAndOrderFront(None);
}

// ---------------------------------------------------------------- controls

fn checkbox(
    sw: &SettingsWindow,
    title: &str,
    get: fn(&Settings) -> bool,
    set: fn(&mut Settings, bool),
) -> Retained<NSView> {
    let mtm = MainThreadMarker::new().unwrap();
    let t = sw.targets.add(mtm, move |sender| {
        let Some(b) = ui::sender::<NSButton>(sender) else { return };
        let on = b.state() == 1;
        change(|s| set(s, on));
    });
    let b = unsafe {
        NSButton::checkboxWithTitle_target_action(&NSString::from_str(title), Some(&t), Some(sel!(fire:)), mtm)
    };
    b.setState(isize::from(get(&app().settings.borrow())));
    ui::view(&b)
}

fn popup(
    sw: &SettingsWindow,
    items: &[&str],
    get: fn(&Settings) -> usize,
    set: fn(&mut Settings, usize),
) -> Retained<NSView> {
    let mtm = MainThreadMarker::new().unwrap();
    let p = NSPopUpButton::initWithFrame_pullsDown(NSPopUpButton::alloc(mtm), NSRect::ZERO, false);
    for i in items {
        p.addItemWithTitle(&NSString::from_str(i));
    }
    p.selectItemAtIndex(get(&app().settings.borrow()) as isize);
    let t = sw.targets.add(mtm, move |sender| {
        let Some(p) = ui::sender::<NSPopUpButton>(sender) else { return };
        let i = p.indexOfSelectedItem().max(0) as usize;
        change(|s| set(s, i));
    });
    unsafe {
        p.setTarget(Some(&t));
        p.setAction(Some(sel!(fire:)));
    }
    ui::view(&p)
}

/// A classic Mac settings form: right-aligned labels, controls on the right.
/// An empty label continues the group above; `""` rows with a `None` control add spacing.
fn form(rows: Vec<(&str, Retained<NSView>)>) -> Retained<NSView> {
    let mtm = MainThreadMarker::new().unwrap();
    let cells: Vec<Retained<NSArray<NSView>>> = rows
        .iter()
        .map(|(label, control)| {
            let l: Retained<NSView> =
                if label.is_empty() { NSGridCell::emptyContentView(mtm) } else { ui::view(&ui::label(label)) };
            NSArray::from_retained_slice(&[l, control.clone()])
        })
        .collect();
    let grid = NSGridView::gridViewWithViews(&NSArray::from_retained_slice(&cells), mtm);
    grid.columnAtIndex(0).setXPlacement(NSGridCellPlacement::Trailing);
    grid.setRowAlignment(NSGridRowAlignment::FirstBaseline);
    grid.setRowSpacing(10.0);
    grid.setColumnSpacing(12.0);
    // A little air above each new group.
    for (i, (label, _)) in rows.iter().enumerate().skip(1) {
        if !label.is_empty() {
            grid.rowAtIndex(i as isize).setTopPadding(10.0);
        }
    }
    let container = NSView::initWithFrame(NSView::alloc(mtm), NSRect::ZERO);
    container.addSubview(&grid);
    grid.setTranslatesAutoresizingMaskIntoConstraints(false);
    grid.topAnchor().constraintEqualToAnchor_constant(&container.topAnchor(), 24.0).setActive(true);
    grid.bottomAnchor().constraintEqualToAnchor_constant(&container.bottomAnchor(), -24.0).setActive(true);
    grid.leadingAnchor()
        .constraintGreaterThanOrEqualToAnchor_constant(&container.leadingAnchor(), 32.0)
        .setActive(true);
    grid.centerXAnchor().constraintEqualToAnchor(&container.centerXAnchor()).setActive(true);
    ui::set_width(&container, 560.0);
    container
}

// ---------------------------------------------------------------- tabs

fn general_tab(sw: &Rc<SettingsWindow>) -> Retained<NSView> {
    let mtm = MainThreadMarker::new().unwrap();
    let path = ui::label(&pretty_path(&app().save_dir()));
    path.setTextColor(Some(&objc2_app_kit::NSColor::secondaryLabelColor()));
    let path_ref = path.clone();
    let choose = ui::text_button(mtm, &sw.targets, "Choose…", move || {
        let panel = NSOpenPanel::openPanel(MainThreadMarker::new().unwrap());
        panel.setCanChooseDirectories(true);
        panel.setCanChooseFiles(false);
        panel.setCanCreateDirectories(true);
        if panel.runModal() == 1 {
            if let Some(p) = panel.URL().and_then(|u| u.path()) {
                let dir = PathBuf::from(p.to_string());
                path_ref.setStringValue(&NSString::from_str(&pretty_path(&dir)));
                change(|s| s.save_dir = Some(dir));
            }
        }
    });
    form(vec![
        ("Save screenshots to:", ui::view(&ui::stack(&[ui::view(&path), ui::view(&choose)], true, 8.0))),
        (
            "File format:",
            popup(
                sw,
                &["PNG", "JPEG"],
                |s| (s.format == ImageFormat::Jpeg) as usize,
                |s, i| s.format = if i == 1 { ImageFormat::Jpeg } else { ImageFormat::Png },
            ),
        ),
        ("After capture:", checkbox(sw, "Show Quick Access Overlay", |s| s.show_overlay, |s, v| s.show_overlay = v)),
        ("", checkbox(sw, "Copy to clipboard", |s| s.copy_after_capture, |s, v| s.copy_after_capture = v)),
        ("", checkbox(sw, "Save to disk", |s| s.save_after_capture, |s, v| s.save_after_capture = v)),
        ("Sounds:", checkbox(sw, "Play capture sounds", |s| s.play_sound, |s, v| s.play_sound = v)),
        (
            "Keep history for:",
            popup(
                sw,
                &["1 day", "1 week", "1 month"],
                |s| match s.history_days {
                    0..=1 => 0,
                    2..=7 => 1,
                    _ => 2,
                },
                |s, i| s.history_days = [1, 7, 30][i],
            ),
        ),
    ])
}

fn capture_tab(sw: &Rc<SettingsWindow>) -> Retained<NSView> {
    form(vec![
        ("Area selection:", checkbox(sw, "Show crosshair", |s| s.show_crosshair, |s, v| s.show_crosshair = v)),
        ("", checkbox(sw, "Show magnifier", |s| s.show_magnifier, |s, v| s.show_magnifier = v)),
        ("", checkbox(sw, "Freeze screen while selecting", |s| s.freeze_screen, |s, v| s.freeze_screen = v)),
        ("Window capture:", checkbox(sw, "Include window shadow", |s| s.window_shadow, |s, v| s.window_shadow = v)),
        (
            "",
            checkbox(
                sw,
                "Add background",
                |s| s.window_background.is_some(),
                |s, v| {
                    s.window_background =
                        v.then(|| s.background_presets.first().map(|p| p.1.clone()).unwrap_or_default())
                },
            ),
        ),
        (
            "Self-timer:",
            popup(
                sw,
                &["3 seconds", "5 seconds", "10 seconds"],
                |s| match s.self_timer_secs {
                    0..=3 => 0,
                    4..=5 => 1,
                    _ => 2,
                },
                |s, i| s.self_timer_secs = [3, 5, 10][i],
            ),
        ),
    ])
}

fn overlay_tab(sw: &Rc<SettingsWindow>) -> Retained<NSView> {
    let mtm = MainThreadMarker::new().unwrap();
    let t = sw.targets.add(mtm, |sender| {
        let Some(s) = ui::sender::<NSSlider>(sender) else { return };
        let v = s.doubleValue() as f32;
        change(|st| st.overlay_scale = v);
    });
    let scale = app().settings.borrow().overlay_scale as f64;
    let size = unsafe {
        NSSlider::sliderWithValue_minValue_maxValue_target_action(scale, 0.6, 1.8, Some(&t), Some(sel!(fire:)), mtm)
    };
    // Save once on release, not on every tick of the drag.
    size.setContinuous(false);
    ui::set_width(&size, 200.0);
    form(vec![
        (
            "Position:",
            popup(
                sw,
                &["Bottom left", "Bottom right", "Top left", "Top right"],
                |s| s.overlay_corner as usize,
                |s, i| {
                    s.overlay_corner = [Corner::BottomLeft, Corner::BottomRight, Corner::TopLeft, Corner::TopRight][i]
                },
            ),
        ),
        ("Size:", ui::view(&size)),
        (
            "Close automatically:",
            popup(
                sw,
                &["Never", "After 5 seconds", "After 10 seconds", "After 30 seconds"],
                |s| match s.overlay_auto_close {
                    None => 0,
                    Some(0..=5) => 1,
                    Some(6..=10) => 2,
                    _ => 3,
                },
                |s, i| s.overlay_auto_close = [None, Some(5), Some(10), Some(30)][i],
            ),
        ),
    ])
}

fn shortcuts_tab(sw: &Rc<SettingsWindow>) -> Retained<NSView> {
    let mtm = MainThreadMarker::new().unwrap();
    let mut rows = vec![];
    for action in Action::ALL {
        let me = Rc::downgrade(sw);
        let record = ui::text_button(mtm, &sw.targets, &shortcut_title(action), || {});
        let button = record.clone();
        let t = sw.targets.add(mtm, move |_| {
            if let Some(sw) = me.upgrade() {
                sw.record(action, &button);
            }
        });
        unsafe { record.setTarget(Some(&t)) };
        ui::set_width(&record, 150.0);
        let button = record.clone();
        let clear = ui::icon_button(mtm, &sw.targets, "xmark.circle.fill", "Remove shortcut", move || {
            change(|s| {
                s.hotkeys.remove(&action);
            });
            app().register_hotkeys();
            app().rebuild_menu();
            button.setTitle(&NSString::from_str(&shortcut_title(action)));
        });
        clear.setBordered(false);
        clear.setContentTintColor(Some(&objc2_app_kit::NSColor::tertiaryLabelColor()));
        rows.push((action.label(), ui::view(&ui::stack(&[ui::view(&record), ui::view(&clear)], true, 6.0))));
    }
    let hint = ui::label("Click a shortcut, then press the new keys. Esc cancels.");
    hint.setTextColor(Some(&objc2_app_kit::NSColor::secondaryLabelColor()));
    hint.setFont(Some(&objc2_app_kit::NSFont::systemFontOfSize(11.0)));
    rows.push(("", ui::view(&hint)));
    form(rows)
}

/// "~/Desktop" instead of "/Users/me/Desktop".
fn pretty_path(p: &std::path::Path) -> String {
    match (dirs::home_dir(), p.to_string_lossy()) {
        (Some(home), s) if s.starts_with(&*home.to_string_lossy()) => s.replacen(&*home.to_string_lossy(), "~", 1),
        (_, s) => s.into_owned(),
    }
}

fn shortcut_title(action: Action) -> String {
    app().settings.borrow().hotkeys.get(&action).map(|s| pretty_shortcut(s)).unwrap_or_else(|| "Record Shortcut".into())
}

impl SettingsWindow {
    /// Captures the next key combination as the shortcut for `action`.
    fn record(self: &Rc<Self>, action: Action, button: &NSButton) {
        self.stop_recording();
        button.setTitle(&NSString::from_str("Type shortcut…"));
        // Our own hotkeys would swallow the combination being recorded.
        app().suspend_hotkeys();
        let me = Rc::downgrade(self);
        let button = button.retain();
        let block = RcBlock::new(move |ev: NonNull<NSEvent>| -> *mut NSEvent {
            let ev = unsafe { ev.as_ref() };
            let Some(sw) = me.upgrade() else { return ev as *const _ as *mut _ };
            if ev.keyCode() == ui::key::ESCAPE {
                button.setTitle(&NSString::from_str(&shortcut_title(action)));
            } else if let Some(spec) = hotkey_spec(ev) {
                change(|s| {
                    s.hotkeys.retain(|a, v| *a == action || *v != spec);
                    s.hotkeys.insert(action, spec);
                });
                button.setTitle(&NSString::from_str(&shortcut_title(action)));
                app().rebuild_menu();
            } else {
                // Needs a modifier; keep waiting.
                return std::ptr::null_mut();
            }
            sw.stop_recording();
            std::ptr::null_mut()
        });
        let monitor = unsafe { NSEvent::addLocalMonitorForEventsMatchingMask_handler(NSEventMask::KeyDown, &block) };
        *self.monitor.borrow_mut() = monitor;
    }

    fn stop_recording(&self) {
        if let Some(m) = self.monitor.borrow_mut().take() {
            unsafe { NSEvent::removeMonitor(&m) };
            app().register_hotkeys();
        }
    }
}

/// "shift+cmd+KeyA"-style spec for global-hotkey, or `None` without a modifier.
fn hotkey_spec(ev: &NSEvent) -> Option<String> {
    let flags = ev.modifierFlags();
    let key = key_name(ev.keyCode())?;
    let mut parts = vec![];
    if flags.contains(NSEventModifierFlags::Shift) {
        parts.push("shift");
    }
    if flags.contains(NSEventModifierFlags::Control) {
        parts.push("ctrl");
    }
    if flags.contains(NSEventModifierFlags::Option) {
        parts.push("alt");
    }
    if flags.contains(NSEventModifierFlags::Command) {
        parts.push("cmd");
    }
    let is_fn_key = key.starts_with('F') && key.len() <= 3;
    if parts.iter().all(|p| *p == "shift") && !is_fn_key {
        return None;
    }
    parts.push(key);
    Some(parts.join("+"))
}

/// macOS virtual key code → W3C key code name (layout-independent).
fn key_name(code: u16) -> Option<&'static str> {
    Some(match code {
        0 => "KeyA",
        11 => "KeyB",
        8 => "KeyC",
        2 => "KeyD",
        14 => "KeyE",
        3 => "KeyF",
        5 => "KeyG",
        4 => "KeyH",
        34 => "KeyI",
        38 => "KeyJ",
        40 => "KeyK",
        37 => "KeyL",
        46 => "KeyM",
        45 => "KeyN",
        31 => "KeyO",
        35 => "KeyP",
        12 => "KeyQ",
        15 => "KeyR",
        1 => "KeyS",
        17 => "KeyT",
        32 => "KeyU",
        9 => "KeyV",
        13 => "KeyW",
        7 => "KeyX",
        16 => "KeyY",
        6 => "KeyZ",
        29 => "Digit0",
        18 => "Digit1",
        19 => "Digit2",
        20 => "Digit3",
        21 => "Digit4",
        23 => "Digit5",
        22 => "Digit6",
        26 => "Digit7",
        28 => "Digit8",
        25 => "Digit9",
        122 => "F1",
        120 => "F2",
        99 => "F3",
        118 => "F4",
        96 => "F5",
        97 => "F6",
        98 => "F7",
        100 => "F8",
        101 => "F9",
        109 => "F10",
        103 => "F11",
        111 => "F12",
        49 => "Space",
        27 => "Minus",
        24 => "Equal",
        33 => "BracketLeft",
        30 => "BracketRight",
        41 => "Semicolon",
        39 => "Quote",
        43 => "Comma",
        47 => "Period",
        44 => "Slash",
        42 => "Backslash",
        50 => "Backquote",
        _ => return None,
    })
}
