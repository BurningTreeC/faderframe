//! A [`Painter`] on the GPU: vello (compute-shader 2D rendering on wgpu)
//! for views whose geometry is dense and changes every frame — analysers,
//! meters, curves — where GTK's renderer rasterises each changed path on
//! the CPU. A frame is rendered into a texture and read back as RGBA
//! pixels (straight alpha) for the toolkit to show; text is laid out with
//! parley in the same font families as the toolkit painter.
//!
//! GTK-free: the host turns [`GpuRenderer::render`]'s pixels into a
//! texture. Creating a renderer fails without a usable GPU adapter; the
//! host then keeps its own painter. On Linux and Windows, frames can skip
//! the readback altogether ([`GpuRenderer::render_to`]: dmabufs, `dmabuf`;
//! shared D3D12 textures, `d3d12`).

#![deny(unsafe_code)]

#[cfg(windows)]
#[allow(unsafe_code)]
mod d3d12;
#[cfg(target_os = "linux")]
#[allow(unsafe_code)]
mod dmabuf;
mod scene;
mod text;

#[cfg(windows)]
pub use d3d12::SharedFrame;
#[cfg(target_os = "linux")]
pub use dmabuf::{DmabufFrame, FOURCC_AB24, MODIFIER_LINEAR, Release};

use faderframe_ui_canvas::Painter;
use std::num::NonZeroUsize;
use vello::wgpu;

pub use scene::ScenePainter;

#[derive(Debug, thiserror::Error)]
pub enum GpuError {
    #[error("no GPU adapter: {0}")]
    Adapter(String),
    /// Only a software rasteriser (WARP, llvmpipe, lavapipe): the toolkit
    /// draws faster on the CPU than vello there, and Windows' WARP crashed
    /// on shaders FXC had compiled.
    #[error("{0} is a software renderer: the toolkit draws instead")]
    Software(String),
    #[error("GPU device: {0}")]
    Device(String),
    #[error("vello: {0}")]
    Render(String),
    #[error("reading the frame back: {0}")]
    Readback(String),
    #[error("frame too large ({0}×{1})")]
    Size(u32, u32),
}

/// A rendered frame: `width × height` RGBA pixels with straight
/// (unpremultiplied) alpha, rows of `width * 4` bytes.
pub struct Frame {
    pub width: u32,
    pub height: u32,
    pub pixels: Pixels,
}

/// A frame's pixels. Dropped (when the toolkit is done showing them), the
/// buffer goes back to its renderer for a later frame: a fresh allocation
/// of a large frame every time costs its page faults again.
pub struct Pixels {
    bytes: Vec<u8>,
    pool: std::sync::Weak<std::sync::Mutex<Vec<Vec<u8>>>>,
}

impl AsRef<[u8]> for Pixels {
    fn as_ref(&self) -> &[u8] {
        &self.bytes
    }
}

impl Drop for Pixels {
    fn drop(&mut self) {
        if let Some(pool) = self.pool.upgrade()
            && let Ok(mut pool) = pool.lock()
            && pool.len() < 4
        {
            pool.push(std::mem::take(&mut self.bytes));
        }
    }
}

impl Frame {
    pub fn stride(&self) -> usize {
        self.width as usize * 4
    }
}

/// The target texture and readback buffer of one frame size.
struct Target {
    width: u32,
    height: u32,
    texture: wgpu::Texture,
    view: wgpu::TextureView,
    buffer: wgpu::Buffer,
    /// Bytes per row in `buffer` (padded to wgpu's copy alignment).
    padded_row: u32,
}

/// What a frame became: pixels read back, or a dmabuf (Linux) or a shared
/// D3D12 texture (Windows) the toolkit imports.
pub enum Output {
    Pixels(Frame),
    #[cfg(target_os = "linux")]
    Dmabuf(DmabufFrame),
    #[cfg(windows)]
    Shared(SharedFrame),
}

