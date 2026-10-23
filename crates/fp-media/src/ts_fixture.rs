//! MPEG-TS test files made from the H.264/AAC MP4 fixture, with timestamps
//! that start late as broadcast recordings and Blu-ray (.m2ts) files do.
//! Our FFmpeg has no muxers, so this writes the transport stream itself.

use crate::decode::Packet;
use crate::input::Input;
use fp_core::source::{FileSource, MemorySource};
use fp_ffmpeg_sys as ff;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

const PMT_PID: u16 = 0x1000;
const VIDEO_PID: u16 = 0x100;
const AUDIO_PID: u16 = 0x101;

/// h264_aac_180_LR.mp4 as MPEG-TS, its timestamps moved `offset` seconds on.
pub(crate) fn h264_aac_ts(offset: f64) -> Arc<MemorySource> {
    Arc::new(MemorySource(h264_aac_ts_keyed(offset).0))
}

/// [`h264_aac_ts`]'s bytes, and the time and byte offset of each video
/// keyframe: where a server would cut HLS segments.
pub(crate) fn h264_aac_ts_keyed(offset: f64) -> (Vec<u8>, Vec<(f64, usize)>) {
    let mut keys = Vec::new();
    let path =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data/h264_aac_180_LR.mp4");
    let src = Arc::new(FileSource::open(&path).expect("fixture"));
    let mut input = Input::open(src, "x.mp4", Arc::new(AtomicBool::new(false))).expect("open");
    let streams: Vec<(ff::AVMediaType, f64, Vec<u8>)> = input
        .streams()
        .iter()
        .map(|&st| {
            // SAFETY: streams and their parameters live while input is open.
            unsafe {
                let par = &*(*st).codecpar;
                let extra = if par.extradata.is_null() {
                    Vec::new()
                } else {
                    std::slice::from_raw_parts(par.extradata, par.extradata_size as usize).to_vec()
                };
                (par.codec_type, crate::q2d((*st).time_base), extra)
            }
        })
        .collect();
    let extra = |kind| &streams.iter().find(|s| s.0 == kind).expect("stream").2;
    let (parameter_sets, length_size) = avc_config(extra(ff::AVMEDIA_TYPE_VIDEO));
    let audio_config = extra(ff::AVMEDIA_TYPE_AUDIO).clone();

    let mut ts = Writer::default();
    ts.section(0, &pat());
    ts.section(PMT_PID, &pmt());
    let mut pkt = Packet::new();
    while input.read(pkt.as_ptr()).expect("read") {
        // SAFETY: a packet just read.
        let p = unsafe { &*pkt.as_ptr() };
        let (kind, tb, _) = &streams[pkt.stream_index()];
        // SAFETY: the packet's data is `size` bytes.
        let data = unsafe { std::slice::from_raw_parts(p.data, p.size as usize) };
        let clock = |t: i64| ((t as f64 * tb + offset) * 90_000.0).round() as u64;
        let (pts, dts) = (clock(p.pts), clock(p.dts));
        match *kind {
            ff::AVMEDIA_TYPE_VIDEO => {
                let mut es = Vec::new();
                if p.flags & ff::AV_PKT_FLAG_KEY as i32 != 0 {
                    es.extend_from_slice(&parameter_sets);
                    keys.push((pts as f64 / 90_000.0 - offset, ts.out.len()));
                }
                annex_b(data, length_size, &mut es);
                let pcr = dts.saturating_sub(9000);
                ts.pes(VIDEO_PID, &pes(0xe0, pts, Some(dts), &es), Some(pcr));
            }
            ff::AVMEDIA_TYPE_AUDIO => {
                let mut es = adts_header(&audio_config, data.len());
                es.extend_from_slice(data);
                ts.pes(AUDIO_PID, &pes(0xc0, pts, None, &es), None);
            }
            _ => {}
        }
        pkt.unref();
    }
    (ts.out, keys)
}

/// The SPS and PPS of an `avcC` record as Annex B, and its NAL length size.
fn avc_config(avcc: &[u8]) -> (Vec<u8>, usize) {
    let length_size = (avcc[4] & 3) as usize + 1;
    let mut out = Vec::new();
    let mut at = 5;
    for mask in [0x1f, 0xff] {
        let count = avcc[at] & mask;
        at += 1;
        for _ in 0..count {
            let len = u16::from_be_bytes([avcc[at], avcc[at + 1]]) as usize;
            out.extend_from_slice(&[0, 0, 0, 1]);
            out.extend_from_slice(&avcc[at + 2..at + 2 + len]);
            at += 2 + len;
        }
    }
    (out, length_size)
}

/// Length-prefixed NAL units to Annex B start codes.
fn annex_b(mut data: &[u8], length_size: usize, out: &mut Vec<u8>) {
    while data.len() > length_size {
        let len = data[..length_size]
            .iter()
            .fold(0usize, |n, &b| n << 8 | b as usize);
        let nal = &data[length_size..(length_size + len).min(data.len())];
        out.extend_from_slice(&[0, 0, 0, 1]);
        out.extend_from_slice(nal);
        data = &data[length_size + nal.len()..];
    }
}

