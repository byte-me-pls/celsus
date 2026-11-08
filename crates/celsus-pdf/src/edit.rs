//! Taking glyphs out of a content stream without moving anything else.
//!
//! A removed glyph is replaced by a `TJ` adjustment of exactly its advance, so the text that follows it in the
//! same operation, and every later operation that relies on the text position, stays where it was.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::io::Write;

use crate::interp::{PageContent, Source, TextRun};
use crate::object::Object;
use crate::write::real;

/// New decoded content for every source that changed, with the given glyphs (`run -> glyph indices`) removed.
pub fn remove_glyphs(
    content: &PageContent,
    removals: &BTreeMap<usize, BTreeSet<usize>>,
) -> HashMap<Source, Vec<u8>> {
    edit(content, removals, &[])
}

/// New content to place at a byte offset of a source's original data.
#[derive(Debug, Clone)]
pub struct Insertion {
    pub source: Source,
    pub at: usize,
    pub data: Vec<u8>,
}

/// Remove glyphs and insert new content in one pass. Offsets refer to the original data.
pub fn edit(
    content: &PageContent,
    removals: &BTreeMap<usize, BTreeSet<usize>>,
    insertions: &[Insertion],
) -> HashMap<Source, Vec<u8>> {
    let mut edits: HashMap<Source, Vec<(usize, usize, Vec<u8>)>> = HashMap::new();
    for ins in insertions {
        edits
            .entry(ins.source)
            .or_default()
            .push((ins.at, ins.at, ins.data.clone()));
    }
    for (&run_index, glyphs) in removals {
        let Some(run) = content.runs.get(run_index) else {
            continue;
        };
        let Some(op) = content.ops.get(&run.source).and_then(|ops| ops.get(run.op)) else {
            continue;
        };
        if glyphs.is_empty() {
            continue;
        }
        let mut text = Vec::new();
        match op.operator.as_slice() {
            b"'" => text.extend_from_slice(b"T* "),
            b"\"" => {
                let num = |i: usize| op.operands.get(i).and_then(Object::as_f64).unwrap_or(0.0);
                write!(text, "{} Tw {} Tc T* ", real(num(0)), real(num(1))).unwrap();
            }
            _ => {}
        }
        text.extend_from_slice(&rewrite(run, glyphs));
        edits
            .entry(run.source)
            .or_default()
            .push((op.start, op.end, text));
    }
    let mut out = HashMap::new();
    for (source, mut list) in edits {
        let Some(data) = content.data.get(&source) else {
            continue;
        };
        // Stable: at one offset, insertions keep their order, and come before a replacement starting there.
        list.sort_by_key(|(start, end, _)| (*start, *end != *start));
        let mut result = Vec::with_capacity(data.len());
        let mut at = 0;
        for (start, end, replacement) in list {
            if start < at {
                continue;
            }
            result.extend_from_slice(&data[at..start]);
            result.extend_from_slice(&replacement);
            at = end;
        }
        result.extend_from_slice(&data[at..]);
        out.insert(source, result);
    }
    out
}

enum Item {
    Text(Vec<u8>),
    Shift(f64),
}

fn push_shift(items: &mut Vec<Item>, n: f64) {
    if n == 0.0 {
        return;
    }
    if let Some(Item::Shift(prev)) = items.last_mut() {
        *prev += n;
    } else {
        items.push(Item::Shift(n));
    }
}

/// `[<kept> -123 <kept>] TJ` for one run.
fn rewrite(run: &TextRun, removed: &BTreeSet<usize>) -> Vec<u8> {
    let mut items: Vec<Item> = Vec::new();
    let scale = if run.font.vertical {
        run.font_size
    } else {
        run.font_size * run.h_scale
    };
    for (i, g) in run.glyphs.iter().enumerate() {
        push_shift(&mut items, g.kern_before);
        if removed.contains(&i) {
            // A TJ number moves the position by -n/1000 of the font size (scaled horizontally by Tz).
            let n = if scale.abs() > 1e-9 {
                -g.advance / scale * 1000.0
            } else {
                0.0
            };
            push_shift(&mut items, n);
        } else if let Some(Item::Text(t)) = items.last_mut() {
            t.extend_from_slice(&g.code);
        } else {
            items.push(Item::Text(g.code.clone()));
        }
    }
    push_shift(&mut items, run.trailing_kern);

    let mut out = b"[".to_vec();
    for item in &items {
        match item {
            Item::Text(t) => {
                out.push(b'<');
                for b in t {
                    write!(out, "{b:02X}").unwrap();
                }
                out.push(b'>');
            }
            Item::Shift(n) => write!(out, " {} ", real(*n)).unwrap(),
        }
    }
    out.extend_from_slice(b"] TJ");
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::document::Document;

    /// A one-page PDF with a single Helvetica line, built in memory.
    fn sample() -> Vec<u8> {
        let content = b"BT /F1 10 Tf 72 700 Td (Hello world) Tj ET";
        let objects = [
            "<< /Type /Catalog /Pages 2 0 R >>".to_string(),
            "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_string(),
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Resources << /Font << /F1 4 0 R >> >> /Contents 5 0 R >>".to_string(),
            "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_string(),
            format!("<< /Length {} >>\nstream\n{}\nendstream", content.len(), String::from_utf8_lossy(content)),
        ];
        let mut pdf = b"%PDF-1.4\n".to_vec();
        let mut offsets = Vec::new();
        for (i, o) in objects.iter().enumerate() {
            offsets.push(pdf.len());
            pdf.extend_from_slice(format!("{} 0 obj\n{o}\nendobj\n", i + 1).as_bytes());
        }
        let xref = pdf.len();
        pdf.extend_from_slice(
            format!("xref\n0 {}\n0000000000 65535 f \n", objects.len() + 1).as_bytes(),
        );
        for o in offsets {
            pdf.extend_from_slice(format!("{o:010} 00000 n \n").as_bytes());
        }
        pdf.extend_from_slice(
            format!(
                "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n",
                objects.len() + 1
            )
            .as_bytes(),
        );
        pdf
    }

    #[test]
    fn removed_glyphs_keep_later_text_in_place() {
        let doc = Document::load(sample()).unwrap();
        let page = &doc.pages().unwrap()[0];
        let content = crate::interp::interpret(&doc, page).unwrap();
        let before: Vec<(String, f32)> = content.runs[0]
            .glyphs
            .iter()
            .map(|g| (g.text.clone(), g.origin.0))
            .collect();
        // Remove "Hello".
        let removals = BTreeMap::from([(0, (0..5).collect::<BTreeSet<_>>())]);
        let edited = remove_glyphs(&content, &removals);
        let data = &edited[&Source::Page];
        // Re-run the edited stream and compare positions of " world".
        let new_doc_runs = crate::interp::interpret_data(&doc, page, data.clone());
        let after: Vec<(String, f32)> = new_doc_runs.runs[0]
            .glyphs
            .iter()
            .map(|g| (g.text.clone(), g.origin.0))
            .collect();
        assert_eq!(after.len(), before.len() - 5);
        for (a, b) in after.iter().zip(&before[5..]) {
            assert_eq!(a.0, b.0);
            assert!((a.1 - b.1).abs() < 0.01, "{a:?} vs {b:?}");
        }
    }
}
