//! What a translation run leaves behind when it cannot finish: a provider that goes away, a job that is stopped,
//! a block the model never answers properly.
//!
//! Like `scripts.rs`, these need the system's fonts and are skipped with a note where there are none.

use std::cell::Cell;
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};

use celsus_core::Language;
use celsus_core::extract::extract_page;
use celsus_core::fonts::FontSet;
use celsus_core::pipeline::{Options, Outcome, Progress, cache_key, translate_document};
use celsus_core::translate::{TranslateError, TranslationRequest, Translator, Usage};

const PAGES: [&str; 3] = [
    "The first page of the book",
    "The second page of the book",
    "The third page of the book",
];

fn fonts(lang: &Language) -> Option<FontSet> {
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
    if fonts.is_empty() {
        eprintln!("no fonts found, skipping");
        return None;
    }
    Some(fonts)
}

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

/// Answers the first `good` requests by putting "Sayfa: " in front of each block, then does `then` for ever.
struct Fake<'a> {
    good: usize,
    asked: Cell<usize>,
    then: fn() -> TranslateError,
    /// Set when the first request is answered, as if the user pressed Stop while it was on its way.
    stop: Option<&'a AtomicBool>,
}

impl Translator for Fake<'_> {
    fn name(&self) -> String {
        "fake".into()
    }
    fn translate(
        &self,
        request: &TranslationRequest,
    ) -> Result<(Vec<String>, Usage), TranslateError> {
        let n = self.asked.get();
        self.asked.set(n + 1);
        if n >= self.good {
            return Err((self.then)());
        }
        if let Some(stop) = self.stop {
            stop.store(true, Ordering::Relaxed);
        }
        let texts = request
            .blocks
            .iter()
            .map(|(_, t)| format!("Sayfa: {t}"))
            .collect();
        Ok((texts, Usage::default()))
    }
}

struct Run {
    outcome: Outcome,
    /// (page, blocks without a translation) of every page reported as done.
    done: Vec<(usize, usize)>,
    /// The text of each page of the file that was written.
    text: Vec<String>,
}

/// Translate the three-page book with page 3 already in the cache, as if an earlier run had done it.
fn run(provider: &Fake, cancel: &AtomicBool) -> Option<Run> {
    let to: Language = "tr".parse().unwrap();
    let fonts = fonts(&to)?;
    let doc = celsus_pdf::Document::load(book(&PAGES)).unwrap();
    let mut options = Options::new(to.clone());
    options.retry_secs = 0;
    let mut cache: BTreeMap<String, String> = BTreeMap::new();
    cache.insert(cache_key(&to, PAGES[2]), format!("Eski: {}", PAGES[2]));
    let mut done = Vec::new();
    let mut progress = |p: Progress| {
        if let Progress::PageDone { page, missing, .. } = p {
            done.push((page, missing));
        }
    };
    let (bytes, outcome) = translate_document(
        &doc,
        &fonts,
        provider,
        &options,
        &mut cache,
        &mut progress,
        cancel,
    )
    .unwrap();
    let back = celsus_pdf::Document::load(bytes).unwrap();
    let text = back
        .pages()
        .unwrap()
        .iter()
        .enumerate()
        .map(|(i, p)| {
            let e = extract_page(&back, p, i as u32 + 1).unwrap();
            e.layout
                .blocks
                .iter()
                .map(|b| b.plain())
                .collect::<Vec<_>>()
                .join(" ")
        })
        .collect();
    Some(Run {
        outcome,
        done,
        text,
    })
}

#[test]
fn a_provider_that_goes_away_keeps_the_finished_pages() {
    let provider = Fake {
        good: 1,
        asked: Cell::new(0),
        then: || TranslateError::Provider("connection refused".into()),
        stop: None,
    };
    let Some(run) = run(&provider, &AtomicBool::new(false)) else {
        return;
    };
    let why = run.outcome.failed.expect("the run reports why it stopped");
    assert!(why.contains("page 2") && why.contains("connection refused"), "{why}");
    assert!(!run.outcome.cancelled);
    // Page 1 was translated, page 2 could not be, and page 3 came from the earlier run.
    assert_eq!(run.done, [(1, 0), (3, 0)]);
    assert!(run.text[0].starts_with("Sayfa:"), "{:?}", run.text);
    assert_eq!(run.text[1], PAGES[1]);
    assert!(run.text[2].starts_with("Eski:"), "{:?}", run.text);
    // Nothing more is asked once the provider is known to be gone: the batch, then one block on its own,
    // three tries each.
    assert_eq!(provider.asked.get(), 1 + 3 + 3);
}

#[test]
fn a_refused_key_stops_at_once_and_keeps_earlier_pages() {
    let provider = Fake {
        good: 0,
        asked: Cell::new(0),
        then: || TranslateError::Fatal("401 invalid api key".into()),
        stop: None,
    };
    let Some(run) = run(&provider, &AtomicBool::new(false)) else {
        return;
    };
    assert!(run.outcome.failed.unwrap().contains("page 1"));
    assert_eq!(provider.asked.get(), 1);
    assert_eq!(run.done, [(3, 0)]);
    assert_eq!(run.text[0], PAGES[0]);
    assert!(run.text[2].starts_with("Eski:"), "{:?}", run.text);
}

#[test]
fn stopping_keeps_what_was_done_and_what_was_there_before() {
    let cancel = AtomicBool::new(false);
    let provider = Fake {
        good: 5,
        asked: Cell::new(0),
        then: || TranslateError::Provider("not reached".into()),
        stop: Some(&cancel),
    };
    let Some(run) = run(&provider, &cancel) else {
        return;
    };
    assert!(run.outcome.cancelled);
    assert_eq!(run.outcome.failed, None);
    assert_eq!(provider.asked.get(), 1);
    assert_eq!(run.done, [(1, 0), (3, 0)]);
    assert_eq!(run.text[1], PAGES[1]);
    assert!(run.text[2].starts_with("Eski:"), "{:?}", run.text);
}

#[test]
fn a_block_that_gets_no_answer_leaves_its_page_unfinished() {
    let provider = Fake {
        good: 0,
        asked: Cell::new(0),
        then: || TranslateError::BadAnswer("not JSON".into()),
        stop: None,
    };
    let Some(run) = run(&provider, &AtomicBool::new(false)) else {
        return;
    };
    // The run goes on (the provider is there, its answers are just unusable), but the pages say so.
    assert_eq!(run.outcome.failed, None);
    assert_eq!(run.done, [(1, 1), (2, 1), (3, 0)]);
    assert_eq!(run.text[0], PAGES[0]);
}
