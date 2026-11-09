//! Loading a PDF file: cross-reference data, indirect objects and the page tree.

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use crate::filters;
use crate::lexer::{Lexer, Token};
use crate::object::{Dict, ObjRef, Object, Stream, get, numbers};
use crate::{Error, Result};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Entry {
    /// Object at a byte offset in the file.
    Offset(usize),
    /// Object `index` inside the object stream `stream`.
    Compressed { stream: u32, index: usize },
}

/// A page with its inherited attributes resolved.
#[derive(Debug, Clone)]
pub struct Page {
    pub id: ObjRef,
    pub dict: Dict,
    pub resources: Dict,
    /// `[x0, y0, x1, y1]` of the crop box (or media box), normalised so x0 < x1 and y0 < y1.
    pub crop_box: [f64; 4],
    pub rotate: i32,
}

impl Page {
    pub fn width(&self) -> f64 {
        self.crop_box[2] - self.crop_box[0]
    }

    pub fn height(&self) -> f64 {
        self.crop_box[3] - self.crop_box[1]
    }
}

/// Object numbers at or above this are ignored. Tables are sized by the highest number, and a real book has far
/// fewer objects.
pub const MAX_OBJECTS: u32 = 1 << 22;

/// How deep loading one object may lead into loading others (a stream's `/Length`, an object stream).
const MAX_NESTING: usize = 24;

/// A decoded object stream and the offset of each object inside it.
type ObjStm = (Vec<u8>, Vec<(u32, usize)>);

pub struct Document {
    pub data: Vec<u8>,
    xref: HashMap<u32, (Entry, u16)>,
    pub trailer: Dict,
    cache: RefCell<HashMap<u32, Rc<Object>>>,
    /// Decoded object streams: their data and the byte offset of each object inside it.
    objstm: RefCell<HashMap<u32, Rc<ObjStm>>>,
    /// The file needed reconstruction because its cross-reference data was broken.
    pub repaired: bool,
    /// How many objects are being loaded inside one another right now.
    nesting: Cell<usize>,
    /// Bytes of the file parsed into objects, and bytes of decoded object streams kept. Objects in a sound file
    /// do not overlap, so the first stays below the file's size; a file whose entries all point into one large
    /// stream would otherwise be copied once per entry.
    parsed: Cell<usize>,
    unpacked: Cell<usize>,
}

impl Document {
    pub fn load(data: Vec<u8>) -> Result<Self> {
        let mut doc = Document {
            data,
            xref: HashMap::new(),
            trailer: Dict::new(),
            cache: RefCell::new(HashMap::new()),
            objstm: RefCell::new(HashMap::new()),
            repaired: false,
            nesting: Cell::new(0),
            parsed: Cell::new(0),
            unpacked: Cell::new(0),
        };
        let read = doc.read_xref_chain();
        if read.is_err()
            || doc.catalog().is_err()
            || doc.pages().map(|p| p.is_empty()).unwrap_or(true)
        {
            doc.reconstruct()?;
        }
        if get(&doc.trailer, "Encrypt").is_some() {
            return Err(Error::Encrypted);
        }
        Ok(doc)
    }

    pub fn open(path: impl AsRef<std::path::Path>) -> Result<Self> {
        Self::load(std::fs::read(path)?)
    }

    fn startxref(&self) -> Result<usize> {
        let tail_start = self.data.len().saturating_sub(2048);
        let tail = &self.data[tail_start..];
        let at = rfind(tail, b"startxref").ok_or(Error::Corrupt("no startxref".into()))?;
        let mut lexer = Lexer::at(tail, at + 9);
        match lexer.next_token()? {
            Some(Token::Int(o)) if o >= 0 && (o as usize) < self.data.len() => Ok(o as usize),
            _ => Err(Error::Corrupt("bad startxref".into())),
        }
    }

    fn read_xref_chain(&mut self) -> Result<()> {
        let mut next = Some(self.startxref()?);
        let mut seen = HashSet::new();
        let mut first = true;
        while let Some(offset) = next.take() {
            if !seen.insert(offset) {
                break;
            }
            let trailer = self.read_xref_section(offset)?;
            if let Some(stm) = get(&trailer, "XRefStm")
                .and_then(Object::as_i64)
                .and_then(|s| usize::try_from(s).ok())
            {
                let _ = self.read_xref_section(stm);
            }
            next = get(&trailer, "Prev")
                .and_then(Object::as_i64)
                .and_then(|p| usize::try_from(p).ok());
            if first {
                self.trailer = trailer;
                first = false;
            } else {
                for (k, v) in trailer {
                    self.trailer.entry(k).or_insert(v);
                }
            }
        }
        self.trailer.remove(&b"Prev"[..]);
        self.trailer.remove(&b"XRefStm"[..]);
        Ok(())
    }

