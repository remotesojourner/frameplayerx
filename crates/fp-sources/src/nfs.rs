//! NFSv3 exports (Linux servers, NAS boxes).
//!
//! # Implementation choice
//!
//! This uses [`nfs3_client`](https://crates.io/crates/nfs3_client), a
//! pure-Rust NFSv3 client (portmapper, MOUNT and NFS over TCP). It is async,
//! so the blocking calls here run it on a small tokio runtime of its own,
//! like SMB. Build with `--no-default-features` to leave NFS out;
//! [`NfsSource::new`] then fails with [`Error::NfsUnavailable`].
//!
//! A source is one export, or a folder inside one, mounted directly
//! (`nfs://host/volume1/videos`), or a whole server (`nfs://host/`), whose
//! root lists its exports. Requests carry AUTH_SYS credentials: the user and
//! group IDs from the configuration, or this process's. Linux servers only
//! accept requests from a privileged source port unless the export has the
//! `insecure` option; one is used when the system allows it.
//!
//! A source on a folder inside an export mounts the export and browses the
//! folder in it, so symbolic links there can lead anywhere in the export.
//! Symbolic links on the server are followed, as long as they lead to
//! somewhere in the same export: a link is listed, and opens, as the file or
//! folder it points to. Broken links, loops and links out of the export are
//! left out of listings.
//!
//! Listed locations retain the source ID and the path relative to the
//! source's root; the host comes from the current configuration. Files are
//! read through the shared block cache with read-ahead.

use crate::config::{NfsConfig, SourceKind};
use crate::error::{Error, Result};
use crate::urlutil::{encode_segment, percent_decode};
use crate::{Source, dir_entry, file_entry, sort_entries};
use fp_core::source::{ByteSource, Entry};
use std::sync::Arc;

/// Builds `nfs://host[:port]/path` with every segment percent-encoded.
/// `path` is `/`-separated and may be empty (the whole server).
pub fn nfs_url(host: &str, port: Option<u16>, path: &str) -> String {
    let host = if host.contains(':') && !host.starts_with('[') {
        format!("[{host}]") // IPv6 literal
    } else {
        host.to_string()
    };
    let mut out = format!("nfs://{host}");
    if let Some(p) = port {
        out.push_str(&format!(":{p}"));
    }
    out.push('/');
    let segments: Vec<String> = segments(path).map(encode_segment).collect();
    out.push_str(&segments.join("/"));
    out
}

/// The non-empty segments of a `/`-separated path.
fn segments(path: &str) -> impl Iterator<Item = &str> {
    path.split('/').filter(|s| !s.is_empty())
}

/// A server path as its segments, `/volume1/videos` as `["volume1", "videos"]`.
fn path_segments(path: &str) -> Vec<String> {
    segments(path).map(str::to_string).collect()
}

/// A location split into host and decoded path segments. Which leading
/// segments are the export only the server knows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NfsLocation {
    /// Host name or address (IPv6 without brackets).
    pub host: String,
    /// TCP port of the NFS service, when given.
    pub port: Option<u16>,
    /// Decoded path segments on the server; empty for the server itself.
    pub path: Vec<String>,
}

impl NfsLocation {
    /// Parses `nfs://host[:port]/path`. Credentials and query are ignored.
    pub fn parse(location: &str) -> Result<NfsLocation> {
        let rest = location
            .get(..6)
            .filter(|p| p.eq_ignore_ascii_case("nfs://"))
            .map(|_| &location[6..])
            .ok_or_else(|| Error::invalid(location, "expected an nfs:// URL"))?;
        let rest = rest.split(['?', '#']).next().unwrap_or("");
        let (authority, path) = rest.split_once('/').unwrap_or((rest, ""));
        let authority = authority.rsplit_once('@').map_or(authority, |(_, h)| h);
        let (host, port) = if let Some(v6) = authority.strip_prefix('[') {
            let (h, after) = v6
                .split_once(']')
                .ok_or_else(|| Error::invalid(location, "bad IPv6 host"))?;
            (h.to_string(), after.strip_prefix(':'))
        } else {
            match authority.rsplit_once(':') {
                Some((h, p)) => (h.to_string(), Some(p)),
                None => (authority.to_string(), None),
            }
        };
        let port = match port {
            Some(p) => Some(
                p.parse::<u16>()
                    .map_err(|_| Error::invalid(location, "bad port"))?,
            ),
            None => None,
        };
        if host.is_empty() {
            return Err(Error::invalid(location, "no host"));
        }
        let path: Vec<String> = segments(path).map(percent_decode).collect();
        if path
            .iter()
            .any(|s| matches!(s.as_str(), "." | "..") || s.contains(['/', '\0']))
        {
            return Err(Error::invalid(location, "path escapes the export"));
        }
        Ok(NfsLocation { host, port, path })
    }

    /// Back to an `nfs://` URL.
    pub fn to_url(&self) -> String {
        nfs_url(&self.host, self.port, &self.path.join("/"))
    }
}

/// An NFSv3 export, or every export of a server.
pub struct NfsSource {
    id: String,
    name: String,
    config: NfsConfig,
    root: String,
    /// The configured export's segments; empty for the whole server.
    export: Vec<String>,
    #[cfg(feature = "nfs")]
    client: Arc<imp::Client>,
}

impl std::fmt::Debug for NfsSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NfsSource")
            .field("id", &self.id)
            .field("root", &self.root)
            .field("uid", &self.config.uid)
            .field("gid", &self.config.gid)
            .finish_non_exhaustive()
    }
}

impl NfsSource {
    /// Creates the source. No connection is made until it is used.
    pub fn new(config: &NfsConfig) -> Result<NfsSource> {
        if config.host.trim().is_empty() {
            return Err(Error::Config("NFS source needs a host".into()));
        }
        if !cfg!(feature = "nfs") {
            return Err(Error::NfsUnavailable(
                "this build of FramePlayerX was compiled without NFS support".into(),
            ));
        }
        let export = path_segments(&config.export);
        if export.iter().any(|s| matches!(s.as_str(), "." | "..")) {
            return Err(Error::Config("NFS export path escapes the export".into()));
        }
        let root = nfs_url(&config.host, config.port, &export.join("/"));
        Ok(NfsSource {
            id: config.id.clone(),
            name: config.name.clone(),
            #[cfg(feature = "nfs")]
            client: Arc::new(imp::Client::new(config, &root)),
            root,
            export,
            config: config.clone(),
        })
    }

    /// URL listed by `list(None)`.
    pub fn root(&self) -> &str {
        &self.root
    }

    /// A source for the whole server (`nfs://host/`).
    fn whole_server(&self) -> bool {
        self.export.is_empty()
    }

    /// Checks that `loc` is on this source's server and under its root.
    fn check_owned(&self, loc: &NfsLocation) -> Result<()> {
        let same = loc.host.eq_ignore_ascii_case(&self.config.host)
            && loc.port == self.config.port
            && loc.path.starts_with(&self.export);
        if same {
            Ok(())
        } else {
            Err(Error::invalid(&loc.to_url(), "not on this source's export"))
        }
    }

