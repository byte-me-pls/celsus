//! Page geometry extracted from a PDF: where each block of text sits and how it is styled.
//!
//! Coordinates are PDF points with the origin at the top-left of the page's crop box.

use serde::{Deserialize, Serialize};

use crate::inline;

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

/// From byte `at` of a line's text on, the words are set in another style: `style` 0 is the block's own, `k`
/// the block's `inline[k - 1]`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Span {
    pub at: u32,
    pub style: u8,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TextLine {
    pub rect: Rect,
    pub text: String,
    /// Distance of the baseline from the top of the page.
    #[serde(default)]
    pub baseline: f32,
    /// Where the style changes inside the line. A line starts in the block's own style.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub spans: Vec<Span>,
}

/// A large first letter with the first lines of a paragraph standing beside it.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Initial {
    /// From the top of the letter down to its baseline.
    pub rect: Rect,
    pub size: f32,
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
    /// Its lines are separate entries (a table of contents, a list), worked out against the whole column.
    #[serde(default)]
    pub separate_lines: bool,
    /// The paragraph opens with a large initial. Its letters are the first of the first line's text, but the
    /// rectangles of the lines do not include it.
    #[serde(default)]
    pub initial: Option<Initial>,
    /// The styles of words set apart inside the block: a title in italics, a term in bold, a command in a
    /// fixed-width face, a link in another colour.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub inline: Vec<TextStyle>,
}

/// The leader of a contents entry: five or more dots in a row, with or without spaces between them. Gives the
/// bytes from the first dot to the end of the last.
pub fn leader_span(text: &str) -> Option<std::ops::Range<usize>> {
    let (mut start, mut end, mut dots) = (0, 0, 0);
    for (at, c) in text.char_indices() {
        match c {
            '.' | '\u{2026}' | '\u{00b7}' => {
                if dots == 0 {
                    start = at;
                }
                dots += if c == '\u{2026}' { 3 } else { 1 };
                end = at + c.len_utf8();
            }
            c if c.is_whitespace() => {}
            _ if dots >= 5 => break,
            _ => dots = 0,
        }
    }
    (dots >= 5).then_some(start..end)
}

