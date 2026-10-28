//! What feeds list: scenes, their encodings and details, and a whole
//! feed's listing as cached between visits.

use crate::error::{Error, Result};
use crate::transcode::Transcode;
use fp_core::format::{VideoFormat, detect_from_name};
use fp_core::source::{Entry, EntryKind, SubtitleFile};
use serde::{Deserialize, Serialize};
use std::path::Path;

/// One encoding of a scene.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MediaSource {
    /// Absolute media URL.
    pub url: String,
    /// Frame height in pixels, when known.
    pub height: Option<u32>,
    /// Frame width in pixels, when known.
    pub width: Option<u32>,
    /// Encoding name (`h265`, `h264`, ...).
    pub label: String,
    /// File size, when the feed says.
    pub size: Option<u64>,
}

impl MediaSource {
    /// A copy the server transcodes as it streams (an encoding named
    /// "transcoding") rather than the file itself.
    pub fn is_transcode(&self) -> bool {
        self.label.to_ascii_lowercase().contains("transcod")
    }

    /// A transcode the server starts at a time, so it seeks (see
    /// [`takes_start`]).
    pub fn takes_start(&self) -> bool {
        self.is_transcode() && takes_start(&self.url)
    }
}

/// A transcode address the server starts at a time given as
/// `start=<seconds>`: `…/stream.mp4?resolution=FULL_HD`.
pub(crate) fn takes_start(url: &str) -> bool {
    let (path, query) = url.split_once('?').unwrap_or((url, ""));
    let file = path.rsplit('/').next().unwrap_or_default();
    file.starts_with("stream.")
        && query
            .split('&')
            .any(|p| p.to_ascii_lowercase().starts_with("resolution="))
}

/// Everything a feed says about one scene.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SceneInfo {
    pub title: String,
    /// Length in seconds.
    pub duration: Option<f64>,
    /// Poster image URL.
    pub thumbnail_url: Option<String>,
    /// Projection and stereo layout the feed declares.
    pub format: Option<VideoFormat>,
    /// Available encodings.
    pub sources: Vec<MediaSource>,
    /// Haptic script URLs.
    pub scripts: Vec<String>,
    pub subtitles: Vec<SubtitleFile>,
    /// Named timestamps in seconds.
    pub markers: Vec<(f64, String)>,
    pub description: Option<String>,
    /// Release date, Unix seconds.
    pub released: Option<i64>,
    /// When the server added it, Unix seconds.
    pub added: Option<i64>,
    /// Studio or site.
    pub studio: Option<String>,
    pub actors: Vec<String>,
    /// Categories and tags.
    pub tags: Vec<String>,
    /// Rating out of 5.
    pub rating: Option<f32>,
    /// The user's favourite on the server; `None` when it doesn't say.
    pub favorite: Option<bool>,
    /// Where the server takes playback events (HereSphere's `eventServer`).
    pub event_server: Option<String>,
    /// The server takes changes to [`Self::favorite`] (HereSphere's
    /// `writeFavorite`).
    pub write_favorite: bool,
}

impl SceneInfo {
    /// The tallest file not above `max_height`; when every file is taller,
    /// the smallest one. Sources of unknown height rank lowest. The
    /// server's transcodes only play when it lists no file (the player
    /// moves to one when the file can't play, or not smoothly).
    pub fn best_source(&self, max_height: Option<u32>) -> Option<&MediaSource> {
        let h = |s: &MediaSource| s.height.unwrap_or(0);
        let fits = |s: &&MediaSource| max_height.is_none_or(|m| h(s) <= m);
        let pick = |transcodes: bool| {
            let of_kind = || {
                self.sources
                    .iter()
                    .filter(move |s| s.is_transcode() == transcodes)
            };
            // `max_by_key` keeps the last of equal keys; reverse to prefer
            // the first listed encoding among equals.
            of_kind()
                .rev()
                .filter(fits)
                .max_by_key(|s| h(s))
                .or_else(|| of_kind().min_by_key(|s| h(s)))
        };
        pick(false).or_else(|| pick(true))
    }

