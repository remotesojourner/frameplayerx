//! Decodes one preview sheet on a worker and hands the UI only the tile
//! underneath the seek pointer. The worker owns no renderer resources.

use crossbeam_channel::{Receiver, RecvTimeoutError, Sender, bounded};
use egui::ColorImage;
use fp_core::Entry;
use fp_sources::{
    Source, TRICKPLAY_MAX_DIMENSION, TRICKPLAY_MAX_PIXELS, TRICKPLAY_SHEET_BYTES, Trickplay,
    TrickplayTiming,
};
use image::{DynamicImage, ImageReader, Limits};
use std::io::Cursor;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

const TILE_WIDTH: u32 = 256;
const TILE_HEIGHT: u32 = 144;

/// Per-playback request state. A closed playback cancels its requests and
/// lets the worker discard the sheet, even if another video never opens.
pub(crate) struct Preview {
    query: Arc<Query>,
    send: Sender<Reply>,
    receive: Receiver<Reply>,
    pending: bool,
    unavailable: bool,
    current: Option<FrameRange>,
    failed: Option<FrameRange>,
    tile: Option<ColorImage>,
}

struct Query {
    source: Arc<dyn Source>,
    entry: Entry,
    cancelled: AtomicBool,
}

impl Query {
    fn cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Relaxed)
    }
}

#[derive(Clone, Copy)]
struct Position {
    seconds: f64,
    duration: f64,
}

struct Job {
    query: Arc<Query>,
    at: Position,
    reply: Sender<Reply>,
}

#[derive(Clone, Copy)]
struct FrameRange {
    duration: f64,
    bounds: [f64; 2],
}

impl FrameRange {
    fn contains(self, seconds: f64, duration: f64) -> bool {
        let [lo, hi] = self.bounds;
        self.duration == duration && lo <= seconds && seconds < hi
    }
}

enum Reply {
    Tile(FrameRange, ColorImage),
    Failed(FrameRange),
    Unavailable,
}

impl Preview {
    pub fn new(source: Arc<dyn Source>, entry: &Entry) -> Option<Self> {
        if !source.has_trickplay(entry) {
            return None;
        }
        let (send, receive) = bounded(1);
        Some(Self {
            query: Arc::new(Query {
                source,
                entry: entry.clone(),
                cancelled: AtomicBool::new(false),
            }),
            send,
            receive,
            pending: false,
            unavailable: false,
            current: None,
            failed: None,
            tile: None,
        })
    }

    /// Whether there is a picture to show for `hover`: its own, or while
    /// that loads, the one the pointer was on a moment ago, so moving along
    /// the bar doesn't blink. A newly decoded tile can then be taken once for
    /// upload by the panel that owns it.
    pub fn update(&mut self, hover: Option<f64>, duration: f64) -> bool {
        if let Ok(reply) = self.receive.try_recv() {
            self.pending = false;
            match reply {
                Reply::Tile(range, tile) => {
                    self.current = Some(range);
                    self.tile = Some(tile);
                }
                Reply::Failed(range) => self.failed = Some(range),
                Reply::Unavailable => self.unavailable = true,
            }
        }
        let seconds = hover.filter(|s| s.is_finite() && duration.is_finite() && duration > 0.0);
        let Some(seconds) = seconds.map(|s| s.clamp(0.0, duration)) else {
            // The next hover starts afresh rather than on this one's picture.
            self.failed = None;
            self.current = None;
            self.tile = None;
            return false;
        };
        if self
            .failed
            .is_some_and(|range| range.contains(seconds, duration))
        {
            return false;
        }
        if !self
            .current
            .is_some_and(|range| range.contains(seconds, duration))
            && !self.pending
            && !self.unavailable
        {
            if let Some(worker) = worker() {
                self.pending = worker
                    .try_send(Job {
                        query: self.query.clone(),
                        at: Position { seconds, duration },
                        reply: self.send.clone(),
                    })
                    .is_ok();
            } else {
                self.unavailable = true;
            }
        }
        self.current.is_some()
    }

    pub fn take_tile(&mut self) -> Option<ColorImage> {
        self.tile.take()
    }
}

