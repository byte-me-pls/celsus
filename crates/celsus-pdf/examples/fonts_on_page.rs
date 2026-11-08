//! Text per font on one page, to check written output: `... --example fonts_on_page -- file.pdf PAGE`

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let path = args.next().ok_or("missing file")?;
    let n: usize = args.next().ok_or("missing page")?.parse()?;
    let doc = celsus_pdf::Document::open(&path)?;
    let page = &doc.pages()?[n - 1];
    let c = celsus_pdf::interp::interpret(&doc, page)?;
    let mut by_font: std::collections::BTreeMap<String, (usize, String)> = Default::default();
    for r in &c.runs {
        let e = by_font
            .entry(format!(
                "{} /{}",
                r.font.name,
                String::from_utf8_lossy(&r.font_resource)
            ))
            .or_default();
        let visible: String = r
            .glyphs
            .iter()
            .filter(|g| g.visible)
            .map(|g| g.text.as_str())
            .collect();
        e.0 += visible.chars().count();
        if e.1.len() < 80 {
            e.1.push_str(&visible);
            e.1.push('|');
        }
    }
    for (font, (count, sample)) in by_font {
        println!(
            "{font}: {count} visible chars: {}",
            sample.chars().take(80).collect::<String>()
        );
    }
    let invisible: usize = c
        .runs
        .iter()
        .map(|r| r.glyphs.iter().filter(|g| !g.visible).count())
        .sum();
    println!("clipped glyphs: {invisible}");
    Ok(())
}
