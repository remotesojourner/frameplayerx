//! The in-headset interface. Each panel's UI is a function of the app
//! state; anything that changes more than view-local state is returned as
//! an [`Action`] for the app to apply.

pub mod controller_map;
pub mod crash;
pub mod depth;
pub mod feeds;
pub mod folders;
pub mod icons;
pub mod jellyfin;
pub mod keyboard;
pub mod library;
pub mod local;
pub mod page;
pub mod perf;
pub mod player;
pub mod playlists;
pub mod premium;
pub mod queue;
pub mod settings;
pub mod sources;
pub mod subtitle_text;
pub mod theme;
pub mod thumbs;
pub mod widgets;

use crate::playback::OpenRequest;
use crate::settings::HapticDeviceConfig;
use fp_core::format::VideoFormat;
use fp_core::source::Entry;
use fp_library::{MediaId, PlaylistId, Query};
use fp_sources::SourceConfig;

/// `h:mm:ss` or `m:ss`.
pub fn fmt_time(t: f64) -> String {
    let t = if t.is_finite() { t.max(0.0) as u64 } else { 0 };
    let (h, m, s) = (t / 3600, (t / 60) % 60, t % 60);
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m}:{s:02}")
    }
}

pub fn fmt_size(bytes: u64) -> String {
    let b = bytes as f64;
    if b >= 1e9 {
        format!("{:.1} GB", b / 1e9)
    } else if b >= 1e6 {
        format!("{:.0} MB", b / 1e6)
    } else {
        format!("{:.0} kB", b / 1e3)
    }
}

/// A Unix time as a short date ("12 Oct 2026").
pub fn fmt_date(unix: i64) -> String {
    const MONTHS: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    let (y, m, d) = fp_sources::timeutil::civil_from_days(unix.div_euclid(86_400));
    format!("{d} {} {y}", MONTHS[(m - 1) as usize])
}

