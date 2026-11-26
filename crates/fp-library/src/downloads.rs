//! Videos downloaded to the headset or a card: one row per video, beside
//! its media row, so a download shares the video's resume point, history
//! and adjustments. Where the files go and how they get there is the app's;
//! this keeps what it needs to find them again.

use std::collections::HashSet;

use fp_core::source::SubtitleFile;
use rusqlite::types::Type;
use rusqlite::{Connection, OptionalExtension, Row, params};

use crate::library::{affected, hidden_sql};
use crate::record::now;
use crate::{Library, MediaId, MediaRecord, Result};

const COLUMNS: &str = "d.media_id, d.folder, d.file, d.size, d.received, d.state, d.error,
    d.subtitles, d.scripts, d.played_offline, d.added_at, d.finished_at";

/// Where a download has got to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DownloadState {
    /// Not finished: waiting its turn, under way, or stopped part way.
    Waiting,
    Done,
    /// Stopped by an error, until retried.
    Failed,
}

impl DownloadState {
    fn as_str(self) -> &'static str {
        match self {
            DownloadState::Waiting => "waiting",
            DownloadState::Done => "done",
            DownloadState::Failed => "failed",
        }
    }

    fn parse(s: &str) -> Option<DownloadState> {
        match s {
            "waiting" => Some(DownloadState::Waiting),
            "done" => Some(DownloadState::Done),
            "failed" => Some(DownloadState::Failed),
            _ => None,
        }
    }
}

/// A video downloaded, or being downloaded.
#[derive(Clone, Debug, PartialEq)]
pub struct Download {
    pub media_id: MediaId,
    /// The download's own folder: a path on the headset, or a `volume:`
    /// location on a card.
    pub folder: String,
    /// The video's file name in [`Self::folder`].
    pub file: String,
    /// Bytes, when the source said.
    pub size: Option<u64>,
    pub received: u64,
    pub state: DownloadState,
    pub error: Option<String>,
    /// Subtitle files saved beside the video; each location is a file name
    /// in [`Self::folder`].
    pub subtitles: Vec<SubtitleFile>,
    /// Haptic script file names in [`Self::folder`].
    pub scripts: Vec<String>,
    /// The last play was watched without the server hearing where it got
    /// to, so the headset's resume point is the newer one.
    pub played_offline: bool,
    pub added_at: i64,
    pub finished_at: Option<i64>,
}

impl Download {
    /// Where file `name` of this download is.
    pub fn location_of(&self, name: &str) -> String {
        format!("{}/{name}", self.folder.trim_end_matches('/'))
    }

    /// Where the video is.
    pub fn video_location(&self) -> String {
        self.location_of(&self.file)
    }

    /// Its subtitle files with their full locations.
    pub fn subtitle_files(&self) -> Vec<SubtitleFile> {
        self.subtitles
            .iter()
            .map(|s| SubtitleFile {
                location: self.location_of(&s.location),
                ..s.clone()
            })
            .collect()
    }

    /// Its haptic scripts' full locations.
    pub fn script_locations(&self) -> Vec<String> {
        self.scripts.iter().map(|s| self.location_of(s)).collect()
    }

    /// Bytes on the headset: all of the file once done, else what came.
    pub fn bytes(&self) -> u64 {
        match self.state {
            DownloadState::Done => self.size.unwrap_or(self.received).max(self.received),
            _ => self.received,
        }
    }
}

/// How many downloads a source has, finished or not, and their bytes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DownloadCount {
    pub videos: usize,
    /// Not finished yet.
    pub unfinished: usize,
    pub bytes: u64,
}

fn json_col<T: serde::de::DeserializeOwned>(row: &Row<'_>, i: usize) -> rusqlite::Result<T> {
    let s: String = row.get(i)?;
    serde_json::from_str(&s)
        .map_err(|e| rusqlite::Error::FromSqlConversionFailure(i, Type::Text, Box::new(e)))
}

