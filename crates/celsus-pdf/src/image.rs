//! Pictures on a page: finding them, taking them out as ordinary image files and putting a replacement in.
//!
//! JPEG pictures are taken out as they are stored. Pictures stored as plain samples (Flate and the like) are
//! written out as PNG. Other codecs (JPEG 2000, CCITT, JBIG2) are left alone.

use crate::object::{Dict, ObjRef, Object, Stream, get};
use crate::{Document, Page, filters};

/// A picture drawn on a page.
#[derive(Debug, Clone)]
pub struct PageImage {
    pub id: ObjRef,
    /// Size in pixels.
    pub width: u32,
    pub height: u32,
}

fn filter_names(dict: &Dict) -> Vec<Vec<u8>> {
    match get(dict, "Filter") {
        Some(Object::Name(n)) => vec![n.clone()],
        Some(Object::Array(a)) => a
            .iter()
            .filter_map(|o| o.as_name().map(<[u8]>::to_vec))
            .collect(),
        _ => Vec::new(),
    }
}

/// The pictures in a page's resources, largest first. Tiny ones (rules, bullets) are left out.
pub fn page_images(doc: &Document, page: &Page) -> Vec<PageImage> {
    let mut out = Vec::new();
    let Some(xobjects) = doc.get_in(&page.resources, "XObject") else {
        return out;
    };
    let Some(dict) = xobjects.as_dict() else {
        return out;
    };
    for entry in dict.values() {
        let Some(id) = entry.as_ref() else { continue };
        let Ok(obj) = doc.get(id) else { continue };
        let Some(stream) = obj.as_stream() else {
            continue;
        };
        let d = &stream.dict;
        if get(d, "Subtype").and_then(Object::as_name) != Some(b"Image") {
            continue;
        }
        let width = doc.get_in(d, "Width").and_then(|v| v.as_i64()).unwrap_or(0);
        let height = doc
            .get_in(d, "Height")
            .and_then(|v| v.as_i64())
            .unwrap_or(0);
        if width >= 64 && height >= 64 && !out.iter().any(|i: &PageImage| i.id == id) {
            out.push(PageImage {
                id,
                width: width as u32,
                height: height as u32,
            });
        }
    }
    out.sort_by_key(|i| std::cmp::Reverse(u64::from(i.width) * u64::from(i.height)));
    out
}

/// A picture as an image file: its bytes and media type (`image/jpeg` or `image/png`).
pub fn export(doc: &Document, id: ObjRef) -> Option<(Vec<u8>, &'static str)> {
    let obj = doc.get(id).ok()?;
    let stream = obj.as_stream()?;
    let mut dict = stream.dict.clone();
    for key in [
        "Filter",
        "DecodeParms",
        "ColorSpace",
        "Width",
        "Height",
        "BitsPerComponent",
    ] {
        if let Some(v) = doc.get_in(&stream.dict, key) {
            dict.insert(key.as_bytes().to_vec(), (*v).clone());
        }
    }
    let names = filter_names(&dict);
    if names.last().map(Vec::as_slice) == Some(b"DCTDecode") {
        // Undo any filters applied on top of the JPEG data.
        let mut outer = dict.clone();
        let rest: Vec<Object> = names[..names.len() - 1]
            .iter()
            .map(|n| Object::Name(n.clone()))
            .collect();
        outer.insert(b"Filter".to_vec(), Object::Array(rest));
        if let Some(Object::Array(p)) = get(&dict, "DecodeParms") {
            outer.insert(
                b"DecodeParms".to_vec(),
                Object::Array(p[..p.len().saturating_sub(1)].to_vec()),
            );
        } else {
            outer.remove(&b"DecodeParms"[..]);
        }
        let data = filters::decode(&outer, &stream.data).ok()?;
        return data
            .starts_with(&[0xFF, 0xD8])
            .then_some((data, "image/jpeg"));
    }
    let samples = filters::decode(&dict, &stream.data).ok()?;
    let width = get(&dict, "Width")?.as_i64()? as u32;
    let height = get(&dict, "Height")?.as_i64()? as u32;
    if get(&dict, "BitsPerComponent").and_then(Object::as_i64) != Some(8) {
        return None;
    }
    let space = get(&dict, "ColorSpace")?;
    let (pixels, channels) = to_pixels(doc, space, &samples, (width * height) as usize)?;
    Some((png(width, height, channels, &pixels), "image/png"))
}

