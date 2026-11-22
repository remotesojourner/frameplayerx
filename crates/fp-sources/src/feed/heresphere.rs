//! The HereSphere JSON API's format.

use super::{
    FeedScene, SceneGroup, SceneInfo, date_field, media_source, rating_field, stereo_from,
    url_field,
};
use crate::error::{Error, Result};
use crate::json::{array, boolean, num, text};
use crate::urlutil::{last_segment, redact, resolve};
use fp_core::format::{Projection, VideoFormat, detect_from_name};
use fp_core::source::{Entry, EntryKind};
use serde_json::Value;

/// Declared format of a HereSphere video.
fn heresphere_format(v: &Value) -> Option<VideoFormat> {
    let projection = text(v, "projection")?;
    let fov = num(v, "fov")
        .filter(|f| *f > 0.0 && *f <= 360.0)
        .map(|f| f as f32);
    let lens = text(v, "lens").unwrap_or_default().to_ascii_uppercase();
    let projection = match projection.to_ascii_lowercase().as_str() {
        "equirectangular" => Projection::Equirect {
            h_fov: fov.unwrap_or(180.0),
            v_fov: 180.0,
        },
        "equirectangular360" => Projection::EQUIRECT_360,
        "fisheye" => match lens.as_str() {
            "MKX220" | "VRCA220" => Projection::fisheye(220.0),
            "MKX200" => Projection::fisheye(200.0),
            _ => Projection::fisheye(fov.unwrap_or(180.0)),
        },
        "perspective" => Projection::Flat,
        // fp-core has no plain cubemap; EAC is the closest renderer.
        "cubemap" | "equiangularcubemap" => Projection::Eac { h_fov: 360.0 },
        _ => return None,
    };
    let stereo = text(v, "stereo")
        .and_then(|s| stereo_from(&s))
        .unwrap_or_default();
    Some(VideoFormat::new(projection, stereo))
}

/// Parses a HereSphere video document fetched from `base`.
pub fn parse_heresphere_video(v: &Value, base: &str) -> SceneInfo {
    let mut sources = Vec::new();
    for m in array(v, "media") {
        let label = text(m, "name").unwrap_or_default();
        for s in array(m, "sources") {
            if let Some(src) = media_source(s, &label, base) {
                sources.push(src);
            }
        }
    }
    let mut markers = Vec::new();
    let mut meta = HereSphereTags::default();
    for t in array(v, "tags") {
        let Some(name) = text(t, "name") else {
            continue;
        };
        // Timed tags are chapters, except facts some servers lay along the
        // timeline; the rest describe the video.
        let start = num(t, "start").filter(|s| *s > 0.0);
        let end = num(t, "end").filter(|e| *e > 0.0);
        match (start, end) {
            (None, None) => meta.add(&name),
            _ if timeline_fact(&name) => {}
            _ => markers.push((start.unwrap_or(0.0) / 1000.0, chapter_name(&name))),
        }
    }
    markers.sort_by(|a, b| a.0.total_cmp(&b.0));
    SceneInfo {
        title: text(v, "title").unwrap_or_default(),
        duration: num(v, "duration")
            .filter(|d| *d > 0.0)
            .map(|ms| ms / 1000.0),
        thumbnail_url: url_field(v, "thumbnailImage", base),
        trickplay_url: None,
        format: heresphere_format(v),
        sources,
        scripts: array(v, "scripts")
            .iter()
            .filter_map(|s| url_field(s, "url", base))
            .collect(),
        subtitles: super::subtitle_files(v, base),
        markers,
        description: text(v, "description"),
        released: date_field(v, "dateReleased"),
        added: date_field(v, "dateAdded"),
        studio: meta.studio,
        actors: meta.actors,
        tags: meta.tags,
        rating: rating_field(v, "rating"),
        favorite: boolean(v, "isFavorite"),
        event_server: url_field(v, "eventServer", base),
        write_favorite: boolean(v, "writeFavorite").unwrap_or(false),
    }
}

/// What HereSphere's prefixed tags (`Studio:`, `Talent:`, `Category:`...)
/// say about a video.
#[derive(Default)]
struct HereSphereTags {
    studio: Option<String>,
    actors: Vec<String>,
    tags: Vec<String>,
}

impl HereSphereTags {
    fn add(&mut self, tag: &str) {
        let (prefix, value) = match tag.split_once(':') {
            Some((p, v)) if !p.contains(' ') || p.eq_ignore_ascii_case("tag group") => {
                (p.trim().to_ascii_lowercase(), v.trim())
            }
            _ => (String::new(), tag.trim()),
        };
        if value.is_empty() {
            return;
        }
        match prefix.as_str() {
            "studio" | "site" => {
                self.studio.get_or_insert_with(|| value.to_string());
            }
            "talent" | "performer" | "actor" | "cast" | "@" => self.actors.push(value.to_string()),
            // Computed facts (resolution, frame rate...) and groupings.
            "feature" | "tag group" | "director" | "movie" | "group" | "%" => {}
            _ => self.tags.push(value.to_string()),
        }
    }
}

