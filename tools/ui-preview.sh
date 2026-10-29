#!/bin/bash
# Renders every FramePlayerX screen to flat PNGs for design review, with the
# desktop preview (no headset; any Vulkan driver, lavapipe on CI).
#
# Usage: tools/ui-preview.sh OUT_DIR
# UI_LANGUAGE=de (or pseudo, a made-up longer language) renders them in
# another language.
# Runs against a throwaway home with a small library made from the test
# clips, so it never touches your own settings or library.
set -euo pipefail
cd "$(dirname "$0")/.."
out="$(mkdir -p "${1:?usage: $0 OUT_DIR}" && cd "$1" && pwd)"
data=crates/fp-media/tests/data

home="$(mktemp -d)"
# Fonts you installed yourself, such as the headset's Noto Sans CJK for
# UI_LANGUAGE=ja or zh-CN, which the throwaway home would hide.
if [ -d "$HOME/.local/share/fonts" ]; then
  mkdir -p "$home/.local/share"
  ln -s "$HOME/.local/share/fonts" "$home/.local/share/fonts"
fi
jf_port_file="$home/jf.port"
python3 - "$home" "$jf_port_file" <<'PY' &
import glob, http.server, json, os, random, sqlite3, sys, urllib.parse

home, port_file = sys.argv[1], sys.argv[2]

def thumb_for(title_prefix):
    db_path = os.path.join(home, ".local/share/frameplayerx/library.sqlite3")
    thumb_dir = os.path.join(home, ".cache/frameplayerx/thumbnails")
    if os.path.exists(db_path):
        with sqlite3.connect(db_path) as db:
            row = db.execute(
                "SELECT id FROM media WHERE title LIKE ? LIMIT 1",
                (title_prefix + "%",),
            ).fetchone()
            if row:
                p = os.path.join(thumb_dir, f"{row[0]}.jpg")
                if os.path.exists(p):
                    return p
    jpgs = sorted(glob.glob(os.path.join(thumb_dir, "[0-9]*.jpg")))
    return jpgs[0] if jpgs else None

THUMBS = {
    "lib_vr": "Sunset Beach Walk",
    "lib_movies": "City Lights",
    "lib_nature": "Mountain Lake",
    "v_aurora": "Aurora Timelapse",
    "v_mountain": "Mountain Lake",
    "v_garden": "Garden Party",
    "v_sunset": "Sunset Beach Walk",
    "v_city": "City Lights",
}

VIEWS = {"Items": [
    {"Id": "lib_vr", "Name": "VR Videos", "Type": "CollectionFolder", "CollectionType": "homevideos", "ChildCount": 5},
    {"Id": "lib_movies", "Name": "Movies", "Type": "CollectionFolder", "CollectionType": "movies", "ChildCount": 18},
    {"Id": "lib_nature", "Name": "Nature & Travel", "Type": "CollectionFolder", "CollectionType": "homevideos", "ChildCount": 9},
]}

RESUME = {"Items": []}

LATEST_VR = [
    {"Id": "v_sunset", "Name": "Sunset Beach Walk", "Type": "Video", "ProductionYear": 2026,
     "RunTimeTicks": 14200000000, "Path": "/media/Sunset Beach Walk_180_LR.mp4",
     "UserData": {"PlaybackPositionTicks": 6500000000}},
    {"Id": "v_aurora", "Name": "Aurora Timelapse", "Type": "Video", "ProductionYear": 2026,
     "RunTimeTicks": 8400000000, "Path": "/media/Aurora Timelapse_360_TB.mkv",
     "UserData": {"IsFavorite": True}},
    {"Id": "v_city", "Name": "City Lights", "Type": "Video", "ProductionYear": 2026,
     "RunTimeTicks": 9800000000, "Path": "/media/City Lights_180_SBS.webm",
     "UserData": {"PlaybackPositionTicks": 3400000000}},
    {"Id": "v_mountain", "Name": "Mountain Lake", "Type": "Video", "ProductionYear": 2025,
     "RunTimeTicks": 11600000000, "Path": "/media/Mountain Lake_360.webm"},
    {"Id": "v_garden", "Name": "Garden Party", "Type": "Video", "ProductionYear": 2026,
     "RunTimeTicks": 7200000000, "Path": "/media/Garden Party_180_LR.mp4",
     "UserData": {"Played": True}},
]

