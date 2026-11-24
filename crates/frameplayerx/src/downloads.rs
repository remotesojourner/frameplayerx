//! Downloads: a network source's videos copied to the headset or a card, so
//! they play with no connection. One downloads at a time, in the order
//! asked for, into a folder of its own with its subtitle files, haptic
//! scripts and picture; the library keeps where ([`fp_library::Download`]).
//! A download waits while a video streams from a network source, and
//! carries on from where it got to after a dropped connection or a restart.

use crate::services::Opener;
use fp_core::source::{Entry, EntryKind, SubtitleFile};
use fp_core::sync::lock;
use fp_core::volume;
use fp_library::{Download, DownloadState, Library, MediaId};
use std::collections::{HashMap, HashSet};
use std::io::{Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

/// The folder on a card that downloads go in: hidden, so a library on the
/// card doesn't take them for videos of its own.
pub const CARD_FOLDER: &str = ".frameplayerx-downloads";

/// What a download's picture is saved as in its folder.
pub const PICTURE: &str = "picture";

/// Read and written at a time.
const CHUNK: usize = 4 << 20;
/// How often what has come is made safe on the disk and noted in the
/// library, so a restart carries on from there.
const SAVE_EVERY: Duration = Duration::from_secs(5);
/// Room left free on the disk beyond the video.
const SPARE: u64 = 256 << 20;
/// Side files and pictures bigger than this aren't kept.
const SIDE_LIMIT: u64 = 64 << 20;
/// How long a download that lost its connection waits before it tries
/// again, at most.
const RETRY_MAX: Duration = Duration::from_secs(300);

/// The folder downloads go in on the headset.
pub fn headset_root() -> PathBuf {
    fp_core::dirs::data_dir().join("downloads")
}

/// Where new downloads go: the headset, or `card` (Settings › General ›
/// Download to).
pub fn root_location(card: Option<&fp_sources::CardRef>) -> String {
    match card {
        None => headset_root().to_string_lossy().into_owned(),
        Some(c) => format!("{}{}/{CARD_FOLDER}", volume::SCHEME, c.id),
    }
}

/// A path for `location` while its card is inserted (a path stays one).
pub fn path_of(location: &str) -> Option<PathBuf> {
    volume::resolve(location).map(|p| PathBuf::from(p.as_ref()))
}

/// Bytes free on the disk `path` is on.
pub fn free_space(path: &Path) -> Option<u64> {
    use std::os::unix::ffi::OsStrExt;
    let c = std::ffi::CString::new(path.as_os_str().as_bytes()).ok()?;
    // SAFETY: statvfs is plain data; all-zero is valid.
    let mut s: libc::statvfs = unsafe { std::mem::zeroed() };
    // SAFETY: `c` is a valid C string and `s` a valid statvfs to fill.
    if unsafe { libc::statvfs(c.as_ptr(), &mut s) } != 0 {
        return None;
    }
    Some(s.f_bavail as u64 * s.f_frsize as u64)
}

/// The nearest existing folder of `path`, to ask its disk how much room it
/// has before the download's own folder is made.
fn existing_ancestor(path: &Path) -> Option<&Path> {
    path.ancestors().find(|p| p.is_dir())
}

/// Bytes free where `location` is, or would be made; `None` while its card
/// is out.
pub fn room_at(location: &str) -> Option<u64> {
    let path = path_of(location)?;
    existing_ancestor(&path).and_then(free_space)
}

/// A name every filesystem a card can have takes: no folder separators or
/// characters FAT and exFAT refuse, not hidden, not too long.
pub fn safe_name(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|c| {
            if c.is_control() || matches!(c, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|') {
                '_'
            } else {
                c
            }
        })
        .collect();
    let mut cleaned = cleaned.trim().trim_start_matches('.').trim().to_string();
    // 255 bytes is the most a name can have; a part file adds 5.
    while cleaned.len() > 200 {
        cleaned.pop();
    }
    let cleaned = cleaned.trim_end_matches(['.', ' ']).to_string();
    if cleaned.is_empty() {
        "video".into()
    } else {
        cleaned
    }
}

/// A download's folder name: random, so two videos of one name don't meet.
fn folder_key() -> String {
    use ring::rand::SecureRandom;
    let mut bytes = [0u8; 8];
    if ring::rand::SystemRandom::new().fill(&mut bytes).is_err() {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos() as u64);
        bytes = nanos.to_le_bytes();
    }
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// A new download's own folder under `root`.
pub fn new_folder(root: &str) -> String {
    format!("{}/{}", root.trim_end_matches('/'), folder_key())
}

/// How the download under way is going.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Progress {
    pub media_id: MediaId,
    pub received: u64,
    pub size: Option<u64>,
    /// Bytes a second since it last started.
    pub speed: f64,
}

impl Progress {
    /// How much has come, 0 to 1, when the size is known.
    pub fn fraction(&self) -> Option<f64> {
        self.size
            .filter(|s| *s > 0)
            .map(|s| (self.received as f64 / s as f64).clamp(0.0, 1.0))
    }
}

