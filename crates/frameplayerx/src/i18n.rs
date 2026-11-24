//! The interface's language. Its messages are Fluent files built in
//! (`i18n/<language>/frameplayerx.ftl`), English the one every other
//! language translates: a message a language lacks shows in English. `tr!`
//! looks one up, its id and arguments checked against the English file
//! when the app is built.

use fp_core::sync::lock;
use i18n_embed::LanguageLoader;
use i18n_embed::fluent::FluentLanguageLoader;
use rust_embed::RustEmbed;
use std::borrow::Cow;
use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};

#[derive(RustEmbed)]
#[folder = "i18n/"]
struct Messages;

/// The languages the interface comes in, each named in itself.
pub const LANGUAGES: [(&str, &str); 6] = [
    ("en", "English"),
    ("de", "Deutsch"),
    ("es", "Español"),
    ("fr", "Français"),
    ("ja", "日本語"),
    ("zh-CN", "简体中文"),
];

/// A made-up language for the preview: English with its letters accented
/// and its vowels doubled, so text left untranslated stands out and text
/// that only just fits in English shows where it won't fit in German.
pub const PSEUDO: &str = "pseudo";

pub static LOADER: LazyLock<FluentLanguageLoader> = LazyLock::new(|| {
    let loader = FluentLanguageLoader::new("frameplayerx", "en".parse().expect("a language id"));
    loader
        .load_fallback_language(&Messages)
        .expect("the English messages are built in");
    loader.set_use_isolating(false);
    loader
});

/// Messages without arguments in the current language, by id.
static CACHE: LazyLock<Mutex<HashMap<&'static str, &'static str>>> =
    LazyLock::new(Default::default);

/// The language shown, one of [`LANGUAGES`] or [`PSEUDO`].
static CURRENT: Mutex<&'static str> = Mutex::new("en");

/// A message in the interface's language: `tr!("id")` as a `&'static str`,
/// or `tr!("id", name = value, ...)` with its arguments, as a `String`.
macro_rules! tr {
    ($id:literal) => {
        $crate::i18n::cached($id, || i18n_embed_fl::fl!($crate::i18n::LOADER, $id))
    };
    ($id:literal, $($args:tt)+) => {
        i18n_embed_fl::fl!($crate::i18n::LOADER, $id, $($args)+)
    };
}

/// The message `id`, made by `make` once per language. Kept for the app's
/// life, so a label can be a `&'static str`; a language change leaves the
/// old ones behind, a few kilobytes each time.
pub fn cached(id: &'static str, make: impl FnOnce() -> String) -> &'static str {
    let mut cache = lock(&CACHE);
    if let Some(s) = cache.get(id) {
        return s;
    }
    let s: &'static str = Box::leak(make().into_boxed_str());
    cache.insert(id, s);
    s
}

/// Shows the interface in `code`, one of [`LANGUAGES`] or [`PSEUDO`];
/// anything else is English.
pub fn set_language(code: &str) {
    let code = LANGUAGES
        .iter()
        .map(|l| l.0)
        .chain([PSEUDO])
        .find(|c| *c == code)
        .unwrap_or("en");
    let id = if code == PSEUDO { "en" } else { code };
    if let Err(e) = LOADER.load_languages(&Messages, &[id.parse().expect("a language id")]) {
        log::warn!("interface language {code}: {e}");
    }
    // egui draws the marks Fluent puts round arguments for mixed
    // directions as boxes.
    LOADER.set_use_isolating(false);
    if code == PSEUDO {
        LOADER.with_bundles_mut(|b| b.set_transform(Some(pseudo)));
    }
    lock(&CACHE).clear();
    *lock(&CURRENT) = code;
}

/// The language shown, one of [`LANGUAGES`] or [`PSEUDO`].
pub fn current() -> &'static str {
    *lock(&CURRENT)
}

/// What Automatic shows: [`system_language`] when the interface comes in
/// it, else English.
pub fn automatic() -> &'static str {
    let Some(lang) = system_language() else {
        return "en";
    };
    let base = lang.split(['-', '_', '.']).next();
    LANGUAGES
        .iter()
        .map(|l| l.0)
        .find(|c| *c == lang)
        .or_else(|| {
            LANGUAGES
                .iter()
                .map(|l| l.0)
                .find(|c| c.split('-').next() == base)
        })
        .unwrap_or("en")
}

/// The headset's language: the one Steam is set to (SteamOS's own locale
/// stays en_US whatever it is), else the system's (`LANG`).
pub fn system_language() -> Option<String> {
    let home = std::env::var_os("HOME");
    let vdf = home.and_then(|h| {
        std::fs::read_to_string(std::path::Path::new(&h).join(".steam/registry.vdf")).ok()
    });
    vdf.as_deref()
        .and_then(steam_code)
        .map(str::to_string)
        .or_else(|| std::env::var("LANG").ok())
}

/// The language a Steam `registry.vdf` names, as a language tag.
fn steam_code(vdf: &str) -> Option<&'static str> {
    let name = vdf.lines().find_map(|l| {
        let mut parts = l.split('"').filter(|p| !p.trim().is_empty());
        (parts.next()? == "language")
            .then(|| parts.next())
            .flatten()
    })?;
    // Steam's own names for the languages it comes in.
    Some(match name {
        "arabic" => "ar",
        "brazilian" => "pt-BR",
        "bulgarian" => "bg",
        "czech" => "cs",
        "danish" => "da",
        "dutch" => "nl",
        "english" => "en",
        "finnish" => "fi",
        "french" => "fr",
        "german" => "de",
        "greek" => "el",
        "hungarian" => "hu",
        "indonesian" => "id",
        "italian" => "it",
        "japanese" => "ja",
        "koreana" => "ko",
        "latam" => "es-419",
        "norwegian" => "nb",
        "polish" => "pl",
        "portuguese" => "pt",
        "romanian" => "ro",
        "russian" => "ru",
        "schinese" => "zh-CN",
        "spanish" => "es",
        "swedish" => "sv",
        "tchinese" => "zh-TW",
        "thai" => "th",
        "turkish" => "tr",
        "ukrainian" => "uk",
        "vietnamese" => "vi",
        _ => return None,
    })
}

