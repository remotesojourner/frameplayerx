//! Persistent user settings (`~/.config/frameplayerx/settings.json`).
//! Source credentials live separately in `sources.json` (mode 0600).

use fp_core::view::{BackgroundEnvironment, Preset, ViewSettings};
use fp_haptics::HapticsSettings;
use fp_remote::config::RemoteConfig;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// A haptic device the user added.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum HapticDeviceConfig {
    /// TCode over serial (`/dev/ttyACM0`), `tcp://host:port` or `udp://host:port`.
    Tcode { endpoint: String },
    /// Intiface Central's server.
    Buttplug { url: String },
    /// The Handy, by connection key.
    Handy { key: String },
}

impl HapticDeviceConfig {
    pub fn label(&self) -> String {
        match self {
            HapticDeviceConfig::Tcode { endpoint } => format!("TCode {endpoint}"),
            HapticDeviceConfig::Buttplug { url } => format!("Intiface {url}"),
            HapticDeviceConfig::Handy { key } => {
                let shown: String = key.chars().take(3).collect();
                format!("The Handy ({shown}…)")
            }
        }
    }
}

/// Which subtitles a video starts with (see [`crate::subtitles::choose`]).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SubtitleMode {
    Off,
    /// Forced subtitles, subtitles in your language when the audio is in
    /// another one, and a subtitle file named like the video.
    #[default]
    Automatic,
    /// Subtitles in your language whenever the video has them.
    Always,
}

/// Settings › Interface size: the sizes offered.
pub const UI_SCALES: std::ops::RangeInclusive<f32> = 0.8..=1.5;

/// Settings › Image cache: the sizes offered, in MB.
pub const IMAGE_CACHE_SIZES: [u32; 5] = [100, 250, 500, 1000, 2000];

/// The most videos Play all takes from a folder and its subfolders, unless
/// a source sets its own limit.
pub const DEFAULT_PLAY_ALL_LIMIT: usize = 300;

/// The cinema row videos start in: the second, about as large a picture as
/// the floating screen's default.
pub const DEFAULT_CINEMA_ROW: u8 = 2;

/// Settings › Video position's screen glow: the background round a flat
/// screen lit by the colours at the picture's edges. Brightness and spread
/// are each a share of their most.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ScreenGlow {
    pub on: bool,
    pub brightness: f32,
    pub spread: f32,
}

impl Default for ScreenGlow {
    fn default() -> Self {
        ScreenGlow {
            on: false,
            brightness: 0.5,
            spread: 0.5,
        }
    }
}

impl ScreenGlow {
    /// What the renderer draws for a video with `view`'s adjustments: its
    /// own where it has them, else these; nothing while it is off.
    pub fn look(&self, view: &ViewSettings) -> Option<fp_render::ScreenGlow> {
        view.screen_glow
            .unwrap_or(self.on)
            .then(|| fp_render::ScreenGlow {
                brightness: view.glow_brightness.unwrap_or(self.brightness),
                spread: view.glow_spread.unwrap_or(self.spread),
            })
    }
}

