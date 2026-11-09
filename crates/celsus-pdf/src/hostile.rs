//! Files written to break a reader: loops between objects, sizes that ask for more memory than exists, content
//! that draws itself. Each must be read, or refused, quickly and within a small stack.

use std::fmt::Write as _;

use crate::write::Saver;
use crate::{Document, ObjRef, image, interp};

/// A file of numbered objects with no cross-reference table, so the reader has to find them itself.
fn file(objects: &[(u32, String)]) -> Vec<u8> {
    let mut pdf = String::from("%PDF-1.7\n");
    for (num, body) in objects {
        write!(pdf, "{num} 0 obj\n{body}\nendobj\n").unwrap();
    }
    pdf.into_bytes()
}

/// The catalog, page tree and one page (objects 1 to 3) with the given page entries.
fn one_page(page: &str) -> Vec<(u32, String)> {
    vec![
        (1, "<< /Type /Catalog /Pages 2 0 R >>".into()),
        (2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".into()),
        (
            3,
            format!("<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] {page} >>"),
        ),
    ]
}

fn stream(dict: &str, data: &str) -> String {
    format!("<< {dict} >>\nstream\n{data}\nendstream")
}

#[test]
fn stream_lengths_that_point_at_each_other() {
    // Every stream's /Length is the next stream, thousands deep, and the last points back at the first.
    let mut objects = one_page("/Contents 10 0 R");
    let count = 20_000;
    for i in 0..count {
        let next = 10 + (i + 1) % count;
        objects.push((10 + i, stream(&format!("/Length {next} 0 R"), "BT ET")));
    }
    let doc = Document::load(file(&objects)).unwrap();
    let pages = doc.pages().unwrap();
    assert_eq!(doc.page_content(&pages[0]).unwrap(), b"BT ET\n");
}

#[test]
fn an_object_stream_packed_inside_another() {
    // Object 10 says it lives in object stream 11, which says it lives in object stream 10. Both are also
    // ordinary objects here, so the file opens; asking for 12 walks the pair.
    let mut objects = one_page("");
    objects.push((
        10,
        stream("/Type /ObjStm /N 1 /First 5 /Length 9", "12 0 null"),
    ));
    let mut pdf = file(&objects);
    let at = pdf.len();
    // A cross-reference stream whose entries put 10 inside 11 and 11 inside 10.
    let rows: [[u8; 4]; 13] = std::array::from_fn(|n| match n {
        10 => [2, 0, 11, 0],
        11 => [2, 0, 10, 0],
        12 => [2, 0, 10, 0],
        _ => [0, 0, 0, 0],
    });
    let hex: String = rows.iter().flatten().map(|b| format!("{b:02x}")).collect();
    write!(
        pdf_text(&mut pdf),
        "20 0 obj\n<< /Type /XRef /Size 13 /W [1 2 1] /Root 1 0 R /Filter /ASCIIHexDecode /Length {} >>\nstream\n{hex}\nendstream\nendobj\nstartxref\n{at}\n%%EOF\n",
        hex.len()
    )
    .unwrap();
    // The table has no catalog, so the reader falls back to scanning; either way it must come back.
    let doc = Document::load(pdf).unwrap();
    let _ = doc.get(ObjRef {
        num: 12,
        generation: 0,
    });
    assert_eq!(doc.pages().unwrap().len(), 1);
}

/// Appending text to a byte buffer.
fn pdf_text(pdf: &mut Vec<u8>) -> Appender<'_> {
    Appender(pdf)
}

struct Appender<'a>(&'a mut Vec<u8>);

impl std::fmt::Write for Appender<'_> {
    fn write_str(&mut self, s: &str) -> std::fmt::Result {
        self.0.extend_from_slice(s.as_bytes());
        Ok(())
    }
}

