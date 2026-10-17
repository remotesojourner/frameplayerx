//! The media currently open: player, format, adjustments, scripts,
//! subtitles and markers, plus progress bookkeeping in the library.

use crate::services::Opener;
use crate::settings::Settings;
use crate::subtitles::{self, Pick};
use crate::watch::{Reports, Watch};
use fp_core::format::{self, ContainerHints, DetectedFormat, Evidence, VideoFormat};
use fp_core::source::{Entry, EntryKind, SubtitleFile};
use fp_core::sync::lock;
use fp_core::view::{Keyframes, ViewSettings};
use fp_core::{PlaybackStatus, playback::now_ms};
use fp_haptics::{Axis, Heatmap, Script};
use fp_library::{Library, MediaId, SessionId};
use fp_media::{Player, PlayerConfig, PlayerState, StreamInfo};
use fp_sources::sidecar::subtitle_file;
use fp_sources::urlutil::last_segment;
use std::sync::Mutex;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

#[derive(Clone, Debug, Default)]
pub struct OpenRequest {
    pub location: String,
    pub source_id: Option<String>,
    /// The browse entry, when opened from a source (carries feed metadata).
    pub entry: Option<Entry>,
    /// Start here instead of the saved resume position.
    pub start_at: Option<f64>,
}

pub struct Playback {
    pub player: Player,
    pub request: OpenRequest,
    pub title: String,
    pub record_id: Option<MediaId>,
    session: Option<SessionId>,
    pub format: VideoFormat,
    pub evidence: Evidence,
    /// Format the file would have without the user's override.
    pub detected: DetectedFormat,
    pub settings: ViewSettings,
    pub keyframes: Keyframes,
    /// Adjustments differ from what is saved for this video.
    pub settings_dirty: bool,
    /// What is saved for this video (the Passthrough tab saves its part
    /// at once, the rest only with Save).
    saved_settings: ViewSettings,
    /// The chroma key changed and is not saved yet (see [`Self::save_key`]),
    /// and when it was last saved: slider drags save twice a second.
    key_pending: bool,
    key_saved_at: Instant,
    pub heatmap: Option<Heatmap>,
    pub markers: Vec<(f64, String)>,
    pub subtitle_files: Vec<SubtitleFile>,
    pub active_subtitle_file: Option<usize>,
    pub script_count: usize,
    last_save: Instant,
    pub notice: Option<String>,
    /// Last decoding check: when, and the player's stall and frame counts.
    decode_check: (Instant, u64, u64, u64),
    /// Tells the server the video came from how watching it goes.
    watch: Option<Watch>,
}

/// The result of opening on a background thread.
pub struct Opened {
    pub playback: Playback,
    pub scripts: Vec<(Axis, Script)>,
}

fn title_from_name(name: &str) -> String {
    let stem = name.rsplit_once('.').map(|(s, _)| s).unwrap_or(name);
    stem.replace(['_', '.'], " ").trim().to_string()
}

/// The format a source declared for a video: on the browse entry, or, when
/// opened from the library or home (no entry), as saved in its record. A
/// DeoVR scene's `_LR_ALPHA.json` name alone makes a flat screen of a
/// fisheye passthrough video: mask in the corners, no height or forward.
fn declared_format(
    entry: Option<&Entry>,
    record: Option<&fp_library::MediaRecord>,
) -> Option<VideoFormat> {
    entry.and_then(|e| e.format).or_else(|| {
        record
            .filter(|r| r.detected.evidence == Evidence::Metadata)
            .map(|r| r.detected.format)
    })
}

