//! Writing PDF objects and whole files.
//!
//! Saving rewrites the file from scratch with every live object, so replaced objects do not linger and the
//! result is no larger than it needs to be. Streams that were not changed are copied byte for byte, still encoded.

use std::collections::BTreeMap;
use std::io::Write;

use crate::Result;
use crate::document::Document;
use crate::lexer::{is_delimiter, is_whitespace};
use crate::object::{Dict, ObjRef, Object, Stream, get};

pub fn serialize(obj: &Object, out: &mut Vec<u8>) {
    match obj {
        Object::Null => out.extend_from_slice(b"null"),
        Object::Bool(b) => out.extend_from_slice(if *b { b"true" } else { b"false" }),
        Object::Int(i) => out.extend_from_slice(i.to_string().as_bytes()),
        Object::Real(r) => out.extend_from_slice(real(*r).as_bytes()),
        Object::String(s) => {
            out.push(b'<');
            for b in s {
                write!(out, "{b:02X}").unwrap();
            }
            out.push(b'>');
        }
        Object::Name(n) => name(n, out),
        Object::Array(a) => {
            out.push(b'[');
            for (i, item) in a.iter().enumerate() {
                if i > 0 {
                    out.push(b' ');
                }
                serialize(item, out);
            }
            out.push(b']');
        }
        Object::Dict(d) => dict(d, out),
        Object::Stream(s) => stream(s, out),
        Object::Ref(r) => write!(out, "{} {} R", r.num, r.generation).unwrap(),
    }
}

/// A number with no exponent and no trailing zeros, as PDF requires.
pub fn real(v: f64) -> String {
    if !v.is_finite() {
        return "0".into();
    }
    let s = format!("{v:.4}");
    let s = s.trim_end_matches('0').trim_end_matches('.');
    if s == "-0" || s.is_empty() {
        "0".into()
    } else {
        s.to_owned()
    }
}

pub fn name(n: &[u8], out: &mut Vec<u8>) {
    out.push(b'/');
    for &b in n {
        if b <= b' ' || b >= 0x7F || b == b'#' || is_delimiter(b) || is_whitespace(b) {
            write!(out, "#{b:02X}").unwrap();
        } else {
            out.push(b);
        }
    }
}

fn dict(d: &Dict, out: &mut Vec<u8>) {
    out.extend_from_slice(b"<<");
    for (k, v) in d {
        name(k, out);
        out.push(b' ');
        serialize(v, out);
    }
    out.extend_from_slice(b">>");
}

fn stream(s: &Stream, out: &mut Vec<u8>) {
    let mut d = s.dict.clone();
    d.insert(b"Length".to_vec(), Object::Int(s.data.len() as i64));
    dict(&d, out);
    out.extend_from_slice(b"\nstream\n");
    out.extend_from_slice(&s.data);
    out.extend_from_slice(b"\nendstream");
}

enum XrefEntry {
    Offset(usize, u16),
    /// Object stream number and index inside it.
    Packed(u32, usize),
}

fn ids_max(entries: &BTreeMap<u32, XrefEntry>, packed: &[(u32, Vec<u8>)]) -> u32 {
    entries
        .keys()
        .chain(packed.iter().map(|(n, _)| n))
        .copied()
        .max()
        .unwrap_or(0)
}

/// Changes to a document, saved as a new file.
#[derive(Clone)]
pub struct Saver<'d> {
    doc: &'d Document,
    changed: BTreeMap<u32, Object>,
    next: u32,
}

impl<'d> Saver<'d> {
    pub fn new(doc: &'d Document) -> Self {
        let next = doc.object_ids().iter().map(|r| r.num).max().unwrap_or(0) + 1;
        Saver {
            doc,
            changed: BTreeMap::new(),
            next,
        }
    }

    /// Add a new object and return its reference.
    pub fn add(&mut self, obj: Object) -> ObjRef {
        let id = ObjRef {
            num: self.next,
            generation: 0,
        };
        self.next += 1;
        self.changed.insert(id.num, obj);
        id
    }

