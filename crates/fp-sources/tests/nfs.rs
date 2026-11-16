//! The NFS source against an in-memory NFSv3 server on a local port.
#![cfg(feature = "nfs")]

use fp_core::source::EntryKind;
use fp_sources::{Error, NfsConfig, SourceConfig, build};
use nfs3_server::memfs::{MemFs, MemFsConfig};
use nfs3_server::nfs3_types::nfs3::{fattr3, filename3, ftype3, nfspath3, nfsstat3};
use nfs3_server::tcp::{NFSTcp, NFSTcpListener};
use nfs3_server::vfs::{
    DirEntryPlus, FileHandleU64, NextResult, NfsReadFileSystem, ReadDirPlusIterator,
};
use std::sync::{Arc, RwLock};

/// Serves `fs` on a free local port, exporting it as `/export`, until the
/// test ends; returns the port.
fn serve(fs: MemFsConfig) -> u16 {
    serve_with_links(fs, &[]).0
}

/// Handles of symbolic links start here, well past the in-memory ones.
const LINK_BASE: u64 = 1 << 48;

/// The links of a [`LinkFs`]: the folder's handle, the link's name and its
/// target. Shared with the test, which can point a link elsewhere.
#[derive(Clone, Default)]
struct Links(Arc<RwLock<Vec<(u64, String, String)>>>);

impl Links {
    /// Points the link `name` at `target`, as `ln -sfn` would.
    fn repoint(&self, name: &str, target: &str) {
        let mut links = self.0.write().unwrap();
        let link = links.iter_mut().find(|(_, n, _)| n == name).unwrap();
        link.2 = target.to_string();
    }
}

/// An in-memory file system (which has no symbolic links of its own) with
/// links added: each in a folder, by name, pointing at a target.
struct LinkFs {
    inner: MemFs,
    links: Links,
}

impl LinkFs {
    fn link(&self, id: &FileHandleU64) -> Option<(u64, String, String)> {
        let id = id.as_u64().checked_sub(LINK_BASE)?;
        self.links
            .0
            .read()
            .unwrap()
            .get(usize::try_from(id).ok()?)
            .cloned()
    }

    fn link_attrs(id: u64, target: &str) -> fattr3 {
        fattr3 {
            type_: ftype3::NF3LNK,
            mode: 0o777,
            nlink: 1,
            uid: 0,
            gid: 0,
            size: target.len() as u64,
            used: target.len() as u64,
            rdev: Default::default(),
            fsid: 0,
            fileid: id,
            atime: Default::default(),
            mtime: Default::default(),
            ctime: Default::default(),
        }
    }
}

/// A folder's entries, then the links in it.
struct WithLinks<I> {
    inner: Option<I>,
    extra: std::vec::IntoIter<DirEntryPlus<FileHandleU64>>,
}

impl<I: ReadDirPlusIterator<FileHandleU64>> ReadDirPlusIterator<FileHandleU64> for WithLinks<I> {
    async fn next(&mut self) -> NextResult<DirEntryPlus<FileHandleU64>> {
        if let Some(inner) = &mut self.inner {
            match inner.next().await {
                NextResult::Eof => self.inner = None,
                other => return other,
            }
        }
        self.extra.next().map_or(NextResult::Eof, NextResult::Ok)
    }
}

impl NfsReadFileSystem for LinkFs {
    type Handle = FileHandleU64;

    fn root_dir(&self) -> FileHandleU64 {
        self.inner.root_dir()
    }

    async fn lookup(
        &self,
        dir: &FileHandleU64,
        name: &filename3<'_>,
    ) -> Result<FileHandleU64, nfsstat3> {
        let found = self
            .links
            .0
            .read()
            .unwrap()
            .iter()
            .position(|(d, n, _)| *d == dir.as_u64() && n.as_bytes() == name.as_ref());
        match found {
            Some(i) => Ok(FileHandleU64::new(LINK_BASE + i as u64)),
            None => self.inner.lookup(dir, name).await,
        }
    }

    async fn getattr(&self, id: &FileHandleU64) -> Result<fattr3, nfsstat3> {
        match self.link(id) {
            Some((_, _, target)) => Ok(Self::link_attrs(id.as_u64(), &target)),
            None => self.inner.getattr(id).await,
        }
    }