    /// The `nfs://` URL a location stands for: a source-owned location
    /// resolved against the current configuration, anything else unchanged.
    fn current_url(&self, value: &str) -> Result<String> {
        if !crate::is_source_location(value) {
            return Ok(value.to_string());
        }
        let value = crate::location::decode(value)
            .filter(|l| l.source_id == self.id && l.kind == "nfs")
            .ok_or_else(|| Error::invalid(value, "not owned by this NFS source"))?;
        if value.key.starts_with('/') || value.key.contains(['?', '#', '\\']) {
            return Err(Error::invalid(
                "NFS source resource",
                "invalid relative path",
            ));
        }
        let relative: Vec<String> = segments(&value.key).map(percent_decode).collect();
        if relative
            .iter()
            .any(|s| matches!(s.as_str(), "." | "..") || s.contains(['/', '\\', '\0']))
        {
            return Err(Error::invalid(
                "NFS source resource",
                "invalid path segment",
            ));
        }
        let mut current = NfsLocation::parse(&self.root)?;
        current.path.extend(relative);
        let mut url = current.to_url();
        if value.key.ends_with('/') && !url.ends_with('/') {
            url.push('/');
        }
        Ok(url)
    }
}

impl Source for NfsSource {
    fn id(&self) -> &str {
        &self.id
    }

    fn name(&self) -> &str {
        &self.name
    }

    fn kind(&self) -> SourceKind {
        SourceKind::Nfs
    }

    fn describe(&self) -> String {
        let what = if self.whole_server() {
            "NFS server"
        } else {
            "NFS export"
        };
        match (self.config.uid, self.config.gid) {
            (None, None) => format!("{what} {}", self.root),
            (uid, gid) => format!(
                "{what} {} as {}:{}",
                self.root,
                uid.map_or("-".into(), |u| u.to_string()),
                gid.map_or("-".into(), |g| g.to_string())
            ),
        }
    }

    fn canonical_location(&self, value: &str) -> Option<String> {
        let url = self.current_url(value).ok()?;
        let loc = NfsLocation::parse(&url).ok()?;
        self.check_owned(&loc).ok()?;
        let rest = loc.path.strip_prefix(self.export.as_slice())?;
        if rest
            .iter()
            .any(|s| s.is_empty() || matches!(s.as_str(), "." | "..") || s.contains(['/', '\0']))
        {
            return None;
        }
        let mut key = rest
            .iter()
            .map(|s| encode_segment(s))
            .collect::<Vec<_>>()
            .join("/");
        if url.ends_with('/') && !key.is_empty() {
            key.push('/');
        }
        Some(crate::location::encode(&self.id, "nfs", &key))
    }

    fn list(&self, location: Option<&str>) -> Result<Vec<Entry>> {
        let loc = NfsLocation::parse(&self.current_url(location.unwrap_or(&self.root))?)?;
        self.check_owned(&loc)?;
        let mut out = Vec::new();
        if self.whole_server() && loc.path.is_empty() {
            for export in self.list_exports()? {
                let mut child = loc.clone();
                child.path = export.clone();
                let url = format!("{}/", child.to_url());
                let url = self.canonical_location(&url).unwrap_or(url);
                out.push(dir_entry(format!("/{}", export.join("/")), url, None));
            }
            sort_entries(&mut out);
            return Ok(out);
        }
        for item in self.list_dir(&loc)? {
            if item.name.starts_with('.') {
                continue;
            }
            let mut child = loc.clone();
            child.path.push(item.name.clone());
            let url = child.to_url();
            let url = if item.is_dir { format!("{url}/") } else { url };
            let url = self.canonical_location(&url).unwrap_or(url);
            out.push(if item.is_dir {
                dir_entry(item.name, url, item.modified)
            } else {
                file_entry(item.name, url, Some(item.size), item.modified)
            });
        }
        sort_entries(&mut out);
        Ok(out)
    }

    fn open(&self, location: &str) -> Result<Arc<dyn ByteSource>> {
        let loc = NfsLocation::parse(&self.current_url(location)?)?;
        self.check_owned(&loc)?;
        if loc.path.len() <= self.export.len() {
            return Err(Error::invalid(
                location,
                "an export or server cannot be opened as a file",
            ));
        }
        self.open_file(&loc)
    }

    /// `nfs://host/folder/sub-folder`, with names as they are on the server.
    fn display_location(&self, location: Option<&str>) -> Option<String> {
        let loc =
            NfsLocation::parse(&self.current_url(location.unwrap_or(&self.root)).ok()?).ok()?;
        self.check_owned(&loc).ok()?;
        let mut out = nfs_url(&loc.host, loc.port, "");
        out.push_str(&loc.path.join("/"));
        Some(out)
    }

    fn parent(&self, location: &str) -> Option<String> {
        let mut loc = NfsLocation::parse(&self.current_url(location).ok()?).ok()?;
        self.check_owned(&loc).ok()?;
        if loc.path.len() <= self.export.len() {
            return None;
        }
        // An export's root goes up to the server's list of exports, when
        // the source is the whole server.
        if self.whole_server() && self.is_export(&loc.path) {
            loc.path.clear();
            return self.canonical_location(&loc.to_url());
        }
        loc.path.pop();
        self.canonical_location(&format!("{}/", loc.to_url()))
    }

    fn sidecars(&self, video: &Entry) -> Result<crate::Sidecars> {
        let mut video = video.clone();
        video.location = self.current_url(&video.location)?;
        crate::sidecars_from_folder(self, &video)
    }
}

/// One directory entry as the NFS layer reports it.
#[cfg_attr(not(feature = "nfs"), allow(dead_code))]
struct DirItem {
    name: String,
    is_dir: bool,
    size: u64,
    modified: Option<i64>,
}

#[cfg(not(feature = "nfs"))]
impl NfsSource {
    fn list_exports(&self) -> Result<Vec<Vec<String>>> {
        Err(Error::NfsUnavailable("compiled without NFS support".into()))
    }

    fn is_export(&self, _path: &[String]) -> bool {
        false
    }

    fn list_dir(&self, _loc: &NfsLocation) -> Result<Vec<DirItem>> {
        Err(Error::NfsUnavailable("compiled without NFS support".into()))
    }

    fn open_file(&self, _loc: &NfsLocation) -> Result<Arc<dyn ByteSource>> {
        Err(Error::NfsUnavailable("compiled without NFS support".into()))
    }
}

#[cfg(feature = "nfs")]
mod imp {
    //! The nfs3_client backed implementation.

