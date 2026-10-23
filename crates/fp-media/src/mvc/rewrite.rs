//! Rewrites two-view MVC, one access unit at a time, into frame-sequential
//! plain H.264 that decodes to the same pictures (see the `mvc` module).

use super::Bad;
use super::bits::{Writer, escape_into, stop_bit, unescape};
use super::syntax::{
    Merged, Pps, Slice, SliceKind, Sps, SubsetSps, parse_pps, parse_slice, parse_sps,
    parse_subset_sps, peek_slice, write_sps,
};
use std::collections::HashMap;
use std::sync::Arc;

/// Which picture of an access unit a merged picture is, in output order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Slot {
    /// The base view (the left eye on Blu-ray).
    Base,
    /// A copy of the base picture standing in for it as the second view's
    /// reference (see [`Rewriter::finish`]); decoded, never shown.
    Copy,
    /// The second view.
    Dependent,
}

impl Slot {
    fn offset(self) -> i32 {
        match self {
            Slot::Base => 0,
            Slot::Copy => 1,
            Slot::Dependent => 2,
        }
    }
}

/// One picture of the merged stream: its NAL units in Annex B, the
/// parameter sets it needs first.
pub(crate) struct Picture {
    pub slot: Slot,
    pub data: Vec<u8>,
    /// An IDR picture: decoding can start here.
    pub idr: bool,
}

/// PPS ids by the length of their ue(v) code, modulo 8 (1, 3, 5, 7 bits
/// and those plus 8): a CAVLC slice's data keeps the bit phase it had, as
/// its I_PCM samples are byte aligned.
const ID_CLASSES: [(u32, &[(u32, u32)]); 4] = [
    (1, &[(0, 0), (15, 30)]),
    (3, &[(1, 2), (31, 62)]),
    (5, &[(3, 6), (63, 126)]),
    (7, &[(7, 14), (127, 254)]),
];
/// The PPS of the copy pictures.
const COPY_PPS: u32 = 255;
/// The most PPS ids a stream may use (each takes eight in the merged one).
const MAX_PPS: u32 = 8;

/// The id of copy `variant` (0 or 1) of the `k`th PPS whose ue(v) code is
/// `residue` bits long, modulo 8.
fn clone_id(k: u32, residue: u32, variant: u32) -> u32 {
    let (_, ranges) = ID_CLASSES
        .iter()
        .find(|(r, _)| *r == residue)
        .expect("odd residue");
    let i = 2 * k + variant;
    let (a, b) = ranges[0];
    if i <= b - a {
        a + i
    } else {
        ranges[1].0 + i - (b - a + 1)
    }
}

type PicId = u64;

/// A decoded picture of either view, as the MVC decoding process and the
/// merged stream hold it.
struct Pic {
    view: usize,
    au: u64,
    frame_num: u32,
    poc: i32,
    poc_msb: i32,
    poc_lsb: i32,
    ref_idc: u32,
    idr: bool,
    anchor: bool,
    inter_view: bool,
    /// A short-term reference of its view.
    short_term: bool,
    /// Not decoded: before the start, or leading it.
    dropped: bool,
    /// Starts the merged stream as an IDR picture though it isn't one.
    force_idr: bool,
    /// Its frame_num in the merged stream, unwrapped.
    counter: u64,
    /// A long-term reference of the merged stream standing in for an
    /// inter-view reference.
    helper: bool,
}

#[derive(Default)]
struct View {
    refs: Vec<PicId>,
    seen: bool,
    prev_ref_frame_num: u32,
    prev_poc_msb: i32,
    prev_poc_lsb: i32,
    prev_frame_num_offset: i64,
    prev_frame_num: u32,
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Start {
    /// For an IDR picture, or an intra one to start as one.
    Waiting,
    /// Started on a non-IDR intra picture with this POC: pictures before it
    /// in output order may predict from what was never decoded.
    Leading(i32),
    Running,
}

/// One view's picture in the access unit being read.
struct Coded {
    id: PicId,
    /// The SPS of its view.
    sps: Arc<Sps>,
    slices: Vec<(Slice, [Vec<Option<PicId>>; 2])>,
    /// Parameter sets (Annex B) that came before it.
    before: Vec<u8>,
    /// The references of both views once it is marked.
    refs_after: Vec<PicId>,
    marked: bool,
}

/// A picture of the merged stream.
struct Out {
    id: PicId,
    idr: bool,
    reference: bool,
    poc: i32,
    ops: Vec<(u32, u32)>,
}

pub(crate) struct Rewriter {
    sps: HashMap<u32, Arc<Sps>>,
    subset: HashMap<u32, Arc<SubsetSps>>,
    pps: HashMap<u32, Arc<Pps>>,
    /// Each PPS id's place among those seen, for its copies' ids.
    pps_index: HashMap<u32, u32>,
    merged: Option<Merged>,
    /// Parameter sets (Annex B) to go ahead of the next picture.
    pending: Vec<u8>,
    /// inter_view_flag of the prefix NAL unit before the base view's slices.
    prefix_inter_view: Option<bool>,
    pics: HashMap<PicId, Pic>,
    next_id: PicId,
    views: [View; 2],
    start: Start,
    aus: u64,
    // The merged stream's references and numbering.
    short: Vec<PicId>,
    long: Option<PicId>,
    max_long_term_set: bool,
    last_ref: u64,
    idr_poc: i32,
}

impl Rewriter {
    pub(crate) fn new() -> Rewriter {
        Rewriter {
            sps: HashMap::new(),
            subset: HashMap::new(),
            pps: HashMap::new(),
            pps_index: HashMap::new(),
            merged: None,
            pending: Vec::new(),
            prefix_inter_view: None,
            pics: HashMap::new(),
            next_id: 0,
            views: Default::default(),
            start: Start::Waiting,
            aus: 0,
            short: Vec::new(),
            long: None,
            max_long_term_set: false,
            last_ref: 0,
            idr_poc: 0,
        }
    }

