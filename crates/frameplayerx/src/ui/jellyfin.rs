//! A Jellyfin/Emby client: the server's libraries and rows of what to watch
//! next, poster grids the server searches, sorts and filters, series with
//! their seasons and episodes, and details pages. Watched marks, resume
//! points and favourites are the server's own.

use super::page::{self, Card};
use super::theme::{self, Weight};
use super::widgets::{self, Kind, Tip};
use super::{Action, View, fmt_time, icons};
use crate::playback::OpenRequest;
use egui::{Align2, Color32, RichText, Sense, Vec2};
use fp_sources::{MbFilter, MbHome, MbItem, MbKind, MbPage, MbQuery, MbSort, MediaBrowserSource};
use std::time::{Duration, Instant};

/// Typing rests this long before a search goes to the server.
const SEARCH_DELAY: Duration = Duration::from_millis(600);
/// Items per page of a grid.
const PAGE: usize = 60;
/// Matches shown per section of a search.
const SEARCH_LIMIT: usize = 50;
/// Height of the banners of series and details pages.
const BANNER: f32 = 320.0;

/// The pages open on a server, the one shown last.
pub struct JellyfinView {
    pub pages: Vec<Page>,
}

pub struct Page {
    /// Answers to this page's requests carry this number.
    pub seq: u64,
    pub loading: bool,
    pub error: Option<String>,
    pub body: Body,
}

pub enum Body {
    /// Libraries and rows of what to watch.
    Home {
        home: Option<MbHome>,
        search: String,
        /// When the search text last changed.
        typed: Option<Instant>,
    },
    /// What a search of the server found, a row per kind (films, series,
    /// collections, other videos), as Jellyfin shows it.
    Search {
        /// The search field, in the header.
        search: String,
        typed: Option<Instant>,
        /// What the results are for.
        asked: String,
        results: Vec<(&'static str, Vec<MbItem>)>,
    },
    /// A library, collection or folder, a page at a time.
    Grid {
        title: String,
        query: MbQuery,
        items: Vec<MbItem>,
        total: usize,
        typed: Option<Instant>,
    },
    Series {
        series: Box<MbItem>,
        seasons: Vec<MbItem>,
        season: Option<String>,
        episodes: Vec<MbItem>,
        next_up: Option<Box<MbItem>>,
    },
    /// A film, episode or video.
    Item { item: Box<MbItem> },
}

/// What to ask a server for.
#[derive(Clone, Debug)]
pub enum Load {
    Home,
    /// The titles matching the text.
    Search(String),
    /// A page of a listing; past the first it adds to what is shown.
    Grid(MbQuery),
    /// A series with its seasons and one season's episodes (`season`, else
    /// the one to watch next).
    Series {
        id: String,
        season: Option<String>,
    },
    Episodes {
        series: String,
        season: String,
    },
    Item {
        id: String,
    },
}

/// A mark the server keeps per item.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mark {
    Favorite,
    Played,
}

/// A server's answer to a [`Load`].
pub enum Loaded {
    Home(MbHome),
    Search {
        text: String,
        results: Vec<(&'static str, Vec<MbItem>)>,
    },
    Grid {
        page: MbPage,
        append: bool,
    },
    Series {
        series: Box<MbItem>,
        seasons: Vec<MbItem>,
        season: Option<String>,
        episodes: Vec<MbItem>,
        next_up: Option<Box<MbItem>>,
    },
    Episodes(Vec<MbItem>),
    Item(Box<MbItem>),
}

/// Asks the server (blocking: run it on a job thread).
pub fn run(src: &MediaBrowserSource, load: &Load) -> fp_sources::Result<Loaded> {
    Ok(match load {
        Load::Home => Loaded::Home(src.home()?),
        Load::Search(text) => Loaded::Search {
            text: text.clone(),
            results: src.search(text, SEARCH_LIMIT)?,
        },
        Load::Grid(q) => Loaded::Grid {
            page: src.items(q)?,
            append: q.start > 0,
        },
        Load::Series { id, season } => {
            let series = Box::new(src.item(id)?);
            let seasons = src.seasons(id)?;
            let next_up = src
                .next_up(Some(id), 1)
                .ok()
                .and_then(|n| n.into_iter().next())
                .map(Box::new);
            let season = season
                .clone()
                .or_else(|| next_up.as_ref().and_then(|e| e.season_id.clone()))
                .or_else(|| seasons.first().map(|s| s.id.clone()));
            let episodes = src.episodes(id, season.as_deref())?;
            Loaded::Series {
                series,
                seasons,
                season,
                episodes,
                next_up,
            }
        }
        Load::Episodes { series, season } => Loaded::Episodes(src.episodes(series, Some(season))?),
        Load::Item { id } => Loaded::Item(Box::new(src.item(id)?)),
    })
}

impl JellyfinView {
    /// The server's front page, loading under `seq`.
    pub fn new(seq: u64) -> JellyfinView {
        JellyfinView {
            pages: vec![Page {
                seq,
                loading: true,
                error: None,
                body: Body::Home {
                    home: None,
                    search: String::new(),
                    typed: None,
                },
            }],
        }
    }

