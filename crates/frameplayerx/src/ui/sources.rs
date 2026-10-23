//! Sources: the list, the browser each kind gets, adding one, PINs.

use super::page::{self, Card};
use super::theme::{self, Weight};
use super::widgets::{self, Kind, Tip};
use super::{Action, Back, PinForm, QuickConnect, Screen, SourceForm, SourcePage, View, icons};
use egui::{Align, Align2, Color32, Layout, RichText, Sense, Vec2};
use fp_sources::{Credentials, SourceConfig, SourceKind, urlutil};

pub(super) fn kind_icon(kind: SourceKind) -> &'static str {
    match kind {
        SourceKind::Local => icons::HARD_DRIVE,
        SourceKind::Library => icons::BOOKS,
        SourceKind::Http => icons::GLOBE,
        SourceKind::WebDav => icons::CLOUD,
        SourceKind::Dlna => icons::BROADCAST,
        SourceKind::DeoVr | SourceKind::HereSphere => icons::VIRTUAL_REALITY,
        SourceKind::MediaBrowser => icons::MONITOR_PLAY,
        SourceKind::Smb => icons::SHARE_NETWORK,
    }
}

/// What the headset itself holds, as the Sources list and Home name it.
pub(super) const LOCAL_NAME: &str = "On this device";
const LOCAL_DESC: &str = "Folders on the headset, its microSD card and USB drives";

/// A page title with an optional line under it and actions on the right.
fn page_header(
    ui: &mut egui::Ui,
    title: &str,
    subtitle: Option<&str>,
    actions: impl FnOnce(&mut egui::Ui),
) {
    ui.add_space(12.0);
    ui.horizontal(|ui| {
        ui.vertical(|ui| {
            ui.spacing_mut().item_spacing.y = 2.0;
            ui.label(
                RichText::new(title)
                    .font(theme::font(Weight::Bold, 30.0))
                    .color(theme::TEXT),
            );
            if let Some(s) = subtitle {
                ui.label(RichText::new(s).color(theme::TEXT_2));
            }
        });
        ui.with_layout(Layout::right_to_left(Align::Center), actions);
    });
    ui.add_space(14.0);
}

/// A rounded square holding an icon, the leading mark of a list row.
pub(super) fn icon_tile(ui: &mut egui::Ui, icon: &str, size: f32, color: Color32) {
    let (rect, _) = ui.allocate_exact_size(Vec2::splat(size), Sense::hover());
    ui.painter()
        .rect_filled(rect, egui::CornerRadius::same(12), theme::SURFACE_3);
    ui.painter().text(
        rect.center(),
        Align2::CENTER_CENTER,
        icon,
        theme::icon(size * 0.5),
        color,
    );
}

/// A source's PIN state on the Sources page.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Lock {
    /// No PIN (and the headset itself, which can't have one).
    None,
    Locked,
    Unlocked,
}

fn lock_of(v: &View, id: &str) -> Lock {
    match v.settings.source_pins.contains_key(id) {
        false => Lock::None,
        true if v.state.unlocked_sources.contains(id) => Lock::Unlocked,
        true => Lock::Locked,
    }
}

/// One row of the Sources list.
struct Row {
    id: String,
    name: String,
    desc: String,
    /// What Home's tile says under the name.
    facts: String,
    icon: &'static str,
    /// The headset itself: no PIN, can't be removed.
    builtin: bool,
    lock: Lock,
}

/// The headset first, then the sources the user added.
fn rows(v: &View) -> Vec<Row> {
    let local = Row {
        id: crate::services::LOCAL_HUB.into(),
        name: LOCAL_NAME.into(),
        desc: LOCAL_DESC.into(),
        facts: "Folders, microSD and USB".into(),
        icon: icons::HARD_DRIVES,
        builtin: true,
        lock: Lock::None,
    };
    std::iter::once(local)
        .chain(v.services.source_configs.iter().map(|c| {
            let lock = lock_of(v, c.id());
            let kind = match c {
                SourceConfig::Library(l) if !crate::libraries::present(l) => {
                    "Library · Not inserted"
                }
                _ => c.kind().label(),
            };
            // The list's line and Home's; a locked source's address can say
            // what is in it, so it shows neither.
            let (desc, facts) = match lock {
                Lock::Locked => (
                    format!("{kind} · Locked with a PIN"),
                    "Locked with a PIN".to_string(),
                ),
                Lock::Unlocked => (
                    format!("{kind} · {} · Unlocked", c.address()),
                    format!("{kind} · Unlocked"),
                ),
                Lock::None => (format!("{kind} · {}", c.address()), kind.to_string()),
            };
            Row {
                id: c.id().to_string(),
                name: c.name().to_string(),
                desc,
                facts,
                icon: kind_icon(c.kind()),
                builtin: false,
                lock,
            }
        }))
        .collect()
}

pub fn sources(ui: &mut egui::Ui, v: &mut View) {
    if v.state.pin_form.is_some() {
        pin_form(ui, v);
        return;
    }
    if v.state.source_form.is_some() {
        // Editing adds the PIN rows: taller than the panel.
        egui::ScrollArea::vertical()
            .auto_shrink(false)
            .show(ui, |ui| add_form(ui, v));
        return;
    }
    if v.state.source.is_some() {
        browse(ui, v);
        return;
    }
    page_header(
        ui,
        "Sources",
        Some("Browse the headset and your servers. What you watch is kept in Watch history."),
        |ui| {
            if widgets::button(ui, Some(icons::PLUS), "Add source", Kind::Primary).clicked() {
                v.state.source_form = Some(SourceForm::default());
            }
        },
    );
    egui::ScrollArea::vertical().show(ui, |ui| {
        widgets::section_label(ui, "On this device and your network");
        ui.add_space(4.0);
        widgets::card(ui, |ui| {
            for (i, row) in rows(v).into_iter().enumerate() {
                if i > 0 {
                    let x = ui.max_rect().x_range();
                    ui.painter().hline(
                        (x.min + 84.0)..=(x.max - 16.0),
                        ui.cursor().top(),
                        egui::Stroke::new(1.0_f32, theme::STROKE),
                    );
                }
                list_row(ui, v, row);
            }
        });
    });
}

