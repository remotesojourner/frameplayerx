//! Send diagnostics, from Settings › About: the facts and logs are gathered
//! as the page opens, and sent with what the user writes.

use super::{App, Job};
use crate::playback::Playback;
use crate::ui;

impl App {
    pub(super) fn open_diagnostics(&mut self) {
        if crate::crash::REPORT_URL.is_none() {
            return;
        }
        self.ui.diagnostics = Some(ui::diagnostics::Form::default());
        let private = crate::crash::Private::gather(
            &self.services.library,
            &self.services.source_configs,
            &self.settings,
        );
        let mut facts = crate::crash::Facts {
            about: self.ui.about.clone(),
            sources: self
                .services
                .source_configs
                .iter()
                .map(|c| format!("{:?}", c.kind()))
                .collect(),
            video: self
                .playback
                .as_ref()
                .and_then(|pb| video_facts(pb, self.settings.hardware_decoding)),
            settings: serde_json::to_string(&self.settings).unwrap_or_default(),
            ..Default::default()
        };
        self.jobs.spawn("diagnostics", move || {
            let log = crate::logger::log_path();
            facts.system = crate::crash::system();
            facts.log = std::fs::read_to_string(&log).unwrap_or_default();
            facts.previous_log =
                std::fs::read_to_string(log.with_extension("log.old")).unwrap_or_default();
            Job::DiagnosticsGathered(Box::new(crate::crash::Diagnostics::gather(
                &facts, &private,
            )))
        });
    }

    pub(super) fn send_diagnostics(&mut self) {
        let (Some(url), Some(f)) = (crate::crash::REPORT_URL, &mut self.ui.diagnostics) else {
            return;
        };
        let words = f.words.trim();
        let Some(gathered) = f.gathered.as_mut() else {
            return;
        };
        if words.is_empty() || f.sending {
            return;
        }
        let body = gathered.report(words).body();
        f.sending = true;
        f.error = None;
        self.jobs.spawn("diagnostics-send", move || {
            let r = post_report(url, &body).map(|reply| {
                serde_json::from_slice::<serde_json::Value>(&reply)
                    .ok()
                    .and_then(|v| v["number"].as_u64())
            });
            Job::DiagnosticsSent(r)
        });
    }

    pub(super) fn diagnostics_gathered(&mut self, d: crate::crash::Diagnostics) {
        if let Some(f) = &mut self.ui.diagnostics {
            f.gathered = Some(d);
        }
    }

    pub(super) fn diagnostics_sent(&mut self, r: Result<Option<u64>, String>) {
        let Some(f) = &mut self.ui.diagnostics else {
            return;
        };
        f.sending = false;
        match r {
            Ok(reference) => {
                f.sent = true;
                f.reference = reference;
            }
            Err(e) => f.error = Some(e),
        }
    }
}

/// Sends a report to the relay; the relay's answer.
pub(super) fn post_report(url: &str, body: &[u8]) -> Result<Vec<u8>, String> {
    let http = fp_sources::http::HttpClient::new(Default::default(), None);
    let reply = http
        .request(
            "POST",
            url,
            &[("Content-Type", "application/json")],
            Some(body),
            4096,
        )
        .map_err(|e| ui::errors::source(&e))?;
    match reply.status {
        200..=299 => Ok(reply.body),
        status => Err(tr!(
            "error-http-status",
            status = status.to_string(),
            url = url
        )),
    }
}

/// What Stream info shows for the open video, in English for its
/// developers. Not its name: the stream's own title can hold one the
/// library doesn't know.
fn video_facts(pb: &Playback, hardware_on: bool) -> Option<String> {
    let v = pb.player.info().video_stream()?;
    let st = pb.player.stats();
    let mut s = format!(
        "Video: {} {}x{}, {:.3} fps, {}-bit {} {:?}",
        v.codec, v.width, v.height, v.fps, v.bit_depth, v.pixel_format, v.transfer
    );
    for (on, what) in [
        (v.interlaced, "interlaced"),
        (v.deinterlaced, "deinterlaced"),
        (v.mvc, "MVC"),
        (v.frame_sequential, "frame sequential"),
    ] {
        if on {
            s += &format!(", {what}");
        }
    }
    s += &format!("\nFormat: {:?}", pb.format);
    s += &format!(
        "\nDecoder: {} ({}; hardware decoding {})",
        st.video_decoder,
        if st.hardware { "hardware" } else { "software" },
        if hardware_on { "on" } else { "off" }
    );
    if let Some(e) = &st.hardware_error {
        s += &format!("\nNot hardware: {e}");
    }
    s += &format!(
        "\nAudio: {} to {}\nFrames: {} shown, {} dropped, {} stalls\nRead ahead: {:.1} of {:.1} s",
        st.audio_decoder,
        st.audio_sink,
        st.frames_shown,
        st.frames_dropped,
        st.stalls,
        st.read_ahead.0,
        st.read_ahead.1
    );
    Some(s)
}
