//! Playlists: the tab listing them, a playlist's own page, and the picker
//! that adds videos to one.

use super::page::{self, Card};
use super::theme::{self, Weight};
use super::widgets::{self, Kind};
use super::{Action, View, icons};
use egui::RichText;
use fp_library::{MediaRecord, Playlist, PlaylistId};

/// The Playlists tab: your playlists, or the one open.
pub fn page(ui: &mut egui::Ui, v: &mut View) {
    if let Some(id) = v.state.playlist {
        playlist(ui, v, id);
        return;
    }
    ui.add_space(12.0);
    ui.label(
        RichText::new(tr!("nav-playlists"))
            .font(theme::font(Weight::Bold, 30.0))
            .color(theme::TEXT),
    );
    ui.add_space(10.0);
    new_playlist(ui, v, &[]);
    ui.add_space(14.0);
    let lists = playlists(v);
    if lists.is_empty() {
        page::empty_state(
            ui,
            icons::PLAYLIST,
            tr!("playlists-none"),
            tr!("playlists-none-desc"),
            |_| {},
        );
        return;
    }
    if let Some(id) = cards(ui, v, &lists) {
        v.state.playlist = Some(id);
        v.state.renaming = false;
    }
}

/// The playlists, read again after a change.
fn playlists(v: &mut View) -> Vec<Playlist> {
    v.state
        .playlists
        .get_or_insert_with(|| v.services.library.playlists().unwrap_or_default())
        .clone()
}

/// A grid of playlists; the one clicked.
fn cards(ui: &mut egui::Ui, v: &mut View, lists: &[Playlist]) -> Option<PlaylistId> {
    let mut picked = None;
    page::scroll(ui, |ui| {
        page::grid(ui, lists.len(), page::LANDSCAPE.x, |ui, i| {
            let p = &lists[i];
            let thumb = p.first.as_ref().and_then(MediaRecord::picture);
            let facts = tr!("count-videos", count = p.item_count);
            let c = page::card(
                ui,
                v.thumbs,
                &Card {
                    thumb: thumb.as_deref(),
                    placeholder: icons::PLAYLIST,
                    tint: theme::ACCENT_HOVER,
                    title: &p.name,
                    facts: &facts,
                    play: false,
                    ..Card::default()
                },
            );
            if c.open {
                picked = Some(p.id);
            }
        });
    });
    picked
}

/// A name field and the button making a playlist of `reqs` (none: empty).
fn new_playlist(ui: &mut egui::Ui, v: &mut View, reqs: &[crate::playback::OpenRequest]) {
    ui.horizontal(|ui| {
        ui.add(
            egui::TextEdit::singleline(&mut v.state.playlist_name)
                .margin(egui::Margin::symmetric(12, 9))
                .hint_text(tr!("playlists-new"))
                .font(theme::font(Weight::Regular, 17.0))
                .desired_width(360.0),
        );
        let name = v.state.playlist_name.trim().to_string();
        let label = if reqs.is_empty() {
            tr!("playlists-make")
        } else {
            tr!("playlists-make-and-add")
        };
        if ui
            .add_enabled_ui(!name.is_empty(), |ui| {
                widgets::button_sized(ui, Some(icons::PLUS), label, Kind::Primary, 44.0)
            })
            .inner
            .clicked()
        {
            v.actions.push(Action::AddToPlaylist {
                playlist: None,
                name,
                reqs: reqs.to_vec(),
            });
        }
    });
}

