//! Tokenizer and object parser for PDF syntax, shared by the file reader and the content stream interpreter.

use crate::object::{Dict, ObjRef, Object};
use crate::{Error, Result};

#[derive(Debug, Clone, PartialEq)]
pub enum Token {
    Int(i64),
    Real(f64),
    String(Vec<u8>),
    Name(Vec<u8>),
    ArrayStart,
    ArrayEnd,
    DictStart,
    DictEnd,
    /// Any bare word: `obj`, `stream`, `R`, `true`, content stream operators, ...
    Keyword(Vec<u8>),
}

pub fn is_whitespace(b: u8) -> bool {
    matches!(b, 0 | 9 | 10 | 12 | 13 | 32)
}

pub fn is_delimiter(b: u8) -> bool {
    matches!(
        b,
        b'(' | b')' | b'<' | b'>' | b'[' | b']' | b'{' | b'}' | b'/' | b'%'
    )
}

fn is_regular(b: u8) -> bool {
    !is_whitespace(b) && !is_delimiter(b)
}

pub struct Lexer<'a> {
    pub data: &'a [u8],
    pub pos: usize,
}

impl<'a> Lexer<'a> {
    pub fn new(data: &'a [u8]) -> Self {
        Self { data, pos: 0 }
    }

    pub fn at(data: &'a [u8], pos: usize) -> Self {
        Self { data, pos }
    }

    pub fn peek_byte(&self) -> Option<u8> {
        self.data.get(self.pos).copied()
    }

    pub fn skip_whitespace(&mut self) {
        while let Some(b) = self.peek_byte() {
            if is_whitespace(b) {
                self.pos += 1;
            } else if b == b'%' {
                while let Some(b) = self.peek_byte() {
                    if b == b'\n' || b == b'\r' {
                        break;
                    }
                    self.pos += 1;
                }
            } else {
                break;
            }
        }
    }

    /// The next token, or `None` at the end of the data.
    pub fn next_token(&mut self) -> Result<Option<Token>> {
        self.skip_whitespace();
        let Some(b) = self.peek_byte() else {
            return Ok(None);
        };
        let token = match b {
            b'[' => {
                self.pos += 1;
                Token::ArrayStart
            }
            b']' => {
                self.pos += 1;
                Token::ArrayEnd
            }
            b'<' if self.data.get(self.pos + 1) == Some(&b'<') => {
                self.pos += 2;
                Token::DictStart
            }
            b'>' if self.data.get(self.pos + 1) == Some(&b'>') => {
                self.pos += 2;
                Token::DictEnd
            }
            b'<' => Token::String(self.hex_string()),
            b'(' => Token::String(self.literal_string()),
            b'/' => Token::Name(self.name()),
            b'{' | b'}' | b')' | b'>' => {
                // Stray delimiters (PostScript calculator functions, broken files): treat as keywords.
                self.pos += 1;
                Token::Keyword(vec![b])
            }
            b'+' | b'-' | b'.' | b'0'..=b'9' => self.number_or_keyword(),
            _ => {
                let start = self.pos;
                while self.peek_byte().is_some_and(is_regular) {
                    self.pos += 1;
                }
                Token::Keyword(self.data[start..self.pos].to_vec())
            }
        };
        Ok(Some(token))
    }

    fn number_or_keyword(&mut self) -> Token {
        let start = self.pos;
        while self.peek_byte().is_some_and(is_regular) {
            self.pos += 1;
        }
        let word = &self.data[start..self.pos];
        parse_number(word).unwrap_or_else(|| Token::Keyword(word.to_vec()))
    }

    fn name(&mut self) -> Vec<u8> {
        self.pos += 1; // '/'
        let mut out = Vec::new();
        while let Some(b) = self.peek_byte() {
            if !is_regular(b) {
                break;
            }
            self.pos += 1;
            if b == b'#' {
                let hex = self.data.get(self.pos..self.pos + 2).and_then(|h| {
                    std::str::from_utf8(h)
                        .ok()
                        .and_then(|s| u8::from_str_radix(s, 16).ok())
                });
                if let Some(v) = hex {
                    out.push(v);
                    self.pos += 2;
                    continue;
                }
            }
            out.push(b);
        }
        out
    }

