//! Bridges between Rust pixel buffers and CoreGraphics images.

use std::ffi::c_void;

use image::RgbaImage;
use objc2_core_foundation::{CFData, CFRetained, CGFloat, CGPoint, CGRect, CGSize};
use objc2_core_graphics::{
    CGBitmapContextCreate, CGBitmapInfo, CGColorRenderingIntent, CGColorSpace, CGContext, CGDataProvider, CGImage,
    CGImageAlphaInfo, CGImageByteOrderInfo,
};
use tiny_skia::Pixmap;

pub fn rect(x: f64, y: f64, w: f64, h: f64) -> CGRect {
    CGRect::new(CGPoint::new(x, y), CGSize::new(w, h))
}

fn srgb() -> CFRetained<CGColorSpace> {
    CGColorSpace::with_name(Some(unsafe { objc2_core_graphics::kCGColorSpaceSRGB })).expect("sRGB color space")
}

/// RGBA8 bytes in memory order, premultiplied or straight per `premultiplied`.
fn image_from_rgba(width: u32, height: u32, bytes: &[u8], premultiplied: bool) -> CFRetained<CGImage> {
    let data = CFData::from_bytes(bytes);
    let provider = CGDataProvider::with_cf_data(Some(&data)).expect("data provider");
    let alpha = if premultiplied { CGImageAlphaInfo::PremultipliedLast } else { CGImageAlphaInfo::Last };
    let info = CGBitmapInfo(alpha.0 | CGImageByteOrderInfo::Order32Big.0);
    unsafe {
        CGImage::new(
            width as usize,
            height as usize,
            8,
            32,
            width as usize * 4,
            Some(&srgb()),
            info,
            Some(&provider),
            std::ptr::null(),
            true,
            CGColorRenderingIntent::RenderingIntentDefault,
        )
    }
    .expect("CGImage")
}

pub fn image_from_pixmap(pm: &Pixmap) -> CFRetained<CGImage> {
    image_from_rgba(pm.width(), pm.height(), pm.data(), true)
}

pub fn image_from_rgba8(img: &RgbaImage) -> CFRetained<CGImage> {
    image_from_rgba(img.width(), img.height(), img.as_raw(), false)
}

/// Redraws any CGImage into a known RGBA layout and returns straight-alpha pixels.
pub fn rgba_from_image(image: &CGImage) -> Option<RgbaImage> {
    let (w, h) = (CGImage::width(Some(image)), CGImage::height(Some(image)));
    if w == 0 || h == 0 {
        return None;
    }
    let mut buf = vec![0u8; w * h * 4];
    let info = CGImageAlphaInfo::PremultipliedLast.0 | CGImageByteOrderInfo::Order32Big.0;
    let ctx = unsafe { CGBitmapContextCreate(buf.as_mut_ptr() as *mut c_void, w, h, 8, w * 4, Some(&srgb()), info) }?;
    CGContext::draw_image(Some(&ctx), rect(0.0, 0.0, w as CGFloat, h as CGFloat), Some(image));
    drop(ctx);
    for px in buf.chunks_exact_mut(4) {
        let a = px[3] as u32;
        if a > 0 && a < 255 {
            (0..3).for_each(|c| px[c] = ((px[c] as u32 * 255 + a / 2) / a).min(255) as u8);
        }
    }
    RgbaImage::from_raw(w as u32, h as u32, buf)
}
