//! Seek previews from Jellyfin (sheets of tiles, by the original media
//! version) and Emby (one picture per listed moment).

use super::{MediaBrowserSource, decode_location, location_version};
use crate::json::{array, get, num, text, uint};
use crate::urlutil::encode_form_value;
use crate::{
    Result, TRICKPLAY_MAX_DIMENSION, TRICKPLAY_MAX_FRAMES, TRICKPLAY_MAX_PIXELS, Trickplay,
    TrickplayTiming,
};
use fp_core::source::Entry;
use fp_core::sync::lock;
use serde_json::Value;
use std::time::{Duration, Instant};

/// The width asked of Emby's pictures, as its own players ask.
const EMBY_WIDTH: u32 = 320;
/// .NET ticks in a millisecond, Emby's unit for moments.
const TICKS_PER_MS: f64 = 10_000.0;

impl MediaBrowserSource {
    pub(super) fn trickplay_for(&self, entry: &Entry) -> Result<Option<Trickplay>> {
        let resolved = self.resolved_location(&entry.location)?;
        let Some(("item", id)) = decode_location(&resolved) else {
            return Ok(None);
        };
        let Some((client, user_id)) = lock(&self.auth)
            .as_ref()
            .map(|auth| (auth.client.clone(), auth.user_id.clone()))
        else {
            return Ok(None);
        };

        // The item, for its versions and on Jellyfin its previews' layout.
        // A source saved with a token doesn't know which server it is, so
        // it takes what the server offers: Jellyfin's sheets in the item,
        // else Emby's list. Servers that offer neither keep the time-only
        // seek tooltip.
        let item_url = format!(
            "{}/Users/{}/Items/{}",
            self.base_url,
            encode_form_value(&user_id),
            encode_form_value(id)
        );
        let deadline = Instant::now() + Duration::from_secs(3);
        let Some(body) = client.get_before(&item_url, deadline, 2 << 20)? else {
            return Ok(None);
        };
        let item: Value = serde_json::from_slice(&body)?;
        let version = location_version(&resolved).map(str::to_owned).or_else(|| {
            self.playing_version(&resolved, &item)
                .and_then(|v| text(&v, "Id"))
        });
        let version_id = version.as_deref().unwrap_or(id);
        let Some(manifest) = get(&item, "Trickplay")
            .and_then(|versions| get(versions, version_id))
            .and_then(Value::as_object)
        else {
            return self.emby_trickplay(&client, id, version_id);
        };
        let Some(timing) = manifest
            .values()
            .filter_map(jellyfin_layout)
            .min_by_key(|timing| match timing {
                TrickplayTiming::Fixed { tile, .. } => (tile[0] < 240, tile[0].abs_diff(240)),
                _ => (true, u32::MAX),
            })
        else {
            return Ok(None);
        };
        let TrickplayTiming::Fixed { tile, .. } = timing else {
            return Ok(None);
        };

        // The 10.10 tile endpoint resolves the route ID even when given
        // MediaSourceId. Using the version's item ID works on both APIs.
        let sheets = format!(
            "{}/Videos/{}/Trickplay/{}",
            self.base_url,
            encode_form_value(version_id),
            tile[0]
        );
        Ok(Some(Trickplay::new(timing, move |index, cancelled| {
            client.get_trickplay_sheet(&format!("{sheets}/{index}.jpg"), cancelled)
        })))
    }