/// What the app hears from the downloads.
#[derive(Clone, Debug, PartialEq)]
pub enum Event {
    /// A video downloaded in full.
    Finished { media_id: MediaId, title: String },
    /// A download stopped for good, until retried.
    Failed {
        media_id: MediaId,
        title: String,
        error: String,
    },
}

#[derive(Default)]
struct State {
    quit: bool,
    /// Something to look at: a download added, retried or removed.
    woken: bool,
    /// A video streams from a network source: downloads wait.
    streaming: bool,
    /// Removed while under way.
    cancelled: HashSet<MediaId>,
    current: Option<Progress>,
    /// Why downloads that lost their connection wait to try again.
    retrying: HashMap<MediaId, String>,
    /// Bumped when anything shown about downloads changes.
    generation: u64,
}

struct Shared {
    state: Mutex<State>,
    wake: Condvar,
}

impl Shared {
    fn bump(&self) {
        lock(&self.state).generation += 1;
    }
}

/// The downloads and the thread that does them.
pub struct Downloads {
    shared: Arc<Shared>,
    library: Library,
    opener: Arc<Opener>,
    events: Mutex<Receiver<Event>>,
    /// Where the headset's downloads are ([`root_location`]).
    headset: String,
}

impl Downloads {
    /// Starts downloading what is waiting, and reads finished downloads
    /// rather than their servers from now on. `headset` is where the
    /// headset's downloads are.
    pub fn start(library: Library, opener: Arc<Opener>, headset: String) -> Downloads {
        let shared = Arc::new(Shared {
            state: Mutex::default(),
            wake: Condvar::new(),
        });
        match library.all_downloads() {
            Ok(all) => {
                for (location, d) in all {
                    if d.state == DownloadState::Done {
                        opener.set_local_copy(&location, Some(d.video_location()));
                    }
                }
            }
            Err(e) => log::warn!("reading the downloads: {e}"),
        }
        let (tx, rx) = channel();
        let worker = Worker {
            shared: shared.clone(),
            library: library.clone(),
            opener: opener.clone(),
            events: tx,
            retry_at: HashMap::new(),
            tries: HashMap::new(),
        };
        if let Err(e) = std::thread::Builder::new()
            .name("fp-downloads".into())
            .spawn(move || worker.run())
        {
            log::error!("downloads: cannot start their thread: {e}");
        }
        let downloads = Downloads {
            shared,
            library,
            opener,
            events: Mutex::new(rx),
            headset,
        };
        downloads.clear_leftovers();
        downloads
    }

    /// Looks at the waiting downloads again: one was added or retried.
    pub fn wake(&self) {
        let mut st = lock(&self.shared.state);
        st.woken = true;
        st.generation += 1;
        self.shared.wake.notify_all();
    }

    /// A video is streaming from a network source, or stopped: downloads
    /// wait while one is.
    pub fn set_streaming(&self, streaming: bool) {
        let mut st = lock(&self.shared.state);
        if st.streaming != streaming {
            st.streaming = streaming;
            st.woken = true;
            self.shared.wake.notify_all();
        }
    }

    /// The download under way.
    pub fn progress(&self) -> Option<Progress> {
        lock(&self.shared.state).current
    }

    /// Why video `id`'s download waits to try again, when it lost its
    /// connection.
    pub fn retrying(&self, id: MediaId) -> Option<String> {
        lock(&self.shared.state).retrying.get(&id).cloned()
    }

    /// Changes whenever something about downloads that is shown does.
    pub fn generation(&self) -> u64 {
        lock(&self.shared.state).generation
    }

    pub fn take_events(&self) -> Vec<Event> {
        lock(&self.events).try_iter().collect()
    }

    /// Stops and forgets video `id`'s download, deleting its files; the
    /// video and its history stay. Touches the disk, so not on the frame
    /// loop.
    pub fn remove(&self, id: MediaId) -> Result<(), String> {
        let folder = self.forget(id)?;
        if let Some(folder) = folder {
            delete_folder(&folder);
        }
        self.wake();
        Ok(())
    }

    /// Stops and forgets video `id`'s download, as its video is about to
    /// go; its folder, for [`delete_folders`]. Doesn't touch the disk.
    pub fn release(&self, id: MediaId) -> Option<String> {
        self.forget(id).unwrap_or_else(|e| {
            log::warn!("forgetting the download of {id}: {e}");
            None
        })
    }

    fn forget(&self, id: MediaId) -> Result<Option<String>, String> {
        {
            let mut st = lock(&self.shared.state);
            if st.current.is_some_and(|p| p.media_id == id) {
                st.cancelled.insert(id);
            }
            st.generation += 1;
        }
        let location = self.library.get(id).ok().flatten().map(|r| r.location);
        let removed = self
            .library
            .remove_download(id)
            .map_err(|e| e.to_string())?;
        if let Some(location) = location {
            self.opener.set_local_copy(&location, None);
        }
        Ok(removed.map(|d| d.folder))
    }

