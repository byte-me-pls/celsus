//! Turning a line of text into positioned glyphs, for every script.
//!
//! - Font fallback: each character is drawn with the font in use if it has the character, otherwise with the
//!   first font of the chain that does (an Arabic word inside English text, Latin inside Chinese).
//! - Direction: the Unicode bidirectional algorithm puts right-to-left text (Arabic, Hebrew, Persian, Urdu) and
//!   the numbers and Latin words inside it in visual order.
//! - Shaping: rustybuzz (HarfBuzz in Rust) joins Arabic letters, forms Indic conjuncts and places marks.
//!
//! Text in Latin, Greek and Cyrillic that the main font covers takes a direct path (character to glyph, advance
//! from the font), which is exactly what such text needs and much faster.

use std::cell::OnceCell;

use crate::fit::Measure;
use crate::fonts::FontSet;

#[derive(Debug, Clone, PartialEq)]
pub struct Glyph {
    pub gid: u16,
    /// How far the pen moves after this glyph, in points.
    pub advance: f32,
    /// Offset of the glyph from the pen position, in points.
    pub dx: f32,
    pub dy: f32,
    /// The text this glyph stands for (its whole cluster), on the first glyph of a cluster only; used so the
    /// output can be searched and copied.
    pub text: String,
}

/// Glyphs of one font, in visual order.
#[derive(Debug, Clone, PartialEq)]
pub struct Run {
    pub face: usize,
    pub glyphs: Vec<Glyph>,
    pub width: f32,
}

/// A line ready to draw: runs from left to right.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ShapedLine {
    pub runs: Vec<Run>,
    pub width: f32,
}

/// Shapes text with a chain of fonts: the block's own font first, then fonts for the target language's script,
/// then the rest.
pub struct Shaper<'f> {
    fonts: &'f FontSet,
    chain: Vec<usize>,
    /// The fonts as the shaping engine wants them, each read when a text first needs it.
    faces: Vec<OnceCell<Option<rustybuzz::Face<'f>>>>,
    /// The paragraph direction: right to left for Arabic, Hebrew and the like.
    pub rtl: bool,
}

/// Characters every line may contain that need no glyph of their own.
fn invisible(c: char) -> bool {
    c.is_control()
        || matches!(c, '\u{200B}'..='\u{200F}' | '\u{202A}'..='\u{202E}' | '\u{2060}'..='\u{2064}' | '\u{FEFF}')
}

impl<'f> Shaper<'f> {
    pub fn new(fonts: &'f FontSet, chain: Vec<usize>, rtl: bool) -> Self {
        let faces = fonts.faces().iter().map(|_| OnceCell::new()).collect();
        Shaper {
            fonts,
            chain,
            faces,
            rtl,
        }
    }

    pub fn primary(&self) -> usize {
        self.chain[0]
    }

    fn has(&self, face: usize, c: char) -> bool {
        self.fonts.faces()[face].font.glyph(c).is_some()
    }

    fn face_for(&self, c: char) -> Option<usize> {
        self.chain.iter().copied().find(|&f| self.has(f, c))
    }

    /// Characters no font of the chain can draw.
    pub fn missing(&self, text: &str) -> Vec<char> {
        let mut out: Vec<char> = text
            .chars()
            .filter(|&c| !c.is_whitespace() && !invisible(c) && self.face_for(c).is_none())
            .collect();
        out.sort_unstable();
        out.dedup();
        out
    }

    /// Text the main font draws on its own, left to right, without shaping.
    fn simple(&self, text: &str) -> bool {
        !self.rtl
            && text
                .chars()
                .all(|c| (c as u32) < 0x0590 && (c.is_whitespace() || self.has(self.primary(), c)))
    }

    pub fn shape_line(&self, text: &str, size: f32) -> ShapedLine {
        if text.is_empty() {
            return ShapedLine::default();
        }
        if self.simple(text) {
            return self.shape_simple(text, size);
        }
        let base = if self.rtl {
            unicode_bidi::Level::rtl()
        } else {
            unicode_bidi::Level::ltr()
        };
        let bidi = unicode_bidi::BidiInfo::new(text, Some(base));
        let Some(para) = bidi.paragraphs.first() else {
            return ShapedLine::default();
        };
        let (levels, bidi_runs) = bidi.visual_runs(para, para.range.clone());
        let mut line = ShapedLine::default();
        for range in bidi_runs {
            let rtl = levels[range.start].is_rtl();
            let mut runs = self
                .font_runs(&text[range.clone()], range.start)
                .into_iter()
                .filter_map(|(face, start, end)| self.shape_run(face, &text[start..end], size, rtl))
                .collect::<Vec<_>>();
            // Font runs inside a right-to-left stretch read right to left too.
            if rtl {
                runs.reverse();
            }
            for run in runs {
                line.width += run.width;
                line.runs.push(run);
            }
        }
        line
    }

