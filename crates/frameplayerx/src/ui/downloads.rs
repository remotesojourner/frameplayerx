//! Downloads, a place in Sources and a tile on Home: every network source
//! with how many of its videos are downloaded, a locked one only as locked,
//! with Unlock. A source opens on its downloads and how each is going. And
//! the Download button on a video's page.

use super::page;
use super::theme::{self, Weight};
use super::widgets::{self, Kind, ListArea, Tip};
use super::{Action, PinForm, ServerCheck, View, fmt_size, icons};
use crate::playback::OpenRequest;
use egui::{Align, Layout, RichText, Sense, Vec2};
use fp_library::{Download, DownloadCount, DownloadState, MediaRecord};
use fp_sources::{SourceConfig, SourceKind};
use std::collections::HashMap;

/// The Downloads place: its list of sources, or one source's downloads.
#[derive(Default)]
pub struct DownloadsView {
    /// The source whose downloads show (id and name), else the list.
    pub source: Option<(String, String)>,
    /// The downloads shown, as read at a generation of the downloads.
    rows: Option<(u64, Vec<Row>)>,
}

impl DownloadsView {
    /// Reads the downloads again: their videos changed.
    pub fn reload(&mut self) {
        self.rows = None;
    }
}

struct Row {
    record: MediaRecord,
    download: Download,
    /// The picture kept with the download, else the video's.
    picture: Option<String>,
}

/// Whether a source's videos can be downloaded: those on a network.
pub fn network(c: &SourceConfig) -> bool {
    !matches!(c.kind(), SourceKind::Local | SourceKind::Library)
}

/// How many downloads each source not locked has.
pub(super) fn counts(v: &mut View) -> HashMap<String, DownloadCount> {
    let generation = v.services.downloads.generation();
    let hidden = v.services.library.hidden();
    if let Some((g, h, c)) = &v.state.download_counts
        && *g == generation
        && *h == hidden
    {
        return c.clone();
    }
    let counts: HashMap<_, _> = v
        .services
        .library
        .download_counts()
        .unwrap_or_default()
        .into_iter()
        .collect();
    v.state.download_counts = Some((generation, hidden, counts.clone()));
    counts
}

/// "3 downloads · 12.4 GB · 1 not finished", or "No downloads".
pub(super) fn summary(c: &DownloadCount) -> String {
    if c.videos == 0 {
        return tr!("downloads-none").to_string();
    }
    let mut s = format!(
        "{} · {}",
        tr!("downloads-count", count = c.videos),
        fmt_size(c.bytes)
    );
    if c.unfinished > 0 {
        s += &format!(" · {}", tr!("downloads-unfinished", count = c.unfinished));
    }
    s
}

/// Every source's downloads added up.
pub(super) fn total(counts: &HashMap<String, DownloadCount>) -> DownloadCount {
    counts
        .values()
        .fold(DownloadCount::default(), |a, c| DownloadCount {
            videos: a.videos + c.videos,
            unfinished: a.unfinished + c.unfinished,
            bytes: a.bytes + c.bytes,
        })
}

fn locked(v: &View, source: &str) -> bool {
    crate::pin::locked(&v.settings.source_pins, &v.state.unlocked_sources, source)
}

/// Where new downloads go, and the room there.
fn destination(v: &View) -> String {
    let root = crate::downloads::root_location(v.settings.download_to.as_ref());
    let place = match &v.settings.download_to {
        None => tr!("downloads-to-headset").to_string(),
        Some(c) => tr!("downloads-to-card", card = c.label.as_str()),
    };
    if crate::downloads::path_of(&root).is_none() {
        return format!("{place} · {}", tr!("downloads-card-missing"));
    }
    match crate::downloads::room_at(&root) {
        Some(free) => format!("{place} · {}", tr!("downloads-free", size = fmt_size(free))),
        None => place,
    }
}

