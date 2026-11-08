//! Largest objects of a PDF and how big each is when written: `... --example sizes -- book.pdf`

use celsus_pdf::{Document, Object};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let path = std::env::args().nth(1).ok_or("usage: sizes book.pdf")?;
    let doc = Document::open(&path)?;
    let mut sizes = Vec::new();
    let mut by_kind: std::collections::BTreeMap<String, (usize, usize)> = Default::default();
    for id in doc.object_ids() {
        let Ok(obj) = doc.get(id) else { continue };
        let mut out = Vec::new();
        celsus_pdf::write::serialize(&obj, &mut out);
        let kind = match obj.as_ref() {
            Object::Stream(s) => format!(
                "stream {}",
                s.dict
                    .get(&b"Subtype"[..])
                    .or(s.dict.get(&b"Type"[..]))
                    .and_then(Object::as_name)
                    .map(|n| String::from_utf8_lossy(n).into_owned())
                    .unwrap_or_default()
            ),
            Object::Dict(_) => "dict".into(),
            Object::Array(_) => "array".into(),
            Object::String(_) => "string".into(),
            _ => "other".into(),
        };
        let e = by_kind.entry(kind.clone()).or_default();
        e.0 += 1;
        e.1 += out.len();
        sizes.push((out.len(), id.num, kind));
    }
    sizes.sort_by(|a, b| b.cmp(a));
    for (len, num, kind) in sizes.iter().take(5) {
        println!("obj {num}: {len} bytes ({kind})");
    }
    for (kind, (n, len)) in by_kind {
        println!("{kind}: {n} objects, {len} bytes");
    }
    Ok(())
}
