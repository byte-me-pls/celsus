//! Words set apart inside a paragraph: a title in italics, a term in bold, a command in a fixed-width face, a
//! link in another colour.
//!
//! The translator sees them as tags around the words, `<i>like this</i>`, and gives them back around the words
//! that say the same in the translation. On the way to the page a tag becomes a single character that means
//! "from here on, style k", so the text can be measured, wrapped and moved as one string.

use crate::layout::TextStyle;

/// `MARK + k` switches to style `k`: 0 is the block's own style, `k` its `inline[k - 1]`. The characters come
/// from a private-use plane, which no text has a use for.
const MARK: u32 = 0xF_0000;

/// How many styles a block may have beside its own. A block with more is not prose.
pub const MAX_STYLES: usize = 8;

/// The names a tag may have, before its number.
const NAMES: [&str; 6] = ["code", "bi", "b", "i", "c", "r"];

pub fn mark(style: usize) -> char {
    char::from_u32(MARK + style.min(MAX_STYLES) as u32).unwrap_or('\u{F0000}')
}

/// The style a mark switches to, if `c` is one.
pub fn style_of(c: char) -> Option<usize> {
    let k = (c as u32).checked_sub(MARK)?;
    (k <= MAX_STYLES as u32).then_some(k as usize)
}

pub fn has_marks(text: &str) -> bool {
    text.chars().any(|c| style_of(c).is_some())
}

pub fn strip_marks(text: &str) -> String {
    text.chars().filter(|c| style_of(*c).is_none()).collect()
}

/// The last mark of `text`: the style a line ends in, which the next line goes on with.
pub fn last_mark(text: &str) -> Option<char> {
    text.chars().rev().find(|c| style_of(*c).is_some())
}

/// `text` cut at its marks: each stretch with its style. What stands before the first mark is in style 0.
pub fn pieces(text: &str) -> Vec<(usize, &str)> {
    let mut out = Vec::new();
    let (mut style, mut from) = (0, 0);
    for (at, c) in text.char_indices() {
        if let Some(k) = style_of(c) {
            if at > from {
                out.push((style, &text[from..at]));
            }
            style = k;
            from = at + c.len_utf8();
        }
    }
    if from < text.len() {
        out.push((style, &text[from..]));
    }
    out
}

/// Put the marks of `text` in order: none that changes nothing, and the spaces around a styled stretch outside
/// it. Every line starts in style 0 and says so where it ends in another.
pub fn tidy(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for (n, line) in text.split('\n').enumerate() {
        if n > 0 {
            out.push('\n');
        }
        let mut current = 0;
        // Spaces not written yet: they go inside a stretch that continues and outside one that ends.
        let mut spaces = String::new();
        for (style, piece) in pieces(line) {
            let core = piece.trim();
            if core.is_empty() {
                spaces.push_str(piece);
                continue;
            }
            if style != current && current != 0 && (style == 0 || !spaces.is_empty()) {
                out.push(mark(0));
                current = 0;
            }
            out.push_str(&spaces);
            out.push_str(&piece[..piece.len() - piece.trim_start().len()]);
            if style != current {
                out.push(mark(style));
                current = style;
            }
            out.push_str(core);
            spaces.clear();
            spaces.push_str(&piece[piece.trim_end().len()..]);
        }
        if current != 0 {
            out.push(mark(0));
        }
    }
    out
}

/// The tag of each inline style of a block, named after what sets it apart from the block's own style.
pub fn names(base: &TextStyle, inline: &[TextStyle]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for s in inline {
        let name = if s.monospace && !base.monospace {
            "code"
        } else if (s.bold, s.italic, s.monospace) == (base.bold, base.italic, base.monospace) {
            "c"
        } else {
            match (s.bold, s.italic) {
                (true, true) => "bi",
                (true, false) => "b",
                (false, true) => "i",
                (false, false) => "r",
            }
        };
        let same = out
            .iter()
            .filter(|n| n.trim_end_matches(|c: char| c.is_ascii_digit()) == name)
            .count();
        out.push(if same == 0 {
            name.to_owned()
        } else {
            format!("{name}{}", same + 1)
        });
    }
    out
}

