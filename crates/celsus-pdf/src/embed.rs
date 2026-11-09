//! Embedding a TrueType font for new text: a Type0 font with Identity-H encoding, where every character code is
//! the glyph id. Only the glyphs that were used are kept, and a ToUnicode map keeps the text searchable.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;

use crate::object::{Dict, ObjRef, Object, Stream};
use crate::ttf::TrueType;
use crate::write::Saver;

pub struct FontEmbedder {
    pub font: TrueType,
    used: BTreeMap<u16, String>,
}

fn name(s: &str) -> Object {
    Object::Name(s.as_bytes().to_vec())
}

fn dict(entries: Vec<(&str, Object)>) -> Dict {
    entries
        .into_iter()
        .map(|(k, v)| (k.as_bytes().to_vec(), v))
        .collect()
}

impl FontEmbedder {
    pub fn new(font: TrueType) -> Self {
        FontEmbedder {
            font,
            used: BTreeMap::new(),
        }
    }

    /// Encode text for this font, two bytes per glyph. Characters the font lacks are skipped.
    pub fn encode(&mut self, text: &str) -> Vec<u8> {
        let mut out = Vec::with_capacity(text.len() * 2);
        for c in text.chars() {
            if let Some(g) = self.font.glyph(c) {
                self.used.entry(g).or_insert_with(|| c.to_string());
                out.extend_from_slice(&g.to_be_bytes());
            }
        }
        out
    }

    /// Encode one glyph chosen by a shaping engine. `text` is what it stands for (empty for the second and later
    /// glyphs of a cluster), so the page can still be searched and copied.
    pub fn encode_glyph(&mut self, gid: u16, text: &str) -> [u8; 2] {
        let entry = self.used.entry(gid).or_default();
        if entry.is_empty() && !text.is_empty() {
            *entry = text.to_owned();
        }
        gid.to_be_bytes()
    }

    pub fn is_used(&self) -> bool {
        !self.used.is_empty()
    }

    /// Write the font objects, putting the font dictionary at `target` (from [`Saver::reserve`]).
    pub fn write(&self, saver: &mut Saver, base_name: &str, target: ObjRef) {
        let gids: BTreeSet<u16> = self.used.keys().copied().collect();
        let program = self.font.subset(&gids);
        // A subset tag derived from the glyphs, as the specification asks for.
        let hash = gids
            .iter()
            .fold(5381u32, |h, &g| h.wrapping_mul(33) ^ u32::from(g));
        let tag: String = (0..6)
            .map(|i| (b'A' + ((hash >> (i * 5)) % 26) as u8) as char)
            .collect();
        let ps_name: String = base_name
            .chars()
            .filter(|c| c.is_ascii_alphanumeric() || *c == '-')
            .collect();
        let full = format!("{tag}+{ps_name}");

        let upem = f64::from(self.font.units_per_em);
        let scale = |v: i16| Object::Int((f64::from(v) * 1000.0 / upem).round() as i64);
        let flags = if self.font.italic { 32 | 64 } else { 32 };
        let file = saver.add(Object::Stream(Stream {
            dict: dict(vec![
                ("Length1", Object::Int(program.len() as i64)),
                ("Filter", name("FlateDecode")),
            ]),
            data: crate::filters::deflate(&program),
        }));
        let b = self.font.bbox;
        let descriptor = saver.add(Object::Dict(dict(vec![
            ("Type", name("FontDescriptor")),
            ("FontName", name(&full)),
            ("Flags", Object::Int(flags)),
            (
                "FontBBox",
                Object::Array(vec![scale(b[0]), scale(b[1]), scale(b[2]), scale(b[3])]),
            ),
            (
                "ItalicAngle",
                Object::Int(if self.font.italic { -12 } else { 0 }),
            ),
            ("Ascent", scale(self.font.ascender)),
            ("Descent", scale(self.font.descender)),
            ("CapHeight", scale(self.font.cap_height)),
            (
                "StemV",
                Object::Int(if self.font.weight >= 600 { 140 } else { 80 }),
            ),
            ("FontFile2", Object::Ref(file)),
        ])));

        let mut widths = Vec::new();
        for &g in &gids {
            let w = (f64::from(self.font.advance(g)) * 1000.0 / upem).round() as i64;
            widths.push(Object::Int(i64::from(g)));
            widths.push(Object::Array(vec![Object::Int(w)]));
        }
        let system_info = dict(vec![
            ("Registry", Object::String(b"Adobe".to_vec())),
            ("Ordering", Object::String(b"Identity".to_vec())),
            ("Supplement", Object::Int(0)),
        ]);
        let cid_font = saver.add(Object::Dict(dict(vec![
            ("Type", name("Font")),
            ("Subtype", name("CIDFontType2")),
            ("BaseFont", name(&full)),
            ("CIDSystemInfo", Object::Dict(system_info)),
            ("FontDescriptor", Object::Ref(descriptor)),
            ("W", Object::Array(widths)),
            ("DW", Object::Int(1000)),
            ("CIDToGIDMap", name("Identity")),
        ])));

        let mut cmap = String::from(concat!(
            "/CIDInit /ProcSet findresource begin\n12 dict begin\nbegincmap\n",
            "/CIDSystemInfo << /Registry (Adobe) /Ordering (UCS) /Supplement 0 >> def\n",
            "/CMapName /Adobe-Identity-UCS def\n/CMapType 2 def\n",
            "1 begincodespacerange\n<0000> <FFFF>\nendcodespacerange\n"
        ));
        let entries: Vec<(&u16, &String)> =
            self.used.iter().filter(|(_, t)| !t.is_empty()).collect();
        for chunk in entries.chunks(100) {
            writeln!(cmap, "{} beginbfchar", chunk.len()).unwrap();
            for (g, text) in chunk {
                let hex: String = text.encode_utf16().map(|u| format!("{u:04X}")).collect();
                writeln!(cmap, "<{g:04X}> <{hex}>").unwrap();
            }
            cmap.push_str("endbfchar\n");
        }
        cmap.push_str("endcmap\nCMapName currentdict /CMap defineresource pop\nend\nend\n");
        let to_unicode = saver.add(Object::Stream(Stream {
            dict: dict(vec![("Filter", name("FlateDecode"))]),
            data: crate::filters::deflate(cmap.as_bytes()),
        }));

        saver.set(
            target,
            Object::Dict(dict(vec![
                ("Type", name("Font")),
                ("Subtype", name("Type0")),
                ("BaseFont", name(&full)),
                ("Encoding", name("Identity-H")),
                (
                    "DescendantFonts",
                    Object::Array(vec![Object::Ref(cid_font)]),
                ),
                ("ToUnicode", Object::Ref(to_unicode)),
            ])),
        );
    }
}
