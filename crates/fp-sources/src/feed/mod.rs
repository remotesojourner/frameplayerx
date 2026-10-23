//! VR scenes published as JSON through the DeoVR feed or the HereSphere
//! API, by any server that offers either.
//!
//! Both map to the same shape: the root lists scene groups as folders, a
//! group lists its scenes as videos, and [`Source::details`] fetches one
//! scene's full description (declared projection and stereo layout,
//! scripts, subtitles, markers). [`Source::open`] resolves a scene to its
//! best media URL (the tallest encoding not above
//! [`FeedConfig::max_height`]) and reads it over HTTP.
//!
//! Parsing is tolerant: numbers may arrive as strings, unknown fields are
//! ignored and missing ones fall back to sensible defaults.
//!
//! Locations: group folders are `<feed url>#group=<encoded name>`, scenes
//! are their scene/API URL, scripts and subtitles their direct URL.

mod deovr;
mod heresphere;
mod scene;

pub use scene::{FeedLibrary, FeedScene, MediaSource, SceneGroup, SceneInfo};

use crate::config::{Credentials, FeedConfig, SourceKind};
use crate::error::{Error, Result};
use crate::http::{HttpClient, HttpOptions, HttpReply};
use crate::httpdir::clean_url_and_credentials;
use crate::json::{array, get, num, text, uint};
use crate::sidecar::Sidecars;
use crate::urlutil::last_segment;
use crate::urlutil::{
    encode_form_value, encode_segment, is_http_url, percent_decode, redact, resolve, same_origin,
};
use crate::watch::{WatchEvent, WatchKind, WatchSession};
use crate::{Source, dir_entry};
use fp_core::format::StereoLayout;
use fp_core::source::{
    ByteSource, Entry, EntryKind, SubtitleFile, is_script_name, is_subtitle_name, is_video_name,
};
use fp_core::sync::lock;
use serde_json::Value;
use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

/// Largest JSON document accepted from a feed.
const JSON_LIMIT: u64 = 64 << 20;

/// Parallel requests when a HereSphere group is listed.
const DETAIL_WORKERS: usize = 6;

// ---- fields both formats use ----

fn url_field(v: &Value, key: &str, base: &str) -> Option<String> {
    text(v, key).and_then(|u| resolve(base, &u).ok())
}

/// A date given as Unix seconds (or milliseconds) or as an ISO 8601 string.
fn date_field(v: &Value, key: &str) -> Option<i64> {
    match get(v, key)? {
        Value::Number(n) => n
            .as_f64()
            .filter(|t| *t > 0.0)
            .map(|t| if t > 1e11 { t / 1000.0 } else { t } as i64),
        Value::String(s) => crate::timeutil::parse_iso8601(s),
        _ => None,
    }
}

/// A rating out of 5.
fn rating_field(v: &Value, key: &str) -> Option<f32> {
    num(v, key)
        .filter(|r| *r > 0.0 && *r <= 5.0)
        .map(|r| r as f32)
}

/// Names from `[{"name": ...}]` (or plain strings).
fn names(v: &Value, key: &str) -> Vec<String> {
    array(v, key)
        .iter()
        .filter_map(|x| text(x, "name").or_else(|| x.as_str().map(|s| s.trim().to_string())))
        .filter(|s| !s.is_empty())
        .collect()
}

/// Subtitles from `[{"url": ..., "name": ..., "language": ...}]`.
fn subtitle_files(v: &Value, base: &str) -> Vec<SubtitleFile> {
    array(v, "subtitles")
        .iter()
        .filter_map(|s| {
            let mut file = crate::sidecar::subtitle_file(&url_field(s, "url", base)?, "");
            if let Some(name) = text(s, "name").filter(|n| !n.is_empty()) {
                file.name = name;
            }
            file.language = text(s, "language").filter(|l| !l.is_empty());
            Some(file)
        })
        .collect()
}

