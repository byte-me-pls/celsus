//! Compare text found by celsus-pdf with Pdfium's, page by page, as character multisets (order and whitespace
//! ignored). `cargo run --release -p celsus-core --example compare_extract -- book.pdf [--pdfium DIR] [--show N]`

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Instant;

use pdfium_render::prelude::*;

fn counts(s: &str) -> HashMap<char, i64> {
    let mut m = HashMap::new();
    for c in s
        .chars()
        .filter(|c| !c.is_whitespace() && *c != '\u{ad}' && *c != '\u{2}' && *c != '-')
    {
        *m.entry(c).or_insert(0) += 1;
    }
    m
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let path = PathBuf::from(args.next().ok_or("missing book.pdf")?);
    let (mut pdfium_dir, mut show) = (None, None);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--pdfium" => pdfium_dir = args.next().map(PathBuf::from),
            "--show" => show = args.next().and_then(|n| n.parse::<usize>().ok()),
            _ => return Err(format!("unexpected argument {a}").into()),
        }
    }

    let t = Instant::now();
    let doc = celsus_pdf::Document::open(&path)?;
    let pages = doc.pages()?;
    let mut ours = Vec::new();
    for page in &pages {
        let content = celsus_pdf::interp::interpret(&doc, page)?;
        let text: String = content
            .runs
            .iter()
            .filter(|r| r.render_mode != 3 && r.render_mode != 7)
            .flat_map(|r| {
                r.glyphs
                    .iter()
                    .filter(|g| g.visible)
                    .map(|g| g.text.as_str())
            })
            .collect();
        ours.push(text);
    }
    let ours_time = t.elapsed();

    let t = Instant::now();
    let pdfium = Pdfium::new(Pdfium::bind_to_library(
        Pdfium::pdfium_platform_library_name_at_path(
            pdfium_dir
                .as_deref()
                .unwrap_or(std::path::Path::new("vendor/pdfium/bin")),
        ),
    )?);
    let document = pdfium.load_pdf_from_file(&path, None)?;
    let mut theirs = Vec::new();
    for page in document.pages().iter() {
        theirs.push(page.text()?.all());
    }
    let pdfium_time = t.elapsed();

    let (mut total, mut diff) = (0i64, 0i64);
    let mut worst: Vec<(f64, usize)> = Vec::new();
    for (i, (a, b)) in ours.iter().zip(&theirs).enumerate() {
        let (ca, cb) = (counts(a), counts(b));
        let n: i64 = cb.values().sum::<i64>().max(ca.values().sum());
        let d: i64 = ca
            .keys()
            .chain(cb.keys())
            .collect::<std::collections::HashSet<_>>()
            .into_iter()
            .map(|k| (ca.get(k).unwrap_or(&0) - cb.get(k).unwrap_or(&0)).abs())
            .sum();
        total += n;
        diff += d;
        if n > 0 {
            worst.push((d as f64 / n as f64, i + 1));
        }
    }
    worst.sort_by(|a, b| b.0.total_cmp(&a.0));
    println!("pages: ours {} / pdfium {}", ours.len(), theirs.len());
    println!("time: ours {ours_time:?} / pdfium {pdfium_time:?}");
    println!(
        "character agreement: {:.3}%",
        100.0 * (1.0 - diff as f64 / total.max(1) as f64)
    );
    println!(
        "worst pages: {:?}",
        worst
            .iter()
            .take(8)
            .map(|(r, p)| format!("p{p} {:.1}%", 100.0 * (1.0 - r)))
            .collect::<Vec<_>>()
    );
    if let Some(p) = show {
        println!(
            "--- ours p{p}:\n{}\n--- pdfium p{p}:\n{}",
            ours[p - 1],
            theirs[p - 1].replace("\r\n", " ")
        );
    }
    Ok(())
}
