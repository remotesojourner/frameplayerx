//! DeoVR feeds and HereSphere APIs: their scene groups as chips over a grid
//! of scenes, searched (in the group chosen) across titles, studios,
//! performers and tags, sorted and filtered, with a details page per scene
//! (description, performers, tags, chapters to start from).
//!
//! A HereSphere library lists scene addresses only: scenes on screen (and,
//! while searching, every scene of the group) have their details fetched as
//! they are needed.

use super::page::{self, Card};
use super::theme::{self, Weight};
use super::widgets::{self, Kind, Tip};
use super::{Action, View, fmt_date, fmt_time, icons};
use crate::playback::OpenRequest;
use egui::{RichText, Sense, Vec2};
use fp_sources::{FeedLibrary, FeedScene, SceneInfo, SourceKind};
use std::collections::{HashMap, HashSet};

/// Scenes shown before "Show more".
const PAGE: usize = 60;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum FeedSort {
    /// As the feed lists them.
    #[default]
    Feed,
    Title,
    /// Newest release (or addition) first.
    Newest,
    Rating,
    Longest,
    Shortest,
}

pub struct FeedView {
    pub kind: SourceKind,
    /// The server's listing is on its way (the one saved last time may be
    /// shown meanwhile).
    pub loading: bool,
    /// A HereSphere scan is on its way: titles, people and tags of every
    /// scene.
    pub scanning: bool,
    pub error: Option<String>,
    /// Groups in feed order with their scenes' URLs.
    pub groups: Vec<(String, Vec<String>)>,
    pub scenes: HashMap<String, FeedScene>,
    /// Every scene once, as first listed.
    pub order: Vec<String>,
    /// The group shown; `None` for all of them.
    pub group: Option<usize>,
    pub search: String,
    pub sort: FeedSort,
    pub scripted_only: bool,
    pub favorites_only: bool,
    /// Scenes shown (paging).
    pub shown: usize,
    pub details: Option<Details>,
    /// Scenes whose details were asked for.
    pub requested: HashSet<String>,
    /// Scenes whose details came since the listing was last saved.
    pub unsaved: usize,
}

/// A scene's details page.
pub struct Details {
    pub url: String,
    pub info: Option<Box<SceneInfo>>,
    pub loading: bool,
    pub error: Option<String>,
}

impl FeedView {
    pub fn new(kind: SourceKind) -> FeedView {
        FeedView {
            kind,
            loading: true,
            scanning: false,
            error: None,
            groups: Vec::new(),
            scenes: HashMap::new(),
            order: Vec::new(),
            group: None,
            search: String::new(),
            sort: FeedSort::Feed,
            scripted_only: false,
            favorites_only: false,
            shown: PAGE,
            details: None,
            requested: HashSet::new(),
            unsaved: 0,
        }
    }

    /// Takes the feed's groups and scenes, keeping what is known already:
    /// details read, and titles a bare listing doesn't have. `cached`: the
    /// listing saved last time, shown while the server answers (only into
    /// an empty page).
    pub fn loaded(&mut self, result: Result<FeedLibrary, String>, cached: bool) {
        if cached && !self.order.is_empty() {
            return;
        }
        if !cached {
            self.loading = false;
        }
        let lib = match result {
            Ok(l) => l,
            Err(e) => {
                self.error = Some(e);
                return;
            }
        };
        self.error = None;
        // A HereSphere listing is addresses only; the scan follows it.
        if !cached && self.kind == SourceKind::HereSphere {
            self.scanning = true;
        }
        self.order = lib.scenes.iter().map(|s| s.url.clone()).collect();
        let mut scenes = HashMap::with_capacity(lib.scenes.len());
        for s in lib.scenes {
            let keep = self
                .scenes
                .remove(&s.url)
                .filter(|old| old.detailed || (s.bare && !old.bare));
            scenes.insert(s.url.clone(), keep.unwrap_or(s));
        }
        self.scenes = scenes;
        self.groups = lib.groups;
        if self.group.is_some_and(|g| g >= self.groups.len()) {
            self.group = None;
        }
        // One group: show it rather than "All" beside it.
        if self.groups.len() == 1 {
            self.group = Some(0);
        }
    }

