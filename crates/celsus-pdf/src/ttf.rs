//! Reading TrueType fonts (glyph outlines in `glyf`) and cutting them down to the glyphs a document uses.
//!
//! The subset keeps every glyph id where it was and only empties the outlines that are not needed. Glyph ids can
//! then be used directly as CIDs (`/CIDToGIDMap /Identity`), and the file shrinks to the glyphs actually drawn.

use std::collections::{BTreeMap, BTreeSet};
use std::io::{Read, Seek, SeekFrom};
use std::sync::Arc;

use crate::{Error, Result};

#[derive(Debug, Clone)]
pub struct TrueType {
    /// The whole file, shared: CJK fonts are tens of megabytes and every copy of a face points at the same bytes.
    data: Arc<Vec<u8>>,
    /// Which font of a collection (.ttc) this is; 0 for a single font.
    index: u32,
    tables: BTreeMap<[u8; 4], (usize, usize)>,
    pub units_per_em: u16,
    pub ascender: i16,
    pub descender: i16,
    pub num_glyphs: u16,
    long_loca: bool,
    advances: Vec<u16>,
    cmap: BTreeMap<u32, u16>,
    /// From the font's OS/2 table: weight class (400 regular, 700 bold) and the italic bit.
    pub weight: u16,
    pub italic: bool,
    pub cap_height: i16,
    pub bbox: [i16; 4],
    /// Glyphs to draw the figures 0 to 9 with in place of the ones the character map names.
    figures: Option<[u16; 10]>,
}

/// What a font file says about one of its faces: its names, its weight and slant, the class of its design.
/// Read from the small tables alone, so that a folder of fonts can be looked through without loading them.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct FaceInfo {
    /// Which font of a collection (.ttc); 0 for a single font.
    pub index: u32,
    /// The family the face belongs to, as menus show it: `Georgia`, `Times New Roman`.
    pub family: String,
    /// The name PDF files call the face by: `Georgia-BoldItalic`.
    pub postscript: String,
    /// 400 for a regular face, 700 for a bold one.
    pub weight: u16,
    pub italic: bool,
    /// The ten PANOSE numbers: the kind of design, its serifs, its weight, its proportions...
    pub panose: [u8; 10],
    /// Whether the licence written in the file lets a part of the font go into a document.
    pub embeddable: bool,
    /// The Unicode ranges the font says it covers, bit by bit.
    pub ranges: [u32; 4],
    /// Outlines in `glyf`: the kind that can be read and cut down here.
    pub truetype: bool,
}

/// The faces of a font file or collection. Nothing comes back for a file that is not a font.
pub fn faces_in<R: Read + Seek>(file: &mut R) -> Vec<FaceInfo> {
    let mut head = [0u8; 12];
    if read_at(file, 0, &mut head).is_none() {
        return Vec::new();
    }
    let starts: Vec<u64> = if &head[..4] == b"ttcf" {
        let count = u32::from_be_bytes([head[8], head[9], head[10], head[11]]).min(64) as usize;
        let mut offsets = vec![0u8; count * 4];
        if read_at(file, 12, &mut offsets).is_none() {
            return Vec::new();
        }
        offsets
            .chunks_exact(4)
            .map(|b| u64::from(u32::from_be_bytes([b[0], b[1], b[2], b[3]])))
            .collect()
    } else {
        vec![0]
    };
    starts
        .iter()
        .enumerate()
        .filter_map(|(index, &at)| face_at(file, at, index as u32))
        .collect()
}

fn read_at<R: Read + Seek>(file: &mut R, at: u64, buf: &mut [u8]) -> Option<()> {
    file.seek(SeekFrom::Start(at)).ok()?;
    file.read_exact(buf).ok()
}

