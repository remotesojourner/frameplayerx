//! Frame recording: video upload, egui panels, and the two eye passes.
//!
//! Two frame slots are in flight. Per-slot resources (command buffer,
//! uniform buffer, video planes, vertex buffers) are reused only after the
//! slot's fence signals. Images freed during a frame go to that slot's
//! garbage list and are destroyed when the slot comes round again, by which
//! time no submitted frame can still read them.

use crate::cinema::Cinema;
use crate::cinema_mesh::RoomData;
use crate::focus::{AimFilter, ComfortFocus, FocusLog, FocusPass, MEASURE_EVERY, REMEASURE_RAD};
use crate::foveation::{Foveation, RateMaps};
use crate::gpu::Gpu;
use crate::hdr::PeakDetector;
use crate::mem::{self, Buffer, Image};
use crate::params::{self, SceneParams, VideoParams};
use crate::peak::PeakPass;
use crate::pipeline::{self, Depth, PipelineDesc, ShadingRate};
use crate::q10c::Converter;
use crate::timing::{FrameTimer, GpuTimes, Mark};
use crate::tp10_eyes::Tp10Eyes;
use crate::video::VideoPlanes;
use crate::{CinemaRoom, Result, VkContext};
use ash::vk;
use egui::epaint::{ClippedPrimitive, Primitive, TextureId, textures::TexturesDelta};
use fp_media::VideoFrame;
use glam::{Mat4, Vec2, Vec3, Vec4Swizzles};
use gpu_allocator::MemoryLocation;
use std::collections::HashMap;
use std::sync::Arc;
use std::thread::JoinHandle;

const SLOTS: usize = 2;
const ROOMS: usize = CinemaRoom::ALL.len();
/// Linear-light background format of eye targets.
pub const PANEL_FORMATS: [vk::Format; 2] = [vk::Format::R8G8B8A8_UNORM, vk::Format::R8G8B8A8_SRGB];

/// Camera for one eye.
#[derive(Clone, Copy, Debug)]
pub struct EyeView {
    /// World → eye.
    pub view: Mat4,
    /// Eye → Vulkan clip space (see [`crate::math::projection`]).
    pub proj: Mat4,
}

/// Where an eye is rendered: an OpenXR swapchain image or an offscreen image.
#[derive(Clone, Copy, Debug)]
pub struct EyeTarget {
    pub image: vk::Image,
    /// sRGB colour view.
    pub view: vk::ImageView,
    pub extent: vk::Extent2D,
    /// Layout to leave the image in (COLOR_ATTACHMENT_OPTIMAL for OpenXR).
    pub final_layout: vk::ImageLayout,
}

pub type PanelId = usize;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum QuadTexture {
    /// Solid colour (tinted by the quad colour).
    White,
    Panel(PanelId),
}

/// A textured quad in world space.
#[derive(Clone, Copy, Debug)]
pub struct QuadDraw {
    pub texture: QuadTexture,
    /// Top-left, top-right, bottom-right, bottom-left.
    pub corners: [Vec3; 4],
    /// Texture coordinates of the top-left and bottom-right corners.
    pub uv: [Vec2; 2],
    /// Linear, premultiplied tint.
    pub color: [f32; 4],
    /// Drawn for this eye only (0 left, 1 right); `None` for both.
    pub eye: Option<usize>,
}

impl QuadDraw {
    /// A quad centred on `transform`'s origin, `width` x `height` metres, in
    /// its local XY plane facing +Z.
    pub fn panel(
        texture: QuadTexture,
        transform: Mat4,
        width: f32,
        height: f32,
        opacity: f32,
    ) -> QuadDraw {
        let (w, h) = (width / 2.0, height / 2.0);
        let p = |x: f32, y: f32| transform.transform_point3(Vec3::new(x, y, 0.0));
        QuadDraw {
            texture,
            corners: [p(-w, h), p(w, h), p(w, -h), p(-w, -h)],
            uv: [Vec2::ZERO, Vec2::ONE],
            color: [opacity; 4],
            eye: None,
        }
    }

    /// A flat ribbon from `a` to `b`, `width` wide, facing `eye`.
    pub fn line(a: Vec3, b: Vec3, width: f32, eye: Vec3, color: [f32; 4]) -> QuadDraw {
        let dir = (b - a).normalize_or_zero();
        let to_eye = (eye - (a + b) * 0.5).normalize_or_zero();
        let side = dir.cross(to_eye).normalize_or_zero() * (width / 2.0);
        QuadDraw {
            texture: QuadTexture::White,
            corners: [a - side, a + side, b + side, b - side],
            uv: [Vec2::ZERO, Vec2::ONE],
            color,
            eye: None,
        }
    }
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct QuadVertex {
    pos: [f32; 3],
    uv: [f32; 2],
    color: [f32; 4],
}

struct Tex {
    image: Image,
    set: vk::DescriptorSet,
}

struct Panel {
    image: Image,
    set: vk::DescriptorSet,
    textures: HashMap<TextureId, Tex>,
    painted: bool,
    /// Bumped by every paint, so copies of the picture know when to update.
    version: u64,
}

struct Slot {
    cmd: vk::CommandBuffer,
    fence: vk::Fence,
    uniform: Buffer,
    scene_set: vk::DescriptorSet,
    scene_set_dirty: bool,
    /// The video image set (index, generation) the scene set points at.
    video_bound: Option<(usize, u64)>,
    /// Or the NV12 decoder buffer it points at, read in place
    /// ([`crate::shared`]), and that frame's description.
    shared_bound: Option<(u64, params::FrameDesc)>,
    /// Or the Q10C decoder buffer the TP10 eye pipeline samples
    /// ([`Tp10Eyes`]), and that frame's description.
    tp10_bound: Option<(u64, params::FrameDesc)>,
    has_video: bool,
    /// Frames in the decoder's buffers this slot's submission reads: kept
    /// until its fence signals, which gives the buffers back.
    frames: Vec<Arc<VideoFrame>>,
    /// Imports of decoder buffers to destroy with the garbage.
    garbage_imports: Vec<crate::shared::Imported>,
    quad_vb: Buffer,
    egui_vb: Buffer,
    egui_ib: Buffer,
    egui_vb_used: u64,
    egui_ib_used: u64,
    staging: Buffer,
    staging_used: u64,
    garbage: Vec<Image>,
    garbage_sets: Vec<vk::DescriptorSet>,
    garbage_buffers: Vec<Buffer>,
    /// Rooms let go of while a frame might still draw them.
    garbage_cinemas: Vec<Cinema>,
}

pub struct Renderer {
    gpu: Arc<Gpu>,
    color_format: vk::Format,
    pool: vk::CommandPool,
    dpool: vk::DescriptorPool,
    sampler: vk::Sampler,
    scene_layout: vk::DescriptorSetLayout,
    scene_pl: vk::PipelineLayout,
    scene_pipe: vk::Pipeline,
    /// The eye passes with comfort focus (`fs_main_comfort`).
    comfort_pipe: vk::Pipeline,
    /// Comfort focus's samples (`fs_focus`), with the eye passes' bindings.
    focus_pipe: vk::Pipeline,
    tex_layout: vk::DescriptorSetLayout,
    quad_pl: vk::PipelineLayout,
    quad_pipe: vk::Pipeline,
    egui_pl: vk::PipelineLayout,
    egui_pipe: vk::Pipeline,
    /// How the video's and the panels' pipelines meet the shading rates.
    video_rate: ShadingRate,
    panel_rate: ShadingRate,
    /// Each [`CinemaRoom`] once loaded, and while loading.
    cinemas: [Option<Cinema>; ROOMS],
    cinema_loads: [Option<JoinHandle<Result<RoomData>>>; ROOMS],
    white: Tex,
    dummy: Image,
    panels: Vec<Option<Panel>>,
    slots: Vec<Slot>,
    /// Two video image sets shared by the slots (see `set_video`), which
    /// is newest, and how often each was recreated.
    videos: [Option<VideoPlanes>; 2],
    video_cur: usize,
    video_gen: [u64; 2],
    /// Shows hardware-decoded 10-bit frames, on GPUs that can.
    q10c: Option<Converter>,
    /// Samples 10-bit frames read in place without converting them.
    tp10_eyes: Option<Tp10Eyes>,
    /// Each eye's shading rates, where the GPU takes them.
    rates: Option<RateMaps>,
    foveation: Foveation,
    /// Where the eyes look, in the play space ([`Renderer::set_gaze`]).
    gaze: Option<Vec3>,
    /// Measures HDR frames for `hdr_peak`; without it HDR keeps the
    /// default peak.
    peak: Option<PeakPass>,
    hdr_peak: PeakDetector,
    /// Measures stereo disparity at the gaze point for comfort focus.
    focus: Option<FocusPass>,
    focus_aim: AimFilter,
    comfort_focus: ComfortFocus,
    /// When comfort focus last measured, where it aimed (play space) and the
    /// video frame it read.
    focus_measured: Option<(std::time::Instant, Vec3, Option<u64>)>,
    /// What comfort focus did lately, for the log ([`Renderer::focus_line`]),
    /// and whether each slot's frame in flight was drawn with it: with a
    /// measurement, or without.
    focus_log: FocusLog,
    focus_drawn: [Option<bool>; SLOTS],
    /// The video frame shown ([`fp_media::VideoFrame::id`]).
    shown_frame: Option<u64>,
    glow: crate::glow::Glow,
    /// Times each frame's parts on the GPU, where it has timestamps.
    timer: Option<FrameTimer>,
    /// The parts of the last frame the GPU finished.
    gpu_times: Option<GpuTimes>,
    /// Decoder buffers imported to be read in place.
    imports: crate::shared::Imports,
    /// The decoder buffer set of the last shared frame, whose imports stay
    /// while frames come copied now and then.
    shared_set: Option<u64>,
    /// What the frame shown last is, when it is an NV12 one read in the
    /// decoder's buffer: such frames have no image set to ask.
    shared_desc: Option<params::FrameDesc>,
    current: usize,
    recording: bool,
}

impl Renderer {
    /// `color_format` is the sRGB format of the eye targets.
    pub fn new(gpu: Arc<Gpu>, color_format: vk::Format) -> Result<Renderer> {
        let d = &gpu.device;
        // SAFETY: plain object creation on a valid device.
        unsafe {
            let pool = d
                .create_command_pool(
                    &vk::CommandPoolCreateInfo::default()
                        .queue_family_index(gpu.queue_family)
                        .flags(vk::CommandPoolCreateFlags::RESET_COMMAND_BUFFER),
                    None,
                )
                .ctx("command pool")?;
            let sizes = [
                vk::DescriptorPoolSize {
                    ty: vk::DescriptorType::UNIFORM_BUFFER,
                    descriptor_count: 16,
                },
                vk::DescriptorPoolSize {
                    ty: vk::DescriptorType::SAMPLER,
                    descriptor_count: 1024,
                },
                vk::DescriptorPoolSize {
                    ty: vk::DescriptorType::SAMPLED_IMAGE,
                    descriptor_count: 2048,
                },
            ];
            let dpool = d
                .create_descriptor_pool(
                    &vk::DescriptorPoolCreateInfo::default()
                        .flags(vk::DescriptorPoolCreateFlags::FREE_DESCRIPTOR_SET)
                        .max_sets(1024)
                        .pool_sizes(&sizes),
                    None,
                )
                .ctx("descriptor pool")?;
            let sampler = d
                .create_sampler(
                    &vk::SamplerCreateInfo::default()
                        .mag_filter(vk::Filter::LINEAR)
                        .min_filter(vk::Filter::LINEAR)
                        .mipmap_mode(vk::SamplerMipmapMode::LINEAR)
                        .address_mode_u(vk::SamplerAddressMode::CLAMP_TO_EDGE)
                        .address_mode_v(vk::SamplerAddressMode::CLAMP_TO_EDGE)
                        .address_mode_w(vk::SamplerAddressMode::CLAMP_TO_EDGE)
                        .max_lod(vk::LOD_CLAMP_NONE),
                    None,
                )
                .ctx("sampler")?;

            // Fewer fragments away from the gaze, where the GPU allows it.
            let foveatable = gpu.shading_rates.is_some();
            let (video_rate, panel_rate) = if foveatable {
                (ShadingRate::Attachment, ShadingRate::Full)
            } else {
                (ShadingRate::Unused, ShadingRate::Unused)
            };
            let scene_layout = pipeline::set_layout(
                &gpu,
                &[
                    (0, vk::DescriptorType::UNIFORM_BUFFER),
                    (1, vk::DescriptorType::SAMPLER),
                    (2, vk::DescriptorType::SAMPLED_IMAGE),
                    (3, vk::DescriptorType::SAMPLED_IMAGE),
                    (4, vk::DescriptorType::SAMPLED_IMAGE),
                ],
            )?;
            let scene_pl = pipeline::layout(&gpu, &[scene_layout], 4)?;
            let scene_pipe = pipeline::create(
                &gpu,
                &PipelineDesc {
                    spv: crate::SCENE_SPV,
                    fragment: c"fs_main",
                    layout: scene_pl,
                    color_format,
                    depth: Depth::None,
                    vertex_stride: 0,
                    instance_stride: 0,
                    attributes: &[],
                    blend: false,
                    descriptor_buffer: false,
                    shading_rate: video_rate,
                },
            )?;
            let comfort_pipe = pipeline::create(
                &gpu,
                &PipelineDesc {
                    spv: crate::SCENE_SPV,
                    fragment: c"fs_main_comfort",
                    layout: scene_pl,
                    color_format,
                    depth: Depth::None,
                    vertex_stride: 0,
                    instance_stride: 0,
                    attributes: &[],
                    blend: false,
                    descriptor_buffer: false,
                    shading_rate: video_rate,
                },
            )?;
            let focus_pipe = pipeline::create(
                &gpu,
                &PipelineDesc {
                    spv: crate::SCENE_SPV,
                    fragment: c"fs_focus",
                    layout: scene_pl,
                    color_format: crate::focus::SAMPLES_FORMAT,
                    depth: Depth::None,
                    vertex_stride: 0,
                    instance_stride: 0,
                    attributes: &[],
                    blend: false,
                    descriptor_buffer: false,
                    shading_rate: ShadingRate::Unused,
                },
            )?;
            let tex_layout = pipeline::set_layout(
                &gpu,
                &[
                    (0, vk::DescriptorType::SAMPLER),
                    (1, vk::DescriptorType::SAMPLED_IMAGE),
                ],
            )?;
            let quad_pl = pipeline::layout(&gpu, &[tex_layout], 64)?;
            let quad_pipe = quad_pipeline(&gpu, quad_pl, color_format, Depth::None, panel_rate)?;
            let egui_pl = pipeline::layout(&gpu, &[tex_layout], 8)?;
            let egui_attrs = [
                vk::VertexInputAttributeDescription {
                    location: 0,
                    binding: 0,
                    format: vk::Format::R32G32_SFLOAT,
                    offset: 0,
                },
                vk::VertexInputAttributeDescription {
                    location: 1,
                    binding: 0,
                    format: vk::Format::R32G32_SFLOAT,
                    offset: 8,
                },
                vk::VertexInputAttributeDescription {
                    location: 2,
                    binding: 0,
                    format: vk::Format::R8G8B8A8_UNORM,
                    offset: 16,
                },
            ];
            let egui_pipe = pipeline::create(
                &gpu,
                &PipelineDesc {
                    spv: crate::EGUI_SPV,
                    fragment: c"fs_main",
                    layout: egui_pl,
                    color_format: PANEL_FORMATS[0],
                    depth: Depth::None,
                    vertex_stride: 20,
                    instance_stride: 0,
                    attributes: &egui_attrs,
                    blend: true,
                    descriptor_buffer: false,
                    shading_rate: ShadingRate::Unused,
                },
            )?;

            let mut r = Renderer {
                gpu: gpu.clone(),
                color_format,
                pool,
                dpool,
                sampler,
                scene_layout,
                scene_pl,
                scene_pipe,
                comfort_pipe,
                focus_pipe,
                tex_layout,
                quad_pl,
                quad_pipe,
                egui_pl,
                egui_pipe,
                video_rate,
                panel_rate,
                cinemas: Default::default(),
                cinema_loads: Default::default(),
                white: Tex {
                    image: Image::new(&gpu, 1, 1, &PANEL_FORMATS, usage_sampled(), 1, "white")?,
                    set: vk::DescriptorSet::null(),
                },
                dummy: Image::new(
                    &gpu,
                    1,
                    1,
                    &[vk::Format::R8G8B8A8_UNORM],
                    usage_sampled(),
                    1,
                    "dummy",
                )?,
                panels: Vec::new(),
                slots: Vec::new(),
                videos: [None, None],
                video_cur: 0,
                video_gen: [0, 0],
                q10c: Converter::new(&gpu),
                tp10_eyes: None,
                rates: RateMaps::new(&gpu, SLOTS),
                foveation: Foveation::Off,
                gaze: None,
                peak: PeakPass::new(&gpu, SLOTS)
                    .inspect_err(|e| log::warn!("HDR scenes aren't measured: {e}"))
                    .ok(),
                hdr_peak: PeakDetector::default(),
                focus: FocusPass::new(&gpu, SLOTS)
                    .inspect_err(|e| log::warn!("comfort focus isn't measured: {e}"))
                    .ok(),
                focus_aim: AimFilter::default(),
                comfort_focus: ComfortFocus::default(),
                focus_measured: None,
                focus_log: FocusLog::default(),
                focus_drawn: [None; SLOTS],
                shown_frame: None,
                glow: Default::default(),
                timer: FrameTimer::new(&gpu, SLOTS),
                gpu_times: None,
                imports: Default::default(),
                shared_set: None,
                shared_desc: None,
                current: 0,
                recording: false,
            };
            r.white.set = r.tex_set(r.white.image.views[1])?;
            // Initialise the white and dummy images.
            let (white, dummy) = (r.white.image.image, r.dummy.image);
            r.one_shot(|gpu, cmd| {
                for (img, value) in [(white, 1.0f32), (dummy, 0.0)] {
                    mem::barrier(
                        gpu,
                        cmd,
                        img,
                        mem::range(0, 1),
                        vk::ImageLayout::UNDEFINED,
                        vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                        mem::TOP,
                        mem::TRANSFER_WRITE,
                    );
                    gpu.device.cmd_clear_color_image(
                        cmd,
                        img,
                        vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                        &vk::ClearColorValue {
                            float32: [value; 4],
                        },
                        &[mem::range(0, 1)],
                    );
                    mem::barrier(
                        gpu,
                        cmd,
                        img,
                        mem::range(0, 1),
                        vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                        vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL,
                        mem::TRANSFER_WRITE,
                        mem::SHADER_READ,
                    );
                }
            })?;
            r.tp10_eyes = tp10_eyes(
                &r.gpu,
                color_format,
                r.sampler,
                r.q10c.is_some(),
                video_rate,
            );
            if !foveatable {
                log::info!("eye passes can't be foveated: no fragment shading rate attachments");
            }
            for _ in 0..SLOTS {
                let slot = r.new_slot()?;
                r.slots.push(slot);
            }
            Ok(r)
        }
    }

