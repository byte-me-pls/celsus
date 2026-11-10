//! Run block extraction over a whole book: `cargo run --release -p celsus-core --example layout_stats -- book.pdf [--top N]`
//!
//! With `--top N`, also lists the N busiest pages (most translatable blocks, plus pictures and protected blocks), to
//! pick hard pages for testing.

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let path = args
        .first()
        .ok_or("usage: layout_stats book.pdf [--top N]")?;
    let top: usize = match args.iter().position(|a| a == "--top") {
        Some(i) => args.get(i + 1).ok_or("--top needs a number")?.parse()?,
        None => 0,
    };
    let mut busy: Vec<(usize, usize, usize, usize)> = Vec::new();
    let t = std::time::Instant::now();
    let doc = celsus_pdf::Document::open(path)?;
    let pages = doc.pages()?;
    let (mut blocks, mut protected, mut chars, mut failed) = (0, 0, 0, 0);
    for (i, page) in pages.iter().enumerate() {
        match celsus_core::extract::extract_page(&doc, page, i as u32 + 1) {
            Ok(e) => {
                let own = e.layout.blocks.iter().filter(|b| !b.protected).count();
                let pictures = celsus_pdf::image::page_images(&doc, page).len();
                busy.push((own + pictures * 3, i + 1, own, pictures));
                blocks += e.layout.blocks.len();
                protected += e.layout.blocks.iter().filter(|b| b.protected).count();
                chars += e
                    .layout
                    .blocks
                    .iter()
                    .filter(|b| !b.protected)
                    .map(|b| b.plain().chars().count())
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
    busy.sort_by(|a, b| b.0.cmp(&a.0));
    for (_, page, own, pictures) in busy.into_iter().take(top) {
        println!("page {page}: {own} blocks to translate, {pictures} pictures");
    }
    Ok(())
}
