//! SMB2/3 shares (Windows, Samba, NAS boxes).
//!
//! # Implementation choice
//!
//! This uses [`smb`](https://crates.io/crates/smb) (smb-rs), a maintained
//! pure-Rust SMB 2.0.2-3.1.1 client with NTLM authentication, signing and
//! encryption, in its blocking `multi_threaded` mode (no async runtime). It
//! cross-compiles for `aarch64-unknown-linux-gnu` with `cargo zigbuild`
//! without any system library, so the `libsmbclient`-via-`libloading`
//! fallback is not needed. Build with `--no-default-features` to leave SMB
//! out; [`SmbSource::new`] then fails with [`Error::SmbUnavailable`].
//!
//! A source is one share (`smb://host/share[/folder]`) or a whole server
//! (`smb://host/`), whose root lists its shares.
//!
//! Locations are `smb://host[:port]/share/dir/file` with percent-encoded
//! segments and no credentials. Files are read through the shared block
//! cache with read-ahead, in requests no larger than the server's
//! negotiated maximum read size.

use crate::config::{SmbConfig, SourceKind};
use crate::error::{Error, Result};
use crate::urlutil::{encode_segment, percent_decode};
use crate::{Source, dir_entry, file_entry, sort_entries};
use fp_core::source::{ByteSource, Entry};
use std::sync::Arc;

#[cfg_attr(not(feature = "smb"), allow(dead_code))]
mod srvsvc;

/// Builds `smb://host[:port]/share/path` with every segment
/// percent-encoded. `path` is `/`-separated and may be empty.
pub fn smb_url(host: &str, port: Option<u16>, share: &str, path: &str) -> String {
    let host = if host.contains(':') && !host.starts_with('[') {
        format!("[{host}]") // IPv6 literal
    } else {
        host.to_string()
    };
    let mut out = format!("smb://{host}");
    if let Some(p) = port.filter(|p| *p != 445) {
        out.push_str(&format!(":{p}"));
    }
    out.push('/');
    out.push_str(&encode_segment(share));
    for seg in path.split('/').filter(|s| !s.is_empty()) {
        out.push('/');
        out.push_str(&encode_segment(seg));
    }
    out
}

/// A location split into share and decoded path segments.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SmbLocation {
    /// Host name or address (IPv6 without brackets).
    pub host: String,
    /// TCP port, when given.
    pub port: Option<u16>,
    /// Empty for the server itself (`smb://host/`), which lists its shares.
    pub share: String,
    /// Decoded path segments inside the share.
    pub path: Vec<String>,
}

impl SmbLocation {
    /// Parses `smb://host[:port]/share/path`, or `smb://host[:port]/` for
    /// the server. Credentials in the URL are ignored (they belong in the
    /// configuration).
    pub fn parse(location: &str) -> Result<SmbLocation> {
        let rest = location
            .get(..6)
            .filter(|p| p.eq_ignore_ascii_case("smb://"))
            .map(|_| &location[6..])
            .ok_or_else(|| Error::invalid(location, "expected an smb:// URL"))?;
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
        let mut segs = path
            .split('/')
            .filter(|s| !s.is_empty())
            .map(percent_decode);
        let share = segs.next().unwrap_or_default();
        let path: Vec<String> = segs.collect();
        if host.is_empty() {
            return Err(Error::invalid(location, "no host"));
        }
        if path.iter().any(|s| s == ".." || s.contains('\\')) {
            return Err(Error::invalid(location, "path escapes the share"));
        }
        Ok(SmbLocation {
            host,
            port,
            share,
            path,
        })
    }

    /// The server itself, not a share on it.
    pub fn is_server(&self) -> bool {
        self.share.is_empty()
    }

    /// Back to an `smb://` URL.
    pub fn to_url(&self) -> String {
        smb_url(&self.host, self.port, &self.share, &self.path.join("/"))
    }

    /// Path inside the share with backslashes (empty for the share root).
    pub fn share_path(&self) -> String {
        self.path.join("\\")
    }
}

/// An SMB2/3 share, or every share on a server.
pub struct SmbSource {
    id: String,
    name: String,
    config: SmbConfig,
    root: String,
    #[cfg(feature = "smb")]
    sessions: Arc<imp::Sessions>,
}

