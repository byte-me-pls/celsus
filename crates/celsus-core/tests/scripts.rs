//! Writing translations in every major writing system into a PDF and reading them back.
//!
//! The fonts come from the system: `C:\Windows\Fonts` on Windows, `/System/Library/Fonts` on macOS,
//! `/usr/share/fonts` on Linux (the CI and the Docker image install Noto and WenQuanYi), or
//! `CELSUS_TEST_FONTS_DIR`. A language whose fonts are not installed is
//! skipped with a note rather than failing.

use std::collections::BTreeMap;
use std::path::PathBuf;

use celsus_core::Language;
use celsus_core::extract::extract_page;
use celsus_core::fonts::FontSet;
use celsus_core::render::BookWriter;
use celsus_core::shape::Shaper;

fn style() -> celsus_core::TextStyle {
    celsus_core::TextStyle {
        font_name: None,
        font_size: 12.0,
        color: 0,
        bold: false,
        italic: false,
        serif: false,
        monospace: false,
    }
}

fn fonts_dir() -> Option<PathBuf> {
    std::env::var_os("CELSUS_TEST_FONTS_DIR")
        .map(PathBuf::from)
        .into_iter()
        .chain([
            PathBuf::from(r"C:\Windows\Fonts"),
            PathBuf::from("/System/Library/Fonts"),
            PathBuf::from("/usr/share/fonts"),
        ])
        .find(|p| p.is_dir())
}

/// Fonts for `lang`, if a font for its writing system is installed.
fn fonts_for(lang: &Language, sample: &str) -> Option<FontSet> {
    let fonts = FontSet::for_language(&fonts_dir()?, lang);
    let script = celsus_core::languages::info(lang).map(|i| i.script);
    let shaper = Shaper::new(&fonts, fonts.chain_for(&style(), script), lang.is_rtl());
    if fonts.is_empty() || !shaper.missing(sample).is_empty() {
        eprintln!("skipping {}: no font covers it here", lang.as_str());
        return None;
    }
    Some(fonts)
}

fn shaper<'f>(fonts: &'f FontSet, lang: &Language) -> Shaper<'f> {
    let script = celsus_core::languages::info(lang).map(|i| i.script);
    Shaper::new(fonts, fonts.chain_for(&style(), script), lang.is_rtl())
}

fn glyph_text(line: &celsus_core::shape::ShapedLine) -> String {
    line.runs
        .iter()
        .flat_map(|r| &r.glyphs)
        .map(|g| g.text.as_str())
        .collect()
}

#[test]
fn arabic_is_joined_and_right_to_left() {
    let ar: Language = "ar".parse().unwrap();
    let word = "سلام";
    let Some(fonts) = fonts_for(&ar, word) else {
        return;
    };
    let s = shaper(&fonts, &ar);
    let line = s.shape_line(word, 12.0);
    // Visual order: the first letter is drawn last (rightmost).
    let drawn: String = line
        .runs
        .iter()
        .rev()
        .flat_map(|r| r.glyphs.iter().rev())
        .map(|g| g.text.as_str())
        .collect();
    assert_eq!(drawn, word);
    // Joined: the letters do not use their isolated forms from the character map.
    let run = &line.runs[0];
    let font = &fonts.faces()[run.face].font;
    let isolated: Vec<u16> = word.chars().rev().filter_map(|c| font.glyph(c)).collect();
    let shaped: Vec<u16> = run.glyphs.iter().map(|g| g.gid).collect();
    assert_ne!(shaped, isolated, "Arabic letters were not joined");
}

#[test]
fn numbers_inside_arabic_stay_left_to_right() {
    let ar: Language = "ar".parse().unwrap();
    let text = "صفحة 123";
    let Some(fonts) = fonts_for(&ar, text) else {
        return;
    };
    let drawn = glyph_text(&shaper(&fonts, &ar).shape_line(text, 12.0));
    // The number comes first (leftmost) and reads 1, 2, 3.
    assert!(drawn.starts_with("123"), "{drawn:?}");
}

