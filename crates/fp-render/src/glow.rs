//! Screen glow: the background round a flat screen lit by the colours at the
//! picture's edges, as a backlit television lights the wall behind it.
//!
//! It works as HyperHDR drives an LED strip: a colour for each of [`ZONES`]
//! strips round the picture's edge, averaged in linear light, sampled inside
//! any letterbox or pillarbox bars its black border detector finds, and
//! eased towards at 50 Hz over about 150 ms. A pass drawn with the eye
//! passes' own bindings (`fs_glow_colours`) measures the colours and the
//! bars; they come back once the frame slot does. The eye passes with the
//! glow are a pipeline of their own (`fs_main_glow`), made on first use, so
//! without it they do no more work than before.

use crate::Result;
use crate::gpu::Gpu;
use crate::mem::{self, Buffer, Image};
use crate::params::SceneParams;
use crate::pipeline::{self, Depth, PipelineDesc, ShadingRate};
use ash::vk;
use glam::Vec4;
use gpu_allocator::MemoryLocation;
use std::time::{Duration, Instant};

/// Colours measured going round the picture: 20 along the top and the
/// bottom and 12 down each side (`GLOW_ZONES` in scene.wgsl).
pub(crate) const ZONES: usize = 64;
/// The colours the eye passes get: softened a little, then spread further,
/// half and a quarter as many.
pub(crate) const COLOURS: usize = ZONES + ZONES / 2 + ZONES / 4;
/// After the colours, the target holds the bars found at the top, bottom,
/// left and right.
const TEXELS: usize = ZONES + 4;
const FORMAT: vk::Format = vk::Format::R8G8B8A8_UNORM;
/// HyperHDR's default update rate, 50 Hz.
const MEASURE_EVERY: Duration = Duration::from_millis(20);
/// The glow moves about two thirds of the way to new colours in this time,
/// HyperHDR's default smoothing time.
const EASE_SECS: f32 = 0.15;
/// At full brightness, the glow beside the edge is as bright as the edge.
const MAX_BRIGHTNESS: f32 = 1.0;
/// At full spread, it fades out over this share of the screen's longer side.
const MAX_SPREAD: f32 = 0.55;
/// How far each strip's light has spread over its neighbours beside the
/// screen, halfway out and at the edge of the glow's reach: a Gaussian's
/// sigma, in zones. Even beside the screen it is softened a little, as an
/// LED's light is, so the eye passes can blend neighbours in a straight line
/// without a crease showing between them.
const SPREAD_ZONES: [f32; 3] = [1.0, 2.0, 6.0];
/// The deepest bar searched for, a share of the picture (`GLOW_BAR_MAX`).
const BAR_MAX: f32 = 0.25;
/// Bars on opposite sides closer than this are one letterbox or pillarbox.
const BAR_MATCH: f32 = 0.01;
/// A new size of bars is taken once measured this many times in a row,
/// HyperHDR's default (`borderFrameCnt`): a second at 50 Hz.
const BAR_HOLD: u32 = 50;

/// How the glow looks: its brightness and spread, each a share of its most.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ScreenGlow {
    pub brightness: f32,
    pub spread: f32,
}

type Palette = [Vec4; ZONES];

/// A linear colour as the eye passes read it (`glow_colour`): the square
/// roots of red, green and blue in 8 bits each, which keeps steps between
/// dark colours small.
fn pack(c: Vec4) -> u32 {
    let byte = |v: f32| (v.clamp(0.0, 1.0).sqrt() * 255.0).round() as u32;
    byte(c.x) | byte(c.y) << 8 | byte(c.z) << 16 | 255 << 24
}

/// Each `n` neighbouring colours of `palette` as their mean.
fn fewer(palette: &Palette, n: usize) -> impl Iterator<Item = Vec4> + '_ {
    palette
        .chunks(n)
        .map(move |c| c.iter().copied().sum::<Vec4>() / n as f32)
}

