//! The performance overlay (Settings › About › Diagnostics): frame time,
//! the video's frame rate and decoder, how a network video's reads go, CPU,
//! GPU and memory, in a small panel in the room that the grip moves.

use super::theme::{self, Weight};
use crate::perf::Sample;
use egui::{Color32, RichText};
use std::time::{Duration, Instant};

/// What the overlay shows.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Overlay {
    /// Render-thread time per frame over the last second: typical and
    /// longest, ms.
    pub frame_ms: Option<(f32, f32)>,
    /// The display's refresh rate.
    pub display_hz: Option<f32>,
    /// The video's frames shown and dropped per second, and its decoder
    /// ("HW · HEVC 10-bit").
    pub video: Option<(f32, f32, String)>,
    /// While a video from the network plays.
    pub network: Option<Network>,
    pub sample: Option<Sample>,
}

/// How a network video's reads go: over the last half second, and since it
/// opened.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Network {
    /// Received, MB/s.
    pub mb_s: f32,
    /// What a read (a 1 MiB block) took on average, from the request to its
    /// last byte, ms; `None` when there was none.
    pub read_ms: Option<f32>,
    /// Reads sent again on a new connection since the video opened (SMB).
    pub reconnects: u64,
    /// Seconds of video read ahead of the picture, how far ahead the player
    /// reads, and the bytes of it held.
    pub buffer: (f32, f32, usize),
}

/// The network rates, from the totals of `NetCounters`. A panel repaints a
/// few times in a row once it paints, so a reading covers at least
/// [`NetRate::WINDOW`] and stands until the next: from the milliseconds
/// between repaints, a video read in 1 MiB blocks would mostly read as 0.
#[derive(Clone, Debug)]
pub struct NetRate {
    since: Instant,
    /// Bytes, reads and nanoseconds reading.
    totals: [u64; 3],
    mb_s: f32,
    read_ms: Option<f32>,
}

impl NetRate {
    const WINDOW: Duration = Duration::from_millis(400);

    pub fn new(totals: [u64; 3], now: Instant) -> Self {
        Self {
            since: now,
            totals,
            mb_s: 0.0,
            read_ms: None,
        }
    }

    /// The rates to show: MB/s and the average read in ms.
    pub fn update(&mut self, totals: [u64; 3], now: Instant) -> (f32, Option<f32>) {
        // Another video's totals are larger, as a new one counts from zero.
        if self.totals.iter().zip(&totals).any(|(old, new)| old > new) {
            *self = Self::new(totals, now);
        }
        let window = now.duration_since(self.since);
        if window >= Self::WINDOW {
            let reads = totals[1] - self.totals[1];
            self.mb_s = (totals[0] - self.totals[0]) as f32 / window.as_secs_f32() / 1e6;
            self.read_ms =
                (reads > 0).then(|| (totals[2] - self.totals[2]) as f32 / reads as f32 / 1e6);
            self.since = now;
            self.totals = totals;
        }
        (self.mb_s, self.read_ms)
    }
}