/// The Downloads place under the way to it, `trail`; what was pressed on
/// the header.
pub fn page(
    ui: &mut egui::Ui,
    v: &mut View,
    trail: &[String],
    d: &mut DownloadsView,
) -> Option<page::Go> {
    if let Some((source, _)) = d.source.clone() {
        if locked(v, &source) || v.services.source_config(&source).is_none() {
            d.source = None;
            d.rows = None;
        } else {
            return source_page(ui, v, trail, &source, d);
        }
    }
    let subtitle = destination(v);
    let go = page::trail_header(ui, trail, Some(&subtitle), |_| {});
    let configs: Vec<SourceConfig> = v
        .services
        .source_configs
        .iter()
        .filter(|c| network(c))
        .cloned()
        .collect();
    if configs.is_empty() {
        page::empty_state(
            ui,
            icons::DOWNLOAD_SIMPLE,
            tr!("downloads-no-sources"),
            tr!("downloads-no-sources-desc"),
            |_| {},
        );
        return go;
    }
    let counts = counts(v);
    egui::ScrollArea::vertical()
        .auto_shrink(false)
        .show_list(ui, |ui| {
            widgets::card(ui, |ui| {
                for (i, c) in configs.iter().enumerate() {
                    if i > 0 {
                        let x = ui.max_rect().x_range();
                        ui.painter().hline(
                            (x.min + 84.0)..=(x.max - 16.0),
                            ui.cursor().top(),
                            egui::Stroke::new(1.0_f32, theme::STROKE),
                        );
                    }
                    if source_row(ui, v, c, counts.get(c.id())) {
                        d.source = Some((c.id().to_string(), c.name().to_string()));
                        d.rows = None;
                    }
                }
            });
        });
    go
}

/// A source in the Downloads list: its count, or Locked and Unlock.
/// Whether it was opened.
fn source_row(
    ui: &mut egui::Ui,
    v: &mut View,
    c: &SourceConfig,
    count: Option<&DownloadCount>,
) -> bool {
    let is_locked = locked(v, c.id());
    let desc = if is_locked {
        tr!("sources-locked").to_string()
    } else {
        summary(&count.copied().unwrap_or_default())
    };
    let mut open = false;
    egui::Frame::new()
        .inner_margin(egui::Margin::symmetric(16, 12))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 16.0;
                super::sources::icon_tile(
                    ui,
                    super::sources::kind_icon(c.kind()),
                    52.0,
                    theme::ACCENT_HOVER,
                );
                ui.vertical(|ui| {
                    ui.spacing_mut().item_spacing.y = 2.0;
                    ui.label(
                        RichText::new(c.name())
                            .font(theme::font(Weight::SemiBold, 19.0))
                            .color(theme::TEXT),
                    );
                    ui.label(
                        RichText::new(&desc)
                            .font(theme::font(Weight::Regular, 15.0))
                            .color(theme::TEXT_2),
                    );
                });
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if is_locked {
                        if widgets::button_sized(
                            ui,
                            Some(icons::LOCK),
                            tr!("sources-unlock"),
                            Kind::Secondary,
                            44.0,
                        )
                        .clicked()
                        {
                            v.state.pin_form = Some(PinForm {
                                source: c.id().to_string(),
                                name: c.name().to_string(),
                                downloads: true,
                                ..Default::default()
                            });
                        }
                    } else {
                        open = widgets::button_sized(
                            ui,
                            Some(icons::FOLDER_OPEN),
                            tr!("downloads-open"),
                            Kind::Secondary,
                            44.0,
                        )
                        .clicked();
                    }
                });
            });
        });
    open
}

fn load(v: &View, source: &str) -> Vec<Row> {
    v.services
        .library
        .downloads_of(source)
        .unwrap_or_default()
        .into_iter()
        .map(|(record, download)| {
            let kept = download.location_of(crate::downloads::PICTURE);
            let picture = crate::downloads::path_of(&kept)
                .is_some_and(|p| p.is_file())
                .then_some(kept)
                .or_else(|| record.picture());
            Row {
                record,
                download,
                picture,
            }
        })
        .collect()
}

