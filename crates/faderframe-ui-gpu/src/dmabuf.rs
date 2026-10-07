//! Frames handed to the toolkit as dmabufs (Linux): no copy through the
//! CPU. The Vulkan device is opened with external memory (dma-buf export);
//! each frame's texture is copied on the GPU into a buffer whose memory is
//! a dmabuf — linear RGBA rows, padded to 256 bytes as importers want them —
//! and the toolkit imports that buffer (DRM `AB24`, linear modifier,
//! straight alpha). Buffers are pooled and reused only after the toolkit
//! has let go of them.

use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use vello::wgpu;
use wgpu::hal::api::Vulkan;
use wgpu::hal::vulkan as hal;

use ash::vk;

/// DRM `ABGR8888` ("AB24"): bytes R, G, B, A in memory — `Rgba8Unorm`.
pub const FOURCC_AB24: u32 = u32::from_le_bytes(*b"AB24");
/// DRM `FORMAT_MOD_LINEAR`.
pub const MODIFIER_LINEAR: u64 = 0;
/// Buffers kept for frames the toolkit may still show.
const POOL: usize = 3;

/// The device extensions dmabuf export needs.
fn extensions() -> [&'static std::ffi::CStr; 2] {
    [
        ash::khr::external_memory_fd::NAME,
        ash::ext::external_memory_dma_buf::NAME,
    ]
}

/// Open `adapter`'s Vulkan device with dmabuf export, if it is a Vulkan
/// adapter that has it.
pub(crate) fn open_device(
    adapter: &wgpu::Adapter,
    desc: &wgpu::DeviceDescriptor<'_>,
) -> Option<(wgpu::Device, wgpu::Queue)> {
    let open = {
        // SAFETY: the guard is only used here, while `adapter` lives; the
        // callback only adds extensions the physical device lists.
        let hal_adapter = unsafe { adapter.as_hal::<Vulkan>() }?;
        let shared = hal_adapter.shared_instance();
        let phd = hal_adapter.raw_physical_device();
        // SAFETY: `phd` comes from this instance.
        let available = unsafe {
            shared
                .raw_instance()
                .enumerate_device_extension_properties(phd)
        }
        .ok()?;
        let has = |name: &std::ffi::CStr| {
            available
                .iter()
                .any(|e| e.extension_name_as_c_str().is_ok_and(|n| n == name))
        };
        if !extensions().iter().all(|e| has(e)) {
            return None;
        }
        // SAFETY: the callback adds supported extensions and removes
        // nothing (wgpu-hal's rules for `open_with_callback`).
        unsafe {
            hal_adapter.open_with_callback(
                desc.required_features,
                &desc.required_limits,
                &desc.memory_hints,
                Some(Box::new(
                    |args: hal::CreateDeviceCallbackArgs<'_, '_, '_>| {
                        for e in extensions() {
                            if !args.extensions.contains(&e) {
                                args.extensions.push(e);
                            }
                        }
                    },
                )),
            )
        }
        .ok()?
    };
    // SAFETY: `open` was created from this adapter with the features asked.
    unsafe { adapter.create_device_from_hal(open, desc) }.ok()
}

/// One pooled buffer: its memory exported as a dmabuf.
struct Slot {
    width: u32,
    height: u32,
    stride: u32,
    buffer: wgpu::Buffer,
    fd: OwnedFd,
    /// Not shown by the toolkit (any more).
    free: Arc<AtomicBool>,
}

/// Exports frames as dmabufs from a device opened by [`open_device`].
pub(crate) struct Exporter {
    device: ash::Device,
    fd_ext: ash::khr::external_memory_fd::Device,
    memory: vk::PhysicalDeviceMemoryProperties,
    slots: Vec<Slot>,
}

/// A frame in a dmabuf: one plane, `fd` valid until `release` is dropped
/// (the toolkit drops it when it no longer shows the frame).
pub struct DmabufFrame {
    pub width: u32,
    pub height: u32,
    pub fourcc: u32,
    pub modifier: u64,
    pub fd: RawFd,
    pub offset: u32,
    pub stride: u32,
    pub release: Release,
}

/// Gives a frame's buffer back to the pool when dropped.
pub struct Release(Arc<AtomicBool>);

impl Drop for Release {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Release);
    }
}

impl Exporter {
    /// For a device opened by [`open_device`] (`None` otherwise).
    pub(crate) fn new(device: &wgpu::Device) -> Option<Self> {
        // SAFETY: the guard is only used here; the handles cloned out of it
        // stay valid as long as the device, which outlives the exporter
        // (both live in the renderer, the exporter dropped first).
        let hal_device = unsafe { device.as_hal::<Vulkan>() }?;
        if !extensions()
            .iter()
            .all(|e| hal_device.enabled_device_extensions().contains(e))
        {
            return None;
        }
        let raw = hal_device.raw_device().clone();
        let instance = hal_device.shared_instance().raw_instance();
        // SAFETY: the physical device belongs to this instance.
        let memory = unsafe {
            instance.get_physical_device_memory_properties(hal_device.raw_physical_device())
        };
        let fd_ext = ash::khr::external_memory_fd::Device::new(instance, &raw);
        Some(Self {
            device: raw,
            fd_ext,
            memory,
            slots: Vec::new(),
        })
    }

