//! Reading a whole file in order, for a download. One connection over the
//! headset's Wi-Fi fills only part of the link, and the block cache playback
//! reads through keeps 8 MiB and fetches 1 MiB at a time. This keeps up to
//! 64 MiB ahead of the reader, in 8 MiB pieces fetched by several requests
//! at once: separate connections over HTTP.

use crate::cache::RangeFetch;
use fp_core::ByteSource;
use fp_core::sync::lock;
use std::collections::BTreeMap;
use std::io;
use std::sync::{Arc, Condvar, Mutex, PoisonError};

/// Bytes each request asks for.
pub(crate) const PIECE: u64 = 8 << 20;
/// Most fetched ahead of the reader.
const AHEAD: u64 = 64 << 20;
/// Requests at once: neighbouring pieces of the file.
pub(crate) const REQUESTS: usize = 4;

/// A file read in order through pieces fetched ahead.
pub(crate) struct BulkSource<F: RangeFetch> {
    inner: Arc<Inner<F>>,
}

struct Inner<F> {
    fetcher: F,
    state: Mutex<State>,
    wake: Condvar,
}

#[derive(Default)]
struct State {
    /// The file offset piece 0 starts at: where reading started, or jumped
    /// to.
    base: u64,
    /// The next piece to fetch.
    next: u64,
    /// The piece the reader is in.
    reading: u64,
    pieces: BTreeMap<u64, Result<Vec<u8>, String>>,
    /// Changes with each jump, so pieces fetched for before it are dropped.
    generation: u64,
    /// The reader has said where it starts; nothing is fetched until then.
    started: bool,
    closing: bool,
}

impl State {
    /// Whether the fetchers may start on piece `self.next`: not too far ahead
    /// of the reader, nor past the end.
    fn may_fetch(&self, size: Option<u64>) -> bool {
        let ahead = self.next.saturating_sub(self.reading) * PIECE;
        let past_end = size.is_some_and(|s| self.base + self.next * PIECE >= s);
        self.started && ahead < AHEAD && !past_end
    }

    /// Starts again from `offset`: what was fetched for elsewhere goes.
    fn restart(&mut self, offset: u64) {
        self.base = offset;
        self.next = 0;
        self.reading = 0;
        self.pieces.clear();
        self.generation += 1;
    }
}

impl<F: RangeFetch> BulkSource<F> {
    /// Reads `fetcher`'s file with [`REQUESTS`] requests at once, or one at
    /// a time from a server that can only send it in order. `first` is the
    /// start of the file, already read: a whole piece, or the whole file.
    pub(crate) fn new(fetcher: F, first: Option<Vec<u8>>) -> BulkSource<F> {
        let mut state = State::default();
        if let Some(first) = first {
            state.pieces.insert(0, Ok(first));
            state.next = 1;
        }
        let inner = Arc::new(Inner {
            fetcher,
            state: Mutex::new(state),
            wake: Condvar::new(),
        });
        let workers = if inner.fetcher.sequential_only() {
            1
        } else {
            REQUESTS
        };
        for i in 0..workers {
            let worker = inner.clone();
            let spawned = std::thread::Builder::new()
                .name(format!("fp-bulk-{i}"))
                .spawn(move || worker.fetch_pieces());
            if let Err(e) = spawned {
                log::warn!("{}: no reader thread: {e}", inner.fetcher.describe());
            }
        }
        BulkSource { inner }
    }
}

impl<F: RangeFetch> Inner<F> {
    fn wait<'a>(&self, st: std::sync::MutexGuard<'a, State>) -> std::sync::MutexGuard<'a, State> {
        self.wake.wait(st).unwrap_or_else(PoisonError::into_inner)
    }

    /// The next piece to fetch, once there is room for it; `None` once
    /// closing.
    fn claim(&self) -> Option<(u64, u64, u64)> {
        let size = self.fetcher.size();
        let mut st = lock(&self.state);
        loop {
            if st.closing {
                return None;
            }
            if st.may_fetch(size) {
                let index = st.next;
                st.next += 1;
                return Some((index, st.generation, st.base + index * PIECE));
            }
            st = self.wait(st);
        }
    }

    /// A worker fetching one piece at a time.
    fn fetch_pieces(&self) {
        while let Some((index, generation, offset)) = self.claim() {
            let piece = self
                .fetcher
                .fetch(offset, PIECE as usize)
                .map_err(|e| e.to_string());
            let mut st = lock(&self.state);
            if st.generation == generation {
                st.pieces.insert(index, piece);
                self.wake.notify_all();
            }
        }
    }
}

impl<F: RangeFetch> ByteSource for BulkSource<F> {
    fn size(&self) -> Option<u64> {
        self.inner.fetcher.size()
    }

    fn remote(&self) -> bool {
        true
    }