    pub fn gpu(&self) -> &Arc<Gpu> {
        &self.gpu
    }

    pub fn color_format(&self) -> vk::Format {
        self.color_format
    }

    /// Lets go of every room but `keep`, which a video is about to need,
    /// so their textures don't stay in memory: one starts loading again
    /// when [`Self::load_cinema`] asks for it.
    pub fn keep_cinema(&mut self, keep: Option<CinemaRoom>) {
        for room in CinemaRoom::ALL {
            if Some(room) == keep {
                continue;
            }
            // A read in progress finishes on its own and is dropped.
            self.cinema_loads[room.slot()] = None;
            if let Some(cinema) = self.cinemas[room.slot()].take() {
                log::info!("{room:?} cinema let go");
                let slot = &mut self.slots[self.current];
                slot.garbage_sets.extend(cinema.pool_sets());
                slot.garbage_cinemas.push(cinema);
            }
        }
    }

    /// Whether `room` is ready to draw. The first call starts reading it on
    /// a thread of its own (opening it, its mesh and decoding its texture take
    /// a while); a call once that is done uploads it. Nothing is drawn of a
    /// room that fails, as the log says.
    pub fn load_cinema(&mut self, room: CinemaRoom) -> bool {
        let i = room.slot();
        if self.cinemas[i].is_some() {
            return true;
        }
        if !room.is_available() {
            return false;
        }
        let load = match self.cinema_loads[i].take() {
            None => {
                let started = std::time::Instant::now();
                let thread = std::thread::Builder::new()
                    .name("cinema load".into())
                    .spawn(move || {
                        let data = crate::cinema_mesh::load(room);
                        log::info!("{room:?} cinema read in {:.0?}", started.elapsed());
                        data
                    });
                match thread {
                    Ok(thread) => self.cinema_loads[i] = Some(thread),
                    Err(e) => {
                        log::warn!("{room:?} cinema not loaded: {e}");
                        room.fail();
                    }
                }
                return false;
            }
            Some(load) if !load.is_finished() => {
                self.cinema_loads[i] = Some(load);
                return false;
            }
            Some(load) => load,
        };
        let started = std::time::Instant::now();
        let made = load
            .join()
            .unwrap_or_else(|_| Err(crate::Error::Unsupported("loading panicked".into())))
            .and_then(|data| self.make_cinema(data));
        match made {
            Ok(cinema) => {
                log::info!("{room:?} cinema uploaded in {:.0?}", started.elapsed());
                self.cinemas[i] = Some(cinema);
                true
            }
            Err(e) => {
                log::warn!("{room:?} cinema unavailable, showing the flat video: {e}");
                room.fail();
                false
            }
        }
    }

    fn make_cinema(&self, data: RoomData) -> Result<Cinema> {
        let (video_rate, panel_rate) = (self.video_rate, self.panel_rate);
        Cinema::new(&self.gpu, |room| {
            room.layout = pipeline::layout(
                &self.gpu,
                &[self.tex_layout, self.tex_layout],
                crate::cinema::PUSH_BYTES,
            )?;
            room.pipeline =
                crate::cinema::pipeline(&self.gpu, room.layout, self.color_format, video_rate)?;
            room.scene_pipe = pipeline::create(
                &self.gpu,
                &PipelineDesc {
                    spv: crate::SCENE_SPV,
                    fragment: c"fs_main",
                    layout: self.scene_pl,
                    color_format: self.color_format,
                    // Behind the seats in front, at `screen_depth`.
                    depth: Depth::Tested,
                    vertex_stride: 0,
                    instance_stride: 0,
                    attributes: &[],
                    blend: true,
                    descriptor_buffer: false,
                    shading_rate: video_rate,
                },
            )?;
            room.quad_pipe = quad_pipeline(
                &self.gpu,
                self.quad_pl,
                self.color_format,
                Depth::Attached,
                panel_rate,
            )?;
            room.upload(self, data, SLOTS)?;
            Ok(())
        })
    }