/// Opens media; blocking (network probing), so run it on a job thread.
pub fn open(
    req: OpenRequest,
    opener: &Opener,
    library: &Library,
    settings: &Settings,
    reports: &Reports,
) -> Result<Opened, String> {
    let name = last_segment(&req.location);
    let source = req.source_id.as_deref().and_then(|id| opener.source(id));
    // Feeds may need a details call to get the stream URL and metadata.
    let entry = match (&req.entry, &source) {
        (Some(e), Some(src)) => Some(src.details(e).unwrap_or_else(|_| e.clone())),
        // From the library or home: a server's details still carry the
        // video's subtitle files and chapters.
        (None, Some(src)) if src.as_media_browser().is_some() => {
            let e = Entry::new(name.clone(), req.location.clone(), EntryKind::Video);
            src.details(&e).ok()
        }
        (e, _) => e.clone(),
    };
    let location = entry
        .as_ref()
        .map_or_else(|| req.location.clone(), |e| e.location.clone());

    // Library record: existing, or created for remote entries (and deleted
    // again if the video doesn't open, so nothing invisible is left behind).
    let mut record = library.get_by_location(&location).ok().flatten();
    let mut created = None;
    if record.is_none()
        && let (Some(e), Some(sid)) = (&entry, &req.source_id)
        && library.upsert_entry(sid, e).is_ok()
    {
        record = library.get_by_location(&location).ok().flatten();
        created = record.as_ref().map(|r| r.id);
    }
    let failed = |e: String| {
        if let Some(id) = created
            && let Err(err) = library.remove_media(id)
        {
            log::warn!("forgetting {location}: {err}");
        }
        e
    };

    let src = opener
        .open_in(req.source_id.as_deref(), &location)
        .map_err(failed)?;
    // A server that keeps resume points knows where the video was left on
    // any device; the library only where it was left here.
    let start_at = req
        .start_at
        .or_else(|| {
            entry
                .as_ref()
                .and_then(|e| e.resume)
                .or_else(|| record.as_ref().map(|r| r.resume_position))
                .filter(|p| settings.resume && *p > 1.0)
        })
        .unwrap_or(0.0);
    // The previous video's decoder must be released first (see
    // App::open_request); a close stuck on the network is not waited out.
    if !wait_closed(CLOSE_WAIT) {
        log::warn!("previous video still closing; opening anyway");
    }
    let (script_locs, subs) = sidecars(
        &location,
        &name,
        entry.as_ref(),
        record.as_ref(),
        source.clone(),
        settings.haptics_enabled,
    );
    let mut pick = Pick::Off;
    let player = Player::open(src, &name, player_config(settings, start_at), |info| {
        pick = subtitles::choose(
            settings.subtitles,
            &settings.subtitle_language,
            info,
            &subs,
            &name,
        );
        pick.stream()
    })
    .map_err(|e| failed(format!("{name}: {e}")))?;
    let info = player.info().clone();
    if settings.hardware_decoding {
        warn_if_software(&name, &info, &player.stats());
    }
    let (resolved, detected) =
        detect_format(&name, &info, entry.as_ref(), record.as_ref(), settings);

    let scripts = load_scripts(opener, &script_locs);
    let duration_ms = (info.duration * 1000.0) as i64;
    let heatmap = scripts
        .iter()
        .find(|(a, _)| *a == Axis::L0)
        .filter(|_| duration_ms > 0)
        .map(|(_, s)| fp_haptics::heatmap_range(s, 0, duration_ms, 240));
    let active_subtitle_file = match pick {
        Pick::File(i) => load_subtitle_file(opener, &player, &subs[i])
            .ok()
            .map(|()| i),
        _ => None,
    };

    let settings_v = record
        .as_ref()
        .and_then(|r| r.view_settings)
        .unwrap_or_else(|| settings.new_video_view());
    let keyframes = record
        .as_ref()
        .map(|r| r.keyframes.clone())
        .unwrap_or_default();
    let title = record
        .as_ref()
        .map(|r| r.title.clone())
        .filter(|t| !t.is_empty())
        .or_else(|| info.title.clone())
        .or_else(|| entry.as_ref().map(|e| e.name.clone()))
        .unwrap_or_else(|| title_from_name(&name));
    let markers = markers(&info, entry.as_ref(), record.as_ref(), library);
    let record_id = record.as_ref().map(|r| r.id);
    let session = record_id.and_then(|id| library.start_playback(id).ok());
    let watch = source.and_then(|src| {
        let video = entry
            .clone()
            .unwrap_or_else(|| Entry::new(title.clone(), location.clone(), EntryKind::Video));
        Watch::start(reports, src, &video, start_at)
    });
    Ok(Opened {
        playback: Playback {
            player,
            request: OpenRequest { location, ..req },
            title,
            record_id,
            session,
            format: resolved.format,
            evidence: resolved.evidence,
            detected,
            settings: settings_v,
            saved_settings: settings_v,
            key_pending: false,
            key_saved_at: Instant::now(),
            keyframes,
            settings_dirty: false,
            heatmap,
            markers,
            subtitle_files: subs,
            active_subtitle_file,
            script_count: scripts.len(),
            last_save: Instant::now(),
            notice: None,
            decode_check: (Instant::now(), 0, 0, 0),
            watch,
        },
        scripts,
    })
}