/// `palette` blurred round the picture by a Gaussian `sigma` zones wide.
fn spread_out(palette: &Palette, sigma: f32) -> Palette {
    let reach = (3.0 * sigma).ceil() as isize;
    let weight = |k: isize| (-(k * k) as f32 / (2.0 * sigma * sigma)).exp();
    let total: f32 = (-reach..=reach).map(weight).sum();
    std::array::from_fn(|i| {
        (-reach..=reach)
            .map(|k| palette[(i as isize + k).rem_euclid(ZONES as isize) as usize] * weight(k))
            .sum::<Vec4>()
            / total
    })
}

/// The letterbox or pillarbox bars the colours are sampled inside: a pair on
/// opposite sides the same size, so a dark sky or a dark side of a scene
/// isn't taken for one.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct Bars {
    /// The bars at the left and right, and at the top and bottom, shares of
    /// the picture.
    shown: [f32; 2],
    candidate: [f32; 2],
    seen: u32,
}

impl Bars {
    /// Takes a measurement: how deep the dark rows run in from the top,
    /// bottom, left and right.
    fn measured(&mut self, [top, bottom, left, right]: [f32; 4]) {
        // A black frame says nothing about the bars.
        if top >= BAR_MAX && left >= BAR_MAX {
            return;
        }
        let pair = |a: f32, b: f32| {
            if (a - b).abs() <= BAR_MATCH {
                a.min(b)
            } else {
                0.0
            }
        };
        let now = [pair(left, right), pair(top, bottom)];
        if now
            .iter()
            .zip(self.candidate)
            .all(|(a, b)| (a - b).abs() <= BAR_MATCH)
        {
            self.seen += 1;
        } else {
            self.candidate = now;
            self.seen = 1;
        }
        if self.seen >= BAR_HOLD {
            self.shown = self.candidate;
        }
    }
}

pub(crate) struct Glow {
    look: Option<ScreenGlow>,
    pass: Option<GlowPass>,
    /// Making the pass failed; it isn't tried again.
    failed: bool,
    measured_at: Option<Instant>,
    target: Option<Palette>,
    shown: Palette,
    eased_at: Option<Instant>,
    bars: Bars,
}

impl Default for Glow {
    fn default() -> Self {
        Glow {
            look: None,
            pass: None,
            failed: false,
            measured_at: None,
            target: None,
            shown: [Vec4::ZERO; ZONES],
            eased_at: None,
            bars: Bars::default(),
        }
    }
}

impl Glow {
    pub fn set(&mut self, look: Option<ScreenGlow>) {
        self.look = look;
        if look.is_none() {
            self.reset();
        }
    }

    pub fn wanted(&self) -> bool {
        self.look.is_some()
    }

    /// Forgets the colours, for a new video or while the glow can't show.
    pub fn reset(&mut self) {
        self.measured_at = None;
        self.target = None;
        self.eased_at = None;
        self.bars = Bars::default();
        if let Some(pass) = self.pass.as_mut() {
            for slot in &mut pass.slots {
                slot.pending = false;
            }
        }
    }

    /// Whether the pass is ready, making it on first use with the eye
    /// passes' layouts: the planar one and, where 10-bit frames are read in
    /// place, theirs.
    pub fn prepare(
        &mut self,
        gpu: &Gpu,
        color_format: vk::Format,
        shading_rate: ShadingRate,
        layouts: (vk::PipelineLayout, Option<vk::PipelineLayout>),
        slots: usize,
    ) -> bool {
        if self.pass.is_none() && !self.failed {
            match GlowPass::new(gpu, color_format, shading_rate, layouts, slots) {
                Ok(pass) => self.pass = Some(pass),
                Err(e) => {
                    log::warn!("screen glow unavailable: {e}");
                    self.failed = true;
                }
            }
        }
        self.pass.is_some()
    }

    /// Takes the colours and bars measured in `slot`, once its fence has
    /// signalled.
    pub fn read(&mut self, slot: usize) {
        let Some((palette, bars)) = self.pass.as_mut().and_then(|p| p.take(slot)) else {
            return;
        };
        self.bars.measured(bars);
        if self.target.replace(palette).is_none() {
            self.shown = palette;
        }
    }

