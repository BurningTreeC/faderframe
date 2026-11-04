//! Windows: video frames in D3D12 textures (`faderframe_video::zero_copy`)
//! to GTK without coming back through memory.
//!
//! UNTESTED: nothing that built this could run Direct3D 12 sharing. It
//! follows GStreamer 1.26's d3d12 library and GTK 4.20's `GdkD3D12Texture`
//! as documented, the GPU painter's hand-over (`gpu_win32`) where it can,
//! and checks itself on the first frame.
//!
//! * The texture comes from GStreamer's d3d12 library, looked up where the
//!   running GStreamer has it (its d3d12 plugin loads it; without it, or
//!   before 1.26, frames come through memory): `gst_d3d12_memory_sync`
//!   waits on the CPU for the converter's work on it — the frames are
//!   decoded ahead, so this is a check, and GTK needs no fence — and
//!   `gst_d3d12_memory_get_resource_handle` gives the `ID3D12Resource`.
//! * GL (GTK's default renderer here): imported into a GL context of GTK's
//!   display through `EXT_memory_object_win32`, as the painter's frames are
//!   (`gpu_win32::GlImport`): the NT handle from
//!   `gst_d3d12_memory_get_nt_handle` (made once per pooled texture, owned
//!   by it), the allocation's size from the texture's device. Each pooled
//!   texture is imported once, known by an id kept on GStreamer's memory
//!   (`gst_d3d12_memory_set_token_data`). Only when GL draws on the adapter
//!   GStreamer decodes on.
//! * Vulkan: a `GdkD3D12Texture`, which GTK imports itself (the converter's
//!   pool shares its textures' heap, which that import needs).
//!
//! The first frame is read back both ways — GStreamer's copy through memory
//! and what GL or GTK sees — and compared; any difference or error, and
//! frames come through memory from then on (`zero_copy::failed`).

use crate::gpu_win32::{GlImport, Shared, d3d12_texture_keeping, download, renderer_kind, same};
use faderframe_video::zero_copy::GpuFrame;
use gtk::gdk;
use std::cell::{Cell, RefCell};
use std::ffi::c_void;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, Ordering};
use windows::Win32::Graphics::Direct3D12::{
    D3D12_RESOURCE_DIMENSION_TEXTURE2D, ID3D12Device, ID3D12Resource,
};
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_R8G8B8A8_UNORM;
use windows::core::Interface;

/// Where GStreamer's d3d12 library is (MinGW builds, MSVC builds).
const LIBRARIES: [&str; 2] = ["libgstd3d12-1.0-0.dll", "gstd3d12-1.0-0.dll"];

/// GL imports kept: every pooled texture a playing decoder has in flight
/// (the frame cache holds up to 48 of them).
const KEEP: usize = 64;

type Boolean = i32;
type IsMemory = unsafe extern "C" fn(*mut c_void) -> Boolean;
type Sync = unsafe extern "C" fn(*mut c_void) -> Boolean;
type ResourceHandle = unsafe extern "C" fn(*mut c_void) -> *mut c_void;
type NtHandle = unsafe extern "C" fn(*mut c_void, *mut *mut c_void) -> Boolean;
type CreateToken = unsafe extern "C" fn() -> i64;
type GetToken = unsafe extern "C" fn(*mut c_void, i64) -> *mut c_void;
type SetToken =
    unsafe extern "C" fn(*mut c_void, i64, *mut c_void, Option<unsafe extern "C" fn(*mut c_void)>);

/// The calls into GStreamer's d3d12 library.
struct Gst {
    _library: libloading::Library,
    is_memory: IsMemory,
    sync: Sync,
    resource: ResourceHandle,
    nt_handle: NtHandle,
    create_token: CreateToken,
    get_token: GetToken,
    set_token: SetToken,
}

