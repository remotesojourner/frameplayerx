//! Resolves Plex media versions, authenticated parts/subtitles and timelines.

use super::{PlexSource, container, resource_url};
use crate::error::{Error, Result};
use crate::json::{array, boolean, num, text, uint};
use crate::media_identity::{
    MediaIdentity, MediaKind, display_title, external_id, release_basename,
};
use crate::urlutil::encode_form_value;
use crate::watch::{WatchEvent, WatchKind, WatchSession};
use fp_core::source::{ByteSource, Entry, SubtitleFile};
use fp_core::sync::lock;
use serde_json::Value;
use std::sync::Arc;
use std::time::{Duration, Instant};

fn parse_guid(value: &str, provider: &str) -> Option<u64> {
    let (scheme, value) = value.split_once("://")?;
    let legacy_provider = scheme.strip_prefix("com.plexapp.agents.");
    if scheme != provider && legacy_provider != Some(provider) {
        return None;
    }
    external_id(value.split('?').next()?)
}

fn guid(item: &Value, provider: &str) -> Option<u64> {
    array(item, "Guid")
        .iter()
        .filter_map(|guid| text(guid, "id"))
        .chain(text(item, "guid"))
        .find_map(|value| parse_guid(&value, provider))
}

fn item_identity(item: &Value, version: Option<&Value>) -> MediaIdentity {
    let kind = match text(item, "type").as_deref() {
        Some("movie") => Some(MediaKind::Movie),
        Some("episode") => Some(MediaKind::Episode),
        _ => None,
    };
    let coordinate = |key| {
        text(item, key)
            .filter(|_| kind == Some(MediaKind::Episode))?
            .parse()
            .ok()
    };
    MediaIdentity {
        title: text(item, "title").and_then(|v| display_title(&v)),
        year: uint(item, "year"),
        kind,
        series_title: text(item, "grandparentTitle").and_then(|v| display_title(&v)),
        season: coordinate("parentIndex"),
        episode: coordinate("index"),
        imdb_id: guid(item, "imdb"),
        tmdb_id: guid(item, "tmdb"),
        release_basename: version
            .and_then(|media| array(media, "Part").first())
            .and_then(|part| text(part, "file"))
            .and_then(|path| release_basename(&path)),
        ..Default::default()
    }
}

impl PlexSource {
    pub(super) fn matching_identity(&self, video: &Entry) -> Result<Option<MediaIdentity>> {
        let location = self.parse_location(&video.location)?;
        if location.kind != "item" {
            return Ok(None);
        }
        let Some(endpoint) = lock(&self.endpoint).clone() else {
            return Ok(None);
        };
        let deadline = Instant::now() + Duration::from_secs(2);
        let item_metadata = |id: &str| -> Result<Option<Value>> {
            let path = format!("/library/metadata/{}?includeGuids=1", encode_form_value(id));
            let url = resource_url(&endpoint.base, &path)?;
            let Some(bytes) = endpoint.client.get_before(&url, deadline, 2 << 20)? else {
                return Ok(None);
            };
            let response = serde_json::from_slice(&bytes)?;
            Ok(array(container(&response), "Metadata").first().cloned())
        };
        let Some(item) = item_metadata(&location.id)? else {
            return Ok(None);
        };
        let selected = location
            .media
            .or_else(|| lock(&self.versions).get(&location.id).cloned());
        let version = self.choose_media(&item, selected.as_deref()).ok();
        let mut identity = item_identity(&item, version);
        if identity.season.is_some()
            && identity.episode.is_some()
            && identity.imdb_id.is_none()
            && identity.tmdb_id.is_none()
            && let Some(key) = text(&item, "grandparentRatingKey")
            && let Ok(Some(series)) = item_metadata(&key)
        {
            identity.series_imdb_id = guid(&series, "imdb");
            identity.series_tmdb_id = guid(&series, "tmdb");
        }
        Ok(Some(identity))
    }