fn face_at<R: Read + Seek>(file: &mut R, at: u64, index: u32) -> Option<FaceInfo> {
    let mut head = [0u8; 12];
    read_at(file, at, &mut head)?;
    let version = u32::from_be_bytes([head[0], head[1], head[2], head[3]]);
    if ![0x0001_0000, 0x7472_7565, 0x4F54_544F].contains(&version) {
        return None;
    }
    let count = usize::from(u16::from_be_bytes([head[4], head[5]]));
    let mut directory = vec![0u8; count.min(256) * 16];
    read_at(file, at + 12, &mut directory)?;
    let table = |tag: &[u8; 4]| {
        directory
            .chunks_exact(16)
            .find(|e| &e[..4] == tag)
            .map(|e| {
                (
                    u64::from(u32::from_be_bytes([e[8], e[9], e[10], e[11]])),
                    u32::from_be_bytes([e[12], e[13], e[14], e[15]]) as usize,
                )
            })
    };
    let mut info = FaceInfo {
        index,
        weight: 400,
        embeddable: true,
        truetype: table(b"glyf").is_some(),
        ..FaceInfo::default()
    };
    if let Some((at, len)) = table(b"OS/2").filter(|(_, len)| *len >= 64) {
        let mut os2 = vec![0u8; len.min(64)];
        read_at(file, at, &mut os2)?;
        let short = |o: usize| u16::from_be_bytes([os2[o], os2[o + 1]]);
        info.weight = short(4);
        // Bit 1 alone forbids embedding; bits 8 and 9 forbid a subset and allow bitmaps only.
        let licence = short(8);
        info.embeddable = licence & 0x000E != 0x0002 && licence & 0x0300 == 0;
        info.panose.copy_from_slice(&os2[32..42]);
        for (k, range) in info.ranges.iter_mut().enumerate() {
            let o = 42 + 4 * k;
            *range = u32::from_be_bytes([os2[o], os2[o + 1], os2[o + 2], os2[o + 3]]);
        }
        // Italic, or oblique.
        info.italic = short(62) & 0x0201 != 0;
    }
    let (at, len) = table(b"name")?;
    let mut names = vec![0u8; len.min(1 << 16)];
    read_at(file, at, &mut names)?;
    let [plain, postscript, typographic] = names_in(&names, [1, 6, 16]);
    info.family = typographic.or(plain)?;
    info.postscript = postscript.unwrap_or_default();
    Some(info)
}

/// The names of the given kinds in a `name` table, in English where the font has them in several languages.
fn names_in<const N: usize>(table: &[u8], ids: [u16; N]) -> [Option<String>; N] {
    let mut found: [Option<(u8, String)>; N] = std::array::from_fn(|_| None);
    let count = usize::from(u16_at(table, 2).unwrap_or(0));
    let strings = usize::from(u16_at(table, 4).unwrap_or(0));
    for i in 0..count {
        let at = 6 + 12 * i;
        let (Some(platform), Some(language), Some(id), Some(len), Some(offset)) = (
            u16_at(table, at),
            u16_at(table, at + 4),
            u16_at(table, at + 6),
            u16_at(table, at + 8),
            u16_at(table, at + 10),
        ) else {
            break;
        };
        let Some(slot) = ids.iter().position(|&wanted| wanted == id) else {
            continue;
        };
        let start = strings + usize::from(offset);
        let Some(bytes) = table.get(start..start + usize::from(len)) else {
            continue;
        };
        // Windows and Unicode names are UTF-16; the old Macintosh ones one byte a letter.
        let (rank, text) = match (platform, language) {
            (3, 0x0409) => (4, utf16(bytes)),
            (3, _) => (2, utf16(bytes)),
            (0, _) => (3, utf16(bytes)),
            (1, 0) => (1, bytes.iter().map(|&b| char::from(b)).collect()),
            _ => continue,
        };
        let text = text.trim().to_owned();
        if !text.is_empty() && found[slot].as_ref().is_none_or(|(best, _)| rank > *best) {
            found[slot] = Some((rank, text));
        }
    }
    found.map(|f| f.map(|(_, text)| text))
}

fn utf16(bytes: &[u8]) -> String {
    let units: Vec<u16> = bytes
        .chunks_exact(2)
        .map(|b| u16::from_be_bytes([b[0], b[1]]))
        .collect();
    String::from_utf16_lossy(&units)
}

/// What the `head` table of a font program says of its style: bold, italic. Fonts put into a PDF often have
/// lost their names and the table that tells the weight; this much stays.
pub fn style_bits(font: &[u8]) -> Option<(bool, bool)> {
    let count = usize::from(u16_at(font, 4)?);
    let head = (0..count).find_map(|i| {
        let entry = font.get(12 + 16 * i..28 + 16 * i)?;
        (&entry[..4] == b"head").then(|| u32_at(entry, 8).map(|o| o as usize))?
    })?;
    let bits = u16_at(font, head.checked_add(44)?)?;
    Some((bits & 1 != 0, bits & 2 != 0))
}