    /// Read one xref table or xref stream; entries already known (from newer sections) win.
    fn read_xref_section(&mut self, offset: usize) -> Result<Dict> {
        if offset >= self.data.len() {
            return Err(Error::Corrupt("xref offset past the end".into()));
        }
        let mut lexer = Lexer::at(&self.data, offset);
        lexer.skip_whitespace();
        if self.data[lexer.pos..].starts_with(b"xref") {
            lexer.pos += 4;
            loop {
                match lexer.next_token()? {
                    Some(Token::Int(start)) => {
                        let Some(Token::Int(count)) = lexer.next_token()? else {
                            return Err(Error::Corrupt("xref subsection".into()));
                        };
                        for i in 0..count.max(0) {
                            let (
                                Some(Token::Int(off)),
                                Some(Token::Int(generation)),
                                Some(Token::Keyword(kind)),
                            ) = (
                                lexer.next_token()?,
                                lexer.next_token()?,
                                lexer.next_token()?,
                            )
                            else {
                                return Err(Error::Corrupt("xref entry".into()));
                            };
                            let Some(num) = start
                                .checked_add(i)
                                .and_then(|n| u32::try_from(n).ok())
                                .filter(|n| *n < MAX_OBJECTS)
                            else {
                                continue;
                            };
                            if kind == b"n" && off > 0 {
                                self.xref
                                    .entry(num)
                                    .or_insert((Entry::Offset(off as usize), generation as u16));
                            } else {
                                // Free entries still shadow older definitions of the same number.
                                self.xref.entry(num).or_insert((Entry::Offset(0), 0));
                            }
                        }
                    }
                    Some(Token::Keyword(k)) if k == b"trailer" => {
                        let trailer = lexer.parse_object(true)?;
                        return trailer
                            .as_dict()
                            .cloned()
                            .ok_or(Error::Corrupt("trailer".into()));
                    }
                    _ => return Err(Error::Corrupt("xref table".into())),
                }
            }
        }
        // A cross-reference stream.
        let (_, obj) = self.parse_indirect_at(offset)?;
        let Object::Stream(stream) = obj else {
            return Err(Error::Corrupt("xref stream".into()));
        };
        let data = filters::decode(&stream.dict, &stream.data)?;
        let w: Vec<usize> = get(&stream.dict, "W")
            .and_then(numbers)
            .ok_or(Error::Corrupt("xref /W".into()))?
            .into_iter()
            .map(|v| v as usize)
            .collect();
        if w.len() < 3 || w.iter().any(|&x| x > 8) {
            return Err(Error::Corrupt("xref /W".into()));
        }
        let size = get(&stream.dict, "Size")
            .and_then(Object::as_i64)
            .unwrap_or(0);
        let index = get(&stream.dict, "Index")
            .and_then(numbers)
            .unwrap_or_else(|| vec![0.0, size as f64]);
        let row = w[0] + w[1] + w[2];
        if row == 0 {
            return Err(Error::Corrupt("xref /W".into()));
        }
        let field = |bytes: &[u8]| bytes.iter().fold(0u64, |acc, &b| acc << 8 | u64::from(b));
        let mut pos = 0;
        for pair in index.chunks(2) {
            let (start, count) = (pair[0] as u32, pair.get(1).copied().unwrap_or(0.0) as u32);
            for i in 0..count {
                let Some(entry) = data.get(pos..pos + row) else {
                    break;
                };
                pos += row;
                let kind = if w[0] == 0 { 1 } else { field(&entry[..w[0]]) };
                let a = field(&entry[w[0]..w[0] + w[1]]);
                let b = field(&entry[w[0] + w[1]..]);
                let Some(num) = start.checked_add(i).filter(|n| *n < MAX_OBJECTS) else {
                    continue;
                };
                let value = match kind {
                    1 => (Entry::Offset(a as usize), b as u16),
                    2 => (
                        Entry::Compressed {
                            stream: a as u32,
                            index: b as usize,
                        },
                        0,
                    ),
                    _ => (Entry::Offset(0), 0),
                };
                self.xref.entry(num).or_insert(value);
            }
        }
        Ok(stream.dict)
    }

