//! The H.264 syntax the MVC rewrite reads and writes: sequence parameter
//! sets (and MVC's subset SPS), picture parameter sets and slice headers.

use super::Bad;
use super::bits::{Reader, Writer, stop_bit};
use std::sync::Arc;

/// Profiles whose SPS carries chroma format, bit depths and scaling lists.
const HIGH_PROFILES: [u32; 13] = [100, 110, 122, 244, 44, 83, 86, 118, 128, 138, 139, 134, 135];

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Sps {
    pub id: u32,
    pub profile: u32,
    pub constraint: u32,
    pub level: u32,
    pub chroma_format: u32,
    pub separate_colour_planes: bool,
    pub bit_depth_luma_minus8: u32,
    pub bit_depth_chroma_minus8: u32,
    pub transform_bypass: bool,
    /// Each scaling list's delta_scale codes as read (None when the list is
    /// absent); None without seq_scaling_matrix_present_flag.
    pub scaling: Option<Vec<Option<Vec<i32>>>>,
    pub log2_max_frame_num: u32,
    pub poc_type: u32,
    pub log2_max_poc_lsb: u32,
    pub delta_pic_order_always_zero: bool,
    pub offset_for_non_ref_pic: i32,
    pub offset_for_top_to_bottom_field: i32,
    pub offsets_for_ref_frame: Vec<i32>,
    pub max_num_ref_frames: u32,
    pub width_mbs: u32,
    pub height_map_units: u32,
    pub frame_mbs_only: bool,
    pub mbaff: bool,
    pub direct_8x8_inference: bool,
    pub crop: Option<[u32; 4]>,
    pub vui: Option<Vui>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Vui {
    /// aspect_ratio_idc, and the sample aspect when it is 255 (Extended_SAR).
    pub aspect_ratio: Option<(u32, Option<(u32, u32)>)>,
    pub overscan_appropriate: Option<bool>,
    /// video_format, video_full_range_flag and the colour description.
    pub signal: Option<(u32, bool, Option<[u32; 3]>)>,
    pub chroma_location: Option<(u32, u32)>,
    /// num_units_in_tick, time_scale, fixed_frame_rate_flag.
    pub timing: Option<(u32, u32, bool)>,
    pub restriction: Option<Restriction>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Restriction {
    pub mvs_over_pic_boundaries: bool,
    pub max_bytes_per_pic_denom: u32,
    pub max_bits_per_mb_denom: u32,
    pub log2_max_mv_length: [u32; 2],
    pub max_num_reorder_frames: u32,
    pub max_dec_frame_buffering: u32,
}

impl Sps {
    pub(crate) fn frame_mbs(&self) -> u32 {
        self.width_mbs * self.height_map_units * (2 - u32::from(self.frame_mbs_only))
    }

    /// MaxDpbFrames of its level for its picture size (A.3.1).
    fn max_dpb_frames(&self) -> u32 {
        let mbs = match self.level {
            0..=10 => 396,
            11 => 900,
            12 | 13 | 20 => 2376,
            21 => 4752,
            22 | 30 => 8100,
            31 => 18000,
            32 => 20480,
            40 | 41 => 32768,
            42 => 34816,
            50 => 110_400,
            51 | 52 => 184_320,
            _ => 696_320,
        };
        (mbs / self.frame_mbs().max(1)).min(16)
    }

    /// The most pictures any picture follows in decoding order but comes
    /// before in output order: as the VUI says, else as inferred (E.2.1).
    pub(crate) fn reorder_frames(&self) -> u32 {
        self.vui
            .as_ref()
            .and_then(|v| v.restriction)
            .map_or_else(|| self.max_dpb_frames(), |r| r.max_num_reorder_frames)
    }

    /// Whether a view coded with `other` decodes its samples the same way:
    /// everything but the numbering of frames and pictures.
    pub(crate) fn decodes_like(&self, other: &Sps) -> Result<(), Bad> {
        let same = [
            ("chroma format", self.chroma_format == other.chroma_format),
            (
                "colour planes",
                self.separate_colour_planes == other.separate_colour_planes,
            ),
            (
                "bit depth",
                (self.bit_depth_luma_minus8, self.bit_depth_chroma_minus8)
                    == (other.bit_depth_luma_minus8, other.bit_depth_chroma_minus8),
            ),
            ("lossless", self.transform_bypass == other.transform_bypass),
            ("scaling lists", self.scaling == other.scaling),
            (
                "picture size",
                (self.width_mbs, self.height_map_units)
                    == (other.width_mbs, other.height_map_units),
            ),
            (
                "frame coding",
                (self.frame_mbs_only, self.mbaff) == (other.frame_mbs_only, other.mbaff),
            ),
            (
                "direct 8x8 inference",
                self.direct_8x8_inference == other.direct_8x8_inference,
            ),
        ];
        match same.iter().find(|(_, s)| !s) {
            Some((what, _)) => Err(Bad::Unsupported(format!("the views differ in {what}"))),
            None => Ok(()),
        }
    }
}

fn scaling_list(r: &mut Reader, size: usize) -> Result<Vec<i32>, Bad> {
    let mut codes = Vec::new();
    let (mut last, mut next) = (8i32, 8i32);
    for _ in 0..size {
        if next != 0 {
            let delta = r.se()?;
            codes.push(delta);
            next = (last + delta + 256).rem_euclid(256);
        }
        if next != 0 {
            last = next;
        }
    }
    Ok(codes)
}

fn skip_hrd(r: &mut Reader) -> Result<(), Bad> {
    let count = r.ue()? + 1;
    if count > 32 {
        return Err(Bad::Broken("HRD"));
    }
    r.u(8)?;
    for _ in 0..count {
        r.ue()?;
        r.ue()?;
        r.u(1)?;
    }
    r.u(20)?;
    Ok(())
}

fn vui(r: &mut Reader) -> Result<Vui, Bad> {
    let mut v = Vui::default();
    if r.flag()? {
        let idc = r.u(8)?;
        let sar = if idc == 255 {
            Some((r.u(16)?, r.u(16)?))
        } else {
            None
        };
        v.aspect_ratio = Some((idc, sar));
    }
    if r.flag()? {
        v.overscan_appropriate = Some(r.flag()?);
    }
    if r.flag()? {
        let format = r.u(3)?;
        let full = r.flag()?;
        let colour = if r.flag()? {
            Some([r.u(8)?, r.u(8)?, r.u(8)?])
        } else {
            None
        };
        v.signal = Some((format, full, colour));
    }
    if r.flag()? {
        v.chroma_location = Some((r.ue()?, r.ue()?));
    }
    if r.flag()? {
        v.timing = Some((r.u(32)?, r.u(32)?, r.flag()?));
    }
    let nal_hrd = r.flag()?;
    if nal_hrd {
        skip_hrd(r)?;
    }
    let vcl_hrd = r.flag()?;
    if vcl_hrd {
        skip_hrd(r)?;
    }
    if nal_hrd || vcl_hrd {
        r.u(1)?; // low_delay_hrd_flag
    }
    r.u(1)?; // pic_struct_present_flag
    if r.flag()? {
        v.restriction = Some(Restriction {
            mvs_over_pic_boundaries: r.flag()?,
            max_bytes_per_pic_denom: r.ue()?,
            max_bits_per_mb_denom: r.ue()?,
            log2_max_mv_length: [r.ue()?, r.ue()?],
            max_num_reorder_frames: r.ue()?,
            max_dec_frame_buffering: r.ue()?,
        });
    }
    Ok(v)
}

fn sps_data(r: &mut Reader) -> Result<Sps, Bad> {
    let profile = r.u(8)?;
    let constraint = r.u(8)?;
    let level = r.u(8)?;
    let id = r.ue()?;
    let mut s = Sps {
        id,
        profile,
        constraint,
        level,
        chroma_format: 1,
        separate_colour_planes: false,
        bit_depth_luma_minus8: 0,
        bit_depth_chroma_minus8: 0,
        transform_bypass: false,
        scaling: None,
        log2_max_frame_num: 4,
        poc_type: 0,
        log2_max_poc_lsb: 4,
        delta_pic_order_always_zero: false,
        offset_for_non_ref_pic: 0,
        offset_for_top_to_bottom_field: 0,
        offsets_for_ref_frame: Vec::new(),
        max_num_ref_frames: 0,
        width_mbs: 0,
        height_map_units: 0,
        frame_mbs_only: true,
        mbaff: false,
        direct_8x8_inference: false,
        crop: None,
        vui: None,
    };
    if id > 31 {
        return Err(Bad::Broken("SPS id"));
    }
    if HIGH_PROFILES.contains(&profile) {
        s.chroma_format = r.ue()?;
        if s.chroma_format == 3 {
            s.separate_colour_planes = r.flag()?;
        }
        s.bit_depth_luma_minus8 = r.ue()?;
        s.bit_depth_chroma_minus8 = r.ue()?;
        s.transform_bypass = r.flag()?;
        if r.flag()? {
            let lists = if s.chroma_format == 3 { 12 } else { 8 };
            let mut all = Vec::with_capacity(lists);
            for i in 0..lists {
                all.push(if r.flag()? {
                    Some(scaling_list(r, if i < 6 { 16 } else { 64 })?)
                } else {
                    None
                });
            }
            s.scaling = Some(all);
        }
    }
    s.log2_max_frame_num = r.ue()? + 4;
    s.poc_type = r.ue()?;
    match s.poc_type {
        0 => s.log2_max_poc_lsb = r.ue()? + 4,
        1 => {
            s.delta_pic_order_always_zero = r.flag()?;
            s.offset_for_non_ref_pic = r.se()?;
            s.offset_for_top_to_bottom_field = r.se()?;
            let n = r.ue()?;
            if n > 255 {
                return Err(Bad::Broken("POC cycle"));
            }
            for _ in 0..n {
                s.offsets_for_ref_frame.push(r.se()?);
            }
        }
        2 => {}
        _ => return Err(Bad::Broken("POC type")),
    }
    if s.log2_max_frame_num > 16 || s.log2_max_poc_lsb > 16 {
        return Err(Bad::Broken("SPS numbering"));
    }
    s.max_num_ref_frames = r.ue()?;
    r.u(1)?; // gaps_in_frame_num_value_allowed_flag
    s.width_mbs = r.ue()? + 1;
    s.height_map_units = r.ue()? + 1;
    s.frame_mbs_only = r.flag()?;
    if !s.frame_mbs_only {
        s.mbaff = r.flag()?;
    }
    s.direct_8x8_inference = r.flag()?;
    if r.flag()? {
        s.crop = Some([r.ue()?, r.ue()?, r.ue()?, r.ue()?]);
    }
    if r.flag()? {
        s.vui = Some(vui(r)?);
    }
    Ok(s)
}

/// A sequence parameter set (NAL unit type 7), from its RBSP.
pub(crate) fn parse_sps(rbsp: &[u8]) -> Result<Sps, Bad> {
    sps_data(&mut Reader::new(rbsp, 8))
}

/// An MVC subset sequence parameter set (NAL unit type 15) for two views.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct SubsetSps {
    pub sps: Sps,
    pub view_ids: Vec<u32>,
    /// The second view's inter-view references, list 0 and 1, in anchor
    /// and in other access units.
    pub anchor_refs: [Vec<u32>; 2],
    pub non_anchor_refs: [Vec<u32>; 2],
}

pub(crate) fn parse_subset_sps(rbsp: &[u8]) -> Result<SubsetSps, Bad> {
    let mut r = Reader::new(rbsp, 8);
    let sps = sps_data(&mut r)?;
    if !matches!(sps.profile, 118 | 128) {
        return Err(Bad::Unsupported(format!(
            "subset SPS of profile {}",
            sps.profile
        )));
    }
    r.u(1)?; // bit_equal_to_one
    let views = r.ue()? + 1;
    if views > 1024 {
        return Err(Bad::Broken("views"));
    }
    let mut view_ids = Vec::new();
    for _ in 0..views {
        view_ids.push(r.ue()?);
    }
    if views != 2 {
        return Err(Bad::Unsupported(format!("{views} views")));
    }
    let refs = |r: &mut Reader| -> Result<Vec<u32>, Bad> {
        let n = r.ue()?;
        if n > 15 {
            return Err(Bad::Broken("inter-view references"));
        }
        (0..n).map(|_| r.ue()).collect()
    };
    let anchor_refs = [refs(&mut r)?, refs(&mut r)?];
    let non_anchor_refs = [refs(&mut r)?, refs(&mut r)?];
    Ok(SubsetSps {
        sps,
        view_ids,
        anchor_refs,
        non_anchor_refs,
    })
}

/// What the merged stream's SPS changes from the base view's.
pub(crate) struct Merged {
    pub level: u32,
    pub max_num_ref_frames: u32,
    pub max_num_reorder_frames: u32,
    pub max_dec_frame_buffering: u32,
}

/// The merged stream's SPS NAL unit (RBSP with its header byte): the base
/// view's, with 16-bit frame numbers and picture order count LSBs (type 0),
/// room for both views' references, twice the frame rate, no HRD, and the
/// reordering the merged stream needs.
pub(crate) fn write_sps(s: &Sps, m: &Merged) -> Vec<u8> {
    let mut w = Writer::new();
    w.u(8, 0x67);
    w.u(8, s.profile);
    w.u(8, s.constraint);
    w.u(8, m.level);
    w.ue(s.id);
    if HIGH_PROFILES.contains(&s.profile) {
        w.ue(s.chroma_format);
        if s.chroma_format == 3 {
            w.flag(s.separate_colour_planes);
        }
        w.ue(s.bit_depth_luma_minus8);
        w.ue(s.bit_depth_chroma_minus8);
        w.flag(s.transform_bypass);
        w.flag(s.scaling.is_some());
        for list in s.scaling.iter().flatten() {
            w.flag(list.is_some());
            for &code in list.iter().flatten() {
                w.se(code);
            }
        }
    }
    w.ue(16 - 4); // log2_max_frame_num_minus4
    w.ue(0); // pic_order_cnt_type
    w.ue(16 - 4); // log2_max_pic_order_cnt_lsb_minus4
    w.ue(m.max_num_ref_frames);
    w.flag(false); // gaps_in_frame_num_value_allowed_flag
    w.ue(s.width_mbs - 1);
    w.ue(s.height_map_units - 1);
    w.flag(s.frame_mbs_only);
    if !s.frame_mbs_only {
        w.flag(s.mbaff);
    }
    w.flag(s.direct_8x8_inference);
    w.flag(s.crop.is_some());
    for &c in s.crop.iter().flatten() {
        w.ue(c);
    }
    let v = s.vui.clone().unwrap_or_default();
    w.flag(true); // vui_parameters_present_flag
    w.flag(v.aspect_ratio.is_some());
    if let Some((idc, sar)) = v.aspect_ratio {
        w.u(8, idc);
        if let Some((x, y)) = sar {
            w.u(16, x);
            w.u(16, y);
        }
    }
    w.flag(v.overscan_appropriate.is_some());
    if let Some(o) = v.overscan_appropriate {
        w.flag(o);
    }
    w.flag(v.signal.is_some());
    if let Some((format, full, colour)) = v.signal {
        w.u(3, format);
        w.flag(full);
        w.flag(colour.is_some());
        for c in colour.iter().flatten() {
            w.u(8, *c);
        }
    }
    w.flag(v.chroma_location.is_some());
    if let Some((top, bottom)) = v.chroma_location {
        w.ue(top);
        w.ue(bottom);
    }
    w.flag(v.timing.is_some());
    if let Some((tick, scale, fixed)) = v.timing {
        w.u(32, tick);
        w.u(32, scale.checked_mul(2).unwrap_or(scale));
        w.flag(fixed);
    }
    w.flag(false); // nal_hrd_parameters_present_flag
    w.flag(false); // vcl_hrd_parameters_present_flag
    w.flag(false); // pic_struct_present_flag
    w.flag(true); // bitstream_restriction_flag
    let r = v.restriction;
    w.flag(r.is_none_or(|r| r.mvs_over_pic_boundaries));
    w.ue(r.map_or(0, |r| r.max_bytes_per_pic_denom));
    w.ue(r.map_or(0, |r| r.max_bits_per_mb_denom));
    w.ue(r.map_or(15, |r| r.log2_max_mv_length[0]));
    w.ue(r.map_or(15, |r| r.log2_max_mv_length[1]));
    w.ue(m.max_num_reorder_frames);
    w.ue(m.max_dec_frame_buffering);
    w.trailing();
    w.into_bytes()
}

/// A picture parameter set (NAL unit type 8).
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Pps {
    pub id: u32,
    pub sps_id: u32,
    pub cabac: bool,
    pub bottom_field_pic_order: bool,
    pub num_ref_idx_default: [u32; 2],
    pub weighted_pred: bool,
    pub weighted_bipred_idc: u32,
    pub deblocking_control: bool,
    pub redundant_pic_cnt_present: bool,
    /// Where entropy_coding_mode_flag starts.
    rest: usize,
    rbsp: Vec<u8>,
}

pub(crate) fn parse_pps(rbsp: Vec<u8>) -> Result<Pps, Bad> {
    let mut r = Reader::new(&rbsp, 8);
    let id = r.ue()?;
    let sps_id = r.ue()?;
    let rest = r.pos();
    let cabac = r.flag()?;
    let bottom_field_pic_order = r.flag()?;
    if r.ue()? != 0 {
        return Err(Bad::Unsupported("slice groups".into()));
    }
    let num_ref_idx_default = [r.ue()? + 1, r.ue()? + 1];
    let weighted_pred = r.flag()?;
    let weighted_bipred_idc = r.u(2)?;
    r.se()?;
    r.se()?;
    r.se()?;
    let deblocking_control = r.flag()?;
    r.u(1)?; // constrained_intra_pred_flag
    let redundant_pic_cnt_present = r.flag()?;
    if id > 255 || sps_id > 31 || num_ref_idx_default.iter().any(|&n| n > 32) {
        return Err(Bad::Broken("PPS"));
    }
    Ok(Pps {
        id,
        sps_id,
        cabac,
        bottom_field_pic_order,
        num_ref_idx_default,
        weighted_pred,
        weighted_bipred_idc,
        deblocking_control,
        redundant_pic_cnt_present,
        rest,
        rbsp,
    })
}

impl Pps {
    /// This PPS as `id`, of SPS `sps_id`, with or without delta POC fields
    /// in its slice headers (see `rewrite`).
    pub(crate) fn write(&self, id: u32, sps_id: u32, bottom_field_pic_order: bool) -> Vec<u8> {
        let mut w = Writer::new();
        w.u(8, u32::from(self.rbsp[0]));
        w.ue(id);
        w.ue(sps_id);
        w.copy(&self.rbsp, self.rest, self.rest + 1);
        w.flag(bottom_field_pic_order);
        let end = stop_bit(&self.rbsp).unwrap_or(self.rest + 2);
        w.copy(&self.rbsp, self.rest + 2, end.max(self.rest + 2));
        w.trailing();
        w.into_bytes()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SliceKind {
    P,
    B,
    I,
}

/// A slice of a picture: its header, and where its parts are in the RBSP.
pub(crate) struct Slice {
    pub rbsp: Vec<u8>,
    pub pps: Arc<Pps>,
    pub first_mb: u32,
    pub slice_type: u32,
    pub kind: SliceKind,
    pub frame_num: u32,
    pub idr_pic_id: u32,
    pub poc_lsb: u32,
    pub delta_poc_bottom: i32,
    pub delta_poc: [i32; 2],
    pub redundant_pic_cnt: u32,
    pub direct_spatial: bool,
    /// num_ref_idx_active for lists 0 and 1 (0 for a list the slice lacks).
    pub num_ref_idx: [u32; 2],
    /// Bits of num_ref_idx_active_override_flag and what it overrides.
    pub override_bits: (usize, usize),
    /// modification_of_pic_nums_idc and its value, for each list.
    pub modifications: [Vec<(u32, u32)>; 2],
    /// Bits of pred_weight_table.
    pub pred_weights: (usize, usize),
    pub no_output_of_prior_pics: bool,
    pub long_term_reference: bool,
    /// The memory management operations of adaptive marking, None for the
    /// sliding window.
    pub mmco: Option<Vec<(u32, u32)>>,
    /// Where cabac_init_idc (or slice_qp_delta) starts.
    pub tail: usize,
    /// Where the slice data starts (CABAC: its alignment bits).
    pub end: usize,
}

/// first_mb_in_slice and pic_parameter_set_id of a slice.
pub(crate) fn peek_slice(rbsp: &[u8], header_bytes: usize) -> Result<(u32, u32), Bad> {
    let mut r = Reader::new(rbsp, header_bytes * 8);
    let first_mb = r.ue()?;
    r.ue()?;
    Ok((first_mb, r.ue()?))
}

/// A slice header (7.3.3), of a picture of the base view or (`mvc`) of the
/// second view.
pub(crate) fn parse_slice(
    rbsp: Vec<u8>,
    header_bytes: usize,
    idr: bool,
    ref_idc: u32,
    sps: &Sps,
    pps: Arc<Pps>,
    mvc: bool,
) -> Result<Slice, Bad> {
    let mut r = Reader::new(&rbsp, header_bytes * 8);
    let first_mb = r.ue()?;
    let slice_type = r.ue()?;
    let kind = match slice_type % 5 {
        0 => SliceKind::P,
        1 => SliceKind::B,
        2 => SliceKind::I,
        _ => return Err(Bad::Unsupported("SP and SI slices".into())),
    };
    r.ue()?; // pic_parameter_set_id
    if sps.separate_colour_planes {
        return Err(Bad::Unsupported("separate colour planes".into()));
    }
    let frame_num = r.u(sps.log2_max_frame_num)?;
    if !sps.frame_mbs_only {
        if r.flag()? {
            return Err(Bad::Unsupported("field pictures".into()));
        }
        if sps.mbaff {
            return Err(Bad::Unsupported("MBAFF".into()));
        }
    }
    let idr_pic_id = if idr { r.ue()? } else { 0 };
    let (mut poc_lsb, mut delta_poc_bottom, mut delta_poc) = (0, 0, [0, 0]);
    if sps.poc_type == 0 {
        poc_lsb = r.u(sps.log2_max_poc_lsb)?;
        if pps.bottom_field_pic_order {
            delta_poc_bottom = r.se()?;
        }
    } else if sps.poc_type == 1 && !sps.delta_pic_order_always_zero {
        delta_poc[0] = r.se()?;
        if pps.bottom_field_pic_order {
            delta_poc[1] = r.se()?;
        }
    }
    let redundant_pic_cnt = if pps.redundant_pic_cnt_present {
        r.ue()?
    } else {
        0
    };
    let direct_spatial = kind == SliceKind::B && r.flag()?;
    let mut num_ref_idx = match kind {
        SliceKind::P => [pps.num_ref_idx_default[0], 0],
        SliceKind::B => pps.num_ref_idx_default,
        SliceKind::I => [0, 0],
    };
    let at = r.pos();
    if kind != SliceKind::I && r.flag()? {
        num_ref_idx[0] = r.ue()? + 1;
        if kind == SliceKind::B {
            num_ref_idx[1] = r.ue()? + 1;
        }
    }
    let override_bits = (at, r.pos());
    if num_ref_idx.iter().any(|&n| n > 32) {
        return Err(Bad::Broken("reference count"));
    }
    let lists = match kind {
        SliceKind::P => 1,
        SliceKind::B => 2,
        SliceKind::I => 0,
    };
    let mut modifications = [Vec::new(), Vec::new()];
    for list in modifications.iter_mut().take(lists) {
        if r.flag()? {
            loop {
                let idc = r.ue()?;
                if idc == 3 {
                    break;
                }
                if idc > 5 || (idc > 3 && !mvc) || list.len() > 64 {
                    return Err(Bad::Broken("reference list modification"));
                }
                list.push((idc, r.ue()?));
            }
        }
    }
    let at = r.pos();
    if (pps.weighted_pred && kind == SliceKind::P)
        || (pps.weighted_bipred_idc == 1 && kind == SliceKind::B)
    {
        r.ue()?; // luma_log2_weight_denom
        let chroma = sps.chroma_format != 0;
        if chroma {
            r.ue()?;
        }
        for &n in num_ref_idx.iter().take(lists) {
            for _ in 0..n {
                if r.flag()? {
                    r.se()?;
                    r.se()?;
                }
                if chroma && r.flag()? {
                    for _ in 0..4 {
                        r.se()?;
                    }
                }
            }
        }
    }
    let pred_weights = (at, r.pos());
    if kind == SliceKind::B && pps.weighted_bipred_idc == 2 {
        return Err(Bad::Unsupported("implicit weighted prediction".into()));
    }
    if kind == SliceKind::B && !direct_spatial {
        return Err(Bad::Unsupported("temporal direct prediction".into()));
    }
    let (mut no_output_of_prior_pics, mut long_term_reference, mut mmco) = (false, false, None);
    if ref_idc != 0 {
        if idr {
            no_output_of_prior_pics = r.flag()?;
            long_term_reference = r.flag()?;
        } else if r.flag()? {
            let mut ops = Vec::new();
            loop {
                let op = r.ue()?;
                if op == 0 {
                    break;
                }
                let arg = match op {
                    1 | 2 | 4 | 6 => r.ue()?,
                    3 => {
                        let a = r.ue()?;
                        r.ue()?;
                        a
                    }
                    5 => 0,
                    _ => return Err(Bad::Broken("memory management operation")),
                };
                if ops.len() > 64 {
                    return Err(Bad::Broken("memory management operations"));
                }
                ops.push((op, arg));
            }
            mmco = Some(ops);
        }
    }
    let tail = r.pos();
    if pps.cabac && kind != SliceKind::I {
        r.ue()?; // cabac_init_idc
    }
    r.se()?; // slice_qp_delta
    if pps.deblocking_control && r.ue()? != 1 {
        r.se()?;
        r.se()?;
    }
    let end = r.pos();
    Ok(Slice {
        rbsp,
        pps,
        first_mb,
        slice_type,
        kind,
        frame_num,
        idr_pic_id,
        poc_lsb,
        delta_poc_bottom,
        delta_poc,
        redundant_pic_cnt,
        direct_spatial,
        num_ref_idx,
        override_bits,
        modifications,
        pred_weights,
        no_output_of_prior_pics,
        long_term_reference,
        mmco,
        tail,
        end,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A 1920x1080 High profile SPS (level 4.1, POC type 0) with a VUI.
    fn sps_1080p() -> Sps {
        Sps {
            id: 0,
            profile: 100,
            constraint: 0,
            level: 41,
            chroma_format: 1,
            separate_colour_planes: false,
            bit_depth_luma_minus8: 0,
            bit_depth_chroma_minus8: 0,
            transform_bypass: false,
            scaling: Some(vec![
                Some(vec![-3, 4, -9]),
                None,
                None,
                None,
                None,
                None,
                None,
                None,
            ]),
            log2_max_frame_num: 9,
            poc_type: 0,
            log2_max_poc_lsb: 5,
            delta_pic_order_always_zero: false,
            offset_for_non_ref_pic: 0,
            offset_for_top_to_bottom_field: 0,
            offsets_for_ref_frame: Vec::new(),
            max_num_ref_frames: 3,
            width_mbs: 120,
            height_map_units: 68,
            frame_mbs_only: true,
            mbaff: false,
            direct_8x8_inference: true,
            crop: Some([0, 0, 0, 4]),
            vui: Some(Vui {
                aspect_ratio: Some((1, None)),
                signal: Some((5, false, Some([1, 1, 1]))),
                timing: Some((1001, 48000, true)),
                ..Vui::default()
            }),
        }
    }

    #[test]
    fn the_merged_sps_keeps_the_picture_and_renumbers() {
        let s = sps_1080p();
        assert_eq!(s.reorder_frames(), 4, "inferred from level 4.1 at 1080p");
        let m = Merged {
            level: 51,
            max_num_ref_frames: 7,
            max_num_reorder_frames: 12,
            max_dec_frame_buffering: 16,
        };
        let nal = write_sps(&s, &m);
        let merged = parse_sps(&nal).unwrap();
        assert_eq!(merged.level, 51);
        assert_eq!((merged.log2_max_frame_num, merged.poc_type), (16, 0));
        assert_eq!(merged.log2_max_poc_lsb, 16);
        assert_eq!(merged.max_num_ref_frames, 7);
        assert!(merged.decodes_like(&s).is_ok());
        assert_eq!(merged.crop, s.crop);
        let v = merged.vui.unwrap();
        assert_eq!(v.timing, Some((1001, 96000, true)), "twice the frame rate");
        assert_eq!(v.signal, s.vui.as_ref().unwrap().signal);
        let r = v.restriction.unwrap();
        assert_eq!(
            (r.max_num_reorder_frames, r.max_dec_frame_buffering),
            (12, 16)
        );
    }

    #[test]
    fn a_pps_is_rewritten_with_its_settings() {
        // id 1 of SPS 1, CABAC, 2/1 references, deblocking control, 8x8.
        let mut w = Writer::new();
        w.u(8, 0x68);
        w.ue(1);
        w.ue(1);
        w.flag(true);
        w.flag(false);
        w.ue(0);
        w.ue(1);
        w.ue(0);
        w.flag(false);
        w.u(2, 0);
        w.se(0);
        w.se(0);
        w.se(-2);
        w.flag(true);
        w.flag(false);
        w.flag(false);
        w.flag(true); // transform_8x8_mode_flag
        w.flag(false);
        w.se(-2);
        w.trailing();
        let pps = parse_pps(w.into_bytes()).unwrap();
        assert_eq!((pps.id, pps.sps_id, pps.cabac), (1, 1, true));
        assert_eq!(pps.num_ref_idx_default, [2, 1]);
        let again = parse_pps(pps.write(31, 0, true)).unwrap();
        assert_eq!((again.id, again.sps_id), (31, 0));
        assert!(again.bottom_field_pic_order);
        assert_eq!(
            (
                again.cabac,
                again.num_ref_idx_default,
                again.deblocking_control
            ),
            (pps.cabac, pps.num_ref_idx_default, pps.deblocking_control)
        );
        let back = parse_pps(again.write(1, 1, false)).unwrap();
        assert_eq!(back.rbsp, pps.rbsp);
    }
}