fn list_row(ui: &mut egui::Ui, v: &mut View, row: Row) {
    let Row {
        id,
        name,
        desc,
        icon,
        builtin,
        lock,
        ..
    } = row;
    egui::Frame::new()
        .inner_margin(egui::Margin::symmetric(16, 12))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 16.0;
                icon_tile(ui, icon, 52.0, theme::ACCENT_HOVER);
                ui.vertical(|ui| {
                    ui.spacing_mut().item_spacing.y = 2.0;
                    ui.label(
                        RichText::new(&name)
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
                    if lock == Lock::Locked {
                        if widgets::button_sized(
                            ui,
                            Some(icons::LOCK),
                            "Unlock",
                            Kind::Secondary,
                            44.0,
                        )
                        .clicked()
                        {
                            v.state.pin_form = Some(PinForm {
                                source: id.clone(),
                                name: name.clone(),
                                ..Default::default()
                            });
                        }
                        return;
                    }
                    if !builtin {
                        if widgets::icon_button(ui, icons::TRASH, 44.0, false)
                            .tip("Remove (and forget its unwatched videos)")
                            .clicked()
                        {
                            v.actions.push(Action::RemoveSource(id.clone()));
                        }
                        if widgets::icon_button(ui, icons::PENCIL_SIMPLE, 44.0, false)
                            .tip("Edit")
                            .clicked()
                        {
                            edit(v, &id);
                        }
                        let tip = if lock == Lock::Unlocked {
                            "Lock"
                        } else {
                            "Lock with a PIN"
                        };
                        if widgets::icon_button(ui, icons::LOCK_OPEN, 44.0, false)
                            .tip(tip)
                            .clicked()
                        {
                            lock_source(v, &id, &name);
                        }
                    }
                    if widgets::button_sized(
                        ui,
                        Some(icons::FOLDER_OPEN),
                        "Browse",
                        Kind::Secondary,
                        44.0,
                    )
                    .clicked()
                    {
                        v.actions.push(Action::OpenSource {
                            source: id.clone(),
                            back_to: Screen::Sources,
                        });
                    }
                });
            });
        });
}

/// The source being browsed, in the browser its kind gets.
fn browse(ui: &mut egui::Ui, v: &mut View) {
    let Some(mut sv) = v.state.source.take() else {
        return;
    };
    let kind = v
        .services
        .source_config(&sv.source)
        .map(|c| c.kind().label())
        .unwrap_or("Folder");
    let back = match &mut sv.page {
        SourcePage::Folders(b) => super::folders::page(ui, v, &sv.name, kind, b),
        SourcePage::Local(l) => super::local::page(ui, v, l),
        SourcePage::Library(l) => super::local::library_page(ui, v, &sv.source, &sv.name, l),
        SourcePage::Jellyfin(j) => super::jellyfin::page(ui, v, &sv.source, &sv.name, j),
        SourcePage::Feed(f) => super::feeds::page(ui, v, &sv.source, &sv.name, f),
    };
    if back {
        match sv.back() {
            Back::Done => {}
            Back::Parent(a) => v.actions.push(*a),
            Back::Leave => {
                v.state.screen = sv.back_to;
                if sv.back_to == Screen::Home {
                    v.state.home = None;
                }
                return;
            }
        }
    }
    // Something on the page may have opened another source meanwhile.
    if v.state.source.is_none() {
        v.state.source = Some(sv);
    }
}

/// Locks a source with a PIN at once; one without a PIN gets the form to
/// choose one.
fn lock_source(v: &mut View, id: &str, name: &str) {
    if v.settings.source_pins.contains_key(id) {
        v.actions.push(Action::LockSource(id.to_string()));
    } else {
        v.state.screen = Screen::Sources;
        v.state.source = None;
        v.state.pin_form = Some(PinForm {
            source: id.to_string(),
            name: name.to_string(),
            set: true,
            ..Default::default()
        });
    }
}

/// Opens the form editing source `id`.
fn edit(v: &mut View, id: &str) {
    if let Some(cfg) = v.services.source_config(id) {
        v.state.source_form = Some(edit_form(cfg));
    }
}

/// Home's row of sources: each opens its browser, the last adds one.
/// Locked sources stay out of it until unlocked under Sources.
pub fn source_row(ui: &mut egui::Ui, v: &mut View, back_to: Screen) {
    let rows: Vec<Row> = rows(v)
        .into_iter()
        .filter(|r| r.lock != Lock::Locked)
        .collect();
    page::carousel(ui, "Sources", None, rows.len() + 1, |ui, i| {
        let Some(row) = rows.get(i) else {
            let c = page::card(
                ui,
                v.thumbs,
                &Card {
                    placeholder: icons::PLUS,
                    tint: theme::ACCENT_HOVER,
                    title: "Add a source",
                    facts: "A library, Jellyfin, DeoVR, SMB…",
                    play: false,
                    ..Card::default()
                },
            );
            if c.open {
                v.state.screen = Screen::Sources;
                v.state.source = None;
                v.state.source_form = Some(SourceForm::default());
            }
            return;
        };
        // A padlock to lock it: always there with a PIN set, on pointing
        // otherwise (it then asks for one).
        let tip = if row.lock == Lock::Unlocked {
            "Lock"
        } else {
            "Lock with a PIN"
        };
        let lockable = (!row.builtin).then_some((icons::LOCK_OPEN, tip));
        let c = page::card(
            ui,
            v.thumbs,
            &Card {
                placeholder: row.icon,
                tint: theme::ACCENT_HOVER,
                title: &row.name,
                facts: &row.facts,
                play: false,
                action: lockable,
                action_always: row.lock == Lock::Unlocked,
                ..Card::default()
            },
        );
        if c.action {
            lock_source(v, &row.id, &row.name);
        } else if c.open {
            v.actions.push(Action::OpenSource {
                source: row.id.clone(),
                back_to,
            });
        }
    });
}

