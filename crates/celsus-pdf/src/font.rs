//! Fonts as text extraction needs them: character codes to Unicode, glyph widths and style hints.

use std::collections::HashMap;

use crate::cmap::CMap;
use crate::document::Document;
use crate::encoding::{BaseEncoding, glyph_name_to_unicode};
use crate::object::{Dict, ObjRef, Object, get, numbers};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FontKind {
    /// Type1, TrueType, MMType1: one byte per code.
    Simple,
    /// Glyphs defined by content streams; widths are scaled by the font matrix.
    Type3,
    /// Type0 with a CID font: codes of one to four bytes.
    Composite,
}

#[derive(Debug, Clone)]
pub struct Font {
    pub id: Option<ObjRef>,
    /// The PostScript name without a subset prefix such as `ABCDEF+`.
    pub name: String,
    pub kind: FontKind,
    pub vertical: bool,
    pub bold: bool,
    pub italic: bool,
    pub serif: bool,
    pub fixed_pitch: bool,
    /// Ascent and descent in text space units (fractions of the font size).
    pub ascent: f32,
    pub descent: f32,
    /// The plain letters the font lists, each with its width at size 1: what its design can be told by.
    pub letters: Vec<(char, f32)>,
    simple_unicode: Vec<Option<String>>,
    simple_widths: Vec<f32>,
    cmap: CMap,
    to_unicode: Option<CMap>,
    cid_widths: HashMap<u32, f32>,
    default_width: f32,
    /// Glyph space to text space for widths: 0.001 except for Type3 fonts.
    width_scale: f32,
}

/// One character code of a shown string.
#[derive(Debug, Clone, PartialEq)]
pub struct Glyph {
    pub code: u32,
    /// Number of bytes the code took in the string.
    pub len: usize,
    /// Unicode text; empty when the font gives no way to know it.
    pub text: String,
    /// Horizontal advance in text space units at font size 1.
    pub width: f32,
    /// Single-byte code 32, which word spacing (`Tw`) applies to.
    pub is_space_code: bool,
}

impl Font {
    pub fn load(doc: &Document, dict: &Dict, id: Option<ObjRef>) -> Font {
        let subtype = get(dict, "Subtype")
            .and_then(Object::as_name)
            .unwrap_or(b"Type1");
        let base = doc
            .get_in(dict, "BaseFont")
            .and_then(|n| n.as_name().map(|n| String::from_utf8_lossy(n).into_owned()))
            .unwrap_or_default();
        // Six letters by the rule; some writers put six hexadecimal digits there.
        let name = match base.split_once('+') {
            Some((prefix, rest))
                if prefix.len() == 6
                    && (prefix.bytes().all(|b| b.is_ascii_uppercase())
                        || prefix.bytes().all(|b| b.is_ascii_hexdigit())) =>
            {
                rest.to_owned()
            }
            _ => base.clone(),
        };
        let kind = match subtype {
            b"Type0" => FontKind::Composite,
            b"Type3" => FontKind::Type3,
            _ => FontKind::Simple,
        };
        let mut font = Font {
            id,
            name,
            kind,
            vertical: false,
            bold: false,
            italic: false,
            serif: false,
            fixed_pitch: false,
            ascent: 0.8,
            descent: -0.2,
            letters: Vec::new(),
            simple_unicode: vec![None; 256],
            simple_widths: vec![0.0; 256],
            cmap: CMap::default(),
            to_unicode: None,
            cid_widths: HashMap::new(),
            default_width: 1.0,
            width_scale: 0.001,
        };
        font.to_unicode = doc
            .get_in(dict, "ToUnicode")
            .and_then(|o| o.as_stream().and_then(|s| doc.stream_data(s).ok()))
            .map(|data| CMap::parse(&data))
            .filter(|c| !c.unicode.is_empty());

        let descendant = (kind == FontKind::Composite)
            .then(|| doc.get_in(dict, "DescendantFonts"))
            .flatten()
            .and_then(|a| a.as_array().and_then(|a| a.first().cloned()))
            .and_then(|d| doc.resolve(&d).ok())
            .and_then(|d| d.as_dict().cloned());
        let descriptor_owner = descendant.as_ref().unwrap_or(dict);
        let descriptor = doc
            .get_in(descriptor_owner, "FontDescriptor")
            .and_then(|d| d.as_dict().cloned());
        font.apply_descriptor(doc, descriptor.as_ref());
        // The font itself, where it is a TrueType one put into the file. The largest ones (Chinese, Japanese)
        // are not unpacked for the little they would add.
        let program = descriptor
            .as_ref()
            .and_then(|d| doc.get_in(d, "FontFile2"))
            .and_then(|o| {
                let stream = o.as_stream()?;
                let size = doc.get_in(&stream.dict, "Length1").and_then(|l| l.as_i64());
                (size.unwrap_or(0) <= 4 << 20).then(|| doc.stream_data(stream).ok())?
            });
        // A font without a name worth the word still says in its own header whether it is bold or italic.
        if let Some((bold, italic)) = program.as_deref().and_then(crate::ttf::style_bits) {
            font.bold |= bold;
            font.italic |= italic;
        }

        match kind {
            FontKind::Composite => font.load_composite(doc, dict, descendant.as_ref()),
            FontKind::Simple | FontKind::Type3 => {
                font.load_simple(doc, dict, descriptor.as_ref(), program.as_deref())
            }
        }
        font
    }