/// Facts about the video that some servers lay along the timeline as timed
/// tags (a summary of its tags, how often it was played, its resolution),
/// so HereSphere shows them on its timeline. They aren't chapters.
fn timeline_fact(name: &str) -> bool {
    const FACTS: [&str; 6] = [
        "summary",
        "played",
        "o-count",
        "resolution",
        "organized",
        "rating",
    ];
    name.split_once(':')
        .is_some_and(|(p, _)| FACTS.contains(&p.trim().to_ascii_lowercase().as_str()))
}

/// A chapter's name from its tag: `Tag:Title`, whose tag is often its title
/// again (`Blowjob:Blowjob`), once; `Marker:Title` as the title.
fn chapter_name(name: &str) -> String {
    match name.split_once(':') {
        Some((tag, title)) => {
            let (tag, title) = (tag.trim(), title.trim());
            if title.is_empty() {
                tag.to_string()
            } else if tag.is_empty()
                || tag.eq_ignore_ascii_case(title)
                || tag.eq_ignore_ascii_case("marker")
            {
                title.to_string()
            } else {
                format!("{tag}: {title}")
            }
        }
        None => name.trim().to_string(),
    }
}

/// A scan's duration in seconds. A video's own document gives milliseconds,
/// but some servers' scans give seconds (4893.57 for 4893570): a day or more
/// can only be milliseconds, anything less is read as seconds (milliseconds
/// would be a clip of under a minute and a half).
fn scan_seconds(d: f64) -> f64 {
    if d >= 86_400.0 { d / 1000.0 } else { d }
}

/// Parses a HereSphere scan (`{"scanData": [...]}`): the details of every
/// video at once, without thumbnails or media.
pub fn parse_heresphere_scan(v: &Value, base: &str) -> Vec<FeedScene> {
    array(v, "scanData")
        .iter()
        .filter_map(|s| {
            let url = url_field(s, "link", base)?;
            let mut meta = HereSphereTags::default();
            for t in array(s, "tags") {
                if let Some(n) = text(t, "name") {
                    meta.add(&n);
                }
            }
            let title = text(s, "title").unwrap_or_else(|| last_segment(&url));
            Some(FeedScene {
                format: detect_from_name(&title),
                title,
                duration: num(s, "duration").filter(|d| *d > 0.0).map(scan_seconds),
                released: date_field(s, "dateReleased"),
                added: date_field(s, "dateAdded"),
                rating: rating_field(s, "rating"),
                favorite: boolean(s, "isFavorite").unwrap_or(false),
                studio: meta.studio,
                actors: meta.actors,
                tags: meta.tags,
                url,
                ..FeedScene::default()
            })
        })
        .collect()
}

/// Parses a HereSphere library (`{"library":[{"name","list":[urls]}]}`).
/// Scenes carry only their URL (named after its last segment) until their
/// details are fetched.
pub fn parse_heresphere_library(v: &Value, base: &str) -> Result<Vec<SceneGroup>> {
    if num(v, "access").is_some_and(|a| a < 0.0) {
        return Err(Error::Auth(redact(base)));
    }
    Ok(array(v, "library")
        .iter()
        .enumerate()
        .map(|(i, g)| SceneGroup {
            name: text(g, "name").unwrap_or_else(|| format!("Group {}", i + 1)),
            scenes: array(g, "list")
                .iter()
                .filter_map(|u| u.as_str())
                .filter_map(|u| resolve(base, u).ok())
                .map(|url| {
                    let name = last_segment(&url);
                    Entry::new(name, url, EntryKind::Video)
                })
                .collect(),
        })
        .collect())
}