    /// Starts again at the next intra picture, as after a seek, with every
    /// parameter set sent again ahead of it.
    pub(crate) fn restart(&mut self) {
        self.start = Start::Waiting;
        self.views = Default::default();
        self.pics.clear();
        self.short.clear();
        self.long = None;
        self.max_long_term_set = false;
        self.prefix_inter_view = None;
        self.pending.clear();
        let mut ids: Vec<u32> = self.sps.keys().copied().collect();
        ids.sort_unstable();
        for id in ids {
            let s = self.sps[&id].clone();
            self.emit_sps(&s);
        }
        let mut ids: Vec<u32> = self.pps.keys().copied().collect();
        ids.sort_unstable();
        for id in ids {
            let p = self.pps[&id].clone();
            self.emit_pps(&p);
        }
    }

    fn emit(&mut self, rbsp: &[u8]) {
        self.pending.extend_from_slice(&[0, 0, 0, 1]);
        escape_into(rbsp, &mut self.pending);
    }

    /// A parameter set: SPS, MVC subset SPS or PPS (other units are left
    /// alone). Those of the container's configuration come this way first.
    pub(crate) fn parameter_set(&mut self, nal: &[u8]) -> Result<(), Bad> {
        let Some(&h) = nal.first() else {
            return Ok(());
        };
        match h & 0x1f {
            7 => {
                let s = parse_sps(&unescape(nal))?;
                for sub in self.subset.values() {
                    s.decodes_like(&sub.sps)?;
                }
                if self.sps.keys().any(|&id| id != s.id) {
                    return Err(Bad::Unsupported("several SPS".into()));
                }
                if self.sps.get(&s.id).is_none_or(|old| **old != s) {
                    self.merged = Some(self.merged_for(&s)?);
                }
                let s = Arc::new(s);
                self.sps.insert(s.id, s.clone());
                self.emit_sps(&s);
            }
            15 => {
                let s = parse_subset_sps(&unescape(nal))?;
                for base in self.sps.values() {
                    base.decodes_like(&s.sps)?;
                }
                self.subset.insert(s.sps.id, Arc::new(s));
            }
            8 => {
                let p = parse_pps(unescape(nal))?;
                let next = self.pps_index.len() as u32;
                let k = *self.pps_index.entry(p.id).or_insert(next);
                if k >= MAX_PPS {
                    return Err(Bad::Unsupported("more than 8 PPS".into()));
                }
                let p = Arc::new(p);
                self.pps.insert(p.id, p.clone());
                self.emit_pps(&p);
            }
            _ => {}
        }
        Ok(())
    }

    /// The merged SPS's numbers for base view SPS `s`: room for both views'
    /// references and the copy, three pictures (base, copy, second view)
    /// for each the base view reorders.
    fn merged_for(&self, s: &Sps) -> Result<Merged, Bad> {
        let base = s.max_num_ref_frames.max(1);
        let max_num_ref_frames = match self.subset.values().next() {
            Some(d) => (base + d.sps.max_num_ref_frames.max(1) + 1).min(16),
            None => 16,
        };
        let level = s.level.max(51);
        let dpb = (184_320 / s.frame_mbs().max(1)).min(16);
        if dpb < max_num_ref_frames {
            return Err(Bad::Unsupported("pictures too large for both views".into()));
        }
        Ok(Merged {
            level,
            max_num_ref_frames,
            max_num_reorder_frames: (3 * s.reorder_frames()).min(dpb),
            max_dec_frame_buffering: dpb,
        })
    }

    fn emit_sps(&mut self, s: &Sps) {
        let Some(m) = &self.merged else {
            return;
        };
        let sps = write_sps(s, m);
        self.emit(&sps);
        self.emit(&copy_pps(s.id));
    }

    fn emit_pps(&mut self, p: &Pps) {
        let k = self.pps_index[&p.id];
        let sps_id = self.sps.keys().next().copied().unwrap_or(p.sps_id);
        if p.cabac {
            let pps = p.write(clone_id(k, 1, 0), sps_id, false);
            self.emit(&pps);
        } else {
            for (residue, _) in ID_CLASSES {
                for variant in 0..2 {
                    let pps = p.write(clone_id(k, residue, variant), sps_id, variant == 1);
                    self.emit(&pps);
                }
            }
        }
    }

