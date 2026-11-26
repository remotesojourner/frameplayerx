//! GPU resources for the textured cinema rooms.

use crate::cinema_mesh::{Placement, RoomData, Texture, Vertex};
use crate::gpu::Gpu;
use crate::mem::{self, Buffer, Image};
use crate::pipeline::{self, Depth, PipelineDesc, ShadingRate};
use crate::{CinemaParams, EyeTarget, Renderer, Result, VkContext};
use ash::vk;
use bytemuck::{Pod, Zeroable};
use glam::{Mat4, Quat, Vec2, Vec3, Vec4};
use gpu_allocator::MemoryLocation;

pub(crate) const DEPTH_FORMAT: vk::Format = vk::Format::D16_UNORM;
const ATLAS_FORMAT: vk::Format = vk::Format::R8G8B8A8_SRGB;
// Five levels cover the reduction from photograph texels to eye pixels.
const ATLAS_MIPS: u32 = 5;
// Patterns about mid-grey, as stored.
const DETAIL_FORMAT: vk::Format = vk::Format::R8G8B8A8_UNORM;
const SPV: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/cinema.spv"));
const DEPTH_ACCESS: (vk::PipelineStageFlags2, vk::AccessFlags2) = (
    vk::PipelineStageFlags2::from_raw(
        vk::PipelineStageFlags2::EARLY_FRAGMENT_TESTS.as_raw()
            | vk::PipelineStageFlags2::LATE_FRAGMENT_TESTS.as_raw(),
    ),
    vk::AccessFlags2::from_raw(
        vk::AccessFlags2::DEPTH_STENCIL_ATTACHMENT_READ.as_raw()
            | vk::AccessFlags2::DEPTH_STENCIL_ATTACHMENT_WRITE.as_raw(),
    ),
);

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct PushConstants {
    view_proj_model: [f32; 16],
    lights: f32,
    /// What bends with the screen bends round a circle this curved, 1 over
    /// its radius, centred on the seats' side of the screen's centre.
    curvature: f32,
    screen_x: f32,
    /// [`CinemaParams::lights_down`].
    lights_down: f32,
    /// The screen's centre (height and depth) and size, for the glow
    /// round it while the lights are down.
    screen: [f32; 4],
}

pub(crate) const PUSH_BYTES: u32 = size_of::<PushConstants>() as u32;

/// A mesh's part of the geometry buffer and where its copies stand.
struct Mesh {
    texture: usize,
    culled: bool,
    near: f32,
    far: f32,
    first_index: u32,
    indices: u32,
    vertex_offset: i32,
    /// A sphere round the mesh, for telling whether a copy is in view.
    centre: Vec3,
    radius: f32,
    placements: Vec<Placement>,
}

#[derive(Default)]
pub(crate) struct Cinema {
    pub layout: vk::PipelineLayout,
    pub pipeline: vk::Pipeline,
    pub scene_pipe: vk::Pipeline,
    pub quad_pipe: vk::Pipeline,
    geometry: Option<Buffer>,
    indices_at: u64,
    /// In drawing order: the culled meshes first, as what is near hides
    /// most of the room.
    meshes: Vec<Mesh>,
    textures: Vec<Image>,
    texture_sets: Vec<vk::DescriptorSet>,
    /// The tiling detail texture, sampled with `repeat`.
    detail_set: vk::DescriptorSet,
    repeat: vk::Sampler,
    /// Each frame slot's copies to draw.
    instances: Vec<Buffer>,
    /// The slot [`Self::place`] last filled, and each mesh's first copy
    /// and number of copies there.
    placed: usize,
    runs: Vec<(u32, u32)>,
    // Both eyes and all frame slots share this transient depth attachment.
    // The eye passes run in order on one queue and clear it on each use.
    depth: Option<Image>,
}

impl Cinema {
    pub fn new(gpu: &Gpu, create: impl FnOnce(&mut Self) -> Result<()>) -> Result<Self> {
        let mut room = Self::default();
        if let Err(e) = create(&mut room) {
            room.destroy(gpu);
            return Err(e);
        }
        Ok(room)
    }