    /// Split text into stretches drawn with one font: a character stays with the current font if it has it.
    fn font_runs(&self, text: &str, offset: usize) -> Vec<(usize, usize, usize)> {
        let mut out: Vec<(usize, usize, usize)> = Vec::new();
        let mut current: Option<usize> = None;
        for (i, c) in text.char_indices() {
            let keep = current.filter(|&f| self.has(f, c) || c.is_whitespace() || invisible(c));
            let face = keep
                .or_else(|| self.face_for(c))
                .or(current)
                .unwrap_or(self.primary());
            match out.last_mut() {
                Some(last) if last.0 == face => last.2 = offset + i + c.len_utf8(),
                _ => out.push((face, offset + i, offset + i + c.len_utf8())),
            }
            current = Some(face);
        }
        out
    }

    fn shape_run(&self, face: usize, text: &str, size: f32, rtl: bool) -> Option<Run> {
        let rb = self
            .faces
            .get(face)?
            .get_or_init(|| {
                let (data, index) = self.fonts.faces().get(face)?.font.source();
                rustybuzz::Face::from_slice(data, index)
            })
            .as_ref()?;
        let mut buffer = rustybuzz::UnicodeBuffer::new();
        buffer.push_str(text);
        buffer.set_direction(if rtl {
            rustybuzz::Direction::RightToLeft
        } else {
            rustybuzz::Direction::LeftToRight
        });
        buffer.guess_segment_properties();
        // A font whose figures were changed for its lining ones draws those here too.
        let lining = [crate::fonts::lining()];
        let other_figures = self.fonts.faces()[face].font.has_other_figures();
        let shaped = rustybuzz::shape(rb, if other_figures { &lining } else { &[] }, buffer);
        let scale = size / rb.units_per_em() as f32;
        let mut starts: Vec<usize> = shaped
            .glyph_infos()
            .iter()
            .map(|g| g.cluster as usize)
            .collect();
        starts.sort_unstable();
        starts.dedup();
        let end_of = |s: usize| {
            starts
                .iter()
                .copied()
                .find(|&x| x > s)
                .unwrap_or(text.len())
        };
        let mut seen = std::collections::BTreeSet::new();
        let mut glyphs = Vec::with_capacity(shaped.len());
        let mut width = 0.0;
        for (info, pos) in shaped.glyph_infos().iter().zip(shaped.glyph_positions()) {
            let start = info.cluster as usize;
            let text_of = if seen.insert(start) {
                text.get(start..end_of(start)).unwrap_or("").to_owned()
            } else {
                String::new()
            };
            let advance = pos.x_advance as f32 * scale;
            width += advance;
            glyphs.push(Glyph {
                gid: info.glyph_id as u16,
                advance,
                dx: pos.x_offset as f32 * scale,
                dy: pos.y_offset as f32 * scale,
                text: text_of,
            });
        }
        self.spread_cluster_text(face, &mut glyphs);
        Some(Run {
            face,
            glyphs,
            width,
        })
    }

    /// A glyph id maps to one text in the PDF, so a glyph used in several clusters (the Devanagari vowel sign ि
    /// in वि and ति) must carry its own character, not its cluster's. Within a cluster, each glyph that is the
    /// font's glyph for one of the cluster's characters takes that character; what is left (a ligature such as
    /// क्ष or لا) goes to the first glyph that has none.
    fn spread_cluster_text(&self, face: usize, glyphs: &mut [Glyph]) {
        let font = &self.fonts.faces()[face].font;
        let mut i = 0;
        while i < glyphs.len() {
            let end = (i + 1..glyphs.len())
                .find(|&j| !glyphs[j].text.is_empty())
                .unwrap_or(glyphs.len());
            if end - i > 1 {
                let text = std::mem::take(&mut glyphs[i].text);
                let mut left: Vec<char> = text.chars().collect();
                let mut assigned = vec![false; end - i];
                for (k, g) in glyphs[i..end].iter_mut().enumerate() {
                    if let Some(pos) = left.iter().position(|&c| font.glyph(c) == Some(g.gid)) {
                        g.text = left.remove(pos).to_string();
                        assigned[k] = true;
                    }
                }
                if !left.is_empty() {
                    let k = assigned.iter().position(|a| !a).unwrap_or(0);
                    glyphs[i + k].text.extend(left);
                }
            }
            i = end;
        }
    }

    fn shape_simple(&self, text: &str, size: f32) -> ShapedLine {
        let face = self.primary();
        let font = &self.fonts.faces()[face].font;
        let scale = size / f32::from(font.units_per_em);
        let mut glyphs = Vec::with_capacity(text.len());
        let mut width = 0.0;
        for c in text.chars() {
            let Some(gid) = font.glyph(c) else { continue };
            let advance = f32::from(font.advance(gid)) * scale;
            width += advance;
            glyphs.push(Glyph {
                gid,
                advance,
                dx: 0.0,
                dy: 0.0,
                text: c.to_string(),
            });
        }
        ShapedLine {
            runs: vec![Run {
                face,
                glyphs,
                width,
            }],
            width,
        }
    }
}

impl Measure for Shaper<'_> {
    fn width(&self, text: &str, size: f32) -> f32 {
        if self.simple(text) {
            return self.fonts.faces()[self.primary()]
                .font
                .text_width(text, size);
        }
        self.shape_line(text, size).width
    }

    fn ascent(&self) -> f32 {
        self.fonts.faces()[self.primary()].ascent()
    }

    fn descent(&self) -> f32 {
        self.fonts.faces()[self.primary()].descent()
    }
}
