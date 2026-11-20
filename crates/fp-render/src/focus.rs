//! Gaze-driven comfort focus for stereo 180° video.
//!
//! The eye passes normally sample each eye's half of the frame by ray
//! direction alone, so the video's disparity stays horizontal in the room
//! whatever the head does. Roll or swing the head and the eyes' baseline no
//! longer matches it: near detail gets vertical disparity and won't fuse.
//! Comfort focus gives each camera a position, [`LENS_SPACING`] apart, and
//! models its picture as a sphere of radius [`Focus::distance`] centred on
//! it. Each eye's ray starts at that eye's real offset from its camera and
//! is intersected with the sphere, then the hit is looked up from the
//! camera, which is geometrically right for any point at that radius. A
//! level, forward-facing head with eyes at the lens spacing has zero
//! offsets, so the picture is unchanged.
//!
//! Close objects also ask the eyes to converge strongly, while the
//! headset's lenses stay focused at a fixed distance; the mismatch
//! (vergence-accommodation conflict) is what makes close-ups tiring. Comfort
//! focus eases it the way stereo depth grading and auto-convergence do: when
//! what the viewer looks at is close, it turns both eyes' images apart
//! symmetrically by a small angle about the head's up, so it asks for less
//! convergence ([`Focus::relief`]). Nothing nearer than ~0.85 m leaves that
//! untouched, and the cyclopean picture never moves or scales. Less
//! convergence makes what is looked at seem farther, and so bigger (half
//! again at 0.4 m), and any change of the easing shows as it growing or
//! shrinking.
//!
//! Measurements aim at the tracked gaze, smoothed, whenever the headset
//! tracks the eyes ([`AimFilter`]), and at the head's forward otherwise.
//! Only what lies there is measured and eased: everything else keeps its
//! disparity, so a far background is pushed past infinity only while the
//! viewer looks at something close. The easing follows the distance in
//! focus, which moves once the latest two readings agree on another, and
//! only for a change of a fifth or more straight ahead ([`RELIEF_STEP`]),
//! more to the side where readings are less precise ([`side_scale`]), so the
//! eyes
//! roaming over the nearer and farther parts of what they look at leave it
//! still; past that it follows by what is beyond the fifth only, so going
//! back and forth between two parts farther apart moves it once rather
//! than every time. It glides in gently, over about a second, so what is looked at
//! doesn't visibly grow; out more promptly, as what is then looked at is
//! farther and pushed past infinity while the easing lasts, once something
//! farther has stayed in focus for a quarter of a second (a glance there and
//! back leaves it be) or what it is eased for hasn't been read for a second.
//! The
//! re-projection follows the distance in focus only once holding its own
//! would misalign either eye's picture of what is looked at by more than
//! [`REDRAW_TOLERANCE_RAD`] ([`redraw_lever`]): it holds still with the
//! head level and facing the video's front, where it changes nothing, and
//! follows closely lying down, where it keeps things in one piece. The
//! part of each eye's offset from its camera that lies along the line
//! through the eyes ([`Lever::side`], all of it with the head turned to
//! one side of the video) only sets what is looked at a little nearer or
//! farther when its distance is wrong, so that part is drawn for a
//! distance of its own ([`Focus::side_distance`]), which follows as the
//! easing does, by what is beyond a fifth: the eyes going over the parts
//! of something near then don't move the pictures against each other at
//! every look, while what would pull them apart up and down stays exact.
//! At the video's left and right edges ([`EDGE_FROM`]) it gives twice the
//! tolerance and glides rather than going at once after a landing.
//!
//! A change while the eyes rest on something shows, as it coming apart and
//! together again, so those are the gentle ones. When the eyes jump and
//! land somewhere else ([`AimFilter::take_landed`]) they take a moment to
//! converge on what is there, and it is drawn for its own distance in that
//! moment ([`LANDING_SECS`]): the re-projection goes there at once, and so
//! does the easing, out for something farther without the wait, and in for
//! something nearer in about a third of a second
//! ([`LANDED_EASE_IN_OMEGA`]): eased in for later, the eyes have converged
//! on it all the way by then and must let go again.
//!
//! Something much nearer than the distance in focus takes over only once it
//! has stayed ([`NEARER_HOLD_SECS`]): a quarter of a second where the eyes
//! have landed for it or the head aims, a second while the eyes rest where
//! they were. What passes in front of what is looked at reads as that near
//! while the eyes are still converged on what is behind it, which only they
//! know; gone again sooner, it moves nothing. A distance the eyes were on
//! in the last few seconds ([`KNOWN_SECS`]) needn't wait when they land
//! back on it.
//!
//! After the eye passes, whenever there is something new to measure (a new
//! video frame, or the eyes moved on by [`REMEASURE_RAD`]) and at least
//! every [`MEASURE_EVERY`], `fs_focus` in `scene.wgsl` draws
//! strips of both eyes' luma round the aim into a small float target, with
//! the eye passes' own bindings, so every frame they can read is measured
//! (a 10-bit one in the decoder's buffer too). The strips lie on a lattice
//! of angles about the cameras' baseline ([`aim_angles`]): each row is the
//! line both cameras see a point on, so a disparity is a shift along it by
//! its own angle, with no vertical part, wherever the aim is. [`FocusPass`]
//! then matches them (`shaders/focus.wgsl`), three 64-thread workgroups per
//! patch of a 3 x 3 cluster, scoring 192 candidate disparities per patch:
//! down to 0.1 m, as close things are where it matters most.
//! [`ComfortFocus`] reads the curves back once the slot's fence has
//! signalled and triangulates the middle patch's disparity ([`nearness`]),
//! or takes the ring's median when the middle has nothing to match (biased
//! towards "far", so mistakes mean less correction rather than a wrong
//! one).

use crate::gpu::Gpu;
use crate::mem::{self, Buffer, Image};
use crate::params::{MAX_COMFORT_CORR_RAD, SceneParams};
use crate::{Error, Result, VkContext};
use ash::vk;
use glam::{Mat3, Quat, Vec2, Vec3};
use gpu_allocator::MemoryLocation;
use std::f32::consts::FRAC_PI_2;
use std::time::{Duration, Instant};

/// Workgroups matching each patch, 64 candidates each (`focus.wgsl`).
const BANDS: usize = 3;
/// Number of disparity candidates evaluated per patch (`focus.wgsl`).
pub(crate) const CANDIDATES: usize = 64 * BANDS;
/// Candidates with texture a patch needs for a reading, among the first
/// band's, whose windows lie within 6° of the aim: half of them, as many as
/// something 3° across against a plain wall gives. Texture farther out says
/// nothing of what is at the aim: where that is plain (a bare wall, the
/// middle of something evenly lit, the black round a cut-out subject), two
/// outlines 25° to 37° apart can look alike enough to match, and read as
/// something 10 cm away.
const MIN_VALID: usize = 32;
/// Candidates below zero disparity (`focus.wgsl`): footage shot with the
/// cameras slightly converged puts distant things there, and without these
/// they would have no candidate to match.
const NEGATIVE: usize = 4;
/// Spacing of the sample lattice, radians: 0.2° (`scene.wgsl`). Each
/// sample averages taps over its cell, which low-passes the picture to a
/// resolution both eyes share, and a 21-sample window still covers only
/// about 4°, a hand's width at arm's length. Candidates are half a step
/// apart, per eye.
const STEP: f32 = 0.0035;
/// Patch grid columns and rows round the aim (`scene.wgsl`).
const GRID_W: usize = 3;
const GRID_H: usize = 3;
/// Patches measured; the middle one is at the aim.
pub(crate) const PATCHES: usize = GRID_W * GRID_H;
const AIM_PATCH: usize = PATCHES / 2;
/// Lattice angle between neighbouring patches' columns, radians
/// (`FOCUS_GRID_X` in `scene.wgsl`).
const GRID_ALONG: f32 = 0.06;
/// Farthest the aim may be from straight ahead along the baseline, radians:
/// 65°. Past it the cameras look nearly along their own baseline: a
/// disparity is less than sin 25° = 0.42 of what it is straight ahead and the
/// lattice's rows close in, so a reading there would be mostly noise.
const MAX_ALONG: f32 = 65.0 * std::f32::consts::PI / 180.0;
/// Each patch's strips in the samples target (`scene.wgsl`, `focus.wgsl`):
/// columns for each eye, and rows.
const SAMPLE_COLS: u32 = 117;
const SAMPLE_ROWS: u32 = 19;
/// The samples target's format: luma, and -1 off the picture.
pub(crate) const SAMPLES_FORMAT: vk::Format = vk::Format::R32_SFLOAT;
/// How often the view is measured at the least: 20 times a second, so a
/// still picture looked at keeps being read. Something new to read is
/// measured at once: a new video frame, or the aim moved by
/// [`REMEASURE_RAD`]. The same frame read again at about the same aim says
/// nothing new, and would let one wrong reading count as two.
pub(crate) const MEASURE_EVERY: Duration = Duration::from_millis(50);
/// How far (radians, 1°: about what the eye tracker is off by) the aim must
/// move for what lies there to be measured before [`MEASURE_EVERY`].
pub(crate) const REMEASURE_RAD: f32 = 0.0175;
/// Ring patches that must agree, by their median, when the middle one has
/// nothing to match.
const MIN_RING_READINGS: usize = 3;

/// Disparity (radians per eye) at and below which no correction is applied
/// (~0.85 m with a 65 mm baseline).
const KAPPA_FAR: f32 = 0.038;
/// Softness of the onset of correction past [`KAPPA_FAR`]: correction grows
/// quadratically at first, then about linearly.
const KAPPA_KNEE: f32 = 0.020;
/// Minimum weighted luma standard deviation in $[0, 1]$ to trust a patch.
const MIN_PATCH_STD: f32 = 0.010;
/// Minimum similarity at the peak for a measurement.
const MIN_PEAK_SCORE: f32 = 0.6;
/// Minimum margin of the peak over the curve's mean, rejecting flat or
/// repetitive profiles.
const MIN_PROMINENCE: f32 = 0.05;
/// A near peak must beat the best far candidate by this much; otherwise the
/// patch is read as far.
const NEAR_MARGIN: f32 = 0.04;
/// Another peak of a patch's curve scoring within this of its best makes
/// the patch ambiguous: a pattern repeating along the baseline (a railing,
/// tiles, stripes) matches at more than one disparity, and the one that
/// happens to score higher may be a depth nothing is at. Such a patch gives
/// no reading, as one with nothing to match does.
const AMBIGUITY: f32 = 0.1;
/// Candidates either side of the best that belong to its own peak.
const PEAK_HALF_WIDTH: usize = 2;
/// How long (seconds) the correction stays with no reading of the distance
/// in focus: none at all, or only ones of something else that don't settle
/// on a distance of their own. The eyes may rest on a plain or dark spot of
/// what is in focus for a moment, but past this they may well be on
/// something else, perhaps far, which the correction would push past
/// infinity.
const UNREAD_HOLD_SECS: f32 = 1.0;
/// How much the distance in focus must change, as a share, for the easing
/// to follow it: a fifth straight ahead, [`side_scale`] times as much to the
/// side (24% at 35°, 31% at 50°, 40% at 60°). Something 0.4 m away has
/// parts a few centimetres nearer and farther, which would otherwise be
/// eased by different amounts and make it grow and shrink as the eyes move
/// over it. Past that it follows by what is beyond it only, staying that
/// share short of the distance in focus: the eyes going back and forth
/// between two parts of something, 0.40 and 0.52 m away, move it once, by a
/// third of what easing for each in turn would move it every time.
const RELIEF_STEP: f32 = 0.2;
/// Natural frequency (rad/s) of the critically damped spring easing the
/// correction in: 90% of a change in about a second, gently enough that
/// what is looked at doesn't visibly swell.
const EASE_IN_OMEGA: f32 = 3.9;
/// Fastest the correction may grow (radians per eye per second, ~3.4°/s).
const EASE_IN_MAX_SPEED: f32 = 0.06;
/// The same easing out: 90% of a change in ~0.45 s, about as long as the
/// eyes take to converge on something new. What they then look at is
/// farther, and pushed past infinity until it is done.
const EASE_OUT_OMEGA: f32 = 9.0;
/// Fastest the correction may shrink (radians per eye per second,
/// ~6.9°/s): the largest goes in 0.4 s.
const EASE_OUT_MAX_SPEED: f32 = 0.12;
/// How long (seconds) something farther must stay in focus before the
/// correction eases out for it: a glance there and back, as most take
/// less, leaves it be.
const EASE_OUT_HOLD_SECS: f32 = 0.25;
/// How long (seconds) something much nearer than the distance in focus
/// (by more than [`RELIEF_STEP`]) must stay read before it takes over,
/// where the eyes have just landed ([`LANDING_SECS`]) or the head aims.
/// Something passing in front of what is looked at reads as that near while
/// the eyes are still converged on what is behind it, and so do two
/// outlines far apart that happen to match for a few frames; changing the
/// picture for either shows as a jolt there and back. With the eyes tracked
/// and resting where they were, they haven't chosen it: it must stay
/// [`UNREAD_HOLD_SECS`], by when they may well have gone to it.
const NEARER_HOLD_SECS: f32 = 0.25;
/// How long (seconds) after the eyes land somewhere else what is read there
/// is acted on at once ([`ComfortFocus::landed`]): the re-projection goes to
/// its distance, and the easing out to less for something farther, while
/// the eyes are still converging on it. Later they have fused it, and a
/// change shows as it coming apart and together again, so it is made
/// gently.
const LANDING_SECS: f32 = 0.3;
/// Natural frequency (rad/s) of the easing out for something farther the
/// eyes have just landed on: 90% of a change in 0.13 s.
const LANDED_EASE_OMEGA: f32 = 30.0;
/// Fastest the correction may shrink then (radians per eye per second).
const LANDED_EASE_MAX_SPEED: f32 = 0.6;
/// Natural frequency (rad/s) of the easing in for something nearer the
/// eyes have just landed on: 90% of a change in 0.28 s, while they are
/// still converging on it. Eased in for later, they have converged on it
/// all the way by then, and must let go again as the easing comes.
const LANDED_EASE_IN_OMEGA: f32 = 14.0;
/// Fastest the correction may grow then (radians per eye per second).
const LANDED_EASE_IN_MAX_SPEED: f32 = 0.3;
/// How long (seconds) the easing keeps that pace once set off, and the
/// re-projection's glide its own ([`LANDED_GLIDE_SECS`]).
const LANDED_PACE_SECS: f32 = 0.3;
/// How long (seconds) a distance stays known after the distance in focus
/// left it: the eyes landing back on it within that needn't wait
/// ([`NEARER_HOLD_SECS`]), as going back and forth between something near
/// and what is behind it is what they do most.
const KNOWN_SECS: f32 = 5.0;
/// How long (seconds) a distance must have been in focus to be known: what
/// the eyes rested on, not what a few readings took for it.
const KNOWN_MIN_SECS: f32 = 0.5;
/// Distances known at a time.
const KNOWN: usize = 4;
/// Time constant (seconds) of the re-projection's glide to the distance of
/// what the eyes have just landed on: 90% of a change in 0.06 s.
const LANDED_GLIDE_SECS: f32 = 0.025;
/// A blink this long (seconds) or shorter keeps the aim where the eyes were.
const BLINK_SECS: f32 = 0.4;
/// A gaze further than this (radians, 45°) from where the head points is
/// the tracker's error: the eyes look through the lenses within about that.
const PLAUSIBLE_GAZE_RAD: f32 = std::f32::consts::FRAC_PI_4;
/// The gaze smoothing's cutoff at rest, Hz: the One Euro filter (Casiez,
/// Roussel and Vogel, CHI 2012) takes out most of the tracker's jitter
/// while the eyes rest on something, and lets a jump of the eyes through
/// at once, by raising its cutoff with the gaze's speed. Lower takes out
/// more jitter but drifts behind the eyes following something slow.
const GAZE_MIN_CUTOFF_HZ: f32 = 1.0;
/// How much the cutoff rises with the gaze's speed, Hz per radian a
/// second: a 20° jump is followed to within a degree in three frames at
/// 90 Hz.
const GAZE_BETA: f32 = 3.0;
/// The cutoff of the speed the cutoff rises by, Hz: a jump's speed shows
/// in a frame or two, a resting gaze's jitter mostly doesn't.
const GAZE_SPEED_CUTOFF_HZ: f32 = 5.0;
/// A gap between gaze samples longer than this (seconds) starts the
/// smoothing afresh rather than smoothing across it.
const GAZE_GAP_SECS: f32 = 0.1;
/// The smoothed gaze further than this (radians, 2°) from the latest
/// sample is catching up with a jump of the eyes ([`AimFilter::settled`]);
/// a resting gaze's jitter keeps within it.
const SETTLED_RAD: f32 = 0.035;
/// Two gaze samples running no further apart than this (radians, 1°) are
/// both of where the eyes have landed after a jump: the aim goes between
/// them, which halves the tracker's jitter in it.
const LANDED_RAD: f32 = 0.0175;

/// The matching pass reading the samples the fragment pass drew.
const COMPUTE_READ: (vk::PipelineStageFlags2, vk::AccessFlags2) = (
    vk::PipelineStageFlags2::COMPUTE_SHADER,
    vk::AccessFlags2::SHADER_SAMPLED_READ,
);