# What the VR Videos grid can be filtered by.
FACETS = {
    "Genres": ["Documentary", "Nature", "Timelapse", "Travel"],
    "Studios": ["Coastline Films", "Northlight VR"],
    "Years": ["2026", "2025"],
    "Tags": ["beach", "city", "drone", "night", "sunset"],
}

# The people a search finds (one without a picture); each is in a few videos.
PEOPLE = [
    {"Id": "p_riley", "Name": "Riley Brooks", "Type": "Person", "ImageTags": {"Primary": "r"}},
    {"Id": "p_rivera", "Name": "Alex Rivera", "Type": "Person", "ImageTags": {}},
]

def grid_items(query):
    """The VR Videos library: by id for a shuffle's pages, shuffled when asked.
    A search's videos by title, a person's by who they are."""
    q = urllib.parse.parse_qs(query)
    if "SearchTerm" in q:
        term = q["SearchTerm"][0].lower()
        if "Video" not in q.get("IncludeItemTypes", [""])[0].split(","):
            return []
        return [i for i in LATEST_VR if term in i["Name"].lower()]
    if q.get("PersonIds"):
        return LATEST_VR[1:4]
    if q.get("ParentId") != ["lib_vr"]:
        return []
    ids = q.get("Ids", [""])[0]
    if ids:
        by_id = {i["Id"]: i for i in LATEST_VR}
        return [by_id[i] for i in ids.split(",") if i in by_id]
    items = list(LATEST_VR)
    if q.get("SortBy") == ["Random"]:
        random.shuffle(items)
    return items

class Handler(http.server.BaseHTTPRequestHandler):
    def log_message(self, *args):
        pass
    def do_GET(self):
        path = self.path.split("?", 1)[0]
        if path.startswith("/Items/") and "/Images/" in path:
            item_id = path.split("/")[2]
            jpg = thumb_for(THUMBS.get(item_id, "Sunset"))
            if jpg and os.path.exists(jpg):
                with open(jpg, "rb") as f:
                    data = f.read()
                self.send_response(200)
                self.send_header("Content-Type", "image/jpeg")
                self.send_header("Content-Length", str(len(data)))
                self.end_headers()
                self.wfile.write(data)
                return
        query = self.path.split("?", 1)[1] if "?" in self.path else ""
        if path.endswith("/Views"):
            body = VIEWS
        elif path == "/Users/u1/Items":
            items = grid_items(query)
            body = {"Items": items, "TotalRecordCount": len(items)}
        elif path == "/Persons":
            term = urllib.parse.parse_qs(query).get("SearchTerm", [""])[0].lower()
            body = {"Items": [p for p in PEOPLE if term in p["Name"].lower()]}
        elif path == "/Items/Filters":
            body = {"Tags": FACETS["Tags"], "Genres": FACETS["Genres"]}
        elif path.lstrip("/") in FACETS:
            body = {"Items": [{"Name": n} for n in FACETS[path.lstrip("/")]]}
        elif path.endswith("/Items/Resume"):
            body = RESUME
        elif path.endswith("/Items/Latest"):
            body = LATEST_VR if "ParentId=lib_vr" in self.path else []
        else:
            body = {"Items": [], "TotalRecordCount": 0}
        raw = json.dumps(body).encode()
        self.send_response(200)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(raw)))
        self.end_headers()
        self.wfile.write(raw)

srv = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Handler)
with open(port_file, "w") as f:
    f.write(str(srv.server_port))
