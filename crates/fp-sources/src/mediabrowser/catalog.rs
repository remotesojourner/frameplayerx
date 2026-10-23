//! What a Jellyfin/Emby client shows: libraries, posters, series and their
//! seasons, resume points, "next up" and favourites, read with the session
//! [`MediaBrowserSource`] browses with.
//!
//! Listings ask the server to search, sort, filter and page
//! (`/Users/{id}/Items`), so a library of thousands of films costs one small
//! request per page.

use super::{
    MediaBrowserSource, detect_from_name, encode_location, ticks_to_seconds, video_3d_format,
};
use crate::error::{Error, Result};
use crate::json::{array, boolean, get, num, text};
use crate::timeutil::parse_iso8601;
use crate::urlutil::encode_form_value;
use fp_core::format::VideoFormat;
use fp_core::source::{Entry, EntryKind};
use fp_core::sync::lock;
use serde_json::Value;

/// Fields asked for in listings, on top of those always sent (name, type,
/// user data, image tags, run time, ratings, episode numbers).
const LIST_FIELDS: &str = "Overview,Genres,Path,DateCreated,ChildCount,PremiereDate";

/// Fields asked for one item's details.
const DETAIL_FIELDS: &str =
    "Overview,Genres,Tags,Studios,People,Path,DateCreated,ChildCount,PremiereDate,MediaSources";

/// What a tag can be on: films, series, episodes and other videos.
const TAGGED: [&str; 5] = ["Movie", "Series", "Episode", "Video", "MusicVideo"];

/// Image types listings ask tags for.
const IMAGES: &str = "EnableImageTypes=Primary,Backdrop,Thumb&ImageTypeLimit=1";

/// Library kinds (`CollectionType`) without video.
const NOT_VIDEO: [&str; 6] = [
    "music",
    "books",
    "photos",
    "livetv",
    "playlists",
    "audiobooks",
];

/// What a library item is.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum MbKind {
    Movie,
    /// A TV series (holds seasons).
    Series,
    /// A season of a series (holds episodes).
    Season,
    /// An episode of a series.
    Episode,
    /// Any other video (home videos, mixed libraries).
    Video,
    MusicVideo,
    /// A collection of films.
    BoxSet,
    Folder,
    /// One of the user's libraries (a view).
    Library,
    /// Anything else (people, photos...).
    #[default]
    Other,
}

impl MbKind {
    fn parse(t: &str) -> MbKind {
        match t {
            "Movie" => MbKind::Movie,
            "Series" => MbKind::Series,
            "Season" => MbKind::Season,
            "Episode" => MbKind::Episode,
            "Video" => MbKind::Video,
            "MusicVideo" => MbKind::MusicVideo,
            "BoxSet" => MbKind::BoxSet,
            "Folder" => MbKind::Folder,
            "CollectionFolder" | "UserView" => MbKind::Library,
            _ => MbKind::Other,
        }
    }

    /// Plays rather than opens.
    pub fn playable(self) -> bool {
        matches!(
            self,
            MbKind::Movie | MbKind::Episode | MbKind::Video | MbKind::MusicVideo
        )
    }

    /// Shown as a 2:3 poster rather than 16:9 art.
    pub fn portrait(self) -> bool {
        matches!(
            self,
            MbKind::Movie | MbKind::Series | MbKind::Season | MbKind::BoxSet
        )
    }
}

/// One item of a Jellyfin/Emby library, with what a client shows of it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct MbItem {
    /// Server id.
    pub id: String,
    pub name: String,
    pub kind: MbKind,
    /// Of a library: `movies`, `tvshows`, `boxsets`, `homevideos`...
    pub collection_type: Option<String>,
    /// Plot or description.
    pub overview: Option<String>,
    /// Production year.
    pub year: Option<i32>,
    /// Length in seconds.
    pub runtime: Option<f64>,
    /// Audience rating out of 10.
    pub community_rating: Option<f32>,
    /// Age rating ("PG-13").
    pub official_rating: Option<String>,
    pub genres: Vec<String>,
    /// Tags (details only).
    pub tags: Vec<String>,
    /// Studios (details only).
    pub studios: Vec<String>,
    /// Cast and crew as (name, role), actors first (details only).
    pub people: Vec<(String, String)>,
    /// Series of an episode or season.
    pub series_id: Option<String>,
    /// Name of that series.
    pub series_name: Option<String>,
    /// Season of an episode.
    pub season_id: Option<String>,
    /// Episode number, or a season's number.
    pub index: Option<u32>,
    /// Season number of an episode.
    pub parent_index: Option<u32>,
    /// Items inside (seasons of a series, films of a collection).
    pub child_count: Option<u32>,
    /// Watched.
    pub played: bool,
    /// Resume position in seconds (0 when not started).
    pub position: f64,
    /// The user's favourite.
    pub favorite: bool,
    /// Unwatched episodes of a series or season.
    pub unplayed: Option<u32>,
    /// When it was added, Unix seconds.
    pub added: Option<i64>,
    /// 2:3 poster (an episode's still for episodes).
    pub poster: Option<String>,
    /// 16:9 art for rows of landscape cards.
    pub landscape: Option<String>,
    /// Large wide art for the top of a details page.
    pub backdrop: Option<String>,
    /// Location for [`Source::open`](crate::Source::open) and browsing.
    pub location: String,
    /// Projection and stereo layout from the file name or 3D metadata.
    pub format: Option<VideoFormat>,
    /// The files it can be played from (details only): one, or several
    /// when the server keeps more than one version (4K and 1080p, a cut).
    pub versions: Vec<MbVersion>,
    /// The version chosen with [`MediaBrowserSource::set_version`]
    /// (details only); `None` plays the tallest that fits the source's
    /// maximum height.
    pub version: Option<String>,
}