fn media_source(v: &Value, label: &str, base: &str) -> Option<MediaSource> {
    let url = url_field(v, "url", base)?;
    let height = uint(v, "height").or_else(|| uint(v, "resolution"));
    Some(MediaSource {
        url,
        height,
        width: uint(v, "width"),
        label: label.to_string(),
        size: num(v, "size").filter(|s| *s > 0.0).map(|s| s as u64),
    })
}

fn stereo_from(mode: &str) -> Option<StereoLayout> {
    Some(match mode.trim().to_ascii_lowercase().as_str() {
        "sbs" | "lr" | "sidebyside" | "fsbs" => StereoLayout::SideBySide,
        "hsbs" | "half_sbs" | "half-sbs" | "halfsbs" => StereoLayout::HalfSideBySide,
        "tb" | "ou" | "topbottom" | "fou" | "ftb" => StereoLayout::TopBottom,
        "hou" | "htb" | "half_tb" | "half-tb" | "half_ou" | "half-ou" | "halfou" | "halftb" => {
            StereoLayout::HalfTopBottom
        }
        "off" | "mono" | "none" | "" => StereoLayout::Mono,
        _ => return None,
    })
}

/// Location of a group folder.
fn group_location(feed_url: &str, name: &str) -> String {
    format!("{feed_url}#group={}", encode_segment(name))
}

/// Splits a group location into feed URL and group name.
fn split_group(location: &str) -> Option<(&str, String)> {
    let (feed, name) = location.split_once("#group=")?;
    Some((feed, percent_decode(name)))
}

/// True when a URL obviously points at a file rather than a scene document.
fn is_direct_file(url: &str) -> bool {
    let name = last_segment(url);
    is_video_name(&name) || is_script_name(&name) || is_subtitle_name(&name)
}

/// True when a response looks like JSON rather than media.
fn is_json_reply(r: &HttpReply) -> bool {
    match r.header("content-type") {
        None => true,
        Some(ct) => {
            let ct = ct.to_ascii_lowercase();
            ct.contains("json") || ct.starts_with("text/") || ct.contains("javascript")
        }
    }
}

/// What scene feeds offer beyond browsing folders.
pub trait SceneFeed: Send + Sync {
    /// Every group and scene of the feed, quickly: a HereSphere listing has
    /// only scene addresses ([`FeedScene::bare`]) until [`SceneFeed::scan`]
    /// or [`SceneFeed::scene`] fills them in.
    fn library(&self) -> Result<FeedLibrary>;

    /// One scene's details; `with_media` also asks for its media sources,
    /// which is slower on some servers.
    fn scene(&self, url: &str, with_media: bool) -> Result<SceneInfo>;

    /// Titles, dates and tags of every scene at once, from servers that
    /// answer a scan (`<url>/scan`); `None` from those that don't.
    /// Slower than the library: seconds, and megabytes, for a large one.
    /// See [`FeedLibrary::merge_scan`].
    fn scan(&self) -> Option<Vec<FeedScene>>;

    /// Marks or unmarks a scene as a favourite on the server, for scenes
    /// whose details say it takes that ([`SceneInfo::write_favorite`]).
    fn set_favorite(&self, url: &str, favorite: bool) -> Result<()>;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Protocol {
    /// The DeoVR feed's JSON. With credentials, documents are fetched with
    /// a `POST` of the form fields `login` and `password` (the DeoVR app's
    /// login); otherwise with `GET`.
    DeoVr,
    /// The HereSphere JSON API. Every request is a `POST` with a JSON body,
    /// carrying `username` and `password` when credentials are configured.
    HereSphere,
}

impl Protocol {
    fn label(self) -> &'static str {
        match self {
            Protocol::DeoVr => "DeoVR feed",
            Protocol::HereSphere => "HereSphere API",
        }
    }
}

