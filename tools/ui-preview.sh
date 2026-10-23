#!/bin/bash
# Renders every FramePlayerX screen to flat PNGs for design review, with the
# desktop preview (no headset; any Vulkan driver, lavapipe on CI).
#
# Usage: tools/ui-preview.sh OUT_DIR
# Runs against a throwaway home with a small library made from the test
# clips, so it never touches your own settings or library.
set -euo pipefail
cd "$(dirname "$0")/.."
out="$(mkdir -p "${1:?usage: $0 OUT_DIR}" && cd "$1" && pwd)"
data=crates/fp-media/tests/data

home="$(mktemp -d)"
trap 'rm -rf "$home"' EXIT
mkdir -p "$home/Videos" "$home/Downloads"
cp "$data/h264_aac_180_LR.mp4" "$home/Videos/Sunset Beach Walk_180_LR.mp4"
cp "$data/camera_clip.mp4" "$home/Videos/Garden Party.mp4"
cp "$data/vp9.webm" "$home/Videos/Mountain Lake_360.webm"
cp "$data/av1_opus.webm" "$home/Videos/City Lights_180_SBS.webm"
cp "$data/hevc10_tb.mkv" "$home/Videos/Aurora Timelapse_360_TB.mkv"
# Different dates, and a folder, for the browse shots' sort orders.
mkdir -p "$home/Videos/Holidays"
touch -d 2026-03-14 "$home/Videos/Sunset Beach Walk_180_LR.mp4"
touch -d 2026-09-02 "$home/Videos/Garden Party.mp4"
touch -d 2025-11-20 "$home/Videos/Mountain Lake_360.webm"
touch -d 2026-06-30 "$home/Videos/City Lights_180_SBS.webm"
touch -d 2026-01-08 "$home/Videos/Aurora Timelapse_360_TB.mkv"
# The Videos library, and one to lock with a PIN.
mkdir -p "$home/Private" "$home/.config/frameplayerx"
cp "$data/camera_clip.mp4" "$home/Private/Rooftop.mp4"
(umask 077 && cat >"$home/.config/frameplayerx/sources.json" <<EOF
[{"type": "library", "id": "videos", "name": "Videos", "folder": "$home/Videos"},
 {"type": "library", "id": "private", "name": "Private videos", "folder": "$home/Private"}]
EOF
)
# A video with Arabic (joined letters, right to left) and English subtitles.
mkdir -p "$home/Subtitled"
cp "$data/h264_aac_180_LR.mp4" "$home/Subtitled/Lantern.mp4"
cat >"$home/Subtitled/Lantern.ar.srt" <<'EOF'
1
00:00:00,000 --> 00:10:00,000
مرحبًا، هذه ترجمة عربية
فيها كلمة English أيضًا
EOF
cat >"$home/Subtitled/Lantern.en.srt" <<'EOF'
1
00:00:00,000 --> 00:10:00,000
Hello, these are English subtitles
EOF

script="$home/ui.fps"
cat >"$script" <<EOF
# Library scan and thumbnails. Nothing watched yet: Home offers sources.
wait 4
panel main home
# The first source tile pointed at: lifted whole at the row's edge.
point main 0.13 0.70
frames 8
panel main home-hover-first
screen history
panel main history-empty
# A library: its videos, then its folders. The headset's own folders.
open-source videos
wait 1
panel main library
library-folders
wait 1
panel main library-folders
open-source on-device
panel main local
# Playlists: none yet, one made of the library, its page; Up next.
details
panel main details
screen playlists
panel main playlists-empty
pick
panel main playlist-picker
playlist Evening
screen playlists
panel main playlists
open-playlist
panel main playlist
screen sources
panel main sources
# Adding a library (a suggestion for Downloads), and editing one.
add-source library
panel main source-add-library
edit-source videos
panel main source-edit-library
# Browsing a folder of the home source, by name and then newest first.
browse device-home $home/Videos
wait 1
panel main browse
browse-sort newest
panel main browse-newest
browse-sort name
# A source locked with a PIN: its row, the unlock form, its PIN settings.
lock private 2468
screen sources
panel main sources-locked
pin-form private
panel main source-unlock
pin-form private wrong
panel main source-unlock-wrong
unlock private 2468
pin-form private set
panel main source-pin
screen settings general
panel main settings-general
screen settings playback
panel main settings-playback
screen settings position
panel main settings-position
screen settings sound
panel main settings-sound
screen settings passthrough
panel main settings-passthrough-locked
unlocked
screen settings passthrough
panel main settings-passthrough
chroma-global
screen settings passthrough
frames 8
panel main settings-passthrough-on
screen settings controller
panel main settings-controller
screen settings controller-open
panel main settings-controller-open
screen settings controller-stick
panel main settings-controller-stick
haptics on
screen settings haptics
panel main settings-haptics
screen settings remote
panel main settings-remote
screen settings updates
panel main settings-updates
screen settings about
panel main settings-about
# Hover state on the first card of a library's videos.
open-source videos
point main 0.12 0.45
frames 8
panel main local-hover
# Its title instead: that opens the details.
point main 0.12 0.63
frames 8
panel main local-hover-title
# The keyboard, from the search field.
point main 0.12 0.30
click
frames 8
panel keyboard keyboard
# Its symbols page (the #+= key), then back to letters.
point keyboard 0.195 0.80
click
frames 8
panel keyboard keyboard-symbols
click
frames 3
button secondary
frames 3
# The player.
open $home/Videos/Sunset Beach Walk_180_LR.mp4
wait-playing
wait 1
pause
point bar 0.5 0.5
frames 8
panel bar player-bar
sbs player-scene
# A hover label: the Hide button.
point bar 0.883 0.69
frames 10
panel bar player-bar-tip
# The adjustments panel.
adjust
frames 8
panel adjust adjust
panel bar player-bar-adjust
# The Position tab, with Put back for moved panels.
adjust 1
frames 8
panel adjust adjust-position
# The Passthrough tab, with chroma key on (unlocked above).
adjust 4
chroma
mask
frames 8
panel adjust adjust-passthrough
# Software decoding falling behind.
slow-decode
frames 8
panel bar player-bar-slow-decode
# What was watched: the history, and Home's banner, over the paused video
# (Back closes the adjustments, then shows the browser).
button secondary
frames 3
button secondary
frames 3
screen history
frames 4
panel main history
screen home
frames 4
panel main home-watched
# Subtitles in Settings › Sound and subtitles, with the languages listed.
screen settings sound
languages
point main 0.6 0.6
stick 0 -1 30
frames 8
panel main settings-subtitles
# The video with subtitle files, set to start in Arabic: the subtitles and
# the Audio & subs tab.
subtitles always ar
open-in device-home $home/Subtitled/Lantern.mp4
wait-playing
wait 1
pause
frames 8
panel subs subtitles-arabic
adjust 5
frames 8
panel adjust adjust-subtitles
# The library queued behind it, and Up next from the control bar.
queue-all
up-next
frames 8
panel main up-next
EOF

cargo build -p frameplayerx
HOME="$home" XDG_CONFIG_HOME="$home/.config" XDG_DATA_HOME="$home/.local/share" \
  XDG_CACHE_HOME="$home/.cache" \
  "${CARGO_TARGET_DIR:-target}/debug/frameplayerx" --preview "$out" --script "$script" --size 1400
ls -l "$out"
