//! The languages Celsus offers by name: the world's widely written languages, each with its English name (for the
//! model), its own name (for people) and its script (for fonts and direction). Any other well-formed language tag
//! still works; it is just not listed.

use serde::Serialize;

use crate::Language;

/// Writing systems, by ISO 15924 code, that need their own fonts or rules.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
pub enum Script {
    Latn,
    Cyrl,
    Grek,
    Armn,
    Geor,
    Arab,
    Hebr,
    Thaa,
    Deva,
    Beng,
    Guru,
    Gujr,
    Orya,
    Taml,
    Telu,
    Knda,
    Mlym,
    Sinh,
    Thai,
    Laoo,
    Khmr,
    Mymr,
    Tibt,
    Ethi,
    /// Simplified Chinese.
    Hans,
    /// Traditional Chinese.
    Hant,
    /// Japanese: kanji and kana.
    Jpan,
    /// Korean: hangul (and hanja).
    Kore,
}

impl Script {
    pub fn is_rtl(self) -> bool {
        matches!(self, Script::Arab | Script::Hebr | Script::Thaa)
    }

    /// Scripts written without spaces between words: lines may break between most characters.
    pub fn breaks_anywhere(self) -> bool {
        matches!(self, Script::Hans | Script::Hant | Script::Jpan)
    }
}

#[derive(Debug, Clone, Copy, Serialize)]
pub struct LanguageInfo {
    /// Lower-case tag as used in file names and URLs: `tr`, `pt-br`, `zh-hans`.
    pub code: &'static str,
    /// English name, which models understand best.
    pub english: &'static str,
    /// The language's own name for itself.
    pub native: &'static str,
    pub script: Script,
    pub rtl: bool,
}

macro_rules! langs {
    ($(($code:literal, $english:literal, $native:literal, $script:ident)),* $(,)?) => {
        &[$(LanguageInfo {
            code: $code,
            english: $english,
            native: $native,
            script: Script::$script,
            rtl: matches!(Script::$script, Script::Arab | Script::Hebr | Script::Thaa),
        }),*]
    };
}

