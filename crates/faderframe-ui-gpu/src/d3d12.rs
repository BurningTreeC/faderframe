//! Frames handed to the toolkit as shared D3D12 textures (Windows): no
//! copy through the CPU. Each frame is copied on the GPU into a pooled
//! texture made shareable (`D3D12_HEAP_FLAG_SHARED`, simultaneous access,
//! so it decays to the common state for the importer), with an NT handle
//! the toolkit's GL (`EXT_memory_object_win32`) or Vulkan side opens.
//! Textures are reused only after the toolkit has let go of them. The
//! importer must sit on the same adapter ([`Exporter::luid`]).

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use vello::wgpu;
use wgpu::hal::api::Dx12;
use windows::Win32::Foundation::{CloseHandle, GENERIC_ALL, HANDLE};
use windows::Win32::Graphics::Direct3D12 as d3d;
use windows::Win32::Graphics::Dxgi::Common as dxgi;

/// Textures kept for frames the toolkit may still show.
const POOL: usize = 3;

/// A frame in a shared texture, valid until `release` is dropped.
pub struct SharedFrame {
    pub width: u32,
    pub height: u32,
    /// The texture (a reference of the frame's own).
    pub resource: d3d::ID3D12Resource,
    /// An NT handle to it, open while the pool keeps the texture.
    pub handle: isize,
    /// The size of its memory (for importers that ask).
    pub size: u64,
    /// Which pooled texture (importers cache by it; a new one never
    /// reuses an id).
    pub slot: u64,
    pub release: Release,
}

impl SharedFrame {
    /// The `ID3D12Resource*` (for C APIs; valid while the frame lives).
    pub fn resource_ptr(&self) -> *mut std::ffi::c_void {
        windows::core::Interface::as_raw(&self.resource)
    }
}

/// Gives a frame's texture back to the pool when dropped.
pub struct Release(Arc<AtomicBool>);

impl Drop for Release {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Release);
    }
}

struct Slot {
    width: u32,
    height: u32,
    texture: wgpu::Texture,
    resource: d3d::ID3D12Resource,
    handle: HANDLE,
    size: u64,
    id: u64,
    free: Arc<AtomicBool>,
}

impl Drop for Slot {
    fn drop(&mut self) {
        // SAFETY: the handle was created for this slot and is closed once.
        let _ = unsafe { CloseHandle(self.handle) };
    }
}

/// Exports frames from wgpu's D3D12 device.
pub(crate) struct Exporter {
    device: d3d::ID3D12Device,
    luid: [u8; 8],
    slots: Vec<Slot>,
    next: u64,
}

impl Exporter {
    /// For a wgpu device on D3D12 (`None` on another backend).
    pub(crate) fn new(device: &wgpu::Device) -> Option<Self> {
        // SAFETY: the guard is only used here; the device interface cloned
        // out of it (a COM reference) keeps the device alive.
        let hal = unsafe { device.as_hal::<Dx12>() }?;
        let raw = hal.raw_device().clone();
        // SAFETY: a valid device.
        let luid = unsafe { raw.GetAdapterLuid() };
        let mut bytes = [0u8; 8];
        bytes[..4].copy_from_slice(&luid.LowPart.to_le_bytes());
        bytes[4..].copy_from_slice(&luid.HighPart.to_le_bytes());
        Some(Self {
            device: raw,
            luid: bytes,
            slots: Vec::new(),
            next: 1,
        })
    }

    /// The adapter's LUID as GL's `GL_DEVICE_LUID_EXT` reports one.
    pub(crate) fn luid(&self) -> [u8; 8] {
        self.luid
    }

