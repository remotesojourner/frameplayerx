//! The time of day as the system shows it: in its time zone (`TZ`, else
//! `/etc/localtime`) and on its locale's 12- or 24-hour clock (`LC_ALL`,
//! `LC_TIME`, else `LANG`).

use std::ffi::CStr;
use std::sync::OnceLock;
use std::time::{SystemTime, UNIX_EPOCH};

/// How the locale writes the hours.
#[derive(Clone, Debug, PartialEq)]
enum Hours {
    TwentyFour,
    /// With the locale's AM and PM.
    Twelve {
        am: String,
        pm: String,
    },
}

/// Seconds since the Unix epoch, now.
pub fn unix_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64)
}

/// The time of day `unix` seconds after the epoch ("22:05", "10:05 PM").
pub fn time_at(unix: i64) -> String {
    local(unix).map_or_else(String::new, |tm| write(hours(), tm.tm_hour, tm.tm_min))
}

fn hours() -> &'static Hours {
    static HOURS: OnceLock<Hours> = OnceLock::new();
    HOURS.get_or_init(|| locale_hours().unwrap_or(Hours::TwentyFour))
}

/// Read with a locale object of its own rather than `setlocale`, which
/// would change the whole process's.
fn locale_hours() -> Option<Hours> {
    // SAFETY: the locale is used only here and freed once read, and
    // nl_langinfo_l's strings are copied before that.
    unsafe {
        let locale = libc::newlocale(libc::LC_TIME_MASK, c"".as_ptr(), std::ptr::null_mut());
        if locale.is_null() {
            return None;
        }
        let item = |item| {
            CStr::from_ptr(libc::nl_langinfo_l(item, locale))
                .to_string_lossy()
                .into_owned()
        };
        let hours = if twelve_hour(&item(libc::T_FMT)) {
            Hours::Twelve {
                am: item(libc::AM_STR),
                pm: item(libc::PM_STR),
            }
        } else {
            Hours::TwentyFour
        };
        libc::freelocale(locale);
        Some(hours)
    }
}

/// The locale's time format (`T_FMT`) counts the hours to 12.
fn twelve_hour(format: &str) -> bool {
    let mut chars = format.chars();
    while let Some(c) = chars.next() {
        if c != '%' {
            continue;
        }
        // Past any flags, width and E or O modifier to the conversion.
        let conversion = chars
            .by_ref()
            .find(|c| !matches!(c, '-' | '_' | '0'..='9' | '^' | '#' | 'E' | 'O'));
        if matches!(conversion, Some('I' | 'l' | 'r' | 'p' | 'P')) {
            return true;
        }
    }
    false
}

fn local(unix: i64) -> Option<libc::tm> {
    let time = unix as libc::time_t;
    // SAFETY: tm is plain data, for which all zeros is a value.
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    // SAFETY: both pointers are to locals that outlive the call.
    let done = unsafe { libc::localtime_r(&time, &mut tm) };
    (!done.is_null()).then_some(tm)
}

fn write(hours: &Hours, hour: i32, minute: i32) -> String {
    match hours {
        Hours::TwentyFour => format!("{hour:02}:{minute:02}"),
        Hours::Twelve { am, pm } => {
            let mark = if hour < 12 { am } else { pm };
            let hour = match hour % 12 {
                0 => 12,
                h => h,
            };
            format!("{hour}:{minute:02} {mark}").trim_end().to_string()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hours_follow_the_locale_format() {
        // en_US, its T_FMT_AMPM, en_GB and C, ja_JP.
        assert!(twelve_hour("%r"));
        assert!(twelve_hour("%I:%M:%S %p"));
        assert!(twelve_hour("%-l:%M %P"));
        assert!(!twelve_hour("%T"));
        assert!(!twelve_hour("%H:%M:%S"));
        assert!(!twelve_hour("%H時%M分%S秒"));
        assert!(!twelve_hour("100%"));
    }

    #[test]
    fn writes_both_clocks() {
        let twelve = Hours::Twelve {
            am: "AM".into(),
            pm: "PM".into(),
        };
        assert_eq!(write(&Hours::TwentyFour, 0, 5), "00:05");
        assert_eq!(write(&Hours::TwentyFour, 22, 5), "22:05");
        assert_eq!(write(&twelve, 0, 5), "12:05 AM");
        assert_eq!(write(&twelve, 9, 30), "9:30 AM");
        assert_eq!(write(&twelve, 12, 0), "12:00 PM");
        assert_eq!(write(&twelve, 22, 5), "10:05 PM");
        let unmarked = Hours::Twelve {
            am: String::new(),
            pm: String::new(),
        };
        assert_eq!(write(&unmarked, 22, 5), "10:05");
    }
}