    /// Emby lists its pictures' moments and serves each on its own.
    fn emby_trickplay(
        &self,
        client: &crate::http::HttpClient,
        id: &str,
        version: &str,
    ) -> Result<Option<Trickplay>> {
        let item = encode_form_value(id);
        let version = encode_form_value(version);
        let set_url = format!(
            "{}/Items/{item}/ThumbnailSet?Width={EMBY_WIDTH}&MediaSourceId={version}",
            self.base_url
        );
        let deadline = Instant::now() + Duration::from_secs(3);
        // Jellyfin has no such list: its answer is no previews.
        let Ok(Some(body)) = client.get_before(&set_url, deadline, 8 << 20) else {
            return Ok(None);
        };
        let Some(frames) = serde_json::from_slice(&body)
            .ok()
            .as_ref()
            .and_then(emby_frames)
        else {
            return Ok(None);
        };
        let times: Vec<u64> = frames.iter().map(|f| f.0).collect();
        let base = self.base_url.clone();
        let client = client.clone();
        Ok(Some(Trickplay::new(
            TrickplayTiming::Listed(times.into()),
            move |index, cancelled| {
                let Some((ms, tag)) = frames.get(index as usize) else {
                    return Err(crate::Error::NotFound("Emby preview picture".into()));
                };
                let ticks = ms * TICKS_PER_MS as u64;
                let url = format!(
                    "{base}/Items/{item}/Images/Thumbnail?maxWidth={EMBY_WIDTH}&tag={}\
                     &PositionTicks={ticks}&MediaSourceId={version}&quality=90",
                    encode_form_value(tag)
                );
                client.get_trickplay_sheet(&url, cancelled)
            },
        )))
    }
}

fn jellyfin_layout(info: &Value) -> Option<TrickplayTiming> {
    let integer = |field| uint(info, field).filter(|value| *value > 0);
    let tile = [integer("Width")?, integer("Height")?];
    let columns = integer("TileWidth")?;
    let rows = integer("TileHeight")?;
    let width = tile[0].checked_mul(columns)?;
    let height = tile[1].checked_mul(rows)?;
    if width > TRICKPLAY_MAX_DIMENSION
        || height > TRICKPLAY_MAX_DIMENSION
        || u64::from(width) * u64::from(height) > TRICKPLAY_MAX_PIXELS
    {
        return None;
    }
    Some(TrickplayTiming::Fixed {
        tile,
        interval_ms: integer("Interval")?,
        frames: integer("ThumbnailCount")?,
        columns,
        rows,
    })
}