    async fn read(
        &self,
        id: &FileHandleU64,
        offset: u64,
        count: u32,
    ) -> Result<(Vec<u8>, bool), nfsstat3> {
        match self.link(id) {
            Some(_) => Err(nfsstat3::NFS3ERR_INVAL),
            None => self.inner.read(id, offset, count).await,
        }
    }

    async fn readdirplus(
        &self,
        dir: &FileHandleU64,
        cookie: u64,
    ) -> Result<impl ReadDirPlusIterator<FileHandleU64>, nfsstat3> {
        let (inner, after) = if cookie >= LINK_BASE {
            (None, Some(cookie - LINK_BASE))
        } else {
            (Some(self.inner.readdirplus(dir, cookie).await?), None)
        };
        let links = self.links.0.read().unwrap().clone();
        let extra: Vec<_> = (0u64..)
            .zip(&links)
            .filter(|(i, (d, _, _))| *d == dir.as_u64() && after.is_none_or(|a| *i > a))
            .map(|(i, (_, name, target))| DirEntryPlus {
                fileid: LINK_BASE + i,
                name: name.as_bytes().to_vec().into(),
                cookie: LINK_BASE + i,
                name_attributes: Some(Self::link_attrs(LINK_BASE + i, target)),
                name_handle: Some(FileHandleU64::new(LINK_BASE + i)),
            })
            .collect();
        Ok(WithLinks {
            inner,
            extra: extra.into_iter(),
        })
    }

    async fn readlink(&self, id: &FileHandleU64) -> Result<nfspath3<'_>, nfsstat3> {
        match self.link(id) {
            Some((_, _, target)) => Ok(nfspath3::from(target.as_bytes().to_vec())),
            None => Err(nfsstat3::NFS3ERR_INVAL),
        }
    }
}

/// Like [`serve`], with symbolic links: (folder, name, target). Returns the
/// port and the links, to point elsewhere while the server runs.
fn serve_with_links(fs: MemFsConfig, links: &[(&str, &str, &str)]) -> (u16, Links) {
    serve_links_as(fs, links, "export")
}

/// Like [`serve_with_links`], exporting the file system as `/{name}`.
fn serve_links_as(fs: MemFsConfig, links: &[(&str, &str, &str)], name: &str) -> (u16, Links) {
    let name = name.to_string();
    let links: Vec<(String, String, String)> = links
        .iter()
        .map(|(d, n, t)| (d.to_string(), n.to_string(), t.to_string()))
        .collect();
    let shared = Links::default();
    let served = shared.clone();
    let (port_tx, port_rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async move {
            let inner = MemFs::new(fs).unwrap();
            for (dir, name, target) in links {
                let dir = inner.lookup_by_path(&dir).await.unwrap().as_u64();
                served.0.write().unwrap().push((dir, name, target));
            }
            let fs = LinkFs {
                inner,
                links: served,
            };
            let mut listener = NFSTcpListener::bind_ro("127.0.0.1:0", fs).await.unwrap();
            listener.with_export_name(&name);
            port_tx.send(listener.get_listen_port()).unwrap();
            listener.handle_forever().await.unwrap();
        });
    });
    (port_rx.recv().unwrap(), shared)
}

/// Serves `fs` as `/{name}` on `port` (0 for any free one) until the
/// returned sender is dropped; returns the port and the server's thread,
/// which ends once its connections are closed.
fn serve_stoppable(
    fs: MemFsConfig,
    name: &str,
    port: u16,
) -> (
    u16,
    std::sync::mpsc::Sender<()>,
    std::thread::JoinHandle<()>,
) {
    let name = name.to_string();
    let (port_tx, port_rx) = std::sync::mpsc::channel();
    let (stop_tx, stop_rx) = std::sync::mpsc::channel::<()>();
    let thread = std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async move {
            let fs = LinkFs {
                inner: MemFs::new(fs).unwrap(),
                links: Links::default(),
            };
            let mut listener = NFSTcpListener::bind_ro(&format!("127.0.0.1:{port}"), fs)
                .await
                .unwrap();
            listener.with_export_name(&name);
            port_tx.send(listener.get_listen_port()).unwrap();
            let server = tokio::spawn(async move {
                let _ = listener.handle_forever().await;
            });
            let _ = tokio::task::spawn_blocking(move || stop_rx.recv()).await;
            server.abort();
        });
        // Dropping the runtime closes the connections still open.
    });
    (port_rx.recv().unwrap(), stop_tx, thread)
}

