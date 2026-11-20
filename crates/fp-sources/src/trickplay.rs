//! Seek previews a server makes for a video: pictures of moments along it,
//! as sheets of equal tiles or one picture per moment, read when wanted.

use crate::Result;
use std::sync::Arc;

/// Limits for compressed and decoded preview sheets.
pub const TRICKPLAY_SHEET_BYTES: u64 = 16 << 20;
pub const TRICKPLAY_MAX_DIMENSION: u32 = 8192;
pub const TRICKPLAY_MAX_PIXELS: u64 = 4096 * 4096;
/// The most pictures a server's list may name: two a second for a day.
pub const TRICKPLAY_MAX_FRAMES: usize = 2 * 24 * 60 * 60;

/// How the pictures map to the video's timeline. A sheet's tiles run left to
/// right, then top to bottom.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TrickplayTiming {
    /// One sheet of `tile`-sized tiles, all of them spread evenly over the
    /// video (DeoVR).
    EvenlySpaced { tile: [u32; 2] },
    /// Sheets of `columns` by `rows` tiles, one every `interval_ms`, `frames`
    /// in all; the last sheet may be partial (Jellyfin).
    Fixed {
        tile: [u32; 2],
        interval_ms: u32,
        frames: u32,
        columns: u32,
        rows: u32,
    },
    /// One picture per moment, in milliseconds, ascending: a moment shows the
    /// last picture at or before it, however unevenly they are spaced (Emby,
    /// Plex).
    Listed(Arc<[u64]>),
}

type Fetch = dyn Fn(u32, &dyn Fn() -> bool) -> Result<Vec<u8>> + Send + Sync;

/// A video's preview layout and its authenticated, bounded image loader.
pub struct Trickplay {
    pub timing: TrickplayTiming,
    fetch: Box<Fetch>,
}

impl Trickplay {
    /// `fetch` reads the numbered sheet (or listed picture), starting at
    /// zero, and checks for cancellation during the request.
    pub fn new(
        timing: TrickplayTiming,
        fetch: impl Fn(u32, &dyn Fn() -> bool) -> Result<Vec<u8>> + Send + Sync + 'static,
    ) -> Self {
        Self {
            timing,
            fetch: Box::new(fetch),
        }
    }

    pub fn fetch_sheet(&self, index: u32, cancelled: &dyn Fn() -> bool) -> Result<Vec<u8>> {
        (self.fetch)(index, cancelled)
    }
}

/// The index of a BIF file, the form Plex keeps preview pictures in: each
/// picture's moment in milliseconds and its bytes' range in the file.
pub(crate) struct BifIndex {
    pub times_ms: Vec<u64>,
    /// Start and end (exclusive) of each picture.
    pub ranges: Vec<(u64, u64)>,
}

const BIF_MAGIC: &[u8; 8] = b"\x89BIF\r\n\x1a\n";
/// A BIF file's header; its index follows.
pub(crate) const BIF_HEADER: u64 = 64;

/// From a BIF file's header: how many pictures it holds, and how many
/// milliseconds one unit of its index's times is.
pub(crate) fn bif_header(head: &[u8]) -> Option<(u32, u32)> {
    if head.len() < BIF_HEADER as usize || &head[..8] != BIF_MAGIC {
        return None;
    }
    let word = |at: usize| u32::from_le_bytes(head[at..at + 4].try_into().expect("four bytes"));
    let count = word(12);
    // No multiplier given means seconds.
    let multiplier = match word(16) {
        0 => 1000,
        m => m,
    };
    (count > 0 && count as usize <= TRICKPLAY_MAX_FRAMES).then_some((count, multiplier))
}

/// The bytes of a BIF index table to read after its header for `count`
/// pictures: one entry each, and one marking where the last ends.
pub(crate) fn bif_table_len(count: u32) -> u64 {
    8 * (u64::from(count) + 1)
}

/// Reads a BIF index table: pictures that run backwards or past the next
/// one's start are left out.
pub(crate) fn bif_index(table: &[u8], multiplier: u32) -> Option<BifIndex> {
    let entries: Vec<(u64, u64)> = table
        .as_chunks::<8>()
        .0
        .iter()
        .map(|&[t0, t1, t2, t3, o0, o1, o2, o3]| {
            (
                u64::from(u32::from_le_bytes([t0, t1, t2, t3])),
                u64::from(u32::from_le_bytes([o0, o1, o2, o3])),
            )
        })
        .collect();
    let mut index = BifIndex {
        times_ms: Vec::new(),
        ranges: Vec::new(),
    };
    for pair in entries.windows(2) {
        let ((time, start), (_, end)) = (pair[0], pair[1]);
        let time = time * u64::from(multiplier);
        let later = index.times_ms.last().is_none_or(|&last| time > last);
        if later && start >= BIF_HEADER && end > start && end - start <= TRICKPLAY_SHEET_BYTES {
            index.times_ms.push(time);
            index.ranges.push((start, end));
        }
    }
    (!index.times_ms.is_empty()).then_some(index)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// A BIF file of `pictures` taken `every` seconds apart.
    pub(crate) fn bif(pictures: &[&[u8]], every: u32) -> Vec<u8> {
        let mut out = BIF_MAGIC.to_vec();
        out.extend(0u32.to_le_bytes());
        out.extend((pictures.len() as u32).to_le_bytes());
        out.extend(1000u32.to_le_bytes());
        out.resize(BIF_HEADER as usize, 0);
        let mut offset = BIF_HEADER as u32 + 8 * (pictures.len() as u32 + 1);
        for (i, picture) in pictures.iter().enumerate() {
            out.extend((i as u32 * every).to_le_bytes());
            out.extend(offset.to_le_bytes());
            offset += picture.len() as u32;
        }
        out.extend(u32::MAX.to_le_bytes());
        out.extend(offset.to_le_bytes());
        for picture in pictures {
            out.extend(*picture);
        }
        out
    }

    #[test]
    fn a_bif_index_gives_each_pictures_moment_and_bytes() {
        let file = bif(&[b"one", b"two!", b"three"], 10);
        let (count, multiplier) = bif_header(&file).unwrap();
        assert_eq!((count, multiplier), (3, 1000));
        let start = BIF_HEADER as usize;
        let table = &file[start..start + bif_table_len(count) as usize];
        let index = bif_index(table, multiplier).unwrap();
        assert_eq!(index.times_ms, [0, 10_000, 20_000]);
        let read = |i: usize| {
            let (a, b) = index.ranges[i];
            &file[a as usize..b as usize]
        };
        assert_eq!(read(1), b"two!");
        assert_eq!(read(2), b"three");
    }

    #[test]
    fn a_bif_file_that_isnt_one_or_runs_backwards_is_refused() {
        assert!(
            bif_header(b"not a bif file at all, but long enough to be read as one....").is_none()
        );
        let mut file = bif(&[b"one", b"two"], 10);
        file[12..16].copy_from_slice(&0u32.to_le_bytes());
        assert!(bif_header(&file).is_none(), "no pictures");
        // The second picture's moment before the first's.
        let file = bif(&[b"one", b"two"], 10);
        let mut table = file[64..64 + 24].to_vec();
        table[8..12].copy_from_slice(&0u32.to_le_bytes());
        let index = bif_index(&table, 1000).unwrap();
        assert_eq!(index.times_ms, [0]);
    }
}
