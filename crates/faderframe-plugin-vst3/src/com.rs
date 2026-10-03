//! Host-side COM objects: what a VST3 plugin can call on FaderFrame.
//!
//! One [`HostApp`] per instance is the host context (`IHostApplication`),
//! the component handler (edits, restarts), the editor's plug frame and its
//! run loop (Linux file descriptors and timers). Callbacks only record what
//! was asked in [`HostState`]; the control thread acts on it when it polls
//! the instance — never inside the callback, which may come from any
//! thread. Plugins also get messages, attribute lists and memory streams
//! from here.

use crate::util::{guid, query_raw, write_wstr};
use std::collections::HashMap;
use std::ffi::{CStr, CString, c_char, c_void};
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use std::sync::{Arc, Mutex};
use vst3::Steinberg::Linux::{
    FileDescriptor, IEventHandler, IEventHandlerTrait, IRunLoop, IRunLoopTrait, ITimerHandler,
    ITimerHandlerTrait, TimerInterval,
};
use vst3::Steinberg::Vst::{
    IAttributeList, IAttributeListTrait, IAudioProcessor, IComponent, IComponentHandler,
    IComponentHandler2, IComponentHandler2Trait, IComponentHandlerTrait, IConnectionPoint,
    IEditController, IHostApplication, IHostApplicationTrait, IMessage, IMessageTrait,
    IMidiMapping, IPlugInterfaceSupport, IPlugInterfaceSupportTrait, IUnitInfo, ParamID,
    ParamValue, String128, TChar,
};
use vst3::Steinberg::{
    FIDString, FUnknown, IBStream, IBStreamTrait, IPlugFrame, IPlugFrameTrait, IPlugView,
    ISizeableStream, ISizeableStreamTrait, TBool, TUID, ViewRect, int32, int64, kInvalidArgument,
    kNoInterface, kResultFalse, kResultOk, kResultTrue, tresult, uint32,
};
use vst3::{Class, ComPtr, ComRef, ComWrapper, Interface};

/// An edit made in the plugin's own editor.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Edit {
    Begin(ParamID),
    Perform(ParamID, ParamValue),
    End(ParamID),
}

#[derive(Default)]
struct RunLoop {
    fds: Vec<(ComPtr<IEventHandler>, FileDescriptor)>,
    timers: Vec<(u32, ComPtr<ITimerHandler>, u32)>,
    next_timer: u32,
}

/// What the plugin asked for, collected for the control thread.
#[derive(Default)]
pub struct HostState {
    restart: AtomicI32,
    dirty: AtomicBool,
    edits: Mutex<Vec<Edit>>,
    resize: Mutex<Option<(u32, u32)>>,
    run_loop: Mutex<RunLoop>,
}

impl HostState {
    /// Restart flags (`RestartFlags_`) requested since the last call.
    pub fn take_restart(&self) -> i32 {
        self.restart.swap(0, Ordering::AcqRel)
    }

    pub fn take_dirty(&self) -> bool {
        self.dirty.swap(false, Ordering::AcqRel)
    }

    pub fn take_edits(&self) -> Vec<Edit> {
        self.edits
            .lock()
            .map(|mut e| std::mem::take(&mut *e))
            .unwrap_or_default()
    }

    pub fn take_resize(&self) -> Option<(u32, u32)> {
        self.resize.lock().ok().and_then(|mut r| r.take())
    }

    pub fn fds(&self) -> Vec<FileDescriptor> {
        let mut out: Vec<FileDescriptor> = self
            .run_loop
            .lock()
            .map(|r| r.fds.iter().map(|(_, fd)| *fd).collect())
            .unwrap_or_default();
        out.sort_unstable();
        out.dedup();
        out
    }

    /// (id, period in ms) of the registered timers.
    pub fn timers(&self) -> Vec<(u32, u32)> {
        self.run_loop
            .lock()
            .map(|r| r.timers.iter().map(|(id, _, ms)| (*id, *ms)).collect())
            .unwrap_or_default()
    }

    /// Run the handlers registered for `fd` (control thread). Handlers are
    /// called without holding the lock: they may (un)register themselves.
    pub fn fd_ready(&self, fd: FileDescriptor) {
        let handlers: Vec<ComPtr<IEventHandler>> = self
            .run_loop
            .lock()
            .map(|r| {
                r.fds
                    .iter()
                    .filter(|(_, f)| *f == fd)
                    .map(|(h, _)| h.clone())
                    .collect()
            })
            .unwrap_or_default();
        for h in handlers {
            // SAFETY: the handler is alive (we hold a reference).
            unsafe { h.onFDIsSet(fd) };
        }
    }