fn download_from_row(row: &Row<'_>) -> rusqlite::Result<Download> {
    let state: String = row.get(5)?;
    Ok(Download {
        media_id: MediaId(row.get(0)?),
        folder: row.get(1)?,
        file: row.get(2)?,
        size: row.get::<_, Option<i64>>(3)?.map(|n| n.max(0) as u64),
        received: row.get::<_, i64>(4)?.max(0) as u64,
        state: DownloadState::parse(&state).ok_or_else(|| {
            rusqlite::Error::FromSqlConversionFailure(
                5,
                Type::Text,
                format!("unknown download state {state}").into(),
            )
        })?,
        error: row.get(6)?,
        subtitles: json_col(row, 7)?,
        scripts: json_col(row, 8)?,
        played_offline: row.get(9)?,
        added_at: row.get(10)?,
        finished_at: row.get(11)?,
    })
}

fn get_download(conn: &Connection, id: MediaId) -> Result<Option<Download>> {
    let mut stmt = conn.prepare_cached(&format!(
        "SELECT {COLUMNS} FROM downloads d WHERE d.media_id = ?1"
    ))?;
    Ok(stmt.query_row([id.0], download_from_row).optional()?)
}

fn query_downloads(
    conn: &Connection,
    sql_after_from: &str,
    params: impl rusqlite::Params,
) -> Result<Vec<Download>> {
    let mut stmt = conn.prepare_cached(&format!(
        "SELECT {COLUMNS} FROM downloads d JOIN media m ON m.id = d.media_id {sql_after_from}"
    ))?;
    let rows = stmt.query_map(params, download_from_row)?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

fn i64_of(n: u64) -> i64 {
    i64::try_from(n).unwrap_or(i64::MAX)
}

impl Library {
    /// Starts a download of video `id` into `folder`, the video to be file
    /// `file` there. A video already downloading or downloaded keeps the
    /// download it has, which is returned.
    pub fn add_download(
        &self,
        id: MediaId,
        folder: &str,
        file: &str,
        size: Option<u64>,
    ) -> Result<Download> {
        self.in_tx(|c| {
            crate::library::ensure_media(c, id)?;
            c.execute(
                "INSERT INTO downloads (media_id, folder, file, size, added_at)
                 VALUES (?1, ?2, ?3, ?4, ?5)
                 ON CONFLICT (media_id) DO NOTHING",
                params![id.0, folder, file, size.map(i64_of), now()],
            )?;
            get_download(c, id)?.ok_or(crate::Error::MediaNotFound(id))
        })
    }

    /// Video `id`'s download, if it has one.
    pub fn download(&self, id: MediaId) -> Result<Option<Download>> {
        self.with(|c| get_download(c, id))
    }

    /// The finished download of the video at `location`.
    pub fn finished_download(&self, location: &str) -> Result<Option<Download>> {
        self.with(|c| {
            Ok(query_downloads(c, "WHERE m.location = ?1 AND d.state = 'done'", [location])?.pop())
        })
    }

    /// Every download, of hidden sources too, oldest first: what the app
    /// downloads in turn and reads from.
    pub fn all_downloads(&self) -> Result<Vec<(String, Download)>> {
        self.with(|c| {
            let mut stmt = c.prepare_cached(&format!(
                "SELECT {COLUMNS}, m.location FROM downloads d JOIN media m ON m.id = d.media_id
                 ORDER BY d.added_at, d.media_id"
            ))?;
            let rows = stmt.query_map([], |r| Ok((r.get(12)?, download_from_row(r)?)))?;
            Ok(rows.collect::<rusqlite::Result<_>>()?)
        })
    }

    /// Source `source_id`'s downloads with their videos, newest first;
    /// nothing while the source is hidden.
    pub fn downloads_of(&self, source_id: &str) -> Result<Vec<(MediaRecord, Download)>> {
        if self.hidden().hides(source_id) {
            return Ok(Vec::new());
        }
        let downloads = self.with(|c| {
            query_downloads(
                c,
                "WHERE m.source_id = ?1 ORDER BY d.added_at DESC, d.media_id DESC",
                [source_id],
            )
        })?;
        let ids: Vec<MediaId> = downloads.iter().map(|d| d.media_id).collect();
        let records = self.with(|c| crate::library::get_many(c, &ids))?;
        Ok(records.into_iter().zip(downloads).collect())
    }

    /// How many downloads each source has, sources that are hidden left
    /// out.
    pub fn download_counts(&self) -> Result<Vec<(String, DownloadCount)>> {
        let (shown, args) = match hidden_sql("m.", &self.hidden()) {
            Some((sql, args)) => (format!("WHERE {sql}"), args),
            None => (String::new(), Vec::new()),
        };
        self.with(|c| {
            let mut stmt = c.prepare(&format!(
                "SELECT m.source_id, COUNT(*), SUM(d.state != 'done'),
                        SUM(CASE WHEN d.state = 'done' THEN MAX(COALESCE(d.size, 0), d.received)
                                 ELSE d.received END)
                 FROM downloads d JOIN media m ON m.id = d.media_id {shown}
                 GROUP BY m.source_id ORDER BY m.source_id"
            ))?;
            let rows = stmt.query_map(rusqlite::params_from_iter(args), |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    DownloadCount {
                        videos: r.get::<_, i64>(1)?.max(0) as usize,
                        unfinished: r.get::<_, i64>(2)?.max(0) as usize,
                        bytes: r.get::<_, i64>(3)?.max(0) as u64,
                    },
                ))
            })?;
            Ok(rows.collect::<rusqlite::Result<_>>()?)
        })
    }

    /// How far video `id`'s download has got, and how big the file is when
    /// the source says.
    pub fn set_download_progress(
        &self,
        id: MediaId,
        received: u64,
        size: Option<u64>,
    ) -> Result<()> {
        self.with(|c| {
            let n = c.execute(
                "UPDATE downloads SET received = ?2, size = COALESCE(?3, size)
                 WHERE media_id = ?1",
                params![id.0, i64_of(received), size.map(i64_of)],
            )?;
            affected(n, id)
        })
    }

    /// Names the file video `id` downloads to, as the server calls it;
    /// only before any of it has come, so a download part way keeps its
    /// file.
    pub fn set_download_file(&self, id: MediaId, file: &str) -> Result<bool> {
        self.with(|c| {
            let n = c.execute(
                "UPDATE downloads SET file = ?2 WHERE media_id = ?1 AND received = 0",
                params![id.0, file],
            )?;
            Ok(n > 0)
        })
    }

    /// The side files saved beside video `id`, by their names in its
    /// folder.
    pub fn set_download_side_files(
        &self,
        id: MediaId,
        subtitles: &[SubtitleFile],
        scripts: &[String],
    ) -> Result<()> {
        let subtitles = serde_json::to_string(subtitles)?;
        let scripts = serde_json::to_string(scripts)?;
        self.with(|c| {
            let n = c.execute(
                "UPDATE downloads SET subtitles = ?2, scripts = ?3 WHERE media_id = ?1",
                params![id.0, subtitles, scripts],
            )?;
            affected(n, id)
        })
    }

    /// Video `id` downloaded in full: `size` bytes.
    pub fn finish_download(&self, id: MediaId, size: u64) -> Result<()> {
        self.with(|c| {
            let n = c.execute(
                "UPDATE downloads SET state = 'done', error = NULL, size = ?2, received = ?2,
                        finished_at = ?3
                 WHERE media_id = ?1",
                params![id.0, i64_of(size), now()],
            )?;
            affected(n, id)
        })
    }

    /// Video `id`'s download stopped with `error`, until retried.
    pub fn fail_download(&self, id: MediaId, error: &str) -> Result<()> {
        self.set_download_state(id, DownloadState::Failed, Some(error))
    }

    /// Video `id`'s failed download waits its turn again.
    pub fn retry_download(&self, id: MediaId) -> Result<()> {
        self.set_download_state(id, DownloadState::Waiting, None)
    }

    fn set_download_state(
        &self,
        id: MediaId,
        state: DownloadState,
        error: Option<&str>,
    ) -> Result<()> {
        self.with(|c| {
            let n = c.execute(
                "UPDATE downloads SET state = ?2, error = ?3 WHERE media_id = ?1 AND state != 'done'",
                params![id.0, state.as_str(), error],
            )?;
            affected(n, id)
        })
    }

    /// Forgets video `id`'s download, returning it so its files can go.
    /// The video and its history stay.
    pub fn remove_download(&self, id: MediaId) -> Result<Option<Download>> {
        self.in_tx(|c| {
            let download = get_download(c, id)?;
            c.execute("DELETE FROM downloads WHERE media_id = ?1", [id.0])?;
            Ok(download)
        })
    }

    /// Whether video `id`'s last play went unheard by its server.
    pub fn set_played_offline(&self, id: MediaId, offline: bool) -> Result<()> {
        self.with(|c| {
            c.execute(
                "UPDATE downloads SET played_offline = ?2 WHERE media_id = ?1",
                params![id.0, offline],
            )?;
            Ok(())
        })
    }

    /// Every download's folder, for clearing away folders nothing owns.
    pub fn download_folders(&self) -> Result<HashSet<String>> {
        self.with(|c| {
            let mut stmt = c.prepare_cached("SELECT folder FROM downloads")?;
            let rows = stmt.query_map([], |r| r.get(0))?;
            Ok(rows.collect::<rusqlite::Result<_>>()?)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Hidden, MediaUpsert};

    fn library_with(locations: &[(&str, &str)]) -> (Library, Vec<MediaId>) {
        let lib = Library::open_in_memory().unwrap();
        let ids = locations
            .iter()
            .map(|(source, location)| {
                lib.upsert(&MediaUpsert::new(*location, *source))
                    .unwrap()
                    .id
            })
            .collect();
        (lib, ids)
    }

    #[test]
    fn a_download_goes_from_waiting_to_done() {
        let (lib, ids) = library_with(&[("smb", "smb://nas/v/a.mkv")]);
        let d = lib
            .add_download(ids[0], "/dl/1", "a.mkv", Some(100))
            .unwrap();
        assert_eq!(d.state, DownloadState::Waiting);
        assert_eq!(d.video_location(), "/dl/1/a.mkv");
        assert_eq!(lib.finished_download("smb://nas/v/a.mkv").unwrap(), None);

        assert!(lib.set_download_file(ids[0], "Film.mkv").unwrap());
        lib.set_download_progress(ids[0], 40, None).unwrap();
        let d = lib.download(ids[0]).unwrap().unwrap();
        assert_eq!((d.received, d.size, d.bytes()), (40, Some(100), 40));
        assert!(
            !lib.set_download_file(ids[0], "Other.mkv").unwrap(),
            "a file part way keeps its name"
        );
        assert_eq!(d.file, "Film.mkv");

        lib.finish_download(ids[0], 100).unwrap();
        let d = lib.finished_download("smb://nas/v/a.mkv").unwrap().unwrap();
        assert_eq!(
            (d.state, d.received, d.bytes()),
            (DownloadState::Done, 100, 100)
        );
        assert!(d.finished_at.is_some());
    }

    #[test]
    fn adding_again_keeps_the_download_there_is() {
        let (lib, ids) = library_with(&[("smb", "smb://nas/v/a.mkv")]);
        lib.add_download(ids[0], "/dl/1", "a.mkv", None).unwrap();
        lib.finish_download(ids[0], 5).unwrap();
        let again = lib.add_download(ids[0], "/dl/2", "b.mkv", None).unwrap();
        assert_eq!(
            (again.folder.as_str(), again.state),
            ("/dl/1", DownloadState::Done)
        );
    }

    #[test]
    fn a_failed_download_can_be_retried_and_a_finished_one_stays_done() {
        let (lib, ids) = library_with(&[("smb", "smb://nas/v/a.mkv")]);
        lib.add_download(ids[0], "/dl/1", "a.mkv", None).unwrap();
        lib.fail_download(ids[0], "gone").unwrap();
        let d = lib.download(ids[0]).unwrap().unwrap();
        assert_eq!(
            (d.state, d.error.as_deref()),
            (DownloadState::Failed, Some("gone"))
        );
        lib.retry_download(ids[0]).unwrap();
        let d = lib.download(ids[0]).unwrap().unwrap();
        assert_eq!((d.state, d.error), (DownloadState::Waiting, None));
        lib.finish_download(ids[0], 9).unwrap();
        assert!(lib.fail_download(ids[0], "late").is_err());
        assert_eq!(
            lib.download(ids[0]).unwrap().unwrap().state,
            DownloadState::Done
        );
    }

    #[test]
    fn side_files_are_named_in_the_folder() {
        let (lib, ids) = library_with(&[("smb", "smb://nas/v/a.mkv")]);
        lib.add_download(ids[0], "volume:ABCD/.dl/1/", "a.mkv", None)
            .unwrap();
        let sub = SubtitleFile {
            name: "a.en.srt".into(),
            location: "a.en.srt".into(),
            language: Some("en".into()),
            language_name: None,
            flags: Default::default(),
        };
        lib.set_download_side_files(ids[0], &[sub], &["a.funscript".into()])
            .unwrap();
        let d = lib.download(ids[0]).unwrap().unwrap();
        assert_eq!(d.subtitle_files()[0].location, "volume:ABCD/.dl/1/a.en.srt");
        assert_eq!(d.subtitle_files()[0].language.as_deref(), Some("en"));
        assert_eq!(d.script_locations(), ["volume:ABCD/.dl/1/a.funscript"]);
    }

    #[test]
    fn counts_and_listings_leave_hidden_sources_out() {
        let (lib, ids) = library_with(&[
            ("smb", "smb://nas/v/a.mkv"),
            ("smb", "smb://nas/v/b.mkv"),
            ("plex", "plex://source/plex/m/item/1"),
        ]);
        for (i, id) in ids.iter().enumerate() {
            lib.add_download(*id, &format!("/dl/{i}"), "v.mkv", Some(10))
                .unwrap();
        }
        lib.finish_download(ids[0], 10).unwrap();
        lib.set_download_progress(ids[1], 4, None).unwrap();
        let counts = lib.download_counts().unwrap();
        assert_eq!(
            counts,
            [
                (
                    "plex".to_string(),
                    DownloadCount {
                        videos: 1,
                        unfinished: 1,
                        bytes: 0
                    }
                ),
                (
                    "smb".to_string(),
                    DownloadCount {
                        videos: 2,
                        unfinished: 1,
                        bytes: 14
                    }
                ),
            ]
        );
        assert_eq!(lib.downloads_of("smb").unwrap().len(), 2);

        lib.set_hidden(Hidden {
            sources: vec!["smb".into()],
        });
        assert_eq!(lib.download_counts().unwrap().len(), 1);
        assert!(lib.downloads_of("smb").unwrap().is_empty());
        // What is downloaded in turn still has them.
        assert_eq!(lib.all_downloads().unwrap().len(), 3);
    }

    #[test]
    fn removing_the_download_keeps_the_video_and_removing_the_video_takes_the_download() {
        let (lib, ids) =
            library_with(&[("smb", "smb://nas/v/a.mkv"), ("smb", "smb://nas/v/b.mkv")]);
        lib.add_download(ids[0], "/dl/0", "a.mkv", None).unwrap();
        lib.add_download(ids[1], "/dl/1", "b.mkv", None).unwrap();
        let removed = lib.remove_download(ids[0]).unwrap().unwrap();
        assert_eq!(removed.folder, "/dl/0");
        assert!(lib.get(ids[0]).unwrap().is_some());
        lib.remove_media(ids[1]).unwrap();
        assert!(lib.download_folders().unwrap().is_empty());
    }

    #[test]
    fn a_play_unheard_by_the_server_is_kept() {
        let (lib, ids) = library_with(&[("mb", "mb:http://jf#item=1")]);
        lib.add_download(ids[0], "/dl/0", "a.mkv", None).unwrap();
        lib.set_played_offline(ids[0], true).unwrap();
        assert!(lib.download(ids[0]).unwrap().unwrap().played_offline);
        lib.set_played_offline(ids[0], false).unwrap();
        assert!(!lib.download(ids[0]).unwrap().unwrap().played_offline);
    }
}