/// Sorted by English name.
pub const LANGUAGES: &[LanguageInfo] = langs![
    ("af", "Afrikaans", "Afrikaans", Latn),
    ("sq", "Albanian", "Shqip", Latn),
    ("am", "Amharic", "አማርኛ", Ethi),
    ("ar", "Arabic", "العربية", Arab),
    ("hy", "Armenian", "Հայերեն", Armn),
    ("as", "Assamese", "অসমীয়া", Beng),
    ("ast", "Asturian", "Asturianu", Latn),
    ("az", "Azerbaijani", "Azərbaycanca", Latn),
    ("ba", "Bashkir", "Башҡортса", Cyrl),
    ("eu", "Basque", "Euskara", Latn),
    ("be", "Belarusian", "Беларуская", Cyrl),
    ("bn", "Bengali", "বাংলা", Beng),
    ("bs", "Bosnian", "Bosanski", Latn),
    ("br", "Breton", "Brezhoneg", Latn),
    ("bg", "Bulgarian", "Български", Cyrl),
    ("my", "Burmese", "မြန်မာ", Mymr),
    ("ca", "Catalan", "Català", Latn),
    ("ceb", "Cebuano", "Cebuano", Latn),
    ("zh-hans", "Chinese (Simplified)", "简体中文", Hans),
    ("zh-hant", "Chinese (Traditional)", "繁體中文", Hant),
    ("cv", "Chuvash", "Чӑвашла", Cyrl),
    ("co", "Corsican", "Corsu", Latn),
    ("crh", "Crimean Tatar", "Qırımtatarca", Latn),
    ("hr", "Croatian", "Hrvatski", Latn),
    ("cs", "Czech", "Čeština", Latn),
    ("da", "Danish", "Dansk", Latn),
    ("dv", "Dhivehi", "ދިވެހި", Thaa),
    ("nl", "Dutch", "Nederlands", Latn),
    ("en", "English", "English", Latn),
    ("eo", "Esperanto", "Esperanto", Latn),
    ("et", "Estonian", "Eesti", Latn),
    ("fo", "Faroese", "Føroyskt", Latn),
    ("fil", "Filipino", "Filipino", Latn),
    ("fi", "Finnish", "Suomi", Latn),
    ("fr", "French", "Français", Latn),
    ("gag", "Gagauz", "Gagauzca", Latn),
    ("gl", "Galician", "Galego", Latn),
    ("ka", "Georgian", "ქართული", Geor),
    ("de", "German", "Deutsch", Latn),
    ("el", "Greek", "Ελληνικά", Grek),
    ("gu", "Gujarati", "ગુજરાતી", Gujr),
    ("ht", "Haitian Creole", "Kreyòl ayisyen", Latn),
    ("ha", "Hausa", "Hausa", Latn),
    ("haw", "Hawaiian", "ʻŌlelo Hawaiʻi", Latn),
    ("he", "Hebrew", "עברית", Hebr),
    ("hi", "Hindi", "हिन्दी", Deva),
    ("hu", "Hungarian", "Magyar", Latn),
    ("is", "Icelandic", "Íslenska", Latn),
    ("ig", "Igbo", "Igbo", Latn),
    ("id", "Indonesian", "Bahasa Indonesia", Latn),
    ("ga", "Irish", "Gaeilge", Latn),
    ("it", "Italian", "Italiano", Latn),
    ("ja", "Japanese", "日本語", Jpan),
    ("jv", "Javanese", "Basa Jawa", Latn),
    ("kn", "Kannada", "ಕನ್ನಡ", Knda),
    ("kk", "Kazakh", "Қазақ тілі", Cyrl),
    ("km", "Khmer", "ខ្មែរ", Khmr),
    ("rw", "Kinyarwanda", "Kinyarwanda", Latn),
    ("ko", "Korean", "한국어", Kore),
    ("ku", "Kurdish (Kurmanji)", "Kurdî", Latn),
    ("ckb", "Kurdish (Sorani)", "کوردی", Arab),
    ("ky", "Kyrgyz", "Кыргызча", Cyrl),
    ("lo", "Lao", "ລາວ", Laoo),
    ("la", "Latin", "Latina", Latn),
    ("lv", "Latvian", "Latviešu", Latn),
    ("lt", "Lithuanian", "Lietuvių", Latn),
    ("lb", "Luxembourgish", "Lëtzebuergesch", Latn),
    ("mk", "Macedonian", "Македонски", Cyrl),
    ("mg", "Malagasy", "Malagasy", Latn),
    ("ms", "Malay", "Bahasa Melayu", Latn),
    ("ml", "Malayalam", "മലയാളം", Mlym),
    ("mt", "Maltese", "Malti", Latn),
    ("mi", "Maori", "Māori", Latn),
    ("mr", "Marathi", "मराठी", Deva),
    ("mn", "Mongolian", "Монгол", Cyrl),
    ("ne", "Nepali", "नेपाली", Deva),
    ("nb", "Norwegian Bokmål", "Norsk bokmål", Latn),
    ("nn", "Norwegian Nynorsk", "Norsk nynorsk", Latn),
    ("oc", "Occitan", "Occitan", Latn),
    ("or", "Odia", "ଓଡ଼ିଆ", Orya),
    ("ps", "Pashto", "پښتو", Arab),
    ("fa", "Persian", "فارسی", Arab),
    ("pl", "Polish", "Polski", Latn),
    ("pt-br", "Portuguese (Brazil)", "Português (Brasil)", Latn),
    ("pt", "Portuguese (Portugal)", "Português (Portugal)", Latn),
    ("pa", "Punjabi", "ਪੰਜਾਬੀ", Guru),
    ("qu", "Quechua", "Runa Simi", Latn),
    ("ro", "Romanian", "Română", Latn),
    ("rm", "Romansh", "Rumantsch", Latn),
    ("ru", "Russian", "Русский", Cyrl),
    ("sm", "Samoan", "Gagana Samoa", Latn),
    ("sa", "Sanskrit", "संस्कृतम्", Deva),
    ("gd", "Scottish Gaelic", "Gàidhlig", Latn),
    ("sr", "Serbian (Cyrillic)", "Српски", Cyrl),
    ("sr-latn", "Serbian (Latin)", "Srpski", Latn),
    ("st", "Sesotho", "Sesotho", Latn),
    ("sn", "Shona", "chiShona", Latn),
    ("sd", "Sindhi", "سنڌي", Arab),
    ("si", "Sinhala", "සිංහල", Sinh),
    ("sk", "Slovak", "Slovenčina", Latn),
    ("sl", "Slovenian", "Slovenščina", Latn),
    ("so", "Somali", "Soomaali", Latn),
    ("es", "Spanish", "Español", Latn),
    (
        "es-419",
        "Spanish (Latin America)",
        "Español (Latinoamérica)",
        Latn
    ),
    ("su", "Sundanese", "Basa Sunda", Latn),
    ("sw", "Swahili", "Kiswahili", Latn),
    ("sv", "Swedish", "Svenska", Latn),
    ("tg", "Tajik", "Тоҷикӣ", Cyrl),
    ("ta", "Tamil", "தமிழ்", Taml),
    ("tt", "Tatar", "Татарча", Cyrl),
    ("te", "Telugu", "తెలుగు", Telu),
    ("th", "Thai", "ไทย", Thai),
    ("bo", "Tibetan", "བོད་ཡིག", Tibt),
    ("ti", "Tigrinya", "ትግርኛ", Ethi),
    ("tr", "Turkish", "Türkçe", Latn),
    ("tk", "Turkmen", "Türkmençe", Latn),
    ("uk", "Ukrainian", "Українська", Cyrl),
    ("ur", "Urdu", "اردو", Arab),
    ("ug", "Uyghur", "ئۇيغۇرچە", Arab),
    ("uz", "Uzbek", "Oʻzbekcha", Latn),
    ("vi", "Vietnamese", "Tiếng Việt", Latn),
    ("cy", "Welsh", "Cymraeg", Latn),
    ("fy", "Western Frisian", "Frysk", Latn),
    ("wo", "Wolof", "Wolof", Latn),
    ("xh", "Xhosa", "isiXhosa", Latn),
    ("yi", "Yiddish", "ייִדיש", Hebr),
    ("yo", "Yoruba", "Yorùbá", Latn),
    ("zu", "Zulu", "isiZulu", Latn),
];