    /// The server's transcodes, tallest first, one per height (the first
    /// listed of an MP4 and a WebM, which takes servers far longer).
    pub fn transcodes(&self) -> Vec<Transcode> {
        let mut found: Vec<&MediaSource> =
            self.sources.iter().filter(|s| s.is_transcode()).collect();
        let webm = |s: &MediaSource| s.url.to_ascii_lowercase().contains(".webm");
        found.sort_by_key(|s| (std::cmp::Reverse(s.height.unwrap_or(0)), webm(s)));
        found.dedup_by(|a, b| a.height == b.height && a.height.is_some() || a.url == b.url);
        found
            .into_iter()
            .map(|s| Transcode {
                id: s.url.clone(),
                label: crate::transcode::height_label(s.height),
                height: s.height,
                seekable: s.takes_start(),
            })
            .collect()
    }

    /// Copies the scene's details into a listing entry (keeping its
    /// location).
    pub fn apply_to(&self, entry: &mut Entry) {
        if !self.title.is_empty() {
            entry.name = self.title.clone();
        }
        entry.kind = EntryKind::Video;
        entry.duration = self.duration.or(entry.duration);
        if self.thumbnail_url.is_some() {
            entry.thumbnail_url = self.thumbnail_url.clone();
        }
        entry.format = self
            .format
            .or_else(|| detect_from_name(&entry.name))
            .or(entry.format);
        if entry.size.is_none() {
            entry.size = self.best_source(None).and_then(|s| s.size);
        }
        entry.scripts = self.scripts.clone();
        entry.subtitles = self.subtitles.clone();
        entry.markers = self.markers.clone();
        if self.description.is_some() {
            entry.description = self.description.clone();
        }
        if !self.tags.is_empty() {
            entry.tags = self.tags.clone();
        }
    }
}

/// A group of scenes in a feed's root.
#[derive(Clone, Debug, PartialEq)]
pub struct SceneGroup {
    /// Group name ("Recent", "Favourites", ...).
    pub name: String,
    /// Scenes, as video entries located at their scene URL.
    pub scenes: Vec<Entry>,
}

/// A scene as a feed's grid shows it: what the listing says, completed as
/// its details arrive ([`FeedScene::apply`]).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct FeedScene {
    /// Scene URL: the location to open and fetch details from.
    pub url: String,
    pub title: String,
    /// Length in seconds.
    pub duration: Option<f64>,
    /// Poster image URL.
    pub thumbnail_url: Option<String>,
    /// Release date, Unix seconds.
    pub released: Option<i64>,
    /// When the server added it, Unix seconds.
    pub added: Option<i64>,
    /// Rating out of 5.
    pub rating: Option<f32>,
    /// The user's favourite on the server.
    pub favorite: bool,
    /// Studio or site.
    pub studio: Option<String>,
    pub actors: Vec<String>,
    /// Categories and tags.
    pub tags: Vec<String>,
    /// Has haptic scripts.
    pub scripted: bool,
    /// Declared projection and stereo layout.
    pub format: Option<VideoFormat>,
    /// The scene's own document was read (so nothing more will come).
    pub detailed: bool,
    /// Only its address is known (a HereSphere listing, before its scan or
    /// details): the title is the address's last part.
    pub bare: bool,
}

impl FeedScene {
    fn from_entry(e: &Entry) -> FeedScene {
        FeedScene {
            url: e.location.clone(),
            title: e.name.clone(),
            duration: e.duration,
            thumbnail_url: e.thumbnail_url.clone(),
            format: e.format,
            scripted: !e.scripts.is_empty(),
            ..FeedScene::default()
        }
    }