/// [`PSEUDO`]'s text.
pub(crate) fn pseudo(s: &str) -> Cow<'_, str> {
    let mut out = String::with_capacity(s.len() * 2);
    for c in s.chars() {
        let accented = match c {
            'a' => 'á',
            'e' => 'é',
            'i' => 'í',
            'o' => 'ö',
            'u' => 'ü',
            'y' => 'ý',
            'c' => 'ç',
            'n' => 'ñ',
            's' => 'š',
            'z' => 'ž',
            'A' => 'Å',
            'E' => 'É',
            'I' => 'Î',
            'O' => 'Ö',
            'U' => 'Ü',
            'C' => 'Ç',
            'N' => 'Ñ',
            'S' => 'Š',
            other => other,
        };
        out.push(accented);
        if "aeiouAEIOU".contains(c) {
            out.push(accented);
        }
    }
    Cow::Owned(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use fluent_syntax::ast::{Entry, Expression, InlineExpression, Pattern, PatternElement};
    use std::collections::BTreeSet;

    /// A language's messages, each with the variables it uses.
    fn messages(lang: &str) -> HashMap<String, BTreeSet<String>> {
        let path = format!("{lang}/frameplayerx.ftl");
        let file = Messages::get(&path).unwrap_or_else(|| panic!("{path} is built in"));
        let text = std::str::from_utf8(&file.data).expect("UTF-8");
        let resource = fluent_syntax::parser::parse(text)
            .unwrap_or_else(|(_, errors)| panic!("{path} doesn't parse: {errors:?}"));
        resource
            .body
            .iter()
            .filter_map(|e| match e {
                Entry::Message(m) => {
                    let mut vars = BTreeSet::new();
                    if let Some(p) = &m.value {
                        pattern_variables(p, &mut vars);
                    }
                    Some((m.id.name.to_string(), vars))
                }
                _ => None,
            })
            .collect()
    }

    fn pattern_variables(p: &Pattern<&str>, out: &mut BTreeSet<String>) {
        for e in &p.elements {
            if let PatternElement::Placeable { expression } = e {
                expression_variables(expression, out);
            }
        }
    }

    fn expression_variables(e: &Expression<&str>, out: &mut BTreeSet<String>) {
        match e {
            Expression::Select { selector, variants } => {
                inline_variables(selector, out);
                for v in variants {
                    pattern_variables(&v.value, out);
                }
            }
            Expression::Inline(i) => inline_variables(i, out),
        }
    }

    fn inline_variables(i: &InlineExpression<&str>, out: &mut BTreeSet<String>) {
        match i {
            InlineExpression::VariableReference { id } => {
                out.insert(id.name.to_string());
            }
            InlineExpression::Placeable { expression } => expression_variables(expression, out),
            InlineExpression::FunctionReference { arguments, .. } => {
                for a in &arguments.positional {
                    inline_variables(a, out);
                }
                for a in &arguments.named {
                    inline_variables(&a.value, out);
                }
            }
            _ => {}
        }
    }

    // A message a translation lacks shows in English, so a missing one is
    // only counted; one English doesn't have, or a variable the app doesn't
    // pass, is a mistake.
    #[test]
    fn every_translation_fits_the_english() {
        let english = messages("en");
        for (code, _) in LANGUAGES {
            let path = format!("{code}/frameplayerx.ftl");
            assert!(Messages::get(&path).is_some(), "{path} is missing");
        }
        let mut coverage = Vec::new();
        for path in Messages::iter() {
            let lang = path.split('/').next().expect("a language folder");
            if lang == "en" {
                continue;
            }
            assert!(
                LANGUAGES.iter().any(|l| l.0 == lang),
                "{lang} is built in but not in LANGUAGES, so it can't be chosen"
            );
            let translated = messages(lang);
            for (id, vars) in &translated {
                let Some(en) = english.get(id) else {
                    panic!("{lang}: {id} isn't an English message");
                };
                let unknown: Vec<_> = vars.difference(en).collect();
                assert!(
                    unknown.is_empty(),
                    "{lang}: {id} uses {unknown:?}, which English doesn't"
                );
            }
            let done = english
                .keys()
                .filter(|id| translated.contains_key(*id))
                .count();
            coverage.push(format!("{lang} {done}/{}", english.len()));
        }
        println!("translated: {}", coverage.join(", "));
    }

    #[test]
    fn steam_names_its_language() {
        let vdf = "\"Registry\"\n{\n\t\"HKCU\"\n\t{\n\t\t\"Software\"\n\t\t{\n\t\t\t\"Valve\"\n\
                   \t\t\t{\n\t\t\t\t\"Steam\"\n\t\t\t\t{\n\t\t\t\t\t\"language\"\t\t\"schinese\"\n";
        assert_eq!(steam_code(vdf), Some("zh-CN"));
        assert_eq!(steam_code("\t\"language\"\t\t\"koreana\""), Some("ko"));
        assert_eq!(steam_code("\t\"languages\"\t\t\"english\""), None);
    }

    #[test]
    fn the_made_up_language_stands_out() {
        assert_eq!(pseudo("Settings"), "Šééttííñgš");
    }
}