/// Settings › Controller › Hold time, in seconds. Below the least, a glance
/// across the panel would press what it passes.
pub const HANDS_FREE_HOLD_SECS: std::ops::RangeInclusive<f32> = 0.4..=2.0;
pub const DEFAULT_HANDS_FREE_HOLD_SECS: f32 = 0.75;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// Use the V4L2 hardware decoder when available.
    pub hardware_decoding: bool,
    /// ALSA device ("default" routes to PipeWire on SteamOS).
    pub audio_device: String,
    pub volume: f32,
    pub subtitles: SubtitleMode,
    /// The language subtitles are wanted in (ISO 639-1).
    pub subtitle_language: String,
    /// The interface's language, one of [`crate::i18n::LANGUAGES`]; none
    /// follows Steam's.
    pub language: Option<String>,
    /// Defaults for videos without saved adjustments.
    pub default_view: ViewSettings,
    /// Adjustments saved to apply to any video, in the order they were made.
    pub presets: Vec<Preset>,
    /// Show the real world around flat screens and the UI.
    pub passthrough: bool,
    pub screen_glow: ScreenGlow,
    /// Resume videos where they were left.
    pub resume: bool,
    /// The format a video starts in when nothing identifies it (no
    /// metadata, file name tag or telling shape).
    pub default_format: fp_core::format::VideoFormat,
    /// The fisheye lens angles offered as presets, in degrees, smallest
    /// first.
    pub fisheye_presets: Vec<f32>,
    /// The angle a video gets when Fisheye is picked for it, and a Fisheye
    /// default format: one of the presets (180° with none).
    pub fisheye_start: f32,
    /// Seconds of inactivity before the playback UI hides.
    pub auto_hide_secs: f32,
    /// Pointing along the seek bar shows the server's picture of that moment.
    pub seek_previews: bool,
    /// Point with the head; holding the dot on a control presses it.
    pub hands_free: bool,
    pub hands_free_hold_secs: f32,
    /// Thumbstick seek step in seconds.
    pub seek_step: f64,
    pub remote: RemoteConfig,
    /// Haptic devices and scripts. Off, the app shows nothing about them,
    /// loads no scripts and runs no haptics engine.
    pub haptics_enabled: bool,
    pub haptics: HapticsSettings,
    pub haptic_devices: Vec<HapticDeviceConfig>,
    pub update_channel: fp_updater::Channel,
    pub check_updates: bool,
    /// UI text size multiplier, in [`UI_SCALES`].
    pub ui_scale: f32,
    /// Refresh rate, render resolution and foveated rendering.
    pub display: fp_core::display::Display,
    /// Room for server pictures kept on the headset, in MB
    /// ([`crate::image_cache`]).
    pub image_cache_mb: u32,
    /// Make thumbnails for videos whose server offers no picture.
    pub make_thumbnails: bool,
    /// A partly watched video shows the frame where it was left off.
    pub resume_frames: bool,
    /// Flat videos and the panels follow the head ([`crate::world::follow`]).
    pub follow: bool,
    /// The video playing starts again when it ends.
    pub loop_video: bool,
    /// Up next plays through, the next video starting when one ends.
    pub autoplay: bool,
    /// Up next starts again from the first video after the last, and X and
    /// Y step round it.
    pub loop_playlist: bool,
    /// The performance overlay (Settings › General › Diagnostics).
    pub perf_overlay: bool,
    /// What the controller buttons and thumbsticks do.
    pub controls: crate::bindings::Bindings,
    /// Where the library and adjust panels were moved to: yaw and pitch in
    /// degrees from their usual places (left and up positive).
    pub panel_offsets: PanelOffsets,
    /// How folders are ordered when browsing a source.
    pub browse_sort: fp_sources::EntrySort,
    /// Sources locked with a PIN: source id to the PIN's hash (see
    /// [`crate::pin`]). Left out of the file when empty.
    #[serde(skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub source_pins: std::collections::BTreeMap<String, String>,
    /// Play all's limit on videos from subfolders, by source id, where it
    /// isn't [`DEFAULT_PLAY_ALL_LIMIT`]; `null` turns it off.
    #[serde(skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub(crate) play_all_limits: std::collections::BTreeMap<String, Option<usize>>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct PanelOffsets {
    pub main: [f32; 2],
    /// The adjust panel, from beside the control bar.
    pub adjust: [f32; 2],
    /// The control bar, from below a flat screen (or in front of you
    /// during a 180°/360° video).
    pub bar: [f32; 2],
    /// The performance overlay, from where it starts.
    pub perf: [f32; 2],
}

/// The fisheye presets to start with: a plain 180° lens, the Canon RF 5.2 mm,
/// the MKX200 and the MKX220.
pub const FISHEYE_PRESETS: [f32; 4] = [180.0, 190.0, 200.0, 220.0];
/// As many fisheye presets as fit on one line.
pub const MAX_FISHEYE_PRESETS: usize = 8;
/// The angle Fisheye starts at to begin with, a plain fisheye lens's.
pub const FISHEYE_START: f32 = 180.0;

/// Adds `angle`, in whole degrees, to `presets`, kept in order, unless it is
/// there or they are full.
pub fn add_fisheye_preset(presets: &mut Vec<f32>, angle: f32) {
    let angle = angle.round();
    if presets.len() < MAX_FISHEYE_PRESETS && !presets.contains(&angle) {
        presets.push(angle);
        presets.sort_by(f32::total_cmp);
    }
}