    fn apply_descriptor(&mut self, doc: &Document, descriptor: Option<&Dict>) {
        let lower = self.name.to_ascii_lowercase();
        let flags = descriptor
            .and_then(|d| doc.get_in(d, "Flags"))
            .and_then(|f| f.as_i64())
            .unwrap_or(0);
        let num = |k: &str| {
            descriptor
                .and_then(|d| doc.get_in(d, k))
                .and_then(|v| v.as_f64())
        };
        self.fixed_pitch = flags & 1 != 0
            || ["courier", "mono", "consol", "menlo", "typewriter"]
                .iter()
                .any(|k| lower.contains(k));
        self.serif = flags & 2 != 0
            || [
                "times",
                "serif",
                "georgia",
                "garamond",
                "minion",
                "palatino",
                "caslon",
                "baskerville",
                "roman",
            ]
            .iter()
            .any(|k| lower.contains(k) && !lower.contains("sans"));
        self.italic = flags & 64 != 0
            || num("ItalicAngle").is_some_and(|a| a.abs() > 0.5)
            || ["italic", "oblique", "-it", ",it"]
                .iter()
                .any(|k| lower.contains(k));
        self.bold = flags & (1 << 18) != 0
            || num("FontWeight").is_some_and(|w| w >= 600.0)
            || ["bold", "black", "heavy", "semibold", "demi", ",bd", "-bd"]
                .iter()
                .any(|k| lower.contains(k));
        // Books set with TeX: the names say more than the flags do.
        if let Some(tex) = tex_style(&lower) {
            self.serif |= tex.serif;
            self.bold |= tex.bold;
            self.italic |= tex.italic;
            self.fixed_pitch |= tex.mono;
        }
        if let Some(a) = num("Ascent").filter(|a| *a > 100.0 && *a < 2000.0) {
            self.ascent = (a / 1000.0) as f32;
        }
        if let Some(d) = num("Descent").filter(|d| *d < 0.0 && *d > -1000.0) {
            self.descent = (d / 1000.0) as f32;
        }
        if let Some(m) = num("MissingWidth") {
            self.default_width = (m / 1000.0) as f32;
        }
    }