/// The aim's angles about the cameras' baseline, `[along, tilt]`, in the
/// video's own frame with its Position corrections applied, where the
/// cameras sit on the x axis: `fs_focus` lays its lattice out in them
/// (`lattice` in `scene.wgsl`). A point and the two cameras span a plane
/// through that axis; `tilt` is the plane's turn about it and `along` the
/// angle within it from straight ahead towards +x. `None` too near the
/// baseline's axis to measure ([`MAX_ALONG`]).
pub(crate) fn aim_angles(scene: &SceneParams, aim: Vec3) -> Option<[f32; 2]> {
    let room = scene.anchor_inv.transform_vector3(aim);
    let d = scene.correction.transform_vector3(room).try_normalize()?;
    let along = d.x.clamp(-1.0, 1.0).asin();
    (along.abs() <= MAX_ALONG).then(|| [along, d.y.atan2(-d.z)])
}

struct SlotFocus {
    samples: Image,
    scores: Buffer,
    set: vk::DescriptorSet,
    /// The lattice's `along` angle where the measurement recorded in this
    /// slot aimed ([`aim_angles`]).
    along: f32,
    pending: bool,
}

pub(crate) struct FocusPass {
    pool: vk::DescriptorPool,
    set_layout: vk::DescriptorSetLayout,
    layout: vk::PipelineLayout,
    pipeline: vk::Pipeline,
    slots: Vec<SlotFocus>,
}

impl FocusPass {
    pub fn new(gpu: &Gpu, slots: usize) -> Result<FocusPass> {
        let mut p = FocusPass {
            pool: vk::DescriptorPool::null(),
            set_layout: vk::DescriptorSetLayout::null(),
            layout: vk::PipelineLayout::null(),
            pipeline: vk::Pipeline::null(),
            slots: Vec::new(),
        };
        if let Err(e) = p.create(gpu, slots) {
            p.destroy(gpu);
            return Err(e);
        }
        Ok(p)
    }

    fn create(&mut self, gpu: &Gpu, slots: usize) -> Result<()> {
        let d = &gpu.device;
        let types = [
            vk::DescriptorType::STORAGE_BUFFER,
            vk::DescriptorType::SAMPLED_IMAGE,
        ];
        let bindings = [0, 1].map(|b| {
            vk::DescriptorSetLayoutBinding::default()
                .binding(b)
                .descriptor_type(types[b as usize])
                .descriptor_count(1)
                .stage_flags(vk::ShaderStageFlags::COMPUTE)
        });
        let n = slots as u32;
        let sizes = types.map(|ty| vk::DescriptorPoolSize {
            ty,
            descriptor_count: n,
        });
        // SAFETY: valid device; all created objects are cleaned up in `destroy`.
        unsafe {
            self.set_layout = d
                .create_descriptor_set_layout(
                    &vk::DescriptorSetLayoutCreateInfo::default().bindings(&bindings),
                    None,
                )
                .ctx("focus descriptor set layout")?;
            let sets = [self.set_layout];
            self.layout = d
                .create_pipeline_layout(
                    &vk::PipelineLayoutCreateInfo::default().set_layouts(&sets),
                    None,
                )
                .ctx("focus pipeline layout")?;
            let module = gpu.shader(crate::FOCUS_SPV)?;
            let info = vk::ComputePipelineCreateInfo::default()
                .stage(
                    vk::PipelineShaderStageCreateInfo::default()
                        .stage(vk::ShaderStageFlags::COMPUTE)
                        .module(module)
                        .name(c"main"),
                )
                .layout(self.layout);
            let pipes = d.create_compute_pipelines(vk::PipelineCache::null(), &[info], None);
            d.destroy_shader_module(module, None);
            self.pipeline = pipes.map_err(|(_, result)| Error::Vk {
                context: "focus pipeline",
                result,
            })?[0];
            self.pool = d
                .create_descriptor_pool(
                    &vk::DescriptorPoolCreateInfo::default()
                        .max_sets(n)
                        .pool_sizes(&sizes),
                    None,
                )
                .ctx("focus descriptor pool")?;
        }
        for _ in 0..slots {
            let slot = self.slot(gpu)?;
            self.slots.push(slot);
        }
        Ok(())
    }

    /// A slot's samples target, scores buffer and descriptors, which never
    /// change.
    fn slot(&self, gpu: &Gpu) -> Result<SlotFocus> {
        let mut samples = Image::new(
            gpu,
            2 * SAMPLE_COLS,
            SAMPLE_ROWS * PATCHES as u32,
            &[SAMPLES_FORMAT],
            vk::ImageUsageFlags::COLOR_ATTACHMENT | vk::ImageUsageFlags::SAMPLED,
            1,
            "focus samples",
        )?;
        let mut scores = match Buffer::new(
            gpu,
            (PATCHES * CANDIDATES * 8) as u64,
            vk::BufferUsageFlags::STORAGE_BUFFER,
            MemoryLocation::GpuToCpu,
            "focus scores",
        ) {
            Ok(b) => b,
            Err(e) => {
                samples.destroy(gpu);
                return Err(e);
            }
        };
        let d = &gpu.device;
        let layouts = [self.set_layout];
        // SAFETY: the pool has room for one set per slot.
        let set = unsafe {
            d.allocate_descriptor_sets(
                &vk::DescriptorSetAllocateInfo::default()
                    .descriptor_pool(self.pool)
                    .set_layouts(&layouts),
            )
        };
        let set = match set.ctx("focus descriptor set") {
            Ok(sets) => sets[0],
            Err(e) => {
                samples.destroy(gpu);
                scores.destroy(gpu);
                return Err(e);
            }
        };
        let buffers = [vk::DescriptorBufferInfo::default()
            .buffer(scores.buffer)
            .range(vk::WHOLE_SIZE)];
        let images = [vk::DescriptorImageInfo::default()
            .image_view(samples.level0)
            .image_layout(vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL)];
        let writes = [
            vk::WriteDescriptorSet::default()
                .dst_set(set)
                .dst_binding(0)
                .descriptor_type(vk::DescriptorType::STORAGE_BUFFER)
                .buffer_info(&buffers),
            vk::WriteDescriptorSet::default()
                .dst_set(set)
                .dst_binding(1)
                .descriptor_type(vk::DescriptorType::SAMPLED_IMAGE)
                .image_info(&images),
        ];
        // SAFETY: the set is new and the buffer and view are valid.
        unsafe { d.update_descriptor_sets(&writes, &[]) };
        Ok(SlotFocus {
            samples,
            scores,
            set,
            along: 0.0,
            pending: false,
        })
    }

    /// The target `fs_focus` draws `slot`'s samples into, ready to draw.
    pub fn begin_samples(
        &mut self,
        gpu: &Gpu,
        cmd: vk::CommandBuffer,
        slot: usize,
    ) -> Option<(vk::ImageView, vk::Extent2D)> {
        let s = self.slots.get(slot)?;
        mem::barrier(
            gpu,
            cmd,
            s.samples.image,
            mem::range(0, 1),
            vk::ImageLayout::UNDEFINED,
            vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL,
            mem::TOP,
            mem::COLOR_WRITE,
        );
        Some((s.samples.level0, s.samples.extent))
    }

    /// Matches the samples drawn for `slot`, which aimed at lattice angle
    /// `along`, for reading back once the slot's fence has signalled.
    pub fn record(&mut self, gpu: &Gpu, cmd: vk::CommandBuffer, slot: usize, along: f32) {
        let Some(s) = self.slots.get_mut(slot) else {
            return;
        };
        mem::barrier(
            gpu,
            cmd,
            s.samples.image,
            mem::range(0, 1),
            vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL,
            vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL,
            mem::COLOR_WRITE,
            COMPUTE_READ,
        );
        let d = &gpu.device;
        // SAFETY: recording into the slot's command buffer; the pipeline,
        // set and buffer stay valid for its life.
        unsafe {
            d.cmd_bind_pipeline(cmd, vk::PipelineBindPoint::COMPUTE, self.pipeline);
            d.cmd_bind_descriptor_sets(
                cmd,
                vk::PipelineBindPoint::COMPUTE,
                self.layout,
                0,
                &[s.set],
                &[],
            );
            d.cmd_dispatch(cmd, PATCHES as u32, BANDS as u32, 1);
            let done = [vk::BufferMemoryBarrier2::default()
                .buffer(s.scores.buffer)
                .size(vk::WHOLE_SIZE)
                .src_stage_mask(vk::PipelineStageFlags2::COMPUTE_SHADER)
                .src_access_mask(vk::AccessFlags2::SHADER_STORAGE_WRITE)
                .dst_stage_mask(vk::PipelineStageFlags2::HOST)
                .dst_access_mask(vk::AccessFlags2::HOST_READ)];
            d.cmd_pipeline_barrier2(
                cmd,
                &vk::DependencyInfo::default().buffer_memory_barriers(&done),
            );
        }
        s.along = along;
        s.pending = true;
    }

    /// Reads the per-patch scores (`PATCHES * CANDIDATES` `(score, std)`
    /// pairs) and the lattice angle `along` where they aimed, from `slot`
    /// once its fence has signalled.
    pub fn take(&mut self, slot: usize) -> Option<(Vec<[f32; 2]>, f32)> {
        let s = self.slots.get_mut(slot)?;
        if !std::mem::take(&mut s.pending) {
            return None;
        }
        let along = s.along;
        let bytes = s.scores.bytes();
        if bytes.len() < PATCHES * CANDIDATES * 8 {
            return None;
        }
        let out = bytes.as_chunks::<8>().0[..PATCHES * CANDIDATES]
            .iter()
            .map(|&[a0, a1, a2, a3, b0, b1, b2, b3]| {
                [
                    f32::from_le_bytes([a0, a1, a2, a3]),
                    f32::from_le_bytes([b0, b1, b2, b3]),
                ]
            })
            .collect();
        Some((out, along))
    }

    pub fn discard(&mut self) {
        for s in &mut self.slots {
            s.pending = false;
        }
    }

    pub fn destroy(&mut self, gpu: &Gpu) {
        for mut s in self.slots.drain(..) {
            s.samples.destroy(gpu);
            s.scores.destroy(gpu);
        }
        // SAFETY: renderer waits for all fences before destroying passes.
        unsafe {
            let d = &gpu.device;
            d.destroy_descriptor_pool(std::mem::take(&mut self.pool), None);
            d.destroy_pipeline(std::mem::take(&mut self.pipeline), None);
            d.destroy_pipeline_layout(std::mem::take(&mut self.layout), None);
            d.destroy_descriptor_set_layout(std::mem::take(&mut self.set_layout), None);
        }
    }
}

/// Maps (fractional) candidate index `idx` in `0..=191` to per-eye
/// disparity: half a lattice step apart, the first [`NEGATIVE`] below zero,
/// the last 0.327 rad (0.093 m straight ahead with a 63 mm baseline). Nearer
/// still the cameras see a thing from more than 37° apart, and their
/// pictures of it have little left in common to match.
pub(crate) fn kappa_at(idx: f32) -> f32 {
    (idx.clamp(0.0, (CANDIDATES - 1) as f32) - NEGATIVE as f32) * 0.5 * STEP
}

/// Per-eye vergence relief (radians) for an object at per-eye disparity
/// `kappa`: exactly zero at or beyond [`KAPPA_FAR`], then growing smoothly
/// (zero slope at the threshold) and capped at [`MAX_COMFORT_CORR_RAD`].
pub(crate) fn correction_from_kappa(kappa: f32) -> f32 {
    let e = kappa - KAPPA_FAR;
    if e <= 0.0 || !e.is_finite() {
        return 0.0;
    }
    (e * e / (e + KAPPA_KNEE)).min(MAX_COMFORT_CORR_RAD)
}

/// Why a patch gave no reading.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Miss {
    /// Too little texture to match, or too much of it off the picture.
    Plain,
    /// No candidate matches well enough, or none stands out.
    Weak,
    /// More than one stands out ([`AMBIGUITY`]).
    Ambiguous,
}

/// Finds the sub-step per-eye disparity of one patch from its
/// `(score, std)` curve, or why it has none: no texture, or no clear peak.
/// `far` is the disparity at and below which what is there needs no
/// relief: [`KAPPA_FAR`] straight ahead, less where the baseline is seen
/// at a slant ([`far_kappa`]).
pub(crate) fn estimate_kappa(
    scores: &[[f32; 2]; CANDIDATES],
    far: f32,
) -> std::result::Result<f32, Miss> {
    let mut valid_count = 0usize;
    let mut valid_at_aim = 0usize;
    let mut sum = 0.0f32;
    let mut best: Option<usize> = None;
    let mut best_far: Option<usize> = None;

    for (i, &[score, std_dev]) in scores.iter().enumerate() {
        if !score.is_finite() || !std_dev.is_finite() || std_dev < MIN_PATCH_STD {
            continue;
        }
        valid_count += 1;
        valid_at_aim += (i < CANDIDATES / BANDS) as usize;
        sum += score;
        if best.is_none_or(|b| score > scores[b][0]) {
            best = Some(i);
        }
        if kappa_at(i as f32) <= far && best_far.is_none_or(|b| score > scores[b][0]) {
            best_far = Some(i);
        }
    }

    if valid_at_aim < MIN_VALID {
        return Err(Miss::Plain);
    }
    let mut best_i = best.ok_or(Miss::Plain)?;
    let mean = sum / valid_count as f32;
    let peak = scores[best_i][0];
    if peak < MIN_PEAK_SCORE || peak - mean < MIN_PROMINENCE {
        return Err(Miss::Weak);
    }
    // Another peak nearly as good: more than one depth fits.
    let valid = |i: usize| {
        let [score, std_dev] = scores[i];
        (score.is_finite() && std_dev.is_finite() && std_dev >= MIN_PATCH_STD).then_some(score)
    };
    let rival = (0..CANDIDATES)
        .filter(|&i| i.abs_diff(best_i) > PEAK_HALF_WIDTH)
        .filter_map(|i| {
            let s = valid(i)?;
            let below = i.checked_sub(1).and_then(valid);
            let above = (i + 1 < CANDIDATES).then(|| valid(i + 1)).flatten();
            (below.is_none_or(|b| s >= b) && above.is_none_or(|a| s >= a)).then_some(s)
        })
        .fold(f32::NEG_INFINITY, f32::max);
    if rival >= peak - AMBIGUITY {
        return Err(Miss::Ambiguous);
    }
    // Asymmetric decision: a near reading must clearly beat every far one.
    if kappa_at(best_i as f32) > far
        && let Some(f) = best_far
        && peak - scores[f][0] < NEAR_MARGIN
    {
        best_i = f;
    }

    // 3-point parabolic refinement around the peak.
    let mut refined = best_i as f32;
    if best_i > 0 && best_i + 1 < CANDIDATES {
        let y0 = scores[best_i - 1][0];
        let y1 = scores[best_i][0];
        let y2 = scores[best_i + 1][0];
        let denom = y0 - 2.0 * y1 + y2;
        if denom < -1e-5 {
            refined += (0.5 * (y0 - y2) / denom).clamp(-0.5, 0.5);
        }
    }
    Ok(kappa_at(refined))
}

/// Lens spacing assumed for every video, metres: the average adult eye
/// spacing, which stereo rigs are built to match.
pub(crate) const LENS_SPACING: f32 = 0.063;
/// The distance in focus before the first reading: the farthest, where the
/// re-projection moves nothing, so the picture starts as filmed.
const START_DISTANCE: f32 = FARTHEST;
/// The nearest and farthest the distance in focus goes, metres: about as
/// near as the candidates reach straight ahead ([`kappa_at`]).
const NEAREST: f32 = 0.1;
const FARTHEST: f32 = 50.0;
/// Time constant (seconds) of the re-projection's glide to a new distance,
/// in nearness: 90% of a change in ~0.7 s.
const DISTANCE_GLIDE_SECS: f32 = 0.3;
/// How far (radians, 0.15°) keeping the re-projection's distance may
/// misalign either eye's picture of what is looked at before it follows the
/// distance in focus: the eye's [`redraw_lever`] times the change in
/// nearness. Lying down that is a change of 2% at 0.4 m, and it follows
/// all the way, as the pictures come apart up and down ([`Lever::up`]).
/// The part of each eye's offset along the line through the eyes
/// ([`Lever::side`]) only moves the pictures towards or away from each
/// other, which the eyes follow: that part is drawn for a distance of its
/// own ([`Focus::side_distance`]) that goes by what is beyond
/// [`RELIEF_STEP`], as the easing does. With the head level and facing the
/// video's front, none ever, as the re-projection changes nothing
/// there.
const REDRAW_TOLERANCE_RAD: f32 = 0.0026;
/// Where the video's left and right edges begin for the re-projection, and
/// where they have fully begun: lattice angles `along` ([`aim_angles`]),
/// radians (30° and 45°). Out there the eyes are far from where their
/// cameras were, as the head is turned to look, readings are at their least
/// precise, and something near fills much of the view: its parts, each at
/// its own distance, would move the pictures at every look. So the
/// re-projection gives twice [`REDRAW_TOLERANCE_RAD`] there, and glides to
/// what the eyes land on as it does to anything else, rather than going at
/// once. Between the two it does part of each.
const EDGE_FROM: f32 = std::f32::consts::FRAC_PI_6;
const EDGE_FULL: f32 = std::f32::consts::FRAC_PI_4;