/// Something the UI asks the app to do.
#[derive(Clone, Debug)]
pub enum Action {
    Open(OpenRequest),
    /// Open item `.1` of a list; grip + thumbstick moves through the list.
    OpenList(Vec<OpenRequest>, usize),
    /// Plays the list from `.1`, through to its end (Play all, a playlist).
    PlayAll(Vec<OpenRequest>, usize),
    /// Queues a video right after the one playing (`next`) or at the end.
    Queue {
        req: OpenRequest,
        next: bool,
    },
    /// Plays the one at this place in Up next.
    QueueJump(usize),
    QueueRemove(usize),
    QueueMove(usize, usize),
    /// Keeps only the video playing in Up next.
    QueueClear,
    /// Shows Up next in the browse panel over the video.
    ShowUpNext,
    /// Adds videos to a playlist, or to a new one named `name`.
    AddToPlaylist {
        playlist: Option<PlaylistId>,
        name: String,
        reqs: Vec<OpenRequest>,
    },
    RenamePlaylist(PlaylistId, String),
    DeletePlaylist(PlaylistId),
    /// Takes out the entry at this position.
    PlaylistRemove(PlaylistId, usize),
    PlaylistMove(PlaylistId, usize, usize),
    TogglePause,
    Seek(f64),
    SeekRelative(f64),
    SetSpeed(f64),
    SetVolume(f32),
    ClosePlayback,
    /// Override the format of the playing video (`None` = detected).
    SetFormat(Option<VideoFormat>),
    SaveView,
    /// The Passthrough tab changed this video's chroma key: saved at once.
    SaveKey,
    ResetView,
    AddKeyframe,
    ClearKeyframes,
    AddBookmark,
    SelectAudio(usize),
    SelectSubtitleStream(Option<usize>),
    SelectSubtitleFile(Option<usize>),
    /// Puts a flat screen, the control bar, the adjust panel and the browse
    /// panel back where they start.
    ResetPlacement,
    SetFavorite(MediaId, bool),
    SetRating(MediaId, u8),
    SetUserFormat(MediaId, Option<VideoFormat>),
    RemoveFromHistory(MediaId),
    /// Scans a library now; `force` makes its thumbnails again.
    Rescan {
        source: String,
        force: bool,
    },
    /// Tries a library's videos that couldn't be read again.
    RetryFailed(String),
    ClearHistory,
    AddSource(SourceConfig),
    /// Saves an edited source (the same id).
    UpdateSource(SourceConfig),
    RemoveSource(String),
    /// Unlock a source with its PIN (the form says when it is wrong).
    UnlockSource {
        source: String,
        pin: String,
    },
    /// Lock a source with a PIN again.
    LockSource(String),
    /// Give a source a PIN, or take it away (`None`).
    SetSourcePin {
        source: String,
        pin: Option<String>,
    },
    /// Opens a source in the browser its kind gets; leaving it returns to
    /// `back_to`.
    OpenSource {
        source: String,
        back_to: Screen,
    },
    /// Lists a folder of a source.
    Browse {
        source: String,
        location: Option<String>,
    },
    /// Loads a page of a Jellyfin/Emby server; the answer goes to the page
    /// that asked (`seq`).
    Jellyfin {
        source: String,
        seq: u64,
        load: jellyfin::Load,
    },
    /// Marks a Jellyfin/Emby item as a favourite, or as watched, on the
    /// server.
    JellyfinMark {
        source: String,
        id: String,
        mark: jellyfin::Mark,
        on: bool,
    },
    /// Plays a Jellyfin/Emby video from one of its versions (a media
    /// source id) from now on; `None` from the tallest that fits.
    JellyfinVersion {
        source: String,
        location: String,
        version: Option<String>,
    },
    /// Reads a DeoVR feed's or HereSphere API's groups and scenes.
    FeedLoad {
        source: String,
    },
    /// Fetches feed scenes' details: `front` (cards on screen) before those
    /// already waiting.
    FeedFill {
        source: String,
        urls: Vec<String>,
        front: bool,
    },
    /// Fetches everything about one scene, for its details page.
    FeedScene {
        source: String,
        url: String,
    },
    /// Marks or unmarks a feed scene as a favourite on the server.
    FeedFavorite {
        source: String,
        url: String,
        on: bool,
    },
    DiscoverDlna,
    /// Request a Quick Connect code from a Jellyfin/Emby server.
    QuickConnectInitiate {
        address: String,
        insecure_tls: bool,
    },
    /// Poll for Quick Connect authorization.
    QuickConnectPoll {
        address: String,
        secret: String,
        insecure_tls: bool,
    },
    AddDevice(HapticDeviceConfig),
    RemoveDevice(usize),
    ReconnectDevices,
    RegenerateToken,
    CheckUpdates,
    InstallUpdate,
    /// Send the crash report the start offered, or don't.
    SendCrashReport,
    DismissCrashReport,
    Recenter,
    TogglePassthrough,
    /// Settings › General › Screen follows you.
    ToggleFollow,
    /// Show the library/browser panel (during playback).
    ShowBrowser(bool),
    /// Hide the player controls (the trigger on empty space brings them
    /// back).
    HideControls,
    Quit,
    /// Quit and start again, to run an installed update.
    Restart,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Screen {
    Home,
    /// Everything watched, searchable and sortable.
    History,
    Sources,
    Settings,
    /// Your playlists, and one of them.
    Playlists,
    /// The queue, opened from the control bar.
    UpNext,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SettingsTab {
    General,
    Playback,
    VideoPosition,
    /// Sound and subtitles.
    Sound,
    Passthrough,
    Controller,
    Haptics,
    Remote,
    Updates,
    About,
}

/// Format filter chips in the library.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FormatFilter {
    All,
    Flat,
    Vr180,
    Vr360,
    Fisheye,
}

/// State of a folder listing.
#[derive(Default)]
pub struct Browse {
    pub source: String,
    pub location: Option<String>,
    /// Parent locations, for the back button.
    pub stack: Vec<Option<String>>,
    pub entries: Vec<Entry>,
    pub loading: bool,
    pub error: Option<String>,
    /// Words the names shown must contain.
    pub filter: String,
}

/// A source being browsed, in the browser its kind gets.
pub struct SourceView {
    pub source: String,
    pub name: String,
    /// Where leaving it goes.
    pub back_to: Screen,
    pub page: SourcePage,
}

pub enum SourcePage {
    /// Files and folders (SMB, WebDAV, HTTP, DLNA, local folders).
    Folders(Browse),
    /// The headset's folders and drives.
    Local(local::LocalView),
    /// A library's videos, and its folders.
    Library(local::LibraryView),
    /// A Jellyfin/Emby server's libraries, series and films.
    Jellyfin(jellyfin::JellyfinView),
    /// A DeoVR feed's or HereSphere API's scenes.
    Feed(feeds::FeedView),
}

/// What going back in a source browser did.
pub enum Back {
    /// Stepped back inside the browser.
    Done,
    /// Back to a parent folder, which lists again.
    Parent(Box<Action>),
    /// Left the browser.
    Leave,
}

impl SourceView {
    /// The folder listing on screen, if any.
    pub fn folder_mut(&mut self) -> Option<&mut Browse> {
        match &mut self.page {
            SourcePage::Folders(b) => Some(b),
            SourcePage::Local(l) => l.folder.as_mut(),
            SourcePage::Library(l) if l.folders => l.folder.as_mut(),
            _ => None,
        }
    }

