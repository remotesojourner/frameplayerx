//! Just enough of the H.264 and HEVC bitstreams to find pictures no other
//! picture is decoded from, which a precise seek can leave out on its way
//! to the target, and how many pictures a stream's decoder keeps at once
//! ([`Dpb`]), which sizes the hardware decoder's buffers.

use crate::mvc::Bad;
use crate::mvc::bits::{Reader, unescape};
use fp_core::format::StereoLayout;
use fp_ffmpeg_sys as ff;

/// H.264's SEI NAL unit type, HEVC's prefix SEI, and the SEI messages that
/// say where a picture can be decoded from and how it holds two eyes.
const H264_SEI: u8 = 6;
const HEVC_SEI: u8 = 39;
const RECOVERY_POINT: usize = 6;
const FRAME_PACKING: usize = 45;

/// Reads the NAL units in one stream's packets.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct NalReader {
    hevc: bool,
    /// Bytes in each NAL unit's length prefix (MP4, Matroska), or 0 for
    /// Annex B start codes (MPEG-TS).
    length_size: usize,
    /// HEVC's highest temporal sub-layer, once an SPS gave it. A
    /// non-reference picture below it can still be referenced by the
    /// layers above.
    top_layer: Option<u8>,
}

impl NalReader {
    /// A reader for an H.264 or HEVC stream with this extradata.
    pub(crate) fn new(codec: ff::AVCodecID, extradata: &[u8]) -> Option<NalReader> {
        let hevc = match codec {
            ff::AV_CODEC_ID_H264 => false,
            ff::AV_CODEC_ID_HEVC => true,
            _ => return None,
        };
        let mut r = NalReader {
            hevc,
            length_size: 0,
            top_layer: None,
        };
        r.length_size = match Record::of(hevc, extradata) {
            Record::Hvcc => (extradata[21] & 3) as usize + 1,
            Record::Avcc => (extradata[4] & 3) as usize + 1,
            Record::AnnexB => 0,
        };
        for nal in parameter_sets(hevc, extradata) {
            r.note_sps(nal);
        }
        Some(r)
    }

    fn note_sps(&mut self, nal: &[u8]) {
        // sps_max_sub_layers_minus1 follows the 4-bit VPS id; layers above
        // the base code it differently.
        if let [h0, h1, b, ..] = *nal
            && self.hevc
            && (h0 >> 1) & 0x3f == 33
            && h0 & 1 == 0
            && h1 >> 3 == 0
        {
            let top = (b >> 1) & 7;
            self.top_layer = Some(self.top_layer.map_or(top, |t| t.max(top)));
        }
    }

    pub(crate) fn length_size(&self) -> usize {
        self.length_size
    }

    /// How a packet's frame packing SEI message says the eyes are packed, and
    /// whether the right eye comes first. None when it has none, or states
    /// a packing that can't be shown.
    pub(crate) fn frame_packing(&self, data: &[u8]) -> Option<(Packing, bool)> {
        units(data, self.length_size).find_map(|nal| {
            let h0 = *nal.first()?;
            let (kind, sei, header) = if self.hevc {
                ((h0 >> 1) & 0x3f, HEVC_SEI, 2)
            } else {
                (h0 & 0x1f, H264_SEI, 1)
            };
            if kind != sei {
                return None;
            }
            sei_frame_packing(&unescape(nal.get(header..)?))
        })
    }

    /// Whether decoding can start at a packet: at an HEVC IDR, CRA or BLA
    /// picture, or an H.264 IDR, or an H.264 I picture that a recovery point
    /// message says is whole at once. Only the first picture counts, so the
    /// start of a packet is enough.
    pub(crate) fn random_access(&self, data: &[u8]) -> bool {
        let mut recovers = false;
        for nal in units(data, self.length_size) {
            let Some(&h0) = nal.first() else {
                continue;
            };
            if self.hevc {
                let kind = (h0 >> 1) & 0x3f;
                if kind < 32 {
                    return (16..=21).contains(&kind);
                }
                continue;
            }
            match h0 & 0x1f {
                5 => return true,
                1 => return recovers && i_slice(nal),
                2..=4 => return false,
                H264_SEI => {
                    recovers |= sei_messages(&unescape(&nal[1..])).any(|(kind, payload)| {
                        kind == RECOVERY_POINT
                            && Reader::new(payload, 0).ue().is_ok_and(|frames| frames == 0)
                    });
                }
                _ => {}
            }
        }
        false
    }

    /// Whether a packet holds only pictures no other picture is decoded
    /// from, and no parameter sets. SPSs on the way are noted.
    pub(crate) fn disposable(&mut self, data: &[u8]) -> bool {
        let (mut pictures, mut parameter_sets) = (0, false);
        for nal in units(data, self.length_size) {
            let Some(&h0) = nal.first() else {
                continue;
            };
            let unreferenced = if self.hevc {
                let Some(&h1) = nal.get(1) else {
                    continue;
                };
                let kind = (h0 >> 1) & 0x3f;
                if (32..=34).contains(&kind) {
                    self.note_sps(nal);
                    parameter_sets = true;
                    continue;
                }
                if kind >= 32 {
                    continue;
                }
                // TRAIL_N, TSA_N, STSA_N, RADL_N, RASL_N and the reserved
                // non-reference types are the even ones up to 14.
                let temporal_id = (h1 & 7).saturating_sub(1);
                kind <= 14 && kind % 2 == 0 && self.top_layer.is_some_and(|t| temporal_id >= t)
            } else {
                match h0 & 0x1f {
                    7 | 8 | 13 | 15 => {
                        parameter_sets = true;
                        continue;
                    }
                    1..=5 => h0 & 0x60 == 0,
                    _ => continue,
                }
            };
            if !unreferenced {
                return false;
            }
            pictures += 1;
        }
        pictures > 0 && !parameter_sets
    }
}