    fn new_slot(&self) -> Result<Slot> {
        let d = &self.gpu.device;
        // SAFETY: allocation from our pool; fence created signalled so the
        // first wait returns immediately.
        unsafe {
            let cmd = d
                .allocate_command_buffers(
                    &vk::CommandBufferAllocateInfo::default()
                        .command_pool(self.pool)
                        .command_buffer_count(1),
                )
                .ctx("command buffer")?[0];
            let fence = d
                .create_fence(
                    &vk::FenceCreateInfo::default().flags(vk::FenceCreateFlags::SIGNALED),
                    None,
                )
                .ctx("fence")?;
            let layouts = [self.scene_layout];
            let scene_set = d
                .allocate_descriptor_sets(
                    &vk::DescriptorSetAllocateInfo::default()
                        .descriptor_pool(self.dpool)
                        .set_layouts(&layouts),
                )
                .ctx("scene descriptor set")?[0];
            let uniform = Buffer::new(
                &self.gpu,
                size_of::<SceneParams>() as u64,
                vk::BufferUsageFlags::UNIFORM_BUFFER,
                MemoryLocation::CpuToGpu,
                "scene params",
            )?;
            let cpu = |size, usage, name| {
                Buffer::new(&self.gpu, size, usage, MemoryLocation::CpuToGpu, name)
            };
            Ok(Slot {
                cmd,
                fence,
                uniform,
                scene_set,
                scene_set_dirty: true,
                video_bound: None,
                shared_bound: None,
                tp10_bound: None,
                has_video: false,
                frames: Vec::new(),
                garbage_imports: Vec::new(),
                quad_vb: cpu(64 << 10, vk::BufferUsageFlags::VERTEX_BUFFER, "quads")?,
                egui_vb: cpu(
                    1 << 20,
                    vk::BufferUsageFlags::VERTEX_BUFFER,
                    "egui vertices",
                )?,
                egui_ib: cpu(1 << 20, vk::BufferUsageFlags::INDEX_BUFFER, "egui indices")?,
                egui_vb_used: 0,
                egui_ib_used: 0,
                staging: cpu(4 << 20, vk::BufferUsageFlags::TRANSFER_SRC, "egui staging")?,
                staging_used: 0,
                garbage: Vec::new(),
                garbage_sets: Vec::new(),
                garbage_buffers: Vec::new(),
                garbage_cinemas: Vec::new(),
            })
        }
    }

    pub(crate) fn tex_set(&self, view: vk::ImageView) -> Result<vk::DescriptorSet> {
        self.tex_set_with(view, self.sampler)
    }

    pub(crate) fn tex_set_with(
        &self,
        view: vk::ImageView,
        sampler: vk::Sampler,
    ) -> Result<vk::DescriptorSet> {
        let layouts = [self.tex_layout];
        // SAFETY: allocation from our pool and writes of valid handles.
        unsafe {
            let set = self
                .gpu
                .device
                .allocate_descriptor_sets(
                    &vk::DescriptorSetAllocateInfo::default()
                        .descriptor_pool(self.dpool)
                        .set_layouts(&layouts),
                )
                .ctx("texture descriptor set")?[0];
            let samplers = [vk::DescriptorImageInfo::default().sampler(sampler)];
            let images = [vk::DescriptorImageInfo::default()
                .image_view(view)
                .image_layout(vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL)];
            let writes = [
                vk::WriteDescriptorSet::default()
                    .dst_set(set)
                    .dst_binding(0)
                    .descriptor_type(vk::DescriptorType::SAMPLER)
                    .image_info(&samplers),
                vk::WriteDescriptorSet::default()
                    .dst_set(set)
                    .dst_binding(1)
                    .descriptor_type(vk::DescriptorType::SAMPLED_IMAGE)
                    .image_info(&images),
            ];
            self.gpu.device.update_descriptor_sets(&writes, &[]);
            Ok(set)
        }
    }

    /// Records and runs commands immediately, waiting for completion.
    pub fn one_shot(&self, f: impl FnOnce(&Gpu, vk::CommandBuffer)) -> Result<()> {
        let d = &self.gpu.device;
        // SAFETY: temporary command buffer and fence, freed before returning.
        unsafe {
            let cmd = d
                .allocate_command_buffers(
                    &vk::CommandBufferAllocateInfo::default()
                        .command_pool(self.pool)
                        .command_buffer_count(1),
                )
                .ctx("one-shot command buffer")?[0];
            d.begin_command_buffer(
                cmd,
                &vk::CommandBufferBeginInfo::default()
                    .flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT),
            )
            .ctx("begin")?;
            f(&self.gpu, cmd);
            d.end_command_buffer(cmd).ctx("end")?;
            let fence = d
                .create_fence(&vk::FenceCreateInfo::default(), None)
                .ctx("fence")?;
            let cmds = [cmd];
            d.queue_submit(
                self.gpu.queue,
                &[vk::SubmitInfo::default().command_buffers(&cmds)],
                fence,
            )
            .ctx("submit")?;
            let r = d.wait_for_fences(&[fence], true, u64::MAX).ctx("wait");
            d.destroy_fence(fence, None);
            d.free_command_buffers(self.pool, &cmds);
            r
        }
    }

    /// Creates an off-screen UI panel of `width` x `height` pixels.
    pub fn create_panel(&mut self, width: u32, height: u32) -> Result<PanelId> {
        let mips = 32 - width.max(height).leading_zeros();
        let image = Image::new(
            &self.gpu,
            width,
            height,
            &PANEL_FORMATS,
            vk::ImageUsageFlags::COLOR_ATTACHMENT
                | vk::ImageUsageFlags::SAMPLED
                | vk::ImageUsageFlags::TRANSFER_SRC
                | vk::ImageUsageFlags::TRANSFER_DST,
            mips,
            "ui panel",
        )?;
        let set = self.tex_set(image.views[1])?;
        let img = image.image;
        // SAFETY: img was just created with `mips` levels and is not in use;
        // cmd is recording.
        self.one_shot(|gpu, cmd| unsafe {
            mem::barrier(
                gpu,
                cmd,
                img,
                mem::range(0, mips),
                vk::ImageLayout::UNDEFINED,
                vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                mem::TOP,
                mem::TRANSFER_WRITE,
            );
            gpu.device.cmd_clear_color_image(
                cmd,
                img,
                vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                &vk::ClearColorValue { float32: [0.0; 4] },
                &[mem::range(0, mips)],
            );
            mem::barrier(
                gpu,
                cmd,
                img,
                mem::range(0, mips),
                vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL,
                mem::TRANSFER_WRITE,
                mem::SHADER_READ,
            );
        })?;
        let panel = Panel {
            image,
            set,
            textures: HashMap::new(),
            painted: false,
            version: 0,
        };
        match self.panels.iter().position(Option::is_none) {
            Some(i) => {
                self.panels[i] = Some(panel);
                Ok(i)
            }
            None => {
                self.panels.push(Some(panel));
                Ok(self.panels.len() - 1)
            }
        }
    }

    pub fn panel_size(&self, id: PanelId) -> Option<(u32, u32)> {
        self.panels
            .get(id)?
            .as_ref()
            .map(|p| (p.image.extent.width, p.image.extent.height))
    }

    /// Destroys a panel (deferred until no frame uses it).
    pub fn destroy_panel(&mut self, id: PanelId) {
        if let Some(Some(p)) = self.panels.get_mut(id).map(Option::take) {
            let slot = &mut self.slots[self.current];
            slot.garbage.push(p.image);
            slot.garbage_sets.push(p.set);
            for (_, t) in p.textures {
                slot.garbage.push(t.image);
                slot.garbage_sets.push(t.set);
            }
        }
    }