/// How far into the video's left or right edge an aim at lattice angle
/// `along` is: 0 short of [`EDGE_FROM`], 1 past [`EDGE_FULL`].
pub(crate) fn edge(along: f32) -> f32 {
    ((along.abs() - EDGE_FROM) / (EDGE_FULL - EDGE_FROM)).clamp(0.0, 1.0)
}

/// Readings the first distance in focus is decided on: the newest that
/// gave a distance and the two before it, within [`PAIR_SECS`].
const RECENT: usize = 3;
/// How long (seconds) a reading that gave a distance counts towards another
/// one agreeing with it. Where little can be matched, readings with nothing
/// to match come between the ones that give a distance: those two still
/// decide together, as two running do. Longer ago, the eyes may have been
/// on something else.
const PAIR_SECS: f32 = 0.5;
/// Two readings agree within this, in nearness (1/metres), straight ahead:
/// three times what the matcher resolves with a 63 mm baseline, under a
/// sixth of a degree of the eyes' convergence (1 m against 1.04 m, 3 m
/// against 3.4 m). To the side, [`side_scale`] times as much.
const AGREE: f32 = 0.04;
/// Nearer than 0.3 m two readings agree within this share of how near they
/// are, where that is more than [`AGREE`]: 1.2%, which is [`AGREE`] at
/// 0.3 m. Something that close seldom keeps still to a millimetre from one
/// reading to the next, and the same movement is more of a change in
/// nearness the nearer it is.
const AGREE_SHARE: f32 = 0.012;
/// How far a reading agreeing with the distance in focus moves it towards
/// itself: an average over about the last two, steady against the
/// matcher's noise while following something moving slowly.
const REFINE: f32 = 0.5;

/// How many times less precise a reading at lattice angle `along`
/// ([`aim_angles`]) is than one straight ahead. To the side the cameras'
/// baseline is seen at a slant and spans `cos(along)` of its angle there,
/// so the same disparity step is `1 / cos(along)` as much nearness: 1.2 at
/// 35°, 1.6 at 50°, 2 at 60°.
pub(crate) fn side_scale(along: f32) -> f32 {
    1.0 / along.cos().max(MAX_ALONG.cos())
}

/// How far, across the aim, the farther of the eyes is from where its
/// camera was, metres ([`redraw_lever`]): a change of the re-projection's
/// distance by `dn` in nearness moves that eye's picture of what is looked
/// at by about `dn` times this, radians, the other eye's the other way.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct Lever {
    /// Along the line through the eyes, as the aim sees it: the pictures
    /// move towards or away from each other, and what is looked at seems a
    /// little nearer or farther. All of it with the head turned to one side
    /// of the video.
    pub side: f32,
    /// Square to that line: one picture moves up and the other down, and
    /// what is looked at comes apart. Grows as the head tilts to a shoulder.
    pub up: f32,
}

impl Lever {
    /// The whole of it.
    pub fn length(self) -> f32 {
        self.side.hypot(self.up)
    }
}

/// The [`Lever`] of the eyes, `offsets` (room frame, left then right) from
/// the point between them, for an `aim` in the play space, and the line
/// through the eyes as the aim sees it (the video's frame, unit; zero
/// where there is none). No lever with the head level, facing the video's
/// front, and the eyes the lens spacing apart.
pub(crate) fn redraw_lever(scene: &SceneParams, aim: Vec3, offsets: [Vec3; 2]) -> (Lever, Vec3) {
    let room = scene.anchor_inv.transform_vector3(aim);
    let Some(d) = scene.correction.transform_vector3(room).try_normalize() else {
        return (Lever::default(), Vec3::ZERO);
    };
    let across = |v: Vec3| v - v.dot(d) * d;
    // The line through the eyes as the aim sees it.
    let line = across(scene.correction.transform_vector3(offsets[1] - offsets[0])).try_normalize();
    let lever = [-1.0f32, 1.0]
        .into_iter()
        .zip(offsets)
        .map(|(side, o)| {
            let camera = Vec3::X * (side * 0.5 * LENS_SPACING);
            let off = across(scene.correction.transform_vector3(o) - camera);
            match line {
                Some(line) => Lever {
                    side: off.dot(line).abs(),
                    up: (off - off.dot(line) * line).length(),
                },
                None => Lever {
                    side: 0.0,
                    up: off.length(),
                },
            }
        })
        .fold(Lever::default(), |a, b| Lever {
            side: a.side.max(b.side),
            up: a.up.max(b.up),
        });
    (lever, line.unwrap_or(Vec3::ZERO))
}

/// How near a point is, 1/metres from between the cameras, that the
/// cameras ([`LENS_SPACING`] apart) see `kappa` either side of lattice
/// angle `along` ([`aim_angles`]): the left one at `along + kappa`, the
/// right one at `along - kappa`, on the same row. In the triangle of the
/// cameras and the point, the angle at the point is `2 kappa`, opposite the
/// baseline, so the law of sines gives the sides: exact wherever the point
/// is, where `0.5 * LENS_SPACING / tan(kappa)` holds only straight ahead
/// (off to the side the baseline is seen at a slant, and reads too far by
/// 1/cos(along): 22% at 35°). 0 for a disparity at or below zero.
pub(crate) fn nearness(along: f32, kappa: f32) -> f32 {
    if !(kappa > 0.0 && along.is_finite()) {
        return 0.0;
    }
    // Each camera's angle from the baseline's +x end.
    let (left, right) = (FRAC_PI_2 - along - kappa, FRAC_PI_2 - along + kappa);
    let sin_right = right.sin();
    if sin_right < 1e-6 {
        return 0.0;
    }
    // 1 / the point's range from the left camera.
    let inv_left = (2.0 * kappa).sin() / (LENS_SPACING * sin_right);
    // Its range from between the cameras, along the left camera's ray.
    let b = LENS_SPACING * inv_left;
    let m = 1.0 - b * left.cos() + 0.25 * b * b;
    inv_left / m.max(1e-12).sqrt()
}

/// The disparity per eye, radians, below which something seen at lattice
/// angle `along` is farther than [`KAPPA_FAR`] means straight ahead: the
/// baseline spans `cos(along)` of its angle there, to first order.
pub(crate) fn far_kappa(along: f32) -> f32 {
    KAPPA_FAR * along.cos()
}

/// Per-eye vergence relief (radians) for something `nearness` (1/metres)
/// away: [`correction_from_kappa`] of its disparity straight ahead, which is
/// how far the eyes converge on it once the re-projection draws it at that
/// distance from where they are.
pub(crate) fn relief_at(nearness: f32) -> f32 {
    correction_from_kappa((0.5 * LENS_SPACING * nearness).atan())
}

/// How near (1/metres) what lies at the aim is: the middle patch's reading,
/// else the median of the ring round it, or none when neither can be
/// measured. `along` is the aim's lattice angle ([`aim_angles`]); each patch
/// is triangulated at its own.
pub(crate) fn aim_reading(scores: &[[f32; 2]], along: f32) -> Option<f32> {
    let curves = scores.as_chunks::<CANDIDATES>().0;
    let read = |i: usize| {
        let at = along + ((i % GRID_W) as f32 - 1.0) * GRID_ALONG;
        let kappa = estimate_kappa(curves.get(i)?, far_kappa(at)).ok()?;
        Some(nearness(at, kappa))
    };
    if let Some(n) = read(AIM_PATCH) {
        return Some(n);
    }
    let mut ring: Vec<f32> = (0..curves.len())
        .filter(|&i| i != AIM_PATCH)
        .filter_map(read)
        .collect();
    ring.sort_by(f32::total_cmp);
    // The lower middle of an even count: the farther one.
    (ring.len() >= MIN_RING_READINGS).then(|| ring[(ring.len() - 1) / 2])
}

/// What comfort focus does to a frame.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Focus {
    /// How far each eye's picture turns apart, radians.
    pub relief: f32,
    /// The radius of the camera spheres, metres: points at this distance
    /// line up for any head pose.
    pub distance: f32,
    /// The distance, metres, each eye's offset from its camera along the
    /// line through the eyes is drawn for ([`Lever::side`]): the same, but
    /// following the distance in focus as the easing does, by what is
    /// beyond a fifth.
    pub side_distance: f32,
}

/// Correction tracker across frames.
#[derive(Default)]
pub(crate) struct ComfortFocus {
    /// Newest reading not yet folded in by [`ComfortFocus::step`]: how near
    /// what lay at its aim was, 1/metres (`None`: nothing to match there),
    /// and the lattice angle `along` there.
    pending: Option<(Option<f32>, f32)>,
    /// The latest readings that gave a distance, wherever the eyes were,
    /// newest last: how near, and when by `clock`. No more than [`RECENT`],
    /// none older than [`PAIR_SECS`].
    recent: Vec<(f32, f32)>,
    /// Seconds stepped through, at most 0.1 s a frame: a stalled frame
    /// doesn't age the readings before it. Every wait goes by it.
    clock: f32,
    /// What the distance in focus glides to, as nearness, once two
    /// readings have agreed on it.
    accepted: Option<f32>,
    /// When, by `clock`, a reading last saw the distance in focus.
    seen: Option<f32>,
    /// Whether the aim follows the eyes ([`ComfortFocus::follows_eyes`]).
    eyes: bool,
    /// [`side_scale`] where the latest reading that gave a distance aimed,
    /// and how far into the video's edge that was ([`edge`]).
    scale: f32,
    edge: f32,
    /// Something much nearer than the distance in focus is being read, not
    /// yet for long enough to take over ([`NEARER_HOLD_SECS`]): since when,
    /// and when last, by `clock`.
    nearer: Option<(f32, f32)>,
    /// The distance in focus has just gone to something nearer the eyes
    /// landed on: the re-projection goes there at once, and the easing in.
    landed_on: bool,
    /// The distance in focus (nearness) `target` was set for, while it is.
    eased_for: Option<f32>,
    /// The correction (radians per eye) being eased towards.
    target: f32,
    /// A smaller correction decided on, the distance in focus (nearness) it
    /// is for, and since when by `clock`: eased to once
    /// [`EASE_OUT_HOLD_SECS`] have passed with nothing nearer looked at
    /// again.
    lower: Option<(f32, f32, f32)>,
    /// Current correction (radians per eye) and its rate of change.
    corr: f32,
    vel: f32,
    /// The re-projection's distance as nearness (1/metres), gliding to
    /// `near_target`, which follows the distance in focus within
    /// [`REDRAW_TOLERANCE_RAD`]; 0 until the first step.
    near: f32,
    near_target: f32,
    /// The same for what lies along the line through the eyes
    /// ([`Focus::side_distance`]), following by what is beyond
    /// [`RELIEF_STEP`].
    side: f32,
    side_target: f32,
    last_step_at: Option<Instant>,
    /// When the eyes last landed somewhere else, by `clock`
    /// ([`ComfortFocus::landed`]).
    landed_at: Option<f32>,
    /// Until when, by `clock`, the easing goes out, goes in, and the
    /// re-projection glides at the pace for what the eyes have just landed
    /// on.
    ease_at_once_until: f32,
    ease_in_at_once_until: f32,
    glide_at_once_until: f32,
    /// Since when, by `clock`, the distance in focus is the one it is.
    accepted_since: f32,
    /// Distances lately in focus ([`KNOWN_SECS`], [`KNOWN_MIN_SECS`]): how
    /// near, and when the distance in focus left each, by `clock`.
    known: Vec<(f32, f32)>,
}

impl ComfortFocus {
    /// Starts afresh, as for another video.
    pub fn reset(&mut self) {
        *self = Self {
            eyes: self.eyes,
            ..Self::default()
        };
    }

    /// Takes the read-back patch scores of a completed frame and the lattice
    /// angle `along` where they aimed ([`aim_angles`]), and tells whether
    /// they gave a distance.
    pub fn ingest(&mut self, scores: &[[f32; 2]], along: f32) -> bool {
        let nearness = aim_reading(scores, along);
        self.pending = Some((nearness, along));
        nearness.is_some()
    }

    /// The eyes have jumped and landed somewhere else
    /// ([`AimFilter::take_landed`]): what is read there within
    /// [`LANDING_SECS`] is acted on at once.
    pub fn landed(&mut self) {
        self.landed_at = Some(self.clock);
    }

    fn just_landed(&self) -> bool {
        self.landed_at
            .is_some_and(|t| self.clock - t <= LANDING_SECS)
    }

    /// Whether the aim follows the eyes (`true`) or the head: only the eyes
    /// tell, by landing somewhere, that they have gone to something else.
    pub fn follows_eyes(&mut self, eyes: bool) {
        self.eyes = eyes;
    }

    /// When (by `clock`) the eyes landed for something much nearer than the
    /// distance in focus, read since `since`: no longer than
    /// [`LANDING_SECS`] before that, or since. Only with the aim following
    /// them.
    fn landed_for(&self, since: f32) -> Option<f32> {
        self.landed_at
            .filter(|&t| self.eyes && t >= since - LANDING_SECS)
    }

    /// Whether something much nearer than the distance in focus, read since
    /// `since` (by `clock`) and now as nearness `to`, has stayed long
    /// enough to take over: [`NEARER_HOLD_SECS`] since the eyes landed for
    /// it ([`ComfortFocus::landed_for`]) or with the head aiming, and no
    /// time at all where its distance, within `step`, is one they were on
    /// lately ([`KNOWN_SECS`]); [`UNREAD_HOLD_SECS`] with the eyes resting
    /// where they were, known or not.
    fn stayed(&self, since: f32, to: f32, step: f32) -> bool {
        let clock = self.clock;
        let chosen = if self.eyes {
            self.landed_for(since).map(|t| since.max(t))
        } else {
            Some(since)
        };
        let known = self
            .known
            .iter()
            .any(|&(k, left)| clock - left <= KNOWN_SECS && to <= k * step && k <= to * step);
        chosen.is_some_and(|t| known || clock - t >= NEARER_HOLD_SECS)
            || clock - since >= UNREAD_HOLD_SECS
    }

    /// The distance in focus, metres, and the easing now, radians per eye;
    /// `None` before the first readings have agreed on a distance.
    pub fn in_focus(&self) -> Option<(f32, f32)> {
        self.accepted.map(|a| (1.0 / a, self.corr))
    }

    /// Advances to `now`, with the eyes' [`redraw_lever`] `lever`. Where the
    /// eyes look doesn't enter: the readings come from there, and the eyes
    /// roaming over what is in focus (or a blink) say nothing about how
    /// near it is.
    pub fn step(&mut self, now: Instant, lever: Lever) -> Focus {
        let dt = self
            .last_step_at
            .map(|prev| now.saturating_duration_since(prev).as_secs_f32().min(0.1))
            .unwrap_or(1.0 / 90.0);
        self.last_step_at = Some(now);
        self.clock += dt;
        if self.near == 0.0 {
            self.near = 1.0 / START_DISTANCE;
            self.near_target = self.near;
            self.side = self.near;
            self.side_target = self.near;
        }

        if let Some((Some(n), along)) = self.pending.take() {
            self.read(n.clamp(1.0 / FARTHEST, 1.0 / NEAREST), along);
        }
        // Something farther has stayed in focus: ease out for it.
        if let Some((want, a, since)) = self.lower
            && self.clock - since >= EASE_OUT_HOLD_SECS
        {
            self.target = want;
            self.eased_for = Some(a);
            self.lower = None;
        }
        // The distance in focus not read for a while: let go of the easing,
        // whether nothing was readable or the readings were of something
        // else that didn't take over. It stays the distance in focus, as
        // the nearest guess there is.
        if self.seen.is_none_or(|t| self.clock - t > UNREAD_HOLD_SECS) {
            self.target = 0.0;
            self.eased_for = None;
            self.lower = None;
        }

        // Critically damped spring towards the target, speed-limited: gently
        // in, more promptly out, and either at once for what the eyes have
        // just landed on.
        let (omega, max_speed) = if self.target > self.corr {
            if self.clock < self.ease_in_at_once_until {
                (LANDED_EASE_IN_OMEGA, LANDED_EASE_IN_MAX_SPEED)
            } else {
                (EASE_IN_OMEGA, EASE_IN_MAX_SPEED)
            }
        } else if self.clock < self.ease_at_once_until {
            (LANDED_EASE_OMEGA, LANDED_EASE_MAX_SPEED)
        } else {
            (EASE_OUT_OMEGA, EASE_OUT_MAX_SPEED)
        };
        let x = self.corr - self.target;
        let decay = (-omega * dt).exp();
        let tmp = (self.vel + omega * x) * dt;
        let next = self.target + (x + tmp) * decay;
        let max_step = max_speed * dt;
        let step = (next - self.corr).clamp(-max_step, max_step);
        self.vel = ((self.vel - omega * tmp) * decay).clamp(-max_speed, max_speed);
        let corr = self.corr + step;
        // A target moved mid-glide, towards where it was heading, never makes
        // it swing past and back.
        if (corr - self.target) * x < 0.0 {
            self.corr = self.target;
            self.vel = 0.0;
        } else {
            self.corr = corr;
        }
        self.corr = self.corr.clamp(0.0, MAX_COMFORT_CORR_RAD);
        if self.target == 0.0 && self.corr < 1e-5 {
            self.corr = 0.0;
            self.vel = 0.0;
        }
        // The re-projection follows the distance in focus once keeping its
        // own would misalign what is looked at: all the way, as up and
        // down that pulls it apart, which the eyes can't follow. Twice as
        // much at the video's edges ([`EDGE_FROM`]).
        let tolerance = REDRAW_TOLERANCE_RAD * (1.0 + self.edge);
        let mut followed = false;
        if let Some(a) = self.accepted
            && (a - self.near_target).abs() * lever.length() > tolerance
        {
            self.near_target = a;
            followed = true;
        }
        // Along the line through the eyes it only sets it a little nearer
        // or farther, and follows as the easing does: for a change beyond
        // [`RELIEF_STEP`], by what is beyond it. The eyes going over the
        // parts of something near would otherwise move the pictures
        // against each other at every look.
        if let Some(a) = self.accepted
            && (a - self.side_target).abs() * lever.side > tolerance
        {
            let step = 1.0 + RELIEF_STEP * self.scale.max(1.0);
            let kept = self.side_target.clamp(a / step, a * step);
            if kept != self.side_target {
                self.side_target = kept;
                followed = true;
            }
        }
        // What the eyes have just landed on: there before they have
        // converged on it. Not at the video's edges, where it glides.
        if followed && self.edge < 1.0 && (self.just_landed() || self.landed_on) {
            self.glide_at_once_until = self.clock + LANDED_PACE_SECS;
        }
        self.landed_on = false;
        let glide = if self.clock < self.glide_at_once_until {
            LANDED_GLIDE_SECS + (DISTANCE_GLIDE_SECS - LANDED_GLIDE_SECS) * self.edge
        } else {
            DISTANCE_GLIDE_SECS
        };
        let glided = 1.0 - (-dt / glide).exp();
        self.near += (self.near_target - self.near) * glided;
        self.side += (self.side_target - self.side) * glided;
        Focus {
            relief: self.corr,
            distance: 1.0 / self.near,
            side_distance: 1.0 / self.side,
        }
    }