fn u16_at(d: &[u8], o: usize) -> Option<u16> {
    d.get(o..o + 2).map(|b| u16::from_be_bytes([b[0], b[1]]))
}

fn i16_at(d: &[u8], o: usize) -> Option<i16> {
    u16_at(d, o).map(|v| v as i16)
}

fn u32_at(d: &[u8], o: usize) -> Option<u32> {
    d.get(o..o + 4)
        .map(|b| u32::from_be_bytes([b[0], b[1], b[2], b[3]]))
}

fn bad(what: &str) -> Error {
    Error::Unsupported(format!("font: {what}"))
}

impl TrueType {
    pub fn parse(data: Vec<u8>) -> Result<Self> {
        Self::parse_shared(Arc::new(data), 0)
    }

    /// How many fonts a file holds: more than one for a collection (.ttc).
    pub fn count_in(data: &[u8]) -> u32 {
        if data.starts_with(b"ttcf") {
            u32_at(data, 8).unwrap_or(0)
        } else {
            1
        }
    }

    /// Font `index` of a file; collections (.ttc) hold several, a plain font only index 0.
    pub fn parse_shared(data: Arc<Vec<u8>>, index: u32) -> Result<Self> {
        let base = if data.starts_with(b"ttcf") {
            if index >= Self::count_in(&data) {
                return Err(bad("no such font in the collection"));
            }
            u32_at(&data, 12 + index as usize * 4).ok_or(bad("collection header"))? as usize
        } else if index == 0 {
            0
        } else {
            return Err(bad("not a font collection"));
        };
        let version = u32_at(&data, base).ok_or(bad("too short"))?;
        if version == 0x4F54544F {
            return Err(bad(
                "CFF-based OpenType fonts are not supported; use a TrueType (.ttf) font",
            ));
        }
        if version != 0x0001_0000 && version != 0x7472_7565 {
            return Err(bad("not a TrueType font"));
        }
        let count = u16_at(&data, base + 4).ok_or(bad("header"))? as usize;
        let mut tables = BTreeMap::new();
        for i in 0..count {
            let rec = base + 12 + i * 16;
            let tag: [u8; 4] = data
                .get(rec..rec + 4)
                .ok_or(bad("table directory"))?
                .try_into()
                .unwrap();
            let offset = u32_at(&data, rec + 8).ok_or(bad("table directory"))? as usize;
            let len = u32_at(&data, rec + 12).ok_or(bad("table directory"))? as usize;
            if offset.checked_add(len).is_none_or(|end| end > data.len()) {
                return Err(bad("table outside the file"));
            }
            tables.insert(tag, (offset, len));
        }
        let t = |tag: &[u8; 4]| {
            tables
                .get(tag)
                .copied()
                .ok_or_else(|| bad(&format!("missing {}", String::from_utf8_lossy(tag))))
        };
        let (head, _) = t(b"head")?;
        let (hhea, _) = t(b"hhea")?;
        let (maxp, _) = t(b"maxp")?;
        let (hmtx, hmtx_len) = t(b"hmtx")?;
        t(b"loca")?;
        t(b"glyf")?;
        let units_per_em = u16_at(&data, head + 18).ok_or(bad("head"))?.max(16);
        let bbox = [
            i16_at(&data, head + 36).unwrap_or(0),
            i16_at(&data, head + 38).unwrap_or(0),
            i16_at(&data, head + 40).unwrap_or(0),
            i16_at(&data, head + 42).unwrap_or(0),
        ];
        let long_loca = i16_at(&data, head + 50).ok_or(bad("head"))? == 1;
        let ascender = i16_at(&data, hhea + 4).ok_or(bad("hhea"))?;
        let descender = i16_at(&data, hhea + 6).ok_or(bad("hhea"))?;
        let metrics = u16_at(&data, hhea + 34).ok_or(bad("hhea"))? as usize;
        let num_glyphs = u16_at(&data, maxp + 4).ok_or(bad("maxp"))?;
        let mut advances = Vec::with_capacity(num_glyphs as usize);
        let mut last = 0;
        for g in 0..num_glyphs as usize {
            if g < metrics && (g + 1) * 4 <= hmtx_len {
                last = u16_at(&data, hmtx + g * 4).unwrap_or(last);
            }
            advances.push(last);
        }
        let (mut weight, mut italic, mut cap_height) = (400, false, (ascender as f32 * 0.7) as i16);
        if let Some(&(os2, len)) = tables.get(b"OS/2") {
            weight = u16_at(&data, os2 + 4).unwrap_or(400);
            italic = u16_at(&data, os2 + 62).is_some_and(|s| s & 1 != 0);
            if len >= 90 {
                cap_height = i16_at(&data, os2 + 88).unwrap_or(cap_height);
            }
        }
        let mut font = TrueType {
            data,
            index,
            tables,
            units_per_em,
            ascender,
            descender,
            num_glyphs,
            long_loca,
            advances,
            cmap: BTreeMap::new(),
            weight,
            italic,
            cap_height,
            bbox,
            figures: None,
        };
        font.cmap = font.read_cmap()?;
        Ok(font)
    }