fn player_config(settings: &Settings, start_at: f64) -> PlayerConfig {
    PlayerConfig {
        hw_decode: if settings.hardware_decoding {
            fp_media::HwDecode::Auto
        } else {
            fp_media::HwDecode::Off
        },
        audio: fp_media::audio::output::Backend::Alsa {
            device: settings.audio_device.clone(),
            latency_ms: 80,
        },
        start_at,
        volume: settings.volume,
        ..Default::default()
    }
}

/// Logs why a video the hardware could decode is decoded in software.
fn warn_if_software(name: &str, info: &fp_media::MediaInfo, st: &fp_media::Stats) {
    if !st.hardware && info.video_stream().is_some_and(has_hardware_decoder) {
        log::warn!(
            "{name}: no hardware decoder ({}), decoding with {}; {}",
            st.hardware_error.as_deref().unwrap_or("not tried"),
            st.video_decoder,
            hardware_holders()
        );
    }
}

/// The format to play the video in, and the one detected without the
/// user's override: user override > container metadata > declared by the
/// source > file name > aspect ratio > the default chosen in Settings.
fn detect_format(
    name: &str,
    info: &fp_media::MediaInfo,
    entry: Option<&Entry>,
    record: Option<&fp_library::MediaRecord>,
    settings: &Settings,
) -> (DetectedFormat, DetectedFormat) {
    let (w, h) = info
        .video_stream()
        .map(|v| (v.width, v.height))
        .unwrap_or((0, 0));
    let user = record.and_then(|r| r.user_format);
    let hints = if info.hints.projection.is_some() || info.hints.stereo.is_some() {
        info.hints
    } else if let Some(f) = declared_format(entry, record) {
        ContainerHints {
            projection: Some(f.projection),
            stereo: Some(f.stereo),
        }
    } else {
        info.hints
    };
    let unidentified = |mut d: DetectedFormat| {
        if d.evidence == Evidence::Default {
            d.format = settings.default_format;
        }
        d
    };
    let resolved = unidentified(format::resolve(user, hints, name, w, h));
    let detected = unidentified(format::resolve(None, hints, name, w, h));
    log::info!(
        "format {}{} ({}), {w}x{h}",
        resolved.format.label(),
        if resolved.format.alpha_packed {
            " with mask"
        } else {
            ""
        },
        resolved.evidence.label()
    );
    (resolved, detected)
}