    /// Folds in a reading of nearness `n` where the eyes are, at lattice
    /// angle `along`, where readings agree within [`AGREE`] times
    /// [`side_scale`] ([`AGREE_SHARE`] of how near, nearer than 0.3 m). The
    /// first two
    /// to agree set the distance in focus; one agreeing with it refines it.
    /// Another distance takes over as soon as the latest two readings that
    /// gave a distance agree on it, both apart from the one in focus and no
    /// more than [`PAIR_SECS`] apart: a lone wrong reading never
    /// moves it, and with the eyes on the edge of something near, readings
    /// going back and forth between it and what is behind keep it where it
    /// is rather than halfway between the two. One more than
    /// [`RELIEF_STEP`] nearer takes over only once it has stayed
    /// ([`ComfortFocus::stayed`]): what passes in front of what is looked
    /// at, and is gone again, moves nothing. Readings with nothing to
    /// match don't come into it, so something that reads only now and then
    /// still takes over.
    fn read(&mut self, n: f32, along: f32) {
        let scale = side_scale(along);
        self.scale = scale;
        self.edge = edge(along);
        let agree = AGREE.max(AGREE_SHARE * n) * scale;
        let clock = self.clock;
        self.recent.retain(|&(_, t)| clock - t <= PAIR_SECS);
        let before = self.recent.last().copied();
        self.recent.push((n, clock));
        if self.recent.len() > RECENT {
            self.recent.remove(0);
        }
        let accepted = self.accepted;
        self.accepted = match accepted {
            None => {
                let agreeing: Vec<f32> = self
                    .recent
                    .iter()
                    .map(|&(p, _)| p)
                    .filter(|p| (p - n).abs() <= agree)
                    .collect();
                (agreeing.len() >= 2).then(|| agreeing.iter().sum::<f32>() / agreeing.len() as f32)
            }
            Some(a) if (n - a).abs() <= agree => {
                // Back on what is in focus: what was nearer has gone.
                self.nearer = None;
                Some(a + (n - a) * REFINE)
            }
            Some(a) => Some(match before {
                Some((p, first)) if (p - n).abs() <= agree && (p - a).abs() > agree => {
                    let to = 0.5 * (p + n);
                    // Farther, or no more than a little nearer than what
                    // is in focus or what the picture is still eased for
                    // (back from a glance at something farther): at once.
                    let step = 1.0 + RELIEF_STEP * scale;
                    let eased = self.eased_for.is_some_and(|e| to <= e * step);
                    let takes_over = if to <= a * step || eased {
                        true
                    } else {
                        // Much nearer: once it has stayed. Read since the
                        // first of these two readings, or since before them
                        // if it was last read no more than [`PAIR_SECS`] ago.
                        let since = match self.nearer {
                            Some((since, last)) if clock - last <= PAIR_SECS => since,
                            _ => first,
                        };
                        self.nearer = Some((since, clock));
                        let stayed = self.stayed(since, to, step);
                        // The eyes landed for it: drawn and eased for at
                        // the pace for what they have just landed on.
                        self.landed_on = stayed && self.landed_for(since).is_some();
                        stayed
                    };
                    if takes_over {
                        self.nearer = None;
                        // What the eyes were on for a while is known for a
                        // while after.
                        if clock - self.accepted_since >= KNOWN_MIN_SECS {
                            self.known.retain(|&(k, left)| {
                                clock - left <= KNOWN_SECS && (k > a * step || a > k * step)
                            });
                            if self.known.len() == KNOWN {
                                self.known.remove(0);
                            }
                            self.known.push((a, clock));
                        }
                        self.accepted_since = clock;
                        to
                    } else {
                        a
                    }
                }
                _ => a,
            }),
        };
        let Some(a) = self.accepted else {
            return;
        };
        if accepted.is_none() {
            self.accepted_since = clock;
        }
        // Eased for what is in focus once this reading sees it, and only for
        // a change of distance of [`RELIEF_STEP`] or more, wider to the side
        // (or out to none): more at once, less once it has lasted
        // ([`EASE_OUT_HOLD_SECS`]).
        if (n - a).abs() > agree {
            return;
        }
        self.seen = Some(clock);
        let step = 1.0 + RELIEF_STEP * scale;
        let moved = self.eased_for.is_none_or(|e| a > e * step || a * step < e);
        // What to ease for: the distance in focus when coming from no
        // easing, and where it needs none; else the nearest to what it is
        // eased for now that is within the step of the distance in focus.
        let ease_for = match self.eased_for {
            Some(e) if self.target > 0.0 && relief_at(a) > 0.0 => e.clamp(a / step, a * step),
            _ => a,
        };
        let want = relief_at(ease_for);
        if !moved && !(want == 0.0 && self.target > 0.0) {
            // Back on what it is eased for: a glance away is over.
            self.lower = None;
        } else if want >= self.target {
            // Something nearer the eyes have just landed on: eased in for
            // while they converge on it, so they stop where it will stay.
            if want > self.target && (self.just_landed() || self.landed_on) {
                self.ease_in_at_once_until = self.clock + LANDED_PACE_SECS;
            }
            self.target = want;
            self.eased_for = Some(ease_for);
            self.lower = None;
        } else if self.just_landed() {
            // Something farther the eyes have just landed on: eased out for
            // at once, so they converge on it where it will stay.
            self.target = want;
            self.eased_for = Some(ease_for);
            self.lower = None;
            self.ease_at_once_until = self.clock + LANDED_PACE_SECS;
        } else {
            let since = self.lower.map_or(clock, |(.., t)| t);
            self.lower = Some((want, ease_for, since));
        }
    }
}

/// The head's turn in the play space from the axes the eye views give it:
/// `back` (+Z, opposite where it looks) and `up`, neither needing to be
/// unit or exactly square to the other.
pub(crate) fn head_turn(back: Vec3, up: Vec3) -> Quat {
    let z = back.normalize_or(Vec3::Z);
    let x = up.cross(z).normalize_or(Vec3::X);
    Quat::from_mat3(&Mat3::from_cols(x, z.cross(x), z)).normalize()
}

/// A direction in head axes (-Z ahead) as yaw and pitch, radians: yaw from
/// ahead towards +X (right), pitch up.
fn yaw_pitch(d: Vec3) -> Vec2 {
    Vec2::new(d.x.atan2(-d.z), d.y.clamp(-1.0, 1.0).asin())
}

/// The direction (head axes) of [`yaw_pitch`] `yp`.
fn from_yaw_pitch(yp: Vec2) -> Vec3 {
    let (sy, cy) = yp.x.sin_cos();
    let (sp, cp) = yp.y.sin_cos();
    Vec3::new(sy * cp, sp, -cy * cp)
}

/// A first-order low-pass's weight for the newest value at cutoff `hz`
/// over `dt` seconds.
fn low_pass(hz: f32, dt: f32) -> f32 {
    let tau = 1.0 / (std::f32::consts::TAU * hz);
    1.0 / (1.0 + tau / dt)
}

/// Picks the direction comfort focus measures in: the tracked gaze while it
/// looks believable, smoothed, held where it was for the length of a blink,
/// then the head's forward. The tracker's ray jitters by about a degree
/// while the eyes rest, which would move the measurement on and off the
/// edge of what is looked at; the smoothing is in head axes, where the
/// tracker's jitter is, so a gaze held through a blink turns with the head.
#[derive(Default)]
pub(crate) struct AimFilter {
    /// The smoothed gaze (head axes, as [`yaw_pitch`]), and its speed.
    smoothed: Option<Vec2>,
    speed: Vec2,
    /// When the last gaze sample was taken in, and where it was.
    last_sample: Option<Instant>,
    raw: Option<Vec2>,
    from: AimFrom,
    /// Where the aim was when the eyes jumped away from it, until they
    /// land, and for how many samples running the smoothing has kept up
    /// with them since.
    jump_from: Option<Vec2>,
    caught_up: u8,
    /// The eyes have landed somewhere else since the last
    /// [`AimFilter::take_landed`].
    landed: bool,
}

/// What the aim followed, the last time it was asked for.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum AimFrom {
    /// Where the head points: no tracker, or the eyes lost for longer than
    /// a blink.
    #[default]
    Head,
    /// The tracked gaze.
    Eyes,
    /// Where the eyes were, through a blink.
    Held,
}

impl AimFilter {
    /// The aim at `now` (play space) from the tracker's `gaze` (play space),
    /// when it has one, for a head turned `head`.
    pub fn aim(&mut self, now: Instant, gaze: Option<Vec3>, head: Quat) -> Vec3 {
        let tracked = gaze
            .and_then(Vec3::try_normalize)
            .filter(|g| g.angle_between(head * Vec3::NEG_Z) <= PLAUSIBLE_GAZE_RAD);
        if let Some(g) = tracked {
            self.take(now, yaw_pitch(head.inverse() * g));
        }
        match (self.smoothed, self.last_sample) {
            (Some(yp), Some(t)) if now.saturating_duration_since(t).as_secs_f32() <= BLINK_SECS => {
                self.from = if tracked.is_some() {
                    AimFrom::Eyes
                } else {
                    AimFrom::Held
                };
                head * from_yaw_pitch(yp)
            }
            _ => {
                self.from = AimFrom::Head;
                head * Vec3::NEG_Z
            }
        }
    }

    /// What the last [`AimFilter::aim`] followed.
    pub fn from(&self) -> AimFrom {
        self.from
    }

    /// Whether the aim is where the eyes are, rather than catching up with
    /// a jump: in a jump it sweeps across whatever lies between, which is
    /// not worth measuring. Always, aiming with the head.
    pub fn settled(&self) -> bool {
        match (self.smoothed, self.raw) {
            (Some(s), Some(r)) => self.jump_from.is_none() && s.distance(r) <= SETTLED_RAD,
            _ => true,
        }
    }

    /// Whether the eyes have jumped and landed somewhere else since the
    /// last call: the aim is there already, and what lies there is what
    /// they are about to converge on ([`ComfortFocus::landed`]).
    pub fn take_landed(&mut self) -> bool {
        std::mem::take(&mut self.landed)
    }

    /// Smooths in one sample, `yp` (head axes), by the One Euro filter. A
    /// jump of the eyes is taken at once where it lands, rather than
    /// smoothed to over a few frames: there the gaze is new, with nothing
    /// before it to smooth with.
    fn take(&mut self, now: Instant, yp: Vec2) {
        let dt = self
            .last_sample
            .map(|t| now.saturating_duration_since(t).as_secs_f32());
        self.last_sample = Some(now);
        let before = self.raw.replace(yp);
        let Some((x, dt)) = self.smoothed.zip(dt).filter(|&(_, dt)| dt <= GAZE_GAP_SECS) else {
            // Afresh after a gap: the eyes may be somewhere else by now.
            self.landed |= self.smoothed.is_some_and(|s| s.distance(yp) > SETTLED_RAD);
            self.smoothed = Some(yp);
            self.speed = Vec2::ZERO;
            self.jump_from = None;
            self.caught_up = 0;
            return;
        };
        // A jump: the gaze leaves where the aim rests.
        if self.jump_from.is_none() && x.distance(yp) > SETTLED_RAD {
            self.jump_from = Some(x);
        }
        let dt = dt.max(1e-4);
        self.speed += ((yp - x) / dt - self.speed) * low_pass(GAZE_SPEED_CUTOFF_HZ, dt);
        let cutoff = GAZE_MIN_CUTOFF_HZ + GAZE_BETA * self.speed.length();
        let next = x + (yp - x) * low_pass(cutoff, dt);
        self.smoothed = Some(next);
        // It has landed once the smoothing has caught up with the gaze and
        // two samples running are close together (one sample can't tell a
        // jump's first step from its end), or the smoothing has kept up for
        // three frames, should the tracker's jitter keep its samples apart.
        // The aim goes where the eyes are at once, rather than over the
        // tenth of a second the last degree or two would take. Back where it
        // left from, it was a stray sample.
        let Some(from) = self.jump_from else {
            return;
        };
        if next.distance(yp) > SETTLED_RAD {
            self.caught_up = 0;
            return;
        }
        self.caught_up += 1;
        let close = before.filter(|b| b.distance(yp) <= LANDED_RAD);
        if close.is_some() || self.caught_up >= 3 {
            self.jump_from = None;
            self.caught_up = 0;
            let at = close.map_or(yp, |b| (b + yp) * 0.5);
            self.landed = from.distance(at) > SETTLED_RAD;
            self.smoothed = Some(if self.landed { at } else { from });
            self.speed = Vec2::ZERO;
        }
    }
}

/// What comfort focus did over a stretch of frames, for one line of the log:
/// how often it measured and how many of those gave a distance, what is in
/// focus, how often the eyes aimed, and what a measurement costs the GPU.
#[derive(Default)]
pub(crate) struct FocusLog {
    since: Option<Instant>,
    /// Frames drawn with comfort focus.
    frames: u32,
    /// Of them: aimed by the eyes, and measured.
    eyes: u32,
    measured: u32,
    /// Measurements read back, and of them those that gave a distance.
    read: u32,
    gave: u32,
    /// The eye passes' GPU time, milliseconds summed and frames, with a
    /// measurement and without.
    gpu: [(f32, u32); 2],
}

impl FocusLog {
    /// A frame drawn with comfort focus at `now`: what its aim followed, and
    /// whether it `measured`.
    pub fn frame(&mut self, now: Instant, from: AimFrom, measured: bool) {
        self.since.get_or_insert(now);
        self.frames += 1;
        self.eyes += (from == AimFrom::Eyes) as u32;
        self.measured += measured as u32;
    }

    /// A measurement read back, which gave a distance (`gave`,
    /// [`ComfortFocus::ingest`]) or didn't.
    pub fn read_back(&mut self, gave: bool) {
        self.read += 1;
        self.gave += gave as u32;
    }

    /// The eye passes of a frame drawn with comfort focus took `ms` on the
    /// GPU, with a measurement (`measured`) or without.
    pub fn gpu(&mut self, measured: bool, ms: f32) {
        let (sum, n) = &mut self.gpu[measured as usize];
        *sum += ms;
        *n += 1;
    }