/// A DeoVR feed or HereSphere API.
pub struct FeedSource {
    protocol: Protocol,
    id: String,
    name: String,
    url: String,
    creds: Option<Credentials>,
    client: HttpClient,
    max_height: Option<u32>,
    /// Each scene's event server, as its details named it.
    event_servers: Mutex<HashMap<String, String>>,
}

impl std::fmt::Debug for FeedSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FeedSource")
            .field("protocol", &self.protocol)
            .field("id", &self.id)
            .field("url", &redact(&self.url))
            .field("credentials", &self.creds)
            .field("max_height", &self.max_height)
            .finish_non_exhaustive()
    }
}

impl FeedSource {
    /// A DeoVR JSON feed.
    pub fn deovr(config: &FeedConfig) -> Result<FeedSource> {
        FeedSource::new(config, Protocol::DeoVr)
    }

    /// A HereSphere JSON API.
    pub fn heresphere(config: &FeedConfig) -> Result<FeedSource> {
        FeedSource::new(config, Protocol::HereSphere)
    }

    fn new(config: &FeedConfig, protocol: Protocol) -> Result<FeedSource> {
        if !is_http_url(&config.url) {
            return Err(Error::invalid(
                &config.url,
                "expected an http:// or https:// feed URL",
            ));
        }
        let (url, creds) = clean_url_and_credentials(&config.url, config.credentials.as_ref())?;
        let opts = HttpOptions {
            insecure_tls: config.insecure_tls,
            ..HttpOptions::default()
        };
        Ok(FeedSource {
            protocol,
            id: config.id.clone(),
            name: config.name.clone(),
            client: HttpClient::new(opts, creds.as_ref()).scoped_to(&url),
            url,
            creds,
            max_height: config.max_height,
            event_servers: Mutex::new(HashMap::new()),
        })
    }

    /// Fetches a document; `Ok(None)` when the server answered with
    /// something that is not JSON (a media stream), without downloading it.
    /// `with_media` asks a HereSphere video document for its media sources.
    fn fetch(&self, url: &str, with_media: Option<bool>) -> Result<Option<Value>> {
        match self.protocol {
            Protocol::DeoVr => match self.creds_for(url) {
                Some(c) => {
                    let form = format!(
                        "login={}&password={}",
                        encode_form_value(&c.username),
                        encode_form_value(&c.password)
                    );
                    self.json(
                        "POST",
                        url,
                        Some("application/x-www-form-urlencoded"),
                        Some(form.as_bytes()),
                    )
                }
                None => self.json("GET", url, None, None),
            },
            Protocol::HereSphere => {
                let mut body = self.heresphere_body(url);
                if let Some(m) = with_media {
                    body.insert("needsMediaSource".into(), Value::Bool(m));
                }
                let bytes = serde_json::to_vec(&Value::Object(body))?;
                self.json("POST", url, Some("application/json"), Some(&bytes))
            }
        }
    }

    /// The login, for the feed's own server only: a document it links to
    /// on another host gets none.
    fn creds_for(&self, url: &str) -> Option<&Credentials> {
        self.creds.as_ref().filter(|_| same_origin(url, &self.url))
    }

    /// A HereSphere request's body for `url`: the user's name and password,
    /// when configured and `url` is on the feed's server.
    fn heresphere_body(&self, url: &str) -> serde_json::Map<String, Value> {
        let mut body = serde_json::Map::new();
        if let Some(c) = self.creds_for(url) {
            body.insert("username".into(), Value::String(c.username.clone()));
            body.insert("password".into(), Value::String(c.password.clone()));
        }
        body
    }

    fn fetch_required(&self, url: &str, with_media: Option<bool>) -> Result<Value> {
        self.fetch(url, with_media)?.ok_or_else(|| {
            Error::parse(
                self.protocol.label(),
                format!("{} is not JSON", redact(url)),
            )
        })
    }

