//! Stream filters. Only what text extraction and writing need: image codecs (DCT, JPX, CCITT, JBIG2) are left encoded.

use crate::object::{Dict, Object, get};
use crate::{Error, Result};

/// Decode `data` with the filters named in a stream dictionary.
pub fn decode(dict: &Dict, data: &[u8]) -> Result<Vec<u8>> {
    let filters: Vec<&[u8]> = match get(dict, "Filter").or_else(|| get(dict, "F")) {
        None => return Ok(data.to_vec()),
        Some(Object::Name(n)) => vec![n.as_slice()],
        Some(Object::Array(a)) => a.iter().filter_map(Object::as_name).collect(),
        Some(_) => return Err(Error::Unsupported("filter".into())),
    };
    let params: Vec<Option<&Dict>> = match get(dict, "DecodeParms").or_else(|| get(dict, "DP")) {
        Some(Object::Array(a)) => a.iter().map(Object::as_dict).collect(),
        Some(o) => vec![o.as_dict()],
        None => vec![],
    };
    let mut out = data.to_vec();
    for (i, f) in filters.iter().enumerate() {
        let p = params.get(i).copied().flatten();
        out = match *f {
            b"FlateDecode" | b"Fl" => predict(inflate(&out)?, p)?,
            b"LZWDecode" | b"LZW" => {
                let early = p
                    .and_then(|p| get(p, "EarlyChange"))
                    .and_then(Object::as_i64)
                    .unwrap_or(1);
                predict(lzw(&out, early != 0), p)?
            }
            b"ASCIIHexDecode" | b"AHx" => ascii_hex(&out),
            b"ASCII85Decode" | b"A85" => ascii85(&out)?,
            b"RunLengthDecode" | b"RL" => run_length(&out),
            other => {
                return Err(Error::Unsupported(
                    String::from_utf8_lossy(other).into_owned(),
                ));
            }
        };
    }
    Ok(out)
}

/// The most one stream may decode to. A few kilobytes of Flate can expand to gigabytes; a real book's streams
/// stay far below this.
pub const MAX_DECODED: usize = 256 << 20;

fn too_large() -> Error {
    Error::Corrupt(format!(
        "a stream decodes to more than {} MiB",
        MAX_DECODED >> 20
    ))
}

/// zlib inflate. Damaged streams are common in old books: keep whatever decodes.
pub fn inflate(data: &[u8]) -> Result<Vec<u8>> {
    use miniz_oxide::inflate::{
        TINFLStatus, decompress_to_vec_with_limit, decompress_to_vec_zlib_with_limit,
    };
    match decompress_to_vec_zlib_with_limit(data, MAX_DECODED) {
        Ok(v) => Ok(v),
        Err(e) if e.status == TINFLStatus::HasMoreOutput => Err(too_large()),
        Err(e) if !e.output.is_empty() => Ok(e.output),
        Err(_) => match decompress_to_vec_with_limit(data, MAX_DECODED) {
            Ok(v) => Ok(v),
            Err(e) if e.status == TINFLStatus::HasMoreOutput => Err(too_large()),
            Err(e) if !e.output.is_empty() => Ok(e.output),
            Err(_) => Err(Error::Corrupt("flate stream".into())),
        },
    }
}

pub fn deflate(data: &[u8]) -> Vec<u8> {
    miniz_oxide::deflate::compress_to_vec_zlib(data, 6)
}

fn predict(data: Vec<u8>, params: Option<&Dict>) -> Result<Vec<u8>> {
    let Some(p) = params else { return Ok(data) };
    let int = |k: &str, d: i64| get(p, k).and_then(Object::as_i64).unwrap_or(d);
    let predictor = int("Predictor", 1);
    if predictor < 2 {
        return Ok(data);
    }
    let colors = int("Colors", 1).clamp(1, 32) as usize;
    let bpc = int("BitsPerComponent", 8).clamp(1, 16) as usize;
    let columns = int("Columns", 1).max(1) as usize;
    let bpp = (colors * bpc).div_ceil(8);
    let row = (colors * bpc * columns).div_ceil(8);

    if predictor == 2 {
        // TIFF predictor, 8-bit components only (the only kind seen in practice).
        let mut out = data;
        if bpc == 8 {
            for line in out.chunks_mut(row) {
                for i in bpp..line.len() {
                    line[i] = line[i].wrapping_add(line[i - bpp]);
                }
            }
        }
        return Ok(out);
    }

    // PNG predictors: every row starts with its own filter type byte.
    let mut out = Vec::with_capacity(data.len());
    let mut prev = vec![0u8; row];
    for chunk in data.chunks(row + 1) {
        if chunk.len() < 2 {
            break;
        }
        let (kind, src) = (chunk[0], &chunk[1..]);
        let mut cur = src.to_vec();
        cur.resize(row, 0);
        for i in 0..row {
            let left = if i >= bpp { cur[i - bpp] } else { 0 };
            let up = prev[i];
            let up_left = if i >= bpp { prev[i - bpp] } else { 0 };
            cur[i] = match kind {
                0 => cur[i],
                1 => cur[i].wrapping_add(left),
                2 => cur[i].wrapping_add(up),
                3 => cur[i].wrapping_add(((u16::from(left) + u16::from(up)) / 2) as u8),
                4 => cur[i].wrapping_add(paeth(left, up, up_left)),
                _ => return Err(Error::Corrupt("png predictor".into())),
            };
        }
        out.extend_from_slice(&cur[..src.len().min(row)]);
        prev = cur;
    }
    Ok(out)
}

fn paeth(a: u8, b: u8, c: u8) -> u8 {
    let p = i16::from(a) + i16::from(b) - i16::from(c);
    let (pa, pb, pc) = (
        (p - i16::from(a)).abs(),
        (p - i16::from(b)).abs(),
        (p - i16::from(c)).abs(),
    );
    if pa <= pb && pa <= pc {
        a
    } else if pb <= pc {
        b
    } else {
        c
    }
}

