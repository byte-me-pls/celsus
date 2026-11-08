//! Run block extraction over a whole book: `cargo run --release -p celsus-core --example layout_stats -- book.pdf`

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let path = std::env::args()
        .nth(1)
        .ok_or("usage: layout_stats book.pdf")?;
    let t = std::time::Instant::now();
    let doc = celsus_pdf::Document::open(&path)?;
    let pages = doc.pages()?;
    let (mut blocks, mut protected, mut chars, mut failed) = (0, 0, 0, 0);
    for (i, page) in pages.iter().enumerate() {
        match celsus_core::extract::extract_page(&doc, page, i as u32 + 1) {
            Ok(e) => {
                blocks += e.layout.blocks.len();
                protected += e.layout.blocks.iter().filter(|b| b.protected).count();
                chars += e
                    .layout
                    .blocks
                    .iter()
                    .filter(|b| !b.protected)
                    .map(|b| b.source().chars().count())
                    .sum::<usize>();
            }
            Err(e) => {
                failed += 1;
                eprintln!("page {}: {e}", i + 1);
            }
        }
    }
    println!(
        "{} pages, {blocks} blocks ({protected} protected), {chars} characters to translate, {failed} failed, {:?}",
        pages.len(),
        t.elapsed()
    );
    Ok(())
}
