//! An opened container: format context over our I/O context.

use crate::bluray::Title;
use crate::decode::Packet;
use crate::io::IoContext;
use crate::nal::{NalReader, Packing};
use crate::second_view::{BASE_PID, SECOND_PID, SecondView};
use crate::{Result, check};
use fp_core::ByteSource;
use fp_ffmpeg_sys as ff;
use std::collections::VecDeque;
use std::ffi::{CStr, CString, c_int, c_void};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

/// Flags of FFmpeg's index entries.
const INDEX_KEYFRAME: c_int = 1;
const INDEX_DISCARD: c_int = 2;
/// A skip looks for a keyframe the index leaves out only when the listed
/// one is at least this far before its target, in seconds: a file listing
/// every keyframe of 2 s GOPs is never looked into.
const UNLISTED_GAP_SECS: f64 = 2.0;
/// And over a network, only when the listed keyframes are this many bytes
/// apart, as each look is a request.
const UNLISTED_GAP_REMOTE: u64 = 4 << 20;
/// Looks in a row that find nothing before a file is taken to list all its
/// keyframes.
const UNLISTED_MISSES: u8 = 2;

pub struct Input {
    pub(crate) fmt: *mut ff::AVFormatContext,
    _io: IoContext,
    _interrupt: Arc<AtomicBool>,
    pub name: String,
    origin: f64,
    /// Packets read ahead while opening, given out first.
    peeked: VecDeque<Packet>,
    /// H.264 streams whose packets carry MVC's second view (3D Blu-ray)
    /// though their configuration doesn't say so.
    mvc_in_band: Vec<usize>,
    /// How streams say their eyes are packed in their first packets.
    frame_packing: Vec<(usize, (Packing, bool))>,
    /// Whether the viewer says the video is frame sequential 3D; None goes
    /// by its labels.
    frame_sequential: Option<bool>,
    /// Whether the viewer says the video is interlaced; None goes by its
    /// headers.
    deinterlace: Option<bool>,
    /// 3D Blu-ray's second view in a stream of its own (SSIF files, and
    /// transport streams holding both views), added to the base view's
    /// packets.
    pair: Option<Pair>,
    /// A Blu-ray title, of which this input reads the first clip itself.
    title: Option<Box<Playing>>,
    unlisted: Unlisted,
}

/// What looking for keyframes its index leaves out has learned of a file.
#[derive(Default)]
struct Unlisted {
    /// Its Matroska video track, once read (`Some(None)`: none to use).
    matroska_track: Option<Option<u64>>,
    /// Looks in a row that found nothing.
    misses: u8,
}

/// The stretch of a video stream's index from the keyframe listed before a
/// skip's target to the next one listed.
struct Gap {
    stream: usize,
    matroska: bool,
    /// The listed keyframe's entry, position and timestamp.
    anchor: usize,
    anchor_pos: u64,
    anchor_ts: i64,
    /// The last entry no later than the target: in MP4, whose index places
    /// every sample, there are entries between.
    last: usize,
    /// Where the next listed keyframe starts, or the file ends.
    end: u64,
    /// The target, in the stream's time base.
    target: i64,
}

/// A keyframe to add to the index.
struct NewKey {
    pos: i64,
    timestamp: i64,
    size: c_int,
}

/// A title being played clip after clip.
struct Playing {
    title: Title,
    current: usize,
    /// The clip being read when not the first, and each of its streams'
    /// ours (the first clip's stream of the same PID).
    other: Option<(Input, Vec<Option<usize>>)>,
}

impl Playing {
    /// The clip holding `seconds` of the title, and that time on the clip's
    /// own clock in seconds.
    fn locate(&self, seconds: f64) -> (usize, f64) {
        let k = self
            .title
            .clips
            .iter()
            .rposition(|c| c.start <= seconds)
            .unwrap_or(0);
        let c = &self.title.clips[k];
        (
            k,
            c.in_time as f64 / 90_000.0 + (seconds - c.start).max(0.0),
        )
    }
}

/// The base view's stream, the second view's (discarded by FFmpeg) and the
/// reader of the latter.
struct Pair {
    base: usize,
    second: usize,
    view: SecondView,
}

// SAFETY: an Input is owned and used by one thread at a time.
unsafe impl Send for Input {}

unsafe extern "C" fn interrupt_cb(opaque: *mut c_void) -> c_int {
    // SAFETY: opaque points at the AtomicBool kept alive by Input's Arc.
    let flag = unsafe { &*(opaque as *const AtomicBool) };
    flag.load(Ordering::Relaxed) as c_int
}

impl Input {
    /// Opens `src`. `name` (file name or URL) helps FFmpeg pick the demuxer;
    /// a `.iso` name opens the Blu-ray image's main title.
    pub fn open(src: Arc<dyn ByteSource>, name: &str, interrupt: Arc<AtomicBool>) -> Result<Input> {
        if crate::bluray::is_disc_image(name) {
            return Input::open_title(crate::bluray::main_title(src)?, name, interrupt);
        }
        Input::open_file(src, name, interrupt)
    }