    pub(super) fn source_tracks(&self, video: &Entry) -> Result<Vec<crate::PlaybackTrack>> {
        let location = self.parse_location(&video.location)?;
        let raw = self.raw_item(&location.id)?;
        let media = self.choose_media(&raw, location.media.as_deref())?;
        let Some(part) = array(media, "Part").first() else {
            return Ok(Vec::new());
        };
        Ok(array(part, "Stream")
            .iter()
            .filter(|s| matches!(num(s, "streamType"), Some(2.0 | 3.0)))
            .filter_map(|s| {
                Some(crate::PlaybackTrack {
                    id: text(s, "id")?,
                    index: num(s, "index").map(|n| n.max(0.0) as usize),
                    location: text(s, "key").and_then(|p| self.asset("subtitle", &p)),
                    subtitle: num(s, "streamType") == Some(3.0),
                    title: text(s, "displayTitle")
                        .or_else(|| text(s, "language"))
                        .or_else(|| text(s, "codec"))
                        .unwrap_or_default(),
                    codec: text(s, "codec").unwrap_or_default(),
                    selected: boolean(s, "selected") == Some(true),
                })
            })
            .collect())
    }

    pub(super) fn entry_details(&self, entry: &Entry) -> Result<Entry> {
        let location = self.parse_location(&entry.location)?;
        if location.kind != "item" {
            return Err(Error::Unsupported("Plex location is not a video".into()));
        }
        let raw = self.raw_item(&location.id)?;
        let selected = location
            .media
            .or_else(|| lock(&self.versions).get(&location.id).cloned());
        let media = self.choose_media(&raw, selected.as_deref())?;
        let item = self
            .catalog_item(&raw)
            .ok_or_else(|| Error::Unsupported("Plex item is not a video".into()))?;
        let mut entry = item.to_entry();
        // Only a version someone picked: the video otherwise keeps the one
        // location it is listed under, and with it one row in the library.
        entry.location = self.location("item", &location.id, selected.as_deref());
        entry.resume = Some(num(&raw, "viewOffset").unwrap_or(0.0).max(0.0) / 1000.0);
        entry.duration = num(&raw, "duration").map(|n| n / 1000.0);
        entry.modified = num(&raw, "updatedAt").map(|n| n as i64);
        if let Some(part) = array(media, "Part").first() {
            entry.size = num(part, "size").map(|n| n.max(0.0) as u64);
            entry.format = text(part, "file")
                .as_deref()
                .and_then(fp_core::format::detect_from_name)
                .or(entry.format);
            entry.subtitles = array(part, "Stream")
                .iter()
                .filter(|s| num(s, "streamType") == Some(3.0))
                .filter_map(|s| {
                    let key = text(s, "key")?;
                    let codec = text(s, "codec").unwrap_or_default();
                    if !matches!(
                        codec.as_str(),
                        "srt" | "ass" | "ssa" | "webvtt" | "vtt" | "subrip"
                    ) {
                        return None;
                    }
                    let location = self.asset("subtitle", &key)?;
                    Some(SubtitleFile {
                        name: format!(
                            "{}.{}",
                            text(s, "displayTitle")
                                .or_else(|| text(s, "language"))
                                .unwrap_or_else(|| "Subtitles".into()),
                            if codec == "subrip" { "srt" } else { &codec }
                        ),
                        location,
                        language: text(s, "languageTag").or_else(|| text(s, "languageCode")),
                        language_name: None,
                        flags: Default::default(),
                    })
                })
                .collect();
        }
        entry.markers = array(&raw, "Chapter")
            .iter()
            .enumerate()
            .filter_map(|(i, c)| {
                Some((
                    num(c, "startTimeOffset")? / 1000.0,
                    text(c, "tag")
                        .or_else(|| text(c, "title"))
                        .unwrap_or_else(|| format!("{}", i + 1)),
                ))
            })
            .collect();
        Ok(entry)
    }

    pub(super) fn make_session(&self, video: &Entry) -> Result<WatchSession> {
        let location = self.parse_location(&video.location)?;
        if location.kind != "item" {
            return Err(Error::Unsupported("Plex location is not a video".into()));
        }
        let mut session = WatchSession::new(location.id.clone(), video.name.clone(), None);
        session.duration_ms = video
            .duration
            .filter(|d| d.is_finite() && *d > 0.0)
            .map(|d| (d * 1000.0) as u64);
        session.media_id = location.media.clone();
        if let Some((_, raw)) = lock(&self.metadata).get(&location.id).cloned()
            && let Ok(media) = self.choose_media(&raw, location.media.as_deref())
            && let Some(part) = array(media, "Part").first()
        {
            session.audio_stream = array(part, "Stream")
                .iter()
                .find(|s| num(s, "streamType") == Some(2.0) && boolean(s, "selected") == Some(true))
                .and_then(|s| text(s, "id"));
            session.subtitle_stream = array(part, "Stream")
                .iter()
                .find(|s| num(s, "streamType") == Some(3.0) && boolean(s, "selected") == Some(true))
                .and_then(|s| text(s, "id"));
        }
        Ok(session)
    }

