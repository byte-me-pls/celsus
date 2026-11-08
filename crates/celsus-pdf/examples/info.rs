//! Print basic facts about a PDF: `cargo run -p celsus-pdf --example info -- book.pdf`

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let path = std::env::args().nth(1).ok_or("usage: info book.pdf")?;
    let start = std::time::Instant::now();
    let doc = celsus_pdf::Document::open(&path)?;
    let pages = doc.pages()?;
    let mut content = 0;
    for page in &pages {
        content += doc.page_content(page)?.len();
    }
    println!(
        "{} pages, first {:.0}x{:.0}pt, {} bytes of page content, repaired: {}, {:?}",
        pages.len(),
        pages.first().map_or(0.0, |p| p.width()),
        pages.first().map_or(0.0, |p| p.height()),
        content,
        doc.repaired,
        start.elapsed()
    );
    Ok(())
}