    pub fn timer_fired(&self, id: u32) {
        let handler = self.run_loop.lock().ok().and_then(|r| {
            r.timers
                .iter()
                .find(|(t, _, _)| *t == id)
                .map(|(_, h, _)| h.clone())
        });
        if let Some(h) = handler {
            // SAFETY: as in `fd_ready`.
            unsafe { h.onTimer() };
        }
    }

    /// Drop every registration (the instance goes away).
    pub fn clear_run_loop(&self) {
        let old = self.run_loop.lock().map(|mut r| std::mem::take(&mut *r));
        // Released outside the lock: a release may call back into us.
        drop(old);
    }
}

/// The host object handed to one plugin instance.
pub struct HostApp {
    pub state: Arc<HostState>,
}

impl HostApp {
    pub fn new() -> ComWrapper<HostApp> {
        ComWrapper::new(HostApp {
            state: Arc::new(HostState::default()),
        })
    }
}

impl Class for HostApp {
    type Interfaces = (
        IHostApplication,
        IComponentHandler,
        IComponentHandler2,
        IPlugFrame,
        IRunLoop,
        IPlugInterfaceSupport,
    );
}

/// Host context pointer for `initialize`.
pub fn context(host: &ComWrapper<HostApp>) -> *mut FUnknown {
    host.as_com_ref::<IHostApplication>()
        .map_or(std::ptr::null_mut(), |r| r.as_ptr() as *mut FUnknown)
}

/// Hand a new object of class `C` out as interface `iid`.
fn hand_out<C, I>(object: C, iid: &[u8; 16], obj: *mut *mut c_void) -> tresult
where
    C: Class + 'static,
    I: Interface,
    C::Interfaces: vst3::com_scrape_types::MakeHeader<C, ComWrapper<C>>,
{
    let wrapper = ComWrapper::new(object);
    let Some(p) = wrapper.to_com_ptr::<I>() else {
        return kNoInterface;
    };
    // SAFETY: `p` is a live object; queryInterface adds the caller's
    // reference, ours is released when `p` drops.
    unsafe { query_raw(p.as_ptr() as *mut FUnknown, iid, obj) }
}

impl IHostApplicationTrait for HostApp {
    unsafe fn getName(&self, name: *mut String128) -> tresult {
        // SAFETY: the plugin passes a String128 buffer.
        let Some(name) = (unsafe { name.as_mut() }) else {
            return kInvalidArgument;
        };
        write_wstr(name, "FaderFrame");
        kResultOk
    }

    unsafe fn createInstance(
        &self,
        cid: *mut TUID,
        iid: *mut TUID,
        obj: *mut *mut c_void,
    ) -> tresult {
        // SAFETY: the plugin passes valid ids and an out pointer.
        let (Some(cid), Some(iid)) = (unsafe { cid.as_ref() }, unsafe { iid.as_ref() }) else {
            return kInvalidArgument;
        };
        let (cid, iid) = (guid(cid), guid(iid));
        if cid == IMessage::IID {
            hand_out::<_, IMessage>(Message::new(), &iid, obj)
        } else if cid == IAttributeList::IID {
            hand_out::<_, IAttributeList>(AttributeList::default(), &iid, obj)
        } else {
            if !obj.is_null() {
                // SAFETY: checked for null.
                unsafe { *obj = std::ptr::null_mut() };
            }
            kResultFalse
        }
    }
}

impl IComponentHandlerTrait for HostApp {
    unsafe fn beginEdit(&self, id: ParamID) -> tresult {
        if let Ok(mut e) = self.state.edits.lock() {
            e.push(Edit::Begin(id));
        }
        kResultOk
    }

    unsafe fn performEdit(&self, id: ParamID, value: ParamValue) -> tresult {
        if let Ok(mut e) = self.state.edits.lock() {
            e.push(Edit::Perform(id, value));
        }
        kResultOk
    }

    unsafe fn endEdit(&self, id: ParamID) -> tresult {
        if let Ok(mut e) = self.state.edits.lock() {
            e.push(Edit::End(id));
        }
        kResultOk
    }

    unsafe fn restartComponent(&self, flags: int32) -> tresult {
        self.state.restart.fetch_or(flags, Ordering::AcqRel);
        kResultOk
    }
}

impl IComponentHandler2Trait for HostApp {
    unsafe fn setDirty(&self, state: TBool) -> tresult {
        if state != 0 {
            self.state.dirty.store(true, Ordering::Release);
        }
        kResultOk
    }

    unsafe fn requestOpenEditor(&self, _name: FIDString) -> tresult {
        kResultFalse
    }

