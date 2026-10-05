//! Application state and the actions everything else triggers.

use std::{
    cell::{Cell, OnceCell, RefCell},
    path::PathBuf,
    ptr::NonNull,
    rc::Rc,
    str::FromStr,
    sync::Arc,
};

use block2::RcBlock;
use global_hotkey::{hotkey::HotKey, GlobalHotKeyEvent, GlobalHotKeyManager, HotKeyState};
use image::RgbaImage;
use objc2::{rc::Retained, sel, AnyThread, MainThreadOnly};
use objc2_app_kit::{
    NSAlert, NSApplication, NSApplicationActivationPolicy, NSEventModifierFlags, NSMenu, NSMenuItem, NSOpenPanel,
    NSSavePanel, NSSound, NSStatusBar, NSStatusItem,
};
use objc2_foundation::{ns_string, MainThreadMarker, NSDate, NSDateFormatter, NSString, NSTimer, NSURL};
use shot_core::{
    doc::encode_png,
    history::{CaptureKind, History},
    settings::{Action, ImageFormat, Settings},
    Document, Fonts,
};

use crate::{
    capture, editor, history_window, ocr, overlay, pin, selection, settings_window,
    ui::{self, Targets},
};

/// A finished capture as it travels between overlay, editor, pins and history.
#[derive(Clone)]
pub struct Capture {
    pub image: Arc<RgbaImage>,
    /// Copy in the history folder, used for drag & drop.
    pub file: Option<PathBuf>,
}

/// The hotkey manager and what each registered id triggers.
type Hotkeys = (GlobalHotKeyManager, Vec<(u32, HotKey, Action)>);

pub struct App {
    pub mtm: MainThreadMarker,
    pub settings: RefCell<Settings>,
    pub history: RefCell<History>,
    pub fonts: Fonts,
    config_path: PathBuf,
    pub overlays: RefCell<Vec<Rc<overlay::QuickOverlay>>>,
    pub recently_closed: RefCell<Vec<Capture>>,
    pub overlays_hidden: Cell<bool>,
    pub pins: RefCell<Vec<Rc<pin::Pin>>>,
    pub editors: RefCell<Vec<Rc<editor::Editor>>>,
    pub selection: RefCell<Option<Rc<selection::Selection>>>,
    hotkeys: RefCell<Option<Hotkeys>>,
    status: RefCell<Option<Retained<NSStatusItem>>>,
    /// Action targets of the current menu bar menu.
    targets: RefCell<Targets>,
}

thread_local! {
    static APP: OnceCell<Rc<App>> = const { OnceCell::new() };
}

pub fn app() -> Rc<App> {
    APP.with(|a| a.get().expect("app initialised").clone())
}