    fn json(
        &self,
        method: &str,
        url: &str,
        content_type: Option<&str>,
        body: Option<&[u8]>,
    ) -> Result<Option<Value>> {
        let mut headers = vec![("Accept", "application/json")];
        if let Some(ct) = content_type {
            headers.push(("Content-Type", ct));
        }
        let (reply, read) =
            self.client
                .request_if(method, url, &headers, body, JSON_LIMIT, is_json_reply)?;
        let reply = reply.error_for_status(url)?;
        if !read {
            return Ok(None);
        }
        serde_json::from_slice(&reply.body)
            .map(Some)
            .map_err(|e| Error::parse(format!("JSON from {}", redact(url)), e))
    }

    fn groups(&self, feed_url: &str) -> Result<Vec<SceneGroup>> {
        let v = self.fetch_required(feed_url, None)?;
        match self.protocol {
            Protocol::DeoVr => Ok(deovr::parse_deovr_root(&v, feed_url)),
            Protocol::HereSphere => heresphere::parse_heresphere_library(&v, feed_url),
        }
    }

    fn parse_scene(&self, v: &Value, base: &str) -> SceneInfo {
        match self.protocol {
            Protocol::DeoVr => deovr::parse_deovr_scene(v, base),
            Protocol::HereSphere => heresphere::parse_heresphere_video(v, base),
        }
    }

    fn group_entries(&self, groups: &[SceneGroup]) -> Vec<Entry> {
        groups
            .iter()
            .map(|g| {
                let mut e = dir_entry(g.name.clone(), group_location(&self.url, &g.name), None);
                e.thumbnail_url = g.scenes.iter().find_map(|s| s.thumbnail_url.clone());
                e
            })
            .collect()
    }

    /// Fills in titles and details for scenes listed by address only (a
    /// HereSphere group), several requests at a time. Scenes whose details
    /// fail keep their address-derived name.
    fn fill(&self, mut scenes: Vec<Entry>) -> Vec<Entry> {
        let next = AtomicUsize::new(0);
        let details: Vec<(usize, SceneInfo)> = std::thread::scope(|scope| {
            let workers: Vec<_> = (0..DETAIL_WORKERS.min(scenes.len()))
                .map(|_| {
                    scope.spawn(|| {
                        let mut out = Vec::new();
                        loop {
                            let i = next.fetch_add(1, Ordering::Relaxed);
                            let Some(e) = scenes.get(i) else { break };
                            match self.scene(&e.location, false) {
                                Ok(info) => out.push((i, info)),
                                Err(err) => log::debug!(
                                    "scene details for {} failed: {err}",
                                    redact(&e.location)
                                ),
                            }
                        }
                        out
                    })
                })
                .collect();
            workers
                .into_iter()
                .flat_map(|w| w.join().unwrap_or_default())
                .collect()
        });
        for (i, info) in details {
            info.apply_to(&mut scenes[i]);
        }
        scenes
    }
}

impl SceneFeed for FeedSource {
    fn library(&self) -> Result<FeedLibrary> {
        let mut lib = FeedLibrary::from_groups(self.groups(&self.url)?);
        // A DeoVR root carries titles, lengths and posters; a HereSphere
        // library only scene addresses.
        if self.protocol == Protocol::HereSphere {
            for s in &mut lib.scenes {
                s.bare = true;
            }
        }
        Ok(lib)
    }

    fn scene(&self, url: &str, with_media: bool) -> Result<SceneInfo> {
        let v = self.fetch_required(url, Some(with_media))?;
        let info = self.parse_scene(&v, url);
        if let Some(events) = &info.event_server {
            lock(&self.event_servers).insert(url.to_string(), events.clone());
        }
        Ok(info)
    }

    /// HereSphere posts the change to the video's own URL, as it does
    /// when the favourite is toggled in the player.
    fn set_favorite(&self, url: &str, favorite: bool) -> Result<()> {
        if self.protocol != Protocol::HereSphere {
            return Err(Error::Unsupported(
                "a DeoVR feed takes no favourites".into(),
            ));
        }
        let mut body = self.heresphere_body(url);
        body.insert("isFavorite".into(), Value::Bool(favorite));
        body.insert("needsMediaSource".into(), Value::Bool(false));
        let bytes = serde_json::to_vec(&Value::Object(body))?;
        self.json("POST", url, Some("application/json"), Some(&bytes))?;
        Ok(())
    }