/// One version of a film or video: a media source of the item.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct MbVersion {
    /// The media source id, for [`MediaBrowserSource::set_version`].
    pub id: String,
    /// The server's name for it ("2160p", "Director's Cut").
    pub name: String,
    /// Picture height in pixels.
    pub height: Option<u32>,
    /// Video codec ("hevc").
    pub codec: Option<String>,
    /// Dynamic range other than SDR ("HDR", "DOVI").
    pub range: Option<String>,
    /// File size in bytes.
    pub size: Option<u64>,
}

impl MbVersion {
    /// What tells it apart: "2160p · HEVC · HDR · 18.2 GB", after the
    /// server's name when that says something more.
    pub fn label(&self) -> String {
        let mut parts = Vec::new();
        let height = self.height.map(|h| format!("{h}p"));
        if !self.name.is_empty() && height.as_deref() != Some(self.name.as_str()) {
            parts.push(self.name.clone());
        }
        parts.extend(height);
        parts.extend(self.codec.as_ref().map(|c| c.to_uppercase()));
        parts.extend(self.range.clone());
        parts.extend(self.size.map(|b| format!("{:.1} GB", b as f64 / 1e9)));
        parts.join(" · ")
    }
}

/// The versions in an item's `MediaSources`.
fn parse_versions(v: &Value) -> Vec<MbVersion> {
    array(v, "MediaSources")
        .iter()
        .filter_map(|ms| {
            let video = array(ms, "MediaStreams")
                .iter()
                .find(|s| text(s, "Type").as_deref() == Some("Video"));
            Some(MbVersion {
                id: text(ms, "Id")?,
                name: text(ms, "Name").unwrap_or_default(),
                height: super::media_source_height(ms),
                codec: video.and_then(|s| text(s, "Codec")),
                range: video
                    .and_then(|s| text(s, "VideoRangeType").or_else(|| text(s, "VideoRange")))
                    .filter(|r| {
                        !r.eq_ignore_ascii_case("SDR") && !r.eq_ignore_ascii_case("Unknown")
                    }),
                size: num(ms, "Size").filter(|s| *s > 0.0).map(|s| s as u64),
            })
        })
        .collect()
}

impl MbItem {
    /// Fraction watched when partly watched.
    pub fn progress(&self) -> Option<f32> {
        let d = self.runtime.filter(|d| *d > 0.0)?;
        (self.position > 0.0).then(|| (self.position / d).clamp(0.0, 1.0) as f32)
    }

    /// "S1:E3" for an episode.
    pub fn episode_label(&self) -> Option<String> {
        match (self.parent_index, self.index) {
            (Some(s), Some(e)) => Some(format!("S{s}:E{e}")),
            (None, Some(e)) => Some(format!("E{e}")),
            _ => None,
        }
    }

    /// The name to remember it by: episodes carry their series and number.
    pub fn full_title(&self) -> String {
        match (&self.series_name, self.episode_label()) {
            (Some(series), Some(ep)) if self.kind == MbKind::Episode => {
                format!("{series} {ep} · {}", self.name)
            }
            _ => self.name.clone(),
        }
    }

    /// A browse entry to open it with.
    pub fn to_entry(&self) -> Entry {
        let kind = if self.kind.playable() {
            EntryKind::Video
        } else {
            EntryKind::Directory
        };
        let mut e = Entry::new(self.full_title(), self.location.clone(), kind);
        e.duration = self.runtime;
        e.thumbnail_url = self.landscape.clone().or_else(|| self.poster.clone());
        e.modified = self.added;
        e.format = self.format;
        e
    }
}

/// Order of a listing.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum MbSort {
    /// By title.
    #[default]
    Name,
    /// Newest additions first.
    Added,
    /// By release date.
    Released,
    /// By audience rating.
    Rating,
    /// By length.
    Runtime,
    /// Shuffled.
    Random,
}

impl MbSort {
    fn api(self) -> &'static str {
        match self {
            MbSort::Name => "SortName",
            MbSort::Added => "DateCreated,SortName",
            MbSort::Released => "PremiereDate,ProductionYear,SortName",
            MbSort::Rating => "CommunityRating,SortName",
            MbSort::Runtime => "Runtime,SortName",
            MbSort::Random => "Random",
        }
    }

    /// Whether this order reads best largest first.
    pub fn default_descending(self) -> bool {
        matches!(self, MbSort::Added | MbSort::Released | MbSort::Rating)
    }
}

/// Which items a listing keeps.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum MbFilter {
    /// Everything.
    #[default]
    All,
    /// Not watched yet.
    Unplayed,
    /// Partly watched.
    Resumable,
    /// The user's favourites.
    Favorites,
}

impl MbFilter {
    fn api(self) -> Option<&'static str> {
        match self {
            MbFilter::All => None,
            MbFilter::Unplayed => Some("IsUnplayed"),
            MbFilter::Resumable => Some("IsResumable"),
            MbFilter::Favorites => Some("IsFavorite"),
        }
    }
}