pub fn start(mtm: MainThreadMarker) {
    // `shot capture-to <file.png>`: silent capture of every display, for scripts.
    let args: Vec<String> = std::env::args().collect();
    if let (Some("capture-to"), Some(path)) = (args.get(1).map(String::as_str), args.get(2)) {
        std::process::exit(capture_to(mtm, path));
    }
    let support = dirs::data_dir().unwrap_or_else(std::env::temp_dir).join("Shot");
    let config_path = support.join("settings.json");
    let settings = Settings::load(&config_path);
    let mut history = History::open(support.join("History")).expect("history folder");
    let _ = history.prune(settings.history_days as u64 * 86_400, shot_core::history::now());
    let fonts = Fonts::system().expect("system fonts");
    // Files dragged or shared in the last session are no longer needed.
    let _ = std::fs::remove_dir_all(temp_dir());

    let app = Rc::new(App {
        mtm,
        settings: RefCell::new(settings),
        history: RefCell::new(history),
        fonts,
        config_path,
        overlays: RefCell::default(),
        recently_closed: RefCell::default(),
        overlays_hidden: Cell::new(false),
        pins: RefCell::default(),
        editors: RefCell::default(),
        selection: RefCell::default(),
        hotkeys: RefCell::default(),
        status: RefCell::default(),
        targets: RefCell::default(),
    });
    APP.with(|a| a.set(app.clone())).ok().expect("started once");

    let ns_app = NSApplication::sharedApplication(mtm);
    ns_app.setActivationPolicy(NSApplicationActivationPolicy::Accessory);
    app.build_status_item();
    app.register_hotkeys();
    GlobalHotKeyEvent::set_event_handler(Some(|e: GlobalHotKeyEvent| {
        if e.state == HotKeyState::Pressed {
            let id = e.id;
            // Carbon delivers hotkeys on the main thread, inside the run loop.
            after(0.0, move || self::app().on_hotkey(id));
        }
    }));
    capture::request_permission();
    // `shot <action>` triggers an action at launch, e.g. `shot area`.
    // Also `shot settings` and `shot open <image or .shot file>`.
    let args: Vec<String> = std::env::args().skip(1).collect();
    match (args.first().map(String::as_str), args.get(1)) {
        (Some("settings"), _) => after(0.2, settings_window::show),
        (Some("open"), Some(file)) => {
            let file = PathBuf::from(file);
            after(0.2, move || self::app().open_file(&file));
        }
        (Some(arg), _) => {
            if let Some(action) = action_from_arg(arg) {
                after(0.2, move || self::app().run(action));
            }
        }
        _ => {}
    }
    ns_app.run();
}

impl App {
    pub fn run(&self, action: Action) {
        let captures = !matches!(action, Action::OpenHistory | Action::RestoreOverlay | Action::ToggleOverlays);
        if captures && !capture::has_permission() {
            return permission_alert();
        }
        match action {
            Action::AllInOne => selection::start(selection::Mode::AllInOne),
            Action::CaptureArea => selection::start(selection::Mode::Area),
            Action::CaptureWindow => selection::start(selection::Mode::Window),
            Action::CaptureText => selection::start(selection::Mode::Text),
            Action::SelfTimer => selection::start(selection::Mode::SelfTimer),
            Action::CaptureFullscreen => self.capture_fullscreen(),
            Action::CapturePrevious => self.capture_previous(),
            Action::OpenHistory => history_window::show(),
            Action::RestoreOverlay => self.restore_overlay(),
            Action::ToggleOverlays => self.toggle_overlays(),
        }
    }

    fn on_hotkey(&self, id: u32) {
        let action =
            self.hotkeys.borrow().as_ref().and_then(|(_, list)| list.iter().find(|(i, ..)| *i == id).map(|(.., a)| *a));
        if let Some(a) = action {
            self.run(a);
        }
    }

    /// Captures the display under the mouse.
    fn capture_fullscreen(&self) {
        let mouse = objc2_app_kit::NSEvent::mouseLocation();
        let main_h = capture::main_height(self.mtm);
        let p = shot_core::Pt::new(mouse.x as f32, (main_h - mouse.y) as f32);
        let displays = capture::displays(self.mtm);
        let Some(d) = displays.iter().find(|d| d.bounds.contains(p)).or(displays.first()) else { return };
        if let Some(img) = capture::region(d.bounds) {
            self.captured(img, CaptureKind::Fullscreen);
        }
    }

    fn capture_previous(&self) {
        let last = self.settings.borrow().last_selection;
        match last.and_then(capture::region) {
            Some(img) => self.captured(img, CaptureKind::Area),
            None => selection::start(selection::Mode::Area),
        }
    }