#[test]
fn a_colour_space_named_after_itself() {
    let mut objects =
        one_page("/Resources << /ColorSpace << /A /B /B /A /C /C >> >> /Contents 4 0 R");
    objects.push((
        4,
        stream("/Length 40", "/A cs 1 0 0 sc /C CS 1 0 0 SC 0 0 9 9 re f"),
    ));
    let doc = Document::load(file(&objects)).unwrap();
    let pages = doc.pages().unwrap();
    interp::interpret(&doc, &pages[0]).unwrap();
}

#[test]
fn an_indexed_picture_based_on_itself() {
    let mut objects = one_page("/Resources << /XObject << /Im 4 0 R >> >>");
    objects.push((
        4,
        stream(
            "/Type /XObject /Subtype /Image /Width 64 /Height 64 /BitsPerComponent 8 /ColorSpace 5 0 R /Length 4",
            "abcd",
        ),
    ));
    objects.push((5, "[/Indexed 5 0 R 255 <000000>]".into()));
    let doc = Document::load(file(&objects)).unwrap();
    let pages = doc.pages().unwrap();
    let pictures = image::page_images(&doc, &pages[0]);
    assert_eq!(pictures.len(), 1);
    assert!(image::export(&doc, pictures[0].id).is_none());
}

#[test]
fn picture_sizes_that_overflow() {
    for (width, height) in [
        ("65536", "65536"),
        ("4294967296", "64"),
        ("9223372036854775807", "9223372036854775807"),
        ("-64", "64"),
        ("0", "0"),
    ] {
        let mut objects = one_page("/Resources << /XObject << /Im 4 0 R >> >>");
        objects.push((
            4,
            stream(
                &format!(
                    "/Type /XObject /Subtype /Image /Width {width} /Height {height} /BitsPerComponent 8 /ColorSpace /DeviceGray /Length 4"
                ),
                "abcd",
            ),
        ));
        let doc = Document::load(file(&objects)).unwrap();
        let pages = doc.pages().unwrap();
        assert!(image::page_images(&doc, &pages[0]).is_empty());
        assert!(
            image::export(
                &doc,
                ObjRef {
                    num: 4,
                    generation: 0
                }
            )
            .is_none()
        );
    }
    // A PNG whose header is cut short, and one that claims more pixels than it holds.
    let mut short = b"\x89PNG\r\n\x1a\n".to_vec();
    short.extend_from_slice(&[0, 0, 0, 2, b'I', b'H', b'D', b'R', 0, 0, 0, 0, 0, 0]);
    assert!(image::xobject(&short).is_none());
    let mut huge = b"\x89PNG\r\n\x1a\n".to_vec();
    huge.extend_from_slice(&[0, 0, 0, 13, b'I', b'H', b'D', b'R']);
    huge.extend_from_slice(&[
        0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 8, 6, 0, 0, 0,
    ]);
    huge.extend_from_slice(&[0; 4]);
    assert!(image::xobject(&huge).is_none());
}

#[test]
fn a_cross_reference_stream_with_empty_rows() {
    // Rows of no bytes and a count of four thousand million: every row "exists".
    let objects = one_page("");
    let mut pdf = file(&objects);
    let at = pdf.len();
    write!(
        pdf_text(&mut pdf),
        "9 0 obj\n<< /Type /XRef /Size 10 /W [0 0 0] /Index [0 4294967295] /Root 1 0 R /Length 0 >>\nstream\n\nendstream\nendobj\nstartxref\n{at}\n%%EOF\n"
    )
    .unwrap();
    let doc = Document::load(pdf).unwrap();
    assert_eq!(doc.pages().unwrap().len(), 1);
}