    /// Takes a HereSphere scan into the scenes (`None`: the server has
    /// none). Scenes whose own details came first keep them: they say the
    /// same and more.
    pub fn scanned(&mut self, scan: Option<Vec<FeedScene>>) {
        self.scanning = false;
        for found in scan.into_iter().flatten() {
            if let Some(s) = self.scenes.get_mut(&found.url).filter(|s| !s.detailed) {
                s.merge(found);
            }
        }
    }

    /// Shows a favourite at once, on the scene's card and page; the server
    /// is told separately.
    pub fn mark_favorite(&mut self, url: &str, on: bool) {
        if let Some(s) = self.scenes.get_mut(url) {
            s.favorite = on;
        }
        if let Some(info) = self
            .details
            .as_mut()
            .filter(|d| d.url == url)
            .and_then(|d| d.info.as_mut())
        {
            info.favorite = Some(on);
        }
    }

    /// Takes a scene's details: into its card, and its page when open
    /// (`full`: with media, for the page).
    pub fn scene_loaded(&mut self, url: &str, result: Result<SceneInfo, String>, full: bool) {
        match result {
            Ok(info) => {
                if let Some(s) = self.scenes.get_mut(url) {
                    s.apply(&info);
                    self.unsaved += 1;
                }
                if full && let Some(d) = self.details.as_mut().filter(|d| d.url == url) {
                    d.loading = false;
                    d.info = Some(Box::new(info));
                }
            }
            Err(e) => {
                log::debug!("scene details {url}: {e}");
                // A card without details keeps what the listing said.
                if let Some(s) = self.scenes.get_mut(url) {
                    s.detailed = true;
                }
                if full && let Some(d) = self.details.as_mut().filter(|d| d.url == url) {
                    d.loading = false;
                    d.error = Some(e);
                }
            }
        }
    }

    /// URLs of the scenes in the group shown (all of them for "All").
    fn scope(&self) -> &[String] {
        match self.group.and_then(|g| self.groups.get(g)) {
            Some((_, urls)) => urls,
            None => &self.order,
        }
    }

    /// The scenes to show: searched, filtered and sorted.
    pub fn visible(&self) -> Vec<&FeedScene> {
        let words: Vec<String> = self
            .search
            .split_whitespace()
            .map(str::to_lowercase)
            .collect();
        // A search looks through the group chosen ("All": every group).
        let mut out: Vec<&FeedScene> = self
            .scope()
            .iter()
            .filter_map(|u| self.scenes.get(u))
            .filter(|s| words.is_empty() || s.matches(&words))
            .filter(|s| !self.scripted_only || s.scripted)
            .filter(|s| !self.favorites_only || s.favorite)
            .collect();
        let title = |s: &FeedScene| widgets::display_title(&s.title).to_lowercase();
        // Known values first in either direction, the rest after in feed
        // order (the sorts are stable).
        use std::cmp::Ordering;
        fn known<T: PartialOrd>(a: Option<T>, b: Option<T>, descending: bool) -> Ordering {
            match (a, b) {
                (Some(a), Some(b)) => {
                    let o = a.partial_cmp(&b).unwrap_or(Ordering::Equal);
                    if descending { o.reverse() } else { o }
                }
                (Some(_), None) => Ordering::Less,
                (None, Some(_)) => Ordering::Greater,
                (None, None) => Ordering::Equal,
            }
        }
        let date = |s: &FeedScene| s.released.or(s.added);
        match self.sort {
            FeedSort::Feed => {}
            FeedSort::Title => out.sort_by_cached_key(|s| title(s)),
            FeedSort::Newest => out.sort_by(|a, b| known(date(a), date(b), true)),
            FeedSort::Rating => out.sort_by(|a, b| known(a.rating, b.rating, true)),
            FeedSort::Longest => out.sort_by(|a, b| known(a.duration, b.duration, true)),
            FeedSort::Shortest => out.sort_by(|a, b| known(a.duration, b.duration, false)),
        }
        out
    }

