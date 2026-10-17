//! Each frame: where the panels go, painting them, and what the compositor
//! draws.

use super::*;

impl App {
    /// Forward becomes where the head faces, and the library and adjust
    /// panels come back in front: moved round to the side or behind, they
    /// stayed there after recentering. How high they were moved is kept.
    pub(super) fn recenter(&mut self) {
        self.reanchor = true;
        self.panel_drag = None;
        self.offsets.main[0] = 0.0;
        self.offsets.adjust[0] = 0.0;
        self.settings.panel_offsets = self.offsets;
    }

    /// The panel the keyboard types into: the library or adjust panel with
    /// a text field in focus.
    pub(super) fn keyboard_target(&self) -> Option<usize> {
        [MAIN, ADJUST]
            .into_iter()
            .find(|&i| self.panels[i].visible && self.panels[i].wants_keyboard())
    }

    /// Squeezing a grip on the library or adjust panel moves it round the
    /// viewer with that hand; where it is let go is saved. Squeezing one on
    /// the keyboard moves the panel it types into, and the keyboard with it.
    pub(super) fn move_panels(
        &mut self,
        grab: Option<(usize, usize)>,
        hands: &[Hand; 2],
        anchor: Mat4,
        buzz: &mut Vec<(usize, f32, i64)>,
    ) {
        use crate::controls::{GRIP, angles, wrap};
        let anchor_rot = anchor.to_scale_rotation_translation().1;
        let grab = grab.and_then(|(panel, hand)| match panel {
            KEYBOARD => self.keyboard_target().map(|t| (t, hand)),
            MAIN | ADJUST => Some((panel, hand)),
            _ => None,
        });
        if let Some((panel, hand)) = grab
            && let Some((_, rot)) = hands[hand].aim
        {
            let start = if panel == MAIN {
                self.offsets.main
            } else {
                self.offsets.adjust
            };
            self.panel_drag = Some(PanelDrag {
                panel,
                hand,
                start: angles(rot, anchor_rot),
                from: start,
            });
            buzz.push((hand, 0.3, 14));
        }
        let Some(PanelDrag {
            panel,
            hand,
            start: (y0, p0),
            from: [oy, op],
        }) = self.panel_drag
        else {
            return;
        };
        let h = &hands[hand];
        if h.squeeze > GRIP * 0.6 {
            if let Some((_, rot)) = h.aim {
                let (y, p) = angles(rot, anchor_rot);
                let to = [wrap(oy + wrap(y - y0)), (op + p - p0).clamp(-45.0, 45.0)];
                if panel == MAIN {
                    self.offsets.main = to;
                } else {
                    self.offsets.adjust = to;
                }
                self.last_activity = Instant::now();
            }
        } else {
            self.panel_drag = None;
            self.settings.panel_offsets = self.offsets;
        }
    }

    pub fn frame(&mut self, renderer: &mut Renderer, input: FrameInput) -> FrameOutput {
        self.update();
        let anchor = self.anchor(&input);
        let eye = input.head.map(|h| h.0).unwrap_or(anchor.w_axis.truncate());
        self.update_visibility();
        let kb_target = self.keyboard_target();
        self.panels[KEYBOARD].visible = kb_target.is_some();
        self.place_panels(anchor, kb_target);
        let buzz = self.route_input(&input, anchor);
        self.paint_panels(renderer, &input, kb_target);
        let (frame, video, passthrough) = self.video(renderer, &input, anchor);
        let (quads, layers) = self.compose(&input, eye);
        FrameOutput {
            frame,
            video,
            quads,
            layers,
            passthrough,
            buzz,
            quit: self.quit,
        }
    }

    /// Background work, remote commands and saving the open video's state.
    fn update(&mut self) {
        self.poll_jobs();
        self.give_up_slow_open();
        self.services
            .set_worker_busy(self.playback.is_some() || self.ui.opening.is_some());
        self.sync_locks();
        self.check_mounts();
        self.remote_events();
        if let Some(p) = &mut self.playback {
            p.save_progress(&self.services.library, false);
            p.sync_watch();
            p.save_key(&self.services.library, false);
            p.watch_decoding(self.settings.hardware_decoding);
        }
        self.publish_status();
    }

