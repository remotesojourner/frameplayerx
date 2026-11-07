//! Video frames read in the hardware decoder's own buffers (zero copy).
//!
//! The decoder lends a buffer with each frame ([`fp_media::SharedPicture`])
//! as a DMA-BUF. Each buffer is imported once and the import kept while the
//! decoder hands it out again: an NV12 picture as two plane images in the
//! decoder's linear layout, sampled by the eye passes as uploaded ones are;
//! a Q10C one as the converter's carriers over its memory
//! ([`crate::q10c::Converter::import`]), converted into the image set's P010
//! planes as copied ones are. Every use takes the images over from the
//! foreign queue family and gives them back after.
//!
//! An import keeps the decoder's memory allocated, which the next video's
//! decoder may need, so imports of another buffer set go as soon as a frame
//! of a new set arrives or the video stops, and any unused for a while go
//! too. They go through the slot's garbage, by which time no frame still
//! reads them.

use crate::gpu::Gpu;
use crate::mem;
use crate::q10c::Picture;
use crate::{Error, Result, VkContext};
use ash::vk;
use fp_media::SharedPicture;
use std::collections::HashMap;
use std::os::fd::{BorrowedFd, IntoRawFd};
use std::time::{Duration, Instant};

/// Imports not used for this long are dropped (a paused video keeps the
/// one on screen).
const KEEP_UNUSED: Duration = Duration::from_secs(5);

/// `len` bytes of the DMA-BUF `fd`, imported as memory of a type in
/// `bits`, dedicated to `image` when the driver wants that. The caller
/// keeps `fd`: the import takes a duplicate.
pub(crate) fn import(
    gpu: &Gpu,
    fd: i32,
    len: u64,
    bits: u32,
    image: Option<vk::Image>,
) -> Result<vk::DeviceMemory> {
    let Some(ext) = gpu.memory_fd.as_ref() else {
        return Err(Error::Unsupported("this GPU can't import DMA-BUFs".into()));
    };
    let handle = vk::ExternalMemoryHandleTypeFlags::DMA_BUF_EXT;
    let mut props = vk::MemoryFdPropertiesKHR::default();
    // SAFETY: fd is an open DMA-BUF (the frame holding it is alive).
    unsafe { ext.get_memory_fd_properties(handle, fd, &mut props) }.ctx("DMA-BUF properties")?;
    let bits = bits & props.memory_type_bits;
    // SAFETY: valid physical device.
    let types = unsafe { gpu.instance.get_physical_device_memory_properties(gpu.pdev) };
    let index = (0..types.memory_type_count)
        .filter(|&i| bits & (1 << i) != 0)
        .max_by_key(|&i| {
            types.memory_types[i as usize]
                .property_flags
                .contains(vk::MemoryPropertyFlags::DEVICE_LOCAL)
        })
        .ok_or_else(|| Error::Unsupported("no memory type for the decoder's buffer".into()))?;
    let dedicated_wanted = image.is_some_and(|image| {
        let mut ded = vk::MemoryDedicatedRequirements::default();
        let mut req = vk::MemoryRequirements2::default().push_next(&mut ded);
        // SAFETY: a valid image.
        unsafe {
            gpu.device.get_image_memory_requirements2(
                &vk::ImageMemoryRequirementsInfo2::default().image(image),
                &mut req,
            );
        }
        ded.prefers_dedicated_allocation == vk::TRUE
            || ded.requires_dedicated_allocation == vk::TRUE
    });
    // SAFETY: fd is open for the duration of this call.
    let own = unsafe { BorrowedFd::borrow_raw(fd) }
        .try_clone_to_owned()
        .map_err(|e| Error::Unsupported(format!("duplicating a DMA-BUF: {e}")))?;
    let mut dedicated = vk::MemoryDedicatedAllocateInfo::default().image(image.unwrap_or_default());
    let raw = own.into_raw_fd();
    let mut import = vk::ImportMemoryFdInfoKHR::default()
        .handle_type(handle)
        .fd(raw);
    let mut info = vk::MemoryAllocateInfo::default()
        .allocation_size(len)
        .memory_type_index(index)
        .push_next(&mut import);
    if dedicated_wanted {
        info = info.push_next(&mut dedicated);
    }
    // SAFETY: valid allocate info; on success the driver owns `raw`.
    match unsafe { gpu.device.allocate_memory(&info, None) } {
        Ok(m) => Ok(m),
        Err(e) => {
            // SAFETY: still ours after a failed import.
            drop(unsafe { <std::os::fd::OwnedFd as std::os::fd::FromRawFd>::from_raw_fd(raw) });
            Err(e).ctx("importing the decoder's buffer")
        }
    }
}

/// `ExternalMemoryImageCreateInfo` for images bound to a DMA-BUF.
pub(crate) fn external_info() -> vk::ExternalMemoryImageCreateInfo<'static> {
    vk::ExternalMemoryImageCreateInfo::default()
        .handle_types(vk::ExternalMemoryHandleTypeFlags::DMA_BUF_EXT)
}