/// Unlocks a source, or sets, changes or removes its PIN.
fn pin_form(ui: &mut egui::Ui, v: &mut View) {
    let has_pin = v
        .state
        .pin_form
        .as_ref()
        .is_some_and(|f| v.settings.source_pins.contains_key(&f.source));
    let Some(f) = v.state.pin_form.as_mut() else {
        return;
    };
    let mut close = false;
    ui.add_space(12.0);
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 12.0;
        if widgets::icon_button(ui, icons::ARROW_LEFT, 48.0, false)
            .tip("Cancel")
            .clicked()
        {
            close = true;
        }
        let title = match (f.set, has_pin) {
            (false, _) => format!("Unlock {}", f.name),
            (true, false) => format!("Lock {} with a PIN", f.name),
            (true, true) => format!("Change the PIN of {}", f.name),
        };
        ui.label(
            RichText::new(title)
                .font(theme::font(Weight::Bold, 30.0))
                .color(theme::TEXT),
        );
    });
    ui.add_space(8.0);
    let help = if f.set {
        "While it is locked, it can't be browsed, it is left off Home, and its videos \
         are hidden from the watch history, search and the remotes. The PIN unlocks \
         it until FramePlayerX restarts or you lock it again."
    } else {
        "Enter its PIN."
    };
    ui.label(RichText::new(help).color(theme::TEXT_2));
    ui.add_space(12.0);
    let field = |ui: &mut egui::Ui, value: &mut String| {
        let r = ui.add(
            egui::TextEdit::singleline(value)
                .margin(egui::Margin::symmetric(12, 9))
                .password(true)
                .char_limit(*crate::pin::LENGTH.end())
                .font(theme::font(Weight::Regular, 17.0))
                .desired_width(220.0),
        );
        value.retain(|c| c.is_ascii_digit());
        r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter))
    };
    let mut submit = false;
    widgets::rows(ui, |r| {
        let label = if f.set && has_pin { "New PIN" } else { "PIN" };
        r.row(label, Some("4 to 8 digits."), |ui| {
            submit |= field(ui, &mut f.pin);
        });
        if f.set {
            r.row("PIN again", None, |ui| {
                submit |= field(ui, &mut f.confirm);
            });
        }
    });
    ui.add_space(12.0);
    let valid = crate::pin::valid(&f.pin);
    let matches = !f.set || f.pin == f.confirm;
    ui.horizontal(|ui| {
        let label = if f.set { "Save PIN" } else { "Unlock" };
        let icon = if f.set { icons::LOCK } else { icons::LOCK_OPEN };
        let ok = valid && matches;
        if ui
            .add_enabled_ui(ok, |ui| {
                widgets::button(ui, Some(icon), label, Kind::Primary)
            })
            .inner
            .clicked()
            || (submit && ok)
        {
            v.actions.push(if f.set {
                Action::SetSourcePin {
                    source: f.source.clone(),
                    pin: Some(f.pin.clone()),
                }
            } else {
                Action::UnlockSource {
                    source: f.source.clone(),
                    pin: f.pin.clone(),
                }
            });
        }
        if f.wrong {
            ui.label(RichText::new("Wrong PIN").color(theme::WARN));
        } else if f.set && !matches && f.confirm.len() >= f.pin.len() && !f.confirm.is_empty() {
            ui.label(RichText::new("The PINs don't match").color(theme::WARN));
        }
    });
    if !f.pin.is_empty() {
        f.wrong = false;
    }
    if close {
        v.state.pin_form = None;
    }
}

const KINDS: [(SourceKind, &str); 8] = [
    (
        SourceKind::Library,
        "A folder on this device, a microSD card or a USB drive: its videos are found, with \
         thumbnails, filters and sorting, and it gets its own place on Home",
    ),
    (
        SourceKind::MediaBrowser,
        "Jellyfin / Emby server URL, e.g. http://192.168.1.10:8096",
    ),
    (
        SourceKind::DeoVr,
        "The URL of any server's DeoVR feed, e.g. http://192.168.1.10:9999/deovr",
    ),
    (
        SourceKind::HereSphere,
        "The URL of any server's HereSphere API, e.g. http://192.168.1.10:9999/heresphere",
    ),
    (
        SourceKind::WebDav,
        "WebDAV share URL (Nextcloud, NAS), e.g. https://nas.local/dav/videos/",
    ),
    (
        SourceKind::Http,
        "Web server folder listing, e.g. http://192.168.1.10:8080/videos/",
    ),
    (
        SourceKind::Smb,
        "Windows / Samba server or share, e.g. smb://nas.local/ or smb://nas.local/videos",
    ),
    (
        SourceKind::Dlna,
        "DLNA media server on your network (Plex, Jellyfin, Emby, Serviio)",
    ),
];

fn address_hint(kind: SourceKind) -> &'static str {
    match kind {
        SourceKind::Local | SourceKind::Library => "/home/steamos/Videos/Movies",
        SourceKind::MediaBrowser => "http://192.168.1.10:8096",
        SourceKind::DeoVr => "http://192.168.1.10:9999/deovr",
        SourceKind::HereSphere => "http://192.168.1.10:9999/heresphere",
        SourceKind::WebDav => "https://nas.local/dav/videos/",
        SourceKind::Http => "http://192.168.1.10:8080/videos/",
        SourceKind::Smb => "smb://nas.local/videos",
        SourceKind::Dlna => "",
    }
}