    /// Everything that happens after a capture: history, sound, clipboard, file, overlay.
    pub fn captured(&self, img: RgbaImage, kind: CaptureKind) {
        let (sound, copy, save, show) = {
            let s = self.settings.borrow();
            (s.play_sound, s.copy_after_capture, s.save_after_capture, s.show_overlay)
        };
        if sound {
            play_sound("Screen Capture");
        }
        let png = encode_png(&img);
        let days = self.settings.borrow().history_days as u64;
        let file = {
            let mut h = self.history.borrow_mut();
            // A menu-bar app runs for weeks, so prune here, not just at launch.
            let _ = h.prune(days * 86_400, shot_core::history::now());
            h.add(kind, &png, img.width(), img.height()).ok().map(|e| h.path(&e))
        };
        if copy {
            ui::copy_png(&png);
        }
        if save {
            self.save(&img);
        }
        let cap = Capture { image: Arc::new(img), file };
        if show {
            overlay::show(cap);
        } else if !copy && !save {
            // With every output turned off, at least don't lose the capture.
            ui::copy_image(&cap.image);
        }
    }

    pub fn save_dir(&self) -> PathBuf {
        self.settings.borrow().save_dir.clone().or_else(dirs::desktop_dir).unwrap_or_else(std::env::temp_dir)
    }

    pub fn image_format(&self) -> ImageFormat {
        self.settings.borrow().format
    }

    /// Saves into the configured folder with a timestamped name.
    pub fn save(&self, img: &RgbaImage) -> Option<PathBuf> {
        let ext = self.image_format().ext();
        let dir = self.save_dir();
        let base = format!("Shot {}", timestamp());
        let path = (0..)
            .map(|i| dir.join(if i == 0 { format!("{base}.{ext}") } else { format!("{base} ({i}).{ext}") }))
            .find(|p| !p.exists())?;
        match write_image(img, &path) {
            Ok(()) => {
                toast(&format!("Saved to {}", dir.file_name().map_or("folder".into(), |n| n.to_string_lossy())));
                Some(path)
            }
            Err(e) => {
                alert("Couldn't save the screenshot", &e.to_string());
                None
            }
        }
    }

    /// "Save As…" panel; returns the chosen path after writing.
    pub fn save_as(&self, img: &RgbaImage) -> Option<PathBuf> {
        let panel = NSSavePanel::savePanel(self.mtm);
        let ext = self.image_format().ext();
        panel.setNameFieldStringValue(&NSString::from_str(&format!("Shot {}.{ext}", timestamp())));
        panel.setDirectoryURL(Some(&NSURL::fileURLWithPath(&NSString::from_str(&self.save_dir().to_string_lossy()))));
        ui::activate(self.mtm);
        if panel.runModal() != 1 {
            return None;
        }
        let path = PathBuf::from(panel.URL()?.path()?.to_string());
        write_image(img, &path).map_err(|e| alert("Couldn't save", &e.to_string())).ok()?;
        Some(path)
    }

    pub fn save_settings(&self) {
        if let Err(e) = self.settings.borrow().save(&self.config_path) {
            eprintln!("saving settings: {e}");
        }
    }

    pub fn open_image_dialog(&self) {
        let panel = NSOpenPanel::openPanel(self.mtm);
        panel.setAllowsMultipleSelection(false);
        ui::activate(self.mtm);
        if panel.runModal() != 1 {
            return;
        }
        let Some(path) = panel.URL().and_then(|u| u.path()) else { return };
        self.open_file(&PathBuf::from(path.to_string()));
    }

    /// Opens an image or a `.shot` project in the editor.
    pub fn open_file(&self, path: &std::path::Path) {
        let doc = if path.extension().is_some_and(|e| e == shot_core::doc::PROJECT_EXT) {
            Document::open_project(path).map_err(|e| e.to_string())
        } else {
            image::open(path).map(|i| Document::new(i.into_rgba8())).map_err(|e| e.to_string())
        };
        match doc {
            Ok(doc) => editor::open(doc),
            Err(e) => alert("Couldn't open the file", &e),
        }
    }

    fn restore_overlay(&self) {
        let last = self.recently_closed.borrow_mut().pop();
        if let Some(cap) = last {
            overlay::show(cap);
        }
    }

