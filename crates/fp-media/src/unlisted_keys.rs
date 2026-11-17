//! Keyframes a video's index leaves out, found without reading what lies
//! between the listed ones: at the start of the samples an MP4's index
//! already places, or of the clusters a Matroska file is made of. FFmpeg
//! still seeks and demuxes once one is added to its index.

use crate::nal::NalReader;
use fp_core::ByteSource;
use std::cmp::Reverse;

/// Samples looked at in one seek. Intra pictures are the largest, so the
/// few largest are the likely keyframes.
const SAMPLE_LOOKS: usize = 3;
/// Bytes read from the start of a sample: room for the parameter sets and
/// SEI messages ahead of its first picture.
const SAMPLE_HEAD: usize = 4096;
/// Clusters looked at in one seek.
const CLUSTER_LOOKS: usize = 4;
/// Bytes read from the start of a cluster to find its first video block.
const CLUSTER_HEAD: usize = 8192;

const EBML: u32 = 0x1a45dfa3;
const SEGMENT: u32 = 0x18538067;
const TRACKS: u32 = 0x1654ae6b;
const TRACK_ENTRY: u32 = 0xae;
const CLUSTER: u32 = 0x1f43b675;
const SIMPLE_BLOCK: u32 = 0xa3;
const BLOCK_GROUP: u32 = 0xa0;
const TIMESTAMP: u32 = 0xe7;

/// Fills `out` from `at`, unless the source ends first or the seek is
/// called off.
fn read_exact(
    src: &dyn ByteSource,
    mut at: u64,
    mut out: &mut [u8],
    cancelled: &impl Fn() -> bool,
) -> Option<()> {
    while !out.is_empty() {
        if cancelled() {
            return None;
        }
        let n = src.read_at(at, out).ok().filter(|&n| n > 0)?;
        out = out.get_mut(n..)?;
        at += n as u64;
    }
    Some(())
}

/// Of the `samples` (index entry, position, size) between a listed keyframe
/// and a seek's target, the latest a decoder can start at.
pub(crate) fn sample_key(
    src: &dyn ByteSource,
    reader: &NalReader,
    mut samples: Vec<(usize, u64, usize)>,
    cancelled: &impl Fn() -> bool,
) -> Option<usize> {
    samples.sort_by_key(|&(_, _, size)| Reverse(size));
    samples.truncate(SAMPLE_LOOKS);
    samples.sort_by_key(|&(index, ..)| Reverse(index));
    let mut head = [0; SAMPLE_HEAD];
    samples.into_iter().find_map(|(index, pos, size)| {
        let head = &mut head[..size.min(SAMPLE_HEAD)];
        read_exact(src, pos, head, cancelled)?;
        reader.random_access(head).then_some(index)
    })
}

/// An EBML element: its ID, where its data starts, and where it ends
/// (`None`: unknown size, which only a Segment or Cluster may have).
struct Element {
    id: u32,
    data: u64,
    end: Option<u64>,
}

/// An EBML variable-length integer's value and width in bytes.
fn vint(bytes: &[u8], max_width: usize) -> Option<(u64, usize)> {
    let first = *bytes.first()?;
    let width = first.leading_zeros() as usize + 1;
    if width > max_width {
        return None;
    }
    let tail = bytes.get(1..width)?;
    let value = u64::from(first ^ (0x80 >> (width - 1)));
    Some((
        tail.iter().fold(value, |n, &b| (n << 8) | u64::from(b)),
        width,
    ))
}

impl Element {
    /// The element whose header starts `bytes`, found at `at` in the file
    /// and inside one ending at `limit`.
    fn parse(bytes: &[u8], at: u64, limit: u64) -> Option<Self> {
        // An ID keeps its length marker.
        let (_, id_width) = vint(bytes, 4)?;
        let id = bytes[..id_width]
            .iter()
            .fold(0u32, |n, &b| (n << 8) | u32::from(b));
        let (size, width) = vint(&bytes[id_width..], 8)?;
        let data = at.checked_add((id_width + width) as u64)?;
        if data > limit {
            return None;
        }
        let end = if size == (1u64 << (7 * width)) - 1 {
            None
        } else {
            let end = data.checked_add(size)?;
            if end > limit {
                return None;
            }
            Some(end)
        };
        Some(Self { id, data, end })
    }
}

/// The header of the element at `at`, inside one ending at `limit`.
fn header(
    src: &dyn ByteSource,
    at: u64,
    limit: u64,
    cancelled: &impl Fn() -> bool,
) -> Option<Element> {
    let mut bytes = [0; 12];
    let len = limit.checked_sub(at)?.min(bytes.len() as u64) as usize;
    read_exact(src, at, &mut bytes[..len], cancelled)?;
    Element::parse(&bytes[..len], at, limit)
}

/// Takes the first element off `rest`, as its ID and data.
fn take_element<'a>(rest: &mut &'a [u8]) -> Option<(u32, &'a [u8])> {
    let e = Element::parse(rest, 0, rest.len() as u64)?;
    let end = e.end? as usize;
    let data = rest.get(e.data as usize..end)?;
    *rest = &rest[end..];
    Some((e.id, data))
}

fn uint(bytes: &[u8]) -> Option<u64> {
    (bytes.len() <= 8).then(|| bytes.iter().fold(0, |n, &b| (n << 8) | u64::from(b)))
}