/// The GPU, vello and the text and image caches; one per UI thread.
pub struct GpuRenderer {
    /// Dmabuf export (dropped before the device it uses).
    #[cfg(target_os = "linux")]
    dmabuf: Option<dmabuf::Exporter>,
    /// Shared texture export (dropped before the device it uses).
    #[cfg(windows)]
    shared: Option<d3d12::Exporter>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    renderer: vello::Renderer,
    scene: vello::Scene,
    target: Option<Target>,
    text: text::TextSystem,
    images: scene::ImageCache,
    adapter: String,
    /// Frame buffers the toolkit gave back.
    pool: std::sync::Arc<std::sync::Mutex<Vec<Vec<u8>>>>,
}

impl GpuRenderer {
    /// Find a GPU and build vello's pipelines (blocks for a moment: shader
    /// compilation).
    pub fn new() -> Result<Self, GpuError> {
        Self::new_on(None)
    }

    /// [`Self::new`], software rasterisers too (tests on machines without a
    /// GPU; `FADERFRAME_GPU_SOFTWARE=1` allows them in [`Self::new_on`]).
    pub fn new_any() -> Result<Self, GpuError> {
        Self::build(None, true)
    }

    /// [`Self::new`] on the adapter with this LUID where there is one (on
    /// Windows: the D3D12 adapter the toolkit's GL draws with, so it can
    /// import the frames). When vello cannot be built there, wgpu's own
    /// choice of adapter is used (frames read back).
    pub fn new_on(luid: Option<[u8; 8]>) -> Result<Self, GpuError> {
        let software = std::env::var("FADERFRAME_GPU_SOFTWARE").as_deref() == Ok("1");
        Self::build(luid, software)
    }

    fn build(luid: Option<[u8; 8]>, software: bool) -> Result<Self, GpuError> {
        let refuse = |a: &wgpu::Adapter| {
            let info = a.get_info();
            (!software && info.device_type == wgpu::DeviceType::Cpu)
                .then(|| GpuError::Software(format!("{} ({:?})", info.name, info.backend)))
        };
        #[cfg_attr(not(windows), allow(unused_mut))]
        let mut instance = instance(Instances::All);
        #[cfg(windows)]
        if let Some(adapter) = d3d12_adapter(instance, luid) {
            if let Some(e) = refuse(&adapter) {
                return Err(e);
            }
            match Self::on_adapter(adapter) {
                Ok(r) => return Ok(r),
                Err(e) => {
                    tracing::warn!("GPU painter on D3D12 ({e}): trying another backend");
                    instance = self::instance(Instances::WithoutDx12);
                }
            }
        }
        #[cfg(not(windows))]
        let _ = luid;
        let adapter =
            pollster::block_on(
                instance.request_adapter(&wgpu::RequestAdapterOptions {
                    power_preference: wgpu::PowerPreference::from_env()
                        .unwrap_or(wgpu::PowerPreference::HighPerformance),
                    force_fallback_adapter: false,
                    compatible_surface: None,
                    apply_limit_buckets: false,
                }),
            )
            .map_err(|e| GpuError::Adapter(e.to_string()))?;
        if let Some(e) = refuse(&adapter) {
            return Err(e);
        }
        Self::on_adapter(adapter)
    }