    /// Gives `p` the bars to measure inside and the glow at `now`; false
    /// until the first colours are in.
    pub fn apply(&mut self, p: &mut SceneParams, now: Instant) -> bool {
        let [x, y] = self.bars.shown;
        (p.glow.z, p.glow.w) = (x, y);
        let (Some(look), Some(target)) = (self.look, self.target) else {
            return false;
        };
        if let Some(last) = self.eased_at.replace(now) {
            let dt = now.saturating_duration_since(last).as_secs_f32();
            let k = 1.0 - (-dt / EASE_SECS).exp();
            for (shown, target) in self.shown.iter_mut().zip(target) {
                *shown = shown.lerp(target, k);
            }
        }
        let share = |v: f32| {
            if v.is_finite() {
                v.clamp(0.0, 1.0)
            } else {
                0.5
            }
        };
        let long = p.screen.x.max(p.screen.y);
        p.glow.x = MAX_BRIGHTNESS * share(look.brightness);
        p.glow.y = MAX_SPREAD * share(look.spread) * long;
        let colours: Vec<u32> = fewer(&spread_out(&self.shown, SPREAD_ZONES[0]), 1)
            .chain(fewer(&spread_out(&self.shown, SPREAD_ZONES[1]), 2))
            .chain(fewer(&spread_out(&self.shown, SPREAD_ZONES[2]), 4))
            .map(pack)
            .collect();
        for (block, chunk) in p.glow_colours.iter_mut().zip(colours.chunks(4)) {
            block.copy_from_slice(chunk);
        }
        true
    }

    /// Whether to measure the colours this frame.
    pub fn due(&self, now: Instant) -> bool {
        self.measured_at
            .is_none_or(|t| now.saturating_duration_since(t) >= MEASURE_EVERY)
    }

    /// The eye passes' pipeline with the glow, planar or for 10-bit frames
    /// read in place.
    pub fn eye_pipeline(&self, tp10: bool) -> vk::Pipeline {
        self.pass.as_ref().map_or(vk::Pipeline::null(), |p| {
            if tp10 { p.tp10_eyes } else { p.eyes }
        })
    }

    /// Measures the colours into `slot`, `bind` binding the pipeline it is
    /// given with the eye passes' descriptors.
    pub fn record(
        &mut self,
        gpu: &Gpu,
        cmd: vk::CommandBuffer,
        slot: usize,
        tp10: bool,
        now: Instant,
        bind: impl FnOnce(vk::Pipeline),
    ) {
        if let Some(pass) = self.pass.as_mut() {
            pass.record(gpu, cmd, slot, tp10, bind);
            self.measured_at = Some(now);
        }
    }

    pub fn destroy(&mut self, gpu: &Gpu) {
        if let Some(mut pass) = self.pass.take() {
            pass.destroy(gpu);
        }
    }
}

struct SlotColours {
    target: Image,
    readback: Buffer,
    pending: bool,
}

struct GlowPass {
    eyes: vk::Pipeline,
    tp10_eyes: vk::Pipeline,
    colours: vk::Pipeline,
    tp10_colours: vk::Pipeline,
    slots: Vec<SlotColours>,
}

impl GlowPass {
    fn new(
        gpu: &Gpu,
        color_format: vk::Format,
        shading_rate: ShadingRate,
        layouts: (vk::PipelineLayout, Option<vk::PipelineLayout>),
        slots: usize,
    ) -> Result<GlowPass> {
        let mut pass = GlowPass {
            eyes: vk::Pipeline::null(),
            tp10_eyes: vk::Pipeline::null(),
            colours: vk::Pipeline::null(),
            tp10_colours: vk::Pipeline::null(),
            slots: Vec::with_capacity(slots),
        };
        if let Err(e) = pass.create(gpu, color_format, shading_rate, layouts, slots) {
            pass.destroy(gpu);
            return Err(e);
        }
        Ok(pass)
    }