/// Haptic script locations (with `with_scripts`) and subtitle files for the
/// video `name`: from its library record, its entry, then the source's
/// sidecar files; scripts of a local file not otherwise found are looked for
/// beside it and in the Interactive folder.
fn sidecars(
    location: &str,
    name: &str,
    entry: Option<&Entry>,
    record: Option<&fp_library::MediaRecord>,
    source: Option<std::sync::Arc<dyn fp_sources::Source>>,
    with_scripts: bool,
) -> (Vec<String>, Vec<SubtitleFile>) {
    let mut scripts: Vec<String> = record.map(|r| r.scripts.clone()).unwrap_or_default();
    let mut subs = Vec::new();
    if let Some(e) = entry {
        scripts.extend(e.scripts.iter().cloned());
        match source.and_then(|src| src.sidecars(e).ok()) {
            Some(side) => {
                scripts.extend(side.scripts.into_iter().map(|s| s.location));
                subs.extend(side.subtitles);
            }
            None => subs.extend(fp_sources::Sidecars::from_entry(e).subtitles),
        }
    }
    // The record knows only where its files are; their names tell the rest.
    if let Some(r) = record {
        subs.extend(r.subtitles.iter().map(|l| subtitle_file(l, name)));
    }
    if !with_scripts {
        scripts.clear();
    } else if scripts.is_empty() && location.starts_with('/') {
        let dirs = vec![fp_core::dirs::interactive_dir()];
        scripts = fp_haptics::funscript::find_scripts(std::path::Path::new(location), &dirs)
            .into_iter()
            .map(|(_, p)| p.display().to_string())
            .collect();
    }
    scripts.dedup();
    let mut seen = std::collections::HashSet::new();
    subs.retain(|s| seen.insert(s.location.clone()));
    (scripts, subs)
}

fn load_scripts(opener: &Opener, locations: &[String]) -> Vec<(Axis, Script)> {
    let mut scripts = Vec::new();
    for loc in locations {
        match opener.fetch(loc, 64 << 20).and_then(|bytes| {
            fp_haptics::funscript::parse_script_file(&last_segment(loc), &bytes)
                .map_err(|e| e.to_string())
        }) {
            Ok(mut s) => scripts.append(&mut s),
            Err(e) => log::warn!("script {loc}: {e}"),
        }
    }
    scripts
}

/// Shows a subtitle file in place of any other.
pub fn load_subtitle_file(
    opener: &Opener,
    player: &Player,
    file: &SubtitleFile,
) -> Result<(), String> {
    let loaded = opener.open(&file.location).and_then(|s| {
        player
            .load_subtitles(s, &last_segment(&file.location))
            .map_err(|e| e.to_string())
    });
    if let Err(e) = &loaded {
        log::warn!("subtitles {}: {e}", file.location);
    }
    loaded.map(|_| ())
}

/// Chapters, the source's markers and the user's bookmarks, in time order.
fn markers(
    info: &fp_media::MediaInfo,
    entry: Option<&Entry>,
    record: Option<&fp_library::MediaRecord>,
    library: &Library,
) -> Vec<(f64, String)> {
    let mut markers: Vec<(f64, String)> = info
        .chapters
        .iter()
        .map(|c| (c.start, c.title.clone()))
        .collect();
    if let Some(e) = entry {
        markers.extend(e.markers.iter().cloned());
    }
    if let Some(r) = record
        && let Ok(m) = library.markers(r.id)
    {
        markers.extend(m.into_iter().map(|m| (m.time, m.name)));
    }
    markers.sort_by(|a, b| a.0.total_cmp(&b.0));
    markers.dedup_by(|a, b| (a.0 - b.0).abs() < 0.5 && a.1 == b.1);
    markers
}

impl Playback {
    pub fn status(&self) -> PlaybackStatus {
        let p = &self.player;
        PlaybackStatus {
            location: self.request.location.clone(),
            title: self.title.clone(),
            duration: p.duration(),
            position: p.position(),
            speed: p.speed(),
            playing: matches!(p.state(), PlayerState::Playing),
            sampled_at_ms: now_ms(),
        }
    }

    /// View settings at the current position (keyframes applied).
    pub fn current_settings(&self) -> ViewSettings {
        self.keyframes.at(self.player.position(), &self.settings)
    }

    /// What is shown now: [`Self::current_settings`] with the global chroma
    /// key unless this video has its own.
    pub fn shown_settings(&self, global: &ViewSettings) -> ViewSettings {
        self.current_settings().with_global_key(global)
    }

    /// Saves progress every few seconds and at the end.
    pub fn save_progress(&mut self, library: &Library, force: bool) {
        if !force && self.last_save.elapsed().as_secs_f64() < 5.0 {
            return;
        }
        self.last_save = Instant::now();
        let finished = self.player.state() == PlayerState::Ended;
        if let Some(s) = self.session
            && let Err(e) = library.update_playback(s, self.player.position(), finished)
        {
            log::warn!("saving progress: {e}");
        }
    }

