//! Fonts used to write translations back into a page.
//!
//! Original PDF fonts are usually subsets that only contain the glyphs of the source text, so a translation can
//! rarely reuse them. Instead each block is written with the closest installed face: of the family that stands
//! in for the document's own where one was found (see [`FontSet::matched`]), and otherwise of the general
//! serif, sans or fixed-pitch family, in the weight and slant of the original and at its size and colour.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use celsus_pdf::font::Font;
use celsus_pdf::ttf::{TrueType, faces_in};

use crate::fit::Measure;
use crate::languages::Script;
use crate::layout::TextStyle;
use crate::typeface;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Family {
    Sans,
    Serif,
    Mono,
}

#[derive(Debug, Clone)]
pub struct FontFace {
    pub name: String,
    /// The name the font file gives the face (`Georgia-Bold`); empty where it gives none.
    pub postscript: String,
    pub font: TrueType,
    pub family: Family,
    pub bold: bool,
    pub italic: bool,
    /// Set for a font loaded for one writing system (Arabic, Devanagari, Chinese...); `None` for the general faces.
    pub script: Option<Script>,
    /// Set for a font of signs (circled numbers, keys, arrows): it lends the characters the text faces lack and
    /// is never the face a text is set in.
    pub symbols: bool,
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
        let postscript = faces_in(&mut std::io::Cursor::new(&data))
            .into_iter()
            .next()
            .map(|face| face.postscript)
            .unwrap_or_default();
        let font = TrueType::parse(data).map_err(|e| FontError::Parse(format!("{name}: {e}")))?;
        let mut face = Self::from_font(name, font, family, bold, italic);
        face.postscript = postscript;
        Ok(face)
    }

    fn from_font(name: String, font: TrueType, family: Family, bold: bool, italic: bool) -> Self {
        let upem = f32::from(font.units_per_em);
        let (ascent, descent) = (
            f32::from(font.ascender) / upem,
            -f32::from(font.descender) / upem,
        );
        Self {
            name,
            postscript: String::new(),
            font,
            family,
            bold,
            italic,
            script: None,
            symbols: false,
            ascent,
            descent,
        }
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
    /// How many of the faces are the general ones. The faces after them stand in for the families of one
    /// document and are used for nothing else.
    own: usize,
    /// The folder the faces came from and the writing system they were picked for; unknown for a set put
    /// together by hand.
    source: Option<(PathBuf, Option<Script>)>,
    /// The faces that stand in for a family of the document, by the family's name.
    matched: HashMap<String, Vec<usize>>,
    /// The faces for the document's other fonts without serifs, with serifs and of fixed pitch: those of the
    /// first family found of each kind, so that a book is not set half in one face and half in another.
    fallback: [Option<Vec<usize>>; 3],
}

impl FontSet {
    pub fn new(faces: Vec<FontFace>) -> Self {
        Self {
            own: faces.len(),
            faces,
            ..Default::default()
        }
    }

