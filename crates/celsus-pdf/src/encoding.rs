//! Built-in single-byte encodings and glyph names.

/// Characters 128..=255 of each encoding; `\0` means undefined. 0..=127 is ASCII for WinAnsi and MacRoman.
const WIN_ANSI_HIGH: &str = "€\0‚ƒ„…†‡ˆ‰Š‹Œ\0Ž\0\0‘’“”•–—˜™š›œ\0žŸ\u{a0}¡¢£¤¥¦§¨©ª«¬\u{ad}®¯°±²³´µ¶·¸¹º»¼½¾¿ÀÁÂÃÄÅÆÇÈÉÊËÌÍÎÏÐÑÒÓÔÕÖ×ØÙÚÛÜÝÞßàáâãäåæçèéêëìíîïðñòóôõö÷øùúûüýþÿ";
const MAC_ROMAN_HIGH: &str = "ÄÅÇÉÑÖÜáàâäãåçéèêëíìîïñóòôöõúùûü†°¢£§•¶ß®©™´¨≠ÆØ∞±≤≥¥µ∂∑∏π∫ªºΩæø¿¡¬√ƒ≈∆«»…\u{a0}ÀÃÕŒœ–—“”‘’÷◊ÿŸ⁄€‹›ﬁﬂ‡·‚„‰ÂÊÁËÈÍÎÏÌÓÔ\u{f8ff}ÒÚÛÙıˆ˜¯˘˙˚¸˝˛ˇ";
/// Adobe StandardEncoding 161..=255.
const STANDARD_HIGH: &str = "¡¢£⁄¥ƒ§¤'“«‹›ﬁﬂ\0–†‡·\0¶•‚„”»…‰\0¿\0`´ˆ˜¯˘˙¨\0˚¸\0˝˛ˇ—\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0Æ\0ª\0\0\0\0ŁØŒº\0\0\0\0\0æ\0\0\0ı\0\0łøœß\0\0\0\0";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BaseEncoding {
    Standard,
    WinAnsi,
    MacRoman,
    /// Symbolic fonts: codes have no standard meaning.
    None,
}

impl BaseEncoding {
    pub fn from_name(name: &[u8]) -> Option<Self> {
        match name {
            b"WinAnsiEncoding" => Some(Self::WinAnsi),
            b"MacRomanEncoding" | b"MacExpertEncoding" => Some(Self::MacRoman),
            b"StandardEncoding" => Some(Self::Standard),
            _ => None,
        }
    }

    pub fn char(self, code: u8) -> Option<char> {
        match self {
            Self::None => None,
            Self::WinAnsi | Self::MacRoman if code < 128 => ascii(code),
            Self::WinAnsi => nth(WIN_ANSI_HIGH, code - 128),
            Self::MacRoman => nth(MAC_ROMAN_HIGH, code - 128),
            Self::Standard => match code {
                0x27 => Some('’'),
                0x60 => Some('‘'),
                0..=127 => ascii(code),
                161..=255 => nth(STANDARD_HIGH, code - 161),
                _ => None,
            },
        }
    }
}

fn ascii(code: u8) -> Option<char> {
    (32..127).contains(&code).then_some(code as char)
}

fn nth(table: &str, i: u8) -> Option<char> {
    table.chars().nth(usize::from(i)).filter(|c| *c != '\0')
}

