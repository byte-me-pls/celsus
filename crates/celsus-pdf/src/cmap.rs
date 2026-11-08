//! CMaps: how a composite font splits a string into character codes, and ToUnicode maps.

use std::collections::HashMap;

use crate::lexer::{Lexer, Token};

/// A codespace range: codes of `len` bytes between `lo` and `hi`.
#[derive(Debug, Clone, Copy)]
struct Codespace {
    len: usize,
    lo: u32,
    hi: u32,
}

#[derive(Debug, Clone, Default)]
pub struct CMap {
    codespaces: Vec<Codespace>,
    /// Code -> CID ranges `(lo, hi, first_cid)`.
    cid_ranges: Vec<(u32, u32, u32)>,
    /// Code -> Unicode text.
    pub unicode: HashMap<u32, String>,
}

impl CMap {
    /// The two-byte identity mapping used by `Identity-H` and `Identity-V`.
    pub fn identity() -> Self {
        CMap {
            codespaces: vec![Codespace {
                len: 2,
                lo: 0,
                hi: 0xFFFF,
            }],
            cid_ranges: vec![(0, 0xFFFF, 0)],
            unicode: HashMap::new(),
        }
    }

    pub fn has_codespaces(&self) -> bool {
        !self.codespaces.is_empty()
    }

    /// Split `bytes` into codes. Without codespace ranges every code is `default_len` bytes long.
    pub fn codes(&self, bytes: &[u8], default_len: usize) -> Vec<(u32, usize)> {
        let mut out = Vec::with_capacity(bytes.len());
        let mut i = 0;
        while i < bytes.len() {
            let mut len = 0;
            if self.codespaces.is_empty() {
                len = default_len;
            } else {
                for n in 1..=4.min(bytes.len() - i) {
                    let code = be(&bytes[i..i + n]);
                    if self
                        .codespaces
                        .iter()
                        .any(|c| c.len == n && (c.lo..=c.hi).contains(&code))
                    {
                        len = n;
                        break;
                    }
                }
                if len == 0 {
                    // Not in any range: take the shortest declared length, as viewers do.
                    len = self.codespaces.iter().map(|c| c.len).min().unwrap_or(1);
                }
            }
            let len = len.min(bytes.len() - i).max(1);
            out.push((be(&bytes[i..i + len]), len));
            i += len;
        }
        out
    }

    pub fn cid(&self, code: u32) -> u32 {
        self.cid_ranges
            .iter()
            .find(|(lo, hi, _)| (*lo..=*hi).contains(&code))
            .map_or(code, |(lo, _, first)| first + (code - lo))
    }

