use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

/// A language identified by a BCP 47 style tag such as `tr`, `en`, `pt-BR` or `zh-Hant`.
///
/// There is no fixed list: any well-formed tag is accepted, so new languages need no code change.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct Language(String);

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
#[error("invalid language tag: {0:?}")]
pub struct InvalidLanguage(String);

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
            return Err(InvalidLanguage(tag.to_owned()));
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
        Ok(Self(canonical))
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
    fn detects_rtl() {
        assert!("ar".parse::<Language>().unwrap().is_rtl());
        assert!("he".parse::<Language>().unwrap().is_rtl());
        assert!(!"tr".parse::<Language>().unwrap().is_rtl());
        assert!("ku-Arab".parse::<Language>().unwrap().is_rtl());
        assert!(!"ku-Latn".parse::<Language>().unwrap().is_rtl());
    }
}
