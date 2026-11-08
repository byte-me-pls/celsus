//! Print decoded page content lines containing a pattern:
//! `cargo run -p celsus-pdf --example grep_content -- file.pdf PAGE PATTERN [CONTEXT]`

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let path = args.next().ok_or("missing file")?;
    let n: usize = args.next().ok_or("missing page")?.parse()?;
    let pattern = args.next().ok_or("missing pattern")?;
    let context: usize = args.next().map(|c| c.parse()).transpose()?.unwrap_or(3);
    let doc = celsus_pdf::Document::open(&path)?;
    let page = &doc.pages()?[n - 1];
    let data = doc.page_content(page)?;
    let text = String::from_utf8_lossy(&data);
    let lines: Vec<&str> = text.lines().collect();
    for (i, l) in lines.iter().enumerate() {
        if l.contains(&pattern) {
            for (j, line) in lines
                .iter()
                .enumerate()
                .take((i + context + 1).min(lines.len()))
                .skip(i.saturating_sub(context))
            {
                println!("{j}: {}", line.chars().take(160).collect::<String>());
            }
            println!("--");
        }
    }
    Ok(())
}