impl std::fmt::Debug for SmbSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SmbSource")
            .field("id", &self.id)
            .field("root", &self.root)
            .field("credentials", &self.config.credentials)
            .finish_non_exhaustive()
    }
}

impl SmbSource {
    /// Creates the source. No connection is made until it is used.
    pub fn new(config: &SmbConfig) -> Result<SmbSource> {
        if config.host.trim().is_empty() {
            return Err(Error::Config("SMB source needs a host".into()));
        }
        if !cfg!(feature = "smb") {
            return Err(Error::SmbUnavailable(
                "this build of FramePlayerX was compiled without SMB support".into(),
            ));
        }
        let root = smb_url(&config.host, config.port, &config.share, &config.path);
        Ok(SmbSource {
            id: config.id.clone(),
            name: config.name.clone(),
            #[cfg(feature = "smb")]
            sessions: Arc::new(imp::Sessions::new(config, &root)),
            root,
            config: config.clone(),
        })
    }

    /// URL listed by `list(None)`.
    pub fn root(&self) -> &str {
        &self.root
    }

    /// A source for the whole server (`smb://host/`).
    fn whole_server(&self) -> bool {
        self.config.share.trim().is_empty()
    }

    fn check_same_share(&self, loc: &SmbLocation) -> Result<()> {
        let same = loc.host.eq_ignore_ascii_case(&self.config.host)
            && (self.whole_server() || loc.share.eq_ignore_ascii_case(&self.config.share))
            && loc.port.unwrap_or(445) == self.config.port.unwrap_or(445);
        if same {
            Ok(())
        } else {
            Err(Error::invalid(&loc.to_url(), "not on this source's share"))
        }
    }
}

impl Source for SmbSource {
    fn id(&self) -> &str {
        &self.id
    }

    fn name(&self) -> &str {
        &self.name
    }

    fn kind(&self) -> SourceKind {
        SourceKind::Smb
    }

    fn describe(&self) -> String {
        let what = if self.whole_server() {
            "SMB server"
        } else {
            "SMB share"
        };
        match &self.config.credentials {
            Some(c) => format!("{what} {} as {}", self.root, c.username),
            None => format!("{what} {} (guest)", self.root),
        }
    }

    fn list(&self, location: Option<&str>) -> Result<Vec<Entry>> {
        let loc = SmbLocation::parse(location.unwrap_or(&self.root))?;
        self.check_same_share(&loc)?;
        if loc.is_server() {
            let mut out: Vec<Entry> = self
                .list_shares()?
                .into_iter()
                .map(|share| {
                    let url = smb_url(&loc.host, loc.port, &share, "");
                    dir_entry(share, format!("{url}/"), None)
                })
                .collect();
            sort_entries(&mut out);
            return Ok(out);
        }
        let mut out = Vec::new();
        for item in self.list_dir(&loc)? {
            if item.name.starts_with('.') {
                continue;
            }
            let mut child = loc.clone();
            child.path.push(item.name.clone());
            let url = child.to_url();
            out.push(if item.is_dir {
                dir_entry(item.name, format!("{url}/"), item.modified)
            } else {
                file_entry(item.name, url, Some(item.size), item.modified)
            });
        }
        sort_entries(&mut out);
        Ok(out)
    }

    fn open(&self, location: &str) -> Result<Arc<dyn ByteSource>> {
        let loc = SmbLocation::parse(location)?;
        self.check_same_share(&loc)?;
        if loc.path.is_empty() {
            return Err(Error::invalid(
                location,
                "a share or server cannot be opened as a file",
            ));
        }
        self.open_file(&loc)
    }

    fn parent(&self, location: &str) -> Option<String> {
        let mut loc = SmbLocation::parse(location).ok()?;
        if loc.path.pop().is_none() {
            // A share's root goes up to the server's list of shares, when
            // the source is the whole server.
            if loc.is_server() || !self.whole_server() {
                return None;
            }
            loc.share.clear();
            return Some(loc.to_url());
        }
        Some(format!("{}/", loc.to_url()))
    }
}

/// One directory entry as the SMB layer reports it.
#[cfg_attr(not(feature = "smb"), allow(dead_code))]
struct DirItem {
    name: String,
    is_dir: bool,
    size: u64,
    modified: Option<i64>,
}

