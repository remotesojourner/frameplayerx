//! Web remote over real loopback HTTP.

mod common;

use common::*;
use fp_core::{PlayerCommand, SubtitleTrack};
use fp_remote::{
    RemoteAudio, RemoteDetails, RemoteEvent, RemoteHub, RemoteMarker, RemoteSubtitles,
};
use std::io::{Read, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpStream};
use std::time::{Duration, Instant};

fn started() -> (RemoteHub, SocketAddr, String) {
    let hub = hub();
    let addr = hub.start_web().unwrap();
    let token = hub.token();
    (hub, addr, token)
}

fn bearer(token: &str) -> String {
    format!("Bearer {token}")
}

const FORM: (&str, &str) = ("Content-Type", "application/x-www-form-urlencoded");

/// Enters `code` on the pairing page.
fn enter(addr: SocketAddr, code: &str) -> HttpResponse {
    http(
        addr,
        "POST",
        "/pair",
        &[FORM],
        format!("code={code}").as_bytes(),
    )
}

/// A code that isn't `code`.
fn not(code: &str) -> &'static str {
    if code == "000000" { "111111" } else { "000000" }
}

#[test]
fn everything_but_assets_requires_the_token() {
    let (_hub, addr, token) = started();
    for path in ["/api/status", "/api/picture", "/api/events"] {
        let r = http(addr, "GET", path, &[], b"");
        assert_eq!(r.status, 401, "{path}");
    }
    let r = http(addr, "POST", "/api/command", &[], br#"{"cmd":"play"}"#);
    assert_eq!(r.status, 401);
    let wrong = "fp_remote_token=0000";
    let r = http(addr, "GET", "/api/status", &[("Cookie", wrong)], b"");
    assert_eq!(r.status, 401);
    // The token never travels in an address, where history keeps it.
    let r = http(addr, "GET", &format!("/api/status?token={token}"), &[], b"");
    assert_eq!(r.status, 401);
    let r = http(addr, "GET", &format!("/?token={token}"), &[], b"");
    assert_eq!(r.status, 401);

    // Unpaired, the page asks for the headset's code; it is not the app.
    let r = http(addr, "GET", "/", &[], b"");
    assert_eq!(r.status, 401);
    assert!(r.text().contains("action=\"/pair\""));
    assert!(!r.text().contains("app.js"));

    // Static code needs no token (it holds no data).
    let r = http(addr, "GET", "/app.js", &[], b"");
    assert_eq!(r.status, 200);
    assert!(
        r.header("Content-Type")
            .unwrap()
            .starts_with("text/javascript")
    );
    let r = http(addr, "GET", "/app.css", &[], b"");
    assert_eq!(r.status, 200);

    // Bearer works too.
    let r = http(
        addr,
        "GET",
        "/api/status",
        &[("Authorization", &bearer(&token))],
        b"",
    );
    assert_eq!(r.status, 200);
}

#[test]
fn the_headsets_code_pairs_a_phone() {
    let (hub, addr, token) = started();
    assert_eq!(
        hub.pairing_address(Ipv4Addr::LOCALHOST),
        format!("127.0.0.1:{}", addr.port())
    );
    let r = enter(addr, "123456");
    assert_eq!(r.status, 401);
    assert!(r.text().contains("isn't showing a code"), "{}", r.text());

    let code = hub.pairing_code().unwrap().unwrap();
    let r = enter(addr, not(&code));
    assert_eq!(r.status, 401);
    assert!(r.text().contains("isn't the code"));
    assert!(r.header("Set-Cookie").is_none());

    // Typed as the headset shows it, with a space.
    let r = enter(addr, &format!("{}+{}", &code[..3], &code[3..]));
    assert_eq!(r.status, 303);
    assert_eq!(r.header("Location"), Some("/"));
    let cookie = r.header("Set-Cookie").unwrap();
    assert!(cookie.starts_with(&format!("fp_remote_token={token};")));
    assert!(cookie.contains("HttpOnly"));
    assert!(cookie.contains("SameSite=Strict"));
    assert_eq!(r.header("Referrer-Policy"), Some("no-referrer"));

    // The browser now sends the cookie.
    let pair = cookie.split(';').next().unwrap();
    let r = http(addr, "GET", "/", &[("Cookie", pair)], b"");
    assert_eq!(r.status, 200);
    assert!(r.text().contains("/app.js"));
    let csp = r.header("Content-Security-Policy").unwrap();
    assert!(csp.contains("script-src 'self'"));
    assert_eq!(r.header("X-Frame-Options"), Some("DENY"));

    hub.publish(&playing("/media/x.mp4", 7.0));
    let r = http(addr, "GET", "/api/status", &[("Cookie", pair)], b"");
    assert_eq!(r.status, 200);
    assert_eq!(r.header("Cache-Control"), Some("no-store"));
    let s = r.json();
    assert_eq!(s["location"], "/media/x.mp4");
    assert_eq!(s["title"], "Test video");
    assert_eq!(s["playing"], true);
    assert!(s["now_ms"].as_u64().unwrap() > 0);
    // Matches PlaybackStatus apart from the extra clock field.
    let back: fp_core::PlaybackStatus = serde_json::from_value(s).unwrap();
    assert_eq!(back.position, 7.0);

    // Used up: another phone needs the new code the headset shows next.
    assert!(enter(addr, &code).text().contains("isn't showing a code"));

    // Rotating the token unpairs.
    hub.regenerate_token().unwrap();
    let r = http(addr, "GET", "/api/status", &[("Cookie", pair)], b"");
    assert_eq!(r.status, 401);
}

#[test]
fn wrong_codes_stop_pairing() {
    let (hub, addr, _) = started();
    let code = hub.pairing_code().unwrap().unwrap();
    for _ in 0..5 {
        assert!(enter(addr, not(&code)).text().contains("isn't the code"));
    }
    let r = enter(addr, &code);
    assert_eq!(r.status, 401, "not even the right one");
    assert!(r.text().contains("Too many wrong codes"));
    assert_eq!(hub.pairing_code().unwrap(), None);

    hub.new_pairing_code();
    let code = hub.pairing_code().unwrap().unwrap();
    assert_eq!(enter(addr, &code).status, 303);
    let r = http(addr, "GET", "/pair", &[], b"");
    assert_eq!(r.status, 405);
}

#[test]
fn commands_and_text_reach_the_event_queue() {
    let (hub, addr, token) = started();
    let auth = bearer(&token);
    let h = [
        ("Authorization", auth.as_str()),
        ("Content-Type", "application/json"),
    ];

    let r = http(
        addr,
        "POST",
        "/api/command",
        &h,
        br#"{"cmd":"seek","position":5}"#,
    );
    assert_eq!(r.status, 200, "{}", r.text());
    assert_eq!(
        next_event(&hub),
        RemoteEvent::Command(PlayerCommand::Seek { position: 5.0 })
    );
    let r = http(
        addr,
        "POST",
        "/api/command",
        &h,
        br#"{"cmd":"open","location":"/media/1.mp4"}"#,
    );
    assert_eq!(r.status, 200);
    assert_eq!(
        next_event(&hub),
        RemoteEvent::Command(PlayerCommand::Open {
            location: "/media/1.mp4".into()
        })
    );
    let r = http(
        addr,
        "POST",
        "/api/command",
        &h,
        br#"{"cmd":"seek_relative","delta":-10}"#,
    );
    assert_eq!(r.status, 200);
    assert_eq!(
        next_event(&hub),
        RemoteEvent::Command(PlayerCommand::SeekRelative { delta: -10.0 })
    );

    for (body, cmd) in [
        (
            &br#"{"cmd":"set_volume","volume":0.5}"#[..],
            PlayerCommand::SetVolume { volume: 0.5 },
        ),
        (br#"{"cmd":"toggle_mute"}"#, PlayerCommand::ToggleMute),
        (br#"{"cmd":"next"}"#, PlayerCommand::Next),
        (
            br#"{"cmd":"select_audio","stream":2}"#,
            PlayerCommand::SelectAudio { stream: 2 },
        ),
        (
            br#"{"cmd":"select_subtitles","track":{"stream":3}}"#,
            PlayerCommand::SelectSubtitles {
                track: Some(SubtitleTrack::Stream(3)),
            },
        ),
        (
            br#"{"cmd":"select_subtitles","track":null}"#,
            PlayerCommand::SelectSubtitles { track: None },
        ),
        (
            br#"{"cmd":"play_queued","index":3}"#,
            PlayerCommand::PlayQueued { index: 3 },
        ),
        (
            br#"{"cmd":"set_loop_video","on":true}"#,
            PlayerCommand::SetLoopVideo { on: true },
        ),
        (
            br#"{"cmd":"set_autoplay","on":false}"#,
            PlayerCommand::SetAutoplay { on: false },
        ),
        (
            br#"{"cmd":"set_loop_playlist","on":true}"#,
            PlayerCommand::SetLoopPlaylist { on: true },
        ),
    ] {
        let r = http(addr, "POST", "/api/command", &h, body);
        assert_eq!(r.status, 200, "{}", String::from_utf8_lossy(body));
        assert_eq!(next_event(&hub), RemoteEvent::Command(cmd));
    }

    // Rejected: bad JSON, unknown command, bad values, wrong method.
    for body in [
        &b"nope"[..],
        br#"{"cmd":"explode"}"#,
        br#"{"cmd":"seek","position":-1}"#,
        br#"{"cmd":"set_speed","speed":0}"#,
        br#"{"cmd":"set_volume","volume":2}"#,
        br#"{"cmd":"open","location":""}"#,
    ] {
        let r = http(addr, "POST", "/api/command", &h, body);
        assert_eq!(r.status, 400, "{}", String::from_utf8_lossy(body));
    }
    let r = http(addr, "GET", "/api/command", &h, b"");
    assert_eq!(r.status, 405);
    let big = vec![b' '; 70 * 1024];
    let r = http(addr, "POST", "/api/command", &h, &big);
    assert_eq!(r.status, 413);

    let r = http(
        addr,
        "POST",
        "/api/text",
        &h,
        r#"{"text":"beach 180\u0007"}"#.as_bytes(),
    );
    assert_eq!(r.status, 200);
    assert_eq!(next_event(&hub), RemoteEvent::Text("beach 180".into()));
    let r = http(addr, "POST", "/api/text", &h, br#"{"words":"x"}"#);
    assert_eq!(r.status, 400);

    assert!(
        hub.events()
            .recv_timeout(Duration::from_millis(200))
            .is_err()
    );
}

#[test]
fn details_and_the_picture_reach_the_page() {
    let (hub, addr, token) = started();
    let auth = bearer(&token);
    let h = [("Authorization", auth.as_str())];
    assert_eq!(http(addr, "GET", "/api/picture", &h, b"").status, 404);
    assert!(http(addr, "GET", "/api/status", &h, b"").json()["picture"].is_null());

    hub.publish(&playing("/media/x.mp4", 7.0));
    hub.publish_details(&RemoteDetails {
        format: "180° 3D".into(),
        picture: Some(picture_file()),
        markers: vec![RemoteMarker {
            time: 30.0,
            name: "Into <the> cove".into(),
        }],
        volume: 0.7,
        audio: vec![RemoteAudio {
            stream: 1,
            label: "English".into(),
            detail: "aac · 2 ch".into(),
            selected: true,
        }],
        subtitles: vec![RemoteSubtitles {
            track: SubtitleTrack::File(0),
            label: "English".into(),
            detail: String::new(),
            selected: false,
        }],
        has_previous: false,
        has_next: true,
        up_next: vec!["Into <the> cove".into(), "Lighthouse".into()],
        up_next_at: 1,
        loop_video: false,
        autoplay: true,
        loop_playlist: true,
    });
    let s = http(addr, "GET", "/api/status", &h, b"").json();
    let d = &s["details"];
    assert_eq!(d["format"], "180° 3D");
    // Server text comes back verbatim as JSON data; the page renders it
    // with textContent.
    assert_eq!(d["markers"][0]["name"], "Into <the> cove");
    assert_eq!(d["volume"], 0.7);
    assert_eq!(d["audio"][0]["stream"], 1);
    assert_eq!(d["subtitles"][0]["track"]["file"], 0);
    assert_eq!(d["has_next"], true);
    assert_eq!(d["up_next"][1], "Lighthouse");
    assert_eq!(d["up_next_at"], 1);
    assert_eq!(
        (&d["loop_video"], &d["autoplay"], &d["loop_playlist"]),
        (&false.into(), &true.into(), &true.into())
    );
    assert!(d.get("picture").is_none(), "the headset's path stays on it");
    assert!(s["picture"].is_u64());

    let r = http(addr, "GET", "/api/picture", &h, b"");
    assert_eq!(r.status, 200);
    assert_eq!(r.header("Content-Type"), Some("image/jpeg"));
    assert_eq!(r.body, JPEG);
    assert_eq!(http(addr, "GET", "/api/picture", &[], b"").status, 401);
    assert_eq!(http(addr, "GET", "/api/nothing", &h, b"").status, 404);
}

#[test]
fn details_changes_reach_live_streams() {
    let (hub, addr, token) = started();
    let (mut s, mut buf) = open_stream(addr, &token);
    next_data(&mut s, &mut buf, |v| v["location"] == "");
    hub.publish(&fp_core::PlaybackStatus {
        playing: false,
        ..playing("/media/paused.mp4", 3.0)
    });
    next_data(&mut s, &mut buf, |v| v["location"] == "/media/paused.mp4");
    // Paused, so no periodic refresh: only the change sends it.
    hub.publish_details(&RemoteDetails {
        volume: 0.25,
        ..Default::default()
    });
    next_data(&mut s, &mut buf, |v| v["details"]["volume"] == 0.25);
}

/// Reads server-sent events until one `data:` line satisfies `f`.
fn next_data(s: &mut TcpStream, buf: &mut Vec<u8>, f: impl Fn(&serde_json::Value) -> bool) {
    let end = Instant::now() + Duration::from_secs(5);
    let mut chunk = [0u8; 4096];
    while Instant::now() < end {
        while let Some(i) = buf.windows(2).position(|w| w == b"\n\n") {
            let event = String::from_utf8_lossy(&buf[..i]).into_owned();
            buf.drain(..i + 2);
            for line in event.lines() {
                if let Some(json) = line.strip_prefix("data: ")
                    && f(&serde_json::from_str(json).unwrap())
                {
                    return;
                }
            }
        }
        let n = s.read(&mut chunk).unwrap();
        assert!(n > 0, "stream closed");
        buf.extend_from_slice(&chunk[..n]);
    }
    panic!("no matching event");
}

#[test]
fn live_status_stream() {
    let (hub, addr, token) = started();
    let mut s = TcpStream::connect(addr).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    write!(
        s,
        "GET /api/events HTTP/1.1\r\nHost: x\r\nCookie: fp_remote_token={token}\r\nAccept: text/event-stream\r\n\r\n"
    )
    .unwrap();
    let mut buf = Vec::new();
    let mut chunk = [0u8; 4096];
    while !buf.windows(4).any(|w| w == b"\r\n\r\n") {
        let n = s.read(&mut chunk).unwrap();
        assert!(n > 0);
        buf.extend_from_slice(&chunk[..n]);
    }
    let split = buf.windows(4).position(|w| w == b"\r\n\r\n").unwrap();
    let head = parse_response(&buf[..split + 4]);
    assert_eq!(head.status, 200);
    assert!(
        head.header("Content-Type")
            .unwrap()
            .starts_with("text/event-stream")
    );
    buf.drain(..split + 4);

    next_data(&mut s, &mut buf, |v| v["location"] == "");
    assert!(wait_for(Duration::from_secs(2), || hub.web_client_count() == 1));
    hub.publish(&playing("/media/live.mp4", 3.0));
    next_data(&mut s, &mut buf, |v| {
        v["location"] == "/media/live.mp4" && v["now_ms"].as_u64().is_some()
    });

    drop(s);
    // The stream notices the closed socket on its next write (the periodic
    // refresh while playing).
    assert!(wait_for(Duration::from_secs(8), || hub.web_client_count() == 0));
}

/// Opens `/api/events` and reads past the response head.
fn open_stream(addr: SocketAddr, token: &str) -> (TcpStream, Vec<u8>) {
    let mut s = TcpStream::connect(addr).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    write!(
        s,
        "GET /api/events HTTP/1.1\r\nHost: x\r\nCookie: fp_remote_token={token}\r\n\r\n"
    )
    .unwrap();
    let mut buf = Vec::new();
    let mut chunk = [0u8; 4096];
    while !buf.windows(4).any(|w| w == b"\r\n\r\n") {
        let n = s.read(&mut chunk).unwrap();
        assert!(n > 0);
        buf.extend_from_slice(&chunk[..n]);
    }
    let split = buf.windows(4).position(|w| w == b"\r\n\r\n").unwrap();
    buf.drain(..split + 4);
    (s, buf)
}

#[test]
fn closing_the_video_reaches_live_streams() {
    let (hub, addr, token) = started();
    let (mut s, mut buf) = open_stream(addr, &token);
    hub.publish(&playing("/media/live.mp4", 3.0));
    next_data(&mut s, &mut buf, |v| v["location"] == "/media/live.mp4");
    hub.publish(&fp_core::PlaybackStatus::default());
    next_data(&mut s, &mut buf, |v| {
        v["location"] == "" && v["playing"] == false
    });
}

#[test]
fn unpairing_ends_live_streams() {
    let (hub, addr, token) = started();
    let (mut s, mut buf) = open_stream(addr, &token);
    next_data(&mut s, &mut buf, |v| v["location"] == "");
    hub.regenerate_token().unwrap();
    hub.publish(&playing("/media/private.mp4", 3.0));
    // The stream ends without telling the old phone what plays now.
    let mut rest = Vec::new();
    let _ = s.read_to_end(&mut rest);
    buf.extend_from_slice(&rest);
    assert!(
        !String::from_utf8_lossy(&buf).contains("private"),
        "{}",
        String::from_utf8_lossy(&buf)
    );
}

#[test]
fn event_streams_are_capped() {
    let hub = RemoteHub::new(fp_remote::RemoteConfig {
        bind_address: Ipv4Addr::LOCALHOST.into(),
        web_port: 0,
        max_event_streams: 1,
        ..Default::default()
    })
    .unwrap();
    let addr = hub.start_web().unwrap();
    let token = hub.token();
    let mut first = TcpStream::connect(addr).unwrap();
    write!(
        first,
        "GET /api/events HTTP/1.1\r\nHost: x\r\nCookie: fp_remote_token={token}\r\n\r\n"
    )
    .unwrap();
    assert!(wait_for(Duration::from_secs(2), || hub.web_client_count() == 1));
    let r = http(
        addr,
        "GET",
        "/api/events",
        &[("Authorization", &bearer(&token))],
        b"",
    );
    assert_eq!(r.status, 503);
    hub.stop();
    assert!(hub.web_addr().is_none());
}