fn load() -> Result<Gst, String> {
    let library = LIBRARIES
        .iter()
        // SAFETY: GStreamer's own library, which runs nothing of note when
        // loaded (its d3d12 plugin has loaded it already when frames come).
        .find_map(|name| unsafe { libloading::Library::new(name) }.ok())
        .ok_or("GStreamer's d3d12 library (1.26 or later) is not there")?;
    macro_rules! symbol {
        ($name:literal, $ty:ty) => {
            // SAFETY: the function the library exports under this name,
            // with the signature its header declares; the library stays
            // loaded as long as the table.
            *unsafe { library.get::<$ty>(concat!($name, "\0").as_bytes()) }
                .map_err(|e| format!("{}: {e}", $name))?
        };
    }
    Ok(Gst {
        is_memory: symbol!("gst_is_d3d12_memory", IsMemory),
        sync: symbol!("gst_d3d12_memory_sync", Sync),
        resource: symbol!("gst_d3d12_memory_get_resource_handle", ResourceHandle),
        nt_handle: symbol!("gst_d3d12_memory_get_nt_handle", NtHandle),
        create_token: symbol!("gst_d3d12_create_user_token", CreateToken),
        get_token: symbol!("gst_d3d12_memory_get_token_data", GetToken),
        set_token: symbol!("gst_d3d12_memory_set_token_data", SetToken),
        _library: library,
    })
}

fn gst() -> Option<&'static Gst> {
    static GST: OnceLock<Option<Gst>> = OnceLock::new();
    GST.get_or_init(|| {
        load()
            .inspect_err(|e| tracing::info!("video frames through memory: {e}"))
            .ok()
    })
    .as_ref()
}

/// Whether video frames can come in D3D12 textures here (GStreamer's d3d12
/// library is there; `FADERFRAME_VIDEO_D3D12=0` says no).
pub(crate) fn available() -> bool {
    std::env::var("FADERFRAME_VIDEO_D3D12").as_deref() != Ok("0") && gst().is_some()
}

/// Our token on GStreamer's memories (asked once, after GStreamer is up).
fn token(gst: &Gst) -> i64 {
    static TOKEN: OnceLock<i64> = OnceLock::new();
    // SAFETY: a counter in the library; no arguments.
    *TOKEN.get_or_init(|| unsafe { (gst.create_token)() })
}

/// Which pooled texture `memory` is: an id of ours kept on it, made the
/// first time (never reused, so a texture made later is never taken for
/// one imported before).
///
/// # Safety
///
/// `memory` is a live `GstD3D12Memory`.
unsafe fn slot_of(gst: &Gst, memory: *mut c_void) -> u64 {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    let token = token(gst);
    // SAFETY: the caller's memory; the data is our id, not a pointer.
    let known = unsafe { (gst.get_token)(memory, token) };
    if !known.is_null() {
        return known as u64;
    }
    let id = NEXT.fetch_add(1, Ordering::Relaxed);
    // SAFETY: as above; nothing to free (no destroy notify).
    unsafe { (gst.set_token)(memory, token, id as *mut c_void, None) };
    id
}

/// A frame's texture, its work done.
struct Texture {
    resource: *mut c_void,
    shared: Shared,
    /// The adapter it is on (`LUID` as GL reports one: eight bytes).
    luid: Option<[u8; 8]>,
}