    /// Rebuild the cross-reference data by scanning the whole file for `n g obj`.
    fn reconstruct(&mut self) -> Result<()> {
        self.repaired = true;
        self.xref.clear();
        self.forget();
        let data = &self.data;
        let mut found: Vec<(u32, u16, usize)> = Vec::new();
        let mut i = 0;
        while let Some(rel) = find(&data[i..], b"obj") {
            let at = i + rel;
            i = at + 3;
            if data.get(at + 3).is_some_and(|b| {
                !crate::lexer::is_whitespace(*b) && !crate::lexer::is_delimiter(*b)
            }) {
                continue;
            }
            if let Some((num, generation, start)) = header_before(data, at)
                && num < MAX_OBJECTS
            {
                found.push((num, generation, start));
            }
        }
        let mut trailer = Dict::new();
        let mut pos = 0;
        while let Some(rel) = find(&data[pos..], b"trailer") {
            let at = pos + rel + 7;
            pos = at;
            if let Ok(Object::Dict(d)) = Lexer::at(data, at).parse_object(true)
                && get(&d, "Root").is_some()
            {
                trailer = d;
            }
        }
        for (num, generation, start) in found {
            self.xref.insert(num, (Entry::Offset(start), generation)); // later definitions win
        }
        // Objects inside object streams, and an xref stream that may hold the trailer keys.
        let ids: Vec<u32> = self.xref.keys().copied().collect();
        for num in ids {
            let Ok(obj) = self.get(ObjRef {
                num,
                generation: self.xref[&num].1,
            }) else {
                continue;
            };
            let Some(dict) = obj.as_dict() else { continue };
            match get(dict, "Type").and_then(Object::as_name) {
                Some(b"ObjStm") => {
                    if let Ok(stm) = self.object_stream(num) {
                        for (index, (inner, _)) in stm.1.iter().enumerate() {
                            if *inner >= MAX_OBJECTS {
                                continue;
                            }
                            self.xref
                                .entry(*inner)
                                .or_insert((Entry::Compressed { stream: num, index }, 0));
                        }
                    }
                }
                Some(b"XRef") if get(&trailer, "Root").is_none() => trailer = dict.clone(),
                Some(b"Catalog") if get(&trailer, "Root").is_none() => {
                    trailer.insert(b"Root".to_vec(), Object::Ref(ObjRef { num, generation: 0 }));
                }
                _ => {}
            }
        }
        self.cache.borrow_mut().clear();
        self.parsed.set(0);
        if get(&trailer, "Root").is_none() {
            return Err(Error::Corrupt("no document catalog".into()));
        }
        self.trailer = trailer;
        Ok(())
    }

    /// Drop everything loaded so far.
    fn forget(&self) {
        self.cache.borrow_mut().clear();
        self.objstm.borrow_mut().clear();
        self.parsed.set(0);
        self.unpacked.set(0);
    }

    /// Count `bytes` of the file as parsed; refuse once far more has been parsed than the file holds.
    fn spend(&self, bytes: usize) -> Result<()> {
        let total = self.parsed.get().saturating_add(bytes);
        let allowed = self
            .data
            .len()
            .saturating_add(self.unpacked.get())
            .saturating_mul(2)
            .saturating_add(1 << 20);
        // Kept even when refused, so that every later object fails at once instead of being scanned first.
        self.parsed.set(total);
        if total > allowed {
            return Err(Error::Corrupt("objects overlap one another".into()));
        }
        Ok(())
    }