    /// Parse a CMap or ToUnicode stream.
    pub fn parse(data: &[u8]) -> CMap {
        let mut cmap = CMap::default();
        let mut lexer = Lexer::new(data);
        let mut operands: Vec<Token> = Vec::new();
        while let Ok(Some(token)) = lexer.next_token() {
            let Token::Keyword(k) = &token else {
                operands.push(token);
                continue;
            };
            match k.as_slice() {
                b"begincodespacerange"
                | b"beginbfchar"
                | b"beginbfrange"
                | b"begincidrange"
                | b"begincidchar" => operands.clear(),
                b"endcodespacerange" => {
                    for pair in operands.chunks(2) {
                        if let [Token::String(lo), Token::String(hi)] = pair {
                            cmap.codespaces.push(Codespace {
                                len: lo.len().clamp(1, 4),
                                lo: be(lo),
                                hi: be(hi),
                            });
                        }
                    }
                    operands.clear();
                }
                b"endbfchar" => {
                    for pair in operands.chunks(2) {
                        if let [Token::String(src), dst] = pair
                            && let Some(text) = dst_text(dst)
                        {
                            cmap.unicode.insert(be(src), text);
                        }
                    }
                    operands.clear();
                }
                b"endbfrange" => {
                    let mut i = 0;
                    while i + 2 < operands.len() {
                        let (Token::String(lo), Token::String(hi)) =
                            (&operands[i], &operands[i + 1])
                        else {
                            i += 1;
                            continue;
                        };
                        let (lo, hi) = (be(lo), be(hi));
                        if hi < lo || hi - lo > 0xFFFF {
                            i += 3;
                            continue;
                        }
                        match &operands[i + 2] {
                            Token::String(dst) => {
                                // Increment the last UTF-16 unit for each code in the range.
                                let units = utf16_units(dst);
                                for (n, code) in (lo..=hi).enumerate() {
                                    let mut u = units.clone();
                                    if let Some(last) = u.last_mut() {
                                        *last = last.wrapping_add(n as u16);
                                    }
                                    if let Ok(s) = String::from_utf16(&u) {
                                        cmap.unicode.insert(code, s);
                                    }
                                }
                                i += 3;
                            }
                            Token::ArrayStart => {
                                let mut j = i + 3;
                                let mut code = lo;
                                while j < operands.len() && operands[j] != Token::ArrayEnd {
                                    if let Some(text) = dst_text(&operands[j])
                                        && code <= hi
                                    {
                                        cmap.unicode.insert(code, text);
                                    }
                                    code += 1;
                                    j += 1;
                                }
                                i = j + 1;
                            }
                            _ => i += 3,
                        }
                    }
                    operands.clear();
                }
                b"endcidrange" => {
                    for triple in operands.chunks(3) {
                        if let [Token::String(lo), Token::String(hi), Token::Int(cid)] = triple {
                            cmap.cid_ranges.push((be(lo), be(hi), *cid as u32));
                        }
                    }
                    operands.clear();
                }
                b"endcidchar" => {
                    for pair in operands.chunks(2) {
                        if let [Token::String(code), Token::Int(cid)] = pair {
                            let c = be(code);
                            cmap.cid_ranges.push((c, c, *cid as u32));
                        }
                    }
                    operands.clear();
                }
                _ => operands.clear(),
            }
        }
        cmap
    }
}

fn be(bytes: &[u8]) -> u32 {
    bytes
        .iter()
        .take(4)
        .fold(0u32, |acc, &b| acc << 8 | u32::from(b))
}

fn utf16_units(bytes: &[u8]) -> Vec<u16> {
    bytes
        .chunks(2)
        .map(|c| u16::from_be_bytes([c[0], *c.get(1).unwrap_or(&0)]))
        .collect()
}

fn dst_text(token: &Token) -> Option<String> {
    match token {
        Token::String(s) => {
            let text = String::from_utf16_lossy(&utf16_units(s));
            (!text.is_empty()).then_some(text)
        }
        // A glyph name destination (rare, allowed by the spec for bfchar).
        Token::Name(n) => crate::encoding::glyph_name_to_unicode(n),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TO_UNICODE: &str = "/CIDInit /ProcSet findresource begin 12 dict begin begincmap
        1 begincodespacerange <0000> <FFFF> endcodespacerange
        2 beginbfchar <0003> <0020> <0011> <011F> endbfchar
        2 beginbfrange <0024> <0026> <0041> <0050> <0052> [<0130> <015F> <00660069>] endbfrange
        endcmap CMapName currentdict /CMap defineresource pop end end";

    #[test]
    fn to_unicode() {
        let c = CMap::parse(TO_UNICODE.as_bytes());
        assert_eq!(c.unicode[&3], " ");
        assert_eq!(c.unicode[&0x11], "ğ");
        assert_eq!(c.unicode[&0x25], "B");
        assert_eq!(c.unicode[&0x26], "C");
        assert_eq!(c.unicode[&0x50], "İ");
        assert_eq!(c.unicode[&0x51], "ş");
        assert_eq!(c.unicode[&0x52], "fi");
        assert_eq!(c.codes(&[0, 3, 0, 0x11], 1), vec![(3, 2), (0x11, 2)]);
    }

    #[test]
    fn mixed_length_codespaces() {
        let c = CMap::parse(b"begincodespacerange <00> <80> <8140> <FFFF> endcodespacerange begincidrange <8140> <8142> 100 endcidrange");
        assert_eq!(
            c.codes(&[0x41, 0x81, 0x41, 0x42], 1),
            vec![(0x41, 1), (0x8141, 2), (0x42, 1)]
        );
        assert_eq!(c.cid(0x8141), 101);
    }
}