/// `frame`'s texture, synchronised (the converter's work on it done), laid
/// out as it must be (RGBA8, the frame's size, one mip); with `for_gl` its
/// NT handle, size and adapter too.
fn texture_of(frame: &GpuFrame, for_gl: bool) -> Result<Texture, String> {
    let gst = gst().ok_or("no d3d12 library")?;
    let memory = frame.memory();
    // SAFETY: `memory` is the frame's own, alive while `frame` is; every
    // call is the library's documented use of a GstD3D12Memory, and the
    // resource it hands out (transfer none) lives as long as the memory.
    unsafe {
        if (gst.is_memory)(memory) == 0 {
            return Err("a frame not in D3D12 memory".into());
        }
        if (gst.sync)(memory) == 0 {
            return Err("GStreamer could not finish the texture".into());
        }
        let resource = (gst.resource)(memory);
        let r = ID3D12Resource::from_raw_borrowed(&resource).ok_or("a frame without a texture")?;
        let desc = r.GetDesc();
        if desc.Dimension != D3D12_RESOURCE_DIMENSION_TEXTURE2D
            || desc.Format != DXGI_FORMAT_R8G8B8A8_UNORM
            || desc.Width != u64::from(frame.width())
            || desc.Height != frame.height()
            || desc.MipLevels != 1
            || desc.DepthOrArraySize != 1
        {
            return Err(format!(
                "a texture laid out otherwise ({:?}, {}x{})",
                desc.Format, desc.Width, desc.Height
            ));
        }
        let mut shared = Shared {
            slot: slot_of(gst, memory),
            handle: 0,
            size: 0,
            width: frame.width(),
            height: frame.height(),
        };
        let mut luid = None;
        if for_gl {
            let mut handle = std::ptr::null_mut();
            if (gst.nt_handle)(memory, &mut handle) == 0 || handle.is_null() {
                return Err("the texture cannot be shared (its heap is not)".into());
            }
            let mut device: Option<ID3D12Device> = None;
            r.GetDevice(&mut device).map_err(|e| e.to_string())?;
            let device = device.ok_or("a texture without a device")?;
            shared.size = device.GetResourceAllocationInfo(0, &[desc]).SizeInBytes;
            shared.handle = handle as isize;
            let l = device.GetAdapterLuid();
            let mut bytes = [0u8; 8];
            bytes[..4].copy_from_slice(&l.LowPart.to_le_bytes());
            bytes[4..].copy_from_slice(&l.HighPart.to_le_bytes());
            luid = Some(bytes);
        }
        Ok(Texture {
            resource,
            shared,
            luid,
        })
    }
}

thread_local! {
    /// The GL import (made at the first frame under GL).
    static GL: RefCell<Option<GlImport>> = const { RefCell::new(None) };
    /// The first frame was checked.
    static CHECKED: Cell<bool> = const { Cell::new(false) };
}

/// `frame` as a GTK texture for `widget`'s renderer; an error says why not
/// (frames then come through memory: `zero_copy::failed`).
pub(crate) fn texture(frame: &GpuFrame, widget: &gtk::Widget) -> Result<gdk::Texture, String> {
    let gl = match renderer_kind(widget) {
        Some("gl") => true,
        Some(_) => false,
        None => return Err("GTK draws without a GPU renderer".into()),
    };
    let t = texture_of(frame, gl)?;
    let expected = if CHECKED.get() {
        None
    } else {
        Some(frame.read_back().ok_or("the frame does not read back")?)
    };
    let texture = if gl {
        GL.with(|g| {
            let mut g = g.borrow_mut();
            if g.is_none() {
                *g = Some(GlImport::keeping(KEEP)?);
            }
            let import = g.as_mut().ok_or("no GL import")?;
            if t.luid.is_some_and(|l| l != import.luid()) {
                return Err("GL draws on another adapter than the video's".to_string());
            }
            if let Some(want) = &expected
                && !same(&import.read_shared(&t.shared)?, want)
            {
                return Err("GL sees other pixels".into());
            }
            import.texture_keeping(t.shared, frame.clone())
        })?
    } else {
        // SAFETY: the clone of the frame keeps its buffer, so its memory
        // and texture, alive (and out of the converter's pool) until GTK
        // lets go of the texture.
        let texture = unsafe { d3d12_texture_keeping(t.resource, frame.clone()) }?;
        if let Some(want) = &expected
            && !same(&download(&texture), want)
        {
            return Err("GTK sees other pixels".into());
        }
        texture
    };
    CHECKED.set(true);
    Ok(texture)
}
