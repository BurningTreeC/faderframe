//! Editor parent windows on Windows: plain top-level Win32 windows the
//! plugins create their child windows in. GTK's main loop dispatches every
//! message of the UI thread, these windows' included; the window procedure
//! only records close requests and resizes for the next UI tick.

use super::{ParentEvent, Rect};
use faderframe_plugin_host::{ParentWindow, WindowApi};
use std::cell::RefCell;
use std::ffi::c_void;
use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows_sys::Win32::Graphics::Gdi::{
    BLACK_BRUSH, EnumDisplayMonitors, GetMonitorInfoW, GetStockObject, HDC, HMONITOR,
    MONITOR_DEFAULTTOPRIMARY, MONITORINFO, MonitorFromWindow,
};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::HiDpi::{GetDpiForSystem, GetDpiForWindow};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::GetActiveWindow;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    AdjustWindowRectEx, CS_HREDRAW, CS_VREDRAW, CreateWindowExW, DefWindowProcW, DestroyWindow,
    GetWindowRect, IDC_ARROW, IsIconic, LoadCursorW, RegisterClassExW, SIZE_MINIMIZED, SW_HIDE,
    SW_RESTORE, SW_SHOW, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SWP_NOZORDER, SetForegroundWindow,
    SetWindowPos, ShowWindow, WM_CLOSE, WM_SIZE, WNDCLASSEXW, WS_CAPTION, WS_CLIPCHILDREN,
    WS_MINIMIZEBOX, WS_OVERLAPPED, WS_OVERLAPPEDWINDOW, WS_SYSMENU,
};

thread_local! {
    /// Recorded by the window procedure, taken by [`Parents::events`].
    static EVENTS: RefCell<Vec<ParentEvent>> = const { RefCell::new(Vec::new()) };
}

const CLASS: &str = "FaderFramePluginEditor";

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}

fn hwnd(win: u64) -> HWND {
    win as usize as HWND
}

fn id(hwnd: HWND) -> u64 {
    hwnd as usize as u64
}

fn style(resizable: bool) -> u32 {
    let base = if resizable {
        WS_OVERLAPPEDWINDOW
    } else {
        WS_OVERLAPPED | WS_CAPTION | WS_SYSMENU | WS_MINIMIZEBOX
    };
    // The plugin's child window is not painted over.
    base | WS_CLIPCHILDREN
}

/// The outer window size for a content (client) area of `size`.
fn outer_size((w, h): (u32, u32), resizable: bool) -> (i32, i32) {
    let mut r = RECT {
        left: 0,
        top: 0,
        right: w.min(16_000) as i32,
        bottom: h.min(16_000) as i32,
    };
    // SAFETY: `r` is a valid rectangle to adjust in place.
    unsafe { AdjustWindowRectEx(&mut r, style(resizable), 0, 0) };
    (r.right - r.left, r.bottom - r.top)
}