    /// One access unit (both views' NAL units, as a Blu-ray rip's packet
    /// holds them) as merged pictures: none while waiting for a picture to
    /// start at.
    pub(crate) fn access_unit<'a>(
        &mut self,
        nals: impl IntoIterator<Item = &'a [u8]>,
    ) -> Result<Vec<Picture>, Bad> {
        let nals: Vec<&[u8]> = nals.into_iter().collect();
        let r = self.rewrite(&nals);
        if let Err(Bad::Broken(_)) = r {
            // Its parameter sets still count (a stream cut mid-way can have
            // them after slices that came too early).
            for &nal in &nals {
                if nal.first().is_some_and(|h| matches!(h & 0x1f, 7 | 8 | 15))
                    && let Err(Bad::Unsupported(why)) = self.parameter_set(nal)
                {
                    return Err(Bad::Unsupported(why));
                }
            }
        }
        r
    }

    fn rewrite(&mut self, nals: &[&[u8]]) -> Result<Vec<Picture>, Bad> {
        let au = self.aus;
        self.aus += 1;
        let (mut base, mut dep): (Option<Coded>, Option<Coded>) = (None, None);
        for &nal in nals {
            let Some(&h) = nal.first() else {
                continue;
            };
            let ref_idc = u32::from(h >> 5 & 3);
            match h & 0x1f {
                7 | 8 | 15 => self.parameter_set(nal)?,
                14 if nal.len() >= 4 => self.prefix_inter_view = Some(nal[3] >> 1 & 1 == 1),
                t @ (1 | 5) => {
                    if dep.is_some() {
                        return Err(Bad::Broken("base view after the second"));
                    }
                    let iv = self.prefix_inter_view.take().unwrap_or(true);
                    self.slice(&mut base, nal, au, 0, t == 5, ref_idc, (t == 5, iv), None)?;
                }
                20 => {
                    let [e0, e1, e2, ..] = nal[1..] else {
                        return Err(Bad::Broken("NAL unit header extension"));
                    };
                    if e0 >> 7 != 0 {
                        return Err(Bad::Unsupported("SVC".into()));
                    }
                    let view_id = u32::from(e1) << 2 | u32::from(e2 >> 6);
                    let Some(b) = base.as_mut() else {
                        return Err(Bad::Broken("second view without the base"));
                    };
                    if !b.marked {
                        b.refs_after = self.mark(b)?;
                        b.marked = true;
                    }
                    let subset = self
                        .subset
                        .values()
                        .find(|s| s.view_ids.get(1) == Some(&view_id))
                        .cloned()
                        .ok_or(Bad::Unsupported(format!("view {view_id}")))?;
                    let flags = (e2 >> 2 & 1 == 1, e2 >> 1 & 1 == 1);
                    let base_id = b.id;
                    let idr = e0 >> 6 & 1 == 0;
                    self.slice(
                        &mut dep,
                        nal,
                        au,
                        1,
                        idr,
                        ref_idc,
                        flags,
                        Some((base_id, subset)),
                    )?;
                }
                _ => {}
            }
        }
        self.finish(base, dep)
    }

    /// A slice of view `view` (0 base, 1 second); `extra`: the base picture
    /// of the access unit and the second view's SPS.
    #[allow(clippy::too_many_arguments)]
    fn slice(
        &mut self,
        coded: &mut Option<Coded>,
        nal: &[u8],
        au: u64,
        view: usize,
        idr: bool,
        ref_idc: u32,
        (anchor, inter_view): (bool, bool),
        extra: Option<(PicId, Arc<SubsetSps>)>,
    ) -> Result<(), Bad> {
        let header_bytes = if view == 0 { 1 } else { 4 };
        let rbsp = unescape(nal);
        let (first_mb, pps_id) = peek_slice(&rbsp, header_bytes)?;
        let pps = self
            .pps
            .get(&pps_id)
            .cloned()
            .ok_or(Bad::Broken("no PPS yet"))?;
        let sps = match &extra {
            None => self.sps.get(&pps.sps_id).cloned(),
            Some((_, subset)) => Some(Arc::new(subset.sps.clone())),
        }
        .ok_or(Bad::Broken("no SPS yet"))?;
        let slice = parse_slice(rbsp, header_bytes, idr, ref_idc, &sps, pps, view == 1)?;
        if slice.redundant_pic_cnt > 0 {
            return Ok(());
        }
        if first_mb == 0 {
            if coded.is_some() {
                return Err(Bad::Broken("two pictures of a view in an access unit"));
            }
            let id = self.start_pic(view, au, &slice, &sps, idr, ref_idc, anchor, inter_view)?;
            if let Some((base, _)) = &extra {
                let b = &self.pics[base];
                if b.idr != idr {
                    return Err(Bad::Broken("IDR in one view only"));
                }
                let dropped = b.dropped;
                self.pics.get_mut(&id).expect("just added").dropped = dropped;
            } else {
                self.start_or_drop(id, &slice)?;
            }
            *coded = Some(Coded {
                id,
                sps: sps.clone(),
                slices: Vec::new(),
                before: std::mem::take(&mut self.pending),
                refs_after: Vec::new(),
                marked: false,
            });
        }
        let Some(c) = coded.as_mut() else {
            return Err(Bad::Broken("a picture without its first slice"));
        };
        let pic = &self.pics[&c.id];
        // Where the merged stream starts, nothing before is decoded: the
        // base picture must be intra, the second view's an anchor.
        let starts = pic.force_idr || extra.as_ref().is_some_and(|(b, _)| self.pics[b].force_idr);
        if starts && slice.kind != SliceKind::I && (pic.view == 0 || !pic.anchor) {
            return Err(Bad::Broken("no intra picture to start at"));
        }
        let lists = if pic.dropped {
            [Vec::new(), Vec::new()]
        } else {
            self.ref_lists(c.id, &slice, &sps, extra.as_ref())?
        };
        c.slices.push((slice, lists));
        Ok(())
    }