    fn load_simple(
        &mut self,
        doc: &Document,
        dict: &Dict,
        descriptor: Option<&Dict>,
        program: Option<&[u8]>,
    ) {
        // Widths.
        let first = doc
            .get_in(dict, "FirstChar")
            .and_then(|v| v.as_i64())
            .unwrap_or(0)
            .clamp(0, 255) as usize;
        if self.kind == FontKind::Type3
            && let Some(m) = doc
                .get_in(dict, "FontMatrix")
                .and_then(|m| numbers(&m))
                .filter(|m| m.len() == 6)
        {
            self.width_scale = m[0] as f32;
            if let Some(bbox) = doc
                .get_in(dict, "FontBBox")
                .and_then(|b| numbers(&b))
                .filter(|b| b.len() == 4)
            {
                let (lo, hi) = ((bbox[1] * m[3]) as f32, (bbox[3] * m[3]) as f32);
                if hi > lo && hi - lo < 3.0 {
                    self.ascent = hi;
                    self.descent = lo.min(0.0);
                }
            }
        }
        let missing = self.default_width;
        let widths = doc.get_in(dict, "Widths").and_then(|w| numbers(&w));
        let listed = first..first + widths.as_ref().map_or(0, Vec::len);
        match widths {
            Some(w) => {
                for (i, v) in w.iter().enumerate() {
                    if let Some(slot) = self.simple_widths.get_mut(first + i) {
                        *slot = *v as f32 * self.width_scale;
                    }
                }
                for (code, slot) in self.simple_widths.iter_mut().enumerate() {
                    if code < first || code >= first + w.len() {
                        *slot = missing * 1000.0 * self.width_scale;
                    }
                }
            }
            None => {
                // The standard 14 fonts may omit widths; use their metrics.
                for (code, slot) in self.simple_widths.iter_mut().enumerate() {
                    *slot = standard_width(&self.name, code as u8) / 1000.0;
                }
            }
        }

        // Encoding.
        let symbolic = descriptor
            .and_then(|d| doc.get_in(d, "Flags"))
            .and_then(|f| f.as_i64())
            .is_some_and(|f| f & 4 != 0 && f & 32 == 0)
            || ["symbol", "dingbat", "wingding"]
                .iter()
                .any(|k| self.name.to_ascii_lowercase().contains(k));
        let truetype = get(dict, "Subtype").and_then(Object::as_name) == Some(b"TrueType");
        let mut base = if symbolic && !truetype {
            BaseEncoding::None
        } else if truetype {
            BaseEncoding::WinAnsi
        } else {
            BaseEncoding::Standard
        };
        let mut differences: Vec<(u8, Vec<u8>)> = Vec::new();
        match doc.get_in(dict, "Encoding").as_deref() {
            Some(Object::Name(n)) => base = BaseEncoding::from_name(n).unwrap_or(base),
            Some(Object::Dict(enc)) => {
                if let Some(n) = doc
                    .get_in(enc, "BaseEncoding")
                    .and_then(|n| n.as_name().map(<[u8]>::to_vec))
                {
                    base = BaseEncoding::from_name(&n).unwrap_or(base);
                }
                if let Some(diffs) = doc.get_in(enc, "Differences") {
                    let mut code = 0i64;
                    for item in diffs.as_array().unwrap_or_default() {
                        match item {
                            Object::Int(c) => code = *c,
                            Object::Name(n) => {
                                if (0..256).contains(&code) {
                                    differences.push((code as u8, n.clone()));
                                }
                                code += 1;
                            }
                            _ => {}
                        }
                    }
                }
            }
            _ => {}
        }
        for code in 0..=255u8 {
            let mut text = base.char(code).map(String::from);
            if symbolic && text.is_none() && (32..127).contains(&code) {
                // Symbolic TrueType subsets frequently keep ASCII codes for ASCII text.
                text = Some((code as char).to_string());
            }
            self.simple_unicode[usize::from(code)] = text;
        }
        for (code, name) in differences {
            self.simple_unicode[usize::from(code)] = glyph_name_to_unicode(&name);
        }
        // Only the codes the font lists a width for: the others all share the width of a missing glyph.
        let letters = letter_widths(listed.filter(|&code| code < 256).filter_map(|code| {
            let text = match &self.to_unicode {
                Some(map) => map.unicode.get(&(code as u32)),
                None => self.simple_unicode[code].as_ref(),
            }?;
            Some((text.as_str(), self.simple_widths[code]))
        }));
        // A subset may list every code, and give the ones it leaves empty the width of the glyph the font
        // draws for a code it has no shape for.
        self.fixed_pitch |= even_pitch(&letters, None)
            || (letters.len() >= FEW_LETTERS
                && program
                    .and_then(empty_glyph_width)
                    .is_some_and(|empty| even_pitch(&letters, Some(empty))));
        self.letters = letters;
    }