    /// The set with, for each family of fonts a document uses, the installed family closest to it: the same
    /// one where it is installed, and otherwise the one whose letters are nearest in width. Text in the
    /// style of a document font is then set in the family that stands in for it. Nothing changes for a set
    /// that was not read from a fonts folder, or where no family comes close.
    pub fn matched<'a>(&self, fonts: impl IntoIterator<Item = &'a Font>) -> FontSet {
        let mut set = self.clone();
        let Some((dir, script)) = &self.source else {
            return set;
        };
        for choice in typeface::choose(dir, *script, fonts) {
            let mut faces = Vec::new();
            for pick in &choice.faces {
                let name = pick.path.display().to_string();
                let loaded = set
                    .faces
                    .iter()
                    .position(|f| f.name == name && f.font.source().1 == pick.index);
                let at = match loaded {
                    Some(at) => at,
                    None => {
                        let Some(mut face) = std::fs::read(&pick.path)
                            .ok()
                            .and_then(|data| TrueType::parse_shared(Arc::new(data), pick.index).ok())
                            .map(|mut font| {
                                // Books are set with lining figures far more often than not.
                                if let Some(lining) = lining_figures(&font) {
                                    font.set_figures(lining);
                                }
                                FontFace::from_font(name, font, choice.class, pick.bold, pick.italic)
                            })
                        else {
                            continue;
                        };
                        face.postscript = pick.postscript.clone();
                        set.faces.push(face);
                        set.faces.len() - 1
                    }
                };
                faces.push(at);
            }
            if faces.is_empty() {
                continue;
            }
            set.fallback[choice.class as usize].get_or_insert_with(|| faces.clone());
            set.matched.insert(choice.key, faces);
        }
        set
    }

    pub fn faces(&self) -> &[FontFace] {
        &self.faces
    }

    pub fn is_empty(&self) -> bool {
        self.faces.is_empty()
    }

    /// The fonts to draw a block with, in order of preference: the general face closest to its style, the faces
    /// for `script`, then everything else.
    pub fn chain_for(&self, style: &TextStyle, script: Option<Script>) -> Vec<usize> {
        let for_script = (0..self.faces.len())
            .filter(|&i| script.is_some() && self.faces[i].script == script)
            .collect::<Vec<_>>();
        // Chinese, Japanese and Korean fonts carry their own digits and Latin letters, designed to sit with the
        // ideographs; drawing those with a Western face in between looks patched. Other scripts mix well.
        let cjk = matches!(
            script,
            Some(Script::Hans | Script::Hant | Script::Jpan | Script::Kore)
        );
        let mut chain: Vec<usize> = Vec::new();
        if cjk {
            chain.extend(&for_script);
        }
        chain.extend(self.best_for(style));
        chain.extend(for_script);
        chain.extend(0..self.faces.len());
        let mut seen = std::collections::HashSet::new();
        chain.retain(|i| seen.insert(*i));
        chain
    }

    /// Index of the face closest to `style`: of the family that stands in for the style's font where there is
    /// one; otherwise family first, then weight, then slant. General faces are preferred over fonts made for
    /// one writing system.
    pub fn best_for(&self, style: &TextStyle) -> Option<usize> {
        let want = if style.monospace {
            Family::Mono
        } else if style.serif {
            Family::Serif
        } else {
            Family::Sans
        };
        let stand_in = style
            .font_name
            .as_deref()
            .and_then(|name| self.matched.get(&typeface::family_key(name)))
            .or(self.fallback[want as usize].as_ref());
        if let Some(faces) = stand_in {
            // The faces come regular first, and of two that suit equally the earlier one is taken.
            return faces.iter().copied().rev().max_by_key(|&i| {
                let f = &self.faces[i];
                u8::from(f.bold == style.bold) * 2 + u8::from(f.italic == style.italic)
            });
        }
        let general = self.faces[..self.own].iter().any(|f| f.script.is_none());
        self.faces[..self.own]
            .iter()
            .enumerate()
            .filter(|(_, f)| !f.symbols && (!general || f.script.is_none()))
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

    /// Load the general faces from a directory (and its subfolders) by their common file names: Noto, Liberation
    /// and DejaVu (Linux), Arial, Times New Roman and Courier New (Windows and macOS). Missing faces are skipped.
    pub fn discover(dir: &Path) -> Self {
        Self::discover_for(dir, None)
    }

    /// The general faces plus the fonts for the writing system of `lang`.
    pub fn for_language(dir: &Path, lang: &crate::Language) -> Self {
        Self::discover_for(dir, crate::languages::info(lang).map(|i| i.script))
    }

    /// The general faces plus the fonts for one writing system, for translating into a language that uses it.
    pub fn discover_for(dir: &Path, script: Option<Script>) -> Self {
        // Of two faces of the same style the later one is used, so the list runs from the least wanted to the
        // most wanted.
        const GENERAL: &[(&str, Family, bool, bool)] = &[
            ("DejaVuSans.ttf", Family::Sans, false, false),
            ("DejaVuSans-Bold.ttf", Family::Sans, true, false),
            ("DejaVuSans-Oblique.ttf", Family::Sans, false, true),
            ("DejaVuSans-BoldOblique.ttf", Family::Sans, true, true),
            ("DejaVuSerif.ttf", Family::Serif, false, false),
            ("DejaVuSerif-Bold.ttf", Family::Serif, true, false),
            ("DejaVuSerif-Italic.ttf", Family::Serif, false, true),
            ("DejaVuSerif-BoldItalic.ttf", Family::Serif, true, true),
            ("DejaVuSansMono.ttf", Family::Mono, false, false),
            ("LiberationSans-Regular.ttf", Family::Sans, false, false),
            ("LiberationSans-Bold.ttf", Family::Sans, true, false),
            ("LiberationSans-Italic.ttf", Family::Sans, false, true),
            ("LiberationSans-BoldItalic.ttf", Family::Sans, true, true),
            ("LiberationSerif-Regular.ttf", Family::Serif, false, false),
            ("LiberationSerif-Bold.ttf", Family::Serif, true, false),
            ("LiberationSerif-Italic.ttf", Family::Serif, false, true),
            ("LiberationSerif-BoldItalic.ttf", Family::Serif, true, true),
            ("LiberationMono-Regular.ttf", Family::Mono, false, false),
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
            // macOS names the same fonts in full (its `Arial.ttf` is found as `arial.ttf` above).
            ("Arial Bold.ttf", Family::Sans, true, false),
            ("Arial Italic.ttf", Family::Sans, false, true),
            ("Arial Bold Italic.ttf", Family::Sans, true, true),
            ("Times New Roman.ttf", Family::Serif, false, false),
            ("Times New Roman Bold.ttf", Family::Serif, true, false),
            ("Times New Roman Italic.ttf", Family::Serif, false, true),
            ("Times New Roman Bold Italic.ttf", Family::Serif, true, true),
            ("Courier New.ttf", Family::Mono, false, false),
        ];
        let index = font_index(dir);
        let path = |file: &str| index.get(&file.to_ascii_lowercase()).cloned();
        let mut faces: Vec<FontFace> = GENERAL
            .iter()
            .filter_map(|(file, family, bold, italic)| {
                FontFace::from_file(&path(file)?, *family, *bold, *italic).ok()
            })
            .collect();
        if let Some(script) = script {
            let mut loaded = 0;
            for (file, family) in script_fonts(script) {
                if loaded == 2 {
                    break;
                }
                let Some(p) = path(file) else { continue };
                let Ok(data) = std::fs::read(&p) else {
                    continue;
                };
                // A collection's first font is its regular face (Microsoft YaHei, Yu Gothic, SimSun...).
                let Ok(font) = TrueType::parse_shared(Arc::new(data), 0) else {
                    continue;
                };
                let mut face =
                    FontFace::from_font(p.display().to_string(), font, *family, false, false);
                face.script = Some(script);
                faces.push(face);
                loaded += 1;
            }
        }
        // The numbers in circles that point from a listing to its notes, the signs of keys, arrows: few text
        // faces have them, and a paragraph is not left untranslated for the lack of one.
        const SYMBOLS: &[&str] = &[
            "NotoSansSymbols-Regular.ttf",
            "NotoSansSymbols2-Regular.ttf",
            "seguisym.ttf",
            "Apple Symbols.ttf",
        ];
        for file in SYMBOLS {
            let Some(p) = path(file) else { continue };
            if let Ok(mut face) = FontFace::from_file(&p, Family::Sans, false, false) {
                face.symbols = true;
                faces.push(face);
            }
        }
        Self {
            own: faces.len(),
            faces,
            source: Some((dir.to_path_buf(), script)),
            ..Default::default()
        }
    }
}

