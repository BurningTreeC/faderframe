//! Windows: the GPU painter's frames to GTK without a readback, as shared
//! D3D12 textures (`faderframe_ui_gpu::SharedFrame`). How depends on GTK's
//! renderer:
//!
//! * GL (GTK's default on Windows): GTK's own D3D12 import into GL imports
//!   the texture's handle as its fence too and fails (falling back to a
//!   copy through the CPU), so the texture is imported here instead — a
//!   WGL context of GTK's display (sharing objects with GTK's), each
//!   pooled texture once through `EXT_memory_object_win32` (dedicated,
//!   `GL_RGBA8`), handed over as a `GdkGLTexture`. Only when GL sits on
//!   the adapter the frames come from (`GL_DEVICE_LUID_EXT`).
//! * Vulkan: a `GdkD3D12Texture`, which GTK imports itself.
//!
//! The first frame of either is read back both ways and compared; any
//! difference or error and frames are read back from then on.

use faderframe_ui_gpu::SharedFrame;
use gtk::prelude::*;
use gtk::{gdk, glib};
use std::ffi::c_void;

/// How frames reach GTK.
pub(crate) enum Handover {
    Gl(GlImport),
    D3d12,
}

/// What GTK's renderer for `widget` is.
pub(crate) fn renderer_kind(widget: &gtk::Widget) -> Option<&'static str> {
    let renderer = widget.native()?.renderer()?;
    let name = renderer.type_().name();
    if name.contains("Vulkan") {
        Some("vulkan")
    } else if name.contains("GL") || name.contains("Ngl") {
        Some("gl")
    } else {
        None
    }
}

const GL_TEXTURE_2D: u32 = 0x0DE1;
const GL_RGBA8: u32 = 0x8058;
const GL_RGBA: u32 = 0x1908;
const GL_UNSIGNED_BYTE: u32 = 0x1401;
const GL_NUM_EXTENSIONS: u32 = 0x821D;
const GL_EXTENSIONS: u32 = 0x1F03;
const GL_HANDLE_TYPE_D3D12_RESOURCE_EXT: u32 = 0x958A;
const GL_DEDICATED_MEMORY_OBJECT_EXT: u32 = 0x9581;
const GL_DEVICE_LUID_EXT: u32 = 0x9599;
const GL_NO_ERROR: u32 = 0;

type CreateMemoryObjects = unsafe extern "system" fn(i32, *mut u32);
type DeleteMemoryObjects = unsafe extern "system" fn(i32, *const u32);
type MemoryObjectParameteriv = unsafe extern "system" fn(u32, u32, *const i32);
type ImportMemoryWin32Handle = unsafe extern "system" fn(u32, u64, u32, *mut c_void);
type TexStorageMem2d = unsafe extern "system" fn(u32, i32, u32, i32, i32, u32, u64);
type GetUnsignedBytev = unsafe extern "system" fn(u32, *mut u8);
type GetStringi = unsafe extern "system" fn(u32, u32) -> *const u8;

/// The GL entry points the import uses (from the current WGL context).
struct Fns {
    create_memory_objects: CreateMemoryObjects,
    delete_memory_objects: DeleteMemoryObjects,
    memory_object_parameteriv: MemoryObjectParameteriv,
    import_memory_win32_handle: ImportMemoryWin32Handle,
    tex_storage_mem_2d: TexStorageMem2d,
    get_unsigned_bytev: GetUnsignedBytev,
    get_stringi: GetStringi,
}

/// A WGL extension function by name (`None`: not there).
fn proc(name: &[u8]) -> Option<unsafe extern "system" fn() -> isize> {
    debug_assert_eq!(name.last(), Some(&0));
    // SAFETY: a null-terminated name; a WGL context is current.
    unsafe { windows_sys::Win32::Graphics::OpenGL::wglGetProcAddress(name.as_ptr()) }
}

