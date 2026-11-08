//! Page geometry extracted from a PDF: where each block of text sits and how it is styled.
//!
//! Coordinates are PDF points with the origin at the top-left of the page's crop box.

use serde::{Deserialize, Serialize};

/// `[x0, y0, x1, y1]` in points.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Rect {
    pub x0: f32,
    pub y0: f32,
    pub x1: f32,
    pub y1: f32,
}

impl Rect {
    pub fn width(&self) -> f32 {
        self.x1 - self.x0
    }

    pub fn height(&self) -> f32 {
        self.y1 - self.y0
    }

    pub fn union(&self, other: &Rect) -> Rect {
        Rect {
            x0: self.x0.min(other.x0),
            y0: self.y0.min(other.y0),
            x1: self.x1.max(other.x1),
            y1: self.y1.max(other.y1),
        }
    }

    /// Overlap with a small tolerance so that touching boxes do not count.
    pub fn overlaps(&self, other: &Rect, tolerance: f32) -> bool {
        self.x0 < other.x1 - tolerance
            && self.x1 > other.x0 + tolerance
            && self.y0 < other.y1 - tolerance
            && self.y1 > other.y0 + tolerance
    }
}

/// Stable identifier of a block within a page, e.g. `b0001`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct BlockId(pub String);

/// The original styling of a block, reused when the translation is written back.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TextStyle {
    pub font_name: Option<String>,
    pub font_size: f32,
    /// sRGB colour as `0xRRGGBB`.
    pub color: u32,
    pub bold: bool,
    pub italic: bool,
    pub serif: bool,
    pub monospace: bool,
}

/// How the lines of a block are aligned, detected from the original line positions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Align {
    Left,
    Center,
    Right,
    /// Every line but the last runs from the left to the right edge.
    Justify,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TextLine {
    pub rect: Rect,
    pub text: String,
    /// Distance of the baseline from the top of the page.
    #[serde(default)]
    pub baseline: f32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TextBlock {
    pub id: BlockId,
    pub rect: Rect,
    pub lines: Vec<TextLine>,
    pub style: TextStyle,
    pub align: Align,
    /// Code, formulas, numbers-only text: kept as is, never sent for translation.
    pub protected: bool,
}

/// Marks a word broken across lines with a hyphen that only exists because of the line break.
pub const SOFT_HYPHEN: char = '\u{AD}';

/// Join the lines of a block into running text: a line ending in a soft hyphen continues the word on the next
/// line, every other line break becomes a space.
pub fn join_lines<'s>(lines: impl IntoIterator<Item = &'s str>) -> String {
    let mut out = String::new();
    for line in lines {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if out.ends_with(SOFT_HYPHEN) {
            out.pop();
        } else if ends_with_broken_word(&out) && line.starts_with(|c: char| c.is_lowercase()) {
            // "under-" + "standing": a word hyphenated by line breaking.
            out.pop();
        } else if !out.is_empty() {
            out.push(' ');
        }
        out.push_str(line);
    }
    if out.ends_with(SOFT_HYPHEN) {
        out.pop();
    }
    out
}

fn ends_with_broken_word(text: &str) -> bool {
    let mut rev = text.chars().rev();
    rev.next() == Some('-') && rev.next().is_some_and(char::is_alphabetic)
}

impl TextBlock {
    pub fn source(&self) -> String {
        join_lines(self.lines.iter().map(|l| l.text.as_str()))
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PageLayout {
    pub page: u32,
    pub width: f32,
    pub height: f32,
    /// True when the text came from OCR of a scanned page rather than the PDF text layer.
    pub ocr: bool,
    pub blocks: Vec<TextBlock>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn joins_hyphenated_words() {
        let lines = ["the bounded con\u{AD}", "text and its", "model\u{AD}"];
        assert_eq!(join_lines(lines), "the bounded context and its model");
        assert_eq!(join_lines(["the un-", "derstanding"]), "the understanding");
        assert_eq!(join_lines(["page 12-", "Fowler"]), "page 12- Fowler");
    }

    #[test]
    fn touching_rects_do_not_overlap() {
        let a = Rect {
            x0: 0.0,
            y0: 0.0,
            x1: 10.0,
            y1: 10.0,
        };
        let b = Rect {
            x0: 10.0,
            y0: 0.0,
            x1: 20.0,
            y1: 10.0,
        };
        assert!(!a.overlaps(&b, 0.3));
        let c = Rect {
            x0: 5.0,
            y0: 5.0,
            x1: 15.0,
            y1: 15.0,
        };
        assert!(a.overlaps(&c, 0.3));
        assert_eq!(a.union(&b).width(), 20.0);
    }
}
