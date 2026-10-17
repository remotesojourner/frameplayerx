//! Which subtitles a video starts with, and how its tracks are named.

use crate::settings::SubtitleMode;
use fp_core::language::Tag;
use fp_core::source::SubtitleFile;
use fp_media::{MediaInfo, StreamInfo};
use fp_sources::urlutil::last_segment;

/// The subtitles a video starts with.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Pick {
    Off,
    /// An embedded stream, by index.
    Stream(usize),
    /// One of the video's subtitle files, by position.
    File(usize),
}

impl Pick {
    pub fn stream(self) -> Option<usize> {
        match self {
            Pick::Stream(i) => Some(i),
            _ => None,
        }
    }
}

struct Track {
    pick: Pick,
    tag: Tag,
    default: bool,
    /// A file with no language, named like the video (`Movie.srt`).
    beside: bool,
}

impl Track {
    /// Full subtitles before forced ones, then without SDH, files (added
    /// on purpose) before embedded streams, then the stream marked default.
    fn rank(&self) -> (bool, bool, bool, bool) {
        (
            self.tag.forced,
            self.tag.sdh,
            matches!(self.pick, Pick::Stream(_)),
            !self.default,
        )
    }
}

fn stem(name: &str) -> &str {
    name.rsplit_once('.').map_or(name, |(s, _)| s)
}

/// A stream's tag, with what its dispositions and title add.
fn stream_tag(s: &StreamInfo) -> Tag {
    let mut tag = Tag::parse(s.language.as_deref().unwrap_or(""));
    let title = s.title.as_deref().unwrap_or("").to_ascii_lowercase();
    tag.forced |= s.forced || title.contains("forced");
    tag.sdh |= s.hearing_impaired || title.contains("sdh");
    tag
}

/// The subtitles to start with, for `mode` and the user's `language`:
///
/// - Always: the best subtitles in `language`, when there are any.
/// - Automatic (and Always without them): subtitles in `language` when the
///   audio is in another one; else forced subtitles in the audio's language
///   or `language`; else a subtitle file named like the video.
pub fn choose(
    mode: SubtitleMode,
    language: &str,
    info: &MediaInfo,
    files: &[SubtitleFile],
    video_name: &str,
) -> Pick {
    if mode == SubtitleMode::Off {
        return Pick::Off;
    }
    let mut tracks: Vec<Track> = files
        .iter()
        .enumerate()
        .map(|(i, f)| Track {
            pick: Pick::File(i),
            tag: Tag::parse(f.language.as_deref().unwrap_or("")),
            default: false,
            beside: f.language.is_none() && stem(&f.name).eq_ignore_ascii_case(stem(video_name)),
        })
        .collect();
    tracks.extend(info.subtitle_streams().map(|s| Track {
        pick: Pick::Stream(s.index),
        tag: stream_tag(s),
        default: s.default,
        beside: false,
    }));
    // A stable sort keeps the files' and streams' own order among equals.
    tracks.sort_by_key(Track::rank);
    let mine = Some(language);
    let audio = info
        .audio
        .and_then(|i| info.streams.iter().find(|s| s.index == i))
        .and_then(|s| Tag::parse(s.language.as_deref().unwrap_or("")).code);
    let in_mine = tracks.iter().find(|t| t.tag.code == mine);
    if let Some(t) = in_mine
        && (mode == SubtitleMode::Always || audio.is_some_and(|a| Some(a) != mine))
    {
        return t.pick;
    }
    let forced = tracks.iter().find(|t| {
        t.tag.forced && (t.tag.code.is_none() || t.tag.code == audio || t.tag.code == mine)
    });
    forced
        .or_else(|| tracks.iter().find(|t| t.beside))
        .map_or(Pick::Off, |t| t.pick)
}

/// A track's name: its language, else its title, else its number.
pub fn track_name(s: &StreamInfo) -> String {
    Tag::parse(s.language.as_deref().unwrap_or(""))
        .name()
        .map(str::to_string)
        .or_else(|| s.title.clone())
        .unwrap_or_else(|| format!("Track {}", s.index))
}