    /// Completes the card with a scene's details.
    pub fn apply(&mut self, info: &SceneInfo) {
        if !info.title.is_empty() {
            self.title = info.title.clone();
        }
        self.duration = info.duration.or(self.duration);
        if info.thumbnail_url.is_some() {
            self.thumbnail_url = info.thumbnail_url.clone();
        }
        self.format = info
            .format
            .or(self.format)
            .or_else(|| detect_from_name(&self.title));
        self.released = info.released.or(self.released);
        self.added = info.added.or(self.added);
        self.rating = info.rating.or(self.rating);
        if let Some(f) = info.favorite {
            self.favorite = f;
        }
        if info.studio.is_some() {
            self.studio = info.studio.clone();
        }
        if !info.actors.is_empty() {
            self.actors = info.actors.clone();
        }
        if !info.tags.is_empty() {
            self.tags = info.tags.clone();
        }
        self.scripted |= !info.scripts.is_empty();
        self.detailed = true;
        self.bare = false;
    }

    /// Takes what a scan learned (titles, dates, tags) without its gaps.
    pub fn merge(&mut self, scan: FeedScene) {
        self.bare = false;
        if !scan.title.is_empty() {
            self.title = scan.title;
        }
        self.duration = scan.duration.or(self.duration);
        self.released = scan.released.or(self.released);
        self.added = scan.added.or(self.added);
        self.rating = scan.rating.or(self.rating);
        self.favorite |= scan.favorite;
        self.studio = scan.studio.or(self.studio.take());
        if !scan.actors.is_empty() {
            self.actors = scan.actors;
        }
        if !scan.tags.is_empty() {
            self.tags = scan.tags;
        }
        self.format = self.format.or(scan.format);
    }

    /// A browse entry to open it with.
    pub fn to_entry(&self) -> Entry {
        let mut e = Entry::new(self.title.clone(), self.url.clone(), EntryKind::Video);
        e.duration = self.duration;
        e.thumbnail_url = self.thumbnail_url.clone();
        e.format = self.format;
        e.modified = self.released.or(self.added);
        e
    }

    /// Whether every word (lower case) is in the title, studio, a
    /// performer or a tag.
    pub fn matches(&self, words: &[String]) -> bool {
        let hay = [
            self.title.as_str(),
            self.studio.as_deref().unwrap_or(""),
            &self.actors.join(" "),
            &self.tags.join(" "),
        ]
        .join(" ")
        .to_lowercase();
        words.iter().all(|w| hay.contains(w.as_str()))
    }
}

/// Everything a feed lists: its scene groups and their scenes.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct FeedLibrary {
    /// Groups in feed order, each with the URLs of its scenes.
    pub groups: Vec<(String, Vec<String>)>,
    /// Every scene once, in the order first listed.
    pub scenes: Vec<FeedScene>,
}

impl FeedLibrary {
    /// Keeps the details read before (`saved`'s detailed scenes) of the
    /// scenes still listed, which the server's listing doesn't carry.
    pub fn keep_details(&mut self, saved: &FeedLibrary) {
        let detailed: std::collections::HashMap<&str, &FeedScene> = saved
            .scenes
            .iter()
            .filter(|s| s.detailed)
            .map(|s| (s.url.as_str(), s))
            .collect();
        for s in &mut self.scenes {
            if !s.detailed
                && let Some(old) = detailed.get(s.url.as_str())
            {
                *s = (*old).clone();
            }
        }
    }

    /// Takes a HereSphere scan into the scenes it describes.
    pub fn merge_scan(&mut self, scan: Vec<FeedScene>) {
        let mut scanned: std::collections::HashMap<String, FeedScene> =
            scan.into_iter().map(|s| (s.url.clone(), s)).collect();
        for s in &mut self.scenes {
            if let Some(found) = scanned.remove(&s.url) {
                s.merge(found);
            }
        }
    }

    /// Where source `source_id`'s last listing is kept, to show at once
    /// the next time it is opened.
    pub fn cache_path(source_id: &str) -> std::path::PathBuf {
        let name: String = source_id
            .chars()
            .map(|c| match c {
                'a'..='z' | 'A'..='Z' | '0'..='9' | '-' | '_' => c,
                _ => '_',
            })
            .collect();
        fp_core::dirs::cache_dir()
            .join("feeds")
            .join(format!("{name}.json"))
    }