    /// A title, its first clip opened as this input.
    fn open_title(title: Title, name: &str, interrupt: Arc<AtomicBool>) -> Result<Input> {
        let first = title
            .clips
            .first()
            .ok_or_else(|| crate::Error::Disc("a title without clips".into()))?;
        let mut input = Input::open_file(first.src.clone(), &first.name, interrupt)?;
        input.name = name.to_string();
        input.origin = first.in_time as f64 / 90_000.0;
        for &st in input.streams() {
            // SAFETY: streams are valid while the input is open.
            let (id, meta) = unsafe { ((*st).id, &mut (*st).metadata) };
            if let Some((_, lang)) = title
                .languages
                .iter()
                .find(|(pid, _)| i32::from(*pid) == id)
                && let Ok(lang) = CString::new(lang.as_str())
            {
                // SAFETY: a valid dictionary slot and NUL-terminated strings.
                unsafe { ff::av_dict_set(meta, c"language".as_ptr(), lang.as_ptr(), 0) };
            }
        }
        input.title = Some(Box::new(Playing {
            title,
            current: 0,
            other: None,
        }));
        Ok(input)
    }

    /// The Blu-ray title this input plays, if it plays one.
    pub(crate) fn title(&self) -> Option<&Title> {
        self.title.as_ref().map(|t| &t.title)
    }

    fn open_file(
        src: Arc<dyn ByteSource>,
        name: &str,
        interrupt: Arc<AtomicBool>,
    ) -> Result<Input> {
        let io = IoContext::new(src.clone(), interrupt.clone())?;
        let cname = CString::new(name.replace('\0', "")).unwrap_or_default();
        // SAFETY: standard FFmpeg open sequence with a custom pb; on failure
        // avformat_open_input frees the context itself.
        unsafe {
            let mut fmt = ff::avformat_alloc_context();
            if fmt.is_null() {
                return Err(crate::Error::Unsupported(
                    "avformat_alloc_context failed".into(),
                ));
            }
            (*fmt).pb = io.as_ptr();
            (*fmt).flags |= ff::AVFMT_FLAG_CUSTOM_IO as c_int;
            (*fmt).interrupt_callback = ff::AVIOInterruptCB {
                callback: Some(interrupt_cb),
                opaque: Arc::as_ptr(&interrupt) as *mut c_void,
            };
            check(
                ff::avformat_open_input(
                    &mut fmt,
                    cname.as_ptr(),
                    std::ptr::null(),
                    std::ptr::null_mut(),
                ),
                "open input",
            )?;
            let mut input = Input {
                fmt,
                _io: io,
                _interrupt: interrupt,
                name: name.to_string(),
                origin: 0.0,
                peeked: VecDeque::new(),
                mvc_in_band: Vec::new(),
                frame_packing: Vec::new(),
                frame_sequential: None,
                deinterlace: None,
                pair: None,
                title: None,
                unlisted: Unlisted::default(),
            };
            check(
                ff::avformat_find_stream_info(fmt, std::ptr::null_mut()),
                "find stream info",
            )?;
            // MPEG-TS streams appear while probing.
            input.pair_views(src);
            input.origin = input.timeline_start();
            input.read_first_packets();
            if let Some(p) = &input.pair {
                input.mvc_in_band.push(p.base);
            }
            Ok(input)
        }
    }

    pub fn streams(&self) -> &[*mut ff::AVStream] {
        // SAFETY: fmt is open; streams has nb_streams valid pointers.
        unsafe {
            let f = &*self.fmt;
            if f.streams.is_null() {
                &[]
            } else {
                std::slice::from_raw_parts(f.streams, f.nb_streams as usize)
            }
        }
    }

    /// Where the media's timeline starts, in the container's seconds. Times
    /// in and out of the player, thumbnails and seeks count from here, so
    /// a video starts at 0 whatever its first timestamp: MPEG-TS and
    /// Blu-ray (.m2ts) files often start seconds or hours in.
    pub(crate) fn origin(&self) -> f64 {
        self.origin
    }

    /// Makes the stream's first picture `start` seconds into the video: a
    /// server asked to transcode from that time begins its picture there,
    /// whatever clock it counts in (Jellyfin and feed servers start at 0,
    /// Emby keeps the file's own times, audio may begin earlier).
    pub(crate) fn start_at(&mut self, start: f64) {
        let picture = self.streams().iter().find_map(|&st| {
            // SAFETY: stream pointers and their parameters are valid while
            // the input is open.
            let (st, kind) = unsafe { (&*st, (*(*st).codecpar).codec_type) };
            let attached = st.disposition & ff::AV_DISPOSITION_ATTACHED_PIC as c_int != 0;
            (kind == ff::AVMEDIA_TYPE_VIDEO && !attached && st.start_time != ff::AV_NOPTS_VALUE)
                .then(|| st.start_time as f64 * crate::q2d(st.time_base))
        });
        self.origin = picture.unwrap_or(self.origin) - start;
    }