/// Whether a share is one to browse: not the server's own (`IPC$`,
/// `ADMIN$`, `C$`, `print$`: hidden ones end in `$`).
#[cfg_attr(not(feature = "smb"), allow(dead_code))]
fn browsable_share(name: &str) -> bool {
    !name.is_empty() && !name.ends_with('$')
}

#[cfg(not(feature = "smb"))]
impl SmbSource {
    fn list_shares(&self) -> Result<Vec<String>> {
        Err(Error::SmbUnavailable("compiled without SMB support".into()))
    }

    fn list_dir(&self, _loc: &SmbLocation) -> Result<Vec<DirItem>> {
        Err(Error::SmbUnavailable("compiled without SMB support".into()))
    }

    fn open_file(&self, _loc: &SmbLocation) -> Result<Arc<dyn ByteSource>> {
        Err(Error::SmbUnavailable("compiled without SMB support".into()))
    }
}

#[cfg(feature = "smb")]
mod imp {
    //! The smb-rs backed implementation.

    use super::{DirItem, SmbLocation, SmbSource};
    use crate::cache::{CacheOptions, CachedSource, RangeFetch};
    use crate::config::SmbConfig;
    use crate::error::{Error, Result};
    use fp_core::source::ByteSource;
    use fp_core::sync::lock;
    use smb::{
        Client, ClientConfig, ConnectionConfig, DirAccessMask, FileAccessMask, FileCreateArgs,
        FileDirectoryInformation, GetLen, IoctlBuffer, PipeTransceiveRequest,
        PipeTransceiveResponse, ReadAt, Status, UncPath,
    };
    use std::collections::HashSet;
    use std::io;
    use std::str::FromStr;
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    /// Seconds between 1601-01-01 and 1970-01-01.
    const FILETIME_UNIX_OFFSET: i64 = 11_644_473_600;

    /// Fallback read size when the negotiated maximum is unknown (the SMB
    /// 2.0.2 limit).
    const DEFAULT_MAX_READ: usize = 64 << 10;

    /// A client signed in to the server, with the shares it has connected
    /// (the configured one, or for a whole server each as it is used).
    pub(super) struct Session {
        client: Client,
        max_read: usize,
        /// Connected shares, lowercase.
        shares: Mutex<HashSet<String>>,
    }

    /// The share's current session, shared by the source and its open
    /// files, so a file that loses its connection reconnects for everyone.
    pub(super) struct Sessions {
        config: SmbConfig,
        root: String,
        current: Mutex<Option<Arc<Session>>>,
    }

    /// Maps an smb-rs error, keeping "not found" and "access denied"
    /// distinguishable.
    fn map_err(e: smb::Error, what: &str) -> Error {
        if let smb::Error::ReceivedErrorMessage(status, _) = &e {
            match *status {
                Status::U32_LOGON_FAILURE | Status::U32_ACCESS_DENIED => {
                    return Error::Auth(what.to_string());
                }
                Status::U32_OBJECT_NAME_NOT_FOUND
                | Status::U32_OBJECT_PATH_NOT_FOUND
                | Status::U32_BAD_NETWORK_NAME => return Error::NotFound(what.to_string()),
                _ => {}
            }
        }
        Error::Smb(format!("{what}: {e}"))
    }

    fn unc(loc: &SmbLocation) -> Result<UncPath> {
        let server = loc.host.clone();
        let share = UncPath::from_str(&format!(r"\\{server}\{}", loc.share))
            .map_err(|e| Error::invalid(&loc.to_url(), e.to_string()))?;
        let path = loc.share_path();
        Ok(if path.is_empty() {
            share
        } else {
            share.with_path(&path)
        })
    }

    impl Sessions {
        pub(super) fn new(config: &SmbConfig, root: &str) -> Sessions {
            Sessions {
                config: config.clone(),
                root: root.to_string(),
                current: Mutex::new(None),
            }
        }