/// A server-side listing: search, sort, filter and paging.
#[derive(Clone, Debug, PartialEq)]
pub struct MbQuery {
    /// Library, folder or collection to list; `None` for the whole server.
    pub parent: Option<String>,
    /// Item types to keep (`Movie`, `Series`...); empty keeps every type.
    pub types: Vec<String>,
    /// Look inside sub-folders too.
    pub recursive: bool,
    /// Words to search for; empty lists everything.
    pub search: String,
    pub sort: MbSort,
    /// Largest first.
    pub descending: bool,
    /// Which items to keep.
    pub filter: MbFilter,
    /// Items to skip (paging).
    pub start: usize,
    /// Items per page.
    pub limit: usize,
    /// Only items with one of these tags.
    pub tags: Vec<String>,
}

impl Default for MbQuery {
    fn default() -> MbQuery {
        MbQuery {
            parent: None,
            types: Vec::new(),
            recursive: false,
            search: String::new(),
            sort: MbSort::Name,
            descending: false,
            filter: MbFilter::All,
            start: 0,
            limit: 60,
            tags: Vec::new(),
        }
    }
}

impl MbQuery {
    /// Everything on the server with `tag`, by name.
    pub fn tagged(tag: &str) -> MbQuery {
        MbQuery {
            types: TAGGED.iter().map(ToString::to_string).collect(),
            recursive: true,
            tags: vec![tag.to_string()],
            ..MbQuery::default()
        }
    }

    /// What opening `item` lists: a film library its films, a TV library
    /// its series, a collection or folder what is in it.
    pub fn children(item: &MbItem) -> MbQuery {
        let types: &[&str] = match (item.kind, item.collection_type.as_deref()) {
            (MbKind::Library, Some("movies")) => &["Movie"],
            (MbKind::Library, Some("tvshows")) => &["Series"],
            (MbKind::Library, Some("boxsets")) => &["BoxSet"],
            (MbKind::Library, Some("homevideos")) => &["Video"],
            (MbKind::Library, Some("musicvideos")) => &["MusicVideo"],
            (MbKind::Library, _) => &["Movie", "Series", "Video", "MusicVideo"],
            _ => &[],
        };
        MbQuery {
            parent: Some(item.id.clone()),
            types: types.iter().map(ToString::to_string).collect(),
            recursive: item.kind == MbKind::Library,
            ..MbQuery::default()
        }
    }

    /// Path and query string of `/Users/{user}/Items` for this listing.
    pub(crate) fn path(&self, user: &str) -> String {
        let mut q = format!(
            "/Users/{user}/Items?Recursive={}&SortBy={}&SortOrder={}&StartIndex={}&Limit={}\
             &Fields={LIST_FIELDS}&{IMAGES}&EnableTotalRecordCount=true",
            self.recursive,
            self.sort.api(),
            if self.descending {
                "Descending"
            } else {
                "Ascending"
            },
            self.start,
            self.limit.max(1),
        );
        if let Some(p) = &self.parent {
            q += &format!("&ParentId={}", encode_form_value(p));
        }
        if !self.types.is_empty() {
            q += &format!("&IncludeItemTypes={}", self.types.join(","));
        }
        if let Some(f) = self.filter.api() {
            q += &format!("&Filters={f}");
        }
        let search = self.search.trim();
        if !search.is_empty() {
            q += &format!("&SearchTerm={}", encode_form_value(search));
        }
        if !self.tags.is_empty() {
            q += &format!("&Tags={}", encode_form_value(&self.tags.join("|")));
        }
        q
    }
}

/// What a search of the whole server looks through, a section each, as
/// Jellyfin's own search shows it: the titles of films, series, collections
/// and other videos. Episodes are left out; a series is found by its name.
pub(crate) const SEARCH_SECTIONS: [(&str, &[&str]); 4] = [
    ("Movies", &["Movie"]),
    ("Shows", &["Series"]),
    ("Collections", &["BoxSet"]),
    ("Videos", &["Video", "MusicVideo"]),
];

/// The tags (at most 20) that have every word of `text` in them, ignoring
/// case.
pub(crate) fn matching_tags(tags: &[String], text: &str) -> Vec<String> {
    let words: Vec<String> = text.split_whitespace().map(str::to_lowercase).collect();
    if words.is_empty() {
        return Vec::new();
    }
    tags.iter()
        .filter(|t| {
            let t = t.to_lowercase();
            words.iter().all(|w| t.contains(w.as_str()))
        })
        .take(20)
        .cloned()
        .collect()
}

/// Path and query string of one section of a search. It asks for no order,
/// as Jellyfin's web client doesn't, so the server ranks the matches itself:
/// exact and leading matches first.
pub(crate) fn search_path(user: &str, text: &str, types: &[&str], limit: usize) -> String {
    format!(
        "/Users/{user}/Items?SearchTerm={}&IncludeItemTypes={}&Recursive=true&Limit={}\
         &Fields={LIST_FIELDS}&{IMAGES}&EnableTotalRecordCount=false",
        encode_form_value(text.trim()),
        types.join(","),
        limit.max(1),
    )
}

/// One page of a listing.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct MbPage {
    /// Items on this page.
    pub items: Vec<MbItem>,
    /// Items in the whole listing.
    pub total: usize,
}

/// A server's front page.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct MbHome {
    /// The user's video libraries.
    pub libraries: Vec<MbItem>,
    /// Partly watched films and episodes.
    pub resume: Vec<MbItem>,
    /// The next episode of each series being watched.
    pub next_up: Vec<MbItem>,
    /// Newest additions, per library.
    pub latest: Vec<(MbItem, Vec<MbItem>)>,
}