    /// Whether any scene says `has`.
    fn any(&self, has: impl Fn(&FeedScene) -> bool) -> bool {
        self.scenes.values().any(has)
    }

    /// Scenes of the group shown that need their details fetched to be
    /// searched, not asked for yet: those known by their address only (no
    /// scan said more), and a DeoVR feed's, whose listing has no tags or
    /// performers.
    fn unfilled(&self) -> Vec<String> {
        let deovr = self.kind == SourceKind::DeoVr;
        self.scope()
            .iter()
            .filter(|u| !self.requested.contains(*u))
            .filter(|u| {
                self.scenes
                    .get(*u)
                    .is_some_and(|s| !s.detailed && (s.bare || deovr))
            })
            .cloned()
            .collect()
    }

    /// The groups and scenes as shown, details included, to save as the
    /// listing shown next time.
    pub fn library(&self) -> FeedLibrary {
        FeedLibrary {
            groups: self.groups.clone(),
            scenes: self
                .order
                .iter()
                .filter_map(|u| self.scenes.get(u).cloned())
                .collect(),
        }
    }
}

/// Opens `scene`.
/// Opens a scene whose page has every detail: opening needs no second fetch.
fn scene_request(
    source: &str,
    scene: &FeedScene,
    info: Option<&SceneInfo>,
    start_at: Option<f64>,
) -> OpenRequest {
    let mut req = request(source, scene, start_at);
    if let Some(info) = info {
        let mut e = scene.to_entry();
        info.apply_to(&mut e);
        req.entry = Some(e);
    }
    req
}

fn request(source: &str, scene: &FeedScene, start_at: Option<f64>) -> OpenRequest {
    OpenRequest {
        location: scene.url.clone(),
        source_id: Some(source.to_string()),
        entry: Some(scene.to_entry()),
        title: None,
        start_at,
        transcode: None,
    }
}

/// "25:12 · VRStudio" or "25:12 · 12 Oct 2026".
fn card_facts(s: &FeedScene) -> String {
    let mut f = Vec::new();
    if let Some(d) = s.duration {
        f.push(fmt_time(d));
    }
    if let Some(studio) = &s.studio {
        f.push(studio.clone());
    } else if let Some(t) = s.released.or(s.added) {
        f.push(fmt_date(t));
    }
    f.join("  ·  ")
}

/// A feed's scenes or a scene's page; true when Back was clicked.
pub fn page(ui: &mut egui::Ui, v: &mut View, source: &str, name: &str, f: &mut FeedView) -> bool {
    if f.details.is_some() {
        return details(ui, v, source, name, f);
    }
    let haptics = v.settings.haptics_enabled;
    if !haptics {
        f.scripted_only = false;
    }
    let back = header(ui, v.actions, source, name, f);
    let mut changed = filters(ui, f, haptics);
    let shown = f.visible();
    let total = shown.len();
    let mut sort = f.sort;
    ui.add_space(6.0);
    ui.horizontal(|ui| {
        changed |= page::sort_bar(ui, &sort_choices(f), &mut sort);
        page::count_label(ui, &page::plural(total, "scene", "scenes"));
    });
    ui.add_space(10.0);
    page::status(ui, f.loading && f.order.is_empty(), f.error.as_deref());

    let list: Vec<FeedScene> = shown.into_iter().take(f.shown).cloned().collect();
    let picked = scene_grid(ui, v.thumbs, f, &list, total, haptics);
    f.sort = sort;
    if changed {
        f.shown = PAGE;
    }
    if picked.more {
        f.shown += PAGE;
    }
    if let Some(i) = picked.play {
        let reqs = list.iter().map(|s| request(source, s, None)).collect();
        v.actions.push(Action::OpenList(reqs, i));
    }
    if let Some(url) = picked.open {
        f.details = Some(Details {
            url: url.clone(),
            info: None,
            loading: true,
            error: None,
        });
        v.actions.push(Action::FeedScene {
            source: source.to_string(),
            url,
        });
    }
    if f.kind == SourceKind::HereSphere {
        fill_details(v.actions, source, f, picked.on_screen);
    }
    back
}