macro_rules! load {
    ($name:literal, $ty:ty) => {{
        let f = proc(concat!($name, "\0").as_bytes()).ok_or(concat!("no ", $name))?;
        // SAFETY: the function GL names so, with the signature its
        // extension specifies.
        unsafe { std::mem::transmute::<unsafe extern "system" fn() -> isize, $ty>(f) }
    }};
}

impl Fns {
    fn load() -> Result<Fns, &'static str> {
        Ok(Fns {
            create_memory_objects: load!("glCreateMemoryObjectsEXT", CreateMemoryObjects),
            delete_memory_objects: load!("glDeleteMemoryObjectsEXT", DeleteMemoryObjects),
            memory_object_parameteriv: load!(
                "glMemoryObjectParameterivEXT",
                MemoryObjectParameteriv
            ),
            import_memory_win32_handle: load!(
                "glImportMemoryWin32HandleEXT",
                ImportMemoryWin32Handle
            ),
            tex_storage_mem_2d: load!("glTexStorageMem2DEXT", TexStorageMem2d),
            get_unsigned_bytev: load!("glGetUnsignedBytevEXT", GetUnsignedBytev),
            get_stringi: load!("glGetStringi", GetStringi),
        })
    }
}

/// A pooled texture imported into GL.
struct Imported {
    slot: u64,
    texture: u32,
    memory: u32,
    used: u64,
}

/// Shared textures imported into a GL context of GTK's.
pub(crate) struct GlImport {
    context: gdk::GLContext,
    fns: Fns,
    luid: [u8; 8],
    imported: Vec<Imported>,
    clock: u64,
}

/// Imported textures kept (the painter's pool, and sizes just left).
const KEEP: usize = 6;

impl GlImport {
    /// A WGL context of GTK's display with `EXT_memory_object_win32`.
    pub(crate) fn new() -> Result<GlImport, String> {
        let display = gdk::Display::default().ok_or("no display")?;
        let context = display.create_gl_context().map_err(|e| e.to_string())?;
        context.realize().map_err(|e| e.to_string())?;
        if !context.type_().name().contains("WGL") {
            return Err(format!("{} (not WGL)", context.type_().name()));
        }
        context.make_current();
        let fns = Fns::load()?;
        let mut count = 0i32;
        // SAFETY: the context is current; one integer is written.
        unsafe {
            windows_sys::Win32::Graphics::OpenGL::glGetIntegerv(GL_NUM_EXTENSIONS, &mut count)
        };
        let has = |want: &str| {
            (0..count.max(0) as u32).any(|i| {
                // SAFETY: an index below GL_NUM_EXTENSIONS; GL owns the string.
                let p = unsafe { (fns.get_stringi)(GL_EXTENSIONS, i) };
                !p.is_null()
                    // SAFETY: GL returns a null-terminated string.
                    && unsafe { std::ffi::CStr::from_ptr(p.cast()) }.to_bytes() == want.as_bytes()
            })
        };
        for ext in ["GL_EXT_memory_object", "GL_EXT_memory_object_win32"] {
            if !has(ext) {
                gdk::GLContext::clear_current();
                return Err(format!("no {ext}"));
            }
        }
        let mut luid = [0u8; 8];
        // SAFETY: GL_DEVICE_LUID_EXT writes GL_LUID_SIZE_EXT (8) bytes.
        unsafe { (fns.get_unsigned_bytev)(GL_DEVICE_LUID_EXT, luid.as_mut_ptr()) };
        gdk::GLContext::clear_current();
        Ok(GlImport {
            context,
            fns,
            luid,
            imported: Vec::new(),
            clock: 0,
        })
    }

    /// The adapter GL draws with.
    pub(crate) fn luid(&self) -> [u8; 8] {
        self.luid
    }