/// What tells an embedded subtitle stream apart besides its name.
pub fn stream_detail(s: &StreamInfo) -> String {
    let tag = stream_tag(s);
    let mut parts = Vec::new();
    let title = s.title.as_deref().filter(|t| !t.is_empty());
    if let Some(t) =
        title.filter(|t| tag.name().is_some() && !t.eq_ignore_ascii_case(&track_name(s)))
    {
        parts.push(t.to_string());
    }
    let said = title.unwrap_or("").to_ascii_lowercase();
    if tag.forced && !said.contains("forced") {
        parts.push("Forced".into());
    }
    if tag.sdh && !said.contains("sdh") {
        parts.push("SDH".into());
    }
    parts.push(codec_name(&s.codec));
    parts.join(" · ")
}

/// A subtitle file's name and detail. The name is its language, else its
/// own name, else its place in the list (`n` counts from 1); the detail is
/// what tells it from the others: forced, SDH, its own name and its format.
pub fn file_label(f: &SubtitleFile, n: usize) -> (String, String) {
    let tag = Tag::parse(f.language.as_deref().unwrap_or(""));
    let own = f.name.trim();
    let said = own.to_ascii_lowercase();
    let mut parts = Vec::new();
    if tag.forced && !said.contains("forced") {
        parts.push("Forced".to_string());
    }
    if tag.sdh && !said.contains("sdh") {
        parts.push("SDH".to_string());
    }
    let title = match tag.name() {
        Some(language) => {
            if !own.is_empty() && !own.eq_ignore_ascii_case(language) {
                parts.push(own.to_string());
            }
            language.to_string()
        }
        None if !own.is_empty() => own.to_string(),
        None => format!("Subtitles {n}"),
    };
    // A file name says its format; a title or no name at all doesn't.
    if !fp_core::source::is_subtitle_name(own)
        && let Some(format) = [own.to_string(), last_segment(&f.location)]
            .iter()
            .find_map(|s| text_format(s.rsplit_once('.')?.1))
    {
        parts.push(format.to_string());
    }
    (title, parts.join(" · "))
}

/// The name of a text subtitle format, by codec or file extension.
fn text_format(codec: &str) -> Option<&'static str> {
    Some(match codec.to_ascii_lowercase().as_str() {
        "subrip" | "srt" => "SRT",
        "ass" | "ssa" => "ASS",
        "webvtt" | "vtt" => "WebVTT",
        "mov_text" => "Text",
        _ => return None,
    })
}

fn codec_name(codec: &str) -> String {
    if let Some(name) = text_format(codec) {
        return name.into();
    }
    match codec {
        "hdmv_pgs_subtitle" => "PGS",
        "dvd_subtitle" => "VobSub",
        "dvb_subtitle" => "DVB",
        other => return other.to_uppercase(),
    }
    .into()
}

/// A subtitle file or stream as the Audio & subs tab lists it.
pub struct Row {
    pub pick: Pick,
    pub title: String,
    pub detail: String,
    /// In a known language other than the user's: these fold away when a
    /// video has many subtitles.
    pub foreign: bool,
}