fn config(port: u16, export: &str) -> SourceConfig {
    SourceConfig::Nfs(NfsConfig {
        id: "nfs".into(),
        name: "NAS".into(),
        host: "127.0.0.1".into(),
        port: Some(port),
        export: export.into(),
        uid: None,
        gid: None,
    })
}

/// Bytes that differ at every offset, to catch reads from the wrong place.
fn pattern(len: usize) -> Vec<u8> {
    (0..len).map(|i| (i * 7 + i / 251) as u8).collect()
}

#[test]
fn browse_an_export_and_read_a_video_across_blocks() {
    let video = pattern(3 * 1024 * 1024 + 12345);
    let mut fs = MemFsConfig::default();
    fs.add_dir("/vr");
    fs.add_file("/vr/Clip #1_180_LR.mp4", video.clone());
    fs.add_file("/vr/Clip #1_180_LR.funscript", b"{}".to_vec());
    fs.add_file("/vr/.hidden.mp4", b"x".to_vec());
    fs.add_dir("/vr/Old Stuff");
    fs.add_file("/notes.txt", b"hello".to_vec());
    let port = serve(fs);

    let source = build(&config(port, "/export")).unwrap();
    let root = source.list(None).unwrap();
    let names: Vec<_> = root.iter().map(|e| e.name.as_str()).collect();
    assert_eq!(names, ["vr", "notes.txt"]);
    assert_eq!(root[0].kind, EntryKind::Directory);

    let vr = source.list(Some(&root[0].location)).unwrap();
    let names: Vec<_> = vr.iter().map(|e| e.name.as_str()).collect();
    assert_eq!(
        names,
        [
            "Old Stuff",
            "Clip #1_180_LR.funscript",
            "Clip #1_180_LR.mp4"
        ]
    );
    let clip = vr.iter().find(|e| e.kind == EntryKind::Video).unwrap();
    assert_eq!(clip.size, Some(video.len() as u64));
    assert!(clip.location.starts_with("source://"), "{}", clip.location);
    assert_eq!(
        source.parent(&clip.location),
        Some(root[0].location.clone())
    );

    let sidecars = source.sidecars(clip).unwrap();
    assert_eq!(sidecars.scripts.len(), 1, "{sidecars:?}");

    let file = source.open(&clip.location).unwrap();
    assert_eq!(file.size(), Some(video.len() as u64));
    let mut buf = vec![0; 300_000];
    // Across a block boundary, near the end, and past it.
    for offset in [0, 1024 * 1024 - 1000, video.len() as u64 - 100] {
        let n = file.read_at(offset, &mut buf).unwrap();
        let start = offset as usize;
        let end = (start + buf.len()).min(video.len());
        assert_eq!(n, end - start, "at {offset}");
        assert_eq!(&buf[..n], &video[start..end], "at {offset}");
    }
    assert_eq!(file.read_at(video.len() as u64 + 10, &mut buf).unwrap(), 0);

    let missing = format!("nfs://127.0.0.1:{port}/export/vr/missing.mp4");
    let err = source.open(&missing).err().unwrap();
    assert!(matches!(err, Error::NotFound(_)), "{err:?}");
}

#[test]
fn a_folder_inside_an_export_can_be_the_source() {
    let mut fs = MemFsConfig::default();
    fs.add_dir("/vr");
    fs.add_file("/vr/a.mp4", b"abc".to_vec());
    let port = serve(fs);
    let source = build(&config(port, "/export/vr")).unwrap();
    let list = source.list(None).unwrap();
    assert_eq!(list.len(), 1);
    assert_eq!(list[0].name, "a.mp4");
    let mut buf = [0; 8];
    let n = source
        .open(&list[0].location)
        .unwrap()
        .read_at(0, &mut buf)
        .unwrap();
    assert_eq!(&buf[..n], b"abc");
}