/// Whether most lines before the last stop well short of `right`, the edge running text would reach in a column
/// that starts at `left`.
pub fn lines_end_short(lines: &[TextLine], left: f32, right: f32) -> bool {
    let n = lines.len();
    let width = right - left;
    if n < 2 || width <= 0.0 {
        return false;
    }
    let short = lines[..n - 1]
        .iter()
        .filter(|l| l.rect.x1 < right - width * 0.25)
        .count();
    short >= 1 && short * 2 >= n - 1
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
        } else if ends_with_broken_word(&out)
            && line
                .trim_start_matches(|c| inline::style_of(c).is_some())
                .starts_with(|c: char| c.is_lowercase())
        {
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
    /// Whether the lines are separate entries rather than running text: a table of contents, a list, verse, a
    /// title over two lines. Most lines stop well before the right edge, where running text fills the measure.
    pub fn keeps_lines(&self) -> bool {
        self.separate_lines
            || (self.lines.len() >= 2 && lines_end_short(&self.lines, self.rect.x0, self.rect.x1))
    }

    /// The text alone. Lines that are separate entries stay on their own lines; running text is joined.
    pub fn plain(&self) -> String {
        self.joined(self.lines.iter().map(|l| l.text.trim().to_owned()))
    }

    /// The text to translate: [`TextBlock::plain`], with tags around the words set in another style.
    pub fn source(&self) -> String {
        if self.inline.is_empty() {
            return self.plain();
        }
        let lines = self.lines.iter().map(|l| {
            let mut out = String::with_capacity(l.text.len() + 4 * (l.spans.len() + 1));
            out.push(inline::mark(0));
            let mut from = 0;
            for span in &l.spans {
                let at = span.at as usize;
                if at >= from && at < l.text.len() && l.text.is_char_boundary(at) {
                    out.push_str(&l.text[from..at]);
                    out.push(inline::mark(usize::from(span.style)));
                    from = at;
                }
            }
            out.push_str(&l.text[from..]);
            out
        });
        let marked = inline::tidy(&self.joined(lines));
        inline::tagged(&marked, &inline::names(&self.style, &self.inline))
    }

    fn joined(&self, lines: impl Iterator<Item = String>) -> String {
        let lines: Vec<String> = lines
            .zip(&self.lines)
            .filter(|(_, l)| !l.text.trim().is_empty())
            .map(|(text, _)| text)
            .collect();
        if self.keeps_lines() {
            lines.join("\n")
        } else {
            join_lines(lines.iter().map(String::as_str))
        }
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

    fn line(x0: f32, x1: f32, y: f32, text: &str) -> TextLine {
        TextLine {
            rect: Rect {
                x0,
                y0: y,
                x1,
                y1: y + 10.0,
            },
            text: text.into(),
            baseline: y + 8.0,
            spans: Vec::new(),
        }
    }

    fn block(lines: Vec<TextLine>) -> TextBlock {
        let x0 = lines.iter().map(|l| l.rect.x0).fold(f32::MAX, f32::min);
        let x1 = lines.iter().map(|l| l.rect.x1).fold(f32::MIN, f32::max);
        TextBlock {
            id: BlockId("b1".into()),
            rect: Rect {
                x0,
                y0: 0.0,
                x1,
                y1: 100.0,
            },
            lines,
            style: TextStyle {
                font_name: None,
                font_size: 10.0,
                color: 0,
                bold: false,
                italic: false,
                serif: true,
                monospace: false,
            },
            align: Align::Left,
            protected: false,
            separate_lines: false,
            initial: None,
            inline: Vec::new(),
        }
    }

    #[test]
    fn words_set_apart_are_tagged_for_the_translator() {
        let mut paragraph = block(vec![
            line(100.0, 400.0, 0.0, "As the book Domain De-"),
            line(100.0, 398.0, 12.0, "sign says, run make and"),
            line(100.0, 210.0, 24.0, "wait."),
        ]);
        let italic = TextStyle {
            italic: true,
            ..paragraph.style.clone()
        };
        let code = TextStyle {
            monospace: true,
            ..paragraph.style.clone()
        };
        paragraph.inline = vec![italic, code];
        let span = |at, style| Span { at, style };
        paragraph.lines[0].spans = vec![span(12, 1)];
        paragraph.lines[1].spans = vec![span(0, 1), span(4, 0), span(15, 2), span(19, 0)];
        assert_eq!(
            paragraph.source(),
            "As the book <i>Domain Design</i> says, run <code>make</code> and wait."
        );
        assert_eq!(
            paragraph.plain(),
            "As the book Domain Design says, run make and wait."
        );
        // Entries on their own lines: no tag runs from one into the next.
        let mut list = block(vec![
            line(100.0, 160.0, 0.0, "Part II"),
            line(
                100.0,
                400.0,
                14.0,
                "The Building Blocks of a Model-Driven Design",
            ),
        ]);
        list.inline = vec![TextStyle {
            bold: true,
            ..list.style.clone()
        }];
        list.lines[0].spans = vec![span(0, 1)];
        list.lines[1].spans = vec![span(4, 1), span(12, 0)];
        assert_eq!(
            list.source(),
            "<b>Part II</b>\nThe <b>Building</b> Blocks of a Model-Driven Design"
        );
    }

    #[test]
    fn tables_of_contents_keep_their_lines_and_paragraphs_do_not() {
        let toc = block(vec![
            line(100.0, 260.0, 0.0, "Chapter 4: Isolating the Domain"),
            line(110.0, 230.0, 12.0, "LAYERED ARCHITECTURE"),
            line(120.0, 220.0, 24.0, "Relating the Layers"),
            line(
                100.0,
                400.0,
                36.0,
                "The Domain Layer Is Where the Model Lives and Breathes",
            ),
        ]);
        assert!(toc.keeps_lines());
        assert_eq!(toc.source().lines().count(), 4);
        let paragraph = block(vec![
            line(100.0, 400.0, 0.0, "Running text fills the measure from one"),
            line(
                100.0,
                398.0,
                12.0,
                "edge to the other, line after line, until",
            ),
            line(100.0, 210.0, 24.0, "the end."),
        ]);
        assert!(!paragraph.keeps_lines());
        assert!(!paragraph.source().contains('\n'));
        let title = block(vec![
            line(100.0, 160.0, 0.0, "Part II"),
            line(
                100.0,
                400.0,
                14.0,
                "The Building Blocks of a Model-Driven Design",
            ),
        ]);
        assert!(title.keeps_lines(), "a two-line title keeps its break");
    }

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