    /// Every frame: tells the video's server how watching goes.
    pub fn sync_watch(&mut self) {
        if let Some(w) = &mut self.watch {
            let p = &self.player;
            let state = p.state();
            w.update(
                p.position(),
                p.is_paused() || state == PlayerState::Ended,
                p.speed(),
                state != PlayerState::Buffering,
            );
        }
    }

    /// Every few seconds: when software decoding falls behind, says why in
    /// the control bar (the picture stutters with no other explanation).
    /// `hardware_on` is the hardware decoding setting.
    pub fn watch_decoding(&mut self, hardware_on: bool) {
        if self.decode_check.0.elapsed().as_secs_f64() < DECODE_CHECK_SECS {
            return;
        }
        let st = self.player.stats();
        let (_, stalls, dropped, shown) = self.decode_check;
        self.decode_check = (
            Instant::now(),
            st.stalls,
            st.frames_dropped,
            st.frames_shown,
        );
        if st.hardware || self.notice.is_some() || self.player.is_paused() {
            return;
        }
        if !falling_behind(
            st.stalls - stalls,
            st.frames_dropped - dropped,
            st.frames_shown - shown,
        ) {
            return;
        }
        let Some(v) = self.player.info().video_stream() else {
            return;
        };
        let notice = slow_decode_notice(v, hardware_on, st.hardware_error.as_deref());
        log::info!(
            "software decoding ({}, {} {}x{} {}) behind: {} stalls, {} of {} frames dropped; \
             hardware: {}; {}",
            st.video_decoder,
            v.codec,
            v.width,
            v.height,
            if v.pixel_format.is_empty() {
                "pixel format unknown"
            } else {
                &v.pixel_format
            },
            st.stalls - stalls,
            st.frames_dropped - dropped,
            st.frames_shown - shown + st.frames_dropped - dropped,
            match (&st.hardware_error, hardware_on) {
                (Some(e), _) => e.as_str(),
                (None, false) => "off in Settings",
                (None, true) => "not tried",
            },
            hardware_holders()
        );
        self.notice = Some(notice);
    }

    pub fn set_format(&mut self, library: &Library, format: Option<VideoFormat>) {
        self.format = format.unwrap_or(self.detected.format);
        self.evidence = if format.is_some() {
            Evidence::User
        } else {
            self.detected.evidence
        };
        if let Some(id) = self.record_id
            && let Err(e) = library.set_user_format(id, format)
        {
            log::warn!("saving format: {e}");
        }
    }

    /// This video's format with its see-through mask turned on or off, when
    /// it is a side-by-side VR video and so could carry one. There is no
    /// screen to show the room around, so this is what the Room button does.
    pub fn mask_toggled(&self) -> Option<VideoFormat> {
        let f = self.format;
        (f.projection != format::Projection::Flat && f.stereo.is_side_by_side()).then_some(
            VideoFormat {
                alpha_packed: !f.alpha_packed,
                ..f
            },
        )
    }

    pub fn save_settings(&mut self, library: &Library) {
        if let Some(id) = self.record_id {
            let r = library
                .set_view_settings(id, Some(&self.settings))
                .and_then(|_| library.set_keyframes(id, &self.keyframes));
            match r {
                Ok(()) => {
                    self.settings_dirty = false;
                    self.saved_settings = self.settings;
                }
                Err(e) => log::warn!("saving adjustments: {e}"),
            }
        } else {
            self.notice = Some(
                "Adjustments apply until you close this video (it is not in the library).".into(),
            );
        }
    }