enum Part<'t> {
    Text(&'t str),
    /// Whether it closes, and its name.
    Tag(bool, &'t str),
}

/// The tag `text` begins with: whether it closes, its name, and its length in bytes.
fn tag_at(text: &str) -> Option<(bool, &str, usize)> {
    let rest = text.strip_prefix('<')?;
    let (closes, rest) = match rest.strip_prefix('/') {
        Some(r) => (true, r),
        None => (false, rest),
    };
    let end = rest.bytes().take(6).position(|b| b == b'>')?;
    let name = &rest[..end];
    let word = name.trim_end_matches(|c: char| c.is_ascii_digit());
    (NAMES.contains(&word) && name.len() - word.len() <= 1)
        .then_some((closes, name, text.len() - rest.len() + end + 1))
}

fn parts(text: &str) -> Vec<Part<'_>> {
    let mut out = Vec::new();
    let (mut from, mut at) = (0, 0);
    while let Some(found) = text[at..].find('<') {
        let open = at + found;
        match tag_at(&text[open..]) {
            Some((closes, name, len)) => {
                if open > from {
                    out.push(Part::Text(&text[from..open]));
                }
                out.push(Part::Tag(closes, name));
                from = open + len;
                at = from;
            }
            None => at = open + 1,
        }
    }
    if from < text.len() {
        out.push(Part::Text(&text[from..]));
    }
    out
}

pub fn has_tags(text: &str) -> bool {
    parts(text).iter().any(|p| matches!(p, Part::Tag(..)))
}

/// The text alone.
pub fn without_tags(text: &str) -> String {
    map_text(text, |t| t.to_owned(), false)
}

/// `text` with `change` applied to what stands between its tags; the tags stay as they are if `keep` says so.
pub fn map_text(text: &str, mut change: impl FnMut(&str) -> String, keep: bool) -> String {
    let mut out = String::with_capacity(text.len());
    for part in parts(text) {
        match part {
            Part::Text(t) => out.push_str(&change(t)),
            Part::Tag(closes, name) if keep => {
                out.push_str(if closes { "</" } else { "<" });
                out.push_str(name);
                out.push('>');
            }
            Part::Tag(..) => {}
        }
    }
    out
}

/// The names of the tags a text opens, sorted: a translation has to have the ones its source has.
pub fn tags(text: &str) -> Vec<String> {
    let mut out: Vec<String> = parts(text)
        .iter()
        .filter_map(|p| match p {
            Part::Tag(false, name) => Some((*name).to_owned()),
            _ => None,
        })
        .collect();
    out.sort();
    out
}

/// Whether a translation has the tags of its source: the same names, and in order if the source's are. How
/// often a name comes is left open, since two stretches may meet in a translation or one may part.
pub fn tags_kept(source: &str, translation: &str) -> bool {
    let (mut want, mut have) = (tags(source), tags(translation));
    want.dedup();
    have.dedup();
    want == have && (marked(source, &want).is_none() || marked(translation, &want).is_some())
}

/// The text for the translator: tags where the marks are. No tag runs over a line break.
pub fn tagged(text: &str, names: &[String]) -> String {
    fn close(out: &mut String, open: &mut Option<&str>) {
        if let Some(name) = open.take() {
            out.push_str("</");
            out.push_str(name);
            out.push('>');
        }
    }
    let mut out = String::with_capacity(text.len() + 16);
    let mut open: Option<&str> = None;
    for c in text.chars() {
        match style_of(c) {
            Some(k) => {
                close(&mut out, &mut open);
                if let Some(name) = k.checked_sub(1).and_then(|k| names.get(k)) {
                    out.push('<');
                    out.push_str(name);
                    out.push('>');
                    open = Some(name);
                }
            }
            None => {
                if c == '\n' {
                    close(&mut out, &mut open);
                }
                out.push(c);
            }
        }
    }
    close(&mut out, &mut open);
    out
}