    /// A free texture for a `width × height` frame: reused, or made while
    /// the pool has room. `None`: every texture is still shown (the caller
    /// reads the frame back).
    pub(crate) fn slot(
        &mut self,
        device: &wgpu::Device,
        width: u32,
        height: u32,
    ) -> Option<(&wgpu::Texture, SharedFrame)> {
        self.slots.retain(|s| {
            (s.width == width && s.height == height) || !s.free.load(Ordering::Acquire)
        });
        let index =
            match self.slots.iter().position(|s| {
                s.width == width && s.height == height && s.free.load(Ordering::Acquire)
            }) {
                Some(i) => i,
                None if self.slots.len() < POOL => {
                    let slot = self.make(device, width, height)?;
                    self.slots.push(slot);
                    self.slots.len() - 1
                }
                None => return None,
            };
        let s = &self.slots[index];
        s.free.store(false, Ordering::Release);
        let frame = SharedFrame {
            width,
            height,
            resource: s.resource.clone(),
            handle: s.handle.0 as isize,
            size: s.size,
            slot: s.id,
            release: Release(Arc::clone(&s.free)),
        };
        Some((&s.texture, frame))
    }

    /// The pooled texture of a frame (tests read it back).
    pub(crate) fn texture_of(&self, slot: u64) -> Option<&wgpu::Texture> {
        self.slots.iter().find(|s| s.id == slot).map(|s| &s.texture)
    }