/// Emby's listed pictures, by moment in milliseconds with each one's image
/// tag: in order, a moment listed twice keeping its last picture.
fn emby_frames(set: &Value) -> Option<Vec<(u64, String)>> {
    let mut frames: Vec<(u64, String)> = array(set, "Thumbnails")
        .iter()
        .filter_map(|t| {
            let ticks = num(t, "PositionTicks").filter(|t| t.is_finite() && *t >= 0.0)?;
            let tag = text(t, "ImageTag").map(|tag| tag.trim().to_owned())?;
            (!tag.is_empty()).then_some(((ticks / TICKS_PER_MS) as u64, tag))
        })
        .collect();
    frames.sort_by_key(|f| f.0);
    let mut kept: Vec<(u64, String)> = Vec::with_capacity(frames.len());
    for frame in frames {
        match kept.last_mut() {
            Some(last) if last.0 == frame.0 => *last = frame,
            _ => kept.push(frame),
        }
    }
    (!kept.is_empty() && kept.len() <= TRICKPLAY_MAX_FRAMES).then_some(kept)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Source;
    use crate::config::MediaBrowserConfig;
    use crate::mediabrowser::encode_version_location;
    use crate::test_server::{TestServer, resp};
    use fp_core::source::EntryKind;
    use serde_json::json;

    fn source(base: &str) -> MediaBrowserSource {
        MediaBrowserSource::new(&MediaBrowserConfig {
            id: "library".into(),
            name: "Server".into(),
            url: base.into(),
            credentials: None,
            insecure_tls: false,
            parallel_downloads: false,
            max_height: None,
            access_token: Some("source-token".into()),
            user_id: Some("user".into()),
        })
        .unwrap()
    }

    #[test]
    fn authenticates_and_fetches_the_requested_sheet_of_the_playing_version() {
        const TILE_BYTES: &[u8] = b"the selected sheet";
        let server = TestServer::start(|req| {
            match req.path() {
                "/Users/user/Items/item" => {
                    let tiles = |width, height, columns, interval_ms| {
                        json!({
                            "Width":width, "Height":height, "TileWidth":columns, "TileHeight":10,
                            "Interval":interval_ms, "ThumbnailCount":101
                        })
                    };
                    let item = json!({
                        "Id":"item",
                        "MediaSources":[
                            {"Id":"primary", "Height":2160}, {"Id":"alternate", "Height":1080}
                        ],
                        "Trickplay":{
                            "primary":{"320":tiles(320, 180, 10, 5000)},
                            "alternate":{
                                "160":tiles(160, 90, 10, 10000),
                                "240":tiles(240, 135, 10000, 10000),
                                "320":tiles(320, 180, 10, 10000),
                                "480":tiles(480, 270, 10, 10000)
                            }
                        }
                    });
                    resp(200, serde_json::to_vec(&item).unwrap(), &[])
                }
                "/Videos/alternate/Trickplay/320/1.jpg" => {
                    resp(200, TILE_BYTES, &[("Content-Type", "image/jpeg")])
                }
                // Jellyfin has no Emby list: asked for a version without
                // sheets.
                "/Items/item/ThumbnailSet" => resp(404, Vec::new(), &[]),
                path => panic!("unexpected request {path}"),
            }
        });
        let source = source(&server.base);
        let entry = |version| {
            let location = encode_version_location(&server.base, "item", version);
            Entry::new(
                "Film",
                source.canonical_location(&location).unwrap(),
                EntryKind::Video,
            )
        };
        let alternate = entry("alternate");
        assert!(source.has_trickplay(&alternate));
        assert_eq!(server.count(|_| true), 0);

        let preview = source.trickplay(&alternate).unwrap().unwrap();
        assert_eq!(
            preview.timing,
            TrickplayTiming::Fixed {
                tile: [320, 180],
                interval_ms: 10000,
                frames: 101,
                columns: 10,
                rows: 10
            }
        );
        assert_eq!(preview.fetch_sheet(1, &|| false).unwrap(), TILE_BYTES);
        assert!(source.trickplay(&entry("missing")).unwrap().is_none());
        assert_eq!(server.count(|req| req.path().starts_with("/Videos/")), 1);
    }

    #[test]
    fn emby_lists_its_pictures_and_serves_each_by_tag_and_moment() {
        let server = TestServer::start(|req| match req.path() {
            "/Users/user/Items/item" => resp(
                200,
                serde_json::to_vec(&json!({
                    "Id":"item",
                    "MediaSources":[{"Id":"mediasource_7", "Height":2160}]
                }))
                .unwrap(),
                &[],
            ),
            "/Items/item/ThumbnailSet" => {
                assert!(
                    req.url.contains("MediaSourceId=mediasource_7"),
                    "{}",
                    req.url
                );
                // Unevenly spaced, out of order, one moment twice.
                resp(
                    200,
                    serde_json::to_vec(&json!({"AspectRatio": 1.78, "Thumbnails": [
                        {"PositionTicks": 300_000_000u64, "ImageTag": "c"},
                        {"PositionTicks": 0, "ImageTag": "a"},
                        {"PositionTicks": 40_000_000, "ImageTag": "old"},
                        {"PositionTicks": 40_000_000, "ImageTag": "b"},
                        {"PositionTicks": 50_000_000, "ImageTag": " "}
                    ]}))
                    .unwrap(),
                    &[],
                )
            }
            "/Items/item/Images/Thumbnail" => {
                for part in [
                    "tag=b&",
                    "PositionTicks=40000000&",
                    "MediaSourceId=mediasource_7",
                ] {
                    assert!(req.url.contains(part), "{}", req.url);
                }
                resp(200, b"picture b", &[("Content-Type", "image/jpeg")])
            }
            path => panic!("unexpected request {path}"),
        });
        let source = source(&server.base);
        let entry = Entry::new(
            "Film",
            source
                .canonical_location(&crate::mediabrowser::encode_location(
                    &server.base,
                    "item",
                    "item",
                ))
                .unwrap(),
            EntryKind::Video,
        );
        let preview = source.trickplay(&entry).unwrap().unwrap();
        assert_eq!(
            preview.timing,
            TrickplayTiming::Listed(vec![0, 4_000, 30_000].into())
        );
        assert_eq!(preview.fetch_sheet(1, &|| false).unwrap(), b"picture b");
        assert!(preview.fetch_sheet(3, &|| false).is_err());
    }
}