#[test]
fn a_whole_server_lists_its_exports() {
    let mut fs = MemFsConfig::default();
    fs.add_file("/a.mp4", b"abc".to_vec());
    let port = serve(fs);
    let source = build(&config(port, "")).unwrap();
    let exports = source.list(None).unwrap();
    assert_eq!(exports.len(), 1);
    assert_eq!(exports[0].name, "/export");
    let files = source.list(Some(&exports[0].location)).unwrap();
    assert_eq!(files[0].name, "a.mp4");
    assert_eq!(
        source.parent(&exports[0].location),
        source
            .parent(&files[0].location)
            .and_then(|p| source.parent(&p))
    );
    assert_eq!(
        source.parent(&files[0].location),
        Some(exports[0].location.clone())
    );
}

#[test]
fn a_missing_export_is_not_found() {
    let port = serve(MemFsConfig::default());
    let source = build(&config(port, "/elsewhere")).unwrap();
    let err = source.list(None).unwrap_err();
    assert!(matches!(err, Error::NotFound(_)), "{err:?}");
}

#[test]
fn symbolic_links_are_listed_and_open_as_what_they_point_to() {
    let video = pattern(1024 * 1024 + 777);
    let movie = pattern(4321);
    let mut fs = MemFsConfig::default();
    fs.add_dir("/vr");
    fs.add_file("/vr/real.mp4", video.clone());
    fs.add_dir("/movies");
    fs.add_file("/movies/m.mp4", movie.clone());
    let (port, _) = serve_with_links(
        fs,
        &[
            ("/vr", "relative.mp4", "real.mp4"),
            ("/vr", "absolute.mp4", "/export/vr/./real.mp4"),
            ("/vr", "chain.mp4", "relative.mp4"),
            ("/vr", "Movies", "../movies"),
            ("/vr", "broken.mp4", "missing.mp4"),
            ("/vr", "loop.mp4", "loop.mp4"),
            ("/vr", "escape", "../../etc"),
            ("/vr", "elsewhere.mp4", "/srv/other.mp4"),
        ],
    );
    let source = build(&config(port, "/export")).unwrap();
    let vr = format!("nfs://127.0.0.1:{port}/export/vr/");
    let list = source.list(Some(&vr)).unwrap();
    let names: Vec<_> = list.iter().map(|e| e.name.as_str()).collect();
    assert_eq!(
        names,
        [
            "Movies",
            "absolute.mp4",
            "chain.mp4",
            "real.mp4",
            "relative.mp4"
        ]
    );
    for entry in list.iter().filter(|e| e.name.ends_with(".mp4")) {
        assert_eq!(entry.kind, EntryKind::Video, "{}", entry.name);
        assert_eq!(entry.size, Some(video.len() as u64), "{}", entry.name);
        let file = source.open(&entry.location).unwrap();
        let mut buf = vec![0; 2000];
        let at = 1024 * 1024 - 500;
        let n = file.read_at(at, &mut buf).unwrap();
        assert_eq!(
            &buf[..n],
            &video[at as usize..at as usize + n],
            "{}",
            entry.name
        );
    }

    // A folder reached through a link browses and opens like any other.
    let movies = list.iter().find(|e| e.name == "Movies").unwrap();
    assert_eq!(movies.kind, EntryKind::Directory);
    let inside = source.list(Some(&movies.location)).unwrap();
    assert_eq!(inside.len(), 1);
    assert_eq!(inside[0].name, "m.mp4");
    assert_eq!(
        source.parent(&inside[0].location),
        Some(movies.location.clone())
    );
    let mut buf = vec![0; movie.len() + 10];
    let n = source
        .open(&inside[0].location)
        .unwrap()
        .read_at(0, &mut buf)
        .unwrap();
    assert_eq!(&buf[..n], &movie[..]);

    // Links that lead nowhere say why when opened directly.
    for name in ["broken.mp4", "loop.mp4", "escape", "elsewhere.mp4"] {
        let err = source.open(&format!("{vr}{name}")).err().unwrap();
        assert!(
            matches!(err, Error::NotFound(_) | Error::InvalidLocation { .. }),
            "{name}: {err:?}"
        );
    }
}