    /// Gives an answer to the page that asked.
    pub fn loaded(&mut self, seq: u64, result: Result<Loaded, String>) {
        let Some(p) = self.pages.iter_mut().find(|p| p.seq == seq) else {
            return;
        };
        p.loading = false;
        let loaded = match result {
            Ok(l) => l,
            Err(e) => {
                p.error = Some(e);
                return;
            }
        };
        p.error = None;
        match (&mut p.body, loaded) {
            (Body::Home { home, .. }, Loaded::Home(h)) => *home = Some(h),
            (Body::Search { asked, results, .. }, Loaded::Search { text, results: r }) => {
                *asked = text;
                *results = r;
            }
            (Body::Grid { items, total, .. }, Loaded::Grid { page, append }) => {
                if append {
                    items.extend(page.items);
                } else {
                    *items = page.items;
                }
                *total = page.total.max(items.len());
            }
            (
                Body::Series {
                    series,
                    seasons,
                    season,
                    episodes,
                    next_up,
                },
                Loaded::Series {
                    series: s,
                    seasons: ss,
                    season: chosen,
                    episodes: es,
                    next_up: n,
                },
            ) => {
                *series = s;
                *seasons = ss;
                *season = chosen;
                *episodes = es;
                *next_up = n;
            }
            (Body::Series { episodes, .. }, Loaded::Episodes(es)) => *episodes = es,
            (Body::Item { item }, Loaded::Item(i)) => *item = i,
            _ => {}
        }
    }

    /// What brings the page shown up to date (after watching something).
    /// Opens a page, loading until answer `seq` arrives.
    fn open(&mut self, seq: u64, body: Body) {
        self.pages.push(Page {
            seq,
            loading: true,
            error: None,
            body,
        });
    }

    pub fn reload(&mut self) -> Option<(u64, Load)> {
        let p = self.pages.last_mut()?;
        p.loading = true;
        let load = match &p.body {
            Body::Home { .. } => Load::Home,
            Body::Search { asked, .. } => Load::Search(asked.clone()),
            Body::Grid { query, items, .. } => Load::Grid(MbQuery {
                start: 0,
                limit: items.len().max(PAGE),
                ..query.clone()
            }),
            Body::Series { series, season, .. } => Load::Series {
                id: series.id.clone(),
                season: season.clone(),
            },
            Body::Item { item } => Load::Item {
                id: item.id.clone(),
            },
        };
        Some((p.seq, load))
    }

