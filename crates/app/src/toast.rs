//! Short-lived HUD messages ("Copied", "Saved to Desktop", …).

use objc2::MainThreadOnly;
use objc2_app_kit::{
    NSFont, NSScreen, NSTextField, NSVisualEffectBlendingMode, NSVisualEffectMaterial, NSVisualEffectState,
    NSVisualEffectView,
};
use objc2_foundation::{MainThreadMarker, NSPoint, NSRect, NSSize, NSString};

use crate::{app::after, ui};

pub fn show(text: &str) {
    let mtm = MainThreadMarker::new().expect("main thread");
    let label = NSTextField::labelWithString(&NSString::from_str(text), mtm);
    label.setFont(Some(&NSFont::systemFontOfSize_weight(13.0, 0.23)));
    let fit = label.fittingSize();
    let (w, h) = (fit.width.min(560.0) + 32.0, fit.height + 18.0);

    let screen = NSScreen::mainScreen(mtm).map(|s| s.visibleFrame()).unwrap_or(NSRect::ZERO);
    let frame = NSRect::new(
        NSPoint::new(screen.origin.x + (screen.size.width - w) / 2.0, screen.origin.y + 80.0),
        NSSize::new(w, h),
    );
    let win = ui::overlay_window(mtm, frame);
    win.setLevel(25); // status-bar level, above normal windows
    win.setIgnoresMouseEvents(true);

    let fx = NSVisualEffectView::initWithFrame(NSVisualEffectView::alloc(mtm), NSRect::new(NSPoint::ZERO, frame.size));
    fx.setMaterial(NSVisualEffectMaterial::HUDWindow);
    fx.setBlendingMode(NSVisualEffectBlendingMode::BehindWindow);
    fx.setState(NSVisualEffectState::Active);
    ui::round_corners(&fx, h / 2.0);
    label.setFrame(NSRect::new(NSPoint::new(16.0, (h - fit.height) / 2.0), NSSize::new(w - 32.0, fit.height)));
    fx.addSubview(&label);
    win.setContentView(Some(&fx));
    win.orderFrontRegardless();
    after(1.8, move || win.close());
}
