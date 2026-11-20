# FramePlayerX user guide

FramePlayerX plays flat, 180°, 360°, fisheye and YouTube-style (EAC) videos,
2D or 3D, on the Steam Frame, from the headset, a microSD card, a USB drive
or your network.

- [Install](#install), [updating](#updating) and [uninstall](#uninstall)
- [Sources](#sources): the headset, its cards and drives, shares and
  servers on your network, and media servers
- [Browsing and history](#browsing-and-history), and
  [Up next and playlists](#up-next-and-playlists)
- [Video formats](#video-formats), [fisheye lenses](#fisheye-lenses),
  [adjusting a video](#adjusting-a-video) and the [cinema](#cinema)
- [Subtitles](#subtitles)
- [Hardware decoding](#hardware-decoding) and
  [server transcoding](#server-transcoding)
- [Display](#display): refresh rate, resolution and foveated rendering
- [Controls](#controls), and [watching lying down](#watching-lying-down)
- [Passthrough videos](#passthrough-videos)
- [Haptics](#haptics)
- [Remote control](#remote-control)
- [Language](#language)
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
do. Run the same file again to reinstall. To install the newest release,
betas included, download `frameplayerx-desktop-install.py` from the
[release page](https://github.com/remotesojourner/frameplayerx/releases/latest)
and run it in Konsole:

```
python3 ~/Downloads/frameplayerx-desktop-install.py --beta
```

The other two ways need **Developer Mode** on, and the headset paired once
with your PC by `frameplayerx-install pair` (below), Frame Control or
Valve's SteamOS Devkit Client.

**With Frame Control, from a PC.** With Frame Control running on your PC
and the headset paired, press the button in your PC's browser:

<a href="https://frameplayerx.org.uk/frame-control"><img src="../assets/frame-control-button.svg" alt="Install with Frame Control" width="240" /></a>

If the Frame Control button doesn't open Frame Control, paste this link into
the browser's address bar instead:

```
frame-control://install?manifest=https%3A%2F%2Fgithub.com%2Fremotesojourner%2Fframeplayerx%2Freleases%2Flatest%2Fdownload%2Fframeplayerx.json
```

Frame Control reads the release's manifest (`frameplayerx.json`), downloads
the latest release and checks its SHA-256. It adds FramePlayerX to the Steam
library as a sideloaded title, in `~/devkit-game/FramePlayerX`, and lists it
under its **Sideloaded titles**.

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

**Installed with Frame Control.** It lives in `~/devkit-game/FramePlayerX`,
which the ways above don't delete. Remove it in Frame Control, under
**Sideloaded titles**: that deletes its folder and its library entry.

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
| `~/.cache/frameplayerx` | Thumbnails, and pictures from your servers |

If you paired a PC with `frameplayerx-install pair` and nothing else uses
that pairing, you can also delete `~/.ssh/id_rsa_frame_devkit` and
`~/.ssh/id_rsa_frame_devkit.pub` on the PC, and the `Host frame` entry in
`~/.ssh/config`.

## Sources

A source is where videos come from: a folder on the headset, a share or
server on your network, or a media server. Add one under **Sources › Add
source**, then open it from Home. Each kind below is described the same
way: how to add it, what browsing it offers, and where it keeps your watch
state (resume points, watched marks and favorites; ratings always stay on
the headset).

| Source | Videos from | Watch state |
|---|---|---|
| [On this device](#on-this-device) | Any folder on the headset, a microSD card or a USB drive | On the headset |
| [Libraries](#libraries) | A folder on the headset, a card or a drive, with thumbnails, filters and sorting | On the headset |
| [SMB shares](#smb-shares) | Windows shares, Samba and most NAS | On the headset |
| [NFS shares](#nfs-shares) | NFS exports on Linux servers and NAS | On the headset |
| [SFTP servers](#sftp-servers) | Any computer you can sign in to with SSH | On the headset |
| [WebDAV servers](#webdav-servers) | Nextcloud, NAS and other WebDAV servers | On the headset |
| [Web folders](#web-folders) | A web server's folder listing | On the headset |
| [DLNA servers](#dlna-servers) | Serviio, and Plex, Jellyfin or Emby with DLNA on | On the headset |
| [Jellyfin and Emby](#jellyfin-and-emby) | Jellyfin and Emby media servers | On the server |
| [Plex](#plex) | Plex Media Server | On the server |
| [DeoVR feeds and HereSphere APIs](#deovr-feeds-and-heresphere-apis) | Any server with a DeoVR feed or HereSphere API | HereSphere: on the server; DeoVR: on the headset |

Any source you add can be renamed, given a new address, locked with a PIN
or removed: see [Editing and locking a source](#editing-and-locking-a-source).

### On this device

Your home folder on the headset and each inserted microSD card or USB
drive, as folders.

- **Add it:** nothing to add: **On this device** is the first source on
  Home.
- **Browsing:** folders and files: search the folder you are in, order by
  name, date or size, and **Play all** (see
  [Up next and playlists](#up-next-and-playlists)).
- **Watch state:** on the headset.

FramePlayerX notices when a card or drive is inserted (a "Found drive"
message appears), and it shows up here. To have its videos with
thumbnails, filters and sorting, make a [library](#libraries) of it, or of
a folder on it.

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
- Large high-bitrate 8K files play best from a fast card (A2/V30 or better).

### Libraries

A folder of videos, on the headset or on a microSD card or USB drive, with
a place of its own on Home.

- **Add it:** Sources › Add source › **Library**, picking the folder or one
  of the folders offered (your Videos, Downloads and Movies folders, and
  each inserted card). Make one for each kind of video, say Movies in
  `~/Videos/Movies` and Gaming clips on the card, and each keeps its own
  videos. Libraries can't overlap: a folder inside a library, or holding
  one, can't be another.
- **Browsing:** **Videos** has every video in its folder, with thumbnails,
  search, format filters and sorting (newest, title, length, rating,
  shuffle); **Folders** has its folder as folders.
- **Watch state:** on the headset.

FramePlayerX looks for new and removed videos in a library when you add
it, when FramePlayerX starts, when its card is inserted, each time you open
it, and when you press **Scan now** on its page. Its thumbnails are made in
the background, on one core at low priority, from the video's keyframes,
and wait while a video plays. Editing a library (the pencil next to it
under **Sources**) renames it or moves it to another folder, and offers
**Rebuild thumbnails** and **Retry** (videos that couldn't be read).

A library on a card knows the card by its own ID, not by its name or where
it is mounted. Two cards with the same name and the same folders are two
different cards: each video keeps its own thumbnail, rating and resume
point, and a library made on one card shows **Not inserted** while the
other is in. When a card is taken out, its library's videos stay listed
with your ratings, resume points and adjustments, and play again once it
is back. Reformatting a card gives it a new ID: edit the library and pick
its folder again.

### SMB shares

Windows shares, Samba and most NAS.

- **Add it:** Sources › Add source › **SMB**. The address is a whole server
  (`smb://nas/`, which lists the shares your login, or a guest without
  one, may open) or one share or folder in it (`smb://nas/videos`). Fill in
  **User name** and **Password** if the share asks for them.
- **Browsing:** folders and files, as [on this device](#on-this-device).
- **Watch state:** on the headset.

### NFS shares

NFS exports on Linux servers and NAS, over NFSv3 and TCP.

- **Add it:** Sources › Add source › **NFS**. The address is a whole server
  (`nfs://nas`, which lists its exports), its export `/` (`nfs://nas/`,
  with the slash; a server without one is added as `nfs://nas` instead) or
  one export or folder in it (`nfs://nas/volume1/videos`, or
  `nas:/volume1/videos` as `mount` writes it). Files are read as the
  **User ID** and **Group ID** you set, or this device's when blank.
- **Browsing:** folders and files, as [on this device](#on-this-device).
  Symbolic links on the server show up as the file or folder they point
  to, as long as it is in the same export.
- **Watch state:** on the headset.

If adding it says access is refused: Linux servers only accept connections
from a privileged port unless the export has the `insecure` option. Add it,
for example `/srv/videos 192.168.1.0/24(ro,insecure)` in `/etc/exports`,
or turn on *Allow connections from non-privileged ports* on a Synology.

### SFTP servers

Any computer you can sign in to with SSH: Linux, macOS and many NAS.

- **Add it:** Sources › Add source › **SFTP**. The address says which
  folder to open, as in the table below; add the port after the name
  (`sftp://pc.local:2222/`) if the server doesn't use 22. A **User name**
  is always needed; one in the address (`sftp://me@pc.local/`) works as
  well. Sign in with a **Password**, a **Key file**, or both: the key is
  tried first.
- **Browsing:** folders and files, as [on this device](#on-this-device).
  Files and folders whose names start with a dot (hidden on Linux and
  macOS) aren't listed.
- **Watch state:** on the headset.

| Address | Opens |
|---|---|
| `sftp://pc.local/` | Your home folder on the server |
| `sftp://pc.local/~/Videos` | A folder in your home folder |
| `sftp://pc.local/srv/media` | Any other folder, from the top of the server's disk |

A key file is a private key on the headset, from OpenSSH (`id_ed25519`,
`id_rsa`) or PuTTY (`.ppk`). Copy it to `~/.ssh` or `~/Downloads` on the
headset and it's listed under the field: tap it to use it. If the key is
password-protected, a **Key passphrase** row appears: enter the passphrase
you set when you made the key, not your account password.

The first time it connects, FramePlayerX remembers the server's identity
key. If the server later answers with a different key, the source stops
with an error instead of signing in. If you reinstalled the server or gave
it a new key, remove the source and add it again; otherwise another
machine may be answering in its place.

### WebDAV servers

Nextcloud, NAS and other WebDAV servers.

- **Add it:** Sources › Add source › **WebDAV**, with the address of the
  folder to open (for example `https://nas.local/dav/videos/`), and a
  **User name** and **Password** if the server asks for them. **Accept
  self-signed certificates** lets in an HTTPS server on your own network
  whose certificate it made itself.
- **Browsing:** folders and files, as [on this device](#on-this-device).
- **Watch state:** on the headset.

### Web folders

The page a web server shows for a folder of files.

- **Add it:** Sources › Add source › **HTTP**, with the folder's address
  (for example `http://192.168.1.10:8080/videos/`).
- **Browsing:** folders and files, as [on this device](#on-this-device).
- **Watch state:** on the headset.

### DLNA servers

Media servers on your network that speak DLNA: Serviio, and Plex, Jellyfin
or Emby with DLNA turned on.

- **Add it:** Sources › Add source › **DLNA** › **Search the network**,
  then pick the server.
- **Browsing:** the server's folders and files, as
  [on this device](#on-this-device).
- **Watch state:** on the headset.

### Jellyfin and Emby

Jellyfin and Emby media servers, which can also
[transcode](#server-transcoding) a video while you watch.

- **Add it:** Sources › Add source › **Jellyfin/Emby**, with the server's
  address (for example `http://192.168.1.10:8096`). Sign in with your user
  name and password, or on Jellyfin with **Quick Connect**: press **Get
  pairing code** and enter the 6-digit code under Quick Connect in your
  Jellyfin app on another device.
- **Browsing:** the server's libraries, its search, and poster grids it
  sorts and filters (below).
- **Watch state:** on the server, kept in step with the headset (below).

Its page has your libraries, *Continue watching*, *Next up* and the latest
additions, and a search of the whole server by title, as Jellyfin's own
search shows it: a row each of movies, shows, collections and other
videos, best matches first (episodes are found through their show), a
*People* row of the cast and crew whose names match (tap one to see what
they are in), and a *Tagged* row of what carries a tag with your words in
it.

Poster grids are searched, sorted (title, date added, release date,
rating, length, last watched, shuffle) and filtered by the server:
unwatched, in progress, favorites, and **Filters** for genres, decades,
resolution, studios and tags. Series open by season and episode. A video's
details have the story, cast, genres and tags (tap a tag to see everything
that has it), and a *Version* row when the server keeps more than one
version of a video (4K and 1080p, a director's cut): *Automatic* plays the
tallest (within the source's [maximum height](#server-transcoding), if it
has one); pick another and the video plays from it wherever you open it
(subtitles included) until the app closes. Each version keeps its own
format and adjustments, as a separate video in your history.

The server hears when a video starts, pauses, resumes, jumps or changes
speed, where it is every 10 seconds, and where it stopped. It keeps the
resume point, counts the play and marks the video watched when you stop
near the end; its dashboard shows it playing on the headset. A video
starts at the server's resume point, also when opened from Watch history
or Home, so you can carry on where you left off on another device. ♡ and
✓ *Watched* on a video's page change the marks on the server at once.

### Plex

Plex Media Server, which can also [transcode](#server-transcoding) a video
while you watch.

- **Add it:** Sources › Add source › **Plex**. With **Link with code**, the
  default, press **Get pairing code**, then visit
  [plex.tv/link](https://plex.tv/link) on your phone or computer, sign in
  to Plex there if asked and enter the code shown in FramePlayerX. The
  headset waits for approval, then shows your servers: choose an owned or
  shared server, wait for its connection test, then **Save**. If the code
  expires, get another and try again; **Cancel** stops waiting.
- **Browsing:** movie, TV and personal-video libraries; *Continue
  watching* and recent additions; a search of the server; sorted and
  shuffled grids with unwatched and in-progress filters, and genre, studio
  and decade filters. Shows open by season and episode, and details
  include artwork, chapters and the versions available. Plex's server
  favorites, people and tag browsing aren't offered; playlists, Up next,
  history and format adjustments work as with any other source.
- **Watch state:** on the server, for the Plex account you linked. It
  hears when a video plays, pauses and stops, with the position and
  length. Its resume point comes before the headset's, even a zero
  position after a reset on another device. ✓ *Watched* changes the
  server's played state; marking a video watched this way adds no entry to
  the server's playback history.

The **Access token** option instead takes a Plex Media Server address (for
example `http://192.168.1.10:32400`) and an access token you already have;
test the connection before saving. A token entered this way can't be
renewed automatically. If a source linked with an earlier version fails to
sign in, link it again. Linking again, or replacing a token, makes a
separate source, keeping the existing one and its history apart from the
new account.

**Connection** offers three modes:

- **Local only** (the default for new sources): Plex's LAN addresses for
  the server, without remote addresses or Relay.
- **Direct connections**: LAN addresses first, then direct remote
  addresses, without Relay. Remote addresses have to use HTTPS, because
  your token goes with every request. If Plex only lists plain HTTP for
  your server, set its **Secure connections** to Preferred or Required.
- **Direct + Relay**: direct connections first, then Plex Relay if needed.

Local playback is free. Remote video needs **Plex Pass** for you or the
server's owner, or **Remote Watch Pass** for you. Plex may count another
subnet or VLAN as remote even over a LAN address; Local only doesn't
change that. Editing a source keeps its mode, and changing the mode checks
the server again before saving. For Local only with an access token, use a
LAN IP address.

Relay has to be turned on at the server too. It can work without opening
a router port, but is limited to 2 Mbit/s and has the same subscription
requirements for remote playback. With Relay, Automatic asks for a lower
bitrate transcode when the original is over the limit; you can also pick a
quality yourself. Direct remote access needs a connection to the server
that can be reached; port forwarding, a VPN or a reverse proxy are ways to
provide one. See Plex's
[Relay documentation](https://support.plex.tv/articles/216766168-accessing-a-server-through-relay/)
and [remote playback requirements](https://support.plex.tv/articles/requirements-for-remote-playback-of-personal-media/).

Plex's `*.plex.direct` names are ordinary HTTPS server addresses. Some
routers or DNS providers block them through DNS rebinding protection; see
[Plex's secure connection guide](https://support.plex.tv/articles/206225077-how-to-use-secure-server-connections/).
If looking one up fails, or the connection times out or can't be opened,
FramePlayerX tries the IP address and port Plex gave for that connection,
when there is one. It keeps the HTTPS name and certificate checks, and
checks the server's identity; the log notes the attempt. This doesn't
apply to Relay. Across VLANs, the headset still needs a route and firewall
permission to the server's port.

`app.plex.tv` is Plex's web app and account sign-in; FramePlayerX uses the
selected server's own connection, and entering `app.plex.tv` as the
address starts linking instead. Plex Home profile switching, live TV,
music, photos, the cloud Watchlist and casting aren't supported.

### DeoVR feeds and HereSphere APIs

Any server with a DeoVR feed or HereSphere API, which may also offer
heights it [transcodes](#server-transcoding) to.

- **Add it:** Sources › Add source › **DeoVR** or **HereSphere**, with the
  address of the server's feed (for example
  `http://192.168.1.10:9999/deovr`) or API (for example
  `http://192.168.1.10:9999/heresphere`).
- **Browsing:** the server's scene groups, searched, sorted and filtered
  (below).
- **Watch state:** a HereSphere API that asks for playback events gets the
  ones HereSphere itself sends (open, play, pause and close, with the
  position), from which the server counts plays and keeps resume points;
  ♡ on a scene's page makes it a favourite on a server that takes
  favourites. A DeoVR feed keeps none, so it stays on the headset.

The server's scene groups are chips over a grid of scenes. Search the
chosen group (or *All*) across titles, studios, performers and tags; order
by title, date, rating or length, either way, or shuffle; and filter with
*Favorites* (and *Scripted*, with haptics on) and **Filters** for studio,
people, tags, length, format and rating, each with how many scenes have it.
A DeoVR feed's studios, tags and performers come with each scene's
details, fetched while you search or filter and kept for next time. ⋯
opens a scene's details: description, performers and tags (tap one to see
all their scenes), and chapters to start from. The page opens with the
scenes from last time while the server sends the current list
("updating…" under the title).

### Editing and locking a source

The pencil next to a source under **Sources** opens its details: rename
it, change its address, user name and password or its PIN, or remove it
with **Remove source** at the top, which turns red as *Remove it?* and
removes the source when pressed again within 4 seconds. Its type stays; a Jellyfin server signed in with Quick Connect keeps that sign-in
unless you clear it.

When a server's address, IP address or root folder changes, edit the
source rather than adding it again: history, resume points, playlists and
adjustments follow it, while a removed and added source is a different
one. HTTP(S), WebDAV, SMB, SFTP and NFS sources keep paths relative to
their root, Jellyfin and Emby item and version IDs, DLNA object IDs, and
Plex its server and item IDs. A feed keeps scene, media and picture
addresses as published, unless a scene ID it gives allows looking it up
again, so changing a feed's address alone can't follow videos whose
addresses changed too. Older saved addresses are converted when their
identity is known, and browsing the source can identify more; unknown
outside or signed addresses, and unidentified old DLNA links, can't be
recovered from a new server address alone.

**Locking with a PIN:** to keep a source private when someone else uses
the headset, press the padlock next to it under **Sources** (or on its
tile on Home) and choose a PIN (4 to 8 digits). While it is locked it
can't be browsed, it is left off Home, and its videos are left out of the
watch history, search and the phone remote; opening one asks for the PIN.
**Unlock** opens it until FramePlayerX restarts; one press of the padlock
locks it again. To change the PIN or remove it, edit the source; removing
the source removes its PIN too. This keeps guests out, it doesn't encrypt
anything: the files themselves stay as they are. A PIN locks that one
source only. Another source on the same server, or with the same folder,
stays open: lock it too, with the same PIN or its own, if it shows the
same videos.

**Parallel downloads:** a server's form has this switch, off to begin
with. On, FramePlayerX downloads several parts of a video at once over
separate connections while it plays, the way some download managers do.
This helps if the server limits the speed of each connection. If it
doesn't, the switch may gain nothing, and a server on hard drives can get
slower, because the drives have to jump between parts of the file. SMB and
NFS shares, SFTP servers and DLNA servers don't have the switch.

**Reports to servers** that keep watch state go out in the background and
in order. Quitting waits a few seconds for the last ones; one the server
doesn't take (it is offline, or the sign-in expired) is noted in
frameplayerx.log and not sent again.

## Browsing and history

**Home** opens on what you were watching (a banner to resume, then
*Continue watching* and *Favorites*) and ends with a row of your sources:
**On this device** first, then your libraries and every other source you
added, then *Add a source*. Each opens in a browser made for that kind of
source (see [Sources](#sources)); Back returns to Home. Its title is always
the source's name, with where you are in it underneath. Sources locked
with a PIN stay off Home until unlocked; the padlock on a tile locks it
(or, without a PIN yet, asks for one).

Point at a title that is cut short, here or on the control bar, to read it
whole.

On a video's card the picture plays it and the title under it opens its
details: your rating and favorite, its format, *Play next*, *Add to queue*
and *Add to playlist*, and what its server says about it (Jellyfin's story,
a DeoVR or HereSphere scene's description, their tags), kept with the video
and brought up to date each time it opens; library search finds those
tags too. In a folder, the ⋯ on a video's row offers the same.

**Filters** opens a drawer on the right: what you pick there applies at
once, any of the choices in a section and all of the sections together,
and each shows as a chip under the sorts; press a chip to take it off, or
*Clear all*. A long section (people, tags, studios, genres) has a box to
find one and shows a few, with **Show all** for the rest. **Shuffle** puts
a list in a new random order each time you pick it or press **Shuffle
again**; opening a video's page and coming back keeps the order. Grids
load more as you scroll to their end.

### Watch history

**Watch history** lists everything you have played, from any source, with
the same search, format filters and orders (recently watched, title, most
watched, rating, length). Remove one video from its details with *Remove
from history*, or everything with *Clear history*; favorites and ratings
stay. Browsing a network source never adds anything here. Removing a source
forgets its videos, except those you watched, marked or put in a playlist.

A video you stopped part way shows the frame where you left off, made
shortly after it stops. Past 90 % it counts as watched: its card gets a ✓,
it starts from the beginning next time and shows its usual picture again.

### Deleting a file

**Delete file**, on a video's details and under the ⋯ on its row in a
folder, deletes the video's file for good. It asks first: pressed once it
turns red as *Delete for good?*, and pressed again within 4 seconds it
deletes. It's there for videos on the headset, a microSD card or a USB
drive, an SMB or NFS share and an SFTP or WebDAV server, and on Jellyfin,
Emby, Plex and HereSphere servers that let you delete. DeoVR feeds, DLNA
servers and web folders can't delete, and a folder is never deleted. A
server deletes the way its own apps do: Jellyfin takes a video's folder
with it when the video has one to itself. The video also leaves your
history, queue and playlists.

### Thumbnails

A video from Jellyfin, Emby, DLNA or a DeoVR or HereSphere server shows the
server's picture; any other gets a thumbnail made on the headset. Pictures
from servers are kept on the headset so they aren't downloaded each time.
**Settings › General › Thumbnails** holds these:

- **Make thumbnails**: off, no new thumbnails are made (the ones made stay).
- **Show where you left off**: off, videos show their usual picture, and the
  frames already made are deleted.
- **Image cache**: how much room server pictures take (500 MB unless you
  pick another size); the ones used longest ago make room for new ones.
  **Clear** deletes them all, so each is downloaded again.

Thumbnails of videos that leave the library, say when you remove a source,
are deleted too.

## Up next and playlists

**Up next** is the list of videos playing in turn; the button beside
Browse on the control bar shows it over the video. Opening a video plays it
alone. **Play all**, at the top of a folder, a library's Videos, a
Jellyfin, Emby or Plex grid, season or search, a scene grid or a playlist,
makes that page's videos Up next and plays the first. On a server's grid
it's the first 300 in the grid's order, as Jellyfin's own Play does;
pointing at the button says so. **⏮** and **⏭** either side of the play
button (or X and Y) step through Up next; pointing at one names the video
it goes to, and it dims at either end of the list or when a video plays
alone. *Play next* (after the video playing) and *Add to queue* (at the
end) are on a video's details, a Jellyfin or scene page and a folder row's
⋯; with nothing playing, either plays it. On the Up next page, pick a
video to play it, move videos up or down, take them out, *Clear*
everything but the video playing, or *Save as playlist*.

In a folder with subfolders, **Include subfolders** beside **Play all**
makes it play the videos in every folder below too, at any depth, in the
folder's order and matching its search. While it looks, a line under the
search says so, with **Cancel**, which leaves Up next as it was; leaving
the folder stops it too. To mix the order, use **Shuffle** on Up next.

It takes at most 300 videos, the first it finds going down from the
folder, and plays them straight away. Edit a source you added to change
that under **Limit videos from subfolders**, or turn it off to take them
all: it then says so once it passes 300, and **Cancel** stops it. **On
this device** always takes 300. If a subfolder can't be read, it offers
to play the videos it found.

**Shuffle** on the Up next page puts the current video first and every
other entry in a new random order, including entries that came before it.
The current video keeps playing; press **Shuffle** again for another order.
To keep this order as a playlist, press **Save as playlist** afterwards.

**Autoplay** (the play sign in a circular arrow, on the control bar) plays
Up next through: the next video starts when one ends. Off, a video stops
at its end, and ⏭ plays the next. **Loop** on the Up next page plays the
first video again after the last, and ⏮ and ⏭ step round the list past
either end. The repeat key beside Up next on the control bar loops the
video playing instead: it starts again from the beginning each time it
ends, before anything in Up next. All three stay on, from video to video
and after a restart, until you turn them off.

**Playlists**, a tab beside Watch history, holds your playlists: make one
there or with *Add to playlist*, which offers your playlists and a name
for a new one. A playlist can hold videos from any source. Its page plays
them all from the top or from the one you pick, moves them up or down,
takes them out, and renames or deletes the playlist (*Delete* asks again).
Videos of a source locked with a PIN are left out until it is unlocked.

## Video formats

The format is picked automatically from, in order: your own choice for that
file, the file's spherical/stereo metadata, the file name, and the picture's
shape. File-name tags understood include `_180`, `_360`, `_LR`/`_SBS`,
`_TB`/`_OU`/`_HTAB`, `_RL`, `_FISHEYE190`, `_MKX200`, `_MKX220`, `_VRCA220`,
`_EAC`, `_2D`, `_3D`. VR180 videos are recognised from their metadata
(VR180 cameras, YouTube VR180 downloads) or from names like
`Trip_VR180.mp4`, `Beach_180_LR.mp4` or `clip_180x180_3dh.mp4`.

Movie shapes (16:9, 4:3, 1.85, scope) and portrait phone clips play on a
flat screen, and full side-by-side or over/under 3D movies (3840x1080,
1920x2160) as flat 3D. A video nothing identifies starts as 180° side by
side; Settings › Playback › **Default format** picks another.

Videos are shown as their file says: a phone clip stored sideways stands
upright, and anamorphic video (DVDs, anamorphic lenses) is widened to its
real shape. A full side-by-side file marked to show at one eye's shape in
2D players (a 1:2 pixel aspect) still shows each eye at its own shape. A
portrait video's screen is as tall as **Screen width**. When a 3D file's
metadata says the right eye comes first, the eyes are swapped to match.

### Changing the format

If a video shows up wrong, open it, press **Adjust** (the sliders button)
on the control bar, then **Format**; the format on the control bar (for
example 180° 3D) opens the same tab. The format list has a row for each
kind of picture (flat screen, 180°, 360°, fisheye and YouTube 360°) and the
format playing is lit in it. Its buttons use the tags VR files carry in
their names: 2D, 3D SBS (side by side), 3D TB (top/bottom), 3D HSBS and
3D HTB (their half-size versions) and 3D FS (frame sequential); point at
one to see its full name. **Automatic** goes back to the detected format.
A choice is remembered for that video, and a video's details in your
library have the same list. Choosing frame sequential, or leaving it,
opens the video again where it was, since it decodes differently. For a 3D
format, **Swap eyes** under the list shows each eye the other picture, for
a file whose depth looks inside out.

### 3D Blu-rays and disc images

3D Blu-rays play as full 3D, each eye at full 1080p, on the Frame's
hardware decoder: disc images (`.iso`), MKV rips (MakeMKV), and from a
BDMV folder the film's `.ssif` file in `BDMV/STREAM/SSIF` (the `.m2ts` of
the same name holds the left eye alone). The few streams it can't take
that way (none of the discs tried) play their left eye in 2D; the decoder
line in the video's info says "3D Blu-ray as 2D".

A disc image, 2D or 3D, plays its main title (its longest playlist) with
its chapters (on the adjust panel's **Chapters** tab) and its audio and
subtitle languages. Menus, extras and other titles aren't offered.

### Frame sequential 3D

Frame sequential 3D files, whose eyes take turns frame by frame at twice
the film's frame rate, play as full 3D at the film's own rate, each eye at
its full size. They are found by the label in the file, or by `FS3D` in
the file name when a file has none. If depth looks inside out, **Swap
eyes** under the format list corrects it. A file nothing labels can be set
to **3D FS** (frame sequential) in the format list, and a mislabelled one
to any other format.

### Interlaced video

Interlaced video (1080i TV recordings and camcorder clips, DVDs) stores
each frame as two fields, every other line each, taken a field's time
apart, so anything moving shows comb lines. It is deinterlaced as it
plays: each field becomes a whole picture, so 25 frames a second become
50 pictures. It is found from the video's header. For a video in your
library whose header doesn't say so, the **Interlaced** switch under the
format list turns it on, and turns it off for one wrongly marked; that
opens the video again where it was, and is remembered for that video.
Interlaced video plays with software decoding, as the Frame's hardware
decoder doesn't take interlaced H.264.

### Fisheye lenses

A fisheye format shows **Fisheye lens** under the format list. Its presets
set the field of view: 180°, 190° (the Canon RF 5.2 mm), and 200° and 220°
(the MKX200 and MKX220 that `_MKX200` and `_MKX220` files come from). A
preset is lit only while the field of view is its angle. When the slider
sits on an angle that isn't a preset, a **+** button with that angle adds it
to the presets, up to eight; Settings › Playback › **Fisheye presets**
removes them, and **Reset** there puts the four back. Under them,
**Fisheye starts at** picks the preset a video gets when you pick Fisheye
for it, and a Fisheye default format. It is 180° to begin with; if you
remove the preset it was, the one nearest 180° takes its place. Files
tagged just `_FISHEYE` stay 180°, as the tag means. If the picture still
looks wrong, for example if straight lines bend as you look around or the
eyes stop lining up towards the edges, use the sliders:

- **Field of view**: the lens's angle, 120° to 300°. The image circle fills
  the shorter side of each eye's image.
- **Edge stretch** and **Rim stretch**: for a lens that doesn't spread its
  view evenly. Edge stretch stretches or squeezes the outer part of the
  circle, Rim stretch mostly its very edge.
- **Lens centre**: where each image circle's centre is, if it isn't in the
  middle of its half of the frame. This is common in footage straight from
  a camera. In 3D, each image has its own.

**Save for this video** keeps the settings for that file.

### Codecs and HDR

Codecs: H.264, HEVC (8/10-bit, HDR10/HLG tone-mapped), AV1, VP9, VP8,
VC-1, MPEG-4, MPEG-2, ProRes. Audio: AAC, AC-3/E-AC-3, Opus, Vorbis, FLAC,
MP3, DTS, TrueHD, PCM (Blu-ray and DVD LPCM too), and spatial (ambisonic)
audio that follows your head. Subtitles: SRT, ASS/SSA, WebVTT, and
embedded text or picture subtitles. Which of these the Frame decodes in
hardware is under [Hardware decoding](#hardware-decoding).

HDR videos (HDR10 and HLG) are fitted to the headset's screen as they play.
Each scene's brightest parts are measured, so midtones keep the brightness
they were graded at and only the highlights are compressed, keeping their
detail. Colours richer than the screen can show are toned down smoothly
instead of being cut off, and the most vivid ones it can show lose a
little saturation to make room for them.

## Adjusting a video

**Adjust** (the sliders button on the control bar, or X on the left
controller) opens the adjust panel for the video playing. Its tabs are
Format, Position, Stereo, Picture, Passthrough, Chapters and, with haptics
on, Haptics; between them they hold the format, position, size, IPD/depth,
stereo alignment, picture controls, subtitle depth, size and height, and
keyframes (adjustments that change smoothly along the video). **Save for this
video** keeps the changes with that video; **Reset** puts the picture's
own adjustments back.

Pull the trigger on a slider's knob and move the pointer to drag it;
pulled elsewhere on the slider, the knob jumps there first. To fine-tune a
slider, point at it and push the thumbstick left or right. Each push is
one step, and holding repeats.

Where videos start is set in Settings: **Video position** for the screen,
zoom, height and background, **Picture** for the picture and a 3D video's
depth and eye alignment, and Sound and subtitles for how far away, how
big and how high subtitles show.

A flat screen's **Distance**, **Width** and **Curvature** are on the
Position tab. Its size is its width, so zooming a flat screen with the
controller makes it wider or narrower, and **Zoom** shows only for 180°,
360° and fisheye videos.

Around a flat screen, and behind the menus, is a near-black grey, or a
[cinema](#cinema). Settings › Video position › **Background** picks the
colour videos start with: black, grey, navy, or any colour with the
red/green/blue sliders.
The same at the bottom of the Position tab changes it for the video that
is playing, and **Save for this video** keeps it with the rest of its
adjustments. Your room replaces it while it shows.

**Screen glow**, under the colour in Settings › Video position, lights the
background round a flat screen the way an LED strip behind a television
lights the wall: each part takes the colour of the picture's edge beside
it, mixing with its neighbours further out, and changes with the picture.
Black bars above and below a film, or beside it, are left out.
**Brightness** sets how bright it is beside the screen, up to as bright as
the picture's edge, and **Spread** how far out it reaches. The same under
the colour on the Position tab changes it for the video that is playing,
and the button beside its switch puts the video back on Settings. It shows
on a colour background only, not in a cinema, over your room or with a
chroma key.

### Comfort focus

**Comfort focus**, on the Stereo tab for stereo 180° and fisheye videos,
is about the thing you're looking at. Without it, rolling your head
sideways or watching lying down makes close things double, because the two
pictures stay side by side while your eyes are now one above the other.
With it on, FramePlayerX measures how far away that thing is and adjusts
each eye's picture to your eyes' actual positions at that distance, so it
fuses at its real depth; things much nearer or farther than it may still
look a little doubled. When it is closer than about 85 cm, it also eases
the depth so your eyes don't have to cross as far, which makes it look
bigger. It measures where your eyes look when the headset
tracks them, otherwise the middle of your view. The easing holds still
while your eyes move over the same thing. When they jump to something
else, the picture is ready for it by about the time they have settled
there, a little later for something nearer you haven't just looked at;
where the headset doesn't track them, it follows gently, over about a
second for something nearer and half a second for something farther.
Something that passes in front of what you look at leaves the picture as
it is, unless it stays for about a second or your eyes go to it.
It assumes the camera's lenses were 63 mm apart, the average eye spacing.
Settings › Picture › **3D videos** turns it on for every video; turning it
on or off for one video keeps that video's choice, and the button beside
it goes back to the setting.

### Cinema

A flat video can play in a cinema instead of on a floating screen. Pick a
room under **Background**, in Settings › Video position for every video
or on the Position tab for the one playing: **Classic**, a stadium hall
with red velvet seats, or **Luxe**, an open room with neon lights and
leather recliners. **Use global** there makes a video follow Settings
again. The picture fills as much of the cinema's screen as its shape
allows, with black beside or above it, and the house lights dim as it
plays and come up when you pause.

You sit in the middle of a row, facing the screen. **Row** and
**Curvature**, under the rooms, pick the row, 1 being the front row, and
how much the screen curves round you, from flat at 0 to its edges 0.8 m
nearer at 1. They start at row 2 and flat, and a video follows Settings'
until you change them on its Position tab. In a cinema, zooming with the
controller moves you a row forward or back, and the Position tab leaves
out the floating screen's Distance, Width and Curvature, which stay as
they are for when the cinema doesn't show. Recentering, **Put back** and
dragging the picture turn the whole cinema with it, and recentering lying
back tips it back with you. **Screen follows you** stays off in a cinema.

The cinema doesn't show while your room does, or for a chroma-keyed video.

### Presets

A preset keeps everything a video saves, to use on any video: the
format, if you picked one, and the Position, Stereo, Picture and
Passthrough tabs. A flat screen's place, where you moved it, isn't saved
for a video, so a preset leaves it too. Presets sit at the top of the
adjust panel, above the tabs:

- **New preset** saves the playing video's as Preset 1, Preset 2 and so
  on, up to 8. Saved from a video left on **Automatic**, a preset keeps
  no format and leaves each video's to detection.
- Press a preset to give the video all of it at once. It's lit while the
  video has everything of it, and **Save for this video** keeps it with
  the video, as with any change.
- **Edit** lists the presets in place of the tab's page to rename or
  delete them; Delete asks again first. **Done** or any tab goes back.
- While you watch, hold the left grip and press D-pad left for the next
  preset; a message names it. Settings › Controller can move it to another
  button.

## Subtitles

Subtitles come from inside the video or from files beside it, named after
it: `Movie.srt`, `Movie.en.srt`, `Movie.de.forced.ass`. Jellyfin and Emby
servers offer the subtitle files they keep, and DeoVR feeds and HereSphere
APIs those they list. Any video can also get subtitles from
[OpenSubtitles](#find-subtitles-on-opensubtitles).

Settings › Sound and subtitles › **Subtitles** sets what a video starts with:

- **Automatic** (the default): subtitles in your language when the audio
  is in another one, forced subtitles for the parts in another language
  (signs, a few lines of foreign dialogue), and a subtitle file named
  exactly like the video.
- **Always**: subtitles in your language whenever the video has them.
- **Off**: none.

**Your language** starts as the one Steam is set to. While a video plays,
the pill beside its format on the control bar names the subtitles showing,
and the audio track when there is more than one. Press it for the audio
tracks and subtitles side by side, subtitles in your language first, each
with what tells it apart: forced, SDH, its title or file name, and its
format. Pick one and it plays at once; a long list scrolls. Press the pill
again, or point off the lists and press, to close it. With no tracks to
list, the pill says **Subtitles**, and its panel still has **Find
subtitles**.

What you pick there is kept with the video: it opens with the same audio
track or subtitles next time, while they are still there. Only what you
picked is kept, so a video whose subtitles you never changed follows
**Subtitles** above, even after you change it.

Subtitles show in any script, with the fonts on the headset: Arabic and
Hebrew read right to left, and Chinese, Japanese or Korean need a font
that covers them. If subtitles in a language show nothing or boxes, copy a
font for it (the Noto fonts cover every language) into
`~/.local/share/fonts` and restart FramePlayerX; frameplayerx.log names
the characters it had no font for.

### Find subtitles on OpenSubtitles

1. Open the audio and subtitles panel and choose **Find subtitles**. The
   first search looks for the video by its server's IDs, or else its file
   name, in your subtitle language. Subtitles made for the same file come
   first, then the most downloaded.
2. Edit the title or language and press **Search** to try again. **Refine
   search** has year and episode fields and filters for forced subtitles,
   SDH and machine translations. When a search by what the video is finds
   nothing, **Search by title** tries its title alone.
3. Pick a result, then press **Download & use**, or **Use** for one you
   downloaded before. **File match** means OpenSubtitles recognizes the
   video file itself. If a result has several files, pick the part you
   want.

Downloads stay on your headset with the video and can be reused offline.
While a downloaded file is selected, use **Earlier** or **Later** in the
subtitles panel to shift its timing by 100 milliseconds per press. **Reset**
returns to the original timing. **Remove download** deletes that file from
the headset. Clearing watch history keeps downloaded subtitles.

Downloads work without an account. To use your account's download
allowance, press **Sign in** at the top of **Find subtitles**, or go to
Settings › Sound and subtitles › **OpenSubtitles**. Sign-in is kept on
this device until you sign out, and subtitles you downloaded stay after
that. FramePlayerX contacts OpenSubtitles only when you use **Find
subtitles** or sign in.

## Hardware decoding

The Frame's hardware decoder plays 4:2:0 HEVC (H.265, 8- and 10-bit) and
H.264 (8-bit) smoothly up to 8K. The Frame has no hardware decoder for AV1,
4:2:2 or 4:4:4 colour or 10-bit H.264, so those decode on the CPU, which
can fall behind at VR sizes; a warning pill on the control bar shows when a
video plays this way, and pointing at it says why ("No hardware decoding
for AV1", "… for 4:2:2 HEVC"). Converted once to 4:2:0 HEVC on a PC, they
play as smoothly as the rest. VP9 decodes on the CPU too and keeps up at
lower resolutions; at 8K, convert it as well.

"Too big for hardware" means the file can't fit even with nothing else
open: the decoder takes at most 8192×4320 pixels a frame. 8192×4320 and
5760×5760 fit; 6144×6144, 7200×7200, 8192×8192 and larger always decode on
the CPU, so convert them to a smaller size.

"Decoder busy" means the file fits, but other video sessions leave it too
little room. Every session counts, idle or not, recording as well as
playing:

| Other sessions open | Tallest video 8192 wide |
|---|---|
| None | 4320 |
| 1 | 4064 |
| 2 | 2720 |
| 3 | 2016 |

Beside one other session, 8K UHD (7680×4320), 5760×5760 and 8-bit 8192×4096
HEVC still play in hardware; 10-bit 8192×4096 HEVC, 8192×4096 H.264 and
8192×4320 need nothing else open. The warning pill's tip names who holds the
decoder, and the decoder pill's tip also says how much room was left.

Recording in the background in Steam holds a session while an app is
running. When turning it off would make room, the warning pill's tip says so:
choose **Recording Off** in Steam Settings › Game Recording and open the video
again, which works at once.

Steam's browser holds a session after Steam starts, and normally lets go of it
once you start using the headset. If a video stays busy, restart the headset
and open the video again.

"Decoder refused it" means the decoder tried and refused the file;
frameplayerx.log says why. A video whose pictures the hardware decoder
reads as broken, picture after picture, plays in software instead.

10-bit video takes more memory in hardware than 8-bit: the decoder keeps
the pictures it predicts from in the buffers it shares with the GPU, as
many as the video says it needs, so an 8K 10-bit video typically holds
about 0.9 GB of them while it plays, where 8K 8-bit holds 0.4 GB. The line
starting `V4L2 capture` in frameplayerx.log says how many buffers a video
got and why. If the decoder can't get as many as the video's pictures
need, the video plays in software straight away, which can't keep up at 8K
10-bit: the warning pill says "Decoder busy", and the log says how many
buffers the decoder got and how many it needed. If the hardware decoder
gives no picture for 5 seconds, the video carries on in software too; the
log's `V4L2 decoder stalled` line then says where the decoder's buffers
were, which is worth including in a bug report.

A damaged stretch of a file, such as a part a download hasn't filled in
yet, doesn't send the video to software. Playback carries on in hardware
from the next keyframe that can be read, without reading the damaged
part; when getting there holds the picture for more than a second,
"Skipping damaged video…" shows over it meanwhile. What lay in between
isn't shown. A *Damaged video* pill on the control bar then stays for the
rest of the video; point at it to see how many stretches were skipped and
how much video in all.

The pill next to the format on the control bar says which decoder plays
the video: HW, or SW in amber, with the codec and bit depth. Point at it
to see the decoder, the format and the resolution, and why a video isn't
decoded in hardware.

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
on the adjust panel's [Format](#changing-the-format) tab.

From a server, the server can instead transcode such a video as it plays,
without converting anything: see [Server transcoding](#server-transcoding).

## Server transcoding

Jellyfin, Emby and Plex can transcode a video live, while it plays, and DeoVR
feeds and HereSphere APIs may offer heights their server transcodes to on
request. Nothing is made ahead of time: the server starts encoding from
the file when FramePlayerX asks, and stops when it stops asking. For those
videos the pill on the control bar starts with the **Quality** playing;
press it for the Quality list beside the audio tracks and subtitles:

- **Automatic** (the default): the file as it is. When the file can't play
  here (a format FramePlayerX has no decoder for, or a picture the hardware
  decoder has no room for), it asks the server to transcode it to the
  tallest height offered and says so on the control bar. It never changes
  the quality while a video plays.
- **Original**: the file as it is, whatever happens.
- A height (**2160p**, **1440p**, …): the server transcodes the file to at
  most that height, the width in proportion, from where you were.

The file always comes first: the server only transcodes when you pick a
height, or under Automatic when the file can't play.

A source can also have a maximum height, which caps the heights offered
and the version a server's Automatic picks. The app has no field for it:
with FramePlayerX closed, add `"max_height": 2160` (or another height) to
the source's entry in `~/.config/frameplayerx/sources.json`. Jellyfin,
Emby, Plex, DeoVR feeds and HereSphere APIs take it.

Skipping while the server transcodes asks it to start again from there,
which takes a few seconds. A feed's height marked "can't skip" is
transcoded only from the start. Jellyfin, Emby and feeds keep the server's
default audio track and none of the file's own subtitles; subtitle files
still show. While Plex transcodes, the audio and subtitle lists select
the original server tracks and restart from the current position, keeping
the pause state. Selected Plex subtitles are burned into the picture.
Downloaded subtitles also work while Plex transcodes. Selecting one turns
off any Plex subtitle and plays the downloaded file locally.
Plex uses progressive H.264/AAC video; multipart originals require a
server transcode. Its server must permit transcoding. HLS-only delivery
is not supported.

The server does the work, so how far it can go depends on it: a server
without a graphics card may not keep up with an 8K file, whatever the
size asked. Jellyfin and Emby send H.264, or HEVC when their admin allowed
it (Dashboard › Playback › Transcoding), and pick their own limits:
Jellyfin transcodes to at most 3840 pixels wide, so an 8K 180° video's
2880p and 2160p both come as 3840×1920.

## Display

**Settings › General › Display** sets how the headset draws the picture.
Changes apply at once, also while a video plays.

| Setting | Choices | To begin with |
|---|---|---|
| Refresh rate | 72, 90, 120 Hz | 120 Hz |
| Render resolution | 2160, 3072, 3744 pixels across each eye | 3744 |
| Foveated rendering | Off, Low, High | Low |

- **Refresh rate**: at 120 Hz, 24, 30 and 60 fps videos get even frame
  timing. A lower rate gives each frame more time and saves battery.
- **Render resolution**: the size of the picture drawn for each eye. Above
  the Frame's 2160-pixel panels, 8K video looks sharper and fine detail
  shimmers less. The menus and the control bar look the same at any
  resolution.
- **Foveated rendering**: the picture keeps full detail within 30° of where
  you look (Low) or 18° (High) and less beyond, where your eyes don't see
  detail anyway. It follows your eyes; without eye tracking it stays in the
  middle of each lens. Menus and the pointer always keep full detail.

As they start, these settings play the most demanding videos (8K 10-bit,
and passthrough videos with a mask) at full frame rate. With foveation off,
8K 10-bit video needs a lower resolution or refresh rate to play smoothly.

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
| X / D-pad left | Previous video in the list you opened it from | [Adjust panel](#adjusting-a-video) |
| Y / D-pad up | Next video | Show or hide the controls |
| Menu / View | Browse | Recenter |
| Bumper | Forward 1 minute | Back 1 minute |
| Grip + thumbstick | Up / down raises or lowers you | Left / right turns the picture, down zooms in, up zooms out |
| Grip + X / Y | Previous / next [chapter or bookmark](#chapters-and-bookmarks) (back within 3 s of one's start goes to the one before) | Grip + D-pad left: the next [preset](#presets); D-pad up as without the grip |
| Grip + A | Bookmark where you are | As without the grip |
| Grip + thumbstick press | Subtitles off, or back on: the ones you had, else those in your language | As without the grip |
| Hold grip + trigger, move | Drag the picture (a flat screen or the dome follows your hand) | The same |
| Trigger | Click; on empty space, show or hide the controls | The same |

On the control bar, point at the speaker for the volume slider above it;
press the speaker to mute. Pointing along the seek bar shows the time there,
and a picture of that moment where the server has made them: Jellyfin's
trickplay images, Emby's and Plex's video preview thumbnails, or a DeoVR
feed's timeline preview. **Settings › Playback › Seek bar previews** turns
the pictures off. What a controller shortcut changes, such as the
volume or a minute's skip, shows for a moment in a small message above where
the control bar sits.

### Recentering and moving things

To recenter, hold the Steam button, as in any game (both grips together, or
the left Menu button, do it too). The picture and the panels come to where
you are looking; looking well up or down (lying back, say) counts, a glance
a little up or down doesn't, so sitting up a flat screen comes back level.
The browse panel (upright, a little below where you look) and a flat screen
come back in front of you, wherever you had moved them (a 180° or 360°
picture keeps its turn). A flat screen opens upright at eye height, the way
the browse panel faces: where you move it lasts while you watch, and isn't
kept with the video. Only the control bar is tilted back towards you.

Each piece moves on its own and takes what hangs off it along. Drag the
picture (grip + trigger) or use Yaw and Pitch to move a flat screen: the
control bar and the adjust panel go with it. Hold grip on the control bar
to move it about the screen: the adjust panel goes with it. Hold grip on
the adjust panel to move it about the bar. During a 180° or 360° video,
Yaw, Pitch and dragging turn only the picture; the control bar and adjust
panel stay where they are. The browse panel (or the keyboard under it)
moves the same way, with grip, and stays where you leave it. To put them
all back where they start, press **Put back** at the top of the adjust
panel's Position tab: a flat screen, the control bar, the adjust panel and
the browse panel return (a 180° or 360° picture keeps its turn; the
panel's **Reset** puts the picture's own adjustments back).

Drags, turns, tilts and zoom count as adjustments: keep them for that video
with **Save for this video** on the adjust panel, or press the left
thumbstick to undo them.

### Watching lying down

Lie back, look where you want the picture, and hold the Steam button to
recenter: it comes up to where you look. Or turn on **Screen follows you**
(the bed button on the control bar, or Settings › General): a flat video
and the menus then follow your head and stay in front of you as you turn,
lie back or roll onto your side. 180° and 360° videos don't follow, since
you look around those. For stereo 180° and fisheye videos,
[Comfort focus](#comfort-focus) keeps close things from doubling when you
watch lying down.

### Chapters and bookmarks

The adjust panel's **Chapters** tab lists the video's chapters and your
bookmarks (**Bookmark** on the control bar, or grip + A on the right
controller, adds one where you are), each with a button to go there;
grip + X and grip + Y step through them. Your bookmarks also have a button
to remove them. Pressing Bookmark within a second of one you already have
adds no other: it says "Already bookmarked here".

### Changing the controls

Every button and thumbstick direction above can be changed in Settings ›
Controller: point at a button, pull the trigger and pick an action.
**Grip + press** above the controllers shows what each does with that
controller's grip held, and changes it there; one left at **Same as press**
does what it does without the grip. **Reset to defaults** puts this layout
back. For example, to zoom with the right thumbstick, set the right
controller's thumbstick **↑↓ Up / down** (or **Grip + ↑↓**) to Zoom. The
trigger, grip + trigger drags, both grips, menu paging and slider steps
are fixed.

### Typing

Text fields bring up a keyboard below the panel you type in; **#+=**
switches it to symbols (`! # $ % ( ) [ ] { }` and the rest) and **abc**
back. You can also type on your phone through the
[web remote](#remote-control).

### Hands-free navigation

To use FramePlayerX without a controller, press the hands-free key (a
person in a frame) in the library's top bar, beside **Room**, and put the
controllers down; press it again, or rest the dot on it, to turn it off. A
dot then shows where your head points: rest it on a button, a card or a spot
on a slider until the ring round it fills, and that is pressed, as the
trigger would press it. The ring fills only while your head is still, so
the dot passing over things presses nothing. **Hold time** in Settings ›
Controller sets how long that takes (0.75 s to start with). Once something is pressed, nothing else
is until you look away from it, so what opens in its place isn't pressed
too; to press the same button again, look off it and back.

What doesn't fit scrolls with keys just outside a panel's right edge: up
and down beside a page, and back and on level with each row of pictures,
each moving it by most of what shows. While a video plays the control bar hides as usual: look up above
the picture and **Show controls** appears there; rest the dot on it to
bring the bar back. It stays out of sight while you watch.

The screen and the menus stay where they are, since you couldn't point at
them if they followed your head: **Screen follows you** is off, and on the
control bar **Recenter** takes the bed button's place. FramePlayerX also
starts facing where you look, so lying down the menus come up where you
can reach them. Move or press a controller and it works at once; the dot
comes back once both controllers have been left still for 3 seconds.

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
the screen: turn on **Remove the background**.

For a video without its own mask and with no Passthrough settings of its
own yet, the colour to remove is found in the video itself, usually a
second or two after it starts playing. It comes from a frame in the middle of the video,
so a studio's intro doesn't decide it, and is used at full strength like
the one-tap colours. The Colour row then says "Found in this video". Until
then, or if no screen colour is found in that frame, the colour from
**Settings > Passthrough** is used.

To use another colour, pick green screen, blue screen or magenta, or set one
with the red/green/blue sliders; that replaces the found one.
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

With **Settings › Remote control › Phone and browser remote** on, your
phone controls the video playing in the headset: play and pause, skipping,
seeking (the bar splits at chapters and bookmarks), previous and next in Up
next, volume and mute, audio and subtitles, speed, Recenter and closing the
video. **Up next** lists what plays in turn (tap one to play it), with its
**Loop**; **Loop video** and **Autoplay** are the switches beside it, the
same ones as on the headset. The field at the bottom types into the headset,
into whichever field is open there. To pair a phone,
open the address that tab shows (such as `192.168.1.20:8790`) in the phone's
browser and enter the 6-digit code under it; the phone stays paired. A code
pairs one phone, and stops working ten minutes after the tab last showed it.
After five wrong codes pairing stops until you press **New pairing code**.
**Unpair all phones** makes every paired phone pair again.

The **DeoVR remote API** (port 23554) works with apps that speak DeoVR's
protocol, without pairing. Both only answer devices on your local network.
The phone's page is in the headset's language, and changes with it.

## Language

FramePlayerX comes in English, German, Spanish, French, Japanese and
Simplified Chinese. **Settings › General › Language** starts on
**Automatic**, which follows the language Steam is set to (English when
Steam's isn't one of these); choose one to keep it whatever Steam's is.
Server names, titles and other text from your servers show as they come.

The translations started as machine drafts, so some wording is likely to be
wrong or awkward. To correct it, open
[Codeberg Translate](https://translate.codeberg.org/projects/frameplayerx/),
sign in with a Codeberg account, pick a language and edit its messages; no
Git is needed. A note beside each message says where it shows, and "Short"
marks a button, chip or tab, which should stay about as long as the English.
To report a mistake without correcting it, open a
[Translation problem](https://github.com/remotesojourner/frameplayerx/issues/new?template=translation.yml)
issue with the language, where it shows and the wording you'd expect.

## Troubleshooting

- When a video stutters, turn on Settings › General › **Performance
  overlay**. It starts to your left. Squeeze a grip on it to move it, and it
  stays where you leave it, next time too. Amber marks trouble.
  - **Display**: frames drawn against the display's rate, and how long the
    app's work on each one typically takes. Low means the headset can't draw
    fast enough.
  - **Video**: frames shown against the film's own rate, and frames dropped
    and stalls since it opened.
  - **Decoder**: hardware or software, and the format.
  - **Buffer**: seconds of video read ahead, against how many it's aiming for.
    Falling means the source isn't keeping up.
  - **Network**: how fast data is arriving, and how many requests have failed.
    With a full buffer it only reads what the film uses, or nothing, so that
    isn't your connection's limit.
  - **Last skip**: how long the last skip took to show a picture, and how long
    its first read of the file took. It says **Opened** until you skip.
  - **CPU** and **GPU**: how busy and how hot they are, amber when the headset
    is slowing them for heat, and the GPU's clock.
  - **Memory**: how much is in use.

  Without a video open, it shows only Display, CPU, GPU and Memory.
- After a crash, the next start asks whether to send a crash report to
  FramePlayerX's developers, as a private issue on GitHub. It contains the
  versions of FramePlayerX and the headset's system, what went wrong, and
  the log of that session with the names of your videos, files and folders,
  your servers' addresses and any password replaced by placeholders such as
  `<video 1>`. **Show the report** shows exactly what would be sent;
  nothing is sent unless you choose **Send report**.
- For help, or to report a problem yourself, **Settings › About** starts
  with links to FramePlayerX's website, this guide, its Discord server, its
  GitHub page, its issue form (choose Bug report and attach the log below)
  and its GitHub Sponsors page. A link opens in Steam's browser; the
  address on it works on a phone or a computer too.
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