/// Samples in a colour space as grey or RGB pixels.
fn to_pixels(
    doc: &Document,
    space: &Object,
    samples: &[u8],
    count: usize,
) -> Option<(Vec<u8>, u8)> {
    let resolved = doc.resolve(space).ok()?;
    match &*resolved {
        Object::Name(n) if n == b"DeviceGray" || n == b"CalGray" => {
            Some((samples.get(..count)?.to_vec(), 1))
        }
        Object::Name(n) if n == b"DeviceRGB" || n == b"CalRGB" => {
            Some((samples.get(..count * 3)?.to_vec(), 3))
        }
        Object::Name(n) if n == b"DeviceCMYK" => {
            let px = samples.get(..count * 4)?;
            Some((px.chunks(4).flat_map(cmyk_to_rgb).collect(), 3))
        }
        Object::Array(a) => match a.first().and_then(Object::as_name)? {
            b"ICCBased" => {
                let profile = doc.resolve(a.get(1)?).ok()?;
                let n = doc.get_in(&profile.as_stream()?.dict, "N")?.as_i64()?;
                let alt = match n {
                    1 => b"DeviceGray".as_slice(),
                    3 => b"DeviceRGB",
                    4 => b"DeviceCMYK",
                    _ => return None,
                };
                to_pixels(doc, &Object::Name(alt.to_vec()), samples, count)
            }
            b"Indexed" => {
                let base = doc.resolve(a.get(1)?).ok()?;
                let lookup = doc.resolve(a.get(3)?).ok()?;
                let table = match &*lookup {
                    Object::Stream(s) => doc.stream_data(s).ok()?,
                    other => other.as_str_bytes()?.to_vec(),
                };
                let n = match &*base {
                    Object::Name(n) if n == b"DeviceGray" => 1,
                    Object::Name(n) if n == b"DeviceCMYK" => 4,
                    _ => 3,
                };
                let expanded: Vec<u8> = samples
                    .get(..count)?
                    .iter()
                    .flat_map(|&i| {
                        let at = i as usize * n;
                        table.get(at..at + n).unwrap_or(&[0, 0, 0, 0][..n]).to_vec()
                    })
                    .collect();
                to_pixels(doc, &base, &expanded, count)
            }
            _ => None,
        },
        _ => None,
    }
}

fn cmyk_to_rgb(p: &[u8]) -> [u8; 3] {
    let k = 255 - u16::from(p[3]);
    let f = |c: u8| ((255 - u16::from(c)) * k / 255) as u8;
    [f(p[0]), f(p[1]), f(p[2])]
}

fn crc32(chunks: &[&[u8]]) -> u32 {
    let mut table = [0u32; 256];
    for (i, slot) in table.iter_mut().enumerate() {
        let mut c = i as u32;
        for _ in 0..8 {
            c = if c & 1 != 0 {
                0xEDB8_8320 ^ (c >> 1)
            } else {
                c >> 1
            };
        }
        *slot = c;
    }
    let mut crc = 0xFFFF_FFFFu32;
    for chunk in chunks {
        for &b in *chunk {
            crc = table[((crc ^ u32::from(b)) & 0xFF) as usize] ^ (crc >> 8);
        }
    }
    crc ^ 0xFFFF_FFFF
}