impl Settings {
    /// Takes the fisheye preset at `index` out. If Fisheye started at it,
    /// it starts at the preset nearest 180° instead (the smaller of two as
    /// near), or 180° with none left.
    pub fn remove_fisheye_preset(&mut self, index: usize) {
        if index < self.fisheye_presets.len()
            && self.fisheye_presets.remove(index) == self.fisheye_start
        {
            let off = |angle: &f32| (angle - FISHEYE_START).abs();
            self.fisheye_start = self
                .fisheye_presets
                .iter()
                .copied()
                .min_by(|a, b| off(a).total_cmp(&off(b)))
                .unwrap_or(FISHEYE_START);
        }
    }

    /// The fisheye presets and where Fisheye starts, as they began.
    pub fn reset_fisheye_presets(&mut self) {
        self.fisheye_presets = FISHEYE_PRESETS.to_vec();
        self.fisheye_start = FISHEYE_START;
    }

    /// Whether the fisheye presets or where Fisheye starts were changed.
    pub fn fisheye_presets_changed(&self) -> bool {
        self.fisheye_presets != FISHEYE_PRESETS || self.fisheye_start != FISHEYE_START
    }

    /// Saves `view` and the `format` picked (none on Automatic) as a preset
    /// named "Preset" and the first number no preset has; its name, or none
    /// when presets are full.
    pub fn add_preset(
        &mut self,
        view: ViewSettings,
        format: Option<fp_core::format::VideoFormat>,
    ) -> Option<String> {
        if self.presets.len() >= MAX_PRESETS {
            return None;
        }
        let name = (1..)
            .map(|n| tr!("preset-name", n = n))
            .find(|name| self.presets.iter().all(|p| p.name != *name))?;
        self.presets.push(Preset {
            name: name.clone(),
            format,
            view,
        });
        Some(name)
    }

    /// The preset after the one at `current` (the first a video matches),
    /// or the first.
    pub fn next_preset(&self, current: Option<usize>) -> Option<usize> {
        let next = current.map_or(0, |i| (i + 1) % self.presets.len().max(1));
        (next < self.presets.len()).then_some(next)
    }
}

/// How many presets there can be: one row at the top of the adjust panel.
pub const MAX_PRESETS: usize = 8;

impl Default for Settings {
    fn default() -> Self {
        Settings {
            hardware_decoding: true,
            audio_device: "default".into(),
            volume: 1.0,
            subtitles: SubtitleMode::default(),
            subtitle_language: crate::i18n::system_language()
                .and_then(|l| fp_core::language::from_locale(&l))
                .unwrap_or("en")
                .to_string(),
            language: None,
            default_view: ViewSettings::default(),
            presets: Vec::new(),
            passthrough: false,
            screen_glow: ScreenGlow::default(),
            resume: true,
            default_format: fp_core::format::VideoFormat::FALLBACK,
            fisheye_presets: FISHEYE_PRESETS.to_vec(),
            fisheye_start: FISHEYE_START,
            auto_hide_secs: 4.0,
            seek_previews: true,
            hands_free: false,
            hands_free_hold_secs: DEFAULT_HANDS_FREE_HOLD_SECS,
            seek_step: 10.0,
            remote: RemoteConfig::default(),
            haptics_enabled: false,
            haptics: HapticsSettings::default(),
            haptic_devices: Vec::new(),
            // A beta build keeps getting betas until the user picks Stable.
            update_channel: if env!("CARGO_PKG_VERSION").contains('-') {
                fp_updater::Channel::Beta
            } else {
                fp_updater::Channel::Stable
            },
            check_updates: true,
            ui_scale: 1.0,
            display: Default::default(),
            image_cache_mb: 500,
            make_thumbnails: true,
            resume_frames: true,
            follow: false,
            loop_video: false,
            autoplay: false,
            loop_playlist: false,
            perf_overlay: false,
            controls: crate::bindings::Bindings::default(),
            panel_offsets: PanelOffsets::default(),
            browse_sort: fp_sources::EntrySort::default(),
            source_pins: Default::default(),
            play_all_limits: Default::default(),
        }
    }
}