/// The page's title, how many scenes, and Refresh; true when Back was
/// clicked.
fn header(
    ui: &mut egui::Ui,
    actions: &mut Vec<Action>,
    source: &str,
    name: &str,
    f: &mut FeedView,
) -> bool {
    let mut subtitle = format!(
        "{} · {}",
        f.kind.label(),
        page::plural(f.order.len(), "scene", "scenes")
    );
    // Scenes are shown (last time's, or a listing before its scan) while
    // the rest arrives.
    if !f.order.is_empty() && (f.loading || f.scanning) {
        subtitle.push_str(" · updating…");
    }
    page::header(ui, name, Some(&subtitle), |ui| {
        if widgets::icon_button(ui, icons::ARROWS_CLOCKWISE, 44.0, false)
            .tip("Refresh")
            .clicked()
        {
            f.loading = true;
            actions.push(Action::FeedLoad {
                source: source.to_string(),
            });
        }
    })
}

/// Search, the Scripted and Favorites filters, and the groups; true when
/// they changed.
fn filters(ui: &mut egui::Ui, f: &mut FeedView, haptics: bool) -> bool {
    let mut changed = false;
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 8.0;
        changed |= page::search_field(ui, &mut f.search, "Search titles, people, tags", 300.0);
        ui.add_space(10.0);
        if haptics
            && f.any(|s| s.scripted)
            && widgets::chip_icon(ui, Some(icons::WAVEFORM), "Scripted", f.scripted_only).clicked()
        {
            f.scripted_only = !f.scripted_only;
            changed = true;
        }
        if f.any(|s| s.favorite)
            && widgets::chip_icon(ui, Some(icons::HEART), "Favorites", f.favorites_only).clicked()
        {
            f.favorites_only = !f.favorites_only;
            changed = true;
        }
    });
    if f.groups.len() > 1 {
        ui.add_space(6.0);
        egui::ScrollArea::horizontal()
            .id_salt("feed-groups")
            .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysHidden)
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 8.0;
                    if widgets::chip(ui, "All", f.group.is_none()).clicked() {
                        f.group = None;
                        changed = true;
                    }
                    for (i, (g, urls)) in f.groups.iter().enumerate() {
                        let label = format!("{g}  {}", urls.len());
                        if widgets::chip(ui, &label, f.group == Some(i)).clicked() {
                            f.group = Some(i);
                            changed = true;
                        }
                    }
                });
            });
    }
    changed
}

/// The orders the scenes can be shown in: those the feed has data for.
fn sort_choices(f: &FeedView) -> Vec<(FeedSort, &'static str)> {
    let mut sorts = vec![(FeedSort::Feed, "Feed order"), (FeedSort::Title, "Title")];
    if f.any(|s| s.released.or(s.added).is_some()) {
        sorts.push((FeedSort::Newest, "Newest"));
    }
    if f.any(|s| s.rating.is_some()) {
        sorts.push((FeedSort::Rating, "Rating"));
    }
    sorts.extend([
        (FeedSort::Longest, "Longest"),
        (FeedSort::Shortest, "Shortest"),
    ]);
    sorts
}

/// What was picked in the grid of scenes.
#[derive(Default)]
struct Picked {
    /// Play scene `.0` of the list.
    play: Option<usize>,
    /// Open a scene's page.
    open: Option<String>,
    more: bool,
    /// Scenes on screen still without their details.
    on_screen: Vec<String>,
}