    /// Draw the figures 0 to 9 with these glyphs: the lining figures of a font whose usual ones are of the
    /// old style, with parts below the line. Left as it is where one of the glyphs is not in the font.
    pub fn set_figures(&mut self, glyphs: [u16; 10]) {
        if glyphs.iter().all(|&g| g < self.num_glyphs) {
            self.figures = Some(glyphs);
        }
    }

    /// Whether the figures are drawn with other glyphs than the character map names.
    pub fn has_other_figures(&self) -> bool {
        self.figures.is_some()
    }

    /// The file the font came from and its index in it, for a shaping engine that reads OpenType layout tables.
    pub fn source(&self) -> (&[u8], u32) {
        (&self.data, self.index)
    }

    fn table(&self, tag: &[u8; 4]) -> &[u8] {
        self.tables
            .get(tag)
            .map_or(&[][..], |&(o, l)| &self.data[o..o + l])
    }

    fn read_cmap(&self) -> Result<BTreeMap<u32, u16>> {
        let cmap = self.table(b"cmap");
        let n = u16_at(cmap, 2).ok_or(bad("cmap"))? as usize;
        // Prefer full Unicode (3,10 or 0,4+), then BMP (3,1 or 0,x).
        let mut best: Option<(u8, usize)> = None;
        for i in 0..n {
            let rec = 4 + i * 8;
            let (platform, encoding) = (
                u16_at(cmap, rec).unwrap_or(0),
                u16_at(cmap, rec + 2).unwrap_or(0),
            );
            let offset = u32_at(cmap, rec + 4).unwrap_or(0) as usize;
            let rank = match (platform, encoding) {
                (3, 10) | (0, 4) | (0, 6) => 3,
                (3, 1) | (0, 3) => 2,
                (0, _) => 1,
                _ => continue,
            };
            if best.is_none_or(|(r, _)| rank > r) {
                best = Some((rank, offset));
            }
        }
        let (_, offset) = best.ok_or(bad("no Unicode cmap"))?;
        let sub = cmap.get(offset..).ok_or(bad("cmap"))?;
        let mut map = BTreeMap::new();
        match u16_at(sub, 0).ok_or(bad("cmap"))? {
            4 => {
                let segs = u16_at(sub, 6).ok_or(bad("cmap"))? as usize / 2;
                let (ends, starts, deltas, ranges) =
                    (14, 16 + segs * 2, 16 + segs * 4, 16 + segs * 6);
                for s in 0..segs {
                    let end = u16_at(sub, ends + s * 2).unwrap_or(0);
                    let start = u16_at(sub, starts + s * 2).unwrap_or(1);
                    let delta = u16_at(sub, deltas + s * 2).unwrap_or(0);
                    let range_offset = u16_at(sub, ranges + s * 2).unwrap_or(0) as usize;
                    if start > end {
                        continue;
                    }
                    for c in start..=end {
                        if c == 0xFFFF {
                            break;
                        }
                        let gid = if range_offset == 0 {
                            c.wrapping_add(delta)
                        } else {
                            let at = ranges + s * 2 + range_offset + (c - start) as usize * 2;
                            match u16_at(sub, at) {
                                Some(0) | None => 0,
                                Some(g) => g.wrapping_add(delta),
                            }
                        };
                        if gid != 0 {
                            map.insert(u32::from(c), gid);
                        }
                    }
                }
            }
            12 => {
                let groups = u32_at(sub, 12).ok_or(bad("cmap"))? as usize;
                for i in 0..groups.min(100_000) {
                    let g = 16 + i * 12;
                    let (start, end, first) =
                        (u32_at(sub, g), u32_at(sub, g + 4), u32_at(sub, g + 8));
                    let (Some(start), Some(end), Some(first)) = (start, end, first) else {
                        break;
                    };
                    if end < start || end - start > 0x10_FFFF {
                        continue;
                    }
                    for c in start..=end {
                        map.insert(c, (first + (c - start)) as u16);
                    }
                }
            }
            other => return Err(bad(&format!("cmap format {other}"))),
        }
        Ok(map)
    }