        fn session(&self) -> Result<Arc<Session>> {
            let mut guard = lock(&self.current);
            if let Some(s) = guard.as_ref() {
                return Ok(s.clone());
            }
            let guest = self.config.credentials.is_none();
            let config = ClientConfig {
                connection: ConnectionConfig {
                    port: self.config.port,
                    timeout: Some(Duration::from_secs(15)),
                    allow_unsigned_guest_access: guest,
                    ..ConnectionConfig::default()
                },
                ..ClientConfig::default()
            };
            let client = Client::new(config);
            let (user, pass) = self.login();
            log::info!("connecting to {}", self.root);
            // A whole server signs in on its IPC$ share, which lists the
            // others; they connect as they are opened.
            let share = self.config.share.trim();
            if share.is_empty() {
                client
                    .ipc_connect(&self.config.host, &user, pass)
                    .map_err(|e| map_err(e, &self.root))?;
            } else {
                let loc = SmbLocation {
                    host: self.config.host.clone(),
                    port: self.config.port,
                    share: share.to_string(),
                    path: Vec::new(),
                };
                client
                    .share_connect(&unc(&loc)?, &user, pass)
                    .map_err(|e| map_err(e, &self.root))?;
            }
            let max_read = client
                .get_connection(&self.config.host)
                .ok()
                .and_then(|c| c.conn_info().map(|i| i.negotiation.max_read_size as usize))
                .filter(|n| *n >= 4096)
                .unwrap_or(DEFAULT_MAX_READ);
            let connected = Some(share.to_lowercase()).filter(|s| !s.is_empty());
            let session = Arc::new(Session {
                client,
                max_read,
                shares: Mutex::new(connected.into_iter().collect()),
            });
            *guard = Some(session.clone());
            Ok(session)
        }

        /// The user name and password to sign in with (guest without).
        fn login(&self) -> (String, String) {
            match &self.config.credentials {
                Some(c) => (c.username.clone(), c.password.clone()),
                None => ("Guest".to_string(), String::new()),
            }
        }

        /// Connects `session` to the share `path` is on, once.
        fn connect_share(&self, session: &Session, path: &UncPath, url: &str) -> Result<()> {
            let Some(share) = path.share().filter(|s| !s.is_empty()) else {
                return Ok(());
            };
            let key = share.to_lowercase();
            if lock(&session.shares).contains(&key) {
                return Ok(());
            }
            let (user, pass) = self.login();
            let root = UncPath::from_str(&format!(r"\\{}\{share}", path.server()))
                .map_err(|e| Error::invalid(url, e.to_string()))?;
            session
                .client
                .share_connect(&root, &user, pass)
                .map_err(|e| map_err(e, url))?;
            lock(&session.shares).insert(key);
            Ok(())
        }

        /// The server's shares worth browsing, by name, asked for on its
        /// `srvsvc` pipe (see [`super::srvsvc`]).
        fn list_shares(&self) -> Result<Vec<String>> {
            use super::srvsvc;
            self.with_session(|s| {
                let pipe = s
                    .client
                    .open_pipe(&self.config.host, "srvsvc")
                    .map_err(|e| map_err(e, &self.root))?;
                let ask = |pdu: Vec<u8>| -> Result<Vec<u8>> {
                    pipe.fsctl_with_options(
                        PipeTransceiveRequest::from(IoctlBuffer::from(pdu)),
                        u32::from(srvsvc::MAX_FRAG),
                    )
                    .map(|reply: PipeTransceiveResponse| reply.to_vec())
                    .map_err(|e| map_err(e, &self.root))
                };
                let listed = ask(srvsvc::bind(1)).and_then(|ack| {
                    srvsvc::bind_accepted(&ack)?;
                    srvsvc::shares(&ask(srvsvc::share_enum(2, &self.config.host))?)
                });
                if let Err(e) = pipe.close() {
                    log::debug!("{}: closing the srvsvc pipe: {e}", self.root);
                }
                Ok(listed?
                    .into_iter()
                    .filter(|share| share.is_disk() && super::browsable_share(&share.name))
                    .map(|share| share.name)
                    .collect())
            })
        }

        /// Runs `op` with a session; after a connection-level failure the
        /// session is dropped so the next call reconnects.
        fn with_session<T>(&self, op: impl Fn(&Arc<Session>) -> Result<T>) -> Result<T> {
            let session = self.session()?;
            let r = op(&session);
            if let Err(Error::Smb(_)) = &r {
                self.discard(&session);
            }
            r
        }