    /// Deletes folders in the download folders that no download owns: left
    /// by a video removed with its source, or by a removal that couldn't
    /// delete them then. Cards that are out are left until next time.
    pub fn clear_leftovers(&self) {
        let Ok(owned) = self.library.download_folders() else {
            return;
        };
        let mut roots = vec![self.headset.clone()];
        roots.extend(
            volume::mounted()
                .into_iter()
                .filter_map(|v| v.id)
                .map(|id| format!("{}{id}/{CARD_FOLDER}", volume::SCHEME)),
        );
        for root in roots {
            let Some(dir) = path_of(&root) else {
                continue;
            };
            let Ok(read) = std::fs::read_dir(&dir) else {
                continue;
            };
            for item in read.flatten() {
                let Some(name) = item.file_name().to_str().map(str::to_string) else {
                    continue;
                };
                if item.file_type().is_ok_and(|t| t.is_dir())
                    && !owned.contains(&format!("{root}/{name}"))
                {
                    log::info!("downloads: removing {name}, which no download owns");
                    let _ = std::fs::remove_dir_all(item.path());
                }
            }
        }
    }

    /// Lets go of every download of source `source_id`, which is being
    /// removed with its videos: the one under way stops, and their videos
    /// aren't read from them any more. Their folders, for
    /// [`delete_folders`] once the videos are gone.
    pub fn release_source(&self, source_id: &str) -> Vec<String> {
        let all = self.library.all_downloads().unwrap_or_default();
        let mut folders = Vec::new();
        for (location, d) in all {
            let of_source = self
                .library
                .get(d.media_id)
                .ok()
                .flatten()
                .is_some_and(|r| r.source_id == source_id);
            if !of_source {
                continue;
            }
            {
                let mut st = lock(&self.shared.state);
                if st.current.is_some_and(|p| p.media_id == d.media_id) {
                    st.cancelled.insert(d.media_id);
                }
            }
            self.opener.set_local_copy(&location, None);
            folders.push(d.folder);
        }
        self.shared.bump();
        folders
    }
}

/// Deletes downloads' folders. Touches the disk, so not on the frame loop.
pub fn delete_folders(folders: &[String]) {
    for f in folders {
        delete_folder(f);
    }
}

impl Drop for Downloads {
    fn drop(&mut self) {
        let mut st = lock(&self.shared.state);
        st.quit = true;
        self.shared.wake.notify_all();
    }
}

fn delete_folder(folder: &str) {
    if let Some(path) = path_of(folder)
        && let Err(e) = std::fs::remove_dir_all(&path)
        && e.kind() != std::io::ErrorKind::NotFound
    {
        log::warn!("deleting download {}: {e}", path.display());
    }
}

/// Why a download stopped before the end.
#[derive(Debug)]
enum Stop {
    /// To carry on later from where it got to: FramePlayerX is quitting, a
    /// video is streaming, or its card is out.
    Paused,
    /// Removed while under way.
    Cancelled,
    /// The connection went: tried again after a while.
    Again(String),
    /// Until retried.
    Failed(String),
}

/// A file being made safe on its disk on a thread of its own, so the
/// download reads on meanwhile, and how much of it that covers.
struct Syncing {
    at: u64,
    thread: std::thread::JoinHandle<std::io::Result<()>>,
}

impl Syncing {
    fn start(file: &std::fs::File, at: u64) -> std::io::Result<Syncing> {
        let file = file.try_clone()?;
        let thread = std::thread::Builder::new()
            .name("fp-download-sync".into())
            .spawn(move || file.sync_data())?;
        Ok(Syncing { at, thread })
    }

    /// Waits for it; how far the file is safe.
    fn finish(self) -> std::io::Result<u64> {
        let at = self.at;
        self.thread
            .join()
            .unwrap_or_else(|_| Err(std::io::Error::other("the sync thread panicked")))
            .map(|()| at)
    }
}

/// What a source's error means for a download.
fn stop_for(e: &fp_sources::Error) -> Stop {
    use fp_sources::Error as E;
    let text = crate::ui::errors::source(e);
    match e {
        E::Io(_) | E::Http { .. } | E::Smb(_) | E::Sftp(_) | E::Nfs(_) | E::NotInserted => {
            Stop::Again(text)
        }
        E::HttpStatus { status, .. } if *status >= 500 || *status == 429 => Stop::Again(text),
        _ => Stop::Failed(text),
    }
}

/// What a disk's error means for a download: out of room waits for none,
/// a card taken out waits for it.
fn disk_stop(e: &std::io::Error, folder: &str) -> Stop {
    if path_of(folder).is_none() {
        return Stop::Paused;
    }
    if e.raw_os_error() == Some(libc::ENOSPC) {
        return Stop::Failed(tr!("download-no-space").to_string());
    }
    Stop::Failed(e.to_string())
}