    /// The file of the version that plays, as the server keeps it, asked
    /// for as a download, which the server refuses a user without Allow
    /// Downloads.
    pub(super) fn open_original_part(
        &self,
        location: &str,
        title: &str,
    ) -> Result<crate::OriginalFile> {
        let location = self.parse_location(location)?;
        if location.kind != "item" {
            return Err(Error::Unsupported("Plex location is not a video".into()));
        }
        if !self.allow_sync()? {
            return Err(Error::Auth(format!(
                "{} does not allow downloads",
                self.name
            )));
        }
        let raw = self.raw_item(&location.id)?;
        let selected = location
            .media
            .or_else(|| lock(&self.versions).get(&location.id).cloned());
        let media = self.choose_media(&raw, selected.as_deref())?;
        let [part] = array(media, "Part") else {
            return Err(Error::Unsupported(
                "a Plex video in several parts has no one file to download".into(),
            ));
        };
        let path =
            text(part, "key").ok_or_else(|| Error::NotFound("Plex media part address".into()))?;
        let endpoint = self.client()?;
        let mut url = url::Url::parse(&resource_url(&endpoint.base, &path)?)
            .map_err(|_| Error::invalid(&path, "invalid part address"))?;
        url.query_pairs_mut().append_pair("download", "1");
        let name = text(part, "file")
            .and_then(|file| release_basename(&file))
            .unwrap_or_else(|| title.to_string());
        Ok(crate::OriginalFile {
            reader: endpoint.client.open_bulk(url.as_str())?,
            name,
        })
    }

    pub(super) fn open_resource(
        &self,
        location: &str,
        session: Option<&WatchSession>,
    ) -> Result<Arc<dyn ByteSource>> {
        let location = self.parse_location(location)?;
        if matches!(location.kind.as_str(), "image" | "subtitle") {
            let endpoint = self.client()?;
            let path = location
                .path
                .ok_or_else(|| Error::invalid("plex resource", "missing path"))?;
            return endpoint
                .client
                .open(&resource_url(&endpoint.base, &path)?)
                .map(|f| Arc::new(f) as Arc<dyn ByteSource>);
        }
        if location.kind != "item" {
            return Err(Error::Unsupported("Plex location is not playable".into()));
        }
        let raw = self.raw_item(&location.id)?;
        let selected = location
            .media
            .or_else(|| lock(&self.versions).get(&location.id).cloned());
        let media = self.choose_media(&raw, selected.as_deref())?;
        let endpoint = self.client()?;
        let parts = array(media, "Part");
        if session.is_some() && endpoint.relay && num(media, "bitrate").is_none_or(|n| n > 1800.0) {
            return Err(Error::Unsupported(
                "Plex Relay requires a lower bitrate; choose Automatic or a transcoded quality"
                    .into(),
            ));
        }
        if parts.len() != 1 {
            return Err(Error::Unsupported(
                "Plex multipart video requires a server transcode".into(),
            ));
        }
        let part = parts
            .first()
            .ok_or_else(|| Error::NotFound("Plex media part".into()))?;
        let path =
            text(part, "key").ok_or_else(|| Error::NotFound("Plex media part address".into()))?;
        let mut url = url::Url::parse(&resource_url(&endpoint.base, &path)?)
            .map_err(|_| Error::invalid(&path, "invalid part address"))?;
        if let Some(session) = session {
            let media_index = array(&raw, "Media")
                .iter()
                .position(|m| text(m, "id") == text(media, "id"))
                .unwrap_or(0);
            let mut decision = url::form_urlencoded::Serializer::new(String::new());
            decision
                .append_pair("path", &format!("/library/metadata/{}", location.id))
                .append_pair("mediaIndex", &media_index.to_string())
                .append_pair("partIndex", "0")
                .append_pair("protocol", "http")
                .append_pair("directPlay", "1")
                // The local demuxer/decoder decides codec support, as with
                // Jellyfin. Plex's generic profile omits supported formats
                // such as HEVC/MKV. Keep the server's access decision below.
                .append_pair("hasMDE", "1")
                // Subtitles are rendered locally from the original part or
                // sidecar, so Plex must not burn in its stored selection.
                .append_pair("subtitles", "none")
                .append_pair("directStream", "1")
                .append_pair("X-Plex-Client-Profile-Name", "Generic")
                .append_pair("X-Plex-Session-Identifier", &session.id)
                .append_pair("session", &session.id);
            let decision = self.json(&format!(
                "/video/:/transcode/universal/decision?{}",
                decision.finish()
            ))?;
            let decision = container(&decision);
            let denial = if num(decision, "generalDecisionCode").is_some_and(|n| n >= 2000.0) {
                Some("generalDecisionText")
            } else if num(decision, "mdeDecisionCode").is_some_and(|n| n != 1000.0) {
                Some("mdeDecisionText")
            } else if num(decision, "directPlayDecisionCode").is_some_and(|n| n != 1000.0) {
                Some("directPlayDecisionText")
            } else {
                None
            };
            if let Some(reason) = denial {
                return Err(Error::Unsupported(
                    text(decision, reason).unwrap_or_else(|| "Plex declined playback".into()),
                ));
            }
            url.query_pairs_mut()
                .append_pair("X-Plex-Session-Identifier", &session.id)
                .append_pair("session", &session.id);
        }
        endpoint
            .client
            .open(url.as_str())
            .map(|f| Arc::new(f) as Arc<dyn ByteSource>)
    }