    /// The frames since the last call as one line, with what is in focus at
    /// `now` ([`ComfortFocus::in_focus`]); `None` when none was drawn with
    /// comfort focus.
    pub fn take_line(&mut self, now: Instant, in_focus: Option<(f32, f32)>) -> Option<String> {
        let w = std::mem::take(self);
        let secs = now.saturating_duration_since(w.since?).as_secs_f32();
        if w.frames == 0 || secs <= 0.0 {
            return None;
        }
        let of = |n: u32, all: u32| 100.0 * n as f32 / all.max(1) as f32;
        let mut line = format!(
            "comfort focus: {:.0} measurements a second, {} read back, {:.0}% gave a distance",
            w.measured as f32 / secs,
            w.read,
            of(w.gave, w.read),
        );
        line += &match in_focus {
            Some((distance, relief)) => {
                format!("; now {distance:.2} m, eased {:.2}°", relief.to_degrees())
            }
            None => "; nothing in focus yet".to_string(),
        };
        line += &format!(
            "; aim by the eyes in {:.0}% of {} frames",
            of(w.eyes, w.frames),
            w.frames,
        );
        if let [(without, n0 @ 1..), (with, n1 @ 1..)] = w.gpu {
            line += &format!(
                "; eye passes {:.2} GPU ms with a measurement, {:.2} without",
                with / n1 as f32,
                without / n0 as f32
            );
        }
        Some(line)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    /// Typical stereo camera baseline in metres.
    const CAMERA_BASELINE: f32 = 0.065;

    fn kappa_for(distance: f32) -> f32 {
        (CAMERA_BASELINE / (2.0 * distance)).atan()
    }

    fn curve(distance: f32, std_dev: f32) -> [[f32; 2]; CANDIDATES] {
        let true_kappa = kappa_for(distance);
        let mut scores = [[0.0f32; 2]; CANDIDATES];
        for (i, slot) in scores.iter_mut().enumerate() {
            let diff = (kappa_at(i as f32) - true_kappa) / 0.012;
            *slot = [0.95 * (-0.5 * diff * diff).exp(), std_dev];
        }
        scores
    }

    /// Nothing to match: no texture.
    fn blank() -> [[f32; 2]; CANDIDATES] {
        curve(1.0, 0.001)
    }

    /// The cluster's curves: `aim` for the middle patch, `ring` round it.
    fn cluster(aim: [[f32; 2]; CANDIDATES], ring: [[f32; 2]; CANDIDATES]) -> Vec<[f32; 2]> {
        (0..PATCHES)
            .flat_map(|i| if i == AIM_PATCH { aim } else { ring })
            .collect()
    }

    fn at(distance: f32) -> Vec<[f32; 2]> {
        cluster(curve(distance, 0.08), curve(distance, 0.08))
    }

    /// The distance read for something `distance` metres straight ahead,
    /// filmed with a 65 mm baseline.
    fn read_as(distance: f32) -> f32 {
        1.0 / nearness(0.0, kappa_for(distance))
    }

    /// The correction for what lies at the aim of `scores`.
    fn aim_correction(scores: &[[f32; 2]]) -> Option<f32> {
        aim_reading(scores, 0.0).map(relief_at)
    }

    const FRAME: Duration = Duration::from_millis(11);
    /// A [`redraw_lever`] so long the re-projection follows every change.
    const LEVER: Lever = Lever { side: 0.0, up: 1.0 };
    const AHEAD: Vec3 = Vec3::NEG_Z;

    /// `degrees` to the right of straight ahead.
    fn right_of_ahead(degrees: f32) -> Vec3 {
        Quat::from_rotation_y(-degrees.to_radians()) * AHEAD
    }

    /// Feeds `scores`, one reading a frame, for `frames` frames, returning
    /// every frame's focus.
    fn run_focus(
        cf: &mut ComfortFocus,
        t: &mut Instant,
        scores: &[[f32; 2]],
        frames: usize,
    ) -> Vec<Focus> {
        (0..frames)
            .map(|_| {
                *t += FRAME;
                cf.ingest(scores, 0.0);
                cf.step(*t, LEVER)
            })
            .collect()
    }

    /// Whether `a` is the picture `b` is, but for the last of a glide.
    fn same(a: &Focus, b: &Focus) -> bool {
        (a.relief - b.relief).abs() < 1e-5 && (a.distance - b.distance).abs() < 1e-3 * b.distance
    }

    /// How far `f` is drawn from where `from` was towards `to` metres: the
    /// share of the way there, in nearness.
    fn gone(from: &Focus, f: &Focus, to: f32) -> f32 {
        (1.0 / f.distance - 1.0 / from.distance) / (1.0 / to - 1.0 / from.distance)
    }

    /// The same, the easing only.
    fn run(cf: &mut ComfortFocus, t: &mut Instant, scores: &[[f32; 2]], frames: usize) -> Vec<f32> {
        run_focus(cf, t, scores, frames)
            .iter()
            .map(|f| f.relief)
            .collect()
    }

    #[test]
    fn parabolic_refinement_recovers_sub_step_disparity() {
        for &d in &[0.11f32, 0.15, 0.2, 0.33, 0.45, 0.7, 1.6, 3.5] {
            let k = estimate_kappa(&curve(d, 0.08), KAPPA_FAR).expect("valid lock");
            let err = (k - kappa_for(d)).abs();
            assert!(err < 0.0005, "{d} m: kappa {k} vs {}", kappa_for(d));
        }
    }

    #[test]
    fn rejects_featureless_or_ambiguous_patches() {
        assert_eq!(estimate_kappa(&blank(), KAPPA_FAR), Err(Miss::Plain));
        assert_eq!(
            estimate_kappa(&[[0.85, 0.05]; CANDIDATES], KAPPA_FAR),
            Err(Miss::Weak)
        );
    }

    #[test]
    fn a_match_far_out_with_nothing_to_match_at_the_aim_is_no_reading() {
        // The aim on something plain: only windows 13° and more to either
        // side have texture, and two of them happen to look alike.
        let mut c = curve(0.12, 0.08);
        for s in &mut c[..CANDIDATES / BANDS] {
            s[1] = 0.001;
        }
        assert_eq!(estimate_kappa(&c, KAPPA_FAR), Err(Miss::Plain));
        // With texture at the aim too, the same match is something 12 cm
        // away.
        let k = estimate_kappa(&curve(0.12, 0.08), KAPPA_FAR).expect("valid");
        assert!((k - kappa_for(0.12)).abs() < 0.0005, "{k}");
        // Enough texture at the aim is half the first band's candidates.
        let mut c = curve(0.12, 0.08);
        for s in &mut c[MIN_VALID..CANDIDATES / BANDS] {
            s[1] = 0.001;
        }
        assert!(estimate_kappa(&c, KAPPA_FAR).is_ok());
    }

    #[test]
    fn near_peak_that_barely_beats_far_reads_as_far() {
        // Just nearer than the threshold: the far side of its own peak
        // scores within the margin.
        let k = estimate_kappa(&curve(0.83, 0.08), KAPPA_FAR).expect("valid");
        assert!(kappa_for(0.83) > KAPPA_FAR);
        assert_eq!(correction_from_kappa(k), 0.0);
    }

    #[test]
    fn a_pattern_matching_at_two_depths_gives_no_reading() {
        // A second peak nearly as good, near or far: a repeating pattern.
        let mut c = curve(0.4, 0.08);
        c[3][0] = 0.93;
        assert_eq!(estimate_kappa(&c, KAPPA_FAR), Err(Miss::Ambiguous));
        let mut c = curve(1.5, 0.08);
        let near = curve(0.45, 0.08);
        for (i, s) in c.iter_mut().enumerate() {
            s[0] = s[0].max(0.9 / 0.95 * near[i][0]);
        }
        assert_eq!(estimate_kappa(&c, KAPPA_FAR), Err(Miss::Ambiguous));
        // A clearly lower one is the pattern's texture: read as usual.
        let mut c = curve(1.5, 0.08);
        for (i, s) in c.iter_mut().enumerate() {
            s[0] = s[0].max(0.7 / 0.95 * near[i][0]);
        }
        let k = estimate_kappa(&c, KAPPA_FAR).expect("valid");
        assert!((k - kappa_for(1.5)).abs() < 0.0005, "{k}");
    }

    #[test]
    fn correction_is_zero_far_and_grows_only_for_near_objects() {
        for &d in &[0.86f32, 1.0, 2.0, 5.0, 50.0] {
            assert_eq!(correction_from_kappa(kappa_for(d)), 0.0, "{d} m");
        }
        let mut prev = 0.0;
        for &d in &[0.8f32, 0.6, 0.45, 0.35, 0.3] {
            let c = correction_from_kappa(kappa_for(d));
            assert!(c > prev, "{d} m: {c} <= {prev}");
            assert!(c < kappa_for(d) - KAPPA_FAR + 1e-6);
            prev = c;
        }
        assert!(correction_from_kappa(1.0) <= MAX_COMFORT_CORR_RAD);
    }

    #[test]
    fn triangulation_is_exact_wherever_the_point_is() {
        // The lattice angles of a direction (`aim_angles`).
        let angles = |p: Vec3| {
            let d = p.normalize();
            [d.x.asin(), d.y.atan2(-d.z)]
        };
        let half = Vec3::X * (0.5 * LENS_SPACING);
        for d in [0.3f32, 0.5, 1.0, 3.0] {
            for (along, tilt) in [
                (0.0f32, 0.0f32),
                (35.0, 0.0),
                (20.0, -20.0),
                (35.0, -35.0),
                (-50.0, 30.0),
                (60.0, -10.0),
            ] {
                let (a, t) = (along.to_radians(), tilt.to_radians());
                let p = Vec3::new(a.sin(), a.cos() * t.sin(), -a.cos() * t.cos()) * d;
                // The left camera is at -x, the right one at +x.
                let [l, l_tilt] = angles(p + half);
                let [r, r_tilt] = angles(p - half);
                assert!((l_tilt - r_tilt).abs() < 1e-5, "both on one row");
                let n = nearness((l + r) / 2.0, (l - r) / 2.0);
                assert!(
                    (n * d - 1.0).abs() < 1e-3,
                    "{d} m at {along}°, {tilt}° read as {} m",
                    1.0 / n
                );
            }
        }
        // The lens spacing assumed (63 mm) against the scene's (65 mm).
        for d in [0.5f32, 1.0, 3.0, 10.0] {
            let got = read_as(d);
            assert!(
                (got / d - 0.063 / 0.065).abs() < 0.01,
                "{d} m read as {got} m"
            );
        }
        assert_eq!(nearness(0.0, 0.0), 0.0);
        assert_eq!(nearness(0.0, -0.01), 0.0);
    }

    #[test]
    fn aims_too_near_the_baseline_axis_are_not_measured() {
        let video = crate::VideoParams {
            format: fp_core::format::VideoFormat::new(
                fp_core::format::Projection::EQUIRECT_180,
                fp_core::format::StereoLayout::SideBySide,
            ),
            ..Default::default()
        };
        let p = crate::params::build(&video, None, [glam::Mat4::IDENTITY; 2]);
        let down = Quat::from_rotation_x(-30f32.to_radians());
        let [along, tilt] = aim_angles(&p, down * right_of_ahead(35.0)).unwrap();
        assert!((along - 35f32.to_radians()).abs() < 1e-4, "{along}");
        assert!((tilt + 30f32.to_radians()).abs() < 1e-4, "{tilt}");
        assert!(aim_angles(&p, right_of_ahead(64.0)).is_some());
        assert!(aim_angles(&p, right_of_ahead(66.0)).is_none());
        assert!(aim_angles(&p, right_of_ahead(-66.0)).is_none());
    }

    #[test]
    fn what_lies_at_the_aim_sets_the_correction_whatever_surrounds_it() {
        let near = correction_from_kappa(kappa_for(0.4));
        // A close thing looked at against a far wall: eased in full.
        let c = aim_correction(&cluster(curve(0.4, 0.08), curve(3.0, 0.08))).unwrap();
        assert!((c - near).abs() < 1e-3, "{c} vs {near}");
        // The far wall looked at past close things: left alone.
        let c = aim_correction(&cluster(curve(3.0, 0.08), curve(0.4, 0.08))).unwrap();
        assert_eq!(c, 0.0);
    }

    #[test]
    fn with_nothing_to_match_at_the_aim_the_ring_decides() {
        let near = correction_from_kappa(kappa_for(0.4));
        let c = aim_correction(&cluster(blank(), curve(0.4, 0.08))).unwrap();
        assert!((c - near).abs() < 1e-3, "{c} vs {near}");
        // Too few of the ring readable: no reading.
        let mut scores = cluster(blank(), blank());
        scores[..2 * CANDIDATES].copy_from_slice(&[curve(0.4, 0.08), curve(0.4, 0.08)].concat());
        assert_eq!(aim_correction(&scores), None);
    }

    #[test]
    fn far_aims_are_left_untouched() {
        let mut cf = ComfortFocus::default();
        let mut t = Instant::now();
        for d in [1.0f32, 3.0, 1000.0] {
            let out = run(&mut cf, &mut t, &at(d), 60);
            assert!(out.iter().all(|&c| c == 0.0), "{d} m moved: {out:?}");
        }
    }

    #[test]
    fn eases_in_gently_and_lets_go_when_the_eyes_stay_on_something_farther() {
        let mut cf = ComfortFocus::default();
        let mut t = Instant::now();
        let want = correction_from_kappa(kappa_for(0.4));
        let out = run(&mut cf, &mut t, &at(0.4), 180);
        let max_step = EASE_IN_MAX_SPEED * FRAME.as_secs_f32() + 1e-6;
        let mut prev = 0.0;
        for &c in &out {
            assert!(c >= prev - 1e-6, "overshoot/oscillation: {out:?}");
            assert!(c - prev <= max_step, "too fast: {}", c - prev);
            prev = c;
        }
        // Under half way in 0.4 s, mostly there by 1.05 s, settled by 2 s.
        assert!(out[36] < 0.5 * want, "abrupt: {}", out[36]);
        assert!(out[95] > 0.9 * want, "slow: {}", out[95]);
        assert!((out[179] - want).abs() < 0.001, "{} vs {want}", out[179]);

        // The eyes move to the wall behind and stay: held a quarter of a
        // second, then within 0.1° of none by 0.9 s.
        let back = run(&mut cf, &mut t, &at(3.0), 82);
        assert!(
            back[..22].iter().all(|&c| (c - want).abs() < 0.001),
            "{back:?}"
        );
        assert!(*back.last().unwrap() < 0.002, "{back:?}");
    }

    #[test]
    fn a_glance_at_something_farther_and_back_leaves_it_be() {
        let mut cf = ComfortFocus::default();
        let mut t = Instant::now();
        let held = *run(&mut cf, &mut t, &at(0.4), 300).last().unwrap();
        let mut out = Vec::new();
        for _ in 0..4 {
            // The wall for a sixth of a second, then back.
            out.extend(run(&mut cf, &mut t, &at(3.0), 15));
            out.extend(run(&mut cf, &mut t, &at(0.4), 40));
        }
        assert!(out.iter().all(|&c| (c - held).abs() < 1e-4), "{out:?}");
    }

    #[test]
    fn a_change_of_mind_mid_glide_never_swings_past() {
        let mut cf = ComfortFocus::default();
        let mut t = Instant::now();
        run(&mut cf, &mut t, &at(0.4), 300);
        // Eased a fifth short of something much nearer, and a fifth short of
        // the first again from that side.
        let step = 1.0 + RELIEF_STEP;
        let low = correction_from_kappa(kappa_for(0.4));
        let high = relief_at(1.0 / read_as(0.25) / step);
        let rest = relief_at(1.0 / read_as(0.4) * step);
        assert!(low < rest && rest < high, "{low} {rest} {high}");
        // Something nearer for a fifth of a second past where it takes over,
        // mid-glide, then the first again for good.
        let hold = (NEARER_HOLD_SECS / FRAME.as_secs_f32()) as usize;
        let mut out = run(&mut cf, &mut t, &at(0.25), hold + 18);
        let back = run(&mut cf, &mut t, &at(0.4), 200);
        out.extend(&back);
        assert!(out.iter().all(|&c| c <= high + 1e-5), "{out:?}");
        assert!(back.iter().all(|&c| c >= low - 1e-5), "{back:?}");
        assert!((back.last().unwrap() - rest).abs() < 1e-4, "{back:?}");
    }

    #[test]
    fn the_distance_in_focus_glides_to_what_is_looked_at_and_stays_without_a_reading() {
        let mut cf = ComfortFocus::default();
        let mut t = Instant::now();
        let first = run_focus(&mut cf, &mut t, &cluster(blank(), blank()), 1);
        assert_eq!(first[0].distance, START_DISTANCE);
        let want = read_as(0.4);
        let out = run_focus(&mut cf, &mut t, &at(0.4), 150);
        // In nearness, 90% of the way within 0.75 s, there by 1.65 s.
        let n = |d: f32| 1.0 / d;
        let from = n(START_DISTANCE);
        assert!(
            n(out[68].distance) - from > 0.9 * (n(want) - from),
            "{:?}",
            out[68]
        );
        assert!((out[149].distance - want).abs() < 0.01, "{:?}", out[149]);
        // Looking away at something unreadable keeps it, even once the
        // easing has let go.
        let away = run_focus(&mut cf, &mut t, &cluster(blank(), blank()), 180);
        assert!((away.last().unwrap().distance - want).abs() < 0.01);
        assert!(away.last().unwrap().relief < 1e-3, "{:?}", away.last());
        // A far wall moves it out.
        let wall = run_focus(&mut cf, &mut t, &at(3.0), 150);
        let far = read_as(3.0);
        assert!(
            (wall.last().unwrap().distance - far).abs() < 0.05 * far,
            "{:?}",
            wall.last()
        );
    }

    #[test]
    fn nothing_readable_holds_it_for_a_second_then_lets_go() {
        let mut cf = ComfortFocus::default();
        let mut t = Instant::now();
        let held = *run(&mut cf, &mut t, &at(0.4), 300).last().unwrap();
        assert!(held > 0.0);
        // A plain spot, nothing to match for most of a second.
        let blank = cluster(blank(), blank());
        let out = run(&mut cf, &mut t, &blank, 85);
        assert!(out.iter().all(|&c| (c - held).abs() < 1e-4), "{out:?}");
        // Longer: within 0.1° of none 0.6 s on.
        let out = run(&mut cf, &mut t, &blank, 60);
        assert!(*out.last().unwrap() < 0.002, "{out:?}");
    }

    #[test]
    fn a_stalled_frame_neither_lets_go_nor_eases_out_by_itself() {
        let blank = cluster(blank(), blank());
        // Nothing readable, and a frame 2 s late: it counts as a tenth of a
        // second, so the easing holds for about as long after it as it
        // would have without, then lets go.
        let mut cf = ComfortFocus::default();
        let mut t = Instant::now();
        let held = *run(&mut cf, &mut t, &at(0.4), 300).last().unwrap();
        t += Duration::from_secs(2);
        let out = run(&mut cf, &mut t, &blank, 76);
        assert!(out.iter().all(|&c| (c - held).abs() < 1e-4), "{out:?}");
        let out = run(&mut cf, &mut t, &blank, 70);
        assert!(*out.last().unwrap() < 0.002, "{out:?}");
        // The eyes on the wall behind, and the next frame 2 s late: it
        // eases out a quarter of a second after the wall took over, by the
        // same count, not at the late frame.
        let mut cf = ComfortFocus::default();
        let held = *run(&mut cf, &mut t, &at(0.4), 300).last().unwrap();
        run(&mut cf, &mut t, &at(3.0), 2);
        t += Duration::from_secs(2);
        let out = run(&mut cf, &mut t, &at(3.0), 12);
        assert!(out.iter().all(|&c| (c - held).abs() < 1e-4), "{out:?}");
        let out = run(&mut cf, &mut t, &at(3.0), 70);
        assert!(*out.last().unwrap() < 0.002, "{out:?}");
    }

    #[test]
    fn the_eyes_roaming_over_what_is_in_focus_keep_it_still() {
        let mut cf = ComfortFocus::default();
        let mut t = Instant::now();
        let held = *run(&mut cf, &mut t, &at(0.4), 300).last().unwrap();
        // Over one thing: readings of it between plain spots with nothing to
        // match, for up to a third of a second at a time, for 5 s.
        let blank = cluster(blank(), blank());
        let mut out = Vec::new();
        for gap in [5, 30, 12, 25, 8, 30, 3, 20].repeat(3) {
            out.extend(run(&mut cf, &mut t, &blank, gap));
            out.extend(run(&mut cf, &mut t, &at(0.4), 2));
        }
        assert!(out.iter().all(|&c| (c - held).abs() < 1e-4), "{out:?}");
    }

    #[test]
    fn the_easing_follows_only_a_change_of_a_fifth_or_more() {
        let mut cf = ComfortFocus::default();
        let mut t = Instant::now();
        let held = *run(&mut cf, &mut t, &at(0.4), 300).last().unwrap();
        // The eyes over nearer and farther parts of one thing: the distance in
        // focus follows them, the easing doesn't.
        let mut out = Vec::new();
        for d in [0.37f32, 0.43, 0.38, 0.42, 0.4].repeat(2) {
            out.extend(run_focus(&mut cf, &mut t, &at(d), 60));
            let want = read_as(d);
            let got = out.last().unwrap().distance;
            assert!((got - want).abs() < 0.01, "{got} m vs {want} m");
        }
        assert!(
            out.iter().all(|f| (f.relief - held).abs() < 1e-4),
            "{out:?}"
        );
        // Something a quarter nearer: eased more, by what is beyond the
        // fifth.
        let nearer = *run_focus(&mut cf, &mut t, &at(0.3), 180).last().unwrap();
        let want = relief_at(1.0 / read_as(0.3) / (1.0 + RELIEF_STEP));
        assert!(want > held + 0.005, "{want} vs {held}");
        assert!((nearer.relief - want).abs() < 0.001, "{nearer:?} vs {want}");
        // Back and forth between the two from then on moves nothing.
        let mut out = Vec::new();
        for d in [0.4f32, 0.3, 0.4, 0.3, 0.4] {
            out.extend(run_focus(&mut cf, &mut t, &at(d), 60));
        }
        assert!(
            out.iter().all(|f| (f.relief - nearer.relief).abs() < 1e-3),
            "{out:?}"
        );
    }

    #[test]
    fn another_distance_takes_over_on_the_second_reading_of_it() {
        let mut cf = ComfortFocus::default();
        let mut t = Instant::now();
        run(&mut cf, &mut t, &at(0.4), 300);
        let near = cf.accepted.unwrap();
        run(&mut cf, &mut t, &at(3.0), 1);
        assert_eq!(cf.accepted, Some(near));
        run(&mut cf, &mut t, &at(3.0), 1);
        let far = 1.0 / read_as(3.0);
        assert!(
            (cf.accepted.unwrap() - far).abs() < 0.01,
            "{:?}",
            cf.accepted
        );
    }

    /// Feeds readings of nearness `n` at lattice angle `along` (radians),
    /// one a frame, for `frames` frames.
    fn feed(cf: &mut ComfortFocus, t: &mut Instant, n: f32, along: f32, frames: usize) {
        for _ in 0..frames {
            *t += FRAME;
            cf.pending = Some((Some(n), along));
            cf.step(*t, LEVER);
        }
    }

    /// Steps `frames` frames with nothing read.
    fn feed_nothing(cf: &mut ComfortFocus, t: &mut Instant, frames: usize) {
        for _ in 0..frames {
            *t += FRAME;
            cf.step(*t, LEVER);
        }
    }

    #[test]
    fn to_the_side_where_readings_are_less_precise_it_takes_more_to_move_it() {
        let side = 50f32.to_radians();
        assert!((side_scale(side) - 1.556).abs() < 0.01);
        assert!((side_scale(0.0) - 1.0).abs() < 1e-6);
        assert!(side_scale(80f32.to_radians()) < 2.4);
        let near = 2.5;
        for along in [0.0, side] {
            let mut cf = ComfortFocus::default();
            let mut t = Instant::now();
            feed(&mut cf, &mut t, near, along, 300);
            let eased = cf.target;
            assert!(eased > 0.0);
            // Two readings 1.4 times [`AGREE`] nearer: another distance
            // straight ahead, the same one less precisely read to the side.
            let n = near + 1.4 * AGREE;
            feed(&mut cf, &mut t, n, along, 2);
            let a = cf.accepted.unwrap();
            if along == 0.0 {
                assert!((a - n).abs() < 1e-5, "{a} vs {n}");
            } else {
                assert!(a > near && n - a > 0.01, "{a} between {near} and {n}");
            }
            // A quarter nearer: eased for straight ahead, not to the side.
            feed(&mut cf, &mut t, 1.25 * near, along, 300);
            assert_eq!(cf.target > eased, along == 0.0, "{along}: {}", cf.target);
        }
    }

    #[test]
    fn the_re_projection_follows_only_changes_that_would_misalign_what_is_looked_at() {
        let half = 0.5 * LENS_SPACING;
        // Lying down, the eyes one above the other.
        let lying = Lever {
            side: half,
            up: half,
        };
        // The head turned 45° to one side of the video: the pictures only
        // move towards or away from each other.
        let turned = Lever {
            side: half * (1.0 - std::f32::consts::FRAC_1_SQRT_2),
            up: 0.0,
        };
        let step = 1.0 + RELIEF_STEP;
        for lever in [lying, turned, Lever::default()] {
            let mut cf = ComfortFocus::default();
            let mut t = Instant::now();
            // The nearness it is drawn for, and for what lies along the line
            // through the eyes.
            let mut go = |n: f32, frames: usize| {
                for _ in 0..frames {
                    t += FRAME;
                    cf.pending = Some((Some(n), 0.0));
                    cf.step(t, lever);
                }
                let f = cf.step(t, lever);
                (1.0 / f.distance, 1.0 / f.side_distance)
            };
            let first = go(2.5, 300);
            // A part of it 6% nearer, and something a quarter nearer.
            let part = go(2.5 * 1.06, 300);
            let nearer = go(2.5 * 1.25, 300);
            let back = go(2.5, 300);
            if lever == Lever::default() {
                assert!(first.0 < 0.03 && back.0 < 0.03, "{first:?} {back:?}");
                assert!(first.1 < 0.03 && back.1 < 0.03, "{first:?} {back:?}");
                continue;
            }
            // Along the line through the eyes: a fifth short of each, coming
            // from farther, and still within a fifth of the first when the
            // eyes are back on it.
            assert!((first.1 - 2.5 / step).abs() < 0.01, "{first:?}");
            assert!((part.1 - 2.5 * 1.06 / step).abs() < 0.01, "{part:?}");
            assert!((nearer.1 - 2.5 * 1.25 / step).abs() < 0.01, "{nearer:?}");
            assert!((back.1 - nearer.1).abs() < 1e-3, "{back:?}");
            // The rest: all the way, lying down every time, with the head
            // turned once a part is far enough from the last.
            assert!((first.0 - 2.5).abs() < 0.01, "{first:?}");
            assert!((nearer.0 - 2.5 * 1.25).abs() < 0.01, "{nearer:?}");
            assert!((back.0 - 2.5).abs() < 0.01, "{back:?}");
            assert_eq!(
                (part.0 - 2.5 * 1.06).abs() < 0.01,
                lever == lying,
                "{part:?}"
            );
        }
    }

    #[test]
    fn the_lever_is_along_the_line_through_the_eyes_turning_and_off_it_tilting() {
        let video = crate::VideoParams {
            format: fp_core::format::VideoFormat::new(
                fp_core::format::Projection::EQUIRECT_180,
                fp_core::format::StereoLayout::SideBySide,
            ),
            ..Default::default()
        };
        let p = crate::params::build(&video, None, [glam::Mat4::IDENTITY; 2]);
        let half = 0.5 * LENS_SPACING;
        // The eyes of a head turned `by`, the lens spacing apart.
        let eyes = |by: Quat| [by * Vec3::NEG_X * half, by * Vec3::X * half];
        // Level and facing the front: where the cameras were.
        let (none, line) = redraw_lever(&p, AHEAD, eyes(Quat::IDENTITY));
        assert!(none.length() < 1e-6, "{none:?}");
        assert!(line.abs_diff_eq(Vec3::X, 1e-6), "{line}");
        // Turned 50° to the right, looking that way: the pictures only move
        // towards or away from each other.
        let turn = Quat::from_rotation_y(-50f32.to_radians());
        let (turned, line) = redraw_lever(&p, turn * AHEAD, eyes(turn));
        let want = half * (1.0 - 50f32.to_radians().cos());
        assert!((turned.side - want).abs() < 1e-5, "{turned:?} vs {want}");
        assert!(turned.up < 1e-6, "{turned:?}");
        assert!(line.abs_diff_eq(turn * Vec3::X, 1e-5), "{line}");
        // Lying on one side, looking ahead: as far off the line through
        // the eyes as along it.
        let (lying, line) = redraw_lever(&p, AHEAD, eyes(Quat::from_rotation_z(FRAC_PI_2)));
        assert!((lying.up - half).abs() < 1e-5, "{lying:?}");
        assert!((lying.side - half).abs() < 1e-5, "{lying:?}");
        assert!(line.abs_diff_eq(Vec3::Y, 1e-5), "{line}");
        // Wider eyes than the lenses, level: along it only.
        let (wide, _) = redraw_lever(&p, AHEAD, [Vec3::NEG_X * 0.034, Vec3::X * 0.034]);
        assert!(
            (wide.side - (0.034 - half)).abs() < 1e-6 && wide.up < 1e-6,
            "{wide:?}"
        );
        // Turned to the right and looking 25° down as well: nearly as much
        // of it is off the line, where a wrong distance pulls the pictures
        // apart, as along it.
        let down = turn * Quat::from_rotation_x(-25f32.to_radians());
        let (both, _) = redraw_lever(&p, down * AHEAD, eyes(turn));
        assert!((both.side - 0.0113).abs() < 0.0005, "{both:?}");
        assert!((both.up - 0.0102).abs() < 0.0005, "{both:?}");
    }

    #[test]
    fn at_the_video_s_edges_the_re_projection_gives_twice_as_much_and_glides() {
        assert_eq!(edge(0.0), 0.0);
        assert_eq!(edge(29f32.to_radians()), 0.0);
        assert!((edge(37.5f32.to_radians()) - 0.5).abs() < 0.01);
        assert_eq!(edge(-50f32.to_radians()), 1.0);
        // The eyes 4 mm from their cameras off the line through them.
        let lever = Lever {
            side: 0.0,
            up: 0.004,
        };
        for (along, at_edge) in [(0.0, false), (50f32.to_radians(), true)] {
            let mut cf = ComfortFocus::default();
            cf.follows_eyes(true);
            let mut t = Instant::now();
            let mut go = |cf: &mut ComfortFocus, n: f32, frames: usize| {
                (0..frames)
                    .map(|_| {
                        t += FRAME;
                        cf.pending = Some((Some(n), along));
                        1.0 / cf.step(t, lever).distance
                    })
                    .collect::<Vec<f32>>()
            };
            go(&mut cf, 4.0, 300);
            // A part of it 5 cm nearer, 0.23° out up and down: followed
            // towards the front, left be at the edge.
            let part = *go(&mut cf, 5.0, 300).last().unwrap();
            assert_eq!((part - 5.0).abs() < 0.01, !at_edge, "{along}: {part}");
            // Something much farther the eyes land on: there in a tenth of a
            // second towards the front, at once, in three quarters at the
            // edge, where it glides.
            cf.landed();
            let far = go(&mut cf, 1.0, 70);
            let done = |n: f32| (part - n) / (part - 1.0);
            assert_eq!(done(far[10]) > 0.9, !at_edge, "{along}: {far:?}");
            assert!(at_edge || done(far[10]) > 0.9, "{far:?}");
            assert!(!at_edge || done(far[10]) < 0.5, "{far:?}");
            assert!(done(far[69]) > 0.9, "{along}: {far:?}");
            // Each eye's picture moves for it only the once, one way, never
            // past it.
            assert!(
                far.windows(2).all(|w| w[1] <= w[0]) && far[69] >= 1.0,
                "{along}: {far:?}"
            );
        }
    }

    #[test]
    fn with_the_head_turned_the_eyes_going_over_something_near_move_only_what_must() {
        // The head turned 50° to something 0.22 m away and the eyes a
        // little down: 11 mm from their cameras along the line through
        // them, 4 mm off it.
        let lever = Lever {
            side: 0.011,
            up: 0.004,
        };
        let mut cf = ComfortFocus::default();
        cf.follows_eyes(true);
        let mut t = Instant::now();
        let mut look = |cf: &mut ComfortFocus, n: f32| {
            cf.landed();
            let mut last = None;
            for _ in 0..30 {
                t += FRAME;
                cf.pending = Some((Some(n), 0.0));
                last = Some(cf.step(t, lever));
            }
            last.unwrap()
        };
        let first = look(&mut cf, 1.0 / 0.22);
        // Twenty looks at parts of it between 0.19 and 0.26 m away.
        let mut exact = 0.0f32;
        let mut side = 0.0f32;
        // Looks that left it drawn for another distance, off the line and
        // along it: each glide is over within the look.
        let mut redrawn = 0;
        let mut redrawn_side = 0;
        let mut before = first;
        for i in 0..20 {
            let d = 0.19 + 0.07 * ((i * 7) % 10) as f32 / 9.0;
            let f = look(&mut cf, 1.0 / d);
            // What is off the line is drawn for where each part is, within
            // what shows.
            let gap = (1.0 / f.distance - 1.0 / d).abs();
            assert!(
                gap * lever.up <= REDRAW_TOLERANCE_RAD + 1e-5,
                "{f:?} for {d}"
            );
            // How far each eye's picture moved, either way, radians.
            let up = (1.0 / f.distance - 1.0 / before.distance).abs();
            let along = (1.0 / f.side_distance - 1.0 / before.side_distance).abs();
            exact += up * lever.up;
            side += along * lever.side;
            redrawn += (up > 1e-3) as u32;
            redrawn_side += (along > 1e-3) as u32;
            before = f;
        }
        assert!(redrawn >= 10, "{redrawn}");
        assert!(redrawn_side <= 3, "{redrawn_side}");
        // Along the line, where one distance for both moved the pictures
        // three times as far as off it, they now move a fifth as far.
        assert!(
            side < 0.2 * exact * lever.side / lever.up,
            "{side} vs {exact}"
        );
    }

    #[test]
    fn small_changes_and_single_spikes_keep_it_still() {
        let mut cf = ComfortFocus::default();
        let mut t = Instant::now();
        let held = *run(&mut cf, &mut t, &at(0.4), 300).last().unwrap();
        for d in [0.41f32, 0.395, 0.405] {
            let out = run(&mut cf, &mut t, &at(d), 20);
            assert!(out.iter().all(|&c| (c - held).abs() < 1e-4), "{out:?}");
        }
        // A single far reading never moves it.
        let mut out = run(&mut cf, &mut t, &at(5.0), 1);
        out.extend(run(&mut cf, &mut t, &at(0.4), 20));
        assert!(out.iter().all(|&c| (c - held).abs() < 1e-4), "{out:?}");
    }

    #[test]
    fn on_the_edge_of_something_near_it_stays_where_it_is() {
        let mut cf = ComfortFocus::default();
        let mut t = Instant::now();
        let near = read_as(0.4);
        let held = *run_focus(&mut cf, &mut t, &at(0.4), 300).last().unwrap();
        assert!((held.distance - near).abs() < 0.01, "{held:?}");
        // The eyes on where it meets the wall behind: readings of both, by
        // turns.
        let mut out = Vec::new();
        for i in 0..60 {
            let d = if i % 2 == 0 { 3.0 } else { 0.4 };
            out.extend(run_focus(&mut cf, &mut t, &at(d), 1));
        }
        assert!(
            out.iter()
                .all(|f| (f.distance - near).abs() < 0.01 && (f.relief - held.relief).abs() < 1e-4),
            "{out:?}"
        );
        // Only the wall, read twice running: it goes there.
        let wall = run_focus(&mut cf, &mut t, &at(3.0), 150);
        let far = read_as(3.0);
        let last = wall.last().unwrap();
        assert!((last.distance - far).abs() < 0.05 * far, "{last:?}");
        assert_eq!(last.relief, 0.0);
    }

    #[test]
    fn something_that_reads_only_now_and_then_still_takes_over() {
        let mut cf = ComfortFocus::default();
        let mut t = Instant::now();
        let held = *run(&mut cf, &mut t, &at(0.4), 300).last().unwrap();
        assert!(held > 0.0);
        // The eyes go to a wall that reads once in four measurements, with
        // nothing to match in between: its second reading takes over, and
        // the easing is out within a second and a half.
        let blank = cluster(blank(), blank());
        let mut out = Vec::new();
        for _ in 0..34 {
            out.extend(run_focus(&mut cf, &mut t, &at(3.0), 1));
            out.extend(run_focus(&mut cf, &mut t, &blank, 3));
        }
        let far = read_as(3.0);
        assert!((1.0 / cf.accepted.unwrap() - far).abs() < 0.05 * far);
        assert!(out[..8].iter().all(|f| (f.relief - held).abs() < 1e-4));
        assert!(out.last().unwrap().relief < 0.002, "{:?}", out.last());
    }

    #[test]
    fn a_reading_from_too_long_ago_decides_nothing() {
        let mut cf = ComfortFocus::default();
        let mut t = Instant::now();
        run(&mut cf, &mut t, &at(0.4), 300);
        let near = cf.accepted.unwrap();
        // One reading of the wall, nothing to match for over half a second,
        // then another: the first no longer counts.
        run(&mut cf, &mut t, &at(3.0), 1);
        run(&mut cf, &mut t, &cluster(blank(), blank()), 50);
        run(&mut cf, &mut t, &at(3.0), 1);
        assert_eq!(cf.accepted, Some(near));
        // The next one does, with the one just before it.
        run(&mut cf, &mut t, &at(3.0), 1);
        assert!(cf.accepted.unwrap() < 0.5, "{:?}", cf.accepted);
    }

    #[test]
    fn readings_of_other_things_that_settle_on_no_distance_let_go_after_a_second() {
        let mut cf = ComfortFocus::default();
        let mut t = Instant::now();
        let held = *run_focus(&mut cf, &mut t, &at(0.4), 300).last().unwrap();
        assert!(held.relief > 0.0);
        // The eyes roam over things at ever other distances, none read twice
        // running: what the easing was for is no longer seen. It holds for a
        // second, then lets go; the distance in focus stays.
        let blank = cluster(blank(), blank());
        let mut out = Vec::new();
        for d in [2.0f32, 3.0, 5.0, 2.4, 4.0].repeat(12) {
            out.extend(run_focus(&mut cf, &mut t, &at(d), 1));
            out.extend(run_focus(&mut cf, &mut t, &blank, 2));
        }
        assert!(
            out[..85]
                .iter()
                .all(|f| (f.relief - held.relief).abs() < 1e-4),
            "{:?}",
            &out[..85]
        );
        let last = out.last().unwrap();
        assert!(last.relief < 0.002, "{last:?}");
        assert!((last.distance - held.distance).abs() < 0.01, "{last:?}");
        // Once: from then on it only goes out.
        assert!(
            out[85..]
                .windows(2)
                .all(|w| w[1].relief <= w[0].relief + 1e-6),
            "{out:?}"
        );
    }

    #[test]
    fn on_an_edge_that_reads_only_now_and_then_it_stays_where_it_is() {
        let mut cf = ComfortFocus::default();
        let mut t = Instant::now();
        let held = *run_focus(&mut cf, &mut t, &at(0.4), 300).last().unwrap();
        // Readings of the near thing and the wall behind by turns, with
        // nothing to match between them, for five seconds.
        let blank = cluster(blank(), blank());
        let mut out = Vec::new();
        for i in 0..110 {
            let d = if i % 2 == 0 { 3.0 } else { 0.4 };
            out.extend(run_focus(&mut cf, &mut t, &at(d), 1));
            out.extend(run_focus(&mut cf, &mut t, &blank, 3));
        }
        assert!(
            out.iter().all(|f| (f.distance - held.distance).abs() < 0.01
                && (f.relief - held.relief).abs() < 1e-4),
            "{out:?}"
        );
    }

    #[test]
    fn things_nearer_than_30_cm_are_read_down_to_10_cm() {
        // The candidates reach 0.093 m straight ahead.
        let nearest = 0.5 * LENS_SPACING / kappa_at((CANDIDATES - 1) as f32).tan();
        assert!((0.09..0.095).contains(&nearest), "{nearest}");
        for d in [0.25f32, 0.2, 0.15, 0.11] {
            let mut cf = ComfortFocus::default();
            let mut t = Instant::now();
            let out = run_focus(&mut cf, &mut t, &at(d), 300);
            let last = out.last().unwrap();
            assert!(
                (last.distance - read_as(d)).abs() < 0.005,
                "{d} m: {last:?}"
            );
            // The easing is at its most from 0.31 m in.
            assert!(
                (last.relief - MAX_COMFORT_CORR_RAD).abs() < 1e-4,
                "{last:?}"
            );
        }
    }

    #[test]
    fn something_very_near_and_moving_stays_in_focus() {
        let mut cf = ComfortFocus::default();
        let mut t = Instant::now();
        run(&mut cf, &mut t, &at(0.15), 300);
        // Coming nearer at 5 cm a second, read 60 times a second: each
        // reading is a millimetre nearer, 0.04 in nearness at this distance,
        // and the distance in focus keeps up with it.
        let mut d = 0.15f32;
        for _ in 0..30 {
            d -= 0.05 / 60.0;
            feed(&mut cf, &mut t, 1.0 / d, 0.0, 1);
            feed_nothing(&mut cf, &mut t, 1);
        }
        let got = 1.0 / cf.accepted.unwrap();
        assert!((got - d).abs() < 0.002, "{got} m against {d} m");
        // Farther than 0.3 m the same step in nearness is another distance.
        let mut cf = ComfortFocus::default();
        feed(&mut cf, &mut t, 2.5, 0.0, 300);
        feed(&mut cf, &mut t, 2.5 + 1.4 * AGREE, 0.0, 1);
        assert_eq!(cf.accepted, Some(2.5));
    }

    #[test]
    fn something_farther_the_eyes_land_on_is_eased_out_for_and_drawn_at_once() {
        let near = read_as(0.4);
        let far = read_as(0.8);
        let mut cf = ComfortFocus::default();
        let mut t = Instant::now();
        let held = *run_focus(&mut cf, &mut t, &at(0.4), 300).last().unwrap();
        assert!((held.distance - near).abs() < 0.01, "{held:?}");
        // The eyes jump to a part of it twice as far and land: by the time
        // they have converged on it, a fifth of a second on, the easing is
        // out and it is drawn at its own distance.
        cf.landed();
        let out = run_focus(&mut cf, &mut t, &at(0.8), 18);
        let last = out.last().unwrap();
        // Eased for a fifth short of it, from the near side.
        let want = relief_at((1.0 + RELIEF_STEP) / far);
        assert!(want < 0.15 * held.relief, "{want}");
        assert!(last.relief - want < 0.05 * held.relief, "{out:?}");
        assert!((last.distance - far).abs() < 0.02 * far, "{out:?}");
        // Never past where it is going.
        assert!(
            out.iter()
                .all(|f| f.relief >= want - 1e-5 && f.distance <= far * 1.001),
            "{out:?}"
        );

        // Without a jump of the eyes (what was looked at moved away, or the
        // head aims), both go gently, as it shows while they happen: by the
        // same frame the easing hasn't started and the re-projection is well
        // short of it.
        let mut cf = ComfortFocus::default();
        let held = *run_focus(&mut cf, &mut t, &at(0.4), 300).last().unwrap();
        let out = run_focus(&mut cf, &mut t, &at(0.8), 18);
        let last = out.last().unwrap();
        assert!((last.relief - held.relief).abs() < 1e-4, "{out:?}");
        assert!(last.distance < 0.75 * far, "{out:?}");
    }

    #[test]
    fn what_is_read_long_after_the_eyes_landed_is_followed_gently() {
        let mut cf = ComfortFocus::default();
        let mut t = Instant::now();
        let held = *run(&mut cf, &mut t, &at(0.4), 300).last().unwrap();
        // The eyes land on something that can't be read for half a second:
        // by then they have settled, and the easing waits and goes gently.
        cf.landed();
        run(&mut cf, &mut t, &cluster(blank(), blank()), 45);
        let out = run(&mut cf, &mut t, &at(3.0), 20);
        assert!(out.iter().all(|&c| (c - held).abs() < 1e-4), "{out:?}");
    }

    #[test]
    fn something_nearer_the_eyes_land_on_is_drawn_and_eased_for_while_they_converge() {
        let mut cf = ComfortFocus::default();
        cf.follows_eyes(true);
        let mut t = Instant::now();
        let far = *run_focus(&mut cf, &mut t, &at(3.0), 300).last().unwrap();
        cf.landed();
        let out = run_focus(&mut cf, &mut t, &at(0.4), 210);
        // Left as it was for the quarter of a second it must stay...
        let hold = (NEARER_HOLD_SECS / FRAME.as_secs_f32()) as usize;
        assert!(out[..hold].iter().all(|f| same(f, &far)), "{out:?}");
        // ...then drawn at its distance within a fifth of a second...
        let near = read_as(0.4);
        assert!(
            (out[hold + 17].distance - near).abs() < 0.02 * near,
            "{:?}",
            out[hold + 17]
        );
        // ...and eased in for in a third of a second, never past it or back.
        let want = correction_from_kappa(kappa_for(0.4));
        let eased = &out[hold..];
        assert!(eased[30].relief > 0.9 * want, "slow: {:?}", eased[30]);
        assert!(
            eased
                .windows(2)
                .all(|w| w[1].relief >= w[0].relief - 1e-6 && w[1].relief <= want + 1e-5),
            "{eased:?}"
        );
        assert!((eased[179].relief - want).abs() < 0.001, "{:?}", eased[179]);

        // With the head aiming, which tells of no jump, it shows while it
        // happens: gently, as ever, under half way in where it was nearly
        // all the way in above.
        let mut cf = ComfortFocus::default();
        run_focus(&mut cf, &mut t, &at(3.0), 300);
        let out = run_focus(&mut cf, &mut t, &at(0.4), hold + 180);
        let eased = &out[hold..];
        assert!(eased[36].relief < 0.5 * want, "abrupt: {:?}", eased[36]);
        assert!((eased[179].relief - want).abs() < 0.001, "{:?}", eased[179]);
    }

    #[test]
    fn back_on_something_near_the_eyes_left_a_moment_ago_it_needn_t_wait() {
        let mut cf = ComfortFocus::default();
        cf.follows_eyes(true);
        let mut t = Instant::now();
        let near = *run_focus(&mut cf, &mut t, &at(0.4), 300).last().unwrap();
        // To the wall behind for a second and a half, and back: in focus on
        // the second reading of it, drawn and eased for within a third of a
        // second of landing.
        cf.landed();
        let far = *run_focus(&mut cf, &mut t, &at(3.0), 140).last().unwrap();
        assert!(far.relief < 1e-4 && far.distance > 2.5, "{far:?}");
        cf.landed();
        let mut out = run_focus(&mut cf, &mut t, &at(0.4), 1);
        let (d, _) = cf.in_focus().unwrap();
        assert!(d > 2.5, "{d}");
        out.extend(run_focus(&mut cf, &mut t, &at(0.4), 1));
        let (d, _) = cf.in_focus().unwrap();
        assert!((d - near.distance).abs() < 0.01, "{d}");
        out.extend(run_focus(&mut cf, &mut t, &at(0.4), 30));
        let (d, _) = cf.in_focus().unwrap();
        assert!((d - near.distance).abs() < 0.01, "{d}");
        assert!(
            (out[19].distance - near.distance).abs() < 0.02 * near.distance,
            "{out:?}"
        );
        assert!(out[31].relief > 0.9 * near.relief, "{out:?}");

        // Something at that distance passing in front of the wall, the eyes
        // resting on the wall: it didn't bring them, and moves nothing. Gone
        // again, it is forgotten: passing again at once, it waits afresh.
        cf.landed();
        let far = *run_focus(&mut cf, &mut t, &at(3.0), 140).last().unwrap();
        let out = [
            run_focus(&mut cf, &mut t, &at(0.4), 60),
            run_focus(&mut cf, &mut t, &at(3.0), 2),
            run_focus(&mut cf, &mut t, &at(0.4), 60),
        ]
        .concat();
        assert!(out.iter().all(|f| same(f, &far)), "{out:?}");

        // Longer ago than it stays known, it waits as anything nearer does.
        let away = (KNOWN_SECS / FRAME.as_secs_f32()) as usize + 20;
        let far = *run_focus(&mut cf, &mut t, &at(3.0), away).last().unwrap();
        cf.landed();
        let hold = (NEARER_HOLD_SECS / FRAME.as_secs_f32()) as usize;
        let out = run_focus(&mut cf, &mut t, &at(0.4), hold);
        assert!(out.iter().all(|f| same(f, &far)), "{out:?}");
    }

    #[test]
    fn what_was_in_focus_for_a_moment_only_is_not_known() {
        let mut cf = ComfortFocus::default();
        cf.follows_eyes(true);
        let mut t = Instant::now();
        run_focus(&mut cf, &mut t, &at(3.0), 300);
        // In focus for a fifth of a second: a few readings may have taken
        // something else for it.
        let hold = (NEARER_HOLD_SECS / FRAME.as_secs_f32()) as usize;
        cf.landed();
        run_focus(&mut cf, &mut t, &at(0.4), hold + 20);
        let (d, _) = cf.in_focus().unwrap();
        assert!((d - read_as(0.4)).abs() < 0.01, "{d}");
        cf.landed();
        run_focus(&mut cf, &mut t, &at(3.0), 100);
        cf.landed();
        run_focus(&mut cf, &mut t, &at(0.4), hold - 2);
        let (d, _) = cf.in_focus().unwrap();
        assert!(d > 2.5, "{d}");
    }

    #[test]
    fn something_nearer_passing_in_front_of_what_the_eyes_rest_on_moves_nothing() {
        let mut cf = ComfortFocus::default();
        cf.follows_eyes(true);
        let mut t = Instant::now();
        let held = *run_focus(&mut cf, &mut t, &at(0.8), 300).last().unwrap();
        let (behind, _) = cf.in_focus().unwrap();
        // Three times something 0.25 m away passes in front for 0.7 s, the
        // eyes still on what is behind it: the picture stays as it is, and
        // so does the distance in focus. Gone when they read what is behind
        // again, each pass waits afresh.
        let mut out = Vec::new();
        for _ in 0..3 {
            out.extend(run_focus(&mut cf, &mut t, &at(0.25), 64));
            out.extend(run_focus(&mut cf, &mut t, &at(0.8), 30));
        }
        assert!(out.iter().all(|f| same(f, &held)), "{out:?}");
        let (d, _) = cf.in_focus().unwrap();
        assert!((d - behind).abs() < 1e-3 * behind, "{d} vs {behind}");
        // Staying there past a second, it is what they look at by now.
        let out = run_focus(&mut cf, &mut t, &at(0.25), 300);
        let stay = (UNREAD_HOLD_SECS / FRAME.as_secs_f32()) as usize;
        assert!(out[..stay - 2].iter().all(|f| same(f, &held)), "{out:?}");
        let last = out.last().unwrap();
        assert!(
            (last.distance - read_as(0.25)).abs() < 0.01 && last.relief > 0.04,
            "{last:?}"
        );
        // Gently: the eyes didn't jump to it, so a tenth of a second after
        // it starts it is under half way there.
        let k = out.iter().position(|f| !same(f, &held)).unwrap();
        assert!(gone(&held, &out[k + 9], read_as(0.25)) < 0.5, "{out:?}");
    }

    #[test]
    fn something_nearer_the_eyes_jump_to_while_it_passes_takes_over_from_then() {
        let mut cf = ComfortFocus::default();
        cf.follows_eyes(true);
        let mut t = Instant::now();
        let held = *run_focus(&mut cf, &mut t, &at(0.8), 300).last().unwrap();
        // In front for half a second already when the eyes jump onto it: a
        // quarter of a second from their landing, not from when it came.
        let hold = (NEARER_HOLD_SECS / FRAME.as_secs_f32()) as usize;
        let before = run_focus(&mut cf, &mut t, &at(0.25), 45);
        cf.landed();
        let after = run_focus(&mut cf, &mut t, &at(0.25), hold + 20);
        assert!(
            before.iter().chain(&after[..hold]).all(|f| same(f, &held)),
            "{before:?} {after:?}"
        );
        let last = after.last().unwrap();
        assert!((last.distance - read_as(0.25)).abs() < 0.01, "{after:?}");
        // A landing long before it came says nothing about it.
        let mut cf = ComfortFocus::default();
        cf.follows_eyes(true);
        run_focus(&mut cf, &mut t, &at(0.8), 300);
        cf.landed();
        let out = run_focus(&mut cf, &mut t, &at(0.8), 40);
        let out = [out, run_focus(&mut cf, &mut t, &at(0.25), 2 * hold)].concat();
        assert!(out.iter().all(|f| same(f, &held)), "{out:?}");
    }

    #[test]
    fn aiming_with_the_head_something_nearer_takes_over_in_a_quarter_of_a_second() {
        let mut cf = ComfortFocus::default();
        let mut t = Instant::now();
        let held = *run_focus(&mut cf, &mut t, &at(0.8), 300).last().unwrap();
        // The head tells of no jump: a short wait whenever, which what is
        // gone again sooner doesn't last.
        let hold = (NEARER_HOLD_SECS / FRAME.as_secs_f32()) as usize;
        let out = run_focus(&mut cf, &mut t, &at(0.25), hold - 2);
        let out = [out, run_focus(&mut cf, &mut t, &at(0.8), 30)].concat();
        assert!(out.iter().all(|f| same(f, &held)), "{out:?}");
        let out = run_focus(&mut cf, &mut t, &at(0.25), hold + 60);
        assert!(out[..hold].iter().all(|f| same(f, &held)), "{out:?}");
        let last = out.last().unwrap();
        assert!((last.distance - read_as(0.25)).abs() < 0.03, "{last:?}");
        // Gently, as nothing hides it: a tenth of a second after it starts,
        // under half way there.
        let k = out.iter().position(|f| !same(f, &held)).unwrap();
        assert!(gone(&held, &out[k + 9], read_as(0.25)) < 0.5, "{out:?}");
    }

    #[test]
    fn a_part_a_little_nearer_and_a_glance_back_take_over_at_once() {
        let mut cf = ComfortFocus::default();
        cf.follows_eyes(true);
        let mut t = Instant::now();
        run_focus(&mut cf, &mut t, &at(0.5), 300);
        // A part of it 15% nearer: the second reading of it.
        run_focus(&mut cf, &mut t, &at(0.435), 2);
        let (d, _) = cf.in_focus().unwrap();
        assert!((d - read_as(0.435)).abs() < 0.005, "{d}");
        // A glance at the wall behind, shorter than the easing waits for,
        // and back: in focus again at once, as the picture is still eased
        // for it.
        run_focus(&mut cf, &mut t, &at(3.0), 15);
        let (d, _) = cf.in_focus().unwrap();
        assert!(d > 2.5, "{d}");
        run_focus(&mut cf, &mut t, &at(0.435), 2);
        let (d, _) = cf.in_focus().unwrap();
        assert!((d - read_as(0.435)).abs() < 0.005, "{d}");
    }

    #[test]
    fn on_an_edge_the_eyes_land_on_it_stays_where_it_is() {
        let mut cf = ComfortFocus::default();
        let mut t = Instant::now();
        let held = *run_focus(&mut cf, &mut t, &at(0.4), 300).last().unwrap();
        // Landing where the near thing meets the wall behind: readings of
        // both by turns decide nothing, at once or later.
        cf.landed();
        let mut out = Vec::new();
        for i in 0..60 {
            let d = if i % 2 == 0 { 3.0 } else { 0.4 };
            out.extend(run_focus(&mut cf, &mut t, &at(d), 1));
        }
        assert!(
            out.iter().all(|f| (f.distance - held.distance).abs() < 0.01
                && (f.relief - held.relief).abs() < 1e-4),
            "{out:?}"
        );
    }

    #[test]
    fn only_something_much_nearer_is_checked_for_having_stayed() {
        // The eyes have just landed each time, on a distance they were on
        // lately. Something much nearer has then stayed, for what they
        // landed on, and is drawn and eased for at the pace for that. What
        // takes over at once (farther, a little nearer, or what the picture
        // is still eased for) isn't checked for it. Either way nothing
        // nearer is left waiting.
        for (before, to, landed_on) in [
            (vec![(2.5, 300)], 0.33, false),
            (vec![(2.5, 300)], 2.8, false),
            (vec![(2.3, 300), (0.33, 15)], 2.3, false),
            (vec![(0.33, 300)], 2.5, true),
        ] {
            let mut cf = ComfortFocus::default();
            cf.follows_eyes(true);
            let mut t = Instant::now();
            for (n, frames) in before {
                feed(&mut cf, &mut t, n, 0.0, frames);
            }
            cf.known.push((to, cf.clock));
            cf.landed();
            cf.read(to, 0.0);
            cf.read(to, 0.0);
            let (d, _) = cf.in_focus().unwrap();
            assert!((1.0 / d - to).abs() < 1e-4, "{to}: {d} m");
            assert_eq!((cf.landed_on, cf.nearer), (landed_on, None), "{to}");
        }
    }

    #[test]
    fn a_measurement_gives_a_distance_from_the_aim_or_the_ring_or_none() {
        let mut cf = ComfortFocus::default();
        assert!(cf.ingest(&at(0.4), 0.0));
        assert!(cf.ingest(&cluster(blank(), curve(0.4, 0.08)), 0.0));
        let mut twice = curve(0.4, 0.08);
        twice[3][0] = 0.93;
        for none in [
            cluster(blank(), blank()),
            cluster([[0.85, 0.05]; CANDIDATES], blank()),
            cluster(twice, blank()),
        ] {
            assert!(!cf.ingest(&none, 0.0));
        }
        // Starting afresh forgets what was in focus.
        let mut t = Instant::now();
        run(&mut cf, &mut t, &at(0.4), 2);
        assert!(cf.in_focus().is_some());
        cf.reset();
        assert_eq!(cf.in_focus(), None);
    }

    #[test]
    fn the_log_line_tells_how_often_a_distance_was_read_what_is_in_focus_and_what_it_cost() {
        let mut log = FocusLog::default();
        let t = Instant::now();
        assert_eq!(log.take_line(t, None), None);
        for i in 0..900u64 {
            let from = if i % 10 == 0 {
                AimFrom::Head
            } else {
                AimFrom::Eyes
            };
            let measured = i % 3 == 0;
            log.frame(t + Duration::from_millis(i * 11), from, measured);
            log.gpu(measured, if measured { 5.5 } else { 5.0 });
            if measured {
                log.read_back(i % 4 != 0);
            }
        }
        let line = log
            .take_line(t + Duration::from_secs(10), Some((0.62, 0.02)))
            .expect("a line");
        assert_eq!(
            line,
            "comfort focus: 30 measurements a second, 300 read back, 75% gave a distance; \
             now 0.62 m, eased 1.15°; aim by the eyes in 90% of 900 frames; \
             eye passes 5.50 GPU ms with a measurement, 5.00 without"
        );
        assert_eq!(log.take_line(t, None), None, "the window starts again");
        // Nothing in focus yet, and no frame timed with a measurement.
        for _ in 0..2 {
            log.frame(t, AimFrom::Held, false);
            log.gpu(false, 5.0);
        }
        assert_eq!(
            log.take_line(t + Duration::from_secs(10), None)
                .expect("a line"),
            "comfort focus: 0 measurements a second, 0 read back, 0% gave a distance; \
             nothing in focus yet; aim by the eyes in 0% of 2 frames"
        );
    }

    #[test]
    fn the_aim_follows_the_eyes_through_a_blink_and_the_head_otherwise() {
        let mut f = AimFilter::default();
        let t = Instant::now();
        let ms = |n: u64| t + Duration::from_millis(n);
        let level = Quat::IDENTITY;
        let looked = right_of_ahead(15.0);
        assert!(f.aim(ms(0), Some(looked), level).abs_diff_eq(looked, 1e-6));
        assert_eq!(f.from(), AimFrom::Eyes);
        // A blink keeps it where the eyes were, turning with the head.
        assert!(f.aim(ms(300), None, level).abs_diff_eq(looked, 1e-6));
        assert_eq!(f.from(), AimFrom::Held);
        let turned = Quat::from_rotation_y(0.3);
        assert!(
            f.aim(ms(350), None, turned)
                .abs_diff_eq(turned * looked, 1e-6)
        );
        // A longer loss hands it to the head.
        assert!(f.aim(ms(500), None, level).abs_diff_eq(AHEAD, 1e-6));
        assert_eq!(f.from(), AimFrom::Head);
        // A gaze 60° off the head is the tracker's error.
        let mut f = AimFilter::default();
        assert!(
            f.aim(ms(0), Some(right_of_ahead(60.0)), level)
                .abs_diff_eq(AHEAD, 1e-6)
        );
        // Without a tracker it is the head.
        assert!(f.aim(ms(0), None, turned).abs_diff_eq(turned * AHEAD, 1e-6));
    }

    #[test]
    fn the_head_turn_comes_from_the_eye_views_axes() {
        let turn = Quat::from_euler(glam::EulerRot::YXZ, 0.7, -0.3, 1.2);
        let got = head_turn(
            turn * Vec3::Z * 2.0,
            turn * Vec3::Y * 2.0 + turn * Vec3::Z * 0.01,
        );
        assert!(got.angle_between(turn) < 0.01, "{got:?} vs {turn:?}");
    }

    /// Repeatable normal noise (xorshift, Box-Muller).
    struct Noise(u64);

    impl Noise {
        fn uniform(&mut self) -> f32 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            ((self.0 >> 40) as f32 + 0.5) / (1u64 << 24) as f32
        }

        fn normal(&mut self) -> f32 {
            let (a, b) = (self.uniform(), self.uniform());
            (-2.0 * a.ln()).sqrt() * (std::f32::consts::TAU * b).cos()
        }
    }