    fn scan(&self) -> Option<Vec<FeedScene>> {
        if self.protocol != Protocol::HereSphere {
            return None;
        }
        match self.fetch(&heresphere::scan_url(&self.url), None) {
            Ok(Some(v)) => Some(heresphere::parse_heresphere_scan(&v, &self.url)),
            Ok(None) => None,
            Err(e) => {
                log::debug!("no HereSphere scan at {}: {e}", redact(&self.url));
                None
            }
        }
    }
}

impl Source for FeedSource {
    fn id(&self) -> &str {
        &self.id
    }

    fn name(&self) -> &str {
        &self.name
    }

    fn kind(&self) -> SourceKind {
        match self.protocol {
            Protocol::DeoVr => SourceKind::DeoVr,
            Protocol::HereSphere => SourceKind::HereSphere,
        }
    }

    fn describe(&self) -> String {
        format!("{} {}", self.protocol.label(), redact(&self.url))
    }

    fn list(&self, location: Option<&str>) -> Result<Vec<Entry>> {
        let feed_url = location.unwrap_or(&self.url);
        let Some((feed, name)) = split_group(feed_url) else {
            return Ok(self.group_entries(&self.groups(feed_url)?));
        };
        let scenes = find_group(self.groups(feed)?, &name, feed_url)?;
        Ok(match self.protocol {
            Protocol::DeoVr => scenes,
            Protocol::HereSphere => self.fill(scenes),
        })
    }

    fn open(&self, location: &str) -> Result<Arc<dyn ByteSource>> {
        let url = if is_direct_file(location) {
            location.to_string()
        } else {
            match self.fetch(location, Some(true))? {
                Some(v) => self
                    .parse_scene(&v, location)
                    .best_source(self.max_height)
                    .map(|s| s.url.clone())
                    .ok_or_else(|| {
                        Error::NotFound(format!("no playable source in {}", redact(location)))
                    })?,
                None => location.to_string(),
            }
        };
        Ok(Arc::new(self.client.open(&url)?))
    }

    fn transcodes(&self, video: &Entry) -> Vec<crate::Transcode> {
        if is_direct_file(&video.location) {
            return Vec::new();
        }
        match self.scene(&video.location, true) {
            Ok(s) => s.transcodes(self.max_height),
            Err(e) => {
                log::warn!("transcodes of {}: {e}", redact(&video.location));
                Vec::new()
            }
        }
    }

    /// `id` is the transcode's own address, as the scene listed it.
    fn open_transcode(
        &self,
        location: &str,
        id: &str,
        start: f64,
        _codec: crate::TranscodeCodec,
    ) -> Result<Arc<dyn ByteSource>> {
        let scene = self.scene(location, true)?;
        let source = scene
            .sources
            .iter()
            .find(|s| s.url == id && s.is_transcode())
            .ok_or_else(|| Error::NotFound(format!("transcode of {}", redact(location))))?;
        let url = if start > 0.0 && source.takes_start() {
            crate::transcode::with_query(&source.url, "start", &format!("{start:.3}"))
        } else {
            source.url.clone()
        };
        Ok(crate::transcode::Streamed::wrap(self.client.open(&url)?))
    }

    fn details(&self, entry: &Entry) -> Result<Entry> {
        if entry.kind != EntryKind::Video || is_direct_file(&entry.location) {
            return Ok(entry.clone());
        }
        let mut e = entry.clone();
        self.scene(&entry.location, true)?.apply_to(&mut e);
        Ok(e)
    }