/// The feature that asks a font for its lining figures.
pub(crate) fn lining() -> rustybuzz::Feature {
    rustybuzz::Feature::new(rustybuzz::ttf_parser::Tag::from_bytes(b"lnum"), 1, ..)
}

/// The glyphs of a font's lining figures, where the figures it draws when not asked are others: those of the
/// old style, as in Georgia or Corbel.
fn lining_figures(font: &TrueType) -> Option<[u16; 10]> {
    const FIGURES: &str = "0123456789";
    let (data, index) = font.source();
    let face = rustybuzz::Face::from_slice(data, index)?;
    let mut buffer = rustybuzz::UnicodeBuffer::new();
    buffer.push_str(FIGURES);
    let shaped = rustybuzz::shape(&face, &[lining()], buffer);
    let glyphs: Vec<u16> = shaped.glyph_infos().iter().map(|g| g.glyph_id as u16).collect();
    let glyphs: [u16; 10] = glyphs.try_into().ok()?;
    // A font without figures of its own has no other ones either: what comes back for it is the glyph for a
    // character it lacks.
    let usual: Vec<u16> = FIGURES.chars().filter_map(|c| font.glyph(c)).collect();
    let drawn = usual.len() == glyphs.len() && usual.iter().chain(&glyphs).all(|&g| g != 0);
    (drawn && usual != glyphs).then_some(glyphs)
}