    pub fn glyph(&self, c: char) -> Option<u16> {
        if let (Some(figures), true) = (&self.figures, c.is_ascii_digit()) {
            return Some(figures[c as usize - '0' as usize]);
        }
        self.cmap
            .get(&(c as u32))
            .copied()
            .filter(|&g| g < self.num_glyphs)
    }

    /// Advance width in font units.
    pub fn advance(&self, gid: u16) -> u16 {
        self.advances.get(gid as usize).copied().unwrap_or(0)
    }

    /// Width of `text` at `size` points; characters without a glyph count as zero.
    pub fn text_width(&self, text: &str, size: f32) -> f32 {
        let units: u32 = text
            .chars()
            .filter_map(|c| self.glyph(c))
            .map(|g| u32::from(self.advance(g)))
            .sum();
        units as f32 / f32::from(self.units_per_em) * size
    }

    fn glyph_range(&self, gid: u16) -> (usize, usize) {
        let loca = self.table(b"loca");
        let g = gid as usize;
        if self.long_loca {
            (
                u32_at(loca, g * 4).unwrap_or(0) as usize,
                u32_at(loca, g * 4 + 4).unwrap_or(0) as usize,
            )
        } else {
            (
                u16_at(loca, g * 2).unwrap_or(0) as usize * 2,
                u16_at(loca, g * 2 + 2).unwrap_or(0) as usize * 2,
            )
        }
    }

    /// Glyph ids referenced by a composite glyph.
    fn components(&self, gid: u16) -> Vec<u16> {
        let glyf = self.table(b"glyf");
        let (start, end) = self.glyph_range(gid);
        let Some(g) = glyf.get(start..end) else {
            return vec![];
        };
        if g.len() < 10 || i16_at(g, 0).unwrap_or(0) >= 0 {
            return vec![];
        }
        let mut out = Vec::new();
        let mut at = 10;
        while let (Some(flags), Some(component)) = (u16_at(g, at), u16_at(g, at + 2)) {
            out.push(component);
            at += 4 + if flags & 1 != 0 { 4 } else { 2 };
            at += if flags & 8 != 0 {
                2
            } else if flags & 0x40 != 0 {
                4
            } else if flags & 0x80 != 0 {
                8
            } else {
                0
            };
            if flags & 0x20 == 0 {
                break;
            }
        }
        out
    }