    /// The earliest start of an audio or video stream, when it is positive.
    /// Other streams don't count: a subtitle file's starts at its first
    /// cue. A start below 0 is an encoder's priming and is kept.
    fn timeline_start(&self) -> f64 {
        self.streams()
            .iter()
            .filter_map(|&st| {
                // SAFETY: stream pointers and their parameters are valid
                // while the input is open.
                let (st, kind) = unsafe { (&*st, (*(*st).codecpar).codec_type) };
                let picture = st.disposition & ff::AV_DISPOSITION_ATTACHED_PIC as c_int != 0;
                let media = matches!(kind, ff::AVMEDIA_TYPE_VIDEO | ff::AVMEDIA_TYPE_AUDIO);
                (media && !picture && st.start_time != ff::AV_NOPTS_VALUE)
                    .then(|| st.start_time as f64 * crate::q2d(st.time_base))
            })
            .reduce(f64::min)
            .filter(|t| *t > 0.0)
            .unwrap_or(0.0)
    }

    /// Pairs Blu-ray's base view (PID 0x1011) with its second view (0x1012)
    /// when both are H.264 streams and the second holds MVC.
    fn pair_views(&mut self, src: Arc<dyn ByteSource>) {
        let pid_stream = |pid: i32| {
            self.streams().iter().position(|&st| {
                // SAFETY: streams and their parameters are valid while open.
                let (st, par) = unsafe { (&*st, &*(*st).codecpar) };
                st.id == pid && par.codec_id == ff::AV_CODEC_ID_H264
            })
        };
        let (Some(base), Some(second)) = (pid_stream(BASE_PID), pid_stream(SECOND_PID)) else {
            return;
        };
        let Some(view) = SecondView::open(src, self._interrupt.clone(), SECOND_PID as u16) else {
            return;
        };
        // SAFETY: the stream is valid while the input is open.
        unsafe { (*self.streams()[second]).discard = ff::AVDISCARD_ALL };
        self.pair = Some(Pair { base, second, view });
    }

    /// Stream `index` is a part of another (the second view of 3D Blu-ray),
    /// not one to show or choose.
    pub(crate) fn hidden(&self, index: usize) -> bool {
        self.pair.as_ref().is_some_and(|p| p.second == index)
    }

    /// Reads up to the second packet of each H.264 and HEVC video stream,
    /// mostly from what probing buffered, to see the 3D layout it states
    /// there, and whether an H.264 stream not marked as MVC holds MVC's second
    /// view anyway (3D Blu-ray remuxed by tools that drop the `mvcC` box). The
    /// packets read are given out first by [`Input::read`].
    fn read_first_packets(&mut self) {
        // Each stream's first two packets: some muxers put the second view
        // of the first access unit at the start of the second.
        let mut unseen: Vec<(usize, NalReader, u32)> = self
            .streams()
            .iter()
            .enumerate()
            .filter_map(|(i, &st)| {
                // SAFETY: streams and their parameters are valid while open.
                let par = unsafe { &*(*st).codecpar };
                let extradata = crate::mvc::extradata(par);
                let mvc = self
                    .pair
                    .as_ref()
                    .is_some_and(|p| p.base == i || p.second == i)
                    || crate::mvc::config(par.codec_id, extradata).is_some();
                (par.codec_type == ff::AVMEDIA_TYPE_VIDEO && !mvc)
                    .then(|| NalReader::new(par.codec_id, extradata))
                    .flatten()
                    .map(|reader| (i, reader, 2))
            })
            .collect();
        while !unseen.is_empty() && self.peeked.len() < 256 {
            let mut pkt = Packet::new();
            if !matches!(self.demux(pkt.as_ptr()), Ok(true)) {
                break;
            }
            let index = pkt.stream_index();
            if let Some(at) = unseen.iter().position(|&(i, _, _)| i == index) {
                let (_, reader, left) = &mut unseen[at];
                let reader = *reader;
                *left -= 1;
                if *left == 0 {
                    unseen.swap_remove(at);
                }
                // SAFETY: a packet just read, holding size bytes at data.
                let data = unsafe {
                    let p = &*pkt.as_ptr();
                    if p.data.is_null() {
                        &[][..]
                    } else {
                        std::slice::from_raw_parts(p.data, p.size.max(0) as usize)
                    }
                };
                if !self.frame_packing.iter().any(|&(i, _)| i == index)
                    && let Some(layout) = reader.frame_packing(data)
                {
                    self.frame_packing.push((index, layout));
                }
                // SAFETY: as above, the parameters of a stream of this input.
                let h264 = unsafe {
                    (*(*self.streams()[index]).codecpar).codec_id == ff::AV_CODEC_ID_H264
                };
                if h264
                    && crate::nal::units(data, reader.length_size())
                        .any(|n| n.first().is_some_and(|h| matches!(h & 0x1f, 15 | 20)))
                {
                    self.mvc_in_band.push(index);
                    unseen.retain(|&(i, _, _)| i != index);
                }
            }
            self.peeked.push_back(pkt.take());
        }
    }

    /// How stream `index` says its eyes are packed in its first packets, and
    /// whether the right eye comes first.
    pub(crate) fn frame_packing(&self, index: usize) -> Option<(Packing, bool)> {
        self.frame_packing
            .iter()
            .find(|&&(i, _)| i == index)
            .map(|&(_, layout)| layout)
    }