#[test]
fn offsets_past_the_end_of_the_file() {
    for trailer in [
        "<< /Size 4 /Root 1 0 R /Prev 99999999 >>",
        "<< /Size 4 /Root 1 0 R /XRefStm 99999999 >>",
        "<< /Size 4 /Root 1 0 R /Prev -5 >>",
        "<< /Size 6 /Root 1 0 R >>",
    ] {
        let objects = one_page("");
        let mut pdf = file(&objects);
        let at = pdf.len();
        let mut table = String::from("xref\n0 6\n0000000000 65535 f \n");
        for num in 1..=3 {
            let offset = crate::document::find(&pdf, format!("{num} 0 obj").as_bytes()).unwrap();
            writeln!(table, "{offset:010} 00000 n ").unwrap();
        }
        // Object 4 is free; object 5 is said to be far past the end.
        table.push_str("0000000000 00000 f \n0099999999 00000 n \n");
        write!(
            pdf_text(&mut pdf),
            "{table}trailer\n{trailer}\nstartxref\n{at}\n%%EOF\n"
        )
        .unwrap();
        let doc = Document::load(pdf).unwrap();
        assert_eq!(doc.pages().unwrap().len(), 1);
        // With a table that could be read, the entry past the end is an error and nothing worse.
        let far = doc.get(ObjRef {
            num: 5,
            generation: 0,
        });
        assert!(doc.repaired || far.is_err());
    }
}

#[test]
fn thousands_of_streams_sharing_one_end() {
    // Each header opens a stream that runs to the single `endstream` at the very end, so every object would
    // hold a copy of almost the whole file.
    let count = 20_000u32;
    let mut pdf = file(&one_page("")).to_vec();
    for i in 0..count {
        write!(pdf_text(&mut pdf), "{} 0 obj\n<< >>\nstream\n", 10 + i).unwrap();
    }
    pdf.extend_from_slice(b"\nendstream\nendobj\n");
    let size = pdf.len();
    // Refusing the file is as good as reading it; what matters is how much of it is kept.
    let Ok(doc) = Document::load(pdf) else {
        return;
    };
    let mut held = 0usize;
    for id in doc.object_ids() {
        if let Ok(obj) = doc.get(id)
            && let Some(s) = obj.as_stream()
        {
            held += s.data.len();
        }
    }
    assert!(
        held <= 2 * size + (1 << 20),
        "{held} bytes kept of a {size} byte file"
    );
}

#[test]
fn an_object_number_in_the_thousands_of_millions() {
    let mut objects = one_page("");
    objects.push((4_000_000_000, "<< /Big true >>".into()));
    objects.push((u32::MAX, "<< /Big true >>".into()));
    let doc = Document::load(file(&objects)).unwrap();
    assert!(doc.object_ids().iter().all(|id| id.num < 100));
    let saved = Saver::new(&doc).save().unwrap();
    assert!(saved.len() < 10_000);
    assert_eq!(Document::load(saved).unwrap().pages().unwrap().len(), 1);
}

#[test]
fn forms_that_draw_forms() {
    // Twelve levels, each drawing the next twenty times: 20^12 operators if every one were run.
    let draws = "/F Do ".repeat(20);
    let mut objects = one_page("/Resources << /XObject << /F 10 0 R >> >> /Contents 4 0 R");
    objects.push((4, stream("/Length 5", "/F Do")));
    for level in 0..12u32 {
        let next = 10 + (level + 1) % 12;
        objects.push((
            10 + level,
            stream(
                &format!(
                    "/Type /XObject /Subtype /Form /BBox [0 0 9 9] /Resources << /XObject << /F {next} 0 R >> >> /Length {}",
                    draws.len()
                ),
                &draws,
            ),
        ));
    }
    let doc = Document::load(file(&objects)).unwrap();
    let pages = doc.pages().unwrap();
    interp::interpret(&doc, &pages[0]).unwrap();
}

#[test]
fn a_font_map_with_endless_ranges() {
    // Each range of twelve bytes stands for 65,536 codes.
    let mut map = String::from("1 begincodespacerange <00000000> <ffffffff> endcodespacerange\n");
    for block in 0..400u32 {
        writeln!(map, "100 beginbfrange").unwrap();
        for i in 0..100u32 {
            let lo = (block * 100 + i) << 16;
            writeln!(map, "<{lo:08x}> <{:08x}> <0041>", lo | 0xFFFF).unwrap();
        }
        writeln!(map, "endbfrange").unwrap();
    }
    let cmap = crate::cmap::CMap::parse(map.as_bytes());
    assert!(cmap.unicode.len() <= (1 << 20) + 0x10000);
}
