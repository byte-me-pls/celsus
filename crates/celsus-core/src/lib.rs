//! Core library for Celsus: book model, languages and the translation pipeline.
//!
//! Both the server and the desktop app link this crate directly.

pub mod assist;
pub mod base64;
pub mod extract;
pub mod fit;
pub mod fonts;
pub mod inline;
pub mod language;
pub mod languages;
pub mod layout;
pub mod metadata;
pub mod pipeline;
pub mod place;
pub mod provider;
pub mod render;
pub mod shape;
pub mod translate;
mod typeface;

pub use language::Language;
pub use layout::{Align, BlockId, PageLayout, Rect, TextBlock, TextStyle};
pub use translate::{Segment, SegmentStatus, TranslationRequest, Translator};
