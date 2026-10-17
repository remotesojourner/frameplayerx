//! The application: owns the services, the open video and the panels, and
//! turns controller input into UI events and playback commands each frame.

mod actions;
mod frame;
mod input;
mod sources;
mod tasks;

use frame::escape;
use tasks::Job;

use crate::jobs::Jobs;
use crate::playback::{self, OpenRequest, Opened, Playback};
use crate::services::Services;
use crate::settings::Settings;
use crate::ui::{
    self, Action, Browse, QuickConnect, Screen, SourcePage, SourceView, UiState, UpdateStatus, View,
};
use crate::world::{Panel, Pointer, anchor_from_head, place, place_around, turned_with_picture};
use fp_core::PlaybackStatus;
use fp_core::format::Projection;
use fp_library::Library;
use fp_media::{PlayerState, VideoFrame};
use fp_render::{QuadDraw, Renderer, VideoParams};
use fp_xr::Hand;
use glam::{Mat4, Quat, Vec2, Vec3};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant};

/// Per-frame input from the XR session (or the preview simulator).
#[derive(Clone, Copy, Debug, Default)]
pub struct FrameInput {
    /// Seconds since start.
    pub time: f64,
    pub dt: f32,
    pub head: Option<(Vec3, Quat)>,
    pub hands: [Hand; 2],
    pub passthrough_available: bool,
    /// The compositor can draw panels and rays as quad layers (OpenXR);
    /// otherwise (the desktop preview) everything is drawn into the eyes.
    pub layers: bool,
}

/// A panel or pointer ray for the compositor to draw as its own quad layer,
/// so it stays put as the head moves.
#[derive(Clone, Copy, Debug)]
pub enum LayerDraw {
    Panel {
        /// Stable per panel: the layer's swapchain is kept under it.
        key: u64,
        panel: fp_render::PanelId,
        px: [u32; 2],
        pose: Mat4,
        size: Vec2,
    },
    Ray {
        /// The active hand's ray is the brighter one.
        active: bool,
        pose: (Vec3, Quat),
        size: Vec2,
    },
}

/// What to draw and do this frame.
pub struct FrameOutput {
    pub frame: Option<Arc<VideoFrame>>,
    pub video: VideoParams,
    pub quads: Vec<QuadDraw>,
    /// Drawn on top of the eyes' pictures, in order (see [`LayerDraw`]).
    pub layers: Vec<LayerDraw>,
    pub passthrough: bool,
    /// (hand, amplitude, milliseconds)
    pub buzz: Vec<(usize, f32, i64)>,
    pub quit: bool,
}

const MAIN: usize = 0;
const BAR: usize = 1;
const ADJUST: usize = 2;
const KEYBOARD: usize = 3;

/// How long a video may take to open before the app gives up on it. A
/// server that never sends a frame (a live transcode it can't keep up with)
/// would otherwise leave "Opening …" up for good.
const OPEN_TIMEOUT: Duration = Duration::from_secs(45);

pub struct App {
    pub settings: Settings,
    saved: Settings,
    services: Services,
    pub ui: UiState,
    thumbs: ui::thumbs::Thumbs,
    panels: [Panel; 4],
    subs: Panel,
    sub_key: String,
    sub_textures: Vec<egui::TextureHandle>,
    /// Sets subtitle text in any script, once its fonts have loaded.
    sub_shaper: Option<ui::subtitle_text::SubtitleText>,
    sub_fonts: Jobs<ui::subtitle_text::SubtitleText>,
    pointer: Pointer,
    pub playback: Option<Playback>,
    open_seq: u64,
    /// When the video being opened was asked for.
    open_started: Instant,
    jobs: Jobs<Job>,
    anchor: Option<Mat4>,
    reanchor: bool,
    ui_visible: bool,
    show_browser: bool,
    last_activity: Instant,
    /// Each hand's ray direction when it last counted as moving.
    activity_aim: [Option<Vec3>; 2],
    controls: crate::controls::Controls,
    trigger_prev: [f32; 2],
    /// Yaw/pitch when a dome drag began.
    drag_start: Option<(f32, f32)>,
    panel_drag: Option<PanelDrag>,
    /// Where the library and adjust panels sit now (saved when let go).
    offsets: crate::settings::PanelOffsets,
    /// The list the open video came from, for next/previous.
    queue: Vec<OpenRequest>,
    queue_pos: usize,
    last_status: Option<PlaybackStatus>,
    update: Option<fp_updater::Update>,
    /// Counts update checks, so a check for a channel the user has since
    /// left is dropped.
    update_check: u64,
    /// Removable drives seen at the last check, and when that was.
    mounts: Vec<std::path::PathBuf>,
    mounts_checked: Instant,
    /// Drives FramePlayerX has tried to mount since they were inserted.
    mount_tried: Vec<String>,
    /// Feed scenes (source, URL) waiting for their details, and how many
    /// are being fetched.
    feed_queue: std::collections::VecDeque<(String, String)>,
    feed_busy: usize,
    /// The cinema's house lights: up (1) while paused, down (0) playing.
    pub(crate) house_lights: f32,
    quit: bool,
}

