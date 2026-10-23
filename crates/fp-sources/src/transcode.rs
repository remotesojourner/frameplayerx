//! Copies of a video a server makes as it streams them: Jellyfin and Emby
//! transcode to what the player asks for, DeoVR feeds and HereSphere APIs
//! (Stash among them) list the transcodes they offer.
//!
//! A transcode has no size and no byte ranges: it plays from the start it
//! was opened at, and seeking opens it again from another time when the
//! server takes one ([`Transcode::seekable`]).

use fp_core::source::ByteSource;
use serde::{Deserialize, Serialize};
use std::io;
use std::sync::Arc;

/// What Jellyfin and Emby transcode video to (a feed's transcodes come
/// made).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TranscodeCodec {
    /// HEVC, or H.264 from a server that doesn't encode HEVC.
    #[default]
    HevcOrH264,
    /// H.264 always.
    H264,
}

impl TranscodeCodec {
    /// The codecs to accept, best first (a MediaBrowser device profile's
    /// `VideoCodec`).
    pub fn codecs(self) -> &'static str {
        match self {
            TranscodeCodec::HevcOrH264 => "hevc,h264",
            TranscodeCodec::H264 => "h264",
        }
    }
}

/// One transcode a server offers of a video.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Transcode {
    /// What [`crate::Source::open_transcode`] takes back.
    pub id: String,
    /// "1080p".
    pub label: String,
    /// Picture height in pixels, when known.
    pub height: Option<u32>,
    /// The server can start it at a time, so seeking works.
    pub seekable: bool,
}

/// A transcode as the player reads it: from the start on, with no size,
/// even when the server sends an estimated one. Knowing a size, FFmpeg
/// would read the end of an MPEG-TS stream for its length, and a
/// transcode can't jump there.
pub(crate) struct Streamed(pub(crate) Arc<dyn ByteSource>);

impl Streamed {
    pub(crate) fn wrap(src: impl ByteSource + 'static) -> Arc<dyn ByteSource> {
        Arc::new(Streamed(Arc::new(src)))
    }
}

impl ByteSource for Streamed {
    fn size(&self) -> Option<u64> {
        None
    }

    fn read_at(&self, offset: u64, buf: &mut [u8]) -> io::Result<usize> {
        self.0.read_at(offset, buf)
    }

    fn describe(&self) -> String {
        format!("transcode {}", self.0.describe())
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
    let mut parts: Vec<String> = query
        .split('&')
        .filter(|p| !p.is_empty())
        .filter(|p| {
            let name = p.split_once('=').map_or(*p, |(n, _)| n);
            !name.eq_ignore_ascii_case(key)
        })
        .map(str::to_string)
        .collect();
    parts.push(format!("{key}={value}"));
    let mut out = format!("{path}?{}", parts.join("&"));
    if let Some(f) = fragment {
        out.push('#');
        out.push_str(f);
    }
    out
}

/// A label for a transcode of this height.
pub(crate) fn height_label(height: Option<u32>) -> String {
    height.map_or_else(|| "Transcode".to_string(), |h| format!("{h}p"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn query_parameters_are_set_once() {
        assert_eq!(with_query("http://s/v", "start", "5"), "http://s/v?start=5");
        assert_eq!(
            with_query("http://s/v?a=1&Start=2#x", "start", "5"),
            "http://s/v?a=1&start=5#x"
        );
        assert_eq!(
            with_query("http://s/v?StartTimeTicks=0&b=2", "StartTimeTicks", "70"),
            "http://s/v?b=2&StartTimeTicks=70"
        );
    }
}