/// One source's downloads, newest first.
fn source_page(
    ui: &mut egui::Ui,
    v: &mut View,
    trail: &[String],
    source: &str,
    d: &mut DownloadsView,
) -> Option<page::Go> {
    let generation = v.services.downloads.generation();
    if d.rows.as_ref().is_none_or(|(g, _)| *g != generation) {
        d.rows = Some((generation, load(v, source)));
    }
    let go = page::trail_header(ui, trail, None, |_| {});
    let rows = d
        .rows
        .as_ref()
        .map(|(_, r)| r.as_slice())
        .unwrap_or_default();
    if rows.is_empty() {
        page::empty_state(
            ui,
            icons::DOWNLOAD_SIMPLE,
            tr!("downloads-empty"),
            tr!("downloads-empty-desc"),
            |_| {},
        );
        return go;
    }
    egui::ScrollArea::vertical()
        .auto_shrink(false)
        .show_list(ui, |ui| {
            for row in rows {
                download_row(ui, v, row);
                ui.add_space(8.0);
            }
        });
    go
}

/// Where a download is: on the headset, or a card, inserted or not.
fn place(d: &Download) -> String {
    let Some(rest) = d.folder.strip_prefix(fp_core::volume::SCHEME) else {
        return tr!("downloads-on-headset").to_string();
    };
    let id = rest.split('/').next().unwrap_or_default();
    match fp_core::volume::mounted()
        .into_iter()
        .find(|m| m.id.as_deref() == Some(id))
    {
        Some(card) => tr!("downloads-on-card", card = card.name()),
        None => tr!("downloads-card-out").to_string(),
    }
}

/// How a download is going: done, under way, waiting or stopped.
fn progress_text(v: &View, d: &Download) -> (String, bool) {
    match d.state {
        DownloadState::Done => (fmt_size(d.bytes()), false),
        DownloadState::Failed => (
            tr!(
                "download-failed",
                error = d.error.as_deref().unwrap_or_default()
            ),
            true,
        ),
        DownloadState::Waiting => {
            let downloads = &v.services.downloads;
            let text = if let Some(p) = downloads.progress().filter(|p| p.media_id == d.media_id) {
                let mut parts = vec![under_way(&p)];
                parts.push(match p.size {
                    Some(size) => tr!(
                        "download-of",
                        done = fmt_size(p.received),
                        size = fmt_size(size)
                    ),
                    None => fmt_size(p.received),
                });
                if p.speed > 0.0 {
                    parts.push(tr!("download-speed", speed = fmt_size(p.speed as u64)));
                }
                parts.join(" · ")
            } else if v.playback.as_ref().is_some_and(|p| p.streams) {
                tr!("download-waits-streaming").to_string()
            } else if let Some(error) = downloads.retrying(d.media_id) {
                tr!("download-retrying", error = error)
            } else {
                tr!("download-waiting").to_string()
            };
            (text, false)
        }
    }
}

/// The size of a download's picture.
const PICTURE: Vec2 = Vec2::new(192.0, 108.0);