fn name_hint(kind: SourceKind) -> &'static str {
    match kind {
        SourceKind::MediaBrowser => "Optional (e.g. Jellyfin)",
        SourceKind::DeoVr => "Optional (e.g. DeoVR feed)",
        SourceKind::HereSphere => "Optional (e.g. HereSphere feed)",
        SourceKind::WebDav => "Optional (e.g. Nextcloud)",
        SourceKind::Http => "Optional (e.g. Video server)",
        SourceKind::Smb => "Optional (e.g. Home NAS)",
        SourceKind::Local | SourceKind::Library => "Optional (e.g. Movies)",
        SourceKind::Dlna => "Optional",
    }
}

fn add_form(ui: &mut egui::Ui, v: &mut View) {
    let dlna_found = v.state.dlna_found.clone();
    let searching = v.state.dlna_searching;
    let pins = v.settings.source_pins.clone();
    let configs = v.services.source_configs.clone();
    let Some(f) = v.state.source_form.as_mut() else {
        return;
    };
    let editing = f.editing.clone();
    let cancel = form_header(ui, f, editing.as_ref());
    let id = match &editing {
        Some(c) => c.id().to_string(),
        None => format!(
            "{}-{}",
            format!("{:?}", f.kind).to_lowercase(),
            fp_core::playback::now_ms()
        ),
    };
    let save = if let Some(SourceConfig::Dlna(old)) = &editing {
        dlna_name_form(ui, f, old)
    } else if f.kind == SourceKind::Dlna {
        dlna_search(ui, v.actions, &dlna_found, searching, &id)
    } else if f.kind == SourceKind::Library {
        library_form(ui, f, id, &configs, v.actions)
    } else {
        server_form(ui, f, id, editing.as_ref(), v.actions)
    };
    if let Some(old) = &editing {
        pin_rows(ui, v, old.id(), old.name(), pins.contains_key(old.id()));
    }
    if let Some(cfg) = save {
        v.actions.push(match editing {
            Some(_) => Action::UpdateSource(cfg),
            None => Action::AddSource(cfg),
        });
        v.state.source_form = None;
    } else if cancel {
        v.state.source_form = None;
    }
}

/// The form's title and its source type; true when cancelled.
fn form_header(ui: &mut egui::Ui, f: &mut SourceForm, editing: Option<&SourceConfig>) -> bool {
    let mut cancel = false;
    ui.add_space(12.0);
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 12.0;
        if widgets::icon_button(ui, icons::ARROW_LEFT, 48.0, false)
            .tip("Cancel")
            .clicked()
        {
            cancel = true;
        }
        let title = match editing {
            Some(c) => format!("Edit {}", c.name()),
            None => "Add a source".to_string(),
        };
        ui.label(
            RichText::new(title)
                .font(theme::font(Weight::Bold, 30.0))
                .color(theme::TEXT),
        );
    });
    ui.add_space(12.0);
    widgets::section_label(ui, "Type");
    ui.add_space(4.0);
    if editing.is_some() {
        // A source keeps its type; add another for a different one.
        ui.horizontal(|ui| {
            widgets::chip_icon(ui, Some(kind_icon(f.kind)), f.kind.label(), true);
        });
    } else {
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing = Vec2::new(8.0, 8.0);
            for (k, _) in KINDS {
                if widgets::chip_icon(ui, Some(kind_icon(k)), k.label(), f.kind == k).clicked() {
                    f.kind = k;
                }
            }
        });
    }
    let help = KINDS
        .iter()
        .find(|(k, _)| *k == f.kind)
        .map(|(_, h)| *h)
        .unwrap_or("");
    ui.add_space(4.0);
    ui.label(RichText::new(help).color(theme::TEXT_2));
    ui.add_space(12.0);
    cancel
}

/// A library's name and folder, with folders worth having one for, and
/// when editing, its videos' upkeep.
fn library_form(
    ui: &mut egui::Ui,
    f: &mut SourceForm,
    id: String,
    configs: &[SourceConfig],
    actions: &mut Vec<Action>,
) -> Option<SourceConfig> {
    let mut save = None;
    let editing = f.editing.is_some();
    widgets::rows(ui, |r| {
        r.row(
            "Name",
            Some("Optional; the folder's name otherwise."),
            |ui| text_field(ui, &mut f.name, name_hint(f.kind), false),
        );
        r.row("Folder", None, |ui| {
            text_field(ui, &mut f.address, address_hint(f.kind), false);
        });
        let suggestions = crate::libraries::suggestions(configs);
        if !suggestions.is_empty() {
            r.content(|ui| {
                ui.horizontal_wrapped(|ui| {
                    ui.spacing_mut().item_spacing = Vec2::new(8.0, 8.0);
                    for s in suggestions {
                        let text = s.display().to_string();
                        if widgets::chip_icon(ui, Some(icons::FOLDER_SIMPLE), &text, false)
                            .clicked()
                        {
                            f.address = text;
                        }
                    }
                });
            });
        }
    });
    ui.add_space(12.0);
    let built = build_config(f, id).and_then(|cfg| match &cfg {
        SourceConfig::Library(l) => match crate::libraries::overlap(l, configs) {
            Some(other) => Err(format!("This folder is in {other}, or holds it")),
            None => Ok(cfg),
        },
        _ => Ok(cfg),
    });
    ui.horizontal(|ui| match built {
        Ok(cfg) => {
            if widgets::button(ui, Some(icons::CHECK), "Save", Kind::Primary).clicked() {
                save = Some(cfg);
            }
        }
        Err(e) => {
            ui.add_enabled_ui(false, |ui| {
                widgets::button(ui, Some(icons::CHECK), "Save", Kind::Primary)
            });
            if !f.address.is_empty() {
                ui.label(RichText::new(e).color(theme::WARN));
            }
        }
    });
    if let Some(SourceConfig::Library(l)) = f.editing.as_ref().filter(|_| editing) {
        ui.add_space(20.0);
        widgets::section_label(ui, "Videos");
        ui.add_space(4.0);
        widgets::rows(ui, |r| {
            r.row(
                "Rebuild thumbnails",
                Some("Reads every video again."),
                |ui| {
                    if widgets::button_sized(ui, None, "Rebuild", Kind::Secondary, 44.0).clicked() {
                        actions.push(Action::Rescan {
                            source: l.id.clone(),
                            force: true,
                        });
                    }
                },
            );
            r.row("Retry videos that couldn't be read", None, |ui| {
                if widgets::button_sized(ui, None, "Retry", Kind::Secondary, 44.0).clicked() {
                    actions.push(Action::RetryFailed(l.id.clone()));
                }
            });
        });
    }
    save
}

