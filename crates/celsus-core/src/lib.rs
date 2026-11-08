//! Core library for Celsus: book model, languages and the translation pipeline.
//!
//! Both the server and the desktop app link this crate directly.

pub mod extract;
pub mod fit;
pub mod fonts;
pub mod language;
pub mod layout;
pub mod render;
pub mod translate;

pub use language::Language;
pub use layout::{Align, BlockId, PageLayout, Rect, TextBlock, TextStyle};
pub use translate::{Segment, SegmentStatus, TranslationRequest, Translator};