    unsafe fn startGroupEdit(&self) -> tresult {
        kResultOk
    }

    unsafe fn finishGroupEdit(&self) -> tresult {
        kResultOk
    }
}

impl IPlugFrameTrait for HostApp {
    unsafe fn resizeView(&self, _view: *mut IPlugView, new_size: *mut ViewRect) -> tresult {
        // SAFETY: the plugin passes the requested rectangle.
        let Some(r) = (unsafe { new_size.as_ref() }) else {
            return kInvalidArgument;
        };
        let (w, h) = ((r.right - r.left).max(1), (r.bottom - r.top).max(1));
        if let Ok(mut s) = self.state.resize.lock() {
            *s = Some((w as u32, h as u32));
        }
        kResultTrue
    }
}

impl IRunLoopTrait for HostApp {
    unsafe fn registerEventHandler(
        &self,
        handler: *mut IEventHandler,
        fd: FileDescriptor,
    ) -> tresult {
        // SAFETY: the plugin passes a live handler; we keep a reference.
        let Some(h) = (unsafe { ComRef::from_raw(handler) }) else {
            return kInvalidArgument;
        };
        if let Ok(mut r) = self.state.run_loop.lock() {
            r.fds.push((h.to_com_ptr(), fd));
        }
        kResultTrue
    }

    unsafe fn unregisterEventHandler(&self, handler: *mut IEventHandler) -> tresult {
        let removed: Vec<_> = self
            .state
            .run_loop
            .lock()
            .map(|mut r| {
                let (gone, keep) = std::mem::take(&mut r.fds)
                    .into_iter()
                    .partition(|(h, _)| h.as_ptr() == handler);
                r.fds = keep;
                gone
            })
            .unwrap_or_default();
        drop(removed);
        kResultTrue
    }

    unsafe fn registerTimer(
        &self,
        handler: *mut ITimerHandler,
        milliseconds: TimerInterval,
    ) -> tresult {
        // SAFETY: as for event handlers.
        let Some(h) = (unsafe { ComRef::from_raw(handler) }) else {
            return kInvalidArgument;
        };
        if let Ok(mut r) = self.state.run_loop.lock() {
            r.next_timer += 1;
            let id = r.next_timer;
            r.timers
                .push((id, h.to_com_ptr(), milliseconds.clamp(1, 60_000) as u32));
        }
        kResultTrue
    }

    unsafe fn unregisterTimer(&self, handler: *mut ITimerHandler) -> tresult {
        let removed: Vec<_> = self
            .state
            .run_loop
            .lock()
            .map(|mut r| {
                let (gone, keep) = std::mem::take(&mut r.timers)
                    .into_iter()
                    .partition(|(_, h, _)| h.as_ptr() == handler);
                r.timers = keep;
                gone
            })
            .unwrap_or_default();
        drop(removed);
        kResultTrue
    }
}

impl IPlugInterfaceSupportTrait for HostApp {
    unsafe fn isPlugInterfaceSupported(&self, iid: *const TUID) -> tresult {
        // SAFETY: the plugin passes a valid id.
        let Some(iid) = (unsafe { iid.as_ref() }) else {
            return kInvalidArgument;
        };
        let iid = guid(iid);
        let supported = [
            IComponent::IID,
            IAudioProcessor::IID,
            IEditController::IID,
            IConnectionPoint::IID,
            IMidiMapping::IID,
            IUnitInfo::IID,
        ];
        if supported.contains(&iid) {
            kResultTrue
        } else {
            kResultFalse
        }
    }
}

/// `IMessage` for plugins talking between their component and controller.
pub struct Message {
    id: Mutex<Option<CString>>,
    attributes: ComWrapper<AttributeList>,
}

impl Default for Message {
    fn default() -> Self {
        Self::new()
    }
}

impl Message {
    pub fn new() -> Self {
        Self {
            id: Mutex::new(None),
            attributes: ComWrapper::new(AttributeList::default()),
        }
    }
}

impl Class for Message {
    type Interfaces = (IMessage,);
}

impl IMessageTrait for Message {
    unsafe fn getMessageID(&self) -> FIDString {
        // The string lives until the id is replaced or the message dies.
        self.id
            .lock()
            .ok()
            .and_then(|id| id.as_ref().map(|c| c.as_ptr()))
            .unwrap_or(std::ptr::null())
    }

    unsafe fn setMessageID(&self, id: FIDString) {
        let value = (!id.is_null())
            // SAFETY: a zero-terminated string from the plugin.
            .then(|| unsafe { CStr::from_ptr(id) }.to_owned());
        if let Ok(mut m) = self.id.lock() {
            *m = value;
        }
    }

