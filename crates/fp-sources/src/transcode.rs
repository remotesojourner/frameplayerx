//! Transcoding: a server encoding a video live as it streams it. Nothing
//! is there until the player asks: Jellyfin and Emby encode to the size
//! asked; DeoVR feeds and HereSphere APIs name the addresses their server
//! encodes from when one is requested.
//!
//! What comes back is one stream with no size and no byte ranges. It starts
//! at the time asked, and a seek asks the server to start encoding again
//! from another time, when it takes one ([`TranscodeTarget::seekable`]).

use fp_core::source::ByteSource;
use fp_core::sync::lock;
use std::collections::VecDeque;
use std::io::{self, Read};
use std::sync::{Arc, Mutex};

/// What a server can transcode a video to when asked: a picture height,
/// and for a feed the address it encodes from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TranscodeTarget {
    /// What [`crate::Source::open_transcode`] takes back.
    pub id: String,
    /// "1440p".
    pub label: String,
    /// Picture height in pixels, when known.
    pub height: Option<u32>,
    /// The server starts encoding at a time asked, so it seeks.
    pub seekable: bool,
}

/// Picture heights offered below a video's own.
const HEIGHTS: [u32; 5] = [2880, 2160, 1440, 1080, 720];

/// The heights a picture `height` pixels tall can be transcoded to: its
/// own, then [`HEIGHTS`] below it.
pub(crate) fn heights_below(height: u32) -> Vec<u32> {
    std::iter::once(height)
        .chain(HEIGHTS.into_iter().filter(|h| *h < height))
        .collect()
}

/// Bits a second asked for a `width` x `height` picture at `fps`: about
/// what an H.264 encode of that size needs to look clean, and no more than
/// `file` (the file's own bitrate and height, when known) shrunk with the
/// picture's area, so each step down also needs a slower network.
pub(crate) fn bitrate(width: u32, height: u32, fps: f64, file: Option<(u64, u32)>) -> u64 {
    let fps = if fps > 0.0 { fps.min(60.0) } else { 30.0 };
    let clean = width as f64 * height as f64 * fps * 0.12;
    let scaled = file
        .filter(|(b, h)| *b > 0 && *h > 0)
        .map_or(f64::MAX, |(b, h)| {
            b as f64 * (height as f64 / h as f64).powi(2)
        });
    (clean.min(scaled) as u64).clamp(2_000_000, 120_000_000)
}

/// The targets not above a source's maximum height (those of unknown
/// height too); when every one is taller, the smallest.
pub(crate) fn within(
    max_height: Option<u32>,
    mut all: Vec<TranscodeTarget>,
) -> Vec<TranscodeTarget> {
    let Some(max) = max_height else {
        return all;
    };
    let smallest = all.iter().rposition(|t| t.height.is_some());
    let fitting: Vec<TranscodeTarget> = all
        .iter()
        .filter(|t| t.height.is_none_or(|h| h <= max))
        .cloned()
        .collect();
    match smallest {
        Some(i) if fitting.is_empty() => vec![all.swap_remove(i)],
        _ => fitting,
    }
}

/// "1440p".
pub(crate) fn height_label(height: Option<u32>) -> String {
    height.map_or_else(|| "Transcode".to_string(), |h| format!("{h}p"))
}

/// How far back a transcode can be read again: FFmpeg looks back a little
/// while it works out the streams.
const KEPT: usize = 16 << 20;

/// How far ahead a read may skip, reading through what lies between.
const SKIP: u64 = 256 << 20;

/// A server's transcoding stream as the player reads it: front to back as
/// the server encodes it, a read returning what has come (a slow server, or a
/// picture that compresses to little, sends a block's worth only after
/// many seconds), the last [`KEPT`] bytes kept to read again. It has no
/// size, even when the server sends an estimate: FFmpeg would read the
/// end of the stream for its length, and a transcode can't jump there.
/// `stop` runs once when it is dropped, on a thread of its own: it tells
/// the server to stop encoding, which Emby otherwise carries on with to the
/// end of the video.
pub(crate) struct Streamed {
    what: String,
    read: Mutex<Reading>,
    stop: Mutex<Option<Box<dyn FnOnce() + Send>>>,
}

