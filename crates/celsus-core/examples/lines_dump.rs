//! One line per translatable block of two or more lines: page, rectangle, whether its lines are kept, first words.
//! Used to compare two builds on the same book: `lines_dump book.pdf > before.txt`.

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let path = std::env::args()
        .nth(1)
        .ok_or("usage: lines_dump book.pdf")?;
    let doc = celsus_pdf::Document::open(&path)?;
    for (i, page) in doc.pages()?.iter().enumerate() {
        let Ok(e) = celsus_core::extract::extract_page(&doc, page, i as u32 + 1) else {
            continue;
        };
        for b in e
            .layout
            .blocks
            .iter()
            .filter(|b| !b.protected && b.lines.len() >= 2)
        {
            let text: String = b.plain().replace('\n', " / ").chars().take(60).collect();
            println!(
                "{} [{:.0},{:.0} {:.0}x{:.0}] {} {} {text}",
                i + 1,
                b.rect.x0,
                b.rect.y0,
                b.rect.width(),
                b.rect.height(),
                b.lines.len(),
                if b.keeps_lines() { "LINES" } else { "flow" }
            );
        }
    }
    Ok(())
}