    /// Parse `num generation obj ... endobj` at `offset`.
    fn parse_indirect_at(&self, offset: usize) -> Result<(ObjRef, Object)> {
        if offset >= self.data.len() {
            return Err(Error::Corrupt(format!("no object at {offset}")));
        }
        self.spend(0)?;
        let mut lexer = Lexer::at(&self.data, offset);
        let (Some(Token::Int(num)), Some(Token::Int(generation)), Some(Token::Keyword(k))) = (
            lexer.next_token()?,
            lexer.next_token()?,
            lexer.next_token()?,
        ) else {
            return Err(Error::Corrupt(format!("no object at {offset}")));
        };
        if k != b"obj" {
            return Err(Error::Corrupt(format!("no object at {offset}")));
        }
        let id = ObjRef {
            num: num as u32,
            generation: generation as u16,
        };
        let obj = lexer.parse_object(true)?;
        let Object::Dict(dict) = obj else {
            self.spend(lexer.pos.saturating_sub(offset))?;
            return Ok((id, obj));
        };
        let dict_end = lexer.pos;
        match lexer.next_token()? {
            Some(Token::Keyword(k)) if k == b"stream" => {}
            _ => {
                self.spend(dict_end.saturating_sub(offset))?;
                return Ok((id, Object::Dict(dict)));
            }
        }
        // The stream data starts after the end-of-line that follows `stream`.
        let mut start = lexer.pos;
        if self.data.get(start) == Some(&b'\r') {
            start += 1;
        }
        if self.data.get(start) == Some(&b'\n') {
            start += 1;
        }
        let declared = match get(&dict, "Length") {
            Some(Object::Int(n)) => usize::try_from(*n).ok(),
            Some(Object::Ref(r)) if r.num != id.num => self
                .get(*r)
                .ok()
                .and_then(|o| o.as_i64())
                .and_then(|n| usize::try_from(n).ok()),
            _ => None,
        };
        let valid = |len: usize| {
            let end = start.checked_add(len)?;
            let after = self.data.get(end..)?;
            let skip = after
                .iter()
                .take_while(|b| crate::lexer::is_whitespace(**b))
                .count();
            after[skip..].starts_with(b"endstream").then_some(end)
        };
        let end = match declared.and_then(valid) {
            Some(end) => end,
            None => {
                // Wrong /Length: find `endstream` and trim the end-of-line before it.
                let rel = find(&self.data[start..], b"endstream")
                    .ok_or(Error::Corrupt("unterminated stream".into()))?;
                let mut end = start + rel;
                if end > start && self.data[end - 1] == b'\n' {
                    end -= 1;
                }
                if end > start && self.data[end - 1] == b'\r' {
                    end -= 1;
                }
                end
            }
        };
        self.spend(end.saturating_sub(offset))?;
        Ok((
            id,
            Object::Stream(Stream {
                dict,
                data: self.data[start..end].to_vec(),
            }),
        ))
    }

    fn object_stream(&self, num: u32) -> Result<Rc<ObjStm>> {
        if let Some(s) = self.objstm.borrow().get(&num) {
            return Ok(s.clone());
        }
        let obj = self.get(ObjRef { num, generation: 0 })?;
        let stream = obj
            .as_stream()
            .ok_or(Error::Corrupt("object stream".into()))?;
        let data = filters::decode(&stream.dict, &stream.data)?;
        let unpacked = self.unpacked.get().saturating_add(data.len());
        if unpacked > filters::MAX_DECODED {
            return Err(Error::Corrupt("object streams are too large".into()));
        }
        self.unpacked.set(unpacked);
        let n = get(&stream.dict, "N")
            .and_then(Object::as_i64)
            .unwrap_or(0)
            .max(0) as usize;
        let first = get(&stream.dict, "First")
            .and_then(Object::as_i64)
            .unwrap_or(0)
            .max(0) as usize;
        let mut lexer = Lexer::new(&data);
        let mut offsets = Vec::new();
        for _ in 0..n {
            match (lexer.next_token()?, lexer.next_token()?) {
                (Some(Token::Int(obj)), Some(Token::Int(off))) => {
                    let (Ok(obj), Ok(off)) = (u32::try_from(obj), usize::try_from(off)) else {
                        continue;
                    };
                    offsets.push((obj, first.saturating_add(off)))
                }
                _ => break,
            }
        }
        let entry = Rc::new((data, offsets));
        self.objstm.borrow_mut().insert(num, entry.clone());
        Ok(entry)
    }

    /// Load an indirect object. Missing objects are `null`, as the specification requires.
    pub fn get(&self, id: ObjRef) -> Result<Rc<Object>> {
        if let Some(o) = self.cache.borrow().get(&id.num) {
            return Ok(o.clone());
        }
        // Loading an object can need another one (a stream's /Length, the object stream it is packed in), and
        // a file can make that a loop.
        if self.nesting.get() >= MAX_NESTING {
            return Err(Error::Corrupt(
                "objects refer to one another in a loop".into(),
            ));
        }
        self.nesting.set(self.nesting.get() + 1);
        let obj = self.load_object(id);
        self.nesting.set(self.nesting.get() - 1);
        let obj = Rc::new(obj?);
        self.cache.borrow_mut().insert(id.num, obj.clone());
        Ok(obj)
    }