    fn toggle_overlays(&self) {
        let hidden = !self.overlays_hidden.get();
        self.overlays_hidden.set(hidden);
        for o in self.overlays.borrow().iter() {
            o.set_hidden(hidden);
        }
        for p in self.pins.borrow().iter() {
            p.set_hidden(hidden);
        }
    }

    /// Recognizes text on a background thread (it can take seconds on big
    /// captures) and copies it when done.
    pub fn ocr(&self, img: &RgbaImage) {
        let img = img.clone();
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let text = objc2::rc::autoreleasepool(|_| ocr::recognize(&img));
            let _ = tx.send(text);
        });
        poll(rx, |text| match text {
            Some(text) if !text.trim().is_empty() => {
                ui::copy_text(&text);
                let preview: String = text.chars().take(48).collect();
                toast(&format!("Copied text: {preview}{}", if text.chars().count() > 48 { "…" } else { "" }));
            }
            _ => toast("No text found"),
        });
    }

    // ------------------------------------------------------------ menu bar

    fn build_status_item(&self) {
        let item = NSStatusBar::systemStatusBar().statusItemWithLength(-1.0);
        if let Some(button) = item.button(self.mtm) {
            button.setImage(Some(&ui::logo_template(15.0)));
        }
        *self.status.borrow_mut() = Some(item);
        self.rebuild_menu();
    }

    /// (Re)builds the menu bar menu, e.g. after shortcuts change.
    pub fn rebuild_menu(&self) {
        let mtm = self.mtm;
        let menu = NSMenu::new(mtm);
        // Each rebuild owns a fresh set of targets; the old menu's go with it.
        let targets = Targets::default();
        let hotkeys = self.settings.borrow().hotkeys.clone();
        let sep = || NSMenuItem::separatorItem(mtm);
        let item = |title: &str, symbol: &str, shortcut: Option<&str>, f: Box<dyn Fn()>| {
            menu_item(mtm, &targets, title, symbol, shortcut, f)
        };
        let action = |a: Action, symbol: &str| {
            item(a.label(), symbol, hotkeys.get(&a).map(String::as_str), Box::new(move || app().run(a)))
        };

        for (a, symbol) in [
            (Action::AllInOne, "square.dashed"),
            (Action::CaptureArea, "crop"),
            (Action::CapturePrevious, "arrow.counterclockwise"),
            (Action::CaptureFullscreen, "display"),
            (Action::CaptureWindow, "macwindow"),
            (Action::SelfTimer, "timer"),
        ] {
            menu.addItem(&action(a, symbol));
        }
        menu.addItem(&sep());
        menu.addItem(&action(Action::CaptureText, "text.viewfinder"));
        menu.addItem(&sep());
        menu.addItem(&item("Open Image…", "photo", Some("cmd+KeyO"), Box::new(|| app().open_image_dialog())));
        menu.addItem(&item(
            "Annotate Clipboard",
            "pencil.tip.crop.circle",
            None,
            Box::new(|| match ui::pasted_image() {
                Some(img) => editor::open(Document::new(img)),
                None => toast("No image on the clipboard"),
            }),
        ));
        menu.addItem(&item(
            "Pin Clipboard",
            "pin",
            None,
            Box::new(|| match ui::pasted_image() {
                Some(img) => pin::show(Arc::new(img), None),
                None => toast("No image on the clipboard"),
            }),
        ));
        menu.addItem(&sep());
        menu.addItem(&action(Action::OpenHistory, "clock.arrow.circlepath"));
        menu.addItem(&action(Action::RestoreOverlay, "arrow.uturn.backward.circle"));
        menu.addItem(&action(Action::ToggleOverlays, "eye.slash"));
        menu.addItem(&item(
            "Unlock All Pins",
            "lock.open",
            None,
            Box::new(|| {
                for p in app().pins.borrow().iter() {
                    p.set_locked(false);
                }
            }),
        ));
        menu.addItem(&sep());
        menu.addItem(&item("Settings…", "gearshape", Some("cmd+Comma"), Box::new(settings_window::show)));
        menu.addItem(&item(
            "Quit Shot",
            "power",
            Some("cmd+KeyQ"),
            Box::new(|| NSApplication::sharedApplication(MainThreadMarker::new().unwrap()).terminate(None)),
        ));
        if let Some(status) = self.status.borrow().as_ref() {
            status.setMenu(Some(&menu));
        }
        *self.targets.borrow_mut() = targets;
    }
}