impl Drop for Preview {
    fn drop(&mut self) {
        self.query.cancelled.store(true, Ordering::Relaxed);
    }
}

fn worker() -> Option<&'static Sender<Job>> {
    static WORKER: OnceLock<Option<Sender<Job>>> = OnceLock::new();
    WORKER
        .get_or_init(|| {
            let (send, receive) = bounded(1);
            std::thread::Builder::new()
                .name("fp-trickplay".into())
                .spawn(move || run_worker(&receive))
                .ok()?;
            Some(send)
        })
        .as_ref()
}

fn run_worker(receive: &Receiver<Job>) {
    let mut cache: Option<SheetCache> = None;
    loop {
        if cache.as_ref().is_some_and(|c| c.query.cancelled()) {
            cache = None;
        }
        let job = match if cache.is_some() {
            receive.recv_timeout(Duration::from_millis(250))
        } else {
            receive.recv().map_err(|_| RecvTimeoutError::Disconnected)
        } {
            Ok(job) => job,
            Err(RecvTimeoutError::Timeout) => continue,
            Err(RecvTimeoutError::Disconnected) => break,
        };
        if job.query.cancelled() {
            continue;
        }
        if !cache
            .as_ref()
            .is_some_and(|c| Arc::ptr_eq(&c.query, &job.query))
        {
            cache = None;
            if let Ok(Some(provider)) = job.query.source.trickplay(&job.query.entry) {
                cache = Some(SheetCache {
                    query: job.query.clone(),
                    provider,
                    sheets: Vec::new(),
                });
            }
        }
        let reply = cache
            .as_mut()
            .and_then(|c| c.tile(job.at))
            .unwrap_or(Reply::Unavailable);
        if matches!(reply, Reply::Unavailable) {
            cache = None;
        }
        let _ = job.reply.try_send(reply);
    }
}

/// A video's previews and the sheets (or listed pictures) decoded for it,
/// the most recently used last.
struct SheetCache {
    query: Arc<Query>,
    provider: Trickplay,
    sheets: Vec<(u32, DynamicImage)>,
}

impl SheetCache {
    fn get(&mut self, index: u32) -> Option<&DynamicImage> {
        if self.query.cancelled() {
            return None;
        }
        if let Some(at) = self.sheets.iter().position(|(i, _)| *i == index) {
            let sheet = self.sheets.remove(at);
            self.sheets.push(sheet);
        } else {
            let bytes = self
                .provider
                .fetch_sheet(index, &|| self.query.cancelled())
                .ok()?;
            if self.query.cancelled() {
                return None;
            }
            self.sheets.push((index, decode_sheet(&bytes)?));
            // As many as fit in the pixels of the largest sheet allowed:
            // going back and forth along the bar then fetches nothing again.
            // That is two of Jellyfin's usual sheets, one of DeoVR's, or a
            // few hundred of Emby's or Plex's pictures.
            let pixels = |sheets: &[(u32, DynamicImage)]| -> u64 {
                sheets
                    .iter()
                    .map(|(_, s)| u64::from(s.width()) * u64::from(s.height()))
                    .sum()
            };
            while self.sheets.len() > 1 && pixels(&self.sheets) > TRICKPLAY_MAX_PIXELS {
                self.sheets.remove(0);
            }
        }
        self.sheets.last().map(|(_, image)| image)
    }