/// How a frame packing message says the eyes are packed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Packing {
    /// Both eyes in each picture (or a 2D one).
    Layout(StereoLayout),
    /// The eyes take turns, picture by picture.
    FrameSequential,
}

/// The type and payload of each message in an SEI RBSP.
fn sei_messages(mut rbsp: &[u8]) -> impl Iterator<Item = (usize, &[u8])> {
    // Each is its type and size, in bytes of 255 and a last one that add up.
    fn number(rest: &mut &[u8]) -> Option<usize> {
        let mut n = 0;
        loop {
            let (&byte, tail) = rest.split_first()?;
            *rest = tail;
            n += byte as usize;
            if byte != 255 {
                return Some(n);
            }
        }
    }
    std::iter::from_fn(move || {
        let kind = number(&mut rbsp)?;
        let size = number(&mut rbsp)?;
        let payload = rbsp.get(..size)?;
        rbsp = &rbsp[size..];
        Some((kind, payload))
    })
}

/// Finds the frame packing message among an SEI RBSP's messages.
fn sei_frame_packing(rbsp: &[u8]) -> Option<(Packing, bool)> {
    let (_, payload) = sei_messages(rbsp).find(|&(kind, _)| kind == FRAME_PACKING)?;
    frame_packing_message(payload)
}

/// Whether an H.264 slice NAL unit is an I or SI slice.
fn i_slice(nal: &[u8]) -> bool {
    let header = unescape(&nal[1..nal.len().min(8)]);
    let mut r = Reader::new(&header, 0);
    r.ue().is_ok() && r.ue().is_ok_and(|kind| matches!(kind % 5, 2 | 4))
}

fn frame_packing_message(payload: &[u8]) -> Option<(Packing, bool)> {
    let mut r = Reader::new(payload, 0);
    r.ue().ok()?; // frame_packing_arrangement_id
    if r.flag().ok()? {
        return None; // cancelled
    }
    let arrangement = r.u(7).ok()?;
    r.flag().ok()?; // quincunx_sampling_flag
    let interpretation = r.u(6).ok()?;
    let packing = match arrangement {
        3 => Packing::Layout(StereoLayout::SideBySide),
        4 => Packing::Layout(StereoLayout::TopBottom),
        5 => Packing::FrameSequential,
        6 => Packing::Layout(StereoLayout::Mono),
        _ => return None,
    };
    let two_d = packing == Packing::Layout(StereoLayout::Mono);
    Some((packing, !two_d && interpretation == 2))
}

/// The NAL units in `data`, each from its header on. A unit cut short by
/// the end of the data is returned as far as it goes.
pub(crate) fn units(data: &[u8], length_size: usize) -> impl Iterator<Item = &[u8]> {
    let mut rest = data;
    std::iter::from_fn(move || {
        if length_size > 0 {
            let len = rest
                .get(..length_size)?
                .iter()
                .fold(0usize, |n, &b| n << 8 | b as usize);
            let tail = &rest[length_size..];
            let unit = tail.get(..len).unwrap_or(tail);
            rest = &tail[unit.len()..];
            Some(unit)
        } else {
            let tail = &rest[start_code(rest)? + 3..];
            let unit = &tail[..start_code(tail).unwrap_or(tail.len())];
            rest = &tail[unit.len()..];
            Some(unit)
        }
    })
}

pub(crate) fn start_code(data: &[u8]) -> Option<usize> {
    data.windows(3).position(|w| w == [0, 0, 1])
}

/// How a stream's extradata holds its parameter sets.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Record {
    /// An HEVC decoder configuration record (MP4, Matroska).
    Hvcc,
    /// An H.264 one.
    Avcc,
    /// Annex B units (MPEG-TS), or nothing.
    AnnexB,
}

impl Record {
    fn of(hevc: bool, extradata: &[u8]) -> Record {
        match *extradata {
            // FFmpeg's test: Annex B starts 00 00 01 or 00 00 00 01.
            [a, b, c, ..] if hevc && extradata.len() >= 23 && (a != 0 || b != 0 || c > 1) => {
                Record::Hvcc
            }
            [1, _, _, _, _, ..] if !hevc => Record::Avcc,
            _ => Record::AnnexB,
        }
    }
}