unsafe extern "system" fn window_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    let push = |e: ParentEvent| {
        let _ = EVENTS.try_with(|q| q.borrow_mut().push(e));
    };
    match msg {
        // The host decides (closes the editor first, then the window).
        WM_CLOSE => {
            push(ParentEvent::Close(id(hwnd)));
            0
        }
        WM_SIZE => {
            if wparam as u32 != SIZE_MINIMIZED {
                let (w, h) = ((lparam & 0xffff) as u32, ((lparam >> 16) & 0xffff) as u32);
                push(ParentEvent::Resized(id(hwnd), (w, h)));
            }
            0
        }
        // SAFETY: forwards the message as received.
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

pub struct Parents {
    class: Vec<u16>,
}

impl Parents {
    pub fn connect() -> Result<Self, String> {
        let class = wide(CLASS);
        // SAFETY: a fully initialised class description whose strings live
        // in `class` (kept with the struct; Windows copies the name anyway).
        let atom = unsafe {
            let wc = WNDCLASSEXW {
                cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
                style: CS_HREDRAW | CS_VREDRAW,
                lpfnWndProc: Some(window_proc),
                cbClsExtra: 0,
                cbWndExtra: 0,
                hInstance: GetModuleHandleW(std::ptr::null()),
                hIcon: std::ptr::null_mut(),
                hCursor: LoadCursorW(std::ptr::null_mut(), IDC_ARROW),
                hbrBackground: GetStockObject(BLACK_BRUSH) as _,
                lpszMenuName: std::ptr::null(),
                lpszClassName: class.as_ptr(),
                hIconSm: std::ptr::null_mut(),
            };
            RegisterClassExW(&wc)
        };
        if atom == 0 {
            return Err(format!(
                "cannot register the editor window class: {}",
                std::io::Error::last_os_error()
            ));
        }
        Ok(Self { class })
    }

    pub fn create_window(
        &self,
        title: &str,
        size: (u32, u32),
        resizable: bool,
        pos: (i32, i32),
    ) -> Result<u64, String> {
        let (w, h) = outer_size(size, resizable);
        let title = wide(title);
        // SAFETY: registered class, valid strings; no parent or menu.
        let hwnd = unsafe {
            CreateWindowExW(
                0,
                self.class.as_ptr(),
                title.as_ptr(),
                style(resizable),
                pos.0,
                pos.1,
                w,
                h,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                GetModuleHandleW(std::ptr::null()),
                std::ptr::null::<c_void>(),
            )
        };
        if hwnd.is_null() {
            return Err(format!(
                "cannot create the editor window: {}",
                std::io::Error::last_os_error()
            ));
        }
        Ok(id(hwnd))
    }

    pub fn parent(&self, win: u64) -> ParentWindow {
        ParentWindow {
            api: WindowApi::Win32,
            handle: win,
        }
    }

    /// The scale of the display showing FaderFrame (96 dpi = 1).
    pub fn scale(&self) -> f64 {
        // SAFETY: plain queries (a null window gives 0).
        let dpi = unsafe {
            match GetDpiForWindow(GetActiveWindow()) {
                0 => GetDpiForSystem(),
                d => d,
            }
        };
        f64::from(dpi.max(96)) / 96.0
    }

    pub fn map_at(&self, win: u64, pos: (i32, i32)) {
        // SAFETY: `win` is a window of ours.
        unsafe {
            SetWindowPos(
                hwnd(win),
                std::ptr::null_mut(),
                pos.0,
                pos.1,
                0,
                0,
                SWP_NOSIZE | SWP_NOZORDER,
            );
            ShowWindow(hwnd(win), SW_SHOW);
            SetForegroundWindow(hwnd(win));
        }
    }

    /// Monitor work areas and the one showing FaderFrame (the active
    /// window's), in the physical pixels window positions use.
    pub fn monitors(&self, _main: Option<&str>) -> (Vec<Rect>, Option<Rect>) {
        unsafe extern "system" fn collect(
            monitor: HMONITOR,
            _: HDC,
            _: *mut RECT,
            data: LPARAM,
        ) -> windows_sys::core::BOOL {
            // SAFETY: `data` is the `Vec` passed below, alive for the call.
            let out = unsafe { &mut *(data as *mut Vec<Rect>) };
            if let Some(r) = work_area(monitor) {
                out.push(r);
            }
            1
        }
        let mut all: Vec<Rect> = Vec::new();
        // SAFETY: the callback only runs during this call.
        unsafe {
            EnumDisplayMonitors(
                std::ptr::null_mut(),
                std::ptr::null(),
                Some(collect),
                &mut all as *mut Vec<Rect> as LPARAM,
            );
        }
        // SAFETY: plain queries.
        let main = unsafe {
            work_area(MonitorFromWindow(
                GetActiveWindow(),
                MONITOR_DEFAULTTOPRIMARY,
            ))
        };
        (all, main)
    }

    pub fn position(&self, win: u64) -> Option<(i32, i32)> {
        let mut r = RECT {
            left: 0,
            top: 0,
            right: 0,
            bottom: 0,
        };
        // SAFETY: `win` is a window of ours; `r` is written.
        (unsafe { GetWindowRect(hwnd(win), &mut r) } != 0).then_some((r.left, r.top))
    }

    pub fn unmap(&self, win: u64) {
        // SAFETY: `win` is a window of ours.
        unsafe { ShowWindow(hwnd(win), SW_HIDE) };
    }

    pub fn raise(&self, win: u64) {
        // SAFETY: `win` is a window of ours.
        unsafe {
            if IsIconic(hwnd(win)) != 0 {
                ShowWindow(hwnd(win), SW_RESTORE);
            } else {
                ShowWindow(hwnd(win), SW_SHOW);
            }
            SetForegroundWindow(hwnd(win));
        }
    }

    pub fn resize(&self, win: u64, size: (u32, u32), resizable: bool) {
        let (w, h) = outer_size(size, resizable);
        // SAFETY: `win` is a window of ours.
        unsafe {
            SetWindowPos(
                hwnd(win),
                std::ptr::null_mut(),
                0,
                0,
                w,
                h,
                SWP_NOMOVE | SWP_NOZORDER | SWP_NOACTIVATE,
            );
        }
    }

    pub fn destroy(&self, win: u64) {
        // SAFETY: `win` is a window of ours; the editor was closed first.
        unsafe { DestroyWindow(hwnd(win)) };
        // Messages of the gone window are no use any more.
        EVENTS.with(|q| {
            q.borrow_mut().retain(|e| match e {
                ParentEvent::Close(w) | ParentEvent::Resized(w, _) => *w != win,
            })
        });
    }

    pub fn events(&self) -> Vec<ParentEvent> {
        EVENTS.with(|q| std::mem::take(&mut *q.borrow_mut()))
    }

    /// Not available here (the screenshot action skips plugin editors).
    pub fn capture(&self, _win: u64, _size: (u32, u32)) -> Result<gtk::gdk::MemoryTexture, String> {
        Err("plugin editor screenshots are X11 only".into())
    }
}

fn work_area(monitor: HMONITOR) -> Option<Rect> {
    let mut info = MONITORINFO {
        cbSize: std::mem::size_of::<MONITORINFO>() as u32,
        rcMonitor: RECT {
            left: 0,
            top: 0,
            right: 0,
            bottom: 0,
        },
        rcWork: RECT {
            left: 0,
            top: 0,
            right: 0,
            bottom: 0,
        },
        dwFlags: 0,
    };
    // SAFETY: `info` has its size set and is written by the call.
    if unsafe { GetMonitorInfoW(monitor, &mut info) } == 0 {
        return None;
    }
    let r = info.rcWork;
    Some((r.left, r.top, r.right - r.left, r.bottom - r.top))
}