    /// Uploads `data`, with room for the copies of `slots` frames in flight.
    pub fn upload(&mut self, renderer: &Renderer, data: RoomData, slots: usize) -> Result<()> {
        let gpu = renderer.gpu();
        let RoomData {
            mut meshes,
            mut textures,
            detail,
        } = data;
        // Without a detail texture, a flat mid-grey one changes nothing.
        let detail = detail.unwrap_or_else(|| {
            textures.push(Texture {
                width: 2,
                height: 2,
                pixels: vec![128; 16],
            });
            textures.len() - 1
        });
        if textures
            .iter()
            .enumerate()
            .any(|(i, t)| i != detail && t.width.min(t.height) < 1 << (ATLAS_MIPS - 1))
        {
            return Err(crate::Error::Unsupported(
                "cinema texture is too small for its mip levels".into(),
            ));
        }
        meshes.sort_by_key(|mesh| !mesh.culled);
        let (mut vertices, mut indices, mut copies) = (Vec::new(), Vec::new(), 0);
        for mesh in meshes {
            let positions = || mesh.vertices.iter().map(|v| Vec3::from_array(v.position));
            let (lo, hi) = positions().fold((Vec3::INFINITY, Vec3::NEG_INFINITY), |(lo, hi), p| {
                (lo.min(p), hi.max(p))
            });
            let centre = (lo + hi) * 0.5;
            let radius = positions().map(|p| p.distance(centre)).fold(0.0, f32::max);
            copies += mesh.placements.len();
            self.meshes.push(Mesh {
                texture: mesh.texture,
                culled: mesh.culled,
                near: mesh.near,
                far: mesh.far,
                first_index: indices.len() as u32,
                indices: mesh.indices.len() as u32,
                vertex_offset: vertices.len() as i32,
                centre,
                radius,
                placements: mesh.placements,
            });
            vertices.extend(mesh.vertices);
            indices.extend(mesh.indices);
        }
        let vertices: &[u8] = bytemuck::cast_slice(&vertices);
        let index_bytes: &[u8] = bytemuck::cast_slice(&indices);
        let texels_at = vertices.len() + index_bytes.len();
        self.indices_at = vertices.len() as u64;
        let geometry = self.geometry.insert(Buffer::new(
            gpu,
            texels_at as u64,
            vk::BufferUsageFlags::TRANSFER_DST
                | vk::BufferUsageFlags::VERTEX_BUFFER
                | vk::BufferUsageFlags::INDEX_BUFFER,
            MemoryLocation::GpuOnly,
            "cinema mesh",
        )?);
        for (i, texture) in textures.iter().enumerate() {
            // The detail texture's mipmaps go down to one texel, its
            // average: where the pattern is too fine to see, it fades out.
            let (format, mips) = if i == detail {
                let largest = texture.width.max(texture.height);
                (DETAIL_FORMAT, u32::BITS - largest.leading_zeros())
            } else {
                (ATLAS_FORMAT, ATLAS_MIPS)
            };
            let image = Image::new(
                gpu,
                texture.width,
                texture.height,
                &[format],
                vk::ImageUsageFlags::SAMPLED
                    | vk::ImageUsageFlags::TRANSFER_DST
                    | vk::ImageUsageFlags::TRANSFER_SRC,
                mips,
                "cinema texture",
            )?;
            self.textures.push(image);
        }
        let texels: usize = textures.iter().map(|t| t.pixels.len()).sum();
        let mut staging = Buffer::new(
            gpu,
            (texels_at + texels) as u64,
            vk::BufferUsageFlags::TRANSFER_SRC,
            MemoryLocation::CpuToGpu,
            "cinema upload",
        )?;
        let bytes = staging.bytes();
        bytes[..vertices.len()].copy_from_slice(vertices);
        bytes[vertices.len()..texels_at].copy_from_slice(index_bytes);
        let mut offsets = Vec::with_capacity(textures.len());
        let mut at = texels_at;
        for texture in &textures {
            bytes[at..at + texture.pixels.len()].copy_from_slice(&texture.pixels);
            offsets.push(at as u64);
            at += texture.pixels.len();
        }
        let images = &self.textures;
        let uploaded = renderer.one_shot(|gpu, cmd| {
            let d = &gpu.device;
            let copy = vk::BufferCopy::default().size(texels_at as u64);
            // SAFETY: both buffers are allocated for this transfer and the
            // command is recording.
            unsafe { d.cmd_copy_buffer(cmd, staging.buffer, geometry.buffer, &[copy]) };
            for (image, &offset) in images.iter().zip(&offsets) {
                upload_atlas(gpu, cmd, image, staging.buffer, offset);
            }
            let ready = [vk::BufferMemoryBarrier2::default()
                .buffer(geometry.buffer)
                .size(vk::WHOLE_SIZE)
                .src_stage_mask(vk::PipelineStageFlags2::TRANSFER)
                .src_access_mask(vk::AccessFlags2::TRANSFER_WRITE)
                .dst_stage_mask(vk::PipelineStageFlags2::VERTEX_INPUT)
                .dst_access_mask(
                    vk::AccessFlags2::VERTEX_ATTRIBUTE_READ | vk::AccessFlags2::INDEX_READ,
                )];
            // SAFETY: records a barrier for the buffer copied above.
            unsafe {
                d.cmd_pipeline_barrier2(
                    cmd,
                    &vk::DependencyInfo::default().buffer_memory_barriers(&ready),
                );
            };
        });
        staging.destroy(gpu);
        uploaded?;
        for image in &self.textures {
            self.texture_sets.push(renderer.tex_set(image.views[0])?);
        }
        // SAFETY: valid create info; the sampler is destroyed with the room.
        self.repeat = unsafe {
            gpu.device.create_sampler(
                &vk::SamplerCreateInfo::default()
                    .mag_filter(vk::Filter::LINEAR)
                    .min_filter(vk::Filter::LINEAR)
                    .mipmap_mode(vk::SamplerMipmapMode::LINEAR)
                    .address_mode_u(vk::SamplerAddressMode::REPEAT)
                    .address_mode_v(vk::SamplerAddressMode::REPEAT)
                    .address_mode_w(vk::SamplerAddressMode::REPEAT)
                    .max_lod(vk::LOD_CLAMP_NONE),
                None,
            )
        }
        .ctx("detail sampler")?;
        self.detail_set = renderer.tex_set_with(self.textures[detail].views[0], self.repeat)?;
        for _ in 0..slots {
            let buffer = Buffer::new(
                gpu,
                (copies * size_of::<Placement>()) as u64,
                vk::BufferUsageFlags::VERTEX_BUFFER,
                MemoryLocation::CpuToGpu,
                "cinema copies",
            )?;
            self.instances.push(buffer);
        }
        Ok(())
    }