/// A panel held by a grip: which panel and hand, and the hand's aim angles
/// and the panel's offset when it was grabbed.
#[derive(Clone, Copy)]
struct PanelDrag {
    panel: usize,
    hand: usize,
    start: (f32, f32),
    from: [f32; 2],
}

fn panel(px: [u32; 2], width_m: f32, ppp: f32) -> Panel {
    let size = Vec2::new(width_m, width_m * px[1] as f32 / px[0] as f32);
    Panel::new(px, size, ppp)
}

/// A panel whose body is `px` pixels and `width_m` wide, floating with
/// room round it for its shadow (ui::widgets::SHADOW_ROOM) at the same
/// scale.
fn floating_panel(px: [u32; 2], width_m: f32, ppp: f32) -> Panel {
    let room = ui::widgets::SHADOW_ROOM;
    let extra = [
        ((room.left + room.right) as f32 * ppp).round() as u32,
        ((room.top + room.bottom) as f32 * ppp).round() as u32,
    ];
    let full = [px[0] + extra[0], px[1] + extra[1]];
    panel(full, width_m * full[0] as f32 / px[0] as f32, ppp)
}

impl App {
    pub fn new(settings: Settings, library: Library) -> App {
        let services = Services::new(&settings, library);
        let thumbs = ui::thumbs::Thumbs::new(services.opener.clone());
        let mut panels = [
            floating_panel([1600, 1000], 1.8, 1.25),
            floating_panel([1400, 268], 1.2, 1.25),
            floating_panel([900, 1020], 0.75, 1.25),
            floating_panel([1200, 456], 1.0, 1.25),
        ];
        panels[BAR].refresh = Some(Duration::from_millis(250));
        panels[ADJUST].refresh = Some(Duration::from_millis(1000));
        panels[MAIN].refresh = Some(Duration::from_millis(1000));
        let subs = panel([1600, 320], 1.6, 1.0);
        subs.ctx
            .style_mut(|s| s.visuals.panel_fill = egui::Color32::TRANSPARENT);
        let sub_fonts = Jobs::new();
        sub_fonts.spawn("fp-fonts", ui::subtitle_text::SubtitleText::load);
        let mut app = App {
            saved: settings.clone(),
            settings,
            services,
            ui: UiState::default(),
            thumbs,
            panels,
            subs,
            sub_key: String::new(),
            sub_textures: Vec::new(),
            sub_shaper: None,
            sub_fonts,
            pointer: Pointer::default(),
            playback: None,
            open_seq: 0,
            open_started: Instant::now(),
            jobs: Jobs::new(),
            anchor: None,
            reanchor: true,
            ui_visible: true,
            show_browser: true,
            last_activity: Instant::now(),
            activity_aim: [None; 2],
            controls: Default::default(),
            trigger_prev: [0.0; 2],
            drag_start: None,
            panel_drag: None,
            offsets: Default::default(),
            queue: Vec::new(),
            queue_pos: 0,
            last_status: None,
            update: None,
            update_check: 0,
            mounts: crate::drives::removable_mounts(),
            // In the past, so the first check (and mount) runs at once.
            mounts_checked: Instant::now()
                .checked_sub(Duration::from_secs(60))
                .unwrap_or_else(Instant::now),
            mount_tried: Vec::new(),
            feed_queue: Default::default(),
            feed_busy: 0,
            house_lights: 1.0,
            quit: false,
        };
        app.offsets = app.settings.panel_offsets;
        app.rescan(false);
        app.connect_devices();
        if app.settings.check_updates && fp_updater::RELEASE_PUBLIC_KEY.is_some() {
            app.check_updates();
        }
        app
    }

    pub fn set_about(&mut self, about: Vec<(String, String)>) {
        self.ui.about = about;
    }

    /// Opens a location (path or URL) as if picked in the UI.
    pub fn open(&mut self, req: OpenRequest) {
        self.apply(Action::Open(req));
    }

    /// One line about the open video and the UI, for the preview harness.
    pub fn status_line(&self) -> String {
        let ui = format!(
            "ui_visible={} browser={} adjust={}",
            self.ui_visible, self.show_browser, self.ui.adjust_open
        );
        match &self.playback {
            Some(p) => format!(
                "{} pos={:.1} vol={:.2} zoom={:.2} yaw={:.1} pitch={:.1} queue={}/{} paused={} {ui}",
                p.title,
                p.player.position(),
                p.player.volume(),
                p.settings.zoom,
                p.settings.yaw,
                p.settings.pitch,
                self.queue_pos + 1,
                self.queue.len(),
                p.player.is_paused()
            ),
            None => format!("no video, screen={:?} {ui}", self.ui.screen),
        }
    }

