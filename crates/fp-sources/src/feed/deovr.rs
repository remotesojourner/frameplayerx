//! The DeoVR JSON format.

use super::{
    MediaSource, SceneGroup, SceneInfo, date_field, media_source, names, rating_field, stereo_from,
    url_field,
};
use crate::json::{array, boolean, get, num, text};
use crate::urlutil::last_segment;
use fp_core::format::{Projection, StereoLayout, VideoFormat, detect_from_name};
use fp_core::source::{Entry, EntryKind, is_video_name};
use serde_json::Value;

/// DeoVR puts a sheet's tile size in its filename, even on signed URLs.
pub(super) fn timeline_tile_size(url: &str) -> Option<[u32; 2]> {
    let url = url::Url::parse(url).ok()?;
    if !matches!(url.scheme(), "http" | "https")
        || !url.username().is_empty()
        || url.password().is_some()
    {
        return None;
    }
    let name = last_segment(url.as_str()).to_ascii_lowercase();
    let (stem, extension) = name.rsplit_once('.')?;
    if !matches!(extension, "jpg" | "jpeg" | "png" | "webp") {
        return None;
    }
    let (_, size) = stem.rsplit_once("timelinepreview")?;
    let (width, height) = size.split_once('x')?;
    let (width, height): (u32, u32) = (width.parse().ok()?, height.parse().ok()?);
    let valid_side = |side| (1..=crate::TRICKPLAY_MAX_DIMENSION).contains(&side);
    (valid_side(width)
        && valid_side(height)
        && u64::from(width) * u64::from(height) <= crate::TRICKPLAY_MAX_PIXELS)
        .then_some([width, height])
}

fn timeline_preview(v: &Value, base: &str) -> Option<String> {
    let preview = url_field(v, "timelinePreview", base)?;
    timeline_tile_size(&preview)?;
    Some(preview)
}

/// Projection for a DeoVR `screenType`.
fn deovr_projection(screen_type: &str) -> Option<Projection> {
    Some(match screen_type.trim().to_ascii_lowercase().as_str() {
        "dome" | "180" => Projection::EQUIRECT_180,
        "sphere" | "360" => Projection::EQUIRECT_360,
        "flat" => Projection::Flat,
        "fisheye" => Projection::fisheye(180.0),
        "mkx200" => Projection::fisheye(200.0),
        "mkx220" | "vrca220" => Projection::fisheye(220.0),
        "rf52" => Projection::fisheye(190.0),
        _ => return None,
    })
}

/// Declared format of a DeoVR scene, `None` when it says nothing.
fn deovr_format(v: &Value) -> Option<VideoFormat> {
    let is3d = boolean(v, "is3d");
    let screen = text(v, "screenType").and_then(|s| deovr_projection(&s));
    let stereo = text(v, "stereoMode").and_then(|s| stereo_from(&s));
    if is3d.is_none() && screen.is_none() && stereo.is_none() {
        return None;
    }
    let (projection, stereo) = match is3d {
        Some(false) if screen.is_none() => (Projection::Flat, stereo.unwrap_or_default()),
        _ => (
            screen.unwrap_or(Projection::EQUIRECT_180),
            stereo.unwrap_or(if is3d == Some(false) {
                StereoLayout::Mono
            } else {
                StereoLayout::SideBySide
            }),
        ),
    };
    Some(VideoFormat::new(projection, stereo))
}

/// Parses a DeoVR scene document fetched from `base`.
pub fn parse_deovr_scene(v: &Value, base: &str) -> SceneInfo {
    let mut sources = Vec::new();
    for enc in array(v, "encodings") {
        let label = text(enc, "name").unwrap_or_default();
        for s in array(enc, "videoSources") {
            if let Some(m) = media_source(s, &label, base) {
                sources.push(m);
            }
        }
    }
    // Some feeds put a single URL on the scene itself.
    if sources.is_empty()
        && let Some(url) =
            url_field(v, "videoUrl", base).or_else(|| url_field(v, "video_url", base))
        && is_video_name(&last_segment(&url))
    {
        sources.push(MediaSource {
            url,
            ..MediaSource::default()
        });
    }
    let mut markers: Vec<(f64, String)> = array(v, "timeStamps")
        .iter()
        .filter_map(|t| Some((num(t, "ts")?, text(t, "name").unwrap_or_default())))
        .collect();
    markers.sort_by(|a, b| a.0.total_cmp(&b.0));
    // A category is `{"tag": {"name"}}` or `{"name"}`.
    let tags = array(v, "categories")
        .iter()
        .filter_map(|c| {
            get(c, "tag")
                .and_then(|t| text(t, "name"))
                .or_else(|| text(c, "name"))
                .or_else(|| c.as_str().map(|s| s.trim().to_string()))
        })
        .filter(|s| !s.is_empty())
        .collect();
    SceneInfo {
        title: text(v, "title").unwrap_or_default(),
        duration: num(v, "videoLength").filter(|d| *d > 0.0),
        thumbnail_url: url_field(v, "thumbnailUrl", base),
        trickplay_url: timeline_preview(v, base),
        format: deovr_format(v),
        sources,
        scripts: array(v, "fleshlight")
            .iter()
            .filter_map(|s| url_field(s, "url", base))
            .collect(),
        subtitles: super::subtitle_files(v, base),
        markers,
        description: text(v, "description"),
        released: date_field(v, "date").or_else(|| date_field(v, "releaseDate")),
        added: None,
        studio: get(v, "paysite")
            .and_then(|p| text(p, "name"))
            .or_else(|| text(v, "studio")),
        actors: names(v, "actors"),
        tags,
        rating: rating_field(v, "rating_avg").or_else(|| rating_field(v, "rating")),
        favorite: boolean(v, "isFavorite"),
        event_server: None,
        write_favorite: false,
    }
}

