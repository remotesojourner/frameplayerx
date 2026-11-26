//! What's New: after an upgrade, what changed since the version last run,
//! in plain words. Each change is a file in `whats-new/` holding its kind
//! and its line in every language. The build is told which version first
//! shipped each one (`FP_WHATS_NEW`, from tools/whats-new.sh), so an
//! upgrade lists only what the version before it didn't have.

use fp_updater::Version;
use rust_embed::RustEmbed;
use serde::Deserialize;
use std::collections::HashMap;

#[derive(RustEmbed)]
#[folder = "whats-new/"]
struct Changes;

/// A line per change, its name and the version that first shipped it,
/// oldest first. Empty in a build made without tools/whats-new.sh.
const SHIPPED: &str = match option_env!("FP_WHATS_NEW") {
    Some(s) => s,
    None => "",
};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
enum Kind {
    Feat,
    Fix,
    Perf,
}

#[derive(Deserialize)]
struct Change {
    kind: Kind,
    /// The line in each language, by code.
    #[serde(flatten)]
    text: HashMap<String, String>,
}

/// What changed from one version to another, each kind's lines oldest
/// first.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct WhatsNew {
    /// The version run before, when known.
    pub from: Option<String>,
    pub to: String,
    pub features: Vec<String>,
    pub fixes: Vec<String>,
    pub performance: Vec<String>,
}

/// What this build has that came after `seen`, the newest version run
/// before; everything up to this build when that isn't known. None when
/// nothing did.
pub fn since(seen: Option<&Version>, current: &Version) -> Option<WhatsNew> {
    between(SHIPPED, seen, current, crate::i18n::current())
}

/// Every change this build holds, as if all were new: the preview's.
pub fn everything(current: &Version) -> Option<WhatsNew> {
    let shipped: String = Changes::iter()
        .filter_map(|f| Some(format!("{} {current}\n", f.strip_suffix(".json")?)))
        .collect();
    between(&shipped, None, current, crate::i18n::current())
}

fn between(
    shipped: &str,
    seen: Option<&Version>,
    current: &Version,
    language: &str,
) -> Option<WhatsNew> {
    let mut new = WhatsNew {
        from: seen.map(Version::to_string),
        to: current.to_string(),
        ..WhatsNew::default()
    };
    for line in shipped.lines() {
        let Some((name, version)) = line.split_once(' ') else {
            continue;
        };
        let Ok(version) = Version::parse(version.trim()) else {
            continue;
        };
        let after_seen = seen.is_none_or(|s| version.cmp_precedence(s).is_gt());
        if !after_seen || version.cmp_precedence(current).is_gt() {
            continue;
        }
        let Some(change) = change(name) else {
            continue;
        };
        let text = text(&change, language);
        match change.kind {
            Kind::Feat => new.features.push(text),
            Kind::Fix => new.fixes.push(text),
            Kind::Perf => new.performance.push(text),
        }
    }
    let empty = new.features.is_empty() && new.fixes.is_empty() && new.performance.is_empty();
    (!empty).then_some(new)
}

fn change(name: &str) -> Option<Change> {
    let file = Changes::get(&format!("{name}.json"))?;
    serde_json::from_slice(&file.data)
        .inspect_err(|e| log::warn!("whats-new/{name}.json: {e}"))
        .ok()
}

/// The change's line in `language`, else in English.
fn text(change: &Change, language: &str) -> String {
    let english = change.text.get("en").map_or("", String::as_str);
    if language == crate::i18n::PSEUDO {
        return crate::i18n::pseudo(english).into_owned();
    }
    change
        .text
        .get(language)
        .map_or(english, String::as_str)
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(s: &str) -> Version {
        Version::parse(s).unwrap()
    }

    /// Two real changes, one a feature and one a fix, shipped in the
    /// versions given.
    fn shipped(downloads: &str, removed_source: &str) -> String {
        format!("downloads {downloads}\nremoved-source-videos {removed_source}\n")
    }

    #[test]
    fn every_change_has_a_kind_and_its_line_in_every_language() {
        let names: Vec<_> = Changes::iter().collect();
        assert!(!names.is_empty());
        for file in names {
            let name = file.strip_suffix(".json").expect("a .json file");
            assert!(
                name.chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-'),
                "{file}: its name goes on one line of FP_WHATS_NEW"
            );
            let change = change(name).unwrap_or_else(|| panic!("{file} doesn't parse"));
            for (code, _) in crate::i18n::LANGUAGES {
                let text = change.text.get(code).map_or("", String::as_str);
                assert!(!text.trim().is_empty(), "{file} has no {code} line");
            }
            assert_eq!(
                change.text.len(),
                crate::i18n::LANGUAGES.len(),
                "{file} has a language the interface doesn't"
            );
        }
    }

    #[test]
    fn an_upgrade_lists_only_what_came_after_the_version_run_before() {
        let list = shipped("0.21.0-beta.2", "0.21.0-beta.1");
        // From the beta before: only the newer beta's change.
        let new = between(&list, Some(&v("0.21.0-beta.1")), &v("0.21.0-beta.2"), "en").unwrap();
        assert_eq!(new.features.len(), 1);
        assert!(new.fixes.is_empty());
        assert_eq!(new.from.as_deref(), Some("0.21.0-beta.1"));
        // From the stable before: every beta's changes.
        let new = between(&list, Some(&v("0.20.0")), &v("0.21.0"), "en").unwrap();
        assert_eq!((new.features.len(), new.fixes.len()), (1, 1));
        // From the last beta to its stable release: nothing new.
        assert_eq!(
            between(&list, Some(&v("0.21.0-beta.2")), &v("0.21.0"), "en"),
            None
        );
        // A change shipped after this build isn't in it.
        assert_eq!(
            between(&list, Some(&v("0.20.0")), &v("0.21.0-beta.1"), "en").map(|n| n.features.len()),
            Some(0)
        );
    }

    #[test]
    fn a_version_not_known_gets_everything_up_to_this_build() {
        let list = shipped("0.21.0-beta.2", "0.21.0-beta.1");
        let new = between(&list, None, &v("0.21.0-beta.2"), "en").unwrap();
        assert_eq!((new.features.len(), new.fixes.len()), (1, 1));
        assert_eq!(new.from, None);
    }

    #[test]
    fn build_metadata_doesnt_make_a_version_newer() {
        let list = shipped("0.21.0-beta.2", "0.21.0-beta.1");
        assert_eq!(
            between(
                &list,
                Some(&v("0.21.0-beta.2")),
                &v("0.21.0-beta.2+branch"),
                "en"
            ),
            None
        );
    }

    #[test]
    fn lines_come_in_the_interface_language_else_english() {
        let list = shipped("0.21.0", "0.21.0");
        let de = between(&list, None, &v("0.21.0"), "de").unwrap();
        let en = between(&list, None, &v("0.21.0"), "en").unwrap();
        assert_ne!(de.features, en.features);
        let unknown = between(&list, None, &v("0.21.0"), "xx").unwrap();
        assert_eq!(unknown.features, en.features);
    }

    #[test]
    fn a_name_without_a_file_or_a_bad_line_is_left_out() {
        let list = "gone 0.21.0\nnonsense\ndownloads not-a-version\ndownloads 0.21.0\n";
        let new = between(list, None, &v("0.21.0"), "en").unwrap();
        assert_eq!(new.features.len(), 1);
    }
}