struct Reading {
    body: Box<dyn Read + Send>,
    /// Bytes read from the body so far.
    pos: u64,
    /// The last of them, from `pos - kept.len()` on.
    kept: VecDeque<u8>,
}

impl Streamed {
    pub(crate) fn wrap(
        body: Box<dyn Read + Send>,
        what: String,
        stop: Option<Box<dyn FnOnce() + Send>>,
    ) -> Arc<dyn ByteSource> {
        Arc::new(Streamed {
            what,
            read: Mutex::new(Reading {
                body,
                pos: 0,
                kept: VecDeque::new(),
            }),
            stop: Mutex::new(stop),
        })
    }
}

impl Reading {
    /// Reads what the body has next (at most `buf.len()`), keeping it.
    fn next(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let n = loop {
            match self.body.read(buf) {
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                r => break r?,
            }
        };
        self.pos += n as u64;
        self.kept.extend(&buf[..n]);
        let over = self.kept.len().saturating_sub(KEPT);
        self.kept.drain(..over);
        Ok(n)
    }
}

impl ByteSource for Streamed {
    fn size(&self) -> Option<u64> {
        None
    }

    fn read_at(&self, offset: u64, buf: &mut [u8]) -> io::Result<usize> {
        let mut r = lock(&self.read);
        let from = r.pos - r.kept.len() as u64;
        if offset < from {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                format!(
                    "{}: can't go back to byte {offset} of a transcode",
                    self.what
                ),
            ));
        }
        if offset < r.pos {
            let at = (offset - from) as usize;
            let n = buf.len().min(r.kept.len() - at);
            for (b, k) in buf[..n].iter_mut().zip(r.kept.range(at..)) {
                *b = *k;
            }
            return Ok(n);
        }
        if offset - r.pos > SKIP {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                format!("{}: can't skip to byte {offset} of a transcode", self.what),
            ));
        }
        // Forward: what lies between is read through (and kept).
        let mut skip = vec![0; 256 << 10];
        while r.pos < offset {
            let want = skip.len().min((offset - r.pos) as usize);
            if r.next(&mut skip[..want])? == 0 {
                return Ok(0);
            }
        }
        r.next(buf)
    }

    fn describe(&self) -> String {
        format!("transcode {}", self.what)
    }

    fn streamed(&self) -> bool {
        true
    }
}

impl Drop for Streamed {
    fn drop(&mut self) {
        if let Some(stop) = lock(&self.stop).take()
            && let Err(e) = std::thread::Builder::new()
                .name("transcode-stop".into())
                .spawn(stop)
        {
            log::warn!("stopping a transcode: {e}");
        }
    }
}

