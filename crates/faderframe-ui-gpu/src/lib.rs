//! A [`Painter`] on the GPU: vello (compute-shader 2D rendering on wgpu)
//! for views whose geometry is dense and changes every frame — analysers,
//! meters, curves — where GTK's renderer rasterises each changed path on
//! the CPU. A frame is rendered into a texture and read back as RGBA
//! pixels (straight alpha) for the toolkit to show; text is laid out with
//! parley in the same font families as the toolkit painter.
//!
//! GTK-free: the host turns [`GpuRenderer::render`]'s pixels into a
//! texture. Creating a renderer fails without a usable GPU adapter; the
//! host then keeps its own painter. On Linux, frames can skip the readback
//! altogether ([`GpuRenderer::render_to`] with dmabuf export, `dmabuf`).

#![deny(unsafe_code)]

#[cfg(target_os = "linux")]
#[allow(unsafe_code)]
mod dmabuf;
mod scene;
mod text;

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

/// What a frame became: pixels read back, or (Linux) a dmabuf the toolkit
/// imports.
pub enum Output {
    Pixels(Frame),
    #[cfg(target_os = "linux")]
    Dmabuf(DmabufFrame),
}

/// The GPU, vello and the text and image caches; one per UI thread.
pub struct GpuRenderer {
    /// Dmabuf export (dropped before the device it uses).
    #[cfg(target_os = "linux")]
    dmabuf: Option<dmabuf::Exporter>,
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
        let instance =
            wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
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
        let renderer = vello::Renderer::new(
            &device,
            vello::RendererOptions {
                use_cpu: false,
                antialiasing_support: vello::AaSupport::area_only(),
                num_init_threads: NonZeroUsize::new(1),
                pipeline_cache: None,
            },
        )
        .map_err(|e| GpuError::Render(e.to_string()))?;
        Ok(Self {
            #[cfg(target_os = "linux")]
            dmabuf,
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
        }
    }

    /// [`Self::render`], as a dmabuf when `dmabuf` (and a buffer is free:
    /// the toolkit may still show the others), else read back.
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
        #[cfg(not(target_os = "linux"))]
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