        /// Forgets `stale` unless it was already replaced, so the next call
        /// connects again.
        fn discard(&self, stale: &Arc<Session>) {
            let mut guard = lock(&self.current);
            if guard.as_ref().is_some_and(|s| Arc::ptr_eq(s, stale)) {
                *guard = None;
            }
        }

        /// Opens a file for reading on the current session.
        fn open_file(&self, path: &UncPath, url: &str) -> Result<OpenFile> {
            self.with_session(|s| {
                self.connect_share(s, path, url)?;
                let args = FileCreateArgs::make_open_existing(
                    FileAccessMask::new().with_generic_read(true),
                );
                let res = s
                    .client
                    .create_file(path, &args)
                    .map_err(|e| map_err(e, url))?;
                match res {
                    smb::Resource::File(f) => Ok(OpenFile {
                        session: s.clone(),
                        file: Arc::new(Handle(Some(f))),
                        generation: 0,
                    }),
                    _ => Err(Error::invalid(url, "not a file")),
                }
            })
        }
    }

    impl SmbSource {
        fn with_session<T>(&self, op: impl Fn(&Arc<Session>) -> Result<T>) -> Result<T> {
            self.sessions.with_session(op)
        }

        pub(super) fn list_shares(&self) -> Result<Vec<String>> {
            self.sessions.list_shares()
        }

        pub(super) fn list_dir(&self, loc: &SmbLocation) -> Result<Vec<DirItem>> {
            let path = unc(loc)?;
            let url = loc.to_url();
            self.with_session(|s| {
                self.sessions.connect_share(s, &path, &url)?;
                let access = DirAccessMask::new()
                    .with_list_directory(true)
                    .with_synchronize(true);
                let args = FileCreateArgs::make_open_existing(access.into());
                let res = s
                    .client
                    .create_file(&path, &args)
                    .map_err(|e| map_err(e, &url))?;
                let smb::Resource::Directory(dir) = res else {
                    return Err(Error::invalid(&url, "not a folder"));
                };
                let mut out = Vec::new();
                for info in dir
                    .query::<FileDirectoryInformation>("*")
                    .map_err(|e| map_err(e, &url))?
                {
                    let info = info.map_err(|e| map_err(e, &url))?;
                    let name = info.file_name.to_string();
                    if name == "." || name == ".." {
                        continue;
                    }
                    let modified = (!info.last_write_time.is_zero()).then(|| {
                        info.last_write_time.since_epoch().as_secs() as i64 - FILETIME_UNIX_OFFSET
                    });
                    out.push(DirItem {
                        name,
                        is_dir: info.file_attributes.directory(),
                        size: info.end_of_file,
                        modified,
                    });
                }
                if let Err(e) = dir.close() {
                    log::debug!("{url}: closing the folder: {e}");
                }
                Ok(out)
            })
        }

        pub(super) fn open_file(&self, loc: &SmbLocation) -> Result<Arc<dyn ByteSource>> {
            let path = unc(loc)?;
            let url = loc.to_url();
            let open = self.sessions.open_file(&path, &url)?;
            let size = open.file.get_len().map_err(|e| map_err(e, &url))?;
            let fetcher = SmbFetcher {
                sessions: self.sessions.clone(),
                path,
                open: Mutex::new(open),
                size,
                url,
            };
            Ok(Arc::new(CachedSource::new(
                fetcher,
                &CacheOptions::default(),
            )))
        }
    }

    /// An SMB file that is closed on the server once nothing uses it. smb-rs
    /// closes handles only when asked, so every video played (and every
    /// handle replaced by a reconnect) stayed open on the NAS until
    /// FramePlayerX disconnected. The close goes out on a thread of its own:
    /// it waits for the server, on a dead connection until the request
    /// times out, and the last reference can go on a player's close path.
    struct Handle(Option<smb::File>);

    impl std::ops::Deref for Handle {
        type Target = smb::File;

        fn deref(&self) -> &smb::File {
            self.0.as_ref().expect("SMB handle used after close")
        }
    }

    impl Drop for Handle {
        fn drop(&mut self) {
            let Some(file) = self.0.take() else {
                return;
            };
            let spawned = std::thread::Builder::new()
                .name("fp-smb-close".into())
                .spawn(move || {
                    if let Err(e) = file.close() {
                        log::debug!("closing an SMB file: {e}");
                    }
                });
            if let Err(e) = spawned {
                log::warn!("cannot start a thread to close an SMB file: {e}");
            }
        }
    }