/// What is known about a language: the exact tag first (`pt-br`), then its main language (`pt`). Chinese without a
/// script is Simplified; a script subtag (`sr-latn`, `ku-arab`) decides the script.
pub fn info(lang: &Language) -> Option<LanguageInfo> {
    let tag = lang.as_str().to_ascii_lowercase();
    let primary = lang.primary().to_ascii_lowercase();
    let found = LANGUAGES
        .iter()
        .find(|l| l.code == tag)
        .or_else(|| match primary.as_str() {
            "zh" if tag.contains("hant")
                || tag.ends_with("-tw")
                || tag.ends_with("-hk")
                || tag.ends_with("-mo") =>
            {
                LANGUAGES.iter().find(|l| l.code == "zh-hant")
            }
            "zh" => LANGUAGES.iter().find(|l| l.code == "zh-hans"),
            "no" => LANGUAGES.iter().find(|l| l.code == "nb"),
            "tl" => LANGUAGES.iter().find(|l| l.code == "fil"),
            p => LANGUAGES.iter().find(|l| l.code == p),
        })
        .copied()?;
    // An explicit script subtag overrides the usual one.
    let script = tag
        .split('-')
        .skip(1)
        .find(|s| s.len() == 4)
        .and_then(|s| match s {
            "latn" => Some(Script::Latn),
            "cyrl" => Some(Script::Cyrl),
            "arab" => Some(Script::Arab),
            "hebr" => Some(Script::Hebr),
            "deva" => Some(Script::Deva),
            "hans" => Some(Script::Hans),
            "hant" => Some(Script::Hant),
            _ => None,
        });
    Some(match script {
        Some(s) => LanguageInfo {
            script: s,
            rtl: s.is_rtl(),
            ..found
        },
        None => found,
    })
}

/// The name a model is given for a language, e.g. `Turkish (tr)`.
pub fn english_name(lang: &Language) -> String {
    match info(lang) {
        Some(i) => format!("{} ({lang})", i.english),
        None => lang.as_str().to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lang(s: &str) -> Language {
        s.parse().unwrap()
    }

    #[test]
    fn the_catalogue_is_well_formed() {
        assert!(LANGUAGES.len() >= 120, "{} languages", LANGUAGES.len());
        let mut codes = std::collections::BTreeSet::new();
        for l in LANGUAGES {
            assert!(codes.insert(l.code), "{} twice", l.code);
            assert_eq!(l.code, l.code.to_ascii_lowercase());
            assert!(l.code.parse::<Language>().is_ok(), "{}", l.code);
            assert!(!l.native.is_empty() && !l.english.is_empty());
        }
        let names: Vec<&str> = LANGUAGES.iter().map(|l| l.english).collect();
        let mut sorted = names.clone();
        sorted.sort_unstable();
        assert_eq!(names, sorted, "kept in English alphabetical order");
    }

    #[test]
    fn tags_find_their_language_and_script() {
        assert_eq!(info(&lang("TR")).unwrap().english, "Turkish");
        assert_eq!(info(&lang("pt-BR")).unwrap().code, "pt-br");
        assert_eq!(info(&lang("pt-AO")).unwrap().code, "pt");
        assert_eq!(info(&lang("zh")).unwrap().script, Script::Hans);
        assert_eq!(info(&lang("zh-TW")).unwrap().script, Script::Hant);
        assert_eq!(info(&lang("zh-Hant")).unwrap().code, "zh-hant");
        assert_eq!(info(&lang("sr-Latn")).unwrap().script, Script::Latn);
        assert_eq!(info(&lang("ku-Arab")).unwrap().script, Script::Arab);
        assert!(
            info(&lang("ar")).unwrap().rtl
                && info(&lang("he")).unwrap().rtl
                && info(&lang("dv")).unwrap().rtl
        );
        assert!(!info(&lang("ru")).unwrap().rtl);
        assert!(info(&lang("xx")).is_none());
        assert_eq!(english_name(&lang("ja")), "Japanese (ja)");
        assert_eq!(english_name(&lang("tlh")), "tlh");
    }
}