    /// One step back: the parent folder, the previous page, or out.
    pub fn back(&mut self) -> Back {
        let parent = |b: &mut Browse| {
            b.stack.pop().map(|parent| {
                b.location = parent.clone();
                b.loading = true;
                b.filter.clear();
                Back::Parent(Box::new(Action::Browse {
                    source: b.source.clone(),
                    location: parent,
                }))
            })
        };
        match &mut self.page {
            SourcePage::Folders(b) => parent(b).unwrap_or(Back::Leave),
            SourcePage::Local(l) => match l.folder.as_mut() {
                Some(b) => parent(b).unwrap_or_else(|| {
                    l.folder = None;
                    Back::Done
                }),
                None => Back::Leave,
            },
            // Up the folders, then to the videos, then out.
            SourcePage::Library(l) if l.folders => match l.folder.as_mut() {
                Some(b) => parent(b).unwrap_or_else(|| {
                    l.folders = false;
                    Back::Done
                }),
                None => {
                    l.folders = false;
                    Back::Done
                }
            },
            SourcePage::Jellyfin(j) if j.pages.len() > 1 => {
                j.pages.pop();
                Back::Done
            }
            SourcePage::Feed(f) if f.details.is_some() => {
                f.details = None;
                Back::Done
            }
            _ => Back::Leave,
        }
    }
}

/// A searchable, filterable, sortable grid of library videos: the watch
/// history, or the videos on the headset.
pub struct LibraryGrid {
    /// What every query keeps (watched only, some sources).
    pub base: Query,
    pub search: String,
    pub sort: fp_library::Sort,
    pub favorites_only: bool,
    pub format_filter: FormatFilter,
    pub stereo_only: bool,
    pub page_size: usize,
    /// Results; `None` when they must be re-queried.
    pub results: Option<Vec<fp_library::MediaRecord>>,
    pub total: usize,
}

impl LibraryGrid {
    pub fn new(base: Query, sort: fp_library::Sort) -> LibraryGrid {
        LibraryGrid {
            base,
            search: String::new(),
            sort,
            favorites_only: false,
            format_filter: FormatFilter::All,
            stereo_only: false,
            page_size: 48,
            results: None,
            total: 0,
        }
    }

    /// Everything played, most recent first.
    pub fn history() -> LibraryGrid {
        let base = Query {
            watched_only: true,
            ..Default::default()
        };
        LibraryGrid::new(base, fp_library::Sort::LastPlayed)
    }

    /// A library's videos, newest first.
    pub fn of(source: &str) -> LibraryGrid {
        let base = Query {
            sources: vec![source.to_string()],
            ..Default::default()
        };
        LibraryGrid::new(base, fp_library::Sort::Added)
    }