    fn create(
        &mut self,
        gpu: &Gpu,
        color_format: vk::Format,
        shading_rate: ShadingRate,
        (planar, tp10): (vk::PipelineLayout, Option<vk::PipelineLayout>),
        slots: usize,
    ) -> Result<()> {
        let make = |layout, tp10: bool, fragment, color_format, shading_rate| {
            pipeline::create(
                gpu,
                &PipelineDesc {
                    spv: if tp10 {
                        crate::SCENE_TP10_SPV
                    } else {
                        crate::SCENE_SPV
                    },
                    fragment,
                    layout,
                    color_format,
                    depth: Depth::None,
                    vertex_stride: 0,
                    instance_stride: 0,
                    attributes: &[],
                    blend: false,
                    descriptor_buffer: tp10,
                    shading_rate,
                },
            )
        };
        self.eyes = make(planar, false, c"fs_main_glow", color_format, shading_rate)?;
        self.colours = make(
            planar,
            false,
            c"fs_glow_colours",
            FORMAT,
            ShadingRate::Unused,
        )?;
        if let Some(layout) = tp10 {
            self.tp10_eyes = make(layout, true, c"fs_main_glow", color_format, shading_rate)?;
            self.tp10_colours = make(
                layout,
                true,
                c"fs_glow_colours",
                FORMAT,
                ShadingRate::Unused,
            )?;
        }
        for _ in 0..slots {
            let mut target = Image::new(
                gpu,
                TEXELS as u32,
                1,
                &[FORMAT],
                vk::ImageUsageFlags::COLOR_ATTACHMENT | vk::ImageUsageFlags::TRANSFER_SRC,
                1,
                "glow colours",
            )?;
            let readback = match Buffer::new(
                gpu,
                (TEXELS * 4) as u64,
                vk::BufferUsageFlags::TRANSFER_DST,
                MemoryLocation::GpuToCpu,
                "glow colours readback",
            ) {
                Ok(b) => b,
                Err(e) => {
                    target.destroy(gpu);
                    return Err(e);
                }
            };
            self.slots.push(SlotColours {
                target,
                readback,
                pending: false,
            });
        }
        Ok(())
    }

    fn record(
        &mut self,
        gpu: &Gpu,
        cmd: vk::CommandBuffer,
        slot: usize,
        tp10: bool,
        bind: impl FnOnce(vk::Pipeline),
    ) {
        let pipeline = if tp10 {
            self.tp10_colours
        } else {
            self.colours
        };
        let Some(s) = self.slots.get_mut(slot) else {
            return;
        };
        if pipeline == vk::Pipeline::null() {
            return;
        }
        let image = &s.target;
        mem::barrier(
            gpu,
            cmd,
            image.image,
            mem::range(0, 1),
            vk::ImageLayout::UNDEFINED,
            vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL,
            mem::TOP,
            mem::COLOR_WRITE,
        );
        let attachment = [vk::RenderingAttachmentInfo::default()
            .image_view(image.level0)
            .image_layout(vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL)
            .load_op(vk::AttachmentLoadOp::DONT_CARE)
            .store_op(vk::AttachmentStoreOp::STORE)];
        let area = vk::Rect2D {
            offset: vk::Offset2D::default(),
            extent: image.extent,
        };
        let d = &gpu.device;
        // SAFETY: recording into the slot's command buffer, whose last
        // submission has finished; the frame's planes are still readable as
        // the eye passes left them.
        unsafe {
            d.cmd_begin_rendering(
                cmd,
                &vk::RenderingInfo::default()
                    .render_area(area)
                    .layer_count(1)
                    .color_attachments(&attachment),
            );
            d.cmd_set_viewport(
                cmd,
                0,
                &[vk::Viewport {
                    width: image.extent.width as f32,
                    height: image.extent.height as f32,
                    max_depth: 1.0,
                    ..Default::default()
                }],
            );
            d.cmd_set_scissor(cmd, 0, &[area]);
            bind(pipeline);
            d.cmd_draw(cmd, 3, 1, 0, 0);
            d.cmd_end_rendering(cmd);
        }
        mem::barrier(
            gpu,
            cmd,
            image.image,
            mem::range(0, 1),
            vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL,
            vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
            mem::COLOR_WRITE,
            mem::TRANSFER_READ,
        );
        let region = [vk::BufferImageCopy::default()
            .image_subresource(
                vk::ImageSubresourceLayers::default()
                    .aspect_mask(vk::ImageAspectFlags::COLOR)
                    .layer_count(1),
            )
            .image_extent(vk::Extent3D {
                width: image.extent.width,
                height: 1,
                depth: 1,
            })];
        let to_host = [vk::BufferMemoryBarrier2::default()
            .buffer(s.readback.buffer)
            .size(vk::WHOLE_SIZE)
            .src_stage_mask(mem::TRANSFER_WRITE.0)
            .src_access_mask(mem::TRANSFER_WRITE.1)
            .dst_stage_mask(vk::PipelineStageFlags2::HOST)
            .dst_access_mask(vk::AccessFlags2::HOST_READ)];
        // SAFETY: the image is ready to copy and the buffer holds all of it.
        unsafe {
            d.cmd_copy_image_to_buffer(
                cmd,
                image.image,
                vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
                s.readback.buffer,
                &region,
            );
            d.cmd_pipeline_barrier2(
                cmd,
                &vk::DependencyInfo::default().buffer_memory_barriers(&to_host),
            );
        }
        s.pending = true;
    }