    /// Frame numbers and picture order count of a view's new picture (8.2.1).
    #[allow(clippy::too_many_arguments)]
    fn start_pic(
        &mut self,
        view: usize,
        au: u64,
        slice: &Slice,
        sps: &Sps,
        idr: bool,
        ref_idc: u32,
        anchor: bool,
        inter_view: bool,
    ) -> Result<PicId, Bad> {
        let v = &mut self.views[view];
        let max_fn = 1u32 << sps.log2_max_frame_num;
        let fnum = slice.frame_num;
        if idr {
            v.prev_ref_frame_num = 0;
        } else if v.seen
            && fnum != v.prev_ref_frame_num
            && fnum != (v.prev_ref_frame_num + 1) % max_fn
        {
            return Err(Bad::Broken("frame_num gap"));
        }
        v.seen = true;
        let (mut poc_msb, poc_lsb) = (0, slice.poc_lsb as i32);
        let (top, bottom) = if sps.poc_type == 0 {
            if idr {
                (v.prev_poc_msb, v.prev_poc_lsb) = (0, 0);
            }
            let max_lsb = 1i32 << sps.log2_max_poc_lsb;
            poc_msb = if poc_lsb < v.prev_poc_lsb && v.prev_poc_lsb - poc_lsb >= max_lsb / 2 {
                v.prev_poc_msb + max_lsb
            } else if poc_lsb > v.prev_poc_lsb && poc_lsb - v.prev_poc_lsb > max_lsb / 2 {
                v.prev_poc_msb - max_lsb
            } else {
                v.prev_poc_msb
            };
            let top = poc_msb + poc_lsb;
            (top, top + slice.delta_poc_bottom)
        } else {
            let offset = if idr {
                0
            } else if v.prev_frame_num > fnum {
                v.prev_frame_num_offset + i64::from(max_fn)
            } else {
                v.prev_frame_num_offset
            };
            v.prev_frame_num_offset = offset;
            v.prev_frame_num = fnum;
            let abs = offset + i64::from(fnum);
            if sps.poc_type == 1 {
                let cycle = &sps.offsets_for_ref_frame;
                let mut n = if cycle.is_empty() { 0 } else { abs };
                if ref_idc == 0 && n > 0 {
                    n -= 1;
                }
                let mut expected = 0i64;
                if n > 0 {
                    let len = cycle.len() as i64;
                    let per_cycle: i64 = cycle.iter().map(|&o| i64::from(o)).sum();
                    let (cycles, within) = ((n - 1) / len, (n - 1) % len);
                    expected = cycles * per_cycle
                        + cycle[..=within as usize]
                            .iter()
                            .map(|&o| i64::from(o))
                            .sum::<i64>();
                }
                if ref_idc == 0 {
                    expected += i64::from(sps.offset_for_non_ref_pic);
                }
                let top = expected + i64::from(slice.delta_poc[0]);
                let bottom = top
                    + i64::from(sps.offset_for_top_to_bottom_field)
                    + i64::from(slice.delta_poc[1]);
                (top as i32, bottom as i32)
            } else {
                let t = if idr {
                    0
                } else if ref_idc == 0 {
                    2 * abs - 1
                } else {
                    2 * abs
                };
                (t as i32, t as i32)
            }
        };
        let id = self.next_id;
        self.next_id += 1;
        self.pics.insert(
            id,
            Pic {
                view,
                au,
                frame_num: fnum,
                poc: top.min(bottom),
                poc_msb,
                poc_lsb,
                ref_idc,
                idr,
                anchor,
                inter_view,
                short_term: false,
                dropped: false,
                force_idr: false,
                counter: 0,
                helper: false,
            },
        );
        Ok(id)
    }

    /// Whether the base view's new picture starts the stream, or is
    /// dropped before or leading the start.
    fn start_or_drop(&mut self, id: PicId, slice: &Slice) -> Result<(), Bad> {
        let pic = self.pics.get_mut(&id).expect("just added");
        match self.start {
            Start::Waiting if pic.idr => self.start = Start::Running,
            Start::Waiting if slice.kind == SliceKind::I && pic.ref_idc != 0 => {
                pic.force_idr = true;
                self.start = Start::Leading(pic.poc);
            }
            Start::Waiting => pic.dropped = true,
            Start::Leading(poc) if pic.poc < poc => pic.dropped = true,
            Start::Leading(_) => self.start = Start::Running,
            Start::Running => {}
        }
        Ok(())
    }