    /// Whether stream `index` carries MVC's second view in its packets,
    /// configured or not.
    pub(crate) fn mvc_in_band(&self, index: usize) -> bool {
        self.mvc_in_band.contains(&index)
    }

    /// Plays the video as frame sequential 3D or not, whatever its labels
    /// say (None: as they say). Read when its info is read and its video
    /// decoder opened.
    pub(crate) fn set_frame_sequential(&mut self, choice: Option<bool>) {
        self.frame_sequential = choice;
    }

    pub(crate) fn frame_sequential(&self) -> Option<bool> {
        self.frame_sequential
    }

    /// Deinterlaces the video or not, whatever its headers say (None: as
    /// they say). Read when its info is read and its video decoder opened.
    pub(crate) fn set_deinterlace(&mut self, choice: Option<bool>) {
        self.deinterlace = choice;
    }

    pub(crate) fn deinterlace(&self) -> Option<bool> {
        self.deinterlace
    }

    /// Best stream of a media type, as FFmpeg chooses it.
    pub fn best_stream(&self, kind: ff::AVMediaType) -> Option<usize> {
        // SAFETY: fmt is open.
        let i = unsafe { ff::av_find_best_stream(self.fmt, kind, -1, -1, std::ptr::null_mut(), 0) };
        let i = (i >= 0).then_some(i as usize)?;
        match &self.pair {
            Some(p) if p.second == i => Some(p.base),
            _ => Some(i),
        }
    }

    /// Seeks so the next packets start at or before `seconds` on the media's
    /// timeline (see [`Input::origin`]).
    pub fn seek(&mut self, seconds: f64) -> Result<()> {
        match self.title.as_ref().map(|t| t.locate(seconds)) {
            Some((k, at)) => self.seek_clip(k, at, false),
            None => self.seek_file(seconds),
        }
    }

    /// Seeks for playback, as [`Input::seek`] does, but when the index lists
    /// the keyframe before `seconds` far back, first looks for a nearer one
    /// it leaves out, so the skip fetches and decodes less.
    pub(crate) fn seek_playback(
        &mut self,
        seconds: f64,
        video: Option<usize>,
        cancelled: impl Fn() -> bool,
    ) -> Result<()> {
        // Also loads Matroska's Cues, when they come late.
        self.seek(seconds)?;
        if let Some(vi) = video
            && self.list_nearer_key(vi, seconds, &cancelled)
            && !cancelled()
        {
            self.seek(seconds)?;
        }
        Ok(())
    }

    /// Adds to the index a keyframe of video stream `vi` that it leaves
    /// out, nearer `seconds` than the listed one before it. False when there
    /// is none, or it isn't worth looking.
    fn list_nearer_key(&mut self, vi: usize, seconds: f64, cancelled: &impl Fn() -> bool) -> bool {
        if self.unlisted.misses >= UNLISTED_MISSES {
            return false;
        }
        let Some(gap) = self.listed_gap(vi, seconds) else {
            return false;
        };
        let key = if gap.matroska {
            self.unlisted_cluster(&gap, cancelled)
        } else {
            self.unlisted_sample(&gap, cancelled)
        };
        if cancelled() {
            return false;
        }
        let Some(key) = key else {
            self.unlisted.misses += 1;
            return false;
        };
        self.unlisted.misses = 0;
        let st = self.streams()[gap.stream];
        // SAFETY: the stream belongs to the open format, and no pointer into
        // its index is held across this, which can move it.
        unsafe {
            ff::av_add_index_entry(st, key.pos, key.timestamp, key.size, 0, INDEX_KEYFRAME) >= 0
        }
    }

    /// The gap in video stream `vi`'s index before `seconds`, when it is
    /// long enough to look into.
    fn listed_gap(&self, vi: usize, seconds: f64) -> Option<Gap> {
        let src = self._io.source();
        if self.title.is_some() || self.pair.is_some() || !src.efficient_random_access() {
            return None;
        }
        // SAFETY: the format, its streams and their index are valid while
        // it is open, and looking entries up changes nothing.
        unsafe {
            let matroska = match CStr::from_ptr((*(*self.fmt).iformat).name).to_bytes() {
                b"matroska,webm" => true,
                b"mov,mp4,m4a,3gp,3g2,mj2" => false,
                _ => return None,
            };
            let st = *self.streams().get(vi)?;
            if !matches!(
                (*(*st).codecpar).codec_id,
                ff::AV_CODEC_ID_H264 | ff::AV_CODEC_ID_HEVC
            ) {
                return None;
            }
            let units = crate::q2d((*st).time_base);
            if units <= 0.0 {
                return None;
            }
            let target = ((seconds.max(0.0) + self.origin) / units).floor() as i64;
            let backward = ff::AVSEEK_FLAG_BACKWARD as c_int;
            let anchor = ff::av_index_search_timestamp(st, target, backward);
            let a = ff::avformat_index_get_entry(st, anchor).as_ref()?;
            if (target - a.timestamp) as f64 * units < UNLISTED_GAP_SECS {
                return None;
            }
            let last =
                ff::av_index_search_timestamp(st, target, backward | ff::AVSEEK_FLAG_ANY as c_int);
            let next = ff::avformat_index_get_entry_from_timestamp(st, target.checked_add(1)?, 0);
            let anchor_pos = u64::try_from(a.pos).ok()?;
            let end = match next.as_ref() {
                Some(n) => u64::try_from(n.pos).ok()?,
                None if !matroska => src.size()?,
                None => return None,
            };
            if end <= anchor_pos || src.remote() && end - anchor_pos < UNLISTED_GAP_REMOTE {
                return None;
            }
            Some(Gap {
                stream: vi,
                matroska,
                anchor: anchor as usize,
                anchor_pos,
                anchor_ts: a.timestamp,
                last: last.max(anchor) as usize,
                end,
                target,
            })
        }
    }