    /// Shows a mark at once wherever the item appears; the server is told
    /// separately.
    fn mark(&mut self, id: &str, mark: Mark, on: bool) {
        let set = |i: &mut MbItem| {
            if i.id != id {
                return;
            }
            match mark {
                Mark::Favorite => i.favorite = on,
                Mark::Played => {
                    i.played = on;
                    i.position = 0.0;
                }
            }
        };
        for p in &mut self.pages {
            match &mut p.body {
                Body::Home { home: Some(h), .. } => {
                    for i in h
                        .resume
                        .iter_mut()
                        .chain(h.next_up.iter_mut())
                        .chain(h.latest.iter_mut().flat_map(|(_, l)| l.iter_mut()))
                    {
                        set(i);
                    }
                }
                Body::Search { results, .. } => results
                    .iter_mut()
                    .flat_map(|(_, items)| items.iter_mut())
                    .for_each(set),
                Body::Grid { items, .. } => items.iter_mut().for_each(set),
                Body::Series {
                    series,
                    episodes,
                    next_up,
                    ..
                } => {
                    set(series);
                    episodes.iter_mut().for_each(set);
                    if let Some(n) = next_up {
                        set(n);
                    }
                }
                Body::Item { item } => set(item),
                _ => {}
            }
        }
    }
}

/// What the page asked for while it was drawn.
enum Go {
    /// Into a library, collection, folder, series or season.
    Open(MbItem),
    /// Item `.1` of a list, the rest queued; `.2` from the start.
    Play(Vec<MbItem>, usize, bool),
    /// Play next (`true`) or Add to queue.
    Queue(MbItem, bool),
    AddToPlaylist(MbItem),
    /// Everything on the server with this tag.
    Tag(String),
    Details(MbItem),
    /// Load again what the page shows (`Load` with the page's number).
    Reload(Load),
    Mark(String, Mark, bool),
    /// Play the item at this location from this version (`None`: the
    /// tallest that fits).
    Version(String, Option<String>),
    Search(String),
}

/// A server's page; true when Back was clicked.
pub fn page(
    ui: &mut egui::Ui,
    v: &mut View,
    source: &str,
    name: &str,
    j: &mut JellyfinView,
) -> bool {
    let subtitle = trail(&j.pages);
    let Some(p) = j.pages.last_mut() else {
        return true;
    };
    let mut refresh = false;
    let back = page::header(ui, name, Some(&subtitle), |ui| {
        refresh = widgets::icon_button(ui, icons::ARROWS_CLOCKWISE, 44.0, false)
            .tip("Refresh")
            .clicked();
        if let Body::Home { search, typed, .. } | Body::Search { search, typed, .. } = &mut p.body
            && page::search_field(ui, search, "Search this server", 220.0)
        {
            *typed = Some(Instant::now());
        }
    });
    if refresh {
        if let Some((seq, load)) = j.reload() {
            v.actions.push(Action::Jellyfin {
                source: source.to_string(),
                seq,
                load,
            });
        }
        return back;
    }
    let Some(p) = j.pages.last_mut() else {
        return true;
    };
    page::status(ui, p.loading && !has_content(&p.body), p.error.as_deref());
    let loading = p.loading;
    let mut go: Option<Go> = None;
    page::scroll(ui, |ui| {
        go = match &mut p.body {
            Body::Home {
                home,
                search,
                typed,
            } => {
                if let Some(t) = *typed
                    && t.elapsed() >= SEARCH_DELAY
                {
                    *typed = None;
                    (!search.trim().is_empty()).then(|| Go::Search(std::mem::take(search)))
                } else {
                    if typed.is_some() {
                        ui.ctx().request_repaint_after(SEARCH_DELAY);
                    }
                    home.as_ref().and_then(|h| home_rows(ui, v, h))
                }
            }
            Body::Search {
                search,
                typed,
                asked,
                results,
            } => search_results(ui, v, search, typed, asked, results, loading),
            Body::Grid {
                query,
                items,
                total,
                typed,
                ..
            } => grid(ui, v, query, items, *total, typed),
            Body::Series {
                series,
                seasons,
                season,
                episodes,
                next_up,
            } => series_page(ui, v, series, seasons, season, episodes, next_up.as_deref()),
            Body::Item { item } => details(ui, v, item),
        };
    });
    if let Some(go) = go {
        follow(v, source, j, go);
    }
    back
}

/// Carries out what the page asked for.
fn follow(v: &mut View, source: &str, j: &mut JellyfinView, go: Go) {
    let ask = |v: &mut View, seq: u64, load: Load| {
        v.actions.push(Action::Jellyfin {
            source: source.to_string(),
            seq,
            load,
        });
    };
    match go {
        Go::Reload(load) => {
            if let Some(p) = j.pages.last_mut() {
                p.loading = true;
                ask(v, p.seq, load);
            }
        }
        Go::Play(list, i, from_start) => {
            let reqs: Vec<OpenRequest> = list
                .iter()
                .enumerate()
                .map(|(k, item)| request(source, item, from_start && k == i))
                .collect();
            v.actions.push(Action::OpenList(reqs, i));
        }
        Go::Queue(item, next) => v.actions.push(Action::Queue {
            req: request(source, &item, false),
            next,
        }),
        Go::AddToPlaylist(item) => {
            v.state.picking = Some(super::Picking {
                reqs: vec![request(source, &item, false)],
                title: item.name.clone(),
            });
        }
        Go::Mark(id, mark, on) => {
            j.mark(&id, mark, on);
            v.actions.push(Action::JellyfinMark {
                source: source.to_string(),
                id,
                mark,
                on,
            });
        }
        Go::Version(location, version) => v.actions.push(Action::JellyfinVersion {
            source: source.to_string(),
            location,
            version,
        }),
        Go::Details(item) => {
            let seq = v.state.next_seq();
            ask(
                v,
                seq,
                Load::Item {
                    id: item.id.clone(),
                },
            );
            j.open(
                seq,
                Body::Item {
                    item: Box::new(item),
                },
            );
        }
        Go::Search(text) => {
            let seq = v.state.next_seq();
            let text = text.trim().to_string();
            ask(v, seq, Load::Search(text.clone()));
            j.open(
                seq,
                Body::Search {
                    search: text.clone(),
                    typed: None,
                    asked: text,
                    results: Vec::new(),
                },
            );
        }
        Go::Open(item) => {
            let seq = v.state.next_seq();
            let (load, body) = page_for(item);
            ask(v, seq, load);
            j.open(seq, body);
        }
        Go::Tag(tag) => {
            let seq = v.state.next_seq();
            let query = MbQuery {
                limit: PAGE,
                ..MbQuery::tagged(&tag)
            };
            let body = Body::Grid {
                title: format!("Tagged {tag}"),
                query: query.clone(),
                items: Vec::new(),
                total: 0,
                typed: None,
            };
            ask(v, seq, Load::Grid(query));
            j.open(seq, body);
        }
    }
}

/// What opening a library, collection, folder, series or season loads,
/// and the page that shows it.
fn page_for(item: MbItem) -> (Load, Body) {
    match item.kind {
        MbKind::Series | MbKind::Season => {
            let (id, season) = if item.kind == MbKind::Season {
                (
                    item.series_id.clone().unwrap_or_default(),
                    Some(item.id.clone()),
                )
            } else {
                (item.id.clone(), None)
            };
            let load = Load::Series {
                id: id.clone(),
                season: season.clone(),
            };
            let series = MbItem {
                id,
                name: item.series_name.clone().unwrap_or(item.name.clone()),
                kind: MbKind::Series,
                ..item
            };
            let body = Body::Series {
                series: Box::new(series),
                seasons: Vec::new(),
                season,
                episodes: Vec::new(),
                next_up: None,
            };
            (load, body)
        }
        _ => {
            let query = MbQuery {
                limit: PAGE,
                ..MbQuery::children(&item)
            };
            let body = Body::Grid {
                title: item.name.clone(),
                query: query.clone(),
                items: Vec::new(),
                total: 0,
                typed: None,
            };
            (Load::Grid(query), body)
        }
    }
}

/// Where the page shown is on the server, as the header's second line: the
/// pages opened to reach it ("Movies › Blade Runner", the last three), or
/// what the server is on its front page.
fn trail(pages: &[Page]) -> String {
    let parts: Vec<String> = pages
        .iter()
        .filter_map(|p| match &p.body {
            Body::Home { .. } => None,
            Body::Search { asked, .. } => Some(format!("Search \"{asked}\"")),
            Body::Grid { title, .. } => Some(title.clone()),
            Body::Series { series, .. } => Some(series.name.clone()),
            Body::Item { item } => Some(item.name.clone()),
        })
        .collect();
    if parts.is_empty() {
        return "Jellyfin / Emby".into();
    }
    parts[parts.len().saturating_sub(3)..].join(&format!("  {}  ", icons::CARET_RIGHT))
}

/// The page shows something already (a reload keeps it on screen).
fn has_content(body: &Body) -> bool {
    match body {
        Body::Home { home, .. } => home.is_some(),
        Body::Search { results, .. } => !results.is_empty(),
        Body::Grid { items, .. } => !items.is_empty(),
        Body::Series { episodes, .. } => !episodes.is_empty(),
        Body::Item { .. } => true,
    }
}

/// Opens `item`, starting from the server's resume point.
fn request(source: &str, item: &MbItem, from_start: bool) -> OpenRequest {
    OpenRequest {
        location: item.location.clone(),
        source_id: Some(source.to_string()),
        entry: Some(item.to_entry()),
        title: None,
        start_at: if from_start {
            Some(0.0)
        } else {
            (item.position > 1.0).then_some(item.position)
        },
    }
}

/// "1982 · 1:57:00", "S1:E3 · Westworld", "4 seasons".
fn card_facts(item: &MbItem) -> String {
    let mut f = Vec::new();
    match item.kind {
        MbKind::Episode => {
            if let Some(e) = item.episode_label() {
                f.push(e);
            }
            if let Some(s) = &item.series_name {
                f.push(s.clone());
            }
        }
        MbKind::Series => {
            if let Some(y) = item.year {
                f.push(y.to_string());
            }
            match item.unplayed {
                Some(n) if n > 0 => f.push(format!("{n} unwatched")),
                _ => {}
            }
        }
        MbKind::BoxSet | MbKind::Folder | MbKind::Library => {
            if let Some(n) = item.child_count {
                f.push(page::plural(n as usize, "item", "items"));
            }
        }
        _ => {
            if let Some(y) = item.year {
                f.push(y.to_string());
            }
        }
    }
    if item.kind.playable()
        && let Some(d) = item.runtime
    {
        f.push(fmt_time(d));
    }
    f.join("  ·  ")
}

/// A VR format worth a badge (flat films get none).
fn vr_badge(item: &MbItem) -> Option<String> {
    format_badge(item.format)
}

/// `format` when it is VR or 3D.
fn format_badge(format: Option<fp_core::format::VideoFormat>) -> Option<String> {
    let f = format?;
    let flat = f.projection == fp_core::format::Projection::Flat
        && f.stereo == fp_core::format::StereoLayout::Mono;
    (!flat).then(|| widgets::format_short(&f))
}

/// An item's card, a poster or 16:9 art.
fn item_card(ui: &mut egui::Ui, v: &mut View, item: &MbItem, portrait: bool) -> page::CardResponse {
    let thumb = if portrait {
        item.poster.clone()
    } else {
        item.landscape.clone().or_else(|| item.poster.clone())
    };
    let facts = card_facts(item);
    let badge = vr_badge(item);
    let placeholder = match item.kind {
        MbKind::Series | MbKind::Season => icons::MONITOR_PLAY,
        MbKind::Library | MbKind::Folder | MbKind::BoxSet => icons::FOLDERS,
        _ => icons::FILM_STRIP,
    };
    page::card(
        ui,
        v.thumbs,
        &Card {
            size: if portrait {
                page::PORTRAIT
            } else {
                page::LANDSCAPE
            },
            thumb: thumb.as_deref(),
            placeholder,
            title: &item.name,
            facts: &facts,
            badge: badge.as_deref(),
            progress: item.progress(),
            watched: item.played,
            favorite: item.favorite,
            more: item.kind.playable(),
            play: item.kind.playable(),
            ..Card::default()
        },
    )
}

/// What clicking a card in `list` does.
fn card_go(c: page::CardResponse, list: &[MbItem], i: usize) -> Option<Go> {
    let item = &list[i];
    if c.more {
        Some(Go::Details(item.clone()))
    } else if c.open && item.kind.playable() {
        Some(Go::Play(list.to_vec(), i, false))
    } else if c.open {
        Some(Go::Open(item.clone()))
    } else {
        None
    }
}

/// A titled row of cards.
fn item_row(
    ui: &mut egui::Ui,
    v: &mut View,
    title: &str,
    items: &[MbItem],
    landscape: bool,
) -> Option<Go> {
    let mut go = None;
    page::carousel(ui, title, None, items.len(), |ui, i| {
        let portrait = !landscape && items[i].kind.portrait();
        let c = item_card(ui, v, &items[i], portrait);
        go = go.take().or_else(|| card_go(c, items, i));
    });
    go
}

fn home_rows(ui: &mut egui::Ui, v: &mut View, h: &MbHome) -> Option<Go> {
    ui.add_space(6.0);
    let mut go = item_row(ui, v, "My media", &h.libraries, true);
    go = go.or(item_row(ui, v, "Continue watching", &h.resume, true));
    go = go.or(item_row(ui, v, "Next up", &h.next_up, true));
    for (library, items) in &h.latest {
        let title = format!("Latest in {}", library.name);
        go = go.or(item_row(ui, v, &title, items, false));
    }
    if h.libraries.is_empty() {
        page::empty_state(
            ui,
            icons::MONITOR_PLAY,
            "No video libraries",
            "This account sees no film, TV or video libraries on the server.",
            |_| {},
        );
    }
    go
}

/// Search results, a row per kind. Typing in the header's search field
/// searches again on this page.
fn search_results(
    ui: &mut egui::Ui,
    v: &mut View,
    search: &str,
    typed: &mut Option<Instant>,
    asked: &str,
    results: &[(&'static str, Vec<MbItem>)],
    loading: bool,
) -> Option<Go> {
    if let Some(t) = *typed {
        if t.elapsed() >= SEARCH_DELAY {
            *typed = None;
            let text = search.trim();
            if !text.is_empty() && text != asked {
                return Some(Go::Reload(Load::Search(text.to_string())));
            }
        } else {
            ui.ctx().request_repaint_after(SEARCH_DELAY);
        }
    }
    ui.add_space(6.0);
    let mut go = None;
    for (section, items) in results {
        go = go.or(item_row(ui, v, section, items, false));
    }
    if results.is_empty() && !loading {
        page::empty_state(
            ui,
            icons::MAGNIFYING_GLASS,
            "Nothing found",
            &format!("No film, series, collection or video on this server is called \"{asked}\"."),
            |_| {},
        );
    }
    go
}

/// A searchable, sortable, filterable listing.
fn grid(
    ui: &mut egui::Ui,
    v: &mut View,
    query: &mut MbQuery,
    items: &[MbItem],
    total: usize,
    typed: &mut Option<Instant>,
) -> Option<Go> {
    let mut changed = false;
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 8.0;
        if page::search_field(ui, &mut query.search, "Search", 220.0) {
            *typed = Some(Instant::now());
        }
        ui.add_space(10.0);
        for (f, label) in [
            (MbFilter::All, "All"),
            (MbFilter::Unplayed, "Unwatched"),
            (MbFilter::Resumable, "In progress"),
            (MbFilter::Favorites, "Favorites"),
        ] {
            if widgets::chip(ui, label, query.filter == f).clicked() && query.filter != f {
                query.filter = f;
                changed = true;
            }
        }
    });
    ui.add_space(6.0);
    ui.horizontal(|ui| {
        if page::sort_bar(
            ui,
            &[
                (MbSort::Name, "Title"),
                (MbSort::Added, "Date added"),
                (MbSort::Released, "Release date"),
                (MbSort::Rating, "Rating"),
                (MbSort::Runtime, "Length"),
                (MbSort::Random, "Shuffle"),
            ],
            &mut query.sort,
        ) {
            query.descending = query.sort.default_descending();
            changed = true;
        }
        if query.sort != MbSort::Random {
            let (icon, tip) = if query.descending {
                (icons::CARET_DOWN, "Largest first")
            } else {
                (icons::CARET_UP, "Smallest first")
            };
            if widgets::icon_button(ui, icon, 38.0, false)
                .tip(tip)
                .clicked()
            {
                query.descending = !query.descending;
                changed = true;
            }
        }
        page::count_label(ui, &page::plural(total, "item", "items"));
    });
    ui.add_space(10.0);
    if let Some(t) = *typed {
        if t.elapsed() >= SEARCH_DELAY {
            *typed = None;
            changed = true;
        } else {
            ui.ctx().request_repaint_after(SEARCH_DELAY);
        }
    }
    if changed {
        query.start = 0;
        query.limit = PAGE;
        return Some(Go::Reload(Load::Grid(query.clone())));
    }
    if items.is_empty() {
        return None;
    }
    let portrait = items.iter().filter(|i| i.kind.portrait()).count() * 2 >= items.len();
    let width = if portrait {
        page::PORTRAIT.x
    } else {
        page::LANDSCAPE.x
    };
    let mut go = None;
    page::grid(ui, items.len(), width, |ui, i| {
        let c = item_card(ui, v, &items[i], portrait);
        go = go.take().or_else(|| card_go(c, items, i));
    });
    if items.len() < total {
        ui.vertical_centered(|ui| {
            if widgets::button(ui, None, "Show more", Kind::Secondary).clicked() {
                go = Some(Go::Reload(Load::Grid(MbQuery {
                    start: items.len(),
                    limit: PAGE,
                    ..query.clone()
                })));
            }
        });
        ui.add_space(20.0);
    }
    go
}

/// Year, rating and other facts about an item.
fn item_facts(item: &MbItem) -> Vec<String> {
    let mut f = Vec::new();
    if let Some(y) = item.year {
        f.push(y.to_string());
    }
    if item.kind.playable()
        && let Some(d) = item.runtime
    {
        f.push(fmt_time(d));
    }
    if let Some(r) = &item.official_rating {
        f.push(r.clone());
    }
    if let Some(r) = item.community_rating {
        f.push(page::rating(r));
    }
    if let Some(b) = format_badge(item.shown_format()) {
        f.push(b);
    }
    if item.played {
        f.push("Watched".into());
    }
    f
}

/// The favourite and watched toggles.
fn mark_chips(ui: &mut egui::Ui, item: &MbItem) -> Option<Go> {
    let mut go = None;
    let heart = if item.favorite {
        "Favorite"
    } else {
        "Add to favorites"
    };
    if widgets::chip_icon(ui, Some(icons::HEART), heart, item.favorite).clicked() {
        go = Some(Go::Mark(item.id.clone(), Mark::Favorite, !item.favorite));
    }
    if widgets::chip_icon(ui, Some(icons::CHECK), "Watched", item.played).clicked() {
        go = Some(Go::Mark(item.id.clone(), Mark::Played, !item.played));
    }
    go
}

/// The start of a long text, cut at a word.
fn clip(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    let cut: String = text.chars().take(max).collect();
    let cut = cut.rsplit_once(' ').map(|(a, _)| a).unwrap_or(&cut);
    format!("{}…", cut.trim_end_matches([',', '.', ';']))
}

#[allow(clippy::too_many_arguments)]
fn series_page(
    ui: &mut egui::Ui,
    v: &mut View,
    series: &MbItem,
    seasons: &[MbItem],
    season: &mut Option<String>,
    episodes: &[MbItem],
    next_up: Option<&MbItem>,
) -> Option<Go> {
    let mut go = None;
    ui.add_space(4.0);
    let key = series
        .backdrop
        .clone()
        .or_else(|| series.landscape.clone())
        .or_else(|| series.poster.clone());
    page::banner(ui, v.thumbs, key.as_deref(), BANNER, |inner| {
        inner.add_space(14.0);
        inner.add(
            egui::Label::new(
                RichText::new(&series.name)
                    .font(theme::font(Weight::Bold, 40.0))
                    .color(Color32::WHITE),
            )
            .truncate(),
        );
        let mut f = item_facts(series);
        if !seasons.is_empty() {
            f.push(page::plural(seasons.len(), "season", "seasons"));
        }
        page::facts(inner, &f);
        if let Some(o) = &series.overview {
            inner.add_space(4.0);
            page::prose(inner, &clip(o, 220));
        }
        inner.add_space(14.0);
        inner.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 12.0;
            if let Some(n) = next_up {
                let ep = n.episode_label().unwrap_or_default();
                let label = if n.position > 1.0 {
                    format!("Resume {ep}")
                } else {
                    format!("Play {ep}")
                };
                if widgets::button(ui, Some(icons::PLAY), &label, Kind::Primary).clicked() {
                    // The next episode, with the rest of its season after it
                    // when it is the season shown.
                    go = Some(match episodes.iter().position(|e| e.id == n.id) {
                        Some(i) => Go::Play(episodes.to_vec(), i, false),
                        None => Go::Play(vec![n.clone()], 0, false),
                    });
                }
            }
            go = go.take().or(mark_chips(ui, series));
        });
    });
    ui.add_space(18.0);
    if seasons.len() > 1 {
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing = Vec2::new(8.0, 8.0);
            for s in seasons {
                let selected = season.as_deref() == Some(s.id.as_str());
                if widgets::chip(ui, &s.name, selected).clicked() && !selected {
                    *season = Some(s.id.clone());
                    go = Some(Go::Reload(Load::Episodes {
                        series: series.id.clone(),
                        season: s.id.clone(),
                    }));
                }
            }
        });
        ui.add_space(12.0);
    }
    for (i, e) in episodes.iter().enumerate() {
        let (play, more) = episode_row(ui, v, e);
        if play {
            go = Some(Go::Play(episodes.to_vec(), i, false));
        }
        if more {
            go = Some(Go::Details(e.clone()));
        }
    }
    ui.add_space(20.0);
    go
}

