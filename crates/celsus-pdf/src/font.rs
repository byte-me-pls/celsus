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
        let name = match base.split_once('+') {
            Some((prefix, rest))
                if prefix.len() == 6 && prefix.bytes().all(|b| b.is_ascii_uppercase()) =>
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

        match kind {
            FontKind::Composite => font.load_composite(doc, dict, descendant.as_ref()),
            FontKind::Simple | FontKind::Type3 => font.load_simple(doc, dict, descriptor.as_ref()),
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

    fn load_simple(&mut self, doc: &Document, dict: &Dict, descriptor: Option<&Dict>) {
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
                        for (n, v) in next.as_array().unwrap().iter().enumerate() {
                            if let Some(v) = v.as_f64() {
                                self.cid_widths
                                    .insert(first as u32 + n as u32, v as f32 / 1000.0);
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
                        if last >= first
                            && last - first < 0x10000
                            && self.cid_widths.len() < 1 << 20
                        {
                            for cid in first..=last {
                                self.cid_widths.insert(cid as u32, v as f32 / 1000.0);
                            }
                        }
                        i += 3;
                    }
                    _ => break,
                }
            }
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