/// Parses a DeoVR feed root (`{"scenes":[{"name","list":[...]}]}`).
pub fn parse_deovr_root(v: &Value, base: &str) -> Vec<SceneGroup> {
    array(v, "scenes")
        .iter()
        .enumerate()
        .map(|(i, g)| SceneGroup {
            name: text(g, "name").unwrap_or_else(|| format!("Group {}", i + 1)),
            scenes: array(g, "list")
                .iter()
                .filter_map(|s| {
                    let url = url_field(s, "video_url", base)
                        .or_else(|| url_field(s, "videoUrl", base))?;
                    let title = text(s, "title").unwrap_or_else(|| last_segment(&url));
                    let mut e = Entry::new(title, url, EntryKind::Video);
                    e.duration = num(s, "videoLength").filter(|d| *d > 0.0);
                    e.thumbnail_url = url_field(s, "thumbnailUrl", base);
                    e.trickplay_url = timeline_preview(s, base);
                    e.format = detect_from_name(&e.name);
                    Some(e)
                })
                .collect(),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn deovr_root() {
        let v = json!({
            "scenes": [
                {"name": "Recent", "list": [
                    {"title": "Beach_180_LR", "videoLength": 1800, "thumbnailUrl": "/img/1.jpg",
                     "video_url": "http://vr:9999/deovr/1"},
                    {"title": "No URL"},
                    {"title": "Rel", "videoLength": "60", "video_url": "/deovr/2",
                     "timelinePreview": "/asset/4096_timelinePreview341x195.jpg"}
                ]},
                {"list": []}
            ]
        });
        let g = parse_deovr_root(&v, "http://vr:9999/deovr");
        assert_eq!(g.len(), 2);
        assert_eq!(g[0].name, "Recent");
        assert_eq!(g[1].name, "Group 2");
        assert_eq!(g[0].scenes.len(), 2);
        let s = &g[0].scenes[0];
        assert_eq!(s.location, "http://vr:9999/deovr/1");
        assert_eq!(s.duration, Some(1800.0));
        assert_eq!(s.thumbnail_url.as_deref(), Some("http://vr:9999/img/1.jpg"));
        assert_eq!(s.format.unwrap().stereo, StereoLayout::SideBySide);
        assert_eq!(g[0].scenes[1].duration, Some(60.0));
        assert_eq!(g[0].scenes[1].location, "http://vr:9999/deovr/2");
        assert_eq!(
            g[0].scenes[1].trickplay_url.as_deref(),
            Some("http://vr:9999/asset/4096_timelinePreview341x195.jpg")
        );
        assert!(parse_deovr_root(&json!({"x": 1}), "http://h/").is_empty());
    }

    #[test]
    fn deovr_scene_and_best_source() {
        let v = json!({
            "title": "Beach",
            "videoLength": 1800,
            "is3d": true,
            "screenType": "mkx200",
            "stereoMode": "sbs",
            "thumbnailUrl": "http://vr/img/1.jpg",
            "timelinePreview": " ../img/4096_timelinePreview341x195.jpg?signature=a%2Fb ",
            "encodings": [
                {"name": "h265", "videoSources": [
                    {"resolution": 4096, "height": 4096, "width": 8192, "url": "http://vr/f/8k.mp4"},
                    {"resolution": 2880, "url": "http://vr/f/6k.mp4"}
                ]},
                {"name": "h264", "videoSources": [
                    {"resolution": "1920", "height": 1920, "width": 3840, "url": "/f/4k.mp4"},
                    {"height": 1920}
                ]}
            ],
            "timeStamps": [{"ts": 300, "name": "Two"}, {"ts": 10, "name": "One"}],
            "fleshlight": [{"title": "Beach.funscript", "url": "http://vr/s/1.funscript"}]
        });
        let s = parse_deovr_scene(&v, "http://vr/deovr/1");
        assert_eq!(s.title, "Beach");
        assert_eq!(s.duration, Some(1800.0));
        assert_eq!(
            s.format,
            Some(VideoFormat::new(
                Projection::fisheye(200.0),
                StereoLayout::SideBySide
            ))
        );
        assert_eq!(s.sources.len(), 3);
        assert_eq!(s.best_source(None).unwrap().url, "http://vr/f/8k.mp4");
        assert_eq!(s.best_source(Some(2880)).unwrap().url, "http://vr/f/6k.mp4");
        assert_eq!(s.best_source(Some(2000)).unwrap().url, "http://vr/f/4k.mp4");
        assert_eq!(s.best_source(Some(100)).unwrap().url, "http://vr/f/4k.mp4");
        assert_eq!(
            s.markers,
            [(10.0, "One".to_string()), (300.0, "Two".to_string())]
        );
        assert_eq!(s.scripts, ["http://vr/s/1.funscript"]);

        let mut e = Entry::new("x", "http://vr/deovr/1", EntryKind::Video);
        s.apply_to(&mut e);
        assert_eq!(e.name, "Beach");
        assert_eq!(e.location, "http://vr/deovr/1");
        assert_eq!(e.scripts.len(), 1);
        assert_eq!(e.markers.len(), 2);
        assert_eq!(e.format, s.format);
        let sheet = e.trickplay_url.as_deref().unwrap();
        assert_eq!(
            sheet,
            "http://vr/img/4096_timelinePreview341x195.jpg?signature=a%2Fb"
        );
        assert_eq!(timeline_tile_size(sheet), Some([341, 195]));
    }

    #[test]
    fn timeline_ignores_unknown_sizes_and_unsafe_urls() {
        for value in [
            "file:///tmp/timelinePreview341x195.jpg",
            "http://user:pass@vr/img/timelinePreview341x195.jpg",
            "/img/timelinePreview0x195.jpg",
            "/img/timelinePreview8192x8192.jpg",
            "/img/timelinePreview341x4294967296.jpg",
            "/img/timelinePreview.jpg",
            "/img/sheet.jpg?hint=timelinePreview341x195.jpg",
        ] {
            let s = parse_deovr_scene(&json!({"timelinePreview": value}), "http://vr/scene");
            assert!(s.trickplay_url.is_none(), "{value}");
        }
    }

    #[test]
    fn deovr_screen_types() {
        let f = |v: Value| deovr_format(&v);
        let p = |v: Value| f(v).map(|x| x.projection);
        assert_eq!(
            p(json!({"screenType": "dome"})),
            Some(Projection::EQUIRECT_180)
        );
        assert_eq!(
            p(json!({"screenType": "sphere"})),
            Some(Projection::EQUIRECT_360)
        );
        assert_eq!(p(json!({"screenType": "flat"})), Some(Projection::Flat));
        assert_eq!(
            p(json!({"screenType": "fisheye"})),
            Some(Projection::fisheye(180.0))
        );
        assert_eq!(
            p(json!({"screenType": "mkx220"})),
            Some(Projection::fisheye(220.0))
        );
        assert_eq!(
            p(json!({"screenType": "VRCA220"})),
            Some(Projection::fisheye(220.0))
        );
        assert_eq!(
            p(json!({"screenType": "rf52"})),
            Some(Projection::fisheye(190.0))
        );
        assert_eq!(
            f(json!({"is3d": true, "stereoMode": "tb"})),
            Some(VideoFormat::new(
                Projection::EQUIRECT_180,
                StereoLayout::TopBottom
            ))
        );
        assert_eq!(
            f(json!({"is3d": false})),
            Some(VideoFormat::new(Projection::Flat, StereoLayout::Mono))
        );
        assert_eq!(
            f(json!({"is3d": "true", "screenType": "sphere", "stereoMode": "off"})),
            Some(VideoFormat::new(
                Projection::EQUIRECT_360,
                StereoLayout::Mono
            ))
        );
        assert_eq!(f(json!({"title": "x"})), None);
    }

    #[test]
    fn deovr_scene_metadata() {
        let v = json!({
            "title": "Beach", "description": "Sun and sand.", "date": 1_700_000_000,
            "paysite": {"id": 3, "name": "VRStudio"},
            "actors": [{"id": 1, "name": "Jane"}, {"id": 2, "name": "Ann"}],
            "categories": [{"tag": {"id": 1, "name": "Outdoor"}}, {"name": "POV"}],
            "rating_avg": 4.5, "isFavorite": true
        });
        let s = parse_deovr_scene(&v, "http://vr/deovr/1");
        assert_eq!(s.description.as_deref(), Some("Sun and sand."));
        let mut e = Entry::new("x", "http://vr/deovr/1", EntryKind::Video);
        s.apply_to(&mut e);
        assert_eq!(e.description.as_deref(), Some("Sun and sand."));
        assert_eq!(s.released, Some(1_700_000_000));
        assert_eq!(s.studio.as_deref(), Some("VRStudio"));
        assert_eq!(s.actors, ["Jane", "Ann"]);
        assert_eq!(s.tags, ["Outdoor", "POV"]);
        assert_eq!(s.rating, Some(4.5));
        assert_eq!(s.favorite, Some(true));
        // Milliseconds and ISO dates both work.
        assert_eq!(
            parse_deovr_scene(&json!({"date": 1_700_000_000_000_i64}), "http://h/").released,
            Some(1_700_000_000)
        );
        assert!(
            parse_deovr_scene(&json!({"date": "2024-01-12"}), "http://h/")
                .released
                .is_some()
        );
    }
}