    /// Reserve an object number to fill in later with [`Saver::set`].
    pub fn reserve(&mut self) -> ObjRef {
        self.add(Object::Null)
    }

    /// Replace an existing object.
    pub fn set(&mut self, id: ObjRef, obj: Object) {
        self.changed.insert(id.num, obj);
    }

    /// The current version of an object: changed here, or as in the document.
    pub fn get(&self, id: ObjRef) -> Result<Object> {
        match self.changed.get(&id.num) {
            Some(o) => Ok(o.clone()),
            None => Ok((*self.doc.get(id)?).clone()),
        }
    }

    pub fn save(&self) -> Result<Vec<u8>> {
        let mut out = Vec::with_capacity(self.doc.data.len() + 1024);
        out.extend_from_slice(b"%PDF-1.7\n%\xE2\xE3\xCF\xD3\n");
        let ids = self.reachable();

        // Streams (and objects with a generation number) are written directly; everything else is packed into
        // compressed object streams, which is what keeps large structure trees small.
        let mut entries: BTreeMap<u32, XrefEntry> = BTreeMap::new();
        let mut packed: Vec<(u32, Vec<u8>)> = Vec::new();
        for (num, generation) in ids {
            let obj = match self.changed.get(&num) {
                Some(o) => o.clone(),
                None => match self.doc.get(ObjRef { num, generation }) {
                    Ok(o) => (*o).clone(),
                    Err(_) => continue,
                },
            };
            if obj.is_null() {
                continue;
            }
            if let Some(d) = obj.as_dict()
                && matches!(
                    get(d, "Type").and_then(Object::as_name),
                    Some(b"XRef" | b"ObjStm")
                )
            {
                continue;
            }
            if matches!(obj, Object::Stream(_)) || generation != 0 {
                let obj = self.inline_lengths(obj);
                entries.insert(num, XrefEntry::Offset(out.len(), generation));
                writeln!(out, "{num} {generation} obj").unwrap();
                serialize(&obj, &mut out);
                out.extend_from_slice(b"\nendobj\n");
            } else {
                let mut body = Vec::new();
                serialize(&obj, &mut body);
                packed.push((num, body));
            }
        }

        let mut next = ids_max(&entries, &packed) + 1;
        for chunk in packed.chunks(100) {
            let stream_num = next;
            next += 1;
            let (mut header, mut body) = (Vec::new(), Vec::new());
            for (index, (num, data)) in chunk.iter().enumerate() {
                write!(header, "{num} {} ", body.len()).unwrap();
                body.extend_from_slice(data);
                body.push(b'\n');
                entries.insert(*num, XrefEntry::Packed(stream_num, index));
            }
            header.push(b'\n');
            let first = header.len();
            header.extend_from_slice(&body);
            let mut d = Dict::new();
            d.insert(b"Type".to_vec(), Object::Name(b"ObjStm".to_vec()));
            d.insert(b"N".to_vec(), Object::Int(chunk.len() as i64));
            d.insert(b"First".to_vec(), Object::Int(first as i64));
            d.insert(b"Filter".to_vec(), Object::Name(b"FlateDecode".to_vec()));
            entries.insert(stream_num, XrefEntry::Offset(out.len(), 0));
            writeln!(out, "{stream_num} 0 obj").unwrap();
            serialize(
                &Object::Stream(Stream {
                    dict: d,
                    data: crate::filters::deflate(&header),
                }),
                &mut out,
            );
            out.extend_from_slice(b"\nendobj\n");
        }

        // The cross-reference stream, which also carries the trailer keys.
        let xref_num = next;
        let size = xref_num + 1;
        let xref_offset = out.len();
        entries.insert(xref_num, XrefEntry::Offset(xref_offset, 0));
        let mut rows = Vec::with_capacity(size as usize * 7);
        for num in 0..size {
            let (kind, a, b): (u8, u32, u16) = match entries.get(&num) {
                Some(XrefEntry::Offset(o, g)) => (1, *o as u32, *g),
                Some(XrefEntry::Packed(s, i)) => (2, *s, *i as u16),
                None => (0, 0, if num == 0 { 65535 } else { 0 }),
            };
            rows.push(kind);
            rows.extend_from_slice(&a.to_be_bytes());
            rows.extend_from_slice(&b.to_be_bytes());
        }
        let mut d = Dict::new();
        d.insert(b"Type".to_vec(), Object::Name(b"XRef".to_vec()));
        d.insert(b"Size".to_vec(), Object::Int(i64::from(size)));
        d.insert(
            b"W".to_vec(),
            Object::Array(vec![Object::Int(1), Object::Int(4), Object::Int(2)]),
        );
        d.insert(b"Filter".to_vec(), Object::Name(b"FlateDecode".to_vec()));
        for key in ["Root", "Info", "ID"] {
            if let Some(v) = get(&self.doc.trailer, key) {
                d.insert(key.as_bytes().to_vec(), v.clone());
            }
        }
        writeln!(out, "{xref_num} 0 obj").unwrap();
        serialize(
            &Object::Stream(Stream {
                dict: d,
                data: crate::filters::deflate(&rows),
            }),
            &mut out,
        );
        write!(out, "\nendobj\nstartxref\n{xref_offset}\n%%EOF\n").unwrap();
        Ok(out)
    }