    unsafe fn getAttributes(&self) -> *mut IAttributeList {
        // Not add-ref'ed: the list belongs to the message.
        self.attributes
            .as_com_ref::<IAttributeList>()
            .map_or(std::ptr::null_mut(), |r| r.as_ptr())
    }
}

enum Attr {
    Int(i64),
    Float(f64),
    Str(Vec<TChar>),
    Bin(Vec<u8>),
}

#[derive(Default)]
pub struct AttributeList {
    values: Mutex<HashMap<Vec<u8>, Attr>>,
}

impl Class for AttributeList {
    type Interfaces = (IAttributeList,);
}

fn key(id: *const c_char) -> Option<Vec<u8>> {
    // SAFETY: attribute ids are zero-terminated strings.
    (!id.is_null()).then(|| unsafe { CStr::from_ptr(id) }.to_bytes().to_vec())
}

impl AttributeList {
    fn set(&self, id: *const c_char, v: Attr) -> tresult {
        let Some(k) = key(id) else {
            return kInvalidArgument;
        };
        if let Ok(mut m) = self.values.lock() {
            m.insert(k, v);
        }
        kResultOk
    }
}

impl IAttributeListTrait for AttributeList {
    unsafe fn setInt(&self, id: *const c_char, value: int64) -> tresult {
        self.set(id, Attr::Int(value))
    }

    unsafe fn getInt(&self, id: *const c_char, value: *mut int64) -> tresult {
        let (Some(k), false) = (key(id), value.is_null()) else {
            return kInvalidArgument;
        };
        match self.values.lock().ok().and_then(|m| match m.get(&k) {
            Some(Attr::Int(v)) => Some(*v),
            _ => None,
        }) {
            Some(v) => {
                // SAFETY: checked for null.
                unsafe { *value = v };
                kResultOk
            }
            None => kResultFalse,
        }
    }

    unsafe fn setFloat(&self, id: *const c_char, value: f64) -> tresult {
        self.set(id, Attr::Float(value))
    }

    unsafe fn getFloat(&self, id: *const c_char, value: *mut f64) -> tresult {
        let (Some(k), false) = (key(id), value.is_null()) else {
            return kInvalidArgument;
        };
        match self.values.lock().ok().and_then(|m| match m.get(&k) {
            Some(Attr::Float(v)) => Some(*v),
            _ => None,
        }) {
            Some(v) => {
                // SAFETY: checked for null.
                unsafe { *value = v };
                kResultOk
            }
            None => kResultFalse,
        }
    }

    unsafe fn setString(&self, id: *const c_char, string: *const TChar) -> tresult {
        if string.is_null() {
            return kInvalidArgument;
        }
        let mut v = Vec::new();
        // SAFETY: a zero-terminated UTF-16 string from the plugin.
        unsafe {
            let mut p = string;
            while *p != 0 {
                v.push(*p);
                p = p.add(1);
            }
        }
        self.set(id, Attr::Str(v))
    }

    unsafe fn getString(&self, id: *const c_char, string: *mut TChar, size: uint32) -> tresult {
        let (Some(k), false) = (key(id), string.is_null()) else {
            return kInvalidArgument;
        };
        let Ok(m) = self.values.lock() else {
            return kResultFalse;
        };
        let Some(Attr::Str(v)) = m.get(&k) else {
            return kResultFalse;
        };
        let room = (size as usize / std::mem::size_of::<TChar>()).saturating_sub(1);
        let n = v.len().min(room);
        // SAFETY: the caller's buffer holds `size` bytes.
        unsafe {
            std::ptr::copy_nonoverlapping(v.as_ptr(), string, n);
            *string.add(n) = 0;
        }
        kResultOk
    }

    unsafe fn setBinary(&self, id: *const c_char, data: *const c_void, size: uint32) -> tresult {
        let bytes = if data.is_null() || size == 0 {
            Vec::new()
        } else {
            // SAFETY: the plugin passes `size` readable bytes.
            unsafe { std::slice::from_raw_parts(data as *const u8, size as usize) }.to_vec()
        };
        self.set(id, Attr::Bin(bytes))
    }

    unsafe fn getBinary(
        &self,
        id: *const c_char,
        data: *mut *const c_void,
        size: *mut uint32,
    ) -> tresult {
        let (Some(k), false, false) = (key(id), data.is_null(), size.is_null()) else {
            return kInvalidArgument;
        };
        let Ok(m) = self.values.lock() else {
            return kResultFalse;
        };
        let Some(Attr::Bin(v)) = m.get(&k) else {
            return kResultFalse;
        };
        // The pointer stays valid while the attribute is unchanged.
        // SAFETY: checked for null.
        unsafe {
            *data = v.as_ptr() as *const c_void;
            *size = v.len() as uint32;
        }
        kResultOk
    }
}