    /// World position of a fractional point on a visible panel (tests).
    fn panel_index(name: &str) -> Option<usize> {
        Some(match name {
            "main" => MAIN,
            "bar" => BAR,
            "adjust" => ADJUST,
            "keyboard" => KEYBOARD,
            _ => return None,
        })
    }

    /// Repaints every panel next frame (the preview changed the UI state).
    pub fn repaint_all(&mut self) {
        for p in self.panels.iter_mut() {
            p.request_repaint();
        }
    }

    /// The renderer's image of a visible panel, for preview screenshots.
    pub fn panel_id(&self, name: &str) -> Option<fp_render::PanelId> {
        let p = match name {
            "subs" => &self.subs,
            _ => &self.panels[Self::panel_index(name)?],
        };
        p.visible.then_some(p.id).flatten()
    }

    pub fn panel_point(&self, name: &str, x: f32, y: f32) -> Option<Vec3> {
        let i = Self::panel_index(name)?;
        let p = &self.panels[i];
        if !p.visible {
            return None;
        }
        let pts = p.points();
        Some(p.world_point(egui::pos2(x * pts.x, y * pts.y)))
    }

    /// Saves and applies settings edited through the UI.
    fn sync_settings(&mut self) {
        if self.settings == self.saved {
            return;
        }
        if self.settings.remote != self.saved.remote {
            self.services.apply_remote(&self.settings.remote);
        }
        if self.settings.haptics_enabled != self.saved.haptics_enabled
            || self.settings.haptics != self.saved.haptics
        {
            self.services.set_haptics(&self.settings);
            if self.settings.haptics_enabled && !self.saved.haptics_enabled {
                self.connect_devices();
            }
        }
        if self.settings.ui_scale != self.saved.ui_scale {
            for p in self.panels.iter_mut() {
                p.ctx.set_zoom_factor(self.settings.ui_scale);
            }
        }
        if let Err(e) = self.settings.save(&Settings::path()) {
            log::warn!("saving settings: {e}");
        }
        self.saved = self.settings.clone();
    }

    fn remote_events(&mut self) {
        for ev in self.services.remote_events() {
            match ev {
                fp_remote::RemoteEvent::Command(c) => {
                    use fp_core::PlayerCommand as C;
                    let a = match c {
                        C::Open { location } => Action::Open(OpenRequest {
                            location,
                            ..Default::default()
                        }),
                        C::Play => {
                            if let Some(p) = &self.playback {
                                p.player.play();
                            }
                            continue;
                        }
                        C::Pause => {
                            if let Some(p) = &self.playback {
                                p.player.pause();
                            }
                            continue;
                        }
                        C::TogglePause => Action::TogglePause,
                        C::Seek { position } => Action::Seek(position),
                        C::SeekRelative { delta } => Action::SeekRelative(delta),
                        C::SetSpeed { speed } => Action::SetSpeed(speed),
                        C::Stop => Action::ClosePlayback,
                        C::Recenter => Action::Recenter,
                    };
                    self.apply(a);
                }
                fp_remote::RemoteEvent::Text(t) => {
                    // Typed on the phone: into whichever panel has a focused field.
                    if let Some(p) = self
                        .panels
                        .iter_mut()
                        .find(|p| p.visible && p.wants_keyboard())
                    {
                        p.push(egui::Event::Text(t));
                    } else {
                        // A search of what was watched.
                        self.ui.history.search = t;
                        self.ui.screen = Screen::History;
                        self.ui.details = None;
                        self.ui.invalidate();
                        self.show_browser = true;
                        self.ui_visible = true;
                    }
                }
            }
        }
    }

    /// Publishes playback status to remotes and the haptics engine.
    fn publish_status(&mut self) {
        let Some(p) = &self.playback else {
            self.last_status = None;
            return;
        };
        let s = p.status();
        let changed = match &self.last_status {
            Some(prev) => {
                fp_remote::significant_change(prev, &s, s.sampled_at_ms)
                    || s.sampled_at_ms.saturating_sub(prev.sampled_at_ms) > 1000
            }
            None => true,
        };
        if changed {
            if let Some(h) = self.services.haptics() {
                h.set_status(s.clone());
            }
            if let Some(r) = &self.services.remote {
                r.publish(&s);
            }
            self.last_status = Some(s);
        }
    }

    pub fn shutdown(mut self) {
        // Cut short the thumbnail being made, so stopping the worker below
        // does not wait for it.
        self.services.set_worker_busy(true);
        if let Some(p) = self.playback.take() {
            p.close(&self.services.library);
        }
        self.services.watch.flush(Duration::from_secs(3));
        self.sync_settings();
        if let Some(r) = self.services.remote.take() {
            r.stop();
        }
        if let Some(w) = self.services.worker.take() {
            w.stop();
        }
    }
}