    fn load_composite(&mut self, doc: &Document, dict: &Dict, descendant: Option<&Dict>) {
        match doc.get_in(dict, "Encoding").as_deref() {
            Some(Object::Name(n)) => {
                self.vertical = n.ends_with(b"-V");
                self.cmap = CMap::identity();
            }
            Some(Object::Stream(s)) => {
                self.vertical = get(&s.dict, "WMode").and_then(Object::as_i64) == Some(1);
                self.cmap = doc
                    .stream_data(s)
                    .map(|d| CMap::parse(&d))
                    .unwrap_or_else(|_| CMap::identity());
                if !self.cmap.has_codespaces() {
                    self.cmap = CMap::identity();
                }
            }
            _ => self.cmap = CMap::identity(),
        }
        let Some(cid_font) = descendant else { return };
        self.default_width = doc
            .get_in(cid_font, "DW")
            .and_then(|v| v.as_f64())
            .unwrap_or(1000.0) as f32
            / 1000.0;
        if let Some(w) = doc.get_in(cid_font, "W") {
            let items = w.as_array().unwrap_or_default();
            let mut i = 0;
            while i < items.len() {
                let Some(first) = items[i].as_i64() else {
                    i += 1;
                    continue;
                };
                match items.get(i + 1).map(|o| doc.resolve(o)) {
                    Some(Ok(next)) if next.as_array().is_some() => {
                        if let Ok(first) = u32::try_from(first)
                            && self.cid_widths.len() < MOST_WIDTHS
                        {
                            for (n, v) in next.as_array().unwrap().iter().enumerate() {
                                if let Some(v) = v.as_f64() {
                                    self.cid_widths
                                        .insert(first.saturating_add(n as u32), v as f32 / 1000.0);
                                }
                            }
                        }
                        i += 2;
                    }
                    Some(Ok(next)) => {
                        let (Some(last), Some(v)) =
                            (next.as_i64(), items.get(i + 2).and_then(Object::as_f64))
                        else {
                            break;
                        };
                        if let Some(range) = width_range(first, last)
                            && self.cid_widths.len() < MOST_WIDTHS
                        {
                            for cid in range {
                                self.cid_widths.insert(cid, v as f32 / 1000.0);
                            }
                        }
                        i += 3;
                    }
                    _ => break,
                }
            }
        }
        if let Some(map) = &self.to_unicode {
            let letters = letter_widths(map.unicode.iter().map(|(&code, text)| {
                let cid = self.cmap.cid(code);
                let width = self.cid_widths.get(&cid).copied();
                (text.as_str(), width.unwrap_or(self.default_width))
            }));
            self.fixed_pitch |= even_pitch(&letters, None);
            self.letters = letters;
        }
    }

    /// Split a shown string into glyphs.
    pub fn decode(&self, bytes: &[u8]) -> Vec<Glyph> {
        let codes: Vec<(u32, usize)> = match self.kind {
            FontKind::Composite => self.cmap.codes(bytes, 2),
            _ => bytes.iter().map(|&b| (u32::from(b), 1)).collect(),
        };
        codes
            .into_iter()
            .map(|(code, len)| {
                let mapped = self
                    .to_unicode
                    .as_ref()
                    .and_then(|t| t.unicode.get(&code))
                    .cloned();
                let (text, width) = match self.kind {
                    FontKind::Composite => {
                        let cid = self.cmap.cid(code);
                        (
                            mapped.unwrap_or_default(),
                            self.cid_widths
                                .get(&cid)
                                .copied()
                                .unwrap_or(self.default_width),
                        )
                    }
                    _ => {
                        let i = code as usize & 0xFF;
                        (
                            mapped
                                .or_else(|| self.simple_unicode[i].clone())
                                .unwrap_or_default(),
                            self.simple_widths[i],
                        )
                    }
                };
                Glyph {
                    code,
                    len,
                    text: clean(text),
                    width,
                    is_space_code: len == 1 && code == 32,
                }
            })
            .collect()
    }
}

