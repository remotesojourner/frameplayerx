//! Cropping a flat video's black bars: a picture letterboxed or pillarboxed
//! in its frame gets a screen of its own shape. The bars are screen glow's
//! ([`crate::glow`]), measured whether or not the glow shows, and taken
//! more carefully than the glow takes them: only once the same bars have
//! held for two seconds, and given up as soon as the picture reaches into
//! them (an IMAX scene, subtitles in the bar), for the rest of the video.

use std::time::Instant;

/// The deepest bar searched for, a share of the picture (`GLOW_BAR_MAX`).
const BAR_MAX: f32 = 0.25;
/// Bars on opposite sides closer than this are a pair, and measurements
/// this close the same.
const BAR_MATCH: f32 = 0.01;
/// Measurements (50 a second) the same bars hold before the picture is
/// cropped to them: two seconds.
const TAKE_AFTER: u32 = 100;
/// Measurements in a row that find picture inside the crop before it gives
/// way, so one odd frame doesn't.
const REVEAL_AFTER: u32 = 3;
/// The crop moves about two thirds of the way to a new size in this time:
/// the screen changes shape over about 0.3 s.
const EASE_SECS: f32 = 0.1;

#[derive(Debug, Default)]
pub(crate) struct Crop {
    on: bool,
    /// The bars measured last, left and right then top and bottom, and how
    /// many measurements in a row found them.
    candidate: [f32; 2],
    seen: u32,
    /// Measurements in a row that found picture inside the crop.
    inside: u32,
    /// What the picture is cropped by, each side, left and right then top
    /// and bottom.
    taken: [f32; 2],
    /// The picture has reached into the bars: no more crop is taken.
    revealed: bool,
    shown: [f32; 2],
    eased_at: Option<Instant>,
}

impl Crop {
    /// Turns cropping on or off; the screen eases back to the whole frame.
    pub fn set(&mut self, on: bool) {
        if on != self.on {
            self.on = on;
            self.forget();
        }
    }

    pub fn wanted(&self) -> bool {
        self.on
    }

    /// Starts afresh, for a new video: the whole frame at once.
    pub fn reset(&mut self) {
        self.forget();
        self.shown = [0.0; 2];
        self.eased_at = None;
    }

    fn forget(&mut self) {
        (self.candidate, self.seen, self.inside) = ([0.0; 2], 0, 0);
        (self.taken, self.revealed) = ([0.0; 2], false);
    }

    /// Takes a measurement: how deep the dark rows run in from the top,
    /// bottom, left and right.
    pub fn measured(&mut self, [top, bottom, left, right]: [f32; 4]) {
        // A black frame says nothing about the bars.
        if top >= BAR_MAX && left >= BAR_MAX {
            return;
        }
        let pair = |a: f32, b: f32| {
            if (a - b).abs() <= BAR_MATCH {
                a.min(b)
            } else {
                0.0
            }
        };
        let now = [pair(left, right), pair(top, bottom)];
        if now.iter().zip(self.taken).any(|(n, t)| *n < t - BAR_MATCH) {
            self.inside += 1;
            if self.inside >= REVEAL_AFTER {
                self.taken = [self.taken[0].min(now[0]), self.taken[1].min(now[1])];
                self.revealed = true;
                self.inside = 0;
            }
        } else {
            self.inside = 0;
        }
        if self.revealed {
            return;
        }
        if now
            .iter()
            .zip(self.candidate)
            .all(|(a, b)| (a - b).abs() <= BAR_MATCH)
        {
            self.seen += 1;
        } else {
            (self.candidate, self.seen) = (now, 1);
        }
        if self.seen >= TAKE_AFTER {
            self.taken = [
                self.taken[0].max(self.candidate[0]),
                self.taken[1].max(self.candidate[1]),
            ];
        }
    }

    /// What to crop off each side at `now`, left and right then top and
    /// bottom, eased towards the crop taken.
    pub fn shown(&mut self, now: Instant) -> [f32; 2] {
        let target = if self.on { self.taken } else { [0.0; 2] };
        if let Some(last) = self.eased_at.replace(now) {
            let dt = now.saturating_duration_since(last).as_secs_f32();
            let k = 1.0 - (-dt / EASE_SECS).exp();
            for (shown, target) in self.shown.iter_mut().zip(target) {
                *shown += (target - *shown) * k;
            }
        }
        self.shown
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    /// A 2.39:1 film in a 16:9 frame: bars of 12.8% at the top and bottom.
    const SCOPE: [f32; 4] = [0.125, 0.125, 0.0, 0.0];
    const FULL: [f32; 4] = [0.0; 4];
    const BLACK: [f32; 4] = [BAR_MAX; 4];

    fn on() -> Crop {
        let mut c = Crop::default();
        c.set(true);
        c
    }

    fn feed(c: &mut Crop, bars: [f32; 4], times: u32) {
        for _ in 0..times {
            c.measured(bars);
        }
    }

    #[test]
    fn bars_that_hold_two_seconds_are_cropped() {
        let mut c = on();
        feed(&mut c, SCOPE, TAKE_AFTER - 1);
        assert_eq!(c.taken, [0.0, 0.0], "not yet");
        feed(&mut c, SCOPE, 1);
        assert_eq!(c.taken, [0.0, 0.125]);
        // Black frames, a fade, don't count either way.
        feed(&mut c, BLACK, 50);
        assert_eq!(c.taken, [0.0, 0.125]);
    }

    #[test]
    fn a_full_frame_opening_still_crops_the_film_after_it() {
        let mut c = on();
        feed(&mut c, FULL, 300);
        feed(&mut c, SCOPE, TAKE_AFTER);
        assert_eq!(c.taken, [0.0, 0.125]);
    }

    #[test]
    fn picture_in_the_bars_gives_the_crop_up_for_good() {
        let mut c = on();
        feed(&mut c, SCOPE, TAKE_AFTER);
        // One odd frame keeps it.
        feed(&mut c, FULL, REVEAL_AFTER - 1);
        feed(&mut c, SCOPE, 1);
        assert_eq!(c.taken, [0.0, 0.125]);
        // Subtitles in the bottom bar: the bars no longer pair.
        feed(&mut c, [0.125, 0.04, 0.0, 0.0], REVEAL_AFTER);
        assert_eq!(c.taken, [0.0, 0.0]);
        // The bars back for a long time: still shown whole.
        feed(&mut c, SCOPE, 10 * TAKE_AFTER);
        assert_eq!(c.taken, [0.0, 0.0]);
    }

    #[test]
    fn an_imax_scene_opens_the_crop_only_as_far_as_it_reaches() {
        let mut c = on();
        feed(&mut c, SCOPE, TAKE_AFTER);
        // 1.90:1 scenes: thinner bars.
        feed(&mut c, [0.03, 0.03, 0.0, 0.0], REVEAL_AFTER);
        assert_eq!(c.taken, [0.0, 0.03]);
    }

    #[test]
    fn off_or_reset_shows_the_whole_frame() {
        let mut c = on();
        feed(&mut c, SCOPE, TAKE_AFTER);
        let t = Instant::now();
        c.shown(t);
        let eased = c.shown(t + Duration::from_secs(2));
        assert!((eased[1] - 0.125).abs() < 1e-3, "{eased:?}");
        c.set(false);
        let off = c.shown(t + Duration::from_secs(4));
        assert!(off[1] < 1e-3, "{off:?}");
        c.set(true);
        feed(&mut c, SCOPE, TAKE_AFTER);
        c.reset();
        assert_eq!(c.shown(t + Duration::from_secs(5)), [0.0, 0.0]);
    }
}