    /// The room's frame of reference for the panels and the video.
    fn anchor(&mut self, input: &FrameInput) -> Mat4 {
        if let Some((pos, rot)) = input.head
            && (self.reanchor || self.anchor.is_none())
        {
            self.anchor = Some(anchor_from_head(pos, rot));
            self.reanchor = false;
        }
        let mut anchor = self.anchor.unwrap_or(Mat4::IDENTITY);
        // A 180°/360° video is drawn from wherever the head is (it was
        // filmed from one spot), so it moves along when you lean; panels
        // fixed in the room then seem to slide against it. During such a
        // video the panels keep to the head's position (not its turning),
        // so they stay put relative to the picture.
        if let (Some((pos, _)), Some(p)) = (input.head, &self.playback)
            && p.format.projection != Projection::Flat
        {
            let (_, rot, _) = anchor.to_scale_rotation_translation();
            anchor = Mat4::from_rotation_translation(rot, pos);
        }
        anchor
    }

    /// Which panels show: the controls hide on their own while a video
    /// plays.
    fn update_visibility(&mut self) {
        let playing = self.playback.is_some();
        let paused = self
            .playback
            .as_ref()
            .is_none_or(|p| p.player.is_paused() || p.player.state() != PlayerState::Playing);
        if playing
            && self.ui_visible
            && !paused
            && !self.ui.adjust_open
            && !self.show_browser
            && self.last_activity.elapsed().as_secs_f32() > self.settings.auto_hide_secs
        {
            self.ui_visible = false;
        }
        let browser = !playing || (self.ui_visible && self.show_browser);
        self.panels[MAIN].visible = browser;
        self.panels[BAR].visible = playing && self.ui_visible && !self.show_browser;
        self.panels[ADJUST].visible = self.panels[BAR].visible && self.ui.adjust_open;
    }

    /// Where each panel goes. During 180°/360° video the control bar turns
    /// with the picture's yaw and pitch, so it stays below what you watch.
    /// (A flat screen doesn't turn, and the adjust panel stays put so its
    /// yaw/pitch sliders don't move away from the pointer.)
    fn place_panels(&mut self, anchor: Mat4, kb_target: Option<usize>) {
        let bar_anchor = match &self.playback {
            Some(p) if p.format.projection != Projection::Flat => {
                let s = p.shown_settings(&self.settings.default_view);
                turned_with_picture(anchor, s.yaw, s.pitch)
            }
            _ => anchor,
        };
        let [my, mp] = self.offsets.main;
        let [ay, ap] = self.offsets.adjust;
        self.panels[MAIN].pose = place_around(anchor, my, mp, 1.5, -0.08);
        self.panels[BAR].pose = place(bar_anchor, 0.0, 1.05, -0.42, 28.0);
        // To the right, clear of the control bar's end.
        self.panels[ADJUST].pose = place_around(anchor, -48.0 + ay, ap, 1.15, -0.08);
        // The keyboard sits below the panel it types into, turned with it, so
        // it is where that panel was moved to.
        let [ky, kp] = match kb_target {
            Some(ADJUST) => [-48.0 + ay, ap],
            Some(_) => [my, mp],
            None => [0.0, 0.0],
        };
        let kb_anchor = place_around(anchor, ky, kp, 0.0, 0.0);
        self.panels[KEYBOARD].pose = place(kb_anchor, 0.0, 0.95, -0.55, 35.0);
    }

