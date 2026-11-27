use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

/// A language identified by a BCP 47 style tag such as `tr`, `en`, `pt-BR` or `zh-Hant`.
///
/// Only languages Celsus knows (see [`crate::languages::LANGUAGES`]) are accepted, with any region or script
/// after them: `pt-BR`, `en-GB`, `sr-Latn`. A book cannot be translated into a language that has no name for
/// the model, no fonts chosen for it and no direction, and a mistyped code would otherwise pass for one.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct Language(String);

/// A language tag that was refused: not shaped like a tag at all, or a language Celsus does not know.
#[derive(Debug, PartialEq, Eq)]
pub struct InvalidLanguage {
    tag: String,
    /// The tag was well formed; the language is just not one Celsus knows.
    unknown: bool,
}

impl InvalidLanguage {
    /// The tag was well formed, but it is not a language Celsus knows.
    pub fn is_unknown(&self) -> bool {
        self.unknown
    }

    /// The known language whose code is nearest to the refused tag, when one is near enough to be a slip.
    pub fn nearest(&self) -> Option<&'static crate::languages::LanguageInfo> {
        if !self.unknown {
            return None;
        }
        let tag = self.tag.to_ascii_lowercase();
        let primary = tag.split('-').next().unwrap_or_default();
        crate::languages::LANGUAGES
            .iter()
            .map(|l| {
                let whole = distance(&tag, l.code);
                let first = distance(primary, l.code.split('-').next().unwrap_or(l.code));
                (whole.min(first), l)
            })
            .filter(|(d, _)| *d <= 1)
            .min_by_key(|(d, l)| (*d, l.code.len(), l.code))
            .map(|(_, l)| l)
    }
}

impl fmt::Display for InvalidLanguage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if !self.unknown {
            return write!(f, "invalid language tag: {:?}", self.tag);
        }
        write!(f, "{:?} is not a language Celsus knows.", self.tag)?;
        match self.nearest() {
            Some(l) => write!(f, " Did you mean {} ({})?", l.code, l.english),
            None => write!(
                f,
                " Use a code from the list of languages Celsus offers, such as tr, de, ar, ja, zh-hans or pt-br."
            ),
        }
    }
}

impl std::error::Error for InvalidLanguage {}

/// How many letters have to be added, removed or changed to turn `a` into `b`.
fn distance(a: &str, b: &str) -> usize {
    let b: Vec<char> = b.chars().collect();
    let mut row: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.chars().enumerate() {
        let mut diagonal = row[0];
        row[0] = i + 1;
        for (j, cb) in b.iter().enumerate() {
            let above = row[j + 1];
            row[j + 1] = (above + 1)
                .min(row[j] + 1)
                .min(diagonal + usize::from(ca != *cb));
            diagonal = above;
        }
    }
    row[b.len()]
}

impl Language {
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The primary subtag, e.g. `pt` for `pt-BR`.
    pub fn primary(&self) -> &str {
        self.0.split('-').next().unwrap_or(&self.0)
    }

    /// Whether the language is written right to left.
    pub fn is_rtl(&self) -> bool {
        if let Some(info) = crate::languages::info(self) {
            return info.rtl;
        }
        const RTL: &[&str] = &["ar", "he", "fa", "ur", "ps", "sd", "ug", "yi", "dv", "ckb"];
        // An explicit script subtag (ku-Arab, ku-Latn) decides; otherwise the language's usual script does.
        match self.0.split('-').skip(1).find(|s| s.len() == 4) {
            Some(script) => matches!(script, "Arab" | "Hebr" | "Thaa" | "Syrc" | "Nkoo" | "Adlm"),
            None => RTL.contains(&self.primary()),
        }
    }
}

impl FromStr for Language {
    type Err = InvalidLanguage;