/// Font files under `dir` by lower-case file name, looking a few folders deep (Linux keeps fonts in
/// `truetype/noto/`, `truetype/wqy/`...). The first file of a name wins.
pub(crate) fn font_index(dir: &Path) -> HashMap<String, PathBuf> {
    let mut out = HashMap::new();
    let mut stack = vec![(dir.to_path_buf(), 0)];
    while let Some((d, depth)) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&d) else {
            continue;
        };
        for e in entries.flatten() {
            let p = e.path();
            if p.is_dir() {
                if depth < 4 {
                    stack.push((p, depth + 1));
                }
            } else if let Some(name) = p.file_name().and_then(|n| n.to_str()) {
                let lower = name.to_ascii_lowercase();
                if lower.ends_with(".ttf") || lower.ends_with(".ttc") {
                    out.entry(lower).or_insert(p);
                }
            }
        }
    }
    out
}

/// Fonts known to cover a writing system, best first: Noto (Linux, the Docker image), then the fonts Windows
/// and macOS ship. Only TrueType outlines can be embedded, so CFF-based files (most .otf) are not listed.
fn script_fonts(script: Script) -> &'static [(&'static str, Family)] {
    use Family::{Sans, Serif};
    match script {
        Script::Latn | Script::Cyrl | Script::Grek => &[],
        Script::Arab => &[
            ("NotoNaskhArabic-Regular.ttf", Serif),
            ("NotoSansArabic-Regular.ttf", Sans),
            ("segoeui.ttf", Sans),
            ("tahoma.ttf", Sans),
        ],
        Script::Hebr => &[
            ("NotoSerifHebrew-Regular.ttf", Serif),
            ("NotoSansHebrew-Regular.ttf", Sans),
            ("segoeui.ttf", Sans),
            ("tahoma.ttf", Sans),
        ],
        Script::Thaa => &[("NotoSansThaana-Regular.ttf", Sans), ("mvboli.ttf", Sans)],
        Script::Deva => &[
            ("NotoSerifDevanagari-Regular.ttf", Serif),
            ("NotoSansDevanagari-Regular.ttf", Sans),
            ("Nirmala.ttf", Sans),
            ("mangal.ttf", Sans),
            ("Arial Unicode.ttf", Sans),
        ],
        Script::Beng => &[
            ("NotoSerifBengali-Regular.ttf", Serif),
            ("NotoSansBengali-Regular.ttf", Sans),
            ("Nirmala.ttf", Sans),
            ("vrinda.ttf", Sans),
            ("Arial Unicode.ttf", Sans),
        ],
        Script::Guru => &[
            ("NotoSansGurmukhi-Regular.ttf", Sans),
            ("Nirmala.ttf", Sans),
            ("raavi.ttf", Sans),
            ("Arial Unicode.ttf", Sans),
        ],
        Script::Gujr => &[
            ("NotoSansGujarati-Regular.ttf", Sans),
            ("Nirmala.ttf", Sans),
            ("shruti.ttf", Sans),
            ("Arial Unicode.ttf", Sans),
        ],
        Script::Orya => &[
            ("NotoSansOriya-Regular.ttf", Sans),
            ("Nirmala.ttf", Sans),
            ("kalinga.ttf", Sans),
            ("NotoSansOriya.ttc", Sans),
            ("Arial Unicode.ttf", Sans),
        ],
        Script::Taml => &[
            ("NotoSansTamil-Regular.ttf", Sans),
            ("Nirmala.ttf", Sans),
            ("latha.ttf", Sans),
            ("Arial Unicode.ttf", Sans),
        ],
        Script::Telu => &[
            ("NotoSansTelugu-Regular.ttf", Sans),
            ("Nirmala.ttf", Sans),
            ("gautami.ttf", Sans),
            ("Arial Unicode.ttf", Sans),
        ],
        Script::Knda => &[
            ("NotoSansKannada-Regular.ttf", Sans),
            ("Nirmala.ttf", Sans),
            ("tunga.ttf", Sans),
            ("NotoSansKannada.ttc", Sans),
            ("Arial Unicode.ttf", Sans),
        ],
        Script::Mlym => &[
            ("NotoSansMalayalam-Regular.ttf", Sans),
            ("Nirmala.ttf", Sans),
            ("kartika.ttf", Sans),
            ("Arial Unicode.ttf", Sans),
        ],
        Script::Sinh => &[
            ("NotoSansSinhala-Regular.ttf", Sans),
            ("Nirmala.ttf", Sans),
            ("iskpota.ttf", Sans),
            ("Sinhala Sangam MN.ttc", Sans),
        ],
        Script::Thai => &[
            ("NotoSerifThai-Regular.ttf", Serif),
            ("NotoSansThai-Regular.ttf", Sans),
            ("LeelawUI.ttf", Sans),
            ("tahoma.ttf", Sans),
        ],
        Script::Laoo => &[
            ("NotoSansLao-Regular.ttf", Sans),
            ("LeelawUI.ttf", Sans),
            ("Lao Sangam MN.ttf", Sans),
            ("Arial Unicode.ttf", Sans),
        ],
        Script::Khmr => &[
            ("NotoSansKhmer-Regular.ttf", Sans),
            ("LeelawUI.ttf", Sans),
            ("Khmer Sangam MN.ttf", Sans),
        ],
        Script::Mymr => &[
            ("NotoSansMyanmar-Regular.ttf", Sans),
            ("mmrtext.ttf", Sans),
            ("NotoSansMyanmar.ttc", Sans),
        ],
        Script::Tibt => &[
            ("NotoSerifTibetan-Regular.ttf", Serif),
            ("himalaya.ttf", Serif),
            ("Kailasa.ttc", Sans),
            ("Arial Unicode.ttf", Sans),
        ],
        Script::Ethi => &[
            ("NotoSansEthiopic-Regular.ttf", Sans),
            ("ebrima.ttf", Sans),
            ("Kefa.ttc", Sans),
        ],
        Script::Armn => &[
            ("NotoSerifArmenian-Regular.ttf", Serif),
            ("NotoSansArmenian-Regular.ttf", Sans),
            ("sylfaen.ttf", Serif),
            ("NotoSansArmenian.ttc", Sans),
            ("Arial Unicode.ttf", Sans),
        ],
        Script::Geor => &[
            ("NotoSerifGeorgian-Regular.ttf", Serif),
            ("NotoSansGeorgian-Regular.ttf", Sans),
            ("sylfaen.ttf", Serif),
            ("Arial Unicode.ttf", Sans),
        ],
        Script::Hans => &[
            ("msyh.ttc", Sans),
            ("simsun.ttc", Serif),
            ("NotoSansSC-Regular.ttf", Sans),
            ("wqy-zenhei.ttc", Sans),
            ("wqy-microhei.ttc", Sans),
            ("PingFang.ttc", Sans),
            ("STHeiti Medium.ttc", Sans),
            ("Songti.ttc", Serif),
            ("DroidSansFallbackFull.ttf", Sans),
            ("Arial Unicode.ttf", Sans),
        ],
        Script::Hant => &[
            ("msjh.ttc", Sans),
            ("mingliu.ttc", Serif),
            ("NotoSansTC-Regular.ttf", Sans),
            ("wqy-zenhei.ttc", Sans),
            ("wqy-microhei.ttc", Sans),
            ("PingFang.ttc", Sans),
            ("STHeiti Medium.ttc", Sans),
            ("Songti.ttc", Serif),
            ("DroidSansFallbackFull.ttf", Sans),
            ("Arial Unicode.ttf", Sans),
        ],
        Script::Jpan => &[
            ("YuGothR.ttc", Sans),
            ("meiryo.ttc", Sans),
            ("msgothic.ttc", Sans),
            ("NotoSansJP-Regular.ttf", Sans),
            ("ipagp.ttf", Sans),
            ("wqy-zenhei.ttc", Sans),
            ("DroidSansFallbackFull.ttf", Sans),
            ("Arial Unicode.ttf", Sans),
        ],
        Script::Kore => &[
            ("malgun.ttf", Sans),
            ("NotoSansKR-Regular.ttf", Sans),
            ("NanumGothic.ttf", Sans),
            ("UnDotum.ttf", Sans),
            ("wqy-zenhei.ttc", Sans),
            ("AppleSDGothicNeo.ttc", Sans),
            ("AppleGothic.ttf", Sans),
            ("Arial Unicode.ttf", Sans),
        ],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The fonts of this machine, where it has any in one of the usual places.
    fn installed() -> Vec<PathBuf> {
        let dir = std::env::var_os("CELSUS_TEST_FONTS_DIR")
            .map(PathBuf::from)
            .into_iter()
            .chain([
                PathBuf::from(r"C:\Windows\Fonts"),
                PathBuf::from("/System/Library/Fonts"),
                PathBuf::from("/usr/share/fonts"),
            ])
            .find(|p| p.is_dir());
        let mut files: Vec<PathBuf> = dir.map(|d| font_index(&d).into_values().collect()).unwrap_or_default();
        files.sort();
        files
    }

    #[test]
    fn other_figures_are_drawn_and_measured_alike() {
        for path in installed().into_iter().take(120) {
            let Some(mut font) = std::fs::read(&path).ok().and_then(|d| TrueType::parse(d).ok()) else {
                continue;
            };
            let usual = font.text_width("0123456789", 10.0);
            let Some(lining) = lining_figures(&font) else {
                assert!(!font.has_other_figures());
                continue;
            };
            font.set_figures(lining);
            assert!(font.has_other_figures(), "{}", path.display());
            for (c, g) in "0123456789".chars().zip(lining) {
                assert_eq!(font.glyph(c), Some(g), "{}", path.display());
            }
            let wide: u32 = lining.iter().map(|&g| u32::from(font.advance(g))).sum();
            let wide = wide as f32 / f32::from(font.units_per_em) * 10.0;
            assert!((font.text_width("0123456789", 10.0) - wide).abs() < 1e-3);
            // Letters are left alone, and the figures are of a like width.
            assert_eq!(font.glyph('a'), TrueType::parse(std::fs::read(&path).unwrap()).unwrap().glyph('a'));
            assert!((wide - usual).abs() < usual * 0.5, "{}", path.display());
        }
    }
}