fn scene_grid(
    ui: &mut egui::Ui,
    thumbs: &mut super::thumbs::Thumbs,
    f: &FeedView,
    list: &[FeedScene],
    total: usize,
    haptics: bool,
) -> Picked {
    let mut picked = Picked::default();
    page::scroll(ui, |ui| {
        if list.is_empty() && !f.loading && f.error.is_none() {
            let (title, body) = if f.order.is_empty() {
                ("No scenes", "The server lists no scenes here.")
            } else {
                (
                    "Nothing matches",
                    "Try another search, or clear the filters.",
                )
            };
            page::empty_state(ui, icons::VIRTUAL_REALITY, title, body, |_| {});
        }
        page::grid(ui, list.len(), page::LANDSCAPE.x, |ui, i| {
            let s = &list[i];
            let title = widgets::display_title(&s.title);
            let facts = card_facts(s);
            let badge = s.format.map(|f| widgets::format_short(&f));
            let c = page::card(
                ui,
                thumbs,
                &Card {
                    thumb: s.thumbnail_url.as_deref(),
                    placeholder: icons::VIRTUAL_REALITY,
                    title: &title,
                    facts: &facts,
                    badge: badge.as_deref(),
                    corner: (haptics && s.scripted).then_some(icons::WAVEFORM),
                    favorite: s.favorite,
                    more: true,
                    ..Card::default()
                },
            );
            if c.visible && !s.detailed {
                picked.on_screen.push(s.url.clone());
            }
            if c.open {
                picked.play = Some(i);
            }
            if c.more {
                picked.open = Some(s.url.clone());
            }
        });
        if total > list.len() {
            ui.vertical_centered(|ui| {
                picked.more = widgets::button(ui, None, "Show more", Kind::Secondary).clicked();
            });
            ui.add_space(20.0);
        }
    });
    picked
}

/// Asks for the details of scenes whose listing lacks them: cards on screen
/// first; while searching, the rest of the group after, so their tags and
/// performers are searched too.
fn fill_details(
    actions: &mut Vec<Action>,
    source: &str,
    f: &mut FeedView,
    mut on_screen: Vec<String>,
) {
    on_screen.retain(|u| f.requested.insert(u.clone()));
    if !on_screen.is_empty() {
        actions.push(Action::FeedFill {
            source: source.to_string(),
            urls: on_screen,
            front: true,
        });
    }
    // A scan on its way brings what a search looks through anyway.
    if !f.search.trim().is_empty() && !f.scanning {
        let rest = f.unfilled();
        if !rest.is_empty() {
            f.requested.extend(rest.iter().cloned());
            actions.push(Action::FeedFill {
                source: source.to_string(),
                urls: rest,
                front: false,
            });
        }
    }
}