/// A server found on the network: only its name is the user's.
fn dlna_name_form(
    ui: &mut egui::Ui,
    f: &mut SourceForm,
    old: &fp_sources::DlnaConfig,
) -> Option<SourceConfig> {
    let mut save = None;
    widgets::rows(ui, |r| {
        r.row("Name", Some(old.location.as_str()), |ui| {
            ui.add(
                egui::TextEdit::singleline(&mut f.name)
                    .margin(egui::Margin::symmetric(12, 9))
                    .font(theme::font(Weight::Regular, 17.0))
                    .desired_width(460.0),
            );
        });
    });
    ui.add_space(12.0);
    let name = f.name.trim();
    if ui
        .add_enabled_ui(!name.is_empty(), |ui| {
            widgets::button(ui, Some(icons::CHECK), "Save", Kind::Primary)
        })
        .inner
        .clicked()
    {
        let mut c = old.clone();
        c.name = name.to_string();
        save = Some(SourceConfig::Dlna(c));
    }
    save
}

/// Searching the network for DLNA servers, and adding one.
fn dlna_search(
    ui: &mut egui::Ui,
    actions: &mut Vec<Action>,
    found: &[fp_sources::DlnaDevice],
    searching: bool,
    id: &str,
) -> Option<SourceConfig> {
    let mut save = None;
    ui.horizontal(|ui| {
        if ui
            .add_enabled_ui(!searching, |ui| {
                widgets::button(
                    ui,
                    Some(icons::MAGNIFYING_GLASS),
                    "Search the network",
                    Kind::Primary,
                )
            })
            .inner
            .clicked()
        {
            actions.push(Action::DiscoverDlna);
        }
        if searching {
            ui.spinner();
        }
    });
    ui.add_space(8.0);
    if found.is_empty() {
        if !searching {
            ui.label(RichText::new("No servers found yet.").color(theme::TEXT_3));
        }
    } else {
        widgets::rows(ui, |r| {
            for d in found {
                r.row(&d.friendly_name, Some(d.manufacturer.as_str()), |ui| {
                    if widgets::button_sized(ui, Some(icons::PLUS), "Add", Kind::Secondary, 44.0)
                        .clicked()
                    {
                        save = Some(SourceConfig::Dlna(d.to_config(id.to_string())));
                    }
                });
            }
        });
    }
    save
}

/// The form for a server or folder: where it is and how to sign in.
fn server_form(
    ui: &mut egui::Ui,
    f: &mut SourceForm,
    id: String,
    editing: Option<&SourceConfig>,
    actions: &mut Vec<Action>,
) -> Option<SourceConfig> {
    let mut save = None;
    let local = f.kind == SourceKind::Local;
    let smb = f.kind == SourceKind::Smb;
    let is_mb = f.kind == SourceKind::MediaBrowser;
    if is_mb && f.use_quick_connect {
        poll_quick_connect(ui, f, actions);
    }
    widgets::rows(ui, |r| {
        r.row(
            "Name",
            Some("Optional; the server's name otherwise."),
            |ui| text_field(ui, &mut f.name, name_hint(f.kind), false),
        );
        r.row(if local { "Folder" } else { "Address" }, None, |ui| {
            text_field(ui, &mut f.address, address_hint(f.kind), false);
        });
        if is_mb {
            r.row(
                "Sign in with",
                Some("Jellyfin's Quick Connect pairs using a 6-digit code on another device."),
                |ui| {
                    if widgets::chip(ui, "Password", !f.use_quick_connect).clicked() {
                        f.use_quick_connect = false;
                    }
                    if widgets::chip(ui, "Quick Connect", f.use_quick_connect).clicked() {
                        f.use_quick_connect = true;
                    }
                },
            );
        }
        if !local {
            if is_mb && f.use_quick_connect {
                quick_connect_rows(r, f, actions);
            } else {
                r.row("User name", Some("If the server asks for one."), |ui| {
                    text_field(ui, &mut f.username, "", false);
                });
                r.row("Password", None, |ui| {
                    text_field(ui, &mut f.password, "", true);
                });
            }
            if !smb {
                r.switch(
                    "Accept self-signed certificates",
                    Some("For HTTPS servers on your own network."),
                    &mut f.insecure_tls,
                );
            }
        }
    });
    ui.add_space(12.0);
    let built = build_config(f, id).map(|cfg| match editing {
        Some(old) => keep_hidden(cfg, old),
        None => cfg,
    });
    ui.horizontal(|ui| match built {
        Ok(cfg) => {
            if widgets::button(ui, Some(icons::CHECK), "Save", Kind::Primary).clicked() {
                save = Some(cfg);
            }
        }
        Err(e) => {
            ui.add_enabled_ui(false, |ui| {
                widgets::button(ui, Some(icons::CHECK), "Save", Kind::Primary)
            });
            if !f.address.is_empty() {
                ui.label(RichText::new(e).color(theme::WARN));
            }
        }
    });
    if !is_mb || !f.use_quick_connect {
        ui.add_space(8.0);
        ui.label(
            RichText::new("Passwords are stored on this device only, in a file only you can read.")
                .font(theme::font(Weight::Regular, 15.0))
                .color(theme::TEXT_3),
        );
    }
    save
}

fn text_field(ui: &mut egui::Ui, value: &mut String, hint: &str, password: bool) {
    ui.add(
        egui::TextEdit::singleline(value)
            .margin(egui::Margin::symmetric(12, 9))
            .password(password)
            .hint_text(hint)
            .font(theme::font(Weight::Regular, 17.0))
            .desired_width(460.0),
    );
}