/// `{base}/Items/{id}/Images/{kind}?...`
fn image_url(base: &str, id: &str, kind: &str, tag: Option<&str>, size: &str) -> String {
    let mut u = format!("{base}/Items/{id}/Images/{kind}?{size}&quality=90");
    if let Some(t) = tag {
        u += &format!("&tag={}", encode_form_value(t));
    }
    u
}

const POSTER: &str = "maxHeight=480";
const LANDSCAPE: &str = "maxWidth=640";
const BACKDROP: &str = "maxWidth=1280";

/// Parses one item. Images follow what the item has, then its parents'
/// (an episode without a still shows its series' poster).
pub(crate) fn parse_item(v: &Value, base: &str) -> Option<MbItem> {
    let id = text(v, "Id")?;
    let name = text(v, "Name").unwrap_or_default();
    let kind = MbKind::parse(&text(v, "Type").unwrap_or_default());
    let user = get(v, "UserData");
    let tags = get(v, "ImageTags");
    // Servers that send no tags at all get the plain primary image.
    let own_primary = match tags {
        None => Some(None),
        Some(t) => text(t, "Primary").map(Some),
    };
    let own_thumb = tags.and_then(|t| text(t, "Thumb"));
    let backdrop_tag = array(v, "BackdropImageTags")
        .first()
        .and_then(|t| t.as_str())
        .map(str::to_string);
    let parent_backdrop = text(v, "ParentBackdropItemId").map(|pid| {
        let tag = array(v, "ParentBackdropImageTags")
            .first()
            .and_then(|t| t.as_str())
            .map(str::to_string);
        (pid, tag)
    });

    let poster = own_primary
        .as_ref()
        .map(|tag| image_url(base, &id, "Primary", tag.as_deref(), POSTER))
        .or_else(|| {
            let sid = text(v, "SeriesId")?;
            let tag = text(v, "SeriesPrimaryImageTag")?;
            Some(image_url(base, &sid, "Primary", Some(&tag), POSTER))
        });
    let backdrop = backdrop_tag
        .as_ref()
        .map(|tag| image_url(base, &id, "Backdrop/0", Some(tag), BACKDROP))
        .or_else(|| {
            let (pid, tag) = parent_backdrop.as_ref()?;
            Some(image_url(base, pid, "Backdrop/0", tag.as_deref(), BACKDROP))
        });
    let landscape = if kind == MbKind::Episode && own_primary.is_some() {
        poster.clone()
    } else {
        own_thumb
            .map(|tag| image_url(base, &id, "Thumb", Some(&tag), LANDSCAPE))
            .or_else(|| {
                let pid = text(v, "ParentThumbItemId")?;
                let tag = text(v, "ParentThumbImageTag");
                Some(image_url(base, &pid, "Thumb", tag.as_deref(), LANDSCAPE))
            })
            .or_else(|| {
                backdrop_tag
                    .as_ref()
                    .map(|tag| image_url(base, &id, "Backdrop/0", Some(tag), LANDSCAPE))
            })
            .or_else(|| {
                let (pid, tag) = parent_backdrop.as_ref()?;
                Some(image_url(
                    base,
                    pid,
                    "Backdrop/0",
                    tag.as_deref(),
                    LANDSCAPE,
                ))
            })
            .or_else(|| (!kind.portrait()).then(|| poster.clone()).flatten())
    };

    // (actor, name, role): actors with their part, crew with their job.
    let mut people: Vec<(bool, String, String)> = array(v, "People")
        .iter()
        .filter_map(|p| {
            let name = text(p, "Name")?;
            let job = text(p, "Type");
            let actor = job.as_deref().is_none_or(|t| t == "Actor");
            let role = text(p, "Role").filter(|_| actor).or(job);
            Some((actor, name, role.unwrap_or_default()))
        })
        .collect();
    // Actors first, each group in the server's order.
    people.sort_by_key(|p| !p.0);

    let path = text(v, "Path").unwrap_or_default();
    let format = kind
        .playable()
        .then(|| {
            detect_from_name(&path)
                .or_else(|| detect_from_name(&name))
                .or_else(|| video_3d_format(v))
        })
        .flatten();
    let location = if kind.playable() {
        encode_location(base, "item", &id)
    } else {
        encode_location(base, "folder", &id)
    };
    let year = num(v, "ProductionYear")
        .filter(|y| *y > 0.0 && *y < 10000.0)
        .map(|y| y as i32);
    Some(MbItem {
        name,
        kind,
        collection_type: text(v, "CollectionType"),
        overview: text(v, "Overview"),
        year,
        runtime: num(v, "RunTimeTicks")
            .filter(|t| *t > 0.0)
            .map(ticks_to_seconds),
        community_rating: num(v, "CommunityRating")
            .filter(|r| *r > 0.0)
            .map(|r| r as f32),
        official_rating: text(v, "OfficialRating"),
        genres: strings(v, "Genres"),
        tags: tag_names(v),
        studios: array(v, "Studios")
            .iter()
            .filter_map(|s| text(s, "Name").or_else(|| s.as_str().map(str::to_string)))
            .collect(),
        people: people.into_iter().map(|(_, n, r)| (n, r)).collect(),
        series_id: text(v, "SeriesId"),
        series_name: text(v, "SeriesName"),
        season_id: text(v, "SeasonId"),
        index: num(v, "IndexNumber")
            .filter(|n| *n >= 0.0)
            .map(|n| n as u32),
        parent_index: num(v, "ParentIndexNumber")
            .filter(|n| *n >= 0.0)
            .map(|n| n as u32),
        child_count: num(v, "ChildCount").filter(|n| *n >= 0.0).map(|n| n as u32),
        played: user.and_then(|u| boolean(u, "Played")).unwrap_or(false),
        position: user
            .and_then(|u| num(u, "PlaybackPositionTicks"))
            .filter(|t| *t > 0.0)
            .map(ticks_to_seconds)
            .unwrap_or(0.0),
        favorite: user.and_then(|u| boolean(u, "IsFavorite")).unwrap_or(false),
        unplayed: user
            .and_then(|u| num(u, "UnplayedItemCount"))
            .filter(|n| *n >= 0.0)
            .map(|n| n as u32),
        added: text(v, "DateCreated").and_then(|d| parse_iso8601(&d)),
        poster,
        landscape,
        backdrop,
        location,
        format,
        versions: parse_versions(v),
        version: None,
        id,
    })
}