impl Settings {
    /// What surrounds flat videos that follow Settings › Video position.
    pub fn environment(&self) -> BackgroundEnvironment {
        match self.default_view.environment {
            BackgroundEnvironment::UseGlobal => BackgroundEnvironment::Colour,
            environment => environment,
        }
    }

    pub fn set_environment(&mut self, environment: BackgroundEnvironment) {
        self.default_view.environment = environment;
    }

    /// The cinema row for videos that follow Settings › Video position.
    pub fn cinema_row(&self) -> u8 {
        self.default_view.cinema_row.unwrap_or(DEFAULT_CINEMA_ROW)
    }

    pub fn set_cinema_row(&mut self, row: u8) {
        self.default_view.cinema_row = Some(row);
    }

    /// The cinema screen's curve for videos that follow Settings › Video
    /// position.
    pub fn cinema_curvature(&self) -> f32 {
        self.default_view.cinema_curvature.unwrap_or(0.0)
    }

    pub fn set_cinema_curvature(&mut self, curvature: f32) {
        self.default_view.cinema_curvature = Some(curvature);
    }

    /// The most videos Play all takes from `source`'s subfolders, `None`
    /// for no limit.
    pub fn play_all_limit(&self, source: &str) -> Option<usize> {
        match self.play_all_limits.get(source) {
            None | Some(Some(0)) => Some(DEFAULT_PLAY_ALL_LIMIT),
            Some(limit) => *limit,
        }
    }

    pub fn set_play_all_limit(&mut self, source: &str, limit: Option<usize>) {
        if matches!(limit, Some(0 | DEFAULT_PLAY_ALL_LIMIT)) {
            self.clear_play_all_limit(source);
        } else {
            self.play_all_limits.insert(source.to_owned(), limit);
        }
    }

    /// Removes the override for a deleted source or a restored default.
    pub fn clear_play_all_limit(&mut self, source: &str) {
        self.play_all_limits.remove(source);
    }

    /// Adjustments for a video that has none saved: the defaults, following
    /// the global chroma key, comfort focus, background, cinema seat and
    /// screen glow.
    pub fn new_video_view(&self) -> ViewSettings {
        ViewSettings {
            key_own: Some(false),
            focus_own: Some(false),
            environment: BackgroundEnvironment::UseGlobal,
            cinema_row: None,
            cinema_curvature: None,
            screen_glow: None,
            glow_brightness: None,
            glow_spread: None,
            ..self.default_view
        }
    }

    pub fn path() -> PathBuf {
        fp_core::dirs::config_dir().join("settings.json")
    }

    pub fn sources_path() -> PathBuf {
        fp_core::dirs::config_dir().join("sources.json")
    }

    /// The language the interface is shown in: the one picked, else
    /// [`crate::i18n::automatic`].
    pub fn interface_language(&self) -> String {
        self.language
            .clone()
            .unwrap_or_else(|| crate::i18n::automatic().to_string())
    }

    /// Loads settings, falling back to defaults (and logging why) when the
    /// file is missing or unreadable.
    pub fn load(path: &Path) -> Settings {
        match std::fs::read_to_string(path) {
            Ok(text) => match serde_json::from_str::<Settings>(&text) {
                Ok(mut s) => {
                    s.display = s.display.offered();
                    s.hands_free_hold_secs = s
                        .hands_free_hold_secs
                        .clamp(*HANDS_FREE_HOLD_SECS.start(), *HANDS_FREE_HOLD_SECS.end());
                    // Saved before the global chroma key existed: its key
                    // was never chosen, so take the current default.
                    if s.default_view.key_own.is_none() {
                        s.default_view.set_key(&ViewSettings::default());
                        s.default_view.key_own = Some(false);
                    }
                    // Saved before height existed: the right grip's up / down
                    // was free then, so it gets the new default.
                    let before_height = serde_json::from_str::<serde_json::Value>(&text)
                        .ok()
                        .is_some_and(|v| v["default_view"].get("height").is_none());
                    if before_height
                        && s.controls.right.grip_y == crate::bindings::AxisAction::Nothing
                    {
                        s.controls.right.grip_y = crate::bindings::AxisAction::Height;
                    }
                    s
                }
                Err(e) => {
                    log::warn!("settings {}: {e}; using defaults", path.display());
                    Settings::default()
                }
            },
            Err(_) => Settings::default(),
        }
    }