fn menu_item(
    mtm: MainThreadMarker,
    targets: &Targets,
    title: &str,
    symbol: &str,
    shortcut: Option<&str>,
    f: Box<dyn Fn()>,
) -> Retained<NSMenuItem> {
    let target = targets.add(mtm, move |_| f());
    let (key, mods) = shortcut.and_then(key_equivalent).unwrap_or((String::new(), NSEventModifierFlags::empty()));
    let item = unsafe {
        NSMenuItem::initWithTitle_action_keyEquivalent(
            NSMenuItem::alloc(mtm),
            &NSString::from_str(title),
            Some(sel!(fire:)),
            &NSString::from_str(&key),
        )
    };
    item.setKeyEquivalentModifierMask(mods);
    item.setImage(ui::symbol(symbol).as_deref());
    unsafe { item.setTarget(Some(&target)) };
    item
}

impl App {
    // ------------------------------------------------------------ hotkeys

    /// Unregisters every global shortcut until the next `register_hotkeys`.
    pub fn suspend_hotkeys(&self) {
        if let Some((manager, list)) = self.hotkeys.borrow_mut().as_mut() {
            let keys: Vec<HotKey> = list.drain(..).map(|(_, k, _)| k).collect();
            let _ = manager.unregister_all(&keys);
        }
    }

    pub fn register_hotkeys(&self) {
        let mut slot = self.hotkeys.borrow_mut();
        if let Some((manager, old)) = slot.take() {
            let keys: Vec<HotKey> = old.iter().map(|(_, k, _)| *k).collect();
            let _ = manager.unregister_all(&keys);
            *slot = Some((manager, vec![]));
        }
        let manager = match slot.take() {
            Some((m, _)) => m,
            None => match GlobalHotKeyManager::new() {
                Ok(m) => m,
                Err(e) => return eprintln!("hotkeys unavailable: {e}"),
            },
        };
        let mut list = vec![];
        for (action, spec) in &self.settings.borrow().hotkeys {
            match HotKey::from_str(spec) {
                Ok(key) => match manager.register(key) {
                    Ok(()) => list.push((key.id(), key, *action)),
                    Err(e) => eprintln!("hotkey {spec} for {}: {e}", action.label()),
                },
                Err(e) => eprintln!("bad hotkey {spec}: {e}"),
            }
        }
        *slot = Some((manager, list));
    }
}

/// Captures all displays side by side into `path`; returns the exit code.
fn capture_to(mtm: MainThreadMarker, path: &str) -> i32 {
    if !capture::has_permission() {
        eprintln!("capture-to: Shot has no Screen Recording permission");
        return 2;
    }
    let displays = capture::displays(mtm);
    let shots: Vec<_> = displays.iter().filter_map(|d| Some((d.bounds, capture::region(d.bounds)?))).collect();
    let Some(bounds) = shots.iter().map(|s| s.0).reduce(|a, b| a.union(&b)) else { return 1 };
    // Compose at the main display's scale.
    let k = displays.first().map_or(1.0, |d| d.scale) as f32;
    let mut out = RgbaImage::new((bounds.w * k) as u32, (bounds.h * k) as u32);
    for (r, img) in shots {
        let img =
            image::imageops::resize(&img, (r.w * k) as u32, (r.h * k) as u32, image::imageops::FilterType::Triangle);
        image::imageops::overlay(&mut out, &img, ((r.x - bounds.x) * k) as i64, ((r.y - bounds.y) * k) as i64);
    }
    match out.save(path) {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("capture-to: {e}");
            1
        }
    }
}