    fn sidecars(&self, video: &Entry) -> Result<Sidecars> {
        let full = if video.scripts.is_empty() && video.subtitles.is_empty() {
            self.details(video)?
        } else {
            video.clone()
        };
        Ok(Sidecars::from_entry(&full))
    }

    /// For a HereSphere video whose details name an event server.
    fn watch_session(&self, video: &Entry) -> Option<WatchSession> {
        if self.protocol != Protocol::HereSphere || is_direct_file(&video.location) {
            return None;
        }
        let cached = lock(&self.event_servers).get(&video.location).cloned();
        let endpoint = match cached {
            Some(e) => e,
            None => self.scene(&video.location, false).ok()?.event_server?,
        };
        Some(WatchSession::new(
            video.location.clone(),
            video.name.clone(),
            Some(endpoint),
        ))
    }

    /// The events HereSphere itself sends: open, play (also after a seek
    /// or a change of speed), pause and close, each with the position in
    /// milliseconds.
    fn report_watch(&self, session: &WatchSession, event: WatchEvent) -> Result<()> {
        const OPEN: u8 = 0;
        const PLAY: u8 = 1;
        const PAUSE: u8 = 2;
        const CLOSE: u8 = 3;
        let Some(endpoint) = &session.endpoint else {
            return Ok(());
        };
        let codes: &[u8] = match event.kind {
            WatchKind::Start if event.paused => &[OPEN],
            WatchKind::Start => &[OPEN, PLAY],
            WatchKind::Tick => &[],
            WatchKind::Seek | WatchKind::Speed if event.paused => &[PAUSE],
            WatchKind::Pause => &[PAUSE],
            WatchKind::Resume | WatchKind::Seek | WatchKind::Speed => &[PLAY],
            WatchKind::Stop => &[CLOSE],
        };
        for &code in codes {
            let body = serde_json::to_vec(&serde_json::json!({
                "username": self.creds.as_ref().map_or("", |c| c.username.as_str()),
                "id": session.item,
                "title": session.title,
                "event": code,
                "time": event.position * 1000.0,
                "speed": event.speed,
                "utc": crate::watch::now_ms(),
                "connectionKey": "",
            }))?;
            self.client
                .request(
                    "POST",
                    endpoint,
                    &[("Content-Type", "application/json")],
                    Some(&body),
                    JSON_LIMIT,
                )?
                .error_for_status(endpoint)?;
        }
        Ok(())
    }

    fn as_scene_feed(&self) -> Option<&dyn SceneFeed> {
        Some(self)
    }
}

