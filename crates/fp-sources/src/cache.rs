//! Block cache with background read-ahead, shared by the HTTP and SMB
//! readers.
//!
//! The demuxer reads 32-256 KiB at a time, mostly sequentially, with
//! occasional seeks (often to the end of an MP4 for its `moov` atom). Each
//! network round trip is expensive, so reads are served from fixed-size
//! blocks (1 MiB by default) kept in a small LRU. When access is sequential
//! the next block is fetched on a background thread so playback never waits
//! on the network for data it was always going to need.

use fp_core::ByteSource;
use fp_core::source::NetCounters;
use fp_core::sync::lock;
use std::collections::HashMap;
use std::io;
use std::sync::atomic::Ordering;
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Condvar, Mutex, PoisonError, Weak};
use std::time::Instant;

/// Fetches byte ranges from the underlying file.
pub(crate) trait RangeFetch: Send + Sync + 'static {
    /// Reads up to `len` bytes at `offset`. Returns fewer bytes only at end
    /// of file, and an empty vector past it.
    fn fetch(&self, offset: u64, len: usize) -> io::Result<Vec<u8>>;
    /// Total size, when known.
    fn size(&self) -> Option<u64>;
    /// Redacted description for logs.
    fn describe(&self) -> String;
    /// True when ranges must be fetched in increasing order (a server
    /// without range support streamed sequentially): read-ahead is then
    /// disabled because it could skip past data the reader still needs.
    fn sequential_only(&self) -> bool {
        false
    }
}

/// Tuning of the block cache.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CacheOptions {
    /// Bytes per block (and per network request). Default 1 MiB.
    pub block_size: usize,
    /// Blocks kept in memory. Default 8 (8 MiB per open file). At least 3
    /// are always kept so a read-ahead block is not evicted before use.
    pub max_blocks: usize,
    /// Fetch the next block in the background when reads are sequential.
    pub read_ahead: bool,
}

impl Default for CacheOptions {
    fn default() -> Self {
        CacheOptions {
            block_size: 1 << 20,
            max_blocks: 8,
            read_ahead: true,
        }
    }
}

/// Counters for one open file, mainly for tests and diagnostics.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CacheStats {
    /// Blocks fetched for a reader that was waiting.
    pub fetches: u64,
    /// Blocks fetched by the read-ahead thread.
    pub read_aheads: u64,
    /// Block lookups served from memory (including blocks a reader waited
    /// on while the read-ahead thread was fetching them).
    pub hits: u64,
}

enum Slot {
    Ready { data: Arc<Vec<u8>>, used: u64 },
    Loading,
}

struct State {
    blocks: HashMap<u64, Slot>,
    tick: u64,
    last_block: Option<u64>,
    stats: CacheStats,
}

struct Inner<F: RangeFetch> {
    fetcher: F,
    block_size: u64,
    max_blocks: usize,
    read_ahead: bool,
    state: Mutex<State>,
    loaded: Condvar,
    ahead: Mutex<Option<Sender<u64>>>,
    net: Arc<NetCounters>,
}

/// A [`ByteSource`] reading through the block cache.
pub(crate) struct CachedSource<F: RangeFetch> {
    inner: Arc<Inner<F>>,
}

impl<F: RangeFetch> CachedSource<F> {
    pub(crate) fn new(fetcher: F, opts: &CacheOptions) -> Self {
        Self::counted(fetcher, opts, Default::default())
    }

    /// [`Self::new`], counting its reads in `net`, which the fetcher may
    /// count its reconnects in.
    pub(crate) fn counted(fetcher: F, opts: &CacheOptions, net: Arc<NetCounters>) -> Self {
        CachedSource {
            inner: Arc::new(Inner {
                fetcher,
                block_size: opts.block_size.max(4096) as u64,
                max_blocks: opts.max_blocks.max(3),
                read_ahead: opts.read_ahead,
                state: Mutex::new(State {
                    blocks: HashMap::new(),
                    tick: 0,
                    last_block: None,
                    stats: CacheStats::default(),
                }),
                loaded: Condvar::new(),
                ahead: Mutex::new(None),
                net,
            }),
        }
    }

