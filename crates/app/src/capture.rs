//! Raw screen capture through CoreGraphics. Coordinates are global CG
//! points: origin at the top-left of the main display, y pointing down.

use image::RgbaImage;
use objc2::runtime::AnyObject;
use objc2_app_kit::NSScreen;
use objc2_core_foundation::{CFArray, CGRect};
use objc2_core_graphics::{
    CGPreflightScreenCaptureAccess, CGRequestScreenCaptureAccess, CGWindowImageOption, CGWindowListCopyWindowInfo,
    CGWindowListOption,
};
use objc2_foundation::{ns_string, MainThreadMarker, NSArray, NSDictionary, NSNumber, NSString};
use shot_core::Rect;

use crate::cg;

/// One display, in global CG points.
#[derive(Clone)]
pub struct Display {
    pub bounds: Rect,
    pub scale: f64,
}

pub fn displays(mtm: MainThreadMarker) -> Vec<Display> {
    let screens = NSScreen::screens(mtm);
    let main_h = screens.firstObject().map_or(0.0, |s| s.frame().size.height);
    screens
        .iter()
        .map(|s| {
            let f = s.frame();
            let bounds = Rect::new(
                f.origin.x as f32,
                (main_h - f.origin.y - f.size.height) as f32,
                f.size.width as f32,
                f.size.height as f32,
            );
            Display { scale: s.backingScaleFactor(), bounds }
        })
        .collect()
}

/// Height of the main display, for flipping between AppKit and CG coordinates.
pub fn main_height(mtm: MainThreadMarker) -> f64 {
    NSScreen::screens(mtm).firstObject().map_or(0.0, |s| s.frame().size.height)
}

fn to_cg(r: Rect) -> CGRect {
    cg::rect(r.x as f64, r.y as f64, r.w as f64, r.h as f64)
}

/// Captures a region of the screen at native (Retina) resolution.
#[allow(deprecated)] // ScreenCaptureKit is async-only; this stays synchronous and still works.
pub fn region(r: Rect) -> Option<RgbaImage> {
    let img = objc2_core_graphics::CGWindowListCreateImage(
        to_cg(r),
        CGWindowListOption::OptionOnScreenOnly,
        0,
        CGWindowImageOption::BestResolution,
    )?;
    cg::rgba_from_image(&img)
}

/// Captures one window, optionally with its drop shadow (transparent around it).
#[allow(deprecated)]
pub fn window(id: u32, shadow: bool) -> Option<RgbaImage> {
    let mut opts = CGWindowImageOption::BestResolution;
    if !shadow {
        opts |= CGWindowImageOption::BoundsIgnoreFraming;
    }
    let img = objc2_core_graphics::CGWindowListCreateImage(
        // CGRectNull: "use the window's own bounds".
        cg::rect(f64::INFINITY, f64::INFINITY, 0.0, 0.0),
        CGWindowListOption::OptionIncludingWindow,
        id,
        opts,
    )?;
    cg::rgba_from_image(&img)
}

#[derive(Debug, Clone)]
pub struct WindowInfo {
    pub id: u32,
    pub bounds: Rect,
    pub owner: String,
    pub title: String,
}

/// On-screen app windows, front to back, excluding our own and system chrome.
pub fn windows() -> Vec<WindowInfo> {
    let opts = CGWindowListOption::OptionOnScreenOnly | CGWindowListOption::ExcludeDesktopElements;
    let Some(list) = CGWindowListCopyWindowInfo(opts, 0) else { return vec![] };
    // CFArray of CFDictionary is toll-free bridged to NSArray of NSDictionary.
    let list: &NSArray<NSDictionary<NSString, AnyObject>> = unsafe { &*(&*list as *const CFArray as *const _) };
    let me = std::process::id() as i64;
    let num = |d: &NSDictionary<NSString, AnyObject>, k: &NSString| {
        d.objectForKey(k).and_then(|o| o.downcast::<NSNumber>().ok()).map(|n| n.as_f64())
    };
    let string = |d: &NSDictionary<NSString, AnyObject>, k: &NSString| {
        d.objectForKey(k).and_then(|o| o.downcast::<NSString>().ok()).map(|s| s.to_string()).unwrap_or_default()
    };
    list.iter()
        .filter_map(|d| {
            // Layer 0 is normal app windows; menus, docks and overlays sit above.
            if num(&d, ns_string!("kCGWindowLayer"))? != 0.0
                || num(&d, ns_string!("kCGWindowOwnerPID"))? as i64 == me
                || num(&d, ns_string!("kCGWindowAlpha")).unwrap_or(1.0) == 0.0
            {
                return None;
            }
            let b = d.objectForKey(ns_string!("kCGWindowBounds"))?.downcast::<NSDictionary>().ok()?;
            let b: &NSDictionary<NSString, AnyObject> = unsafe { &*(&*b as *const NSDictionary as *const _) };
            let bounds = Rect::new(
                num(b, ns_string!("X"))? as f32,
                num(b, ns_string!("Y"))? as f32,
                num(b, ns_string!("Width"))? as f32,
                num(b, ns_string!("Height"))? as f32,
            );
            let id = num(&d, ns_string!("kCGWindowNumber"))? as u32;
            (bounds.w > 40.0 && bounds.h > 40.0).then(|| WindowInfo {
                id,
                bounds,
                owner: string(&d, ns_string!("kCGWindowOwnerName")),
                title: string(&d, ns_string!("kCGWindowName")),
            })
        })
        .collect()
}

pub fn has_permission() -> bool {
    CGPreflightScreenCaptureAccess()
}

/// Shows the system prompt (once per app) and adds Shot to the Settings list.
pub fn request_permission() {
    if !CGPreflightScreenCaptureAccess() {
        CGRequestScreenCaptureAccess();
    }
}
