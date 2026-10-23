//! An opened container: format context over our I/O context.

use crate::io::IoContext;
use crate::{Result, check};
use fp_core::{ByteSource, UrlOpener};
use fp_ffmpeg_sys as ff;
use std::ffi::{CStr, CString, c_char, c_int, c_void};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

pub struct Input {
    pub(crate) fmt: *mut ff::AVFormatContext,
    _io: IoContext,
    _interrupt: Arc<AtomicBool>,
    /// What the format context's `io_open` opens with (its `opaque`).
    _nested: Box<Nested>,
    pub name: String,
    origin: f64,
}

type IoOpen = unsafe extern "C" fn(
    *mut ff::AVFormatContext,
    *mut *mut ff::AVIOContext,
    *const c_char,
    c_int,
    *mut *mut ff::AVDictionary,
) -> c_int;
type IoClose = unsafe extern "C" fn(*mut ff::AVFormatContext, *mut ff::AVIOContext) -> c_int;

/// The files a demuxer opens beside its input: http(s) ones (an HLS
/// playlist's segments) through `urls` when there is one, refused
/// otherwise, so FFmpeg never connects anywhere itself; local ones as
/// FFmpeg would.
struct Nested {
    urls: Option<Arc<dyn UrlOpener>>,
    interrupt: Arc<AtomicBool>,
    open: Option<IoOpen>,
    close: Option<IoClose>,
}

/// The URL without its query, which may carry a token, for the log.
fn without_query(url: &str) -> &str {
    url.split(['?', '#']).next().unwrap_or(url)
}

unsafe extern "C" fn io_open(
    s: *mut ff::AVFormatContext,
    pb: *mut *mut ff::AVIOContext,
    url: *const c_char,
    flags: c_int,
    options: *mut *mut ff::AVDictionary,
) -> c_int {
    // SAFETY: opaque is the Input's Nested, alive as long as the context;
    // url is a C string.
    let (nested, name) = unsafe {
        (
            &*((*s).opaque as *const Nested),
            CStr::from_ptr(url).to_string_lossy(),
        )
    };
    if !name.contains("://") || name.starts_with("file:") {
        return match nested.open {
            // SAFETY: FFmpeg's own io_open, with its own arguments.
            Some(open) => unsafe { open(s, pb, url, flags, options) },
            None => ff::averror(38), // ENOSYS
        };
    }
    let Some(urls) = nested
        .urls
        .as_ref()
        .filter(|_| flags & ff::AVIO_FLAG_WRITE as c_int == 0)
    else {
        log::warn!("not opening {}: no network here", without_query(&name));
        return ff::averror(1); // EPERM
    };
    match urls.open_url(&name).and_then(|src| {
        IoContext::new(src, nested.interrupt.clone()).map_err(std::io::Error::other)
    }) {
        Ok(io) => {
            // SAFETY: pb is FFmpeg's out-parameter.
            unsafe { *pb = io.into_raw() };
            0
        }
        Err(e) => {
            log::warn!("opening {}: {e}", without_query(&name));
            ff::averror(5) // EIO
        }
    }
}

unsafe extern "C" fn io_close(s: *mut ff::AVFormatContext, pb: *mut ff::AVIOContext) -> c_int {
    if pb.is_null() {
        return 0;
    }
    // SAFETY: pb is a context io_open handed out, ours or FFmpeg's; opaque
    // is the Input's Nested.
    unsafe {
        if IoContext::is_ours(pb) {
            IoContext::free_raw(pb);
            0
        } else {
            match (*((*s).opaque as *const Nested)).close {
                Some(close) => close(s, pb),
                None => 0,
            }
        }
    }
}

// SAFETY: an Input is owned and used by one thread at a time.
unsafe impl Send for Input {}

unsafe extern "C" fn interrupt_cb(opaque: *mut c_void) -> c_int {
    // SAFETY: opaque points at the AtomicBool kept alive by Input's Arc.
    let flag = unsafe { &*(opaque as *const AtomicBool) };
    flag.load(Ordering::Relaxed) as c_int
}

impl Input {
    /// Opens `src`. `name` (file name or URL) helps FFmpeg pick the demuxer.
    pub fn open(src: Arc<dyn ByteSource>, name: &str, interrupt: Arc<AtomicBool>) -> Result<Input> {
        Input::open_with(src, name, interrupt, None)
    }

    /// Opens `src`, an HLS playlist (or any input) at the URL `name`: the
    /// playlists and segments it names, relative to `name`, open through
    /// `urls`.
    pub fn open_with(
        src: Arc<dyn ByteSource>,
        name: &str,
        interrupt: Arc<AtomicBool>,
        urls: Option<Arc<dyn UrlOpener>>,
    ) -> Result<Input> {
        let io = IoContext::new(src, interrupt.clone())?;
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
            let nested = Box::new(Nested {
                urls,
                interrupt: interrupt.clone(),
                open: (*fmt).io_open,
                close: (*fmt).io_close2,
            });
            (*fmt).opaque = &*nested as *const Nested as *mut c_void;
            (*fmt).io_open = Some(io_open);
            (*fmt).io_close2 = Some(io_close);
            // The HLS demuxer's own: keep-alive and parallel requests reuse
            // FFmpeg's HTTP connection, which our contexts don't have; a
            // server's segments may be named without an extension.
            let mut options: *mut ff::AVDictionary = std::ptr::null_mut();
            for (k, v) in [
                (c"http_persistent", c"0"),
                (c"http_multiple", c"0"),
                (c"extension_picky", c"0"),
            ] {
                ff::av_dict_set(&mut options, k.as_ptr(), v.as_ptr(), 0);
            }
            let opened =
                ff::avformat_open_input(&mut fmt, cname.as_ptr(), std::ptr::null(), &mut options);
            ff::av_dict_free(&mut options);
            check(opened, "open input")?;
            let mut input = Input {
                fmt,
                _io: io,
                _interrupt: interrupt,
                _nested: nested,
                name: name.to_string(),
                origin: 0.0,
            };
            check(
                ff::avformat_find_stream_info(fmt, std::ptr::null_mut()),
                "find stream info",
            )?;
            input.origin = input.timeline_start();
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

    /// Makes the stream's first moment `start` seconds into the video: a
    /// server's transcode opened from that time on starts its own clock at
    /// 0 (or wherever the container does).
    pub(crate) fn start_at(&mut self, start: f64) {
        self.origin -= start;
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

    /// Best stream of a media type, as FFmpeg chooses it.
    pub fn best_stream(&self, kind: ff::AVMediaType) -> Option<usize> {
        // SAFETY: fmt is open.
        let i = unsafe { ff::av_find_best_stream(self.fmt, kind, -1, -1, std::ptr::null_mut(), 0) };
        (i >= 0).then_some(i as usize)
    }

    /// Seeks so the next packets start at or before `seconds` on the media's
    /// timeline (see [`Input::origin`]).
    pub fn seek(&mut self, seconds: f64) -> Result<()> {
        let ts = ((seconds.max(0.0) + self.origin) * ff::AV_TIME_BASE as f64) as i64;
        // SAFETY: fmt is open.
        unsafe {
            check(
                ff::avformat_seek_file(
                    self.fmt,
                    -1,
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

    /// Reads the next packet into `pkt`. Returns false at end of file.
    pub fn read(&mut self, pkt: *mut ff::AVPacket) -> Result<bool> {
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