/// Label, value, and whether the value is a warning sign: frames over the
/// display's time, or video frames dropped.
pub fn rows(o: &Overlay) -> Vec<(&'static str, String, bool)> {
    const NONE: &str = "–";
    let gb = |bytes: u64| bytes as f64 / (1u64 << 30) as f64;
    let mut rows = Vec::new();
    let budget = o.display_hz.filter(|hz| *hz > 0.0).map(|hz| 1000.0 / hz);
    rows.push(match o.frame_ms {
        Some((typical, longest)) => {
            let hz = o
                .display_hz
                .map_or(String::new(), |hz| format!(" · {hz:.0} Hz"));
            let (typical, longest_ms) = (format!("{typical:.1}"), format!("{longest:.1}"));
            (
                tr!("perf-frame"),
                tr!(
                    "perf-frame-value",
                    typical = typical,
                    longest = longest_ms,
                    hz = hz
                ),
                budget.is_some_and(|b| longest > b),
            )
        }
        None => (tr!("perf-frame"), NONE.into(), false),
    });
    if let Some((fps, dropped, decoder)) = &o.video {
        let (fps_text, dropped_text) = (format!("{fps:.1}"), format!("{dropped:.0}"));
        rows.push((
            tr!("perf-video"),
            tr!("perf-video-value", fps = fps_text, dropped = dropped_text),
            *dropped > 0.0,
        ));
        rows.push((tr!("adjust-decoder"), decoder.clone(), false));
    }
    if let Some(n) = &o.network {
        let (mb_s, reconnects) = (format!("{:.1}", n.mb_s), n.reconnects.to_string());
        rows.push((
            tr!("perf-network"),
            match n.read_ms {
                Some(ms) => {
                    let ms = format!("{ms:.0}");
                    tr!(
                        "perf-network-reads",
                        mb_s = mb_s,
                        ms = ms,
                        reconnects = reconnects
                    )
                }
                None => tr!("perf-network-value", mb_s = mb_s, reconnects = reconnects),
            },
            n.reconnects > 0,
        ));
        let (ahead, target, bytes) = n.buffer;
        let (ahead_text, target, mb) = (
            format!("{ahead:.1}"),
            format!("{target:.0}"),
            format!("{:.0}", bytes as f64 / 1e6),
        );
        rows.push((
            tr!("perf-buffer"),
            tr!(
                "perf-buffer-value",
                ahead = ahead_text,
                target = target,
                mb = mb
            ),
            ahead < 1.0,
        ));
    }
    let s = o.sample.unwrap_or_default();
    rows.push((
        "CPU",
        match (s.cpu, s.app_cpu) {
            (Some(all), Some(app)) => {
                let (all, app) = (format!("{all:.0}"), format!("{app:.0}"));
                tr!("perf-cpu-value", all = all, app = app)
            }
            (Some(all), None) => format!("{all:.0}%"),
            _ => NONE.into(),
        },
        false,
    ));
    rows.push((
        "GPU",
        match (s.gpu, s.gpu_mhz) {
            (Some(busy), Some(mhz)) => format!("{busy:.0}% · {mhz} MHz"),
            (Some(busy), None) => format!("{busy:.0}%"),
            (None, Some(mhz)) => format!("{mhz} MHz"),
            (None, None) => NONE.into(),
        },
        false,
    ));
    rows.push((
        tr!("perf-memory"),
        match (s.mem, s.app_mem) {
            (Some((used, total)), Some(app)) => {
                let (used, total, app) = (
                    format!("{:.1}", gb(used)),
                    format!("{:.1}", gb(total)),
                    format!("{:.1}", gb(app)),
                );
                tr!("perf-memory-app", used = used, total = total, app = app)
            }
            (Some((used, total)), None) => {
                let (used, total) = (format!("{:.1}", gb(used)), format!("{:.1}", gb(total)));
                tr!("perf-memory-value", used = used, total = total)
            }
            _ => NONE.into(),
        },
        false,
    ));
    rows
}