#[test]
fn hebrew_is_right_to_left() {
    let he: Language = "he".parse().unwrap();
    let word = "שלום";
    let Some(fonts) = fonts_for(&he, word) else {
        return;
    };
    let drawn = glyph_text(&shaper(&fonts, &he).shape_line(word, 12.0));
    assert_eq!(drawn, "םולש");
}

#[test]
fn devanagari_forms_conjuncts() {
    let hi: Language = "hi".parse().unwrap();
    // क्ष is three characters that make one conjunct; ि is a vowel sign drawn before its consonant.
    let word = "क्षत्रिय";
    let Some(fonts) = fonts_for(&hi, word) else {
        return;
    };
    let line = shaper(&fonts, &hi).shape_line(word, 12.0);
    let glyphs: usize = line.runs.iter().map(|r| r.glyphs.len()).sum();
    assert!(
        glyphs < word.chars().count(),
        "{glyphs} glyphs for {} chars",
        word.chars().count()
    );
    // Every character is still there for searching and copying.
    let mut drawn: Vec<char> = glyph_text(&line).chars().collect();
    let mut want: Vec<char> = word.chars().collect();
    drawn.sort_unstable();
    want.sort_unstable();
    assert_eq!(drawn, want);
}

#[test]
fn chinese_breaks_between_characters() {
    let zh: Language = "zh-hans".parse().unwrap();
    let text = "这是一本关于历史的书，讲述了古代文明的兴衰。";
    let Some(fonts) = fonts_for(&zh, text) else {
        return;
    };
    let s = shaper(&fonts, &zh);
    let lines = celsus_core::fit::wrap(text, 60.0, 10.0, &s);
    assert!(lines.len() > 2, "{lines:?}");
    // Closing punctuation never starts a line.
    assert!(
        lines
            .iter()
            .all(|l| !l.starts_with('，') && !l.starts_with('。')),
        "{lines:?}"
    );
}

#[test]
fn latin_inside_chinese_falls_back_per_character() {
    let zh: Language = "zh-hans".parse().unwrap();
    let text = "Rust 语言";
    let Some(fonts) = fonts_for(&zh, text) else {
        return;
    };
    let line = shaper(&fonts, &zh).shape_line(text, 12.0);
    assert_eq!(glyph_text(&line), text);
    assert!(line.width > 0.0);
}

/// A one-page PDF with one line of Helvetica text.
fn sample_pdf(text: &str) -> Vec<u8> {
    page_pdf(&format!("BT /F1 14 Tf 72 700 Td ({text}) Tj ET"), "")
}