    /// A free buffer for a `width × height` frame (rows of `stride`
    /// bytes): reused, or made while the pool has room. `None`: every
    /// buffer is still shown (the caller reads the frame back instead).
    pub(crate) fn slot(
        &mut self,
        device: &wgpu::Device,
        width: u32,
        height: u32,
        stride: u32,
    ) -> Option<(&wgpu::Buffer, DmabufFrame)> {
        // Buffers of another size go once they are free.
        self.slots.retain(|s| {
            (s.width == width && s.height == height) || !s.free.load(Ordering::Acquire)
        });
        let index =
            match self.slots.iter().position(|s| {
                s.width == width && s.height == height && s.free.load(Ordering::Acquire)
            }) {
                Some(i) => i,
                None if self.slots.len() < POOL => {
                    let slot = self.make(device, width, height, stride)?;
                    self.slots.push(slot);
                    self.slots.len() - 1
                }
                None => return None,
            };
        let s = &self.slots[index];
        s.free.store(false, Ordering::Release);
        let frame = DmabufFrame {
            width,
            height,
            fourcc: FOURCC_AB24,
            modifier: MODIFIER_LINEAR,
            fd: s.fd.as_raw_fd(),
            offset: 0,
            stride: s.stride,
            release: Release(Arc::clone(&s.free)),
        };
        Some((&s.buffer, frame))
    }

    /// The buffer behind a frame's descriptor (tests read it back).
    pub(crate) fn buffer_of(&self, fd: RawFd) -> Option<&wgpu::Buffer> {
        self.slots
            .iter()
            .find(|s| s.fd.as_raw_fd() == fd)
            .map(|s| &s.buffer)
    }

    fn memory_type(&self, bits: u32) -> Option<u32> {
        let types = &self.memory.memory_types[..self.memory.memory_type_count as usize];
        let fits = |i: usize| bits & (1 << i) != 0;
        // Device-local first (on an integrated GPU everything is).
        (0..types.len())
            .find(|&i| {
                fits(i)
                    && types[i]
                        .property_flags
                        .contains(vk::MemoryPropertyFlags::DEVICE_LOCAL)
            })
            .or_else(|| (0..types.len()).find(|&i| fits(i)))
            .map(|i| i as u32)
    }

    fn make(&self, device: &wgpu::Device, width: u32, height: u32, stride: u32) -> Option<Slot> {
        let size = u64::from(stride) * u64::from(height);
        let d = &self.device;
        let mut external = vk::ExternalMemoryBufferCreateInfo::default()
            .handle_types(vk::ExternalMemoryHandleTypeFlags::DMA_BUF_EXT);
        let info = vk::BufferCreateInfo::default()
            .size(size)
            .usage(vk::BufferUsageFlags::TRANSFER_DST | vk::BufferUsageFlags::TRANSFER_SRC)
            .sharing_mode(vk::SharingMode::EXCLUSIVE)
            .push_next(&mut external);
        // SAFETY: a valid create info for this device.
        let raw = unsafe { d.create_buffer(&info, None) }.ok()?;
        // SAFETY: `raw` is a buffer of this device.
        let req = unsafe { d.get_buffer_memory_requirements(raw) };
        let destroy_buffer = || {
            // SAFETY: not bound, not in use.
            unsafe { d.destroy_buffer(raw, None) }
        };
        let Some(kind) = self.memory_type(req.memory_type_bits) else {
            destroy_buffer();
            return None;
        };
        let mut export = vk::ExportMemoryAllocateInfo::default()
            .handle_types(vk::ExternalMemoryHandleTypeFlags::DMA_BUF_EXT);
        let mut dedicated = vk::MemoryDedicatedAllocateInfo::default().buffer(raw);
        let alloc = vk::MemoryAllocateInfo::default()
            .allocation_size(req.size)
            .memory_type_index(kind)
            .push_next(&mut export)
            .push_next(&mut dedicated);
        // SAFETY: a valid allocation for this device's memory type.
        let Ok(memory) = (unsafe { d.allocate_memory(&alloc, None) }) else {
            destroy_buffer();
            return None;
        };
        let free_all = || {
            // SAFETY: neither is in use by the GPU yet.
            unsafe {
                d.destroy_buffer(raw, None);
                d.free_memory(memory, None);
            }
        };
        // SAFETY: the memory was allocated for this buffer (dedicated).
        if unsafe { d.bind_buffer_memory(raw, memory, 0) }.is_err() {
            free_all();
            return None;
        }
        let get = vk::MemoryGetFdInfoKHR::default()
            .memory(memory)
            .handle_type(vk::ExternalMemoryHandleTypeFlags::DMA_BUF_EXT);
        // SAFETY: the memory was allocated exportable as a dma-buf.
        let fd = match unsafe { self.fd_ext.get_memory_fd(&get) } {
            Ok(fd) if fd >= 0 => fd,
            _ => {
                free_all();
                return None;
            }
        };
        // SAFETY: a fresh descriptor the driver handed over; we own it.
        let fd = unsafe { OwnedFd::from_raw_fd(fd) };
        // wgpu uses the buffer; it is destroyed (and its memory freed —
        // the dmabuf lives on while the toolkit holds it) when wgpu is
        // done with it.
        let owner = d.clone();
        let on_drop: wgpu::hal::DropCallback = Box::new(move || {
            // SAFETY: wgpu no longer uses the buffer.
            unsafe {
                owner.destroy_buffer(raw, None);
                owner.free_memory(memory, None);
            }
        });
        // SAFETY: the buffer and its memory outlive the wgpu buffer (they
        // are destroyed in its drop callback, after wgpu's last use).
        let hal_buffer = unsafe { hal::Buffer::from_raw_externally_owned(raw, on_drop) };
        // SAFETY: created on this device, with the usages it was made for.
        let buffer = unsafe {
            device.create_buffer_from_hal::<Vulkan>(
                hal_buffer,
                &wgpu::BufferDescriptor {
                    label: Some("frame dmabuf"),
                    size,
                    usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::COPY_SRC,
                    mapped_at_creation: false,
                },
            )
        };
        Some(Slot {
            width,
            height,
            stride,
            buffer,
            fd,
            free: Arc::new(AtomicBool::new(true)),
        })
    }
}