    /// A copy of the font that only has outlines for `used` (plus `.notdef` and composite parts).
    pub fn subset(&self, used: &BTreeSet<u16>) -> Vec<u8> {
        let mut keep: BTreeSet<u16> = used
            .iter()
            .copied()
            .filter(|&g| g < self.num_glyphs)
            .collect();
        keep.insert(0);
        let mut stack: Vec<u16> = keep.iter().copied().collect();
        while let Some(g) = stack.pop() {
            for c in self.components(g) {
                if c < self.num_glyphs && keep.insert(c) {
                    stack.push(c);
                }
            }
        }
        let glyf = self.table(b"glyf");
        let mut new_glyf = Vec::new();
        let mut loca = Vec::with_capacity((self.num_glyphs as usize + 1) * 4);
        for gid in 0..self.num_glyphs {
            loca.extend_from_slice(&(new_glyf.len() as u32).to_be_bytes());
            if keep.contains(&gid) {
                let (s, e) = self.glyph_range(gid);
                if let Some(bytes) = glyf.get(s..e) {
                    new_glyf.extend_from_slice(bytes);
                    while new_glyf.len() % 4 != 0 {
                        new_glyf.push(0);
                    }
                }
            }
        }
        loca.extend_from_slice(&(new_glyf.len() as u32).to_be_bytes());

        let mut head = self.table(b"head").to_vec();
        if head.len() >= 54 {
            head[8..12].copy_from_slice(&[0; 4]); // checksum adjustment, recomputed below
            head[50..52].copy_from_slice(&1i16.to_be_bytes()); // long loca offsets
        }
        // post version 3: no glyph names.
        let mut post = self
            .table(b"post")
            .get(..32)
            .map(<[u8]>::to_vec)
            .unwrap_or_else(|| vec![0; 32]);
        post[0..4].copy_from_slice(&0x0003_0000u32.to_be_bytes());

        let mut tables: Vec<([u8; 4], Vec<u8>)> = vec![
            (*b"glyf", new_glyf),
            (*b"head", head),
            (*b"loca", loca),
            (*b"post", post),
        ];
        for tag in [
            b"OS/2", b"cmap", b"cvt ", b"fpgm", b"hhea", b"hmtx", b"maxp", b"name", b"prep",
        ] {
            if self.tables.contains_key(tag) {
                tables.push((*tag, self.table(tag).to_vec()));
            }
        }
        tables.sort_by_key(|(tag, _)| *tag);
        build_font(tables)
    }
}

fn checksum(data: &[u8]) -> u32 {
    data.chunks(4).fold(0u32, |acc, c| {
        let mut b = [0u8; 4];
        b[..c.len()].copy_from_slice(c);
        acc.wrapping_add(u32::from_be_bytes(b))
    })
}