/// Glyph names that are not a single ASCII letter, from the Adobe Glyph List (the subset books use).
const NAMES: &[(&str, &str)] = &[
    ("space", " "),
    ("exclam", "!"),
    ("quotedbl", "\""),
    ("numbersign", "#"),
    ("dollar", "$"),
    ("percent", "%"),
    ("ampersand", "&"),
    ("quotesingle", "'"),
    ("quoteright", "’"),
    ("parenleft", "("),
    ("parenright", ")"),
    ("asterisk", "*"),
    ("plus", "+"),
    ("comma", ","),
    ("hyphen", "-"),
    ("period", "."),
    ("slash", "/"),
    ("zero", "0"),
    ("one", "1"),
    ("two", "2"),
    ("three", "3"),
    ("four", "4"),
    ("five", "5"),
    ("six", "6"),
    ("seven", "7"),
    ("eight", "8"),
    ("nine", "9"),
    ("colon", ":"),
    ("semicolon", ";"),
    ("less", "<"),
    ("equal", "="),
    ("greater", ">"),
    ("question", "?"),
    ("at", "@"),
    ("bracketleft", "["),
    ("backslash", "\\"),
    ("bracketright", "]"),
    ("asciicircum", "^"),
    ("underscore", "_"),
    ("grave", "`"),
    ("quoteleft", "‘"),
    ("braceleft", "{"),
    ("bar", "|"),
    ("braceright", "}"),
    ("asciitilde", "~"),
    ("exclamdown", "¡"),
    ("cent", "¢"),
    ("sterling", "£"),
    ("fraction", "⁄"),
    ("yen", "¥"),
    ("florin", "ƒ"),
    ("section", "§"),
    ("currency", "¤"),
    ("quotedblleft", "“"),
    ("guillemotleft", "«"),
    ("guilsinglleft", "‹"),
    ("guilsinglright", "›"),
    ("fi", "fi"),
    ("fl", "fl"),
    ("ff", "ff"),
    ("ffi", "ffi"),
    ("ffl", "ffl"),
    ("endash", "–"),
    ("dagger", "†"),
    ("daggerdbl", "‡"),
    ("periodcentered", "·"),
    ("paragraph", "¶"),
    ("bullet", "•"),
    ("quotesinglbase", "‚"),
    ("quotedblbase", "„"),
    ("quotedblright", "”"),
    ("guillemotright", "»"),
    ("ellipsis", "…"),
    ("perthousand", "‰"),
    ("questiondown", "¿"),
    ("acute", "´"),
    ("circumflex", "ˆ"),
    ("tilde", "˜"),
    ("macron", "¯"),
    ("breve", "˘"),
    ("dotaccent", "˙"),
    ("dieresis", "¨"),
    ("ring", "˚"),
    ("cedilla", "¸"),
    ("hungarumlaut", "˝"),
    ("ogonek", "˛"),
    ("caron", "ˇ"),
    ("emdash", "—"),
    ("AE", "Æ"),
    ("ordfeminine", "ª"),
    ("Lslash", "Ł"),
    ("Oslash", "Ø"),
    ("OE", "Œ"),
    ("ordmasculine", "º"),
    ("ae", "æ"),
    ("dotlessi", "ı"),
    ("lslash", "ł"),
    ("oslash", "ø"),
    ("oe", "œ"),
    ("germandbls", "ß"),
    ("Euro", "€"),
    ("trademark", "™"),
    ("copyright", "©"),
    ("registered", "®"),
    ("degree", "°"),
    ("plusminus", "±"),
    ("multiply", "×"),
    ("divide", "÷"),
    ("mu", "µ"),
    ("logicalnot", "¬"),
    ("brokenbar", "¦"),
    ("onehalf", "½"),
    ("onequarter", "¼"),
    ("threequarters", "¾"),
    ("onesuperior", "¹"),
    ("twosuperior", "²"),
    ("threesuperior", "³"),
    ("minus", "−"),
    ("nbspace", "\u{a0}"),
    ("sfthyphen", "\u{ad}"),
    ("Eth", "Ð"),
    ("eth", "ð"),
    ("Thorn", "Þ"),
    ("thorn", "þ"),
    ("Gbreve", "Ğ"),
    ("gbreve", "ğ"),
    ("Idotaccent", "İ"),
    ("Idot", "İ"),
    ("Scedilla", "Ş"),
    ("scedilla", "ş"),
    ("Scaron", "Š"),
    ("scaron", "š"),
    ("Zcaron", "Ž"),
    ("zcaron", "ž"),
    ("Ydieresis", "Ÿ"),
    ("bulletoperator", "∙"),
    ("notequal", "≠"),
    ("lessequal", "≤"),
    ("greaterequal", "≥"),
    ("infinity", "∞"),
    ("arrowright", "→"),
    ("arrowleft", "←"),
    ("arrowup", "↑"),
    ("arrowdown", "↓"),
    ("arrowboth", "↔"),
    ("checkmark", "✓"),
    ("dotlessj", "ȷ"),
    ("quotereversed", "‛"),
    ("figuredash", "‒"),
    ("horizontalbar", "―"),
    ("emspace", "\u{2003}"),
    ("enspace", "\u{2002}"),
    ("Delta", "∆"),
    ("Omega", "Ω"),
    ("pi", "π"),
    ("summation", "∑"),
    ("product", "∏"),
    ("integral", "∫"),
    ("radical", "√"),
    ("approxequal", "≈"),
    ("partialdiff", "∂"),
    ("lozenge", "◊"),
    ("alpha", "α"),
    ("beta", "β"),
    ("gamma", "γ"),
    ("delta", "δ"),
    ("epsilon", "ε"),
    ("lambda", "λ"),
    ("sigma", "σ"),
    ("theta", "θ"),
    ("omega", "ω"),
];