fn action_from_arg(arg: &str) -> Option<Action> {
    Some(match arg.trim_start_matches("--") {
        "all-in-one" => Action::AllInOne,
        "area" => Action::CaptureArea,
        "window" => Action::CaptureWindow,
        "fullscreen" => Action::CaptureFullscreen,
        "previous" => Action::CapturePrevious,
        "self-timer" => Action::SelfTimer,
        "text" => Action::CaptureText,
        "history" => Action::OpenHistory,
        _ => return None,
    })
}

/// "shift+cmd+Digit4" → ("4", ⇧⌘): how a shortcut shows up in a menu.
fn key_equivalent(spec: &str) -> Option<(String, NSEventModifierFlags)> {
    let mut mods = NSEventModifierFlags::empty();
    let mut key = None;
    for part in spec.split('+') {
        match part.to_ascii_lowercase().as_str() {
            "shift" => mods |= NSEventModifierFlags::Shift,
            "cmd" | "command" | "super" => mods |= NSEventModifierFlags::Command,
            "alt" | "option" => mods |= NSEventModifierFlags::Option,
            "ctrl" | "control" => mods |= NSEventModifierFlags::Control,
            _ => {
                key = Some(match part {
                    f if f.starts_with('F') && f[1..].parse::<u32>().is_ok() => {
                        // NSF1FunctionKey is U+F704.
                        char::from_u32(0xF703 + f[1..].parse::<u32>().ok()?)?.to_string()
                    }
                    "Space" => " ".to_string(),
                    other => key_char(other).to_lowercase(),
                })
            }
        }
    }
    Some((key?, mods))
}

/// "shift+cmd+Digit4" → "⇧⌘4".
pub fn pretty_shortcut(spec: &str) -> String {
    spec.split('+')
        .map(|part| match part.to_ascii_lowercase().as_str() {
            "shift" => "⇧".to_string(),
            "cmd" | "command" | "super" => "⌘".to_string(),
            "alt" | "option" => "⌥".to_string(),
            "ctrl" | "control" => "⌃".to_string(),
            _ => key_char(part).to_uppercase(),
        })
        .collect()
}

/// "KeyA" → "A", "Digit4" → "4", "BracketLeft" → "[": the character on the key.
fn key_char(code: &str) -> String {
    let named = match code {
        "Space" => "Space",
        "Comma" => ",",
        "Period" => ".",
        "Minus" => "-",
        "Equal" => "=",
        "Slash" => "/",
        "Backslash" => "\\",
        "Semicolon" => ";",
        "Quote" => "'",
        "Backquote" => "`",
        "BracketLeft" => "[",
        "BracketRight" => "]",
        other => return other.trim_start_matches("Key").trim_start_matches("Digit").to_string(),
    };
    named.to_string()
}

pub fn write_image(img: &RgbaImage, path: &std::path::Path) -> Result<(), image::ImageError> {
    let is_jpeg = path.extension().is_some_and(|e| e.eq_ignore_ascii_case("jpg") || e.eq_ignore_ascii_case("jpeg"));
    if is_jpeg {
        // JPEG has no alpha channel.
        image::DynamicImage::ImageRgba8(img.clone()).into_rgb8().save(path)
    } else {
        img.save(path)
    }
}

/// A PNG in the temp folder, for dragging captures into other apps.
pub fn temp_png(img: &RgbaImage) -> Option<PathBuf> {
    static COUNTER: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    let dir = temp_dir();
    std::fs::create_dir_all(&dir).ok()?;
    // Each drag/share gets its own file: another app may still be reading the last one.
    let n = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let path = dir.join(format!("Shot {}{}.png", timestamp(), if n == 0 { String::new() } else { format!(" ({n})") }));
    img.save(&path).ok()?;
    Some(path)
}

fn temp_dir() -> PathBuf {
    std::env::temp_dir().join("Shot")
}

