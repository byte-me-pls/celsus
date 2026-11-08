//! Debugging aid: `cargo run -p celsus-pdf --example dump -- book.pdf PAGE`

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let path = args.next().ok_or("usage: dump book.pdf PAGE")?;
    let n: usize = args.next().ok_or("missing page")?.parse()?;
    let doc = celsus_pdf::Document::open(&path)?;
    let pages = doc.pages()?;
    let page = &pages[n - 1];
    let data = doc.page_content(page)?;
    let content = celsus_pdf::interp::interpret(&doc, page)?;
    println!(
        "content bytes {}, ops {:?}",
        data.len(),
        content
            .ops
            .iter()
            .map(|(k, v)| (format!("{k:?}"), v.len()))
            .collect::<Vec<_>>()
    );
    println!("contents entry: {:?}", page.dict.get(&b"Contents"[..]));
    println!(
        "runs {}, paths {}, images {}",
        content.runs.len(),
        content.paths.len(),
        content.images.len()
    );
    for r in content.runs.iter().take(12) {
        let text: String = r.glyphs.iter().map(|g| g.text.as_str()).collect();
        println!(
            "{:?} op{} {} {:.1}pt mode{} {:?} {:?}",
            r.source, r.op, r.font.name, r.size, r.render_mode, r.glyphs[0].bounds, text
        );
    }
    println!(
        "--- first 600 bytes of content:\n{}",
        String::from_utf8_lossy(&data[..data.len().min(600)])
    );
    Ok(())
}