/// The video's subtitle files and embedded streams, labelled: those in
/// `language` first, then those in no known language, then the rest by
/// language name. Files come before streams in the same language.
pub fn rows(language: &str, info: &MediaInfo, files: &[SubtitleFile]) -> Vec<Row> {
    let mut rows: Vec<(Option<&str>, Row)> = files
        .iter()
        .enumerate()
        .map(|(i, f)| {
            let (title, detail) = file_label(f, i + 1);
            let code = Tag::parse(f.language.as_deref().unwrap_or("")).code;
            let pick = Pick::File(i);
            (
                code,
                Row {
                    pick,
                    title,
                    detail,
                    foreign: false,
                },
            )
        })
        .collect();
    rows.extend(info.subtitle_streams().map(|s| {
        let row = Row {
            pick: Pick::Stream(s.index),
            title: track_name(s),
            detail: stream_detail(s),
            foreign: false,
        };
        (stream_tag(s).code, row)
    }));
    let group = |code: Option<&str>| match code {
        Some(c) if c == language => 0,
        None => 1,
        Some(_) => 2,
    };
    rows.sort_by_key(|(code, _)| (group(*code), code.and_then(fp_core::language::name)));
    rows.into_iter()
        .map(|(code, mut row)| {
            row.foreign = group(code) == 2;
            row
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use fp_media::StreamKind;

    fn stream(index: usize, kind: StreamKind, language: Option<&str>) -> StreamInfo {
        StreamInfo {
            index,
            kind,
            codec: "subrip".into(),
            language: language.map(str::to_string),
            title: None,
            default: false,
            forced: false,
            hearing_impaired: false,
            width: 0,
            height: 0,
            fps: 0.0,
            bit_depth: 8,
            pixel_format: String::new(),
            interlaced: false,
            transfer: Default::default(),
            rotation: 0.0,
            sample_rate: 0,
            channels: 0,
            ambisonic: false,
        }
    }

    /// A video with `audio` and embedded subtitles from index 2.
    fn video(audio: Option<&str>, subs: Vec<StreamInfo>) -> MediaInfo {
        let mut streams = vec![
            stream(0, StreamKind::Video, None),
            stream(1, StreamKind::Audio, audio),
        ];
        streams.extend(subs);
        MediaInfo {
            name: "Movie.mkv".into(),
            container: "matroska".into(),
            duration: 60.0,
            streams,
            chapters: Vec::new(),
            hints: Default::default(),
            video: Some(0),
            audio: Some(1),
            title: None,
        }
    }

    fn sub(index: usize, language: &str) -> StreamInfo {
        stream(index, StreamKind::Subtitle, Some(language))
    }

    fn file(name: &str, language: Option<&str>) -> SubtitleFile {
        SubtitleFile {
            name: name.into(),
            location: format!("/v/{name}"),
            language: language.map(str::to_string),
        }
    }

    use SubtitleMode::{Always, Automatic, Off};

    #[test]
    fn a_first_track_in_another_language_is_not_shown() {
        let info = video(Some("eng"), vec![sub(2, "ara"), sub(3, "eng")]);
        assert_eq!(choose(Automatic, "en", &info, &[], "Movie.mkv"), Pick::Off);
        assert_eq!(
            choose(Always, "en", &info, &[], "Movie.mkv"),
            Pick::Stream(3)
        );
        assert_eq!(choose(Off, "en", &info, &[], "Movie.mkv"), Pick::Off);
    }

    #[test]
    fn your_language_when_the_audio_is_foreign() {
        let info = video(Some("jpn"), vec![sub(2, "ara"), sub(3, "eng")]);
        assert_eq!(
            choose(Automatic, "en", &info, &[], "Movie.mkv"),
            Pick::Stream(3)
        );
        // Unknown audio: nothing to go on.
        let info = video(None, vec![sub(2, "eng")]);
        assert_eq!(choose(Automatic, "en", &info, &[], "Movie.mkv"), Pick::Off);
    }

    #[test]
    fn forced_subtitles_for_the_audio() {
        let mut forced = sub(3, "eng");
        forced.forced = true;
        let mut foreign_forced = sub(4, "ger");
        foreign_forced.forced = true;
        let info = video(Some("eng"), vec![sub(2, "eng"), foreign_forced, forced]);
        assert_eq!(
            choose(Automatic, "en", &info, &[], "Movie.mkv"),
            Pick::Stream(3)
        );
        // Always wants full subtitles over forced ones.
        assert_eq!(
            choose(Always, "en", &info, &[], "Movie.mkv"),
            Pick::Stream(2)
        );
        let mut titled = sub(2, "eng");
        titled.title = Some("English (Forced)".into());
        let info = video(Some("eng"), vec![titled]);
        assert_eq!(
            choose(Automatic, "en", &info, &[], "Movie.mkv"),
            Pick::Stream(2)
        );
    }

    #[test]
    fn files() {
        let info = video(Some("eng"), vec![sub(2, "eng")]);
        let files = [file("Movie.ar.srt", Some("ar")), file("Movie.srt", None)];
        // The untagged file beside the video, not the Arabic one.
        assert_eq!(
            choose(Automatic, "en", &info, &files, "Movie.mkv"),
            Pick::File(1)
        );
        let files = [
            file("Movie.ar.srt", Some("ar")),
            file("Movie.en.srt", Some("en")),
        ];
        assert_eq!(
            choose(Always, "en", &info, &files, "Movie.mkv"),
            Pick::File(1)
        );
        assert_eq!(
            choose(Always, "ar", &info, &files, "Movie.mkv"),
            Pick::File(0)
        );
        // A file named after something else isn't the video's own.
        let files = [file("Stream.subrip", None)];
        assert_eq!(
            choose(Automatic, "en", &info, &files, "Movie.mkv"),
            Pick::Off
        );
    }

    #[test]
    fn sdh_and_default_break_ties() {
        let mut sdh = sub(2, "eng");
        sdh.hearing_impaired = true;
        let mut marked = sub(4, "eng");
        marked.default = true;
        let info = video(Some("jpn"), vec![sdh, sub(3, "eng"), marked]);
        assert_eq!(
            choose(Automatic, "en", &info, &[], "Movie.mkv"),
            Pick::Stream(4)
        );
    }

    #[test]
    fn labels() {
        let mut s = sub(3, "ara");
        s.title = Some("Arabic".into());
        assert_eq!(
            (track_name(&s), stream_detail(&s)),
            ("Arabic".into(), "SRT".into())
        );
        s.title = Some("Commentary".into());
        s.hearing_impaired = true;
        assert_eq!(stream_detail(&s), "Commentary · SDH · SRT");
        let s = stream(5, StreamKind::Subtitle, None);
        assert_eq!(track_name(&s), "Track 5");
        assert_eq!(
            file_label(&file("Movie.en.srt", Some("en")), 1),
            ("English".into(), "Movie.en.srt".into())
        );
        assert_eq!(
            file_label(&file("Movie.srt", None), 1),
            ("Movie.srt".into(), String::new())
        );
        // A server's files, named by its address alone or by a title.
        let served = |name: &str, codec: &str, language: &str| SubtitleFile {
            name: name.into(),
            location: format!("http://jf/Videos/1/m/Subtitles/4/Stream.{codec}"),
            language: Some(language.into()),
        };
        assert_eq!(
            file_label(&served("", "subrip", "ger.forced"), 1),
            ("German".into(), "Forced · SRT".into())
        );
        assert_eq!(
            file_label(&served("", "subrip", ".sdh"), 3),
            ("Subtitles 3".into(), "SDH · SRT".into())
        );
        assert_eq!(
            file_label(&served("Commentary", "ass", "eng"), 1),
            ("English".into(), "Commentary · ASS".into())
        );
    }

    #[test]
    fn rows_put_your_language_first() {
        let info = video(Some("eng"), vec![sub(2, "ara"), sub(3, "eng")]);
        let files = [
            file("Movie.fr.srt", Some("fr")),
            file("Movie.srt", None),
            file("Movie.en.srt", Some("en")),
        ];
        let order: Vec<_> = rows("en", &info, &files)
            .iter()
            .map(|r| (r.pick, r.title.clone(), r.foreign))
            .collect();
        assert_eq!(
            order,
            [
                (Pick::File(2), "English".into(), false),
                (Pick::Stream(3), "English".into(), false),
                (Pick::File(1), "Movie.srt".into(), false),
                (Pick::Stream(2), "Arabic".into(), true),
                (Pick::File(0), "French".into(), true),
            ]
        );
    }
}