/// One episode: still, number and title, facts, the start of its story;
/// returns (play clicked, details clicked).
fn episode_row(ui: &mut egui::Ui, v: &mut View, e: &MbItem) -> (bool, bool) {
    let (rect, resp) =
        ui.allocate_exact_size(Vec2::new(ui.available_width(), 132.0), Sense::click());
    if !ui.is_rect_visible(rect) {
        return (false, false);
    }
    let t = ui
        .ctx()
        .animate_bool_with_time(resp.id, resp.hovered(), 0.12);
    ui.painter().rect_filled(
        rect,
        egui::CornerRadius::same(14),
        theme::SURFACE.gamma_multiply(t),
    );
    let thumb =
        egui::Rect::from_min_size(rect.min + Vec2::new(10.0, 12.0), Vec2::new(192.0, 108.0));
    page::picture(
        ui,
        v.thumbs,
        e.landscape.as_deref().or(e.poster.as_deref()),
        thumb,
        10,
        icons::FILM_STRIP,
    );
    let painter = ui.painter();
    if let Some(p) = e.progress().filter(|p| *p > 0.005) {
        let bar = egui::Rect::from_min_max(
            egui::pos2(thumb.left() + 8.0, thumb.bottom() - 8.0),
            egui::pos2(thumb.right() - 8.0, thumb.bottom() - 4.0),
        );
        painter.rect_filled(
            bar,
            egui::CornerRadius::same(2),
            Color32::from_black_alpha(140),
        );
        let mut done = bar;
        done.set_width(bar.width() * p);
        painter.rect_filled(done, egui::CornerRadius::same(2), theme::ACCENT);
    }
    if t > 0.01 {
        painter.circle_filled(
            thumb.center(),
            24.0,
            Color32::from_black_alpha((t * 150.0) as u8),
        );
        widgets::play_mark(
            painter,
            thumb.center(),
            20.0,
            Color32::from_white_alpha((t * 255.0) as u8),
        );
    }
    let left = thumb.right() + 18.0;
    let width = rect.right() - left - 70.0;
    let text = |s: String, font: egui::FontId, color: Color32, rows: usize| {
        let mut job =
            egui::text::LayoutJob::single_section(s, egui::TextFormat::simple(font, color));
        job.wrap = egui::text::TextWrapping {
            max_width: width,
            max_rows: rows,
            break_anywhere: false,
            overflow_character: Some('…'),
        };
        ui.fonts(|f| f.layout_job(job))
    };
    let title = match e.index {
        Some(n) => format!("{n}. {}", e.name),
        None => e.name.clone(),
    };
    let mut facts = Vec::new();
    if let Some(d) = e.runtime {
        facts.push(fmt_time(d));
    }
    if e.position > 1.0
        && let Some(d) = e.runtime
    {
        facts.push(format!("{} left", fmt_time(d - e.position)));
    }
    if let Some(b) = vr_badge(e) {
        facts.push(b);
    }
    let title = text(title, theme::font(Weight::SemiBold, 18.0), theme::TEXT, 1);
    let facts = text(
        facts.join("  ·  "),
        theme::font(Weight::Regular, 15.0),
        theme::TEXT_2,
        1,
    );
    let story = text(
        e.overview.clone().unwrap_or_default(),
        theme::font(Weight::Regular, 15.0),
        theme::TEXT_3,
        2,
    );
    let painter = ui.painter();
    painter.galley(egui::pos2(left, rect.top() + 14.0), title, theme::TEXT);
    painter.galley(egui::pos2(left, rect.top() + 40.0), facts, theme::TEXT_2);
    painter.galley(egui::pos2(left, rect.top() + 64.0), story, theme::TEXT_3);
    if e.played {
        painter.text(
            egui::pos2(rect.right() - 60.0, rect.center().y),
            Align2::CENTER_CENTER,
            icons::CHECK_CIRCLE,
            theme::icon_fill(26.0),
            theme::ACCENT_HOVER,
        );
    }
    let more = egui::Rect::from_center_size(
        egui::pos2(rect.right() - 24.0, rect.center().y),
        Vec2::splat(36.0),
    );
    let more_resp = ui.interact(more, resp.id.with("more"), Sense::click());
    let m = ui
        .ctx()
        .animate_bool_with_time(more_resp.id, more_resp.hovered(), 0.12);
    let painter = ui.painter();
    painter.circle_filled(
        more.center(),
        18.0,
        Color32::from_white_alpha((m * 20.0) as u8),
    );
    painter.text(
        more.center(),
        Align2::CENTER_CENTER,
        icons::DOTS_THREE,
        theme::icon(24.0),
        theme::TEXT_2.lerp_to_gamma(Color32::WHITE, m.max(t * 0.6)),
    );
    let more_clicked = more_resp.tip("Details").clicked();
    (resp.clicked() && !more_clicked, more_clicked)
}