    /// Waits for the next slot and starts recording.
    pub fn begin_frame(&mut self) -> Result<()> {
        self.current = (self.current + 1) % SLOTS;
        let d = &self.gpu.device;
        let slot = &mut self.slots[self.current];
        // SAFETY: the slot's previous submission is complete after the wait,
        // so its resources and garbage can be reused/destroyed.
        unsafe {
            d.wait_for_fences(&[slot.fence], true, u64::MAX)
                .ctx("wait for frame")?;
            for mut img in slot.garbage.drain(..) {
                img.destroy(&self.gpu);
            }
            if !slot.garbage_sets.is_empty() {
                let _ = d.free_descriptor_sets(self.dpool, &slot.garbage_sets);
                slot.garbage_sets.clear();
            }
            for mut b in slot.garbage_buffers.drain(..) {
                b.destroy(&self.gpu);
            }
            for mut i in slot.garbage_imports.drain(..) {
                i.destroy(&self.gpu);
            }
            for room in slot.garbage_cinemas.drain(..) {
                room.destroy(&self.gpu);
            }
            // Its decoder buffers go back to the decoder.
            slot.frames.clear();
            // A decoder buffer is read only in a frame that took it over
            // (set_video), so the binding doesn't carry over.
            if slot.shared_bound.take().is_some() | slot.tp10_bound.take().is_some() {
                slot.has_video = false;
                slot.scene_set_dirty = true;
            }
            d.reset_command_buffer(slot.cmd, vk::CommandBufferResetFlags::empty())
                .ctx("reset")?;
            d.begin_command_buffer(
                slot.cmd,
                &vk::CommandBufferBeginInfo::default()
                    .flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT),
            )
            .ctx("begin frame")?;
        }
        slot.egui_vb_used = 0;
        slot.egui_ib_used = 0;
        slot.staging_used = 0;
        let cmd = slot.cmd;
        if let Some(hist) = self.peak.as_mut().and_then(|p| p.take(self.current)) {
            self.hdr_peak.add(&hist);
        }
        if let Some((scores, along)) = self.focus.as_mut().and_then(|f| f.take(self.current)) {
            let gave = self.comfort_focus.ingest(&scores, along);
            self.focus_log.read_back(gave);
        }
        self.glow.read(self.current);
        if let Some(t) = self.timer.as_mut() {
            // Read before `begin` resets the queries for this frame.
            self.gpu_times = t.read(&self.gpu, self.current);
            t.begin(&self.gpu, cmd, self.current);
        }
        if let Some(measured) = self.focus_drawn[self.current].take()
            && let Some(ms) = self.gpu_times.and_then(|t| t.eyes)
        {
            self.focus_log.gpu(measured, ms);
        }
        self.recording = true;
        Ok(())
    }

    /// The parts of the last frame the GPU finished, two frames back.
    pub fn gpu_times(&self) -> Option<GpuTimes> {
        self.gpu_times
    }

    /// What comfort focus did since the last call, as one line for the log:
    /// how often it measured and how many of those gave a distance, what is
    /// in focus, how often the eyes aimed and what a measurement costs.
    /// `None` when no frame was drawn with it.
    pub fn focus_line(&mut self) -> Option<String> {
        self.focus_log
            .take_line(std::time::Instant::now(), self.comfort_focus.in_focus())
    }

    fn stamp(&mut self, mark: Mark) {
        if !self.recording {
            return;
        }
        let cmd = self.slots[self.current].cmd;
        if let Some(t) = self.timer.as_mut() {
            t.stamp(&self.gpu, cmd, self.current, mark);
        }
    }

    /// Uploads the frame to show (or clears the video when `None`).
    /// Shows `frame` this frame. Each video frame is uploaded once, into
    /// whichever of the two shared image sets the frame in flight isn't
    /// reading: an 8K frame is 50 MB, and copying it for every frame slot
    /// cost the render thread its 90 Hz deadline.
    ///
    /// A frame in the decoder's own buffer ([`VideoFrame::shared`]) is read
    /// there instead, without a copy, and kept until the GPU is done with it.
    pub fn set_video(&mut self, frame: Option<&Arc<VideoFrame>>) -> Result<()> {
        self.stamp(Mark::Video);
        let r = self.show_video(frame);
        self.stamp(Mark::VideoDone);
        r
    }

    fn show_video(&mut self, frame: Option<&Arc<VideoFrame>>) -> Result<()> {
        self.shown_frame = frame.map(|f| f.id);
        let Some(f) = frame else {
            let slot = &mut self.slots[self.current];
            if slot.has_video {
                slot.scene_set_dirty = true;
            }
            slot.has_video = false;
            slot.shared_bound = None;
            slot.tp10_bound = None;
            // Nothing playing: the decoder's buffers can go.
            self.imports.sweep(None, &mut slot.garbage_imports);
            self.shared_set = None;
            self.shared_desc = None;
            self.forget_scene();
            return Ok(());
        };
        if let Some(p) = f.shared().copied() {
            let r = self.set_shared_video(f, &p);
            if let Err(e) = &r {
                log::warn!(
                    "reading the decoder's buffer failed, frames are copied from now on: {e}"
                );
                fp_media::set_shared_frames(false);
                let slot = &mut self.slots[self.current];
                slot.has_video = false;
                slot.shared_bound = None;
                slot.tp10_bound = None;
                slot.scene_set_dirty = true;
            }
            return r;
        }
        {
            let slot = &mut self.slots[self.current];
            if slot.shared_bound.take().is_some() | slot.tp10_bound.take().is_some() {
                slot.scene_set_dirty = true;
            }
            self.imports
                .sweep(self.shared_set, &mut slot.garbage_imports);
        }
        let resident = self.videos[self.video_cur]
            .as_ref()
            .is_some_and(|v| v.key == Some(f.id));
        if !resident {
            // The other set was last read by the frame before the one in
            // flight, which begin_frame has waited for.
            let target = 1 - self.video_cur;
            let slot = &mut self.slots[self.current];
            match VideoPlanes::upload(
                &mut self.videos[target],
                &self.gpu,
                slot.cmd,
                f,
                &mut slot.garbage,
                self.q10c.as_ref(),
            ) {
                Ok(recreated) => {
                    if recreated {
                        self.video_gen[target] += 1;
                        // Another size or layout: most likely another video.
                        self.hdr_peak.reset();
                        if let Some(p) = self.peak.as_mut() {
                            p.discard();
                        }
                        self.comfort_focus.reset();
                        if let Some(f) = self.focus.as_mut() {
                            f.discard();
                        }
                    }
                    self.video_cur = target;
                    if let (Some(p), Some(v)) = (self.peak.as_mut(), self.videos[target].as_ref()) {
                        p.record(&self.gpu, slot.cmd, self.current, v);
                    }
                }
                Err(e) => {
                    log::warn!("video frame upload failed: {e}");
                    slot.has_video = false;
                    slot.scene_set_dirty = true;
                    return Err(e);
                }
            }
        }
        self.bind_video_set();
        Ok(())
    }

    /// Points this slot's scene set at the newest video image set.
    fn bind_video_set(&mut self) {
        self.shared_desc = None;
        let bound = (self.video_cur, self.video_gen[self.video_cur]);
        let slot = &mut self.slots[self.current];
        slot.tp10_bound = None;
        if slot.video_bound != Some(bound) || !slot.has_video {
            slot.scene_set_dirty = true;
            slot.video_bound = Some(bound);
        }
        slot.has_video = true;
    }

    /// Shows a frame in the decoder's buffer `p`: NV12 sampled there by
    /// the eye passes, Q10C converted from there into an image set.
    fn set_shared_video(&mut self, f: &Arc<VideoFrame>, p: &fp_media::SharedPicture) -> Result<()> {
        let slot = &mut self.slots[self.current];
        self.imports.sweep(Some(p.set), &mut slot.garbage_imports);
        self.shared_set = Some(p.set);
        if !slot.frames.iter().any(|g| g.id == f.id) {
            slot.frames.push(f.clone());
        }
        match f.layout {
            fp_media::PixelLayout::Nv12 => {
                self.imports
                    .nv12(&self.gpu, slot.cmd, p, (f.width, f.height))?;
                let desc = params::FrameDesc {
                    width: f.width,
                    height: f.height,
                    layout: f.layout,
                    color: f.color,
                };
                if slot.shared_bound.map(|(b, _)| b) != Some(p.buffer) || !slot.has_video {
                    slot.scene_set_dirty = true;
                }
                slot.shared_bound = Some((p.buffer, desc));
                slot.tp10_bound = None;
                slot.video_bound = None;
                slot.has_video = true;
                self.shared_desc = Some(desc);
                Ok(())
            }
            // HDR scenes are measured in converted planes, so HDR converts.
            fp_media::PixelLayout::Q10c
                if self.tp10_eyes.is_some()
                    && f.color.transfer == fp_media::info::Transfer::Sdr =>
            {
                self.sample_shared_q10c(f, p)
            }
            fp_media::PixelLayout::Q10c => {
                self.convert_shared_q10c(f, p)?;
                self.bind_video_set();
                Ok(())
            }
            other => Err(crate::Error::Unsupported(format!(
                "decoder buffers of {other:?} frames"
            ))),
        }
    }

    /// Shows the Q10C frame `f` in decoder buffer `p` by sampling it there
    /// in the eye passes ([`Tp10Eyes`]): no conversion, no image set.
    fn sample_shared_q10c(
        &mut self,
        f: &Arc<VideoFrame>,
        p: &fp_media::SharedPicture,
    ) -> Result<()> {
        let Some(conv) = self.q10c.as_ref() else {
            return Err(crate::Error::Unsupported(
                "this GPU can't show 10-bit hardware-decoded (Q10C) frames".into(),
            ));
        };
        let geometry = crate::q10c::Geometry {
            width: f.width,
            height: f.height,
            stride: p.stride,
            rows: p.rows,
            uv_offset: p.uv_offset,
            len: p.len,
        };
        let slot = &mut self.slots[self.current];
        let gpu = &self.gpu;
        self.imports
            .tp10(gpu, slot.cmd, p, || conv.sample(gpu, geometry, p.fd))?;
        let desc = params::FrameDesc {
            width: f.width,
            height: f.height,
            layout: f.layout,
            color: f.color,
        };
        slot.tp10_bound = Some((p.buffer, desc));
        slot.shared_bound = None;
        slot.video_bound = None;
        if !slot.has_video {
            slot.scene_set_dirty = true;
        }
        slot.has_video = true;
        self.shared_desc = Some(desc);
        Ok(())
    }

    /// Converts the Q10C frame `f` in decoder buffer `p` into the image set
    /// the frame in flight isn't reading, as copied ones are (`set_video`).
    fn convert_shared_q10c(
        &mut self,
        f: &Arc<VideoFrame>,
        p: &fp_media::SharedPicture,
    ) -> Result<()> {
        if self.videos[self.video_cur]
            .as_ref()
            .is_some_and(|v| v.key == Some(f.id))
        {
            return Ok(());
        }
        let Some(conv) = self.q10c.as_ref() else {
            return Err(crate::Error::Unsupported(
                "this GPU can't show 10-bit hardware-decoded (Q10C) frames".into(),
            ));
        };
        let target = 1 - self.video_cur;
        let slot = &mut self.slots[self.current];
        if VideoPlanes::q10c_outputs(&mut self.videos[target], &self.gpu, f, &mut slot.garbage)? {
            self.video_gen[target] += 1;
            self.imports
                .sweep_target((target, self.video_gen[target]), &mut slot.garbage_imports);
            // Another size: most likely another video.
            self.hdr_peak.reset();
            if let Some(peak) = self.peak.as_mut() {
                peak.discard();
            }
            self.comfort_focus.reset();
            if let Some(f) = self.focus.as_mut() {
                f.discard();
            }
        }
        let Some(set) = self.videos[target].as_mut() else {
            return Ok(());
        };
        let geometry = crate::q10c::Geometry {
            width: f.width,
            height: f.height,
            stride: p.stride,
            rows: p.rows,
            uv_offset: p.uv_offset,
            len: p.len,
        };
        let gpu = &self.gpu;
        let picture = self.imports.q10c(p, (target, self.video_gen[target]), || {
            conv.import(gpu, geometry, p.fd, &set.images)
        })?;
        set.desc.color = f.color;
        conv.record(gpu, slot.cmd, picture, &set.images);
        set.key = Some(f.id);
        self.video_cur = target;
        // HDR frames are measured in the converted planes, as copied ones are.
        if let (Some(peak), Some(v)) = (self.peak.as_mut(), self.videos[target].as_ref()) {
            peak.record(&self.gpu, slot.cmd, self.current, v);
        }
        Ok(())
    }

    /// Whether hardware-decoded 10-bit HEVC ([`fp_media::PixelLayout::Q10c`]
    /// frames) can be shown; see [`fp_media::set_hevc_10bit_hardware`].
    pub fn shows_q10c(&self) -> bool {
        self.q10c.is_some()
    }

    /// The HDR scene peak in nits that tone mapping uses now.
    pub fn hdr_peak(&self) -> f32 {
        self.hdr_peak.peak_nits()
    }

    /// The video shown last, if it is HDR: its transfer and the scene peak
    /// in nits tone mapping uses.
    pub fn hdr_scene(&self) -> Option<(fp_media::info::Transfer, f32)> {
        let desc = match self.shared_desc {
            Some(desc) => desc,
            None => self.videos[self.video_cur].as_ref()?.desc,
        };
        (desc.color.transfer != fp_media::info::Transfer::Sdr)
            .then(|| (desc.color.transfer, self.hdr_peak.peak_nits()))
    }

    /// Starts the HDR peak afresh, ignoring measurements still on their way.
    fn forget_scene(&mut self) {
        self.hdr_peak.reset();
        self.glow.reset();
        if let Some(p) = self.peak.as_mut() {
            p.discard();
        }
        self.comfort_focus.reset();
        if let Some(f) = self.focus.as_mut() {
            f.discard();
        }
    }

    /// Whether this GPU reads frames in the hardware decoder's own buffers
    /// instead of copies; see [`fp_media::set_shared_frames`].
    pub fn reads_decoder_buffers(&self) -> bool {
        self.gpu.memory_fd.is_some()
    }

    /// The video image set this slot shows, if any.
    fn slot_video(&self, slot: usize) -> Option<&VideoPlanes> {
        let s = &self.slots[slot];
        if !s.has_video {
            return None;
        }
        self.videos[s.video_bound?.0].as_ref()
    }

    fn staging_alloc(&mut self, bytes: &[u8]) -> Result<(vk::Buffer, u64)> {
        let slot = &mut self.slots[self.current];
        let need = slot.staging_used + bytes.len() as u64 + 256;
        if need > slot.staging.size {
            let bigger = Buffer::new(
                &self.gpu,
                need.max(slot.staging.size * 2),
                vk::BufferUsageFlags::TRANSFER_SRC,
                MemoryLocation::CpuToGpu,
                "egui staging",
            )?;
            let old = std::mem::replace(&mut slot.staging, bigger);
            slot.garbage_buffers.push(old);
            // Earlier copies this frame reference the old buffer; it stays
            // alive until the slot comes round again.
            slot.staging_used = 0;
        }
        let off = (slot.staging_used + 15) & !15;
        slot.staging.bytes()[off as usize..off as usize + bytes.len()].copy_from_slice(bytes);
        slot.staging_used = off + bytes.len() as u64;
        Ok((slot.staging.buffer, off))
    }

    fn upload_egui_texture(
        &mut self,
        panel: PanelId,
        id: TextureId,
        delta: &egui::epaint::ImageDelta,
    ) -> Result<()> {
        let egui::ImageData::Color(img) = &delta.image;
        let [w, h] = img.size;
        let bytes: &[u8] = bytemuck::cast_slice(&img.pixels);
        let (buf, off) = self.staging_alloc(bytes)?;
        let cmd = self.slots[self.current].cmd;
        let need_new = delta.pos.is_none();
        if need_new {
            let image = Image::new(
                &self.gpu,
                w as u32,
                h as u32,
                &[vk::Format::R8G8B8A8_UNORM],
                usage_sampled(),
                1,
                "egui texture",
            )?;
            let set = self.tex_set(image.views[0])?;
            let Some(Some(p)) = self.panels.get_mut(panel) else {
                return Ok(());
            };
            if let Some(old) = p.textures.insert(id, Tex { image, set }) {
                let slot = &mut self.slots[self.current];
                slot.garbage.push(old.image);
                slot.garbage_sets.push(old.set);
            }
        }
        let Some(Some(p)) = self.panels.get(panel) else {
            return Ok(());
        };
        let Some(tex) = p.textures.get(&id) else {
            return Ok(());
        };
        let [x, y] = delta.pos.unwrap_or([0, 0]);
        let old_layout = if need_new {
            vk::ImageLayout::UNDEFINED
        } else {
            vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL
        };
        let gpu = &self.gpu;
        mem::barrier(
            gpu,
            cmd,
            tex.image.image,
            mem::range(0, 1),
            old_layout,
            vk::ImageLayout::TRANSFER_DST_OPTIMAL,
            mem::SHADER_READ,
            mem::TRANSFER_WRITE,
        );
        let region = vk::BufferImageCopy::default()
            .buffer_offset(off)
            .image_subresource(vk::ImageSubresourceLayers {
                aspect_mask: vk::ImageAspectFlags::COLOR,
                mip_level: 0,
                base_array_layer: 0,
                layer_count: 1,
            })
            .image_offset(vk::Offset3D {
                x: x as i32,
                y: y as i32,
                z: 0,
            })
            .image_extent(vk::Extent3D {
                width: w as u32,
                height: h as u32,
                depth: 1,
            });
        // SAFETY: recording; region lies inside the image.
        unsafe {
            gpu.device.cmd_copy_buffer_to_image(
                cmd,
                buf,
                tex.image.image,
                vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                &[region],
            );
        };
        mem::barrier(
            gpu,
            cmd,
            tex.image.image,
            mem::range(0, 1),
            vk::ImageLayout::TRANSFER_DST_OPTIMAL,
            vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL,
            mem::TRANSFER_WRITE,
            mem::SHADER_READ,
        );
        Ok(())
    }

    /// Paints egui output into a panel. `pixels_per_point` maps egui points
    /// to panel pixels.
    pub fn paint_panel(
        &mut self,
        panel: PanelId,
        primitives: &[ClippedPrimitive],
        delta: &TexturesDelta,
        pixels_per_point: f32,
    ) -> Result<()> {
        for (id, d) in &delta.set {
            self.upload_egui_texture(panel, *id, d)?;
        }
        let Some(Some(p)) = self.panels.get(panel) else {
            return Ok(());
        };
        let (pw, ph) = (p.image.extent.width, p.image.extent.height);
        let (img, view, mips) = (p.image.image, p.image.level0, p.image.mips);

        // Gather geometry into the slot's buffers.
        let mut draws: Vec<(vk::DescriptorSet, vk::Rect2D, u32, u32, i32)> = Vec::new();
        {
            let slot = &mut self.slots[self.current];
            let mut vbytes = Vec::new();
            let mut ibytes = Vec::new();
            let (mut vcount, mut icount) = (
                (slot.egui_vb_used / 20) as i32,
                (slot.egui_ib_used / 4) as u32,
            );
            for cp in primitives {
                let Primitive::Mesh(mesh) = &cp.primitive else {
                    continue;
                };
                if mesh.indices.is_empty() {
                    continue;
                }
                let set = match p.textures.get(&mesh.texture_id) {
                    Some(t) => t.set,
                    None => self.white.set,
                };
                let r = cp.clip_rect;
                let x0 = (r.min.x * pixels_per_point).round().clamp(0.0, pw as f32) as i32;
                let y0 = (r.min.y * pixels_per_point).round().clamp(0.0, ph as f32) as i32;
                let x1 = (r.max.x * pixels_per_point).round().clamp(0.0, pw as f32) as i32;
                let y1 = (r.max.y * pixels_per_point).round().clamp(0.0, ph as f32) as i32;
                if x1 <= x0 || y1 <= y0 {
                    continue;
                }
                let scissor = vk::Rect2D {
                    offset: vk::Offset2D { x: x0, y: y0 },
                    extent: vk::Extent2D {
                        width: (x1 - x0) as u32,
                        height: (y1 - y0) as u32,
                    },
                };
                vbytes.extend_from_slice(bytemuck::cast_slice(&mesh.vertices));
                ibytes.extend_from_slice(bytemuck::cast_slice(&mesh.indices));
                draws.push((set, scissor, icount, mesh.indices.len() as u32, vcount));
                vcount += mesh.vertices.len() as i32;
                icount += mesh.indices.len() as u32;
            }
            for (buf, used, bytes, usage, name) in [
                (
                    &mut slot.egui_vb,
                    &mut slot.egui_vb_used,
                    &vbytes,
                    vk::BufferUsageFlags::VERTEX_BUFFER,
                    "egui vertices",
                ),
                (
                    &mut slot.egui_ib,
                    &mut slot.egui_ib_used,
                    &ibytes,
                    vk::BufferUsageFlags::INDEX_BUFFER,
                    "egui indices",
                ),
            ] {
                let need = *used + bytes.len() as u64;
                if need > buf.size {
                    // Grow; geometry already written this frame is copied over.
                    let mut bigger = Buffer::new(
                        &self.gpu,
                        (need * 2).max(buf.size * 2),
                        usage,
                        MemoryLocation::CpuToGpu,
                        name,
                    )?;
                    let keep = *used as usize;
                    bigger.bytes()[..keep].copy_from_slice(&buf.bytes()[..keep]);
                    let old = std::mem::replace(buf, bigger);
                    slot.garbage_buffers.push(old);
                }
                buf.bytes()[*used as usize..need as usize].copy_from_slice(bytes);
                *used = need;
            }
        }
        let slot = &self.slots[self.current];
        let (cmd, gpu) = (slot.cmd, &self.gpu);
        let d = &gpu.device;
        mem::barrier(
            gpu,
            cmd,
            img,
            mem::range(0, 1),
            vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL,
            vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL,
            mem::SHADER_READ,
            mem::COLOR_WRITE,
        );
        let attachment = [vk::RenderingAttachmentInfo::default()
            .image_view(view)
            .image_layout(vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL)
            .load_op(vk::AttachmentLoadOp::CLEAR)
            .store_op(vk::AttachmentStoreOp::STORE)
            .clear_value(vk::ClearValue {
                color: vk::ClearColorValue { float32: [0.0; 4] },
            })];
        let area = vk::Rect2D {
            offset: vk::Offset2D::default(),
            extent: vk::Extent2D {
                width: pw,
                height: ph,
            },
        };
        // SAFETY: recording into the slot's command buffer with valid handles.
        unsafe {
            d.cmd_begin_rendering(
                cmd,
                &vk::RenderingInfo::default()
                    .render_area(area)
                    .layer_count(1)
                    .color_attachments(&attachment),
            );
            d.cmd_bind_pipeline(cmd, vk::PipelineBindPoint::GRAPHICS, self.egui_pipe);
            d.cmd_set_viewport(
                cmd,
                0,
                &[vk::Viewport {
                    x: 0.0,
                    y: 0.0,
                    width: pw as f32,
                    height: ph as f32,
                    min_depth: 0.0,
                    max_depth: 1.0,
                }],
            );
            let size = [pw as f32 / pixels_per_point, ph as f32 / pixels_per_point];
            d.cmd_push_constants(
                cmd,
                self.egui_pl,
                vk::ShaderStageFlags::VERTEX | vk::ShaderStageFlags::FRAGMENT,
                0,
                bytemuck::cast_slice(&size),
            );
            d.cmd_bind_vertex_buffers(cmd, 0, &[slot.egui_vb.buffer], &[0]);
            d.cmd_bind_index_buffer(cmd, slot.egui_ib.buffer, 0, vk::IndexType::UINT32);
            for (set, scissor, first, count, base) in draws {
                d.cmd_set_scissor(cmd, 0, &[scissor]);
                d.cmd_bind_descriptor_sets(
                    cmd,
                    vk::PipelineBindPoint::GRAPHICS,
                    self.egui_pl,
                    0,
                    &[set],
                    &[],
                );
                d.cmd_draw_indexed(cmd, count, 1, first, base, 0);
            }
            d.cmd_end_rendering(cmd);
        }
        generate_mips(gpu, cmd, img, pw, ph, mips);
        // Free textures egui no longer needs (after this frame).
        if let Some(Some(p)) = self.panels.get_mut(panel) {
            p.painted = true;
            p.version += 1;
            let slot = &mut self.slots[self.current];
            for id in &delta.free {
                if let Some(t) = p.textures.remove(id) {
                    slot.garbage.push(t.image);
                    slot.garbage_sets.push(t.set);
                }
            }
        }
        Ok(())
    }

    fn update_scene_set(&mut self) {
        if !self.slots[self.current].scene_set_dirty {
            return;
        }
        let dummy = self.dummy.views[0];
        let shared = self.slots[self.current]
            .shared_bound
            .filter(|_| self.slots[self.current].has_video)
            .and_then(|(buffer, _)| self.imports.nv12_views(buffer));
        let views: Vec<vk::ImageView> = match (shared, self.slot_video(self.current)) {
            (Some([y, uv]), _) => vec![y, uv, dummy],
            (None, Some(v)) => {
                let mut vs: Vec<vk::ImageView> = v.images.iter().map(|i| i.views[0]).collect();
                vs.resize(3, dummy);
                vs
            }
            (None, None) => vec![dummy; 3],
        };
        let slot = &mut self.slots[self.current];
        let ubo = [vk::DescriptorBufferInfo::default()
            .buffer(slot.uniform.buffer)
            .range(vk::WHOLE_SIZE)];
        let samplers = [vk::DescriptorImageInfo::default().sampler(self.sampler)];
        let images: Vec<[vk::DescriptorImageInfo; 1]> = views
            .iter()
            .map(|&v| {
                [vk::DescriptorImageInfo::default()
                    .image_view(v)
                    .image_layout(vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL)]
            })
            .collect();
        let mut writes = vec![
            vk::WriteDescriptorSet::default()
                .dst_set(slot.scene_set)
                .dst_binding(0)
                .descriptor_type(vk::DescriptorType::UNIFORM_BUFFER)
                .buffer_info(&ubo),
            vk::WriteDescriptorSet::default()
                .dst_set(slot.scene_set)
                .dst_binding(1)
                .descriptor_type(vk::DescriptorType::SAMPLER)
                .image_info(&samplers),
        ];
        for (i, info) in images.iter().enumerate() {
            writes.push(
                vk::WriteDescriptorSet::default()
                    .dst_set(slot.scene_set)
                    .dst_binding(2 + i as u32)
                    .descriptor_type(vk::DescriptorType::SAMPLED_IMAGE)
                    .image_info(info),
            );
        }
        // SAFETY: the set is not in use by any pending submission (the slot's
        // fence was waited on in begin_frame).
        unsafe { self.gpu.device.update_descriptor_sets(&writes, &[]) };
        slot.scene_set_dirty = false;
    }

    /// How much the next [`Renderer::draw`]s give up away from the gaze;
    /// nothing where the GPU can't shade at lower rates.
    pub fn set_foveation(&mut self, level: Foveation) {
        if level != self.foveation {
            log::info!("eye passes foveated: {level:?}");
            self.foveation = level;
        }
    }

    /// The direction the eyes look, in the play space, for the next
    /// [`Renderer::draw`]s: foveated eye passes are sharp around it. `None`
    /// (no eye tracking) looks straight ahead through each lens.
    pub fn set_gaze(&mut self, gaze: Option<Vec3>) {
        self.gaze = gaze;
    }

    /// Lights the background round a flat screen with the picture's edge
    /// colours, or not with `None`. It shows only on an opaque background,
    /// not in a cinema or with a chroma key.
    pub fn set_screen_glow(&mut self, glow: Option<crate::ScreenGlow>) {
        self.glow.set(glow);
    }

    /// Records both eye passes.
    pub fn draw(
        &mut self,
        targets: &[EyeTarget; 2],
        eyes: &[EyeView; 2],
        video: &VideoParams,
        quads: &[QuadDraw],
    ) -> Result<()> {
        self.stamp(Mark::Eyes);
        let r = self.draw_eyes(targets, eyes, video, quads);
        self.stamp(Mark::EyesDone);
        r
    }

    fn draw_eyes(
        &mut self,
        targets: &[EyeTarget; 2],
        eyes: &[EyeView; 2],
        video: &VideoParams,
        quads: &[QuadDraw],
    ) -> Result<()> {
        let mut video = *video;
        video.cinema = video.cinema.filter(|cinema| {
            cinema.room.is_available()
                && video.format.projection == fp_core::format::Projection::Flat
        });
        // Until the room is loaded, the picture has its size and place in it
        // with nothing drawn round it.
        if let Some(cinema) = video.cinema
            && self.load_cinema(cinema.room)
            && let Some(room) = &mut self.cinemas[cinema.room.slot()]
            && let Err(e) =
                room.prepare_depth(&self.gpu, targets, &mut self.slots[self.current].garbage)
        {
            log::warn!(
                "{:?} cinema unavailable, showing the flat video: {e}",
                cinema.room
            );
            cinema.room.fail();
            video.cinema = None;
        }
        self.update_scene_set();
        let vp = eyes.map(|e| e.proj * e.view);
        let inv = vp.map(|m| m.inverse());
        if let Some(cinema) = video.cinema
            && let Some(room) = &mut self.cinemas[cinema.room.slot()]
        {
            let to_room = cinema.pose.inverse();
            let eye = |e: &EyeView| (to_room * e.view.inverse()).w_axis.truncate();
            let between = (eye(&eyes[0]) + eye(&eyes[1])) * 0.5;
            room.place(self.current, vp.map(|m| m * cinema.pose), between);
        }
        let slot_idx = self.current;
        let s = &self.slots[slot_idx];
        let frame_desc = match (s.tp10_bound, s.shared_bound) {
            (Some((_, desc)), _) | (None, Some((_, desc))) if s.has_video => Some(desc),
            _ => self.slot_video(slot_idx).map(|v| v.desc),
        };
        if let Some(cinema) = video.cinema
            && self.cinemas[cinema.room.slot()].is_some()
            && s.tp10_bound.is_some()
            && let Some(t) = &mut self.tp10_eyes
            && let Err(e) = t.prepare_cinema(&self.gpu, self.color_format, self.video_rate)
        {
            log::warn!("cinema unavailable for this device, showing the flat video: {e}");
            cinema.room.fail();
            video.cinema = None;
        }
        let mut p = params::build(&video, frame_desc.as_ref(), inv);
        if video.cinema.is_some() {
            let depth = eyes.map(|e| {
                crate::cinema::screen_depth(
                    e.view * video.screen_pose,
                    e.proj,
                    p.screen.xy(),
                    p.screen.z,
                )
            });
            (p.fill.z, p.fill.w) = (depth[0], depth[1]);
        }
        p.hdr = params::hdr_levels(self.hdr_peak.peak_nits());
        // Screen glow, round a flat screen on an opaque background: not in a
        // cinema, nor over passthrough or a chroma key.
        let now = std::time::Instant::now();
        let glow_shows = self.glow.wanted()
            && video.cinema.is_none()
            && p.mode[0] == 0
            && p.bg_color.w >= 1.0
            && p.key2.y == 0.0
            && p.key2.z == 0.0
            && self.slots[slot_idx].has_video
            && frame_desc.is_some();
        let (glow, measure_glow) = if glow_shows
            && self.glow.prepare(
                &self.gpu,
                self.color_format,
                self.video_rate,
                (self.scene_pl, self.tp10_eyes.as_ref().map(|t| t.layout)),
                SLOTS,
            ) {
            (self.glow.apply(&mut p, now), self.glow.due(now))
        } else {
            self.glow.reset();
            (false, false)
        };
        let glow_eyes = glow.then(|| (self.glow.eye_pipeline(false), self.glow.eye_pipeline(true)));
        let want_focus = video.settings.comfort_focus
            && video.format.can_comfort_focus()
            && self.slots[slot_idx].has_video;
        // Where comfort focus measures this frame, if it does.
        let mut measure = None;
        if want_focus {
            let now = std::time::Instant::now();
            // The head's turn: between the two eyes' views.
            let to_world = eyes.map(|e| e.view.inverse());
            let head = crate::focus::head_turn(
                (to_world[0].z_axis + to_world[1].z_axis).truncate(),
                (to_world[0].y_axis + to_world[1].y_axis).truncate(),
            );
            // Where the eyes look whenever the headset tracks them, foveated
            // rendering on or not.
            let aim = self.focus_aim.aim(now, self.gaze, head);
            // Landed somewhere else: what is read there is acted on at once.
            if self.focus_aim.take_landed() {
                self.comfort_focus.landed();
            }
            // Each eye's offset from the point between them, and the head's
            // up, in the room frame, to draw each eye from where it is.
            let room = |v: Vec3| p.anchor_inv.transform_vector3(v);
            let at = to_world.map(|m| m.w_axis.truncate());
            let mid = (at[0] + at[1]) * 0.5;
            let offsets = at.map(|e| room(e - mid));
            let (lever, line) = crate::focus::redraw_lever(&p, aim, offsets);
            self.comfort_focus
                .follows_eyes(self.focus_aim.from() != crate::focus::AimFrom::Head);
            let focus = self.comfort_focus.step(now, lever);
            let up = room((to_world[0].y_axis + to_world[1].y_axis).truncate());
            if let Some([along, tilt]) = crate::focus::aim_angles(&p, aim)
                && self.focus.is_some()
                // Not while the aim sweeps across a jump of the eyes.
                && self.focus_aim.settled()
                // Something new to read: another frame, the eyes elsewhere, or
                // a still picture not read for a while.
                && self.focus_measured.is_none_or(|(t, at, frame)| {
                    frame != self.shown_frame
                        || at.angle_between(aim) > REMEASURE_RAD
                        || now.saturating_duration_since(t) >= MEASURE_EVERY
                })
            {
                p.focus.x = along;
                p.focus.y = tilt;
                self.focus_measured = Some((now, aim, self.shown_frame));
                measure = Some(along);
            }
            self.focus_log
                .frame(now, self.focus_aim.from(), measure.is_some());
            self.focus_drawn[slot_idx] = Some(measure.is_some());
            // The measurement reads the video as filmed, without it.
            params::set_comfort_focus(
                &mut p,
                focus.relief,
                focus.distance,
                offsets,
                up,
                (line, focus.side_distance),
            );
        } else {
            self.focus_drawn[slot_idx] = None;
            self.comfort_focus.reset();
            self.focus_aim = AimFilter::default();
            if let Some(f) = self.focus.as_mut() {
                f.discard();
            }
        }
        let tp10 = match (self.slots[slot_idx].tp10_bound, self.tp10_eyes.as_mut()) {
            (Some((buffer, desc)), Some(t)) => self.imports.tp10_picture(buffer).map(|pic| {
                params::sample_tp10(&mut p, desc.height, pic.luma_bands, pic.chroma_bands);
                t.write(slot_idx, &p, pic);
            }),
            _ => None,
        }
        .is_some();
        // Quad vertices: two triangles per quad.
        let mut verts: Vec<QuadVertex> = Vec::with_capacity(quads.len() * 6);
        let mut sets = Vec::with_capacity(quads.len());
        for q in quads {
            let set = match q.texture {
                QuadTexture::White => self.white.set,
                QuadTexture::Panel(id) => match self.panels.get(id).and_then(Option::as_ref) {
                    Some(p) => p.set,
                    None => continue,
                },
            };
            let [a, b] = q.uv;
            let uvs = [
                Vec2::new(a.x, a.y),
                Vec2::new(b.x, a.y),
                Vec2::new(b.x, b.y),
                Vec2::new(a.x, b.y),
            ];
            for i in [0usize, 1, 2, 0, 2, 3] {
                verts.push(QuadVertex {
                    pos: q.corners[i].to_array(),
                    uv: uvs[i].to_array(),
                    color: q.color,
                });
            }
            sets.push((set, q.eye));
        }
        {
            let slot = &mut self.slots[slot_idx];
            slot.uniform.bytes()[..size_of::<SceneParams>()]
                .copy_from_slice(bytemuck::bytes_of(&p));
            let bytes: &[u8] = bytemuck::cast_slice(&verts);
            if bytes.len() as u64 > slot.quad_vb.size {
                let bigger = Buffer::new(
                    &self.gpu,
                    bytes.len() as u64 * 2,
                    vk::BufferUsageFlags::VERTEX_BUFFER,
                    MemoryLocation::CpuToGpu,
                    "quads",
                )?;
                let old = std::mem::replace(&mut slot.quad_vb, bigger);
                slot.garbage_buffers.push(old);
            }
            slot.quad_vb.bytes()[..bytes.len()].copy_from_slice(bytes);
        }
        let slot = &self.slots[slot_idx];
        let (gpu, cmd) = (&self.gpu, slot.cmd);
        let d = &gpu.device;
        let bg = video.background;
        let room = video.cinema.and_then(|cinema| {
            self.cinemas[cinema.room.slot()]
                .as_ref()
                .map(|room| (cinema, room))
        });
        for (eye, t) in targets.iter().enumerate() {
            mem::barrier(
                gpu,
                cmd,
                t.image,
                mem::range(0, 1),
                vk::ImageLayout::UNDEFINED,
                vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL,
                mem::TOP,
                mem::COLOR_WRITE,
            );
            let attachment = [vk::RenderingAttachmentInfo::default()
                .image_view(t.view)
                .image_layout(vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL)
                .load_op(vk::AttachmentLoadOp::CLEAR)
                .store_op(vk::AttachmentStoreOp::STORE)
                .clear_value(vk::ClearValue {
                    color: vk::ClearColorValue { float32: bg },
                })];
            let area = vk::Rect2D {
                offset: vk::Offset2D::default(),
                extent: t.extent,
            };
            let level = self.foveation;
            let mut rate = match self.rates.as_mut().filter(|_| level != Foveation::Off) {
                Some(m) => Some(
                    m.prepare(
                        gpu,
                        cmd,
                        slot_idx,
                        eye,
                        t.extent,
                        eyes[eye].proj.inverse(),
                        self.gaze
                            .map_or(Vec3::NEG_Z, |g| eyes[eye].view.transform_vector3(g)),
                        level,
                    )?,
                ),
                None => None,
            };
            let mut rendering = vk::RenderingInfo::default()
                .render_area(area)
                .layer_count(1)
                .color_attachments(&attachment);
            let depth = room.map(|(_, room)| room.depth_attachment(gpu, cmd));
            if let Some(depth) = depth.as_ref() {
                rendering = rendering.depth_attachment(depth);
            }
            if let Some(rate) = rate.as_mut() {
                rendering = rendering.push_next(rate);
            }
            // SAFETY: recording with valid pipelines, sets and buffers.
            unsafe {
                d.cmd_begin_rendering(cmd, &rendering);
                d.cmd_set_viewport(
                    cmd,
                    0,
                    &[vk::Viewport {
                        x: 0.0,
                        y: 0.0,
                        width: t.extent.width as f32,
                        height: t.extent.height as f32,
                        min_depth: 0.0,
                        max_depth: 1.0,
                    }],
                );
                d.cmd_set_scissor(cmd, 0, &[area]);
                if let Some((cinema, room)) = room {
                    room.draw(gpu, cmd, vp[eye], cinema);
                }
                let screen_area = if room.is_some() {
                    if frame_desc.is_some() {
                        crate::cinema::screen_area(
                            vp[eye] * video.screen_pose,
                            p.screen.xy(),
                            p.screen.z,
                            area,
                        )
                    } else {
                        None
                    }
                } else {
                    Some(area)
                };
                if let Some(screen_area) = screen_area {
                    d.cmd_set_scissor(cmd, 0, &[screen_area]);
                    let scene_pl = match self.tp10_eyes.as_ref().filter(|_| tp10) {
                        Some(t) => {
                            if room.is_some() {
                                t.bind_cinema(gpu, cmd, slot_idx);
                            } else if let Some((_, glowing)) = glow_eyes {
                                t.bind_with(gpu, cmd, slot_idx, glowing);
                            } else {
                                t.bind(gpu, cmd, slot_idx, want_focus);
                            }
                            t.layout
                        }
                        None => {
                            // Comfort focus is for 180° and fisheye video, never
                            // the cinema's flat screen.
                            let pipe = match room {
                                Some((_, room)) => room.scene_pipe,
                                None if want_focus => self.comfort_pipe,
                                None => glow_eyes.map_or(self.scene_pipe, |(glowing, _)| glowing),
                            };
                            d.cmd_bind_pipeline(cmd, vk::PipelineBindPoint::GRAPHICS, pipe);
                            d.cmd_bind_descriptor_sets(
                                cmd,
                                vk::PipelineBindPoint::GRAPHICS,
                                self.scene_pl,
                                0,
                                &[slot.scene_set],
                                &[],
                            );
                            self.scene_pl
                        }
                    };
                    d.cmd_push_constants(
                        cmd,
                        scene_pl,
                        vk::ShaderStageFlags::VERTEX | vk::ShaderStageFlags::FRAGMENT,
                        0,
                        &(eye as u32).to_le_bytes(),
                    );
                    d.cmd_draw(cmd, 3, 1, 0, 0);
                }
                if !verts.is_empty() {
                    d.cmd_set_scissor(cmd, 0, &[area]);
                    let pipe = room.map_or(self.quad_pipe, |(_, room)| room.quad_pipe);
                    d.cmd_bind_pipeline(cmd, vk::PipelineBindPoint::GRAPHICS, pipe);
                    d.cmd_push_constants(
                        cmd,
                        self.quad_pl,
                        vk::ShaderStageFlags::VERTEX | vk::ShaderStageFlags::FRAGMENT,
                        0,
                        bytemuck::bytes_of(&vp[eye]),
                    );
                    d.cmd_bind_vertex_buffers(cmd, 0, &[slot.quad_vb.buffer], &[0]);
                    for (i, &(set, only)) in sets.iter().enumerate() {
                        if only.is_some_and(|e| e != eye) {
                            continue;
                        }
                        d.cmd_bind_descriptor_sets(
                            cmd,
                            vk::PipelineBindPoint::GRAPHICS,
                            self.quad_pl,
                            0,
                            &[set],
                            &[],
                        );
                        d.cmd_draw(cmd, 6, 1, (i * 6) as u32, 0);
                    }
                }
                d.cmd_end_rendering(cmd);
            }
            if t.final_layout != vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL {
                let dst = if t.final_layout == vk::ImageLayout::TRANSFER_SRC_OPTIMAL {
                    mem::TRANSFER_READ
                } else {
                    mem::SHADER_READ
                };
                mem::barrier(
                    gpu,
                    cmd,
                    t.image,
                    mem::range(0, 1),
                    vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL,
                    t.final_layout,
                    mem::COLOR_WRITE,
                    dst,
                );
            }
        }
        if let Some(along) = measure {
            self.record_focus(slot_idx, tp10, along);
        }
        if measure_glow {
            self.record_glow(slot_idx, tp10, now);
        }
        Ok(())
    }

    /// Measures the picture's edge colours for screen glow with the eye
    /// passes' own layout and bindings, as [`Self::record_focus`] does.
    fn record_glow(&mut self, slot_idx: usize, tp10: bool, now: std::time::Instant) {
        let slot = &self.slots[slot_idx];
        let (cmd, scene_set) = (slot.cmd, slot.scene_set);
        let (gpu, scene_pl) = (&self.gpu, self.scene_pl);
        let tp10_eyes = self.tp10_eyes.as_ref().filter(|_| tp10);
        self.glow
            .record(gpu, cmd, slot_idx, tp10, now, |pipe| match tp10_eyes {
                Some(t) => t.bind_with(gpu, cmd, slot_idx, pipe),
                // SAFETY: recording; the pipeline was made with the eye
                // passes' layout and the slot's scene set is written for
                // this frame.
                None => unsafe {
                    let d = &gpu.device;
                    d.cmd_bind_pipeline(cmd, vk::PipelineBindPoint::GRAPHICS, pipe);
                    d.cmd_bind_descriptor_sets(
                        cmd,
                        vk::PipelineBindPoint::GRAPHICS,
                        scene_pl,
                        0,
                        &[scene_set],
                        &[],
                    );
                },
            });
    }

    /// Draws comfort focus's samples round the aim, at lattice angle `along`
    /// (`fs_focus`, which takes it from the scene's parameters), with the eye
    /// passes' own pipeline layout and bindings, so they read the frame as
    /// the eye passes just did, then matches them.
    fn record_focus(&mut self, slot_idx: usize, tp10: bool, along: f32) {
        let cmd = self.slots[slot_idx].cmd;
        let Some(focus) = self.focus.as_mut() else {
            return;
        };
        let Some((view, extent)) = focus.begin_samples(&self.gpu, cmd, slot_idx) else {
            return;
        };
        let d = &self.gpu.device;
        let attachment = [vk::RenderingAttachmentInfo::default()
            .image_view(view)
            .image_layout(vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL)
            .load_op(vk::AttachmentLoadOp::DONT_CARE)
            .store_op(vk::AttachmentStoreOp::STORE)];
        let area = vk::Rect2D {
            offset: vk::Offset2D::default(),
            extent,
        };
        // SAFETY: recording into the slot's command buffer with valid
        // pipelines and descriptors; the frame's planes are still readable as
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
                    x: 0.0,
                    y: 0.0,
                    width: extent.width as f32,
                    height: extent.height as f32,
                    min_depth: 0.0,
                    max_depth: 1.0,
                }],
            );
            d.cmd_set_scissor(cmd, 0, &[area]);
            match self.tp10_eyes.as_ref().filter(|_| tp10) {
                Some(t) => t.bind_focus(&self.gpu, cmd, slot_idx),
                None => {
                    d.cmd_bind_pipeline(cmd, vk::PipelineBindPoint::GRAPHICS, self.focus_pipe);
                    d.cmd_bind_descriptor_sets(
                        cmd,
                        vk::PipelineBindPoint::GRAPHICS,
                        self.scene_pl,
                        0,
                        &[self.slots[slot_idx].scene_set],
                        &[],
                    );
                }
            }
            d.cmd_draw(cmd, 3, 1, 0, 0);
            d.cmd_end_rendering(cmd);
        }
        focus.record(&self.gpu, cmd, slot_idx, along);
    }

    /// Ends recording and submits the frame.
    pub fn end_frame(&mut self) -> Result<()> {
        let slot = &self.slots[self.current];
        if let Some((buffer, _)) = slot.shared_bound {
            // Acquired in set_video; released to the decoder once read.
            self.imports.release_nv12(&self.gpu, slot.cmd, buffer);
        }
        if let Some((buffer, _)) = slot.tp10_bound {
            self.imports.release_tp10(&self.gpu, slot.cmd, buffer);
        }
        self.stamp(Mark::End);
        let slot = &self.slots[self.current];
        let d = &self.gpu.device;
        // SAFETY: the command buffer is recording; fence is unsignalled after reset.
        unsafe {
            d.end_command_buffer(slot.cmd).ctx("end frame")?;
            d.reset_fences(&[slot.fence]).ctx("reset fence")?;
            let cmds = [slot.cmd];
            d.queue_submit(
                self.gpu.queue,
                &[vk::SubmitInfo::default().command_buffers(&cmds)],
                slot.fence,
            )
            .ctx("submit frame")?;
        }
        self.recording = false;
        Ok(())
    }

    /// How many times a panel has been painted (0: never, or no such panel).
    pub fn panel_version(&self, panel: PanelId) -> u64 {
        match self.panels.get(panel) {
            Some(Some(p)) if p.painted => p.version,
            _ => 0,
        }
    }

    /// Copies a panel's picture into `dst`, an image of the same size and a
    /// compatible RGBA8 format (an OpenXR quad layer's swapchain image, left
    /// in COLOR_ATTACHMENT_OPTIMAL as OpenXR expects). Recorded into this
    /// frame, after the panel's paint; call between `begin_frame` and
    /// `end_frame`.
    pub fn copy_panel_to(&mut self, panel: PanelId, dst: vk::Image) -> Result<()> {
        let Some(Some(p)) = self.panels.get(panel) else {
            return Err(crate::Error::Unsupported("no such panel".into()));
        };
        if !self.recording {
            return Err(crate::Error::Unsupported("copy outside a frame".into()));
        }
        let (src, extent) = (p.image.image, p.image.extent);
        let cmd = self.slots[self.current].cmd;
        let gpu = &self.gpu;
        // The whole image is overwritten: its old contents can go.
        mem::barrier(
            gpu,
            cmd,
            dst,
            mem::range(0, 1),
            vk::ImageLayout::UNDEFINED,
            vk::ImageLayout::TRANSFER_DST_OPTIMAL,
            mem::TOP,
            mem::TRANSFER_WRITE,
        );
        mem::barrier(
            gpu,
            cmd,
            src,
            mem::range(0, 1),
            vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL,
            vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
            mem::SHADER_READ,
            mem::TRANSFER_READ,
        );
        let layers = vk::ImageSubresourceLayers {
            aspect_mask: vk::ImageAspectFlags::COLOR,
            mip_level: 0,
            base_array_layer: 0,
            layer_count: 1,
        };
        let region = vk::ImageCopy::default()
            .src_subresource(layers)
            .dst_subresource(layers)
            .extent(vk::Extent3D {
                width: extent.width,
                height: extent.height,
                depth: 1,
            });
        // SAFETY: recording; both images are extent-sized and in the
        // transfer layouts set above.
        unsafe {
            gpu.device.cmd_copy_image(
                cmd,
                src,
                vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
                dst,
                vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                &[region],
            );
        };
        mem::barrier(
            gpu,
            cmd,
            src,
            mem::range(0, 1),
            vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
            vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL,
            mem::TRANSFER_READ,
            mem::SHADER_READ,
        );
        mem::barrier(
            gpu,
            cmd,
            dst,
            mem::range(0, 1),
            vk::ImageLayout::TRANSFER_DST_OPTIMAL,
            vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL,
            mem::TRANSFER_WRITE,
            mem::COLOR_WRITE,
        );
        Ok(())
    }

    /// Fills `dst` (`width`×`height`, RGBA8, e.g. an OpenXR swapchain image)
    /// with `rgba` and leaves it in COLOR_ATTACHMENT_OPTIMAL. Waits for the
    /// GPU; for small, static pictures.
    pub fn upload_rgba(&self, dst: vk::Image, width: u32, height: u32, rgba: &[u8]) -> Result<()> {
        let mut staging = Buffer::new(
            &self.gpu,
            rgba.len() as u64,
            vk::BufferUsageFlags::TRANSFER_SRC,
            MemoryLocation::CpuToGpu,
            "upload",
        )?;
        staging.bytes()[..rgba.len()].copy_from_slice(rgba);
        let buf = staging.buffer;
        let r = self.one_shot(|gpu, cmd| {
            mem::barrier(
                gpu,
                cmd,
                dst,
                mem::range(0, 1),
                vk::ImageLayout::UNDEFINED,
                vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                mem::TOP,
                mem::TRANSFER_WRITE,
            );
            let region = vk::BufferImageCopy::default()
                .image_subresource(vk::ImageSubresourceLayers {
                    aspect_mask: vk::ImageAspectFlags::COLOR,
                    mip_level: 0,
                    base_array_layer: 0,
                    layer_count: 1,
                })
                .image_extent(vk::Extent3D {
                    width,
                    height,
                    depth: 1,
                });
            // SAFETY: recording; the buffer holds width*height texels.
            unsafe {
                gpu.device.cmd_copy_buffer_to_image(
                    cmd,
                    buf,
                    dst,
                    vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                    &[region],
                );
            };
            mem::barrier(
                gpu,
                cmd,
                dst,
                mem::range(0, 1),
                vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL,
                mem::TRANSFER_WRITE,
                mem::COLOR_WRITE,
            );
        });
        staging.destroy(&self.gpu);
        r
    }

    /// Blocks until all submitted frames are done.
    pub fn wait_idle(&self) {
        self.gpu.wait_idle();
    }

    /// Reads a painted panel back as `(width, height, RGBA8)`: sRGB-encoded,
    /// premultiplied alpha, as egui painted it. For screenshots of the UI
    /// (the desktop preview); waits for the GPU.
    pub fn read_panel(&self, panel: PanelId) -> Result<(u32, u32, Vec<u8>)> {
        let Some(Some(p)) = self.panels.get(panel) else {
            return Err(crate::Error::Unsupported("no such panel".into()));
        };
        let (image, extent) = (p.image.image, p.image.extent);
        let mut readback = Buffer::new(
            &self.gpu,
            extent.width as u64 * extent.height as u64 * 4,
            vk::BufferUsageFlags::TRANSFER_DST,
            MemoryLocation::GpuToCpu,
            "panel readback",
        )?;
        let buffer = readback.buffer;
        self.wait_idle();
        self.one_shot(|gpu, cmd| {
            mem::barrier(
                gpu,
                cmd,
                image,
                mem::range(0, 1),
                vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL,
                vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
                mem::SHADER_READ,
                mem::TRANSFER_READ,
            );
            let region = vk::BufferImageCopy::default()
                .image_subresource(vk::ImageSubresourceLayers {
                    aspect_mask: vk::ImageAspectFlags::COLOR,
                    mip_level: 0,
                    base_array_layer: 0,
                    layer_count: 1,
                })
                .image_extent(vk::Extent3D {
                    width: extent.width,
                    height: extent.height,
                    depth: 1,
                });
            // SAFETY: level 0 is in TRANSFER_SRC; the buffer holds it.
            unsafe {
                gpu.device.cmd_copy_image_to_buffer(
                    cmd,
                    image,
                    vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
                    buffer,
                    &[region],
                );
            };
            mem::barrier(
                gpu,
                cmd,
                image,
                mem::range(0, 1),
                vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
                vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL,
                mem::TRANSFER_READ,
                mem::SHADER_READ,
            );
        })?;
        let n = (extent.width * extent.height * 4) as usize;
        let px = readback.bytes()[..n].to_vec();
        readback.destroy(&self.gpu);
        Ok((extent.width, extent.height, px))
    }
}