fn build_font(tables: Vec<([u8; 4], Vec<u8>)>) -> Vec<u8> {
    let n = tables.len() as u16;
    let mut pow = 1u16;
    let mut log = 0u16;
    while pow * 2 <= n {
        pow *= 2;
        log += 1;
    }
    let mut out = Vec::new();
    out.extend_from_slice(&0x0001_0000u32.to_be_bytes());
    out.extend_from_slice(&n.to_be_bytes());
    out.extend_from_slice(&(pow * 16).to_be_bytes());
    out.extend_from_slice(&log.to_be_bytes());
    out.extend_from_slice(&(n * 16 - pow * 16).to_be_bytes());
    let mut offset = 12 + 16 * tables.len();
    let mut head_at = None;
    for (tag, data) in &tables {
        out.extend_from_slice(tag);
        out.extend_from_slice(&checksum(data).to_be_bytes());
        out.extend_from_slice(&(offset as u32).to_be_bytes());
        out.extend_from_slice(&(data.len() as u32).to_be_bytes());
        if tag == b"head" {
            head_at = Some(offset);
        }
        offset += data.len().div_ceil(4) * 4;
    }
    for (_, data) in &tables {
        out.extend_from_slice(data);
        while out.len() % 4 != 0 {
            out.push(0);
        }
    }
    if let Some(h) = head_at {
        let adjust = 0xB1B0_AFBAu32.wrapping_sub(checksum(&out));
        out[h + 8..h + 12].copy_from_slice(&adjust.to_be_bytes());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn system_font() -> Option<Vec<u8>> {
        [
            "C:\\Windows\\Fonts\\arial.ttf",
            "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf",
            "/Library/Fonts/Arial.ttf",
        ]
        .iter()
        .find_map(|p| std::fs::read(p).ok())
    }

    /// A font file with nothing in it but the tables that describe it.
    fn described(os2: &[u8], names: &[(u16, u16, u16, &str)], head: &[u8]) -> Vec<u8> {
        let mut name = Vec::new();
        let mut strings = Vec::new();
        name.extend(0u16.to_be_bytes());
        name.extend((names.len() as u16).to_be_bytes());
        name.extend((6 + 12 * names.len() as u16).to_be_bytes());
        for &(platform, language, id, text) in names {
            let bytes: Vec<u8> = if platform == 1 {
                text.bytes().collect()
            } else {
                text.encode_utf16().flat_map(u16::to_be_bytes).collect()
            };
            for v in [
                platform,
                1,
                language,
                id,
                bytes.len() as u16,
                strings.len() as u16,
            ] {
                name.extend(v.to_be_bytes());
            }
            strings.extend(bytes);
        }
        name.extend(strings);
        let tables: [(&[u8; 4], &[u8]); 4] = [
            (b"OS/2", os2),
            (b"glyf", &[]),
            (b"head", head),
            (b"name", &name),
        ];
        let mut out = vec![0, 1, 0, 0, 0, tables.len() as u8, 0, 0, 0, 0, 0, 0];
        let mut at = 12 + 16 * tables.len();
        for (tag, data) in tables {
            out.extend(*tag);
            out.extend(0u32.to_be_bytes());
            out.extend((at as u32).to_be_bytes());
            out.extend((data.len() as u32).to_be_bytes());
            at += data.len();
        }
        for (_, data) in tables {
            out.extend(data);
        }
        out
    }

    #[test]
    fn a_font_file_describes_its_faces() {
        let mut os2 = vec![0u8; 78];
        os2[4..6].copy_from_slice(&700u16.to_be_bytes());
        os2[8..10].copy_from_slice(&8u16.to_be_bytes());
        os2[32..36].copy_from_slice(&[2, 4, 5, 2]);
        os2[42..46].copy_from_slice(&0x0000_0207u32.to_be_bytes());
        os2[62..64].copy_from_slice(&0x21u16.to_be_bytes());
        let mut head = vec![0u8; 54];
        head[44..46].copy_from_slice(&3u16.to_be_bytes());
        let font = described(
            &os2,
            &[
                (1, 0, 1, "Old Name"),
                (3, 0x0407, 1, "Beispiel Fett"),
                (3, 0x0409, 1, "Sample Bold"),
                (3, 0x0409, 16, "Sample"),
                (3, 0x0409, 6, "Sample-BoldItalic"),
            ],
            &head,
        );
        let faces = faces_in(&mut std::io::Cursor::new(&font));
        assert_eq!(faces.len(), 1);
        let face = &faces[0];
        assert_eq!(
            (face.family.as_str(), face.postscript.as_str()),
            ("Sample", "Sample-BoldItalic")
        );
        assert_eq!(
            (face.weight, face.italic, face.embeddable, face.truetype),
            (700, true, true, true)
        );
        assert_eq!(
            (&face.panose[..4], face.ranges[0]),
            (&[2, 4, 5, 2][..], 0x207)
        );
        assert_eq!(style_bits(&font), Some((true, true)));

        // A licence that keeps the font out of documents.
        os2[8..10].copy_from_slice(&2u16.to_be_bytes());
        let kept = described(&os2, &[(1, 0, 1, "Kept")], &head);
        let faces = faces_in(&mut std::io::Cursor::new(&kept));
        assert_eq!(
            (faces[0].family.as_str(), faces[0].embeddable),
            ("Kept", false)
        );

        assert!(faces_in(&mut std::io::Cursor::new(b"%PDF-1.7 not a font at all")).is_empty());
        assert!(faces_in(&mut std::io::Cursor::new(&font[..40])).is_empty());
    }

    #[test]
    fn a_system_font_describes_itself() {
        let Some(data) = system_font() else { return };
        let faces = faces_in(&mut std::io::Cursor::new(&data));
        assert_eq!(faces.len(), 1);
        assert!(!faces[0].family.is_empty() && faces[0].truetype && faces[0].embeddable);
        assert_eq!(
            (faces[0].weight, faces[0].italic, faces[0].panose[0]),
            (400, false, 2)
        );
    }

    #[test]
    fn parse_measure_and_subset() {
        let Some(data) = system_font() else { return };
        let font = TrueType::parse(data.clone()).unwrap();
        let g = font.glyph('ş').expect("Turkish glyph");
        assert!(font.advance(g) > 0);
        assert!(font.text_width("Merhaba", 10.0) > 20.0);
        let used: BTreeSet<u16> = "Çeviri ğüşıöç"
            .chars()
            .filter_map(|c| font.glyph(c))
            .collect();
        let subset = font.subset(&used);
        assert!(
            subset.len() < data.len() / 4,
            "{} vs {}",
            subset.len(),
            data.len()
        );
        let again = TrueType::parse(subset).unwrap();
        assert_eq!(again.glyph('ş'), Some(g));
        assert_eq!(again.advance(g), font.advance(g));
    }
}
