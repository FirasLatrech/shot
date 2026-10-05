//! Drawing helpers for `drawRect:` in flipped views.

use objc2::{rc::Retained, runtime::AnyObject};
use objc2_app_kit::{NSColor, NSFont, NSFontAttributeName, NSForegroundColorAttributeName, NSStringDrawing};
use objc2_core_foundation::CGFloat;
use objc2_core_graphics::{CGContext, CGImage, CGInterpolationQuality};
use objc2_foundation::{NSDictionary, NSPoint, NSSize, NSString};
use shot_core::Rect;

use crate::cg;

pub type Rgba = (f64, f64, f64, f64);
pub const WHITE: Rgba = (1.0, 1.0, 1.0, 1.0);
pub const BLACK: Rgba = (0.0, 0.0, 0.0, 1.0);
pub const ACCENT: Rgba = (0.0, 0.48, 1.0, 1.0);

fn r(rect: Rect) -> objc2_core_foundation::CGRect {
    cg::rect(rect.x as f64, rect.y as f64, rect.w as f64, rect.h as f64)
}

pub fn fill(c: &CGContext, rect: Rect, (red, g, b, a): Rgba) {
    CGContext::set_rgb_fill_color(Some(c), red, g, b, a);
    CGContext::fill_rect(Some(c), r(rect));
}

pub fn stroke(c: &CGContext, rect: Rect, width: f64, (red, g, b, a): Rgba) {
    CGContext::set_rgb_stroke_color(Some(c), red, g, b, a);
    CGContext::stroke_rect_with_width(Some(c), r(rect), width);
}

pub fn fill_ellipse(c: &CGContext, rect: Rect, (red, g, b, a): Rgba) {
    CGContext::set_rgb_fill_color(Some(c), red, g, b, a);
    CGContext::fill_ellipse_in_rect(Some(c), r(rect));
}

pub fn stroke_ellipse(c: &CGContext, rect: Rect, width: f64, (red, g, b, a): Rgba) {
    CGContext::set_rgb_stroke_color(Some(c), red, g, b, a);
    CGContext::set_line_width(Some(c), width);
    CGContext::stroke_ellipse_in_rect(Some(c), r(rect));
}

pub fn line(c: &CGContext, x0: f32, y0: f32, x1: f32, y1: f32, width: f64, color: Rgba) {
    // A thin filled rect avoids building a path for axis-aligned lines.
    let w = width as f32;
    let rect = if (y0 - y1).abs() < f32::EPSILON {
        Rect::new(x0.min(x1), y0 - w / 2.0, (x1 - x0).abs(), w)
    } else {
        Rect::new(x0 - w / 2.0, y0.min(y1), w, (y1 - y0).abs())
    };
    fill(c, rect, color);
}

/// Dims everything in `bounds` except `hole`.
pub fn dim_except(c: &CGContext, bounds: Rect, hole: Option<Rect>, color: Rgba) {
    let Some(h) = hole.and_then(|h| h.intersect(&bounds)) else { return fill(c, bounds, color) };
    fill(c, Rect::new(bounds.x, bounds.y, bounds.w, h.y - bounds.y), color);
    fill(c, Rect::new(bounds.x, h.bottom(), bounds.w, bounds.bottom() - h.bottom()), color);
    fill(c, Rect::new(bounds.x, h.y, h.x - bounds.x, h.h), color);
    fill(c, Rect::new(h.right(), h.y, bounds.right() - h.right(), h.h), color);
}

/// Draws an image upright in a flipped view.
pub fn image(c: &CGContext, img: &CGImage, rect: Rect, smooth: bool) {
    CGContext::save_g_state(Some(c));
    let q = if smooth { CGInterpolationQuality::High } else { CGInterpolationQuality::None };
    CGContext::set_interpolation_quality(Some(c), q);
    CGContext::translate_ctm(Some(c), rect.x as CGFloat, rect.bottom() as CGFloat);
    CGContext::scale_ctm(Some(c), 1.0, -1.0);
    CGContext::draw_image(Some(c), cg::rect(0.0, 0.0, rect.w as f64, rect.h as f64), Some(img));
    CGContext::restore_g_state(Some(c));
}

