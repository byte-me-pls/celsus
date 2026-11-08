//! Print the operations of a page around the ones that draw some text:
//! `... --example ops_near -- file.pdf PAGE TEXT`

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let path = args.next().ok_or("missing file")?;
    let n: usize = args.next().ok_or("missing page")?.parse()?;
    let needle = args.next().ok_or("missing text")?;
    let doc = celsus_pdf::Document::open(&path)?;
    let page = &doc.pages()?[n - 1];
    let c = celsus_pdf::interp::interpret(&doc, page)?;
    let data = &c.data[&celsus_pdf::interp::Source::Page];
    let ops = &c.ops[&celsus_pdf::interp::Source::Page];
    for r in c
        .runs
        .iter()
        .filter(|r| r.source == celsus_pdf::interp::Source::Page)
    {
        let text: String = r.glyphs.iter().map(|g| g.text.as_str()).collect();
        if text == needle {
            for i in r.op.saturating_sub(6)..(r.op + 4).min(ops.len()) {
                println!(
                    "{i}: {}",
                    String::from_utf8_lossy(&data[ops[i].start..ops[i].end])
                );
            }
            println!(
                "--- run glyphs: {:?}",
                r.glyphs
                    .iter()
                    .map(|g| (&g.text, g.visible, g.origin))
                    .collect::<Vec<_>>()
            );
            break;
        }
    }
    Ok(())
}
