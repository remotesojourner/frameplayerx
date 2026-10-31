//! Other programs holding the Frame's hardware video decoder, and closing
//! them on request.
//!
//! The iris driver lowers what it admits for each session open on it, idle
//! ones too: with Steam's interface (`steamwebhelper`, whose Chromium decodes
//! store videos on V4L2) holding two, an 8192x4096 stream gets room for
//! 8192x2720 only (measured on a Frame). Ending that process frees its
//! sessions; Steam starts its interface again at once, without them.

use std::path::Path;

/// The decoder's device node; `/dev/video-dec0` links to it.
const DECODER_LINK: &str = "/dev/video-dec0";

/// A process with the decoder open.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Holder {
    pub pid: u32,
    /// The executable's file name (`steamwebhelper`).
    pub name: String,
    /// Open handles on the decoder: each is a session the driver counts.
    pub sessions: usize,
    pub kind: Kind,
}

/// What a holder is, for what to say about closing it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// Steam's interface: Steam restarts it at once.
    SteamInterface,
    /// SteamVR's streaming or compositor: closing it ends the VR session,
    /// FramePlayerX's with it, so it is never offered.
    SteamVr,
    Other,
}

impl Holder {
    /// Whether FramePlayerX offers to close it.
    pub fn closable(&self) -> bool {
        self.kind != Kind::SteamVr
    }
}

fn kind_of(name: &str, cmdline: &str) -> Kind {
    if name == "steamwebhelper" {
        Kind::SteamInterface
    } else if cmdline.contains("/steamvr/") || name.starts_with("vr") {
        Kind::SteamVr
    } else {
        Kind::Other
    }
}

/// The decoder's device path (`/dev/video22`), if it has one.
fn decoder_device() -> Option<std::path::PathBuf> {
    std::fs::canonicalize(DECODER_LINK).ok()
}

/// Processes other than this one with the decoder open, from `/proc`. Those
/// of other users can't be read and are left out.
pub fn find() -> Vec<Holder> {
    let Some(device) = decoder_device() else {
        return Vec::new();
    };
    let me = std::process::id();
    let Ok(procs) = std::fs::read_dir("/proc") else {
        return Vec::new();
    };
    let mut out: Vec<Holder> = procs
        .flatten()
        .filter_map(|e| e.file_name().to_str()?.parse::<u32>().ok())
        .filter(|&pid| pid != me)
        .filter_map(|pid| holder(Path::new(&format!("/proc/{pid}")), pid, &device))
        .collect();
    out.sort_by(|a, b| b.sessions.cmp(&a.sessions).then(a.pid.cmp(&b.pid)));
    out
}

fn holder(proc_dir: &Path, pid: u32, device: &Path) -> Option<Holder> {
    let sessions = std::fs::read_dir(proc_dir.join("fd"))
        .ok()?
        .flatten()
        .filter(|fd| std::fs::read_link(fd.path()).is_ok_and(|t| t == device))
        .count();
    if sessions == 0 {
        return None;
    }
    let cmdline = std::fs::read(proc_dir.join("cmdline"))
        .map(|b| String::from_utf8_lossy(&b).replace('\0', " "))
        .unwrap_or_default();
    let name = std::fs::read_link(proc_dir.join("exe"))
        .ok()
        .and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
        .or_else(|| {
            std::fs::read_to_string(proc_dir.join("comm"))
                .ok()
                .map(|s| s.trim().to_string())
        })
        .unwrap_or_else(|| pid.to_string());
    let kind = kind_of(&name, &cmdline);
    Some(Holder {
        pid,
        name,
        sessions,
        kind,
    })
}

/// Asks the closable `holders` to end (SIGTERM), then waits up to two
/// seconds for their sessions to close; true when every one asked is gone.
pub fn close(holders: &[Holder]) -> bool {
    let asked: Vec<u32> = holders
        .iter()
        .filter(|h| h.closable())
        .map(|h| h.pid)
        .collect();
    for &pid in &asked {
        log::info!("closing pid {pid} to free the hardware decoder");
        // SAFETY: kill(2) with a pid read from /proc and a plain signal; a
        // pid that has gone away since only makes it fail with ESRCH.
        if unsafe { libc::kill(pid as libc::pid_t, libc::SIGTERM) } != 0 {
            log::warn!(
                "cannot close pid {pid}: {}",
                std::io::Error::last_os_error()
            );
        }
    }
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
    loop {
        let left: Vec<u32> = find()
            .into_iter()
            .map(|h| h.pid)
            .filter(|p| asked.contains(p))
            .collect();
        if left.is_empty() {
            return true;
        }
        if std::time::Instant::now() >= deadline {
            log::warn!("decoder still held by {left:?}");
            return false;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
}

/// "steamwebhelper (pid 4351, 2 sessions)" for the log.
pub fn describe(holders: &[Holder]) -> String {
    if holders.is_empty() {
        return "no other process".into();
    }
    holders
        .iter()
        .map(|h| format!("{} (pid {}, {} session(s))", h.name, h.pid, h.sessions))
        .collect::<Vec<_>>()
        .join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn steam_interface_is_closable_and_steamvr_is_not() {
        assert_eq!(
            kind_of("steamwebhelper", "steamwebhelper --type=zygote"),
            Kind::SteamInterface
        );
        assert_eq!(
            kind_of("vrcompositor", "/opt/steamvr/bin/linuxarm64/vrcompositor"),
            Kind::SteamVr
        );
        assert_eq!(kind_of("mpv", "mpv video.mp4"), Kind::Other);
        let h = |kind| Holder {
            pid: 1,
            name: String::new(),
            sessions: 1,
            kind,
        };
        assert!(h(Kind::SteamInterface).closable());
        assert!(h(Kind::Other).closable());
        assert!(!h(Kind::SteamVr).closable());
    }

    #[test]
    fn a_process_holding_a_file_is_found_by_its_fds() {
        // This test process stands in: it opens a file and is looked up
        // through its own /proc entry as if that file were the decoder.
        let dir = std::env::temp_dir().join(format!("fpx-holders-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("video99");
        std::fs::write(&path, b"").unwrap();
        let device = std::fs::canonicalize(&path).unwrap();
        let _a = std::fs::File::open(&device).unwrap();
        let _b = std::fs::File::open(&device).unwrap();
        let me = std::process::id();
        let h = holder(Path::new("/proc/self"), me, &device).unwrap();
        assert_eq!(h.sessions, 2);
        assert_eq!(h.pid, me);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