#[test]
fn links_in_a_folder_source_reach_the_rest_of_its_export() {
    let movie = pattern(4321);
    let mut fs = MemFsConfig::default();
    fs.add_dir("/videos");
    fs.add_file("/videos/own.mp4", b"own".to_vec());
    fs.add_dir("/other-folder");
    fs.add_file("/other-folder/other-video.mp4", movie.clone());
    let (port, _) = serve_with_links(
        fs,
        &[
            ("/videos", "relative.mp4", "../other-folder/other-video.mp4"),
            (
                "/videos",
                "absolute.mp4",
                "/export/other-folder/other-video.mp4",
            ),
            ("/videos", "Other", ".."),
            ("/videos", "escape.mp4", "../../etc/passwd"),
        ],
    );
    // The server exports /export; the source is the folder /export/videos.
    let source = build(&config(port, "/export/videos")).unwrap();
    let list = source.list(None).unwrap();
    let names: Vec<_> = list.iter().map(|e| e.name.as_str()).collect();
    assert_eq!(names, ["Other", "absolute.mp4", "own.mp4", "relative.mp4"]);
    for name in ["relative.mp4", "absolute.mp4"] {
        let entry = list.iter().find(|e| e.name == name).unwrap();
        assert_eq!(entry.size, Some(movie.len() as u64), "{name}");
        let mut buf = vec![0; movie.len() + 10];
        let n = source
            .open(&entry.location)
            .unwrap()
            .read_at(0, &mut buf)
            .unwrap();
        assert_eq!(&buf[..n], &movie[..], "{name}");
    }
    // Browsing stays in the source's folder: a link up to the export's
    // root shows its folders under the link, and the source's root has no
    // parent.
    let other = list.iter().find(|e| e.name == "Other").unwrap();
    let names: Vec<_> = source
        .list(Some(&other.location))
        .unwrap()
        .into_iter()
        .map(|e| e.name)
        .collect();
    assert_eq!(names, ["other-folder", "videos"]);
    assert!(source.parent(&other.location).is_some());
    assert!(
        source
            .parent(
                &source
                    .canonical_location(&format!("nfs://127.0.0.1:{port}/export/videos/"))
                    .unwrap()
            )
            .is_none()
    );
    // Nothing outside the export, and nothing outside the folder by URL.
    let outside = format!("nfs://127.0.0.1:{port}/export/other-folder/other-video.mp4");
    assert!(matches!(
        source.open(&outside).err().unwrap(),
        Error::InvalidLocation { .. }
    ));
}

#[test]
fn a_link_pointed_elsewhere_is_followed_to_its_new_target() {
    let first = pattern(4321);
    let second = pattern(9876);
    let mut fs = MemFsConfig::default();
    fs.add_dir("/videos");
    fs.add_dir("/other");
    fs.add_file("/other/first.mp4", first.clone());
    fs.add_file("/other/second.mp4", second.clone());
    fs.add_dir("/season1");
    fs.add_file("/season1/episode.mp4", first.clone());
    fs.add_dir("/season2");
    fs.add_file("/season2/episode.mp4", second.clone());
    let (port, links) = serve_with_links(
        fs,
        &[
            ("/videos", "latest.mp4", "../other/first.mp4"),
            ("/videos", "Current", "../season1"),
        ],
    );
    let source = build(&config(port, "/export/videos")).unwrap();
    let read = |location: &str| {
        let file = source.open(location).unwrap();
        let mut buf = vec![0; 10_000];
        let n = file.read_at(0, &mut buf).unwrap();
        buf.truncate(n);
        buf
    };
    let size_of = |list: &[fp_core::source::Entry], name: &str| {
        list.iter().find(|e| e.name == name).unwrap().size
    };

    let list = source.list(None).unwrap();
    let latest = list.iter().find(|e| e.name == "latest.mp4").unwrap();
    let latest = latest.location.clone();
    let current = list.iter().find(|e| e.name == "Current").unwrap();
    let current = current.location.clone();
    assert_eq!(size_of(&list, "latest.mp4"), Some(first.len() as u64));
    assert_eq!(read(&latest), first);
    let inside = source.list(Some(&current)).unwrap();
    let episode = inside[0].location.clone();
    assert_eq!(size_of(&inside, "episode.mp4"), Some(first.len() as u64));
    assert_eq!(read(&episode), first);

    links.repoint("latest.mp4", "../other/second.mp4");
    links.repoint("Current", "/export/season2");

    // Opened again without a refresh, the links lead to their new targets.
    assert_eq!(read(&latest), second);
    assert_eq!(read(&episode), second);
    // And a refresh lists what they point to now.
    let list = source.list(None).unwrap();
    assert_eq!(size_of(&list, "latest.mp4"), Some(second.len() as u64));
    let inside = source.list(Some(&current)).unwrap();
    assert_eq!(size_of(&inside, "episode.mp4"), Some(second.len() as u64));
}