fn download_row(ui: &mut egui::Ui, v: &mut View, row: &Row) {
    let d = &row.download;
    let (state, failed) = progress_text(v, d);
    let place = place(d);
    let here = crate::downloads::path_of(&d.video_location()).is_some();
    widgets::card(ui, |ui| {
        egui::Frame::new()
            .inner_margin(egui::Margin::same(12))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 18.0;
                    let (rect, _) = ui.allocate_exact_size(PICTURE, Sense::hover());
                    page::picture(
                        ui,
                        v.thumbs,
                        row.picture.as_deref(),
                        rect,
                        10,
                        icons::FILM_STRIP,
                    );
                    ui.vertical(|ui| {
                        ui.spacing_mut().item_spacing.y = 4.0;
                        ui.add_space(8.0);
                        ui.label(
                            RichText::new(widgets::display_title(&row.record.title))
                                .font(theme::font(Weight::SemiBold, 19.0))
                                .color(theme::TEXT),
                        );
                        ui.label(
                            RichText::new(&state)
                                .font(theme::font(Weight::Regular, 15.0))
                                .color(if failed { theme::ERROR } else { theme::TEXT_2 }),
                        );
                        ui.label(
                            RichText::new(&place)
                                .font(theme::font(Weight::Regular, 15.0))
                                .color(theme::TEXT_3),
                        );
                    });
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        ui.spacing_mut().item_spacing.x = 10.0;
                        let (idle, icon) = if d.state == DownloadState::Done {
                            (tr!("download-remove"), icons::TRASH)
                        } else {
                            (tr!("download-cancel"), icons::X_CIRCLE)
                        };
                        let mut parts = Parts {
                            state: v.state,
                            services: v.services,
                            actions: v.actions,
                        };
                        armed_remove(ui, &mut parts, d, idle, icon);
                        if d.state == DownloadState::Failed
                            && widgets::button(
                                ui,
                                Some(icons::ARROW_CLOCKWISE),
                                tr!("download-retry"),
                                Kind::Secondary,
                            )
                            .clicked()
                        {
                            v.actions.push(Action::RetryDownload(d.media_id));
                        }
                        if d.state == DownloadState::Done
                            && here
                            && widgets::button(
                                ui,
                                Some(icons::PLAY),
                                tr!("player-play"),
                                Kind::Primary,
                            )
                            .clicked()
                        {
                            v.actions.push(super::library::open_record(&row.record));
                        }
                    });
                });
            });
    });
}

/// What the Download button needs of the view: few enough fields that a
/// page can lend them while it holds the rest.
pub(super) struct Parts<'a> {
    pub state: &'a mut super::UiState,
    pub services: &'a crate::services::Services,
    pub actions: &'a mut Vec<Action>,
}

/// Whether the video at `location` of `source_id` may be downloaded,
/// asking its server once in the background when only it can tell.
fn allowed(p: &mut Parts, source_id: &str, location: &str) -> bool {
    if p.services.opener.source(source_id).is_none() {
        return false;
    }
    if let Some(allowed) = p.services.opener.download_allowed_in(source_id, location) {
        return allowed;
    }
    let key = (source_id.to_string(), location.to_string());
    match p.state.download_checks.get(&key) {
        Some(ServerCheck::Answered(allowed)) => *allowed,
        Some(ServerCheck::Asking) => false,
        Some(ServerCheck::Failed(at)) if at.elapsed() < super::CHECK_RETRY => false,
        _ => {
            p.state.download_checks.insert(key, ServerCheck::Asking);
            p.actions.push(Action::CheckDownload {
                source_id: source_id.to_string(),
                location: location.to_string(),
            });
            false
        }
    }
}

/// "Downloading 42 %", or "Downloading…" when the size isn't known.
fn under_way(p: &crate::downloads::Progress) -> String {
    match p.fraction() {
        Some(f) => {
            let percent = (f * 100.0).floor() as u32;
            tr!("download-progress", percent = percent)
        }
        None => tr!("download-under-way").to_string(),
    }
}