    /// A file handle and the session it belongs to.
    #[derive(Clone)]
    struct OpenFile {
        session: Arc<Session>,
        file: Arc<Handle>,
        /// Counts reopens, so threads that fail together reconnect once.
        generation: u64,
    }

    impl OpenFile {
        fn read(&self, out: &mut [u8], offset: u64) -> smb::Result<usize> {
            let mut done = 0;
            while done < out.len() {
                let chunk = (out.len() - done).min(self.session.max_read);
                let n = self
                    .file
                    .read_at(&mut out[done..done + chunk], offset + done as u64)?;
                if n == 0 {
                    break;
                }
                done += n;
            }
            Ok(done)
        }
    }

    /// Lets go of a handle on a dead connection without waiting: the last
    /// reference to a session logs off and disconnects, which on a dead
    /// connection blocks until the requests time out.
    fn release(open: OpenFile) {
        let spawned = std::thread::Builder::new()
            .name("fp-smb-release".into())
            .spawn(move || drop(open));
        if let Err(e) = spawned {
            log::warn!("cannot start a thread to release an SMB handle: {e}");
        }
    }

    /// Reads ranges of one open SMB file.
    struct SmbFetcher {
        sessions: Arc<Sessions>,
        path: UncPath,
        open: Mutex<OpenFile>,
        size: u64,
        url: String,
    }

    impl SmbFetcher {
        /// Replaces the handle of generation `failed` with one on a new
        /// connection; when another thread already did, returns its handle.
        fn reopen(&self, failed: &OpenFile) -> Result<OpenFile> {
            let mut open = lock(&self.open);
            if open.generation != failed.generation {
                return Ok(open.clone());
            }
            self.sessions.discard(&failed.session);
            let mut fresh = self.sessions.open_file(&self.path, &self.url)?;
            fresh.generation = failed.generation + 1;
            release(std::mem::replace(&mut *open, fresh.clone()));
            Ok(fresh)
        }
    }

