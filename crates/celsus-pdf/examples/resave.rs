//! Load and save a PDF without changes, then check the copy reads the same:
//! `cargo run --release -p celsus-pdf --example resave -- in.pdf out.pdf`

use celsus_pdf::{Document, Page, interp, write::Saver};

fn text(d: &Document, p: &Page) -> String {
    interp::interpret(d, p)
        .map(|c| {
            c.runs
                .iter()
                .flat_map(|r| r.glyphs.iter().map(|g| g.text.clone()))
                .collect()
        })
        .unwrap_or_default()
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let (input, output) = (
        args.next().ok_or("missing in.pdf")?,
        args.next().ok_or("missing out.pdf")?,
    );
    let doc = Document::open(&input)?;
    let bytes = Saver::new(&doc).save()?;
    std::fs::write(&output, &bytes)?;
    let copy = Document::open(&output)?;
    let (a, b) = (doc.pages()?, copy.pages()?);
    let same = a
        .iter()
        .zip(&b)
        .filter(|(pa, pb)| text(&doc, pa) == text(&copy, pb))
        .count();
    println!(
        "{} -> {} bytes, pages {} / {}, identical text on {same} pages, copy repaired: {}",
        doc.data.len(),
        bytes.len(),
        a.len(),
        b.len(),
        copy.repaired
    );
    Ok(())
}