    pub(super) fn timeline(&self, session: &WatchSession, event: WatchEvent) -> Result<()> {
        let mut query = url::form_urlencoded::Serializer::new(String::new());
        let position = if event.position.is_finite() {
            (event.position.max(0.0) * 1000.0) as u64
        } else {
            0
        };
        query
            .append_pair("key", &format!("/library/metadata/{}", session.item))
            .append_pair("ratingKey", &session.item)
            .append_pair(
                "state",
                if event.kind == WatchKind::Stop {
                    "stopped"
                } else if event.paused {
                    "paused"
                } else {
                    "playing"
                },
            )
            .append_pair("time", &position.to_string())
            .append_pair("X-Plex-Session-Identifier", &session.id);
        if let Some(duration) = session.duration_ms {
            query.append_pair("duration", &duration.to_string());
        }
        let reported = self.request("POST", &format!("/:/timeline?{}", query.finish()), &[]);
        // Where it was left has changed.
        lock(&self.metadata).remove(&session.item);
        reported?;
        Ok(())
    }
}

#[cfg(test)]
mod identity_tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn subtitle_matching_never_uses_plex_keys_as_external_ids() {
        let version = json!({"Part":[{"file":"/private/media/Show.S01E04.Release.mkv"}]});
        let item = json!({
            "type":"episode", "title":"Episode title", "grandparentTitle":"Show",
            "parentIndex":1, "index":4, "grandparentGuid":"imdb://tt11111",
            "guid":"plex://episode/109", "ratingKey":"9001",
            "Guid":[{"id":"tmdb://102030"}]
        });
        let identity = item_identity(&item, Some(&version));
        assert_eq!(identity.kind, Some(MediaKind::Episode));
        assert_eq!(identity.imdb_id, None);
        assert_eq!(identity.tmdb_id, Some(102030));
        assert_eq!(identity.series_imdb_id, None);
        assert_eq!((identity.season, identity.episode), (Some(1), Some(4)));
        assert_eq!(
            identity.release_basename.as_deref(),
            Some("Show.S01E04.Release.mkv")
        );

        let movie = json!({"type":"movie", "guid":"com.plexapp.agents.imdb://tt0046889?lang=en"});
        let identity = item_identity(&movie, None);
        assert_eq!(identity.kind, Some(MediaKind::Movie));
        assert_eq!(identity.imdb_id, Some(46889));
    }
}