/// A PNG file of 8-bit grey (1 channel) or RGB (3 channels) pixels.
pub fn png(width: u32, height: u32, channels: u8, pixels: &[u8]) -> Vec<u8> {
    let row = width as usize * channels as usize;
    let mut raw = Vec::with_capacity((row + 1) * height as usize);
    for line in pixels.chunks(row).take(height as usize) {
        raw.push(0);
        raw.extend_from_slice(line);
    }
    let mut out = b"\x89PNG\r\n\x1a\n".to_vec();
    let mut chunk = |kind: &[u8], data: &[u8]| {
        out.extend_from_slice(&(data.len() as u32).to_be_bytes());
        out.extend_from_slice(kind);
        out.extend_from_slice(data);
        out.extend_from_slice(&crc32(&[kind, data]).to_be_bytes());
    };
    let mut ihdr = Vec::new();
    ihdr.extend_from_slice(&width.to_be_bytes());
    ihdr.extend_from_slice(&height.to_be_bytes());
    ihdr.extend_from_slice(&[8, if channels == 1 { 0 } else { 2 }, 0, 0, 0]);
    chunk(b"IHDR", &ihdr);
    chunk(b"IDAT", &filters::deflate(&raw));
    chunk(b"IEND", &[]);
    out
}

/// Width, height and number of colour components of a JPEG file.
pub fn jpeg_info(data: &[u8]) -> Option<(u32, u32, u8)> {
    if !data.starts_with(&[0xFF, 0xD8]) {
        return None;
    }
    let mut i = 2;
    while i + 4 <= data.len() {
        if data[i] != 0xFF {
            i += 1;
            continue;
        }
        let marker = data[i + 1];
        if marker == 0xFF {
            i += 1;
            continue;
        }
        if marker == 0xD8 || (0xD0..=0xD7).contains(&marker) || marker == 0x01 {
            i += 2;
            continue;
        }
        let len = u16::from_be_bytes([data[i + 2], data[i + 3]]) as usize;
        let is_sof = (0xC0..=0xCF).contains(&marker) && ![0xC4, 0xC8, 0xCC].contains(&marker);
        if is_sof && i + 9 < data.len() {
            let h = u16::from_be_bytes([data[i + 5], data[i + 6]]) as u32;
            let w = u16::from_be_bytes([data[i + 7], data[i + 8]]) as u32;
            return Some((w, h, data[i + 9]));
        }
        i += 2 + len;
    }
    None
}

/// An image XObject holding a JPEG file as it is.
pub fn jpeg_xobject(jpeg: &[u8]) -> Option<Stream> {
    let (w, h, components) = jpeg_info(jpeg)?;
    let space: &[u8] = match components {
        1 => b"DeviceGray",
        4 => b"DeviceCMYK",
        _ => b"DeviceRGB",
    };
    let mut dict = Dict::new();
    dict.insert(b"Type".to_vec(), Object::Name(b"XObject".to_vec()));
    dict.insert(b"Subtype".to_vec(), Object::Name(b"Image".to_vec()));
    dict.insert(b"Width".to_vec(), Object::Int(i64::from(w)));
    dict.insert(b"Height".to_vec(), Object::Int(i64::from(h)));
    dict.insert(b"ColorSpace".to_vec(), Object::Name(space.to_vec()));
    dict.insert(b"BitsPerComponent".to_vec(), Object::Int(8));
    dict.insert(b"Filter".to_vec(), Object::Name(b"DCTDecode".to_vec()));
    Some(Stream {
        dict,
        data: jpeg.to_vec(),
    })
}

/// An image XObject from a JPEG or PNG file.
pub fn xobject(file: &[u8]) -> Option<Stream> {
    if file.starts_with(&[0xFF, 0xD8]) {
        jpeg_xobject(file)
    } else if file.starts_with(b"\x89PNG") {
        png_xobject(file)
    } else {
        None
    }
}

fn image_dict(width: u32, height: u32, space: Object) -> Dict {
    let mut dict = Dict::new();
    dict.insert(b"Type".to_vec(), Object::Name(b"XObject".to_vec()));
    dict.insert(b"Subtype".to_vec(), Object::Name(b"Image".to_vec()));
    dict.insert(b"Width".to_vec(), Object::Int(i64::from(width)));
    dict.insert(b"Height".to_vec(), Object::Int(i64::from(height)));
    dict.insert(b"ColorSpace".to_vec(), space);
    dict.insert(b"BitsPerComponent".to_vec(), Object::Int(8));
    dict.insert(b"Filter".to_vec(), Object::Name(b"FlateDecode".to_vec()));
    dict
}