srv.serve_forever()
PY
jf_pid=$!
trap 'kill "$jf_pid" 2>/dev/null || true; rm -rf "$home"' EXIT
while [ ! -s "$jf_port_file" ]; do sleep 0.05; done
jf_port="$(cat "$jf_port_file")"
mkdir -p "$home/Videos" "$home/Downloads"
cp "$data/camera_clip.mp4" "$home/Videos/Sunset Beach Walk.mp4"
cp "$data/h264_aac_180_LR.mp4" "$home/Videos/Garden Party.mp4"
cp "$data/vp9.webm" "$home/Videos/Mountain Lake_360.webm"
cp "$data/av1_opus.webm" "$home/Videos/City Lights_180_SBS.webm"
cp "$data/hevc10_tb.mkv" "$home/Videos/Aurora Timelapse_360_TB.mkv"
# Different dates, and a folder, for the browse shots' sort orders.
mkdir -p "$home/Videos/Holidays"
touch -d 2026-03-14 "$home/Videos/Sunset Beach Walk.mp4"
touch -d 2026-09-02 "$home/Videos/Garden Party.mp4"
touch -d 2025-11-20 "$home/Videos/Mountain Lake_360.webm"
touch -d 2026-06-30 "$home/Videos/City Lights_180_SBS.webm"
touch -d 2026-01-08 "$home/Videos/Aurora Timelapse_360_TB.mkv"
# The Videos library, a Jellyfin server, and a library to lock with a PIN.
mkdir -p "$home/Private" "$home/.config/frameplayerx"
cp "$data/h264_aac_180_LR.mp4" "$home/Private/Rooftop.mp4"
if [ -n "${UI_LANGUAGE:-}" ]; then
  printf '{"language": "%s"}\n' "$UI_LANGUAGE" >"$home/.config/frameplayerx/settings.json"
fi
(umask 077 && cat >"$home/.config/frameplayerx/sources.json" <<EOF
[{"type": "library", "id": "videos", "name": "Videos", "folder": "$home/Videos"},
 {"type": "mediabrowser", "id": "jellyfin", "name": "Jellyfin", "url": "http://127.0.0.1:$jf_port", "access_token": "tok", "user_id": "u1"},
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
wait 6
panel main home
# The first source tile pointed at: lifted whole at the row's edge.
point main 0.13 0.70
frames 8
panel main home-hover-first
point main 0.5 0.99
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
open-source jellyfin
wait 1
panel main jellyfin
# A library of the server, and its Filters drawer (Back closes it).
point main 0.133 0.37
click
wait 1
panel main jellyfin-grid
filters
wait 1
panel main jellyfin-filters
button secondary
frames 4
# Search: titles, and the people whose names match.
button secondary
frames 4
jellyfin-search ri
wait 1
panel main jellyfin-search
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
chroma-global off
screen settings controller
panel main settings-controller
screen settings controller-open
panel main settings-controller-open
screen settings controller-grip
panel main settings-controller-grip
screen settings controller-stick
panel main settings-controller-stick
haptics on
screen settings haptics
panel main settings-haptics
remote on
screen settings remote
panel main settings-remote
remote off
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
open $home/Videos/Sunset Beach Walk.mp4
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
look -21 -3
point bar 0.52 0.65
frames 4
shot player
look 0 0
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
frames 15
panel main home-watched
# Subtitles in Settings › Sound and subtitles, with the languages listed.
screen settings sound
languages
point main 0.6 0.6
stick 0 -1 30
frames 8
panel main settings-subtitles
# The video with subtitle files, set to start in Arabic: the subtitles, the
# audio and subtitles picker over the bar, and the Chapters tab.
subtitles always ar
open-in device-home $home/Subtitled/Lantern.mp4
wait-playing
wait 1
pause
frames 8
panel subs subtitles-arabic
tracks
frames 8
panel tracks tracks
panel bar player-bar-tracks
sbs player-tracks-scene
point tracks 0.5 0.02
click
frames 4
adjust 5
frames 8
panel adjust adjust-chapters
# The library queued behind it: Next on the bar, and Up next from it.
queue-all
point bar 0.6215 0.69
frames 10
panel bar player-bar-next
up-next
frames 8
panel main up-next
shuffle
frames 8
panel main up-next-shuffled
EOF

cargo build -p frameplayerx
HOME="$home" XDG_CONFIG_HOME="$home/.config" XDG_DATA_HOME="$home/.local/share" \
  XDG_CACHE_HOME="$home/.cache" \
  "${CARGO_TARGET_DIR:-target}/debug/frameplayerx" --preview "$out" --script "$script" --size 1400
ls -l "$out"