    /// Saves this video's chroma key (the Passthrough tab) without the
    /// other, unsaved adjustments: the tab promises it is kept with the
    /// video, and toggling it every time was the result of waiting for Save.
    pub fn save_key(&mut self, library: &Library, force: bool) {
        if !self.key_pending || (!force && self.key_saved_at.elapsed().as_secs_f64() < 0.5) {
            return;
        }
        self.key_pending = false;
        self.key_saved_at = Instant::now();
        let Some(id) = self.record_id else {
            return;
        };
        let mut saved = self.saved_settings;
        saved.set_key(&self.settings);
        saved.key_own = self.settings.key_own;
        match library.set_view_settings(id, Some(&saved)) {
            Ok(()) => {
                self.saved_settings = saved;
                self.settings_dirty = self.settings != saved;
            }
            Err(e) => log::warn!("saving the background removal: {e}"),
        }
    }

    /// The Passthrough tab changed the chroma key; [`Self::save_key`] saves it.
    pub fn key_changed(&mut self) {
        self.key_pending = true;
    }

    pub fn add_bookmark(&mut self, library: &Library) -> fp_library::Result<()> {
        let t = self.player.position();
        let name = format!("Bookmark {}", crate::ui::fmt_time(t));
        if let Some(id) = self.record_id {
            library.add_marker(id, t, &name)?;
        }
        self.markers.push((t, name));
        self.markers.sort_by(|a, b| a.0.total_cmp(&b.0));
        Ok(())
    }

    /// Saves where the video was and closes it. The player's threads are
    /// stopped and joined on a thread of their own: one can be inside a
    /// network read or a long software decode, and waiting for it here
    /// froze the headset's frame loop (quitting while a 10-bit file over
    /// SMB was buffering hung the app).
    pub fn close(mut self, library: &Library) {
        self.save_progress(library, true);
        self.save_key(library, true);
        if let Some(w) = self.watch.take() {
            // Near the end, the server counts it as watched.
            w.stop(self.player.position());
        }
        let player = self.player;
        let closing = std::thread::Builder::new()
            .name("fp-close".into())
            .spawn(move || {
                let t = Instant::now();
                drop(player);
                log::info!("video closed in {} ms", t.elapsed().as_millis());
            });
        match closing {
            Ok(h) => lock(&CLOSING).push(h),
            // The player was dropped (closed) here.
            Err(e) => log::warn!("cannot start close thread: {e}"),
        }
    }
}

/// Who in FramePlayerX holds hardware decoders, for the log when a video
/// cannot get one: sessions left by videos that never finished closing
/// keep every later video in software until a restart.
fn hardware_holders() -> String {
    let closing = lock(&CLOSING).iter().filter(|h| !h.is_finished()).count();
    format!(
        "FramePlayerX holds {} hardware decoder session(s), {closing} closed video(s) still closing",
        fp_media::hardware_sessions()
    )
}

/// Players still closing (see [`Playback::close`]).
static CLOSING: Mutex<Vec<JoinHandle<()>>> = Mutex::new(Vec::new());

/// How long opening a video waits for the previous one to close.
const CLOSE_WAIT: Duration = Duration::from_secs(5);