/// An image XObject from an 8-bit, non-interlaced PNG. Grey, RGB and palette images keep their compressed data
/// as it is (PNG rows are Flate with predictors, which PDF reads directly); images with an alpha channel are laid
/// over white.
pub fn png_xobject(file: &[u8]) -> Option<Stream> {
    let mut at = 8;
    let (mut width, mut height, mut depth, mut kind, mut interlace) = (0u32, 0u32, 0u8, 0u8, 0u8);
    let mut palette = Vec::new();
    let mut idat = Vec::new();
    while at + 8 <= file.len() {
        let len = u32::from_be_bytes(file[at..at + 4].try_into().ok()?) as usize;
        let name = &file[at + 4..at + 8];
        let data = file.get(at + 8..at + 8 + len)?;
        match name {
            b"IHDR" => {
                width = u32::from_be_bytes(data[0..4].try_into().ok()?);
                height = u32::from_be_bytes(data[4..8].try_into().ok()?);
                depth = data[8];
                kind = data[9];
                interlace = data[12];
            }
            b"PLTE" => palette = data.to_vec(),
            b"IDAT" => idat.extend_from_slice(data),
            b"IEND" => break,
            _ => {}
        }
        at += 12 + len;
    }
    if depth != 8 || interlace != 0 || width == 0 || height == 0 {
        return None;
    }
    let colors: i64 = match kind {
        0 | 3 => 1,
        2 => 3,
        4 => 2,
        6 => 4,
        _ => return None,
    };
    if kind == 4 || kind == 6 {
        // Undo the row filters, then drop the alpha channel over a white page.
        let raw = filters::inflate(&idat).ok()?;
        let pixels = unfilter(&raw, width as usize, height as usize, colors as usize)?;
        let channels = colors as usize - 1;
        let out: Vec<u8> = pixels
            .chunks(colors as usize)
            .flat_map(|p| {
                let a = u16::from(p[channels]);
                p[..channels]
                    .iter()
                    .map(move |&c| ((u16::from(c) * a + 255 * (255 - a)) / 255) as u8)
                    .collect::<Vec<_>>()
            })
            .collect();
        let space = if channels == 1 {
            "DeviceGray"
        } else {
            "DeviceRGB"
        };
        return Some(Stream {
            dict: image_dict(width, height, Object::Name(space.as_bytes().to_vec())),
            data: filters::deflate(&out),
        });
    }
    let space = match kind {
        0 => Object::Name(b"DeviceGray".to_vec()),
        2 => Object::Name(b"DeviceRGB".to_vec()),
        _ => Object::Array(vec![
            Object::Name(b"Indexed".to_vec()),
            Object::Name(b"DeviceRGB".to_vec()),
            Object::Int((palette.len() / 3) as i64 - 1),
            Object::String(palette),
        ]),
    };
    let mut dict = image_dict(width, height, space);
    let mut params = Dict::new();
    params.insert(b"Predictor".to_vec(), Object::Int(15));
    params.insert(b"Colors".to_vec(), Object::Int(colors));
    params.insert(b"BitsPerComponent".to_vec(), Object::Int(8));
    params.insert(b"Columns".to_vec(), Object::Int(i64::from(width)));
    dict.insert(b"DecodeParms".to_vec(), Object::Dict(params));
    Some(Stream { dict, data: idat })
}