    /// Chooses frame slot `slot`'s copies: each at the detail for its
    /// distance from `eyes`, the point between the eyes in room metres, and
    /// of a culled mesh only those in view, nearest first. `room_to_clip`
    /// takes room metres to each eye's clip space.
    pub fn place(&mut self, slot: usize, room_to_clip: [Mat4; 2], eyes: Vec3) {
        let planes = room_to_clip.map(|m| {
            let (x, y, w) = (m.row(0), m.row(1), m.row(3));
            [w + x, w - x, w + y, w - y].map(|p: Vec4| p / p.truncate().length().max(1e-6))
        });
        let in_view = |centre: Vec3, radius: f32| {
            planes.iter().any(|eye| {
                eye.iter()
                    .all(|p| p.truncate().dot(centre) + p.w >= -radius)
            })
        };
        let mut drawn = Vec::new();
        self.runs.clear();
        for mesh in &self.meshes {
            let first = drawn.len();
            drawn.extend(mesh.placements.iter().filter_map(|&p| {
                let at = Vec3::from_array(p.at);
                let distance = at.distance(eyes);
                let shown = (mesh.near..mesh.far).contains(&distance)
                    && (!mesh.culled
                        || in_view(at + Quat::from_rotation_y(p.yaw) * mesh.centre, mesh.radius));
                shown.then_some((distance, p))
            }));
            if mesh.culled {
                drawn[first..].sort_by(|a, b| a.0.total_cmp(&b.0));
            }
            self.runs.push((first as u32, (drawn.len() - first) as u32));
        }
        let drawn: Vec<Placement> = drawn.into_iter().map(|(_, p)| p).collect();
        let bytes: &[u8] = bytemuck::cast_slice(&drawn);
        self.instances[slot].bytes()[..bytes.len()].copy_from_slice(bytes);
        self.placed = slot;
    }