/// Accent suffixes of glyph names and the combining mark they add.
const ACCENTS: &[(&str, char)] = &[
    ("acute", '\u{301}'),
    ("grave", '\u{300}'),
    ("circumflex", '\u{302}'),
    ("tilde", '\u{303}'),
    ("dieresis", '\u{308}'),
    ("ring", '\u{30a}'),
    ("cedilla", '\u{327}'),
    ("caron", '\u{30c}'),
    ("breve", '\u{306}'),
    ("macron", '\u{304}'),
    ("ogonek", '\u{328}'),
    ("dotaccent", '\u{307}'),
    ("hungarumlaut", '\u{30b}'),
    ("commaaccent", '\u{326}'),
];

/// Unicode text for a glyph name, following the Adobe Glyph List rules.
pub fn glyph_name_to_unicode(name: &[u8]) -> Option<String> {
    let name = std::str::from_utf8(name).ok()?;
    // Variants such as "a.sc" or "one.oldstyle" map like their base name.
    let base = name.split('.').next().unwrap_or(name);
    if base.is_empty() {
        return None;
    }
    if base.contains('_') {
        // Ligatures written as components: "f_f_i".
        return base
            .split('_')
            .map(|p| glyph_name_to_unicode(p.as_bytes()))
            .collect();
    }
    if base.len() == 1 && base.as_bytes()[0].is_ascii_alphabetic() {
        return Some(base.to_owned());
    }
    if let Some((_, s)) = NAMES.iter().find(|(n, _)| *n == base) {
        return Some((*s).to_owned());
    }
    if let Some(hex) = base.strip_prefix("uni")
        && hex.len() >= 4
        && hex.len() % 4 == 0
    {
        let units: Option<Vec<u16>> = hex
            .as_bytes()
            .chunks(4)
            .map(|c| u16::from_str_radix(std::str::from_utf8(c).ok()?, 16).ok())
            .collect();
        return String::from_utf16(&units?).ok();
    }
    if let Some(hex) = base.strip_prefix('u')
        && (4..=6).contains(&hex.len())
        && let Ok(v) = u32::from_str_radix(hex, 16)
    {
        return char::from_u32(v).map(String::from);
    }
    // Accented letters: "aacute", "Ccedilla", "scommaaccent" -> base letter + combining mark, then composed.
    for (suffix, mark) in ACCENTS {
        if let Some(letter) = base.strip_suffix(suffix)
            && letter.len() == 1
            && letter.as_bytes()[0].is_ascii_alphabetic()
        {
            return Some(compose(letter.chars().next().unwrap(), *mark));
        }
    }
    None
}

