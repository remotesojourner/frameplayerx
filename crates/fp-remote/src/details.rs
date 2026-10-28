//! What the web remote shows of the open video beyond its playback status:
//! its picture and format, markers, volume, tracks and Up next. These
//! change rarely, so the app publishes them only when they do.

use fp_core::SubtitleTrack;
use fp_core::sync::lock;
use serde::Serialize;
use std::path::PathBuf;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

/// The open video's details, worded in the app's language.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct RemoteDetails {
    /// Short format description, e.g. "180° 3D".
    pub format: String,
    /// The video's picture on the headset, served as `/api/picture`.
    #[serde(skip)]
    pub picture: Option<PathBuf>,
    /// Chapters and bookmarks, in time order.
    pub markers: Vec<RemoteMarker>,
    /// 0 to 1.5; 0 is muted.
    pub volume: f64,
    pub audio: Vec<RemoteAudio>,
    /// Without Off: none selected means off.
    pub subtitles: Vec<RemoteSubtitles>,
    /// Whether Up next has a video before / after this one (round the
    /// list when it loops).
    pub has_previous: bool,
    pub has_next: bool,
    /// Up next's titles, and which one plays.
    pub up_next: Vec<String>,
    pub up_next_at: usize,
    /// The switches: Loop video, Autoplay and Up next's Loop.
    pub loop_video: bool,
    pub autoplay: bool,
    pub loop_playlist: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct RemoteMarker {
    /// Seconds from the start.
    pub time: f64,
    pub name: String,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct RemoteAudio {
    /// For [`fp_core::PlayerCommand::SelectAudio`].
    pub stream: usize,
    pub label: String,
    /// Codec and channels, e.g. "aac · 2 ch".
    pub detail: String,
    pub selected: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct RemoteSubtitles {
    /// For [`fp_core::PlayerCommand::SelectSubtitles`].
    pub track: SubtitleTrack,
    pub label: String,
    pub detail: String,
    pub selected: bool,
}

/// The latest details, a change counter, and a number that changes with
/// the picture so a page fetches it again.
#[derive(Default)]
pub(crate) struct DetailsCell {
    details: Mutex<(RemoteDetails, u64)>,
    version: AtomicU64,
}

impl DetailsCell {
    pub(crate) fn publish(&self, d: &RemoteDetails) {
        let mut g = lock(&self.details);
        if g.0 == *d {
            return;
        }
        if g.0.picture != d.picture {
            g.1 += 1;
        }
        g.0 = d.clone();
        drop(g);
        self.version.fetch_add(1, Ordering::Release);
    }

    /// The details, and the picture's number when there is one.
    pub(crate) fn snapshot(&self) -> (RemoteDetails, Option<u64>) {
        let g = lock(&self.details);
        let picture = g.0.picture.as_ref().map(|_| g.1);
        (g.0.clone(), picture)
    }

    pub(crate) fn picture(&self) -> Option<PathBuf> {
        lock(&self.details).0.picture.clone()
    }

    pub(crate) fn version(&self) -> u64 {
        self.version.load(Ordering::Acquire)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn changes_are_counted_and_a_new_picture_gets_a_new_number() {
        let cell = DetailsCell::default();
        let mut d = RemoteDetails {
            volume: 1.0,
            picture: Some("/a.jpg".into()),
            ..Default::default()
        };
        cell.publish(&d);
        let (v, p) = (cell.version(), cell.snapshot().1);
        cell.publish(&d);
        assert_eq!(cell.version(), v, "the same details are no change");
        d.volume = 0.5;
        cell.publish(&d);
        assert_eq!(cell.version(), v + 1);
        assert_eq!(cell.snapshot().1, p, "same picture, same number");
        d.picture = Some("/b.jpg".into());
        cell.publish(&d);
        assert_ne!(cell.snapshot().1, p);
        d.picture = None;
        cell.publish(&d);
        assert_eq!(cell.snapshot().1, None);
    }
}