    use super::{DirItem, NfsLocation, NfsSource, path_segments};
    use crate::cache::{CacheOptions, CachedSource, RangeFetch};
    use crate::config::NfsConfig;
    use crate::error::{Error, Result};
    use fp_core::source::{ByteSource, NetCounters};
    use fp_core::sync::lock;
    use nfs3_client::nfs3_types::mount::{dirpath, mountstat3};
    use nfs3_client::nfs3_types::nfs3::{
        self, FSINFO3args, LOOKUP3args, Nfs3Option, Nfs3Result, READ3args, READDIRPLUS3args,
        READLINK3args, cookieverf3, diropargs3, fattr3, filename3, ftype3, nfs_fh3, nfsstat3,
    };
    use nfs3_client::nfs3_types::rpc::{auth_stat, auth_unix, opaque_auth};
    use nfs3_client::nfs3_types::xdr_codec::Opaque;
    use nfs3_client::tokio::TokioIo;
    use nfs3_client::{MountClient, MountError, Nfs3Client, PortmapperClient, RpcError};
    use std::collections::{HashMap, VecDeque};
    use std::io;
    use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, ToSocketAddrs};
    use std::sync::atomic::{AtomicBool, AtomicU16, Ordering};
    use std::sync::{Arc, Mutex, OnceLock};
    use std::time::{Duration, Instant};
    use tokio::net::{TcpSocket, TcpStream};

    /// How long connecting, and each request, may take.
    const TIMEOUT: Duration = Duration::from_secs(15);
    /// The portmapper's well-known port.
    const PORTMAPPER: u16 = 111;
    /// Largest READ asked for, whatever the server allows.
    const MAX_READ: u32 = 1 << 20;
    /// Directory handles remembered per session before starting over.
    const MAX_HANDLES: usize = 4096;
    /// How long a server that wouldn't list its exports waits to be asked
    /// again.
    const RETRY_EXPORTS: Duration = Duration::from_secs(60);
    /// Symbolic links followed in one path before giving up, as Linux does.
    const MAX_LINKS: u32 = 40;

    type Io = TokioIo<TcpStream>;

    /// Set once binding a privileged port was refused: the system doesn't
    /// let this process, so it isn't tried again.
    static PRIVILEGED_DENIED: AtomicBool = AtomicBool::new(false);

    /// Waits for `work` on the runtime every source shares, giving up after
    /// [`TIMEOUT`].
    fn run<T>(what: &str, work: impl Future<Output = Result<T>>) -> Result<T> {
        static RUNTIME: OnceLock<std::result::Result<tokio::runtime::Runtime, String>> =
            OnceLock::new();
        let runtime = RUNTIME.get_or_init(|| {
            tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .thread_name("fp-nfs")
                .enable_all()
                .build()
                .map_err(|e| e.to_string())
        });
        match runtime {
            Ok(rt) => rt.block_on(async {
                tokio::time::timeout(TIMEOUT, work)
                    .await
                    .unwrap_or_else(|_| Err(Error::Nfs(format!("{what}: timed out"))))
            }),
            Err(e) => Err(Error::Nfs(format!("{what}: cannot start NFS: {e}"))),
        }
    }

    /// What to add when the server turns us away: Linux servers refuse
    /// requests from unprivileged ports unless the export says `insecure`.
    fn port_hint() -> &'static str {
        if PRIVILEGED_DENIED.load(Ordering::Relaxed) {
            " (connected from an unprivileged port: the export may need the `insecure` option)"
        } else {
            ""
        }
    }

    fn io_err(e: io::Error, what: &str) -> Error {
        Error::Nfs(format!("{what}: {e}"))
    }

    fn rpc_err(e: RpcError, what: &str) -> Error {
        match e {
            RpcError::Auth(stat) => match stat {
                auth_stat::AUTH_TOOWEAK => Error::Auth(format!("{what}{}", port_hint())),
                _ => Error::Auth(format!("{what} ({stat})")),
            },
            RpcError::ProgUnavail | RpcError::ProgMismatch { .. } => {
                Error::Nfs(format!("{what}: the server doesn't offer NFSv3 ({e})"))
            }
            e => Error::Nfs(format!("{what}: {e}")),
        }
    }

    fn mount_err(e: MountError, what: &str) -> Error {
        match e {
            MountError::Denied(mountstat3::MNT3ERR_ACCES | mountstat3::MNT3ERR_PERM) => {
                Error::Auth(format!("{what}{}", port_hint()))
            }
            MountError::Denied(mountstat3::MNT3ERR_NOENT) => {
                Error::NotFound(format!("{what}: no such export"))
            }
            MountError::Denied(mountstat3::MNT3ERR_NOTDIR) => Error::invalid(what, "not a folder"),
            MountError::Denied(stat) => Error::Nfs(format!("{what}: mount refused ({stat})")),
            MountError::Rpc(e) => rpc_err(e, what),
        }
    }

    /// Maps an NFS status, keeping "not found" and "access denied"
    /// distinguishable. A stale handle is a connection-level error, so the
    /// path is looked up again on a fresh session.
    fn status_err(stat: nfsstat3, what: &str) -> Error {
        match stat {
            nfsstat3::NFS3ERR_NOENT => Error::NotFound(what.to_string()),
            nfsstat3::NFS3ERR_ACCES | nfsstat3::NFS3ERR_PERM => Error::Auth(what.to_string()),
            nfsstat3::NFS3ERR_NOTDIR => Error::invalid(what, "not a folder"),
            nfsstat3::NFS3ERR_ISDIR => Error::invalid(what, "not a file"),
            _ => Error::Nfs(format!("{what}: {stat:?}")),
        }
    }

    /// Connects to `addr`, from a privileged port when the system allows
    /// (many servers insist on one), else from any.
    async fn connect(addr: SocketAddr, what: &str) -> Result<Io> {
        static NEXT_PORT: OnceLock<AtomicU16> = OnceLock::new();
        const LOW: u16 = 600;
        const HIGH: u16 = 1023;
        if !PRIVILEGED_DENIED.load(Ordering::Relaxed) {
            let next = NEXT_PORT.get_or_init(|| {
                let seed = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map_or(0, |d| d.subsec_nanos());
                AtomicU16::new(seed as u16)
            });
            for _ in 0..64 {
                let port = LOW + next.fetch_add(1, Ordering::Relaxed) % (HIGH - LOW + 1);
                let (socket, any) = if addr.is_ipv4() {
                    (TcpSocket::new_v4(), IpAddr::V4(Ipv4Addr::UNSPECIFIED))
                } else {
                    (TcpSocket::new_v6(), IpAddr::V6(Ipv6Addr::UNSPECIFIED))
                };
                let socket = socket.map_err(|e| io_err(e, what))?;
                match socket.bind(SocketAddr::new(any, port)) {
                    Ok(()) => {}
                    Err(e) if e.kind() == io::ErrorKind::PermissionDenied => {
                        log::info!("no privileged port for NFS ({e}); using any");
                        PRIVILEGED_DENIED.store(true, Ordering::Relaxed);
                        break;
                    }
                    Err(e) if e.kind() == io::ErrorKind::AddrInUse => continue,
                    Err(e) => return Err(io_err(e, what)),
                }
                match socket.connect(addr).await {
                    Ok(stream) => return Ok(TokioIo::new(stream)),
                    Err(e)
                        if matches!(
                            e.kind(),
                            io::ErrorKind::AddrInUse | io::ErrorKind::AddrNotAvailable
                        ) =>
                    {
                        continue;
                    }
                    Err(e) => return Err(io_err(e, what)),
                }
            }
        }
        TcpStream::connect(addr)
            .await
            .map(TokioIo::new)
            .map_err(|e| io_err(e, what))
    }

    /// The AUTH_SYS credential requests carry.
    fn credential(config: &NfsConfig) -> opaque_auth<'static> {
        #[cfg(unix)]
        // SAFETY: getuid and getgid cannot fail and touch no memory.
        let (uid, gid) = unsafe { (libc::getuid(), libc::getgid()) };
        #[cfg(not(unix))]
        let (uid, gid) = (65534, 65534);
        let uid = config.uid.unwrap_or(uid);
        let gid = config.gid.unwrap_or(gid);
        opaque_auth::auth_unix(&auth_unix {
            stamp: 0,
            machinename: Opaque::borrowed(b"frameplayerx"),
            uid,
            gid,
            gids: vec![gid],
        })
    }

    fn dir_path(path: &str) -> dirpath<'static> {
        dirpath(Opaque::owned(path.as_bytes().to_vec()))
    }

    /// A server path from its segments: `/volume1/videos`, `/` for none.
    fn server_path(segments: &[String]) -> String {
        format!("/{}", segments.join("/"))
    }

    /// The server's address and the ports of its MOUNT and NFS services.
    #[derive(Clone, Copy)]
    struct Ports {
        ip: IpAddr,
        mount: u16,
        nfs: u16,
    }

    /// A path looked up before: its handle, and where it really is inside
    /// its export once the links on the way are followed.
    #[derive(Clone)]
    struct Known {
        fh: nfs_fh3,
        real: Vec<String>,
    }

    /// One name still to look up while resolving a path, and the path asked
    /// for that it completes, to remember once it is found.
    struct Step {
        name: String,
        done: Option<String>,
    }

    /// Puts `names` in front of the steps still to take; the last carries
    /// `done`. With no names, `done` is returned: it is found already.
    fn prepend(
        pending: &mut VecDeque<Step>,
        names: Vec<String>,
        done: Option<String>,
    ) -> Option<String> {
        let n = names.len();
        if n == 0 {
            return done;
        }
        let mut done = done;
        for (i, name) in names.into_iter().enumerate().rev() {
            let done = if i == n - 1 { done.take() } else { None };
            pending.push_front(Step { name, done });
        }
        None
    }

    /// An absolute path with `.` and `..` taken out (`..` at the root stays
    /// there, as it does on the server).
    fn normalize(path: &str) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        for seg in super::segments(path) {
            match seg {
                "." => {}
                ".." => {
                    out.pop();
                }
                seg => out.push(seg.to_string()),
            }
        }
        out
    }

    /// The cache key of `rest` inside the export at `export_path`.
    fn key(export_path: &str, rest: &[String]) -> String {
        let mut out = export_path.trim_end_matches('/').to_string();
        for name in rest {
            out.push('/');
            out.push_str(name);
        }
        out
    }

    /// A connection to the NFS service, the exports it mounted and handles
    /// it looked up.
    struct Session {
        client: Nfs3Client<Io>,
        ports: Ports,
        credential: opaque_auth<'static>,
        /// Root handle of each mounted export, by server path.
        roots: HashMap<String, nfs_fh3>,
        /// Handles of folders and files under them, by the server path
        /// asked for (which may go through links).
        handles: HashMap<String, Known>,
        /// Largest READ the server takes.
        rtmax: u32,
    }

    impl Session {
        /// The root handle of `export`, mounting it first if need be.
        async fn root(&mut self, export: &[String], url: &str) -> Result<nfs_fh3> {
            let path = server_path(export);
            if let Some(fh) = self.roots.get(&path) {
                return Ok(fh.clone());
            }
            let io = connect(SocketAddr::new(self.ports.ip, self.ports.mount), url).await?;
            let mut mount =
                MountClient::new_with_auth(io, self.credential.clone(), opaque_auth::default());
            let ok = mount
                .mnt(dir_path(&path))
                .await
                .map_err(|e| mount_err(e, url))?;
            let fh = nfs_fh3 {
                data: ok.fhandle.0.clone(),
            };
            if self.roots.is_empty() {
                self.rtmax = match self.client.fsinfo(&FSINFO3args::from(fh.clone())).await {
                    Ok(Nfs3Result::Ok(info)) if info.rtmax > 0 => info.rtmax.min(MAX_READ),
                    _ => 64 * 1024,
                };
            }
            self.roots.insert(path, fh.clone());
            Ok(fh)
        }

        /// The handle of `rest` inside `export`, following symbolic links.
        async fn handle(
            &mut self,
            export: &[String],
            rest: &[String],
            url: &str,
        ) -> Result<nfs_fh3> {
            Ok(self.resolve(export, rest, url).await?.fh)
        }

        /// Looks up `rest` inside `export` from the longest part of it seen
        /// before, following symbolic links the way the server's own system
        /// would: a relative target from the link's folder, an absolute one
        /// from the server's root. A target outside the export can't be
        /// reached through it, so it is an error, as are links that go round
        /// in circles.
        async fn resolve(
            &mut self,
            export: &[String],
            rest: &[String],
            url: &str,
        ) -> Result<Known> {
            let root = self.root(export, url).await?;
            let export_path = server_path(export);
            let mut at = Known {
                fh: root.clone(),
                real: Vec::new(),
            };
            let mut start = 0;
            for n in (1..=rest.len()).rev() {
                if let Some(known) = self.handles.get(&key(&export_path, &rest[..n])) {
                    at = known.clone();
                    start = n;
                    break;
                }
            }
            let mut pending: VecDeque<Step> = (start..rest.len())
                .map(|i| Step {
                    name: rest[i].clone(),
                    done: Some(key(&export_path, &rest[..=i])),
                })
                .collect();
            let mut links = 0;
            while let Some(step) = pending.pop_front() {
                let mut done = step.done;
                match step.name.as_str() {
                    "" | "." => {}
                    ".." => {
                        if at.real.pop().is_none() {
                            return Err(Error::invalid(url, "a link leads outside the export"));
                        }
                        // The folder above, looked up again from the root.
                        let real = std::mem::take(&mut at.real);
                        at.fh = root.clone();
                        done = prepend(&mut pending, real, done);
                    }
                    name => {
                        let (fh, attrs) = self.lookup(&at.fh, name, url).await?;
                        if attrs.type_ == ftype3::NF3LNK {
                            links += 1;
                            if links > MAX_LINKS {
                                return Err(Error::invalid(
                                    url,
                                    "too many levels of symbolic links",
                                ));
                            }
                            let target = self.readlink(&fh, url).await?;
                            let names = if target.starts_with('/') {
                                let absolute = normalize(&target);
                                let inside = absolute
                                    .strip_prefix(export)
                                    .ok_or_else(|| {
                                        Error::invalid(url, "a link leads outside the export")
                                    })?
                                    .to_vec();
                                at = Known {
                                    fh: root.clone(),
                                    real: Vec::new(),
                                };
                                inside
                            } else {
                                path_segments(&target)
                            };
                            done = prepend(&mut pending, names, done);
                        } else {
                            at.fh = fh;
                            at.real.push(name.to_string());
                        }
                    }
                }
                if let Some(done) = done {
                    self.remember(done, at.clone());
                }
            }
            Ok(at)
        }

        /// Looks `name` up in the folder `dir`: its handle and attributes.
        async fn lookup(
            &mut self,
            dir: &nfs_fh3,
            name: &str,
            url: &str,
        ) -> Result<(nfs_fh3, fattr3)> {
            let found = self
                .client
                .lookup(&LOOKUP3args {
                    what: diropargs3 {
                        dir: dir.clone(),
                        name: filename3::from(name.as_bytes()),
                    },
                })
                .await
                .map_err(|e| rpc_err(e, url))?;
            let ok = match found {
                Nfs3Result::Ok(ok) => ok,
                Nfs3Result::Err((stat, _)) => return Err(status_err(stat, url)),
            };
            let attrs = match ok.obj_attributes {
                Nfs3Option::Some(attrs) => attrs,
                Nfs3Option::None => self.getattr(&ok.object, url).await?,
            };
            Ok((ok.object, attrs))
        }

        async fn getattr(&mut self, fh: &nfs_fh3, url: &str) -> Result<fattr3> {
            match self
                .client
                .getattr(&fh.clone().into())
                .await
                .map_err(|e| rpc_err(e, url))?
            {
                Nfs3Result::Ok(ok) => Ok(ok.obj_attributes),
                Nfs3Result::Err((stat, _)) => Err(status_err(stat, url)),
            }
        }

        /// Where the symbolic link `fh` points.
        async fn readlink(&mut self, fh: &nfs_fh3, url: &str) -> Result<String> {
            let reply = self
                .client
                .readlink(&READLINK3args {
                    symlink: fh.clone(),
                })
                .await
                .map_err(|e| rpc_err(e, url))?;
            match reply {
                Nfs3Result::Ok(ok) => {
                    let target = String::from_utf8_lossy(ok.data.0.as_ref()).into_owned();
                    if target.is_empty() || target.contains('\0') {
                        return Err(Error::invalid(url, "a link with no target"));
                    }
                    Ok(target)
                }
                Nfs3Result::Err((stat, _)) => Err(status_err(stat, url)),
            }
        }

        fn remember(&mut self, path: String, known: Known) {
            if self.handles.len() >= MAX_HANDLES {
                self.handles.clear();
            }
            self.handles.insert(path, known);
        }

        /// Lists the folder `rest` inside `export`.
        async fn list(
            &mut self,
            export: &[String],
            rest: &[String],
            url: &str,
        ) -> Result<Vec<DirItem>> {
            let dir = self.resolve(export, rest, url).await?;
            let export_path = server_path(export);
            let mut out = Vec::new();
            let mut cookie = 0;
            let mut cookieverf = cookieverf3::default();
            loop {
                let reply = self
                    .client
                    .readdirplus(&READDIRPLUS3args {
                        dir: dir.fh.clone(),
                        cookie,
                        cookieverf,
                        dircount: 16 * 1024,
                        maxcount: 128 * 1024,
                    })
                    .await
                    .map_err(|e| rpc_err(e, url))?;
                let ok = match reply {
                    Nfs3Result::Ok(ok) => ok,
                    Nfs3Result::Err((stat, _)) => return Err(status_err(stat, url)),
                };
                cookieverf = ok.cookieverf;
                let entries = ok.reply.entries.into_inner();
                let empty = entries.is_empty();
                for entry in entries {
                    cookie = entry.cookie;
                    let name = String::from_utf8_lossy(entry.name.as_ref()).into_owned();
                    if name == "." || name == ".." || name.contains(['/', '\0']) {
                        continue;
                    }
                    let child = [rest, std::slice::from_ref(&name)].concat();
                    let attrs = match entry.name_attributes {
                        Nfs3Option::Some(attrs) if attrs.type_ != ftype3::NF3LNK => {
                            if let Nfs3Option::Some(fh) = entry.name_handle {
                                let mut real = dir.real.clone();
                                real.push(name.clone());
                                self.remember(key(&export_path, &child), Known { fh, real });
                            }
                            attrs
                        }
                        // A link is listed as what it leads to; a broken
                        // one, or one leading out of the export, can't be
                        // opened, so it isn't.
                        _ => match self.attributes(export, &child, url).await {
                            Ok(attrs) => attrs,
                            Err(e) => {
                                log::debug!("{url}: skipping {name}: {e}");
                                continue;
                            }
                        },
                    };
                    // Devices, sockets and the like aren't media.
                    let is_dir = match attrs.type_ {
                        ftype3::NF3DIR => true,
                        ftype3::NF3REG => false,
                        _ => continue,
                    };
                    out.push(DirItem {
                        name,
                        is_dir,
                        size: attrs.size,
                        modified: Some(i64::from(attrs.mtime.seconds)),
                    });
                }
                if ok.reply.eof || empty {
                    break;
                }
            }
            Ok(out)
        }

        /// Attributes of `rest` inside `export`, or of what it links to.
        async fn attributes(
            &mut self,
            export: &[String],
            rest: &[String],
            url: &str,
        ) -> Result<fattr3> {
            let fh = self.handle(export, rest, url).await?;
            self.getattr(&fh, url).await
        }

        /// Size of the regular file at `fh`.
        async fn file_size(&mut self, fh: &nfs_fh3, url: &str) -> Result<u64> {
            let attrs = self.getattr(fh, url).await?;
            match attrs.type_ {
                ftype3::NF3REG => Ok(attrs.size),
                ftype3::NF3DIR => Err(Error::invalid(url, "not a file")),
                _ => Err(Error::invalid(url, "not a regular file")),
            }
        }

        /// One READ: up to `count` bytes at `offset`, and whether it reached
        /// the end of the file.
        async fn read(
            &mut self,
            fh: &nfs_fh3,
            offset: u64,
            count: u32,
            url: &str,
        ) -> Result<(Vec<u8>, bool)> {
            let reply = self
                .client
                .read(&READ3args {
                    file: fh.clone(),
                    offset,
                    count: count.min(self.rtmax),
                })
                .await
                .map_err(|e| rpc_err(e, url))?;
            match reply {
                Nfs3Result::Ok(ok) => Ok((ok.data.into_owned(), ok.eof)),
                Nfs3Result::Err((stat, _)) => Err(status_err(stat, url)),
            }
        }
    }

    /// The source's current session, shared with its open files, so a file
    /// that loses its connection reconnects for everyone; and the server's
    /// exports, once asked.
    pub(super) struct Client {
        config: NfsConfig,
        root: String,
        current: Mutex<Option<Arc<Mutex<Session>>>>,
        exports: Mutex<Option<Vec<Vec<String>>>>,
        /// What to mount for the configured folder, and when to ask again
        /// if the server wouldn't say.
        mount_point: Mutex<Option<(Vec<String>, Option<Instant>)>>,
    }

    impl Client {
        pub(super) fn new(config: &NfsConfig, root: &str) -> Client {
            Client {
                config: config.clone(),
                root: root.to_string(),
                current: Mutex::new(None),
                exports: Mutex::new(None),
                mount_point: Mutex::new(None),
            }
        }

        /// The server's address, looked up anew for each session.
        fn server_ip(&self) -> Result<IpAddr> {
            let host = self
                .config
                .host
                .trim_start_matches('[')
                .trim_end_matches(']');
            (host, PORTMAPPER)
                .to_socket_addrs()
                .map_err(|e| io_err(e, &self.root))?
                .next()
                .map(|a| a.ip())
                .ok_or_else(|| Error::Nfs(format!("{}: host not found", self.root)))
        }

        /// Where the MOUNT and NFS services listen. With a port in the
        /// address, MOUNT is first tried on it too (servers in user space
        /// often offer both on one port, and no portmapper); otherwise, and
        /// when that fails, the portmapper is asked.
        fn ports(&self) -> Result<Ports> {
            let ip = self.server_ip()?;
            let root = &self.root;
            let fixed_nfs = self.config.port;
            if let Some(port) = fixed_nfs {
                let credential = credential(&self.config);
                let both = run(root, async move {
                    let io = connect(SocketAddr::new(ip, port), root).await?;
                    MountClient::new_with_auth(io, credential, opaque_auth::default())
                        .null()
                        .await
                        .map_err(|e| rpc_err(e, root))
                });
                match both {
                    Ok(()) => {
                        return Ok(Ports {
                            ip,
                            mount: port,
                            nfs: port,
                        });
                    }
                    Err(e) => {
                        log::debug!("{root}: no MOUNT on port {port} ({e}); asking portmapper");
                    }
                }
            }
            run(root, async move {
                let io = connect(SocketAddr::new(ip, PORTMAPPER), root).await?;
                let mut portmap = PortmapperClient::new(io);
                let mut getport = async |prog, vers, what: &str| {
                    portmap
                        .getport(prog, vers, nfs3_client::nfs3_types::portmap::IPPROTO_TCP)
                        .await
                        .map_err(|e| Error::Nfs(format!("{root}: no {what} service: {e}")))
                };
                let mount = getport(
                    nfs3_client::nfs3_types::mount::PROGRAM,
                    nfs3_client::nfs3_types::mount::VERSION,
                    "NFSv3 mount",
                )
                .await?;
                let nfs = match fixed_nfs {
                    Some(port) => port,
                    None => getport(nfs3::PROGRAM, nfs3::VERSION, "NFSv3").await?,
                };
                Ok(Ports { ip, mount, nfs })
            })
        }

        /// The current session, connecting if there is none.
        fn session(&self) -> Result<Arc<Mutex<Session>>> {
            let mut current = lock(&self.current);
            if let Some(s) = &*current {
                return Ok(s.clone());
            }
            log::info!("connecting to {}", self.root);
            let ports = self.ports()?;
            let credential = credential(&self.config);
            let root = &self.root;
            let client = run(root, async {
                let io = connect(SocketAddr::new(ports.ip, ports.nfs), root).await?;
                Ok(Nfs3Client::new_with_auth(
                    io,
                    credential.clone(),
                    opaque_auth::default(),
                ))
            })?;
            let session = Arc::new(Mutex::new(Session {
                client,
                ports,
                credential,
                roots: HashMap::new(),
                handles: HashMap::new(),
                rtmax: 64 * 1024,
            }));
            *current = Some(session.clone());
            Ok(session)
        }

        /// Runs `op` with the current session; after a connection-level
        /// failure the session is dropped so the next call connects again.
        fn with_session<T>(&self, op: impl FnOnce(&Arc<Mutex<Session>>) -> Result<T>) -> Result<T> {
            let session = self.session()?;
            let r = op(&session);
            if let Err(Error::Nfs(_)) = &r {
                self.discard(&session);
            }
            r
        }

        /// Forgets `stale` unless it was already replaced.
        fn discard(&self, stale: &Arc<Mutex<Session>>) {
            let mut current = lock(&self.current);
            if current.as_ref().is_some_and(|s| Arc::ptr_eq(s, stale)) {
                *current = None;
            }
        }

        /// The server's exports, longest first, asked once.
        fn exports(&self) -> Result<Vec<Vec<String>>> {
            if let Some(known) = &*lock(&self.exports) {
                return Ok(known.clone());
            }
            let ports = self.ports()?;
            let credential = credential(&self.config);
            let root = &self.root;
            let mut exports: Vec<Vec<String>> = run(root, async {
                let io = connect(SocketAddr::new(ports.ip, ports.mount), root).await?;
                let mut mount = MountClient::new_with_auth(io, credential, opaque_auth::default());
                let list = mount.export().await.map_err(|e| rpc_err(e, root))?;
                Ok(list
                    .into_inner()
                    .into_iter()
                    .map(|node| path_segments(&String::from_utf8_lossy(node.ex_dir.0.as_ref())))
                    .collect())
            })?;
            exports.sort_by_key(|e| std::cmp::Reverse(e.len()));
            exports.dedup();
            *lock(&self.exports) = Some(exports.clone());
            Ok(exports)
        }

        /// The export to mount for the configured folder `folder`: the
        /// server's export holding it, so links in it can lead anywhere in
        /// that export (`../other` from `/media/videos` in `/media`). A
        /// server that won't list its exports gets the folder mounted
        /// directly, and is asked again a minute later.
        fn mount_point(&self, folder: &[String]) -> Vec<String> {
            if let Some((known, retry)) = &*lock(&self.mount_point)
                && retry.is_none_or(|at| Instant::now() < at)
            {
                return known.clone();
            }
            let (point, retry) = match self.exports() {
                Ok(exports) => (
                    exports
                        .into_iter()
                        .find(|e| folder.starts_with(e))
                        .unwrap_or_else(|| folder.to_vec()),
                    None,
                ),
                Err(e) => {
                    log::info!(
                        "{}: exports not listed ({e}); mounting the folder",
                        self.root
                    );
                    (folder.to_vec(), Some(Instant::now() + RETRY_EXPORTS))
                }
            };
            *lock(&self.mount_point) = Some((point.clone(), retry));
            point
        }

        /// The exports known so far, without asking the server.
        fn known_exports(&self) -> Option<Vec<Vec<String>>> {
            lock(&self.exports).clone()
        }

        #[cfg(test)]
        pub(super) fn set_exports(&self, exports: &[&str]) {
            let mut list: Vec<Vec<String>> = exports.iter().map(|e| path_segments(e)).collect();
            list.sort_by_key(|e| std::cmp::Reverse(e.len()));
            *lock(&self.exports) = Some(list);
        }
    }

    impl NfsSource {
        pub(super) fn list_exports(&self) -> Result<Vec<Vec<String>>> {
            let mut exports = self.client.exports()?;
            exports.sort();
            Ok(exports)
        }

        /// Whether `path` is one of the server's exports (only those already
        /// known; asked of the server when none are).
        pub(super) fn is_export(&self, path: &[String]) -> bool {
            let exports = match self.client.known_exports() {
                Some(e) => e,
                None => self.client.exports().unwrap_or_default(),
            };
            exports.iter().any(|e| e.as_slice() == path)
        }

        /// Splits a server path into the export to mount and the path inside
        /// it: the longest of the server's exports the path is in (for a
        /// source on one folder, the export holding that folder).
        pub(super) fn split<'a>(&self, path: &'a [String]) -> Result<(Vec<String>, &'a [String])> {
            if !self.whole_server() {
                let export = self.client.mount_point(&self.export);
                let n = export.len();
                return Ok((export, &path[n..]));
            }
            let exports = self.client.exports()?;
            exports
                .into_iter()
                .find(|e| path.starts_with(e))
                .map(|e| {
                    let n = e.len();
                    (e, &path[n..])
                })
                .ok_or_else(|| {
                    Error::NotFound(format!(
                        "{}: not in any export",
                        super::nfs_url(&self.config.host, self.config.port, &path.join("/"),)
                    ))
                })
        }

        pub(super) fn list_dir(&self, loc: &NfsLocation) -> Result<Vec<DirItem>> {
            let url = loc.to_url();
            let (export, rest) = self.split(&loc.path)?;
            self.client.with_session(|session| {
                let mut s = lock(session);
                run(&url, s.list(&export, rest, &url))
            })
        }

        pub(super) fn open_file(&self, loc: &NfsLocation) -> Result<Arc<dyn ByteSource>> {
            let url = loc.to_url();
            let (export, rest) = self.split(&loc.path)?;
            let file = OpenFile::open(&self.client, &export, rest, &url, 0)?;
            let net = Arc::new(NetCounters::default());
            let fetcher = NfsFetcher {
                client: self.client.clone(),
                export,
                rest: rest.to_vec(),
                size: file.size,
                open: Mutex::new(file),
                url,
                net: net.clone(),
            };
            Ok(Arc::new(CachedSource::counted(
                fetcher,
                &CacheOptions::default(),
                net,
            )))
        }
    }

    /// A file's handle and the session it was looked up on.
    #[derive(Clone)]
    struct OpenFile {
        session: Arc<Mutex<Session>>,
        fh: nfs_fh3,
        size: u64,
        /// Counts reopens, so threads that fail together reconnect once.
        generation: u64,
    }

    impl OpenFile {
        fn open(
            client: &Client,
            export: &[String],
            rest: &[String],
            url: &str,
            generation: u64,
        ) -> Result<OpenFile> {
            client.with_session(|session| {
                let mut s = lock(session);
                let (fh, size) = run(url, async {
                    let fh = s.handle(export, rest, url).await?;
                    let size = s.file_size(&fh, url).await?;
                    Ok((fh, size))
                })?;
                Ok(OpenFile {
                    session: session.clone(),
                    fh,
                    size,
                    generation,
                })
            })
        }

        /// Reads `len` bytes at `offset`, fewer only at the end of the file.
        fn read(&self, offset: u64, len: usize, url: &str) -> Result<Vec<u8>> {
            let mut out = Vec::with_capacity(len);
            while out.len() < len {
                let at = offset + out.len() as u64;
                let want = u32::try_from(len - out.len()).unwrap_or(u32::MAX);
                let (data, eof) = {
                    let mut s = lock(&self.session);
                    run(url, s.read(&self.fh, at, want, url))?
                };
                let short = data.is_empty();
                out.extend_from_slice(&data);
                if eof || short {
                    break;
                }
            }
            out.truncate(len);
            Ok(out)
        }
    }

    /// Reads ranges of one NFS file.
    struct NfsFetcher {
        client: Arc<Client>,
        export: Vec<String>,
        rest: Vec<String>,
        open: Mutex<OpenFile>,
        size: u64,
        url: String,
        net: Arc<NetCounters>,
    }

    impl NfsFetcher {
        /// Replaces the handle of generation `failed` with one on a new
        /// connection; when another thread already did, returns its handle.
        fn reopen(&self, failed: &OpenFile) -> Result<OpenFile> {
            let mut open = lock(&self.open);
            if open.generation != failed.generation {
                return Ok(open.clone());
            }
            self.net.retries.fetch_add(1, Ordering::Relaxed);
            self.client.discard(&failed.session);
            let fresh = OpenFile::open(
                &self.client,
                &self.export,
                &self.rest,
                &self.url,
                failed.generation + 1,
            )?;
            *open = fresh.clone();
            Ok(fresh)
        }
    }

    impl RangeFetch for NfsFetcher {
        fn fetch(&self, offset: u64, len: usize) -> io::Result<Vec<u8>> {
            if offset >= self.size {
                return Ok(Vec::new());
            }
            let open = lock(&self.open).clone();
            match open.read(offset, len, &self.url) {
                Ok(data) => Ok(data),
                Err(e) => {
                    // A server that drops the connection, a request that
                    // timed out or a handle gone stale must not end
                    // playback: reconnect and read again, once.
                    log::warn!("{e} at {offset}; reconnecting");
                    let fresh = self.reopen(&open)?;
                    Ok(fresh.read(offset, len, &self.url)?)
                }
            }
        }

        fn size(&self) -> Option<u64> {
            Some(self.size)
        }

        fn describe(&self) -> String {
            self.url.clone()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config() -> NfsConfig {
        NfsConfig {
            id: "n".into(),
            name: "NAS".into(),
            host: "nas.local".into(),
            port: None,
            export: "/volume1/videos".into(),
            uid: Some(1000),
            gid: Some(100),
        }
    }

    #[test]
    fn urls_roundtrip() {
        assert_eq!(nfs_url("nas", None, ""), "nfs://nas/");
        assert_eq!(
            nfs_url("nas", Some(2049), "/volume1/My Videos/Clip #1.mp4"),
            "nfs://nas:2049/volume1/My%20Videos/Clip%20%231.mp4"
        );
        assert_eq!(nfs_url("fe80::1", None, "/srv"), "nfs://[fe80::1]/srv");
        let l = NfsLocation::parse("nfs://nas:2049/volume1/My%20Videos/Clip%20%231.mp4").unwrap();
        assert_eq!(l.host, "nas");
        assert_eq!(l.port, Some(2049));
        assert_eq!(l.path, ["volume1", "My Videos", "Clip #1.mp4"]);
        assert_eq!(
            l.to_url(),
            "nfs://nas:2049/volume1/My%20Videos/Clip%20%231.mp4"
        );
        let v6 = NfsLocation::parse("NFS://[fe80::1]:2049/srv/").unwrap();
        assert_eq!((v6.host.as_str(), v6.port), ("fe80::1", Some(2049)));
        assert_eq!(v6.path, ["srv"]);
        for url in ["nfs://nas", "nfs://nas/", "nfs://192.168.0.10/"] {
            assert!(NfsLocation::parse(url).unwrap().path.is_empty(), "{url}");
        }
        assert!(NfsLocation::parse("nfs:///srv").is_err());
        assert!(NfsLocation::parse("smb://nas/s").is_err());
        assert!(NfsLocation::parse("nfs://nas/srv/../etc").is_err());
        assert!(NfsLocation::parse("nfs://nas/srv/%2e%2e/etc").is_err());
        assert!(NfsLocation::parse("nfs://nas:x/s").is_err());
    }

    #[test]
    fn source_basics_without_network() {
        let s = NfsSource::new(&config());
        if !cfg!(feature = "nfs") {
            assert!(matches!(s, Err(Error::NfsUnavailable(_))));
            return;
        }
        let s = s.unwrap();
        assert_eq!(s.root(), "nfs://nas.local/volume1/videos");
        assert_eq!(s.kind(), SourceKind::Nfs);
        assert!(s.describe().contains("1000:100"));
        assert_eq!(
            s.parent("nfs://nas.local/volume1/videos/a.mp4"),
            Some(crate::location::encode("n", "nfs", ""))
        );
        assert_eq!(
            s.parent("nfs://nas.local/volume1/videos/sub/a.mp4"),
            Some(crate::location::encode("n", "nfs", "sub/"))
        );
        assert_eq!(s.parent("nfs://nas.local/volume1/videos/"), None);
        assert!(matches!(
            s.list(Some("nfs://other/volume1/videos/")),
            Err(Error::InvalidLocation { .. })
        ));
        assert!(matches!(
            s.list(Some("nfs://nas.local/volume1/music/")),
            Err(Error::InvalidLocation { .. })
        ));
        assert!(matches!(
            s.open("nfs://nas.local/volume1/videos"),
            Err(Error::InvalidLocation { .. })
        ));
        // Readable addresses, for the root and folders under it.
        assert_eq!(
            s.display_location(None).as_deref(),
            Some("nfs://nas.local/volume1/videos")
        );
        let sub = s
            .canonical_location("nfs://nas.local/volume1/videos/My%20Films/2024/")
            .unwrap();
        assert_eq!(
            s.display_location(Some(&sub)).as_deref(),
            Some("nfs://nas.local/volume1/videos/My Films/2024")
        );
        assert_eq!(s.display_location(Some("nfs://other/x/")), None);
        let mut whole = config();
        whole.export.clear();
        whole.port = Some(2049);
        let whole = NfsSource::new(&whole).unwrap();
        assert_eq!(
            whole.display_location(None).as_deref(),
            Some("nfs://nas.local:2049/")
        );
        let mut bad = config();
        bad.host.clear();
        assert!(matches!(NfsSource::new(&bad), Err(Error::Config(_))));
    }

    #[cfg(feature = "nfs")]
    #[test]
    fn stable_locations_follow_the_configured_host_and_export() {
        let before = NfsSource::new(&config()).unwrap();
        let stable = before
            .canonical_location("nfs://nas.local/volume1/videos/sub/Clip%20%231.mp4")
            .unwrap();
        assert_eq!(
            crate::location::decode(&stable).unwrap().key,
            "sub/Clip%20%231.mp4"
        );
        let mut changed = config();
        changed.host = "10.0.0.2".into();
        changed.export = "/export/videos".into();
        let after = NfsSource::new(&changed).unwrap();
        assert_eq!(
            after.current_url(&stable).unwrap(),
            "nfs://10.0.0.2/export/videos/sub/Clip%20%231.mp4"
        );
        assert_eq!(after.canonical_location(&stable), Some(stable.clone()));
        for bad in [
            crate::location::encode("other", "nfs", "file.mp4"),
            crate::location::encode("n", "smb", "file.mp4"),
            crate::location::encode("n", "nfs", "../file.mp4"),
            crate::location::encode("n", "nfs", "%2e%2e/file.mp4"),
            crate::location::encode("n", "nfs", "%2Foutside/file.mp4"),
        ] {
            assert!(after.current_url(&bad).is_err(), "{bad}");
        }
        assert!(
            before
                .canonical_location("nfs://nas.local/volume1/videos-other/a.mp4")
                .is_none()
        );
        assert!(
            before
                .canonical_location("nfs://nas.local:2049/volume1/videos/a.mp4")
                .is_none()
        );
    }

    #[cfg(feature = "nfs")]
    #[test]
    fn a_whole_server_goes_up_from_an_export_to_its_list_of_exports() {
        let mut c = config();
        c.export.clear();
        let s = NfsSource::new(&c).unwrap();
        assert_eq!(s.root(), "nfs://nas.local/");
        assert!(s.describe().starts_with("NFS server"));
        s.client.set_exports(&["/volume1/videos", "/volume1"]);
        let stable = s
            .canonical_location("nfs://nas.local/volume1/videos/a.mp4")
            .unwrap();
        assert_eq!(
            crate::location::decode(&stable).unwrap().key,
            "volume1/videos/a.mp4"
        );
        assert_eq!(
            s.parent("nfs://nas.local/volume1/videos/sub/"),
            Some(crate::location::encode("n", "nfs", "volume1/videos/"))
        );
        assert_eq!(
            s.parent("nfs://nas.local/volume1/videos/"),
            Some(crate::location::encode("n", "nfs", ""))
        );
        assert_eq!(
            s.parent("nfs://nas.local/volume1/other/"),
            Some(crate::location::encode("n", "nfs", "volume1/"))
        );
        assert_eq!(s.parent("nfs://nas.local/"), None);
        assert!(matches!(
            s.open("nfs://nas.local/"),
            Err(Error::InvalidLocation { .. })
        ));
        // The longest export a path is in is the one mounted.
        let path = path_segments("/volume1/videos/a.mp4");
        let (export, rest) = s.split(&path).unwrap();
        assert_eq!(
            (export.join("/"), rest.join("/")),
            ("volume1/videos".into(), "a.mp4".into())
        );
        let path = path_segments("/volume1/music/a.mp3");
        let (export, rest) = s.split(&path).unwrap();
        assert_eq!(
            (export.join("/"), rest.join("/")),
            ("volume1".into(), "music/a.mp3".into())
        );
    }

    #[cfg(feature = "nfs")]
    #[test]
    fn unreachable_server_is_an_error_not_a_hang() {
        let mut c = config();
        c.host = "127.0.0.1".into();
        // Nothing listens on the portmapper port in the test environment,
        // or if something does, the mount fails: either way an error.
        let s = NfsSource::new(&c).unwrap();
        assert!(s.list(None).is_err());
    }
}