    /// A sample in an MP4's `gap` that decoding can start at.
    fn unlisted_sample(&self, gap: &Gap, cancelled: &impl Fn() -> bool) -> Option<NewKey> {
        let st = self.streams()[gap.stream];
        // SAFETY: as in `listed_gap`; the extradata has extradata_size bytes.
        unsafe {
            let par = &*(*st).codecpar;
            let extradata = if par.extradata.is_null() {
                &[][..]
            } else {
                std::slice::from_raw_parts(par.extradata, par.extradata_size as usize)
            };
            let reader = NalReader::new(par.codec_id, extradata)?;
            let samples = (gap.anchor + 1..=gap.last)
                .filter_map(|i| {
                    let e = ff::avformat_index_get_entry(st, i as c_int).as_ref()?;
                    let pos = u64::try_from(e.pos).ok()?;
                    (e.flags() & INDEX_DISCARD == 0).then_some((i, pos, e.size() as usize))
                })
                .collect();
            let src = self._io.source();
            let i = crate::unlisted_keys::sample_key(&*src, &reader, samples, cancelled)? as c_int;
            let e = ff::avformat_index_get_entry(st, i).as_ref()?;
            // FFmpeg updates the first entry with this timestamp: it must be
            // this one.
            let before = ff::avformat_index_get_entry(st, i - 1).as_ref()?;
            (before.timestamp < e.timestamp).then_some(NewKey {
                pos: e.pos,
                timestamp: e.timestamp,
                size: e.size(),
            })
        }
    }

    /// A cluster in a Matroska file's `gap` that opens with a keyframe.
    fn unlisted_cluster(&mut self, gap: &Gap, cancelled: &impl Fn() -> bool) -> Option<NewKey> {
        let src = self._io.source();
        let start = crate::unlisted_keys::next_cluster(&*src, gap.anchor_pos, gap.end, cancelled)?;
        let track = match self.unlisted.matroska_track {
            Some(track) => track,
            None => {
                let track = crate::unlisted_keys::matroska_track(&*src, cancelled);
                if !cancelled() {
                    self.unlisted.matroska_track = Some(track);
                }
                track
            }
        }?;
        let (pos, timestamp) =
            crate::unlisted_keys::cluster_key(&*src, track, start, gap.end, gap.target, cancelled)?;
        (timestamp > gap.anchor_ts).then_some(NewKey {
            pos: i64::try_from(pos).ok()?,
            timestamp,
            size: 0,
        })
    }

    /// Seeks so the next packets start at the first keyframe after
    /// `seconds`.
    pub(crate) fn seek_after(&mut self, seconds: f64) -> Result<()> {
        match self.title.as_ref().map(|t| t.locate(seconds)) {
            Some((k, at)) => self.seek_clip(k, at, true),
            None => self.seek_after_file(seconds),
        }
    }

    /// Reads clip `k` of the title from `at` seconds of its clock: before
    /// it, or from the first keyframe `after`.
    fn seek_clip(&mut self, k: usize, at: f64, after: bool) -> Result<()> {
        let current = self.title.as_ref().map_or(0, |t| t.current);
        if k == 0 {
            if let Some(t) = &mut self.title {
                (t.current, t.other) = (0, None);
            }
            let at = at - self.origin;
            return if after {
                self.seek_after_file(at)
            } else {
                self.seek_file(at)
            };
        }
        if current != k {
            self.open_clip(k)?;
        }
        let Some((other, _)) = self.title.as_mut().and_then(|t| t.other.as_mut()) else {
            return Ok(());
        };
        let at = at - other.origin;
        if after {
            other.seek_after_file(at)
        } else {
            other.seek_file(at)
        }
    }

