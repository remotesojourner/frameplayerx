<div align="center">

<img src="assets/logo.svg" alt="FramePlayerX Logo" width="160" />

# FramePlayerX

[![CI](https://github.com/remotesojourner/frameplayerx/actions/workflows/ci.yml/badge.svg?branch=main&event=push)](https://github.com/remotesojourner/frameplayerx/actions/workflows/ci.yml)
[![Latest Release](https://img.shields.io/github/v/release/remotesojourner/frameplayerx?sort=semver&style=flat&logo=github&label=release)](https://github.com/remotesojourner/frameplayerx/releases/latest)
[![Latest Beta](https://img.shields.io/github/v/release/remotesojourner/frameplayerx?include_prereleases&sort=date&style=flat&logo=github&label=beta&color=orange)](https://github.com/remotesojourner/frameplayerx/releases)
[![License: Apache-2.0](https://img.shields.io/badge/license-Apache--2.0-blue?style=flat)](LICENSE)

A VR video player for the Valve Steam Frame. It plays flat, 180°, 360° and fisheye videos, in 2D or 3D, from the headset, a microSD card, a USB drive or a media server. It runs natively on the headset and plays 8K video smoothly with the display at 120 Hz.

<a href="https://frameplayerx.org.uk/"><img src="assets/website-button.svg" alt="Website: frameplayerx.org.uk" width="220" /></a>&nbsp;&nbsp;
<a href="https://discord.gg/tDZRuZDY7d"><img src="assets/discord-button.svg" alt="Discord: join the server" width="220" /></a>

</div>

---

## Features

- **Video formats**: flat, 180°, 360° and fisheye videos in 2D or 3D, 3D Blu-rays and interlaced video, detected automatically
- **Hardware decoding**: HEVC and H.264 up to 8K, and AV1 on SteamOS 0.5.10 and later, on the Frame's own video decoder
- **Performance**: foveated rendering that follows your eyes keeps 8K 10-bit video at full frame rate, with the CPU almost idle
- **HDR**: HDR10 and HLG videos fitted to the Frame's screen
- **Cinema**: a full-size cinema for flat videos, with house lights that dim as the film starts
- **Your videos**: on the headset, a microSD card or a USB drive, network shares, Jellyfin, Emby, Plex and any server with a DeoVR feed or HereSphere API, with downloads to watch offline
- **Watch history, Up next and playlists**: carry on where you left off, queue videos and keep playlists from any source
- **Subtitles**: from a file, inside the video or found on OpenSubtitles from the player
- **Adjustments**: position, depth, picture and comfort focus for each video, and a screen that follows you lying down
- **Passthrough videos**: your room around the people in passthrough videos and behind green or blue screens
- **Controls**: choose what each button does, go hands-free, or use your phone as a remote
- **No FramePlayerX account or tracking**: it connects only to the servers and services you set up or use, and to check for updates

Every feature, in more detail, is on the website's [Features page](https://frameplayerx.org.uk/features).

---

## Requirements

| Requirement | Notes |
|---|---|
| Valve Steam Frame | Developer Mode only to install with Frame Control or a PC |
| A PC (Windows, macOS or Linux) | Optional: only to install from a PC |
| Media server | Optional: Jellyfin, Emby or Plex, a DeoVR feed or HereSphere API, a NAS share, an SFTP server, WebDAV or DLNA |

---

## Quick Start

<p align="center">
  <a href="https://github.com/remotesojourner/frameplayerx/releases/latest/download/Install-FramePlayerX.desktop"><img src="assets/install-button.svg" alt="Install in Desktop Mode" width="240" /></a>
  <a href="https://frameplayerx.org.uk/frame-control"><img src="assets/frame-control-button.svg" alt="Install with Frame Control" width="240" /></a>
</p>

1. **Install FramePlayerX** with one of the buttons above, or from a terminal:

   - **Desktop Mode, on the headset**: open this page in the headset's browser and press **Desktop Mode** to download the installer. Open `Install-FramePlayerX.desktop` from your Downloads folder and confirm when Desktop Mode asks whether to trust it. A terminal window opens, installs the latest release and adds FramePlayerX to your Steam library. It may ask to close Steam for a moment; the app then shows up in Gaming Mode.

   - **Frame Control from a PC**: turn on Developer Mode (Steam Settings › System › Developer Mode) and pair the headset once with [Frame Control](https://github.com/saphid/frame-control). Then press its button in your PC's browser and it installs the latest release on the headset.

   - **`frameplayerx-install`, from a PC's terminal**: turn on Developer Mode, download `frameplayerx-install` for your PC from the [release page](https://github.com/remotesojourner/frameplayerx/releases/latest), then:

     ```bash
     frameplayerx-install pair 192.168.0.68    # once, with the headset's IP address
     frameplayerx-install                      # installs the latest release
     ```

     For `pair`, open Steam Settings › Developer › Pair new host on the headset and keep it showing.

2. **Start it** from the Steam library on the headset. Browse the headset and its cards under **On this device**, and add a library of a folder, or a media server, under **Sources › Add source**.

To update later, open **Settings › Updates** and press **Download and install** when a new version is listed, then **Restart now**. To remove it again, see [Uninstall](docs/USER-GUIDE.md#uninstall) in the user guide.

---

## Documentation

| Guide | What's in it |
|---|---|
| [User guide](docs/USER-GUIDE.md), also [on the website](https://frameplayerx.org.uk/guide) | Installing, updating and uninstalling, sources, browsing and playlists, video formats and adjustments, subtitles, hardware decoding, display settings, controls, passthrough, haptics, remote control and troubleshooting |
| [Development](docs/DEVELOPMENT.md) | Building and testing, how the code is organised, running without a headset (Monado and the preview harness), and releasing |
| [Contributing](CONTRIBUTING.md) | What to do before opening a pull request, what it has to say and how to test it |

---

## Help translate

FramePlayerX comes in English, German, Spanish, French, Japanese and Simplified Chinese. All but English started as machine drafts, so some wording is likely to be wrong or awkward, and corrections are welcome.

- **Correct or translate** on [Codeberg Translate](https://translate.codeberg.org/projects/frameplayerx/), Codeberg's Weblate. No Git needed: sign in with a Codeberg account, pick a language and edit its messages. A note beside each message says where it shows; "Short" marks a button, chip or tab, which should stay about as long as the English. Changes come back to this repository as pull requests.
- **Report a mistake** without translating: open a [Translation problem](https://github.com/remotesojourner/frameplayerx/issues/new?template=translation.yml) issue with the language, where it shows and the wording you'd expect.
- **Add a language**: start it on Codeberg Translate, or ask in an issue. Right-to-left languages (Arabic, Hebrew) and scripts that need shaping (Thai, Hindi and other Indic scripts) can't be shown in the interface yet.

---

## Technology Stack

| Component | Technology |
|---|---|
| Language | Rust |
| VR | OpenXR, talking to the runtime directly (no Khronos loader) |
| Graphics | Vulkan through [ash](https://github.com/ash-rs/ash); the projections are drawn per pixel in a shader, straight from the decoder's buffers, and foveated through variable-rate shading guided by eye tracking |
| Interface | [egui](https://github.com/emilk/egui) panels in VR, with the [Inter](https://rsms.me/inter/) font and [Phosphor](https://phosphoricons.com/) icons; subtitle text set by [cosmic-text](https://github.com/pop-os/cosmic-text) |
| Video | [FFmpeg](https://ffmpeg.org/) 7.1 for demuxing and software decoding, the Frame's V4L2 hardware decoder (H.264, 8- and 10-bit HEVC, and 8- and 10-bit AV1 on SteamOS 0.5.10+), [dav1d](https://code.videolan.org/videolan/dav1d) for software AV1 fallback |
| Audio | ALSA |
| Storage | SQLite (videos, watch history, adjustments), JSON settings |
| Builds | `cargo zigbuild` for Linux ARM64 against glibc 2.28 |

---

## License

FramePlayerX is licensed under the [Apache License, Version 2.0](LICENSE). It's a modified version of FramePlayer by yellkell; see [NOTICE](NOTICE). Bundled third-party libraries and fonts keep their own licences, and the controller pictures are renders of Valve's Steam Frame controller models; see [packaging/licenses](packaging/licenses/THIRD-PARTY.txt).

FramePlayerX is an independent project, not affiliated with or endorsed by Valve. Steam and Steam Frame are trademarks of Valve Corporation.

Unless you explicitly state otherwise, any contribution intentionally submitted for inclusion in FramePlayerX by you shall be licensed under the Apache License, Version 2.0, without any additional terms or conditions.

---

## Credits

FramePlayerX began as a fork of [FramePlayer](https://github.com/yellkell/frameplayer) by [yellkell](https://github.com/yellkell), who created the original player and released it as open source. Thank you, yellkell, for the work this project is built on.

### Acknowledgements

- Reading hardware-decoded frames straight from the decoder's buffers, without copying them, follows [VideoFrameVR](https://github.com/seaweeduk/VideoFrameVR)'s approach ([#122](https://github.com/remotesojourner/frameplayerx/pull/122))
- HDR tone mapping is modelled on [mpv](https://mpv.io)'s: the BT.2390 curve, fitted to each scene's measured peak
- [Frame Control](https://github.com/saphid/frame-control) by saphid installs FramePlayerX from a PC, and FramePlayerX's install links follow its format
- The DeoVR remote API, the DeoVR feed and the HereSphere API come from those players; FramePlayerX implements them so the servers and phone apps made for them work with it
- [FFmpeg](https://ffmpeg.org) and [dav1d](https://code.videolan.org/videolan/dav1d) decode the video, and the libraries and fonts in [Technology Stack](#technology-stack) do much of the rest. FramePlayerX uses code of FFmpeg licensed under the LGPL 2.1 or later; where to download its source, and every bundled component with its licence, is in [packaging/licenses](packaging/licenses/THIRD-PARTY.txt)
- The Luxe cinema's recliners are [Recliner Brown_Leather](https://sketchfab.com/3d-models/recliner-brown-leather-4e3e06cf74fe48c8b7472a3549f6b5d2) by [Puneet Sharma](https://sketchfab.com/Eshi_Nito), licensed under [CC-BY 4.0](https://creativecommons.org/licenses/by/4.0/)
- The pictures and videos in the screenshots are AI-generated

### Players that inspired FramePlayerX

Thanks to the developers of these players for showing what the Steam Frame can do. FramePlayerX is written independently, but their work spurred us on, and they're well worth a try:

- [MatineeVR](https://embedding-shapes.itch.io/matineevr) by embedding-shapes
- [VideoFrameVR](https://github.com/seaweeduk/VideoFrameVR) by seaweeduk

---

## FAQ

### Does it run on other headsets?

No. FramePlayerX is built only for the Steam Frame, so it can use the Frame's decoder, controllers and runtime directly instead of carrying everything a cross-platform player needs.

---

### Which videos play best?

HEVC (H.265), H.264, and AV1 (on SteamOS 0.5.10 and later): the Frame's hardware decoder plays them smoothly up to 8K. The Frame has no hardware decoder for 4:2:2 video (or AV1 on earlier SteamOS versions), so those play on the CPU, which can fall behind at VR sizes; a pill on the control bar shows when a video plays this way. Converted once to HEVC on a PC, they play as smoothly as the rest; the [user guide](docs/USER-GUIDE.md#hardware-decoding) has the commands.

---

### Does it work with my media server or NAS?

Yes, if it offers one of these: Jellyfin (sign in with Quick Connect or a password), Emby (a password), Plex (sign in with a linking code or an access token), a DeoVR feed or HereSphere API, SMB, SFTP, NFS, WebDAV or DLNA. Add it under Sources › Add source.

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

## Screenshots

| Home | Player & adjustments |
| :---: | :---: |
| ![Home screen](assets/screenshots/home-watched.png) | ![Player and adjustments](assets/screenshots/player.png) |
| **Video library** | **Jellyfin server** |
| ![Video library](assets/screenshots/library.png) | ![Jellyfin server](assets/screenshots/jellyfin.png) |
| **Local folders** | **Video details** |
| ![Local folders](assets/screenshots/library-folders.png) | ![Video details](assets/screenshots/details.png) |
| **Up next queue** | **Controller settings** |
| ![Up next queue](assets/screenshots/up-next.png) | ![Controller settings](assets/screenshots/settings-controller.png) |

---

## A Note on AI

This project was built with extensive help from [Claude Code](https://claude.com/claude-code).