    /// The final reference picture lists of a slice (8.2.4 with H.8.2): the
    /// pictures it predicts from, None where a list has no picture.
    fn ref_lists(
        &self,
        id: PicId,
        slice: &Slice,
        sps: &Sps,
        extra: Option<&(PicId, Arc<SubsetSps>)>,
    ) -> Result<[Vec<Option<PicId>>; 2], Bad> {
        let mut out = [Vec::new(), Vec::new()];
        if slice.kind == SliceKind::I {
            return Ok(out);
        }
        let pic = &self.pics[&id];
        let max_fn = 1i64 << sps.log2_max_frame_num;
        let cur = i64::from(pic.frame_num);
        let pic_num = |p: PicId| {
            let f = i64::from(self.pics[&p].frame_num);
            if f > cur { f - max_fn } else { f }
        };
        let short: Vec<PicId> = if pic.idr {
            Vec::new()
        } else {
            self.views[pic.view]
                .refs
                .iter()
                .copied()
                .filter(|p| self.pics[p].short_term)
                .collect()
        };
        let poc = |p: &PicId| self.pics[p].poc;
        let mut init = [Vec::new(), Vec::new()];
        if slice.kind == SliceKind::P {
            let mut l0 = short.clone();
            l0.sort_by_key(|&p| std::cmp::Reverse(pic_num(p)));
            init[0] = l0;
        } else {
            let mut before: Vec<PicId> =
                short.iter().copied().filter(|p| poc(p) < pic.poc).collect();
            before.sort_by_key(|p| std::cmp::Reverse(poc(p)));
            let mut after: Vec<PicId> =
                short.iter().copied().filter(|p| poc(p) > pic.poc).collect();
            after.sort_by_key(poc);
            let l0: Vec<PicId> = before.iter().chain(&after).copied().collect();
            let mut l1: Vec<PicId> = after.iter().chain(&before).copied().collect();
            if l1.len() > 1 && l1 == l0 {
                l1.swap(0, 1);
            }
            init = [l0, l1];
        }
        let mut inter = [Vec::new(), Vec::new()];
        if let Some((base, subset)) = extra {
            let refs = if pic.anchor {
                &subset.anchor_refs
            } else {
                &subset.non_anchor_refs
            };
            for l in 0..2 {
                if refs[l].contains(&subset.view_ids[0]) && self.pics[base].inter_view {
                    inter[l].push(*base);
                }
            }
        }
        let lists = if slice.kind == SliceKind::B { 2 } else { 1 };
        for l in 0..lists {
            let n = slice.num_ref_idx[l] as usize;
            let mut list: Vec<Option<PicId>> = init[l]
                .iter()
                .chain(&inter[l])
                .map(|&p| Some(p))
                .take(n)
                .collect();
            list.resize(n, None);
            let mut pred = cur;
            let mut view_pred = -1i64;
            for (i, &(idc, value)) in slice.modifications[l].iter().enumerate() {
                let target = match idc {
                    0 | 1 => {
                        let step = i64::from(value) + 1;
                        pred =
                            (if idc == 0 { pred - step } else { pred + step }).rem_euclid(max_fn);
                        let num = if pred > cur { pred - max_fn } else { pred };
                        short.iter().copied().find(|&p| pic_num(p) == num)
                    }
                    4 | 5 => {
                        let k = inter[l].len() as i64;
                        if k == 0 {
                            None
                        } else {
                            let step = i64::from(value) + 1;
                            view_pred = (if idc == 4 {
                                view_pred - step
                            } else {
                                view_pred + step
                            })
                            .rem_euclid(k);
                            Some(inter[l][view_pred as usize])
                        }
                    }
                    _ => return Err(Bad::Unsupported("long-term references".into())),
                };
                let target = target.ok_or(Bad::Broken("a reference that was not decoded"))?;
                if i >= n {
                    return Err(Bad::Broken("reference list modification"));
                }
                let rest: Vec<Option<PicId>> = list[i..]
                    .iter()
                    .copied()
                    .filter(|&p| p != Some(target))
                    .collect();
                list.truncate(i);
                list.push(Some(target));
                list.extend(rest);
                list.resize(n, None);
            }
            out[l] = list;
        }
        Ok(out)
    }