pub fn overlay(ctx: &egui::Context, o: &Overlay) {
    let clear = egui::Frame::new().fill(Color32::TRANSPARENT);
    egui::CentralPanel::default().frame(clear).show(ctx, |ui| {
        egui::Frame::new()
            .fill(Color32::from_black_alpha(200))
            .corner_radius(12)
            .inner_margin(egui::Margin::symmetric(16, 12))
            .show(ui, |ui| {
                egui::Grid::new("perf")
                    .num_columns(2)
                    .spacing([16.0, 6.0])
                    .show(ui, |ui| {
                        for (label, value, warn) in rows(o) {
                            ui.label(
                                RichText::new(label)
                                    .font(theme::font(Weight::SemiBold, 24.0))
                                    .color(theme::TEXT_2),
                            );
                            ui.label(
                                RichText::new(value)
                                    .font(theme::font(Weight::Regular, 24.0))
                                    .color(if warn { theme::WARN } else { theme::TEXT }),
                            );
                            ui.end_row();
                        }
                    });
            });
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rows_say_what_is_known_and_flag_trouble() {
        let o = Overlay {
            frame_ms: Some((6.12, 12.4)),
            display_hz: Some(90.0),
            video: Some((59.94, 0.0, "HW · HEVC 10-bit".into())),
            network: None,
            sample: Some(Sample {
                cpu: Some(34.4),
                app_cpu: Some(21.0),
                mem: Some((5 << 30, 15 << 30)),
                app_mem: Some(3 << 29),
                gpu: Some(41.0),
                gpu_mhz: Some(680),
            }),
        };
        let r = rows(&o);
        assert_eq!(
            r[0],
            ("Frame", "6.1 ms, longest 12.4 · 90 Hz".into(), true),
            "over 11.1 ms"
        );
        assert_eq!(r[1], ("Video", "59.9 fps · 0 dropped/s".into(), false));
        assert_eq!(r[2], ("Decoder", "HW · HEVC 10-bit".into(), false));
        assert_eq!(r[3].1, "34% · app 21%");
        assert_eq!(r[4].1, "41% · 680 MHz");
        assert_eq!(r[5].1, "5.0 of 15.0 GB · app 1.5 GB");

        // Nothing known yet, no video: a dash rather than a made-up zero.
        let r = rows(&Overlay::default());
        assert_eq!(r.len(), 4);
        assert!(r.iter().all(|(_, v, warn)| v == "–" && !warn));
    }

    #[test]
    fn a_repaint_right_after_a_reading_keeps_it() {
        // The panel repaints a few times 12 ms apart after each paint: those
        // must not leave the row saying 0 MB/s while a film is being read.
        let start = Instant::now();
        let at = |ms| start + Duration::from_millis(ms);
        let mib = 1 << 20;
        let mut rate = NetRate::new([0, 0, 0], start);
        let (mb_s, read_ms) = rate.update([3 * mib, 3, 150_000_000], at(500));
        assert!((mb_s - 6.29).abs() < 0.01, "{mb_s}");
        assert_eq!(read_ms, Some(50.0));
        assert_eq!(
            rate.update([3 * mib, 3, 150_000_000], at(512)),
            (mb_s, read_ms)
        );
        // Nothing arrived over a whole window: that is a real 0.
        assert_eq!(
            rate.update([3 * mib, 3, 150_000_000], at(1000)),
            (0.0, None)
        );
        // A new video counts from zero, and starts a reading of its own.
        assert_eq!(rate.update([mib, 1, 1], at(1100)), (0.0, None));
        let (mb_s, _) = rate.update([2 * mib, 2, 2], at(1500));
        assert!((mb_s - 2.62).abs() < 0.01, "{mb_s}");
    }

    #[test]
    fn a_network_video_says_how_its_reads_go() {
        // A film from a NAS stalled with nothing on screen saying why: the
        // reads, their timeouts and the buffer running dry show it.
        let mut o = Overlay {
            network: Some(Network {
                mb_s: 12.34,
                read_ms: Some(48.6),
                reconnects: 0,
                buffer: (3.24, 4.0, 41_200_000),
            }),
            ..Overlay::default()
        };
        let r = rows(&o);
        assert_eq!(
            r[1],
            (
                "Network",
                "12.3 MB/s · 49 ms/read · 0 reconnects".into(),
                false
            )
        );
        assert_eq!(r[2], ("Buffer", "3.2 of 4 s · 41 MB".into(), false));
        o.network = Some(Network {
            mb_s: 0.0,
            read_ms: None,
            reconnects: 5,
            buffer: (0.2, 4.0, 0),
        });
        let r = rows(&o);
        assert_eq!(r[1], ("Network", "0.0 MB/s · 5 reconnects".into(), true));
        assert_eq!(r[2], ("Buffer", "0.2 of 4 s · 0 MB".into(), true));
    }
}