fn ascii_hex(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut high = None;
    for &b in data {
        let v = match b {
            b'0'..=b'9' => b - b'0',
            b'a'..=b'f' => b - b'a' + 10,
            b'A'..=b'F' => b - b'A' + 10,
            b'>' => break,
            _ => continue,
        };
        match high.take() {
            Some(h) => out.push(h << 4 | v),
            None => high = Some(v),
        }
    }
    if let Some(h) = high {
        out.push(h << 4);
    }
    out
}

fn ascii85(data: &[u8]) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    let mut group = [0u8; 5];
    let mut n = 0;
    let body = data.strip_prefix(b"<~").unwrap_or(data);
    for &b in body {
        match b {
            b'~' => break,
            b'z' if n == 0 => out.extend_from_slice(&[0; 4]),
            b'!'..=b'u' => {
                group[n] = b - b'!';
                n += 1;
                if n == 5 {
                    let v = group.iter().fold(0u64, |acc, &d| acc * 85 + u64::from(d));
                    out.extend_from_slice(&(v as u32).to_be_bytes());
                    n = 0;
                }
            }
            _ if crate::lexer::is_whitespace(b) => {}
            _ => return Err(Error::Corrupt("ascii85".into())),
        }
    }
    if n > 1 {
        for slot in group.iter_mut().skip(n) {
            *slot = 84;
        }
        let v = group.iter().fold(0u64, |acc, &d| acc * 85 + u64::from(d));
        out.extend_from_slice(&(v as u32).to_be_bytes()[..n - 1]);
    }
    Ok(out)
}

fn run_length(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < data.len() {
        let len = data[i];
        i += 1;
        match len {
            128 => break,
            0..=127 => {
                let n = usize::from(len) + 1;
                out.extend_from_slice(&data[i..(i + n).min(data.len())]);
                i += n;
            }
            _ => {
                if let Some(&b) = data.get(i) {
                    out.extend(std::iter::repeat_n(b, 257 - usize::from(len)));
                }
                i += 1;
            }
        }
    }
    out
}

fn lzw(data: &[u8], early_change: bool) -> Vec<u8> {
    let mut out = Vec::new();
    let mut table: Vec<Vec<u8>> = Vec::new();
    let reset = |t: &mut Vec<Vec<u8>>| {
        t.clear();
        t.extend((0..=255u8).map(|b| vec![b]));
        t.push(Vec::new()); // 256 clear
        t.push(Vec::new()); // 257 end
    };
    reset(&mut table);
    let (mut bits, mut acc, mut nbits) = (9u32, 0u32, 0u32);
    let mut prev: Option<Vec<u8>> = None;
    for &byte in data {
        acc = acc << 8 | u32::from(byte);
        nbits += 8;
        while nbits >= bits {
            let code = ((acc >> (nbits - bits)) & ((1 << bits) - 1)) as usize;
            nbits -= bits;
            match code {
                256 => {
                    reset(&mut table);
                    bits = 9;
                    prev = None;
                    continue;
                }
                257 => return out,
                _ => {}
            }
            if out.len() > MAX_DECODED {
                return out;
            }
            let entry = if code < table.len() {
                table[code].clone()
            } else if let Some(p) = &prev {
                let mut e = p.clone();
                e.push(p[0]);
                e
            } else {
                return out;
            };
            out.extend_from_slice(&entry);
            if let Some(mut p) = prev.take() {
                p.push(entry[0]);
                table.push(p);
            }
            prev = Some(entry);
            let limit = table.len() + usize::from(early_change);
            bits = match limit {
                ..512 => 9,
                512..1024 => 10,
                1024..2048 => 11,
                _ => 12,
            };
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flate_round_trip() {
        let text = b"BT /F1 12 Tf (Merhaba) Tj ET".repeat(20);
        assert_eq!(inflate(&deflate(&text)).unwrap(), text);
    }

    #[test]
    fn ascii_filters() {
        assert_eq!(ascii_hex(b"48 65 6C 6C 6F>"), b"Hello");
        assert_eq!(ascii85(b"<~87cURD]i,\"Ebo80~>").unwrap(), b"Hello World!");
        assert_eq!(
            run_length(&[2, b'a', b'b', b'c', 254, b'x', 128]),
            b"abcxxx"
        );
    }

    #[test]
    fn decompression_bombs_are_refused() {
        // 300 MiB of zeros compresses to a few hundred kilobytes.
        let bomb = miniz_oxide::deflate::compress_to_vec_zlib(&vec![0u8; 300 << 20], 9);
        assert!(bomb.len() < 1 << 20);
        assert!(inflate(&bomb).is_err());
        assert_eq!(inflate(&deflate(b"fine")).unwrap(), b"fine");
    }

    #[test]
    fn lzw_spec_example() {
        // The example from the PDF specification, section 7.4.4.2.
        let encoded = [0x80, 0x0B, 0x60, 0x50, 0x22, 0x0C, 0x0C, 0x85, 0x01];
        assert_eq!(lzw(&encoded, true), b"-----A---B");
    }

    #[test]
    fn png_up_predictor() {
        let mut dict = Dict::new();
        dict.insert(b"Predictor".to_vec(), Object::Int(12));
        dict.insert(b"Columns".to_vec(), Object::Int(3));
        // Row 1: none [1,2,3]; row 2: up [1,1,1] -> [2,3,4]
        let data = vec![0, 1, 2, 3, 2, 1, 1, 1];
        assert_eq!(predict(data, Some(&dict)).unwrap(), vec![1, 2, 3, 2, 3, 4]);
    }
}