    /// Runs gaze samples at `hz` for `seconds` with the head level, from
    /// `t`, `trace` giving each one's yaw and pitch in degrees; the aims'
    /// yaw and pitch in degrees.
    fn follow(
        f: &mut AimFilter,
        t: &mut Instant,
        hz: f32,
        seconds: f32,
        mut trace: impl FnMut() -> (f32, f32),
    ) -> Vec<Vec2> {
        let n = (seconds * hz).round() as usize;
        (0..n)
            .map(|_| {
                *t += Duration::from_secs_f32(1.0 / hz);
                let (y, p) = trace();
                let g = from_yaw_pitch(Vec2::new(y, p) * std::f32::consts::PI / 180.0);
                yaw_pitch(f.aim(*t, Some(g), Quat::IDENTITY)) * 180.0 / std::f32::consts::PI
            })
            .collect()
    }

    #[test]
    fn a_resting_gaze_comes_out_with_under_half_its_jitter() {
        for hz in [72.0f32, 90.0, 120.0] {
            let mut f = AimFilter::default();
            let mut t = Instant::now();
            follow(&mut f, &mut t, hz, 1.0, || (5.0, -3.0));
            let mut noise = Noise(7);
            let mut raw = Vec::new();
            let out = follow(&mut f, &mut t, hz, 4.0, || {
                let s = (5.0 + 0.5 * noise.normal(), -3.0 + 0.5 * noise.normal());
                raw.push(Vec2::new(s.0, s.1));
                s
            });
            let spread = |v: &[Vec2]| {
                let mean = v.iter().copied().sum::<Vec2>() / v.len() as f32;
                (v.iter().map(|s| (*s - mean).length_squared()).sum::<f32>() / v.len() as f32)
                    .sqrt()
            };
            let ratio = spread(&out) / spread(&raw);
            assert!(ratio < 0.5, "{hz} Hz: jitter x{ratio}");
        }
    }