    fn on_adapter(adapter: wgpu::Adapter) -> Result<Self, GpuError> {
        let info = adapter.get_info();
        let desc = wgpu::DeviceDescriptor {
            label: Some("faderframe-ui-gpu"),
            required_features: wgpu::Features::empty(),
            required_limits: adapter.limits(),
            ..Default::default()
        };
        // With dmabuf export where the GPU has it (Vulkan on Linux).
        #[cfg(target_os = "linux")]
        let opened = if std::env::var("FADERFRAME_GPU_DMABUF").as_deref() == Ok("0") {
            None
        } else {
            dmabuf::open_device(&adapter, &desc)
        };
        #[cfg(not(target_os = "linux"))]
        let opened = None;
        let (device, queue) = match opened {
            Some(d) => d,
            None => pollster::block_on(adapter.request_device(&desc))
                .map_err(|e| GpuError::Device(e.to_string()))?,
        };
        #[cfg(target_os = "linux")]
        let dmabuf = dmabuf::Exporter::new(&device);
        #[cfg(windows)]
        let shared = if std::env::var("FADERFRAME_GPU_SHARE").as_deref() == Ok("0") {
            None
        } else {
            d3d12::Exporter::new(&device)
        };
        // Shaders the backend's compiler refuses (internal errors: FXC) or
        // that do not validate are an error, not a panic.
        let invalid = device.push_error_scope(wgpu::ErrorFilter::Validation);
        let internal = device.push_error_scope(wgpu::ErrorFilter::Internal);
        let renderer = vello::Renderer::new(
            &device,
            vello::RendererOptions {
                use_cpu: false,
                antialiasing_support: vello::AaSupport::area_only(),
                num_init_threads: NonZeroUsize::new(1),
                pipeline_cache: None,
            },
        );
        let failed = pollster::block_on(internal.pop());
        let failed = pollster::block_on(invalid.pop()).or(failed);
        if let Some(e) = failed {
            return Err(GpuError::Render(describe(&e)));
        }
        let renderer = renderer.map_err(|e| GpuError::Render(e.to_string()))?;
        Ok(Self {
            #[cfg(target_os = "linux")]
            dmabuf,
            #[cfg(windows)]
            shared,
            device,
            queue,
            renderer,
            scene: vello::Scene::new(),
            target: None,
            text: text::TextSystem::new(),
            images: scene::ImageCache::default(),
            adapter: format!("{} ({:?})", info.name, info.backend),
            pool: Default::default(),
        })
    }

    /// Which GPU renders (for logs).
    pub fn adapter(&self) -> &str {
        &self.adapter
    }

    /// Whether frames can go out as shared D3D12 textures, and the LUID of
    /// the adapter they are on.
    pub fn shared_luid(&self) -> Option<[u8; 8]> {
        #[cfg(windows)]
        return self.shared.as_ref().map(d3d12::Exporter::luid);
        #[cfg(not(windows))]
        None
    }

    /// Whether frames can go out as dmabufs.
    pub fn exports_dmabufs(&self) -> bool {
        #[cfg(target_os = "linux")]
        return self.dmabuf.is_some();
        #[cfg(not(target_os = "linux"))]
        false
    }

    /// Render a frame of `width × height` device pixels: `paint` draws in
    /// logical pixels, `scale` device pixels each.
    pub fn render(
        &mut self,
        width: u32,
        height: u32,
        scale: f32,
        paint: impl FnOnce(&mut dyn Painter),
    ) -> Result<Frame, GpuError> {
        match self.render_to(width, height, scale, false, paint)? {
            Output::Pixels(frame) => Ok(frame),
            #[cfg(target_os = "linux")]
            Output::Dmabuf(_) => Err(GpuError::Readback("a dmabuf was not asked for".into())),
            #[cfg(windows)]
            Output::Shared(_) => Err(GpuError::Readback(
                "a shared texture was not asked for".into(),
            )),
        }
    }