    fn load_object(&self, id: ObjRef) -> Result<Object> {
        Ok(match self.xref.get(&id.num) {
            None | Some((Entry::Offset(0), _)) => Object::Null,
            Some((Entry::Offset(off), _)) => self.parse_indirect_at(*off)?.1,
            Some((Entry::Compressed { stream, index }, _)) => {
                if *stream == id.num {
                    return Err(Error::Corrupt("object stream contains itself".into()));
                }
                let stm = self.object_stream(*stream)?;
                let offset = stm
                    .1
                    .get(*index)
                    .filter(|(n, _)| *n == id.num)
                    .or_else(|| stm.1.iter().find(|(n, _)| *n == id.num))
                    .map(|(_, o)| *o)
                    .filter(|o| *o < stm.0.len())
                    .ok_or(Error::Corrupt("object missing from object stream".into()))?;
                let mut lexer = Lexer::at(&stm.0, offset);
                let obj = lexer.parse_object(true)?;
                self.spend(lexer.pos.saturating_sub(offset))?;
                obj
            }
        })
    }

    /// Every object the cross-reference data lists as in use.
    pub fn object_ids(&self) -> Vec<ObjRef> {
        let mut ids: Vec<ObjRef> = self
            .xref
            .iter()
            .filter(|(_, (e, _))| *e != Entry::Offset(0))
            .map(|(&num, &(_, generation))| ObjRef { num, generation })
            .collect();
        ids.sort();
        ids
    }

    /// Follow references until a direct object.
    pub fn resolve(&self, obj: &Object) -> Result<Rc<Object>> {
        let mut current = Rc::new(obj.clone());
        for _ in 0..32 {
            match current.as_ref() {
                Object::Ref(r) => current = self.get(*r)?,
                _ => return Ok(current),
            }
        }
        Err(Error::Corrupt("reference loop".into()))
    }

    /// Resolve `dict[key]`.
    pub fn get_in(&self, dict: &Dict, key: &str) -> Option<Rc<Object>> {
        get(dict, key)
            .and_then(|o| self.resolve(o).ok())
            .filter(|o| !o.is_null())
    }

    /// Decode a stream's data.
    pub fn stream_data(&self, stream: &Stream) -> Result<Vec<u8>> {
        // /Length or /DecodeParms can themselves be references.
        let mut dict = stream.dict.clone();
        for key in ["Filter", "DecodeParms"] {
            if let Some(v) = self.get_in(&stream.dict, key) {
                dict.insert(key.as_bytes().to_vec(), (*v).clone());
            }
        }
        filters::decode(&dict, &stream.data)
    }

    pub fn catalog(&self) -> Result<Rc<Object>> {
        let root = get(&self.trailer, "Root").ok_or(Error::Corrupt("no /Root".into()))?;
        let cat = self.resolve(root)?;
        if cat.as_dict().is_none() {
            return Err(Error::Corrupt("catalog is not a dictionary".into()));
        }
        Ok(cat)
    }

    /// All pages in order, with inherited resources, boxes and rotation applied.
    pub fn pages(&self) -> Result<Vec<Page>> {
        let catalog = self.catalog()?;
        let root = get(catalog.as_dict().unwrap(), "Pages")
            .ok_or(Error::Corrupt("no page tree".into()))?;
        let mut out = Vec::new();
        let mut seen = HashSet::new();
        self.walk_pages(root, &Inherited::default(), &mut out, &mut seen, 0)?;
        Ok(out)
    }