struct Worker {
    shared: Arc<Shared>,
    library: Library,
    opener: Arc<Opener>,
    events: Sender<Event>,
    /// Downloads waiting out a lost connection, until when.
    retry_at: HashMap<MediaId, Instant>,
    tries: HashMap<MediaId, u32>,
}

impl Worker {
    fn run(mut self) {
        loop {
            if self.should_wait() {
                if !self.sleep(Duration::from_secs(60)) {
                    return;
                }
                continue;
            }
            match self.next() {
                Some((location, d)) => self.download(&location, d),
                None => {
                    if !self.sleep(Duration::from_secs(30)) {
                        return;
                    }
                }
            }
        }
    }

    /// Waits until woken or `limit` passes; false once quitting.
    fn sleep(&self, limit: Duration) -> bool {
        let mut st = lock(&self.shared.state);
        if !st.woken && !st.quit {
            st = self
                .shared
                .wake
                .wait_timeout(st, limit)
                .map(|(st, _)| st)
                .unwrap_or_else(|e| e.into_inner().0);
        }
        st.woken = false;
        !st.quit
    }

    fn should_wait(&self) -> bool {
        let st = lock(&self.shared.state);
        st.quit || st.streaming
    }

    /// The oldest download waiting whose disk is there and whose wait after
    /// a lost connection is over.
    fn next(&mut self) -> Option<(String, Download)> {
        let all = self.library.all_downloads().ok()?;
        let now = Instant::now();
        let waiting: HashSet<MediaId> = all
            .iter()
            .filter(|(_, d)| d.state == DownloadState::Waiting)
            .map(|(_, d)| d.media_id)
            .collect();
        self.retry_at.retain(|id, _| waiting.contains(id));
        self.tries.retain(|id, _| waiting.contains(id));
        all.into_iter().find(|(_, d)| {
            d.state == DownloadState::Waiting
                && path_of(&d.folder).is_some()
                && self.retry_at.get(&d.media_id).is_none_or(|at| *at <= now)
        })
    }

    fn download(&mut self, location: &str, d: Download) {
        let id = d.media_id;
        let title = self
            .library
            .get(id)
            .ok()
            .flatten()
            .map(|r| r.title)
            .unwrap_or_default();
        let began = Instant::now();
        let outcome = self.fetch(location, &d);
        {
            let mut st = lock(&self.shared.state);
            st.current = None;
            st.cancelled.remove(&id);
            match &outcome {
                Err(Stop::Again(error)) => {
                    st.retrying.insert(id, error.clone());
                }
                Err(Stop::Paused) => {}
                _ => {
                    st.retrying.remove(&id);
                }
            }
            st.generation += 1;
        }
        let gone = || self.library.download(id).ok().flatten().is_none();
        match outcome {
            Ok(size) => {
                self.retry_at.remove(&id);
                self.tries.remove(&id);
                if gone() {
                    delete_folder(&d.folder);
                    return;
                }
                let finished = self
                    .library
                    .finish_download(id, size)
                    .and_then(|()| self.library.download(id));
                match finished {
                    Ok(Some(d)) => {
                        self.opener
                            .set_local_copy(location, Some(d.video_location()));
                        log::info!(
                            "downloaded {title} ({size} bytes in {:.1} s)",
                            began.elapsed().as_secs_f64()
                        );
                        let _ = self.events.send(Event::Finished {
                            media_id: id,
                            title,
                        });
                    }
                    Ok(None) => {}
                    Err(e) => log::warn!("finishing the download of {title}: {e}"),
                }
            }
            Err(Stop::Paused) => {}
            Err(Stop::Cancelled) => delete_folder(&d.folder),
            Err(Stop::Again(error)) => {
                let tries = self.tries.entry(id).or_insert(0);
                *tries += 1;
                let wait = Duration::from_secs(15 << (*tries).min(5)).min(RETRY_MAX);
                log::warn!(
                    "downloading {title}: {error}; trying again in {} s",
                    wait.as_secs()
                );
                self.retry_at.insert(id, Instant::now() + wait);
            }
            Err(Stop::Failed(error)) => {
                log::warn!("downloading {title}: {error}");
                if gone() {
                    delete_folder(&d.folder);
                    return;
                }
                if let Err(e) = self.library.fail_download(id, &error) {
                    log::warn!("noting the failed download of {title}: {e}");
                }
                let _ = self.events.send(Event::Failed {
                    media_id: id,
                    title,
                    error,
                });
            }
        }
    }

    /// Stops a download part way when it should; checked between reads.
    fn interrupted(&self, id: MediaId) -> Option<Stop> {
        let st = lock(&self.shared.state);
        if st.cancelled.contains(&id) {
            Some(Stop::Cancelled)
        } else if st.quit || st.streaming {
            Some(Stop::Paused)
        } else {
            None
        }
    }

    fn set_progress(&self, progress: Progress) {
        lock(&self.shared.state).current = Some(progress);
    }