    /// Objects reachable from the trailer, with their generation numbers. Anything else (content streams that
    /// were replaced, leftovers of earlier edits) is not written.
    fn reachable(&self) -> BTreeMap<u32, u16> {
        let generations: BTreeMap<u32, u16> = self
            .doc
            .object_ids()
            .iter()
            .map(|r| (r.num, r.generation))
            .collect();
        let mut seen: BTreeMap<u32, u16> = BTreeMap::new();
        let mut stack: Vec<Object> = ["Root", "Info"]
            .iter()
            .filter_map(|k| get(&self.doc.trailer, k).cloned())
            .collect();
        while let Some(obj) = stack.pop() {
            match obj {
                Object::Ref(r) => {
                    if seen.contains_key(&r.num) {
                        continue;
                    }
                    let generation = generations.get(&r.num).copied().unwrap_or(0);
                    seen.insert(r.num, generation);
                    if let Ok(o) = self.get(ObjRef {
                        num: r.num,
                        generation,
                    }) {
                        stack.push(o);
                    }
                }
                Object::Array(a) => stack.extend(a),
                Object::Dict(d) => stack.extend(d.into_values()),
                Object::Stream(s) => stack.extend(s.dict.into_values()),
                _ => {}
            }
        }
        seen
    }

    /// `/Length` may point at another object; the writer always states it directly.
    fn inline_lengths(&self, obj: Object) -> Object {
        match obj {
            Object::Stream(mut s) => {
                s.dict.remove(&b"Length"[..]);
                Object::Stream(s)
            }
            other => other,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serializes_objects() {
        let mut d = Dict::new();
        d.insert(b"Type".to_vec(), Object::Name(b"Font".to_vec()));
        d.insert(b"Odd Name".to_vec(), Object::Real(1.5));
        d.insert(
            b"Kids".to_vec(),
            Object::Array(vec![
                Object::Ref(ObjRef {
                    num: 3,
                    generation: 0,
                }),
                Object::Int(-2),
            ]),
        );
        d.insert(b"T".to_vec(), Object::String(b"a)".to_vec()));
        let mut out = Vec::new();
        serialize(&Object::Dict(d), &mut out);
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "<</Kids [3 0 R -2]/Odd#20Name 1.5/T <6129>/Type /Font>>"
        );
        assert_eq!(real(0.30000001), "0.3");
        assert_eq!(real(-0.00001), "0");
        assert_eq!(real(12.0), "12");
    }
}