/// The pipeline that samples 10-bit frames in the decoder's buffers, where
/// the GPU converts Q10C (`has_q10c`) and reads those buffers in place.
/// `FRAMEPLAYERX_TP10_EYES=0` converts them as before, for comparisons.
fn tp10_eyes(
    gpu: &Gpu,
    color_format: vk::Format,
    sampler: vk::Sampler,
    has_q10c: bool,
    shading_rate: ShadingRate,
) -> Option<Tp10Eyes> {
    let allowed = std::env::var("FRAMEPLAYERX_TP10_EYES").map_or(true, |v| v.trim() != "0");
    if !has_q10c || gpu.memory_fd.is_none() || !allowed {
        return None;
    }
    match Tp10Eyes::new(gpu, color_format, sampler, SLOTS, shading_rate) {
        Ok(t) => {
            log::info!("10-bit HEVC read in place is sampled by the eye passes, not converted");
            Some(t)
        }
        Err(e) => {
            log::warn!("10-bit HEVC read in place is converted: {e}");
            None
        }
    }
}

fn usage_sampled() -> vk::ImageUsageFlags {
    vk::ImageUsageFlags::SAMPLED | vk::ImageUsageFlags::TRANSFER_DST
}

fn quad_pipeline(
    gpu: &Gpu,
    layout: vk::PipelineLayout,
    color_format: vk::Format,
    depth: Depth,
    shading_rate: ShadingRate,
) -> Result<vk::Pipeline> {
    let attributes = [
        vk::VertexInputAttributeDescription {
            location: 0,
            binding: 0,
            format: vk::Format::R32G32B32_SFLOAT,
            offset: std::mem::offset_of!(QuadVertex, pos) as u32,
        },
        vk::VertexInputAttributeDescription {
            location: 1,
            binding: 0,
            format: vk::Format::R32G32_SFLOAT,
            offset: std::mem::offset_of!(QuadVertex, uv) as u32,
        },
        vk::VertexInputAttributeDescription {
            location: 2,
            binding: 0,
            format: vk::Format::R32G32B32A32_SFLOAT,
            offset: std::mem::offset_of!(QuadVertex, color) as u32,
        },
    ];
    pipeline::create(
        gpu,
        &PipelineDesc {
            spv: crate::QUAD_SPV,
            fragment: c"fs_main",
            layout,
            color_format,
            depth,
            vertex_stride: size_of::<QuadVertex>() as u32,
            instance_stride: 0,
            attributes: &attributes,
            blend: true,
            descriptor_buffer: false,
            shading_rate,
        },
    )
}