    /// Marks a decoded picture as its view's reference (8.2.5, per view as
    /// H.8.3 does), returning the references both views then hold.
    fn mark(&mut self, c: &Coded) -> Result<Vec<PicId>, Bad> {
        let (slice, _) = c.slices.first().ok_or(Bad::Broken("empty picture"))?;
        let pic = &self.pics[&c.id];
        let (view, idr, ref_idc, dropped, fnum) =
            (pic.view, pic.idr, pic.ref_idc, pic.dropped, pic.frame_num);
        let sps = &c.sps;
        if ref_idc != 0 {
            let max_fn = 1i64 << sps.log2_max_frame_num;
            let pic_num = |f: u32| {
                let f = i64::from(f);
                if f > i64::from(fnum) { f - max_fn } else { f }
            };
            let refs = self.views[view].refs.clone();
            if idr {
                if slice.long_term_reference {
                    return Err(Bad::Unsupported("long-term references".into()));
                }
                for p in &refs {
                    self.pics.get_mut(p).expect("held").short_term = false;
                }
            } else if let Some(ops) = &slice.mmco {
                for &(op, arg) in ops {
                    if op != 1 {
                        return Err(Bad::Unsupported(format!(
                            "memory management operation {op}"
                        )));
                    }
                    let num = i64::from(fnum) - (i64::from(arg) + 1);
                    for p in &refs {
                        let q = self.pics.get_mut(p).expect("held");
                        if q.short_term && pic_num(q.frame_num) == num {
                            q.short_term = false;
                        }
                    }
                }
            } else {
                let held: Vec<PicId> = refs
                    .iter()
                    .copied()
                    .filter(|p| self.pics[p].short_term)
                    .collect();
                if held.len() >= sps.max_num_ref_frames.max(1) as usize
                    && let Some(&oldest) = held
                        .iter()
                        .min_by_key(|&&p| pic_num(self.pics[&p].frame_num))
                {
                    self.pics.get_mut(&oldest).expect("held").short_term = false;
                }
            }
            let v = &mut self.views[view];
            v.refs.retain(|p| self.pics[p].short_term);
            if !dropped {
                v.refs.push(c.id);
            }
            let pic = self.pics.get_mut(&c.id).expect("decoded");
            pic.short_term = !dropped;
            v.prev_ref_frame_num = pic.frame_num;
            if sps.poc_type == 0 {
                (v.prev_poc_msb, v.prev_poc_lsb) = (pic.poc_msb, pic.poc_lsb);
            }
        }
        Ok(self
            .views
            .iter()
            .flat_map(|v| v.refs.iter().copied())
            .collect())
    }

    /// Ends an access unit: marks its pictures, decides how the second
    /// view's inter-view reference is held, numbers and marks the merged
    /// pictures and writes them.
    fn finish(&mut self, base: Option<Coded>, dep: Option<Coded>) -> Result<Vec<Picture>, Bad> {
        let Some(mut base) = base else {
            if dep.is_some() {
                return Err(Bad::Broken("second view without the base"));
            }
            return Ok(Vec::new());
        };
        if !base.marked {
            base.refs_after = self.mark(&base)?;
        }
        let mut dep = dep;
        if let Some(d) = dep.as_mut() {
            d.refs_after = self.mark(d)?;
        }
        let out = if self.pics[&base.id].dropped {
            // Their parameter sets still go ahead of the next picture.
            let mut before = std::mem::take(&mut base.before);
            if let Some(d) = dep.as_mut() {
                before.append(&mut d.before);
            }
            before.append(&mut self.pending);
            self.pending = before;
            Vec::new()
        } else {
            self.merge(&base, dep.as_mut())?
        };
        let keep: Vec<PicId> = self
            .views
            .iter()
            .flat_map(|v| v.refs.iter().copied())
            .collect();
        self.pics
            .retain(|p, _| keep.contains(p) || self.short.contains(p) || self.long == Some(*p));
        Ok(out)
    }

    fn merge(&mut self, base: &Coded, mut dep: Option<&mut Coded>) -> Result<Vec<Picture>, Bad> {
        let b = base.id;
        let mut order = vec![(b, Slot::Base, base.refs_after.clone())];
        if let Some(d) = dep.as_deref_mut() {
            let used = d
                .slices
                .iter()
                .any(|(_, lists)| lists.iter().flatten().any(|&p| p == Some(b)));
            let colocated = d
                .slices
                .iter()
                .any(|(s, lists)| s.kind == SliceKind::B && lists[1].first() == Some(&Some(b)));
            if used && self.pics[&b].ref_idc == 0 {
                // A non-reference base picture the second view predicts
                // from: a long-term reference until that is decoded.
                self.pics.get_mut(&b).expect("decoded").helper = true;
            } else if colocated {
                // colZeroFlag (8.4.1.2.2) needs RefPicList1[0] to be a
                // short-term reference, which an inter-view one isn't
                // (H.8.4): a long-term copy of the base picture, all
                // skipped macroblocks, stands in for it.
                let id = self.next_id;
                self.next_id += 1;
                let bp = &self.pics[&b];
                let copy = Pic {
                    au: bp.au,
                    poc: bp.poc,
                    ref_idc: 0,
                    idr: false,
                    helper: true,
                    ..*bp
                };
                self.pics.insert(id, copy);
                for (_, lists) in &mut d.slices {
                    for p in lists.iter_mut().flatten() {
                        if *p == Some(b) {
                            *p = Some(id);
                        }
                    }
                }
                order.push((id, Slot::Copy, base.refs_after.clone()));
            }
            order.push((d.id, Slot::Dependent, d.refs_after.clone()));
        }
        let mut pictures = Vec::new();
        for (id, slot, keep) in order {
            let coded = match slot {
                Slot::Base => Some(base),
                Slot::Copy => None,
                Slot::Dependent => dep.as_deref(),
            };
            for (_, lists) in coded.iter().flat_map(|c| &c.slices) {
                self.check_held(lists)?;
            }
            let m = self.number(id, slot, &keep, self.pics[&b].poc)?;
            let mut data = Vec::new();
            match coded {
                Some(c) => {
                    data.extend_from_slice(&c.before);
                    for (slice, lists) in &c.slices {
                        self.write_slice(&mut data, &m, slice, lists)?;
                    }
                }
                None => self.write_copy(&mut data, &m, b)?,
            }
            pictures.push(Picture {
                slot,
                data,
                idr: m.idr,
            });
        }
        Ok(pictures)
    }

