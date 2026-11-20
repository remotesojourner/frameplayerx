//! The media currently open: player, format, adjustments, scripts,
//! subtitles and markers, plus progress bookkeeping in the library.

use crate::services::Opener;
use crate::settings::Settings;
use crate::subtitles::{self, Pick};
use crate::watch::{Reports, Watch};
use fp_core::format::{self, ContainerHints, DetectedFormat, Evidence, VideoFormat};
use fp_core::source::{Entry, EntryKind, SubtitleFile};
use fp_core::sync::lock;
use fp_core::view::{BackgroundEnvironment, Keyframes, ViewSettings};
use fp_core::{PlaybackStatus, playback::now_ms};
use fp_haptics::{Axis, Heatmap, Script};
use fp_library::{Library, MediaId, SessionId};
use fp_media::{Cue, Player, PlayerConfig, PlayerState, StreamInfo};
use fp_sources::sidecar::subtitle_file;
use fp_sources::urlutil::last_segment;
use fp_sources::{Source, TranscodeTarget};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

#[derive(Clone, Debug, Default)]
pub struct OpenRequest {
    pub location: String,
    pub source_id: Option<String>,
    /// The browse entry, when opened from a source (carries feed metadata).
    pub entry: Option<Entry>,
    /// The library record's title, when opened from the library.
    pub title: Option<String>,
    /// Start here instead of the saved resume position.
    pub start_at: Option<f64>,
    /// The file as it is, or transcoded by the server.
    pub quality: Quality,
    /// What to ask the server to transcode the file to
    /// ([`TranscodeTarget::id`]): what [`Quality::Transcode`] names, or
    /// what Automatic fell back to.
    pub transcode: Option<String>,
    /// Stable original server track IDs, rather than transcoded stream indexes.
    pub source_audio: Option<String>,
    /// "0" explicitly disables server subtitles.
    pub source_subtitle: Option<String>,
    /// Keep the current pause state while changing quality or server tracks.
    pub paused: bool,
}

/// Whether the file plays as it is or the server transcodes it, as the
/// Quality list in the audio and subtitles picker offers them.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum Quality {
    /// The file; when it can't play here, the server transcodes it to the
    /// tallest height it offers.
    #[default]
    Automatic,
    /// The file, whatever happens.
    Original,
    /// The server transcodes it to this [`TranscodeTarget::id`].
    Transcode(String),
}

impl OpenRequest {
    /// A persistent resource can only be opened through its embedded owner.
    pub(crate) fn bound_source_id(&self) -> Result<Option<String>, String> {
        if !fp_sources::is_source_location(&self.location) {
            return Ok(self.source_id.clone());
        }
        let owner = fp_sources::location_owner(&self.location).ok_or(tr!("open-no-source"))?;
        if self.source_id.as_deref().is_some_and(|id| id != owner) {
            return Err(tr!("open-no-source").into());
        }
        Ok(Some(owner))
    }

    /// What to call the video while it opens: the entry's or the record's
    /// title, else its file name.
    pub fn shown_title(&self) -> String {
        self.entry
            .as_ref()
            .map(|e| e.name.clone())
            .or_else(|| self.title.clone())
            .filter(|t| !t.is_empty())
            .unwrap_or_else(|| title_from_name(&last_segment(&self.location)))
    }
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
    pub backdrop: BackdropSearch,
    /// No colour is picked for this video's chroma key, so it starts with
    /// the one found in it: none had its own key when opened, and none was
    /// picked since.
    colour_open: bool,
    pub heatmap: Option<Heatmap>,
    pub markers: Vec<Marker>,
    pub(crate) trickplay: Option<crate::trickplay::Preview>,
    pub subtitle_files: Vec<SubtitleFile>,
    pub active_subtitle_file: Option<usize>,
    pub managed_subtitles: Vec<fp_library::UserSubtitle>,
    pub subtitle_signature: fp_library::OriginalSubtitleSignature,
    /// The subtitles Subtitles on / off last turned off, to turn back on.
    pub last_subtitles: Option<Pick>,
    /// The audio and subtitles picked for this video in the player (see
    /// [`Self::save_tracks`]).
    pub picked: fp_library::PickedTracks,
    pub script_count: usize,
    last_save: Instant,
    pub notice: Option<Notice>,
    /// Last decoding check: when, and the player's stall and frame counts.
    decode_check: (Instant, u64, u64, u64),
    /// Damaged stretches jumped over so far, and the seconds of video they
    /// held in all, for the notice.
    damaged_jumps: u64,
    damaged_skipped: f64,
    /// When the jump over a damaged stretch now under way began.
    skipping_since: Option<Instant>,
    /// Tells the server the video came from how watching it goes.
    watch: Option<Watch>,
    /// What the server can transcode the video to, tallest first.
    pub targets: Vec<TranscodeTarget>,
    /// What the server is transcoding it to; `None` for the file as it is.
    pub transcoding: Option<TranscodeTarget>,
    pub source_tracks: Vec<fp_sources::PlaybackTrack>,
}

/// The size, each way, the backdrop's picture is scaled to fit: plenty of
/// samples to tell a backdrop, cheap to decode into.
const BACKDROP_PICTURE: u32 = 160;

/// Where the search for a video's backdrop colour has got to.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Backdrop {
    /// Not started yet, or its picture is being decoded.
    Looking,
    /// Its colour at full strength ([`fp_render::backdrop::pure`]).
    Found([f32; 3]),
    /// The middle of the video shows none.
    NotFound,
}

/// The search for a video's backdrop colour, the colour its chroma key
/// starts with: the keyframe in the middle of the video, so a studio's
/// intro doesn't decide it, decoded in software by a second reader of the
/// file playback opened (for a transcode, the original).
pub struct BackdropSearch {
    source_id: Option<String>,
    location: String,
    pub state: Backdrop,
    answer: Option<mpsc::Receiver<Option<[f32; 3]>>>,
    /// Stops the search's reads when the video closes.
    stop: Arc<AtomicBool>,
}