/// A translation on its way to the page: marks where its tags are. `None` when the tags are not the block's or
/// not in order (one left open, one inside another); the text is then set without them, in the block's style.
pub fn marked(text: &str, names: &[String]) -> Option<String> {
    let mut out = String::with_capacity(text.len());
    let mut open: Option<&str> = None;
    for part in parts(text) {
        match part {
            Part::Text(t) => {
                if open.is_some() && t.contains('\n') {
                    return None;
                }
                out.push_str(t);
            }
            Part::Tag(false, name) => {
                if open.is_some() {
                    return None;
                }
                let k = names.iter().position(|n| n == name)?;
                out.push(mark(k + 1));
                open = Some(name);
            }
            Part::Tag(true, name) => {
                if open != Some(name) {
                    return None;
                }
                out.push(mark(0));
                open = None;
            }
        }
    }
    open.is_none().then(|| tidy(&out))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn style(bold: bool, italic: bool, monospace: bool, color: u32) -> TextStyle {
        TextStyle {
            font_name: None,
            font_size: 10.0,
            color,
            bold,
            italic,
            serif: true,
            monospace,
        }
    }

    #[test]
    fn styles_are_named_after_what_sets_them_apart() {
        let base = style(false, false, false, 0);
        let inline = [
            style(false, true, false, 0),
            style(true, false, false, 0),
            style(false, false, true, 0),
            style(false, false, false, 0x0000cc),
            style(false, true, false, 0x0000cc),
            style(true, true, false, 0),
        ];
        assert_eq!(names(&base, &inline), ["i", "b", "code", "c", "i2", "bi"]);
        // Upright words in a heading set in bold italics.
        let heading = style(true, true, false, 0);
        assert_eq!(names(&heading, &[style(false, false, false, 0)]), ["r"]);
    }

    #[test]
    fn tags_and_marks_say_the_same() {
        let names = vec!["i".to_owned(), "code".to_owned()];
        let text = format!(
            "See {}Domain Design{} and run {}make all{} first.",
            mark(1),
            mark(0),
            mark(2),
            mark(0)
        );
        let tags = tagged(&text, &names);
        assert_eq!(tags, "See <i>Domain Design</i> and run <code>make all</code> first.");
        assert_eq!(marked(&tags, &names).as_deref(), Some(text.as_str()));
        assert_eq!(without_tags(&tags), strip_marks(&text));
        assert_eq!(super::tags(&tags), ["code", "i"]);
    }

    #[test]
    fn marks_are_put_in_order() {
        // The space between two italic words is theirs; the one after them is not.
        let text = format!("{}the {}a {}title {}of {}it", mark(0), mark(1), mark(1), mark(0), mark(0));
        assert_eq!(tidy(&text), format!("the {}a title{} of it", mark(1), mark(0)));
        // A style does not run over a line break.
        let lines = format!("{}one\ntwo", mark(1));
        assert_eq!(tidy(&lines), format!("{}one{}\ntwo", mark(1), mark(0)));
        assert_eq!(tagged(&tidy(&lines), &["b".to_owned()]), "<b>one</b>\ntwo");
        assert_eq!(tidy("  plain text "), "  plain text");
    }

    #[test]
    fn tags_out_of_order_are_not_taken() {
        let names = vec!["i".to_owned(), "b".to_owned()];
        assert!(marked("<i>open", &names).is_none());
        assert!(marked("<i>one <b>in</b> another</i>", &names).is_none());
        assert!(marked("closed</i> only", &names).is_none());
        assert!(marked("<code>not of this block</code>", &names).is_none());
        assert!(marked("<i>over\nlines</i>", &names).is_none());
        // Text that only looks like a tag is text.
        assert_eq!(marked("a <br> and x<y>z", &names).as_deref(), Some("a <br> and x<y>z"));
        assert_eq!(without_tags("if a<b> then <b2>go</b2>"), "if a then go");
    }

    #[test]
    fn a_translation_keeps_the_tags_of_its_source() {
        let source = "See <i>Domain Design</i> and <i>Patterns</i>, then run <code>make</code>.";
        assert!(tags_kept(source, "<code>make</code> komutundan önce <i>Domain Design ve Patterns</i> okunur."));
        assert!(!tags_kept(source, "Önce Domain Design okunur, sonra <code>make</code>."));
        assert!(!tags_kept(source, "<i>Domain Design okunur, sonra <code>make</code></i>."));
        assert!(!tags_kept(source, "<i>Domain Design</i> ve <b>Patterns</b>, sonra <code>make</code>."));
        assert!(tags_kept("no tags here", "burada etiket yok"));
        assert!(!tags_kept("no tags here", "burada <b>etiket</b> yok"));
    }
}