/// The parameter sets in a stream's extradata, each a whole NAL unit; of an
/// avcC record, the SPSs alone.
fn parameter_sets(hevc: bool, extradata: &[u8]) -> Vec<&[u8]> {
    // Each unit in a record is its 16-bit length, then the unit.
    fn take<'a>(rest: &mut &'a [u8], into: &mut Vec<&'a [u8]>) -> Option<()> {
        let [hi, lo, tail @ ..] = *rest else {
            return None;
        };
        let len = u16::from_be_bytes([*hi, *lo]) as usize;
        into.push(tail.get(..len)?);
        *rest = &tail[len..];
        Some(())
    }
    let mut out = Vec::new();
    match Record::of(hevc, extradata) {
        // Arrays of units after 22 bytes of header: each its type, a count,
        // then the units.
        Record::Hvcc => {
            let Some((&arrays, mut rest)) = extradata[22..].split_first() else {
                return out;
            };
            for _ in 0..arrays {
                let [_, hi, lo, tail @ ..] = rest else {
                    break;
                };
                rest = tail;
                for _ in 0..u16::from_be_bytes([*hi, *lo]) {
                    if take(&mut rest, &mut out).is_none() {
                        return out;
                    }
                }
            }
        }
        // The SPS count in the low 5 bits of the sixth byte, then the SPSs.
        Record::Avcc => {
            let Some((&count, mut rest)) = extradata.get(5..).and_then(<[u8]>::split_first) else {
                return out;
            };
            for _ in 0..count & 0x1f {
                if take(&mut rest, &mut out).is_none() {
                    break;
                }
            }
        }
        Record::AnnexB => out.extend(units(extradata, 0)),
    }
    out
}

/// How many pictures a stream's decoder keeps at once (its decoded picture
/// buffer), as its sequence parameter set declares. Of several, the largest
/// has the most pictures, then the most reordered: one SPS's own figures,
/// never one's pictures with another's reorder count.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct Dpb {
    /// Pictures kept at once, the one being decoded included: HEVC's
    /// `sps_max_dec_pic_buffering_minus1 + 1`, which counts it (H.265
    /// C.5.2.2 brings the DPB down to one less than that before a picture
    /// is decoded), and
    /// H.264's `max_dec_frame_buffering + 1`, whose DPB takes the current
    /// picture in only once it is decoded (H.264 C.4.5).
    pub pictures: u32,
    /// Of those, the most waiting for their turn in output order
    /// (`sps_max_num_reorder_pics`, `max_num_reorder_frames`).
    pub reorder: u32,
}

/// The most pictures either standard lets a DPB hold (H.265 A.4.2's
/// MaxDpbSize, H.264 A.3.1's MaxDpbFrames). An SPS declaring more is
/// misread or broken, and isn't believed.
const MAX_DPB: u32 = 16;

/// The DPB a sequence parameter set declares, `nal` being the whole NAL
/// unit. `None` for any other unit, an HEVC SPS of a layer above the base
/// (which the hardware decoder doesn't decode), and an SPS this can't read.
pub(crate) fn sps_dpb(hevc: bool, nal: &[u8]) -> Option<Dpb> {
    let dpb = match *nal {
        [h0, h1, ..] if hevc && (h0 >> 1) & 0x3f == 33 && h0 & 1 == 0 && h1 >> 3 == 0 => {
            hevc_sps_dpb(&unescape(nal)).ok()?
        }
        [h0, ..] if !hevc && h0 & 0x1f == 7 => {
            let sps = crate::mvc::syntax::parse_sps(&unescape(nal)).ok()?;
            let frames = sps.dec_frame_buffering();
            Dpb {
                pictures: frames.saturating_add(1),
                reorder: sps.reorder_frames().min(frames),
            }
        }
        _ => return None,
    };
    let current = u32::from(!hevc);
    (dpb.pictures <= MAX_DPB + current).then_some(dpb)
}

/// An HEVC SPS of the base layer (7.3.2.2) up to its sub-layer ordering
/// information, from its RBSP with the 2-byte NAL unit header. The decoder
/// decodes every temporal sub-layer, so the highest one's figures count;
/// they are also the largest (7.4.3.2.1), which is what is taken, so a
/// stream that breaks that rule still gets room.
fn hevc_sps_dpb(rbsp: &[u8]) -> Result<Dpb, Bad> {
    fn skip(r: &mut Reader, bits: u32) -> Result<(), Bad> {
        (0..bits.div_ceil(32)).try_for_each(|i| r.u((bits - 32 * i).min(32)).map(drop))
    }
    let mut r = Reader::new(rbsp, 16);
    r.u(4)?; // sps_video_parameter_set_id
    let sub_layers = r.u(3)? as usize; // sps_max_sub_layers_minus1
    if sub_layers > 6 {
        return Err(Bad::Broken("sps_max_sub_layers_minus1"));
    }
    r.u(1)?; // sps_temporal_id_nesting_flag
    // profile_tier_level(1, sps_max_sub_layers_minus1), 7.3.3: the general
    // profile (88 bits) and level, then each sub-layer's, when present.
    skip(&mut r, 88)?;
    r.u(8)?; // general_level_idc
    let mut present = [(false, false); 6];
    for p in &mut present[..sub_layers] {
        *p = (r.flag()?, r.flag()?);
    }
    if sub_layers > 0 {
        skip(&mut r, 2 * (8 - sub_layers as u32))?; // reserved_zero_2bits
    }
    for &(profile, level) in &present[..sub_layers] {
        if profile {
            skip(&mut r, 88)?;
        }
        if level {
            r.u(8)?;
        }
    }
    r.ue()?; // sps_seq_parameter_set_id
    if r.ue()? == 3 {
        r.u(1)?; // separate_colour_plane_flag
    }
    r.ue()?; // pic_width_in_luma_samples
    r.ue()?; // pic_height_in_luma_samples
    if r.flag()? {
        for _ in 0..4 {
            r.ue()?; // conf_win_*_offset
        }
    }
    r.ue()?; // bit_depth_luma_minus8
    r.ue()?; // bit_depth_chroma_minus8
    r.ue()?; // log2_max_pic_order_cnt_lsb_minus4
    // Without sps_sub_layer_ordering_info_present_flag, only the highest
    // sub-layer's figures are sent, and hold for those below.
    let first = if r.flag()? { 0 } else { sub_layers };
    let mut dpb = Dpb {
        pictures: 0,
        reorder: 0,
    };
    for _ in first..=sub_layers {
        dpb.pictures = dpb.pictures.max(r.ue()?.saturating_add(1));
        dpb.reorder = dpb.reorder.max(r.ue()?);
        r.ue()?; // sps_max_latency_increase_plus1
    }
    Ok(dpb)
}