#[test]
fn dot_dot_in_a_link_target_goes_up_from_where_earlier_links_lead() {
    let right = pattern(4321);
    let wrong = pattern(1234);
    let mut fs = MemFsConfig::default();
    fs.add_dir("/a");
    fs.add_dir("/a/b");
    fs.add_file("/a/clip.mp4", right.clone());
    fs.add_file("/clip.mp4", wrong.clone());
    let (port, _) = serve_with_links(
        fs,
        &[
            // `dir/..` is `a`, not the export's root.
            ("/", "dir", "a/b"),
            ("/", "absolute.mp4", "/export/dir/../clip.mp4"),
            ("/", "relative.mp4", "dir/../clip.mp4"),
            // Above the export and back in again.
            ("/", "round.mp4", "/export/../export/dir/../clip.mp4"),
            ("/", "up-and-in.mp4", "../export/a/clip.mp4"),
            // Ending above the export, or in another folder there.
            ("/", "above.mp4", "/export/.."),
            ("/", "sibling.mp4", "/export/../other/clip.mp4"),
            ("/", "dir-above.mp4", "/export/dir/../../../clip.mp4"),
        ],
    );
    let source = build(&config(port, "/export")).unwrap();
    let root = format!("nfs://127.0.0.1:{port}/export/");
    for name in ["absolute.mp4", "relative.mp4", "round.mp4", "up-and-in.mp4"] {
        let mut buf = vec![0; 10_000];
        let n = source
            .open(&format!("{root}{name}"))
            .unwrap_or_else(|e| panic!("{name}: {e:?}"))
            .read_at(0, &mut buf)
            .unwrap();
        assert_eq!(&buf[..n], &right[..], "{name}");
    }
    for name in ["above.mp4", "sibling.mp4", "dir-above.mp4"] {
        let err = source.open(&format!("{root}{name}")).err().unwrap();
        assert!(
            matches!(err, Error::InvalidLocation { .. }),
            "{name}: {err:?}"
        );
    }
}

#[test]
fn a_servers_root_export_is_not_its_list_of_exports() {
    let video = pattern(4321);
    let mut fs = MemFsConfig::default();
    fs.add_dir("/videos");
    fs.add_file("/videos/clip.mp4", video.clone());
    fs.add_file("/top.mp4", video.clone());
    // The server exports `/`; the source is the whole server.
    let (port, _) = serve_links_as(fs, &[], "");
    let source = build(&config(port, "")).unwrap();
    let exports = source.list(None).unwrap();
    assert_eq!(exports.len(), 1);
    assert_eq!(exports[0].name, "/");
    assert_eq!(
        source.display_location(None).as_deref(),
        Some(format!("nfs://127.0.0.1:{port}").as_str())
    );
    assert_eq!(exports[0].kind, EntryKind::Directory);

    // Opening the export `/` shows what is in it, not the exports again.
    let root = source.list(Some(&exports[0].location)).unwrap();
    let names: Vec<_> = root.iter().map(|e| e.name.as_str()).collect();
    assert_eq!(names, ["videos", "top.mp4"]);
    assert_eq!(
        source
            .display_location(Some(&exports[0].location))
            .as_deref(),
        Some(format!("nfs://127.0.0.1:{port}/").as_str())
    );
    let videos = &root[0];
    let inside = source.list(Some(&videos.location)).unwrap();
    assert_eq!(inside.len(), 1);
    let mut buf = vec![0; 10_000];
    let n = source
        .open(&inside[0].location)
        .unwrap()
        .read_at(0, &mut buf)
        .unwrap();
    assert_eq!(&buf[..n], &video[..]);

    // Up from a file at the top is the export `/`; up from that, the list.
    let top = root.iter().find(|e| e.name == "top.mp4").unwrap();
    assert_eq!(
        source.parent(&top.location).as_deref(),
        Some(exports[0].location.as_str())
    );
    assert_eq!(
        source.parent(&videos.location).as_deref(),
        Some(exports[0].location.as_str())
    );
    let list = source.parent(&exports[0].location).unwrap();
    assert_ne!(list, exports[0].location);
    assert_eq!(source.list(Some(&list)).unwrap().len(), 1);
    assert!(source.open(&exports[0].location).is_err());
    assert!(source.open(&list).is_err());
}