/// Compose a Latin letter with a combining mark when a precomposed character exists.
fn compose(letter: char, mark: char) -> String {
    const TABLE: &str =
        "AÀÁÂÃÄÅ CÇ EÈÉÊË IÌÍÎÏ NÑ OÒÓÔÕÖ UÙÚÛÜ YÝ aàáâãäå cç eèéêë iìíîï nñ oòóôõö uùúûü yý";
    let marks = [
        '\u{300}', '\u{301}', '\u{302}', '\u{303}', '\u{308}', '\u{30a}',
    ];
    for group in TABLE.split(' ') {
        let mut chars = group.chars();
        if chars.next() == Some(letter) {
            let rest: Vec<char> = chars.collect();
            if mark == '\u{327}' && rest.len() == 1 {
                return rest[0].to_string();
            }
            if let Some(i) = marks.iter().position(|m| *m == mark)
                && let Some(c) = rest.get(i)
                && rest.len() >= 4
            {
                return c.to_string();
            }
        }
    }
    let extra = [
        ('G', '\u{306}', 'Ğ'),
        ('g', '\u{306}', 'ğ'),
        ('S', '\u{327}', 'Ş'),
        ('s', '\u{327}', 'ş'),
        ('I', '\u{307}', 'İ'),
        ('S', '\u{30c}', 'Š'),
        ('s', '\u{30c}', 'š'),
        ('Z', '\u{30c}', 'Ž'),
        ('z', '\u{30c}', 'ž'),
        ('C', '\u{30c}', 'Č'),
        ('c', '\u{30c}', 'č'),
        ('R', '\u{30c}', 'Ř'),
        ('r', '\u{30c}', 'ř'),
        ('E', '\u{30c}', 'Ě'),
        ('e', '\u{30c}', 'ě'),
        ('C', '\u{301}', 'Ć'),
        ('c', '\u{301}', 'ć'),
        ('S', '\u{301}', 'Ś'),
        ('s', '\u{301}', 'ś'),
        ('Z', '\u{301}', 'Ź'),
        ('z', '\u{301}', 'ź'),
        ('N', '\u{301}', 'Ń'),
        ('n', '\u{301}', 'ń'),
        ('A', '\u{328}', 'Ą'),
        ('a', '\u{328}', 'ą'),
        ('E', '\u{328}', 'Ę'),
        ('e', '\u{328}', 'ę'),
        ('Z', '\u{307}', 'Ż'),
        ('z', '\u{307}', 'ż'),
        ('O', '\u{30b}', 'Ő'),
        ('o', '\u{30b}', 'ő'),
        ('U', '\u{30b}', 'Ű'),
        ('u', '\u{30b}', 'ű'),
        ('y', '\u{308}', 'ÿ'),
        ('Y', '\u{308}', 'Ÿ'),
    ];
    extra
        .iter()
        .find(|(l, m, _)| *l == letter && *m == mark)
        .map(|(_, _, c)| c.to_string())
        .unwrap_or_else(|| format!("{letter}{mark}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn table_sizes() {
        assert_eq!(WIN_ANSI_HIGH.chars().count(), 128);
        assert_eq!(MAC_ROMAN_HIGH.chars().count(), 128);
        assert_eq!(STANDARD_HIGH.chars().count(), 95);
    }

    #[test]
    fn encodings() {
        assert_eq!(BaseEncoding::WinAnsi.char(0x93), Some('“'));
        assert_eq!(BaseEncoding::WinAnsi.char(0xFC), Some('ü'));
        assert_eq!(BaseEncoding::MacRoman.char(0x8A), Some('ä'));
        assert_eq!(BaseEncoding::MacRoman.char(0xD5), Some('’'));
        assert_eq!(BaseEncoding::Standard.char(0x27), Some('’'));
        assert_eq!(BaseEncoding::Standard.char(0xAE), Some('ﬁ'));
        assert_eq!(BaseEncoding::Standard.char(0xFB), Some('ß'));
        assert_eq!(BaseEncoding::Standard.char(0xF5), Some('ı'));
    }

    #[test]
    fn glyph_names() {
        let n = |s: &str| glyph_name_to_unicode(s.as_bytes());
        assert_eq!(n("a").as_deref(), Some("a"));
        assert_eq!(n("eacute").as_deref(), Some("é"));
        assert_eq!(n("gbreve").as_deref(), Some("ğ"));
        assert_eq!(n("Scedilla").as_deref(), Some("Ş"));
        assert_eq!(n("uni0130").as_deref(), Some("İ"));
        assert_eq!(n("u1F600").as_deref(), Some("😀"));
        assert_eq!(n("f_f_i").as_deref(), Some("ffi"));
        assert_eq!(n("one.oldstyle").as_deref(), Some("1"));
        assert_eq!(n("g123"), None);
    }
}