pub(super) fn strings(v: &Value, key: &str) -> Vec<String> {
    array(v, key)
        .iter()
        .filter_map(|s| s.as_str())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

/// An item's tags: Jellyfin's `Tags`, or Emby's `TagItems` (each a name
/// and an id).
pub(super) fn tag_names(v: &Value) -> Vec<String> {
    let tags = strings(v, "Tags");
    if !tags.is_empty() {
        return tags;
    }
    array(v, "TagItems")
        .iter()
        .filter_map(|t| text(t, "Name"))
        .map(|t| t.trim().to_string())
        .filter(|t| !t.is_empty())
        .collect()
}

/// Items of a `{"Items": [...]}` answer, or of a bare array (`/Latest`).
pub(crate) fn parse_list(v: &Value, base: &str) -> Vec<MbItem> {
    let items = match v {
        Value::Array(a) => a.as_slice(),
        _ => array(v, "Items"),
    };
    items.iter().filter_map(|i| parse_item(i, base)).collect()
}

/// A page of a `{"Items": [...], "TotalRecordCount": n}` answer.
pub(crate) fn parse_page(v: &Value, base: &str) -> MbPage {
    let items = parse_list(v, base);
    let total = num(v, "TotalRecordCount")
        .map(|n| n.max(0.0) as usize)
        .unwrap_or(items.len());
    MbPage {
        total: total.max(items.len()),
        items,
    }
}

/// The user's libraries that hold video.
pub(crate) fn video_libraries(items: Vec<MbItem>) -> Vec<MbItem> {
    items
        .into_iter()
        .filter(|i| {
            i.collection_type
                .as_deref()
                .is_none_or(|t| !NOT_VIDEO.contains(&t))
        })
        .map(|mut i| {
            i.kind = MbKind::Library;
            i
        })
        .collect()
}

impl MediaBrowserSource {
    fn user(&self) -> Result<String> {
        Ok(self.client()?.1)
    }

    /// The user's video libraries.
    pub fn libraries(&self) -> Result<Vec<MbItem>> {
        let user = self.user()?;
        let v = self.api_get_json(&format!("/Users/{user}/Views?{IMAGES}"))?;
        Ok(video_libraries(parse_list(&v, &self.base_url)))
    }

    /// One page of a listing.
    pub fn items(&self, query: &MbQuery) -> Result<MbPage> {
        let user = self.user()?;
        let v = self.api_get_json(&query.path(&user))?;
        Ok(parse_page(&v, &self.base_url))
    }

    /// One item with everything a details page shows.
    pub fn item(&self, id: &str) -> Result<MbItem> {
        let user = self.user()?;
        let path = format!(
            "/Users/{user}/Items/{}?Fields={DETAIL_FIELDS}",
            encode_form_value(id)
        );
        let v = self.api_get_json(&path)?;
        let mut item = parse_item(&v, &self.base_url)
            .ok_or_else(|| Error::NotFound(format!("item {id} on {}", self.name)))?;
        item.version = self
            .chosen_version(id)
            .filter(|c| item.versions.iter().any(|v| &v.id == c));
        Ok(item)
    }

    /// The titles matching `text`, by section (`SEARCH_SECTIONS`), best
    /// matches first and at most `limit` a section. Sections without a
    /// match are left out.
    pub fn search(&self, text: &str, limit: usize) -> Result<Vec<(&'static str, Vec<MbItem>)>> {
        let user = self.user()?;
        let mut found = Vec::new();
        for (section, types) in SEARCH_SECTIONS {
            let v = self.api_get_json(&search_path(&user, text, types, limit))?;
            let items = parse_list(&v, &self.base_url);
            if !items.is_empty() {
                found.push((section, items));
            }
        }
        // The server searches titles only: what carries a tag with the
        // words in it is a section of its own.
        let tags = matching_tags(&self.all_tags().unwrap_or_default(), text);
        if !tags.is_empty() {
            let query = MbQuery {
                tags,
                limit,
                ..MbQuery::tagged("")
            };
            let items = self.items(&query)?.items;
            if !items.is_empty() {
                found.push(("Tagged", items));
            }
        }
        Ok(found)
    }

    /// Every tag on the server's videos, read again after a few minutes.
    fn all_tags(&self) -> Result<Vec<String>> {
        if let Some((at, tags)) = lock(&self.tags).as_ref()
            && at.elapsed() < std::time::Duration::from_secs(300)
        {
            return Ok(tags.clone());
        }
        let user = self.user()?;
        let query = format!(
            "UserId={user}&Recursive=true&IncludeItemTypes={}",
            TAGGED.join(",")
        );
        // Jellyfin lists them among its filters; Emby has no such call, but
        // a list of its own.
        let tags = match self.api_get_json(&format!("/Items/Filters?{query}")) {
            Ok(v) => strings(&v, "Tags"),
            Err(Error::NotFound(_)) => {
                let v = self.api_get_json(&format!("/Tags?{query}"))?;
                array(&v, "Items")
                    .iter()
                    .filter_map(|t| text(t, "Name"))
                    .collect()
            }
            Err(e) => return Err(e),
        };
        *lock(&self.tags) = Some((std::time::Instant::now(), tags.clone()));
        Ok(tags)
    }

    /// Partly watched films and episodes, most recent first.
    pub fn resume(&self, limit: usize) -> Result<Vec<MbItem>> {
        let user = self.user()?;
        let v = self.api_get_json(&format!(
            "/Users/{user}/Items/Resume?Limit={limit}&MediaTypes=Video&Fields={LIST_FIELDS}&{IMAGES}"
        ))?;
        Ok(parse_list(&v, &self.base_url))
    }

    /// The next episode to watch of each series (of one series with
    /// `series`).
    pub fn next_up(&self, series: Option<&str>, limit: usize) -> Result<Vec<MbItem>> {
        let user = self.user()?;
        let mut path =
            format!("/Shows/NextUp?UserId={user}&Limit={limit}&Fields={LIST_FIELDS}&{IMAGES}");
        if let Some(s) = series {
            path += &format!("&SeriesId={}", encode_form_value(s));
        }
        let v = self.api_get_json(&path)?;
        Ok(parse_list(&v, &self.base_url))
    }

    /// Newest additions to a library.
    pub fn latest(&self, library: &str, limit: usize) -> Result<Vec<MbItem>> {
        let user = self.user()?;
        let v = self.api_get_json(&format!(
            "/Users/{user}/Items/Latest?ParentId={}&Limit={limit}&Fields={LIST_FIELDS}&{IMAGES}",
            encode_form_value(library)
        ))?;
        Ok(parse_list(&v, &self.base_url))
    }

    /// The seasons of a series.
    pub fn seasons(&self, series: &str) -> Result<Vec<MbItem>> {
        let user = self.user()?;
        let v = self.api_get_json(&format!(
            "/Shows/{}/Seasons?UserId={user}&Fields={LIST_FIELDS}&{IMAGES}",
            encode_form_value(series)
        ))?;
        Ok(parse_list(&v, &self.base_url))
    }

    /// The episodes of a series, of one season with `season`.
    pub fn episodes(&self, series: &str, season: Option<&str>) -> Result<Vec<MbItem>> {
        let user = self.user()?;
        let mut path = format!(
            "/Shows/{}/Episodes?UserId={user}&Fields={LIST_FIELDS}&{IMAGES}",
            encode_form_value(series)
        );
        if let Some(s) = season {
            path += &format!("&SeasonId={}", encode_form_value(s));
        }
        let v = self.api_get_json(&path)?;
        Ok(parse_list(&v, &self.base_url))
    }

    /// The front page: libraries, then rows that are left out when the
    /// server cannot give them.
    pub fn home(&self) -> Result<MbHome> {
        let libraries = self.libraries()?;
        let soft = |r: Result<Vec<MbItem>>, what: &str| {
            r.unwrap_or_else(|e| {
                log::debug!("{what} on {}: {e}", self.name);
                Vec::new()
            })
        };
        let resume = soft(self.resume(16), "resume");
        let next_up = soft(self.next_up(None, 16), "next up");
        let latest = libraries
            .iter()
            .filter(|l| l.collection_type.as_deref() != Some("boxsets"))
            .map(|l| (l.clone(), soft(self.latest(&l.id, 16), "latest")))
            .filter(|(_, items)| !items.is_empty())
            .collect();
        Ok(MbHome {
            libraries,
            resume,
            next_up,
            latest,
        })
    }

    /// Marks or unmarks an item as a favourite on the server.
    pub fn set_favorite(&self, id: &str, favorite: bool) -> Result<()> {
        let user = self.user()?;
        let method = if favorite { "POST" } else { "DELETE" };
        let path = format!("/Users/{user}/FavoriteItems/{}", encode_form_value(id));
        self.api_request(method, &path, None)?;
        Ok(())
    }

    /// Marks an item (a whole series or season too) as watched or not.
    pub fn set_played(&self, id: &str, played: bool) -> Result<()> {
        let user = self.user()?;
        let method = if played { "POST" } else { "DELETE" };
        let path = format!("/Users/{user}/PlayedItems/{}", encode_form_value(id));
        self.api_request(method, &path, None)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fp_core::format::{Projection, StereoLayout};
    use serde_json::json;

    const BASE: &str = "http://jf:8096";

    #[test]
    fn versions_are_told_apart() {
        let v = json!({
            "Id": "m1",
            "Name": "Film",
            "Type": "Movie",
            "MediaSources": [
                {
                    "Id": "a",
                    "Name": "2160p",
                    "Size": 18_200_000_000_u64,
                    "MediaStreams": [
                        {"Type": "Video", "Codec": "hevc", "Height": 2160, "VideoRange": "HDR"}
                    ]
                },
                {
                    "Id": "b",
                    "Name": "Director's Cut",
                    "MediaStreams": [
                        {"Type": "Video", "Codec": "h264", "Height": 1080, "VideoRange": "SDR"}
                    ]
                }
            ]
        });
        let item = parse_item(&v, "http://s").unwrap();
        let labels: Vec<String> = item.versions.iter().map(MbVersion::label).collect();
        assert_eq!(
            labels,
            [
                "2160p · HEVC · HDR · 18.2 GB",
                "Director's Cut · 1080p · H264"
            ]
        );
        assert_eq!(item.versions[1].id, "b");
        assert_eq!(item.version, None);
    }

    #[test]
    fn movies_with_user_data_and_images() {
        let v = json!({
            "Id": "m1", "Name": "Blade Runner", "Type": "Movie",
            "ProductionYear": 1982, "RunTimeTicks": 70_200_000_000_i64,
            "CommunityRating": 7.9, "OfficialRating": "R",
            "Genres": ["Sci-Fi", "Noir"],
            "Overview": "A blade runner must pursue replicants.",
            "Path": "/media/Blade Runner (1982)_180_LR.mkv",
            "DateCreated": "2024-01-15T12:00:00.0000000Z",
            "ImageTags": {"Primary": "p1", "Thumb": "t1"},
            "BackdropImageTags": ["b1"],
            "UserData": {"PlaybackPositionTicks": 35_100_000_000_i64, "Played": false,
                         "IsFavorite": true}
        });
        let m = parse_item(&v, BASE).unwrap();
        assert_eq!(m.kind, MbKind::Movie);
        assert_eq!((m.year, m.runtime), (Some(1982), Some(7020.0)));
        assert_eq!(m.community_rating, Some(7.9));
        assert_eq!(m.genres, ["Sci-Fi", "Noir"]);
        assert!(m.favorite && !m.played);
        assert_eq!(m.position, 3510.0);
        assert_eq!(m.progress(), Some(0.5));
        assert_eq!(
            m.poster.as_deref(),
            Some("http://jf:8096/Items/m1/Images/Primary?maxHeight=480&quality=90&tag=p1")
        );
        assert_eq!(
            m.landscape.as_deref(),
            Some("http://jf:8096/Items/m1/Images/Thumb?maxWidth=640&quality=90&tag=t1")
        );
        assert_eq!(
            m.backdrop.as_deref(),
            Some("http://jf:8096/Items/m1/Images/Backdrop/0?maxWidth=1280&quality=90&tag=b1")
        );
        assert_eq!(m.location, "mb:http://jf:8096#item=m1");
        let f = m.format.unwrap();
        assert_eq!(
            (f.projection, f.stereo),
            (Projection::EQUIRECT_180, StereoLayout::SideBySide)
        );
        assert!(m.added.is_some());
        let e = m.to_entry();
        assert_eq!(e.kind, EntryKind::Video);
        assert_eq!(e.duration, Some(7020.0));
        assert_eq!(e.thumbnail_url, m.landscape);
    }

    #[test]
    fn episodes_fall_back_to_their_series() {
        let still = json!({
            "Id": "e1", "Name": "The Original", "Type": "Episode",
            "SeriesId": "s1", "SeriesName": "Westworld", "SeasonId": "se1",
            "IndexNumber": 1, "ParentIndexNumber": 1,
            "ImageTags": {"Primary": "still"},
            "ParentBackdropItemId": "s1", "ParentBackdropImageTags": ["sb"],
            "UserData": {"Played": true}
        });
        let e = parse_item(&still, BASE).unwrap();
        assert_eq!(e.episode_label().as_deref(), Some("S1:E1"));
        assert_eq!(e.full_title(), "Westworld S1:E1 · The Original");
        assert!(e.played);
        assert_eq!(e.landscape, e.poster);
        assert!(
            e.poster
                .as_ref()
                .unwrap()
                .contains("/Items/e1/Images/Primary")
        );
        assert!(
            e.backdrop
                .as_ref()
                .unwrap()
                .contains("/Items/s1/Images/Backdrop/0")
        );

        let bare = json!({
            "Id": "e2", "Name": "Chestnut", "Type": "Episode", "IndexNumber": 2,
            "SeriesId": "s1", "SeriesPrimaryImageTag": "sp", "ImageTags": {}
        });
        let e = parse_item(&bare, BASE).unwrap();
        assert!(
            e.poster
                .as_ref()
                .unwrap()
                .contains("/Items/s1/Images/Primary")
        );
        assert_eq!(e.episode_label().as_deref(), Some("E2"));
    }

    #[test]
    fn folders_series_and_people() {
        let v = json!({
            "Id": "s1", "Name": "Westworld", "Type": "Series", "ChildCount": 4,
            "UserData": {"UnplayedItemCount": 12},
            "Studios": [{"Name": "HBO"}],
            "People": [
                {"Name": "Jonathan Nolan", "Type": "Director"},
                {"Name": "Evan Rachel Wood", "Role": "Dolores", "Type": "Actor"},
                {"Name": "Ed Harris", "Role": "Man in Black", "Type": "Actor"}
            ]
        });
        let s = parse_item(&v, BASE).unwrap();
        assert_eq!(s.kind, MbKind::Series);
        assert!(!s.kind.playable() && s.kind.portrait());
        assert_eq!(s.location, "mb:http://jf:8096#folder=s1");
        assert_eq!((s.child_count, s.unplayed), (Some(4), Some(12)));
        assert_eq!(s.studios, ["HBO"]);
        let names: Vec<&str> = s.people.iter().map(|p| p.0.as_str()).collect();
        assert_eq!(names, ["Evan Rachel Wood", "Ed Harris", "Jonathan Nolan"]);
        assert_eq!(s.people[0].1, "Dolores");
        assert_eq!(s.people[2].1, "Director");
        assert!(s.format.is_none());
        assert_eq!(s.to_entry().kind, EntryKind::Directory);
        // No image tags at all: the primary image is still offered.
        let plain = parse_item(&json!({"Id": "x", "Type": "Video"}), BASE).unwrap();
        assert!(plain.poster.is_some() && plain.landscape.is_some());
    }

    #[test]
    fn libraries_without_video_are_left_out() {
        let v = json!({"Items": [
            {"Id": "a", "Name": "Movies", "Type": "CollectionFolder", "CollectionType": "movies"},
            {"Id": "b", "Name": "Music", "Type": "CollectionFolder", "CollectionType": "music"},
            {"Id": "c", "Name": "VR", "Type": "CollectionFolder"},
            {"Id": "d", "Name": "Shows", "Type": "UserView", "CollectionType": "tvshows"}
        ]});
        let libs = video_libraries(parse_list(&v, BASE));
        let names: Vec<&str> = libs.iter().map(|l| l.name.as_str()).collect();
        assert_eq!(names, ["Movies", "VR", "Shows"]);
        assert!(libs.iter().all(|l| l.kind == MbKind::Library));
        assert_eq!(MbQuery::children(&libs[0]).types, ["Movie"]);
        assert_eq!(MbQuery::children(&libs[2]).types, ["Series"]);
        assert_eq!(MbQuery::children(&libs[1]).types.len(), 4);
        assert!(MbQuery::children(&libs[0]).recursive);
    }

    #[test]
    fn listing_paths() {
        let q = MbQuery {
            parent: Some("lib 1".into()),
            types: vec!["Movie".into()],
            recursive: true,
            search: " blade runner ".into(),
            sort: MbSort::Added,
            descending: true,
            filter: MbFilter::Unplayed,
            start: 60,
            limit: 60,
            tags: Vec::new(),
        };
        let p = q.path("u1");
        assert!(p.starts_with("/Users/u1/Items?Recursive=true&SortBy=DateCreated,SortName"));
        for part in [
            "SortOrder=Descending",
            "StartIndex=60",
            "Limit=60",
            "ParentId=lib%201",
            "IncludeItemTypes=Movie",
            "Filters=IsUnplayed",
            "SearchTerm=blade%20runner",
            "EnableTotalRecordCount=true",
        ] {
            assert!(p.contains(part), "{part} missing from {p}");
        }
        let all = MbQuery::default().path("u1");
        assert!(!all.contains("ParentId") && !all.contains("Filters") && !all.contains("Search"));
    }

    #[test]
    fn tags_are_searched_and_listed() {
        let tags: Vec<String> = ["Beach Day", "beach", "Sunset", "Night Beach Party"]
            .iter()
            .map(ToString::to_string)
            .collect();
        assert_eq!(
            matching_tags(&tags, "BEACH"),
            ["Beach Day", "beach", "Night Beach Party"]
        );
        assert_eq!(matching_tags(&tags, "beach party"), ["Night Beach Party"]);
        assert!(matching_tags(&tags, "  ").is_empty());
        let p = MbQuery::tagged("Beach Day").path("u1");
        assert!(p.contains("&Tags=Beach%20Day"), "{p}");
        assert!(p.contains("Recursive=true"), "{p}");
        assert!(
            p.contains("IncludeItemTypes=Movie,Series,Episode,Video,MusicVideo"),
            "{p}"
        );
        let two = MbQuery {
            tags: vec!["a".into(), "b".into()],
            ..MbQuery::default()
        };
        assert!(two.path("u1").contains("&Tags=a%7Cb"));
    }

    #[test]
    fn search_looks_for_titles_ranked_by_the_server() {
        let p = search_path("u1", " blade runner ", &["Video", "MusicVideo"], 50);
        assert!(
            p.starts_with("/Users/u1/Items?SearchTerm=blade%20runner&"),
            "{p}"
        );
        for part in [
            "IncludeItemTypes=Video,MusicVideo",
            "Recursive=true",
            "Limit=50",
        ] {
            assert!(p.contains(part), "{part} missing from {p}");
        }
        assert!(!p.contains("SortBy"), "the server ranks the matches: {p}");
        let types: Vec<&str> = SEARCH_SECTIONS
            .iter()
            .flat_map(|(_, t)| t.iter().copied())
            .collect();
        assert!(types.contains(&"Movie") && types.contains(&"Series"));
        assert!(!types.contains(&"Episode"));
    }

    #[test]
    fn pages_and_bare_arrays() {
        let page = parse_page(
            &json!({"Items": [{"Id": "1", "Type": "Movie"}], "TotalRecordCount": 250}),
            BASE,
        );
        assert_eq!((page.items.len(), page.total), (1, 250));
        let latest = parse_list(
            &json!([{"Id": "1", "Type": "Movie"}, {"Name": "no id"}]),
            BASE,
        );
        assert_eq!(latest.len(), 1);
    }
}