/// An NV12 picture's two planes in the decoder's buffer, as R8 and R8G8
/// images of its linear layout.
pub(crate) struct Nv12 {
    images: [vk::Image; 2],
    memory: [vk::DeviceMemory; 2],
    pub views: [vk::ImageView; 2],
    /// Out of UNDEFINED, in SHADER_READ_ONLY_OPTIMAL between uses.
    laid_out: bool,
}

impl Nv12 {
    fn import(gpu: &Gpu, p: &SharedPicture, size: (u32, u32)) -> Result<Nv12> {
        let mut n = Nv12 {
            images: [vk::Image::null(); 2],
            memory: [vk::DeviceMemory::null(); 2],
            views: [vk::ImageView::null(); 2],
            laid_out: false,
        };
        let planes = [
            (vk::Format::R8_UNORM, size, 0),
            (
                vk::Format::R8G8_UNORM,
                (size.0.div_ceil(2), size.1.div_ceil(2)),
                p.uv_offset as u64,
            ),
        ];
        for (i, (format, (w, h), offset)) in planes.into_iter().enumerate() {
            if let Err(e) = n.plane(gpu, p, i, format, w, h, offset) {
                n.destroy(gpu);
                return Err(e);
            }
        }
        Ok(n)
    }

    /// Plane `i`: an image at `offset` in the buffer, with its own import
    /// of it (as VideoFrameVR does on the Frame).
    #[allow(clippy::too_many_arguments)]
    fn plane(
        &mut self,
        gpu: &Gpu,
        p: &SharedPicture,
        i: usize,
        format: vk::Format,
        w: u32,
        h: u32,
        offset: u64,
    ) -> Result<()> {
        let d = &gpu.device;
        let layouts = [vk::SubresourceLayout {
            offset,
            row_pitch: p.stride as u64,
            ..Default::default()
        }];
        let mut explicit = vk::ImageDrmFormatModifierExplicitCreateInfoEXT::default()
            .drm_format_modifier(0) // DRM_FORMAT_MOD_LINEAR
            .plane_layouts(&layouts);
        let mut external = external_info();
        let info = vk::ImageCreateInfo::default()
            .image_type(vk::ImageType::TYPE_2D)
            .format(format)
            .extent(vk::Extent3D {
                width: w,
                height: h,
                depth: 1,
            })
            .mip_levels(1)
            .array_layers(1)
            .samples(vk::SampleCountFlags::TYPE_1)
            .tiling(vk::ImageTiling::DRM_FORMAT_MODIFIER_EXT)
            .usage(vk::ImageUsageFlags::SAMPLED)
            .initial_layout(vk::ImageLayout::UNDEFINED)
            .push_next(&mut explicit)
            .push_next(&mut external);
        // SAFETY: valid create info; destroyed by `destroy`.
        unsafe {
            self.images[i] = d.create_image(&info, None).ctx("decoder plane image")?;
            let req = d.get_image_memory_requirements(self.images[i]);
            if req.size > p.len as u64 {
                return Err(Error::Unsupported(format!(
                    "a decoder plane needs {} bytes, the buffer has {}",
                    req.size, p.len
                )));
            }
            self.memory[i] = import(
                gpu,
                p.fd,
                p.len as u64,
                req.memory_type_bits,
                Some(self.images[i]),
            )?;
            d.bind_image_memory(self.images[i], self.memory[i], 0)
                .ctx("bind decoder plane")?;
        }
        self.views[i] = mem::view(gpu, self.images[i], format, 0, 1)?;
        Ok(())
    }

    /// Takes the planes over from the decoder before the eye passes read
    /// them, ready to sample.
    fn acquire(&mut self, gpu: &Gpu, cmd: vk::CommandBuffer) {
        let old = if self.laid_out {
            vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL
        } else {
            vk::ImageLayout::UNDEFINED
        };
        self.laid_out = true;
        for &image in &self.images {
            mem::ownership_barrier(
                gpu,
                cmd,
                image,
                old,
                vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL,
                mem::TOP,
                (
                    vk::PipelineStageFlags2::FRAGMENT_SHADER,
                    vk::AccessFlags2::SHADER_SAMPLED_READ,
                ),
                (gpu.foreign_queue, gpu.queue_family),
            );
        }
    }

    /// Gives the planes back to the decoder after the eye passes.
    fn release(&self, gpu: &Gpu, cmd: vk::CommandBuffer) {
        for &image in &self.images {
            mem::ownership_barrier(
                gpu,
                cmd,
                image,
                vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL,
                vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL,
                (
                    vk::PipelineStageFlags2::FRAGMENT_SHADER,
                    vk::AccessFlags2::SHADER_SAMPLED_READ,
                ),
                mem::BOTTOM,
                (gpu.queue_family, gpu.foreign_queue),
            );
        }
    }

    fn destroy(&mut self, gpu: &Gpu) {
        // SAFETY: not in use (garbage of a finished slot, or the device is
        // idle); null handles are ignored.
        unsafe {
            for v in &mut self.views {
                gpu.device.destroy_image_view(std::mem::take(v), None);
            }
            for i in &mut self.images {
                gpu.device.destroy_image(std::mem::take(i), None);
            }
            for m in &mut self.memory {
                gpu.device.free_memory(std::mem::take(m), None);
            }
        }
    }
}