/// Waits up to `limit` for closing players to finish; true when none is
/// left.
pub fn wait_closed(limit: Duration) -> bool {
    let deadline = Instant::now() + limit;
    let mut closing = lock(&CLOSING);
    loop {
        let (done, open): (Vec<_>, Vec<_>) = std::mem::take(&mut *closing)
            .into_iter()
            .partition(JoinHandle::is_finished);
        for h in done {
            let _ = h.join();
        }
        *closing = open;
        if closing.is_empty() {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        drop(closing);
        std::thread::sleep(Duration::from_millis(10));
        closing = lock(&CLOSING);
    }
}

const DECODE_CHECK_SECS: f64 = 10.0;

/// Over one check period: playback stalled more than once waiting for the
/// decoder, or dropped more than 5% of frames.
fn falling_behind(stalls: u64, dropped: u64, shown: u64) -> bool {
    stalls >= 2 || dropped * 20 > dropped + shown
}

/// Whether the Frame's hardware decoder takes this stream.
fn has_hardware_decoder(v: &StreamInfo) -> bool {
    matches!(v.codec.as_str(), "hevc" | "h264" | "vp9") && v.bit_depth <= 8 && v.chroma() == "4:2:0"
}

/// The stream's codec, with its bit depth and chroma when the hardware
/// decoder lacks them ("10-bit HEVC", "4:2:2 HEVC").
fn stream_desc(v: &StreamInfo) -> String {
    let codec = match v.codec.as_str() {
        "hevc" => "HEVC".to_string(),
        "h264" => "H.264".to_string(),
        "av1" => "AV1".to_string(),
        "vp9" => "VP9".to_string(),
        c => c.to_uppercase(),
    };
    // The hardware decodes 8-bit 4:2:0 only: say which of the two it isn't.
    let chroma = v.chroma();
    match (v.bit_depth > 8, chroma != "4:2:0") {
        (true, true) => format!("{}-bit {chroma} {codec}", v.bit_depth),
        (true, false) => format!("{}-bit {codec}", v.bit_depth),
        (false, true) => format!("{chroma} {codec}"),
        (false, false) => codec,
    }
}

/// Why a video decodes in software, for the stream info: the hardware
/// decoder's error when it was tried, else why it wasn't.
pub(crate) fn software_reason(
    v: &StreamInfo,
    hardware_on: bool,
    hardware_error: Option<&str>,
) -> String {
    match hardware_error {
        Some(e) => e.to_string(),
        None if !has_hardware_decoder(v) => {
            format!("no hardware decoding for {}", stream_desc(v))
        }
        None if !hardware_on => "hardware decoding is off in Settings".to_string(),
        None => "hardware decoder not tried".to_string(),
    }
}

/// Why a software-decoded video stutters, and what plays smoothly instead.
/// `hardware_on` is the hardware decoding setting; `hardware_error` why the
/// hardware decoder, when tried, is not decoding.
pub(crate) fn slow_decode_notice(
    v: &StreamInfo,
    hardware_on: bool,
    hardware_error: Option<&str>,
) -> String {
    let desc = stream_desc(v);
    if !has_hardware_decoder(v) {
        return format!(
            "No hardware decoding for {desc}, so it stutters. 8-bit 4:2:0 H.265 plays smoothly"
        );
    }
    if !hardware_on {
        return format!("{desc} in software stutters: turn on hardware decoding in Settings");
    }
    // Out of decoder memory or sessions: other apps (Steam's streaming
    // among them) hold the hardware decoder.
    let busy = hardware_error.is_none_or(|e| {
        let e = e.to_ascii_lowercase();
        e.contains("memory") || e.contains("busy") || e.contains("no free")
    });
    if busy {
        format!("Hardware decoder busy: {desc} in software stutters")
    } else {
        format!(
            "The hardware decoder can't play this {desc} ({}x{}), so it stutters in software",
            v.width, v.height
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn titles_from_names() {
        assert_eq!(title_from_name("My.Video_2024.mkv"), "My Video 2024");
    }

    #[test]
    fn library_opens_keep_the_format_the_source_declared() {
        use fp_core::format::{Projection, StereoLayout};
        use fp_core::source::EntryKind;
        let lib = Library::open_in_memory().unwrap();
        let loc = "http://127.0.0.1:1/deovr/scene/7/Room_Scene_7_LR_ALPHA.json";
        let mut e = Entry::new("Room Scene", loc, EntryKind::Video);
        e.format = Some(VideoFormat::new(
            Projection::fisheye(190.0),
            StereoLayout::SideBySide,
        ));
        lib.upsert_entry("feed", &e).unwrap();
        let record = lib.get_by_location(loc).unwrap();
        let resolve = |declared: Option<VideoFormat>| {
            let hints = declared.map_or_else(ContainerHints::default, |f| ContainerHints {
                projection: Some(f.projection),
                stereo: Some(f.stereo),
            });
            format::resolve(None, hints, &last_segment(loc), 2160, 1080).format
        };

        // From the source's browser and from the library or home (no
        // entry): a masked fisheye, not a flat screen.
        for declared in [
            declared_format(Some(&e), None),
            declared_format(None, record.as_ref()),
        ] {
            let f = resolve(declared);
            assert_eq!(f.projection, Projection::fisheye(190.0));
            assert_eq!(f.stereo, StereoLayout::SideBySide);
            assert!(f.alpha_packed);
        }
        // What the name alone gives.
        assert_eq!(resolve(None).projection, Projection::Flat);

        // A format guessed from the name or shape is not a declaration.
        let local = "/v/Clip_LR.mp4";
        lib.upsert(&fp_library::MediaUpsert::new(
            local,
            crate::services::LIBRARY_SOURCE,
        ))
        .unwrap();
        let r = lib.get_by_location(local).unwrap();
        assert_eq!(declared_format(None, r.as_ref()), None);
    }

    #[test]
    fn opening_waits_for_closing_videos_but_not_forever() {
        let (tx, rx) = std::sync::mpsc::channel::<()>();
        let slow = std::thread::spawn(move || {
            let _ = rx.recv();
        });
        CLOSING.lock().unwrap().push(slow);
        assert!(!wait_closed(Duration::from_millis(50)), "still closing");
        drop(tx);
        assert!(wait_closed(Duration::from_secs(5)));
        assert!(CLOSING.lock().unwrap().is_empty());
    }

    #[test]
    fn slow_decoding_is_noticed_and_explained() {
        assert!(!falling_behind(0, 0, 600));
        assert!(!falling_behind(1, 10, 590), "one stall, few drops");
        assert!(falling_behind(2, 0, 400));
        assert!(falling_behind(0, 40, 560));
        assert!(!falling_behind(0, 0, 0));
        let stream = |codec: &str, bit_depth| StreamInfo {
            index: 0,
            kind: fp_media::StreamKind::Video,
            codec: codec.into(),
            language: None,
            title: None,
            default: true,
            forced: false,
            hearing_impaired: false,
            width: 8192,
            height: 4096,
            fps: 59.94,
            bit_depth,
            pixel_format: if bit_depth > 8 {
                "yuv420p10le".into()
            } else {
                "yuv420p".into()
            },
            interlaced: false,
            transfer: Default::default(),
            rotation: 0.0,
            sample_rate: 0,
            channels: 0,
            ambisonic: false,
        };
        let n = slow_decode_notice(&stream("hevc", 10), true, None);
        assert!(
            n.contains("10-bit HEVC") && n.contains("8-bit 4:2:0 H.265"),
            "{n}"
        );
        let n = slow_decode_notice(&stream("av1", 8), true, None);
        assert!(n.contains("for AV1,"), "{n}");
        let busy = "V4L2 REQBUFS: Cannot allocate memory (os error 12)";
        let n = slow_decode_notice(&stream("hevc", 8), true, Some(busy));
        assert!(n.contains("busy"), "{n}");
        // 8-bit, but 4:2:2: the hardware can't, whatever Windows says.
        let mut s422 = stream("hevc", 8);
        s422.pixel_format = "yuv422p".into();
        let n = slow_decode_notice(&s422, true, None);
        assert!(n.contains("for 4:2:2 HEVC"), "{n}");
        let n = slow_decode_notice(&stream("hevc", 8), false, None);
        assert!(n.contains("turn on hardware decoding"), "{n}");
        let refused = "V4L2: stream size unknown";
        let n = slow_decode_notice(&stream("hevc", 8), true, Some(refused));
        assert!(n.contains("can't play this HEVC (8192x4096)"), "{n}");

        // The stream info's reason: the error when tried, else why not.
        let r = software_reason(&stream("hevc", 10), true, None);
        assert_eq!(r, "no hardware decoding for 10-bit HEVC");
        let r = software_reason(&stream("hevc", 10), false, None);
        assert_eq!(r, "no hardware decoding for 10-bit HEVC");
        let r = software_reason(&stream("hevc", 8), false, None);
        assert_eq!(r, "hardware decoding is off in Settings");
        let r = software_reason(&stream("hevc", 8), true, Some(busy));
        assert_eq!(r, busy);
    }
}