    fn tile(&mut self, at: Position) -> Option<Reply> {
        let (index, bounds, crop) = match self.provider.timing.clone() {
            TrickplayTiming::EvenlySpaced { tile } => {
                let all = FrameRange {
                    duration: at.duration,
                    bounds: [0.0, f64::INFINITY],
                };
                let Some(sheet) = self.get(0) else {
                    return Some(Reply::Failed(all));
                };
                let columns = sheet.width() / tile[0].max(1);
                let frames = columns.checked_mul(sheet.height() / tile[1].max(1))?;
                let interval = at.duration / f64::from(frames);
                let frame = frame_at(at.seconds, interval, frames)?;
                let bounds = spaced(frame, frames, interval);
                (0, bounds, Some(tile_at(frame, columns, tile)?))
            }
            TrickplayTiming::Fixed {
                tile,
                interval_ms,
                frames,
                columns,
                rows,
            } => {
                let per_sheet = columns.checked_mul(rows).filter(|&n| n > 0)?;
                let interval = f64::from(interval_ms) / 1000.0;
                let frame = frame_at(at.seconds, interval, frames)?;
                let index = frame / per_sheet;
                let within = tile_at(frame % per_sheet, columns, tile)?;
                (index, spaced(frame, frames, interval), Some(within))
            }
            TrickplayTiming::Listed(times) => {
                // The last picture at or before the moment; before the first,
                // the first.
                let ms = (at.seconds * 1000.0).max(0.0) as u64;
                let frame = times.partition_point(|&t| t <= ms).saturating_sub(1);
                let lo = if frame == 0 {
                    0.0
                } else {
                    times[frame] as f64 / 1000.0
                };
                let hi = times
                    .get(frame + 1)
                    .map_or(f64::INFINITY, |&t| t as f64 / 1000.0);
                (u32::try_from(frame).ok()?, [lo, hi], None)
            }
        };
        let range = FrameRange {
            duration: at.duration,
            bounds,
        };
        let Some(sheet) = self.get(index) else {
            return Some(Reply::Failed(range));
        };
        let picture = match crop {
            Some([x, y, width, height]) => {
                if x.checked_add(width)? > sheet.width() || y.checked_add(height)? > sheet.height()
                {
                    return Some(Reply::Failed(range));
                }
                sheet.crop_imm(x, y, width, height)
            }
            None => sheet.clone(),
        };
        let rgba = picture.thumbnail(TILE_WIDTH, TILE_HEIGHT).into_rgba8();
        Some(Reply::Tile(
            range,
            ColorImage::from_rgba_unmultiplied(
                [rgba.width() as usize, rgba.height() as usize],
                rgba.as_raw(),
            ),
        ))
    }
}

/// Where tile `within` of a sheet `columns` tiles wide is: x, y, width,
/// height.
fn tile_at(within: u32, columns: u32, [width, height]: [u32; 2]) -> Option<[u32; 4]> {
    let columns = columns.max(1);
    Some([
        (within % columns).checked_mul(width)?,
        (within / columns).checked_mul(height)?,
        width,
        height,
    ])
}

/// The moments evenly spaced `frame` of `frames` covers: the last runs on
/// to the end.
fn spaced(frame: u32, frames: u32, interval: f64) -> [f64; 2] {
    let lo = f64::from(frame) * interval;
    let hi = if frame + 1 >= frames {
        f64::INFINITY
    } else {
        f64::from(frame + 1) * interval
    };
    [lo, hi]
}

fn frame_at(seconds: f64, interval: f64, frames: u32) -> Option<u32> {
    if interval.is_finite() && interval > 0.0 && frames > 0 {
        Some(((seconds / interval).floor() as u32).min(frames - 1))
    } else {
        None
    }
}

fn decode_sheet(bytes: &[u8]) -> Option<DynamicImage> {
    if bytes.len() as u64 > TRICKPLAY_SHEET_BYTES {
        return None;
    }
    let reader = || {
        let mut reader = ImageReader::new(Cursor::new(bytes))
            .with_guessed_format()
            .ok()?;
        let mut limits = Limits::default();
        limits.max_image_width = Some(TRICKPLAY_MAX_DIMENSION);
        limits.max_image_height = Some(TRICKPLAY_MAX_DIMENSION);
        limits.max_alloc = Some(TRICKPLAY_MAX_PIXELS * 4);
        reader.limits(limits);
        Some(reader)
    };
    let (width, height) = reader()?.into_dimensions().ok()?;
    if u64::from(width) * u64::from(height) > TRICKPLAY_MAX_PIXELS {
        return None;
    }
    reader()?.decode().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_boundaries_and_clamps_to_the_last_real_frame() {
        for (seconds, want) in [(0.0, 0), (4.99, 0), (5.0, 1), (19.9, 3), (30.0, 3)] {
            assert_eq!(frame_at(seconds, 5.0, 4), Some(want));
        }
        assert_eq!(frame_at(0.0, 0.0, 4), None);
        assert_eq!(frame_at(0.0, 5.0, 0), None);
    }
}
