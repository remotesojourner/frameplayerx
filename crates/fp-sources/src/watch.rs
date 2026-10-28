//! Watch sync: telling the server a video came from how watching it goes,
//! for servers that keep track (Jellyfin and Emby; HereSphere APIs that
//! name an event server). From it they keep resume points, count plays and
//! mark videos watched.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

/// One playback of one video, as its server knows it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WatchSession {
    /// The server's name for the video: a Jellyfin item, a HereSphere
    /// video URL.
    pub item: String,
    pub title: String,
    /// Where the server wants the events (a HereSphere event server).
    pub endpoint: Option<String>,
    /// Ties one playback's reports together.
    pub id: String,
}

impl WatchSession {
    pub(crate) fn new(item: String, title: String, endpoint: Option<String>) -> WatchSession {
        WatchSession {
            item,
            title,
            endpoint,
            id: session_id(),
        }
    }
}

/// What changed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WatchKind {
    /// The video opened.
    Start,
    /// Still open: every few seconds.
    Tick,
    Pause,
    Resume,
    /// Jumped to another position.
    Seek,
    /// Plays faster or slower.
    Speed,
    /// The video closed.
    Stop,
}

/// One report: what changed, where, and how the video plays on.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct WatchEvent {
    pub kind: WatchKind,
    /// Seconds into the video.
    pub position: f64,
    pub paused: bool,
    /// Playback rate, 1 at normal speed.
    pub speed: f64,
}

/// 32 hex digits, as Jellyfin's own play session ids, different for every
/// playback.
pub(crate) fn session_id() -> String {
    static COUNT: AtomicU64 = AtomicU64::new(0);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos() as u64);
    let n = COUNT.fetch_add(1, Ordering::Relaxed);
    format!("{nanos:016x}{:08x}{n:08x}", std::process::id())
}

/// Unix time in milliseconds.
pub(crate) fn now_ms() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0.0, |d| d.as_secs_f64() * 1000.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_ids_differ() {
        let a = WatchSession::new("i".into(), "t".into(), None);
        let b = WatchSession::new("i".into(), "t".into(), None);
        assert_ne!(a.id, b.id);
        assert_eq!(a.id.len(), 32);
        assert!(a.id.chars().all(|c| c.is_ascii_hexdigit()));
    }
}