/// A film's, episode's or video's page.
fn details(ui: &mut egui::Ui, v: &mut View, item: &mut MbItem) -> Option<Go> {
    let mut go = None;
    ui.add_space(4.0);
    let key = item
        .backdrop
        .clone()
        .or_else(|| item.landscape.clone())
        .or_else(|| item.poster.clone());
    page::banner(ui, v.thumbs, key.as_deref(), BANNER, |inner| {
        inner.add_space(14.0);
        if let (Some(series), Some(ep)) = (&item.series_name, item.episode_label()) {
            widgets::section_label(inner, &format!("{series} · {ep}"));
        }
        inner.add(
            egui::Label::new(
                RichText::new(&item.name)
                    .font(theme::font(Weight::Bold, 40.0))
                    .color(Color32::WHITE),
            )
            .truncate(),
        );
        page::facts(inner, &item_facts(item));
        inner.add_space(16.0);
        inner.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 12.0;
            let resume = item.position > 1.0;
            let label = if resume {
                format!("Resume from {}", fmt_time(item.position))
            } else {
                "Play".to_string()
            };
            if widgets::button(ui, Some(icons::PLAY), &label, Kind::Primary).clicked() {
                go = Some(Go::Play(vec![item.clone()], 0, false));
            }
            if resume
                && widgets::button(
                    ui,
                    Some(icons::ARROW_COUNTER_CLOCKWISE),
                    "Start over",
                    Kind::Secondary,
                )
                .clicked()
            {
                go = Some(Go::Play(vec![item.clone()], 0, true));
            }
        });
        if matches!(
            item.kind,
            MbKind::Movie | MbKind::Episode | MbKind::Video | MbKind::MusicVideo
        ) {
            inner.add_space(8.0);
            inner.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 12.0;
                let button = |ui: &mut egui::Ui, icon, label| {
                    widgets::button(ui, Some(icon), label, Kind::Secondary).clicked()
                };
                if button(ui, icons::SKIP_FORWARD, "Play next") {
                    go = Some(Go::Queue(item.clone(), true));
                }
                if button(ui, icons::QUEUE, "Add to queue") {
                    go = Some(Go::Queue(item.clone(), false));
                }
                if button(ui, icons::PLAYLIST, "Add to playlist") {
                    go = Some(Go::AddToPlaylist(item.clone()));
                }
            });
        }
        inner.add_space(8.0);
        inner.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 8.0;
            go = go.take().or(mark_chips(ui, item));
            if item.kind == MbKind::Episode
                && let Some(id) = &item.series_id
                && widgets::chip_icon(ui, Some(icons::MONITOR_PLAY), "Series", false).clicked()
            {
                go = Some(Go::Open(MbItem {
                    id: id.clone(),
                    name: item.series_name.clone().unwrap_or_default(),
                    kind: MbKind::Series,
                    ..MbItem::default()
                }));
            }
        });
    });
    ui.add_space(20.0);
    ui.set_max_width(1100.0);
    if item.versions.len() > 1 {
        if let Some(version) = version_chips(ui, item) {
            item.version = version.clone();
            go = Some(Go::Version(item.location.clone(), version));
        }
        ui.add_space(14.0);
    }
    if let Some(o) = &item.overview {
        page::prose(ui, o);
        ui.add_space(14.0);
    }
    let line = |ui: &mut egui::Ui, label: &str, text: String| {
        if text.is_empty() {
            return;
        }
        widgets::section_label(ui, label);
        ui.label(RichText::new(text).color(theme::TEXT_2));
        ui.add_space(10.0);
    };
    line(ui, "Genres", item.genres.join("  ·  "));
    let (cast, crew): (Vec<_>, Vec<_>) = item
        .people
        .iter()
        .partition(|(_, role)| !matches!(role.as_str(), "Director" | "Writer" | "Producer"));
    line(
        ui,
        "Cast",
        cast.iter()
            .take(12)
            .map(|(n, r)| {
                if r.is_empty() || r == "Actor" {
                    n.clone()
                } else {
                    format!("{n} ({r})")
                }
            })
            .collect::<Vec<_>>()
            .join(",  "),
    );
    line(
        ui,
        "Crew",
        crew.iter()
            .map(|(n, r)| format!("{n} ({r})"))
            .collect::<Vec<_>>()
            .join(",  "),
    );
    line(ui, "Studios", item.studios.join("  ·  "));
    if !item.tags.is_empty() {
        widgets::section_label(ui, "Tags");
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing = Vec2::new(8.0, 8.0);
            for t in &item.tags {
                if widgets::chip(ui, t, false).clicked() {
                    go = Some(Go::Tag(t.clone()));
                }
            }
        });
    }
    ui.add_space(20.0);
    go
}

