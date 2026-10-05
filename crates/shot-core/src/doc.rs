//! The editable document behind the Annotate window, and its project-file format.

use std::{io::Cursor, path::Path, sync::Arc};

use image::RgbaImage;
use serde::{Deserialize, Serialize};

use crate::{
    annot::{Annotation, Shape},
    background::Background,
    geom::{Pt, Rect},
};

/// File extension of editable project files.
pub const PROJECT_EXT: &str = "shot";
const PROJECT_VERSION: u32 = 1;
/// Largest coordinate accepted from a project file, in pixels.
const MAX_DIM: f32 = 100_000.0;
/// Largest output side, in pixels.
pub const MAX_PX: u32 = 32_768;

/// One image placed on the canvas. The first layer is the original capture;
/// more are added by dropping screenshots into the editor.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Layer {
    #[serde(with = "png_b64")]
    pub image: Arc<RgbaImage>,
    pub at: Pt,
}

impl Layer {
    pub fn rect(&self) -> Rect {
        Rect::new(self.at.x, self.at.y, self.image.width() as f32, self.image.height() as f32)
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Document {
    pub layers: Vec<Layer>,
    pub annotations: Vec<Annotation>,
    /// Visible region in canvas coordinates; `None` shows the whole canvas.
    pub crop: Option<Rect>,
    pub background: Option<Background>,
    /// Final output size in pixels, for the resize option.
    pub output_size: Option<(u32, u32)>,
}

#[derive(Serialize, Deserialize)]
struct ProjectFile {
    version: u32,
    #[serde(flatten)]
    doc: Document,
}

impl Document {
    pub fn new(image: RgbaImage) -> Self {
        Self { layers: vec![Layer { image: Arc::new(image), at: Pt::default() }], ..Default::default() }
    }

    /// Union of all layers: the area every pixel buffer is rendered into.
    pub fn canvas_bounds(&self) -> Rect {
        self.layers.iter().map(Layer::rect).reduce(|a, b| a.union(&b)).unwrap_or_default()
    }

    /// The region that ends up in the exported image.
    pub fn visible_bounds(&self) -> Rect {
        let canvas = self.canvas_bounds();
        self.crop.and_then(|c| c.intersect(&canvas)).unwrap_or(canvas)
    }

    /// Next number for the counter tool: one past the highest on the canvas.
    pub fn next_counter(&self) -> u32 {
        self.annotations
            .iter()
            .filter_map(|a| match a.shape {
                Shape::Counter { n, .. } => Some(n),
                _ => None,
            })
            .max()
            .unwrap_or(0)
            .saturating_add(1)
    }

    /// Topmost annotation under `p`.
    pub fn hit(&self, p: Pt, tol: f32) -> Option<usize> {
        self.annotations.iter().rposition(|a| a.hit(p, tol))
    }

    /// Topmost added layer (never the base capture) under `p`.
    pub fn hit_layer(&self, p: Pt) -> Option<usize> {
        self.layers.iter().enumerate().skip(1).rev().find(|(_, l)| l.rect().contains(p)).map(|(i, _)| i)
    }

    /// Rotates layers and annotations 90° clockwise around the canvas.
    pub fn rotate_cw(&mut self) {
        let c = self.canvas_bounds();
        // (x, y) -> (c.bottom - y + c.x, x - c.x + c.y) keeps the canvas origin fixed.
        let map = |p: Pt| Pt::new(c.x + (c.bottom() - p.y), c.y + (p.x - c.x));
        let map_rect = |r: Rect| Rect::from_pts(map(Pt::new(r.x, r.y)), map(Pt::new(r.right(), r.bottom())));
        for l in &mut self.layers {
            let r = map_rect(l.rect());
            l.image = Arc::new(image::imageops::rotate90(&*l.image));
            l.at = Pt::new(r.x, r.y);
        }
        self.crop = self.crop.map(map_rect);
        self.transform_annotations(map, map_rect);
        self.output_size = self.output_size.map(|(w, h)| (h, w));
    }

    /// Mirrors layers and annotations; `horizontal` flips left/right.
    pub fn flip(&mut self, horizontal: bool) {
        let c = self.canvas_bounds();
        let map = move |p: Pt| {
            if horizontal {
                Pt::new(c.x + c.right() - p.x, p.y)
            } else {
                Pt::new(p.x, c.y + c.bottom() - p.y)
            }
        };
        let map_rect = |r: Rect| Rect::from_pts(map(Pt::new(r.x, r.y)), map(Pt::new(r.right(), r.bottom())));
        for l in &mut self.layers {
            let r = map_rect(l.rect());
            l.image = Arc::new(if horizontal {
                image::imageops::flip_horizontal(&*l.image)
            } else {
                image::imageops::flip_vertical(&*l.image)
            });
            l.at = Pt::new(r.x, r.y);
        }
        self.crop = self.crop.map(map_rect);
        self.transform_annotations(map, map_rect);
    }

    fn transform_annotations(&mut self, map: impl Fn(Pt) -> Pt, map_rect: impl Fn(Rect) -> Rect) {
        for a in &mut self.annotations {
            let bounds = a.bounds();
            match &mut a.shape {
                Shape::Arrow { from, to, ctrl, .. } => {
                    *from = map(*from);
                    *to = map(*to);
                    *ctrl = ctrl.map(&map);
                }
                Shape::Line { from, to } => {
                    *from = map(*from);
                    *to = map(*to);
                }
                Shape::Rect { rect, .. }
                | Shape::Ellipse { rect, .. }
                | Shape::Blur { rect, .. }
                | Shape::Pixelate { rect }
                | Shape::Spotlight { rect, .. } => *rect = map_rect(*rect),
                Shape::Pencil { points } | Shape::Highlighter { points } => {
                    points.iter_mut().for_each(|p| *p = map(*p))
                }
                // Counters are anchored at their center, so the point maps directly.
                Shape::Counter { at, .. } => *at = map(*at),
                // Text stays upright; move its center, not its top-left corner,
                // so it doesn't swing off the canvas.
                Shape::Text { at, .. } => {
                    let b = bounds;
                    let c = map(b.center());
                    *at = Pt::new(c.x - b.w / 2.0 + (at.x - b.x), c.y - b.h / 2.0 + (at.y - b.y));
                }
            }
        }
    }

    pub fn to_project_bytes(&self) -> Result<Vec<u8>, serde_json::Error> {
        serde_json::to_vec(&ProjectFile { version: PROJECT_VERSION, doc: self.clone() })
    }

    /// Parses a project file. Files from a newer version are refused rather
    /// than half-loaded, and every value is clamped to sane limits, since
    /// project files can come from anywhere.
    pub fn from_project_bytes(bytes: &[u8]) -> Result<Self, serde_json::Error> {
        use serde::de::Error;
        let file: ProjectFile = serde_json::from_slice(bytes)?;
        if file.version > PROJECT_VERSION {
            return Err(serde_json::Error::custom(format!(
                "this project was saved by a newer version of Shot (format {}); please update",
                file.version
            )));
        }
        if file.doc.layers.is_empty() {
            return Err(serde_json::Error::custom("the project has no images"));
        }
        let mut doc = file.doc;
        doc.sanitize();
        Ok(doc)
    }

    /// Clamps sizes, positions and counts so rendering can't panic or run
    /// out of memory, and drops references to files outside the project.
    pub fn sanitize(&mut self) {
        let lim = |v: f32, max: f32| if v.is_finite() { v.clamp(-max, max) } else { 0.0 };
        let pt = |p: &mut Pt| *p = Pt::new(lim(p.x, MAX_DIM), lim(p.y, MAX_DIM));
        let rect = |r: &mut Rect| {
            *r = Rect::new(lim(r.x, MAX_DIM), lim(r.y, MAX_DIM), lim(r.w, MAX_DIM).abs(), lim(r.h, MAX_DIM).abs())
        };
        for l in &mut self.layers {
            pt(&mut l.at);
        }
        for a in &mut self.annotations {
            a.width = lim(a.width, 200.0).max(0.5);
            match &mut a.shape {
                Shape::Arrow { from, to, ctrl, .. } => {
                    pt(from);
                    pt(to);
                    if let Some(c) = ctrl {
                        pt(c);
                    }
                }
                Shape::Line { from, to } => {
                    pt(from);
                    pt(to);
                }
                Shape::Rect { rect: r, .. }
                | Shape::Ellipse { rect: r, .. }
                | Shape::Blur { rect: r, .. }
                | Shape::Pixelate { rect: r }
                | Shape::Spotlight { rect: r, .. } => rect(r),
                Shape::Pencil { points } | Shape::Highlighter { points } => points.iter_mut().for_each(pt),
                Shape::Text { at, size, .. } => {
                    pt(at);
                    *size = lim(*size, 1000.0).max(4.0);
                }
                Shape::Counter { at, n } => {
                    pt(at);
                    *n = (*n).min(1_000_000);
                }
            }
        }
        self.crop = self.crop.map(|mut r| {
            rect(&mut r);
            r
        });
        self.output_size =
            self.output_size.filter(|&(w, h)| w > 0 && h > 0).map(|(w, h)| (w.min(MAX_PX), h.min(MAX_PX)));
        if let Some(bg) = &mut self.background {
            bg.sanitize();
            // A project must not pull arbitrary local files into an export.
            if matches!(bg.fill, crate::background::Fill::Image(_)) {
                bg.fill = crate::background::PRESETS[0].clone();
            }
        }
    }

    pub fn save_project(&self, path: &Path) -> std::io::Result<()> {
        crate::history::write_atomic(path, &self.to_project_bytes()?)
    }

    pub fn open_project(path: &Path) -> std::io::Result<Self> {
        Ok(Self::from_project_bytes(&std::fs::read(path)?)?)
    }
}

/// Undo/redo over whole-document snapshots. Layers are `Arc`ed, so a
/// snapshot only copies annotation data, never pixels.
#[derive(Default)]
pub struct UndoStack {
    undo: Vec<Document>,
    redo: Vec<Document>,
}

impl UndoStack {
    const LIMIT: usize = 100;

    /// Call before mutating `doc`.
    pub fn checkpoint(&mut self, doc: &Document) {
        if self.undo.len() == Self::LIMIT {
            self.undo.remove(0);
        }
        self.undo.push(doc.clone());
        self.redo.clear();
    }
    pub fn undo(&mut self, doc: &mut Document) -> bool {
        self.undo.pop().map(|prev| self.redo.push(std::mem::replace(doc, prev))).is_some()
    }
    pub fn redo(&mut self, doc: &mut Document) -> bool {
        self.redo.pop().map(|next| self.undo.push(std::mem::replace(doc, next))).is_some()
    }
}

pub fn encode_png(img: &RgbaImage) -> Vec<u8> {
    let mut out = Cursor::new(Vec::new());
    img.write_to(&mut out, image::ImageFormat::Png).expect("PNG encoding to memory cannot fail");
    out.into_inner()
}

mod png_b64 {
    use std::sync::Arc;

    use base64::{engine::general_purpose::STANDARD, Engine};
    use image::RgbaImage;
    use serde::{de::Error, Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(img: &Arc<RgbaImage>, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&STANDARD.encode(super::encode_png(img)))
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Arc<RgbaImage>, D::Error> {
        let bytes = STANDARD.decode(String::deserialize(d)?).map_err(D::Error::custom)?;
        let mut reader =
            image::ImageReader::new(std::io::Cursor::new(bytes)).with_guessed_format().map_err(D::Error::custom)?;
        // Tall scrolling captures exceed the decoder's default 512 MiB budget.
        let mut limits = image::Limits::default();
        limits.max_alloc = Some(4 << 30);
        limits.max_image_width = Some(super::MAX_PX);
        limits.max_image_height = Some(super::MAX_PX * 4);
        reader.limits(limits);
        let img = reader.decode().map_err(D::Error::custom)?;
        Ok(Arc::new(img.into_rgba8()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geom::Color;

    fn doc() -> Document {
        let mut img = RgbaImage::new(4, 2);
        img.put_pixel(0, 0, image::Rgba([255, 0, 0, 255]));
        let mut d = Document::new(img);
        d.annotations.push(Annotation::new(Shape::Counter { at: Pt::new(1.0, 1.0), n: 3 }, Color::RED, 2.0));
        d
    }

    #[test]
    fn project_roundtrip() {
        let d = doc();
        let back = Document::from_project_bytes(&d.to_project_bytes().unwrap()).unwrap();
        assert_eq!(back.annotations, d.annotations);
        assert_eq!(*back.layers[0].image, *d.layers[0].image);
        assert_eq!(back.next_counter(), 4);
    }

    #[test]
    fn rotate_four_times_is_identity() {
        let mut d = doc();
        d.annotations.push(Annotation::new(
            Shape::Rect { rect: Rect::new(0.5, 0.25, 2.0, 1.0), filled: false },
            Color::RED,
            1.0,
        ));
        let before = d.clone();
        d.rotate_cw();
        assert_eq!(d.canvas_bounds(), Rect::new(0.0, 0.0, 2.0, 4.0));
        for _ in 0..3 {
            d.rotate_cw();
        }
        assert_eq!(d.annotations, before.annotations);
        assert_eq!(*d.layers[0].image, *before.layers[0].image);
    }

    #[test]
    fn hostile_project_is_clamped_or_refused() {
        let mut d = doc();
        d.annotations.push(Annotation::new(
            Shape::Text { at: Pt::new(-1e30, 1e30), text: "x".into(), style: Default::default(), size: 1e9 },
            Color::RED,
            1e9,
        ));
        d.output_size = Some((u32::MAX, 0));
        let back = Document::from_project_bytes(&d.to_project_bytes().unwrap()).unwrap();
        let Shape::Text { at, size, .. } = &back.annotations[1].shape else { panic!() };
        assert!(at.x >= -MAX_DIM && at.y <= MAX_DIM && *size <= 1000.0 && back.annotations[1].width <= 200.0);
        assert_eq!(back.output_size, None);

        let newer =
            String::from_utf8(d.to_project_bytes().unwrap()).unwrap().replacen("\"version\":1", "\"version\":99", 1);
        assert!(Document::from_project_bytes(newer.as_bytes()).is_err());
    }

    #[test]
    fn undo_redo() {
        let mut d = doc();
        let mut u = UndoStack::default();
        u.checkpoint(&d);
        d.annotations.clear();
        assert!(u.undo(&mut d));
        assert_eq!(d.annotations.len(), 1);
        assert!(u.redo(&mut d));
        assert!(d.annotations.is_empty());
        assert!(!u.redo(&mut d));
    }
}
