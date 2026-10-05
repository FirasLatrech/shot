//! Platform-independent core of Shot: the annotation model, the renderer that
//! turns a document into pixels, the background tool, and on-disk storage.
//! Nothing in here touches macOS, so all of it is unit-testable.

pub mod annot;
pub mod background;
pub mod doc;
pub mod effects;
pub mod geom;
pub mod history;
pub mod logo;
pub mod render;
pub mod settings;
pub mod text;

pub use annot::{Annotation, ArrowStyle, Shape, TextStyle};
pub use background::{Background, Fill};
pub use doc::{Document, Layer};
pub use geom::{Color, Pt, Rect};
pub use render::render;
pub use text::Fonts;