    /// The colours and the bars at the top, bottom, left and right.
    fn take(&mut self, slot: usize) -> Option<(Palette, [f32; 4])> {
        let s = self.slots.get_mut(slot)?;
        if !std::mem::take(&mut s.pending) {
            return None;
        }
        let bytes = s.readback.bytes().first_chunk::<{ TEXELS * 4 }>()?;
        let texels = bytes.as_chunks::<4>().0;
        let palette = std::array::from_fn(|i| Vec4::from_array(texels[i].map(f32::from)) / 255.0);
        let bars = std::array::from_fn(|i| f32::from(texels[ZONES + i][0]) / 255.0 * BAR_MAX);
        Some((palette, bars))
    }

    fn destroy(&mut self, gpu: &Gpu) {
        for mut s in self.slots.drain(..) {
            s.target.destroy(gpu);
            s.readback.destroy(gpu);
        }
        // SAFETY: not in use (the renderer waits for the device), or never
        // used; nulls are ignored.
        unsafe {
            let d = &gpu.device;
            for p in [self.eyes, self.tp10_eyes, self.colours, self.tp10_colours] {
                d.destroy_pipeline(p, None);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn measure(bars: &mut Bars, times: u32, m: [f32; 4]) {
        for _ in 0..times {
            bars.measured(m);
        }
    }

    #[test]
    fn a_letterbox_is_taken_once_it_holds_for_a_second() {
        let mut bars = Bars::default();
        // A 2.39:1 film in a 16:9 frame: 12.7% top and bottom.
        let film = [0.127, 0.125, 0.0, 0.0];
        measure(&mut bars, BAR_HOLD - 1, film);
        assert_eq!(bars.shown, [0.0, 0.0]);
        bars.measured(film);
        assert_eq!(bars.shown, [0.0, 0.125]);
        // A fade to black changes nothing.
        measure(&mut bars, BAR_HOLD, [BAR_MAX; 4]);
        assert_eq!(bars.shown, [0.0, 0.125]);
        // Full frame again, held as long.
        measure(&mut bars, BAR_HOLD, [0.0; 4]);
        assert_eq!(bars.shown, [0.0, 0.0]);
    }

    #[test]
    fn spreading_keeps_the_light_and_runs_on_round_the_corner() {
        let mut palette = [Vec4::ZERO; ZONES];
        palette[0] = Vec4::ONE;
        let spread = spread_out(&palette, 2.0);
        let total: Vec4 = spread.iter().copied().sum();
        assert!((total - Vec4::ONE).abs().max_element() < 1e-5, "{total}");
        // The first zone's light reaches the last ones, round the top left.
        assert!(spread[ZONES - 1].x > 0.1 && spread[1].x > 0.1);
        assert!((spread[ZONES - 1].x - spread[1].x).abs() < 1e-6);
    }

    #[test]
    fn dark_on_one_side_only_is_no_bar() {
        let mut bars = Bars::default();
        // A night sky at the top, a lit street below; then a pillarbox.
        measure(&mut bars, BAR_HOLD, [0.2, 0.0, 0.0, 0.0]);
        assert_eq!(bars.shown, [0.0, 0.0]);
        measure(&mut bars, BAR_HOLD, [0.0, 0.0, 0.125, 0.13]);
        assert_eq!(bars.shown, [0.125, 0.0]);
    }
}