    pub fn query(&self) -> Query {
        use fp_library::ProjectionKind as P;
        let projections = match self.format_filter {
            FormatFilter::All => vec![],
            FormatFilter::Flat => vec![P::Flat],
            FormatFilter::Vr180 => vec![P::Equirect180],
            FormatFilter::Vr360 => vec![P::Equirect360, P::Eac],
            FormatFilter::Fisheye => vec![P::Fisheye],
        };
        Query {
            text: self.search.trim().to_string(),
            favorites_only: self.favorites_only,
            projections,
            stereo: self.stereo_only.then_some(fp_library::StereoFilter::Stereo),
            sort: if self.search.trim().is_empty() {
                self.sort
            } else {
                fp_library::Sort::Relevance
            },
            limit: Some(self.page_size),
            ..self.base.clone()
        }
    }
}

/// The PIN form of the Sources page.
#[derive(Default)]
pub struct PinForm {
    pub source: String,
    /// The source's name, for the title.
    pub name: String,
    /// Setting a new PIN (entered twice); else unlocking.
    pub set: bool,
    pub pin: String,
    pub confirm: String,
    /// The last PIN entered was wrong.
    pub wrong: bool,
}

/// Add-source form, also for editing one.
pub struct SourceForm {
    /// The source being edited (its id and what the form doesn't show are
    /// kept); `None` when adding.
    pub editing: Option<SourceConfig>,
    pub kind: fp_sources::SourceKind,
    pub name: String,
    pub address: String,
    pub username: String,
    pub password: String,
    pub insecure_tls: bool,
    /// Signing in to a Jellyfin/Emby server with Quick Connect rather than a
    /// password.
    pub use_quick_connect: bool,
    pub quick_connect: QuickConnect,
}

/// Signing in to a Jellyfin/Emby server with Quick Connect.
#[derive(Clone, Debug, Default, PartialEq)]
pub enum QuickConnect {
    #[default]
    Idle,
    /// Asking the server for a pairing code.
    Requesting,
    /// Showing `code` until it is approved in another app; the server is
    /// asked with `secret` every few seconds.
    Waiting {
        code: String,
        secret: String,
        polled: std::time::Instant,
    },
    Authorized {
        token: String,
        user_id: Option<String>,
    },
    Failed(String),
}

impl Default for SourceForm {
    fn default() -> Self {
        SourceForm {
            editing: None,
            kind: fp_sources::SourceKind::MediaBrowser,
            name: String::new(),
            address: String::new(),
            username: String::new(),
            password: String::new(),
            insecure_tls: false,
            use_quick_connect: true,
            quick_connect: QuickConnect::Idle,
        }
    }
}

/// Status of the update check, shown in settings.
#[derive(Default, Clone, Debug)]
pub enum UpdateStatus {
    #[default]
    Unknown,
    Checking,
    UpToDate,
    Available {
        version: String,
        notes: String,
    },
    Downloading {
        done: u64,
        total: u64,
    },
    Installed {
        version: String,
    },
    Failed(String),
}

/// A remappable controller input, by hand (see [`crate::bindings`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RemapSlot {
    Button(usize, crate::bindings::Button),
    Axis(usize, crate::bindings::Axis),
}

/// Videos being added to a playlist, and what the picker calls them.
pub struct Picking {
    pub reqs: Vec<OpenRequest>,
    pub title: String,
}

/// View-local UI state.
pub struct UiState {
    pub screen: Screen,
    pub settings_tab: SettingsTab,
    /// The controller input whose choices are open in Settings > Controller.
    pub remap_open: Option<RemapSlot>,
    /// The subtitle languages are listed in Settings › Sound and subtitles.
    pub languages_open: bool,
    pub history: LibraryGrid,
    /// "Clear history" was pressed once: pressing again within a few
    /// seconds clears it.
    pub clear_armed: Option<std::time::Instant>,
    pub home: Option<HomeRows>,
    pub details: Option<MediaId>,
    /// The playlist open on the Playlists screen.
    pub playlist: Option<PlaylistId>,
    /// The playlists, and the open one's videos, as last read.
    pub playlists: Option<Vec<fp_library::Playlist>>,
    pub playlist_items: Option<(PlaylistId, Vec<fp_library::PlaylistEntry>)>,
    /// A new playlist's name, or the open one's while it is renamed.
    pub playlist_name: String,
    pub renaming: bool,
    /// Delete was pressed once on the open playlist: pressing again within
    /// a few seconds deletes it.
    pub delete_armed: Option<std::time::Instant>,
    /// Videos being added to a playlist (the picker shows instead of the
    /// screen).
    pub picking: Option<Picking>,
    /// Up next as last shown, with the queue's version it was made from.
    pub up_next: Option<(u64, Vec<queue::Row>)>,
    /// The source being browsed (shown on the Sources screen).
    pub source: Option<SourceView>,
    /// Counts requests to sources, so answers reach the page that asked.
    pub seq: u64,
    pub source_form: Option<SourceForm>,
    /// Entering a source's PIN (to unlock it, or to set one).
    pub pin_form: Option<PinForm>,
    /// Sources with a PIN that were unlocked since FramePlayerX started.
    pub unlocked_sources: std::collections::HashSet<String>,
    pub dlna_found: Vec<fp_sources::DlnaDevice>,
    pub dlna_searching: bool,
    pub new_device: String,
    pub new_device_kind: usize,
    /// Libraries being scanned, with how far each got.
    pub scans: std::collections::BTreeMap<String, String>,
    pub worker_status: Option<String>,
    pub update: UpdateStatus,
    pub adjust_open: bool,
    pub adjust_tab: usize,
    /// Subtitles in other languages are listed in the Audio & subs tab.
    pub foreign_subtitles_open: bool,
    /// The last run crashed: asks whether to send a report.
    pub crash: Option<crash::CrashPrompt>,
    pub open_error: Option<String>,
    pub opening: Option<String>,
    pub toast: Option<(String, std::time::Instant)>,
    /// Hide the seek-preview while scrubbing has not moved.
    pub scrub: Option<f64>,
    /// The volume before muting from the control bar.
    pub unmute_volume: Option<f32>,
    pub keyboard_shift: bool,
    /// The keyboard shows its symbols page.
    pub keyboard_symbols: bool,
    /// Facts for the About tab (runtime, GPU, decoder...).
    pub about: Vec<(String, String)>,
}