/// While a pairing code shows, asks the server every few seconds whether
/// it was approved.
fn poll_quick_connect(ui: &egui::Ui, f: &mut SourceForm, actions: &mut Vec<Action>) {
    if let QuickConnect::Waiting { secret, polled, .. } = &mut f.quick_connect {
        ui.ctx()
            .request_repaint_after(std::time::Duration::from_millis(500));
        if polled.elapsed() >= std::time::Duration::from_secs(2) {
            *polled = std::time::Instant::now();
            actions.push(Action::QuickConnectPoll {
                address: f.address.clone(),
                secret: secret.clone(),
                insecure_tls: f.insecure_tls,
            });
        }
    }
}

/// Signing in with Quick Connect: get a code, wait for it, or done.
fn quick_connect_rows(r: &mut widgets::Rows, f: &mut SourceForm, actions: &mut Vec<Action>) {
    let state = f.quick_connect.clone();
    if let QuickConnect::Authorized { .. } = state {
        r.row("Quick Connect", Some("Authorized with server."), |ui| {
            if widgets::button_sized(ui, None, "Clear", Kind::Secondary, 36.0).clicked() {
                f.quick_connect = QuickConnect::Idle;
            }
            ui.label(
                RichText::new(format!("{} Quick Connect authorized", icons::CHECK_CIRCLE))
                    .font(theme::font(Weight::SemiBold, 17.0))
                    .color(theme::OK),
            );
        });
    } else if let QuickConnect::Waiting { code, .. } = state {
        r.row(
            "Pairing code",
            Some("Enter this code in your Jellyfin or Emby app under Quick Connect:"),
            |ui| {
                if widgets::button_sized(ui, None, "Cancel", Kind::Secondary, 36.0).clicked() {
                    f.quick_connect = QuickConnect::Idle;
                }
                ui.label(RichText::new("Waiting for approval…").color(theme::TEXT_3));
                ui.spinner();
                let spaced: String = code
                    .chars()
                    .map(|c| c.to_string())
                    .collect::<Vec<_>>()
                    .join(" ");
                ui.label(
                    RichText::new(spaced)
                        .font(theme::font(Weight::Bold, 26.0))
                        .color(theme::ACCENT),
                );
            },
        );
    } else {
        let desc = if let QuickConnect::Failed(err) = &state {
            format!("Error: {err}")
        } else {
            "Pair without typing your password.".to_string()
        };
        let requesting = state == QuickConnect::Requesting;
        r.row("Quick Connect", Some(&desc), |ui| {
            let can_initiate = !f.address.trim().is_empty() && !requesting;
            if ui
                .add_enabled_ui(can_initiate, |ui| {
                    widgets::button(
                        ui,
                        Some(icons::KEY_RETURN),
                        "Get pairing code",
                        Kind::Secondary,
                    )
                })
                .inner
                .clicked()
            {
                f.quick_connect = QuickConnect::Requesting;
                actions.push(Action::QuickConnectInitiate {
                    address: f.address.clone(),
                    insecure_tls: f.insecure_tls,
                });
            }
            if requesting {
                ui.label(RichText::new("Connecting…").color(theme::TEXT_3));
                ui.spinner();
            }
        });
    }
}

/// The PIN part of the edit form: set one, change it or take it away.
fn pin_rows(ui: &mut egui::Ui, v: &mut View, id: &str, name: &str, has_pin: bool) {
    ui.add_space(20.0);
    widgets::section_label(ui, "PIN");
    ui.add_space(4.0);
    let desc = if has_pin {
        "Locked, it is left off Home and its videos are hidden until the PIN is entered."
    } else {
        "A PIN keeps it, and its videos, hidden from anyone else using the headset."
    };
    widgets::rows(ui, |r| {
        r.row(
            if has_pin {
                "Locked with a PIN"
            } else {
                "No PIN"
            },
            Some(desc),
            |ui| {
                let form = |set| PinForm {
                    source: id.to_string(),
                    name: name.to_string(),
                    set,
                    ..Default::default()
                };
                if has_pin {
                    if widgets::button_sized(
                        ui,
                        Some(icons::TRASH),
                        "Remove PIN",
                        Kind::Danger,
                        44.0,
                    )
                    .clicked()
                    {
                        v.actions.push(Action::SetSourcePin {
                            source: id.to_string(),
                            pin: None,
                        });
                    }
                    if widgets::button_sized(ui, None, "Change PIN", Kind::Secondary, 44.0)
                        .clicked()
                    {
                        v.state.pin_form = Some(form(true));
                    }
                } else if widgets::button_sized(
                    ui,
                    Some(icons::LOCK),
                    "Set a PIN",
                    Kind::Secondary,
                    44.0,
                )
                .clicked()
                {
                    v.state.pin_form = Some(form(true));
                }
            },
        );
    });
}

/// The form filled in from a saved source, to edit it.
pub fn edit_form(cfg: &SourceConfig) -> SourceForm {
    let mut f = SourceForm {
        editing: Some(cfg.clone()),
        kind: cfg.kind(),
        name: cfg.name().to_string(),
        use_quick_connect: false,
        ..SourceForm::default()
    };
    let mut creds = |c: &Option<Credentials>| {
        if let Some(c) = c {
            f.username = c.username.clone();
            f.password = c.password.clone();
        }
    };
    match cfg {
        SourceConfig::Local(c) => f.address = c.root.display().to_string(),
        SourceConfig::Library(c) => f.address = library_address(c),
        SourceConfig::Http(c) | SourceConfig::WebDav(c) => {
            creds(&c.credentials);
            f.address = c.url.clone();
            f.insecure_tls = c.insecure_tls;
        }
        SourceConfig::Dlna(_) => {}
        SourceConfig::DeoVr(c) | SourceConfig::HereSphere(c) => {
            creds(&c.credentials);
            f.address = c.url.clone();
            f.insecure_tls = c.insecure_tls;
        }
        SourceConfig::Smb(c) => {
            creds(&c.credentials);
            f.address = fp_sources::smb::smb_url(&c.host, c.port, &c.share, &c.path);
        }
        SourceConfig::MediaBrowser(c) => {
            creds(&c.credentials);
            f.address = c.url.clone();
            f.insecure_tls = c.insecure_tls;
            // Signed in with Quick Connect: kept until cleared.
            if c.credentials.is_none() {
                f.use_quick_connect = true;
                if let Some(token) = &c.access_token {
                    f.quick_connect = QuickConnect::Authorized {
                        token: token.clone(),
                        user_id: c.user_id.clone(),
                    };
                }
            }
        }
    }
    f
}

