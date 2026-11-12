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
    run_with(provider, cancel, &[])
}

/// The same, with `settled` as the pages that were translated before and are only written again.
fn run_with(provider: &Fake, cancel: &AtomicBool, settled: &[usize]) -> Option<Run> {
    let to: Language = "tr".parse().unwrap();
    let fonts = fonts(&to)?;
    let doc = celsus_pdf::Document::load(book(&PAGES)).unwrap();
    let mut options = Options::new(to.clone());
    options.retry_secs = 0;
    options.settled = settled.to_vec();
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
    assert!(
        why.contains("page 2") && why.contains("connection refused"),
        "{why}"
    );
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
fn a_model_that_never_answers_is_not_waited_for_twice() {
    let provider = Fake {
        good: 0,
        asked: Cell::new(0),
        then: || TranslateError::NoAnswer { seconds: 180 },
        stop: None,
    };
    let Some(run) = run(&provider, &AtomicBool::new(false)) else {
        return;
    };
    let why = run
        .outcome
        .failed
        .expect("the run says the model was silent");
    assert!(why.contains("did not answer within 180 seconds"), "{why}");
    assert_eq!(provider.asked.get(), 1);
    assert_eq!(run.done, [(3, 0)]);
    assert_eq!(run.text[0], PAGES[0]);
}

#[test]
fn a_model_that_falls_silent_gets_one_more_try_and_then_the_run_stops() {
    let provider = Fake {
        good: 1,
        asked: Cell::new(0),
        then: || TranslateError::NoAnswer { seconds: 180 },
        stop: None,
    };
    let Some(run) = run(&provider, &AtomicBool::new(false)) else {
        return;
    };
    let why = run
        .outcome
        .failed
        .expect("the run says the model was silent");
    assert!(
        why.contains("page 2") && why.contains("did not answer"),
        "{why}"
    );
    // Page 1, then page 2 asked twice.
    assert_eq!(provider.asked.get(), 3);
    assert_eq!(run.done, [(1, 0), (3, 0)]);
}

#[test]
fn pages_translated_before_are_written_again_without_asking() {
    let provider = Fake {
        good: usize::MAX,
        asked: Cell::new(0),
        then: || TranslateError::Provider("never".into()),
        stop: None,
    };
    // Pages 2 and 3 were translated before. Page 3's translation is still kept, page 2's is gone.
    let Some(run) = run_with(&provider, &AtomicBool::new(false), &[2, 3]) else {
        return;
    };
    assert_eq!(run.outcome.failed, None);
    // Only page 1 was asked for: nothing is paid twice for the others.
    assert_eq!(provider.asked.get(), 1);
    assert!(run.text[0].starts_with("Sayfa:"), "{:?}", run.text);
    assert!(run.text[2].starts_with("Eski:"), "{:?}", run.text);
    // Page 2 stays as it was in the original and is reported as missing its text.
    assert_eq!(run.text[1], PAGES[1]);
    // The pages translated before are written first, then the new one.
    assert_eq!(run.done, [(2, 1), (3, 0), (1, 0)]);
}

#[test]
fn a_model_whose_answers_are_never_usable_stops_the_run() {
    // Eight pages, so that the answers run out of chances before the pages run out.
    let pages: Vec<String> = (1..=8)
        .map(|n| format!("Page number {n} of a longer book"))
        .collect();
    let texts: Vec<&str> = pages.iter().map(String::as_str).collect();
    let to: Language = "tr".parse().unwrap();
    let Some(fonts) = fonts(&to) else {
        return;
    };
    let provider = Fake {
        good: 0,
        asked: Cell::new(0),
        then: || TranslateError::BadAnswer("not JSON".into()),
        stop: None,
    };
    let doc = celsus_pdf::Document::load(book(&texts)).unwrap();
    let mut options = Options::new(to);
    options.retry_secs = 0;
    let mut cache: BTreeMap<String, String> = BTreeMap::new();
    let mut progress = |_: Progress| {};
    let (_, outcome) = translate_document(
        &doc,
        &fonts,
        &provider,
        &options,
        &mut cache,
        &mut progress,
        &AtomicBool::new(false),
    )
    .unwrap();
    let why = outcome
        .failed
        .expect("the run says the model is not up to it");
    assert!(why.contains("could not be used 8 times in a row"), "{why}");
    // Two requests a page (the page, then its one block alone) until the eighth unusable answer: not sixteen.
    assert_eq!(provider.asked.get(), 8);
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

#[test]
fn the_file_is_handed_out_while_a_long_translation_goes_on() {
    let to: Language = "tr".parse().unwrap();
    let Some(fonts) = fonts(&to) else {
        return;
    };
    let provider = Fake {
        good: usize::MAX,
        asked: Cell::new(0),
        then: || TranslateError::Provider("never".into()),
        stop: None,
    };
    let doc = celsus_pdf::Document::load(book(&PAGES)).unwrap();
    let mut options = Options::new(to);
    options.retry_secs = 0;
    // As often as a page is finished.
    options.written_every = 1;
    let mut cache: BTreeMap<String, String> = BTreeMap::new();
    let mut files: Vec<Vec<u8>> = Vec::new();
    let mut progress = |p: Progress| {
        if let Progress::Written { bytes } = p {
            files.push(bytes);
        }
        // Each page takes a moment, as a model's answer would.
        std::thread::sleep(std::time::Duration::from_millis(400));
    };
    let (last, outcome) = translate_document(
        &doc,
        &fonts,
        &provider,
        &options,
        &mut cache,
        &mut progress,
        &AtomicBool::new(false),
    )
    .unwrap();
    assert_eq!(outcome.failed, None);
    assert!(!files.is_empty(), "no file was handed out on the way");
    // A file handed out on the way opens, with the pages finished by then in it and the rest as they were.
    let text = |bytes: &[u8]| -> Vec<String> {
        let back = celsus_pdf::Document::load(bytes.to_vec()).unwrap();
        back.pages()
            .unwrap()
            .iter()
            .enumerate()
            .map(|(i, p)| {
                let e = extract_page(&back, p, i as u32 + 1).unwrap();
                let blocks: Vec<String> = e.layout.blocks.iter().map(|b| b.plain()).collect();
                blocks.join(" ")
            })
            .collect()
    };
    let early = text(&files[0]);
    assert!(early[0].starts_with("Sayfa:"), "{early:?}");
    assert_eq!(early[2], PAGES[2], "{early:?}");
    let end = text(&last);
    assert!(end.iter().all(|t| t.starts_with("Sayfa:")), "{end:?}");
}