    /// Downloads `d`'s video, then its side files and picture; the video's
    /// size once it is all there.
    fn fetch(&self, location: &str, d: &Download) -> Result<u64, Stop> {
        let id = d.media_id;
        let record = self
            .library
            .get(id)
            .map_err(|e| Stop::Failed(e.to_string()))?
            .ok_or(Stop::Cancelled)?;
        let source = self
            .opener
            .source(&record.source_id)
            .ok_or_else(|| Stop::Failed(tr!("source-not-available").to_string()))?;
        let entry = Entry::new(record.title.clone(), location, EntryKind::Video);
        let entry = source.details(&entry).map_err(|e| stop_for(&e))?;
        let original = source.open_download(&entry).map_err(|e| stop_for(&e))?;
        let folder = path_of(&d.folder).ok_or(Stop::Paused)?;
        let mut file = d.file.clone();
        if file.is_empty() || d.received == 0 {
            let name = safe_name(&original.name);
            match self.library.set_download_file(id, &name) {
                Ok(true) => file = name,
                Ok(false) => {}
                Err(e) => return Err(Stop::Failed(e.to_string())),
            }
        }
        let size = original.reader.size().or(d.size);
        let video = folder.join(&file);
        let part = folder.join(format!("{file}.part"));
        let had = std::fs::metadata(&part).map_or(0, |m| m.len());
        // What was made safe last time; more may be in the file, but not
        // surely on the disk.
        let start = had.min(d.received);
        if let Some(size) = size {
            let room = existing_ancestor(&folder).and_then(free_space);
            let needed = size.saturating_sub(start) + SPARE;
            if room.is_some_and(|room| room < needed) {
                return Err(Stop::Failed(tr!("download-no-space").to_string()));
            }
        }
        std::fs::create_dir_all(&folder).map_err(|e| disk_stop(&e, &d.folder))?;
        let mut out = std::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(false)
            .open(&part)
            .map_err(|e| disk_stop(&e, &d.folder))?;
        out.set_len(start).map_err(|e| disk_stop(&e, &d.folder))?;
        out.seek(SeekFrom::Start(start))
            .map_err(|e| disk_stop(&e, &d.folder))?;
        let began = Instant::now();
        let mut offset = start;
        let mut saved_at = Instant::now();
        let mut syncing: Option<Syncing> = None;
        let mut buf = vec![0u8; CHUNK];
        self.set_progress(Progress {
            media_id: id,
            received: offset,
            size,
            speed: 0.0,
        });
        loop {
            if let Some(stop) = self.interrupted(id) {
                let _ = out.sync_all();
                let _ = self.library.set_download_progress(id, offset, size);
                log::info!(
                    "download stopped at {offset} bytes, after {} at {:.1} MB/s",
                    offset - start,
                    (offset - start) as f64 / began.elapsed().as_secs_f64().max(0.001) / 1e6
                );
                return Err(stop);
            }
            let want = size.map_or(CHUNK, |s| {
                (s.saturating_sub(offset)).min(CHUNK as u64) as usize
            });
            if want == 0 {
                break;
            }
            let n = match original.reader.read_at(offset, &mut buf[..want]) {
                Ok(n) => n,
                Err(e) => {
                    let _ = out.sync_all();
                    let _ = self.library.set_download_progress(id, offset, size);
                    return Err(Stop::Again(e.to_string()));
                }
            };
            if n == 0 {
                if size.is_some_and(|s| offset < s) {
                    let _ = out.sync_all();
                    let _ = self.library.set_download_progress(id, offset, size);
                    return Err(Stop::Again(tr!("download-ended-early").to_string()));
                }
                break;
            }
            out.write_all(&buf[..n])
                .map_err(|e| disk_stop(&e, &d.folder))?;
            offset += n as u64;
            let secs = began.elapsed().as_secs_f64().max(0.001);
            self.set_progress(Progress {
                media_id: id,
                received: offset,
                size,
                speed: (offset - start) as f64 / secs,
            });
            if saved_at.elapsed() >= SAVE_EVERY {
                saved_at = Instant::now();
                // What the last one made safe is noted; the next starts.
                if let Some(done) = syncing.take_if(|s| s.thread.is_finished()) {
                    let safe = done.finish().map_err(|e| disk_stop(&e, &d.folder))?;
                    if self.library.set_download_progress(id, safe, size).is_err() {
                        return Err(Stop::Cancelled);
                    }
                }
                if syncing.is_none() {
                    syncing =
                        Some(Syncing::start(&out, offset).map_err(|e| disk_stop(&e, &d.folder))?);
                }
            }
        }
        if let Some(s) = syncing.take() {
            s.finish().map_err(|e| disk_stop(&e, &d.folder))?;
        }
        out.sync_all().map_err(|e| disk_stop(&e, &d.folder))?;
        drop(out);
        std::fs::rename(&part, &video).map_err(|e| disk_stop(&e, &d.folder))?;
        self.fetch_side_files(&*source, &entry, &record, d, &folder, &file);
        Ok(offset)
    }