/// A scene's page; true when Back was clicked.
fn details(ui: &mut egui::Ui, v: &mut View, source: &str, name: &str, f: &mut FeedView) -> bool {
    let Some(d) = f.details.as_ref() else {
        return false;
    };
    let mut scene = f.scenes.get(&d.url).cloned().unwrap_or_else(|| FeedScene {
        url: d.url.clone(),
        ..FeedScene::default()
    });
    if let Some(info) = &d.info {
        scene.apply(info);
    }
    let title = widgets::display_title(&scene.title);
    let back = page::header(ui, name, Some(&title), |_| {});
    page::status(ui, d.loading, d.error.as_deref());
    let info = d.info.clone();
    // The server says whether it takes favourites from players.
    let writable = info.as_ref().is_some_and(|i| i.write_favorite);
    let (mut play_at, mut search) = (None::<f64>, None::<String>);
    let mut favorite = None::<bool>;
    let queued = scene_request(source, &scene, info.as_deref(), None);
    egui::ScrollArea::vertical()
        .auto_shrink(false)
        .show(ui, |ui| {
            ui.add_space(8.0);
            ui.horizontal_top(|ui| {
                ui.spacing_mut().item_spacing.x = 28.0;
                let (rect, _) = ui.allocate_exact_size(Vec2::new(512.0, 288.0), Sense::hover());
                widgets::shadow(ui.painter(), rect, 16, 0.6);
                page::picture(
                    ui,
                    v.thumbs,
                    scene.thumbnail_url.as_deref(),
                    rect,
                    16,
                    icons::VIRTUAL_REALITY,
                );
                ui.vertical(|ui| {
                    ui.spacing_mut().item_spacing.y = 8.0;
                    ui.label(
                        RichText::new(&title)
                            .font(theme::font(Weight::Bold, 34.0))
                            .color(theme::TEXT),
                    );
                    let mut facts = Vec::new();
                    if let Some(t) = scene.duration {
                        facts.push(fmt_time(t));
                    }
                    if let Some(t) = scene.released {
                        facts.push(fmt_date(t));
                    }
                    if let Some(fm) = scene.format {
                        facts.push(widgets::format_short(&fm));
                    }
                    if let Some(r) = scene.rating {
                        facts.push(page::rating(r));
                    }
                    if scene.favorite && !writable {
                        facts.push(format!("{}  Favorite", icons::HEART));
                    }
                    if scene.scripted && v.settings.haptics_enabled {
                        facts.push(format!("{}  Haptics", icons::WAVEFORM));
                    }
                    page::facts(ui, &facts);
                    ui.add_space(10.0);
                    if widgets::button(ui, Some(icons::PLAY), "Play", Kind::Primary).clicked() {
                        play_at = Some(0.0);
                    }
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 12.0;
                        super::queue::buttons(ui, v, &queued);
                    });
                    if writable {
                        let label = if scene.favorite {
                            "Favorite"
                        } else {
                            "Add to favorites"
                        };
                        if widgets::chip_icon(ui, Some(icons::HEART), label, scene.favorite)
                            .clicked()
                        {
                            favorite = Some(!scene.favorite);
                        }
                    }
                    if let Some(info) = &info
                        && !info.sources.is_empty()
                    {
                        let mut versions: Vec<String> = info
                            .sources
                            .iter()
                            .map(|s| match (s.height, s.label.is_empty()) {
                                (Some(h), false) => format!("{h}p {}", s.label),
                                (Some(h), true) => format!("{h}p"),
                                (None, _) => s.label.clone(),
                            })
                            .filter(|s| !s.is_empty())
                            .collect();
                        versions.dedup();
                        if !versions.is_empty() {
                            ui.label(
                                RichText::new(format!("Versions: {}", versions.join(", ")))
                                    .font(theme::font(Weight::Regular, 15.0))
                                    .color(theme::TEXT_3),
                            );
                        }
                    }
                });
            });
            ui.add_space(22.0);
            ui.set_max_width(1100.0);
            if let Some(text) = info.as_ref().and_then(|i| i.description.as_deref()) {
                page::prose(ui, text);
                ui.add_space(16.0);
            }
            if !scene.actors.is_empty() {
                widgets::section_label(ui, "Performers");
                search = search.take().or(page::pills(ui, &scene.actors));
                ui.add_space(14.0);
            }
            if !scene.tags.is_empty() {
                widgets::section_label(ui, "Tags");
                search = search.take().or(page::pills(ui, &scene.tags));
                ui.add_space(14.0);
            }
            let markers = info.as_ref().map(|i| i.markers.as_slice()).unwrap_or(&[]);
            if !markers.is_empty() {
                widgets::section_label(ui, "Chapters");
                ui.add_space(2.0);
                ui.horizontal_wrapped(|ui| {
                    ui.spacing_mut().item_spacing = Vec2::new(8.0, 8.0);
                    for (t, name) in markers {
                        let label = if name.is_empty() {
                            fmt_time(*t)
                        } else {
                            format!("{}  {name}", fmt_time(*t))
                        };
                        if widgets::chip_icon(ui, Some(icons::PLAY), &label, false)
                            .tip("Play from here")
                            .clicked()
                        {
                            play_at = Some(*t);
                        }
                    }
                });
            }
            ui.add_space(24.0);
        });
    if let Some(t) = play_at {
        let req = scene_request(source, &scene, info.as_deref(), Some(t));
        v.actions.push(Action::Open(req));
    }
    if let Some(on) = favorite {
        f.mark_favorite(&scene.url, on);
        v.actions.push(Action::FeedFavorite {
            source: source.to_string(),
            url: scene.url.clone(),
            on,
        });
    }
    if let Some(s) = search {
        // A performer or tag: the scenes that have it.
        f.search = s;
        f.group = None;
        f.shown = PAGE;
        f.details = None;
    }
    back
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scene(url: &str, title: &str, duration: f64) -> FeedScene {
        FeedScene {
            url: url.into(),
            title: title.into(),
            duration: Some(duration),
            ..FeedScene::default()
        }
    }

    fn view() -> FeedView {
        let mut f = FeedView::new(SourceKind::DeoVr);
        f.loaded(
            Ok(FeedLibrary {
                groups: vec![
                    ("Recent".into(), vec!["a".into(), "b".into()]),
                    ("Scripted".into(), vec!["c".into()]),
                ],
                scenes: vec![
                    scene("a", "Beach", 60.0),
                    FeedScene {
                        studio: Some("Lake Films".into()),
                        scripted: true,
                        ..scene("b", "Alps", 600.0)
                    },
                    FeedScene {
                        actors: vec!["Jane".into()],
                        rating: Some(4.5),
                        ..scene("c", "Canyon", 30.0)
                    },
                ],
            }),
            false,
        );
        f
    }

    fn titles(f: &FeedView) -> Vec<String> {
        f.visible().iter().map(|s| s.title.clone()).collect()
    }

    #[test]
    fn groups_search_filters_and_orders() {
        let mut f = view();
        assert!(!f.loading && f.group.is_none());
        assert_eq!(titles(&f), ["Beach", "Alps", "Canyon"]);
        f.group = Some(1);
        assert_eq!(titles(&f), ["Canyon"]);
        // A search looks through the group chosen, studios and performers
        // too; "All" looks through every group.
        f.search = "lake".into();
        assert!(titles(&f).is_empty());
        f.search = "JANE".into();
        assert_eq!(titles(&f), ["Canyon"]);
        f.group = None;
        f.search = "lake".into();
        assert_eq!(titles(&f), ["Alps"]);
        f.search.clear();
        f.sort = FeedSort::Title;
        assert_eq!(titles(&f), ["Alps", "Beach", "Canyon"]);
        f.sort = FeedSort::Longest;
        assert_eq!(titles(&f), ["Alps", "Beach", "Canyon"]);
        f.sort = FeedSort::Shortest;
        assert_eq!(titles(&f), ["Canyon", "Beach", "Alps"]);
        f.sort = FeedSort::Rating;
        assert_eq!(titles(&f)[0], "Canyon");
        f.sort = FeedSort::Feed;
        f.scripted_only = true;
        assert_eq!(titles(&f), ["Alps"]);
    }

    #[test]
    fn favourites_show_at_once() {
        let mut f = view();
        f.details = Some(Details {
            url: "a".into(),
            info: Some(Box::new(SceneInfo {
                favorite: Some(false),
                write_favorite: true,
                ..SceneInfo::default()
            })),
            loading: false,
            error: None,
        });
        f.mark_favorite("a", true);
        assert!(f.scenes["a"].favorite);
        let d = f.details.as_ref().unwrap();
        assert_eq!(d.info.as_ref().unwrap().favorite, Some(true));
        f.favorites_only = true;
        assert_eq!(titles(&f), ["Beach"]);
    }

    #[test]
    fn a_deovr_search_fetches_details_and_they_are_kept() {
        // A DeoVR listing has titles but no tags: a search fetches the rest.
        let mut f = view();
        assert_eq!(f.unfilled(), ["a", "b", "c"]);
        let info = SceneInfo {
            tags: vec!["Sunset".into()],
            ..SceneInfo::default()
        };
        f.scene_loaded("a", Ok(info), false);
        assert_eq!((f.unfilled(), f.unsaved), (vec!["b".into(), "c".into()], 1));
        f.search = "sunset".into();
        assert_eq!(titles(&f), ["Beach"]);
        let saved = f.library();
        assert!(saved.scenes[0].detailed && saved.scenes[0].tags == ["Sunset"]);
        assert_eq!(saved.groups, f.groups);
        // A HereSphere scene a scan described needs no fetching.
        let mut h = view();
        h.kind = SourceKind::HereSphere;
        assert!(h.unfilled().is_empty());
    }

    #[test]
    fn details_fill_cards_and_survive_a_refresh() {
        let mut f = view();
        // Known by their address only, as a HereSphere listing without a
        // scan has them.
        for s in f.scenes.values_mut() {
            s.bare = true;
        }
        f.details = Some(Details {
            url: "a".into(),
            info: None,
            loading: true,
            error: None,
        });
        let info = SceneInfo {
            title: "Beach Day".into(),
            scripts: vec!["s.funscript".into()],
            ..SceneInfo::default()
        };
        f.scene_loaded("a", Ok(info), true);
        assert!(f.details.as_ref().unwrap().info.is_some());
        assert!(f.scenes["a"].detailed && f.scenes["a"].scripted);
        // A failed fetch is not retried, and the card keeps its listing.
        f.scene_loaded("b", Err("timeout".into()), false);
        assert!(f.scenes["b"].detailed);
        assert_eq!(f.scenes["b"].title, "Alps");
        assert_eq!(f.unfilled(), ["c"]);
        // A search in a group fetches that group's scenes only.
        f.group = Some(0);
        assert!(f.unfilled().is_empty());
        f.group = None;

        f.loaded(
            Ok(FeedLibrary {
                groups: vec![("Recent".into(), vec!["a".into()])],
                scenes: vec![scene("a", "Beach", 60.0)],
            }),
            false,
        );
        assert_eq!(f.scenes["a"].title, "Beach Day");
        assert_eq!(f.group, Some(0), "a lone group is shown as such");
    }

    #[test]
    fn last_listing_first_then_the_servers_then_its_scan() {
        let bare = |url: &str| FeedScene {
            bare: true,
            ..scene(url, url, 0.0)
        };
        let listing = |scenes: Vec<FeedScene>| FeedLibrary {
            groups: vec![(
                "Recent".into(),
                scenes.iter().map(|s| s.url.clone()).collect(),
            )],
            scenes,
        };
        let mut f = FeedView::new(SourceKind::HereSphere);
        // Last time's listing, with the titles its scan gave.
        f.loaded(Ok(listing(vec![scene("12", "Beach", 60.0)])), true);
        assert!(f.loading && !f.scanning && titles(&f) == ["Beach"]);
        // Not over a page that shows something already (a refresh).
        f.loaded(Ok(listing(vec![scene("99", "Old", 1.0)])), true);
        assert_eq!(titles(&f), ["Beach"]);

        // The server's bare listing keeps the titles it doesn't have; a new
        // scene shows its address until the scan.
        f.loaded(Ok(listing(vec![bare("12"), bare("13")])), false);
        assert!(!f.loading && f.scanning);
        assert_eq!(titles(&f), ["Beach", "13"]);
        // Bare scenes are what a search would fetch (the page waits for the
        // scan instead while it is on its way).
        f.search = "13".into();
        assert_eq!(f.unfilled(), ["13"]);
        // A card whose own details came first keeps them.
        if let Some(s) = f.scenes.get_mut("12") {
            s.detailed = true;
            s.duration = Some(2627.0);
        }
        f.scanned(Some(vec![
            FeedScene {
                tags: vec!["Sun".into()],
                ..scene("13", "Lake", 90.0)
            },
            scene("12", "Beach", 2.6),
        ]));
        assert!(!f.scanning && !f.scenes["13"].bare);
        assert_eq!(f.scenes["12"].duration, Some(2627.0));
        assert!(f.unfilled().is_empty(), "the scan answers the search");
        f.search = "sun".into();
        assert_eq!(titles(&f), ["Lake"]);
    }
}