/// The largest DPB the SPSs in a stream's extradata declare.
pub(crate) fn extradata_dpb(hevc: bool, extradata: &[u8]) -> Option<Dpb> {
    parameter_sets(hevc, extradata)
        .into_iter()
        .filter_map(|nal| sps_dpb(hevc, nal))
        .max()
}

/// The largest DPB the SPSs ahead of the first picture of an Annex B packet
/// declare, where an access unit has its parameter sets. Only the units
/// before the picture are read through, not the picture itself.
pub(crate) fn packet_dpb(hevc: bool, data: &[u8]) -> Option<Dpb> {
    let (mut found, mut at) = (None, 0);
    while let Some(code) = data.get(at..).and_then(start_code) {
        let begin = at + code + 3;
        let Some(&h0) = data.get(begin) else {
            break;
        };
        let picture = if hevc {
            (h0 >> 1) & 0x3f < 32
        } else {
            matches!(h0 & 0x1f, 1..=5)
        };
        if picture {
            break;
        }
        let end = start_code(&data[begin..]).map_or(data.len(), |n| begin + n);
        found = found.max(sps_dpb(hevc, &data[begin..end]));
        at = end;
    }
    found
}

/// The largest DPB an HEVC stream of pictures `width` x `height` may
/// declare at level 6.2, the highest level with limits (H.265 A.4.2, with
/// Table A.8's MaxLumaPs and the Main profiles' maxDpbPicBuf of 6): what a
/// decoder must be ready for when it can't read the stream's SPS. 6
/// pictures at 8K, 16 at 4K and below.
pub(crate) fn hevc_most_dpb(width: u32, height: u32) -> Dpb {
    const MAX_LUMA_PS: u64 = 35_651_584;
    const MAX_DPB_PIC_BUF: u32 = 6;
    let size = width as u64 * height as u64;
    let pictures = if size <= MAX_LUMA_PS >> 2 {
        4 * MAX_DPB_PIC_BUF
    } else if size <= MAX_LUMA_PS >> 1 {
        2 * MAX_DPB_PIC_BUF
    } else if size <= (3 * MAX_LUMA_PS) >> 2 {
        4 * MAX_DPB_PIC_BUF / 3
    } else {
        MAX_DPB_PIC_BUF
    }
    .min(MAX_DPB);
    Dpb {
        pictures,
        reorder: pictures - 1,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_frame_packing_message_says_the_3d_layout() {
        let h264 = NalReader {
            hevc: false,
            length_size: 0,
            top_layer: None,
        };
        let hevc = NalReader { hevc: true, ..h264 };
        // id 0, not cancelled, then the arrangement, sampling and eye order.
        let sei = |payload: [u8; 2]| vec![45, 2, payload[0], payload[1], 0x80];
        let side_by_side = sei([0x81, 0x81]);
        let cases = [
            (
                side_by_side.clone(),
                Some((Packing::Layout(StereoLayout::SideBySide), false)),
            ),
            (
                sei([0x81, 0x82]),
                Some((Packing::Layout(StereoLayout::SideBySide), true)),
            ),
            (
                sei([0x82, 0x01]),
                Some((Packing::Layout(StereoLayout::TopBottom), false)),
            ),
            (
                sei([0x83, 0x01]),
                Some((Packing::Layout(StereoLayout::Mono), false)),
            ),
            (sei([0x82, 0x81]), Some((Packing::FrameSequential, false))),
            (sei([0x82, 0x82]), Some((Packing::FrameSequential, true))),
            (sei([0x80, 0x81]), None),       // checkerboard
            (vec![45, 1, 0xc0, 0x80], None), // cancelled
            (vec![45, 9, 0x81], None),       // cut short
        ];
        for (rbsp, want) in cases {
            let in_h264 = [&[6u8][..], &rbsp].concat();
            assert_eq!(h264.frame_packing(&annex_b(&[&in_h264])), want, "{rbsp:x?}");
            let in_hevc = [&[39u8 << 1, 1][..], &rbsp].concat();
            assert_eq!(hevc.frame_packing(&annex_b(&[&in_hevc])), want, "{rbsp:x?}");
        }

        // After other messages and other units, in a length-prefixed packet.
        let two = [&[6u8, 5, 3, 1, 2, 3][..], &side_by_side].concat();
        let with_lengths = NalReader {
            length_size: 4,
            ..h264
        };
        assert_eq!(
            with_lengths.frame_packing(&prefixed(&[&[0x65, 1, 2], &two])),
            Some((Packing::Layout(StereoLayout::SideBySide), false))
        );
        // Not an SEI unit, and another message type with a long type number.
        assert_eq!(
            h264.frame_packing(&annex_b(&[&[0x65, 45, 2, 0x81, 0x81]])),
            None
        );
        assert_eq!(h264.frame_packing(&annex_b(&[&[6, 255, 1, 0, 0x80]])), None);
    }

    fn prefixed(nals: &[&[u8]]) -> Vec<u8> {
        let mut out = Vec::new();
        for n in nals {
            out.extend_from_slice(&(n.len() as u32).to_be_bytes());
            out.extend_from_slice(n);
        }
        out
    }

    fn annex_b(nals: &[&[u8]]) -> Vec<u8> {
        let mut out = Vec::new();
        for n in nals {
            out.extend_from_slice(&[0, 0, 0, 1]);
            out.extend_from_slice(n);
        }
        out
    }

    /// An hvcC holding one SPS with `max_sub_layers_minus1`.
    fn hvcc(max_sub_layers_minus1: u8) -> Vec<u8> {
        let mut c = vec![1u8; 22];
        c[21] = 0xf0 | 3; // 4-byte lengths
        c.push(1); // arrays
        c.extend_from_slice(&[0x80 | 33, 0, 1, 0, 4]);
        c.extend_from_slice(&[33 << 1, 1, max_sub_layers_minus1 << 1, 0]);
        c
    }

    const AVCC: [u8; 7] = [1, 0x64, 0, 0x28, 0xff, 0xe0, 0];

    #[test]
    fn h264_non_reference_slices() {
        let mut r = NalReader::new(ff::AV_CODEC_ID_H264, &AVCC).unwrap();
        assert!(r.disposable(&prefixed(&[&[0x01, 0x9a]])));
        // With an SEI ahead, and a picture in several slices.
        assert!(r.disposable(&prefixed(&[&[0x06, 5], &[0x01, 1], &[0x01, 2]])));
        assert!(!r.disposable(&prefixed(&[&[0x41, 0x9a]])), "referenced");
        assert!(!r.disposable(&prefixed(&[&[0x65, 0x88]])), "IDR");
        assert!(
            !r.disposable(&prefixed(&[&[0x67, 0x64], &[0x01, 0x9a]])),
            "SPS"
        );
        assert!(!r.disposable(&prefixed(&[&[0x01, 0x9a], &[0x21, 0x9a]])));
        assert!(!r.disposable(&prefixed(&[&[0x06, 5]])), "no picture");
        assert!(!r.disposable(&[]));
    }

    #[test]
    fn decoding_can_start_at_idr_cra_and_recovery_points() {
        let r = NalReader::new(ff::AV_CODEC_ID_H264, &AVCC).unwrap();
        let idr: &[u8] = &[0x65, 0x88];
        let i: &[u8] = &[0x61, 0x88];
        let p: &[u8] = &[0x41, 0x9a];
        // recovery_frame_cnt 0, then 1.
        let at_once: &[u8] = &[0x06, 6, 1, 0x80, 0x80];
        let later: &[u8] = &[0x06, 6, 1, 0x40, 0x80];
        assert!(r.random_access(&prefixed(&[&[0x09, 0x10], idr])));
        assert!(r.random_access(&prefixed(&[at_once, i])));
        assert!(!r.random_access(&prefixed(&[i])), "no recovery point");
        assert!(!r.random_access(&prefixed(&[later, i])));
        assert!(!r.random_access(&prefixed(&[at_once, p])));
        assert!(!r.random_access(&prefixed(&[p, idr])), "first picture");
        assert!(!r.random_access(&[]));

        let r = NalReader::new(ff::AV_CODEC_ID_HEVC, &hvcc(0)).unwrap();
        let vps: &[u8] = &[32 << 1, 1, 0x0c];
        for kind in [16u8, 19, 20, 21] {
            assert!(
                r.random_access(&prefixed(&[vps, &[kind << 1, 1, 0xaf]])),
                "{kind}"
            );
        }
        assert!(
            !r.random_access(&prefixed(&[&[1 << 1, 1, 0xaf]])),
            "TRAIL_R"
        );
        assert!(!r.random_access(&prefixed(&[&[8 << 1, 1, 0xaf]])), "RASL");
        let mut cut = prefixed(&[&[21 << 1, 1, 0xaf, 0xff, 0xff]]);
        cut.truncate(7);
        assert!(r.random_access(&cut), "cut short");
    }

    #[test]
    fn h264_annex_b() {
        let mut r = NalReader::new(ff::AV_CODEC_ID_H264, &[]).unwrap();
        assert!(r.disposable(&annex_b(&[&[0x09, 0x10], &[0x01, 0x9a, 0, 0, 3, 1]])));
        assert!(!r.disposable(&annex_b(&[&[0x09, 0x10], &[0x41, 0x9a]])));
        // Three-byte start codes.
        assert!(r.disposable(&[0, 0, 1, 0x01, 0x9a, 0, 0, 1, 0x01, 0x9b]));
    }

    #[test]
    fn hevc_non_reference_pictures_of_the_top_layer() {
        let mut r = NalReader::new(ff::AV_CODEC_ID_HEVC, &hvcc(0)).unwrap();
        let trail_n: &[u8] = &[0 << 1, 1, 0xaf];
        let trail_r: &[u8] = &[1 << 1, 1, 0xaf];
        let rasl_n: &[u8] = &[8 << 1, 1, 0xaf];
        let sei: &[u8] = &[39 << 1, 1, 5];
        assert!(r.disposable(&prefixed(&[sei, trail_n, trail_n])));
        assert!(r.disposable(&prefixed(&[rasl_n])));
        assert!(!r.disposable(&prefixed(&[trail_r])));
        assert!(!r.disposable(&prefixed(&[&[19 << 1, 1, 0xaf]])), "IDR");
        assert!(
            !r.disposable(&prefixed(&[&[34 << 1, 1, 0xc1], trail_n])),
            "PPS"
        );

        // Three sub-layers: only the top one's are unused by the others.
        let mut r = NalReader::new(ff::AV_CODEC_ID_HEVC, &hvcc(2)).unwrap();
        assert!(!r.disposable(&prefixed(&[trail_n])));
        assert!(!r.disposable(&prefixed(&[&[0, 2, 0xaf]])));
        assert!(r.disposable(&prefixed(&[&[0, 3, 0xaf]])));
    }

    #[test]
    fn hevc_parameter_sets_in_band_wait_for_an_sps() {
        let trail_n: &[u8] = &[0, 1, 0xaf];
        // A keyframe as hev1 MP4s and MPEG-TS carry it: AUD, VPS, SPS, PPS.
        let key: [&[u8]; 5] = [
            &[35 << 1, 1, 0x10],
            &[32 << 1, 1, 0x0c],
            &[33 << 1, 1, 0x01],
            &[34 << 1, 1, 0xc1],
            &[19 << 1, 1, 0xaf],
        ];
        let hev1 = [
            1u8, 1, 0x60, 0, 0, 0, 0xb0, 0, 0, 0, 0, 0, 0xb7, 0xf0, 0, 0xfc, 0xfd, 0xf8, 0xf8,
            0x3b, 0xf0, 0x8b, 0,
        ];
        for (mut r, pack) in [
            (
                NalReader::new(ff::AV_CODEC_ID_HEVC, &[]).unwrap(),
                annex_b as fn(&[&[u8]]) -> Vec<u8>,
            ),
            (
                NalReader::new(ff::AV_CODEC_ID_HEVC, &hev1).unwrap(),
                prefixed,
            ),
        ] {
            assert!(!r.disposable(&pack(&[trail_n])), "layers unknown");
            assert!(!r.disposable(&pack(&key)));
            assert!(r.disposable(&pack(&[trail_n])));
        }
    }

    /// The video stream's extradata of a fixture.
    fn extradata(name: &str) -> Vec<u8> {
        let input = crate::info::tests::open(name);
        let i = input.best_stream(ff::AVMEDIA_TYPE_VIDEO).expect("video");
        // SAFETY: the stream and its parameters live while input is open;
        // extradata holds extradata_size bytes.
        unsafe {
            let par = &*(*input.streams()[i]).codecpar;
            std::slice::from_raw_parts(par.extradata, par.extradata_size as usize).to_vec()
        }
    }

    // hevc10_bframes.mp4: `ffmpeg -f lavfi -i testsrc2=size=256x128:rate=30
    // -frames:v 30 -c:v libx265 -pix_fmt yuv420p10le -x265-params
    // bframes=3:keyint=10:crf=45:info=0 -tag:v hvc1 -map_metadata -1
    // -fflags +bitexact -flags:v +bitexact`. Its SPS, as `-bsf:v
    // trace_headers` shows it: one sub-layer, sps_max_dec_pic_buffering_minus1
    // 4, sps_max_num_reorder_pics 2.
    const HEVC10_DPB: Dpb = Dpb {
        pictures: 5,
        reorder: 2,
    };

    #[test]
    fn an_hevc_stream_declares_its_dpb_in_its_sps() {
        let hvcc = extradata("hevc10_bframes.mp4");
        assert_eq!(Record::of(true, &hvcc), Record::Hvcc);
        assert_eq!(extradata_dpb(true, &hvcc), Some(HEVC10_DPB));
        // The same parameter sets in band, as the decoder is sent them:
        // Annex B, ahead of the picture.
        let sets = parameter_sets(true, &hvcc);
        assert_eq!(sets.len(), 3, "VPS, SPS, PPS");
        let idr: &[u8] = &[19 << 1, 1, 0xaf];
        let aud: &[u8] = &[35 << 1, 1, 0x10];
        let packet = annex_b(&[&[aud][..], &sets[..], &[idr][..]].concat());
        assert_eq!(packet_dpb(true, &packet), Some(HEVC10_DPB));
        // MPEG-TS extradata is Annex B too.
        let annex_b_sets = annex_b(&sets);
        assert_eq!(Record::of(true, &annex_b_sets), Record::AnnexB);
        assert_eq!(extradata_dpb(true, &annex_b_sets), Some(HEVC10_DPB));
        // A picture without parameter sets says nothing, and nothing after
        // the picture is read.
        assert_eq!(packet_dpb(true, &annex_b(&[aud, idr])), None);
        assert_eq!(
            packet_dpb(true, &annex_b(&[&[idr][..], &sets[..]].concat())),
            None
        );
        assert_eq!(packet_dpb(true, &[]), None);
    }

    #[test]
    fn an_h264_stream_declares_its_dpb_in_its_vui() {
        // h264_bframes.mp4: as hevc10_bframes.mp4, with `-c:v libx264
        // -pix_fmt yuv420p -bf 3 -g 10 -crf 45`. trace_headers: level 1.2,
        // 16x8 macroblocks, max_dec_frame_buffering 4, max_num_reorder_frames
        // 2.
        let avcc = extradata("h264_bframes.mp4");
        assert_eq!(Record::of(false, &avcc), Record::Avcc);
        let dpb = Dpb {
            pictures: 5,
            reorder: 2,
        };
        assert_eq!(extradata_dpb(false, &avcc), Some(dpb));
        let sets = parameter_sets(false, &avcc);
        assert_eq!(sets.len(), 1, "the SPS alone");
        let slice: &[u8] = &[0x65, 0x88];
        assert_eq!(packet_dpb(false, &annex_b(&[sets[0], slice])), Some(dpb));
        assert_eq!(packet_dpb(false, &annex_b(&[slice, sets[0]])), None);

        // Without the VUI's figure: its level's most for its size (A.3.1),
        // at most 16.
        let mut sps = crate::mvc::syntax::parse_sps(&unescape(sets[0])).unwrap();
        sps.vui = None;
        assert_eq!(sps.dec_frame_buffering(), 16);
        // Level 4 at 1080p: 32768 macroblocks over 120x68.
        (sps.level, sps.width_mbs, sps.height_map_units) = (40, 120, 68);
        assert_eq!(sps.dec_frame_buffering(), 4);
    }

    /// An HEVC SPS of layer `layer` with `sub_layers` + 1 temporal
    /// sub-layers, the lowest with a profile of its own and the next with a
    /// level, and (dec_pic_buffering_minus1, num_reorder_pics) for the
    /// highest sub-layer alone or for each.
    fn hevc_sps(layer: u8, sub_layers: u32, ordering: &[(u32, u32)]) -> Vec<u8> {
        numbered_hevc_sps(0, layer, sub_layers, ordering)
    }

    /// [`hevc_sps`] with `sps_seq_parameter_set_id` `id`.
    fn numbered_hevc_sps(id: u32, layer: u8, sub_layers: u32, ordering: &[(u32, u32)]) -> Vec<u8> {
        use crate::mvc::bits::{Writer, escape_into};
        let mut w = Writer::new();
        w.u(4, 0); // sps_video_parameter_set_id
        w.u(3, sub_layers);
        w.flag(true); // sps_temporal_id_nesting_flag
        // Main 10, its compatibility flag, progressive, frame only; then the
        // 43 reserved bits and general_inbld_flag.
        let profile = |w: &mut Writer| {
            w.u(2, 0);
            w.flag(false);
            w.u(5, 2);
            w.u(32, 1 << 29);
            w.u(4, 0b1001);
            w.u(32, 0);
            w.u(12, 0);
        };
        profile(&mut w);
        w.u(8, 153); // general_level_idc: 5.1
        for i in 0..sub_layers {
            w.flag(i == 0);
            w.flag(i == 1);
        }
        if sub_layers > 0 {
            for _ in sub_layers..8 {
                w.u(2, 0);
            }
        }
        for i in 0..sub_layers {
            if i == 0 {
                profile(&mut w);
            }
            if i == 1 {
                w.u(8, 120);
            }
        }
        w.ue(id); // sps_seq_parameter_set_id
        w.ue(1); // 4:2:0
        w.ue(3840);
        w.ue(2176);
        w.flag(true); // conformance window: 16 rows off the bottom
        for offset in [0, 0, 0, 8] {
            w.ue(offset);
        }
        w.ue(2); // 10-bit
        w.ue(2);
        w.ue(4); // log2_max_pic_order_cnt_lsb_minus4
        w.flag(ordering.len() > 1);
        for &(buffering, reorder) in ordering {
            w.ue(buffering);
            w.ue(reorder);
            w.ue(0);
        }
        w.trailing();
        let mut nal = vec![33 << 1 | layer >> 5, (layer & 0x1f) << 3 | 1];
        escape_into(&w.into_bytes(), &mut nal);
        nal
    }

    #[test]
    fn hevc_sub_layers_count_their_highest() {
        // The highest sub-layer's figures alone, holding for those below.
        let sps = hevc_sps(0, 2, &[(5, 3)]);
        // The reserved zero bits needed emulation prevention bytes.
        assert!(sps.windows(3).any(|w| w == [0, 0, 3]));
        let dpb = Dpb {
            pictures: 6,
            reorder: 3,
        };
        assert_eq!(sps_dpb(true, &sps), Some(dpb));
        // Each sub-layer's: the highest's, which are the largest.
        let sps = hevc_sps(0, 2, &[(1, 0), (3, 1), (5, 3)]);
        assert_eq!(sps_dpb(true, &sps), Some(dpb));
        // One that breaks the rule still gets room for its largest.
        let sps = hevc_sps(0, 2, &[(5, 3), (3, 1), (1, 0)]);
        assert_eq!(sps_dpb(true, &sps), Some(dpb));
        // One sub-layer.
        let sps = hevc_sps(0, 0, &[(4, 2)]);
        assert_eq!(sps_dpb(true, &sps), Some(HEVC10_DPB));
    }

    #[test]
    fn of_several_spss_the_largest_dpb_counts() {
        // Two SPSs whose DPBs differ, the larger first: a stream whose
        // pictures use SPS 0 and carry SPS 1 besides, which they don't.
        let larger = numbered_hevc_sps(0, 0, 0, &[(5, 3)]);
        let smaller = numbered_hevc_sps(1, 0, 0, &[(2, 1)]);
        let six = Dpb {
            pictures: 6,
            reorder: 3,
        };
        assert_eq!(sps_dpb(true, &smaller).map(|d| d.pictures), Some(3));
        // In an hvcC, one array of both.
        let mut hvcc = vec![1u8; 22];
        hvcc[21] = 0xf0 | 3;
        hvcc.extend_from_slice(&[1, 0x80 | 33, 0, 2]);
        for sps in [&larger, &smaller] {
            hvcc.extend_from_slice(&(sps.len() as u16).to_be_bytes());
            hvcc.extend_from_slice(sps);
        }
        assert_eq!(Record::of(true, &hvcc), Record::Hvcc);
        assert_eq!(parameter_sets(true, &hvcc).len(), 2);
        assert_eq!(extradata_dpb(true, &hvcc), Some(six));
        // As Annex B extradata, either way round.
        assert_eq!(
            extradata_dpb(true, &annex_b(&[&larger[..], &smaller])),
            Some(six)
        );
        assert_eq!(
            extradata_dpb(true, &annex_b(&[&smaller[..], &larger])),
            Some(six)
        );
        // Ahead of a picture in a packet.
        let idr: &[u8] = &[19 << 1, 1, 0xaf];
        assert_eq!(
            packet_dpb(true, &annex_b(&[&larger[..], &smaller, idr])),
            Some(six)
        );
        // Of the same DPB, the most reordered: one SPS's own figures.
        let fewer_reordered = numbered_hevc_sps(1, 0, 0, &[(5, 1)]);
        assert_eq!(
            packet_dpb(true, &annex_b(&[&larger[..], &fewer_reordered, idr])),
            Some(six)
        );
    }

    #[test]
    fn what_is_not_a_base_layer_sps_says_nothing() {
        // Another layer's SPS (MV-HEVC's second view), another unit, an
        // SPS cut short, one past the most any level allows, H.264's SPS
        // read as HEVC.
        assert_eq!(sps_dpb(true, &hevc_sps(1, 0, &[(4, 2)])), None);
        assert_eq!(sps_dpb(true, &[34 << 1, 1, 0xc1]), None);
        let sps = hevc_sps(0, 0, &[(4, 2)]);
        assert_eq!(sps_dpb(true, &sps[..sps.len() - 4]), None);
        assert_eq!(
            sps_dpb(true, &hevc_sps(0, 0, &[(15, 2)])).map(|d| d.pictures),
            Some(16)
        );
        assert_eq!(sps_dpb(true, &hevc_sps(0, 0, &[(16, 2)])), None);
        assert_eq!(sps_dpb(false, &sps), None);
        assert_eq!(extradata_dpb(true, &[]), None);
    }

    #[test]
    fn the_most_an_hevc_dpb_may_be_shrinks_with_the_picture() {
        let most = |w, h| hevc_most_dpb(w, h).pictures;
        assert_eq!(most(8192, 4320), 6);
        assert_eq!(most(8192, 4096), 6);
        assert_eq!(most(7680, 3840), 6);
        assert_eq!(most(6144, 3072), 8);
        assert_eq!(most(5760, 2880), 12);
        assert_eq!(most(3840, 2160), 16);
        assert_eq!(most(1920, 1080), 16);
    }

    #[test]
    fn other_codecs_and_cut_short_data() {
        assert!(NalReader::new(ff::AV_CODEC_ID_VP9, &[]).is_none());
        let mut r = NalReader::new(ff::AV_CODEC_ID_H264, &AVCC).unwrap();
        // A length running past the end: the unit as far as it goes.
        assert!(r.disposable(&[0, 0, 0, 9, 0x01, 0x9a]));
        assert!(!r.disposable(&[0, 0, 0]));
        assert_eq!(units(&[0, 0, 0, 0, 0, 0, 1], 0).count(), 1);
    }
}