    fn walk_pages(
        &self,
        node: &Object,
        inherited: &Inherited,
        out: &mut Vec<Page>,
        seen: &mut HashSet<u32>,
        depth: usize,
    ) -> Result<()> {
        let Some(id) = node.as_ref() else {
            return Ok(());
        };
        if depth > 64 || !seen.insert(id.num) {
            return Ok(());
        }
        let obj = self.get(id)?;
        let Some(dict) = obj.as_dict() else {
            return Ok(());
        };
        let mut here = inherited.clone();
        if let Some(r) = self
            .get_in(dict, "Resources")
            .and_then(|r| r.as_dict().cloned())
        {
            here.resources = Some(r);
        }
        for (key, slot) in [
            ("MediaBox", &mut here.media_box),
            ("CropBox", &mut here.crop_box),
        ] {
            if let Some(b) = self
                .get_in(dict, key)
                .and_then(|b| numbers(&b))
                .filter(|b| b.len() == 4)
            {
                *slot = Some([b[0], b[1], b[2], b[3]]);
            }
        }
        if let Some(r) = self.get_in(dict, "Rotate").and_then(|r| r.as_i64()) {
            here.rotate = r as i32;
        }
        let kids = self.get_in(dict, "Kids");
        let is_page =
            get(dict, "Type").and_then(Object::as_name) == Some(b"Page") || kids.is_none();
        if is_page {
            let media = here.media_box.unwrap_or([0.0, 0.0, 612.0, 792.0]);
            let mut b = here.crop_box.unwrap_or(media);
            // The crop box is clipped to the media box.
            b = [
                b[0].max(media[0].min(media[2])),
                b[1].max(media[1].min(media[3])),
                b[2].min(media[2].max(media[0])),
                b[3].min(media[3].max(media[1])),
            ];
            let crop_box = [
                b[0].min(b[2]),
                b[1].min(b[3]),
                b[0].max(b[2]),
                b[1].max(b[3]),
            ];
            out.push(Page {
                id,
                dict: dict.clone(),
                resources: here.resources.clone().unwrap_or_default(),
                crop_box,
                rotate: here.rotate.rem_euclid(360),
            });
            return Ok(());
        }
        if let Some(kids) = kids {
            for kid in kids.as_array().unwrap_or_default() {
                self.walk_pages(kid, &here, out, seen, depth + 1)?;
            }
        }
        Ok(())
    }

    /// The decoded content of a page: its content streams joined in order.
    pub fn page_content(&self, page: &Page) -> Result<Vec<u8>> {
        let Some(contents) = get(&page.dict, "Contents") else {
            return Ok(Vec::new());
        };
        let contents = self.resolve(contents)?;
        let parts: Vec<Object> = match contents.as_ref() {
            Object::Array(a) => a.clone(),
            other => vec![other.clone()],
        };
        let mut out = Vec::new();
        for part in parts {
            let obj = self.resolve(&part)?;
            if let Some(stream) = obj.as_stream() {
                match self.stream_data(stream) {
                    Ok(data) => {
                        if out.len() + data.len() > filters::MAX_DECODED {
                            return Err(Error::Corrupt("a page's content is too large".into()));
                        }
                        out.extend_from_slice(&data)
                    }
                    Err(Error::Corrupt(_)) => continue,
                    Err(e) => return Err(e),
                }
                out.push(b'\n');
            }
        }
        Ok(out)
    }
}

#[derive(Debug, Clone, Default)]
struct Inherited {
    resources: Option<Dict>,
    media_box: Option<[f64; 4]>,
    crop_box: Option<[f64; 4]>,
    rotate: i32,
}

pub(crate) fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

fn rfind(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).rposition(|w| w == needle)
}

/// For an `obj` keyword at `at`, find the `num generation` that precede it and the offset where `num` starts.
fn header_before(data: &[u8], at: usize) -> Option<(u32, u16, usize)> {
    let mut i = at;
    let mut fields = [(0usize, 0usize); 2];
    for slot in fields.iter_mut().rev() {
        while i > 0 && crate::lexer::is_whitespace(data[i - 1]) {
            i -= 1;
        }
        let end = i;
        while i > 0 && data[i - 1].is_ascii_digit() {
            i -= 1;
        }
        if i == end {
            return None;
        }
        *slot = (i, end);
    }
    if i > 0
        && !crate::lexer::is_whitespace(data[i - 1])
        && !crate::lexer::is_delimiter(data[i - 1])
    {
        return None;
    }
    let num: u32 = std::str::from_utf8(&data[fields[0].0..fields[0].1])
        .ok()?
        .parse()
        .ok()?;
    let generation: u16 = std::str::from_utf8(&data[fields[1].0..fields[1].1])
        .ok()?
        .parse()
        .ok()?;
    Some((num, generation, fields[0].0))
}