    /// Reads clip `k` (not the first) from now on, from where its play item
    /// starts.
    fn open_clip(&mut self, k: usize) -> Result<()> {
        let ours: Vec<(i32, ff::AVMediaType)> = self
            .streams()
            .iter()
            // SAFETY: streams and their parameters are valid while open.
            .map(|&st| unsafe { ((*st).id, (*(*st).codecpar).codec_type) })
            .collect();
        let interrupt = self._interrupt.clone();
        let Some(t) = &mut self.title else {
            return Ok(());
        };
        let clip = &t.title.clips[k];
        let mut other = Input::open_file(clip.src.clone(), &clip.name, interrupt)?;
        let map = other
            .streams()
            .iter()
            .map(|&st| {
                // SAFETY: as above.
                let id = unsafe { ((*st).id, (*(*st).codecpar).codec_type) };
                ours.iter().position(|o| *o == id)
            })
            .collect();
        // A play item starting into its clip.
        let start = clip.in_time as f64 / 90_000.0;
        if start > other.origin + 1.0 {
            other.seek_file(start - other.origin)?;
        }
        (t.current, t.other) = (k, Some((other, map)));
        Ok(())
    }

    fn seek_file(&mut self, seconds: f64) -> Result<()> {
        self.peeked.clear();
        let ts = ((seconds.max(0.0) + self.origin) * ff::AV_TIME_BASE as f64) as i64;
        let (stream, ts) = self.seek_stream(ts);
        // SAFETY: fmt is open.
        unsafe {
            check(
                ff::avformat_seek_file(
                    self.fmt,
                    stream,
                    i64::MIN,
                    ts,
                    ts,
                    ff::AVSEEK_FLAG_BACKWARD as c_int,
                ),
                "seek",
            )?;
        }
        Ok(())
    }

    fn seek_after_file(&mut self, seconds: f64) -> Result<()> {
        self.peeked.clear();
        let ts = ((seconds.max(0.0) + self.origin) * ff::AV_TIME_BASE as f64) as i64;
        let (stream, ts) = self.seek_stream(ts);
        // SAFETY: fmt is open.
        unsafe {
            check(
                ff::avformat_seek_file(self.fmt, stream, ts + 1, ts + 1, i64::MAX, 0),
                "seek",
            )?;
        }
        Ok(())
    }

    /// Raw stream-timebase timestamps of all indexed keyframes in `stream_index`
    /// strictly after `after_dts` (or `after_pos` when `after_dts` is unknown).
    pub(crate) fn video_keyframes_after(
        &self,
        stream_index: usize,
        after_dts: i64,
        after_pos: i64,
    ) -> Vec<i64> {
        if self.title.is_some() {
            return Vec::new();
        }
        let Some(&st) = self.streams().get(stream_index) else {
            return Vec::new();
        };
        // SAFETY: `st` is a valid stream of the open format context.
        unsafe {
            let n = ff::avformat_index_get_entries_count(st);
            let mut out = Vec::new();
            for i in 0..n {
                let e = ff::avformat_index_get_entry(st, i);
                if e.is_null() {
                    continue;
                }
                let e = &*e;
                if e.flags() & INDEX_KEYFRAME == 0 {
                    continue;
                }
                if after_pos >= 0 && e.pos == after_pos {
                    continue;
                }
                if after_dts != ff::AV_NOPTS_VALUE && e.timestamp <= after_dts {
                    continue;
                }
                if e.timestamp != ff::AV_NOPTS_VALUE {
                    out.push(e.timestamp);
                }
            }
            out
        }
    }

    /// Seeks `stream_index` directly to an indexed keyframe at `timestamp`
    /// (in the stream's own time base, avoiding microsecond rounding drift).
    pub(crate) fn seek_video_timestamp(
        &mut self,
        stream_index: usize,
        timestamp: i64,
    ) -> Result<()> {
        self.peeked.clear();
        if let Some(p) = &mut self.pair {
            p.view.seek();
        }
        // SAFETY: `self.fmt` is open and `stream_index` is valid.
        let r = unsafe {
            let r = ff::avformat_seek_file(
                self.fmt,
                stream_index as c_int,
                timestamp,
                timestamp,
                i64::MAX,
                0,
            );
            if r < 0 {
                ff::avformat_seek_file(
                    self.fmt,
                    stream_index as c_int,
                    i64::MIN,
                    timestamp,
                    i64::MAX,
                    ff::AVSEEK_FLAG_BACKWARD as c_int,
                )
            } else {
                r
            }
        };
        check(r, "seek")?;
        Ok(())
    }

    /// The stream to seek by, and `ts` (microseconds) in its time base.
    /// With the views paired, the base view's: FFmpeg would seek by the
    /// first video stream, the discarded second view.
    fn seek_stream(&mut self, ts: i64) -> (c_int, i64) {
        let Some(p) = &mut self.pair else {
            return (-1, ts);
        };
        p.view.seek();
        let base = p.base;
        // SAFETY: the stream is valid while the input is open.
        let tb = unsafe { (*self.streams()[base]).time_base };
        let us = ff::AVRational {
            num: 1,
            den: ff::AV_TIME_BASE as c_int,
        };
        // SAFETY: plain arithmetic.
        (base as c_int, unsafe { ff::av_rescale_q(ts, us, tb) })
    }

