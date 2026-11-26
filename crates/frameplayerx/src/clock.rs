//! The time of day in the system's time zone (`TZ`, else `/etc/localtime`),
//! on a 12- or 24-hour clock as Settings › General has it.

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

static TWENTY_FOUR_HOUR: AtomicBool = AtomicBool::new(false);

/// Shows the time on a 24-hour clock, else on a 12-hour one.
pub fn set_24_hour(on: bool) {
    TWENTY_FOUR_HOUR.store(on, Ordering::Relaxed);
}

/// Seconds since the Unix epoch, now.
pub fn unix_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64)
}

/// The time of day `unix` seconds after the epoch ("22:05", "10:05 PM").
pub fn time_at(unix: i64) -> String {
    let twenty_four = TWENTY_FOUR_HOUR.load(Ordering::Relaxed);
    local(unix).map_or_else(String::new, |tm| write(twenty_four, tm.tm_hour, tm.tm_min))
}

fn local(unix: i64) -> Option<libc::tm> {
    let time = unix as libc::time_t;
    // SAFETY: tm is plain data, for which all zeros is a value.
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    // SAFETY: both pointers are to locals that outlive the call.
    let done = unsafe { libc::localtime_r(&time, &mut tm) };
    (!done.is_null()).then_some(tm)
}

fn write(twenty_four: bool, hour: i32, minute: i32) -> String {
    if twenty_four {
        return format!("{hour:02}:{minute:02}");
    }
    let mark = if hour < 12 { "AM" } else { "PM" };
    let hour = match hour % 12 {
        0 => 12,
        h => h,
    };
    format!("{hour}:{minute:02} {mark}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_both_clocks() {
        assert_eq!(write(true, 0, 5), "00:05");
        assert_eq!(write(true, 22, 5), "22:05");
        assert_eq!(write(false, 0, 5), "12:05 AM");
        assert_eq!(write(false, 9, 30), "9:30 AM");
        assert_eq!(write(false, 12, 0), "12:00 PM");
        assert_eq!(write(false, 22, 5), "10:05 PM");
    }
}