    fn hex_string(&mut self) -> Vec<u8> {
        self.pos += 1; // '<'
        let mut out = Vec::new();
        let mut high: Option<u8> = None;
        while let Some(b) = self.peek_byte() {
            self.pos += 1;
            if b == b'>' {
                break;
            }
            let v = match b {
                b'0'..=b'9' => b - b'0',
                b'a'..=b'f' => b - b'a' + 10,
                b'A'..=b'F' => b - b'A' + 10,
                _ => continue,
            };
            match high.take() {
                Some(h) => out.push(h << 4 | v),
                None => high = Some(v),
            }
        }
        if let Some(h) = high {
            out.push(h << 4);
        }
        out
    }

    fn literal_string(&mut self) -> Vec<u8> {
        self.pos += 1; // '('
        let mut out = Vec::new();
        let mut depth = 1;
        while let Some(b) = self.peek_byte() {
            self.pos += 1;
            match b {
                b'(' => {
                    depth += 1;
                    out.push(b);
                }
                b')' => {
                    depth -= 1;
                    if depth == 0 {
                        break;
                    }
                    out.push(b);
                }
                b'\\' => {
                    let Some(e) = self.peek_byte() else { break };
                    self.pos += 1;
                    match e {
                        b'n' => out.push(b'\n'),
                        b'r' => out.push(b'\r'),
                        b't' => out.push(b'\t'),
                        b'b' => out.push(8),
                        b'f' => out.push(12),
                        b'\r' => {
                            if self.peek_byte() == Some(b'\n') {
                                self.pos += 1;
                            }
                        }
                        b'\n' => {}
                        b'0'..=b'7' => {
                            let mut v = u32::from(e - b'0');
                            for _ in 0..2 {
                                match self.peek_byte() {
                                    Some(d @ b'0'..=b'7') => {
                                        v = v * 8 + u32::from(d - b'0');
                                        self.pos += 1;
                                    }
                                    _ => break,
                                }
                            }
                            out.push(v as u8);
                        }
                        other => out.push(other),
                    }
                }
                b'\r' => {
                    // An end of line inside a string is always a single LF.
                    if self.peek_byte() == Some(b'\n') {
                        self.pos += 1;
                    }
                    out.push(b'\n');
                }
                _ => out.push(b),
            }
        }
        out
    }

    /// Parse one object. With `refs`, `num generation R` is read as a reference (file syntax); content streams have none.
    pub fn parse_object(&mut self, refs: bool) -> Result<Object> {
        let token = self
            .next_token()?
            .ok_or(Error::Syntax("unexpected end of data", self.pos))?;
        self.object_from(token, refs, 0)
    }

    fn object_from(&mut self, token: Token, refs: bool, depth: usize) -> Result<Object> {
        if depth > 256 {
            return Err(Error::Syntax("nesting too deep", self.pos));
        }
        Ok(match token {
            Token::Int(i) => {
                if refs && i >= 0 {
                    let save = self.pos;
                    if let Ok(Some(Token::Int(generation))) = self.next_token()
                        && let Ok(Some(Token::Keyword(k))) = self.next_token()
                        && k == b"R"
                        && (0..=65535).contains(&generation)
                    {
                        return Ok(Object::Ref(ObjRef {
                            num: i as u32,
                            generation: generation as u16,
                        }));
                    }
                    self.pos = save;
                }
                Object::Int(i)
            }
            Token::Real(r) => Object::Real(r),
            Token::String(s) => Object::String(s),
            Token::Name(n) => Object::Name(n),
            Token::ArrayStart => {
                let mut items = Vec::new();
                loop {
                    match self.next_token()? {
                        None => break,
                        Some(Token::ArrayEnd) => break,
                        Some(t) => items.push(self.object_from(t, refs, depth + 1)?),
                    }
                }
                Object::Array(items)
            }
            Token::DictStart => Object::Dict(self.dict_body(refs, depth)?),
            Token::Keyword(k) => match k.as_slice() {
                b"true" => Object::Bool(true),
                b"false" => Object::Bool(false),
                b"null" => Object::Null,
                _ => return Err(Error::Syntax("unexpected keyword", self.pos)),
            },
            Token::ArrayEnd | Token::DictEnd => {
                return Err(Error::Syntax("unexpected delimiter", self.pos));
            }
        })
    }