    /// [`Self::render`], handed over without a readback when `dmabuf` — a
    /// dmabuf (Linux) or a shared texture (Windows), when one is free (the
    /// toolkit may still show the others) — else read back.
    pub fn render_to(
        &mut self,
        width: u32,
        height: u32,
        scale: f32,
        dmabuf: bool,
        paint: impl FnOnce(&mut dyn Painter),
    ) -> Result<Output, GpuError> {
        let max = self.device.limits().max_texture_dimension_2d;
        if width == 0 || height == 0 || width > max || height > max {
            return Err(GpuError::Size(width, height));
        }
        self.text.begin_frame();
        self.scene.reset();
        {
            let mut painter =
                ScenePainter::new(&mut self.scene, &mut self.text, &mut self.images, scale);
            paint(&mut painter);
            painter.finish();
        }
        self.ensure_target(width, height);
        let target = self.target.as_ref().ok_or(GpuError::Size(width, height))?;
        self.renderer
            .render_to_texture(
                &self.device,
                &self.queue,
                &self.scene,
                &target.view,
                &vello::RenderParams {
                    base_color: vello::peniko::Color::TRANSPARENT,
                    width,
                    height,
                    antialiasing_method: vello::AaConfig::Area,
                },
            )
            .map_err(|e| GpuError::Render(e.to_string()))?;
        #[cfg(target_os = "linux")]
        if dmabuf
            && let Some(exporter) = self.dmabuf.as_mut()
            && let Some((buffer, frame)) =
                exporter.slot(&self.device, width, height, target.padded_row)
        {
            let mut encoder = self
                .device
                .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                    label: Some("to dmabuf"),
                });
            encoder.copy_texture_to_buffer(
                target.texture.as_image_copy(),
                wgpu::TexelCopyBufferInfo {
                    buffer,
                    layout: wgpu::TexelCopyBufferLayout {
                        offset: 0,
                        bytes_per_row: Some(target.padded_row),
                        rows_per_image: None,
                    },
                },
                wgpu::Extent3d {
                    width,
                    height,
                    depth_or_array_layers: 1,
                },
            );
            self.queue.submit([encoder.finish()]);
            // Done before the toolkit reads it (no fences cross over).
            self.device
                .poll(wgpu::PollType::wait_indefinitely())
                .map_err(|e| GpuError::Readback(e.to_string()))?;
            return Ok(Output::Dmabuf(frame));
        }
        #[cfg(windows)]
        if dmabuf
            && let Some(exporter) = self.shared.as_mut()
            && let Some((texture, frame)) = exporter.slot(&self.device, width, height)
        {
            let mut encoder = self
                .device
                .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                    label: Some("to shared texture"),
                });
            encoder.copy_texture_to_texture(
                target.texture.as_image_copy(),
                texture.as_image_copy(),
                wgpu::Extent3d {
                    width,
                    height,
                    depth_or_array_layers: 1,
                },
            );
            self.queue.submit([encoder.finish()]);
            // Done (and decayed to the common state) before the toolkit
            // reads it: no fences cross over.
            self.device
                .poll(wgpu::PollType::wait_indefinitely())
                .map_err(|e| GpuError::Readback(e.to_string()))?;
            return Ok(Output::Shared(frame));
        }
        #[cfg(not(any(target_os = "linux", windows)))]
        let _ = dmabuf;
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("readback"),
            });
        encoder.copy_texture_to_buffer(
            target.texture.as_image_copy(),
            wgpu::TexelCopyBufferInfo {
                buffer: &target.buffer,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(target.padded_row),
                    rows_per_image: None,
                },
            },
            wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
        );
        self.queue.submit([encoder.finish()]);
        let slice = target.buffer.slice(..);
        let (tx, rx) = std::sync::mpsc::channel();
        slice.map_async(wgpu::MapMode::Read, move |r| {
            let _ = tx.send(r);
        });
        self.device
            .poll(wgpu::PollType::wait_indefinitely())
            .map_err(|e| GpuError::Readback(e.to_string()))?;
        rx.recv()
            .map_err(|e| GpuError::Readback(e.to_string()))?
            .map_err(|e| GpuError::Readback(e.to_string()))?;
        let row = width as usize * 4;
        let size = row * height as usize;
        let mut pixels = self
            .pool
            .lock()
            .ok()
            .and_then(|mut p| {
                let i = p.iter().position(|b| b.capacity() >= size)?;
                Some(p.swap_remove(i))
            })
            .unwrap_or_else(|| Vec::with_capacity(size));
        pixels.clear();
        {
            let mapped = slice
                .get_mapped_range()
                .map_err(|e| GpuError::Readback(e.to_string()))?;
            for y in 0..height as usize {
                let start = y * target.padded_row as usize;
                pixels.extend_from_slice(&mapped[start..start + row]);
            }
        }
        target.buffer.unmap();

        Ok(Output::Pixels(Frame {
            width,
            height,
            pixels: Pixels {
                bytes: pixels,
                pool: std::sync::Arc::downgrade(&self.pool),
            },
        }))
    }

    /// A dmabuf frame's rows as they are in its buffer (each `stride`
    /// bytes): what the toolkit reads, copied back for tests.
    #[cfg(target_os = "linux")]
    #[doc(hidden)]
    pub fn read_dmabuf(&self, frame: &DmabufFrame) -> Result<Vec<u8>, GpuError> {
        let source = self
            .dmabuf
            .as_ref()
            .and_then(|e| e.buffer_of(frame.fd))
            .ok_or_else(|| GpuError::Readback("no such dmabuf".into()))?;
        let size = u64::from(frame.stride) * u64::from(frame.height);
        let staging = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("dmabuf check"),
            size,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
        encoder.copy_buffer_to_buffer(source, 0, &staging, 0, size);
        self.queue.submit([encoder.finish()]);
        let slice = staging.slice(..);
        let (tx, rx) = std::sync::mpsc::channel();
        slice.map_async(wgpu::MapMode::Read, move |r| {
            let _ = tx.send(r);
        });
        self.device
            .poll(wgpu::PollType::wait_indefinitely())
            .map_err(|e| GpuError::Readback(e.to_string()))?;
        rx.recv()
            .map_err(|e| GpuError::Readback(e.to_string()))?
            .map_err(|e| GpuError::Readback(e.to_string()))?;
        let bytes = slice
            .get_mapped_range()
            .map_err(|e| GpuError::Readback(e.to_string()))?
            .to_vec();
        Ok(bytes)
    }

    /// A shared frame's pixels (rows of `width * 4` bytes): what the
    /// toolkit reads, copied back for checks.
    #[cfg(windows)]
    #[doc(hidden)]
    pub fn read_shared(&self, frame: &SharedFrame) -> Result<Vec<u8>, GpuError> {
        let texture = self
            .shared
            .as_ref()
            .and_then(|e| e.texture_of(frame.slot))
            .ok_or_else(|| GpuError::Readback("no such shared texture".into()))?;
        let align = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
        let padded = (frame.width * 4).div_ceil(align) * align;
        let staging = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("shared check"),
            size: u64::from(padded) * u64::from(frame.height),
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
        encoder.copy_texture_to_buffer(
            texture.as_image_copy(),
            wgpu::TexelCopyBufferInfo {
                buffer: &staging,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(padded),
                    rows_per_image: None,
                },
            },
            wgpu::Extent3d {
                width: frame.width,
                height: frame.height,
                depth_or_array_layers: 1,
            },
        );
        self.queue.submit([encoder.finish()]);
        let slice = staging.slice(..);
        let (tx, rx) = std::sync::mpsc::channel();
        slice.map_async(wgpu::MapMode::Read, move |r| {
            let _ = tx.send(r);
        });
        self.device
            .poll(wgpu::PollType::wait_indefinitely())
            .map_err(|e| GpuError::Readback(e.to_string()))?;
        rx.recv()
            .map_err(|e| GpuError::Readback(e.to_string()))?
            .map_err(|e| GpuError::Readback(e.to_string()))?;
        let mapped = slice
            .get_mapped_range()
            .map_err(|e| GpuError::Readback(e.to_string()))?;
        let row = frame.width as usize * 4;
        let mut out = Vec::with_capacity(row * frame.height as usize);
        for y in 0..frame.height as usize {
            let start = y * padded as usize;
            out.extend_from_slice(&mapped[start..start + row]);
        }
        Ok(out)
    }

    fn ensure_target(&mut self, width: u32, height: u32) {
        if self
            .target
            .as_ref()
            .is_none_or(|t| t.width != width || t.height != height)
        {
            let texture = self.device.create_texture(&wgpu::TextureDescriptor {
                label: Some("frame"),
                size: wgpu::Extent3d {
                    width,
                    height,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Rgba8Unorm,
                usage: wgpu::TextureUsages::STORAGE_BINDING | wgpu::TextureUsages::COPY_SRC,
                view_formats: &[],
            });
            let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
            let align = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
            let padded_row = (width * 4).div_ceil(align) * align;
            let buffer = self.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("frame readback"),
                size: u64::from(padded_row) * u64::from(height),
                usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
                mapped_at_creation: false,
            });
            self.target = Some(Target {
                width,
                height,
                texture,
                view,
                buffer,
                padded_row,
            });
        }
    }
}