    pub fn prepare_depth(
        &mut self,
        gpu: &Gpu,
        targets: &[EyeTarget; 2],
        garbage: &mut Vec<Image>,
    ) -> Result<()> {
        let [a, b] = targets.map(|eye| eye.extent);
        let (w, h) = (a.width.max(b.width), a.height.max(b.height));
        if self
            .depth
            .as_ref()
            .is_some_and(|i| i.extent.width == w && i.extent.height == h)
        {
            return Ok(());
        }
        let new = Image::new(
            gpu,
            w,
            h,
            &[DEPTH_FORMAT],
            vk::ImageUsageFlags::DEPTH_STENCIL_ATTACHMENT
                | vk::ImageUsageFlags::TRANSIENT_ATTACHMENT,
            1,
            "cinema depth",
        )?;
        // The current slot's fence will finish after the old image's last
        // use, including an earlier draw() in the same command buffer.
        garbage.extend(self.depth.replace(new));
        Ok(())
    }

    pub fn depth_attachment(
        &self,
        gpu: &Gpu,
        cmd: vk::CommandBuffer,
    ) -> vk::RenderingAttachmentInfo<'static> {
        let image = self.depth.as_ref().expect("prepared cinema depth");
        // Contents are cleared for each eye. Synchronize with prior depth
        // attachment access on this queue, including previous frame slots.
        mem::barrier(
            gpu,
            cmd,
            image.image,
            mem::range(0, 1).aspect_mask(vk::ImageAspectFlags::DEPTH),
            vk::ImageLayout::UNDEFINED,
            vk::ImageLayout::DEPTH_STENCIL_ATTACHMENT_OPTIMAL,
            DEPTH_ACCESS,
            DEPTH_ACCESS,
        );
        vk::RenderingAttachmentInfo::default()
            .image_view(image.level0)
            .image_layout(vk::ImageLayout::DEPTH_STENCIL_ATTACHMENT_OPTIMAL)
            .load_op(vk::AttachmentLoadOp::CLEAR)
            .store_op(vk::AttachmentStoreOp::DONT_CARE)
            .clear_value(vk::ClearValue {
                depth_stencil: vk::ClearDepthStencilValue {
                    depth: 1.0,
                    stencil: 0,
                },
            })
    }

    /// Draws the copies [`Self::place`] chose.
    pub fn draw(&self, gpu: &Gpu, cmd: vk::CommandBuffer, view_proj: Mat4, cinema: CinemaParams) {
        let geometry = self.geometry.as_ref().expect("uploaded cinema mesh");
        let (centre, size) = (cinema.room.screen_centre(), cinema.room.screen_size());
        let push = PushConstants {
            view_proj_model: (view_proj * cinema.pose).to_cols_array(),
            lights: cinema.lights.clamp(0.0, 1.0),
            curvature: cinema.curvature.max(0.0),
            screen_x: centre.x,
            lights_down: cinema.lights_down.clamp(0.0, 1.0),
            screen: [centre.y, centre.z, size.x, size.y],
        };
        let d = &gpu.device;
        // SAFETY: an eye pass with a compatible depth attachment is active,
        // and the previously uploaded textures and buffers live with the
        // renderer.
        unsafe {
            let bind = vk::PipelineBindPoint::GRAPHICS;
            d.cmd_bind_pipeline(cmd, bind, self.pipeline);
            d.cmd_push_constants(
                cmd,
                self.layout,
                vk::ShaderStageFlags::VERTEX | vk::ShaderStageFlags::FRAGMENT,
                0,
                bytemuck::bytes_of(&push),
            );
            d.cmd_bind_vertex_buffers(
                cmd,
                0,
                &[geometry.buffer, self.instances[self.placed].buffer],
                &[0, 0],
            );
            d.cmd_bind_index_buffer(cmd, geometry.buffer, self.indices_at, vk::IndexType::UINT32);
            d.cmd_bind_descriptor_sets(cmd, bind, self.layout, 1, &[self.detail_set], &[]);
            let mut bound = None;
            for (mesh, &(first, count)) in self.meshes.iter().zip(&self.runs) {
                if count == 0 {
                    continue;
                }
                if bound != Some(mesh.texture) {
                    let set = [self.texture_sets[mesh.texture]];
                    d.cmd_bind_descriptor_sets(cmd, bind, self.layout, 0, &set, &[]);
                    bound = Some(mesh.texture);
                }
                d.cmd_draw_indexed(
                    cmd,
                    mesh.indices,
                    count,
                    mesh.first_index,
                    mesh.vertex_offset,
                    first,
                );
            }
        }
    }

    /// The room's descriptor sets, from the renderer's pool: [`Self::destroy`]
    /// leaves them to it.
    pub fn pool_sets(&self) -> impl Iterator<Item = vk::DescriptorSet> + '_ {
        self.texture_sets
            .iter()
            .copied()
            .chain([self.detail_set])
            .filter(|set| *set != vk::DescriptorSet::null())
    }

    pub fn destroy(mut self, gpu: &Gpu) {
        // SAFETY: all passes are idle (or have never used the room).
        unsafe {
            for pipeline in [self.pipeline, self.scene_pipe, self.quad_pipe] {
                gpu.device.destroy_pipeline(pipeline, None);
            }
            gpu.device.destroy_pipeline_layout(self.layout, None);
            gpu.device.destroy_sampler(self.repeat, None);
        }
        for buffer in self.geometry.iter_mut().chain(&mut self.instances) {
            buffer.destroy(gpu);
        }
        for image in self.textures.iter_mut().chain(self.depth.as_mut()) {
            image.destroy(gpu);
        }
    }
}

