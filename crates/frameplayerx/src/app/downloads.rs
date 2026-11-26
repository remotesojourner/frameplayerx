//! Downloads asked for from the panels: starting, removing and retrying
//! them, and saying how they went.

use super::*;

impl App {
    pub(super) fn check_download(&mut self, source_id: String, location: String) {
        let opener = self.services.opener.clone();
        self.jobs.spawn("download-check", move || {
            let allowed = match opener.ask_download_allowed_in(&source_id, &location) {
                Ok(v) => Some(v),
                Err(e) => {
                    log::warn!("checking whether {location} may be downloaded: {e}");
                    None
                }
            };
            Job::DownloadCheck {
                source_id,
                location,
                allowed,
            }
        });
    }

    /// Notes a download of `req`'s video, which the downloads then take in
    /// turn. Asks its server first, so not on the frame loop.
    pub(super) fn start_download(&mut self, req: OpenRequest, title: String) {
        let library = self.services.library.clone();
        let opener = self.services.opener.clone();
        let downloads = self.services.downloads.clone();
        let root = crate::downloads::root_location(self.settings.download_to.as_ref());
        self.jobs.spawn("download", move || {
            let location = req.location.clone();
            let result = add_download(&library, &opener, &root, req);
            if result.is_ok() {
                downloads.wake();
            }
            Job::DownloadAdded {
                location,
                title,
                result,
            }
        });
    }

    pub(super) fn download_added(
        &mut self,
        location: String,
        title: String,
        result: Result<(), String>,
    ) {
        self.ui.download_asked.remove(&location);
        let title = ui::widgets::display_title(&title);
        match result {
            Ok(()) => self
                .ui
                .toast(tr!("toast-download-started", title = title.as_str())),
            Err(e) => self.ui.toast(tr!(
                "toast-download-failed",
                title = title.as_str(),
                error = e
            )),
        }
        self.panels[MAIN].request_repaint();
    }

    pub(super) fn remove_download(&mut self, id: fp_library::MediaId) {
        let downloads = self.services.downloads.clone();
        self.jobs
            .spawn("download-remove", move || match downloads.remove(id) {
                Ok(()) => Job::Done,
                Err(e) => Job::Toast(tr!("download-remove-failed", error = e)),
            });
        self.ui.download_armed = None;
    }

    pub(super) fn retry_download(&mut self, id: fp_library::MediaId) {
        match self.services.library.retry_download(id) {
            Ok(()) => self.services.downloads.wake(),
            Err(e) => log::warn!("retrying the download of {id}: {e}"),
        }
    }

    /// What the downloads say: one finished or failed. A locked source's
    /// videos aren't named.
    pub(super) fn poll_downloads(&mut self) {
        self.services
            .downloads
            .set_streaming(self.playback.as_ref().is_some_and(|p| p.streams));
        for event in self.services.downloads.take_events() {
            let (id, text) = match event {
                crate::downloads::Event::Finished { media_id, title } => (
                    media_id,
                    tr!(
                        "toast-downloaded",
                        title = ui::widgets::display_title(&title)
                    ),
                ),
                crate::downloads::Event::Failed {
                    media_id,
                    title,
                    error,
                } => (
                    media_id,
                    tr!(
                        "toast-download-failed",
                        title = ui::widgets::display_title(&title),
                        error = error
                    ),
                ),
            };
            let hidden = self
                .services
                .library
                .get(id)
                .ok()
                .flatten()
                .is_some_and(|r| self.source_locked(&r.source_id));
            if !hidden {
                self.ui.toast(text);
            }
        }
        let generation = self.services.downloads.generation();
        if generation != self.downloads_seen {
            self.downloads_seen = generation;
            self.panels[MAIN].request_repaint();
        }
    }
}

/// Finds or makes `req`'s video in the library and notes its download in a
/// new folder under `root`, once its server says it may be downloaded.
fn add_download(
    library: &Library,
    opener: &crate::services::Opener,
    root: &str,
    req: OpenRequest,
) -> Result<(), String> {
    let source_id = req
        .source_id
        .clone()
        .ok_or_else(|| tr!("source-not-available").to_string())?;
    let source = opener
        .source(&source_id)
        .ok_or_else(|| tr!("source-not-available").to_string())?;
    if crate::downloads::path_of(root).is_none() {
        return Err(tr!("open-not-inserted").to_string());
    }
    let entry = req.entry.clone().unwrap_or_else(|| {
        fp_core::source::Entry::new(
            req.shown_title(),
            req.location.clone(),
            fp_core::source::EntryKind::Video,
        )
    });
    // As playing it would: its details name it the way its library row does.
    let entry = source.details(&entry).map_err(|e| ui::errors::source(&e))?;
    if !source
        .ask_download_allowed(&entry.location)
        .map_err(|e| ui::errors::source(&e))?
    {
        return Err(tr!("download-not-allowed").to_string());
    }
    let record = match library
        .get_by_location(&entry.location)
        .map_err(|e| e.to_string())?
    {
        Some(r) => r,
        None => {
            library
                .upsert_entry(&source_id, &entry)
                .map_err(|e| e.to_string())?;
            library
                .get_by_location(&entry.location)
                .map_err(|e| e.to_string())?
                .ok_or_else(|| tr!("source-not-available").to_string())?
        }
    };
    library
        .add_download(
            record.id,
            &crate::downloads::new_folder(root),
            "",
            entry.size,
        )
        .map_err(|e| e.to_string())?;
    Ok(())
}