/// A video's Download button; once pressed, how its download is going,
/// with Cancel download or Remove download, which ask again, or Retry.
pub(super) fn button(ui: &mut egui::Ui, mut p: Parts, req: &OpenRequest, title: &str) {
    let library = &p.services.library;
    // Its library row has the location its source keeps for it.
    let stable = req
        .source_id
        .as_deref()
        .and_then(|s| p.services.opener.source(s))
        .and_then(|s| s.canonical_location(&req.location));
    let record = [Some(req.location.clone()), stable]
        .into_iter()
        .flatten()
        .find_map(|l| library.get_by_location(&l).ok().flatten());
    let download = record
        .as_ref()
        .and_then(|r| library.download(r.id).ok().flatten());
    let Some(d) = download else {
        if p.state.download_asked.contains(&req.location) {
            ui.add_enabled_ui(false, |ui| {
                widgets::button(
                    ui,
                    Some(icons::DOWNLOAD_SIMPLE),
                    tr!("download-starting"),
                    Kind::Secondary,
                )
            });
            return;
        }
        let source = req
            .source_id
            .clone()
            .or_else(|| fp_sources::location_owner(&req.location))
            .or_else(|| record.as_ref().map(|r| r.source_id.clone()));
        let Some(source) = source.filter(|s| !s.is_empty()) else {
            return;
        };
        if allowed(&mut p, &source, &req.location)
            && widgets::button(
                ui,
                Some(icons::DOWNLOAD_SIMPLE),
                tr!("download"),
                Kind::Secondary,
            )
            .tip(tr!("download-tip"))
            .clicked()
        {
            p.state.download_asked.insert(req.location.clone());
            p.actions.push(Action::Download {
                req: OpenRequest {
                    source_id: Some(source),
                    ..req.clone()
                },
                title: title.to_string(),
            });
        }
        return;
    };
    match d.state {
        DownloadState::Failed => {
            if widgets::button(
                ui,
                Some(icons::ARROW_CLOCKWISE),
                tr!("download-retry-video"),
                Kind::Secondary,
            )
            .tip(&tr!(
                "download-failed",
                error = d.error.as_deref().unwrap_or_default()
            ))
            .clicked()
            {
                p.actions.push(Action::RetryDownload(d.media_id));
            }
        }
        DownloadState::Waiting => {
            let label = match p.services.downloads.progress() {
                Some(progress) if progress.media_id == d.media_id => under_way(&progress),
                _ => tr!("download-waiting").to_string(),
            };
            armed_remove(ui, &mut p, &d, &label, icons::DOWNLOAD_SIMPLE);
        }
        DownloadState::Done => {
            armed_remove(ui, &mut p, &d, tr!("download-remove"), icons::TRASH);
        }
    }
}

/// `idle`, which pressed once asks to remove the download, and pressed
/// again does.
fn armed_remove(ui: &mut egui::Ui, p: &mut Parts, d: &Download, idle: &str, icon: &str) {
    let key = d.media_id.to_string();
    let before = p
        .state
        .download_armed
        .as_ref()
        .filter(|(k, _)| *k == key)
        .map(|&(_, at)| at);
    let mut at = before;
    let confirm = if d.state == DownloadState::Done {
        tr!("download-remove-confirm")
    } else {
        tr!("download-cancel-confirm")
    };
    let confirmed = super::confirm_button(ui, &mut at, idle, confirm, |ui, label, kind| {
        widgets::button(ui, Some(icon), label, kind)
    });
    if at != before {
        p.state.download_armed = at.map(|at| (key, at));
    }
    if confirmed {
        p.actions.push(Action::RemoveDownload(d.media_id));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_summary_counts_videos_size_and_those_under_way() {
        assert_eq!(summary(&DownloadCount::default()), "No downloads");
        let c = DownloadCount {
            videos: 3,
            unfinished: 1,
            bytes: 12_400_000_000,
        };
        assert_eq!(summary(&c), "3 downloads · 12.4 GB · 1 not finished");
        let one = DownloadCount {
            videos: 1,
            unfinished: 0,
            bytes: 2_000_000,
        };
        assert_eq!(summary(&one), "1 download · 2 MB");
        let counts = HashMap::from([("a".to_string(), c), ("b".to_string(), one)]);
        assert_eq!(
            total(&counts),
            DownloadCount {
                videos: 4,
                unfinished: 1,
                bytes: 12_402_000_000
            }
        );
    }
}