    /// Whether the merged stream holds every picture a slice predicts from,
    /// as it must when the slice is decoded.
    fn check_held(&self, lists: &[Vec<Option<PicId>>; 2]) -> Result<(), Bad> {
        for p in lists.iter().flatten().flatten() {
            if !(self.short.contains(p) || self.long == Some(*p)) {
                return Err(Bad::Broken("a reference the merged stream lacks"));
            }
        }
        Ok(())
    }

    /// Numbers a merged picture (frame_num, and a POC from its access unit's
    /// base picture's, `base_poc`) and works out the marking that keeps the
    /// merged stream's references those of both views (with the stand-in for
    /// the inter-view reference).
    fn number(&mut self, id: PicId, slot: Slot, keep: &[PicId], base_poc: i32) -> Result<Out, Bad> {
        let (idr, reference, helper, au) = {
            let p = &self.pics[&id];
            (
                slot == Slot::Base && (p.idr || p.force_idr),
                p.ref_idc != 0 || p.helper,
                p.helper,
                p.au,
            )
        };
        if idr {
            self.idr_poc = base_poc;
        }
        let poc = 4 * (base_poc - self.idr_poc) + slot.offset();
        let mut ops = Vec::new();
        if idr {
            self.short.clear();
            self.long = None;
            self.max_long_term_set = false;
            self.last_ref = 0;
            self.short.push(id);
            self.pics.get_mut(&id).expect("decoded").counter = 0;
        } else {
            let counter = self.last_ref + 1;
            self.pics.get_mut(&id).expect("decoded").counter = counter;
            if reference {
                self.last_ref = counter;
                let mut i = 0;
                while i < self.short.len() {
                    let p = self.short[i];
                    if keep.contains(&p) {
                        i += 1;
                    } else {
                        ops.push((1, (counter - self.pics[&p].counter - 1) as u32));
                        self.short.remove(i);
                    }
                }
                if let Some(l) = self.long
                    && (self.pics.get(&l).is_none_or(|p| p.au != au) || slot == Slot::Dependent)
                {
                    ops.push((2, 0));
                    self.long = None;
                }
                if helper {
                    if !self.max_long_term_set {
                        ops.push((4, 1));
                        self.max_long_term_set = true;
                    }
                    ops.push((6, 0));
                    self.long = Some(id);
                } else {
                    self.short.push(id);
                }
            }
        }
        let held = self.short.len() + usize::from(self.long.is_some());
        let declared = self.merged.as_ref().map_or(16, |m| m.max_num_ref_frames) as usize;
        if held > declared {
            return Err(Bad::Unsupported(
                "more references than both views declare".into(),
            ));
        }
        Ok(Out {
            id,
            idr,
            reference,
            poc,
            ops,
        })
    }
}

impl Rewriter {
    /// A slice of the merged stream: its header rewritten (numbering, every
    /// reference list spelled out, marking), its data as it was.
    fn write_slice(
        &self,
        out: &mut Vec<u8>,
        m: &Out,
        slice: &Slice,
        lists: &[Vec<Option<PicId>>; 2],
    ) -> Result<(), Bad> {
        let pic = &self.pics[&m.id];
        let ref_idc = if m.reference { pic.ref_idc.max(1) } else { 0 };
        let mut head = Writer::new();
        head.u(8, ref_idc << 5 | if m.idr { 5 } else { 1 });
        head.ue(slice.first_mb);
        head.ue(slice.slice_type);
        let mut mid = Writer::new();
        mid.u(16, (pic.counter % 65536) as u32);
        if m.idr {
            mid.ue(slice.idr_pic_id);
        }
        mid.u(16, m.poc.rem_euclid(65536) as u32);
        let mut rest = Writer::new();
        if slice.pps.redundant_pic_cnt_present {
            rest.ue(0);
        }
        if slice.kind == SliceKind::B {
            rest.flag(slice.direct_spatial);
        }
        rest.copy(&slice.rbsp, slice.override_bits.0, slice.override_bits.1);
        self.write_lists(&mut rest, pic.counter, slice.kind, lists);
        rest.copy(&slice.rbsp, slice.pred_weights.0, slice.pred_weights.1);
        if m.reference {
            write_marking(&mut rest, m, slice.no_output_of_prior_pics);
        }
        rest.copy(&slice.rbsp, slice.tail, slice.end);
        let k = self.pps_index[&slice.pps.id];
        let mut w = Writer::new();
        w.append(&head);
        if slice.pps.cabac {
            w.ue(clone_id(k, 1, 0));
            w.append(&mid);
            w.append(&rest);
            w.align_ones();
            w.extend(&slice.rbsp[slice.end.div_ceil(8)..]);
        } else {
            // The PPS id's code (and a delta_pic_order_cnt_bottom of 0 when
            // that makes it fit) brings the data back to its bit phase.
            let fixed = (head.len() + mid.len() + rest.len()) as i64;
            let need = (slice.end as i64 - fixed).rem_euclid(8) as u32;
            let (variant, residue) = if need.is_multiple_of(2) {
                (1, (need + 7) % 8)
            } else {
                (0, need)
            };
            w.ue(clone_id(k, residue, variant));
            w.append(&mid);
            if variant == 1 {
                w.se(0);
            }
            w.append(&rest);
            debug_assert_eq!(w.len() % 8, slice.end % 8);
            let stop = stop_bit(&slice.rbsp).ok_or(Bad::Broken("slice without data"))?;
            w.copy(&slice.rbsp, slice.end, stop.max(slice.end));
            w.trailing();
        }
        out.extend_from_slice(&[0, 0, 0, 1]);
        escape_into(&w.into_bytes(), out);
        Ok(())
    }

