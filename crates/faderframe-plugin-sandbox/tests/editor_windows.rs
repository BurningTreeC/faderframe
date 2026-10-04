//! Windows: a sandboxed plugin's editor is a child window of one of
//! FaderFrame's, across processes. Creating and destroying it sends the
//! parent messages synchronously while FaderFrame waits for the helper's
//! answer — this must neither deadlock nor wait out a timeout.
#![cfg(windows)]
#![allow(clippy::unwrap_used)]

use faderframe_audio_graph::NodeIo;
use faderframe_core::ParameterId;
use faderframe_plugin_host::{
    AudioPortInfo, EditorRequests, ParameterInfo, ParentWindow, PluginCategory, PluginDescriptor,
    PluginEditor, PluginError, PluginFactory, PluginFormat, PluginInstance, PluginProcessContext,
    PluginProcessor, PluginRegistry, ProcessConfig, ProcessStatus, TailLength, WindowApi,
};
use faderframe_plugin_sandbox::{Launcher, child, instantiate_sandboxed};
use std::time::{Duration, Instant};
use windows_sys::Win32::Foundation::HWND;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DestroyWindow, GW_CHILD, GetWindow, GetWindowThreadProcessId, WS_CHILD,
    WS_OVERLAPPEDWINDOW, WS_VISIBLE,
};

const SIZE: (u32, u32) = (320, 200);

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}

/// A plain window of the system's "STATIC" class.
fn window(style: u32, parent: HWND, size: (u32, u32)) -> HWND {
    let class = wide("STATIC");
    // SAFETY: a system window class, NUL-terminated strings, no extras.
    unsafe {
        CreateWindowExW(
            0,
            class.as_ptr(),
            class.as_ptr(),
            style,
            0,
            0,
            size.0 as i32,
            size.1 as i32,
            parent,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null(),
        )
    }
}

struct Windowed;
struct WindowedInstance {
    descriptor: PluginDescriptor,
    child: HWND,
}

fn descriptor() -> PluginDescriptor {
    PluginDescriptor {
        format: PluginFormat::Vst3,
        id: "test.window".into(),
        name: "Windowed".into(),
        vendor: "FaderFrame".into(),
        version: "1".into(),
        category: PluginCategory::Effect,
        audio_inputs: vec![AudioPortInfo {
            channels: 2,
            is_main: true,
        }],
        audio_outputs: vec![AudioPortInfo {
            channels: 2,
            is_main: true,
        }],
        note_inputs: 0,
        note_outputs: 0,
    }
}

impl PluginFactory for Windowed {
    fn format(&self) -> PluginFormat {
        PluginFormat::Vst3
    }
    fn scan(&self) -> Vec<PluginDescriptor> {
        vec![descriptor()]
    }
    fn instantiate(&self, _id: &str) -> Result<Box<dyn PluginInstance>, PluginError> {
        Ok(Box::new(WindowedInstance {
            descriptor: descriptor(),
            child: std::ptr::null_mut(),
        }))
    }
}

struct Silent;

impl PluginProcessor for Silent {
    fn process(&mut self, _ctx: &PluginProcessContext<'_>, _io: &mut NodeIo<'_>) -> ProcessStatus {
        ProcessStatus::Continue
    }
    fn reset(&mut self) {}
}

impl PluginInstance for WindowedInstance {
    fn descriptor(&self) -> &PluginDescriptor {
        &self.descriptor
    }
    fn parameters(&self) -> &[ParameterInfo] {
        &[]
    }
    fn parameter(&mut self, _id: ParameterId) -> Option<f64> {
        None
    }
    fn set_parameter(&mut self, id: ParameterId, _v: f64) -> Result<(), PluginError> {
        Err(PluginError::UnknownParameter(id))
    }
    fn latency_samples(&self) -> u32 {
        0
    }
    fn tail(&self) -> TailLength {
        TailLength::None
    }
    fn save_state(&mut self) -> Result<Vec<u8>, PluginError> {
        Ok(Vec::new())
    }
    fn load_state(&mut self, _data: &[u8]) -> Result<(), PluginError> {
        Ok(())
    }
    fn create_processor(
        &mut self,
        _c: &ProcessConfig,
    ) -> Result<Box<dyn PluginProcessor>, PluginError> {
        Ok(Box::new(Silent))
    }
    fn editor(&mut self) -> Option<&mut dyn PluginEditor> {
        Some(self)
    }
}