    fn read_at(&self, offset: u64, buf: &mut [u8]) -> io::Result<usize> {
        let inner = &self.inner;
        if buf.is_empty() || inner.fetcher.size().is_some_and(|s| offset >= s) {
            return Ok(0);
        }
        let mut st = lock(&inner.state);
        if !st.started {
            // The start already read stands for reading from there.
            if !(offset == st.base && st.pieces.contains_key(&0)) {
                st.restart(offset);
            }
            st.started = true;
            inner.wake.notify_all();
        }
        let index = match offset.checked_sub(st.base).map(|d| d / PIECE) {
            Some(i) if i >= st.reading && i <= st.next => i,
            // Elsewhere than reading on: start again from there.
            _ => {
                st.restart(offset);
                0
            }
        };
        if index > st.reading {
            st.reading = index;
            st.pieces.retain(|i, _| *i >= index);
            inner.wake.notify_all();
        }
        loop {
            if st.closing {
                return Err(io::ErrorKind::Interrupted.into());
            }
            if st.pieces.contains_key(&index) {
                break;
            }
            st = inner.wait(st);
        }
        let start = st.base + index * PIECE;
        let piece = match st.pieces.get(&index) {
            Some(Ok(data)) => data,
            Some(Err(e)) => {
                let e = io::Error::other(e.clone());
                // Read again from here next time.
                st.restart(offset);
                inner.wake.notify_all();
                return Err(e);
            }
            None => unreachable!("waited for it"),
        };
        let within = (offset - start) as usize;
        if within >= piece.len() {
            return Ok(0);
        }
        let n = buf.len().min(piece.len() - within);
        buf[..n].copy_from_slice(&piece[within..within + n]);
        if within + n == piece.len() {
            st.pieces.remove(&index);
            st.reading = index + 1;
            inner.wake.notify_all();
        }
        Ok(n)
    }

    fn describe(&self) -> String {
        self.inner.fetcher.describe()
    }
}

impl<F: RangeFetch> Drop for BulkSource<F> {
    /// Lets its fetchers go: one inside a request ends with it.
    fn drop(&mut self) {
        lock(&self.inner.state).closing = true;
        self.inner.wake.notify_all();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A file in memory, noting where each request starts.
    struct Mem {
        data: Arc<Vec<u8>>,
        sequential: bool,
        asked: Mutex<Vec<u64>>,
    }

    impl Mem {
        fn new(len: usize) -> Mem {
            Mem {
                data: Arc::new((0..len).map(|i| (i % 253) as u8).collect()),
                sequential: false,
                asked: Mutex::new(Vec::new()),
            }
        }

        fn asked(&self) -> Vec<u64> {
            let mut asked = lock(&self.asked).clone();
            asked.sort_unstable();
            asked
        }
    }

    impl RangeFetch for Mem {
        fn fetch(&self, offset: u64, len: usize) -> io::Result<Vec<u8>> {
            lock(&self.asked).push(offset);
            let start = (offset as usize).min(self.data.len());
            let end = (start + len).min(self.data.len());
            Ok(self.data[start..end].to_vec())
        }
        fn size(&self) -> Option<u64> {
            Some(self.data.len() as u64)
        }
        fn describe(&self) -> String {
            "mem".into()
        }
        fn sequential_only(&self) -> bool {
            self.sequential
        }
    }

    fn read_all(src: &dyn ByteSource, from: u64, chunk: usize) -> Vec<u8> {
        let mut out = Vec::new();
        let mut buf = vec![0; chunk];
        let mut at = from;
        loop {
            let n = src.read_at(at, &mut buf).unwrap();
            if n == 0 {
                return out;
            }
            out.extend_from_slice(&buf[..n]);
            at += n as u64;
        }
    }

    #[test]
    fn pieces_fetched_at_once_come_back_in_order() {
        let mem = Mem::new(3 * PIECE as usize + 12345);
        let data = mem.data.clone();
        let src = BulkSource::new(mem, None);
        assert_eq!(read_all(&src, 0, 3 << 20), *data);
        assert_eq!(src.inner.fetcher.asked(), [0, PIECE, 2 * PIECE, 3 * PIECE]);
    }

    #[test]
    fn a_download_part_way_starts_where_it_left_off() {
        let mem = Mem::new(2 * PIECE as usize + 99);
        let data = mem.data.clone();
        let src = BulkSource::new(mem, None);
        let from = PIECE + 5;
        assert_eq!(read_all(&src, from, 1 << 20), data[from as usize..]);
        assert_eq!(src.inner.fetcher.asked(), [from, from + PIECE]);
    }

    #[test]
    fn the_first_piece_read_already_isnt_read_again() {
        let mem = Mem::new(PIECE as usize + 10);
        let data = mem.data.clone();
        let first = data[..PIECE as usize].to_vec();
        let src = BulkSource::new(mem, Some(first));
        assert_eq!(read_all(&src, 0, 1 << 20), *data);
        assert_eq!(src.inner.fetcher.asked(), [PIECE]);
    }

    #[test]
    fn a_server_that_sends_only_in_order_is_asked_one_piece_at_a_time() {
        let mut mem = Mem::new(2 * PIECE as usize + 1);
        mem.sequential = true;
        let data = mem.data.clone();
        let src = BulkSource::new(mem, None);
        assert_eq!(read_all(&src, 0, 4 << 20), *data);
        // In order, as asked.
        assert_eq!(*lock(&src.inner.fetcher.asked), [0, PIECE, 2 * PIECE]);
    }
}