    /// Pointer rays to panel events, grips moving panels, and controller
    /// commands; returns the haptic pulses to play.
    fn route_input(&mut self, input: &FrameInput, anchor: Mat4) -> Vec<(usize, f32, i64)> {
        let routed = {
            let [a, b, c, d] = &mut self.panels;
            self.pointer
                .route(&mut [a, b, c, d], &input.hands, input.dt)
        };
        let mut buzz = Vec::new();
        self.move_panels(routed.grab, &input.hands, anchor, &mut buzz);
        self.controller(
            input,
            routed.over_ui,
            routed.click_outside,
            anchor,
            &mut buzz,
        );
        buzz
    }

    /// Paints the panels that changed (and the keyboard), then carries out
    /// what they asked for.
    fn paint_panels(
        &mut self,
        renderer: &mut Renderer,
        input: &FrameInput,
        kb_target: Option<usize>,
    ) {
        let mut actions = Vec::new();
        if self.thumbs.poll(&self.panels[MAIN].ctx) {
            self.panels[MAIN].request_repaint();
        }
        let devices = self.services.haptic_statuses();
        let passthrough_available = input.passthrough_available;
        for idx in [MAIN, BAR, ADJUST] {
            if !self.panels[idx].needs_paint() {
                continue;
            }
            let App {
                panels,
                ui: state,
                settings,
                services,
                thumbs,
                playback,
                ..
            } = self;
            let mut view = View {
                state,
                settings,
                services,
                thumbs,
                playback: playback.as_mut(),
                actions: &mut actions,
                passthrough_available,
                devices: &devices,
            };
            let r = panels[idx].paint(renderer, input.time, |ctx| match idx {
                MAIN => ui::library::browser(ctx, &mut view),
                BAR => ui::player::control_bar(ctx, &mut view),
                _ => ui::player::adjust_panel(ctx, &mut view),
            });
            if let Err(e) = r {
                log::error!("painting panel: {e}");
            }
        }
        if let Some(target) = kb_target {
            let mut events = Vec::new();
            let mut close = false;
            let shift = &mut self.ui.keyboard_shift;
            let symbols = &mut self.ui.keyboard_symbols;
            if self.panels[KEYBOARD].needs_paint() {
                let r = self.panels[KEYBOARD].paint(renderer, input.time, |ctx| {
                    let (e, c) = ui::keyboard::keyboard(ctx, shift, symbols);
                    events = e;
                    close = c;
                });
                if let Err(e) = r {
                    log::error!("painting the keyboard: {e}");
                }
            }
            for e in events {
                self.panels[target].push(e);
            }
            if close {
                self.panels[target].push(escape());
            }
        }
        for a in actions {
            self.apply(a);
        }
        self.sync_settings();
    }

    /// The video frame and how to draw it, and whether the room shows.
    fn video(
        &mut self,
        renderer: &mut Renderer,
        input: &FrameInput,
        anchor: Mat4,
    ) -> (Option<Arc<VideoFrame>>, VideoParams, bool) {
        let mut video = VideoParams::default();
        let mut frame = None;
        let flat_or_none = self
            .playback
            .as_ref()
            .is_none_or(|p| p.format.projection == Projection::Flat);
        // A chroma-keyed or alpha-packed video shows the room where its
        // background was, whatever its projection and whether or not
        // passthrough is on.
        let keyed = self.playback.as_ref().is_some_and(|p| {
            p.shown_settings(&self.settings.default_view).chroma_key
                || p.format.alpha_pack_scale().is_some()
        });
        let passthrough =
            input.passthrough_available && ((self.settings.passthrough && flat_or_none) || keyed);
        if passthrough {
            video.background = [0.0; 4];
        }
        let (anchor_yaw, _, _) = anchor
            .to_scale_rotation_translation()
            .1
            .to_euler(glam::EulerRot::YXZ);
        if let Some(p) = &self.playback {
            frame = p.player.current_frame();
            let mut s = p.shown_settings(&self.settings.default_view);
            if flat_or_none && !passthrough {
                let playing = p.player.state() == PlayerState::Playing;
                let target = if playing { 0.0 } else { 1.0 };
                self.house_lights += (target - self.house_lights) * (input.dt / 1.5).min(1.0);
                video.environment = self.settings.environment;
                video.house_lights = self.house_lights;
            }
            video.format = p.format;
            video.screen_pose = place(anchor, 0.0, s.screen_distance, 0.0, 0.0);
            s.yaw += anchor_yaw.to_degrees();
            video.forward_yaw = anchor_yaw.to_degrees();
            video.settings = s;
            if let Some((_, rot)) = input.head {
                let rel = anchor.to_scale_rotation_translation().1.inverse() * rot;
                p.player.set_head_orientation(rel.to_array());
            }
            self.subtitles(renderer, input.time, anchor, &s);
        } else {
            self.subs.visible = false;
        }
        (frame, video, passthrough)
    }