/// A wgpu error with its description (its display is only the kind).
fn describe(e: &wgpu::Error) -> String {
    match e {
        wgpu::Error::Validation { description, .. } | wgpu::Error::Internal { description, .. } => {
            description.clone()
        }
        other => other.to_string(),
    }
}

/// Which of the process's instances.
#[derive(Clone, Copy)]
enum Instances {
    All,
    /// For a renderer that could not be built on D3D12.
    #[cfg_attr(not(windows), allow(dead_code))]
    WithoutDx12,
}

/// The process's wgpu instance (one for every renderer, as wgpu would
/// have it), made once and never dropped. Dropping the last one unloads the
/// graphics DLLs, and on Windows' software rasteriser (WARP) its threads
/// can still be winding down then: the GPU tests, which make and drop a
/// renderer each, died with 0xc0000005 between two of them.
fn instance(which: Instances) -> &'static wgpu::Instance {
    static ALL: std::sync::OnceLock<wgpu::Instance> = std::sync::OnceLock::new();
    static WITHOUT_DX12: std::sync::OnceLock<wgpu::Instance> = std::sync::OnceLock::new();
    let fresh = wgpu::InstanceDescriptor::new_without_display_handle_from_env;
    match which {
        Instances::All => ALL.get_or_init(|| wgpu::Instance::new(fresh())),
        Instances::WithoutDx12 => WITHOUT_DX12.get_or_init(|| {
            let mut desc = fresh();
            desc.backends.remove(wgpu::Backends::DX12);
            wgpu::Instance::new(desc)
        }),
    }
}