    /// ref_pic_list_modification spelling out every entry: short-term
    /// pictures by their frame number, the stand-in as long-term 0.
    fn write_lists(
        &self,
        w: &mut Writer,
        counter: u64,
        kind: SliceKind,
        lists: &[Vec<Option<PicId>>; 2],
    ) {
        let n = match kind {
            SliceKind::P => 1,
            SliceKind::B => 2,
            SliceKind::I => 0,
        };
        for list in lists.iter().take(n) {
            let Some(fill) = list.iter().flatten().next().copied() else {
                w.flag(false);
                continue;
            };
            w.flag(true);
            let mut pred = (counter % 65536) as i64;
            for p in list {
                let q = &self.pics[&p.unwrap_or(fill)];
                if q.helper {
                    w.ue(2);
                    w.ue(0);
                } else {
                    let f = (q.counter % 65536) as i64;
                    let d = match (pred - f).rem_euclid(65536) {
                        0 => 65536,
                        d => d,
                    };
                    w.ue(0);
                    w.ue((d - 1) as u32);
                    pred = f;
                }
            }
            w.ue(3);
        }
    }

    /// A copy picture: P, every macroblock skipped from the base picture
    /// decoded just before, no deblocking, so exactly that picture.
    fn write_copy(&self, out: &mut Vec<u8>, m: &Out, base: PicId) -> Result<(), Bad> {
        let mbs = self
            .sps
            .values()
            .next()
            .ok_or(Bad::Broken("no SPS"))?
            .frame_mbs();
        let counter = self.pics[&m.id].counter;
        let mut w = Writer::new();
        w.u(8, 1 << 5 | 1);
        w.ue(0);
        w.ue(5); // P, as every slice of the picture
        w.ue(COPY_PPS);
        w.u(16, (counter % 65536) as u32);
        w.u(16, m.poc.rem_euclid(65536) as u32);
        w.flag(false); // num_ref_idx_active_override_flag
        w.flag(true);
        w.ue(0);
        w.ue((counter - self.pics[&base].counter - 1) as u32);
        w.ue(3);
        write_marking(&mut w, m, false);
        w.se(0); // slice_qp_delta
        w.ue(1); // disable_deblocking_filter_idc
        w.ue(mbs); // mb_skip_run
        w.trailing();
        out.extend_from_slice(&[0, 0, 0, 1]);
        escape_into(&w.into_bytes(), out);
        Ok(())
    }
}

/// dec_ref_pic_marking of a merged reference picture.
fn write_marking(w: &mut Writer, m: &Out, no_output_of_prior_pics: bool) {
    if m.idr {
        w.flag(no_output_of_prior_pics);
        w.flag(false);
    } else {
        w.flag(!m.ops.is_empty());
        for &(op, arg) in &m.ops {
            w.ue(op);
            w.ue(arg);
        }
        if !m.ops.is_empty() {
            w.ue(0);
        }
    }
}

/// The PPS of the copy pictures: CAVLC, one reference, deblocking off by
/// slice.
fn copy_pps(sps_id: u32) -> Vec<u8> {
    let mut w = Writer::new();
    w.u(8, 0x68);
    w.ue(COPY_PPS);
    w.ue(sps_id);
    w.flag(false); // entropy_coding_mode_flag
    w.flag(false);
    w.ue(0); // num_slice_groups_minus1
    w.ue(0); // one reference
    w.ue(0);
    w.flag(false);
    w.u(2, 0);
    w.se(0);
    w.se(0);
    w.se(0);
    w.flag(true); // deblocking_filter_control_present_flag
    w.flag(false);
    w.flag(false);
    w.trailing();
    w.into_bytes()
}