/// `url` with the query parameter `key` set to `value` (replaced when it is
/// there, else added).
pub(crate) fn with_query(url: &str, key: &str, value: &str) -> String {
    let (base, fragment) = match url.split_once('#') {
        Some((b, f)) => (b, Some(f)),
        None => (url, None),
    };
    let (path, query) = base.split_once('?').unwrap_or((base, ""));
    let mut parts: Vec<&str> = query
        .split('&')
        .filter(|p| !p.is_empty())
        .filter(|p| {
            let name = p.split_once('=').map_or(*p, |(n, _)| n);
            !name.eq_ignore_ascii_case(key)
        })
        .collect();
    let set = format!("{key}={value}");
    parts.push(&set);
    let mut out = format!("{path}?{}", parts.join("&"));
    if let Some(f) = fragment {
        out.push('#');
        out.push_str(f);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};

    #[test]
    fn query_parameters_are_set_once() {
        assert_eq!(with_query("http://s/v", "start", "5"), "http://s/v?start=5");
        assert_eq!(
            with_query("http://s/v?a=1&Start=2#x", "start", "5"),
            "http://s/v?a=1&start=5#x"
        );
    }

    #[test]
    fn heights_go_down_from_the_videos_own() {
        assert_eq!(heights_below(3840), [3840, 2880, 2160, 1440, 1080, 720]);
        assert_eq!(heights_below(1080), [1080, 720]);
        assert_eq!(heights_below(480), [480]);
    }

    #[test]
    fn the_maximum_height_caps_transcodes() {
        let t = |h: Option<u32>| TranscodeTarget {
            id: format!("{h:?}"),
            label: height_label(h),
            height: h,
            seekable: true,
        };
        let all = vec![t(Some(2160)), t(Some(1080)), t(None), t(Some(720))];
        let ids = |v: Vec<TranscodeTarget>| v.into_iter().map(|t| t.label).collect::<Vec<_>>();
        assert_eq!(
            ids(within(None, all.clone())),
            ["2160p", "1080p", "Transcode", "720p"]
        );
        assert_eq!(
            ids(within(Some(1440), all.clone())),
            ["1080p", "Transcode", "720p"]
        );
        assert_eq!(ids(within(Some(480), all[..2].to_vec())), ["1080p"]);
    }

    #[test]
    fn bitrates_follow_the_picture() {
        assert_eq!(bitrate(1920, 1080, 30.0, None), 7_464_960);
        assert_eq!(bitrate(4320, 2160, 30.0, None), 33_592_320);
        assert_eq!(bitrate(7680, 3840, 60.0, None), 120_000_000);
        assert_eq!(bitrate(320, 240, 0.0, None), 2_000_000);
        // A 60 Mbit/s 8K file: a quarter of the picture, a quarter of that.
        assert_eq!(
            bitrate(3840, 1920, 30.0, Some((60_000_000, 3840))),
            15_000_000
        );
        assert_eq!(
            bitrate(1920, 1080, 30.0, Some((40_000_000, 1080))),
            7_464_960
        );
    }

    /// A server's body that sends `chunk` bytes at a time.
    struct Trickle {
        data: Vec<u8>,
        at: usize,
        chunk: usize,
    }

    impl Read for Trickle {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            let n = buf.len().min(self.chunk).min(self.data.len() - self.at);
            buf[..n].copy_from_slice(&self.data[self.at..self.at + n]);
            self.at += n;
            Ok(n)
        }
    }

    #[test]
    fn a_transcode_reads_what_has_come_and_stops_when_dropped() {
        let data: Vec<u8> = (0..KEPT + 100_000).map(|i| (i % 251) as u8).collect();
        let stopped = Arc::new(AtomicBool::new(false));
        let s = {
            let stopped = stopped.clone();
            let body = Trickle {
                data: data.clone(),
                at: 0,
                chunk: 1000,
            };
            Streamed::wrap(
                Box::new(body),
                "test".into(),
                Some(Box::new(move || stopped.store(true, Ordering::Release))),
            )
        };
        assert_eq!(s.size(), None);
        assert!(s.streamed());
        let mut buf = vec![0; 64 << 10];
        // What has come, not a full buffer.
        assert_eq!(s.read_at(0, &mut buf).unwrap(), 1000);
        // Back again, from what is kept.
        assert_eq!(s.read_at(10, &mut buf[..5]).unwrap(), 5);
        assert_eq!(buf[..5], data[10..15]);
        // Ahead, reading through.
        let at = KEPT as u64 + 50_000;
        assert_eq!(s.read_at(at, &mut buf).unwrap(), 1000);
        assert_eq!(buf[..1000], data[at as usize..at as usize + 1000]);
        // The start is gone now.
        assert!(s.read_at(0, &mut buf).is_err());
        assert!(s.read_at(at + SKIP + 2000, &mut buf).is_err());
        drop(s);
        let t = std::time::Instant::now();
        while !stopped.load(Ordering::Acquire) && t.elapsed().as_secs() < 5 {
            std::thread::yield_now();
        }
        assert!(stopped.load(Ordering::Acquire));
    }
}