/// A playlist's page: its videos in order, played through from the one
/// clicked, with moving, taking out, renaming and deleting.
fn playlist(ui: &mut egui::Ui, v: &mut View, id: PlaylistId) {
    let Some(p) = playlists(v).into_iter().find(|p| p.id == id) else {
        v.state.playlist = None;
        return;
    };
    if v.state
        .playlist_items
        .as_ref()
        .is_none_or(|(i, _)| *i != id)
    {
        let items = v.services.library.playlist_items(id).unwrap_or_default();
        v.state.playlist_items = Some((id, items));
    }
    let entries = v
        .state
        .playlist_items
        .as_ref()
        .map(|(_, e)| e.clone())
        .unwrap_or_default();
    let reqs: Vec<_> = entries
        .iter()
        .map(|e| super::library::request(&e.record))
        .collect();
    let armed = v
        .state
        .delete_armed
        .is_some_and(|t| t.elapsed().as_secs_f32() < 4.0);
    let facts = tr!("count-videos", count = entries.len());
    let back = page::header(ui, &p.name, Some(&facts), |ui| {
        let (label, kind) = if armed {
            (tr!("playlists-delete-confirm"), Kind::Danger)
        } else {
            (tr!("playlists-delete"), Kind::Secondary)
        };
        if widgets::button_sized(ui, Some(icons::TRASH), label, kind, 44.0).clicked() {
            if armed {
                v.actions.push(Action::DeletePlaylist(id));
                v.state.delete_armed = None;
            } else {
                v.state.delete_armed = Some(std::time::Instant::now());
            }
        }
        if armed {
            ui.ctx()
                .request_repaint_after(std::time::Duration::from_millis(500));
        }
        if widgets::button_sized(
            ui,
            Some(icons::PENCIL_SIMPLE),
            tr!("playlists-rename"),
            Kind::Secondary,
            44.0,
        )
        .clicked()
        {
            v.state.renaming = true;
            v.state.playlist_name = p.name.clone();
        }
        if !reqs.is_empty()
            && widgets::button_sized(
                ui,
                Some(icons::PLAY),
                tr!("common-play-all"),
                Kind::Primary,
                44.0,
            )
            .clicked()
        {
            v.actions.push(Action::PlayAll(reqs.clone(), 0));
        }
    });
    if back {
        v.state.playlist = None;
        v.state.renaming = false;
        return;
    }
    if v.state.renaming {
        ui.horizontal(|ui| {
            ui.add(
                egui::TextEdit::singleline(&mut v.state.playlist_name)
                    .margin(egui::Margin::symmetric(12, 9))
                    .font(theme::font(Weight::Regular, 17.0))
                    .desired_width(360.0),
            );
            let name = v.state.playlist_name.trim().to_string();
            if ui
                .add_enabled_ui(!name.is_empty(), |ui| {
                    widgets::button_sized(
                        ui,
                        Some(icons::CHECK),
                        tr!("common-save"),
                        Kind::Primary,
                        44.0,
                    )
                })
                .inner
                .clicked()
            {
                v.actions.push(Action::RenamePlaylist(id, name));
                v.state.renaming = false;
                v.state.playlist_name.clear();
            }
            if widgets::button_sized(ui, None, tr!("common-cancel"), Kind::Ghost, 44.0).clicked() {
                v.state.renaming = false;
                v.state.playlist_name.clear();
            }
        });
        ui.add_space(8.0);
    }
    if entries.is_empty() {
        page::empty_state(
            ui,
            icons::PLAYLIST,
            tr!("playlists-empty"),
            tr!("playlists-empty-desc"),
            |_| {},
        );
        return;
    }
    let n = entries.len();
    page::scroll(ui, |ui| {
        ui.add_space(4.0);
        for (i, e) in entries.iter().enumerate() {
            let thumb = e.record.picture();
            let facts = super::library::facts(&e.record, v.settings.default_format);
            let resp = page::list_row(
                ui,
                v.thumbs,
                thumb.as_deref(),
                &widgets::display_title(&e.record.title),
                &facts,
                false,
                (i == 0, i + 1 == n),
            );
            // Positions count entries hidden by a lock, so a move lands
            // beside the neighbour shown.
            if resp.open {
                v.actions.push(Action::PlayAll(reqs.clone(), i));
            } else if resp.up {
                v.actions.push(Action::PlaylistMove(
                    id,
                    e.position,
                    entries[i - 1].position,
                ));
            } else if resp.down {
                v.actions.push(Action::PlaylistMove(
                    id,
                    e.position,
                    entries[i + 1].position,
                ));
            } else if resp.remove {
                v.actions.push(Action::PlaylistRemove(id, e.position));
            }
            ui.add_space(4.0);
        }
    });
}

