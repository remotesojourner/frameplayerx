//! A playback-owned timeline picture drawn above the controls, sized for
//! one tile. Its panel is hidden between hovers, so pointing at the seek bar
//! again doesn't build it anew, and released when the video closes.

use super::*;
use crate::ui::player::SeekHover;
use crate::ui::{theme, widgets};

pub(super) struct SeekPreview {
    pub(super) panel: Panel,
    texture: egui::TextureHandle,
    label: String,
    open: u64,
}

impl SeekPreview {
    fn new(open: u64, scale: f32, image: egui::ColorImage) -> Self {
        let mut panel = panel([384, 256], 0.29, 1.5).growing();
        panel.set_scale(scale);
        let texture = panel
            .ctx
            .load_texture("seek-preview", image, egui::TextureOptions::LINEAR);
        Self {
            panel,
            texture,
            label: String::new(),
            open,
        }
    }
}

impl App {
    pub(super) fn seek_preview_shown(&self) -> bool {
        self.seek_preview
            .as_ref()
            .is_some_and(|p| p.open == self.open_seq && p.panel.visible)
            && !self.ui.tracks_open
    }

    pub(super) fn paint_seek_preview(
        &mut self,
        renderer: &mut Renderer,
        input: &FrameInput,
        hover: Option<SeekHover>,
    ) {
        let hover = hover.filter(|_| {
            self.settings.seek_previews
                && self.panels[BAR].visible
                && self.ui_visible
                && !self.show_browser
                && !self.ui.tracks_open
        });
        if self
            .seek_preview
            .as_ref()
            .is_some_and(|p| p.open != self.open_seq)
        {
            self.release_seek_preview(renderer);
        }
        let mut ready = false;
        let mut tile = None;
        if let Some(playback) = self.playback.as_mut()
            && let Some(preview) = playback.trickplay.as_mut()
        {
            ready = preview.update(
                hover.as_ref().map(|hover| hover.time),
                playback.player.duration(),
            );
            if ready {
                tile = preview.take_tile();
            }
        }
        if self.playback.is_none() {
            self.release_seek_preview(renderer);
            return;
        }
        let Some(hover) = hover.filter(|_| ready) else {
            // The bar shows its time bubble again.
            if let Some(preview) = self.seek_preview.as_mut().filter(|p| p.panel.visible) {
                preview.panel.visible = false;
                self.panels[BAR].request_repaint();
            }
            return;
        };
        if let Some(image) = tile {
            if let Some(preview) = self.seek_preview.as_mut() {
                preview.texture.set(image, egui::TextureOptions::LINEAR);
                preview.panel.request_repaint();
            } else {
                let scale = self
                    .settings
                    .ui_scale
                    .clamp(*UI_SCALES.start(), *UI_SCALES.end());
                self.seek_preview = Some(SeekPreview::new(self.open_seq, scale, image));
            }
        }
        let Some(preview) = self.seek_preview.as_mut() else {
            return;
        };
        let toast = &self.panels[TOAST];
        let clearance = if toast.visible { toast.size.y } else { 0.0 };
        let bar = &mut self.panels[BAR];
        if !preview.panel.visible {
            bar.request_repaint();
            preview.panel.request_repaint();
        }
        preview.panel.visible = true;
        preview.panel.pose = seek_preview_pose(bar, &preview.panel, hover.x, clearance);
        if hover.label != preview.label {
            preview.label = hover.label;
            preview.panel.request_repaint();
        }
        if preview.panel.needs_paint()
            && let Err(error) = preview.panel.paint(renderer, input.time, |ctx| {
                picture(ctx, &preview.texture, &preview.label);
            })
        {
            log::warn!("painting seek preview: {error}");
            self.release_seek_preview(renderer);
        }
    }

    fn release_seek_preview(&mut self, renderer: &mut Renderer) {
        if let Some(preview) = self.seek_preview.take() {
            if let Some(panel) = preview.panel.id {
                renderer.destroy_panel(panel);
            }
            if preview.panel.visible {
                self.panels[BAR].request_repaint();
            }
        }
    }
}

fn seek_preview_pose(bar: &Panel, preview: &Panel, x: f32, clearance: f32) -> Mat4 {
    let point = bar.size.x / bar.points().x;
    let room = widgets::SHADOW_ROOM;
    let inset = room.left.max(room.right) as f32 * point;
    let limit = (bar.size.x / 2.0 - inset - preview.size.x / 2.0).max(0.0);
    let x = ((x - bar.points().x / 2.0) * point).clamp(-limit, limit);
    let y =
        bar.size.y / 2.0 - room.top as f32 * point + 5.0 * point + preview.size.y / 2.0 + clearance;
    bar.pose * Mat4::from_translation(Vec3::new(x, y, 0.01))
}

fn picture(ctx: &egui::Context, texture: &egui::TextureHandle, label: &str) {
    egui::CentralPanel::default()
        .frame(egui::Frame::NONE)
        .show(ctx, |ui| {
            let area = ui.max_rect();
            let size = texture.size_vec2();
            let scale = (area.width() / size.x).min(area.height() / size.y);
            let image = egui::Rect::from_center_size(area.center(), size * scale);
            let painter = ui.painter();
            painter.image(
                texture.id(),
                image,
                egui::Rect::from_min_max(egui::Pos2::ZERO, egui::pos2(1.0, 1.0)),
                egui::Color32::WHITE,
            );
            let label = widgets::one_line(
                painter,
                label.to_string(),
                theme::font(theme::Weight::SemiBold, 15.0),
                theme::TEXT,
                image.width() - 24.0,
            );
            let padding = egui::vec2(7.0, 4.0);
            let size = label.size() + padding * 2.0;
            let pos = egui::pos2(
                image.center().x - size.x / 2.0,
                image.bottom() - size.y - 5.0,
            );
            painter.rect_filled(
                egui::Rect::from_min_size(pos, size),
                6.0,
                egui::Color32::from_black_alpha(220),
            );
            painter.galley(pos + padding, label, theme::TEXT);
        });
}