/// An import on its way out, destroyed with its slot's garbage.
pub(crate) enum Imported {
    Nv12(Nv12),
    Q10c(Picture),
}

impl Imported {
    pub(crate) fn destroy(&mut self, gpu: &Gpu) {
        match self {
            Imported::Nv12(n) => n.destroy(gpu),
            Imported::Q10c(p) => p.destroy(gpu),
        }
    }
}

struct Entry<T> {
    import: T,
    set: u64,
    used: Instant,
}

/// Where a Q10C import converts to: the renderer's image set and its
/// generation (the converter's descriptors name the output images).
pub(crate) type Target = (usize, u64);

/// The imports of the decoder's buffers, by buffer.
#[derive(Default)]
pub(crate) struct Imports {
    nv12: HashMap<u64, Entry<Nv12>>,
    q10c: HashMap<(u64, Target), Entry<Picture>>,
}

impl Imports {
    /// Moves imports of buffer sets other than `keep` (all when `None`),
    /// or unused for [`KEEP_UNUSED`], to `garbage`.
    pub(crate) fn retire(&mut self, keep: Option<u64>, garbage: &mut Vec<Imported>) {
        let now = Instant::now();
        let stale = |set: u64, used: Instant| keep != Some(set) || now - used > KEEP_UNUSED;
        let old: Vec<u64> = self
            .nv12
            .iter()
            .filter(|(_, e)| stale(e.set, e.used))
            .map(|(&k, _)| k)
            .collect();
        for k in old {
            if let Some(e) = self.nv12.remove(&k) {
                garbage.push(Imported::Nv12(e.import));
            }
        }
        let old: Vec<(u64, Target)> = self
            .q10c
            .iter()
            .filter(|(_, e)| stale(e.set, e.used))
            .map(|(&k, _)| k)
            .collect();
        for k in old {
            if let Some(e) = self.q10c.remove(&k) {
                garbage.push(Imported::Q10c(e.import));
            }
        }
    }

    /// Q10C imports converting into an image set that was replaced.
    pub(crate) fn retire_target(&mut self, target: Target, garbage: &mut Vec<Imported>) {
        let old: Vec<(u64, Target)> = self
            .q10c
            .keys()
            .filter(|(_, t)| t.0 == target.0 && t.1 != target.1)
            .copied()
            .collect();
        for k in old {
            if let Some(e) = self.q10c.remove(&k) {
                garbage.push(Imported::Q10c(e.import));
            }
        }
    }

    /// The NV12 picture `p` of visible `size`, imported on first use, taken
    /// over for this frame's command buffer.
    pub(crate) fn nv12(
        &mut self,
        gpu: &Gpu,
        cmd: vk::CommandBuffer,
        p: &SharedPicture,
        size: (u32, u32),
    ) -> Result<&Nv12> {
        let e = match self.nv12.entry(p.buffer) {
            std::collections::hash_map::Entry::Occupied(o) => o.into_mut(),
            std::collections::hash_map::Entry::Vacant(v) => v.insert(Entry {
                import: Nv12::import(gpu, p, size)?,
                set: p.set,
                used: Instant::now(),
            }),
        };
        e.used = Instant::now();
        e.import.acquire(gpu, cmd);
        Ok(&e.import)
    }

    /// The views of NV12 buffer `buffer`, if imported.
    pub(crate) fn nv12_views(&self, buffer: u64) -> Option<[vk::ImageView; 2]> {
        self.nv12.get(&buffer).map(|e| e.import.views)
    }

    /// Gives NV12 buffer `buffer` back after the eye passes.
    pub(crate) fn release_nv12(&self, gpu: &Gpu, cmd: vk::CommandBuffer, buffer: u64) {
        if let Some(e) = self.nv12.get(&buffer) {
            e.import.release(gpu, cmd);
        }
    }

    /// The Q10C import of buffer `p` converting into `target`, made by
    /// `make` on first use.
    pub(crate) fn q10c(
        &mut self,
        p: &SharedPicture,
        target: Target,
        make: impl FnOnce() -> Result<Picture>,
    ) -> Result<&mut Picture> {
        let e = match self.q10c.entry((p.buffer, target)) {
            std::collections::hash_map::Entry::Occupied(o) => o.into_mut(),
            std::collections::hash_map::Entry::Vacant(v) => v.insert(Entry {
                import: make()?,
                set: p.set,
                used: Instant::now(),
            }),
        };
        e.used = Instant::now();
        Ok(&mut e.import)
    }

    /// Destroys everything (the device is idle).
    pub(crate) fn destroy(&mut self, gpu: &Gpu) {
        for (_, mut e) in self.nv12.drain() {
            e.import.destroy(gpu);
        }
        for (_, mut e) in self.q10c.drain() {
            e.import.destroy(gpu);
        }
    }
}