impl PluginEditor for WindowedInstance {
    fn can_embed(&mut self, api: WindowApi) -> bool {
        api == WindowApi::Win32
    }
    fn can_float(&mut self, _api: WindowApi) -> bool {
        false
    }
    fn open_embedded(&mut self, _api: WindowApi, _scale: f64) -> Result<(u32, u32), PluginError> {
        Ok(SIZE)
    }
    fn attach(&mut self, parent: ParentWindow) -> Result<(), PluginError> {
        // A child of a window in another process (no WS_EX_NOPARENTNOTIFY:
        // the parent hears of it synchronously).
        self.child = window(WS_CHILD | WS_VISIBLE, parent.handle as usize as HWND, SIZE);
        if self.child.is_null() {
            return Err(PluginError::Failed("CreateWindowExW failed".into()));
        }
        Ok(())
    }
    fn open_floating(&mut self, _api: WindowApi, _title: &str) -> Result<(), PluginError> {
        Err(PluginError::Failed("embeds only".into()))
    }
    fn close(&mut self) {
        if !self.child.is_null() {
            // SAFETY: our own window, destroyed on the thread that made it.
            unsafe { DestroyWindow(self.child) };
            self.child = std::ptr::null_mut();
        }
    }
    fn is_open(&self) -> bool {
        !self.child.is_null()
    }
    fn can_resize(&mut self) -> bool {
        false
    }
    fn set_size(&mut self, _width: u32, _height: u32) -> Option<(u32, u32)> {
        None
    }
    fn take_requests(&mut self) -> EditorRequests {
        EditorRequests::default()
    }
}

/// When started as a helper this "test" serves the host, then exits.
#[test]
fn helper_entry() {
    if !child::is_helper() {
        return;
    }
    let mut registry = PluginRegistry::with_builtins();
    registry.add_factory(Box::new(Windowed));
    std::process::exit(child::run(registry));
}

#[test]
fn an_editor_embeds_into_our_window_across_processes() {
    let launcher = Launcher {
        exe: std::env::current_exe().unwrap(),
        args: ["helper_entry", "--exact", "--nocapture", "--test-threads=1"]
            .map(String::from)
            .to_vec(),
        env: Vec::new(),
    };
    let mut inst = instantiate_sandboxed(&launcher, PluginFormat::Vst3, "test.window").unwrap();
    assert!(inst.sandboxed());
    // Ours, not shown (nothing appears on screen).
    let parent = window(WS_OVERLAPPEDWINDOW, std::ptr::null_mut(), (400, 300));
    assert!(!parent.is_null());
    let t = Instant::now();
    let ed = inst.editor().unwrap();
    assert!(ed.can_embed(WindowApi::Win32));
    assert_eq!(ed.open_embedded(WindowApi::Win32, 1.0).unwrap(), SIZE);
    ed.attach(ParentWindow {
        api: WindowApi::Win32,
        handle: parent as usize as u64,
    })
    .unwrap();
    assert!(ed.is_open());
    // SAFETY: plain queries on our window and its child.
    let (child, owner) = unsafe {
        let child = GetWindow(parent, GW_CHILD);
        let mut pid = 0u32;
        GetWindowThreadProcessId(child, &mut pid);
        (child, pid)
    };
    assert!(
        !child.is_null(),
        "the editor's window is our window's child"
    );
    assert_ne!(owner, std::process::id(), "made by the helper");
    ed.close();
    // SAFETY: a plain query on our window.
    assert!(unsafe { GetWindow(parent, GW_CHILD) }.is_null(), "gone");
    assert!(
        t.elapsed() < Duration::from_secs(5),
        "no deadlock: {:?}",
        t.elapsed()
    );
    // SAFETY: our own window.
    unsafe { DestroyWindow(parent) };
}