    #[test]
    fn a_jump_of_the_eyes_is_followed_within_a_degree_in_three_frames() {
        for hz in [72.0f32, 90.0, 120.0] {
            let mut f = AimFilter::default();
            let mut t = Instant::now();
            follow(&mut f, &mut t, hz, 1.0, || (-10.0, 0.0));
            let out = follow(&mut f, &mut t, hz, 0.2, || (10.0, 0.0));
            // Within a degree by 33 ms: three frames at 72 and 90 Hz, four
            // at 120.
            let frames = (0.0333 * hz).ceil() as usize;
            let off = (out[frames - 1].x - 10.0).abs();
            assert!(off < 1.0, "{hz} Hz: {off}° off after {frames} frames");
            assert!(f.settled());
            // Unsettled while it catches up with the eyes.
            follow(&mut f, &mut t, hz, 1.0 / hz, || (-10.0, 0.0));
            assert!(!f.settled());
            follow(&mut f, &mut t, hz, frames as f32 / hz, || (-10.0, 0.0));
            assert!(f.settled());
        }
    }

    #[test]
    fn a_jump_of_the_eyes_lands_the_aim_at_once_and_says_so() {
        let level = Quat::IDENTITY;
        let t = Instant::now();
        let at = |n: u64| t + Duration::from_micros(n * 8333);
        // Resting 5° left with a little jitter, then a jump to 12° right
        // that takes two samples on the way.
        let mut f = AimFilter::default();
        let mut n = 0;
        for i in 0..60 {
            f.aim(
                at(n),
                Some(right_of_ahead(-5.0 + 0.2 * (i % 3) as f32)),
                level,
            );
            n += 1;
        }
        assert!(f.settled() && !f.take_landed());
        for deg in [1.0f32, 9.0] {
            f.aim(at(n), Some(right_of_ahead(deg)), level);
            n += 1;
            assert!(!f.settled(), "mid-jump at {deg}°");
            assert!(!f.take_landed(), "mid-jump at {deg}°");
        }
        // The first sample where they land is 3° from the one before...
        f.aim(at(n), Some(right_of_ahead(12.0)), level);
        assert!(!f.settled() && !f.take_landed());
        // ...the second is beside it, and the smoothing has caught up:
        // landed, and the aim is there, between the two.
        let aim = f.aim(at(n + 1), Some(right_of_ahead(12.2)), level);
        assert!(aim.angle_between(right_of_ahead(12.1)) < 1e-3, "{aim:?}");
        assert!(f.settled());
        assert!(f.take_landed());
        assert!(!f.take_landed(), "said once");

        // A stray sample of the tracker's, 6° off for one frame, is no
        // landing, and neither is the gaze resting.
        let mut f = AimFilter::default();
        for i in 0..40u64 {
            let deg = if i == 20 { 6.0 } else { 0.1 * (i % 3) as f32 };
            f.aim(at(i), Some(right_of_ahead(deg)), level);
            assert!(!f.take_landed(), "sample {i}");
        }
        assert!(f.settled());

        // The eyes back from a blink somewhere else have landed there.
        let mut f = AimFilter::default();
        f.aim(at(0), Some(right_of_ahead(-5.0)), level);
        f.aim(at(1), Some(right_of_ahead(-5.1)), level);
        assert!(!f.take_landed());
        f.aim(at(40), Some(right_of_ahead(10.0)), level);
        assert!(f.take_landed());
    }