    /// The GL texture of `frame`'s pooled texture (imported the first
    /// time). The context is current.
    fn gl_texture(&mut self, frame: &SharedFrame) -> Result<u32, String> {
        use windows_sys::Win32::Graphics::OpenGL as gl;
        self.clock += 1;
        if let Some(i) = self.imported.iter_mut().find(|i| i.slot == frame.slot) {
            i.used = self.clock;
            return Ok(i.texture);
        }
        // Room for it: the least recently used goes (a pooled texture of
        // a size left behind; one still in use is imported again).
        if self.imported.len() >= KEEP
            && let Some(old) = (0..self.imported.len()).min_by_key(|&i| self.imported[i].used)
        {
            let i = self.imported.swap_remove(old);
            // SAFETY: objects of this context.
            unsafe {
                gl::glDeleteTextures(1, &i.texture);
                (self.fns.delete_memory_objects)(1, &i.memory);
            }
        }
        let (mut memory, mut texture) = (0u32, 0u32);
        let dedicated = 1i32;
        // SAFETY: the context is current; the handle is an NT handle to a
        // committed (dedicated) D3D12 texture of `frame.size` bytes on this
        // adapter, RGBA8, `width × height`, one mip; GL does not take the
        // handle over.
        let error = unsafe {
            (self.fns.create_memory_objects)(1, &mut memory);
            (self.fns.memory_object_parameteriv)(
                memory,
                GL_DEDICATED_MEMORY_OBJECT_EXT,
                &dedicated,
            );
            (self.fns.import_memory_win32_handle)(
                memory,
                frame.size,
                GL_HANDLE_TYPE_D3D12_RESOURCE_EXT,
                frame.handle as *mut c_void,
            );
            gl::glGenTextures(1, &mut texture);
            gl::glBindTexture(GL_TEXTURE_2D, texture);
            (self.fns.tex_storage_mem_2d)(
                GL_TEXTURE_2D,
                1,
                GL_RGBA8,
                frame.width as i32,
                frame.height as i32,
                memory,
                0,
            );
            gl::glBindTexture(GL_TEXTURE_2D, 0);
            gl::glGetError()
        };
        if error != GL_NO_ERROR {
            // SAFETY: objects of this context.
            unsafe {
                gl::glDeleteTextures(1, &texture);
                (self.fns.delete_memory_objects)(1, &memory);
            }
            return Err(format!("GL error {error:#x} importing the texture"));
        }
        self.imported.push(Imported {
            slot: frame.slot,
            texture,
            memory,
            used: self.clock,
        });
        Ok(texture)
    }

    /// `frame` as a GTK texture; its pooled texture goes back to the
    /// painter when GTK lets go of it.
    pub(crate) fn texture(&mut self, frame: SharedFrame) -> Result<gdk::Texture, String> {
        self.context.make_current();
        let id = self.gl_texture(&frame);
        // SAFETY: the context is current.
        unsafe { windows_sys::Win32::Graphics::OpenGL::glFlush() };
        gdk::GLContext::clear_current();
        let id = id?;
        let builder = gdk::GLTextureBuilder::new()
            .set_context(Some(&self.context))
            .set_id(id)
            .set_width(frame.width as i32)
            .set_height(frame.height as i32)
            .set_format(gdk::MemoryFormat::R8g8b8a8);
        // SAFETY: the texture lives in the import cache (and its D3D12
        // memory in the painter's pool) at least until GTK releases this
        // texture — the closure keeps the frame until then.
        Ok(unsafe { builder.build_with_release_func(move || drop(frame)) })
    }

    /// The pixels GL sees of `frame`'s texture (rows of `width * 4`
    /// bytes): the import's self-test.
    pub(crate) fn read(&mut self, frame: &SharedFrame) -> Result<Vec<u8>, String> {
        use windows_sys::Win32::Graphics::OpenGL as gl;
        self.context.make_current();
        let id = self.gl_texture(frame);
        let out = id.map(|id| {
            let mut pixels = vec![0u8; frame.width as usize * frame.height as usize * 4];
            // SAFETY: the context is current; the buffer holds the level.
            unsafe {
                gl::glBindTexture(GL_TEXTURE_2D, id);
                gl::glPixelStorei(gl::GL_PACK_ALIGNMENT, 1);
                gl::glGetTexImage(
                    GL_TEXTURE_2D,
                    0,
                    GL_RGBA,
                    GL_UNSIGNED_BYTE,
                    pixels.as_mut_ptr().cast(),
                );
                gl::glBindTexture(GL_TEXTURE_2D, 0);
            }
            pixels
        });
        gdk::GLContext::clear_current();
        out
    }
}