impl BackdropSearch {
    pub fn new(source_id: Option<String>, location: String) -> BackdropSearch {
        BackdropSearch {
            source_id,
            location,
            state: Backdrop::Looking,
            answer: None,
            stop: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Starts the search on a thread of its own, once. `duration` stands in
    /// when the file doesn't say how long it is.
    pub fn start(&mut self, opener: Arc<Opener>, duration: f64) {
        if self.answer.is_some() || self.state != Backdrop::Looking {
            return;
        }
        let (tx, rx) = mpsc::channel();
        let (source_id, location, stop) = (
            self.source_id.clone(),
            self.location.clone(),
            self.stop.clone(),
        );
        let spawned = std::thread::Builder::new()
            .name("backdrop".into())
            .spawn(move || {
                crate::prober::background();
                let began = Instant::now();
                let found =
                    middle_backdrop(&opener, source_id.as_deref(), &location, duration, stop)
                        .unwrap_or_else(|e| {
                            log::warn!("looking for the backdrop of {location}: {e}");
                            None
                        });
                log::info!(
                    "backdrop search: {:.2} s, {}",
                    began.elapsed().as_secs_f64(),
                    if found.is_some() { "found" } else { "none" }
                );
                let _ = tx.send(found);
            });
        match spawned {
            Ok(_) => self.answer = Some(rx),
            Err(e) => {
                log::warn!("looking for the backdrop: {e}");
                self.state = Backdrop::NotFound;
            }
        }
    }

    /// Takes the search's answer once it has one.
    pub fn poll(&mut self) {
        let Some(rx) = &self.answer else {
            return;
        };
        let found = match rx.try_recv() {
            Ok(found) => found,
            Err(mpsc::TryRecvError::Empty) => return,
            Err(mpsc::TryRecvError::Disconnected) => None,
        };
        self.answer = None;
        self.state = match found {
            Some(c) => {
                let pure = fp_render::backdrop::pure(c);
                log::info!("the video's backdrop is sRGB {c:.2?}, keyed as {pure:.2?}");
                Backdrop::Found(pure)
            }
            None => Backdrop::NotFound,
        };
    }
}

impl Drop for BackdropSearch {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
    }
}

/// The backdrop in the keyframe at the middle of `location`, the file read
/// anew through `opener`; `stop` makes its reads fail.
fn middle_backdrop(
    opener: &Opener,
    source_id: Option<&str>,
    location: &str,
    duration: f64,
    stop: Arc<AtomicBool>,
) -> Result<Option<[f32; 3]>, String> {
    let src = opener.open_in(source_id, location)?;
    let mut grabber = fp_media::thumb::Grabber::open(src, &last_segment(location), stop)
        .map_err(|e| e.to_string())?;
    let length = Some(grabber.info().duration)
        .filter(|d| *d > 0.0)
        .unwrap_or(duration);
    // A still or a moment's clip has no middle to seek to.
    let middle = if length > 1.0 { length / 2.0 } else { 0.0 };
    let picture = grabber
        .grab(
            middle,
            BACKDROP_PICTURE,
            BACKDROP_PICTURE,
            fp_media::thumb::Crop::Full,
        )
        .map_err(|e| e.to_string())?;
    Ok(fp_render::backdrop::find(&picture.pixels))
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

/// What is saved with a video: not where a flat screen was moved to, as it
/// opens where the browse panel is. A 180°/360° picture keeps its turn.
fn kept_with_video(mut s: ViewSettings, format: VideoFormat) -> ViewSettings {
    if format.projection == format::Projection::Flat {
        s.yaw = 0.0;
        s.pitch = 0.0;
    }
    s
}

/// Opens media; blocking (network probing), so run it on a job thread.
pub fn open(
    mut req: OpenRequest,
    opener: &Opener,
    library: &Library,
    settings: &Settings,
    reports: &Reports,
) -> Result<Opened, String> {
    req.source_id = req.bound_source_id()?;
    let legacy_record = (!fp_sources::is_source_location(&req.location))
        .then(|| library.get_by_location(&req.location).ok().flatten())
        .flatten()
        .filter(|record| req.source_id.as_deref() == Some(record.source_id.as_str()));
    let name = last_segment(&req.location);
    let source = req.source_id.as_deref().and_then(|id| opener.source(id));
    // Feeds may need a details call to get the stream URL and metadata.
    let entry = match (&req.entry, &source) {
        (Some(e), Some(src)) if fp_sources::is_source_location(&e.location) => {
            Some(src.details(e).map_err(|e| crate::ui::errors::source(&e))?)
        }
        (Some(e), Some(src)) => Some(src.details(e).unwrap_or_else(|_| e.clone())),
        // From the library or home: a server's details still carry the
        // video's subtitle files and chapters. Its address only names the
        // server, so it goes by the title the library has until then.
        (None, Some(src))
            if fp_sources::is_source_location(&req.location)
                || src.canonical_location(&req.location).is_some()
                || src.as_catalog().is_some() =>
        {
            let title = req.title.clone().unwrap_or_else(|| name.clone());
            let e = Entry::new(title, req.location.clone(), EntryKind::Video);
            let details = src.details(&e);
            if fp_sources::is_source_location(&req.location) {
                Some(details.map_err(|e| crate::ui::errors::source(&e))?)
            } else {
                details.ok()
            }
        }
        (e, _) => e.clone(),
    };
    if let Some(entry) = &entry
        && fp_sources::is_source_location(&entry.location)
    {
        let owner = fp_sources::location_owner(&entry.location).ok_or(tr!("open-no-source"))?;
        if req.source_id.as_deref() != Some(owner.as_str()) {
            return Err(tr!("open-no-source").into());
        }
    }
    let location = entry
        .as_ref()
        .map_or_else(|| req.location.clone(), |e| e.location.clone());
    // What messages call it: a Plex location names no file.
    let label = if name.is_empty() {
        entry
            .as_ref()
            .map_or_else(|| req.shown_title(), |e| e.name.clone())
    } else {
        name.clone()
    };

    // Library record: existing, or created for remote entries (and deleted
    // again if the video doesn't open, so nothing invisible is left behind).
    // A colliding legacy record is kept during migration rather than losing
    // its user data. Opening that history row must retain its own record ID.
    let mut record = legacy_record.or_else(|| library.get_by_location(&location).ok().flatten());
    let mut created = None;
    if let (Some(e), Some(sid)) = (&entry, &req.source_id) {
        match &record {
            None => {
                if library.upsert_entry(sid, e).is_ok() {
                    record = library.get_by_location(&location).ok().flatten();
                    created = record.as_ref().map(|r| r.id);
                }
            }
            // What its own source says now, as titles are kept current; a
            // video another source has (a library's) is left to it.
            Some(r) if &r.source_id == sid => {
                if let Ok(u) = fp_library::MediaUpsert::from_entry(sid, e)
                    && let Err(err) = library.update_details(
                        r.id,
                        &fp_library::SourceDetails {
                            title: Some(u.title.as_str()).filter(|t| !t.is_empty()),
                            description: u.description.as_deref(),
                            thumbnail_url: u.thumbnail_url.as_deref(),
                            tags: u.tags.as_deref(),
                        },
                    )
                {
                    log::warn!("updating the details of {location}: {err}");
                }
                record = library.get(r.id).ok().flatten();
            }
            Some(_) => {}
        }
    }
    let failed = |e: String| {
        if let Some(id) = created
            && let Err(err) = library.remove_media(id)
        {
            log::warn!("forgetting {location}: {err}");
        }
        e
    };

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
    let (script_locs, mut subs) = sidecars(
        &location,
        &name,
        entry.as_ref(),
        record.as_ref(),
        source.clone(),
        settings.haptics_enabled,
    );
    let picked = record
        .as_ref()
        .map(|r| r.tracks.clone())
        .unwrap_or_default();
    let managed_subtitles = record.as_ref().map_or_else(Vec::new, |r| {
        library.user_subtitles(r.id).unwrap_or_else(|err| {
            log::warn!("reading subtitle downloads: {err}");
            Vec::new()
        })
    });
    let mut original = subtitle_signature(
        &location,
        entry
            .as_ref()
            .and_then(|e| e.size)
            .or_else(|| record.as_ref().and_then(|r| r.size)),
        entry
            .as_ref()
            .and_then(|e| e.modified)
            .or_else(|| record.as_ref().and_then(|r| r.mtime)),
    );
    if managed_subtitles.iter().any(|s| {
        s.original_signature
            .as_ref()
            .is_some_and(|s| s.moviehash.is_some())
    }) && let Some(file) = local_subtitle_original(&location)
        && let Ok(Some(hash)) = fp_sources::media_identity::movie_hash(
            &file,
            Instant::now() + Duration::from_secs(1),
            || false,
        )
    {
        original.moviehash = Some(hash.hash);
    }
    let mut eligible: Vec<usize> = (0..subs.len()).collect();
    for attachment in &managed_subtitles {
        let file = attachment.as_file();
        let present = opener.open(&file.location).is_ok();
        let changed = attachment
            .original_signature
            .as_ref()
            .is_some_and(|signature| signature.differs_from(&original));
        if present && !changed {
            eligible.push(subs.len());
        }
        subs.push(file);
    }
    let starting_files: Vec<_> = eligible.iter().map(|&i| subs[i].clone()).collect();
    let source_tracks = source
        .as_ref()
        .filter(|s| s.kind() == fp_sources::SourceKind::Plex)
        .map(|src| {
            let video = entry
                .clone()
                .unwrap_or_else(|| Entry::new(name.clone(), location.clone(), EntryKind::Video));
            src.playback_tracks(&video).unwrap_or_default()
        })
        .unwrap_or_default();
    // What was picked for this video, as the server's track IDs: a transcode
    // has only the tracks the server puts in it.
    if !source_tracks.is_empty() {
        use_saved_source_tracks(&mut req, &picked, &source_tracks);
    }
    let open_player = |src: Arc<dyn fp_core::ByteSource>, config: PlayerConfig| {
        let direct = config.timeline.is_none();
        let server_tracks = !direct && !source_tracks.is_empty();
        let mut pick = Pick::Off;
        Player::open(src, &name, config, |info| {
            let audio = if server_tracks {
                None
            } else {
                req.source_audio
                    .as_ref()
                    .and_then(|id| source_tracks.iter().find(|t| !t.subtitle && t.id == *id))
                    .and_then(|t| t.index)
                    .or(picked.audio)
            };
            pick = subtitles::starting(
                picked.subtitles.as_ref(),
                audio,
                settings.subtitles,
                &settings.subtitle_language,
                info,
                &starting_files,
                &name,
            );
            if let Pick::File(i) = pick {
                pick = Pick::File(eligible[i]);
            }
            // A local file picked for this video beats the server subtitle a
            // request carries over.
            let explicit_local = match (pick, &picked.subtitles) {
                (Pick::File(i), Some(fp_library::PickedSubtitles::File(location))) => {
                    let file = &subs[i].location;
                    file == location
                        && !source_tracks
                            .iter()
                            .any(|t| t.subtitle && t.location.as_ref() == Some(file))
                }
                _ => false,
            };
            if !explicit_local && let Some(id) = req.source_subtitle.as_deref() {
                let track = source_tracks.iter().find(|t| t.subtitle && t.id == id);
                pick = track
                    .and_then(|t| {
                        t.location
                            .as_ref()
                            .and_then(|location| subs.iter().position(|s| &s.location == location))
                            .map(Pick::File)
                            .or_else(|| t.index.map(Pick::Stream))
                    })
                    .unwrap_or(Pick::Off);
            }
            if server_tracks && !explicit_local {
                pick = Pick::Off;
            }
            fp_media::Tracks {
                audio,
                subtitle: pick.stream(),
            }
        })
        .map(|p| (p, pick))
    };
    let mut source_session = source.as_ref().and_then(|src| {
        let video = entry
            .clone()
            .unwrap_or_else(|| Entry::new(name.clone(), location.clone(), EntryKind::Video));
        src.watch_session(&video)
    });
    if source
        .as_ref()
        .is_some_and(|s| s.kind() == fp_sources::SourceKind::Plex)
        && let Some(session) = source_session.as_mut()
    {
        if req.source_audio.is_some() {
            session.audio_stream = req.source_audio.clone();
        }
        session.subtitle_stream = Some(req.source_subtitle.clone().unwrap_or_else(|| "0".into()));
    }
    let targets = source
        .as_ref()
        .map(|src| {
            let video = entry
                .clone()
                .unwrap_or_else(|| Entry::new(name.clone(), location.clone(), EntryKind::Video));
            src.transcode_targets(&video)
        })
        .unwrap_or_default();
    let user_format = record.as_ref().and_then(|r| r.user_format);
    let duration = entry
        .as_ref()
        .and_then(|e| e.duration)
        .or_else(|| record.as_ref().and_then(|r| r.duration))
        .unwrap_or(0.0);
    // From where it was asked to start when the server can start it there
    // (and so seek), else from the beginning.
    let transcoded = |t: &TranscodeTarget| {
        let fail = |e: String| format!("{label} ({}): {e}", t.label);
        let src = source.clone().ok_or_else(|| fail("no server".into()))?;
        let restart = restart_at(src, &location, t, source_session.clone());
        let first = if t.seekable { start_at } else { 0.0 };
        let stream = restart(first).map_err(fail)?;
        let config = PlayerConfig {
            timeline: Some(fp_media::Timeline {
                start: first,
                duration,
                restart: t.seekable.then_some(restart),
            }),
            ..player_config(settings, 0.0, user_format)
        };
        open_player(stream, config).map_err(|e| fail(crate::ui::errors::media(&e)))
    };
    let asked = req
        .transcode
        .as_ref()
        .and_then(|id| targets.iter().find(|t| &t.id == id))
        .cloned();
    let mut notice = None;
    let (player, pick, transcoding) = match asked {
        Some(t) => {
            let (p, pick) = transcoded(&t).map_err(failed)?;
            (p, pick, Some(t))
        }
        None => {
            let src = match (&source, &source_session) {
                (Some(src), Some(session)) => match src.open_playback(&location, session) {
                    Ok(bytes) => Ok(Some(bytes)),
                    Err(e)
                        if src.kind() == fp_sources::SourceKind::Plex
                            && req.quality == Quality::Automatic
                            && !targets.is_empty()
                            && matches!(e, fp_sources::Error::Unsupported(_)) =>
                    {
                        log::info!("{label}: {e}; asking Plex to transcode");
                        Ok(None)
                    }
                    Err(e) => Err(crate::ui::errors::source(&e)),
                },
                _ => opener
                    .open_in(req.source_id.as_deref(), &location)
                    .map(Some),
            }
            .map_err(failed)?;
            match src.map(|src| open_player(src, player_config(settings, start_at, user_format))) {
                Some(Ok((p, pick))) => (p, pick, None),
                Some(Err(e))
                    if e.cannot_play_here()
                        && req.quality == Quality::Automatic
                        && !targets.is_empty() =>
                {
                    let t = targets[0].clone();
                    log::warn!("{label}: {e}; the server transcodes it to {}", t.label);
                    let (p, pick) = transcoded(&t).map_err(failed)?;
                    notice = Some(Notice::new(
                        tr!("notice-pill-transcoding"),
                        tr!("transcode-cannot-play", quality = t.label.as_str()),
                    ));
                    (p, pick, Some(t))
                }
                Some(Err(e)) => {
                    return Err(failed(format!("{label}: {}", crate::ui::errors::media(&e))));
                }
                None => {
                    let t = targets[0].clone();
                    let (p, pick) = transcoded(&t).map_err(failed)?;
                    notice = Some(Notice::new(
                        tr!("notice-pill-transcoding"),
                        tr!("transcode-cannot-play", quality = t.label.as_str()),
                    ));
                    (p, pick, Some(t))
                }
            }
        }
    };
    // Only for this open: the request is cloned for later ones (a Loop
    // restart, Up next), which start playing.
    if std::mem::take(&mut req.paused) {
        player.pause();
    }
    let info = player.info().clone();
    if settings.hardware_decoding {
        warn_if_software(&label, &info, &player.stats());
    }
    let (resolved, detected) =
        detect_format(&name, &info, entry.as_ref(), record.as_ref(), settings);
    // A server's video is recorded with only a guess at its format until
    // it is probed; what the opened file says shows in the history at once.
    // A transcode says less than the file (servers drop its VR metadata).
    if let Some(r) = &record
        && transcoding.is_none()
        && detected.evidence != Evidence::Default
        && let Err(e) = library.set_detected(r.id, detected)
    {
        log::warn!("saving the format of {location}: {e}");
    }

    let script_source = req
        .source_id
        .clone()
        .or_else(|| record.as_ref().map(|r| r.source_id.clone()))
        .filter(|id| !id.is_empty());
    let scripts = load_scripts(opener, script_source.as_deref(), &script_locs);
    let duration_ms = (info.duration * 1000.0) as i64;
    let heatmap = scripts
        .iter()
        .find(|(a, _)| *a == Axis::L0)
        .filter(|_| duration_ms > 0)
        .map(|(_, s)| fp_haptics::heatmap_range(s, 0, duration_ms, 240));
    let active_subtitle_file = match pick {
        Pick::File(i) => read_subtitle_file(opener, &subs[i]).ok().map(|cues| {
            player.set_external_subtitles(cues);
            if let Some(download) = managed_subtitles
                .iter()
                .find(|s| s.location() == subs[i].location)
            {
                player.set_external_subtitle_delay(download.delay_ms);
            }
            i
        }),
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
    let trickplay = source
        .as_ref()
        .zip(entry.as_ref())
        .and_then(|(source, video)| crate::trickplay::Preview::new(source.clone(), video));
    let watch = source.and_then(|src| {
        let video = entry
            .clone()
            .unwrap_or_else(|| Entry::new(title.clone(), location.clone(), EntryKind::Video));
        match source_session {
            Some(session) => Some(Watch::start_session(reports, src, session, start_at)),
            None => Watch::start(reports, src, &video, start_at),
        }
    });
    let backdrop = BackdropSearch::new(req.source_id.clone(), location.clone());
    Ok(Opened {
        playback: Playback {
            player,
            request: OpenRequest {
                // A legacy row retained after a migration collision keeps
                // its own user state across quality changes and reopens.
                location: record.as_ref().map_or(location, |r| r.location.clone()),
                entry,
                transcode: transcoding.as_ref().map(|t| t.id.clone()),
                ..req
            },
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
            backdrop,
            colour_open: !settings_v.own_key(),
            keyframes,
            settings_dirty: false,
            heatmap,
            markers,
            trickplay,
            subtitle_files: subs,
            active_subtitle_file,
            managed_subtitles,
            subtitle_signature: original,
            last_subtitles: None,
            picked,
            script_count: scripts.len(),
            last_save: Instant::now(),
            notice,
            decode_check: (Instant::now(), 0, 0, 0),
            damaged_jumps: 0,
            damaged_skipped: 0.0,
            skipping_since: None,
            watch,
            targets,
            transcoding,
            source_tracks,
        },
        scripts,
    })
}

/// Asks `source` to transcode `location` to `t` from a time (seconds).
fn restart_at(
    source: Arc<dyn Source>,
    location: &str,
    t: &TranscodeTarget,
    session: Option<fp_sources::WatchSession>,
) -> fp_media::Restart {
    let (location, id) = (location.to_string(), t.id.clone());
    Arc::new(move |at| {
        match &session {
            Some(session) => source.open_transcode_session(&location, &id, at, session),
            None => source.open_transcode(&location, &id, at),
        }
        .map_err(|e| e.to_string())
    })
}

/// The player's settings for a video, frame sequential and interlaced or not
/// as the viewer last said in `user_format`, else by its labels and headers.
fn player_config(
    settings: &Settings,
    start_at: f64,
    user_format: Option<VideoFormat>,
) -> PlayerConfig {
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
        frame_sequential: user_format.map(|f| f.stereo == format::StereoLayout::FrameSequential),
        deinterlace: user_format.and_then(|f| f.interlaced),
        ..Default::default()
    }
}

/// Puts what was picked for a video on its request. The saved pick goes first:
/// the request can be an old copy, kept in Up next before the pick was made.
fn use_saved_source_tracks(
    req: &mut OpenRequest,
    picked: &fp_library::PickedTracks,
    tracks: &[fp_sources::PlaybackTrack],
) {
    let (audio, subtitle) = saved_source_tracks(picked, tracks);
    req.source_audio = audio.or(req.source_audio.take());
    req.source_subtitle = subtitle.or(req.source_subtitle.take());
}

/// What was picked for a video, as its server's track IDs: audio, then
/// subtitles ("0" for off).
fn saved_source_tracks(
    picked: &fp_library::PickedTracks,
    tracks: &[fp_sources::PlaybackTrack],
) -> (Option<String>, Option<String>) {
    use fp_library::PickedSubtitles;
    let audio = picked
        .audio
        .and_then(|i| tracks.iter().find(|t| !t.subtitle && t.index == Some(i)))
        .map(|t| t.id.clone());
    let subtitles = tracks.iter().filter(|t| t.subtitle);
    let subtitle = match &picked.subtitles {
        None => None,
        Some(PickedSubtitles::Off) => Some("0".to_string()),
        Some(PickedSubtitles::Stream(i)) => subtitles
            .clone()
            .find(|t| t.index == Some(*i))
            .map(|t| t.id.clone()),
        Some(PickedSubtitles::File(file)) => subtitles
            .clone()
            .find(|t| t.location.as_ref() == Some(file))
            .map(|t| t.id.clone())
            .or_else(|| Some("0".into())),
    };
    (audio, subtitle)
}

/// How to keep a server's subtitle track picked while transcoding (`None` is
/// off), the way a pick in the player is kept.
pub(crate) fn picked_source_subtitles(
    id: Option<&str>,
    tracks: &[fp_sources::PlaybackTrack],
) -> Option<fp_library::PickedSubtitles> {
    use fp_library::PickedSubtitles;
    let Some(id) = id else {
        return Some(PickedSubtitles::Off);
    };
    let track = tracks.iter().find(|t| t.subtitle && t.id == id)?;
    track
        .location
        .clone()
        .map(PickedSubtitles::File)
        .or_else(|| track.index.map(PickedSubtitles::Stream))
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
        .map(StreamInfo::display_size)
        .unwrap_or((0, 0));
    let user = record.and_then(|r| r.user_format);
    let hints = if info.hints.projection.is_some() || info.hints.stereo.is_some() {
        info.hints
    } else if let Some(f) = declared_format(entry, record) {
        ContainerHints {
            projection: Some(f.projection),
            stereo: Some(f.stereo),
            eyes_swapped: f.eyes_swapped,
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
    source: Option<Arc<dyn Source>>,
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
    } else if scripts.is_empty()
        && let Some(path) = fp_core::volume::resolve(location).filter(|p| p.starts_with('/'))
    {
        let dirs = vec![fp_core::dirs::interactive_dir()];
        scripts = fp_haptics::funscript::find_scripts(std::path::Path::new(path.as_ref()), &dirs)
            .into_iter()
            .map(|(_, p)| p.display().to_string())
            .collect();
    }
    scripts.dedup();
    let mut seen = std::collections::HashSet::new();
    subs.retain(|s| seen.insert(s.location.clone()));
    (scripts, subs)
}

fn load_scripts(
    opener: &Opener,
    source_id: Option<&str>,
    locations: &[String],
) -> Vec<(Axis, Script)> {
    let mut scripts = Vec::new();
    for loc in locations {
        match opener.fetch_in(source_id, loc, 64 << 20).and_then(|bytes| {
            fp_haptics::funscript::parse_script_file(&last_segment(loc), &bytes)
                .map_err(|e| e.to_string())
        }) {
            Ok(mut s) => scripts.append(&mut s),
            Err(e) => log::warn!("script {loc}: {e}"),
        }
    }
    scripts
}

fn local_video_path(location: &str) -> Option<std::path::PathBuf> {
    let local = fp_core::volume::resolve(location);
    let path = local.as_deref().unwrap_or(location);
    let path = std::path::Path::new(path.strip_prefix("file://").unwrap_or(path));
    path.is_absolute().then(|| path.to_path_buf())
}

pub(crate) fn local_subtitle_original(location: &str) -> Option<fp_core::source::FileSource> {
    let path = local_video_path(location)?;
    std::fs::metadata(&path)
        .ok()?
        .is_file()
        .then(|| fp_core::source::FileSource::open(&path).ok())
        .flatten()
}

/// Metadata available without opening the video remotely. The media ID also
/// identifies the selected version of a server item.
fn subtitle_signature(
    location: &str,
    size: Option<u64>,
    mtime: Option<i64>,
) -> fp_library::OriginalSubtitleSignature {
    let metadata = local_video_path(location).and_then(|path| std::fs::metadata(path).ok());
    let metadata = metadata.filter(std::fs::Metadata::is_file);
    let local_mtime = metadata
        .as_ref()
        .and_then(|m| m.modified().ok())
        .and_then(|m| m.duration_since(std::time::UNIX_EPOCH).ok())
        .and_then(|m| i64::try_from(m.as_secs()).ok());
    fp_library::OriginalSubtitleSignature {
        size: metadata.as_ref().map(std::fs::Metadata::len).or(size),
        mtime: local_mtime.or(mtime),
        ..Default::default()
    }
}

/// A subtitle file's cues. Blocks while a server answers, so not on the
/// frame loop.
pub fn read_subtitle_file(opener: &Opener, file: &SubtitleFile) -> Result<Vec<Cue>, String> {
    let read = opener.open(&file.location).and_then(|s| {
        fp_media::subtitle::load_file(s, &last_segment(&file.location)).map_err(|e| e.to_string())
    });
    if let Err(e) = &read {
        log::warn!("subtitles {}: {e}", file.location);
    }
    read
}

/// A place on the video's timeline: a chapter, a marker its source gives,
/// or the viewer's bookmark.
#[derive(Clone, Debug, PartialEq)]
pub struct Marker {
    pub time: f64,
    pub name: String,
    /// For the viewer's bookmark, which they can remove: the library's rows
    /// it shows, more than one where the same bookmark was added again
    /// before that was prevented, none for a video not in the library.
    pub bookmark: Option<Vec<fp_library::MarkerId>>,
}

impl Marker {
    fn chapter(time: f64, name: String) -> Marker {
        Marker {
            time,
            name,
            bookmark: None,
        }
    }
}

/// How near the viewer's bookmark makes another one there a repeat, seconds.
const BOOKMARK_GAP: f64 = 1.0;

/// Chapters, the source's markers and the user's bookmarks, in time order.
fn markers(
    info: &fp_media::MediaInfo,
    entry: Option<&Entry>,
    record: Option<&fp_library::MediaRecord>,
    library: &Library,
) -> Vec<Marker> {
    let mut markers: Vec<Marker> = info
        .chapters
        .iter()
        .map(|c| Marker::chapter(c.start, c.title.clone()))
        .collect();
    if let Some(e) = entry {
        markers.extend(
            e.markers
                .iter()
                .map(|(t, n)| Marker::chapter(*t, n.clone())),
        );
    }
    if let Some(r) = record
        && let Ok(m) = library.markers(r.id)
    {
        markers.extend(m.into_iter().map(|m| Marker {
            time: m.time,
            bookmark: (!m.from_source).then(|| vec![m.id]),
            name: m.name,
        }));
    }
    collapse(&mut markers);
    markers
}

/// Sorts `markers` by time and shows each place once: the same name at
/// the same moment, a bookmark's rows together.
fn collapse(markers: &mut Vec<Marker>) {
    markers.sort_by(|a, b| a.time.total_cmp(&b.time));
    markers.dedup_by(|a, b| {
        let same = (a.time - b.time).abs() < 0.5
            && a.name == b.name
            && a.bookmark.is_some() == b.bookmark.is_some();
        if same && let (Some(rows), Some(kept)) = (a.bookmark.as_mut(), b.bookmark.as_mut()) {
            kept.append(rows);
        }
        same
    });
}

/// Removes the viewer's bookmark at `index` in `markers` and its rows.
fn remove_bookmark(
    markers: &mut Vec<Marker>,
    index: usize,
    library: &Library,
) -> fp_library::Result<()> {
    let Some(Some(rows)) = markers.get(index).map(|m| &m.bookmark) else {
        return Ok(());
    };
    for row in rows {
        library.remove_marker(*row)?;
    }
    markers.remove(index);
    Ok(())
}

/// Whether the viewer has a bookmark within [`BOOKMARK_GAP`] of `time`.
fn bookmarked_near(markers: &[Marker], time: f64) -> bool {
    markers
        .iter()
        .any(|m| m.bookmark.is_some() && (m.time - time).abs() < BOOKMARK_GAP)
}

/// The cinema room `environment` names, if this build has it.
pub fn room_for(environment: BackgroundEnvironment) -> Option<fp_render::CinemaRoom> {
    let room = match environment {
        BackgroundEnvironment::Classic => fp_render::CinemaRoom::Classic,
        BackgroundEnvironment::Luxe => fp_render::CinemaRoom::Luxe,
        BackgroundEnvironment::Colour | BackgroundEnvironment::UseGlobal => return None,
    };
    room.is_available().then_some(room)
}

impl Playback {
    pub fn active_managed_subtitle(&self) -> Option<&fp_library::UserSubtitle> {
        let file = self.subtitle_files.get(self.active_subtitle_file?)?;
        self.managed_subtitles
            .iter()
            .find(|s| s.location() == file.location)
    }

    pub fn subtitle_needs_review(&self, location: &str) -> bool {
        self.managed_subtitles.iter().any(|s| {
            s.location() == location
                && s.original_signature
                    .as_ref()
                    .is_some_and(|known| known.differs_from(&self.subtitle_signature))
        })
    }

    /// The subtitles showing: a file, an embedded stream, or none.
    pub fn subtitles_showing(&self) -> Pick {
        match (self.active_subtitle_file, self.player.subtitle_stream()) {
            (Some(i), _) => Pick::File(i),
            (None, Some(s)) => Pick::Stream(s),
            (None, None) => Pick::Off,
        }
    }

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

    /// Returns `(held, jumped)`: whether a jump over a damaged stretch has
    /// held the picture for [`SKIPPING_SHOWN_AFTER`] or more, so the viewer
    /// is told, and whether a jump finished since the last poll, which
    /// [`Self::damage_notice`] then counts.
    pub fn poll_damage_skip(&mut self) -> (bool, bool) {
        let skipping_now = self.player.is_skipping_damaged();
        let (jumps, skipped) = self.player.damaged();
        let (skipping, done) = damage_skip_step(&mut self.damaged_jumps, skipping_now, jumps);
        if done {
            self.damaged_skipped += skipped;
        }
        let held = held_long(&mut self.skipping_since, skipping, Instant::now());
        (held, done)
    }

    /// The control bar's notice once a damaged stretch was jumped over: one
    /// pill however many there are, rather than a message for each.
    pub fn damage_notice(&self) -> Option<Notice> {
        damage_notice(self.damaged_jumps, self.damaged_skipped)
    }

    /// View settings at the current position (keyframes applied).
    pub fn current_settings(&self) -> ViewSettings {
        self.keyframes.at(self.player.position(), &self.settings)
    }

    /// The cinema room chosen for this flat video at this point of it, if
    /// this build has it.
    pub fn wanted_room(&self, settings: &Settings) -> Option<fp_render::CinemaRoom> {
        use BackgroundEnvironment::*;
        let environment = match self.current_settings().environment {
            UseGlobal => settings.environment(),
            choice => choice,
        };
        room_for(environment).filter(|_| self.format.projection == format::Projection::Flat)
    }

    /// The cinema row this video sits in, its own or the global one.
    pub fn cinema_row(&self, settings: &Settings) -> u8 {
        self.current_settings()
            .cinema_row
            .unwrap_or_else(|| settings.cinema_row())
    }

    /// How much the cinema's screen curves for this video, its own or the
    /// global curve, blended between keyframes either way.
    pub fn cinema_curvature(&self, settings: &Settings) -> f32 {
        let global = settings.cinema_curvature();
        self.keyframes
            .value_at(self.player.position(), &self.settings, |s| {
                s.cinema_curvature.unwrap_or(global)
            })
    }

    /// Whether a cinema room can be round this video: not while passthrough
    /// shows your room, or for a video whose own background is see-through.
    fn room_allowed(&self, settings: &Settings, passthrough_available: bool) -> bool {
        !(settings.passthrough && passthrough_available)
            && !self.shown_settings(&settings.default_view).chroma_key
            && self.format.alpha_pack_scale().is_none()
    }

    /// The room to have ready for this video: the one chosen, while it can
    /// show.
    pub fn room_to_load(
        &self,
        settings: &Settings,
        passthrough_available: bool,
    ) -> Option<fp_render::CinemaRoom> {
        self.wanted_room(settings)
            .filter(|_| self.room_allowed(settings, passthrough_available))
    }

    /// The cinema room round this video, when it shows.
    pub fn cinema_room(
        &self,
        settings: &Settings,
        passthrough_available: bool,
    ) -> Option<fp_render::CinemaRoom> {
        self.room_to_load(settings, passthrough_available)
            .filter(|_| self.player.info().video_stream().is_some())
    }

    /// Whether Adjust can offer this video a cinema room: a flat video's
    /// picture, where a room can show.
    pub fn rooms_offered(&self, settings: &Settings, passthrough_available: bool) -> bool {
        fp_render::CinemaRoom::any_available()
            && self.format.projection == format::Projection::Flat
            && self.player.info().video_stream().is_some()
            && self.room_allowed(settings, passthrough_available)
    }

    /// What is shown now: [`Self::current_settings`] with the global chroma
    /// key unless this video has its own, the global background while it
    /// follows that, and the backdrop's colour once found, until a colour is
    /// picked for this video.
    pub fn shown_settings(&self, global: &ViewSettings) -> ViewSettings {
        let mut s = self.current_settings().with_global_key(global);
        if s.environment == BackgroundEnvironment::UseGlobal {
            s.background = global.background;
        }
        if let Some(c) = self.found_colour() {
            s.key_color = c;
        }
        s
    }

    /// The colour found for this video's chroma key, while none is picked.
    pub fn found_colour(&self) -> Option<[f32; 3]> {
        match self.backdrop.state {
            Backdrop::Found(c) if self.colour_open => Some(c),
            _ => None,
        }
    }

    /// A colour was picked for this video: the one found no longer applies.
    pub fn colour_picked(&mut self) {
        self.colour_open = false;
    }

    /// Whether the backdrop is to be searched for: the chroma key is on, no
    /// colour is picked for this video, and its own mask isn't in use, its
    /// red corners being no backdrop.
    pub fn wants_backdrop(&self, global: &ViewSettings) -> bool {
        let s = self.current_settings().with_global_key(global);
        self.colour_open && s.chroma_key && self.format.alpha_pack_scale().is_none()
    }

    /// Takes the search's answer once it has one. A video with its own
    /// chroma key, turned on in this session with no colour picked, keeps
    /// the colour found, saved as the Passthrough tab saves it.
    pub fn poll_backdrop(&mut self) {
        let before = self.backdrop.state;
        self.backdrop.poll();
        if before == Backdrop::Looking
            && let Some(c) = self.found_colour()
            && self.settings.own_key()
        {
            self.settings.key_color = c;
            self.key_changed();
        }
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
        let notice = slow_decode_notice(
            v,
            hardware_on,
            st.hardware_error.as_deref(),
            &st.hardware_holders,
        );
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

    /// Puts a flat screen the way the browse panel is turned, `yaw`
    /// degrees (left positive), as it opens: upright, at eye height.
    pub fn place_flat(&mut self, yaw: f32) {
        if self.format.projection == format::Projection::Flat {
            for s in [&mut self.settings, &mut self.saved_settings] {
                [s.yaw, s.pitch] = [yaw, 0.0];
            }
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

    /// Saves the adjustments with the video: true when saved, false when the
    /// video isn't in the library (its notice says so). A failure keeps them
    /// unsaved, to save again.
    pub fn save_settings(&mut self, library: &Library) -> fp_library::Result<bool> {
        let Some(id) = self.record_id else {
            self.notice = Some(Notice::new(
                tr!("notice-pill-not-saved"),
                tr!("notice-not-in-library"),
            ));
            return Ok(false);
        };
        library
            .set_view_settings(id, Some(&kept_with_video(self.settings, self.format)))
            .and_then(|_| library.set_keyframes(id, &self.keyframes))
            .inspect_err(|e| log::warn!("saving adjustments: {e}"))?;
        self.settings_dirty = false;
        self.saved_settings = self.settings;
        Ok(true)
    }

    /// Saves the audio and subtitles picked in the player, which the video
    /// opens with next time; what wasn't picked keeps following the
    /// settings.
    pub fn save_tracks(&self, library: &Library) {
        let Some(id) = self.record_id else { return };
        if let Err(e) = library.set_tracks(id, &self.picked) {
            log::warn!(
                "saving the tracks picked for {}: {e}",
                self.request.location
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
        match library.set_view_settings(id, Some(&kept_with_video(saved, self.format))) {
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

    /// The format picked for this video: none on Automatic.
    pub fn picked_format(&self) -> Option<VideoFormat> {
        (self.evidence == Evidence::User).then_some(self.format)
    }

    /// What a preset keeps of this video: its adjustments as they are saved
    /// with it, and the format picked.
    pub fn preset_parts(&self) -> (ViewSettings, Option<VideoFormat>) {
        (
            kept_with_video(self.settings, self.format),
            self.picked_format(),
        )
    }

    /// The first of `presets` this video has everything of.
    pub fn matching_preset(&self, presets: &[fp_core::view::Preset]) -> Option<usize> {
        let flat = self.format.projection == format::Projection::Flat;
        presets
            .iter()
            .position(|p| p.matches(&self.settings, self.picked_format(), flat))
    }

    /// Bookmarks where the video is; false, adding none, when the viewer
    /// has one there already.
    pub fn add_bookmark(&mut self, library: &Library) -> fp_library::Result<bool> {
        let t = self.player.position();
        if bookmarked_near(&self.markers, t) {
            return Ok(false);
        }
        let name = tr!("bookmark-name", time = crate::ui::fmt_time(t));
        let rows = match self.record_id {
            Some(id) => vec![library.add_marker(id, t, &name)?],
            None => Vec::new(),
        };
        self.markers.push(Marker {
            time: t,
            name,
            bookmark: Some(rows),
        });
        collapse(&mut self.markers);
        Ok(true)
    }

    /// Removes the viewer's bookmark at `index` in [`Self::markers`], with
    /// every row of it the library holds. Chapters stay.
    pub fn remove_bookmark(&mut self, index: usize, library: &Library) -> fp_library::Result<()> {
        remove_bookmark(&mut self.markers, index, library)
    }

    /// Saves where the video was and closes it. The player's threads are
    /// stopped and joined on a thread of their own: one can be inside a
    /// network read or a long software decode, and waiting for it here
    /// froze the headset's frame loop (quitting while a 10-bit file over
    /// SMB was buffering hung the app).
    pub fn close(mut self, library: &Library) {
        self.trickplay = None;
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
    hardware_decodes(v, fp_media::hevc_10bit_hardware())
}

/// 8-bit 4:2:0 HEVC and H.264, and 10-bit 4:2:0 HEVC when `hevc_10bit` (the
/// renderer shows it, see [`fp_media::set_hevc_10bit_hardware`]). Not
/// interlaced H.264, nor video being deinterlaced, which takes pictures in
/// memory.
fn hardware_decodes(v: &StreamInfo, hevc_10bit: bool) -> bool {
    let depth = v.bit_depth <= 8 || (hevc_10bit && v.codec == "hevc" && v.bit_depth == 10);
    matches!(v.codec.as_str(), "hevc" | "h264")
        && depth
        && v.chroma() == "4:2:0"
        && !v.deinterlaced
        && !(v.interlaced && v.codec == "h264")
}

/// The stream's codec, with its bit depth and chroma when they are not
/// plain 8-bit 4:2:0 ("10-bit HEVC", "4:2:2 HEVC"), and whether it is
/// interlaced.
fn stream_desc(v: &StreamInfo) -> String {
    let codec = match v.codec.as_str() {
        "hevc" => "HEVC".to_string(),
        "h264" => "H.264".to_string(),
        "av1" => "AV1".to_string(),
        "vp9" => "VP9".to_string(),
        c => c.to_uppercase(),
    };
    let chroma = v.chroma();
    let desc = match (v.bit_depth > 8, chroma != "4:2:0") {
        (true, true) => format!("{}-bit {chroma} {codec}", v.bit_depth),
        (true, false) => format!("{}-bit {codec}", v.bit_depth),
        (false, true) => format!("{chroma} {codec}"),
        (false, false) => codec,
    };
    if v.interlaced || v.deinterlaced {
        tr!("decoder-stream-interlaced", stream = desc)
    } else {
        desc
    }
}

/// Why a video decodes in software, for the stream info: the hardware
/// decoder's error when it was tried (less the "unsupported: " most start
/// with), else why it wasn't.
pub(crate) fn software_reason(
    v: &StreamInfo,
    hardware_on: bool,
    hardware_error: Option<&str>,
) -> String {
    match hardware_error {
        Some(e) => e.strip_prefix("unsupported: ").unwrap_or(e).to_string(),
        None if !has_hardware_decoder(v) => tr!("decoder-why-none", stream = stream_desc(v)),
        None if !hardware_on => tr!("decoder-why-off").to_string(),
        None => tr!("decoder-why-untried").to_string(),
    }
}

/// A warning pill on the control bar: a few words, and all of it in the tip.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Notice {
    pub label: String,
    pub detail: String,
}

impl Notice {
    fn new(label: impl Into<String>, detail: impl Into<String>) -> Notice {
        Notice {
            label: label.into(),
            detail: detail.into(),
        }
    }
}

/// A program holding the hardware decoder, as the person in the headset knows
/// it.
fn holder_name(program: &str) -> String {
    match program {
        "steam" => "Steam".into(),
        "steamwebhelper" => tr!("decoder-holder-steam-browser").to_string(),
        other => other.into(),
    }
}

/// Why a software-decoded video stutters, and what plays smoothly instead.
/// `hardware_on` is the hardware decoding setting; `hardware_error` why the
/// hardware decoder, when tried, is not decoding, and `holders` who held it.
pub(crate) fn slow_decode_notice(
    v: &StreamInfo,
    hardware_on: bool,
    hardware_error: Option<&str>,
    holders: &fp_media::SessionHolders,
) -> Notice {
    let desc = stream_desc(v);
    if !has_hardware_decoder(v) {
        return Notice::new(
            tr!("decoder-pill-none"),
            tr!("decoder-notice-none", stream = desc),
        );
    }
    if !hardware_on {
        return Notice::new(
            tr!("decoder-pill-off"),
            tr!("decoder-notice-off", stream = desc),
        );
    }
    let size = format!("{}x{}", v.width, v.height);
    if hardware_error.is_some_and(|e| e.contains("too big")) {
        return Notice::new(
            tr!("decoder-pill-too-big"),
            tr!("decoder-notice-too-big", stream = desc, size = size),
        );
    }
    if holders.recording_blocks {
        return Notice::new(
            tr!("decoder-pill-busy"),
            tr!("decoder-notice-recording", stream = desc),
        );
    }
    // Out of decoder memory or sessions: other apps hold the hardware decoder.
    let busy = hardware_error.is_none_or(|e| {
        let e = e.to_ascii_lowercase();
        e.contains("memory") || e.contains("busy") || e.contains("no free")
    });
    if !busy {
        Notice::new(
            tr!("decoder-pill-refused"),
            tr!("decoder-notice-refused", stream = desc, size = size),
        )
    } else if holders.programs.is_empty() {
        Notice::new(
            tr!("decoder-pill-busy"),
            tr!("decoder-notice-busy", stream = desc),
        )
    } else {
        let names = holders
            .programs
            .iter()
            .map(|p| holder_name(p))
            .collect::<Vec<_>>()
            .join(", ");
        Notice::new(
            tr!("decoder-pill-busy"),
            tr!("decoder-notice-held", stream = desc, holders = names),
        )
    }
}

/// How long a jump over a damaged stretch holds the picture before
/// "Skipping damaged video…" shows: one through what was read ahead is over
/// in milliseconds, too soon to notice.
const SKIPPING_SHOWN_AFTER: Duration = Duration::from_secs(1);

/// Whether a jump, `skipping` now and since `since` (kept up to date here),
/// has held the picture for [`SKIPPING_SHOWN_AFTER`] by `now`.
fn held_long(since: &mut Option<Instant>, skipping: bool, now: Instant) -> bool {
    if !skipping {
        *since = None;
        return false;
    }
    now.duration_since(*since.get_or_insert(now)) >= SKIPPING_SHOWN_AFTER
}

/// The notice for `jumps` damaged stretches skipped, `skipped` seconds of
/// video in all (rounded up, so a short one doesn't read as none).
fn damage_notice(jumps: u64, skipped: f64) -> Option<Notice> {
    (jumps > 0).then(|| {
        Notice::new(
            tr!("notice-pill-damaged"),
            tr!(
                "notice-damaged",
                count = jumps,
                time = crate::ui::fmt_time(skipped.ceil())
            ),
        )
    })
}

fn damage_skip_step(seen_jumps: &mut u64, skipping: bool, total_jumps: u64) -> (bool, bool) {
    let completed = !skipping && total_jumps > *seen_jumps;
    if completed {
        *seen_jumps = total_jumps;
    }
    (skipping, completed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repeated_bookmarks_show_once_and_are_removed_together() {
        let library = Library::open_in_memory().unwrap();
        let id = library
            .upsert(&fp_library::MediaUpsert::new("/videos/clip.mp4", "local"))
            .unwrap()
            .id;
        // Ten presses at one moment, from before repeats were prevented,
        // and one bookmark further on.
        for _ in 0..10 {
            library.add_marker(id, 60.2, "Bookmark 1:00").unwrap();
        }
        library.add_marker(id, 300.0, "Bookmark 5:00").unwrap();
        let info = fp_media::MediaInfo {
            name: "clip.mp4".into(),
            container: "mp4".into(),
            duration: 600.0,
            streams: Vec::new(),
            chapters: vec![fp_media::info::Chapter {
                start: 60.0,
                end: 120.0,
                title: "Opening".into(),
            }],
            hints: Default::default(),
            video: None,
            audio: None,
            title: None,
        };
        let record = library.get(id).unwrap();
        let mut m = markers(&info, None, record.as_ref(), &library);
        let shown: Vec<_> = m.iter().map(|m| (m.time, m.name.as_str())).collect();
        assert_eq!(
            shown,
            [
                (60.0, "Opening"),
                (60.2, "Bookmark 1:00"),
                (300.0, "Bookmark 5:00")
            ]
        );
        assert_eq!(m[0].bookmark, None);
        assert_eq!(m[1].bookmark.as_ref().map(Vec::len), Some(10));

        // Another press near a bookmark is a repeat; near a chapter it isn't.
        assert!(bookmarked_near(&m, 60.9));
        assert!(!bookmarked_near(&m, 61.5));
        assert!(!bookmarked_near(&m[..1], 60.0));

        // A chapter can't be removed; a bookmark goes with all its copies.
        remove_bookmark(&mut m, 0, &library).unwrap();
        assert_eq!(m.len(), 3);
        remove_bookmark(&mut m, 1, &library).unwrap();
        assert_eq!(m.len(), 2);
        let left: Vec<_> = library.markers(id).unwrap();
        assert_eq!(left.len(), 1);
        assert_eq!(left[0].name, "Bookmark 5:00");
    }

    #[test]
    fn the_backdrop_is_found_in_a_second_read_of_the_file() {
        let dir = std::env::temp_dir().join(format!("fpx-backdrop-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        // A person in front of a green screen, as a still picture.
        let path = dir.join("screen.png");
        image::RgbImage::from_fn(320, 180, |x, y| {
            if (140..180).contains(&x) && y > 50 {
                image::Rgb([170, 130, 110])
            } else {
                image::Rgb([40, 190, 70])
            }
        })
        .save(&path)
        .unwrap();
        let search = |location: &std::path::Path| {
            let mut s = BackdropSearch::new(None, location.to_string_lossy().into_owned());
            s.start(Arc::new(Opener::new()), 0.0);
            let until = Instant::now() + Duration::from_secs(20);
            while s.state == Backdrop::Looking && Instant::now() < until {
                std::thread::sleep(Duration::from_millis(20));
                s.poll();
            }
            s.state
        };
        // The green at full strength.
        let Backdrop::Found([r, g, b]) = search(&path) else {
            panic!("no backdrop found");
        };
        assert!(
            r < 0.02 && g > 0.98 && (b - 0.2).abs() < 0.03,
            "{:?}",
            [r, g, b]
        );
        assert_eq!(search(&dir.join("gone.mp4")), Backdrop::NotFound);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn source_owned_requests_bind_the_owner_without_a_library_record() {
        let location = fp_sources::location::encode("server", "mb", "item=movie&version=edition");
        let mut request = OpenRequest {
            location,
            ..Default::default()
        };
        assert_eq!(
            request.bound_source_id().unwrap().as_deref(),
            Some("server")
        );
        request.source_id = Some("another-account".into());
        assert!(request.bound_source_id().is_err());
        request.source_id = None;
        request.location = "source://wrong/server/mb/item".into();
        assert!(request.bound_source_id().is_err());
        request.location = "http://public.example/movie.mp4".into();
        assert!(request.bound_source_id().unwrap().is_none());
    }

    #[test]
    fn an_entry_cannot_change_another_sources_saved_metadata() {
        let library = Library::open_in_memory().unwrap();
        let private = fp_sources::location::encode("private", "http", "clip.mp4");
        let id = library
            .upsert_entry(
                "private",
                &Entry::new("Private title", &private, EntryKind::Video),
            )
            .unwrap()
            .id;
        let opener = Opener::new();
        opener.insert_source(Arc::new(fp_sources::LocalSource::new(
            &fp_sources::LocalConfig {
                id: "public".into(),
                name: "Public".into(),
                root: "/".into(),
            },
        )));
        let request = OpenRequest {
            location: "/public/clip.mp4".into(),
            source_id: Some("public".into()),
            entry: Some(Entry::new("Changed title", private, EntryKind::Video)),
            ..Default::default()
        };
        assert!(
            open(
                request,
                &opener,
                &library,
                &Settings::default(),
                &Reports::start()
            )
            .is_err()
        );
        assert_eq!(library.get(id).unwrap().unwrap().title, "Private title");
        assert!(library.history(10).unwrap().is_empty());
    }

    #[test]
    fn titles_from_names() {
        assert_eq!(title_from_name("My.Video_2024.mkv"), "My Video 2024");
    }

    struct RootedClip {
        root: &'static str,
        opened: Arc<Mutex<Vec<(String, String)>>>,
    }

    impl Source for RootedClip {
        fn id(&self) -> &str {
            "rooted"
        }
        fn name(&self) -> &str {
            "Rooted clip"
        }
        fn kind(&self) -> fp_sources::SourceKind {
            fp_sources::SourceKind::Http
        }
        fn describe(&self) -> String {
            self.root.into()
        }
        fn canonical_location(&self, location: &str) -> Option<String> {
            let key = if fp_sources::is_source_location(location) {
                let reference = fp_sources::location::decode(location)?;
                if reference.source_id != self.id() || reference.kind != "http" {
                    return None;
                }
                reference.key
            } else {
                location.strip_prefix(self.root)?.into()
            };
            Some(fp_sources::location::encode(self.id(), "http", &key))
        }
        fn list(&self, _: Option<&str>) -> fp_sources::Result<Vec<Entry>> {
            Ok(Vec::new())
        }
        fn open(&self, location: &str) -> fp_sources::Result<Arc<dyn fp_core::ByteSource>> {
            let reference = fp_sources::location::decode(location)
                .filter(|r| r.source_id == self.id() && r.kind == "http" && r.key == "clip.mp4")
                .ok_or_else(|| fp_sources::Error::Unsupported("not this source's clip".into()))?;
            lock(&self.opened).push((self.root.into(), reference.key));
            let clip = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../fp-media/tests/data/h264_aac_180_LR.mp4");
            Ok(Arc::new(fp_core::source::FileSource::open(&clip)?))
        }
    }

    fn reopen_rooted_clip(legacy_collision: bool) {
        let library = Library::open_in_memory().unwrap();
        let stable = fp_sources::location::encode("rooted", "http", "clip.mp4");
        let original = "http://old.example/videos/clip.mp4";
        let canonical_id = library
            .upsert(&fp_library::MediaUpsert::new(&stable, "rooted"))
            .unwrap()
            .id;
        let canonical_view = ViewSettings {
            zoom: 1.25,
            brightness: 0.1,
            ..Default::default()
        };
        library
            .set_view_settings(canonical_id, Some(&canonical_view))
            .unwrap();
        let (expected_id, expected_view) = if legacy_collision {
            let id = library
                .upsert(&fp_library::MediaUpsert::new(original, "rooted"))
                .unwrap()
                .id;
            let view = ViewSettings {
                zoom: 1.75,
                brightness: -0.2,
                ..Default::default()
            };
            library.set_view_settings(id, Some(&view)).unwrap();
            library.set_favorite(id, true).unwrap();
            (id, view)
        } else {
            (canonical_id, canonical_view)
        };
        let playlist = library.create_playlist("Keep this version").unwrap();
        library.add_to_playlist(playlist, &[expected_id]).unwrap();
        let opened = Arc::new(Mutex::new(Vec::new()));
        let opener = Opener::new();
        opener.insert_source(Arc::new(RootedClip {
            root: "http://old.example/videos/",
            opened: opened.clone(),
        }));
        let settings = Settings {
            hardware_decoding: false,
            resume: false,
            haptics_enabled: false,
            ..Default::default()
        };
        let reports = Reports::start();
        let request = OpenRequest {
            location: original.into(),
            source_id: Some("rooted".into()),
            entry: Some(Entry::new("Clip", original, EntryKind::Video)),
            paused: true,
            ..Default::default()
        };
        let first = open(request, &opener, &library, &settings, &reports)
            .unwrap()
            .playback;
        assert_eq!(first.record_id, Some(expected_id));
        assert_eq!(first.settings, expected_view);
        assert_eq!(first.request.entry.as_ref().unwrap().location, stable);
        assert!(first.player.is_paused());
        assert!(!first.request.paused, "for this open only");
        let mut reopen = first.request.clone();
        reopen.paused = true;
        reopen.quality = Quality::Original;
        reopen.start_at = Some(0.0);
        first.close(&library);

        // Quality changes clone Playback.request. The source now has a
        // different endpoint and must receive the canonical entry, while
        // a legacy row retained after a collision must keep its own ID.
        opener.insert_source(Arc::new(RootedClip {
            root: "https://new.example/moved/",
            opened: opened.clone(),
        }));
        let second = open(reopen, &opener, &library, &settings, &reports)
            .unwrap()
            .playback;
        assert_eq!(second.record_id, Some(expected_id));
        assert_eq!(second.settings, expected_view);
        assert_eq!(second.request.entry.as_ref().unwrap().location, stable);
        assert_eq!(
            second.request.location,
            if legacy_collision { original } else { &stable }
        );
        assert_eq!(
            *lock(&opened),
            [
                ("http://old.example/videos/".into(), "clip.mp4".into()),
                ("https://new.example/moved/".into(), "clip.mp4".into()),
            ]
        );
        let history = library.history(10).unwrap();
        assert_eq!(history.len(), 2);
        assert!(history.iter().all(|entry| entry.media_id == expected_id));
        assert_eq!(
            library.playlist_items(playlist).unwrap()[0].record.id,
            expected_id
        );
        assert_eq!(
            library.get(canonical_id).unwrap().unwrap().view_settings,
            Some(canonical_view)
        );
        if legacy_collision {
            assert!(library.get(expected_id).unwrap().unwrap().favorite);
            assert!(library.get_by_location(original).unwrap().is_some());
        } else {
            assert!(library.get_by_location(original).unwrap().is_none());
        }
        second.close(&library);
        assert!(wait_closed(Duration::from_secs(5)));
    }

    #[test]
    fn canonical_entries_survive_endpoint_changes_and_quality_reopens() {
        reopen_rooted_clip(false);
    }

    #[test]
    fn colliding_legacy_history_keeps_its_user_data_on_quality_reopen() {
        reopen_rooted_clip(true);
    }

    /// A server that can transcode to 1080p or 720p using a real test clip.
    /// By default its original file cannot play.
    #[derive(Default)]
    struct Unplayable {
        files: Mutex<u32>,
        asked: Mutex<Vec<(String, f64)>>,
        plex: bool,
        original_available: bool,
        transcode_subtitles: Mutex<Vec<Option<String>>>,
    }

    fn open_test_clip() -> fp_sources::Result<Arc<dyn fp_core::ByteSource>> {
        let clip = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../fp-media/tests/data/h264_aac_180_LR.mp4");
        Ok(Arc::new(fp_core::source::FileSource::open(&clip)?))
    }

    impl Source for Unplayable {
        fn id(&self) -> &str {
            "server"
        }
        fn name(&self) -> &str {
            "Server"
        }
        fn kind(&self) -> fp_sources::SourceKind {
            if self.plex {
                fp_sources::SourceKind::Plex
            } else {
                fp_sources::SourceKind::MediaBrowser
            }
        }
        fn describe(&self) -> String {
            String::new()
        }
        fn list(&self, _: Option<&str>) -> fp_sources::Result<Vec<Entry>> {
            Ok(Vec::new())
        }
        fn open(&self, _: &str) -> fp_sources::Result<Arc<dyn fp_core::ByteSource>> {
            *lock(&self.files) += 1;
            if self.original_available {
                return open_test_clip();
            }
            if self.plex {
                return Err(fp_sources::Error::Unsupported(
                    "Plex requires conversion".into(),
                ));
            }
            Ok(Arc::new(fp_core::source::MemorySource(vec![7; 64 << 10])))
        }
        fn watch_session(&self, _: &Entry) -> Option<fp_sources::WatchSession> {
            self.plex.then(|| fp_sources::WatchSession {
                item: "1".into(),
                title: "Film".into(),
                endpoint: None,
                id: "session".into(),
                duration_ms: Some(300000),
                media_id: Some("10".into()),
                audio_stream: None,
                subtitle_stream: None,
            })
        }
        fn playback_tracks(&self, _: &Entry) -> fp_sources::Result<Vec<fp_sources::PlaybackTrack>> {
            Ok(vec![fp_sources::PlaybackTrack {
                id: "301".into(),
                index: Some(2),
                location: None,
                subtitle: true,
                title: "Server subtitles".into(),
                codec: "srt".into(),
                selected: true,
            }])
        }
        fn transcode_targets(&self, _: &Entry) -> Vec<TranscodeTarget> {
            [1080, 720]
                .map(|h| TranscodeTarget {
                    id: h.to_string(),
                    label: format!("{h}p"),
                    height: Some(h),
                    seekable: true,
                })
                .to_vec()
        }
        fn open_transcode(
            &self,
            _: &str,
            id: &str,
            start: f64,
        ) -> fp_sources::Result<Arc<dyn fp_core::ByteSource>> {
            lock(&self.asked).push((id.to_string(), start));
            open_test_clip()
        }
        fn open_transcode_session(
            &self,
            location: &str,
            id: &str,
            start: f64,
            session: &fp_sources::WatchSession,
        ) -> fp_sources::Result<Arc<dyn fp_core::ByteSource>> {
            lock(&self.transcode_subtitles).push(session.subtitle_stream.clone());
            self.open_transcode(location, id, start)
        }
    }

    fn open_from(
        server: Arc<Unplayable>,
        quality: Quality,
        transcode: Option<&str>,
    ) -> Result<Opened, String> {
        let opener = Opener::new();
        opener.insert_source(server);
        let mut entry = Entry::new("Film", "mb:http://s#item=1", EntryKind::Video);
        entry.duration = Some(300.0);
        let req = OpenRequest {
            location: entry.location.clone(),
            source_id: Some("server".into()),
            entry: Some(entry),
            start_at: Some(30.0),
            quality,
            transcode: transcode.map(str::to_string),
            ..Default::default()
        };
        let settings = Settings {
            resume: false,
            ..Settings::default()
        };
        let lib = Library::open_in_memory().unwrap();
        open(req, &opener, &lib, &settings, &Reports::start())
    }

    #[test]
    fn a_file_that_cannot_play_plays_the_servers_transcode() {
        let server = Arc::new(Unplayable::default());
        let pb = open_from(server.clone(), Quality::Automatic, None)
            .unwrap()
            .playback;
        assert_eq!(pb.transcoding.as_ref().map(|t| t.id.as_str()), Some("1080"));
        assert_eq!(pb.request.transcode.as_deref(), Some("1080"));
        assert!(
            pb.notice
                .as_ref()
                .is_some_and(|n| n.detail.contains("1080p"))
        );
        // From where it was asked, as long as the video, and it seeks.
        assert_eq!(*lock(&server.asked), [("1080".to_string(), 30.0)]);
        assert_eq!(pb.player.duration(), 300.0);
        assert!(pb.player.can_seek());
        assert_eq!(pb.targets.len(), 2);
    }

    #[test]
    fn plex_conversion_required_falls_back_only_under_automatic() {
        let server = Arc::new(Unplayable {
            plex: true,
            ..Default::default()
        });
        let pb = open_from(server.clone(), Quality::Automatic, None)
            .unwrap()
            .playback;
        assert_eq!(pb.transcoding.as_ref().map(|t| t.id.as_str()), Some("1080"));
        assert_eq!(*lock(&server.asked), [("1080".into(), 30.0)]);
        drop(pb);
        lock(&server.asked).clear();
        assert!(open_from(server.clone(), Quality::Original, None).is_err());
        assert!(lock(&server.asked).is_empty());
    }

    #[test]
    fn saved_subtitle_download_restores_with_timing_in_plex_original_and_transcode() {
        use fp_library::{NewUserSubtitle, PickedSubtitles, PickedTracks, SubtitleStore};

        let dir = std::env::temp_dir().join(format!("fpx-plex-subs-{}", std::process::id()));
        let store = SubtitleStore::new(dir.clone());
        let server = Arc::new(Unplayable {
            plex: true,
            original_available: true,
            ..Default::default()
        });
        let opener = Opener::with_subtitle_store(store.clone());
        opener.insert_source(server.clone());
        let mut entry = Entry::new("Film", "plex://source/server/item/1", EntryKind::Video);
        entry.duration = Some(300.0);
        let library = Library::open_in_memory().unwrap();
        let id = library.upsert_entry("server", &entry).unwrap().id;
        let bytes = b"1\n00:00:00,150 --> 00:00:00,550\nLocal original\n\n\
                      2\n00:00:30,150 --> 00:00:30,550\nLocal transcode\n";
        let attachment = store
            .commit(
                store.stage(bytes).unwrap(),
                &library,
                &library.subtitle_target(id).unwrap(),
                NewUserSubtitle {
                    provider: "opensubtitles".into(),
                    provider_file_id: 42,
                    download_parameters: "srt:utf-8".into(),
                    display_name: "Film.en.srt".into(),
                    language: "en".into(),
                    language_name: None,
                    release: None,
                    flags: Default::default(),
                    checksum: "fixture".into(),
                    original_signature: None,
                },
            )
            .unwrap();
        library
            .set_tracks(
                id,
                &PickedTracks {
                    audio: None,
                    subtitles: Some(PickedSubtitles::File(attachment.location())),
                },
            )
            .unwrap();
        library.set_subtitle_delay(id, &attachment.id, 500).unwrap();
        let settings = Settings {
            hardware_decoding: false,
            resume: false,
            ..Default::default()
        };
        let reports = Reports::start();

        for (quality, target, start, text) in [
            (Quality::Original, None, 0.8, "Local original"),
            (
                Quality::Transcode("720".into()),
                Some("720"),
                30.8,
                "Local transcode",
            ),
        ] {
            let request = OpenRequest {
                location: entry.location.clone(),
                source_id: Some("server".into()),
                entry: Some(entry.clone()),
                start_at: Some(start),
                quality,
                transcode: target.map(str::to_owned),
                // A copied request still has a server subtitle selected.
                source_subtitle: Some("301".into()),
                paused: true,
                ..Default::default()
            };
            let pb = open(request, &opener, &library, &settings, &reports)
                .unwrap()
                .playback;
            assert_eq!(pb.transcoding.as_ref().map(|t| t.id.as_str()), target);
            assert_eq!(pb.request.source_subtitle.as_deref(), Some("0"));
            assert_eq!(
                pb.active_managed_subtitle().map(|sub| &sub.id),
                Some(&attachment.id)
            );
            assert!(pb.player.is_paused());
            let until = Instant::now() + Duration::from_secs(5);
            while pb.player.current_frame().is_none() {
                assert!(
                    Instant::now() < until,
                    "video did not decode: {:?}",
                    pb.player.error()
                );
                std::thread::sleep(Duration::from_millis(5));
            }
            let cues = pb.player.subtitles();
            assert_eq!(cues.len(), 1, "position {}", pb.player.position());
            assert_eq!(cues[0].text, text);
        }
        assert_eq!(*lock(&server.transcode_subtitles), [Some("0".into())]);
        store.remove(&library, id, &attachment.id).unwrap();
        std::fs::remove_dir(dir).unwrap();
    }

    #[test]
    fn under_original_a_file_that_cannot_play_fails() {
        let server = Arc::new(Unplayable::default());
        assert!(open_from(server.clone(), Quality::Original, None).is_err());
        assert!(lock(&server.asked).is_empty());
    }

    #[test]
    fn a_transcode_picked_plays_without_the_file() {
        let server = Arc::new(Unplayable::default());
        let q = Quality::Transcode("720".into());
        let pb = open_from(server.clone(), q, Some("720")).unwrap().playback;
        assert_eq!(pb.transcoding.map(|t| t.label), Some("720p".to_string()));
        assert!(pb.notice.is_none());
        assert_eq!(*lock(&server.files), 0);
    }

    #[test]
    fn an_opening_video_shows_its_title_not_its_address() {
        let from_library = OpenRequest {
            location: "https://server/heresphere/4127".into(),
            title: Some("Beach Day".into()),
            ..Default::default()
        };
        assert_eq!(from_library.shown_title(), "Beach Day");
        let from_a_feed = OpenRequest {
            location: "https://server/deovr/4127".into(),
            entry: Some(Entry::new(
                "Beach Day",
                "https://server/deovr/4127",
                EntryKind::Video,
            )),
            ..Default::default()
        };
        assert_eq!(from_a_feed.shown_title(), "Beach Day");
        let by_address = OpenRequest {
            location: "https://nas/videos/Beach_Day.mp4?token=abc".into(),
            ..Default::default()
        };
        assert_eq!(by_address.shown_title(), "Beach Day");
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
                eyes_swapped: f.eyes_swapped,
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
        lib.upsert(&fp_library::MediaUpsert::new(local, "movies"))
            .unwrap();
        let r = lib.get_by_location(local).unwrap();
        assert_eq!(declared_format(None, r.as_ref()), None);
    }

    #[test]
    fn where_a_flat_screen_was_moved_is_not_saved() {
        let turned = ViewSettings {
            yaw: 30.0,
            pitch: -10.0,
            zoom: 1.2,
            ..ViewSettings::default()
        };
        let flat = kept_with_video(turned, VideoFormat::default());
        assert_eq!((flat.yaw, flat.pitch, flat.zoom), (0.0, 0.0, 1.2));
        let vr = VideoFormat {
            projection: format::Projection::EQUIRECT_180,
            ..VideoFormat::default()
        };
        assert_eq!(kept_with_video(turned, vr), turned);
    }

    #[test]
    fn opening_waits_for_closing_videos_but_not_forever() {
        let (tx, rx) = mpsc::channel::<()>();
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
            deinterlaced: false,
            transfer: Default::default(),
            rotation: 0,
            pixel_aspect: 1.0,
            mvc: false,
            frame_sequential: false,
            sample_rate: 0,
            channels: 0,
            ambisonic: false,
        };
        let nobody = fp_media::SessionHolders::default();
        let n = slow_decode_notice(&stream("hevc", 10), true, None, &nobody).detail;
        assert!(
            n.contains("10-bit HEVC") && n.contains("8-bit 4:2:0 H.265"),
            "{n}"
        );
        let n = slow_decode_notice(&stream("av1", 8), true, None, &nobody).detail;
        assert!(n.contains("for AV1,"), "{n}");
        let busy = "hardware decoder busy: other video sessions leave room for 8192x4064, \
                    not 8192x4096 10-bit";
        let n = slow_decode_notice(&stream("hevc", 8), true, Some(busy), &nobody).detail;
        assert!(n.contains("busy"), "{n}");
        let wrapper = "V4L2 REQBUFS: Cannot allocate memory (os error 12)";
        let n = slow_decode_notice(&stream("hevc", 8), true, Some(wrapper), &nobody).detail;
        assert!(n.contains("busy"), "{n}");
        // Granted fewer buffers than the video's reference pictures need:
        // the driver ran out of memory partway, as busy as when it has
        // none. (A 10-bit stream gets the same where the renderer shows
        // 10-bit, a setting this test leaves alone.)
        let short = "unsupported: V4L2 REQBUFS capture granted 5 of the 13 buffers asked for, \
                     fewer than the 6 the decoder keeps: the driver is out of memory";
        let n = slow_decode_notice(&stream("hevc", 8), true, Some(short), &nobody);
        assert_eq!(n.label, "Decoder busy");
        assert!(n.detail.contains("busy"), "{}", n.detail);
        // Named when they are known, by the names people know.
        let held = fp_media::SessionHolders {
            programs: vec!["steam".into(), "steamwebhelper".into(), "mpv".into()],
            recording_blocks: false,
        };
        let n = slow_decode_notice(&stream("hevc", 8), true, Some(busy), &held).detail;
        assert!(
            n.contains("held by Steam, Steam's browser, mpv") && n.contains("HEVC"),
            "{n}"
        );
        assert!(n.contains("restart the headset"), "{n}");
        // Asked to turn recording off only when that frees enough room.
        assert!(!n.contains("Recording Off"), "{n}");
        let recording = fp_media::SessionHolders {
            programs: vec!["steam".into()],
            recording_blocks: true,
        };
        let n = slow_decode_notice(&stream("hevc", 8), true, Some(busy), &recording).detail;
        assert!(n.contains("Choose Recording Off"), "{n}");
        assert_eq!(
            slow_decode_notice(&stream("hevc", 8), true, Some(busy), &recording).label,
            "Decoder busy"
        );
        // Too big says so, whoever else is open.
        let too_big = "too big for the hardware decoder: 8192x8192, even with nothing else open";
        let mut square = stream("hevc", 8);
        square.height = 8192;
        let n = slow_decode_notice(&square, true, Some(too_big), &recording).detail;
        assert!(n.contains("HEVC (8192x8192) is too big"), "{n}");
        assert_eq!(
            slow_decode_notice(&square, true, Some(too_big), &recording).label,
            "Too big for hardware"
        );
        assert!(!n.contains("busy") && !n.contains("Recording Off"), "{n}");
        // 8-bit, but 4:2:2: the hardware can't, whatever Windows says.
        let mut s422 = stream("hevc", 8);
        s422.pixel_format = "yuv422p".into();
        let n = slow_decode_notice(&s422, true, None, &nobody).detail;
        assert!(n.contains("for 4:2:2 HEVC"), "{n}");
        let n = slow_decode_notice(&stream("hevc", 8), false, None, &nobody).detail;
        assert!(n.contains("turn on hardware decoding"), "{n}");
        let refused = "unsupported: V4L2: stream size unknown";
        let n = slow_decode_notice(&stream("hevc", 8), true, Some(refused), &nobody).detail;
        assert!(n.contains("can't play this HEVC (8192x4096)"), "{n}");

        // The stream info's reason: the error when tried, else why not.
        let r = software_reason(&stream("hevc", 10), true, None);
        assert_eq!(r, "no hardware decoding for 10-bit HEVC");
        let r = software_reason(&stream("hevc", 10), false, None);
        assert_eq!(r, "no hardware decoding for 10-bit HEVC");
        let r = software_reason(&stream("hevc", 8), false, None);
        assert_eq!(r, "hardware decoding is off in Settings");
        let r = software_reason(&stream("hevc", 10), true, Some(busy));
        assert_eq!(r, busy);
        let r = software_reason(&stream("hevc", 8), true, Some(refused));
        assert_eq!(r, "V4L2: stream size unknown");

        // 10-bit HEVC in hardware where the renderer shows it; never other
        // 10-bit codecs.
        assert!(hardware_decodes(&stream("hevc", 10), true));
        assert!(!hardware_decodes(&stream("hevc", 10), false));
        assert!(!hardware_decodes(&stream("h264", 10), true));
        assert!(!hardware_decodes(&stream("vp9", 8), true));
        let mut s12 = stream("hevc", 12);
        s12.pixel_format = "yuv420p12le".into();
        assert!(!hardware_decodes(&s12, true));
        let mut s422 = stream("hevc", 10);
        s422.pixel_format = "yuv422p10le".into();
        assert!(!hardware_decodes(&s422, true));
        assert!(hardware_decodes(&stream("h264", 8), false));

        // Interlaced H.264, and video being deinterlaced, decode in software.
        let mut tv = stream("h264", 8);
        tv.interlaced = true;
        assert!(!hardware_decodes(&tv, false));
        assert_eq!(
            software_reason(&tv, true, None),
            "no hardware decoding for interlaced H.264"
        );
        let mut forced = stream("hevc", 8);
        forced.deinterlaced = true;
        assert!(!hardware_decodes(&forced, false));
    }

    #[test]
    fn picks_become_the_servers_tracks_and_back() {
        use fp_library::{PickedSubtitles, PickedTracks};
        use fp_sources::PlaybackTrack;
        let track =
            |id: &str, index: Option<usize>, location: Option<&str>, subtitle| PlaybackTrack {
                id: id.into(),
                index,
                location: location.map(Into::into),
                subtitle,
                title: String::new(),
                codec: String::new(),
                selected: false,
            };
        let tracks = [
            track("201", Some(1), None, false),
            track("202", Some(2), None, false),
            track("301", Some(3), None, true),
            track("302", None, Some("plex://sub"), true),
        ];
        let saved =
            |audio, subtitles| saved_source_tracks(&PickedTracks { audio, subtitles }, &tracks);
        assert_eq!(saved(None, None), (None, None), "nothing picked");
        assert_eq!(
            saved(Some(2), Some(PickedSubtitles::Stream(3))),
            (Some("202".into()), Some("301".into()))
        );
        assert_eq!(
            saved(None, Some(PickedSubtitles::File("plex://sub".into()))).1,
            Some("302".into())
        );
        assert_eq!(saved(None, Some(PickedSubtitles::Off)).1, Some("0".into()));
        assert_eq!(
            saved(Some(9), Some(PickedSubtitles::Stream(9))),
            (None, None)
        );

        assert_eq!(
            picked_source_subtitles(None, &tracks),
            Some(PickedSubtitles::Off)
        );
        assert_eq!(
            picked_source_subtitles(Some("301"), &tracks),
            Some(PickedSubtitles::Stream(3))
        );
        assert_eq!(
            picked_source_subtitles(Some("302"), &tracks),
            Some(PickedSubtitles::File("plex://sub".into()))
        );
        assert_eq!(picked_source_subtitles(Some("999"), &tracks), None);

        // An old copy of the request from Up next, then subtitles turned off
        // and another audio track picked: the saved picks win.
        let mut old = OpenRequest {
            source_audio: Some("201".into()),
            source_subtitle: Some("301".into()),
            ..Default::default()
        };
        use_saved_source_tracks(
            &mut old,
            &PickedTracks {
                audio: Some(2),
                subtitles: Some(PickedSubtitles::Off),
            },
            &tracks,
        );
        assert_eq!(old.source_audio.as_deref(), Some("202"));
        assert_eq!(old.source_subtitle.as_deref(), Some("0"));
        // Nothing saved: what the request asks for stands.
        let mut asked = OpenRequest {
            source_audio: Some("201".into()),
            ..Default::default()
        };
        use_saved_source_tracks(&mut asked, &PickedTracks::default(), &tracks);
        assert_eq!(asked.source_audio.as_deref(), Some("201"));
    }

    #[test]
    fn damaged_stretches_make_one_notice_that_counts_them() {
        assert_eq!(damage_notice(0, 0.0), None);
        let one = damage_notice(1, 0.43).unwrap();
        assert_eq!(one.label, tr!("notice-pill-damaged"));
        assert!(one.detail.contains("0:01"), "{}", one.detail);
        let three = damage_notice(3, 72.2).unwrap();
        assert!(
            three.detail.contains('3') && three.detail.contains("1:13"),
            "{}",
            three.detail
        );
    }

    #[test]
    fn only_a_jump_held_long_enough_to_notice_says_so() {
        let start = Instant::now();
        let at = |ms| start + Duration::from_millis(ms);
        let mut since = None;
        assert!(!held_long(&mut since, false, at(0)));
        // A jump from what was read ahead, over before anyone notices.
        assert!(!held_long(&mut since, true, at(0)));
        assert!(!held_long(&mut since, true, at(120)));
        assert!(!held_long(&mut since, false, at(130)));
        // One that holds the picture: shown once it has for a second.
        assert!(!held_long(&mut since, true, at(1000)));
        assert!(!held_long(&mut since, true, at(1900)));
        assert!(held_long(&mut since, true, at(2000)));
        assert!(held_long(&mut since, true, at(4000)));
        // Done, and the next jump starts its own second.
        assert!(!held_long(&mut since, false, at(4100)));
        assert!(!held_long(&mut since, true, at(5000)));
        assert!(held_long(&mut since, true, at(6000)));
    }

    #[test]
    fn damage_skip_step_reports_in_flight_and_completion() {
        let mut seen = 0;
        assert_eq!(damage_skip_step(&mut seen, false, 0), (false, false));
        // Jump starts: skipping is live, not yet completed.
        assert_eq!(damage_skip_step(&mut seen, true, 1), (true, false));
        assert_eq!(seen, 0);
        // Jump completes: signals completion once, then returns to steady state.
        assert_eq!(damage_skip_step(&mut seen, false, 1), (false, true));
        assert_eq!(seen, 1);
        assert_eq!(damage_skip_step(&mut seen, false, 1), (false, false));
        // Another jump occurs later.
        assert_eq!(damage_skip_step(&mut seen, true, 2), (true, false));
        assert_eq!(damage_skip_step(&mut seen, false, 2), (false, true));
        assert_eq!(seen, 2);
    }
}