/// A handful of letters of one width proves nothing about a font.
const FEW_LETTERS: usize = 12;

/// The plain letters among the glyphs of a font, each with its width.
fn letter_widths<'a>(glyphs: impl Iterator<Item = (&'a str, f32)>) -> Vec<(char, f32)> {
    glyphs
        .filter_map(|(text, width)| {
            let mut chars = text.chars();
            match (chars.next(), chars.next()) {
                (Some(c), None) if c.is_ascii_alphabetic() && width > 0.0 => Some((c, width)),
                _ => None,
            }
        })
        .collect()
}

/// Whether the letters of a font all advance by the same amount: that is what makes a face fixed pitch,
/// whatever its name and its flags say. Letters as wide as `empty`, the glyph for a missing shape, are
/// codes nothing is drawn from.
fn even_pitch(letters: &[(char, f32)], empty: Option<f32>) -> bool {
    let same = |a: f32, b: f32| (a - b).abs() <= b * 0.01;
    let mut seen: Vec<char> = Vec::new();
    let mut pitch = None;
    for &(c, width) in letters {
        if empty.is_some_and(|e| same(width, e)) {
            continue;
        }
        if !same(width, *pitch.get_or_insert(width)) {
            return false;
        }
        if !seen.contains(&c) {
            seen.push(c);
        }
    }
    seen.len() >= FEW_LETTERS
}

/// The width, in ems, of the first glyph of a TrueType font: the one drawn where a shape is missing.
fn empty_glyph_width(font: &[u8]) -> Option<f32> {
    let short = |at: usize| {
        font.get(at..at.checked_add(2)?)
            .map(|b| u16::from_be_bytes([b[0], b[1]]))
    };
    let table = |tag: &[u8; 4]| {
        (0..usize::from(short(4)?)).find_map(|i| {
            let entry = font.get(12 + 16 * i..28 + 16 * i)?;
            (&entry[..4] == tag)
                .then(|| u32::from_be_bytes([entry[8], entry[9], entry[10], entry[11]]) as usize)
        })
    };
    let per_em = short(table(b"head")?.checked_add(18)?)?;
    let width = short(table(b"hmtx")?)?;
    (per_em > 0).then(|| f32::from(width) / f32::from(per_em))
}

/// Drop control characters and private-use code points that carry no text.
fn clean(text: String) -> String {
    if text
        .chars()
        .all(|c| !c.is_control() && !('\u{e000}'..='\u{f8ff}').contains(&c))
    {
        return text;
    }
    text.chars()
        .filter(|c| !c.is_control() && !('\u{e000}'..='\u{f8ff}').contains(c))
        .collect()
}

