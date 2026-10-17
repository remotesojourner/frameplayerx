//! Language tags as containers, file names and servers write them (`en`,
//! `eng`, `ger`, `pt-BR`, `English`, `de.forced`), reduced to an ISO 639-1
//! code and an English name.

/// ISO 639-1 code, English name, and the other codes and names tags use
/// for the language (ISO 639-2/T and /B, common spellings). By name.
pub const LANGUAGES: &[(&str, &str, &[&str])] = &[
    ("sq", "Albanian", &["sqi", "alb"]),
    ("ar", "Arabic", &["ara"]),
    ("hy", "Armenian", &["hye", "arm"]),
    ("eu", "Basque", &["eus", "baq"]),
    ("bn", "Bengali", &["ben", "bangla"]),
    ("bs", "Bosnian", &["bos"]),
    ("bg", "Bulgarian", &["bul"]),
    ("my", "Burmese", &["mya", "bur"]),
    ("ca", "Catalan", &["cat"]),
    (
        "zh",
        "Chinese",
        &["zho", "chi", "chs", "cht", "cmn", "mandarin"],
    ),
    ("hr", "Croatian", &["hrv"]),
    ("cs", "Czech", &["ces", "cze"]),
    ("da", "Danish", &["dan"]),
    ("nl", "Dutch", &["nld", "dut", "flemish"]),
    ("en", "English", &["eng"]),
    ("et", "Estonian", &["est"]),
    ("tl", "Filipino", &["fil", "tgl", "tagalog"]),
    ("fi", "Finnish", &["fin"]),
    ("fr", "French", &["fra", "fre"]),
    ("gl", "Galician", &["glg"]),
    ("ka", "Georgian", &["kat", "geo"]),
    ("de", "German", &["deu", "ger"]),
    ("el", "Greek", &["ell", "gre"]),
    ("gu", "Gujarati", &["guj"]),
    ("he", "Hebrew", &["heb", "iw"]),
    ("hi", "Hindi", &["hin"]),
    ("hu", "Hungarian", &["hun"]),
    ("is", "Icelandic", &["isl", "ice"]),
    ("id", "Indonesian", &["ind"]),
    ("ga", "Irish", &["gle"]),
    ("it", "Italian", &["ita"]),
    ("ja", "Japanese", &["jpn"]),
    ("kn", "Kannada", &["kan"]),
    ("kk", "Kazakh", &["kaz"]),
    ("km", "Khmer", &["khm"]),
    ("ko", "Korean", &["kor"]),
    ("lo", "Lao", &["lao"]),
    ("lv", "Latvian", &["lav"]),
    ("lt", "Lithuanian", &["lit"]),
    ("mk", "Macedonian", &["mkd", "mac"]),
    ("ms", "Malay", &["msa", "may"]),
    ("ml", "Malayalam", &["mal"]),
    ("mr", "Marathi", &["mar"]),
    ("mn", "Mongolian", &["mon"]),
    ("ne", "Nepali", &["nep"]),
    (
        "no",
        "Norwegian",
        &["nor", "nob", "nno", "nb", "nn", "bokmal"],
    ),
    ("fa", "Persian", &["fas", "per", "farsi"]),
    ("pl", "Polish", &["pol"]),
    ("pt", "Portuguese", &["por"]),
    ("pa", "Punjabi", &["pan"]),
    ("ro", "Romanian", &["ron", "rum"]),
    ("ru", "Russian", &["rus"]),
    ("sr", "Serbian", &["srp"]),
    ("si", "Sinhala", &["sin"]),
    ("sk", "Slovak", &["slk", "slo"]),
    ("sl", "Slovenian", &["slv"]),
    ("es", "Spanish", &["spa", "castilian", "latino"]),
    ("sw", "Swahili", &["swa"]),
    ("sv", "Swedish", &["swe"]),
    ("ta", "Tamil", &["tam"]),
    ("te", "Telugu", &["tel"]),
    ("th", "Thai", &["tha"]),
    ("tr", "Turkish", &["tur"]),
    ("uk", "Ukrainian", &["ukr"]),
    ("ur", "Urdu", &["urd"]),
    ("uz", "Uzbek", &["uzb"]),
    ("vi", "Vietnamese", &["vie"]),
    ("cy", "Welsh", &["cym", "wel"]),
];