/// A library's folder as its form shows it: the path, or while its card is
/// out, the card's name and the folder on it.
fn library_address(c: &fp_sources::LibraryConfig) -> String {
    match fp_core::volume::resolve(&c.root()) {
        Some(path) => path.into_owned(),
        None => SourceConfig::Library(c.clone()).address(),
    }
}

/// What the form doesn't show, kept from the source being edited.
fn keep_hidden(mut new: SourceConfig, old: &SourceConfig) -> SourceConfig {
    match (&mut new, old) {
        (SourceConfig::DeoVr(n), SourceConfig::DeoVr(o))
        | (SourceConfig::HereSphere(n), SourceConfig::HereSphere(o)) => n.max_height = o.max_height,
        (SourceConfig::MediaBrowser(n), SourceConfig::MediaBrowser(o)) => {
            n.max_height = o.max_height;
        }
        _ => {}
    }
    new
}

/// Builds a source configuration from the form, validating the address.
pub fn build_config(f: &SourceForm, id: String) -> Result<SourceConfig, String> {
    let address = f.address.trim().to_string();
    if address.is_empty() {
        return Err("Enter an address".into());
    }
    let credentials = (!f.username.trim().is_empty())
        .then(|| Credentials::new(f.username.trim(), f.password.clone()));
    let default_name = |a: &str| {
        let host = a.split("://").nth(1).unwrap_or(a);
        host.split(['/', ':']).next().unwrap_or(host).to_string()
    };
    let name = if f.name.trim().is_empty() {
        default_name(&address)
    } else {
        f.name.trim().to_string()
    };
    let http_url = |a: &str| -> Result<String, String> {
        let url = urlutil::with_http_scheme(a.trim_end_matches('/'));
        if urlutil::is_http_url(&url) {
            Ok(url)
        } else {
            Err("Address must start with http:// or https://".into())
        }
    };
    Ok(match f.kind {
        SourceKind::Local | SourceKind::Library => {
            let path = std::path::Path::new(&address);
            let name = if f.name.trim().is_empty() {
                crate::libraries::default_name(path)
            } else {
                f.name.trim().to_string()
            };
            // Unchanged: kept as it is, also while its card is out.
            if let Some(SourceConfig::Library(old)) = &f.editing
                && address == library_address(old)
            {
                return Ok(SourceConfig::Library(fp_sources::LibraryConfig {
                    name,
                    ..old.clone()
                }));
            }
            SourceConfig::Library(crate::libraries::config_for(id, name, path)?)
        }
        SourceKind::Http | SourceKind::WebDav => {
            let c = fp_sources::HttpConfig {
                id,
                name,
                url: http_url(&address)?,
                credentials,
                insecure_tls: f.insecure_tls,
            };
            if f.kind == SourceKind::Http {
                SourceConfig::Http(c)
            } else {
                SourceConfig::WebDav(c)
            }
        }
        SourceKind::DeoVr | SourceKind::HereSphere => {
            let c = fp_sources::FeedConfig {
                id,
                name,
                url: http_url(&address)?,
                credentials,
                insecure_tls: f.insecure_tls,
                max_height: None,
            };
            if f.kind == SourceKind::DeoVr {
                SourceConfig::DeoVr(c)
            } else {
                SourceConfig::HereSphere(c)
            }
        }
        SourceKind::Smb => {
            let rest = address
                .strip_prefix("smb://")
                .unwrap_or(&address)
                .trim_start_matches(['/', '\\'])
                .replace('\\', "/");
            let mut parts = rest.splitn(3, '/');
            let hostport = parts.next().unwrap_or_default().to_string();
            let share = parts.next().unwrap_or_default().to_string();
            let path = parts
                .next()
                .unwrap_or_default()
                .trim_end_matches('/')
                .to_string();
            // No share: the whole server, which lists its shares.
            if hostport.is_empty() {
                return Err(
                    "Use smb://server/, smb://server/share or smb://server/share/folder".into(),
                );
            }
            let (host, port) = match hostport.rsplit_once(':') {
                Some((h, p)) => (
                    h.to_string(),
                    Some(p.parse::<u16>().map_err(|_| "Bad port".to_string())?),
                ),
                None => (hostport, None),
            };
            SourceConfig::Smb(fp_sources::SmbConfig {
                id,
                name,
                host,
                port,
                share,
                path,
                credentials,
            })
        }
        SourceKind::MediaBrowser => {
            let (credentials, access_token, user_id) = if f.use_quick_connect {
                match &f.quick_connect {
                    QuickConnect::Authorized { token, user_id } => {
                        (None, Some(token.clone()), user_id.clone())
                    }
                    QuickConnect::Waiting { .. } => {
                        return Err("Approve the code in your Jellyfin/Emby app".into());
                    }
                    _ => return Err("Click 'Get pairing code' to sign in".into()),
                }
            } else if credentials.is_none() {
                return Err("Enter username and password".into());
            } else {
                (credentials, None, None)
            };
            SourceConfig::MediaBrowser(fp_sources::MediaBrowserConfig {
                id,
                name,
                url: http_url(&address)?,
                credentials,
                insecure_tls: f.insecure_tls,
                max_height: None,
                access_token,
                user_id,
            })
        }
        SourceKind::Dlna => return Err("Pick a server found on the network".into()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn form_validation() {
        let mut f = SourceForm {
            kind: SourceKind::Smb,
            address: "smb://nas:1445/videos/vr".into(),
            ..Default::default()
        };
        match build_config(&f, "x".into()).unwrap() {
            SourceConfig::Smb(c) => {
                assert_eq!(
                    (c.host.as_str(), c.port, c.share.as_str(), c.path.as_str()),
                    ("nas", Some(1445), "videos", "vr")
                );
                assert_eq!(c.name, "nas");
            }
            other => panic!("{other:?}"),
        }
        // The whole server, by address or as Windows writes it.
        for address in ["smb://192.168.0.10/", "smb://192.168.0.10", "\\\\nas"] {
            f.address = address.into();
            match build_config(&f, "x".into()).unwrap() {
                SourceConfig::Smb(c) => {
                    assert!(c.share.is_empty() && c.path.is_empty(), "{address}: {c:?}");
                    assert!(!c.host.is_empty(), "{address}");
                }
                other => panic!("{other:?}"),
            }
        }
        f.address = "smb://".into();
        assert!(build_config(&f, "x".into()).is_err());
        f.kind = SourceKind::DeoVr;
        f.address = "192.168.1.5:9999/deovr".into();
        f.username = "me".into();
        match build_config(&f, "x".into()).unwrap() {
            SourceConfig::DeoVr(c) => {
                assert_eq!(c.url, "http://192.168.1.5:9999/deovr");
                assert!(c.credentials.is_some());
            }
            other => panic!("{other:?}"),
        }
        f.kind = SourceKind::Library;
        f.address = "relative/path".into();
        assert!(build_config(&f, "x".into()).is_err());
        let dir = std::env::temp_dir();
        f.address = dir.join("fpx-no-such-folder").display().to_string();
        assert!(build_config(&f, "x".into()).is_err(), "no such folder");
        f.address = dir.display().to_string();
        f.name.clear();
        match build_config(&f, "x".into()).unwrap() {
            SourceConfig::Library(c) => {
                assert_eq!((c.folder.as_path(), c.card), (dir.as_path(), None));
                assert_eq!(c.name, dir.file_name().unwrap().to_string_lossy());
            }
            other => panic!("{other:?}"),
        }

        // MediaBrowser validation
        f.kind = SourceKind::MediaBrowser;
        f.address = "192.168.1.10:8096/".into();
        f.username.clear();
        f.password.clear();
        f.quick_connect = QuickConnect::Idle;
        assert!(build_config(&f, "mb-1".into()).is_err());

        f.quick_connect = QuickConnect::Authorized {
            token: "tok_abc".into(),
            user_id: Some("user_123".into()),
        };
        match build_config(&f, "mb-1".into()).unwrap() {
            SourceConfig::MediaBrowser(c) => {
                assert_eq!(c.url, "http://192.168.1.10:8096");
                assert_eq!(c.name, "192.168.1.10");
                assert_eq!(c.access_token.as_deref(), Some("tok_abc"));
                assert_eq!(c.user_id.as_deref(), Some("user_123"));
                assert!(c.credentials.is_none());
            }
            other => panic!("{other:?}"),
        }

        // MediaBrowser with username/password
        f.use_quick_connect = false;
        f.quick_connect = QuickConnect::Idle;
        assert!(build_config(&f, "mb-2".into()).is_err());
        f.username = "alice".into();
        f.password = "hunter2".into();
        match build_config(&f, "mb-2".into()).unwrap() {
            SourceConfig::MediaBrowser(c) => {
                assert_eq!(c.url, "http://192.168.1.10:8096");
                assert!(c.credentials.is_some());
                assert!(c.access_token.is_none());
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn editing_gives_back_what_was_saved() {
        use fp_sources::{FeedConfig, HttpConfig, MediaBrowserConfig, SmbConfig};
        let creds = Some(Credentials::new("bob", "pw"));
        let saved = [
            SourceConfig::Smb(SmbConfig {
                id: "s".into(),
                name: "NAS".into(),
                host: "nas".into(),
                port: Some(1445),
                share: "videos".into(),
                path: "vr".into(),
                credentials: creds.clone(),
            }),
            SourceConfig::WebDav(HttpConfig {
                id: "w".into(),
                name: "Cloud".into(),
                url: "https://nas/dav".into(),
                credentials: creds.clone(),
                insecure_tls: true,
            }),
            SourceConfig::DeoVr(FeedConfig {
                id: "d".into(),
                name: "Feed".into(),
                url: "http://x:9999/deovr".into(),
                credentials: None,
                insecure_tls: false,
                max_height: Some(2160),
            }),
            SourceConfig::MediaBrowser(MediaBrowserConfig {
                id: "j".into(),
                name: "Jellyfin".into(),
                url: "http://jf:8096".into(),
                credentials: None,
                insecure_tls: false,
                max_height: None,
                access_token: Some("tok".into()),
                user_id: Some("u".into()),
            }),
            SourceConfig::Library(fp_sources::LibraryConfig {
                id: "l".into(),
                name: "Private".into(),
                folder: "/home/deck/Private".into(),
                card: None,
            }),
            // On a card that is out.
            SourceConfig::Library(fp_sources::LibraryConfig {
                id: "c".into(),
                name: "Clips".into(),
                folder: "Clips".into(),
                card: Some(fp_sources::CardRef {
                    id: "0000-0000".into(),
                    label: "SDCARD".into(),
                }),
            }),
        ];
        for c in saved {
            let f = edit_form(&c);
            assert_eq!(f.editing.as_ref(), Some(&c));
            let back = build_config(&f, c.id().into()).map(|n| keep_hidden(n, &c));
            assert_eq!(back.as_ref(), Ok(&c), "{c:?}");
        }
    }
}