    /// Puts an already fetched block in the cache (the HTTP probe request
    /// returns block 0 with the size).
    pub(crate) fn seed(&self, index: u64, data: Vec<u8>) {
        let mut st = lock(&self.inner.state);
        self.inner.insert_ready(&mut st, index, Arc::new(data));
    }

    pub(crate) fn fetcher(&self) -> &F {
        &self.inner.fetcher
    }

    pub(crate) fn stats(&self) -> CacheStats {
        lock(&self.inner.state).stats
    }

    pub(crate) fn block_size(&self) -> u64 {
        self.inner.block_size
    }
}

impl<F: RangeFetch> Inner<F> {
    fn insert_ready(&self, st: &mut State, index: u64, data: Arc<Vec<u8>>) {
        st.tick += 1;
        let used = st.tick;
        st.blocks.insert(index, Slot::Ready { data, used });
        let ready = st
            .blocks
            .values()
            .filter(|s| matches!(s, Slot::Ready { .. }))
            .count();
        if ready > self.max_blocks {
            let victim = st
                .blocks
                .iter()
                .filter(|(i, _)| **i != index)
                .filter_map(|(i, s)| match s {
                    Slot::Ready { used, .. } => Some((*used, *i)),
                    Slot::Loading => None,
                })
                .min()
                .map(|(_, i)| i);
            if let Some(v) = victim {
                st.blocks.remove(&v);
            }
        }
    }

    /// Returns block `index`, fetching it (or waiting for the read-ahead
    /// thread to finish fetching it) when needed.
    fn block(&self, index: u64) -> io::Result<Arc<Vec<u8>>> {
        let mut st = lock(&self.state);
        loop {
            st.tick += 1;
            let tick = st.tick;
            match st.blocks.get_mut(&index) {
                Some(Slot::Ready { data, used }) => {
                    *used = tick;
                    let data = data.clone();
                    st.stats.hits += 1;
                    return Ok(data);
                }
                Some(Slot::Loading) => {
                    st = self.loaded.wait(st).unwrap_or_else(PoisonError::into_inner);
                }
                None => break,
            }
        }
        st.blocks.insert(index, Slot::Loading);
        st.stats.fetches += 1;
        drop(st);
        self.load(index)
    }

    /// Fetches a block already marked `Loading` and publishes the result.
    fn load(&self, index: u64) -> io::Result<Arc<Vec<u8>>> {
        let started = Instant::now();
        let result = self
            .fetcher
            .fetch(index * self.block_size, self.block_size as usize);
        if let Ok(data) = &result {
            let net = &self.net;
            net.bytes.fetch_add(data.len() as u64, Ordering::Relaxed);
            net.reads.fetch_add(1, Ordering::Relaxed);
            let ns = started.elapsed().as_nanos() as u64;
            net.read_ns.fetch_add(ns, Ordering::Relaxed);
        }
        let mut st = lock(&self.state);
        let out = match result {
            Ok(data) => {
                let data = Arc::new(data);
                self.insert_ready(&mut st, index, data.clone());
                Ok(data)
            }
            Err(e) => {
                st.blocks.remove(&index);
                Err(e)
            }
        };
        drop(st);
        self.loaded.notify_all();
        out
    }

    /// Fetches a block reserved for read-ahead (see
    /// [`CachedSource::note_access`]).
    fn read_ahead(&self, index: u64) {
        lock(&self.state).stats.read_aheads += 1;
        if let Err(e) = self.load(index) {
            log::debug!(
                "read-ahead of block {index} failed for {}: {e}",
                self.fetcher.describe()
            );
        }
    }