/// Widths of the standard 14 fonts for ASCII, used only when a font dictionary has no /Widths.
fn standard_width(name: &str, code: u8) -> f32 {
    const HELVETICA: [u16; 95] = [
        278, 278, 355, 556, 556, 889, 667, 191, 333, 333, 389, 584, 278, 333, 278, 278, 556, 556,
        556, 556, 556, 556, 556, 556, 556, 556, 278, 278, 584, 584, 584, 556, 1015, 667, 667, 722,
        722, 667, 611, 778, 722, 278, 500, 667, 556, 833, 722, 778, 667, 778, 722, 667, 611, 722,
        667, 944, 667, 667, 611, 278, 278, 278, 469, 556, 333, 556, 556, 500, 556, 556, 278, 556,
        556, 222, 222, 500, 222, 833, 556, 556, 556, 556, 333, 500, 278, 556, 500, 722, 500, 500,
        500, 334, 260, 334, 584,
    ];
    const TIMES: [u16; 95] = [
        250, 333, 408, 500, 500, 833, 778, 180, 333, 333, 500, 564, 250, 333, 250, 278, 500, 500,
        500, 500, 500, 500, 500, 500, 500, 500, 278, 278, 564, 564, 564, 444, 921, 722, 667, 667,
        722, 611, 556, 722, 722, 333, 389, 722, 611, 889, 722, 722, 556, 722, 667, 556, 611, 722,
        722, 944, 722, 722, 611, 333, 278, 333, 469, 500, 333, 444, 500, 444, 500, 444, 333, 500,
        500, 278, 278, 500, 278, 778, 500, 500, 500, 500, 333, 389, 278, 500, 500, 722, 500, 500,
        444, 480, 200, 480, 541,
    ];
    let lower = name.to_ascii_lowercase();
    if lower.contains("courier") {
        return 600.0;
    }
    let table = if lower.contains("times") {
        &TIMES
    } else {
        &HELVETICA
    };
    match code {
        32..=126 => f32::from(table[usize::from(code - 32)]),
        _ => {
            if lower.contains("times") {
                500.0
            } else {
                556.0
            }
        }
    }
}

struct TexStyle {
    serif: bool,
    bold: bool,
    italic: bool,
    mono: bool,
}

/// The style of a Computer Modern text font from its (lower-case) name, e.g. `abcdef+cmbx12`: the family TeX
/// sets books in, whose names carry none of the usual words. Fonts of formulas and symbols are left alone.
fn tex_style(lower: &str) -> Option<TexStyle> {
    let name = lower.rsplit('+').next().unwrap_or(lower);
    let family = name.trim_end_matches(|c: char| c.is_ascii_digit());
    if family.len() == name.len() {
        return None;
    }
    let (serif, bold, italic, mono) = match family {
        "cmr" | "cmcsc" | "cmdunh" | "cmu" => (true, false, false, false),
        "cmb" | "cmbx" => (true, true, false, false),
        "cmti" | "cmsl" => (true, false, true, false),
        "cmbxti" | "cmbxsl" => (true, true, true, false),
        "cmss" => (false, false, false, false),
        "cmssbx" | "cmssdc" => (false, true, false, false),
        "cmssi" => (false, false, true, false),
        "cmtt" | "cmtcsc" => (false, false, false, true),
        "cmitt" | "cmsltt" => (false, false, true, true),
        _ => return None,
    };
    Some(TexStyle {
        serif,
        bold,
        italic,
        mono,
    })
}

/// More widths than any font has; a file made to waste memory stops here.
const MOST_WIDTHS: usize = 1 << 20;

/// The character codes `first..=last` of a width entry, if they are a range worth walking. The ends are whatever
/// the file says: negative, or so far apart that their difference does not fit a number.
fn width_range(first: i64, last: i64) -> Option<std::ops::RangeInclusive<u32>> {
    let (first, last) = (u32::try_from(first).ok()?, u32::try_from(last).ok()?);
    (last >= first && last - first < 0x10000).then_some(first..=last)
}

#[cfg(test)]
mod tex_tests {
    use super::{empty_glyph_width, even_pitch, letter_widths, tex_style};

    #[test]
    fn letters_of_one_width_make_a_fixed_pitch() {
        let letters = [
            "a", "b", "c", "d", "e", "f", "g", "i", "l", "m", "n", "o", "t", "w",
        ];
        let fixed =
            |glyphs: Vec<(&str, f32)>, empty| even_pitch(&letter_widths(glyphs.into_iter()), empty);
        assert!(fixed(letters.iter().map(|&c| (c, 0.5)).collect(), None));
        // The wide forms of another script and what is not a letter do not count.
        let mut mixed: Vec<(&str, f32)> = letters.iter().map(|&c| (c, 0.5)).collect();
        mixed.extend([("\u{3042}", 1.0), ("", 0.364), ("fi", 0.9)]);
        assert!(fixed(mixed, None));
        // A text face: the i is narrower than the m.
        let text = |c: &str| if c == "i" || c == "l" { 0.278 } else { 0.556 };
        assert!(!fixed(
            letters.iter().map(|&c| (c, text(c))).collect(),
            None
        ));
        // Numbers are of one width in most faces, and four letters are too few to tell.
        let digits = [
            "0", "1", "2", "3", "4", "5", "6", "7", "8", "9", "a", "b", "c", "d",
        ];
        assert!(!fixed(digits.iter().map(|&c| (c, 0.5)).collect(), None));
    }