pub(crate) fn pipeline(
    gpu: &Gpu,
    layout: vk::PipelineLayout,
    color_format: vk::Format,
    shading_rate: ShadingRate,
) -> Result<vk::Pipeline> {
    let attributes = [
        (
            0,
            vk::Format::R32G32B32_SFLOAT,
            std::mem::offset_of!(Vertex, position),
        ),
        (
            0,
            vk::Format::R32G32_SFLOAT,
            std::mem::offset_of!(Vertex, uv),
        ),
        (
            0,
            vk::Format::R8G8B8A8_UINT,
            std::mem::offset_of!(Vertex, flags),
        ),
        (1, vk::Format::R32G32B32A32_SFLOAT, 0),
    ];
    let attributes: [_; 4] = std::array::from_fn(|i| {
        let (binding, format, offset) = attributes[i];
        vk::VertexInputAttributeDescription {
            location: i as u32,
            binding,
            format,
            offset: offset as u32,
        }
    });
    pipeline::create(
        gpu,
        &PipelineDesc {
            spv: SPV,
            fragment: c"fs_main",
            layout,
            color_format,
            depth: Depth::Tested,
            vertex_stride: size_of::<Vertex>() as u32,
            instance_stride: size_of::<Placement>() as u32,
            attributes: &attributes,
            blend: false,
            descriptor_buffer: false,
            shading_rate,
        },
    )
}