#[test]
fn the_export_root_can_be_a_source_of_its_own() {
    let video = pattern(4321);
    let mut fs = MemFsConfig::default();
    fs.add_dir("/videos");
    fs.add_file("/videos/clip.mp4", video.clone());
    let (port, _) = serve_links_as(fs, &[], "");
    // `nfs://host/`: the export `/` itself, not the server's exports.
    let source = build(&config(port, "/")).unwrap();
    assert_eq!(
        source.display_location(None).as_deref(),
        Some(format!("nfs://127.0.0.1:{port}/").as_str())
    );
    let root = source.list(None).unwrap();
    assert_eq!(root.len(), 1);
    assert_eq!(root[0].name, "videos");
    assert_eq!(
        source.display_location(Some(&root[0].location)).as_deref(),
        Some(format!("nfs://127.0.0.1:{port}/videos").as_str())
    );
    let inside = source.list(Some(&root[0].location)).unwrap();
    let mut buf = vec![0; 10_000];
    let n = source
        .open(&inside[0].location)
        .unwrap()
        .read_at(0, &mut buf)
        .unwrap();
    assert_eq!(&buf[..n], &video[..]);

    // Whether a server has the export `/`, to tell when adding a source.
    let check = |port, export: &str| match config(port, export) {
        SourceConfig::Nfs(c) => fp_sources::nfs::exports_root(&c).unwrap(),
        _ => unreachable!(),
    };
    assert!(check(port, "/"));
    let other = serve(MemFsConfig::default());
    assert!(!check(other, "/"), "exports only /export");
}

#[test]
fn refreshing_a_whole_servers_list_asks_it_again() {
    let (port, stop, thread) = serve_stoppable(MemFsConfig::default(), "first", 0);
    let source = build(&config(port, "")).unwrap();
    let names = || -> Vec<String> {
        let list = source.list(None).unwrap();
        list.into_iter().map(|e| e.name).collect()
    };
    assert_eq!(names(), ["/first"]);
    // The server comes back with other exports.
    drop(stop);
    thread.join().unwrap();
    let (_, _stop, _thread) = serve_stoppable(MemFsConfig::default(), "second", port);
    assert_eq!(names(), ["/second"], "listed again, not from before");
}

#[test]
fn a_connection_the_server_closed_is_replaced_at_once() {
    let files = || {
        let mut fs = MemFsConfig::default();
        fs.add_dir("/videos");
        fs.add_file("/videos/clip.mp4", pattern(4321));
        fs
    };
    let (port, stop, thread) = serve_stoppable(files(), "srv", 0);
    let source = build(&config(port, "")).unwrap();
    let exports = source.list(None).unwrap();
    let export = exports[0].location.clone();
    assert_eq!(source.list(Some(&export)).unwrap().len(), 1);
    // The server closes every connection, as one does with idle ones, and
    // is there again.
    drop(stop);
    thread.join().unwrap();
    let (_, _stop, _thread) = serve_stoppable(files(), "srv", port);
    let started = std::time::Instant::now();
    let listed = source.list(Some(&export));
    assert_eq!(listed.map(|l| l.len()).unwrap(), 1);
    assert!(
        started.elapsed() < std::time::Duration::from_secs(5),
        "took {:?}",
        started.elapsed()
    );
}