    /// The subtitle files and haptic scripts beside the video, and its
    /// picture. One that can't be had is left out: the video still plays.
    fn fetch_side_files(
        &self,
        source: &dyn fp_sources::Source,
        entry: &Entry,
        record: &fp_library::MediaRecord,
        d: &Download,
        folder: &Path,
        file: &str,
    ) {
        let source_id = Some(record.source_id.as_str());
        let side = match source.sidecars(entry) {
            Ok(side) => side,
            Err(e) => {
                log::warn!("side files of {}: {e}", record.title);
                fp_sources::Sidecars::from_entry(entry)
            }
        };
        let stem = file.rsplit_once('.').map_or(file, |(s, _)| s);
        let mut taken: HashSet<String> = HashSet::from([file.to_string(), PICTURE.to_string()]);
        let mut unique = |name: String| {
            let mut candidate = name.clone();
            let mut n = 1;
            while !taken.insert(candidate.clone()) {
                n += 1;
                candidate = match name.rsplit_once('.') {
                    Some((s, ext)) => format!("{s} ({n}).{ext}"),
                    None => format!("{name} ({n})"),
                };
            }
            candidate
        };
        let save = |name: &str, location: &str| -> bool {
            match self.opener.fetch_in(source_id, location, SIDE_LIMIT) {
                Ok(bytes) => std::fs::write(folder.join(name), bytes)
                    .map_err(|e| log::warn!("saving {name}: {e}"))
                    .is_ok(),
                Err(e) => {
                    log::warn!("downloading {name} of {}: {e}", record.title);
                    false
                }
            }
        };
        let mut subtitles: Vec<SubtitleFile> = Vec::new();
        for sub in &side.subtitles {
            let ext = [sub.location.as_str(), sub.name.as_str()]
                .into_iter()
                .filter(|n| fp_core::source::is_subtitle_name(n))
                .find_map(|n| n.rsplit_once('.').map(|(_, e)| e))
                .unwrap_or("srt")
                .to_ascii_lowercase();
            let lang = sub
                .language
                .as_deref()
                .map(|l| format!(".{}", safe_name(l)))
                .unwrap_or_default();
            let name = unique(safe_name(&format!("{stem}{lang}.{ext}")));
            if save(&name, &sub.location) {
                subtitles.push(SubtitleFile {
                    location: name,
                    ..sub.clone()
                });
            }
        }
        let mut scripts = Vec::new();
        for script in &side.scripts {
            let axis = script
                .axis
                .as_deref()
                .map(|a| format!(".{}", safe_name(a)))
                .unwrap_or_default();
            let name = unique(safe_name(&format!("{stem}{axis}.funscript")));
            if save(&name, &script.location) {
                scripts.push(name);
            }
        }
        if let Err(e) = self
            .library
            .set_download_side_files(d.media_id, &subtitles, &scripts)
        {
            log::warn!("noting the side files of {}: {e}", record.title);
        }
        if let Some(picture) = &record.thumbnail_url {
            save(PICTURE, picture);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fp_core::source::{ByteSource, MemorySource};
    use fp_library::MediaUpsert;
    use fp_sources::{OriginalFile, ScriptFile, Sidecars};
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

    /// A folder of its own for a test, deleted after.
    struct Scratch(PathBuf);

    impl Scratch {
        fn new() -> Scratch {
            let dir = std::env::temp_dir().join(format!("fp-downloads-{}", folder_key()));
            std::fs::create_dir_all(&dir).unwrap();
            Scratch(dir)
        }

        fn root(&self) -> String {
            self.0.to_string_lossy().into_owned()
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// A file read through, noting the lowest place read.
    struct Watched {
        inner: MemorySource,
        lowest: Arc<AtomicU64>,
    }

    impl ByteSource for Watched {
        fn size(&self) -> Option<u64> {
            self.inner.size()
        }
        fn read_at(&self, offset: u64, buf: &mut [u8]) -> std::io::Result<usize> {
            self.lowest.fetch_min(offset, Ordering::Relaxed);
            self.inner.read_at(offset, buf)
        }
        fn describe(&self) -> String {
            "watched".into()
        }
    }

    /// A server with one film, its subtitles and script.
    struct Server {
        film: Vec<u8>,
        lowest: Arc<AtomicU64>,
        refuse: AtomicBool,
    }

    const FILM: &str = "smb://nas/v/Film.mkv";

    impl fp_sources::Source for Server {
        fn id(&self) -> &str {
            "nas"
        }
        fn name(&self) -> &str {
            "NAS"
        }
        fn kind(&self) -> fp_sources::SourceKind {
            fp_sources::SourceKind::Smb
        }
        fn describe(&self) -> String {
            "nas".into()
        }
        fn list(&self, _: Option<&str>) -> fp_sources::Result<Vec<Entry>> {
            Ok(Vec::new())
        }
        fn open(&self, location: &str) -> fp_sources::Result<Arc<dyn ByteSource>> {
            let bytes = match location {
                "smb://nas/v/Film.en.srt" => b"1\n00:00:01,000 --> 00:00:02,000\nHi\n".to_vec(),
                "smb://nas/v/Film.funscript" => b"{\"actions\":[]}".to_vec(),
                _ => return Err(fp_sources::Error::NotFound(location.into())),
            };
            Ok(Arc::new(MemorySource(bytes)))
        }
        fn download_allowed(&self, _: &str) -> Option<bool> {
            Some(true)
        }
        fn open_download(&self, _: &Entry) -> fp_sources::Result<OriginalFile> {
            if self.refuse.load(Ordering::Relaxed) {
                return Err(fp_sources::Error::Auth("no downloads".into()));
            }
            Ok(OriginalFile {
                reader: Arc::new(Watched {
                    inner: MemorySource(self.film.clone()),
                    lowest: self.lowest.clone(),
                }),
                name: "Film: Cut.mkv".into(),
            })
        }
        fn sidecars(&self, _: &Entry) -> fp_sources::Result<Sidecars> {
            Ok(Sidecars {
                scripts: vec![ScriptFile {
                    name: "Film.funscript".into(),
                    location: "smb://nas/v/Film.funscript".into(),
                    axis: None,
                }],
                subtitles: vec![SubtitleFile {
                    name: "Film.en.srt".into(),
                    location: "smb://nas/v/Film.en.srt".into(),
                    language: Some("en".into()),
                    language_name: None,
                    flags: Default::default(),
                }],
            })
        }
    }

    /// A library with the film, and an opener with its server.
    fn setup() -> (Library, Arc<Opener>, Arc<Server>, MediaId) {
        let library = Library::open_in_memory().unwrap();
        let id = library.upsert(&MediaUpsert::new(FILM, "nas")).unwrap().id;
        let server = Arc::new(Server {
            film: (0..CHUNK * 2 + 123).map(|i| (i % 251) as u8).collect(),
            lowest: Arc::new(AtomicU64::new(u64::MAX)),
            refuse: AtomicBool::new(false),
        });
        let opener = Arc::new(Opener::new());
        opener.insert_source(server.clone());
        (library, opener, server, id)
    }

    fn wait_for(library: &Library, id: MediaId, state: DownloadState) -> Download {
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            if let Some(d) = library.download(id).unwrap()
                && d.state == state
            {
                return d;
            }
            assert!(
                Instant::now() < deadline,
                "the download never got {state:?}"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    fn next_event(downloads: &Downloads) -> Event {
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            if let Some(e) = downloads.take_events().pop() {
                return e;
            }
            assert!(Instant::now() < deadline, "nothing was said");
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    #[test]
    fn a_download_comes_whole_with_its_side_files_and_is_read_from_then_on() {
        let scratch = Scratch::new();
        let (library, opener, server, id) = setup();
        library
            .add_download(id, &new_folder(&scratch.root()), "", None)
            .unwrap();
        let downloads = Downloads::start(library.clone(), opener.clone(), scratch.root());
        assert!(matches!(next_event(&downloads), Event::Finished { .. }));
        let d = wait_for(&library, id, DownloadState::Done);
        assert_eq!(d.file, "Film_ Cut.mkv", "the server's name, made safe");
        let video = path_of(&d.video_location()).unwrap();
        assert_eq!(std::fs::read(&video).unwrap(), server.film);
        assert!(!video.with_extension("mkv.part").exists());
        assert_eq!(d.size, Some(server.film.len() as u64));
        let subtitles = d.subtitle_files();
        assert_eq!(subtitles[0].name, "Film.en.srt");
        assert_eq!(subtitles[0].language.as_deref(), Some("en"));
        assert!(subtitles[0].location.ends_with("/Film_ Cut.en.srt"));
        assert!(path_of(&subtitles[0].location).unwrap().is_file());
        assert_eq!(d.scripts, ["Film_ Cut.funscript"]);
        assert_eq!(opener.local_copy(FILM), Some(d.video_location()));
        let read = opener.open_in(Some("nas"), FILM).unwrap();
        assert_eq!(read.describe(), video.display().to_string());

        downloads.remove(id).unwrap();
        assert!(!video.exists());
        assert!(library.download(id).unwrap().is_none());
        assert!(library.get(id).unwrap().is_some(), "the video stays");
        assert_eq!(opener.local_copy(FILM), None);
    }

    #[test]
    fn a_download_part_way_carries_on_from_what_was_made_safe() {
        let scratch = Scratch::new();
        let (library, opener, server, id) = setup();
        let folder = new_folder(&scratch.root());
        library.add_download(id, &folder, "", None).unwrap();
        library.set_download_file(id, "Film.mkv").unwrap();
        let safe = CHUNK as u64;
        library
            .set_download_progress(id, safe, Some(server.film.len() as u64))
            .unwrap();
        // More came than was made safe; that part is fetched again.
        let dir = path_of(&folder).unwrap();
        std::fs::create_dir_all(&dir).unwrap();
        let mut part = server.film[..CHUNK].to_vec();
        part.extend_from_slice(&[0xEE; 4096]);
        std::fs::write(dir.join("Film.mkv.part"), part).unwrap();

        let downloads = Downloads::start(library.clone(), opener.clone(), scratch.root());
        assert!(matches!(next_event(&downloads), Event::Finished { .. }));
        let d = wait_for(&library, id, DownloadState::Done);
        assert_eq!(d.file, "Film.mkv", "a file part way keeps its name");
        assert_eq!(std::fs::read(dir.join("Film.mkv")).unwrap(), server.film);
        assert_eq!(server.lowest.load(Ordering::Relaxed), safe);
    }

    #[test]
    fn downloads_wait_while_a_video_streams() {
        let scratch = Scratch::new();
        let (library, opener, _server, id) = setup();
        let downloads = Downloads::start(library.clone(), opener.clone(), scratch.root());
        downloads.set_streaming(true);
        library
            .add_download(id, &new_folder(&scratch.root()), "", None)
            .unwrap();
        downloads.wake();
        std::thread::sleep(Duration::from_millis(300));
        let d = library.download(id).unwrap().unwrap();
        assert_eq!((d.state, d.received), (DownloadState::Waiting, 0));
        assert_eq!(downloads.progress(), None);
        downloads.set_streaming(false);
        wait_for(&library, id, DownloadState::Done);
    }

    #[test]
    fn a_refused_download_stops_until_retried() {
        let scratch = Scratch::new();
        let (library, opener, server, id) = setup();
        server.refuse.store(true, Ordering::Relaxed);
        library
            .add_download(id, &new_folder(&scratch.root()), "", None)
            .unwrap();
        let downloads = Downloads::start(library.clone(), opener.clone(), scratch.root());
        assert!(matches!(next_event(&downloads), Event::Failed { .. }));
        let d = wait_for(&library, id, DownloadState::Failed);
        assert!(d.error.is_some());

        server.refuse.store(false, Ordering::Relaxed);
        library.retry_download(id).unwrap();
        downloads.wake();
        wait_for(&library, id, DownloadState::Done);
    }

    #[test]
    fn folders_no_download_owns_are_cleared_away() {
        let scratch = Scratch::new();
        let (library, opener, _server, id) = setup();
        let owned = new_folder(&scratch.root());
        library.add_download(id, &owned, "", None).unwrap();
        let orphan = scratch.0.join("0123456789abcdef");
        std::fs::create_dir_all(&orphan).unwrap();
        std::fs::write(orphan.join("old.mkv"), b"old").unwrap();
        let downloads = Downloads::start(library.clone(), opener, scratch.root());
        wait_for(&library, id, DownloadState::Done);
        assert!(!orphan.exists());
        assert!(path_of(&owned).unwrap().is_dir());
        drop(downloads);
    }

    #[test]
    fn names_suit_any_card() {
        assert_eq!(safe_name("Film: The Cut?.mkv"), "Film_ The Cut_.mkv");
        assert_eq!(safe_name("a/b\\c.mp4"), "a_b_c.mp4");
        assert_eq!(safe_name("..hidden.mkv"), "hidden.mkv");
        assert_eq!(safe_name("  "), "video");
        assert_eq!(safe_name("ends with dot."), "ends with dot");
        let long = format!("{}.mkv", "é".repeat(150));
        assert!(safe_name(&long).len() <= 200);
    }

    #[test]
    fn a_new_folder_is_its_own() {
        let a = new_folder("/dl/");
        let b = new_folder("/dl");
        assert!(a.starts_with("/dl/") && b.starts_with("/dl/"));
        assert_ne!(a, b);
        assert_eq!(a.len(), "/dl/".len() + 16);
    }

    #[test]
    fn errors_say_whether_to_try_again() {
        let io = fp_sources::Error::Io(std::io::Error::other("reset"));
        assert!(matches!(stop_for(&io), Stop::Again(_)));
        let busy = fp_sources::Error::HttpStatus {
            status: 503,
            url: "u".into(),
        };
        assert!(matches!(stop_for(&busy), Stop::Again(_)));
        let refused = fp_sources::Error::Auth("no".into());
        assert!(matches!(stop_for(&refused), Stop::Failed(_)));
        let transcode_only = fp_sources::Error::Unsupported("only transcodes".into());
        assert!(matches!(stop_for(&transcode_only), Stop::Failed(_)));
    }

    #[test]
    fn progress_is_a_fraction_of_a_known_size() {
        let p = Progress {
            media_id: MediaId(1),
            received: 25,
            size: Some(100),
            speed: 0.0,
        };
        assert_eq!(p.fraction(), Some(0.25));
        assert_eq!(Progress { size: None, ..p }.fraction(), None);
    }
}