    /// Gives up a block reserved for read-ahead; a reader waiting for it
    /// fetches it itself.
    fn unreserve(&self, index: u64) {
        let mut st = lock(&self.state);
        if matches!(st.blocks.get(&index), Some(Slot::Loading)) {
            st.blocks.remove(&index);
        }
        drop(st);
        self.loaded.notify_all();
    }
}

fn read_ahead_worker<F: RangeFetch>(weak: Weak<Inner<F>>, rx: Receiver<u64>) {
    while let Ok(mut index) = rx.recv() {
        let Some(inner) = weak.upgrade() else { break };
        // After a seek only the newest request matters.
        while let Ok(newer) = rx.try_recv() {
            inner.unreserve(index);
            index = newer;
        }
        inner.read_ahead(index);
    }
}

impl<F: RangeFetch> CachedSource<F> {
    /// Records that the reader used block `index` and starts a read-ahead of
    /// the next block when access looks sequential.
    fn note_access(&self, index: u64) {
        let inner = &self.inner;
        if !inner.read_ahead || inner.fetcher.sequential_only() {
            return;
        }
        let next = index + 1;
        {
            let mut st = lock(&inner.state);
            let sequential = matches!(st.last_block, Some(l) if l == index || l + 1 == index);
            st.last_block = Some(index);
            let in_file = inner
                .fetcher
                .size()
                .is_none_or(|s| next * inner.block_size < s);
            if !sequential || !in_file || st.blocks.contains_key(&next) {
                return;
            }
            // Reserved now, not when the thread gets to it: a reader that
            // gets there first waits for the read-ahead instead of fetching
            // the block itself.
            st.blocks.insert(next, Slot::Loading);
        }
        let mut ahead = lock(&inner.ahead);
        if ahead.is_none() {
            let (tx, rx) = mpsc::channel();
            let weak = Arc::downgrade(inner);
            match std::thread::Builder::new()
                .name("fp-read-ahead".into())
                .spawn(move || read_ahead_worker(weak, rx))
            {
                Ok(_) => *ahead = Some(tx),
                Err(e) => log::warn!("cannot start read-ahead thread: {e}"),
            }
        }
        let sent = ahead.as_ref().is_some_and(|tx| tx.send(next).is_ok());
        drop(ahead);
        if !sent {
            inner.unreserve(next);
        }
    }
}

impl<F: RangeFetch> ByteSource for CachedSource<F> {
    fn size(&self) -> Option<u64> {
        self.inner.fetcher.size()
    }

