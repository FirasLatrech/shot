//! On-device text recognition and QR/barcode reading via Vision.

use image::RgbaImage;
use objc2::{rc::Retained, AnyThread};
use objc2_foundation::{NSArray, NSDictionary};
use objc2_vision::{
    VNDetectBarcodesRequest, VNImageRequestHandler, VNRecognizeTextRequest, VNRequest, VNRequestTextRecognitionLevel,
};

use crate::cg;

/// Recognized text (any of Vision's 30+ languages) followed by any barcode
/// payloads, or `None` if Vision failed.
pub fn recognize(img: &RgbaImage) -> Option<String> {
    // Vision reads small UI text better when it's not tiny.
    let img = if img.width() < 600 {
        let k = 600.0 / img.width() as f32;
        image::imageops::resize(img, 600, (img.height() as f32 * k) as u32, image::imageops::FilterType::CatmullRom)
    } else {
        img.clone()
    };
    let cg = cg::image_from_rgba8(&img);
    let handler = unsafe {
        VNImageRequestHandler::initWithCGImage_options(VNImageRequestHandler::alloc(), &cg, &NSDictionary::new())
    };

    let text = VNRecognizeTextRequest::new();
    text.setRecognitionLevel(VNRequestTextRecognitionLevel::Accurate);
    text.setUsesLanguageCorrection(true);
    text.setAutomaticallyDetectsLanguage(true);
    let codes = unsafe { VNDetectBarcodesRequest::new() };
    let requests: [Retained<VNRequest>; 2] = [
        Retained::into_super(Retained::into_super(text.clone())),
        Retained::into_super(Retained::into_super(codes.clone())),
    ];
    handler.performRequests_error(&NSArray::from_retained_slice(&requests)).ok()?;

    let mut lines: Vec<String> = text
        .results()
        .map(|r| r.iter().filter_map(|o| o.topCandidates(1).firstObject()).map(|c| c.string().to_string()).collect())
        .unwrap_or_default();
    if let Some(found) = unsafe { codes.results() } {
        lines.extend(found.iter().filter_map(|b| unsafe { b.payloadStringValue() }).map(|s| s.to_string()));
    }
    Some(lines.join("\n"))
}

#[cfg(test)]
mod tests {
    use image::RgbaImage;
    use shot_core::{Annotation, Color, Document, Fonts, Pt, Shape, TextStyle};

    #[test]
    fn reads_rendered_text() {
        let mut doc = Document::new(RgbaImage::from_pixel(900, 200, image::Rgba([255, 255, 255, 255])));
        let text = "Hello from Shot 2026".to_string();
        doc.annotations.push(Annotation::new(
            Shape::Text { at: Pt::new(40.0, 60.0), text, style: TextStyle::Plain, size: 56.0 },
            Color::BLACK,
            2.0,
        ));
        let img = shot_core::render(&doc, &Fonts::system().unwrap());
        let out = super::recognize(&img).expect("vision ran");
        assert!(out.contains("Hello from Shot"), "got {out:?}");
    }
}