/// Blits level 0 down the mip chain and leaves every level shader-readable.
fn generate_mips(gpu: &Gpu, cmd: vk::CommandBuffer, image: vk::Image, w: u32, h: u32, mips: u32) {
    let d = &gpu.device;
    mem::barrier(
        gpu,
        cmd,
        image,
        mem::range(0, 1),
        vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL,
        vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
        mem::COLOR_WRITE,
        mem::TRANSFER_READ,
    );
    let (mut mw, mut mh) = (w as i32, h as i32);
    for level in 1..mips {
        let (nw, nh) = ((mw / 2).max(1), (mh / 2).max(1));
        mem::barrier(
            gpu,
            cmd,
            image,
            mem::range(level, 1),
            vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL,
            vk::ImageLayout::TRANSFER_DST_OPTIMAL,
            mem::SHADER_READ,
            mem::TRANSFER_WRITE,
        );
        let sub = |l| vk::ImageSubresourceLayers {
            aspect_mask: vk::ImageAspectFlags::COLOR,
            mip_level: l,
            base_array_layer: 0,
            layer_count: 1,
        };
        let blit = vk::ImageBlit {
            src_subresource: sub(level - 1),
            src_offsets: [vk::Offset3D::default(), vk::Offset3D { x: mw, y: mh, z: 1 }],
            dst_subresource: sub(level),
            dst_offsets: [vk::Offset3D::default(), vk::Offset3D { x: nw, y: nh, z: 1 }],
        };
        // SAFETY: recording; levels are in the right layouts.
        unsafe {
            d.cmd_blit_image(
                cmd,
                image,
                vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
                image,
                vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                &[blit],
                vk::Filter::LINEAR,
            );
        };
        mem::barrier(
            gpu,
            cmd,
            image,
            mem::range(level, 1),
            vk::ImageLayout::TRANSFER_DST_OPTIMAL,
            vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
            mem::TRANSFER_WRITE,
            mem::TRANSFER_READ,
        );
        mw = nw;
        mh = nh;
    }
    mem::barrier(
        gpu,
        cmd,
        image,
        mem::range(0, mips),
        vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
        vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL,
        mem::TRANSFER_READ,
        mem::SHADER_READ,
    );
}

