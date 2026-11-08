//! Fonts used to write translations back into a page.
//!
//! Original PDF fonts are usually subsets that only contain the glyphs of the source text, so a translation can
//! rarely reuse them. Instead each block is written with the closest bundled face (serif/sans/mono, bold, italic),
//! keeping the original size and colour.

use std::path::Path;

use celsus_pdf::ttf::TrueType;

use crate::fit::Measure;
use crate::layout::TextStyle;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Family {
    Sans,
    Serif,
    Mono,
}

#[derive(Debug, Clone)]
pub struct FontFace {
    pub name: String,
    pub font: TrueType,
    pub family: Family,
    pub bold: bool,
    pub italic: bool,
    ascent: f32,
    descent: f32,
}

#[derive(Debug, thiserror::Error)]
pub enum FontError {
    #[error("cannot read font {0}: {1}")]
    Io(String, std::io::Error),
    #[error("not a usable TrueType/OpenType font: {0}")]
    Parse(String),
}

impl FontFace {
    pub fn from_bytes(
        name: impl Into<String>,
        data: Vec<u8>,
        family: Family,
        bold: bool,
        italic: bool,
    ) -> Result<Self, FontError> {
        let name = name.into();
        let font = TrueType::parse(data).map_err(|e| FontError::Parse(format!("{name}: {e}")))?;
        let upem = f32::from(font.units_per_em);
        let (ascent, descent) = (
            f32::from(font.ascender) / upem,
            -f32::from(font.descender) / upem,
        );
        Ok(Self {
            name,
            font,
            family,
            bold,
            italic,
            ascent,
            descent,
        })
    }

    pub fn from_file(
        path: &Path,
        family: Family,
        bold: bool,
        italic: bool,
    ) -> Result<Self, FontError> {
        let name = path.display().to_string();
        let data = std::fs::read(path).map_err(|e| FontError::Io(name.clone(), e))?;
        Self::from_bytes(name, data, family, bold, italic)
    }

    /// Characters of `text` this face has no glyph for (whitespace excluded).
    pub fn missing_chars(&self, text: &str) -> Vec<char> {
        let mut missing: Vec<char> = text
            .chars()
            .filter(|c| !c.is_whitespace() && self.font.glyph(*c).is_none())
            .collect();
        missing.sort_unstable();
        missing.dedup();
        missing
    }
}

impl Measure for FontFace {
    fn width(&self, text: &str, size: f32) -> f32 {
        self.font.text_width(text, size)
    }

    fn ascent(&self) -> f32 {
        self.ascent
    }

    fn descent(&self) -> f32 {
        self.descent
    }
}

/// The faces available for writing translations.
#[derive(Debug, Clone, Default)]
pub struct FontSet {
    faces: Vec<FontFace>,
}

impl FontSet {
    pub fn new(faces: Vec<FontFace>) -> Self {
        Self { faces }
    }

    pub fn faces(&self) -> &[FontFace] {
        &self.faces
    }

    pub fn is_empty(&self) -> bool {
        self.faces.is_empty()
    }

    /// Index of the face closest to `style`: family first, then weight, then slant.
    pub fn best_for(&self, style: &TextStyle) -> Option<usize> {
        let want = if style.monospace {
            Family::Mono
        } else if style.serif {
            Family::Serif
        } else {
            Family::Sans
        };
        self.faces
            .iter()
            .enumerate()
            .max_by_key(|(_, f)| {
                let family = match (f.family == want, f.family) {
                    (true, _) => 4,
                    (false, Family::Sans) => 2, // the safest stand-in for anything
                    _ => 0,
                };
                family + u8::from(f.bold == style.bold) * 2 + u8::from(f.italic == style.italic)
            })
            .map(|(i, _)| i)
    }

    /// Load a set from a directory using the common file names of the Noto, DejaVu and Liberation families,
    /// falling back to the standard Windows fonts. Missing faces are skipped.
    pub fn discover(dir: &Path) -> Self {
        const CANDIDATES: &[(&str, Family, bool, bool)] = &[
            ("NotoSans-Regular.ttf", Family::Sans, false, false),
            ("NotoSans-Bold.ttf", Family::Sans, true, false),
            ("NotoSans-Italic.ttf", Family::Sans, false, true),
            ("NotoSans-BoldItalic.ttf", Family::Sans, true, true),
            ("NotoSerif-Regular.ttf", Family::Serif, false, false),
            ("NotoSerif-Bold.ttf", Family::Serif, true, false),
            ("NotoSerif-Italic.ttf", Family::Serif, false, true),
            ("NotoSerif-BoldItalic.ttf", Family::Serif, true, true),
            ("NotoSansMono-Regular.ttf", Family::Mono, false, false),
            ("arial.ttf", Family::Sans, false, false),
            ("arialbd.ttf", Family::Sans, true, false),
            ("ariali.ttf", Family::Sans, false, true),
            ("arialbi.ttf", Family::Sans, true, true),
            ("times.ttf", Family::Serif, false, false),
            ("timesbd.ttf", Family::Serif, true, false),
            ("timesi.ttf", Family::Serif, false, true),
            ("timesbi.ttf", Family::Serif, true, true),
            ("cour.ttf", Family::Mono, false, false),
        ];
        let faces = CANDIDATES
            .iter()
            .filter_map(|(file, family, bold, italic)| {
                FontFace::from_file(&dir.join(file), *family, *bold, *italic).ok()
            })
            .collect();
        Self { faces }
    }
}
