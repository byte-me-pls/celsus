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
    let content = format!("BT /F1 14 Tf 72 700 Td ({text}) Tj ET");
    let objects = [
        "<< /Type /Catalog /Pages 2 0 R >>".to_string(),
        "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_string(),
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Resources << /Font << /F1 4 0 R >> >> /Contents 5 0 R >>".to_string(),
        "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_string(),
        format!("<< /Length {} >>\nstream\n{content}\nendstream", content.len()),
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
        let all: String = read.layout.blocks.iter().map(|b| b.source()).collect();
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