impl Drop for Renderer {
    fn drop(&mut self) {
        self.gpu.wait_idle();
        let gpu = self.gpu.clone();
        let d = &gpu.device;
        // SAFETY: the device is idle; everything below was created by us.
        unsafe {
            for v in &mut self.videos {
                if let Some(mut v) = v.take() {
                    v.destroy(&gpu);
                }
            }
            if let Some(mut c) = self.q10c.take() {
                c.destroy(&gpu);
            }
            if let Some(mut p) = self.peak.take() {
                p.destroy(&gpu);
            }
            if let Some(mut f) = self.focus.take() {
                f.destroy(&gpu);
            }
            self.glow.destroy(&gpu);
            if let Some(mut t) = self.timer.take() {
                t.destroy(&gpu);
            }
            if let Some(mut t) = self.tp10_eyes.take() {
                t.destroy(&gpu);
            }
            for room in &mut self.cinemas {
                if let Some(room) = room.take() {
                    room.destroy(&gpu);
                }
            }
            if let Some(mut m) = self.rates.take() {
                m.destroy(&gpu);
            }
            self.imports.destroy(&gpu);
            for mut s in self.slots.drain(..) {
                for mut i in s.garbage.drain(..) {
                    i.destroy(&gpu);
                }
                for mut i in s.garbage_imports.drain(..) {
                    i.destroy(&gpu);
                }
                for mut b in s.garbage_buffers.drain(..) {
                    b.destroy(&gpu);
                }
                for room in s.garbage_cinemas.drain(..) {
                    room.destroy(&gpu);
                }
                for mut b in [s.uniform, s.quad_vb, s.egui_vb, s.egui_ib, s.staging] {
                    b.destroy(&gpu);
                }
                d.destroy_fence(s.fence, None);
            }
            for p in self.panels.drain(..).flatten() {
                let mut img = p.image;
                img.destroy(&gpu);
                for (_, mut t) in p.textures {
                    t.image.destroy(&gpu);
                }
            }
            self.white.image.destroy(&gpu);
            self.dummy.destroy(&gpu);
            for p in [
                self.scene_pipe,
                self.comfort_pipe,
                self.focus_pipe,
                self.quad_pipe,
                self.egui_pipe,
            ] {
                d.destroy_pipeline(p, None);
            }
            for l in [self.scene_pl, self.quad_pl, self.egui_pl] {
                d.destroy_pipeline_layout(l, None);
            }
            d.destroy_descriptor_set_layout(self.scene_layout, None);
            d.destroy_descriptor_set_layout(self.tex_layout, None);
            d.destroy_sampler(self.sampler, None);
            d.destroy_descriptor_pool(self.dpool, None);
            d.destroy_command_pool(self.pool, None);
        }
    }
}