fn timestamp() -> String {
    let f = NSDateFormatter::new();
    f.setDateFormat(Some(ns_string!("yyyy-MM-dd 'at' HH.mm.ss")));
    f.stringFromDate(&NSDate::now()).to_string()
}

/// Plays a system UI sound ("Screen Capture") or a named alert sound ("Tink").
pub fn play_sound(name: &str) {
    let path =
        format!("/System/Library/Components/CoreAudio.component/Contents/SharedSupport/SystemSounds/system/{name}.aif");
    let sound = NSSound::initWithContentsOfFile_byReference(NSSound::alloc(), &NSString::from_str(&path), true)
        .or_else(|| NSSound::soundNamed(&NSString::from_str(name)));
    if let Some(s) = sound {
        let _ = s.play();
    }
}

pub fn alert(title: &str, detail: &str) {
    let mtm = MainThreadMarker::new().expect("main thread");
    let a = NSAlert::new(mtm);
    a.setMessageText(&NSString::from_str(title));
    a.setInformativeText(&NSString::from_str(detail));
    ui::activate(mtm);
    a.runModal();
}

/// Without Screen Recording access macOS hides every window from captures,
/// leaving only the wallpaper, so explain instead of producing empty shots.
fn permission_alert() {
    let mtm = MainThreadMarker::new().expect("main thread");
    let a = NSAlert::new(mtm);
    a.setMessageText(ns_string!("Shot needs Screen Recording access"));
    a.setInformativeText(ns_string!(
        "Without it, macOS hides your windows and screenshots only show the wallpaper.\n\n\
         Turn on Shot in Privacy & Security → Screen Recording, then reopen Shot."
    ));
    a.addButtonWithTitle(ns_string!("Open System Settings"));
    a.addButtonWithTitle(ns_string!("Quit & Reopen Shot"));
    a.addButtonWithTitle(ns_string!("Cancel"));
    ui::activate(mtm);
    match a.runModal() {
        1000 => {
            capture::request_permission();
            let url = NSURL::URLWithString(ns_string!(
                "x-apple.systempreferences:com.apple.preference.security?Privacy_ScreenCapture"
            ));
            if let Some(url) = url {
                objc2_app_kit::NSWorkspace::sharedWorkspace().openURL(&url);
            }
        }
        1001 => relaunch(),
        _ => {}
    }
}

/// Starts a fresh copy of the app and quits this one; macOS only applies a
/// new Screen Recording grant to newly started processes.
fn relaunch() {
    if let Some(bundle) = std::env::current_exe().ok().and_then(|exe| exe.ancestors().nth(3).map(|p| p.to_path_buf())) {
        if bundle.extension().is_some_and(|e| e == "app") {
            // The path goes in as $0, never into the script text.
            let _ = std::process::Command::new("/bin/sh").args(["-c", "sleep 1; open \"$0\""]).arg(&bundle).spawn();
        }
    }
    NSApplication::sharedApplication(MainThreadMarker::new().expect("main thread")).terminate(None);
}

pub fn toast(text: &str) {
    crate::toast::show(text);
}

/// Waits on the main thread (without blocking it) for a background result.
pub fn poll<T: 'static>(rx: std::sync::mpsc::Receiver<T>, done: impl FnOnce(T) + 'static) {
    match rx.try_recv() {
        Ok(v) => done(v),
        Err(std::sync::mpsc::TryRecvError::Empty) => after(0.03, move || poll(rx, done)),
        Err(std::sync::mpsc::TryRecvError::Disconnected) => {}
    }
}

/// Runs `f` on the main thread after `secs`.
pub fn after(secs: f64, f: impl FnOnce() + 'static) {
    let f = Cell::new(Some(f));
    let block = RcBlock::new(move |_: NonNull<NSTimer>| {
        if let Some(f) = f.take() {
            f();
        }
    });
    unsafe { NSTimer::scheduledTimerWithTimeInterval_repeats_block(secs, false, &block) };
}
