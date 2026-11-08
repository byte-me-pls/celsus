//! Reading TrueType fonts (glyph outlines in `glyf`) and cutting them down to the glyphs a document uses.
//!
//! The subset keeps every glyph id where it was and only empties the outlines that are not needed. Glyph ids can
//! then be used directly as CIDs (`/CIDToGIDMap /Identity`), and the file shrinks to the glyphs actually drawn.

use std::collections::{BTreeMap, BTreeSet};

use crate::{Error, Result};

#[derive(Debug, Clone)]
pub struct TrueType {
    data: Vec<u8>,
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
        let version = u32_at(&data, 0).ok_or(bad("too short"))?;
        if version == 0x4F54544F {
            return Err(bad(
                "CFF-based OpenType fonts are not supported; use a TrueType (.ttf) font",
            ));
        }
        if version != 0x0001_0000 && version != 0x7472_7565 {
            return Err(bad("not a TrueType font"));
        }
        let count = u16_at(&data, 4).ok_or(bad("header"))? as usize;
        let mut tables = BTreeMap::new();
        for i in 0..count {
            let rec = 12 + i * 16;
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
        };
        font.cmap = font.read_cmap()?;
        Ok(font)
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
        loop {
            let (Some(flags), Some(component)) = (u16_at(g, at), u16_at(g, at + 2)) else {
                break;
            };
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