    /// Reads the next packet into `pkt`. Returns false at the end.
    pub fn read(&mut self, pkt: *mut ff::AVPacket) -> Result<bool> {
        if self.title.is_none() {
            return self.read_file(pkt);
        }
        loop {
            let in_other = self.title.as_ref().is_some_and(|t| t.other.is_some());
            let more = match self.title.as_mut().and_then(|t| t.other.as_mut()) {
                Some((other, _)) if in_other => other.read_file(pkt)?,
                _ => self.read_file(pkt)?,
            };
            if !more {
                if self.next_clip()? {
                    continue;
                }
                return Ok(false);
            }
            let Some(t) = &self.title else {
                return Ok(true);
            };
            let clip = &t.title.clips[t.current];
            let first_in = t.title.clips[0].in_time;
            let shift = first_in + (clip.start * 90_000.0).round() as i64 - clip.in_time;
            let out_time = clip.out_time;
            // SAFETY: a packet just read, ours to change.
            let p = unsafe { &mut *pkt };
            if let Some((_, map)) = &t.other {
                match map.get(p.stream_index as usize).copied().flatten() {
                    Some(i) => p.stream_index = i as c_int,
                    None => {
                        // SAFETY: as above.
                        unsafe { ff::av_packet_unref(pkt) };
                        continue;
                    }
                }
            }
            let ts = if p.dts == ff::AV_NOPTS_VALUE {
                p.pts
            } else {
                p.dts
            };
            if ts != ff::AV_NOPTS_VALUE && ts >= out_time {
                // Past the play item: on to the next once its video is.
                let video =
                    Some(p.stream_index as usize) == self.best_stream(ff::AVMEDIA_TYPE_VIDEO);
                // SAFETY: as above.
                unsafe { ff::av_packet_unref(pkt) };
                if video && !self.next_clip()? {
                    return Ok(false);
                }
                continue;
            }
            for v in [&mut p.pts, &mut p.dts] {
                if *v != ff::AV_NOPTS_VALUE {
                    *v += shift;
                }
            }
            return Ok(true);
        }
    }

    /// Moves on to the title's next clip; false after the last.
    fn next_clip(&mut self) -> Result<bool> {
        let Some(t) = &self.title else {
            return Ok(false);
        };
        let next = t.current + 1;
        if next >= t.title.clips.len() {
            return Ok(false);
        }
        self.open_clip(next)?;
        Ok(true)
    }

    /// The next packet of this input's own file.
    fn read_file(&mut self, pkt: *mut ff::AVPacket) -> Result<bool> {
        if let Some(p) = self.peeked.pop_front() {
            // SAFETY: both packets are valid; pkt is the caller's to fill.
            unsafe { ff::av_packet_move_ref(pkt, p.as_ptr()) };
        } else if !self.demux(pkt)? {
            return Ok(false);
        }
        self.add_second_view(pkt)?;
        Ok(true)
    }

    /// Appends the second view's units to a packet of the base view.
    fn add_second_view(&mut self, pkt: *mut ff::AVPacket) -> Result<()> {
        let Some(p) = &mut self.pair else {
            return Ok(());
        };
        // SAFETY: a packet just read.
        let (index, dts, pos) = unsafe {
            let k = &*pkt;
            let dts = if k.dts == ff::AV_NOPTS_VALUE {
                k.pts
            } else {
                k.dts
            };
            (k.stream_index as usize, dts, k.pos)
        };
        if index != p.base || dts == ff::AV_NOPTS_VALUE {
            return Ok(());
        }
        let Some(second) = p.view.take(dts, pos.max(0) as u64) else {
            return Ok(());
        };
        // SAFETY: av_grow_packet makes room after the data (copying it when
        // shared); the second view's units then fill that room.
        unsafe {
            let at = (*pkt).size as usize;
            check(
                ff::av_grow_packet(pkt, second.len() as c_int),
                "grow packet",
            )?;
            std::ptr::copy_nonoverlapping(second.as_ptr(), (*pkt).data.add(at), second.len());
        }
        Ok(())
    }

    /// The demuxer's next packet.
    fn demux(&mut self, pkt: *mut ff::AVPacket) -> Result<bool> {
        // SAFETY: pkt is a valid allocated packet owned by the caller.
        let r = unsafe { ff::av_read_frame(self.fmt, pkt) };
        if r == ff::AVERROR_EOF_ {
            return Ok(false);
        }
        check(r, "read packet")?;
        Ok(true)
    }
}

