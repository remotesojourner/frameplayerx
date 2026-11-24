//! Send diagnostics, from Settings › About: what went wrong in the user's
//! words, what goes with them and where, the report itself on request, and
//! Send once something is written.

use super::theme::{self, Weight};
use super::widgets::{self, Kind, ListArea};
use super::{Action, View, icons, page};
use egui::RichText;

#[derive(Default)]
pub struct Form {
    /// What went wrong, as the user writes it.
    pub words: String,
    /// The rest of the report, gathered as the page opened.
    pub gathered: Option<crate::crash::Diagnostics>,
    /// The whole report is on the page: the words it was made with, and it.
    pub shown: Option<(String, String)>,
    pub sending: bool,
    /// Why it could not be sent.
    pub error: Option<String>,
    pub sent: bool,
    /// The number the relay gave it, for the user to quote.
    pub reference: Option<u64>,
}

fn contains() -> [&'static str; 6] {
    [
        tr!("diagnostics-contains-words"),
        tr!("diagnostics-contains-versions"),
        tr!("diagnostics-contains-video"),
        tr!("diagnostics-contains-settings"),
        tr!("diagnostics-contains-logs"),
        tr!("crash-contains-not"),
    ]
}

pub fn page(ui: &mut egui::Ui, v: &mut View) {
    let actions = &mut *v.actions;
    let Some(f) = v.state.diagnostics.as_mut() else {
        return;
    };
    if page::header(ui, tr!("diagnostics-title"), None, |_| {}) {
        actions.push(Action::CloseDiagnostics);
        return;
    }
    egui::ScrollArea::vertical()
        .id_salt("diagnostics")
        .auto_shrink([false, false])
        .show_list(ui, |ui| {
            if f.sent {
                sent(ui, f, actions);
                return;
            }
            ui.add_space(4.0);
            ui.label(
                RichText::new(tr!("diagnostics-desc"))
                    .font(theme::font(Weight::Regular, 18.0))
                    .color(theme::TEXT_2),
            );
            ui.add_space(16.0);
            widgets::section_label(ui, tr!("diagnostics-what"));
            ui.add_space(6.0);
            ui.add(
                egui::TextEdit::multiline(&mut f.words)
                    .char_limit(crate::crash::WORDS_MAX)
                    .desired_rows(3)
                    .desired_width(f32::INFINITY)
                    .margin(egui::Margin::symmetric(12, 9))
                    .hint_text(tr!("diagnostics-what-hint"))
                    .font(theme::font(Weight::Regular, 17.0)),
            );
            ui.add_space(16.0);
            super::crash::list(ui, tr!("crash-contains"), &contains());
            super::crash::list(ui, tr!("crash-goes"), &super::crash::goes());
            ui.add_space(8.0);
            buttons(ui, f, actions);
            if let Some(e) = &f.error {
                ui.add_space(6.0);
                ui.label(
                    RichText::new(tr!("crash-send-failed", error = e.as_str())).color(theme::ERROR),
                );
            }
            if f.shown.is_some() {
                report(ui, f);
            }
            ui.add_space(24.0);
        });
}

fn buttons(ui: &mut egui::Ui, f: &mut Form, actions: &mut Vec<Action>) {
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 10.0;
        if f.sending {
            ui.spinner();
            ui.label(RichText::new(tr!("crash-sending")).color(theme::TEXT_2));
            return;
        }
        let gathered = f.gathered.is_some();
        let send = ui
            .add_enabled_ui(gathered && !f.words.trim().is_empty(), |ui| {
                widgets::button(
                    ui,
                    Some(icons::CHECK),
                    tr!("diagnostics-send"),
                    Kind::Primary,
                )
            })
            .inner;
        if send.clicked() {
            actions.push(Action::SendDiagnostics);
        }
        let label = if f.shown.is_some() {
            tr!("crash-hide-report")
        } else {
            tr!("crash-show-report")
        };
        let show = ui
            .add_enabled_ui(gathered, |ui| {
                widgets::button(ui, Some(icons::EYE), label, Kind::Secondary)
            })
            .inner;
        if show.clicked() {
            f.shown = match f.shown {
                Some(_) => None,
                None => Some((String::new(), String::new())),
            };
        }
        if widgets::button(ui, None, tr!("common-cancel"), Kind::Ghost).clicked() {
            actions.push(Action::CloseDiagnostics);
        }
        if !gathered {
            ui.spinner();
            ui.label(RichText::new(tr!("diagnostics-gathering")).color(theme::TEXT_2));
        }
    });
}

/// The report as it would be sent, made again when the words change.
fn report(ui: &mut egui::Ui, f: &mut Form) {
    let (Some(gathered), Some(shown)) = (f.gathered.as_mut(), f.shown.as_mut()) else {
        return;
    };
    if shown.0 != f.words || shown.1.is_empty() {
        *shown = (f.words.clone(), gathered.report(&f.words).text);
    }
    ui.add_space(14.0);
    widgets::section_label(ui, tr!("crash-report-heading"));
    egui::Frame::new()
        .fill(theme::SURFACE_2)
        .corner_radius(12)
        .inner_margin(egui::Margin::same(16))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.label(
                RichText::new(&shown.1)
                    .font(egui::FontId::monospace(13.0))
                    .color(theme::TEXT_2),
            );
        });
}

fn sent(ui: &mut egui::Ui, f: &Form, actions: &mut Vec<Action>) {
    ui.add_space(18.0);
    ui.horizontal(|ui| {
        ui.label(
            RichText::new(icons::CHECK_CIRCLE)
                .font(theme::icon(30.0))
                .color(theme::OK),
        );
        ui.label(
            RichText::new(tr!("diagnostics-sent"))
                .font(theme::font(Weight::Bold, 24.0))
                .color(theme::TEXT),
        );
    });
    if let Some(n) = f.reference {
        ui.add_space(8.0);
        ui.label(
            RichText::new(tr!("diagnostics-reference", number = n.to_string()))
                .font(theme::font(Weight::Regular, 18.0))
                .color(theme::TEXT_2),
        );
    }
    ui.add_space(16.0);
    if widgets::button(ui, Some(icons::CHECK), tr!("common-done"), Kind::Primary).clicked() {
        actions.push(Action::CloseDiagnostics);
    }
}