/// Reverse PNG row filtering for 8-bit samples with `bpp` bytes per pixel.
fn unfilter(raw: &[u8], width: usize, height: usize, bpp: usize) -> Option<Vec<u8>> {
    let row = width * bpp;
    let mut out = vec![0u8; row * height];
    for y in 0..height {
        let line = raw.get(y * (row + 1)..(y + 1) * (row + 1))?;
        let (kind, src) = (line[0], &line[1..]);
        for x in 0..row {
            let a = if x >= bpp { out[y * row + x - bpp] } else { 0 };
            let b = if y > 0 { out[(y - 1) * row + x] } else { 0 };
            let c = if x >= bpp && y > 0 {
                out[(y - 1) * row + x - bpp]
            } else {
                0
            };
            let predicted = match kind {
                0 => 0,
                1 => a,
                2 => b,
                3 => ((u16::from(a) + u16::from(b)) / 2) as u8,
                4 => {
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
                _ => return None,
            };
            out[y * row + x] = src[x].wrapping_add(predicted);
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn png_rgb_keeps_its_data_with_predictors() {
        let file = png(2, 2, 3, &[255, 0, 0, 0, 255, 0, 0, 0, 255, 9, 9, 9]);
        let s = xobject(&file).unwrap();
        assert_eq!(s.dict.get(&b"Width"[..]), Some(&Object::Int(2)));
        let mut dict = s.dict.clone();
        dict.remove(&b"Type"[..]);
        // What a PDF reader gets back is the original pixels.
        assert_eq!(
            filters::decode(&dict, &s.data).unwrap(),
            vec![255, 0, 0, 0, 255, 0, 0, 0, 255, 9, 9, 9]
        );
    }

    #[test]
    fn png_alpha_is_laid_over_white() {
        // One RGBA pixel, half transparent black, filter type 0.
        let raw = filters::deflate(&[0, 0, 0, 0, 128]);
        let mut file = b"\x89PNG\r\n\x1a\n".to_vec();
        let mut chunk = |kind: &[u8], data: &[u8]| {
            file.extend_from_slice(&(data.len() as u32).to_be_bytes());
            file.extend_from_slice(kind);
            file.extend_from_slice(data);
            file.extend_from_slice(&crc32(&[kind, data]).to_be_bytes());
        };
        chunk(b"IHDR", &[0, 0, 0, 1, 0, 0, 0, 1, 8, 6, 0, 0, 0]);
        chunk(b"IDAT", &raw);
        chunk(b"IEND", &[]);
        let s = png_xobject(&file).unwrap();
        assert_eq!(filters::inflate(&s.data).unwrap(), vec![127, 127, 127]);
    }

    #[test]
    fn unfilters_every_kind() {
        // Two rows of one grey pixel each: Sub on row 0 (no left neighbour), Paeth on row 1 (up only).
        assert_eq!(unfilter(&[1, 10, 4, 5], 1, 2, 1), Some(vec![10, 15]));
        assert_eq!(unfilter(&[3, 10, 2, 5], 1, 2, 1), Some(vec![10, 15])); // average of 0 and 10 is 5
        assert_eq!(unfilter(&[9, 0], 1, 1, 1), None);
    }

    /// A one-page PDF drawing a 64x64 grey picture stored as Flate samples, plus a tiny 8x8 one.
    fn picture_pdf() -> Vec<u8> {
        let big = filters::deflate(&vec![200u8; 64 * 64]);
        let small = filters::deflate(&[0u8; 64]);
        let content = b"q 300 0 0 400 100 200 cm /Im1 Do Q q 8 0 0 8 0 0 cm /Im2 Do Q";
        let mut objects: Vec<Vec<u8>> = vec![
            b"<< /Type /Catalog /Pages 2 0 R >>".to_vec(),
            b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec(),
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Resources << /XObject << /Im1 4 0 R /Im2 6 0 R >> >> /Contents 5 0 R >>".to_vec(),
        ];
        let stream = |dict: String, data: &[u8]| {
            let mut o = format!("{dict}\nstream\n").into_bytes();
            o.extend_from_slice(data);
            o.extend_from_slice(b"\nendstream");
            o
        };
        objects.push(stream(
            format!("<< /Type /XObject /Subtype /Image /Width 64 /Height 64 /ColorSpace /DeviceGray /BitsPerComponent 8 /Filter /FlateDecode /Length {} >>", big.len()),
            &big,
        ));
        objects.push(stream(format!("<< /Length {} >>", content.len()), content));
        objects.push(stream(
            format!("<< /Type /XObject /Subtype /Image /Width 8 /Height 8 /ColorSpace /DeviceGray /BitsPerComponent 8 /Filter /FlateDecode /Length {} >>", small.len()),
            &small,
        ));
        let mut pdf = b"%PDF-1.4\n".to_vec();
        let mut offsets = Vec::new();
        for (i, o) in objects.iter().enumerate() {
            offsets.push(pdf.len());
            pdf.extend_from_slice(format!("{} 0 obj\n", i + 1).as_bytes());
            pdf.extend_from_slice(o);
            pdf.extend_from_slice(b"\nendobj\n");
        }
        let xref = pdf.len();
        pdf.extend_from_slice(
            format!("xref\n0 {}\n0000000000 65535 f \n", objects.len() + 1).as_bytes(),
        );
        for o in offsets {
            pdf.extend_from_slice(format!("{o:010} 00000 n \n").as_bytes());
        }
        pdf.extend_from_slice(
            format!(
                "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n",
                objects.len() + 1
            )
            .as_bytes(),
        );
        pdf
    }

    #[test]
    fn finds_exports_and_replaces_a_picture() {
        let doc = Document::load(picture_pdf()).unwrap();
        let page = &doc.pages().unwrap()[0];
        let images = page_images(&doc, page);
        assert_eq!(images.len(), 1, "tiny pictures are left out");
        let im = &images[0];
        assert_eq!((im.width, im.height), (64, 64));
        let (file, mime) = export(&doc, im.id).unwrap();
        assert_eq!(mime, "image/png");
        // The exported PNG goes back into a PDF as the same pixels.
        let back = png_xobject(&file).unwrap();
        let mut d = back.dict.clone();
        d.remove(&b"Type"[..]);
        assert_eq!(
            filters::decode(&d, &back.data).unwrap(),
            vec![200u8; 64 * 64]
        );

        // Replace it with a "redrawn" JPEG and save: the page still draws Im1, which is now that JPEG.
        let mut jpeg = vec![0xFF, 0xD8];
        jpeg.extend_from_slice(&[0xFF, 0xC0, 0x00, 0x11, 0x08, 0x02, 0x00, 0x01, 0x80, 0x03]);
        let mut saver = crate::write::Saver::new(&doc);
        saver.set(im.id, Object::Stream(xobject(&jpeg).unwrap()));
        let saved = Document::load(saver.save().unwrap()).unwrap();
        let page = &saved.pages().unwrap()[0];
        let images = page_images(&saved, page);
        assert_eq!((images[0].width, images[0].height), (384, 512));
        let (out, mime) = export(&saved, images[0].id).unwrap();
        assert_eq!((out, mime), (jpeg, "image/jpeg"));
    }

    #[test]
    fn jpeg_xobject_takes_size_and_colours_from_the_file() {
        let mut j = vec![0xFF, 0xD8];
        j.extend_from_slice(&[0xFF, 0xC0, 0x00, 0x11, 0x08, 0x00, 0x10, 0x00, 0x20, 0x01]);
        let s = xobject(&j).unwrap();
        assert_eq!(s.dict.get(&b"Width"[..]), Some(&Object::Int(32)));
        assert_eq!(
            s.dict.get(&b"ColorSpace"[..]),
            Some(&Object::Name(b"DeviceGray".to_vec()))
        );
        assert!(xobject(b"GIF89a").is_none());
    }

    #[test]
    fn reads_jpeg_size() {
        // SOI, then a baseline SOF0 of 300x200 with 3 components.
        let mut j = vec![0xFF, 0xD8, 0xFF, 0xE0, 0x00, 0x04, 0x00, 0x00];
        j.extend_from_slice(&[0xFF, 0xC0, 0x00, 0x11, 0x08, 0x00, 0xC8, 0x01, 0x2C, 0x03]);
        assert_eq!(jpeg_info(&j), Some((300, 200, 3)));
    }

    #[test]
    fn png_has_valid_crc() {
        // The IEND chunk's CRC is fixed.
        let p = png(1, 1, 1, &[0]);
        assert!(p.ends_with(&[0xAE, 0x42, 0x60, 0x82]));
    }
}