fn upload_atlas(gpu: &Gpu, cmd: vk::CommandBuffer, atlas: &Image, buffer: vk::Buffer, offset: u64) {
    let src_layout = vk::ImageLayout::TRANSFER_SRC_OPTIMAL;
    let dst_layout = vk::ImageLayout::TRANSFER_DST_OPTIMAL;
    let image = atlas.image;
    let (width, height) = (atlas.extent.width, atlas.extent.height);
    mem::barrier(
        gpu,
        cmd,
        image,
        mem::range(0, atlas.mips),
        vk::ImageLayout::UNDEFINED,
        dst_layout,
        mem::TOP,
        mem::TRANSFER_WRITE,
    );
    let sub = |mip| {
        vk::ImageSubresourceLayers::default()
            .aspect_mask(vk::ImageAspectFlags::COLOR)
            .mip_level(mip)
            .layer_count(1)
    };
    let head = vk::BufferImageCopy::default()
        .buffer_offset(offset)
        .image_subresource(sub(0))
        .image_extent(vk::Extent3D {
            width,
            height,
            depth: 1,
        });
    // SAFETY: the first level is ready and the staging buffer contains all texels.
    unsafe {
        gpu.device
            .cmd_copy_buffer_to_image(cmd, buffer, image, dst_layout, &[head]);
    };
    for level in 1..atlas.mips {
        mem::barrier(
            gpu,
            cmd,
            image,
            mem::range(level - 1, 1),
            dst_layout,
            src_layout,
            mem::TRANSFER_WRITE,
            mem::TRANSFER_READ,
        );
        let extent = |mip: u32| vk::Offset3D {
            x: (width >> mip).max(1) as i32,
            y: (height >> mip).max(1) as i32,
            z: 1,
        };
        let blit = vk::ImageBlit::default()
            .src_subresource(sub(level - 1))
            .src_offsets([vk::Offset3D::default(), extent(level - 1)])
            .dst_subresource(sub(level))
            .dst_offsets([vk::Offset3D::default(), extent(level)]);
        // SAFETY: the linear-filterable RGBA8 image has nonoverlapping
        // source and destination mip regions in their respective layouts.
        unsafe {
            gpu.device.cmd_blit_image(
                cmd,
                image,
                src_layout,
                image,
                dst_layout,
                &[blit],
                vk::Filter::LINEAR,
            );
        };
    }
    let shader = vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL;
    mem::barrier(
        gpu,
        cmd,
        image,
        mem::range(0, atlas.mips - 1),
        src_layout,
        shader,
        mem::TRANSFER_READ,
        mem::SHADER_READ,
    );
    mem::barrier(
        gpu,
        cmd,
        image,
        mem::range(atlas.mips - 1, 1),
        dst_layout,
        shader,
        mem::TRANSFER_WRITE,
        mem::SHADER_READ,
    );
}

/// How much nearer than the screen's nearest corner the picture is drawn:
/// enough that the room's own screen behind it never shows through, and
/// far less than the metres between any row and the screen, so the seats in
/// front still hide it.
const SCREEN_DEPTH_MARGIN: f32 = 1.0;

/// The video's outline, `arc` radians round a circle towards the viewer, in
/// its own metres: the corners, then the middles of the top and bottom.
fn screen_outline(size: Vec2, arc: f32) -> [Vec3; 6] {
    let h = size * 0.5;
    // A curve's corners come forward and in; its edges' middles stay where
    // the flat screen's are, and its outline runs between them.
    let (x, z) = if arc > 1e-3 {
        let r = size.x / arc;
        (r * (arc / 2.0).sin(), r * (1.0 - (arc / 2.0).cos()))
    } else {
        (h.x, 0.0)
    };
    [
        Vec3::new(x, h.y, z),
        Vec3::new(-x, h.y, z),
        Vec3::new(x, -h.y, z),
        Vec3::new(-x, -h.y, z),
        Vec3::new(0.0, h.y, 0.0),
        Vec3::new(0.0, -h.y, 0.0),
    ]
}

/// The depth an eye draws the video at in a cinema, so that the room in
/// front of the screen hides it: its nearest corner's,
/// [`SCREEN_DEPTH_MARGIN`] nearer. The screen is a straight-sided curve
/// bowing away from the seats, so no point of it is nearer than a corner.
/// 0, in front of everything, when the screen reaches behind the eye.
pub(crate) fn screen_depth(eye_from_screen: Mat4, proj: Mat4, size: Vec2, arc: f32) -> f32 {
    let nearest = screen_outline(size, arc)[..4]
        .iter()
        .map(|&c| eye_from_screen.transform_point3(c).z)
        .fold(f32::NEG_INFINITY, f32::max);
    let z = nearest + SCREEN_DEPTH_MARGIN;
    let clip = proj * Vec4::new(0.0, 0.0, z, 1.0);
    if z.is_nan() || z >= 0.0 || clip.w <= 0.0 || !clip.is_finite() {
        return 0.0;
    }
    (clip.z / clip.w).clamp(0.0, 1.0)
}