/// An ADTS header for an AAC frame of `len` bytes, from its
/// AudioSpecificConfig.
fn adts_header(config: &[u8], len: usize) -> Vec<u8> {
    let object = config[0] >> 3;
    let rate = ((config[0] & 7) << 1) | (config[1] >> 7);
    let channels = (config[1] >> 3) & 0xf;
    let n = len + 7;
    vec![
        0xff,
        0xf1,
        ((object - 1) << 6) | (rate << 2) | (channels >> 2),
        ((channels & 3) << 6) | (n >> 11) as u8,
        (n >> 3) as u8,
        ((n & 7) << 5) as u8 | 0x1f,
        0xfc,
    ]
}

fn pes(stream_id: u8, pts: u64, dts: Option<u64>, payload: &[u8]) -> Vec<u8> {
    let header = if dts.is_some() { 10 } else { 5 };
    let len = 3 + header + payload.len();
    // Video may leave the length out (0), and must when it doesn't fit.
    let len = if stream_id == 0xe0 || len > 0xffff {
        0
    } else {
        len
    };
    let mut v = vec![0, 0, 1, stream_id];
    v.extend_from_slice(&(len as u16).to_be_bytes());
    v.extend_from_slice(&[0x80, if dts.is_some() { 0xc0 } else { 0x80 }, header as u8]);
    timestamp(&mut v, if dts.is_some() { 3 } else { 2 }, pts);
    if let Some(dts) = dts {
        timestamp(&mut v, 1, dts);
    }
    v.extend_from_slice(payload);
    v
}

fn timestamp(v: &mut Vec<u8>, prefix: u8, t: u64) {
    v.push((prefix << 4) | (((t >> 30) & 7) as u8) << 1 | 1);
    v.extend_from_slice(&((((t >> 15) & 0x7fff) << 1 | 1) as u16).to_be_bytes());
    v.extend_from_slice(&((((t & 0x7fff) << 1) | 1) as u16).to_be_bytes());
}

fn pat() -> Vec<u8> {
    let [hi, lo] = PMT_PID.to_be_bytes();
    with_crc(vec![0x00, 0xb0, 13, 0, 1, 0xc1, 0, 0, 0, 1, 0xe0 | hi, lo])
}

fn pmt() -> Vec<u8> {
    let [vh, vl] = VIDEO_PID.to_be_bytes();
    let [ah, al] = AUDIO_PID.to_be_bytes();
    with_crc(vec![
        0x02,
        0xb0,
        23,
        0,
        1,
        0xc1,
        0,
        0,
        0xe0 | vh,
        vl,
        0xf0,
        0, // PCR on the video
        0x1b,
        0xe0 | vh,
        vl,
        0xf0,
        0, // H.264
        0x0f,
        0xe0 | ah,
        al,
        0xf0,
        0, // AAC in ADTS
    ])
}

fn with_crc(mut section: Vec<u8>) -> Vec<u8> {
    let mut crc = 0xffff_ffffu32;
    for &b in &section {
        crc ^= (b as u32) << 24;
        for _ in 0..8 {
            crc = if crc & 0x8000_0000 != 0 {
                (crc << 1) ^ 0x04c1_1db7
            } else {
                crc << 1
            };
        }
    }
    section.extend_from_slice(&crc.to_be_bytes());
    section
}

#[derive(Default)]
struct Writer {
    out: Vec<u8>,
    counters: std::collections::HashMap<u16, u8>,
}

impl Writer {
    fn section(&mut self, pid: u16, section: &[u8]) {
        let mut data = vec![0]; // pointer field
        data.extend_from_slice(section);
        self.pes(pid, &data, None);
    }

    /// Splits `data` into 188-byte packets, the first carrying `pcr` (90 kHz)
    /// and the last padded through its adaptation field.
    fn pes(&mut self, pid: u16, mut data: &[u8], pcr: Option<u64>) {
        let mut first = true;
        while first || !data.is_empty() {
            let pcr = pcr.filter(|_| first);
            let n = data.len().min(if pcr.is_some() { 176 } else { 184 });
            let adaptation = (pcr.is_some() || n < 184).then(|| 183 - n);
            let cc = self.counters.entry(pid).or_default();
            let [hi, lo] = pid.to_be_bytes();
            let start = self.out.len();
            self.out.extend_from_slice(&[
                0x47,
                if first { 0x40 } else { 0 } | hi,
                lo,
                if adaptation.is_some() { 0x30 } else { 0x10 } | *cc,
            ]);
            *cc = (*cc + 1) & 0xf;
            if let Some(len) = adaptation {
                self.out.push(len as u8);
                if len > 0 {
                    self.out.push(if pcr.is_some() { 0x10 } else { 0 });
                }
                if let Some(pcr) = pcr {
                    self.out.extend_from_slice(&[
                        (pcr >> 25) as u8,
                        (pcr >> 17) as u8,
                        (pcr >> 9) as u8,
                        (pcr >> 1) as u8,
                        ((pcr & 1) as u8) << 7 | 0x7e,
                        0,
                    ]);
                }
                self.out.resize(start + 5 + len, 0xff);
            }
            self.out.extend_from_slice(&data[..n]);
            data = &data[n..];
            first = false;
        }
    }
}
