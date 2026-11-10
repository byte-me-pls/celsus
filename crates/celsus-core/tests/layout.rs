//! Pages that used to come out visibly wrong: entries of a table of contents run together, two texts printed
//! over each other read as one, a translation cut off by the clipping area of the text it replaces.
//!
//! Two of them write a page and need the system's fonts; they are skipped with a note where there are none.

use std::collections::BTreeMap;
use std::path::PathBuf;

use celsus_core::Language;
use celsus_core::extract::extract_page;
use celsus_core::fonts::FontSet;
use celsus_core::render::BookWriter;

/// A one-page PDF with the given content. `/F1` is Helvetica, `/F2` is Times, `/F3` is a face whose name and
/// flags say nothing, with one width for every character, and `/F4` is Times in italics.
fn page(content: &str) -> Vec<u8> {
    let objects = [
        "<< /Type /Catalog /Pages 2 0 R >>".to_string(),
        "<< /Type /Pages /Kids [5 0 R] /Count 1 >>".to_string(),
        "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_string(),
        "<< /Type /Font /Subtype /Type1 /BaseFont /Times-Roman >>".to_string(),
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Resources << /Font << /F1 3 0 R /F2 4 0 R /F3 7 0 R /F4 8 0 R >> >> /Contents 6 0 R >>".to_string(),
        format!("<< /Length {} >>\nstream\n{content}\nendstream", content.len()),
        format!(
            "<< /Type /Font /Subtype /TrueType /BaseFont /c8bf69+Listing /Encoding /WinAnsiEncoding /FirstChar 32 /LastChar 126 /Widths [{}] >>",
            "500 ".repeat(95).trim_end()
        ),
        "<< /Type /Font /Subtype /Type1 /BaseFont /Times-Italic >>".to_string(),
    ];
    let mut pdf = b"%PDF-1.4\n".to_vec();
    let mut offsets = Vec::new();
    for (i, o) in objects.iter().enumerate() {
        offsets.push(pdf.len());
        pdf.extend_from_slice(format!("{} 0 obj\n{o}\nendobj\n", i + 1).as_bytes());
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

/// The text of every block on the page.
fn blocks(content: &str) -> Vec<String> {
    let doc = celsus_pdf::Document::load(page(content)).unwrap();
    let pages = doc.pages().unwrap();
    let extracted = extract_page(&doc, &pages[0], 1).unwrap();
    extracted.layout.blocks.iter().map(|b| b.plain()).collect()
}

const TITLES: [&str; 4] = [
    "Relationships between the contexts",
    "Customers and suppliers in a team",
    "A layer that keeps the model clean",
    "Services that are open to all hosts",
];

/// Lines of nearly the same length, one under the other, with or without a page number at the end of each row.
fn entries(numbers: Option<[&str; 4]>) -> String {
    let mut out = String::new();
    for (i, title) in TITLES.iter().enumerate() {
        let y = 700 - 13 * i;
        out.push_str(&format!("BT /F1 10 Tf 72 {y} Td ({title}) Tj ET\n"));
        if let Some(numbers) = numbers {
            out.push_str(&format!("BT /F1 10 Tf 400 {y} Td ({}) Tj ET\n", numbers[i]));
        }
    }
    out
}

/// The fonts of this machine for a language, or nothing where there are none to write a page with.
fn system_fonts(lang: &Language) -> Option<FontSet> {
    let dir = std::env::var_os("CELSUS_TEST_FONTS_DIR")
        .map(PathBuf::from)
        .into_iter()
        .chain([
            PathBuf::from(r"C:\Windows\Fonts"),
            PathBuf::from("/System/Library/Fonts"),
            PathBuf::from("/usr/share/fonts"),
        ])
        .find(|p| p.is_dir())?;
    let fonts = FontSet::for_language(&dir, lang);
    if fonts.is_empty() { None } else { Some(fonts) }
}

#[test]
fn a_page_number_ends_its_entry() {
    // Without the numbers these four lines are a paragraph like any other.
    let plain = blocks(&entries(None));
    assert!(
        plain.iter().any(|b| TITLES.iter().all(|t| b.contains(t))),
        "{plain:?}"
    );
    // With them, each line is an entry of its own and stays beside its number.
    let listed = blocks(&entries(Some(["352", "354", "356", "358"])));
    for title in TITLES {
        assert!(listed.iter().any(|b| b == title), "{title}: {listed:?}");
    }
    // The pages before the first chapter are counted in roman numerals.
    let front = blocks(&entries(Some(["ix", "xi", "xiv", "xix"])));
    for title in TITLES {
        assert!(front.iter().any(|b| b == title), "{title}: {front:?}");
    }
}

#[test]
fn a_table_in_the_next_column_does_not_cut_a_paragraph() {
    // Six lines of a paragraph on the left. On the right, the cells of a table, each a text of its own,
    // stand between its lines in the order of the page.
    let mut content = String::new();
    for i in 0..6 {
        let y = 700 - 13 * i;
        content.push_str(&format!(
            "BT /F1 10 Tf 72 {y} Td ({}) Tj ET
",
            TITLES[i % 4]
        ));
    }
    for (row, y) in [705, 679, 653].into_iter().enumerate() {
        for (column, x) in [330, 390, 450, 510].into_iter().enumerate() {
            let word = ["north", "south", "east", "west"][(row + column) % 4];
            content.push_str(&format!(
                "BT /F1 10 Tf {x} {y} Td ({word}) Tj ET
"
            ));
        }
    }
    let found = blocks(&content);
    assert!(
        found
            .iter()
            .any(|b| b.matches(TITLES[0]).count() == 2 && b.matches(TITLES[1]).count() == 2),
        "{found:?}"
    );
}

#[test]
fn a_word_in_another_type_stays_in_its_line() {
    // A grey label lies on the start of the line. The word set apart further on lies on neither of the two
    // texts: it belongs to the line it was written in, not to the label.
    let found = blocks(
        "BT /F2 10 Tf 0.6 g 70 700 Td (Runtime problems) Tj ET\n\
         BT /F1 10 Tf 72 700 Td 0 g (The interpreter reports ) Tj 0.3 g (not) Tj 0 g ( the error we saw) Tj ET",
    );
    assert!(
        found
            .iter()
            .any(|b| b.contains("reports not the error we saw")),
        "{found:?}"
    );
    assert!(found.iter().any(|b| b == "Runtime problems"), "{found:?}");
}

#[test]
fn art_under_the_panel_of_a_text_does_not_cut_it() {
    let line = "BT /F1 10 Tf 0 g 72 700 Td (The words of this line run on over the art that lies below it) Tj ET";
    // The dark shape is hidden by the white panel drawn after it, and the line is read whole.
    let hidden = blocks(&format!(
        "0.2 g 200 690 100 40 re f\n1 g 60 600 500 150 re f\n{line}"
    ));
    assert!(
        hidden
            .iter()
            .any(|b| b.contains("run on over the art that lies below")),
        "{hidden:?}"
    );
    // Drawn the other way round the shape is there to see, and what is printed on it is a text of its own.
    let shown = blocks(&format!(
        "1 g 60 600 500 150 re f\n0.2 g 200 690 100 40 re f\n{line}"
    ));
    assert!(
        !shown
            .iter()
            .any(|b| b.contains("run on over the art that lies below")),
        "{shown:?}"
    );
}

#[test]
fn a_step_in_the_baseline_starts_another_text() {
    // Two labels of a drawing, a word's width apart: on one baseline they read as one line.
    let level = blocks(
        "BT /F1 10 Tf 72 700 Td (Input buffer) Tj ET\n\
         BT /F1 10 Tf 132 700 Td (Output buffer) Tj ET",
    );
    assert!(
        level.iter().any(|b| b == "Input buffer Output buffer"),
        "{level:?}"
    );
    // Set a point higher, the second one was placed by itself.
    let stepped = blocks(
        "BT /F1 10 Tf 72 700 Td (Input buffer) Tj ET\n\
         BT /F1 10 Tf 132 701 Td (Output buffer) Tj ET",
    );
    assert!(stepped.iter().any(|b| b == "Input buffer"), "{stepped:?}");
    assert!(stepped.iter().any(|b| b == "Output buffer"), "{stepped:?}");
}

#[test]
fn a_line_alone_on_its_page_keeps_its_size() {
    let lang: Language = "tr".parse().unwrap();
    let Some(fonts) = system_fonts(&lang) else {
        eprintln!("no fonts found, skipping");
        return;
    };
    // Nothing else on the page shows how wide its text area is; the translation is twice as long as the line.
    let source = page("BT /F1 9 Tf 270 60 Td (Printed in Canada) Tj ET");
    let doc = celsus_pdf::Document::load(source).unwrap();
    let pages = doc.pages().unwrap();
    let extracted = extract_page(&doc, &pages[0], 1).unwrap();
    let block = &extracted.layout.blocks[0];
    let translations = BTreeMap::from([(
        block.id.clone(),
        "Bu kitap Kanada'da basılmıştır".to_string(),
    )]);
    let mut writer = BookWriter::new(&doc, &fonts);
    let report = writer
        .translate_page(&pages[0], &extracted, &translations, &lang)
        .unwrap();
    assert_eq!(report.replaced.len(), 1, "{:?}", report.kept);

    let back = celsus_pdf::Document::load(writer.finish().unwrap()).unwrap();
    let back_pages = back.pages().unwrap();
    let read = extract_page(&back, &back_pages[0], 1).unwrap();
    let sizes: Vec<f32> = read
        .content
        .runs
        .iter()
        .filter(|r| {
            r.glyphs
                .iter()
                .any(|g| g.visible && !g.text.trim().is_empty())
        })
        .map(|r| r.size)
        .collect();
    assert!(!sizes.is_empty(), "the translation was written");
    assert!(sizes.iter().all(|s| (s - 9.0).abs() < 0.1), "{sizes:?}");
    assert_eq!(
        read.layout
            .blocks
            .iter()
            .map(|b| b.lines.len())
            .sum::<usize>(),
        1
    );
}

#[test]
fn the_title_of_an_entry_gives_its_colour() {
    // The dots that lead to the page number are lighter than the title and more than its letters.
    let dots = " .".repeat(40);
    let source = page(&format!(
        "BT /F1 10 Tf 0.1 g 72 700 Td (Getting started) Tj 0.7 g ({dots} ) Tj 0.1 g (12) Tj ET"
    ));
    let doc = celsus_pdf::Document::load(source).unwrap();
    let pages = doc.pages().unwrap();
    let extracted = extract_page(&doc, &pages[0], 1).unwrap();
    let entry = extracted
        .layout
        .blocks
        .iter()
        .find(|b| b.plain().contains("Getting started"))
        .expect("the entry is read");
    assert!(entry.style.color & 0xff < 0x40, "{:06x}", entry.style.color);
}

#[test]
fn a_face_of_one_width_is_a_listing() {
    let read = |font: &str| {
        let source = page(&format!(
            "BT /{font} 10 Tf 72 700 Td (Changes that are not staged for the next commit) Tj ET"
        ));
        let doc = celsus_pdf::Document::load(source).unwrap();
        let pages = doc.pages().unwrap();
        extract_page(&doc, &pages[0], 1).unwrap().layout.blocks
    };
    // What a program printed is left as it is, and its face is known by the widths alone.
    let listing = read("F3");
    assert!(
        listing[0].style.monospace && listing[0].protected,
        "{:?}",
        listing[0].style
    );
    assert_eq!(listing[0].style.font_name.as_deref(), Some("Listing"));
    let prose = read("F1");
    assert!(!prose[0].style.monospace && !prose[0].protected);
}

/// A paragraph of three lines with a title in italics that runs from the first line into the second.
const WITH_A_TITLE: &str = "BT /F2 10 Tf 72 700 Td (The layers are described at length in the second part of the book ) Tj /F4 10 Tf (Domain) Tj ET
BT /F4 10 Tf 72 688 Td (Design) Tj /F2 10 Tf (, which every member of the team should have read before the) Tj ET
BT /F2 10 Tf 72 676 Td (internationalisation of the model begins.) Tj ET";

#[test]
fn words_in_italics_are_set_apart_for_the_translator() {
    let doc = celsus_pdf::Document::load(page(WITH_A_TITLE)).unwrap();
    let pages = doc.pages().unwrap();
    let extracted = extract_page(&doc, &pages[0], 1).unwrap();
    let block = &extracted.layout.blocks[0];
    assert_eq!(
        block.source(),
        "The layers are described at length in the second part of the book <i>Domain Design</i>, which every \
         member of the team should have read before the internationalisation of the model begins."
    );
    assert!(!block.style.italic && block.inline.len() == 1 && block.inline[0].italic);
    // A text in one style has nothing to tell apart.
    assert_eq!(
        blocks("BT /F4 10 Tf 72 700 Td (All of this line is in italics) Tj ET"),
        ["All of this line is in italics"]
    );
}

#[test]
fn words_in_italics_are_written_in_italics() {
    let lang: Language = "tr".parse().unwrap();
    let Some(fonts) = system_fonts(&lang) else {
        eprintln!("no fonts found, skipping");
        return;
    };
    let write = |translation: &str| {
        let doc = celsus_pdf::Document::load(page(WITH_A_TITLE)).unwrap();
        let pages = doc.pages().unwrap();
        let extracted = extract_page(&doc, &pages[0], 1).unwrap();
        let id = extracted.layout.blocks[0].id.clone();
        let mut writer = BookWriter::new(&doc, &fonts);
        writer
            .translate_page(
                &pages[0],
                &extracted,
                &BTreeMap::from([(id, translation.to_owned())]),
                &lang,
            )
            .unwrap();
        let out = celsus_pdf::Document::load(writer.finish().unwrap()).unwrap();
        let pages = out.pages().unwrap();
        extract_page(&out, &pages[0], 1).unwrap().layout.blocks
    };
    let written = write(
        "Katmanlar, ekibin her üyesinin alan modeli üzerindeki çalışma başlamadan önce okuması gereken \
         <i>Alan Tasarımı</i> kitabında anlatılır.",
    );
    assert_eq!(written.len(), 1, "{written:?}");
    if written[0].inline.iter().any(|s| s.italic) {
        assert!(
            written[0].source().contains("<i>Alan Tasarımı</i>"),
            "{}",
            written[0].source()
        );
    } else {
        eprintln!("no italic face found, skipping");
    }
    // Tags that are not in order are left out, and the words are written in the style of the paragraph.
    let plain = write(
        "Katmanlar, ekibin her üyesinin alan modeli üzerindeki çalışma başlamadan önce okuması gereken \
         <i>Alan Tasarımı kitabında anlatılır.",
    );
    assert!(
        plain[0].inline.is_empty() && !plain[0].plain().contains('<'),
        "{}",
        plain[0].source()
    );
}

#[test]
fn a_sign_no_text_face_has_is_borrowed() {
    let lang: Language = "tr".parse().unwrap();
    let Some(fonts) = system_fonts(&lang) else {
        eprintln!("no fonts found, skipping");
        return;
    };
    if !fonts.faces().iter().any(|f| f.symbols) {
        eprintln!("no font of signs found, skipping");
        return;
    }
    // The number in a circle that points back to a line of a listing: the note is translated all the same.
    let source =
        page("BT /F1 10 Tf 72 700 Td (1. The first note explains the command above) Tj ET");
    let doc = celsus_pdf::Document::load(source).unwrap();
    let pages = doc.pages().unwrap();
    let extracted = extract_page(&doc, &pages[0], 1).unwrap();
    let block = &extracted.layout.blocks[0];
    let translations = BTreeMap::from([(
        block.id.clone(),
        "\u{2460} \u{0130}lk not yukar\u{0131}daki komutu a\u{00e7}\u{0131}klar".to_string(),
    )]);
    let mut writer = BookWriter::new(&doc, &fonts);
    let report = writer
        .translate_page(&pages[0], &extracted, &translations, &lang)
        .unwrap();
    assert_eq!(report.replaced.len(), 1, "{:?}", report.kept);
    assert!(report.kept.is_empty(), "{:?}", report.kept);
}

#[test]
fn texts_printed_over_each_other_are_read_apart() {
    // A label in another font lies on the line, a little to the right: the letters of the two alternate.
    let found = blocks(
        "BT /F1 10 Tf 72 700 Td (The interpreter reports the error) Tj ET\n\
         BT /F2 10 Tf 74 700 Td (Runtime problems are shown here) Tj ET",
    );
    assert!(
        found
            .iter()
            .any(|b| b.contains("The interpreter reports the error")),
        "{found:?}"
    );
    assert!(
        found
            .iter()
            .any(|b| b.contains("Runtime problems are shown here")),
        "{found:?}"
    );
}

#[test]
fn a_translation_longer_than_the_clipped_original_is_not_cut() {
    let lang: Language = "tr".parse().unwrap();
    let Some(fonts) = system_fonts(&lang) else {
        eprintln!("no fonts found, skipping");
        return;
    };
    // The line is drawn inside a clipping area that is just as high as the line itself.
    let source = page(
        "q 72 694 200 16 re W n\n\
         BT /F1 12 Tf 72 700 Td (A short line of plain text here) Tj ET\n\
         Q",
    );
    let doc = celsus_pdf::Document::load(source).unwrap();
    let pages = doc.pages().unwrap();
    let extracted = extract_page(&doc, &pages[0], 1).unwrap();
    let block = &extracted.layout.blocks[0];
    let long =
        "Burada daha uzun bir çeviri var ve tek satıra sığmadığı için ikinci bir satıra iniyor";
    let translations = BTreeMap::from([(block.id.clone(), long.to_string())]);
    let mut writer = BookWriter::new(&doc, &fonts);
    let report = writer
        .translate_page(&pages[0], &extracted, &translations, &lang)
        .unwrap();
    assert_eq!(report.replaced.len(), 1, "{:?}", report.kept);

    let back = celsus_pdf::Document::load(writer.finish().unwrap()).unwrap();
    let back_pages = back.pages().unwrap();
    let read = extract_page(&back, &back_pages[0], 1).unwrap();
    let drawn: Vec<_> = read
        .content
        .runs
        .iter()
        .flat_map(|r| &r.glyphs)
        .filter(|g| !g.text.trim().is_empty())
        .collect();
    assert!(drawn.len() > 40, "the translation was written");
    assert!(
        drawn.iter().all(|g| g.visible),
        "part of the translation lies outside the clipping area"
    );
}