    #[test]
    fn a_jump_sampled_on_its_way_lands_once_at_its_end() {
        let level = Quat::IDENTITY;
        let t = Instant::now();
        let at = |n: u64| t + Duration::from_micros(n * 8333);
        let mut f = AimFilter::default();
        let mut n = 0;
        for _ in 0..60 {
            f.aim(at(n), Some(right_of_ahead(-5.0)), level);
            n += 1;
        }
        // 12° at 290° a second, five samples on the way: no landing there,
        // and nothing measured.
        let mut landings = 0;
        for i in 1..=5 {
            f.aim(at(n), Some(right_of_ahead(-5.0 + 2.4 * i as f32)), level);
            n += 1;
            landings += f.take_landed() as u32;
            assert!(!f.settled(), "sample {i}");
        }
        assert_eq!(landings, 0);
        // At rest again: landed within three frames, once, the aim there.
        let mut aim = Vec3::ZERO;
        for _ in 0..3 {
            aim = f.aim(at(n), Some(right_of_ahead(7.0)), level);
            n += 1;
            landings += f.take_landed() as u32;
        }
        assert_eq!(landings, 1);
        assert!(f.settled());
        assert!(aim.angle_between(right_of_ahead(7.0)) < 1e-3, "{aim:?}");
    }

    #[test]
    fn a_resting_gaze_stays_settled() {
        let mut f = AimFilter::default();
        let mut t = Instant::now();
        follow(&mut f, &mut t, 90.0, 1.0, || (5.0, -3.0));
        let mut noise = Noise(11);
        let mut unsettled = 0;
        for _ in 0..360 {
            follow(&mut f, &mut t, 90.0, 1.0 / 90.0, || {
                (5.0 + 0.5 * noise.normal(), -3.0 + 0.5 * noise.normal())
            });
            unsettled += usize::from(!f.settled());
        }
        assert!(unsettled < 4, "unsettled {unsettled} of 360 frames");
    }
}