/// The scan endpoint beside a HereSphere library URL (query kept).
pub(super) fn scan_url(library: &str) -> String {
    let (path, query) = match library.split_once('?') {
        Some((p, q)) => (p, Some(q)),
        None => (library, None),
    };
    let mut url = format!("{}/scan", path.trim_end_matches('/'));
    if let Some(q) = query {
        url.push('?');
        url.push_str(q);
    }
    url
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::feed::FeedLibrary;
    use fp_core::format::StereoLayout;
    use serde_json::json;

    #[test]
    fn heresphere_library_and_video() {
        let lib = json!({"access": 1, "library": [
            {"name": "All", "list": ["http://vr:9999/heresphere/1", "/heresphere/2"]}
        ]});
        let g = parse_heresphere_library(&lib, "http://vr:9999/heresphere").unwrap();
        assert_eq!(g[0].scenes.len(), 2);
        assert_eq!(g[0].scenes[1].location, "http://vr:9999/heresphere/2");
        assert_eq!(g[0].scenes[1].name, "2");
        assert!(matches!(
            parse_heresphere_library(&json!({"access": -1}), "http://h/"),
            Err(Error::Auth(_))
        ));

        let v = json!({
            "title": "Beach", "duration": 1_800_000.0, "thumbnailImage": "http://x/t.jpg",
            "projection": "fisheye", "stereo": "sbs", "lens": "MKX220", "fov": 180.0,
            "media": [{"name": "h265", "sources": [
                {"resolution": 2880, "height": 2880, "width": 5760, "size": 123, "url": "http://x/6k.mp4"},
                {"resolution": 1440, "height": 1440, "width": 2880, "url": "http://x/3k.mp4"}
            ]}],
            "scripts": [{"name": "Beach.funscript", "url": "http://x/Beach.funscript"}],
            "subtitles": [{"name": "English", "language": "en", "url": "http://x/Beach.en.srt"}],
            "tags": [{"name": "Talk", "start": 1500.0, "end": 9000.0}, {"name": "Genre:VR"}]
        });
        let s = parse_heresphere_video(&v, "http://x/heresphere/1");
        assert_eq!(s.duration, Some(1800.0));
        assert_eq!(
            s.format,
            Some(VideoFormat::new(
                Projection::fisheye(220.0),
                StereoLayout::SideBySide
            ))
        );
        assert_eq!(s.best_source(Some(2000)).unwrap().url, "http://x/3k.mp4");
        assert_eq!(s.best_source(None).unwrap().size, Some(123));
        assert_eq!(s.subtitles[0].location, "http://x/Beach.en.srt");
        assert_eq!(
            (
                s.subtitles[0].name.as_str(),
                s.subtitles[0].language.as_deref()
            ),
            ("English", Some("en"))
        );
        assert_eq!(s.markers, [(1.5, "Talk".to_string())]);
    }

    #[test]
    fn heresphere_projections() {
        let p = |v: Value| heresphere_format(&v).map(|f| (f.projection, f.stereo));
        assert_eq!(
            p(json!({"projection": "equirectangular", "stereo": "tb"})),
            Some((Projection::EQUIRECT_180, StereoLayout::TopBottom))
        );
        assert_eq!(
            p(json!({"projection": "equirectangular360", "stereo": "mono"})),
            Some((Projection::EQUIRECT_360, StereoLayout::Mono))
        );
        assert_eq!(
            p(json!({"projection": "fisheye", "lens": "Linear", "fov": 190})),
            Some((Projection::fisheye(190.0), StereoLayout::Mono))
        );
        assert_eq!(
            p(json!({"projection": "fisheye", "lens": "MKX200"})),
            Some((Projection::fisheye(200.0), StereoLayout::Mono))
        );
        assert_eq!(
            p(json!({"projection": "perspective"})).map(|x| x.0),
            Some(Projection::Flat)
        );
        assert_eq!(
            p(json!({"projection": "equiangularCubemap"})).map(|x| x.0),
            Some(Projection::Eac { h_fov: 360.0 })
        );
        assert_eq!(p(json!({"projection": "weird"})), None);
        assert_eq!(p(json!({})), None);
    }

    #[test]
    fn heresphere_tags_describe_or_mark() {
        let v = json!({
            "title": "Beach", "dateReleased": "2023-05-01", "dateAdded": "2024-02-03",
            "rating": 4, "isFavorite": true,
            "tags": [
                {"name": "Studio:VRStudio"}, {"name": "Talent:Jane"}, {"name": "Talent:Ann"},
                {"name": "Category:Outdoor"}, {"name": "Feature:8K"}, {"name": "Tag Group:Set"},
                {"name": "POV"}, {"name": "Intro", "start": 2000.0, "end": 9000.0},
                {"name": "Marker:Dance", "start": 60000.0}
            ]
        });
        let s = parse_heresphere_video(&v, "http://x/heresphere/1");
        assert_eq!(s.studio.as_deref(), Some("VRStudio"));
        assert_eq!(s.actors, ["Jane", "Ann"]);
        assert_eq!(s.tags, ["Outdoor", "POV"]);
        assert_eq!(
            s.markers,
            [(2.0, "Intro".to_string()), (60.0, "Dance".to_string())]
        );
        assert!(s.released.is_some() && s.added > s.released);
        assert_eq!(s.rating, Some(4.0));
        assert_eq!(s.favorite, Some(true));
    }

    #[test]
    fn chapters_apart_from_facts_on_the_timeline() {
        // As a server that lays facts along the timeline sends them.
        let v = json!({
            "duration": 3_000_000.0,
            "tags": [
                {"name": "Summary:Big | Outdoor", "start": 0.1, "track": 0},
                {"name": "Played:3", "start": 0.1, "end": 500_000.0, "track": 1},
                {"name": "O-Count:0", "start": 500_000.0, "end": 1_000_000.0, "track": 1},
                {"name": "Resolution:4320p", "start": 1_000_000.0, "end": 1_500_000.0, "track": 1},
                {"name": "Organized:true", "start": 1_500_000.0, "end": 2_000_000.0, "track": 1},
                {"name": "Rating:?", "start": 2_000_000.0, "end": 3_000_000.0, "track": 1},
                {"name": "Side Fuck:Side Fuck", "start": 1_340_000.0, "track": 0},
                {"name": "Blowjob:Blowjob", "start": 80_000.0, "track": 0},
                {"name": "Blowjob:Close up", "start": 90_000.0, "track": 0},
                {"name": "#:Outdoor", "track": 2},
                {"name": "@:Jane Doe", "track": 3},
                {"name": "%:Summer Collection", "track": 4},
                {"name": "Studio:VRStudio", "track": 5}
            ]
        });
        let s = parse_heresphere_video(&v, "http://x/heresphere/1");
        assert_eq!(
            s.markers,
            [
                (80.0, "Blowjob".to_string()),
                (90.0, "Blowjob: Close up".to_string()),
                (1340.0, "Side Fuck".to_string())
            ]
        );
        assert_eq!(s.tags, ["Outdoor"]);
        assert_eq!(s.actors, ["Jane Doe"]);
        assert_eq!(s.studio.as_deref(), Some("VRStudio"));
    }

    #[test]
    fn libraries_scans_and_cards() {
        let base = "http://x/heresphere";
        let g = parse_heresphere_library(
            &json!({"library": [
                {"name": "Recent", "list": ["/heresphere/1", "/heresphere/2"]},
                {"name": "Favs", "list": ["/heresphere/2", "/heresphere/3"]}
            ]}),
            base,
        )
        .unwrap();
        let mut lib = FeedLibrary::from_groups(g);
        assert_eq!(lib.groups.len(), 2);
        assert_eq!(
            lib.groups[1].1,
            ["http://x/heresphere/2", "http://x/heresphere/3"]
        );
        assert_eq!(lib.scenes.len(), 3, "scenes are listed once");
        assert_eq!(lib.scenes[0].title, "1");

        let scan = parse_heresphere_scan(
            &json!({"scanData": [
                {"link": "/heresphere/2", "title": "Lake_180_LR", "duration": 90000,
                 "dateReleased": "2022-01-01", "isFavorite": true,
                 "tags": [{"name": "Talent:Jane"}, {"name": "Category:Nature"}]},
                {"title": "no link"},
                // Some servers' scans give seconds where the video's own
                // document gives milliseconds.
                {"link": "/heresphere/3", "duration": 4893.57}
            ]}),
            base,
        );
        assert_eq!(scan.len(), 2);
        assert_eq!(scan[1].duration, Some(4893.57));
        lib.scenes[1].merge(scan.into_iter().next().unwrap());
        let s = &lib.scenes[1];
        assert_eq!((s.title.as_str(), s.duration), ("Lake_180_LR", Some(90.0)));
        assert!(s.favorite && s.format.is_some() && !s.detailed);
        let words = |t: &str| vec![t.to_string()];
        assert!(s.matches(&words("jane")) && s.matches(&words("nature")));
        assert!(s.matches(&["lake".into(), "jane".into()]));
        assert!(!s.matches(&words("forest")));

        let mut card = lib.scenes[0].clone();
        card.apply(&SceneInfo {
            title: "Forest".into(),
            thumbnail_url: Some("http://x/t.jpg".into()),
            scripts: vec!["http://x/a.funscript".into()],
            ..SceneInfo::default()
        });
        assert!(card.detailed && card.scripted);
        assert_eq!(
            card.to_entry().thumbnail_url.as_deref(),
            Some("http://x/t.jpg")
        );
        assert_eq!(card.to_entry().location, "http://x/heresphere/1");

        assert_eq!(scan_url("http://h/heresphere/"), "http://h/heresphere/scan");
        assert_eq!(
            scan_url("http://h/hs?apikey=1"),
            "http://h/hs/scan?apikey=1"
        );
    }
}