    /// What the compositor draws: panels back to front, then pointers,
    /// as quads in the eyes or (when it can) layers of their own.
    fn compose(&mut self, input: &FrameInput, eye: Vec3) -> (Vec<QuadDraw>, Vec<LayerDraw>) {
        let mut quads = Vec::new();
        // Subtitles fade in like the panels (a panel that never fades in is
        // never drawn).
        self.subs.update_fade(input.dt);
        if self.subs.visible {
            quads.extend(self.subs.quad());
        }
        for p in self.panels.iter_mut() {
            p.update_fade(input.dt);
        }
        let mut order: Vec<usize> = (0..4).filter(|&i| self.panels[i].shown()).collect();
        order.sort_by(|&a, &b| {
            let da = (self.panels[a].pose.w_axis.truncate() - eye).length();
            let db = (self.panels[b].pose.w_axis.truncate() - eye).length();
            db.total_cmp(&da)
        });
        let mut layers = Vec::new();
        for i in order {
            let p = &self.panels[i];
            match p.id {
                // Fully shown: the compositor draws it. Fading: drawn into
                // the eyes, where opacity works.
                Some(panel) if input.layers && p.fully_shown() => layers.push(LayerDraw::Panel {
                    key: i as u64,
                    panel,
                    px: p.px,
                    pose: p.pose,
                    size: p.size,
                }),
                _ => quads.extend(p.quad()),
            }
        }
        let any_ui = self.panels.iter().any(|p| p.visible);
        if input.layers {
            layers.extend(self.pointer.ray_layers(eye, any_ui));
        } else {
            quads.extend(self.pointer.quads(eye, any_ui));
        }
        (quads, layers)
    }

    pub(super) fn subtitles(
        &mut self,
        renderer: &mut Renderer,
        time: f64,
        anchor: Mat4,
        s: &fp_core::view::ViewSettings,
    ) {
        if self.sub_shaper.is_none() {
            self.sub_shaper = self.sub_fonts.poll().pop();
        }
        let Some(p) = &self.playback else { return };
        let cues = p.player.subtitles();
        let key: String = cues
            .iter()
            .map(|c| format!("{}|{}|{};", c.start, c.text, c.bitmaps.len()))
            .collect();
        self.subs.visible = !cues.is_empty();
        let d = s.subtitle_distance.clamp(0.5, 20.0);
        let w = 0.9 * d;
        self.subs.size = Vec2::new(w, w * 320.0 / 1600.0);
        self.subs.pose = place(anchor, 0.0, d, -0.2 * d, 0.0);
        if key != self.sub_key && self.subs.visible {
            self.sub_key = key;
            let textures = &mut self.sub_textures;
            let shaper = &mut self.sub_shaper;
            if let Err(e) = self.subs.paint(renderer, time, |ctx| {
                ui::player::subtitles(ctx, &cues, textures, shaper.as_mut());
            }) {
                log::error!("subtitles: {e}");
            }
        }
    }
}

/// The Escape key, which closes the focused text field's keyboard.
pub(super) fn escape() -> egui::Event {
    egui::Event::Key {
        key: egui::Key::Escape,
        physical_key: None,
        pressed: true,
        repeat: false,
        modifiers: egui::Modifiers::NONE,
    }
}