    fn make(&mut self, device: &wgpu::Device, width: u32, height: u32) -> Option<Slot> {
        let desc = d3d::D3D12_RESOURCE_DESC {
            Dimension: d3d::D3D12_RESOURCE_DIMENSION_TEXTURE2D,
            Alignment: 0,
            Width: u64::from(width),
            Height: height,
            DepthOrArraySize: 1,
            MipLevels: 1,
            Format: dxgi::DXGI_FORMAT_R8G8B8A8_UNORM,
            SampleDesc: dxgi::DXGI_SAMPLE_DESC {
                Count: 1,
                Quality: 0,
            },
            Layout: d3d::D3D12_TEXTURE_LAYOUT_UNKNOWN,
            Flags: d3d::D3D12_RESOURCE_FLAG_ALLOW_SIMULTANEOUS_ACCESS
                | d3d::D3D12_RESOURCE_FLAG_ALLOW_RENDER_TARGET,
        };
        let heap = d3d::D3D12_HEAP_PROPERTIES {
            Type: d3d::D3D12_HEAP_TYPE_DEFAULT,
            CPUPageProperty: d3d::D3D12_CPU_PAGE_PROPERTY_UNKNOWN,
            MemoryPoolPreference: d3d::D3D12_MEMORY_POOL_UNKNOWN,
            CreationNodeMask: 0,
            VisibleNodeMask: 0,
        };
        let mut made: Option<d3d::ID3D12Resource> = None;
        // SAFETY: valid descriptions for this device; the result is written
        // to `made`.
        unsafe {
            self.device.CreateCommittedResource(
                &heap,
                d3d::D3D12_HEAP_FLAG_SHARED,
                &desc,
                d3d::D3D12_RESOURCE_STATE_COMMON,
                None,
                &mut made,
            )
        }
        .ok()?;
        let resource = made?;
        // SAFETY: `resource` is a shared resource of this device.
        let handle = unsafe {
            self.device
                .CreateSharedHandle(&resource, None, GENERIC_ALL.0, None)
        }
        .ok()?;
        // SAFETY: the same description.
        let size = unsafe { self.device.GetResourceAllocationInfo(0, &[desc]) }.SizeInBytes;
        let extent = wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        };
        // SAFETY: the resource matches the description (a 2D RGBA8 texture
        // of one mip and sample).
        let hal_texture = unsafe {
            wgpu::hal::dx12::Device::texture_from_raw(
                resource.clone(),
                wgpu::TextureFormat::Rgba8Unorm,
                wgpu::TextureDimension::D2,
                extent,
                1,
                1,
            )
        };
        // SAFETY: created on this device, used as declared.
        let texture = unsafe {
            device.create_texture_from_hal::<Dx12>(
                hal_texture,
                &wgpu::TextureDescriptor {
                    label: Some("shared frame"),
                    size: extent,
                    mip_level_count: 1,
                    sample_count: 1,
                    dimension: wgpu::TextureDimension::D2,
                    format: wgpu::TextureFormat::Rgba8Unorm,
                    usage: wgpu::TextureUsages::COPY_DST | wgpu::TextureUsages::COPY_SRC,
                    view_formats: &[],
                },
                // New (every frame overwrites it all).
                wgpu::wgt::TextureUses::UNINITIALIZED,
            )
        };
        let id = self.next;
        self.next += 1;
        Some(Slot {
            width,
            height,
            texture,
            resource,
            handle,
            size,
            id,
            free: Arc::new(AtomicBool::new(true)),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Without vello: a pattern copied into a shared texture reads back
    /// the same, and its NT handle opens on a second D3D12 device (as the
    /// toolkit opens it) as the same texture.
    #[test]
    fn a_shared_texture_carries_the_frame_to_another_device() {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
            backends: wgpu::Backends::DX12,
            ..wgpu::InstanceDescriptor::new_without_display_handle_from_env()
        });
        let Some(adapter) = pollster::block_on(instance.enumerate_adapters(wgpu::Backends::DX12))
            .into_iter()
            .next()
        else {
            eprintln!("skipped: no D3D12 adapter");
            return;
        };
        let (device, queue) =
            pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default())).unwrap();
        let mut exporter = Exporter::new(&device).expect("a D3D12 device");
        let (w, h) = (40u32, 30u32);
        let pattern: Vec<u8> = (0..w * h * 4).map(|i| (i * 7 % 251) as u8).collect();
        let source = device.create_texture(&wgpu::TextureDescriptor {
            label: None,
            size: wgpu::Extent3d {
                width: w,
                height: h,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::COPY_DST | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        queue.write_texture(
            source.as_image_copy(),
            &pattern,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(w * 4),
                rows_per_image: None,
            },
            source.size(),
        );
        let Some((texture, frame)) = exporter.slot(&device, w, h) else {
            eprintln!("skipped: no shared textures on this D3D12");
            return;
        };
        let mut enc = device.create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
        enc.copy_texture_to_texture(
            source.as_image_copy(),
            texture.as_image_copy(),
            source.size(),
        );
        let staging = device.create_buffer(&wgpu::BufferDescriptor {
            label: None,
            size: u64::from(256 * h),
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        enc.copy_texture_to_buffer(
            texture.as_image_copy(),
            wgpu::TexelCopyBufferInfo {
                buffer: &staging,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(256),
                    rows_per_image: None,
                },
            },
            source.size(),
        );
        queue.submit([enc.finish()]);
        staging.slice(..).map_async(wgpu::MapMode::Read, |_| {});
        device.poll(wgpu::PollType::wait_indefinitely()).unwrap();
        let mapped = staging.slice(..).get_mapped_range().unwrap();
        for y in 0..h as usize {
            let row = w as usize * 4;
            assert_eq!(
                &mapped[y * 256..y * 256 + row],
                &pattern[y * row..(y + 1) * row],
                "row {y}"
            );
        }
        drop(mapped);
        // The handle, on a device of its own.
        // SAFETY: the guard is only used here; the adapter interface is a
        // COM reference of its own.
        let raw_adapter = unsafe { adapter.as_hal::<Dx12>() }
            .map(|a| a.raw_adapter().clone())
            .unwrap();
        let mut other: Option<d3d::ID3D12Device> = None;
        // SAFETY: a valid adapter; the device is written to `other`.
        unsafe {
            d3d::D3D12CreateDevice(
                &*raw_adapter,
                windows::Win32::Graphics::Direct3D::D3D_FEATURE_LEVEL_11_0,
                &mut other,
            )
        }
        .unwrap();
        let other = other.unwrap();
        let mut opened: Option<d3d::ID3D12Resource> = None;
        // SAFETY: an NT handle to a shared resource on the same adapter;
        // the resource is written to `opened`.
        let result = unsafe { other.OpenSharedHandle(HANDLE(frame.handle as *mut _), &mut opened) };
        match result.map(|()| opened) {
            Ok(Some(r)) => {
                // SAFETY: a valid resource.
                let desc = unsafe { r.GetDesc() };
                assert_eq!((desc.Width, desc.Height), (u64::from(w), h));
                assert_eq!(desc.Format, dxgi::DXGI_FORMAT_R8G8B8A8_UNORM);
            }
            Ok(None) => panic!("the shared handle opened nothing"),
            Err(e) => panic!("the shared handle does not open: {e}"),
        }
        assert_ne!(exporter.luid(), [0; 8]);
    }
}