/// An in-memory `IBStream` (plugin state).
#[derive(Default)]
pub struct MemoryStream {
    inner: Mutex<(Vec<u8>, usize)>,
}

impl MemoryStream {
    pub fn with_data(data: Vec<u8>) -> ComWrapper<MemoryStream> {
        ComWrapper::new(MemoryStream {
            inner: Mutex::new((data, 0)),
        })
    }

    pub fn empty() -> ComWrapper<MemoryStream> {
        Self::with_data(Vec::new())
    }

    pub fn data(&self) -> Vec<u8> {
        self.inner.lock().map(|i| i.0.clone()).unwrap_or_default()
    }

    pub fn rewind(&self) {
        if let Ok(mut i) = self.inner.lock() {
            i.1 = 0;
        }
    }
}

/// The stream as the pointer `getState`/`setState` take.
pub fn stream_ptr(s: &ComWrapper<MemoryStream>) -> *mut IBStream {
    s.as_com_ref::<IBStream>()
        .map_or(std::ptr::null_mut(), |r| r.as_ptr())
}

impl Class for MemoryStream {
    type Interfaces = (IBStream, ISizeableStream);
}

impl IBStreamTrait for MemoryStream {
    unsafe fn read(&self, buffer: *mut c_void, n: int32, read: *mut int32) -> tresult {
        let Ok(mut i) = self.inner.lock() else {
            return kResultFalse;
        };
        let (data, pos) = &mut *i;
        let count = (n.max(0) as usize).min(data.len().saturating_sub(*pos));
        if count > 0 && !buffer.is_null() {
            // SAFETY: the caller's buffer holds `n` bytes.
            unsafe {
                std::ptr::copy_nonoverlapping(data.as_ptr().add(*pos), buffer as *mut u8, count)
            };
        }
        *pos += count;
        if !read.is_null() {
            // SAFETY: checked for null.
            unsafe { *read = count as int32 };
        }
        kResultOk
    }

    unsafe fn write(&self, buffer: *mut c_void, n: int32, written: *mut int32) -> tresult {
        let Ok(mut i) = self.inner.lock() else {
            return kResultFalse;
        };
        let (data, pos) = &mut *i;
        let count = n.max(0) as usize;
        if count > 0 && !buffer.is_null() {
            // SAFETY: the plugin passes `n` readable bytes.
            let src = unsafe { std::slice::from_raw_parts(buffer as *const u8, count) };
            let end = *pos + count;
            if data.len() < end {
                data.resize(end, 0);
            }
            data[*pos..end].copy_from_slice(src);
            *pos = end;
        }
        if !written.is_null() {
            // SAFETY: checked for null.
            unsafe { *written = count as int32 };
        }
        kResultOk
    }

    unsafe fn seek(&self, pos: int64, mode: int32, result: *mut int64) -> tresult {
        use vst3::Steinberg::IBStream_::IStreamSeekMode_::{kIBSeekCur, kIBSeekEnd, kIBSeekSet};
        let Ok(mut i) = self.inner.lock() else {
            return kResultFalse;
        };
        let base = match mode as u32 {
            m if m == kIBSeekSet => 0,
            m if m == kIBSeekCur => i.1 as i64,
            m if m == kIBSeekEnd => i.0.len() as i64,
            _ => return kInvalidArgument,
        };
        let new = (base + pos).max(0) as usize;
        i.1 = new;
        if !result.is_null() {
            // SAFETY: checked for null.
            unsafe { *result = new as int64 };
        }
        kResultOk
    }

    unsafe fn tell(&self, pos: *mut int64) -> tresult {
        if pos.is_null() {
            return kInvalidArgument;
        }
        let p = self.inner.lock().map_or(0, |i| i.1);
        // SAFETY: checked for null.
        unsafe { *pos = p as int64 };
        kResultOk
    }
}

impl ISizeableStreamTrait for MemoryStream {
    unsafe fn getStreamSize(&self, size: *mut int64) -> tresult {
        if size.is_null() {
            return kInvalidArgument;
        }
        let n = self.inner.lock().map_or(0, |i| i.0.len());
        // SAFETY: checked for null.
        unsafe { *size = n as int64 };
        kResultOk
    }

    unsafe fn setStreamSize(&self, size: int64) -> tresult {
        if let Ok(mut i) = self.inner.lock() {
            i.0.resize(size.max(0) as usize, 0);
            i.1 = i.1.min(i.0.len());
        }
        kResultOk
    }
}