/// The number of a Matroska file's one video track, when it is H.264 or
/// HEVC and its blocks' timestamps are its stream's: no timestamp scale or
/// codec delay of its own. FFmpeg doesn't give track numbers.
pub(crate) fn matroska_track(src: &dyn ByteSource, cancelled: &impl Fn() -> bool) -> Option<u64> {
    let size = src.size()?;
    let ebml = header(src, 0, size, cancelled)?;
    if ebml.id != EBML {
        return None;
    }
    let segment = header(src, ebml.end?, size, cancelled)?;
    if segment.id != SEGMENT {
        return None;
    }
    let limit = segment.end.unwrap_or(size);
    let mut at = segment.data;
    // The tracks come among the first few elements, before any cluster.
    for _ in 0..8 {
        let e = header(src, at, limit, cancelled)?;
        if e.id == CLUSTER {
            return None;
        }
        let end = e.end?;
        if e.id == TRACKS {
            let len = usize::try_from(end - e.data)
                .ok()
                .filter(|&n| n <= 64 * 1024)?;
            let mut data = vec![0; len];
            read_exact(src, e.data, &mut data, cancelled)?;
            return video_track(&data);
        }
        at = end;
    }
    None
}

fn video_track(mut tracks: &[u8]) -> Option<u64> {
    let mut video = None;
    while !tracks.is_empty() {
        let (id, mut entry) = take_element(&mut tracks)?;
        if id != TRACK_ENTRY {
            continue;
        }
        let (mut kind, mut number, mut own_time) = (0, 0, false);
        let mut codec = &[][..];
        while !entry.is_empty() {
            let (id, data) = take_element(&mut entry)?;
            match id {
                0x83 => kind = uint(data)?,             // TrackType
                0xd7 => number = uint(data)?,           // TrackNumber
                0x86 => codec = data,                   // CodecID
                0x56aa => own_time |= uint(data)? != 0, // CodecDelay
                0x23314f => {
                    // TrackTimestampScale, as a 4- or 8-byte float.
                    own_time |= !matches!(
                        data,
                        [] | [0x3f, 0x80, 0, 0] | [0x3f, 0xf0, 0, 0, 0, 0, 0, 0]
                    );
                }
                _ => {}
            }
        }
        if kind == 1 {
            if video.is_some()
                || number == 0
                || own_time
                || !matches!(codec, b"V_MPEG4/ISO/AVC" | b"V_MPEGH/ISO/HEVC")
            {
                return None;
            }
            video = Some(number);
        }
    }
    video
}

/// The timestamp of the first block of `track` in a cluster whose start is
/// `bytes` (read from `base`), and whether that block is a keyframe.
fn first_video(bytes: &[u8], cluster: &Element, track: u64, base: u64) -> Option<(i64, bool)> {
    let limit = cluster.end?;
    let mut at = cluster.data;
    let mut time = None;
    while at < limit {
        let e = Element::parse(bytes.get((at - base) as usize..)?, at, limit)?;
        let end = e.end?;
        let payload = bytes.get((e.data - base) as usize..)?;
        let payload = &payload[..((end - e.data) as usize).min(payload.len())];
        match e.id {
            TIMESTAMP if (end - e.data) as usize == payload.len() => {
                time = Some(i64::try_from(uint(payload)?).ok()?);
            }
            SIMPLE_BLOCK => {
                let (number, width) = vint(payload, 8)?;
                if number == track {
                    let &[hi, lo, flags] = payload.get(width..width + 3)? else {
                        return None;
                    };
                    let pts = time?.checked_add(i64::from(i16::from_be_bytes([hi, lo])))?;
                    // A keyframe, shown, not laced, not discardable, and
                    // with a picture after its header.
                    let key = flags & 0x8f == 0x80 && end - e.data > (width + 3) as u64;
                    return Some((pts, key));
                }
            }
            // Its keyframes aren't flagged.
            BLOCK_GROUP => return None,
            _ => {}
        }
        at = end;
    }
    None
}

/// Where the cluster after the one at `anchor` starts, when that is before
/// `end`.
pub(crate) fn next_cluster(
    src: &dyn ByteSource,
    anchor: u64,
    end: u64,
    cancelled: &impl Fn() -> bool,
) -> Option<u64> {
    let first = header(src, anchor, end, cancelled)?;
    if first.id != CLUSTER {
        return None;
    }
    first.end.filter(|&next| next < end)
}

/// Of the clusters from `at` to `end`, where the latest that opens with a
/// keyframe of `track` no later than `target` starts, and that keyframe's
/// timestamp. A look costs one short read: the clusters' sizes lead from
/// one to the next.
pub(crate) fn cluster_key(
    src: &dyn ByteSource,
    track: u64,
    mut at: u64,
    end: u64,
    target: i64,
    cancelled: &impl Fn() -> bool,
) -> Option<(u64, i64)> {
    if end > src.size()? {
        return None;
    }
    let mut best = None;
    let mut head = [0; CLUSTER_HEAD];
    for _ in 0..CLUSTER_LOOKS {
        if at >= end {
            break;
        }
        let head = &mut head[..(end - at).min(CLUSTER_HEAD as u64) as usize];
        if read_exact(src, at, head, cancelled).is_none() {
            break;
        }
        let Some(cluster) = Element::parse(head, at, end).filter(|c| c.id == CLUSTER) else {
            break;
        };
        let Some(next) = cluster.end else {
            break;
        };
        if let Some((pts, key)) = first_video(head, &cluster, track, at) {
            if pts > target {
                break;
            }
            if key {
                best = Some((at, pts));
            }
        }
        at = next;
    }
    best
}