    fn dict_body(&mut self, refs: bool, depth: usize) -> Result<Dict> {
        let mut dict = Dict::new();
        loop {
            match self.next_token()? {
                None | Some(Token::DictEnd) => break,
                Some(Token::Name(key)) => {
                    let Some(value) = self.next_token()? else {
                        break;
                    };
                    if value == Token::DictEnd {
                        break;
                    }
                    // A broken entry (seen in real files: a key with a space, `/Maker, Inc(...)`) is
                    // skipped; the rest of the dictionary is still read.
                    match self.object_from(value, refs, depth + 1) {
                        Ok(value) if !value.is_null() => {
                            dict.insert(key, value);
                        }
                        Ok(_) | Err(Error::Syntax("unexpected keyword", _)) => {}
                        Err(e) => return Err(e),
                    }
                }
                // Garbage inside a dictionary: skip it rather than fail the whole file.
                Some(_) => continue,
            }
        }
        Ok(dict)
    }
}

fn parse_number(word: &[u8]) -> Option<Token> {
    let s = std::str::from_utf8(word).ok()?;
    if let Ok(i) = s.parse::<i64>() {
        return Some(Token::Int(i));
    }
    // Tolerate what some producers write: repeated signs ("--5", "+-5") and a trailing dot ("5.").
    let body = s.trim_start_matches(['+', '-']);
    let negative = s.len() > body.len() && s[..s.len() - body.len()].contains('-');
    if body.is_empty() || !body.bytes().all(|b| b.is_ascii_digit() || b == b'.') {
        return None;
    }
    let v: f64 = body
        .parse()
        .ok()
        .or_else(|| body.trim_end_matches('.').parse().ok())?;
    Some(Token::Real(if negative { -v } else { v }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn obj(s: &str) -> Object {
        Lexer::new(s.as_bytes()).parse_object(true).unwrap()
    }

    #[test]
    fn numbers_and_refs() {
        assert_eq!(obj("42"), Object::Int(42));
        assert_eq!(obj("-3.5"), Object::Real(-3.5));
        assert_eq!(obj(".5"), Object::Real(0.5));
        assert_eq!(
            obj("12 0 R"),
            Object::Ref(ObjRef {
                num: 12,
                generation: 0
            })
        );
        assert_eq!(
            obj("[1 2 R 3]"),
            Object::Array(vec![
                Object::Ref(ObjRef {
                    num: 1,
                    generation: 2
                }),
                Object::Int(3)
            ])
        );
        assert_eq!(
            obj("[1 2 3]"),
            Object::Array(vec![Object::Int(1), Object::Int(2), Object::Int(3)])
        );
    }

    #[test]
    fn strings() {
        assert_eq!(
            obj(r"(a\(b\)c \101\n)"),
            Object::String(b"a(b)c A\n".to_vec())
        );
        assert_eq!(
            obj("(nested (parens) ok)"),
            Object::String(b"nested (parens) ok".to_vec())
        );
        assert_eq!(obj("<48 65 6c6C6>"), Object::String(b"Hell`".to_vec()));
    }

    #[test]
    fn names_and_dicts() {
        assert_eq!(obj("/A#20B"), Object::Name(b"A B".to_vec()));
        let d = obj("<< /Type /Page /Count 3 /Kids [4 0 R] /Empty null >>");
        let d = d.as_dict().unwrap();
        assert_eq!(d.get(&b"Type"[..]), Some(&Object::Name(b"Page".to_vec())));
        assert_eq!(d.get(&b"Count"[..]), Some(&Object::Int(3)));
        assert!(!d.contains_key(&b"Empty"[..]));
    }

    #[test]
    fn broken_dictionary_entries_are_skipped() {
        let d =
            obj("<< /Author (Eric Evans) /Maker, LLC(http://x) /Title (Domain-Driven Design) >>");
        let d = d.as_dict().unwrap();
        assert_eq!(
            d.get(&b"Title"[..]),
            Some(&Object::String(b"Domain-Driven Design".to_vec()))
        );
        assert_eq!(
            d.get(&b"Author"[..]),
            Some(&Object::String(b"Eric Evans".to_vec()))
        );
    }

    #[test]
    fn content_tokens() {
        let mut l = Lexer::new(b"BT /F1 12 Tf (Hi) Tj ET % comment\n");
        let mut kinds = Vec::new();
        while let Some(t) = l.next_token().unwrap() {
            kinds.push(t);
        }
        assert_eq!(kinds.len(), 7);
        assert_eq!(kinds[0], Token::Keyword(b"BT".to_vec()));
        assert_eq!(kinds[5], Token::Keyword(b"Tj".to_vec()));
    }
}