/// The versions the server keeps of a film or video (4K and 1080p, a
/// director's cut); the one clicked, to play it from wherever it is opened.
fn version_chips(ui: &mut egui::Ui, item: &MbItem) -> Option<Option<String>> {
    let mut chosen = None;
    widgets::section_label(ui, "Version");
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing = Vec2::new(8.0, 8.0);
        if widgets::chip(ui, "Automatic", item.version.is_none())
            .tip("The best that fits the server's maximum height")
            .clicked()
        {
            chosen = Some(None);
        }
        for version in &item.versions {
            let on = item.version.as_deref() == Some(version.id.as_str());
            let label = match format_badge(version.format) {
                Some(b) => format!("{} · {b}", version.label()),
                None => version.label(),
            };
            if widgets::chip(ui, &label, on).clicked() {
                chosen = Some(Some(version.id.clone()));
            }
        }
    });
    chosen
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(id: &str, kind: MbKind) -> MbItem {
        MbItem {
            id: id.into(),
            name: id.into(),
            kind,
            ..MbItem::default()
        }
    }

    #[test]
    fn answers_reach_the_page_that_asked() {
        let mut j = JellyfinView::new(1);
        j.pages.push(Page {
            seq: 2,
            loading: true,
            error: None,
            body: Body::Grid {
                title: "Movies".into(),
                query: MbQuery {
                    parent: Some("lib".into()),
                    ..MbQuery::default()
                },
                items: vec![],
                total: 0,
                typed: None,
            },
        });
        let page = |n: usize| MbPage {
            items: (0..n)
                .map(|i| item(&i.to_string(), MbKind::Movie))
                .collect(),
            total: 100,
        };
        j.loaded(
            2,
            Ok(Loaded::Grid {
                page: page(60),
                append: false,
            }),
        );
        j.loaded(
            2,
            Ok(Loaded::Grid {
                page: page(40),
                append: true,
            }),
        );
        // An answer for a page that is gone is dropped.
        j.loaded(
            9,
            Ok(Loaded::Grid {
                page: page(5),
                append: false,
            }),
        );
        let Body::Grid { items, total, .. } = &j.pages[1].body else {
            panic!()
        };
        assert_eq!((items.len(), *total), (100, 100));
        assert!(!j.pages[1].loading);
        // The home page is still waiting.
        assert!(j.pages[0].loading);
        j.loaded(1, Err("offline".into()));
        assert_eq!(j.pages[0].error.as_deref(), Some("offline"));

        // The header says where the page is.
        assert_eq!(trail(&j.pages[..1]), "Jellyfin / Emby");
        assert_eq!(trail(&j.pages), "Movies");

        // A reload covers everything shown.
        let (seq, load) = j.reload().unwrap();
        assert_eq!(seq, 2);
        let Load::Grid(q) = load else { panic!() };
        assert_eq!((q.start, q.limit), (0, 100));
    }

    #[test]
    fn marks_show_everywhere() {
        let mut j = JellyfinView::new(1);
        let mut watched = item("e1", MbKind::Episode);
        watched.position = 300.0;
        j.loaded(
            1,
            Ok(Loaded::Home(MbHome {
                resume: vec![watched.clone()],
                ..MbHome::default()
            })),
        );
        j.pages.push(Page {
            seq: 2,
            loading: false,
            error: None,
            body: Body::Item {
                item: Box::new(watched),
            },
        });
        j.mark("e1", Mark::Played, true);
        j.mark("e1", Mark::Favorite, true);
        let Body::Item { item } = &j.pages[1].body else {
            panic!()
        };
        assert!(item.played && item.favorite && item.position == 0.0);
        let Body::Home { home: Some(h), .. } = &j.pages[0].body else {
            panic!()
        };
        assert!(h.resume[0].played);
    }

    #[test]
    fn search_pages_show_rows_by_kind() {
        let mut j = JellyfinView::new(1);
        j.pages.push(Page {
            seq: 2,
            loading: true,
            error: None,
            body: Body::Search {
                search: "alien".into(),
                typed: None,
                asked: "alien".into(),
                results: vec![],
            },
        });
        assert!(!has_content(&j.pages[1].body));
        j.loaded(
            2,
            Ok(Loaded::Search {
                text: "aliens".into(),
                results: vec![
                    ("Movies", vec![item("m1", MbKind::Movie)]),
                    ("Shows", vec![item("s1", MbKind::Series)]),
                ],
            }),
        );
        let Body::Search { asked, results, .. } = &j.pages[1].body else {
            panic!()
        };
        assert_eq!(asked, "aliens");
        assert_eq!(results.len(), 2);
        assert_eq!(trail(&j.pages), "Search \"aliens\"");

        // Marks show in the results; a reload searches again.
        j.mark("s1", Mark::Favorite, true);
        let Body::Search { results, .. } = &j.pages[1].body else {
            panic!()
        };
        assert!(results[1].1[0].favorite);
        let (seq, load) = j.reload().unwrap();
        assert_eq!(seq, 2);
        assert!(matches!(load, Load::Search(t) if t == "aliens"));
    }

    #[test]
    fn requests_resume_from_the_server() {
        let mut m = item("m1", MbKind::Movie);
        m.location = "mb:http://jf#item=m1".into();
        assert_eq!(request("jf", &m, false).start_at, None);
        m.position = 120.0;
        assert_eq!(request("jf", &m, false).start_at, Some(120.0));
        assert_eq!(request("jf", &m, true).start_at, Some(0.0));
        let r = request("jf", &m, false);
        assert_eq!(r.source_id.as_deref(), Some("jf"));
        assert_eq!(r.entry.unwrap().location, "mb:http://jf#item=m1");
        assert_eq!(clip("one two three four", 9), "one two…");
        assert_eq!(clip("short", 9), "short");
    }
}