fn find_group(groups: Vec<SceneGroup>, name: &str, location: &str) -> Result<Vec<Entry>> {
    groups
        .into_iter()
        .find(|g| g.name == name)
        .map(|g| g.scenes)
        .ok_or_else(|| Error::NotFound(redact(location)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn transcodes_are_listed_the_one_that_fits_first() {
        let v = json!({"title": "A", "encodings": [
            {"name": "direct", "videoSources": [{"resolution": 2880, "url": "https://vr/f.mp4"}]},
            {"name": "transcoding", "videoSources": [
                {"resolution": 720, "url": "https://vr/scene/1/stream.mp4?resolution=STANDARD_HD"},
                {"resolution": 2160, "url": "https://vr/scene/1/stream.mp4?resolution=FOUR_K"},
                {"resolution": 1080, "url": "https://vr/other/1080"}
            ]}
        ]});
        let s = deovr::parse_deovr_scene(&v, "https://vr/");
        let t = s.transcodes(Some(1440));
        let labels: Vec<&str> = t.iter().map(|t| t.label.as_str()).collect();
        assert_eq!(labels, ["1080p", "720p", "2160p"]);
        // Stash's take a start time; an address of unknown kind doesn't.
        assert!(!t[0].seekable && t[1].seekable && t[2].seekable);
        assert_eq!(
            t[1].id,
            "https://vr/scene/1/stream.mp4?resolution=STANDARD_HD"
        );
        assert!(s.transcodes(None)[0].label == "2160p");
    }

    #[test]
    fn the_file_comes_before_the_servers_transcode() {
        // A server with live transcodes may list them first, claiming
        // more than the file (2160 against 2048).
        let transcodes = json!({"name": "transcoding", "videoSources": [
            {"resolution": 2160, "url": "https://vr/scene/1/stream.mp4?resolution=FOUR_K"},
            {"resolution": 1080, "url": "https://vr/scene/1/stream.mp4?resolution=FULL_HD"}
        ]});
        let direct = json!({"name": "direct", "videoSources": [
            {"resolution": 2048, "url": "https://vr/scene/1/stream"}
        ]});
        let deovr = json!({"title": "A", "encodings": [transcodes, direct]});
        let s = deovr::parse_deovr_scene(&deovr, "https://vr/deovr/1");
        assert!(s.sources[0].is_transcode() && !s.sources[2].is_transcode());
        assert_eq!(
            s.best_source(None).unwrap().url,
            "https://vr/scene/1/stream"
        );
        // A limit the file is over: the transcode that fits.
        assert_eq!(
            s.best_source(Some(1080)).unwrap().url,
            "https://vr/scene/1/stream.mp4?resolution=FULL_HD"
        );
        // Nothing fits: the smallest file, before any transcode.
        assert_eq!(
            s.best_source(Some(100)).unwrap().url,
            "https://vr/scene/1/stream"
        );

        // HereSphere lists the file first; the same choice.
        let heresphere = json!({"title": "A", "media": [
            {"name": "direct", "sources": [{"resolution": 2048, "url": "https://vr/scene/1/stream"}]},
            {"name": "transcoding", "sources": [
                {"resolution": 2160, "url": "https://vr/scene/1/stream.mp4?resolution=FOUR_K"}
            ]}
        ]});
        let s = heresphere::parse_heresphere_video(&heresphere, "https://vr/heresphere/1");
        assert_eq!(
            s.best_source(None).unwrap().url,
            "https://vr/scene/1/stream"
        );

        // Only transcodes: one of them.
        let only = json!({"title": "A", "encodings": [{"name": "Transcoding", "videoSources": [
            {"resolution": 720, "url": "https://vr/scene/1/stream.mp4?resolution=STANDARD_HD"}
        ]}]});
        let s = deovr::parse_deovr_scene(&only, "https://vr/deovr/1");
        assert!(s.best_source(None).unwrap().is_transcode());
    }

    #[test]
    fn describe_and_debug_hide_secrets() {
        let config = FeedConfig {
            id: "f".into(),
            name: "Feed".into(),
            url: "http://u:pwx9@vr:9999/deovr?apikey=SECRETKEY".into(),
            credentials: Some(Credentials::new("bob", "hunter2")),
            insecure_tls: false,
            max_height: None,
        };
        let d = FeedSource::deovr(&config).unwrap();
        let h = FeedSource::heresphere(&config).unwrap();
        for t in [
            d.describe(),
            format!("{d:?}"),
            h.describe(),
            format!("{h:?}"),
        ] {
            assert!(
                !t.contains("SECRETKEY") && !t.contains("hunter2") && !t.contains("pwx9"),
                "{t}"
            );
        }
        assert!(
            FeedSource::deovr(&FeedConfig {
                url: "ftp://x/".into(),
                ..config
            })
            .is_err()
        );
    }

    #[test]
    fn group_locations() {
        let l = group_location("http://h/deovr?apikey=1", "Recent & New");
        assert_eq!(l, "http://h/deovr?apikey=1#group=Recent%20%26%20New");
        assert_eq!(
            split_group(&l),
            Some(("http://h/deovr?apikey=1", "Recent & New".to_string()))
        );
        assert!(is_direct_file("http://h/a/b.mp4?x=1"));
        assert!(!is_direct_file("http://h/deovr/12"));
    }
}
