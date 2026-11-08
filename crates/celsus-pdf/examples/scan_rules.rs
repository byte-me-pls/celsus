//! List the pages with the most thin rules (likely tables): `cargo run -p celsus-pdf --example scan_rules -- book.pdf`

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let path = std::env::args()
        .nth(1)
        .ok_or("usage: scan_rules book.pdf")?;
    let doc = celsus_pdf::Document::open(&path)?;
    let mut scores = Vec::new();
    for (i, page) in doc.pages()?.iter().enumerate() {
        let c = celsus_pdf::interp::interpret(&doc, page)?;
        let thin = c.paths.iter().filter(|p| {
            let (w, h) = (p.bounds[2] - p.bounds[0], p.bounds[3] - p.bounds[1]);
            (w <= 2.5 && h > 6.0) || (h <= 2.5 && w > 6.0)
        });
        let vertical = thin
            .clone()
            .filter(|p| p.bounds[2] - p.bounds[0] <= 2.5)
            .count();
        scores.push((
            vertical * 3 + thin.count(),
            i + 1,
            c.runs.len(),
            c.images.len(),
        ));
    }
    scores.sort_by(|a, b| b.cmp(a));
    for (score, page, runs, images) in scores.iter().take(8) {
        println!("p{page}: score {score}, runs {runs}, images {images}");
    }
    Ok(())
}