    #[test]
    fn the_codes_a_subset_leaves_empty_do_not_hide_the_pitch() {
        // Every code is listed; the capitals are not in the subset and have the width of the empty glyph.
        let lower = "abcdefgiklnoprstuxy".chars().map(|c| (c, 0.5));
        let upper = ('A'..='Z').map(|c| (c, 0.364));
        let letters: Vec<(char, f32)> = lower.chain(upper).collect();
        assert!(!even_pitch(&letters, None));
        assert!(even_pitch(&letters, Some(0.364)));
        // Four letters of a text face that happen to be as wide as each other are still too few.
        let word: Vec<(char, f32)> = "page"
            .chars()
            .map(|c| (c, 0.556))
            .chain(upper_of_a_text_face())
            .collect();
        assert!(!even_pitch(&word, Some(0.6)));
    }

    fn upper_of_a_text_face() -> impl Iterator<Item = (char, f32)> {
        ('A'..='Z').map(|c| (c, 0.6))
    }

    #[test]
    fn the_empty_glyph_of_a_truetype_font_is_measured() {
        // Two tables: `head` with 1000 units to the em, `hmtx` whose first width is 364.
        let mut font = vec![0, 1, 0, 0, 0, 2, 0, 0, 0, 0, 0, 0];
        for (tag, offset, len) in [(b"head", 44u32, 54u32), (b"hmtx", 98, 4)] {
            font.extend_from_slice(tag);
            font.extend_from_slice(&[0; 4]);
            font.extend_from_slice(&offset.to_be_bytes());
            font.extend_from_slice(&len.to_be_bytes());
        }
        let mut head = [0u8; 54];
        head[18..20].copy_from_slice(&1000u16.to_be_bytes());
        font.extend_from_slice(&head);
        font.extend_from_slice(&[0x01, 0x6c, 0, 0]);
        assert_eq!(empty_glyph_width(&font), Some(0.364));
        // Cut short, or not a font at all.
        assert_eq!(empty_glyph_width(&font[..60]), None);
        assert_eq!(empty_glyph_width(b"%PDF-1.4"), None);
    }

    #[test]
    fn computer_modern_names_give_the_style() {
        let s = tex_style("cmr12").unwrap();
        assert!(s.serif && !s.bold && !s.italic && !s.mono);
        let s = tex_style("abcdef+cmbx10").unwrap();
        assert!(s.serif && s.bold);
        let s = tex_style("cmti12").unwrap();
        assert!(s.serif && s.italic);
        assert!(tex_style("cmtt10").unwrap().mono);
        assert!(!tex_style("cmss10").unwrap().serif);
        // Formula fonts, and names that only look alike.
        assert!(tex_style("cmmi10").is_none());
        assert!(tex_style("cmsy10").is_none());
        assert!(tex_style("cmr").is_none());
        assert!(tex_style("arial").is_none());
    }

    #[test]
    fn a_width_range_from_a_hostile_file_is_not_walked() {
        assert_eq!(super::width_range(10, 12), Some(10..=12));
        assert_eq!(super::width_range(7, 7), Some(7..=7));
        // Ends whose difference overflows, as in `/W [-9223372036854775808 9223372036854775807 500]`.
        assert_eq!(super::width_range(i64::MIN, i64::MAX), None);
        assert_eq!(super::width_range(-1, 5), None);
        assert_eq!(super::width_range(0, i64::MAX), None);
        assert_eq!(super::width_range(0, 0x10000), None);
        assert_eq!(super::width_range(5, 4), None);
    }
}