pub struct HomeRows {
    pub continue_watching: Vec<fp_library::MediaRecord>,
    /// The last video watched, for the banner when nothing is half-watched.
    pub last_watched: Option<fp_library::MediaRecord>,
    pub favorites: Vec<fp_library::MediaRecord>,
}

impl Default for UiState {
    fn default() -> Self {
        UiState {
            screen: Screen::Home,
            settings_tab: SettingsTab::General,
            remap_open: None,
            languages_open: false,
            history: LibraryGrid::history(),
            clear_armed: None,
            home: None,
            details: None,
            playlist: None,
            playlists: None,
            playlist_items: None,
            playlist_name: String::new(),
            renaming: false,
            delete_armed: None,
            picking: None,
            up_next: None,
            source: None,
            seq: 0,
            source_form: None,
            pin_form: None,
            unlocked_sources: Default::default(),
            dlna_found: Vec::new(),
            dlna_searching: false,
            new_device: String::new(),
            new_device_kind: 0,
            scans: Default::default(),
            worker_status: None,
            update: UpdateStatus::Unknown,
            adjust_open: false,
            adjust_tab: 0,
            foreign_subtitles_open: false,
            crash: None,
            open_error: None,
            opening: None,
            toast: None,
            scrub: None,
            unmute_volume: None,
            keyboard_shift: false,
            keyboard_symbols: false,
            about: Vec::new(),
        }
    }
}

impl UiState {
    /// Library changed: re-query on the next paint.
    pub fn invalidate(&mut self) {
        self.history.results = None;
        self.home = None;
        self.playlists = None;
        self.playlist_items = None;
        self.up_next = None;
        if let Some(SourceView {
            page: SourcePage::Library(l),
            ..
        }) = self.source.as_mut()
        {
            l.grid.results = None;
        }
    }

    pub fn toast(&mut self, text: impl Into<String>) {
        self.toast = Some((text.into(), std::time::Instant::now()));
    }

    /// A new request number (see [`UiState::seq`]).
    pub fn next_seq(&mut self) -> u64 {
        self.seq += 1;
        self.seq
    }

    /// The folder listing on screen, if any.
    pub fn folder_mut(&mut self) -> Option<&mut Browse> {
        self.source.as_mut().and_then(SourceView::folder_mut)
    }

    /// The Jellyfin/Emby page of `source`, if that is what is open.
    pub fn jellyfin_page(&mut self, source: &str) -> Option<&mut jellyfin::JellyfinView> {
        match self.source.as_mut()? {
            SourceView {
                source: s,
                page: SourcePage::Jellyfin(j),
                ..
            } if s == source => Some(j),
            _ => None,
        }
    }

    /// The scene feed page of `source`, if that is what is open.
    pub fn feed_page(&mut self, source: &str) -> Option<&mut feeds::FeedView> {
        match self.source.as_mut()? {
            SourceView {
                source: s,
                page: SourcePage::Feed(f),
                ..
            } if s == source => Some(f),
            _ => None,
        }
    }

    /// One step back in the source browser; leaving it returns to the
    /// screen it was opened from. Returns what must list again.
    pub fn source_back(&mut self) -> Option<Action> {
        let sv = self.source.as_mut()?;
        match sv.back() {
            Back::Done => None,
            Back::Parent(a) => Some(*a),
            Back::Leave => {
                self.screen = sv.back_to;
                self.source = None;
                if self.screen == Screen::Home {
                    self.home = None;
                }
                None
            }
        }
    }
}

