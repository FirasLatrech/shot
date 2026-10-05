//! Shot: a native macOS screenshot tool.

mod app;
mod background_panel;
mod capture;
mod cg;
mod editor;
mod gfx;
mod history_window;
mod ocr;
mod overlay;
mod pin;
mod selection;
mod settings_window;
mod toast;
mod ui;

fn main() {
    let mtm = objc2_foundation::MainThreadMarker::new().expect("Shot must start on the main thread");
    app::start(mtm);
}
