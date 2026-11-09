//! Show a sample of protected blocks and why: `cargo run --release -p celsus-core --example protected_sample -- book.pdf`

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let path = std::env::args()
        .nth(1)
        .ok_or("usage: protected_sample book.pdf")?;
    let doc = celsus_pdf::Document::open(&path)?;
    let mut counts = std::collections::BTreeMap::<String, usize>::new();
    let mut samples = std::collections::BTreeMap::<String, Vec<String>>::new();
    for (i, page) in doc.pages()?.iter().enumerate() {
        let e = celsus_core::extract::extract_page(&doc, page, i as u32 + 1)?;
        for b in e.layout.blocks.iter().filter(|b| b.protected) {
            let s = b.source();
            let letters = s.chars().filter(|c| c.is_alphabetic()).count();
            let why = if letters < 2 {
                "few letters"
            } else if b.style.monospace {
                "monospace"
            } else {
                "code-like or rotated"
            };
            *counts.entry(why.into()).or_default() += 1;
            let list = samples.entry(why.into()).or_default();
            if list.len() < 12 {
                list.push(s.chars().take(50).collect());
            }
        }
    }
    for (why, n) in counts {
        println!("{why}: {n}  e.g. {:?}", samples[&why]);
    }
    Ok(())
}
