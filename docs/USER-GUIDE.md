# FramePlayerX user guide

FramePlayerX plays flat, 180°, 360°, fisheye and YouTube-style (EAC) videos,
2D or 3D, on the Steam Frame, from the headset, a microSD card, a USB drive
or your network.

- [Install](#install), [updating](#updating) and [uninstall](#uninstall)
- [Where your videos can be](#where-your-videos-can-be): browsing,
  PIN locks, microSD cards and USB drives
- [Video formats](#video-formats), [fisheye lenses](#fisheye-lenses) and
  [videos that stutter](#videos-that-stutter-av1-and-422)
- [Controls](#controls), and [watching lying down](#watching-lying-down)
- [Passthrough videos](#passthrough-videos)
- [Haptics](#haptics)
- [Remote control](#remote-control)
- [Troubleshooting](#troubleshooting)

## Install

**On the headset, in Desktop Mode (no PC).** Open this page in the
headset's browser and press the button to download the installer:

<a href="https://github.com/remotesojourner/frameplayerx/releases/latest/download/Install-FramePlayerX.desktop"><img src="../assets/install-button.svg" alt="Install in Desktop Mode" width="240" /></a>

Open `Install-FramePlayerX.desktop` from your Downloads folder and confirm
when Desktop Mode asks whether to trust it. A terminal window opens and:

- downloads the latest stable release and checks it against the checksum
  GitHub recorded for it;
- removes other copies of FramePlayerX and their library entries (your
  settings and watch history stay);
- installs it into `~/frameplayerx` and adds **FramePlayerX** to the Steam
  library with its artwork.

If Steam can't be updated while it's running, the installer asks to close
it; the app then appears when you return to Gaming Mode, which it offers to
do. Run the same file again to reinstall. From a terminal, the installer
also takes `--beta` (the newest release, betas included); see
[tools/frame-desktop-install](../tools/frame-desktop-install/README.md).

The other two ways need **Developer Mode** on, and the headset paired once
with your PC by `frameplayerx-install pair` (below), Frame Control,
FrameDrop or Valve's SteamOS Devkit Client.

**With FrameDrop or Frame Control, from a PC.** With the app running on
your PC and the headset paired, press the button in your PC's browser:

<a href="https://framedropvr.com/install?manifest=https%3A%2F%2Fgithub.com%2Fremotesojourner%2Fframeplayerx%2Freleases%2Flatest%2Fdownload%2Fframedrop.json"><img src="../assets/framedrop-button.svg" alt="Install with FrameDrop" width="240" /></a>
<a href="https://remotesojourner.github.io/frameplayerx/frame-control.html"><img src="../assets/frame-control-button.svg" alt="Install with Frame Control" width="240" /></a>

GitHub doesn't keep links that open apps, so the Frame Control button opens a
small page that opens Frame Control. If that doesn't work, paste this link
into the browser's address bar instead:

```
frame-control://install?manifest=https%3A%2F%2Fgithub.com%2Fremotesojourner%2Fframeplayerx%2Freleases%2Flatest%2Fdownload%2Fframeplayerx.json
```

Either app reads the release's manifest (`framedrop.json` or
`frameplayerx.json`), downloads the latest release, checks its SHA-256 and
unpacks it into `~/frameplayerx`.

**From a PC with the installer.** Download `frameplayerx-install` for your
PC from the release page and run it:

```
frameplayerx-install pair 192.168.0.68       # once: the headset's IP address
frameplayerx-install                         # latest release
frameplayerx-install --channel beta          # newest release, betas too
frameplayerx-install --zip frameplayerx-0.1.0-aarch64.zip
frameplayerx-install status | uninstall
```

To pair, open Steam Settings › Developer › Pair new host on the headset
and keep that screen showing while `pair` runs. It creates an RSA key
(`~/.ssh/id_rsa_frame_devkit`; the headset does not accept ed25519 keys),
registers it with the headset and adds a `Host frame` entry to
`~/.ssh/config`. If you skip this step, `frameplayerx-install` offers to
pair when it cannot log in.

It reuses the `frame` SSH alias created when pairing, installs into
`~/frameplayerx` (keeping the previous version as `~/frameplayerx.old`) and
adds **FramePlayerX** to the Steam library with artwork. Add
`--restart-steam` to make it show up right away.

**By hand.** Copy the zip to the headset, unzip it into your home folder
and add `~/frameplayerx/frameplayerx.sh` to Steam as a non-Steam game.

## Updating

FramePlayerX checks for a new version each time it starts, but doesn't
install it by itself. To update, open **Settings › Updates**: when a new
version is out it's listed there with what changed. Press **Download and
install**, then **Restart now**. **Check now** looks again without
restarting.

The **Stable** channel gets finished releases only; **Beta** also gets
test versions before they are released. Switch off **Check for updates at
start** to only check when you press **Check now**. Installing again with
any of the ways above also updates it.

## Uninstall

Uninstalling removes the app, its entry in the Steam library and its
library artwork. You choose whether your settings, source passwords, watch
history and thumbnails go too; kept, a later install picks up where you
left off. Your videos are never touched.

**On the headset, in Desktop Mode (no PC).** Open this page in the
headset's browser and press the button to download the uninstaller:

<a href="https://github.com/remotesojourner/frameplayerx/releases/latest/download/Uninstall-FramePlayerX.desktop"><img src="../assets/uninstall-button.svg" alt="Uninstall in Desktop Mode" width="240" /></a>

Open `Uninstall-FramePlayerX.desktop` from your Downloads folder and
confirm when Desktop Mode asks whether to trust it. A terminal window
opens, asks you to confirm, removes FramePlayerX, then asks whether to
delete your settings and watch history too. If it closes Steam, the entry
is gone when you return to Gaming Mode, which it offers to do.

From Konsole, the same installer's `--remove` does this, and
`--remove --purge` deletes your settings and watch history without asking:

```
curl -fsSL https://github.com/remotesojourner/frameplayerx/releases/latest/download/frameplayerx-desktop-install.py -o /tmp/frameplayerx-desktop-install.py
python3 /tmp/frameplayerx-desktop-install.py --remove
```

**From a PC.** With the headset still paired (Developer Mode on):

```
frameplayerx-install uninstall                  # keeps your settings and watch history
frameplayerx-install uninstall --purge          # deletes them too
```

Add `--restart-steam` so the library updates right away.

**Installed with Frame Control or FrameDrop.** They install into the same
folder, `~/frameplayerx`, so either way above removes it.

**By hand.** Delete the `frameplayerx` and `frameplayerx.old` folders in
your home folder, and remove FramePlayerX from the Steam library.

**Deleting your settings and watch history later.** If you kept them, or
removed the app by hand, they stay behind. To delete them, run this in
Konsole on the headset:

```
rm -rf ~/.config/frameplayerx ~/.local/share/frameplayerx ~/.cache/frameplayerx
```

| Folder | What's in it |
|---|---|
| `~/.config/frameplayerx` | Settings, your sources and their passwords |
| `~/.local/share/frameplayerx` | The watch history, ratings, adjustments, the log, and the `Interactive` folder, including any haptic scripts you put there |
| `~/.cache/frameplayerx` | Thumbnails |

If you paired a PC with `frameplayerx-install pair` and nothing else uses
that pairing, you can also delete `~/.ssh/id_rsa_frame_devkit` and
`~/.ssh/id_rsa_frame_devkit.pub` on the PC, and the `Host frame` entry in
`~/.ssh/config`.

## Where your videos can be

| Where | How |
|---|---|
| Any folder on the headset, a microSD card or a USB drive | Browse it under Home › On this device, or make it a library: Sources › Add source › Library |
| Any server with a DeoVR feed or HereSphere API | Sources › Add source › DeoVR feed or HereSphere API |
| NAS / Windows share | Sources › Add source › SMB share: a whole server (`smb://nas/`, which lists its shares) or one share or folder (`smb://nas/videos`) |
| Nextcloud and other WebDAV | Sources › Add source › WebDAV |
| Jellyfin, Emby | Sources › Add source › Jellyfin / Emby (a password, or Quick Connect on Jellyfin) |
| Plex, Serviio and other DLNA servers | Sources › Add source › DLNA › Search the network |
| A web server folder listing | Sources › Add source › HTTP |

### Home, watch history and browsing

**Home** opens on what you were watching (a banner to resume, then
*Continue watching* and *Favorites*) and ends with a row of your sources:
**On this device** first, then your libraries and every other source you
added, then *Add a source*.
Each opens in a browser made for that kind of source; Back returns to Home.
Its title is always the source's name, with where you are in it underneath.
Sources locked with a PIN stay off Home until unlocked; the padlock on a
tile locks it (or, without a PIN yet, asks for one).

On a video's card the picture plays it and the title under it opens its
details: your rating and favorite, its format, *Play next*, *Add to queue*
and *Add to playlist*, and what its server says about it (Jellyfin's story,
a DeoVR or HereSphere scene's description, their tags), kept with the video
and brought up to date each time it opens; library search finds those
tags too. In a folder, the ⋯ on a video's row offers
the same.

**Watch history** lists everything you have played, from any source, with
the same search, format filters and orders (recently watched, title, most
watched, rating, length). Remove one video from its details with *Remove
from history*, or everything with *Clear history*; favorites and ratings
stay. Browsing a network source never adds anything here. Removing a source
forgets its videos, except those you watched, marked or put in a playlist.

### Up next and playlists

**Up next** is the list of videos playing in turn; the button beside
Browse on the control bar shows it over the video. Opening a video from a
folder, grid or season makes that list Up next, and X and Y step through
it. *Play all*, a playlist and what you queue yourself also play through:
the next video starts when one ends. *Play next* (after the video playing)
and *Add to queue* (at the end) are on a video's details, a Jellyfin or
scene page and a folder row's ⋯; with nothing playing, either plays it. On
the Up next page, pick a video to play it, move videos up or down, take
them out, *Clear* everything but the video playing, or *Save as playlist*.

**Playlists**, a tab beside Watch history, holds your playlists: make one
there or with *Add to playlist*, which offers your playlists and a name
for a new one. A playlist can hold videos from any source. Its page plays
them all from the top or from the one you pick, moves them up or down,
takes them out, and renames or deletes the playlist (*Delete* asks again).
Videos of a source locked with a PIN are left out until it is unlocked.

| Source | What browsing it offers |
|---|---|
| On this device | The home folder and the inserted microSD card and USB drives, as folders: play any video, or *Play all* in a folder |
| Library | **Videos**: every video in its folder, with thumbnails, search, format filters and sorting (newest, title, length, rating, shuffle). **Folders**: its folder as folders. **Scan now** looks for new and removed videos |
| Jellyfin / Emby | Your libraries, *Continue watching*, *Next up* and the latest additions; a search of the whole server by title, as Jellyfin's own search shows it: a row each of movies, shows, collections and other videos, best matches first (episodes are found through their show), and a *Tagged* row of what carries a tag with your words in it; poster grids searched, sorted and filtered by the server (unwatched, in progress, favorites); series by season and episode; details with the story, cast, genres and tags (tap a tag to see everything that has it), and a *Version* row when the server keeps more than one version of a video (4K and 1080p, a director's cut): *Automatic* plays the tallest that fits the source's maximum height; pick another and the video plays from it wherever you open it (subtitles included) until the app closes. Each version keeps its own format and adjustments, as a separate video in your history. Watched marks, favorites and resume points are the server's (see [Watch sync](#watch-sync-with-jellyfin-emby-and-heresphere)) |
| DeoVR feed / HereSphere API | The server's scene groups as chips over a grid of scenes; search the chosen group (or *All*) across titles, studios, performers and tags (a DeoVR feed's tags and performers come with each scene's details, fetched while you search and kept for next time); order by title, date, rating or length; a *Favorites* filter (and *Scripted*, with haptics on). ⋯ opens a scene's details: description, performers and tags (tap one to see all their scenes), chapters to start from, and ♡ to make it a favourite on a HereSphere API that takes favourites. The page opens with the scenes from last time while the server sends the current list ("updating…" under the title). A HereSphere API can also follow what you watch (see [Watch sync](#watch-sync-with-jellyfin-emby-and-heresphere)) |
| SMB, WebDAV, HTTP folders, DLNA, local folders | Folders and files: search the folder you are in, order by name, date or size, *Play all* |

### Locking a source with a PIN

To keep a share or folder private when someone else uses the headset, press
the padlock next to it under **Sources** (or on its tile on Home) and choose
a PIN (4 to 8 digits). While it is locked it can't be browsed, it is left
off Home, and its videos are left out of the watch history, search and the
phone remote; opening one asks for the PIN. **Unlock** opens it until
FramePlayerX restarts; one press of the padlock locks it again. To change
the PIN or remove it, edit the source (the pencil under **Sources**).
Removing the source also removes its PIN. This keeps guests out, it doesn't
encrypt anything: the files themselves stay as they are.

A PIN locks that one source only. Another source on the same server, or
with the same folder, stays open: lock it too, with the same PIN or its
own, if it shows the same videos.

### Editing a source

The pencil next to a source under **Sources** opens its details: rename it,
change its address, user name and password, or its PIN. Its type stays; a
Jellyfin server signed in with Quick Connect keeps that sign-in
unless you clear it.

### Watch sync with Jellyfin, Emby and HereSphere

These servers keep track of what you watch, and FramePlayerX keeps them in
step with the headset:

- **Jellyfin and Emby** hear when a video starts, pauses, resumes, jumps or
  changes speed, where it is every 10 seconds, and where it stopped. The
  server keeps the resume point, counts the play and marks the video
  watched when you stop near the end; its dashboard shows it playing on the
  headset. A video starts at the server's resume point, also when opened
  from Watch history or Home, so you can carry on where you left off on
  another device. ♡ and ✓ *Watched* on a Jellyfin page change the marks on
  the server at once.
- **HereSphere APIs** that ask for playback events get the ones HereSphere
  itself sends (open, play, pause and close, with the position), from which
  the server counts plays and keeps resume points. ♡ on a scene's page
  makes it a favourite on a server that takes favourites.

Reports go out in the background and in order. Quitting waits a few
seconds for the last ones; one the server doesn't take (it is offline, or
the sign-in expired) is noted in frameplayerx.log and not sent again.

DeoVR feeds, DLNA, network shares and folders keep no watch state on the
server: their resume points, favorites and ratings stay on the headset.

### Libraries

A library is a folder of videos, on the headset or on a microSD card or USB
drive, with a place of its own on Home: add one under **Sources › Add
source › Library**, picking the folder or one of the folders offered (your
Videos, Downloads and Movies folders, and each inserted card). Make one for
each kind of video, say Movies in `~/Videos/Movies` and Gaming clips on the
card, and each keeps its own videos. Libraries can't overlap: a folder
inside a library, or holding one, can't be another.

FramePlayerX looks for new and removed videos in a library when you add
it, when FramePlayerX starts, when its card is inserted, each time you open
it, and when you press **Scan now** on its page. Its thumbnails are made in
the background. The pencil next to it under **Sources** renames it or moves
it to another folder, and offers **Rebuild thumbnails** and **Retry**
(videos that couldn't be read).

A library on a card knows the card by its own ID, not by its name or where
it is mounted. Two cards with the same name and the same folders are two
different cards: each video keeps its own thumbnail, rating and resume
point, and a library made on one card shows **Not inserted** while the
other is in. Reformatting a card gives it a new ID: edit the library and
pick its folder again.

### microSD cards and USB drives (VR180 and everything else)

FramePlayerX notices when a card or drive is inserted (a "Found drive"
message appears) and you can browse it under **On this device**. To have
its videos with thumbnails, filters and sorting, make a library of it, or
of a folder on it (see [Libraries](#libraries)).

**Copying videos from a Windows PC or Mac:** format the card as **exFAT**
on the computer (Windows: right-click the card › Format › exFAT), copy the
videos onto it and put it in the Frame. exFAT has no 4 GB file limit, so
8K files are fine; FAT32 cannot hold files over 4 GB.

- The Frame's SteamOS only mounts ext4 cards itself (the kind Steam
  formats for games) and no USB drives. FramePlayerX mounts exFAT, FAT32
  and NTFS cards, and USB drives, itself while it is running, under
  `/run/media/steamos/<name>`.
- Steam may say the card needs formatting. **Don't format it from Steam**:
  that erases the card and makes it ext4, which Windows and macOS can't
  read. FramePlayerX still finds the videos.
- An ext4 card formatted by Steam works too (shared with games: Steam game
  folders, `steamapps`, are skipped), but only the Frame can read it.
- When the card is removed its library's videos stay listed with your
  ratings, resume points and adjustments; they play again once it is back.
- VR180 videos are recognised from their metadata (VR180 cameras, YouTube
  VR180 downloads) or from names like `Trip_VR180.mp4`,
  `Beach_180_LR.mp4` or `clip_180x180_3dh.mp4`. If one shows up wrong, set
  the format once under ⚙ › Format; it is remembered for that file.
- Large high-bitrate 8K files play best from a fast card (A2/V30 or better).

## Video formats

The format is picked automatically from, in order: your own choice for that
file, the file's spherical/stereo metadata, the file name, and the picture's
shape. File-name tags understood include `_180`, `_360`, `_LR`/`_SBS`,
`_TB`/`_OU`, `_RL`, `_FISHEYE190`, `_MKX200`, `_MKX220`, `_VRCA220`,
`_EAC`, `_2D`, `_3D`.

Movie shapes (16:9, 4:3, 1.85, scope) and portrait phone clips play on a
flat screen, and full side-by-side or over/under 3D movies (3840x1080,
1920x2160) as flat 3D. A video nothing identifies starts as 180° side by
side; Settings › Playback › **Default format** picks another.

Videos are shown as their file says: a phone clip stored sideways stands
upright, and anamorphic video (DVDs, anamorphic lenses) is widened to its
real shape. A portrait video's screen is as tall as **Screen width**.
When a 3D file's metadata says the right eye comes first, the eyes are
swapped to match.

To change it: open the video, press **⚙** on the control bar, **Format**.
Overrides are remembered per video. The same panel has position, zoom,
IPD/depth, stereo alignment, picture controls, subtitle depth and
keyframes (adjustments that change smoothly along the video).

Codecs: H.264, HEVC (8/10-bit, HDR10/HLG tone-mapped), AV1, VP9, VP8,
VC-1, MPEG-4, MPEG-2, ProRes. Audio: AAC, AC-3/E-AC-3, Opus, Vorbis, FLAC,
MP3, DTS, TrueHD, PCM (Blu-ray and DVD LPCM too), and spatial (ambisonic)
audio that follows your head.
Subtitles: SRT, ASS/SSA, WebVTT, and embedded text or picture subtitles.

### Fisheye lenses

The fisheye presets (190° for the Canon RF 5.2 mm, 200° and 220° for
`_MKX200` and `_MKX220` files) suit most videos. If one still looks wrong,
for example if straight lines bend as you look around or the eyes stop
lining up towards the edges, open **⚙ › Format** and use the sliders under
**Fisheye lens**:

- **Field of view**: the lens's angle, 120° to 300°. The image circle fills
  the shorter side of each eye's image.
- **Lens k1** and **Lens k2**: correct a lens that doesn't keep to an even
  fisheye spread towards its edge.
- **Lens centre**: where each image circle's centre is, if it isn't in the
  middle of its half of the frame. This is common in footage straight from
  a camera. In 3D, each image has its own.

To fine-tune a slider, point at it and push the thumbstick left or right.
Each push is one step, and holding repeats. **Save for this video** keeps
the settings for that file.

### Subtitles

Subtitles come from inside the video or from files beside it, named after
it: `Movie.srt`, `Movie.en.srt`, `Movie.de.forced.ass`. Jellyfin and Emby
servers offer the subtitle files they keep, and DeoVR feeds and HereSphere
APIs those they list.

Settings › Sound and subtitles › **Subtitles** sets what a video starts with:

- **Automatic** (the default): subtitles in your language when the audio
  is in another one, forced subtitles for the parts in another language
  (signs, a few lines of foreign dialogue), and a subtitle file named
  exactly like the video.
- **Always**: subtitles in your language whenever the video has them.
- **Off**: none.

**Your language** starts as the system's (English when it has none). While
a video plays, **⚙** › **Audio & subs** lists its subtitles, those in your
language first, each with what tells it apart: forced, SDH, its title or
file name, and its format. A video with many keeps those in other
languages under **Other languages**.

Subtitles show in any script, with the fonts on the headset: Arabic and
Hebrew read right to left, and Chinese, Japanese or Korean need a font
that covers them. If subtitles in a language show nothing or boxes, copy a
font for it (the Noto fonts cover every language) into
`~/.local/share/fonts` and restart FramePlayerX; frameplayerx.log names
the characters it had no font for.

### Videos that stutter: AV1 and 4:2:2

The Frame's hardware decoder plays 4:2:0 HEVC (H.265, 8- and 10-bit) and
H.264 (8-bit), up to 8K. AV1, 4:2:2 or 4:4:4 colour and 10-bit H.264 have
no hardware path on the Frame, so they decode on the CPU, which can't keep
up with VR resolutions: they stutter or stop to buffer, and the control bar
says why ("No hardware decoding for AV1", "… for 4:2:2 HEVC"). Convert them
to 4:2:0 HEVC once on a PC and they play smoothly.

"Hardware decoder busy" means the file is fine but the decoder had no room
for it (Steam's own streaming uses it too, and an 8192×4096 H.264 or
10-bit HEVC video only fits when nothing else is decoding). "The hardware decoder can't play
this HEVC" means it tried and refused the file; frameplayerx.log says why.

The pill next to the format on the control bar says which decoder plays
the video: HW, or SW in amber, with the codec and bit depth. Hover it, or
open Adjust → Format → Video stream, to see why a video isn't decoded in
hardware.

To check a file: `ffprobe -v error -select_streams v:0 -show_entries
stream=codec_name,profile,pix_fmt video.mp4` (`yuv422p` or `yuv422p10le`
is 4:2:2, `yuv420p10le` is 10-bit), or MediaInfo's *Format*, *Bit depth*
and *Chroma subsampling*. Windows' file properties don't tell these
reliably.

With an NVIDIA graphics card (fast):

```
ffmpeg -hwaccel cuda -i video.mp4 -map 0:v:0 -map 0:a? -c:v hevc_nvenc -preset p5 -profile:v main -pix_fmt yuv420p -rc vbr -cq 20 -b:v 0 -tag:v hvc1 -c:a copy -movflags +faststart video_8bit.mp4
```

Any PC (slower; `-crf` lower is better quality and bigger):

```
ffmpeg -i video.mp4 -map 0:v:0 -map 0:a? -c:v libx265 -preset fast -crf 20 -profile:v main -pix_fmt yuv420p -tag:v hvc1 -c:a copy -movflags +faststart video_8bit.mp4
```

These make 8-bit HEVC, which always plays in hardware. To keep a 10-bit or
HDR video at 10 bits, use `-profile:v main10` with `-pix_fmt p010le`
(NVIDIA) or `-pix_fmt yuv420p10le` (any PC); it plays in hardware too, but
at 8192×4096 only while nothing else is using the decoder.

Or in HandBrake: the **H.265** encoder for your graphics card (NVEnc, AMD
VCE or Intel QSV; the 10-bit one for a 10-bit or HDR video), Constant
Quality about 20, framerate Same as source, audio Passthru, and
**Dimensions › Cropping: None** (automatic cropping cuts into side-by-side
VR pictures).

Keep the format tags in the name (`_180_LR` and so on); conversions drop
the VR metadata, so a file without tags may need its format set once
under ⚙ › Format.

## Controls

Each button does one thing: the right hand runs playback, the left hand the
picture. Play/pause and seeking are under either thumb.

| Control | Right hand | Left hand |
|---|---|---|
| Thumbstick left / right | Seek back / forward 10 s (adjustable; hold to repeat). Pointing at a list: previous / next page. Pointing at a slider: one fine step | The same |
| Thumbstick up / down | Volume (or scroll a menu you point at) | Tilt the picture (or scroll a menu you point at) |
| Thumbstick press | Mute | Reset the picture |
| A / D-pad down | Play / pause | Play / pause |
| B / D-pad right | Back: closes the open panel, then shows the browse panel over the video, then returns to the video | Your room on / off: around flat videos and the panels, or through a side-by-side passthrough video's mask |
| X / D-pad left | Previous video in the list you opened it from | Adjust panel |
| Y / D-pad up | Next video | Show or hide the controls |
| Menu / View | Browse | Recenter |
| Bumper | Forward 1 minute | Back 1 minute |
| Grip + thumbstick | Up / down raises or lowers you | Left / right turns the picture, up / down zooms in / out |
| Hold grip + trigger, move | Drag the picture (a flat screen or the dome follows your hand) | The same |
| Trigger | Click; on empty space, show or hide the controls | The same |

To recenter, hold the Steam button, as in any game (both grips together, or
the left Menu button, do it too). The picture and the panels come to where
you are looking; looking well up or down (lying back, say) counts, a glance
a little up or down doesn't, so sitting up a flat screen comes back level.
The browse panel (a little below where you look) and a flat screen come
back in front of you, wherever you had moved them (a 180° or 360° picture
keeps its turn). A flat screen opens upright at eye height, the way the
browse panel faces: where you move it lasts while you watch, and isn't kept
with the video.

Each piece moves on its own and takes what hangs off it along. Drag the
picture (grip + trigger) or use Yaw and Pitch to move a flat screen: the
control bar and the adjust panel go with it. Hold grip on the control bar
to move it about the screen: the adjust panel goes with it. Hold grip on
the adjust panel to move it about the bar. During a 180° or 360° video,
Yaw, Pitch and dragging turn only the picture; the control bar and adjust
panel stay where they are. The browse panel (or the keyboard under it)
moves the same way, with grip, and stays where you leave it. The format on the control bar
(for example 180° SBS) opens the adjust panel's Format tab.

### Watching lying down

Lie back, look where you want the picture, and hold the Steam button to
recenter: it comes up to where you look. Or turn on **Screen follows you**
(the bed button on the control bar, or Settings › General): a flat video
and the menus then follow your head and stay in front of you as you turn,
lie back or roll onto your side. 180° and 360° videos don't follow, since
you look around those.

Every button and thumbstick direction above can be changed in Settings ›
Controller (tap a row, pick an action; Reset to defaults puts this layout
back). For example, to zoom with the right thumbstick, set the right controller's
thumbstick **↑↓ Up / down** (or **Grip + ↑↓**) to Zoom. The trigger,
grip + trigger drags, both grips, menu paging and slider steps are fixed.

Drags, turns, tilts and zoom count as adjustments: save them for that video
under ⚙, or press the left thumbstick to undo them.

Text fields bring up a keyboard below the panel you type in; **#+=**
switches it to symbols (`! # $ % ( ) [ ] { }` and the rest) and **abc** back. You can also
type on your phone through the web remote.

## Passthrough videos

The adjust panel's **Passthrough** tab (the sliders button on the control
bar, then the glasses tab) makes part of a video see-through so your room
shows behind it. The **Room** button (the armchair) on the bar shows your
room around a flat video's screen; on a side-by-side 180° or fisheye
video it turns the video's own mask on and off.

### Videos with their own mask (`_alpha`)

Many passthrough videos carry a see-through mask inside the
picture, and their names end in `_alpha` (for example
`Scene_4096p_FISHEYE190_alpha.mp4`). FramePlayerX reads the mask by itself:
only the people stay and the room shows around them, with nothing to set.
For a file whose name doesn't say so (renamed, or named by a server), press
**Room** on the control bar, or turn on **Use the video's mask** in the
Passthrough tab; either is remembered for that file. Only side-by-side
videos can carry a mask.

### Green and blue screens (chroma key)

Videos filmed against a green or blue screen can show your room instead of
the screen. Turn on **Remove the background** and pick the colour (green
screen, blue screen, or any colour with the red/green/blue sliders).
**Similarity** sets how close a colour must be to disappear, **Edge softness**
feathers the cut, and **Spill removal** takes the screen's tint off hair and
edges. A keyed video turns passthrough on by itself, whatever its format.

**Settings > Passthrough** sets this for every video. To change one video,
open the adjust panel's **Passthrough** tab while it plays: changing anything
there gives that video its own settings (save them like any other
adjustment), and turning **Own settings for this video** off makes it follow
Settings again.
## Haptics

Haptics are off until you turn on **Settings › General › Haptic devices**.
Until then FramePlayerX shows nothing about them and loads no scripts.

Scripts (`.funscript`, including multi-axis `video.surge.funscript` etc.)
are found next to the video, in `~/.local/share/frameplayerx/Interactive`
(Settings › Haptics shows it), or supplied by the server.
Add a device in **Settings › Haptics**: Intiface Central, TCode devices
(OSR2/SR6 over USB serial, TCP or UDP), or The Handy (connection key). The
seek bar shows the script's intensity.

## Remote control

**Settings › Remote control › Phone / browser remote** shows a QR code; scan
it to control playback, find videos and type from your phone. The
**DeoVR remote API** (port 23554) works with apps that speak DeoVR's
protocol. Both only answer devices on your local network.

## Troubleshooting

- When a video stutters, turn on Settings › General › **Performance
  overlay**. Low in your view, it shows how long each frame takes to draw
  (amber when longer than the display allows), the video's frame rate and
  dropped frames, its decoder, how busy the CPU and GPU are, and memory.
- After a crash, the next start asks whether to send a crash report to
  FramePlayerX's developers, as a private issue on GitHub. It contains the
  versions of FramePlayerX and the headset's system, what went wrong, and
  the log of that session with the names of your videos, files and folders,
  your servers' addresses and any password replaced by placeholders such as
  `<video 1>`. **Show the report** shows exactly what would be sent;
  nothing is sent unless you choose **Send report**.
- Log: `~/.local/share/frameplayerx/frameplayerx.log`; the run before is kept
  as `frameplayerx.log.old` (the one to send after a freeze or crash, once
  FramePlayerX has been started again)
- Settings: `~/.config/frameplayerx/settings.json` (delete to reset)
- Network source passwords: `~/.config/frameplayerx/sources.json`
  (readable only by you)
- "Is SteamVR running?": FramePlayerX needs the headset's OpenXR runtime;
  start it from the Steam library, not a desktop terminal.
- "… has signed this headset out": a Jellyfin server ends a Quick
  Connect sign-in when the account's password changes or the device is
  signed out on the server. Pair again: the pencil next to the source under
  **Sources**, *Clear* next to Quick Connect, then *Get pairing code*.