/// A one-page PDF that draws `content` with Helvetica as `/F1`. `more` goes into the page's dictionary.
fn page_pdf(content: &str, more: &str) -> Vec<u8> {
    let objects = [
        "<< /Type /Catalog /Pages 2 0 R >>".to_string(),
        "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_string(),
        format!(
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Resources << /Font << /F1 4 0 R >> >> /Contents 5 0 R {more} >>"
        ),
        "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_string(),
        format!(
            "<< /Length {} >>\nstream\n{content}\nendstream",
            content.len()
        ),
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

const SAMPLES: &[(&str, &str)] = &[
    ("ru", "История древнего мира"),
    ("el", "Ιστορία του αρχαίου κόσμου"),
    ("ar", "تاريخ العالم القديم"),
    ("he", "ההיסטוריה של העולם העתיק"),
    ("fa", "تاریخ جهان باستان"),
    ("hi", "प्राचीन विश्व का इतिहास"),
    ("bn", "প্রাচীন বিশ্বের ইতিহাস"),
    ("ta", "பண்டைய உலக வரலாறு"),
    ("th", "ประวัติศาสตร์โลกยุคโบราณ"),
    ("zh-hans", "古代世界的历史"),
    ("zh-hant", "古代世界的歷史"),
    ("ja", "古代世界の歴史"),
    ("ko", "고대 세계의 역사"),
    ("ka", "ძველი მსოფლიოს ისტორია"),
    ("hy", "Հին աշխարհի պատմություն"),
    ("am", "የጥንታዊው ዓለም ታሪክ"),
];

/// Every sample is written over an English line and can be found again in the output: the glyphs carry their
/// text, so the translated PDF stays searchable and copyable.
#[test]
fn every_script_is_written_and_read_back() {
    let source = sample_pdf("A History of the Ancient World");
    let out_dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("scripts");
    std::fs::create_dir_all(&out_dir).unwrap();
    let mut written = 0;
    for (code, text) in SAMPLES {
        let lang: Language = code.parse().unwrap();
        let Some(fonts) = fonts_for(&lang, text) else {
            continue;
        };
        let doc = celsus_pdf::Document::load(source.clone()).unwrap();
        let pages = doc.pages().unwrap();
        let extracted = extract_page(&doc, &pages[0], 1).unwrap();
        let block = &extracted.layout.blocks[0];
        let translations = BTreeMap::from([(block.id.clone(), text.to_string())]);
        let mut writer = BookWriter::new(&doc, &fonts);
        let report = writer
            .translate_page(&pages[0], &extracted, &translations, &lang)
            .unwrap();
        assert_eq!(report.replaced.len(), 1, "{code}: {:?}", report.kept);
        let bytes = writer.finish().unwrap();
        std::fs::write(out_dir.join(format!("{code}.pdf")), &bytes).unwrap();

        let back = celsus_pdf::Document::load(bytes).unwrap();
        let back_pages = back.pages().unwrap();
        let read = extract_page(&back, &back_pages[0], 1).unwrap();
        let all: String = read.layout.blocks.iter().map(|b| b.plain()).collect();
        let squash = |s: &str| -> Vec<char> {
            let mut v: Vec<char> = s.chars().filter(|c| !c.is_whitespace()).collect();
            v.sort_unstable();
            v
        };
        // Right-to-left text comes back in drawing order, so the characters are compared as a set.
        assert_eq!(squash(&all), squash(text), "{code}: read back {all:?}");
        assert!(
            !all.contains("History"),
            "{code}: the English text is still there"
        );
        if !lang.is_rtl() && !matches!(*code, "hi" | "bn" | "ta" | "th") {
            assert_eq!(
                all.split_whitespace().collect::<String>(),
                text.split_whitespace().collect::<String>(),
                "{code}"
            );
        }
        written += 1;
    }
    eprintln!(
        "{written} of {} scripts written to {}",
        SAMPLES.len(),
        out_dir.display()
    );
    assert!(written > 0, "no fonts found for any script");
}

/// Two columns under a headline, with a row of figures and a link in the left one.
fn two_column_pdf() -> Vec<u8> {
    let mut content = String::from(
        "BT /F1 14 Tf 72 720 Td (A headline that runs across both of the columns of this page) Tj ET\n",
    );
    let column = |content: &mut String, x: u32, word: &str| {
        for (i, y) in (0..6).map(|i| (i, 680 - 14 * i)) {
            content.push_str(&format!(
                "BT /F1 10 Tf {x} {y} Td (The {word} column has line {i} of running text) Tj ET\n"
            ));
        }
    };
    column(&mut content, 72, "first");
    column(&mut content, 326, "second");
    content.push_str("BT /F1 10 Tf 120 570 Td (1234 5678) Tj ET\n");
    // A rule under the left column, and a link over its figures.
    content.push_str("72 560 150 1 re f\n");
    page_pdf(
        &content,
        "/Annots [<< /Type /Annot /Subtype /Link /Rect [118 566 172 580] /Border [0 0 0] >>]",
    )
}

/// A page of two columns written in Arabic has its columns change places, each with everything in it, and what
/// lies across them stays.
#[test]
fn columns_change_places_for_a_language_read_from_right_to_left() {
    let ar: Language = "ar".parse().unwrap();
    let first = "هذا نص العمود الأول 111 وهو يمتد على عدة أسطر في العمود حتى نهايته";
    let second = "هذا نص العمود الثاني 222 وهو يمتد على عدة أسطر في العمود حتى نهايته";
    let headline = "عنوان 333 يمتد فوق العمودين";
    let Some(fonts) = fonts_for(&ar, first) else {
        return;
    };
    let source = two_column_pdf();
    let doc = celsus_pdf::Document::load(source).unwrap();
    let pages = doc.pages().unwrap();
    let extracted = extract_page(&doc, &pages[0], 1).unwrap();
    let id_of = |word: &str| {
        let found: Vec<_> = extracted
            .layout
            .blocks
            .iter()
            .filter(|b| b.plain().contains(word))
            .collect();
        assert_eq!(found.len(), 1, "blocks with {word:?}");
        found[0].id.clone()
    };
    let translations = BTreeMap::from([
        (id_of("first"), first.to_string()),
        (id_of("second"), second.to_string()),
        (id_of("headline"), headline.to_string()),
    ]);
    let mut writer = BookWriter::new(&doc, &fonts);
    let report = writer
        .translate_page(&pages[0], &extracted, &translations, &ar)
        .unwrap();
    assert_eq!(report.replaced.len(), 3, "{:?}", report.kept);
    let bytes = writer.finish().unwrap();
    let out_dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("scripts");
    std::fs::create_dir_all(&out_dir).unwrap();
    std::fs::write(out_dir.join("columns-ar.pdf"), &bytes).unwrap();

    let back = celsus_pdf::Document::load(bytes).unwrap();
    let back_pages = back.pages().unwrap();
    let read = extract_page(&back, &back_pages[0], 1).unwrap();
    let middle_of = |word: &str| {
        let found: Vec<_> = read
            .layout
            .blocks
            .iter()
            .filter(|b| b.plain().contains(word))
            .collect();
        assert_eq!(found.len(), 1, "{word:?} is on the page once");
        (found[0].rect.x0 + found[0].rect.x1) / 2.0
    };
    // The first column is now on the right, the second on the left.
    assert!(
        middle_of("111") > 316.0,
        "first column at {}",
        middle_of("111")
    );
    assert!(
        middle_of("222") < 296.0,
        "second column at {}",
        middle_of("222")
    );
    // The left column goes as far to the right as it takes for its right edge to be where the right column's was.
    let edge_of = |word: &str| {
        let b = extracted
            .layout
            .blocks
            .iter()
            .find(|b| b.plain().contains(word));
        b.unwrap().rect.x1
    };
    let dx = edge_of("second") - edge_of("first");
    // The figures were not translated; they went with their column.
    let was = extracted
        .layout
        .blocks
        .iter()
        .find(|b| b.plain().contains("1234"))
        .unwrap()
        .rect;
    let figures = read
        .layout
        .blocks
        .iter()
        .find(|b| b.plain().contains("1234"))
        .unwrap()
        .rect;
    assert!((figures.x0 - was.x0 - dx).abs() < 0.5, "{figures:?}");
    assert!((figures.y0 - was.y0).abs() < 0.5, "{figures:?}");
    // So did the rule under it and the link over it.
    assert!(
        read.obstacles
            .iter()
            .any(|r| (r.x0 - 72.0 - dx).abs() < 0.5),
        "{:?}",
        read.obstacles
    );
    let link = back_pages[0]
        .dict
        .get(b"Annots".as_slice())
        .and_then(|a| a.as_array())
        .and_then(|a| a[0].as_dict())
        .and_then(|d| d.get(b"Rect".as_slice()))
        .and_then(celsus_pdf::object::numbers)
        .unwrap();
    assert!((link[0] - 118.0 - f64::from(dx)).abs() < 0.5, "{link:?}");
    // The headline lies across the columns and stays over them.
    let top = read
        .layout
        .blocks
        .iter()
        .find(|b| b.plain().contains("333"))
        .unwrap()
        .rect;
    assert!((top.x1 - edge_of("headline")).abs() < 1.0, "{top:?}");
}

/// A one-page PDF with a line of text and, under it, a picture with `bits` bits a pixel.
fn text_on_a_picture(bits: u8) -> Vec<u8> {
    let content = "q 400 0 0 30 72 690 cm /Im0 Do Q\nBT /F1 14 Tf 72 700 Td (The words of a scanned page) Tj ET";
    let objects = [
        "<< /Type /Catalog /Pages 2 0 R >>".to_string(),
        "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_string(),
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Resources << /Font << /F1 4 0 R >> /XObject << /Im0 6 0 R >> >> /Contents 5 0 R >>".to_string(),
        "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_string(),
        format!("<< /Length {} >>\nstream\n{content}\nendstream", content.len()),
        format!(
            "<< /Type /XObject /Subtype /Image /Width 8 /Height 1 /ColorSpace /DeviceGray /BitsPerComponent {bits} /Length {} >>\nstream\n{}\nendstream",
            if bits == 1 { 1 } else { 8 },
            if bits == 1 { "\u{0}" } else { "\u{0}\u{0}\u{0}\u{0}\u{0}\u{0}\u{0}\u{0}" }
        ),
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

/// Two lines, the first indented and ending short of the margin by as much: line for line they are centred
/// on one another. Among paragraphs that begin the same way they are a paragraph, and set from the left.
#[test]
fn a_short_indented_paragraph_is_not_taken_for_centred_lines() {
    let words = |n: usize| vec!["nn"; n].join(" ");
    let content = format!(
        "BT /F1 10 Tf 12 TL 86 700 Td ({}) Tj -14 -12 Td ({}) Tj T* ({}) Tj \
         14 -12 Td ({}) Tj -14 -12 Td ({}) Tj ET",
        words(24),
        words(25),
        words(9),
        words(23),
        words(25)
    );
    let doc = celsus_pdf::Document::load(page_pdf(&content, "")).unwrap();
    let pages = doc.pages().unwrap();
    let page = extract_page(&doc, &pages[0], 1).unwrap();
    let blocks = &page.layout.blocks;
    assert_eq!(
        blocks.iter().map(|b| b.lines.len()).collect::<Vec<_>>(),
        [3, 2]
    );
    let (first, second) = (&blocks[1].lines[0].rect, &blocks[1].lines[1].rect);
    assert!(
        ((first.x0 + first.x1) - (second.x0 + second.x1)).abs() < 4.0,
        "the two lines are centred on one another: {first:?} {second:?}"
    );
    assert_ne!(blocks[1].align, celsus_core::layout::Align::Center);
}

/// A title of two lines set flush right, the first line much longer: it is flush right, not a paragraph with
/// a hanging indent, and its translation ends where it ended.
#[test]
fn a_title_of_two_lines_set_flush_right_stays_flush_right() {
    let content = "BT /F1 28 Tf 136.8 700 Td (Building Quietly Reliable) Tj ET\n\
                   BT /F1 28 Tf 332.884 668 Td (Services) Tj ET";
    let doc = celsus_pdf::Document::load(page_pdf(content, "")).unwrap();
    let pages = doc.pages().unwrap();
    let page = extract_page(&doc, &pages[0], 1).unwrap();
    let blocks = &page.layout.blocks;
    assert_eq!(blocks.len(), 1, "{blocks:?}");
    let lines = &blocks[0].lines;
    assert!(
        (lines[0].rect.x1 - lines[1].rect.x1).abs() < 3.0,
        "the two lines end together: {lines:?}"
    );
    assert_eq!(blocks[0].align, celsus_core::layout::Align::Right);
}

/// A page with a paragraph of three lines and, under it, a figure drawn from `foot` up to 660 (in the
/// coordinates of the page, from its bottom), written once with a font of ours: a page whose text is seen when
/// the page is looked at, as the text of a real book is.
fn paragraph_over_a_figure(foot: u32, fonts: &FontSet, tr: &Language) -> Vec<u8> {
    let content = format!(
        "BT /F1 12 Tf 14 TL 72 700 Td (The first of the three lines of a paragraph that stands over a figure) Tj \
         T* (and goes on in a second line that is about as long as the first one) Tj \
         T* (and ends in a third line which is the last line of the paragraph.) Tj ET\n\
         0.8 w 100 {foot} 300 {} re S",
        660 - foot
    );
    let doc = celsus_pdf::Document::load(page_pdf(&content, "")).unwrap();
    let pages = doc.pages().unwrap();
    let extracted = extract_page(&doc, &pages[0], 1).unwrap();
    assert_eq!(extracted.layout.blocks.len(), 1);
    let text = "Bir şeklin üstünde duran üç satırlık bir paragrafın ilk satırı budur, ikinci satırı da \
                aşağı yukarı onun kadar uzundur ve paragraf üçüncü, yani son satırıyla burada biter.";
    let translations = BTreeMap::from([(extracted.layout.blocks[0].id.clone(), text.to_string())]);
    let mut writer = BookWriter::new(&doc, fonts);
    writer
        .translate_page(&pages[0], &extracted, &translations, tr)
        .unwrap();
    writer.finish().unwrap()
}

const TWICE_AS_LONG: &str = "Bir şeklin üstünde duran üç satırlık bir paragrafın ilk satırı budur, ikinci satırı \
    da aşağı yukarı onun kadar uzundur ve paragraf üçüncü satırıyla bitmez, çünkü çevirisi aslından çok daha \
    uzun tutmuştur: dördüncü, beşinci ve altıncı satırlar da gelir, onlara da sayfada yer açmak gerekir, yoksa \
    yazı okunamayacak kadar küçülür ve sayfanın düzeni bozulur.";

/// A translation that needs more lines than its original moves the figure under it down, when there is room
/// under the figure, and keeps its size. The figure is drawn once more in its new place, and its old place is
/// painted over.
#[test]
fn a_figure_moves_down_for_a_longer_translation() {
    let tr: Language = "tr".parse().unwrap();
    let Some(fonts) = fonts_for(&tr, TWICE_AS_LONG) else {
        return;
    };
    let doc = celsus_pdf::Document::load(paragraph_over_a_figure(560, &fonts, &tr)).unwrap();
    let pages = doc.pages().unwrap();
    let extracted = extract_page(&doc, &pages[0], 1).unwrap();
    let block = &extracted.layout.blocks[0];
    let (size, lines) = (block.style.font_size, block.lines.len());
    let figure_top = |page: &celsus_core::extract::ExtractedPage| {
        page.obstacles
            .iter()
            .filter(|o| o.width() > 250.0)
            .map(|o| o.y0)
            .fold(0.0, f32::max)
    };
    let was = figure_top(&extracted);
    let translations = BTreeMap::from([(block.id.clone(), TWICE_AS_LONG.to_string())]);
    let mut writer = BookWriter::new(&doc, &fonts);
    let report = writer
        .translate_page(&pages[0], &extracted, &translations, &tr)
        .unwrap();
    assert_eq!(report.replaced.len(), 1, "{:?}", report.kept);

    let back = celsus_pdf::Document::load(writer.finish().unwrap()).unwrap();
    let back_pages = back.pages().unwrap();
    let read = extract_page(&back, &back_pages[0], 1).unwrap();
    let now = &read.layout.blocks[0];
    assert!(now.lines.len() > lines, "{} lines", now.lines.len());
    assert!(
        (now.style.font_size - size).abs() < 0.05,
        "{}",
        now.style.font_size
    );
    let content =
        String::from_utf8_lossy(&read.content.data[&celsus_pdf::interp::Source::Page]).into_owned();
    assert!(content.contains("W n\n1 0 0 1 0 -"), "{content}");
    assert!(
        !content.contains("2 Tr"),
        "no pen for text that kept its size"
    );
    // The figure stands lower than it did, under the last line of the text.
    let moved = figure_top(&read) - was;
    assert!(moved > 14.0, "moved {moved}");
    assert!(now.rect.y1 < figure_top(&read), "{:?}", now.rect);
}

/// With no room under the figure it stays, and the text gets smaller instead: darker and with heavier strokes,
/// to read as well as it did.
#[test]
fn text_that_gets_smaller_is_drawn_heavier() {
    let tr: Language = "tr".parse().unwrap();
    let longer = &TWICE_AS_LONG[..TWICE_AS_LONG.find("onlara").unwrap()];
    let Some(fonts) = fonts_for(&tr, TWICE_AS_LONG) else {
        return;
    };
    let doc = celsus_pdf::Document::load(paragraph_over_a_figure(60, &fonts, &tr)).unwrap();
    let pages = doc.pages().unwrap();
    let extracted = extract_page(&doc, &pages[0], 1).unwrap();
    let block = &extracted.layout.blocks[0];
    let size = block.style.font_size;
    let translations = BTreeMap::from([(block.id.clone(), longer.trim().to_string())]);
    let mut writer = BookWriter::new(&doc, &fonts);
    let report = writer
        .translate_page(&pages[0], &extracted, &translations, &tr)
        .unwrap();
    assert_eq!(report.replaced.len(), 1, "{:?}", report.kept);

    let back = celsus_pdf::Document::load(writer.finish().unwrap()).unwrap();
    let back_pages = back.pages().unwrap();
    let read = extract_page(&back, &back_pages[0], 1).unwrap();
    assert!(read.layout.blocks[0].style.font_size < size - 0.3);
    let content =
        String::from_utf8_lossy(&read.content.data[&celsus_pdf::interp::Source::Page]).into_owned();
    assert!(!content.contains("W n\n1 0 0 1 0 -"), "nothing moved");
    assert!(
        content.contains(" RG ") && content.contains("2 Tr"),
        "{content}"
    );
}

/// The text laid on a scanned page says what the picture shows. It is read as the page's text and known to be
/// of the scan; text over any other picture is ordinary text.
#[test]
fn the_text_layer_of_a_scanned_page_is_read_as_its_text() {
    let read = |bits: u8| {
        let doc = celsus_pdf::Document::load(text_on_a_picture(bits)).unwrap();
        let pages = doc.pages().unwrap();
        let page = extract_page(&doc, &pages[0], 1).unwrap();
        let blocks: Vec<String> = page.layout.blocks.iter().map(|b| b.plain()).collect();
        (blocks, page.scans.len())
    };
    assert_eq!(
        read(1),
        (vec!["The words of a scanned page".to_string()], 1)
    );
    assert_eq!(
        read(8),
        (vec!["The words of a scanned page".to_string()], 0)
    );
}

/// The words of a scanned page are in its picture and stay there. The translation is written over a cover in
/// the white of the paper, and the old text layer goes, so that the page is searched in its new language.
#[test]
fn a_translation_on_a_scanned_page_covers_the_words_of_the_picture() {
    let tr: Language = "tr".parse().unwrap();
    let text = "Taranmış bir sayfanın sözcükleri";
    let Some(fonts) = fonts_for(&tr, text) else {
        return;
    };
    let doc = celsus_pdf::Document::load(text_on_a_picture(1)).unwrap();
    let pages = doc.pages().unwrap();
    let extracted = extract_page(&doc, &pages[0], 1).unwrap();
    let block = &extracted.layout.blocks[0];
    let was = block.rect;
    let translations = BTreeMap::from([(block.id.clone(), text.to_string())]);
    let mut writer = BookWriter::new(&doc, &fonts);
    let report = writer
        .translate_page(&pages[0], &extracted, &translations, &tr)
        .unwrap();
    assert_eq!(report.replaced.len(), 1, "{:?}", report.kept);
    let bytes = writer.finish().unwrap();

    let back = celsus_pdf::Document::load(bytes).unwrap();
    let back_pages = back.pages().unwrap();
    let read = extract_page(&back, &back_pages[0], 1).unwrap();
    let all: Vec<String> = read.layout.blocks.iter().map(|b| b.plain()).collect();
    assert_eq!(all, [text], "only the translation is the page's text now");
    // A white rectangle is painted where the line stood, before the new text and after the picture.
    let content =
        String::from_utf8_lossy(&read.content.data[&celsus_pdf::interp::Source::Page]).into_owned();
    let picture = content.find("/Im0 Do").expect("the picture is still drawn");
    let cover = content.find("1 1 1 rg").expect("a cover in white");
    let words = content.rfind("BT").expect("the new text");
    assert!(picture < cover && cover < words, "{content}");
    // The translation does not go lower than the line it replaces.
    let now = read.layout.blocks[0].rect;
    assert!(now.y1 <= was.y1 + 14.0 * 0.3, "{now:?} against {was:?}");
}
