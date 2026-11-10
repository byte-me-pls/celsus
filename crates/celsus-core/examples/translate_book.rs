//! Translate pages of a PDF with a model and write the translated PDF:
//!
//!     cargo run --release -p celsus-core --example translate_book -- in.pdf out.pdf --to tr [--from en]
//!         [--pages 40-45] [--cache cache.json] [--title "..."] [--subject "..."] [--fonts DIR]
//!
//! The provider comes from `.env` or the environment: `CELSUS_API_URL` (an OpenAI-compatible `/v1` endpoint),
//! `CELSUS_MODEL`, and the key in `CELSUS_API_KEY`, `NVIDIA_API_KEY` or `OPENAI_API_KEY`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;

use celsus_core::Language;
use celsus_core::fonts::FontSet;
use celsus_core::pipeline::{Options, Progress, translate_document};
use celsus_core::provider::OpenAiCompatible;

fn load_env(path: &Path) {
    let Ok(text) = std::fs::read_to_string(path) else {
        return;
    };
    for line in text.lines().map(str::trim).filter(|l| !l.starts_with('#')) {
        if let Some((k, v)) = line.split_once('=')
            && std::env::var_os(k.trim()).is_none()
        {
            // SAFETY: called at start-up before any other thread exists.
            unsafe { std::env::set_var(k.trim(), v.trim().trim_matches('"')) };
        }
    }
}

fn env(names: &[&str]) -> Option<String> {
    names
        .iter()
        .find_map(|n| std::env::var(n).ok().filter(|v| !v.is_empty()))
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    load_env(Path::new(".env"));
    let mut args = std::env::args().skip(1);
    let (mut input, mut output, mut to) = (None, None, None);
    let mut options = Options::new("tr".parse()?);
    let mut cache_path = PathBuf::from("data/cache.json");
    let mut fonts_dir = PathBuf::from(if cfg!(windows) {
        r"C:\Windows\Fonts"
    } else {
        "/usr/share/fonts/truetype"
    });
    while let Some(a) = args.next() {
        let mut value = || args.next().ok_or(format!("{a} needs a value"));
        match a.as_str() {
            "--to" => to = Some(value()?.parse::<Language>()?),
            "--from" => options.from = Some(value()?.parse::<Language>()?),
            "--pages" => {
                let r = value()?;
                let (a, b): (usize, usize) = match r.split_once('-') {
                    Some((a, b)) => (a.parse()?, b.parse()?),
                    None => (r.parse()?, r.parse()?),
                };
                options.pages = (a..=b).collect();
            }
            "--cache" => cache_path = PathBuf::from(value()?),
            "--title" => options.book.title = value()?,
            "--subject" => options.book.subject = value()?,
            "--fonts" => fonts_dir = PathBuf::from(value()?),
            _ if input.is_none() => input = Some(PathBuf::from(&a)),
            _ if output.is_none() => output = Some(PathBuf::from(&a)),
            _ => return Err(format!("unexpected argument {a}").into()),
        }
    }
    let (input, output) = (
        input.ok_or("missing in.pdf")?,
        output.ok_or("missing out.pdf")?,
    );
    options.to = to.ok_or("missing --to LANGUAGE")?;
    let provider = OpenAiCompatible::new(
        env(&["CELSUS_API_URL"]).ok_or("set CELSUS_API_URL")?,
        env(&["CELSUS_API_KEY", "NVIDIA_API_KEY", "OPENAI_API_KEY"]),
        env(&["CELSUS_MODEL"]).ok_or("set CELSUS_MODEL")?,
    );
    let doc = celsus_pdf::Document::open(&input)?;
    let mut cache: BTreeMap<String, String> = std::fs::read(&cache_path)
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default();
    let started = std::time::Instant::now();
    let mut show = |p: Progress| match p {
        Progress::PageDone {
            page,
            translated,
            replaced,
            kept,
            missing,
        } => {
            println!(
                "page {page}: {translated} translated, {replaced} replaced, {kept} kept, {missing} missing"
            )
        }
        Progress::Waiting { seconds } => println!("  rate limited, waiting {seconds}s"),
        Progress::Note { message } => println!("  {message}"),
        _ => {}
    };
    let result = translate_document(
        &doc,
        &FontSet::for_language(&fonts_dir, &options.to),
        &provider,
        &options,
        &mut cache,
        &mut show,
        &AtomicBool::new(false),
    );
    if let Some(dir) = cache_path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(&cache_path, serde_json::to_vec_pretty(&cache)?)?;
    let (bytes, outcome) = result?;
    std::fs::write(&output, &bytes)?;
    if let Some(why) = &outcome.failed {
        println!("stopped early: {why}");
    }
    println!(
        "{} pages, {} blocks replaced, {} kept; {} requests, {} + {} tokens; {:.0}s; wrote {}",
        outcome.pages,
        outcome.replaced,
        outcome.kept,
        outcome.requests,
        outcome.usage.prompt_tokens,
        outcome.usage.completion_tokens,
        started.elapsed().as_secs_f32(),
        output.display()
    );
    Ok(())
}
