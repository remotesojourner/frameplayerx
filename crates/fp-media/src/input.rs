//! An opened container: format context over our I/O context.

use crate::io::IoContext;
use crate::{Result, check};
use fp_core::ByteSource;
use fp_ffmpeg_sys as ff;
use std::ffi::{CString, c_int, c_void};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

pub struct Input {
    pub(crate) fmt: *mut ff::AVFormatContext,
    _io: IoContext,
    _interrupt: Arc<AtomicBool>,
    pub name: String,
    origin: f64,
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

    /// Seeks so the next packets start at the first keyframe after
    /// `seconds`.
    pub(crate) fn seek_after(&mut self, seconds: f64) -> Result<()> {
        let ts = ((seconds.max(0.0) + self.origin) * ff::AV_TIME_BASE as f64) as i64;
        // SAFETY: fmt is open.
        unsafe {
            check(
                ff::avformat_seek_file(self.fmt, -1, ts + 1, ts + 1, i64::MAX, 0),
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
