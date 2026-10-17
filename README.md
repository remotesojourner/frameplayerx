<p align="center">
  <img src="assets/logo.svg" alt="FramePlayerX Logo" width="160" />
</p>

# FramePlayerX

[![CI](https://github.com/remotesojourner/frameplayerx/actions/workflows/ci.yml/badge.svg?branch=main&event=push)](https://github.com/remotesojourner/frameplayerx/actions/workflows/ci.yml)
[![Latest Release](https://img.shields.io/github/v/release/remotesojourner/frameplayerx?sort=semver&style=flat&logo=github&label=release)](https://github.com/remotesojourner/frameplayerx/releases/latest)
[![Latest Beta](https://img.shields.io/github/v/release/remotesojourner/frameplayerx?include_prereleases&sort=date&style=flat&logo=github&label=beta&color=orange)](https://github.com/remotesojourner/frameplayerx/releases)
[![License: Apache-2.0](https://img.shields.io/badge/license-Apache--2.0-blue?style=flat)](LICENSE)

A VR video player for the Valve Steam Frame. It plays flat, 180°, 360° and fisheye videos, in 2D or 3D, from the headset, a microSD card, a USB drive or a media server. It runs natively on the headset.

> [!NOTE]
> FramePlayerX is a fork of [FramePlayer](https://github.com/yellkell/frameplayer) by [yellkell](https://github.com/yellkell), who created the original player and released it as open source. Thank you, yellkell, for the work this project is built on.

---

## Features

- **VR and flat videos**: plays 180°, 360°, fisheye and flat videos, in 2D or 3D. The format is detected automatically, and you can change it for any video
- **Picture adjustments**: position, zoom, depth and picture settings, saved for each video
- **Cinema**: flat videos can play on the screen of a cinema hall, lit by the picture
- **Videos on the headset**: finds videos in the Videos and Downloads folders and on microSD cards and USB drives, including cards formatted on Windows or a Mac
- **Media servers**: browse and play from Jellyfin, Emby, any server with a DeoVR feed or HereSphere API, network shares (SMB), WebDAV, DLNA servers and web folders. Jellyfin, Emby and HereSphere APIs hear what you watch, so their resume points, watched marks and play counts stay in step with the headset
- **Watch history**: carry on where you left off, and mark favorites and ratings
- **PIN locks**: hide a source behind a PIN until you unlock it
- **Passthrough videos**: see your room around the people in passthrough videos, and behind green or blue screens
- **Subtitles and spatial audio**: subtitles from a file or inside the video, picked in your language and shown in any script, and 360° audio that follows your head
- **Haptic devices**: optional, off until turned on; syncs supported haptic devices with videos that have scripts
- **Phone remote**: scan a QR code to control playback and browse from your phone. Apps made for DeoVR's remote work too
- **Custom controls**: choose what each controller button does
- **Updates**: checks for a new version when it starts, and installs it from Settings when you choose to, on a Stable or a Beta channel
- **No accounts or tracking**: it only connects to the servers you add, and to GitHub to check for updates

---

## Requirements

| Requirement | Notes |
|---|---|
| Valve Steam Frame | Developer Mode only to install with Frame Control, FrameDrop or a PC |
| A PC (Windows, macOS or Linux) | Optional: only to install from a PC |
| Media server | Optional: Jellyfin or Emby, a DeoVR feed or HereSphere API, a NAS share, WebDAV or DLNA |

---

## Quick Start

<p align="center">
  <a href="https://github.com/remotesojourner/frameplayerx/releases/latest/download/Install-FramePlayerX.desktop"><img src="assets/install-button.svg" alt="Install in Desktop Mode" width="240" /></a>
  <a href="https://framedropvr.com/install?manifest=https%3A%2F%2Fgithub.com%2Fremotesojourner%2Fframeplayerx%2Freleases%2Flatest%2Fdownload%2Fframedrop.json"><img src="assets/framedrop-button.svg" alt="Install with FrameDrop" width="240" /></a>
  <a href="https://remotesojourner.github.io/frameplayerx/frame-control.html"><img src="assets/frame-control-button.svg" alt="Install with Frame Control" width="240" /></a>
</p>

1. **Install FramePlayerX** with one of the buttons above, or from a terminal:

   - **Desktop Mode, on the headset**: open this page in the headset's browser and press **Desktop Mode** to download the installer. Open `Install-FramePlayerX.desktop` from your Downloads folder and confirm when Desktop Mode asks whether to trust it. A terminal window opens, installs the latest release and adds FramePlayerX to your Steam library. It may ask to close Steam for a moment; the app then shows up in Gaming Mode.

   - **FrameDrop or Frame Control, from a PC**: turn on Developer Mode (Steam Settings › System › Developer Mode) and pair the headset once with [FrameDrop](https://framedropvr.com) or [Frame Control](https://github.com/saphid/steam-frame). Then press its button in your PC's browser and it installs the latest release on the headset.

   - **`frameplayerx-install`, from a PC's terminal**: turn on Developer Mode, download `frameplayerx-install` for your PC from the [release page](https://github.com/remotesojourner/frameplayerx/releases/latest), then:

     ```bash
     frameplayerx-install pair 192.168.0.68    # once, with the headset's IP address
     frameplayerx-install                      # installs the latest release
     ```

     For `pair`, open Steam Settings › Developer › Pair new host on the headset and keep it showing.

2. **Start it** from the Steam library on the headset. FramePlayerX finds the videos on the headset and its cards and drives by itself. Add a media server under **Sources › Add source**.

To update later, open **Settings › Updates** and press **Download and install** when a new version is listed, then restart FramePlayerX. To remove it again, see [Uninstall](docs/USER-GUIDE.md#uninstall) in the user guide.

---

## Documentation

| Guide | What's in it |
|---|---|
| [User guide](docs/USER-GUIDE.md) | Installing, updating and uninstalling, sources, browsing, PIN locks, video formats, controls, passthrough, haptics, remote control and troubleshooting |
| [Development](docs/DEVELOPMENT.md) | Building and testing, how the code is organised, running without a headset (Monado and the preview harness), and releasing |

---

## Technology Stack

| Component | Technology |
|---|---|
| Language | Rust |
| VR | OpenXR, talking to the runtime directly (no Khronos loader) |
| Graphics | Vulkan through [ash](https://github.com/ash-rs/ash); the projections are drawn per pixel in a shader |
| Interface | [egui](https://github.com/emilk/egui) panels in VR, with the [Inter](https://rsms.me/inter/) font and [Phosphor](https://phosphoricons.com/) icons; subtitle text set by [cosmic-text](https://github.com/pop-os/cosmic-text) |
| Video | [FFmpeg](https://ffmpeg.org/) 7.1 for demuxing and software decoding, the Frame's V4L2 hardware decoder, [dav1d](https://code.videolan.org/videolan/dav1d) for AV1 |
| Audio | ALSA |
| Storage | SQLite (videos, watch history, adjustments), JSON settings |
| Builds | `cargo zigbuild` for Linux ARM64 against glibc 2.28 |

---

## License

FramePlayerX is licensed under the [Apache License, Version 2.0](LICENSE). It's a modified version of FramePlayer by yellkell; see [NOTICE](NOTICE). Bundled third-party libraries and fonts keep their own licences, and the controller pictures are renders of Valve's Steam Frame controller models; see [packaging/licenses](packaging/licenses/THIRD-PARTY.txt).

FramePlayerX is an independent project, not affiliated with or endorsed by Valve. Steam and Steam Frame are trademarks of Valve Corporation.

Unless you explicitly state otherwise, any contribution intentionally submitted for inclusion in FramePlayerX by you shall be licensed under the Apache License, Version 2.0, without any additional terms or conditions.

---

## FAQ

### Does it run on other headsets?

No. FramePlayerX is built only for the Steam Frame, so it can use the Frame's decoder, controllers and runtime directly instead of carrying everything a cross-platform player needs.

---

### Why does a video stutter?

Usually because it's 10-bit HEVC, 4:2:2 or AV1, which the Frame's hardware decoder can't play, so it's decoded on the CPU. The control bar says so. Converting it to 8-bit HEVC once on a PC makes it play smoothly; the [user guide](docs/USER-GUIDE.md#videos-that-stutter-10-bit-hevc-and-av1) has the commands.

---

### Does it work with my media server or NAS?

Yes, if it offers one of these: Jellyfin or Emby (sign in with Quick Connect or a password), a DeoVR feed or HereSphere API, SMB, WebDAV or DLNA. Add it under Sources › Add source.

---

### Will it be released on Steam?

Not for now. I develop FramePlayerX for myself in my free time, and I don't see the point in paying Steam's $100 fee to publish it. If donations through [GitHub Sponsors](https://github.com/sponsors/remotesojourner) ever reach that amount, I'll spend them on releasing it on Steam. I don't expect any, though: it's an app built with the help of AI to solve an immediate need of my own. Meanwhile, the installers in [Quick Start](#quick-start) add it to your Steam library like a game.

---

### Why a fork instead of contributing to FramePlayer?

FramePlayer reached Jellyfin, where most of the films I wanted to watch are, only through DLNA, and I wanted to use it directly. It also didn't support half side-by-side videos. I also had a different idea for browsing sources than its library screen, and while FramePlayer focuses on adult videos, I wanted a player for every kind of media. Together that made a different app rather than a few changes to the original.

---

### Do you plan to add new features?

If I come across a new idea or receive a suggestion that fits a VR video player for the Frame, I will consider adding it, but there are no promises: this project exists to solve my own needs first.

---

## A Note on AI

This project was built with extensive help from [Claude Code](https://claude.com/claude-code).
