//! Deleting a book takes what was translated for it out of the cache, so that the same file added again is not
//! taken for translated; text that another book's translated pages hold stays.

use std::sync::Mutex;

use celsus_core::Language;
use celsus_core::pipeline::{Cache, keys_of};
use celsus_store::{assistant, db, library};

/// A PDF with one line of Helvetica text on each page.
fn book(pages: &[&str]) -> Vec<u8> {
    let n = pages.len();
    let kids: Vec<String> = (0..n).map(|i| format!("{} 0 R", 4 + 2 * i)).collect();
    let mut objects = vec![
        "<< /Type /Catalog /Pages 2 0 R >>".to_string(),
        format!("<< /Type /Pages /Kids [{}] /Count {n} >>", kids.join(" ")),
        "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_string(),
    ];
    for (i, text) in pages.iter().enumerate() {
        let content = format!("BT /F1 14 Tf 72 700 Td ({text}) Tj ET");
        objects.push(format!(
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Resources << /Font << /F1 3 0 R >> >> /Contents {} 0 R >>",
            5 + 2 * i
        ));
        objects.push(format!(
            "<< /Length {} >>\nstream\n{content}\nendstream",
            content.len()
        ));
    }
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

#[test]
fn a_deleted_book_takes_its_translations_and_leaves_those_of_other_books() {
    let dir = std::env::temp_dir().join(format!("celsus-forget-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let lib = dir.join("library");
    let conn = db::open(&dir.join("celsus.db")).unwrap();
    let to: Language = "tr".parse().unwrap();
    let mut ids = Vec::new();
    for (name, own) in [
        ("a.pdf", "Only in the first book"),
        ("b.pdf", "Only in the second book"),
    ] {
        let file = dir.join(name);
        std::fs::write(&file, book(&["A heading both books share", own])).unwrap();
        let b = library::add_book(&conn, &lib, &file).unwrap();
        // Both pages translated.
        let doc = celsus_pdf::Document::open(library::source_file(&lib, b.id)).unwrap();
        for key in keys_of(&doc, &to, &[1, 2]) {
            db::DbCache(&conn).put(&key, "çeviri");
        }
        for p in [1, 2] {
            db::mark_page(&conn, b.id, "tr", p, 0).unwrap();
        }
        ids.push(b.id);
    }
    let key = |text: &str| celsus_core::pipeline::cache_key(&to, text);
    let known = |c: &rusqlite::Connection, text: &str| db::DbCache(c).get(&key(text)).is_some();
    assert!(known(&conn, "Only in the first book") && known(&conn, "A heading both books share"));

    let db = Mutex::new(conn);
    let mut told = false;
    let gone = assistant::remove_book(&db, &lib, ids[0], || told = true).unwrap();
    let conn = db.into_inner().unwrap();
    assert!(told);
    assert_eq!(gone, 1);
    assert!(!known(&conn, "Only in the first book"));
    assert!(known(&conn, "A heading both books share"));
    assert!(known(&conn, "Only in the second book"));
    assert!(db::book(&conn, ids[0]).is_err());
    assert!(!library::source_file(&lib, ids[0]).exists());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn removing_a_translation_leaves_the_book_its_other_languages_and_other_books() {
    let dir = std::env::temp_dir().join(format!("celsus-untranslate-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let lib = dir.join("library");
    let conn = db::open(&dir.join("celsus.db")).unwrap();
    let mut ids = Vec::new();
    for (name, own) in [
        ("a.pdf", "Only in the first book"),
        ("b.pdf", "Only in the second book"),
    ] {
        let file = dir.join(name);
        std::fs::write(&file, book(&["A heading both books share", own])).unwrap();
        let b = library::add_book(&conn, &lib, &file).unwrap();
        let doc = celsus_pdf::Document::open(library::source_file(&lib, b.id)).unwrap();
        // The first book in Turkish and German, the second in Turkish.
        let langs: &[&str] = if ids.is_empty() {
            &["tr", "de"]
        } else {
            &["tr"]
        };
        for lang in langs {
            let to: Language = lang.parse().unwrap();
            for key in keys_of(&doc, &to, &[1, 2]) {
                db::DbCache(&conn).put(&key, "çeviri");
            }
            for p in [1, 2] {
                db::mark_page(&conn, b.id, lang, p, 0).unwrap();
            }
            std::fs::write(library::output_file(&lib, b.id, lang), b"%PDF").unwrap();
        }
        ids.push(b.id);
    }
    let known = |c: &rusqlite::Connection, lang: &str, text: &str| {
        let to: Language = lang.parse().unwrap();
        db::DbCache(c)
            .get(&celsus_core::pipeline::cache_key(&to, text))
            .is_some()
    };

    let db = Mutex::new(conn);
    let tr: Language = "tr".parse().unwrap();
    let gone = assistant::remove_translation(&db, &lib, ids[0], &tr).unwrap();
    let conn = db.into_inner().unwrap();
    assert_eq!(gone, 1);
    assert!(
        db::translated_pages(&conn, ids[0], "tr")
            .unwrap()
            .is_empty()
    );
    assert!(!library::output_file(&lib, ids[0], "tr").exists());
    assert!(!known(&conn, "tr", "Only in the first book"));
    // The shared heading is still on the second book's Turkish pages.
    assert!(known(&conn, "tr", "A heading both books share"));
    // The book, its German, and the other book are as they were.
    assert!(library::source_file(&lib, ids[0]).exists());
    assert_eq!(db::translated_pages(&conn, ids[0], "de").unwrap().len(), 2);
    assert!(known(&conn, "de", "Only in the first book"));
    assert_eq!(db::translated_pages(&conn, ids[1], "tr").unwrap().len(), 2);
    let _ = std::fs::remove_dir_all(&dir);
}