    fn from_str(tag: &str) -> Result<Self, Self::Err> {
        let tag = tag.trim();
        let mut parts = tag.split('-');
        let primary = parts.next().unwrap_or_default();
        let primary_ok =
            (2..=3).contains(&primary.len()) && primary.chars().all(|c| c.is_ascii_alphabetic());
        let rest_ok = parts
            .all(|p| (1..=8).contains(&p.len()) && p.chars().all(|c| c.is_ascii_alphanumeric()));
        if !primary_ok || !rest_ok {
            return Err(InvalidLanguage {
                tag: tag.to_owned(),
                unknown: false,
            });
        }
        // Canonical case: language lower, script title, region upper.
        let canonical = tag
            .split('-')
            .enumerate()
            .map(|(i, p)| match (i, p.len()) {
                (0, _) => p.to_ascii_lowercase(),
                (_, 4) => {
                    let mut s = p.to_ascii_lowercase();
                    s[..1].make_ascii_uppercase();
                    s
                }
                (_, 2) => p.to_ascii_uppercase(),
                _ => p.to_ascii_lowercase(),
            })
            .collect::<Vec<_>>()
            .join("-");
        let language = Self(canonical);
        if crate::languages::info(&language).is_none() {
            return Err(InvalidLanguage {
                tag: tag.to_owned(),
                unknown: true,
            });
        }
        Ok(language)
    }
}

impl TryFrom<String> for Language {
    type Error = InvalidLanguage;
    fn try_from(value: String) -> Result<Self, Self::Error> {
        value.parse()
    }
}

impl From<Language> for String {
    fn from(value: Language) -> Self {
        value.0
    }
}

impl fmt::Display for Language {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_and_canonicalises() {
        assert_eq!("TR".parse::<Language>().unwrap().as_str(), "tr");
        assert_eq!("pt-br".parse::<Language>().unwrap().as_str(), "pt-BR");
        assert_eq!(
            "zh-hant-tw".parse::<Language>().unwrap().as_str(),
            "zh-Hant-TW"
        );
    }

    #[test]
    fn rejects_garbage() {
        assert!("".parse::<Language>().is_err());
        assert!("english".parse::<Language>().is_err());
        assert!("en_US".parse::<Language>().is_err());
    }

    #[test]
    fn accepts_only_known_languages() {
        // Every code in the list, as it is written there and in canonical case.
        for l in crate::languages::LANGUAGES {
            assert!(l.code.parse::<Language>().is_ok(), "{}", l.code);
            assert!(
                l.code.to_ascii_uppercase().parse::<Language>().is_ok(),
                "{}",
                l.code
            );
        }
        // Regions and scripts of known languages, and the usual other names of a few.
        for tag in [
            "en-GB",
            "pt-AO",
            "zh",
            "zh-TW",
            "zh-Hant-HK",
            "sr-Latn",
            "ku-Arab",
            "no",
            "tl",
        ] {
            assert!(tag.parse::<Language>().is_ok(), "{tag}");
        }
        let e = "xq".parse::<Language>().unwrap_err();
        assert!(e.to_string().contains("\"xq\""), "{e}");
        let e = "trr".parse::<Language>().unwrap_err();
        assert_eq!(e.nearest().map(|l| l.code), Some("tr"));
        assert!(e.to_string().contains("Did you mean tr (Turkish)?"), "{e}");
        let e = "qqq-x".parse::<Language>().unwrap_err();
        assert!(e.nearest().is_none());
        assert!(e.to_string().contains("list of languages"), "{e}");
        // A malformed tag suggests nothing.
        assert!("en_US".parse::<Language>().unwrap_err().nearest().is_none());
    }

    #[test]
    fn detects_rtl() {
        assert!("ar".parse::<Language>().unwrap().is_rtl());
        assert!("he".parse::<Language>().unwrap().is_rtl());
        assert!(!"tr".parse::<Language>().unwrap().is_rtl());
        assert!("ku-Arab".parse::<Language>().unwrap().is_rtl());
        assert!(!"ku-Latn".parse::<Language>().unwrap().is_rtl());
    }
}