/// Draws an SF Symbol tinted `color`, centered in `rect`.
pub fn symbol(name: &str, rect: Rect, point_size: f64, color: Rgba) {
    use objc2_app_kit::{NSImage, NSImageSymbolConfiguration};
    let Some(img) = NSImage::imageWithSystemSymbolName_accessibilityDescription(&NSString::from_str(name), None) else {
        return;
    };
    let config = NSImageSymbolConfiguration::configurationWithPointSize_weight(point_size, 0.3)
        .configurationByApplyingConfiguration(&NSImageSymbolConfiguration::configurationWithHierarchicalColor(
            &ns_color(color),
        ));
    let Some(img) = img.imageWithSymbolConfiguration(&config) else { return };
    let s = img.size();
    let dest = Rect::new(
        rect.center().x - s.width as f32 / 2.0,
        rect.center().y - s.height as f32 / 2.0,
        s.width as f32,
        s.height as f32,
    );
    unsafe {
        img.drawInRect_fromRect_operation_fraction_respectFlipped_hints(
            crate::ui::ns_rect(dest),
            objc2_foundation::NSRect::ZERO,
            objc2_app_kit::NSCompositingOperation::SourceOver,
            1.0,
            true,
            None,
        )
    };
}

/// A filled rounded rectangle in any color.
pub fn round_rect(rect: Rect, radius: f64, color: Rgba) {
    ns_color(color).setFill();
    objc2_app_kit::NSBezierPath::bezierPathWithRoundedRect_xRadius_yRadius(crate::ui::ns_rect(rect), radius, radius)
        .fill();
}

pub fn fill_rounded_ns(rect: Rect, radius: f64, color: &NSColor) {
    color.setFill();
    objc2_app_kit::NSBezierPath::bezierPathWithRoundedRect_xRadius_yRadius(crate::ui::ns_rect(rect), radius, radius)
        .fill();
}

/// Fills with any NSColor, including dynamic light/dark ones.
pub fn fill_ns(rect: Rect, color: &NSColor) {
    color.setFill();
    objc2_app_kit::NSBezierPath::fillRect(crate::ui::ns_rect(rect));
}

/// Runs `draw` with a soft drop shadow under everything it paints.
pub fn with_shadow(c: &CGContext, blur: f64, alpha: f64, draw: impl FnOnce()) {
    CGContext::save_g_state(Some(c));
    let color = ns_color((0.0, 0.0, 0.0, alpha)).CGColor();
    CGContext::set_shadow_with_color(Some(c), objc2_core_foundation::CGSize::new(0.0, -blur / 4.0), blur, Some(&color));
    draw();
    CGContext::restore_g_state(Some(c));
}

/// Runs `draw` with drawing restricted to `rect`.
pub fn clipped(c: &CGContext, rect: Rect, draw: impl FnOnce()) {
    CGContext::save_g_state(Some(c));
    CGContext::clip_to_rect(Some(c), r(rect));
    draw();
    CGContext::restore_g_state(Some(c));
}

pub fn ns_color((r, g, b, a): Rgba) -> Retained<NSColor> {
    NSColor::colorWithSRGBRed_green_blue_alpha(r, g, b, a)
}

fn attrs(size: f64, bold: bool, color: &NSColor) -> Retained<NSDictionary<NSString, AnyObject>> {
    let weight = if bold { 0.4 } else { 0.0 };
    let font = NSFont::monospacedDigitSystemFontOfSize_weight(size, weight);
    let keys: [&NSString; 2] = unsafe { [NSFontAttributeName, NSForegroundColorAttributeName] };
    let vals: [&AnyObject; 2] = [&font, color];
    NSDictionary::from_slices(&keys, &vals)
}

pub fn text_size(text: &str, size: f64, bold: bool) -> NSSize {
    unsafe { NSString::from_str(text).sizeWithAttributes(Some(&attrs(size, bold, &ns_color(WHITE)))) }
}

/// Draws text with its top-left at (x, y); needs a current NSGraphicsContext.
pub fn text(text: &str, x: f32, y: f32, size: f64, bold: bool, color: Rgba) {
    text_in(text, x, y, size, bold, &ns_color(color));
}

/// Like [`text`], with any NSColor (e.g. dynamic `labelColor` for light/dark mode).
pub fn text_in(text: &str, x: f32, y: f32, size: f64, bold: bool, color: &NSColor) {
    unsafe {
        NSString::from_str(text)
            .drawAtPoint_withAttributes(NSPoint::new(x as f64, y as f64), Some(&attrs(size, bold, color)))
    };
}

/// A dark rounded "pill" label, as used for size readouts and hints.
pub fn pill(label: &str, x: f32, y: f32, size: f64) -> Rect {
    let s = text_size(label, size, true);
    let pad = (size * 0.6) as f32;
    let rect = Rect::new(x, y, s.width as f32 + pad * 2.0, s.height as f32 + pad);
    let path = objc2_app_kit::NSBezierPath::bezierPathWithRoundedRect_xRadius_yRadius(
        crate::ui::ns_rect(rect),
        rect.h as f64 / 2.0,
        rect.h as f64 / 2.0,
    );
    ns_color((0.1, 0.1, 0.12, 0.85)).setFill();
    path.fill();
    text(label, x + pad, y + pad / 2.0, size, true, WHITE);
    rect
}