    fn read_at(&self, offset: u64, buf: &mut [u8]) -> io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        let size = self.size();
        let bs = self.inner.block_size;
        let mut done = 0usize;
        while done < buf.len() {
            let pos = offset + done as u64;
            if size.is_some_and(|s| pos >= s) {
                break;
            }
            let index = pos / bs;
            let block = match self.inner.block(index) {
                Ok(b) => b,
                // Return what we have; the caller will ask again.
                Err(_) if done > 0 => break,
                Err(e) => return Err(e),
            };
            self.note_access(index);
            let within = (pos - index * bs) as usize;
            if within >= block.len() {
                break; // past the end of a short (final) block
            }
            let n = (block.len() - within).min(buf.len() - done);
            buf[done..done + n].copy_from_slice(&block[within..within + n]);
            done += n;
            if (block.len() as u64) < bs {
                break; // a short block is the last one
            }
        }
        Ok(done)
    }

    fn describe(&self) -> String {
        self.inner.fetcher.describe()
    }

    fn net(&self) -> Option<Arc<NetCounters>> {
        Some(self.inner.net.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::{Duration, Instant};

    struct Mem {
        data: Vec<u8>,
        calls: Arc<AtomicUsize>,
        fail_at: Option<u64>,
        sequential: bool,
    }

    impl RangeFetch for Mem {
        fn fetch(&self, offset: u64, len: usize) -> io::Result<Vec<u8>> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            if self.fail_at == Some(offset) {
                return Err(io::Error::other("boom"));
            }
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

    fn data(n: usize) -> Vec<u8> {
        (0..n).map(|i| (i * 7 % 251) as u8).collect()
    }

    fn source(n: usize, opts: CacheOptions) -> (CachedSource<Mem>, Arc<AtomicUsize>, Vec<u8>) {
        let d = data(n);
        let calls = Arc::new(AtomicUsize::new(0));
        let mem = Mem {
            data: d.clone(),
            calls: calls.clone(),
            fail_at: None,
            sequential: false,
        };
        (CachedSource::new(mem, &opts), calls, d)
    }

    fn opts(block: usize, blocks: usize, ahead: bool) -> CacheOptions {
        CacheOptions {
            block_size: block,
            max_blocks: blocks,
            read_ahead: ahead,
        }
    }

    #[test]
    fn reads_across_blocks_and_eof() {
        let (s, _, d) = source(10_000, opts(4096, 4, false));
        let mut buf = vec![0u8; 5000];
        assert_eq!(s.read_at(3000, &mut buf).unwrap(), 5000);
        assert_eq!(buf, d[3000..8000]);
        assert_eq!(s.read_at(9000, &mut buf).unwrap(), 1000);
        assert_eq!(buf[..1000], d[9000..]);
        assert_eq!(s.read_at(10_000, &mut buf).unwrap(), 0);
        assert_eq!(s.read_at(50_000, &mut buf).unwrap(), 0);
        assert_eq!(s.read_at(0, &mut []).unwrap(), 0);
    }

    #[test]
    fn what_comes_over_the_network_is_counted() {
        // The performance overlay's throughput and read times: only what was
        // fetched, not what the cache served again.
        let (s, _, _) = source(10_000, opts(4096, 4, false));
        let mut buf = vec![0u8; 10_000];
        for _ in 0..2 {
            assert_eq!(s.read_at(0, &mut buf).unwrap(), 10_000);
        }
        let net = s.net().unwrap();
        let count = |c: &std::sync::atomic::AtomicU64| c.load(Ordering::Relaxed);
        assert_eq!((count(&net.bytes), count(&net.reads)), (10_000, 3));
        assert!(count(&net.read_ns) > 0);
    }

    #[test]
    fn caches_blocks_and_evicts_lru() {
        let (s, calls, _) = source(64 * 1024, opts(4096, 3, false));
        let mut buf = [0u8; 100];
        for _ in 0..5 {
            s.read_at(10, &mut buf).unwrap();
        }
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        s.read_at(5000, &mut buf).unwrap(); // block 1
        s.read_at(9000, &mut buf).unwrap(); // block 2
        s.read_at(10, &mut buf).unwrap(); // block 0 again: hit, now most recent
        s.read_at(13_000, &mut buf).unwrap(); // block 3 evicts block 1
        assert_eq!(calls.load(Ordering::SeqCst), 4);
        s.read_at(10, &mut buf).unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 4);
        s.read_at(5000, &mut buf).unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 5);
        let st = s.stats();
        assert_eq!(st.fetches, 5);
        assert!(st.hits >= 6);
    }

    #[test]
    fn sequential_reads_trigger_read_ahead() {
        let (s, _, d) = source(64 * 1024, opts(4096, 8, true));
        let mut buf = vec![0u8; 1024];
        let mut pos = 0u64;
        while pos < 8192 {
            let n = s.read_at(pos, &mut buf).unwrap();
            assert_eq!(buf[..n], d[pos as usize..pos as usize + n]);
            pos += n as u64;
        }
        // Block 2 is fetched in the background; wait for it.
        let deadline = Instant::now() + Duration::from_secs(5);
        while s.stats().read_aheads == 0 && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(s.stats().read_aheads >= 1);
        let before = s.stats().fetches;
        s.read_at(8192, &mut buf).unwrap();
        assert_eq!(s.stats().fetches, before, "block 2 came from read-ahead");
        assert_eq!(buf[..], d[8192..9216]);
    }

    #[test]
    fn a_reader_ahead_of_the_read_ahead_waits_for_it() {
        // Repeated: the reader has to get there before the thread runs.
        for _ in 0..50 {
            let (s, _, d) = source(64 * 1024, opts(4096, 8, true));
            let mut buf = vec![0u8; 2048];
            s.read_at(0, &mut buf).unwrap();
            s.read_at(2048, &mut buf).unwrap();
            s.read_at(4096, &mut buf).unwrap();
            assert_eq!(buf[..], d[4096..6144]);
            let st = s.stats();
            assert_eq!(st.fetches, 1, "only block 0 by a reader: {st:?}");
        }
    }

    #[test]
    fn random_access_does_not_read_ahead() {
        let (s, _, _) = source(64 * 1024, opts(4096, 8, true));
        let mut buf = [0u8; 16];
        for off in [0u64, 40_000, 9000, 60_000, 20_000] {
            s.read_at(off, &mut buf).unwrap();
        }
        std::thread::sleep(Duration::from_millis(50));
        assert_eq!(s.stats().read_aheads, 0);
    }

    #[test]
    fn sequential_only_fetchers_never_read_ahead() {
        let calls = Arc::new(AtomicUsize::new(0));
        let mem = Mem {
            data: data(32 * 1024),
            calls: calls.clone(),
            fail_at: None,
            sequential: true,
        };
        let s = CachedSource::new(mem, &opts(4096, 4, true));
        let mut buf = [0u8; 4096];
        for i in 0..4 {
            s.read_at(i * 4096, &mut buf).unwrap();
        }
        std::thread::sleep(Duration::from_millis(30));
        assert_eq!(s.stats().read_aheads, 0);
        assert_eq!(calls.load(Ordering::SeqCst), 4);
    }

    #[test]
    fn errors_propagate_and_partial_reads_succeed() {
        let calls = Arc::new(AtomicUsize::new(0));
        let mem = Mem {
            data: data(16 * 1024),
            calls,
            fail_at: Some(4096),
            sequential: false,
        };
        let s = CachedSource::new(mem, &opts(4096, 4, false));
        let mut buf = vec![0u8; 6000];
        // First block succeeds, second fails: a short read, not an error.
        assert_eq!(s.read_at(0, &mut buf).unwrap(), 4096);
        assert!(s.read_at(5000, &mut buf).is_err());
    }

    #[test]
    fn seeded_block_is_served_without_fetch() {
        let (s, calls, d) = source(10_000, opts(4096, 4, false));
        s.seed(0, d[..4096].to_vec());
        let mut buf = [0u8; 10];
        s.read_at(5, &mut buf).unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        assert_eq!(buf, d[5..15]);
    }

    #[test]
    fn concurrent_readers_share_fetches() {
        let (s, calls, d) = source(256 * 1024, opts(16 * 1024, 32, false));
        let s = Arc::new(s);
        let threads: Vec<_> = (0..8)
            .map(|t| {
                let s = s.clone();
                let d = d.clone();
                std::thread::spawn(move || {
                    let mut buf = vec![0u8; 3000];
                    for i in 0..50u64 {
                        let off = (i * 5003 + t * 17) % 250_000;
                        let n = s.read_at(off, &mut buf).unwrap();
                        assert_eq!(buf[..n], d[off as usize..off as usize + n]);
                    }
                })
            })
            .collect();
        for t in threads {
            t.join().unwrap();
        }
        // 16 blocks in the file, each fetched exactly once.
        assert!(calls.load(Ordering::SeqCst) <= 16);
    }

    #[test]
    fn drop_stops_read_ahead_thread() {
        let (s, _, _) = source(64 * 1024, opts(4096, 8, true));
        let mut buf = [0u8; 4096];
        s.read_at(0, &mut buf).unwrap();
        s.read_at(4096, &mut buf).unwrap();
        let weak = Arc::downgrade(&s.inner);
        drop(s);
        let deadline = Instant::now() + Duration::from_secs(5);
        while weak.upgrade().is_some() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(weak.upgrade().is_none());
    }
}