/// The D3D12 adapter with `luid` (or, without one, the system's preferred
/// D3D12 adapter): frames shared with the toolkit must stay on its GPU.
/// `None`: keep wgpu's own choice.
#[cfg(windows)]
#[allow(unsafe_code)]
fn d3d12_adapter(instance: &wgpu::Instance, luid: Option<[u8; 8]>) -> Option<wgpu::Adapter> {
    if std::env::var("FADERFRAME_GPU_SHARE").as_deref() == Ok("0") {
        return None;
    }
    let adapters = pollster::block_on(instance.enumerate_adapters(wgpu::Backends::DX12));
    let luid_of = |a: &wgpu::Adapter| -> Option<[u8; 8]> {
        // SAFETY: the guard is only used here, while `a` lives.
        let hal = unsafe { a.as_hal::<wgpu::hal::api::Dx12>() }?;
        // SAFETY: a valid adapter.
        let desc = unsafe { hal.raw_adapter().GetDesc1() }.ok()?;
        let mut b = [0u8; 8];
        b[..4].copy_from_slice(&desc.AdapterLuid.LowPart.to_le_bytes());
        b[4..].copy_from_slice(&desc.AdapterLuid.HighPart.to_le_bytes());
        Some(b)
    };
    match luid {
        Some(want) => adapters.into_iter().find(|a| luid_of(a) == Some(want)),
        None => adapters
            .into_iter()
            .find(|a| a.get_info().device_type != wgpu::DeviceType::Cpu),
    }
}