impl Drop for Input {
    fn drop(&mut self) {
        // SAFETY: closes the context opened in open(); the custom pb is freed
        // afterwards by IoContext's Drop.
        unsafe { ff::avformat_close_input(&mut self.fmt) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fp_core::source::FileSource;
    use std::sync::atomic::AtomicUsize;

    /// A test file, counting its reads.
    struct Counted {
        file: FileSource,
        reads: Arc<AtomicUsize>,
    }

    impl ByteSource for Counted {
        fn size(&self) -> Option<u64> {
            self.file.size()
        }
        fn efficient_random_access(&self) -> bool {
            true
        }
        fn read_at(&self, offset: u64, buf: &mut [u8]) -> std::io::Result<usize> {
            self.reads.fetch_add(1, Ordering::Relaxed);
            self.file.read_at(offset, buf)
        }
        fn describe(&self) -> String {
            self.file.describe()
        }
    }

    fn open_counted(file: &str) -> (Input, Arc<AtomicUsize>) {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/data")
            .join(file);
        let reads = Arc::new(AtomicUsize::new(0));
        let src = Counted {
            file: FileSource::open(&path).unwrap(),
            reads: reads.clone(),
        };
        let input = Input::open(Arc::new(src), file, Arc::new(AtomicBool::new(false))).unwrap();
        (input, reads)
    }

    /// Where the video starts after a skip to `seconds`, made as playback
    /// makes it or as an ordinary seek, and whether that is a keyframe.
    fn skip_lands(file: &str, seconds: f64, playback: bool, cancelled: bool) -> f64 {
        let (mut input, _) = open_counted(file);
        let vi = input.best_stream(ff::AVMEDIA_TYPE_VIDEO).unwrap();
        if playback {
            input
                .seek_playback(seconds, Some(vi), || cancelled)
                .unwrap();
        } else {
            input.seek(seconds).unwrap();
        }
        let mut pkt = Packet::new();
        while input.read(pkt.as_ptr()).unwrap() {
            if pkt.stream_index() == vi {
                // SAFETY: a packet just read, and a stream of this input.
                let (pts, tb, key) = unsafe {
                    let p = &*pkt.as_ptr();
                    let key = p.flags & ff::AV_PKT_FLAG_KEY as c_int != 0;
                    (p.pts, (*input.streams()[vi]).time_base, key)
                };
                assert!(key, "{file} starts at a picture that isn't a keyframe");
                return (pts as f64 * crate::q2d(tb) * 1000.0).round() / 1000.0;
            }
            pkt.unref();
        }
        panic!("{file}: no video after the skip");
    }

    #[test]
    fn skips_start_at_keyframes_the_index_leaves_out() {
        // Pictures decoding can start at every 2 s, listed only every 6 s:
        // H.264 IDRs, HEVC CRAs, and in Matroska clusters opening with one.
        for file in [
            "h264_sparse_stss.mp4",
            "hevc_sparse_stss.mp4",
            "h264_sparse_cues.mkv",
        ] {
            assert_eq!(skip_lands(file, 10.8, false, false), 6.0, "{file}");
            assert_eq!(skip_lands(file, 10.8, true, false), 10.0, "{file}");
            assert_eq!(
                skip_lands(file, 10.8, true, true),
                6.0,
                "{file}, called off"
            );
            assert_eq!(
                skip_lands(file, 7.0, true, false),
                6.0,
                "{file}, near a listed one"
            );
        }
        assert_eq!(skip_lands("h264_dense_cues.mkv", 10.8, true, false), 10.0);
        // Keyframes 6 s apart, all listed.
        assert_eq!(skip_lands("h264_long_gop.mp4", 10.8, true, false), 6.0);
    }

    #[test]
    fn a_file_listing_all_its_keyframes_is_soon_left_alone() {
        let (mut input, reads) = open_counted("h264_long_gop.mp4");
        let vi = input.best_stream(ff::AVMEDIA_TYPE_VIDEO);
        let mut looks = Vec::new();
        for _ in 0..3 {
            let before = reads.load(Ordering::Relaxed);
            input.seek_playback(10.8, vi, || false).unwrap();
            looks.push(reads.load(Ordering::Relaxed) - before);
        }
        assert!(looks[0] > 0 && looks[1] > 0, "{looks:?}");
        assert_eq!(looks[2], 0, "{looks:?}");
    }

    #[test]
    fn video_keyframes_after_and_seek_video_timestamp_land_on_second_keyframe() {
        let path =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data/h264_aac_180_LR.mp4");
        let mut input = Input::open(
            Arc::new(FileSource::open(&path).unwrap()),
            "h264_aac_180_LR.mp4",
            Arc::new(AtomicBool::new(false)),
        )
        .unwrap();
        let vi = input.best_stream(ff::AVMEDIA_TYPE_VIDEO).unwrap();
        let all = input.video_keyframes_after(vi, i64::MIN, -1);
        assert!(all.len() >= 2, "expected >= 2 keyframes, got {all:?}");
        assert!(all.windows(2).all(|w| w[0] < w[1]));

        // Seeking directly to the second keyframe lands on it rather than
        // sliding back to keyframe 0 (even with B-frame PTS > DTS).
        input.seek_video_timestamp(vi, all[1]).unwrap();
        let mut pkt = Packet::new();
        while input.read(pkt.as_ptr()).unwrap() {
            if pkt.stream_index() == vi {
                // SAFETY: valid packet read from `input`.
                let (dts, pos, flags) = unsafe {
                    let p = &*pkt.as_ptr();
                    (p.dts, p.pos, p.flags)
                };
                assert_ne!(flags & ff::AV_PKT_FLAG_KEY as i32, 0);
                assert_eq!(dts, all[1], "pos={pos}");
                break;
            }
            pkt.unref();
        }

        // Excluding the first keyframe starts from the second.
        let tail = input.video_keyframes_after(vi, all[0], i64::MAX);
        assert_eq!(tail.first().copied(), Some(all[1]));
    }
}