impl Drop for GlImport {
    fn drop(&mut self) {
        self.context.make_current();
        for i in self.imported.drain(..) {
            // SAFETY: objects of this context.
            unsafe {
                windows_sys::Win32::Graphics::OpenGL::glDeleteTextures(1, &i.texture);
                (self.fns.delete_memory_objects)(1, &i.memory);
            }
        }
        gdk::GLContext::clear_current();
    }
}

/// `frame` as a `GdkD3D12Texture` (GTK's Vulkan renderer imports it); the
/// frame goes back to the painter when GTK lets go of it.
pub(crate) fn d3d12_texture(frame: SharedFrame) -> Result<gdk::Texture, String> {
    use gdk4_win32::ffi;
    use glib::translate::from_glib_full;
    unsafe extern "C" fn release(data: glib::ffi::gpointer) {
        // SAFETY: `data` is the box made below, released once by GTK.
        drop(unsafe { Box::from_raw(data.cast::<SharedFrame>()) });
    }
    let resource = frame.resource_ptr();
    let data = Box::into_raw(Box::new(frame));
    // SAFETY: a fresh builder; the resource is a valid ID3D12Resource kept
    // alive by the boxed frame, which GTK hands to `release` when it no
    // longer uses the texture (or right away when it fails).
    unsafe {
        let builder = ffi::gdk_d3d12_texture_builder_new();
        ffi::gdk_d3d12_texture_builder_set_resource(builder, resource);
        ffi::gdk_d3d12_texture_builder_set_premultiplied(builder, glib::ffi::GFALSE);
        let mut error = std::ptr::null_mut();
        let texture =
            ffi::gdk_d3d12_texture_builder_build(builder, Some(release), data.cast(), &mut error);
        glib::gobject_ffi::g_object_unref(builder.cast());
        if texture.is_null() {
            drop(Box::from_raw(data));
            let e: glib::Error = from_glib_full(error);
            return Err(e.to_string());
        }
        Ok(from_glib_full(texture))
    }
}

/// GTK's view of a texture, row by row (the D3D12 hand-over's self-test).
pub(crate) fn download(texture: &gdk::Texture) -> Vec<u8> {
    let mut d = gdk::TextureDownloader::new(texture);
    d.set_format(gdk::MemoryFormat::R8g8b8a8);
    let (bytes, stride) = d.download_bytes();
    let row = texture.width() as usize * 4;
    (0..texture.height() as usize)
        .flat_map(|y| bytes[y * stride..y * stride + row].to_vec())
        .collect()
}

/// The first frames match what the painter read back: pixel values at
/// most one apart (formats may round).
pub(crate) fn same(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).all(|(x, y)| x.abs_diff(*y) <= 1)
}

/// Wanted at all (`FADERFRAME_GPU_SHARE=0` turns the hand-over off).
pub(crate) fn wanted() -> bool {
    std::env::var("FADERFRAME_GPU_SHARE").as_deref() != Ok("0")
}

/// Which kind of hand-over for GTK's renderer, and the adapter the painter
/// should render on.
pub(crate) fn handover(widget: &gtk::Widget) -> (Option<Handover>, Option<[u8; 8]>) {
    if !wanted() {
        return (None, None);
    }
    match renderer_kind(widget) {
        Some("gl") => match GlImport::new() {
            Ok(g) => {
                let luid = g.luid();
                (Some(Handover::Gl(g)), Some(luid))
            }
            Err(e) => {
                tracing::info!("no shared textures into GL: {e}");
                (None, None)
            }
        },
        Some("vulkan") => (Some(Handover::D3d12), None),
        _ => (None, None),
    }
}
