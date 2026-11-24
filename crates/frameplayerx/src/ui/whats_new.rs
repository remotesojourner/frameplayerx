//! The page the first start after an upgrade shows: what changed since
//! the version run before, by kind.

use super::theme::{self, Weight};
use super::widgets::{self, Kind, ListArea};
use super::{Action, View, icons, page};
use egui::RichText;

pub fn page(ui: &mut egui::Ui, v: &mut View) {
    let Some(new) = v.state.whats_new.as_ref() else {
        return;
    };
    let versions = match &new.from {
        Some(from) => tr!(
            "whats-new-versions",
            from = from.as_str(),
            to = new.to.as_str()
        ),
        None => tr!("whats-new-version", version = new.to.as_str()),
    };
    // The arrow closes it too.
    let back = page::header(ui, tr!("whats-new-title"), Some(&versions), |ui| {
        close_button(ui, v.actions);
    });
    egui::ScrollArea::vertical()
        .auto_shrink([false, false])
        .show_list(ui, |ui| {
            ui.add_space(8.0);
            list(ui, tr!("whats-new-features"), &new.features);
            list(ui, tr!("whats-new-fixes"), &new.fixes);
            list(ui, tr!("whats-new-performance"), &new.performance);
            ui.add_space(10.0);
        });
    if back {
        v.actions.push(Action::CloseWhatsNew);
    }
}

fn close_button(ui: &mut egui::Ui, actions: &mut Vec<Action>) {
    if widgets::button(
        ui,
        Some(icons::CHECK),
        tr!("whats-new-close"),
        Kind::Primary,
    )
    .clicked()
    {
        actions.push(Action::CloseWhatsNew);
    }
}

fn list(ui: &mut egui::Ui, title: &str, lines: &[String]) {
    if lines.is_empty() {
        return;
    }
    widgets::section_label(ui, title);
    for line in lines {
        ui.horizontal_top(|ui| {
            ui.label(
                RichText::new("•")
                    .font(theme::font(Weight::Bold, 18.0))
                    .color(theme::ACCENT),
            );
            ui.add(
                egui::Label::new(
                    RichText::new(line)
                        .font(theme::font(Weight::Regular, 18.0))
                        .color(theme::TEXT),
                )
                .wrap(),
            );
        });
        ui.add_space(4.0);
    }
    ui.add_space(14.0);
}
