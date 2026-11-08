//! Splitting a content stream into operations, keeping the byte range of each so it can be edited in place.

use crate::lexer::{Lexer, Token};
use crate::object::Object;

#[derive(Debug, Clone, PartialEq)]
pub struct Op {
    pub operator: Vec<u8>,
    pub operands: Vec<Object>,
    /// Byte range in the decoded stream, from the first operand to the end of the operator.
    pub start: usize,
    pub end: usize,
}

/// Parse a content stream. Malformed parts are skipped instead of failing the page.
pub fn parse(data: &[u8]) -> Vec<Op> {
    let mut ops = Vec::new();
    let mut lexer = Lexer::new(data);
    let mut operands: Vec<Object> = Vec::new();
    let mut start = 0;
    loop {
        lexer.skip_whitespace();
        let token_start = lexer.pos;
        let token = match lexer.next_token() {
            Ok(Some(t)) => t,
            Ok(None) => break,
            Err(_) => {
                lexer.pos = token_start + 1;
                operands.clear();
                continue;
            }
        };
        if operands.is_empty() {
            start = token_start;
        }
        match token {
            Token::Keyword(k) if k == b"BI" => {
                let end = skip_inline_image(data, lexer.pos);
                lexer.pos = end;
                ops.push(Op {
                    operator: k,
                    operands: Vec::new(),
                    start,
                    end,
                });
                operands.clear();
            }
            Token::Keyword(k) if !matches!(k.as_slice(), b"true" | b"false" | b"null") => {
                ops.push(Op {
                    operator: k,
                    operands: std::mem::take(&mut operands),
                    start,
                    end: lexer.pos,
                });
            }
            _ => {
                // An operand: rebuild the object from this token (arrays and dicts consume more tokens).
                lexer.pos = token_start;
                match lexer.parse_object(false) {
                    Ok(obj) => operands.push(obj),
                    Err(_) => lexer.pos = token_start + 1,
                }
                if operands.len() > 1024 {
                    operands.clear();
                }
            }
        }
    }
    ops
}

/// Inline image data runs from after `ID` to an `EI` surrounded by whitespace.
fn skip_inline_image(data: &[u8], from: usize) -> usize {
    let Some(id) = find_keyword(data, from, b"ID") else {
        return data.len();
    };
    let mut i = id + 3; // "ID" and one whitespace byte
    while i + 1 < data.len() {
        if &data[i..i + 2] == b"EI"
            && crate::lexer::is_whitespace(data[i - 1])
            && data
                .get(i + 2)
                .is_none_or(|b| crate::lexer::is_whitespace(*b))
        {
            return i + 2;
        }
        i += 1;
    }
    data.len()
}

fn find_keyword(data: &[u8], from: usize, word: &[u8]) -> Option<usize> {
    let mut lexer = Lexer::at(data, from);
    loop {
        lexer.skip_whitespace();
        let at = lexer.pos;
        match lexer.next_token() {
            Ok(Some(Token::Keyword(k))) if k == word => return Some(at),
            Ok(Some(_)) => {}
            _ => return None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ops_with_ranges() {
        let data = b"q 1 0 0 1 50 700 cm BT /F1 12 Tf [(Hel) -20 (lo)] TJ ET Q";
        let ops = parse(data);
        let names: Vec<&[u8]> = ops.iter().map(|o| o.operator.as_slice()).collect();
        assert_eq!(
            names,
            vec![&b"q"[..], b"cm", b"BT", b"Tf", b"TJ", b"ET", b"Q"]
        );
        let tj = &ops[4];
        assert_eq!(&data[tj.start..tj.end], b"[(Hel) -20 (lo)] TJ");
        assert_eq!(tj.operands.len(), 1);
    }

    #[test]
    fn inline_images_are_skipped() {
        let data = b"BI /W 2 /H 1 /BPC 8 /CS /G ID \x00EI\xff EI Q";
        let ops = parse(data);
        assert_eq!(ops.len(), 2);
        assert_eq!(ops[0].operator, b"BI");
        assert_eq!(ops[1].operator, b"Q");
    }
}