/// A folder row's ⋯: the video, what can be done with it, and the
/// playlists to add it to. Otherwise adding videos to a playlist: a new one,
/// or one of yours, a single video with Play, Play next and Add to queue too.
pub fn picker(ui: &mut egui::Ui, v: &mut View) {
    let Some(picking) = v.state.picking.take() else {
        return;
    };
    let one_video = picking.from_row && picking.reqs.len() == 1;
    let back = if one_video {
        let name = &picking.title;
        let stem = match name.rsplit_once('.') {
            Some((stem, _)) if fp_core::source::is_video_name(name) => stem,
            _ => name,
        };
        let source = picking.reqs[0]
            .source_id
            .as_deref()
            .and_then(|id| v.services.opener.source(id))
            .map(|s| s.name().to_string());
        page::header(ui, &widgets::display_title(stem), source.as_deref(), |_| {})
    } else {
        let subtitle = match picking.reqs.len() {
            1 => widgets::display_title(&picking.title),
            n => format!("{}: {}", picking.title, tr!("count-videos", count = n)),
        };
        page::header(ui, tr!("playlists-picker-title"), Some(&subtitle), |_| {})
    };
    if back {
        return;
    }
    let (from_row, title) = (picking.from_row, picking.title.clone());
    v.state.picking = Some(picking);
    let reqs = v
        .state
        .picking
        .as_ref()
        .map(|p| p.reqs.clone())
        .unwrap_or_default();
    if let [req] = reqs.as_slice() {
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 12.0;
            if widgets::button(ui, Some(icons::PLAY), tr!("player-play"), Kind::Primary).clicked() {
                v.actions.push(Action::Open(req.clone()));
                v.state.picking = None;
            }
            if widgets::button(
                ui,
                Some(icons::SKIP_FORWARD),
                tr!("jf-play-next"),
                Kind::Secondary,
            )
            .clicked()
            {
                v.actions.push(Action::Queue {
                    req: req.clone(),
                    next: true,
                });
                v.state.picking = None;
            }
            if widgets::button(
                ui,
                Some(icons::QUEUE),
                tr!("jf-add-to-queue"),
                Kind::Secondary,
            )
            .clicked()
            {
                v.actions.push(Action::Queue {
                    req: req.clone(),
                    next: false,
                });
                v.state.picking = None;
            }
            // A folder row's video: its file can be downloaded, or deleted.
            if from_row {
                let parts = super::downloads::Parts {
                    state: v.state,
                    services: v.services,
                    actions: v.actions,
                };
                super::downloads::button(ui, parts, req, &title);
            }
            if from_row
                && super::can_delete_entry(v, req.source_id.as_deref(), &req.location)
                && super::delete_button(ui, v.state, &req.location)
            {
                v.actions.push(Action::DeleteFile(super::DeleteTarget {
                    title: title.clone(),
                    location: req.location.clone(),
                    source_id: req.source_id.clone(),
                    catalog_item_id: None,
                }));
            }
        });
        ui.add_space(18.0);
    }
    if one_video {
        widgets::section_label(ui, tr!("playlists-picker-title"));
        ui.add_space(6.0);
    }
    new_playlist(ui, v, &reqs);
    ui.add_space(14.0);
    let lists = playlists(v);
    if !lists.is_empty() {
        widgets::section_label(ui, tr!("playlists-or-add-to"));
        ui.add_space(6.0);
        if let Some(id) = cards(ui, v, &lists) {
            v.actions.push(Action::AddToPlaylist {
                playlist: Some(id),
                name: String::new(),
                reqs,
            });
        }
    }
}