/// Everything a panel's UI may read or change.
pub struct View<'a> {
    pub state: &'a mut UiState,
    pub queue: &'a crate::queue::Queue,
    pub settings: &'a mut crate::settings::Settings,
    pub services: &'a crate::services::Services,
    pub thumbs: &'a mut thumbs::Thumbs,
    pub playback: Option<&'a mut crate::playback::Playback>,
    pub actions: &'a mut Vec<Action>,
    pub passthrough_available: bool,
    pub devices: &'a [fp_haptics::DeviceStatus],
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn times() {
        assert_eq!(fmt_time(0.0), "0:00");
        assert_eq!(fmt_time(65.4), "1:05");
        assert_eq!(fmt_time(3600.0 + 61.0), "1:01:01");
        assert_eq!(fmt_time(f64::NAN), "0:00");
        assert_eq!(fmt_size(1_500_000_000), "1.5 GB");
        assert_eq!(fmt_date(0), "1 Jan 1970");
        assert_eq!(fmt_date(1_791_763_200), "12 Oct 2026");
    }

    #[test]
    fn back_climbs_folders_then_leaves_the_source() {
        let mut s = UiState {
            screen: Screen::Sources,
            ..Default::default()
        };
        s.source = Some(SourceView {
            source: "nas".into(),
            name: "NAS".into(),
            back_to: Screen::Home,
            page: SourcePage::Folders(Browse {
                source: "nas".into(),
                location: Some("smb://nas/v/a".into()),
                stack: vec![None, Some("smb://nas/v".into())],
                filter: "beach".into(),
                ..Default::default()
            }),
        });
        match s.source_back() {
            Some(Action::Browse { source, location }) => {
                assert_eq!(source, "nas");
                assert_eq!(location.as_deref(), Some("smb://nas/v"));
            }
            other => panic!("{other:?}"),
        }
        assert!(
            s.folder_mut()
                .is_some_and(|b| b.filter.is_empty() && b.loading)
        );
        assert!(matches!(
            s.source_back(),
            Some(Action::Browse { location: None, .. })
        ));
        assert!(s.source_back().is_none());
        assert!(s.source.is_none());
        assert_eq!(s.screen, Screen::Home, "back to where it was opened");
    }

    #[test]
    fn back_closes_inner_pages_first() {
        let mut s = UiState::default();
        let local = local::LocalView {
            folder: Some(Browse {
                source: crate::services::HOME_SOURCE.into(),
                ..Default::default()
            }),
        };
        s.source = Some(SourceView {
            source: crate::services::LOCAL_HUB.into(),
            name: "On this device".into(),
            back_to: Screen::Sources,
            page: SourcePage::Local(local),
        });
        // The folder closes, back to the headset's page...
        assert!(s.source_back().is_none());
        assert!(s.source.is_some() && s.folder_mut().is_none());
        // ...then the page itself.
        assert!(s.source_back().is_none());
        assert!(s.source.is_none());

        // A library's folders: up to its top, then its videos, then out.
        let mut lib = local::LibraryView::new("movies");
        lib.folders = true;
        lib.folder = Some(Browse {
            source: "movies".into(),
            location: Some("/v/Movies/Old".into()),
            stack: vec![None],
            ..Default::default()
        });
        s.source = Some(SourceView {
            source: "movies".into(),
            name: "Movies".into(),
            back_to: Screen::Sources,
            page: SourcePage::Library(lib),
        });
        assert!(matches!(
            s.source_back(),
            Some(Action::Browse { location: None, .. })
        ));
        assert!(s.source_back().is_none());
        assert!(s.source.is_some() && s.folder_mut().is_none(), "its videos");
        assert!(s.source_back().is_none());
        assert!(s.source.is_none());

        let mut j = jellyfin::JellyfinView::new(1);
        j.pages.push(jellyfin::Page {
            seq: 2,
            loading: false,
            error: None,
            body: jellyfin::Body::Item {
                item: Box::default(),
            },
        });
        s.source = Some(SourceView {
            source: "jf".into(),
            name: "Jellyfin".into(),
            back_to: Screen::Sources,
            page: SourcePage::Jellyfin(j),
        });
        assert!(s.source_back().is_none());
        assert!(matches!(
            s.source.as_ref().map(|v| &v.page),
            Some(SourcePage::Jellyfin(j)) if j.pages.len() == 1
        ));
    }

    #[test]
    fn grids_keep_their_base_query() {
        let mut g = LibraryGrid::history();
        g.search = "beach".into();
        g.format_filter = FormatFilter::Vr180;
        let q = g.query();
        assert!(q.watched_only);
        assert_eq!(q.text, "beach");
        assert_eq!(q.sort, fp_library::Sort::Relevance);
        assert_eq!(q.projections, [fp_library::ProjectionKind::Equirect180]);
        let movies = LibraryGrid::of("movies").query();
        assert!(!movies.watched_only);
        assert_eq!(movies.sources, ["movies"]);
        assert_eq!(movies.sort, fp_library::Sort::Added);
    }
}