    /// Writes atomically (temp file + rename).
    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let tmp = path.with_extension("json.tmp");
        let json = serde_json::to_string_pretty(self).map_err(std::io::Error::other)?;
        std::fs::write(&tmp, json)?;
        std::fs::rename(&tmp, path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn presets_get_free_names_and_next_goes_round() {
        let mut s = Settings::default();
        let view = |zoom| ViewSettings {
            zoom,
            ..ViewSettings::default()
        };
        assert_eq!(s.next_preset(None), None);
        let first = s.add_preset(view(1.2), None).unwrap();
        let second = s.add_preset(view(1.5), None).unwrap();
        assert_ne!(first, second);
        s.presets.remove(0);
        assert_eq!(
            s.add_preset(view(2.0), None),
            Some(first),
            "a freed name is reused"
        );
        // Next: after the one matching, round to the first, the first when
        // none matches.
        assert_eq!(s.next_preset(Some(0)), Some(1));
        assert_eq!(s.next_preset(Some(1)), Some(0));
        assert_eq!(s.next_preset(None), Some(0));
        while s.add_preset(view(1.0), None).is_some() {}
        assert_eq!(s.presets.len(), MAX_PRESETS);
    }

    #[test]
    fn play_all_limits_default_and_round_trip_per_source() {
        let mut settings: Settings = serde_json::from_str(r#"{"volume": 0.25}"#).unwrap();
        assert_eq!(settings.volume, 0.25);
        assert_eq!(settings.play_all_limit("webdav"), Some(300));
        let saved = serde_json::to_value(&settings).unwrap();
        assert!(saved.get("play_all_limits").is_none());

        settings.set_play_all_limit("webdav", Some(128));
        settings.set_play_all_limit("smb", None);
        let saved = serde_json::to_value(&settings).unwrap();
        assert_eq!(saved["play_all_limits"]["webdav"], 128);
        assert!(saved["play_all_limits"].get("smb").unwrap().is_null());
        let mut restored: Settings = serde_json::from_value(saved).unwrap();
        assert_eq!(restored.play_all_limit("webdav"), Some(128));
        assert_eq!(restored.play_all_limit("smb"), None);
        assert_eq!(restored.play_all_limit("unknown"), Some(300));

        restored.set_play_all_limit("webdav", Some(DEFAULT_PLAY_ALL_LIMIT));
        restored.clear_play_all_limit("smb");
        assert!(restored.play_all_limits.is_empty());
    }

    #[test]
    fn a_zero_play_all_limit_falls_back_to_the_default() {
        let settings: Settings =
            serde_json::from_str(r#"{"play_all_limits": {"webdav": 0}}"#).unwrap();
        assert_eq!(settings.play_all_limit("webdav"), Some(300));
    }

    #[test]
    fn a_hold_time_too_short_to_use_loads_as_the_shortest() {
        let dir = std::env::temp_dir().join(format!("fp-hold-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("settings.json");
        std::fs::write(&path, r#"{"hands_free": true, "hands_free_hold_secs": 0}"#).unwrap();
        let s = Settings::load(&path);
        assert!(s.hands_free);
        assert_eq!(s.hands_free_hold_secs, *HANDS_FREE_HOLD_SECS.start());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn round_trip_and_partial_files() {
        let dir = std::env::temp_dir().join(format!("fp-settings-{}", std::process::id()));
        let path = dir.join("settings.json");
        let mut s = Settings {
            volume: 0.5,
            ..Default::default()
        };
        s.haptic_devices.push(HapticDeviceConfig::Tcode {
            endpoint: "tcp://10.0.0.2:8000".into(),
        });
        s.save(&path).unwrap();
        assert_eq!(Settings::load(&path), s);
        // Source PINs are only written once one is set.
        assert!(
            !std::fs::read_to_string(&path)
                .unwrap()
                .contains("source_pins")
        );
        s.source_pins.insert("nas".into(), crate::pin::hash("2468"));
        s.save(&path).unwrap();
        assert_eq!(Settings::load(&path), s);
        std::fs::write(&path, r#"{"volume": 0.25}"#).unwrap();
        let p = Settings::load(&path);
        assert_eq!(p.volume, 0.25);
        assert_eq!(p.seek_step, 10.0);
        assert!(!p.haptics_enabled, "haptics stay off until turned on");
        assert!(!p.hands_free, "and hands-free navigation");
        assert_eq!(p.environment(), BackgroundEnvironment::Colour);
        let mut cinema = Settings::default();
        assert_eq!(cinema.cinema_row(), DEFAULT_CINEMA_ROW);
        cinema.set_environment(BackgroundEnvironment::Classic);
        cinema.set_cinema_row(5);
        cinema.set_cinema_curvature(0.5);
        cinema.save(&path).unwrap();
        let cinema = Settings::load(&path);
        assert_eq!(cinema.environment(), BackgroundEnvironment::Classic);
        assert_eq!(cinema.cinema_row(), 5);
        assert_eq!(cinema.cinema_curvature(), 0.5);
        let view = cinema.new_video_view();
        assert_eq!(view.environment, BackgroundEnvironment::UseGlobal);
        assert_eq!(view.cinema_row, None, "a new video follows the global row");
        assert_eq!(view.cinema_curvature, None, "and the global curve");
        assert_eq!(p.default_format, fp_core::format::VideoFormat::FALLBACK);
        assert_eq!(p.fisheye_presets, FISHEYE_PRESETS);
        assert_eq!(p.fisheye_start, FISHEYE_START);
        // Saved before the global chroma key: the old green default goes.
        std::fs::write(
            &path,
            r#"{"default_view": {"zoom": 1.5, "key_color": [0.0, 1.0, 0.0]}}"#,
        )
        .unwrap();
        let p = Settings::load(&path);
        assert_eq!(p.default_view.zoom, 1.5);
        assert_eq!(p.default_view.key_color, ViewSettings::default().key_color);
        assert_eq!(p.default_view.key_own, Some(false));
        // Saved before height: the right grip's free up / down gets it; one
        // set to nothing since stays so.
        let old =
            r#"{"default_view": {"zoom": 1.0}, "controls": {"right": {"grip_y": "nothing"}}}"#;
        std::fs::write(&path, old).unwrap();
        let height = crate::bindings::AxisAction::Height;
        assert_eq!(Settings::load(&path).controls.right.grip_y, height);
        let mut s = Settings::default();
        s.controls.right.grip_y = crate::bindings::AxisAction::Nothing;
        s.save(&path).unwrap();
        assert_eq!(Settings::load(&path), s);
        std::fs::write(&path, "garbage").unwrap();
        assert_eq!(Settings::load(&path).volume, 1.0);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn fisheye_presets_stay_in_order_without_repeats() {
        let mut presets = FISHEYE_PRESETS.to_vec();
        add_fisheye_preset(&mut presets, 129.6);
        add_fisheye_preset(&mut presets, 190.2);
        assert_eq!(presets, [130.0, 180.0, 190.0, 200.0, 220.0]);
        for angle in [140.0, 150.0, 160.0, 170.0, 250.0] {
            add_fisheye_preset(&mut presets, angle);
        }
        assert_eq!(presets.len(), MAX_FISHEYE_PRESETS);
        assert!(!presets.contains(&250.0), "full: {presets:?}");
    }

    #[test]
    fn removing_the_preset_fisheye_starts_at_moves_it_nearest_180() {
        let mut s = Settings {
            fisheye_start: 200.0,
            ..Settings::default()
        };
        s.remove_fisheye_preset(1); // 190°: not where it starts
        assert_eq!((s.fisheye_start, s.fisheye_presets.len()), (200.0, 3));
        s.remove_fisheye_preset(1); // 200°
        assert_eq!(s.fisheye_start, 180.0);
        s.remove_fisheye_preset(0); // 180°, leaving 220°
        assert_eq!(s.fisheye_start, 220.0);
        s.remove_fisheye_preset(0);
        assert_eq!(s.fisheye_start, FISHEYE_START, "none left");
        assert!(s.fisheye_presets_changed());
        s.reset_fisheye_presets();
        assert!(!s.fisheye_presets_changed());

        // Two as near: the smaller.
        s.fisheye_presets = vec![170.0, 180.0, 190.0];
        s.remove_fisheye_preset(1);
        assert_eq!(s.fisheye_start, 170.0);
    }
}