/// The ISO 639-1 code of one word of a tag: a code (`ar`, `ara`) or an
/// English name (`Arabic`), any case.
fn word_code(word: &str) -> Option<&'static str> {
    LANGUAGES
        .iter()
        .find(|(code, name, other)| {
            word.eq_ignore_ascii_case(code)
                || word.eq_ignore_ascii_case(name)
                || other.iter().any(|o| word.eq_ignore_ascii_case(o))
        })
        .map(|(code, _, _)| *code)
}

/// The English name for an ISO 639-1 code.
pub fn name(code: &str) -> Option<&'static str> {
    LANGUAGES
        .iter()
        .find(|(c, _, _)| *c == code)
        .map(|(_, name, _)| *name)
}

/// What a subtitle or audio tag says: `de.forced`, `en.sdh`, `pt-BR`,
/// `English`, or a container's `ara`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Tag {
    /// ISO 639-1 code; `None` when no word of the tag is a language.
    pub code: Option<&'static str>,
    /// Only the parts in another language (signs, foreign dialogue).
    pub forced: bool,
    /// For the deaf and hard of hearing: describes sounds too.
    pub sdh: bool,
}

impl Tag {
    /// The first word that names a language is the language; the words
    /// after it are flags (`hi` after a language is hearing impaired, not
    /// Hindi).
    pub fn parse(tag: &str) -> Tag {
        let mut out = Tag::default();
        for word in tag
            .split(['.', '_', '-', ' ', '(', ')', '[', ']'])
            .filter(|w| !w.is_empty())
        {
            let w = word.to_ascii_lowercase();
            if w == "forced" {
                out.forced = true;
            } else if matches!(w.as_str(), "sdh" | "cc") || (out.code.is_some() && w == "hi") {
                out.sdh = true;
            } else if out.code.is_none() {
                out.code = word_code(&w);
            }
        }
        out
    }

    /// The language's English name.
    pub fn name(&self) -> Option<&'static str> {
        self.code.and_then(name)
    }
}

/// The language of a locale such as `LANG` (`en_US.UTF-8`, `pt_BR`).
pub fn from_locale(locale: &str) -> Option<&'static str> {
    let lang = locale.split(['_', '-', '.', '@']).next()?;
    word_code(lang)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codes_and_names() {
        for (tag, code) in [
            ("ara", "ar"),
            ("ar", "ar"),
            ("Arabic", "ar"),
            ("ger", "de"),
            ("deu", "de"),
            ("pt-BR", "pt"),
            ("zh-Hans", "zh"),
            ("chi", "zh"),
            ("nob", "no"),
            ("ENG", "en"),
        ] {
            assert_eq!(Tag::parse(tag).code, Some(code), "{tag}");
        }
        assert_eq!(Tag::parse("und").code, None);
        assert_eq!(Tag::parse("").code, None);
        assert_eq!(Tag::parse("ara").name(), Some("Arabic"));
        assert_eq!(name("de"), Some("German"));
    }

    #[test]
    fn flags() {
        let t = Tag::parse("de.forced");
        assert_eq!((t.code, t.forced, t.sdh), (Some("de"), true, false));
        let t = Tag::parse("en.hi");
        assert_eq!((t.code, t.forced, t.sdh), (Some("en"), false, true));
        let t = Tag::parse("hi");
        assert_eq!((t.code, t.sdh), (Some("hi"), false));
        let t = Tag::parse("English (Forced)");
        assert_eq!((t.code, t.forced), (Some("en"), true));
        let t = Tag::parse("forced");
        assert_eq!((t.code, t.forced), (None, true));
        let t = Tag::parse("sdh");
        assert_eq!((t.code, t.sdh), (None, true));
    }

    #[test]
    fn locales() {
        assert_eq!(from_locale("en_US.UTF-8"), Some("en"));
        assert_eq!(from_locale("pt_BR"), Some("pt"));
        assert_eq!(from_locale("C"), None);
        assert_eq!(from_locale(""), None);
    }

    #[test]
    fn table_is_consistent() {
        for (i, (code, name, _)) in LANGUAGES.iter().enumerate() {
            assert_eq!(code.len(), 2, "{code}");
            assert_eq!(word_code(code), Some(*code), "{code}");
            assert_eq!(word_code(name), Some(*code), "{name}");
            if i > 0 {
                assert!(LANGUAGES[i - 1].1 < *name, "{name} out of order");
            }
        }
    }
}