/// A conservative raster rectangle around the video, `arc` radians round a
/// circle towards the viewer. If an edge crosses the near plane, let the
/// existing ray projection clip it instead.
pub(crate) fn screen_area(
    view_proj: Mat4,
    size: Vec2,
    arc: f32,
    full: vk::Rect2D,
) -> Option<vk::Rect2D> {
    let corners = screen_outline(size, arc).map(|c| view_proj * c.extend(1.0));
    if corners.iter().all(|p| p.w <= 0.0 || p.z < 0.0) {
        return None;
    }
    if corners
        .iter()
        .any(|p| !p.is_finite() || p.w <= 0.0 || p.z < 0.0)
    {
        return Some(full);
    }
    let ndc = corners.map(|p| Vec2::new(p.x / p.w, p.y / p.w));
    let lo = ndc.into_iter().fold(Vec2::splat(f32::INFINITY), Vec2::min);
    let hi = ndc
        .into_iter()
        .fold(Vec2::splat(f32::NEG_INFINITY), Vec2::max);
    if lo.x >= 1.0 || lo.y >= 1.0 || hi.x <= -1.0 || hi.y <= -1.0 {
        return None;
    }
    let span = Vec2::new(full.extent.width as f32, full.extent.height as f32);
    // Leave room for the representative sample of a coarse foveated fragment
    // at the edge of the screen.
    let pad = Vec2::splat(8.0);
    let lo = (((lo + 1.0) * 0.5 * span).floor() - pad).max(Vec2::ZERO);
    let hi = (((hi + 1.0) * 0.5 * span).ceil() + pad).min(span);
    Some(vk::Rect2D {
        offset: vk::Offset2D {
            x: full.offset.x + lo.x as i32,
            y: full.offset.y + lo.y as i32,
        },
        extent: vk::Extent2D {
            width: (hi.x - lo.x) as u32,
            height: (hi.y - lo.y) as u32,
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_screen_area_holds_a_curved_picture() {
        let (size, arc) = (Vec2::new(8.0, 3.5), 0.6);
        let full = vk::Rect2D {
            offset: vk::Offset2D { x: 0, y: 0 },
            extent: vk::Extent2D {
                width: 1000,
                height: 1000,
            },
        };
        // From 6 m away, a little below the middle.
        let view_proj = glam::camera::rh::proj::directx::perspective(1.6, 1.0, 0.1, 100.0)
            * glam::camera::rh::view::look_at_mat4(Vec3::new(0.0, -0.5, 6.0), Vec3::ZERO, Vec3::Y);
        let area = screen_area(view_proj, size, arc, full).expect("in view");
        let (x0, y0) = (area.offset.x as f32, area.offset.y as f32);
        let (x1, y1) = (
            x0 + area.extent.width as f32,
            y0 + area.extent.height as f32,
        );
        let r = size.x / arc;
        for i in 0..=20 {
            let a = (i as f32 / 20.0 - 0.5) * arc;
            for y in [-size.y / 2.0, size.y / 2.0] {
                let p = view_proj * Vec4::new(r * a.sin(), y, r * (1.0 - a.cos()), 1.0);
                let at = (Vec2::new(p.x, p.y) / p.w + 1.0) * 500.0;
                assert!(
                    (x0..=x1).contains(&at.x) && (y0..=y1).contains(&at.y),
                    "{at} outside {area:?}"
                );
            }
        }
    }

    #[test]
    fn the_picture_is_behind_the_seats_and_before_the_room_screen() {
        let (size, arc) = (Vec2::new(8.0, 3.5), 0.6);
        let proj = crate::math::projection(crate::math::Fov::symmetric(100.0), 0.05, 100.0);
        // The screen 10 m ahead; its curve brings the corners forward.
        let eye_from_screen = Mat4::from_translation(Vec3::new(0.0, 0.0, -10.0));
        let depth = screen_depth(eye_from_screen, proj, size, arc);
        let at = |z: f32| {
            let p = proj * Vec4::new(0.0, 0.0, z, 1.0);
            p.z / p.w
        };
        let r = size.x / arc;
        let corner = -10.0 + r * (1.0 - (arc / 2.0).cos());
        assert!(depth < at(corner), "in front of the room's screen");
        assert!(depth > at(-1.5), "behind a seat back 1.5 m away");
        // Turned away, the screen reaches behind the eye: drawn over all.
        let behind = Mat4::from_translation(Vec3::new(0.0, 0.0, 2.0));
        assert_eq!(screen_depth(behind, proj, size, arc), 0.0);
    }
}