    impl RangeFetch for SmbFetcher {
        fn fetch(&self, offset: u64, len: usize) -> io::Result<Vec<u8>> {
            if offset >= self.size {
                return Ok(Vec::new());
            }
            let len = len.min((self.size - offset) as usize);
            let mut out = vec![0u8; len];
            let open = lock(&self.open).clone();
            let done = match open.read(&mut out, offset) {
                Ok(n) => n,
                Err(e) => {
                    // A NAS that drops the connection, or a request that
                    // timed out (the connection is then unusable), must not
                    // end playback: reconnect and read again, once.
                    log::warn!("{} at {offset}: {e}; reconnecting", self.url);
                    let fresh = self.reopen(&open);
                    release(open);
                    let fresh = fresh.map_err(|e| io::Error::other(e.to_string()))?;
                    fresh
                        .read(&mut out, offset)
                        .map_err(|e| io::Error::other(format!("{}: {e}", self.url)))?
                }
            };
            out.truncate(done);
            Ok(out)
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
    use crate::config::Credentials;

    #[test]
    fn urls_roundtrip() {
        assert_eq!(smb_url("nas", None, "media", ""), "smb://nas/media");
        assert_eq!(
            smb_url("nas", Some(4445), "My Share", "/vr/Clip #1.mp4"),
            "smb://nas:4445/My%20Share/vr/Clip%20%231.mp4"
        );
        assert_eq!(smb_url("fe80::1", Some(445), "s", ""), "smb://[fe80::1]/s");
        let l = SmbLocation::parse("smb://u:pw@nas:4445/My%20Share/vr/Clip%20%231.mp4").unwrap();
        assert_eq!(l.host, "nas");
        assert_eq!(l.port, Some(4445));
        assert_eq!(l.share, "My Share");
        assert_eq!(l.path, ["vr", "Clip #1.mp4"]);
        assert_eq!(l.share_path(), "vr\\Clip #1.mp4");
        assert_eq!(l.to_url(), "smb://nas:4445/My%20Share/vr/Clip%20%231.mp4");
        let v6 = SmbLocation::parse("SMB://[fe80::1]:139/s/").unwrap();
        assert_eq!((v6.host.as_str(), v6.port), ("fe80::1", Some(139)));
        assert!(v6.path.is_empty());
        // The server itself.
        for url in ["smb://nas", "smb://nas/", "smb://192.168.0.10/"] {
            let server = SmbLocation::parse(url).unwrap();
            assert!(server.is_server() && server.path.is_empty(), "{url}");
        }
        assert_eq!(smb_url("192.168.0.10", None, "", ""), "smb://192.168.0.10/");
        assert!(SmbLocation::parse("smb:///s").is_err());
        assert!(SmbLocation::parse("http://nas/s").is_err());
        assert!(SmbLocation::parse("smb://nas/s/../x").is_err());
        assert!(SmbLocation::parse("smb://nas:x/s").is_err());
    }

    fn config() -> SmbConfig {
        SmbConfig {
            id: "s".into(),
            name: "NAS".into(),
            host: "nas.local".into(),
            port: None,
            share: "media".into(),
            path: "vr/new".into(),
            credentials: Some(Credentials::new("bob", "hunter2")),
        }
    }

    #[test]
    fn source_basics_without_network() {
        let s = SmbSource::new(&config());
        if !cfg!(feature = "smb") {
            assert!(matches!(s, Err(Error::SmbUnavailable(_))));
            return;
        }
        let s = s.unwrap();
        assert_eq!(s.root(), "smb://nas.local/media/vr/new");
        assert!(!s.describe().contains("hunter2"));
        assert!(!format!("{s:?}").contains("hunter2"));
        assert_eq!(
            s.parent("smb://nas.local/media/vr/new/a.mp4").as_deref(),
            Some("smb://nas.local/media/vr/new/")
        );
        assert_eq!(s.parent("smb://nas.local/media"), None);
        assert!(matches!(
            s.list(Some("smb://other/media/")),
            Err(Error::InvalidLocation { .. })
        ));
        assert!(matches!(
            s.open("smb://nas.local/media"),
            Err(Error::InvalidLocation { .. })
        ));
        let mut bad = config();
        bad.host.clear();
        assert!(matches!(SmbSource::new(&bad), Err(Error::Config(_))));
    }

    #[test]
    fn a_whole_server_lists_its_shares_and_reaches_into_any() {
        let mut c = config();
        c.share.clear();
        c.path.clear();
        let s = SmbSource::new(&c);
        if !cfg!(feature = "smb") {
            assert!(matches!(s, Err(Error::SmbUnavailable(_))));
            return;
        }
        let s = s.unwrap();
        assert_eq!(s.root(), "smb://nas.local/");
        assert!(s.describe().starts_with("SMB server"));
        // Any share on it is the source's; another server isn't.
        assert!(
            s.check_same_share(&SmbLocation::parse("smb://nas.local/tv/a").unwrap())
                .is_ok()
        );
        assert!(
            s.check_same_share(&SmbLocation::parse("smb://other/tv/a").unwrap())
                .is_err()
        );
        // Up from a share's root is the list of shares; nothing above that.
        assert_eq!(
            s.parent("smb://nas.local/tv/").as_deref(),
            Some("smb://nas.local/")
        );
        assert_eq!(
            s.parent("smb://nas.local/tv/shows/").as_deref(),
            Some("smb://nas.local/tv/")
        );
        assert_eq!(s.parent("smb://nas.local/"), None);
        assert!(matches!(
            s.open("smb://nas.local/"),
            Err(Error::InvalidLocation { .. })
        ));
        // The server's own shares are left out.
        let shown: Vec<&str> = ["media", "IPC$", "ADMIN$", "C$", "print$", "tv"]
            .into_iter()
            .filter(|n| browsable_share(n))
            .collect();
        assert_eq!(shown, ["media", "tv"]);
    }

    #[cfg(feature = "smb")]
    #[test]
    fn unreachable_server_is_an_error_not_a_hang() {
        let mut c = config();
        // Port 9 on localhost: connection refused straight away.
        c.host = "127.0.0.1".into();
        c.port = Some(9);
        let s = SmbSource::new(&c).unwrap();
        let err = s.list(None).unwrap_err();
        assert!(matches!(err, Error::Smb(_)), "{err:?}");
        assert!(!err.to_string().contains("hunter2"));
    }
}