    /// A listing saved by [`FeedLibrary::save`]; `None` when there is none
    /// (or it can't be read).
    pub fn load(path: &Path) -> Option<FeedLibrary> {
        let data = std::fs::read(path).ok()?;
        serde_json::from_slice(&data)
            .map_err(|e| log::debug!("feed listing {}: {e}", path.display()))
            .ok()
    }

    /// Saves the listing readable only by the user: its poster and scene
    /// URLs can carry the server's API key.
    pub fn save(&self, path: &Path) -> Result<()> {
        let json = serde_json::to_vec(self)
            .map_err(|e| Error::Config(format!("cannot serialize the feed listing: {e}")))?;
        crate::config::write_private(path, &json)?;
        Ok(())
    }

    pub(super) fn from_groups(groups: Vec<SceneGroup>) -> FeedLibrary {
        let mut seen = std::collections::HashSet::new();
        let mut scenes = Vec::new();
        let groups = groups
            .into_iter()
            .map(|g| {
                let urls = g
                    .scenes
                    .iter()
                    .map(|e| {
                        if seen.insert(e.location.clone()) {
                            scenes.push(FeedScene::from_entry(e));
                        }
                        e.location.clone()
                    })
                    .collect();
                (g.name, urls)
            })
            .collect();
        FeedLibrary { groups, scenes }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn details_set_the_favourite_only_when_they_say() {
        let mut s = FeedScene {
            favorite: true,
            ..FeedScene::default()
        };
        s.apply(&SceneInfo::default());
        assert!(s.favorite, "details that don't say keep the listing's");
        // Unmarked since the listing was made.
        s.apply(&SceneInfo {
            favorite: Some(false),
            ..SceneInfo::default()
        });
        assert!(!s.favorite);
    }

    #[test]
    fn details_read_before_outlast_a_new_listing() {
        let scene = |url: &str, detailed: bool| FeedScene {
            url: url.into(),
            title: url.into(),
            tags: if detailed { vec!["Sun".into()] } else { vec![] },
            detailed,
            ..FeedScene::default()
        };
        let saved = FeedLibrary {
            groups: vec![],
            scenes: vec![scene("a", true), scene("b", false), scene("gone", true)],
        };
        let mut fresh = FeedLibrary {
            groups: vec![],
            scenes: vec![scene("a", false), scene("b", false), scene("new", false)],
        };
        fresh.keep_details(&saved);
        let tags: Vec<(&str, usize)> = fresh
            .scenes
            .iter()
            .map(|s| (s.url.as_str(), s.tags.len()))
            .collect();
        assert_eq!(tags, [("a", 1), ("b", 0), ("new", 0)]);
    }

    #[test]
    fn listings_take_a_scan_and_save() {
        let scene = |n: &str| Entry::new(n, format!("http://x/heresphere/{n}"), EntryKind::Video);
        let mut lib = FeedLibrary::from_groups(vec![SceneGroup {
            name: "Recent".into(),
            scenes: vec![scene("12"), scene("13")],
        }]);
        for s in &mut lib.scenes {
            s.bare = true;
        }
        lib.merge_scan(vec![FeedScene {
            url: "http://x/heresphere/12".into(),
            title: "Beach".into(),
            tags: vec!["Sun".into()],
            ..FeedScene::default()
        }]);
        assert!(!lib.scenes[0].bare && lib.scenes[0].title == "Beach");
        assert!(lib.scenes[1].bare && lib.scenes[1].title == "13");

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("feeds").join("x.json");
        lib.save(&path).unwrap();
        assert_eq!(FeedLibrary::load(&path).as_ref(), Some(&lib));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600, "poster URLs can carry an API key");
        }
        assert_eq!(FeedLibrary::load(&dir.path().join("none.json")), None);
        std::fs::write(&path, b"not json").unwrap();
        assert_eq!(FeedLibrary::load(&path), None);
    }
}
