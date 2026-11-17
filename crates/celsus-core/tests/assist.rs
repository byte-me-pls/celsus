//! What is handed out to an assistant translating a book, when some of its text was translated before: in the
//! same book added again, or because the pages are being translated anew.

use std::collections::{BTreeMap, BTreeSet, HashSet};

use celsus_core::Language;
use celsus_core::assist::{self, BookInfo, Fresh, Submitted};
use celsus_core::extract::extract_page;
use celsus_core::pipeline::cache_key;

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

/// The one block of a page of `book`, as an assistant gets it: its id and its text.
fn block(doc: &celsus_pdf::Document, page: usize) -> (String, String) {
    let pages = doc.pages().unwrap();
    let e = extract_page(doc, &pages[page - 1], page as u32).unwrap();
    let b = e.layout.blocks.iter().find(|b| !b.protected).unwrap();
    (b.id.0.clone(), b.source())
}

const PAGES: [&str; 3] = [
    "The first page of the book",
    "The second page of the book",
    "The third page of the book",
];

#[test]
fn pages_translated_before_are_ready_without_asking() {
    let to: Language = "tr".parse().unwrap();
    let doc = celsus_pdf::Document::load(book(&PAGES)).unwrap();
    let mut cache = BTreeMap::new();
    for p in [1, 2] {
        cache.insert(cache_key(&to, &block(&doc, p).1), format!("Sayfa {p}"));
    }
    let done = BTreeSet::new();
    let p = assist::pending(
        &doc,
        &BookInfo::default(),
        &done,
        &mut cache,
        &to,
        &[],
        4000,
        Fresh::none(),
    )
    .unwrap();
    assert_eq!(p.ready, vec![1, 2]);
    assert_eq!(p.blocks.iter().map(|b| b.page).collect::<Vec<_>>(), vec![3]);
    // Once written, they count as done, and the assistant is told where they came from.
    let mut p = p;
    p.count_ready(&BTreeSet::new());
    assert_eq!((p.pages_left, p.pages_done), (1, 2));
    assert_eq!(p.finished_from_earlier_translations, 2);
    assert!(p.note.contains("translate_again"));
    // The same book with every page known: nothing to ask, every page ready to be written.
    cache.insert(cache_key(&to, &block(&doc, 3).1), "Sayfa 3".into());
    let p = assist::pending(
        &doc,
        &BookInfo::default(),
        &done,
        &mut cache,
        &to,
        &[],
        4000,
        Fresh::none(),
    )
    .unwrap();
    assert!(p.blocks.is_empty());
    assert_eq!(p.ready, vec![1, 2, 3]);
}

#[test]
fn pages_translated_again_are_handed_out_until_they_are_given_anew() {
    let to: Language = "tr".parse().unwrap();
    let doc = celsus_pdf::Document::load(book(&PAGES)).unwrap();
    let mut cache = BTreeMap::new();
    for p in 1..=3 {
        cache.insert(cache_key(&to, &block(&doc, p).1), format!("Eski {p}"));
    }
    let done: BTreeSet<usize> = [1, 2, 3].into();
    let redo: BTreeSet<usize> = [2].into();
    let mut keys = HashSet::new();
    let fresh = Fresh {
        pages: &redo,
        keys: &keys,
    };
    let p = assist::pending(
        &doc,
        &BookInfo::default(),
        &done,
        &mut cache,
        &to,
        &[],
        4000,
        fresh,
    )
    .unwrap();
    assert_eq!(p.blocks.iter().map(|b| b.page).collect::<Vec<_>>(), vec![2]);
    assert_eq!((p.pages_left, p.pages_done), (1, 2));
    assert!(p.ready.is_empty());

    let (id, source) = block(&doc, 2);
    let given = [Submitted {
        page: 2,
        id,
        text: "Kitabın ikinci sayfası".into(),
    }];
    let a = assist::accept(&doc, &mut cache, &to, &given, false, fresh).unwrap();
    assert_eq!(a.complete, vec![2]);
    assert_eq!(a.fresh_keys, vec![cache_key(&to, &source)]);
    assert_eq!(cache[&cache_key(&to, &source)], "Kitabın ikinci sayfası");

    keys.extend(a.fresh_keys);
    let fresh = Fresh {
        pages: &redo,
        keys: &keys,
    };
    let p = assist::pending(
        &doc,
        &BookInfo::default(),
        &done,
        &mut cache,
        &to,
        &[],
        4000,
        fresh,
    )
    .unwrap();
    assert!(p.blocks.is_empty());
    assert_eq!(p.ready, vec![2]);
}
