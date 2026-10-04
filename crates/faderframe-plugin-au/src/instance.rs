//! The control-thread side of an Audio Unit: parameters, state (the unit's
//! `ClassInfo` property list), `.aupreset` files, latency and tail, and
//! the editor view.

// Apple's constant names appear in match patterns.
#![allow(non_upper_case_globals)]

use crate::ffi::*;
use crate::processor::{AuProcessor, MAX_CHANNELS, RtState, SharedRt};
use crate::view;
use faderframe_core::ParameterId;
use faderframe_plugin_host::scan::ScannedPlugin;
use faderframe_plugin_host::{
    EditorEdit, EditorRequests, ParameterInfo, ParameterUnit, ParentWindow, PluginDescriptor,
    PluginEditor, PluginError, PluginFormat, PluginInstance, PluginPoll, PluginProcessor,
    ProcessConfig, TailLength, WindowApi,
};
use faderframe_realtime::TryCell;
use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use std::cell::RefCell;
use std::ffi::{CStr, c_void};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

fn failed(what: &str, status: OSStatus) -> PluginError {
    PluginError::Failed(format!("{what} (OSStatus {status})"))
}

/// Flags the unit's property listener raises (any thread).
#[derive(Default)]
struct Notices {
    restart: AtomicBool,
    params: AtomicBool,
}

unsafe extern "C" fn on_property(user: *mut c_void, _unit: AudioUnit, id: u32, _: u32, _: u32) {
    // SAFETY: `user` is the instance's notices, alive until the listeners
    // are removed in Drop.
    let Some(n) = (unsafe { user.cast::<Notices>().as_ref() }) else {
        return;
    };
    match id {
        kAudioUnitProperty_Latency => n.restart.store(true, Ordering::Release),
        kAudioUnitProperty_ParameterList | kAudioUnitProperty_ParameterInfo => {
            n.params.store(true, Ordering::Release);
        }
        _ => {}
    }
}

const WATCHED: [u32; 3] = [
    kAudioUnitProperty_Latency,
    kAudioUnitProperty_ParameterList,
    kAudioUnitProperty_ParameterInfo,
];

/// Parameter moves in the unit's own editor (delivered on the main run
/// loop).
#[derive(Default)]
struct Edits {
    queue: RefCell<Vec<EditorEdit>>,
}

unsafe extern "C" fn on_event(
    ref_con: *mut c_void,
    _object: *mut c_void,
    event: *const AudioUnitEvent,
    _host_time: u64,
    value: f32,
) {
    // SAFETY: `ref_con` is the instance's edit queue, alive until the
    // listener is disposed; `event` is valid for the call.
    let (Some(edits), Some(e)) = (unsafe { ref_con.cast::<Edits>().as_ref() }, unsafe {
        event.as_ref()
    }) else {
        return;
    };
    let id = ParameterId(e.mParameter.mParameterID);
    let edit = match e.mEventType {
        kAudioUnitEvent_ParameterValueChange => EditorEdit::Value(id, f64::from(value)),
        kAudioUnitEvent_BeginParameterChangeGesture => EditorEdit::Begin(id),
        kAudioUnitEvent_EndParameterChangeGesture => EditorEdit::End(id),
        _ => return,
    };
    if let Ok(mut q) = edits.queue.try_borrow_mut() {
        q.push(edit);
    }
}

struct Editor {
    view: Retained<AnyObject>,
    size: (u32, u32),
}

pub struct AuInstance {
    descriptor: PluginDescriptor,
    scanned: ScannedPlugin,
    unit: AudioUnit,
    params: Vec<ParameterInfo>,
    /// Parameters with value strings of their own.
    with_strings: Vec<u32>,
    rt: Option<SharedRt>,
    config: Option<ProcessConfig>,
    latency: u32,
    tail: TailLength,
    needs_restart: bool,
    activations: u64,
    notices: *mut Notices,
    edits: *mut Edits,
    listener: AUEventListenerRef,
    editor: Option<Editor>,
}

impl AuInstance {
    pub fn new(scanned: &ScannedPlugin) -> Result<Self, PluginError> {
        let desc = crate::scan::parse_id(&scanned.id)
            .ok_or_else(|| PluginError::NotFound(scanned.id.clone()))?;
        let component =
            crate::scan::find(&desc).ok_or_else(|| PluginError::NotFound(scanned.id.clone()))?;
        let mut unit: AudioUnit = std::ptr::null_mut();
        // SAFETY: a registered component; `unit` is written.
        let status = unsafe { AudioComponentInstanceNew(component, &mut unit) };
        if status != 0 || unit.is_null() {
            return Err(failed(
                &format!("{}: cannot instantiate", scanned.name),
                status,
            ));
        }
        let notices = Box::into_raw(Box::<Notices>::default());
        for id in WATCHED {
            // SAFETY: `notices` stays alive until removed in Drop.
            unsafe { AudioUnitAddPropertyListener(unit, id, on_property, notices.cast()) };
        }
        let mut s = Self {
            descriptor: scanned.descriptor(PluginFormat::AudioUnit),
            scanned: scanned.clone(),
            unit,
            params: Vec::new(),
            with_strings: Vec::new(),
            rt: None,
            config: None,
            latency: 0,
            tail: TailLength::None,
            needs_restart: false,
            activations: 0,
            notices,
            edits: Box::into_raw(Box::<Edits>::default()),
            listener: std::ptr::null_mut(),
            editor: None,
        };
        s.query_params();
        Ok(s)
    }

    fn get<T: Copy>(&self, id: u32, scope: u32, element: u32, mut value: T) -> Option<T> {
        let mut size = std::mem::size_of::<T>() as u32;
        // SAFETY: `value` has room for `size` bytes.
        let status = unsafe {
            AudioUnitGetProperty(
                self.unit,
                id,
                scope,
                element,
                (&mut value as *mut T).cast(),
                &mut size,
            )
        };
        (status == 0).then_some(value)
    }

    fn set<T>(&self, id: u32, scope: u32, element: u32, value: &T) -> OSStatus {
        // SAFETY: `value` is a valid `T` of the property's type.
        unsafe {
            AudioUnitSetProperty(
                self.unit,
                id,
                scope,
                element,
                (value as *const T).cast(),
                std::mem::size_of::<T>() as u32,
            )
        }
    }

    fn query_params(&mut self) {
        self.params.clear();
        self.with_strings.clear();
        let mut size = 0u32;
        let mut writable = 0u8;
        // SAFETY: plain property queries; buffers are sized from the info.
        let ids: Vec<u32> = unsafe {
            if AudioUnitGetPropertyInfo(
                self.unit,
                kAudioUnitProperty_ParameterList,
                kAudioUnitScope_Global,
                0,
                &mut size,
                &mut writable,
            ) != 0
            {
                return;
            }
            let mut ids = vec![0u32; size as usize / 4];
            if AudioUnitGetProperty(
                self.unit,
                kAudioUnitProperty_ParameterList,
                kAudioUnitScope_Global,
                0,
                ids.as_mut_ptr().cast(),
                &mut size,
            ) != 0
            {
                return;
            }
            ids.truncate(size as usize / 4);
            ids
        };
        for id in ids {
            // SAFETY: an all-zero AudioUnitParameterInfo is valid (null
            // strings); the element of ParameterInfo is the parameter id.
            let zero: AudioUnitParameterInfo = unsafe { std::mem::zeroed() };
            let Some(info) = self.get(
                kAudioUnitProperty_ParameterInfo,
                kAudioUnitScope_Global,
                id,
                zero,
            ) else {
                continue;
            };
            let has_cf = info.flags & kAudioUnitParameterFlag_HasCFNameString != 0;
            let name = if has_cf && !info.cfNameString.is_null() {
                cf_string(info.cfNameString)
            } else {
                // SAFETY: a NUL-terminated C string within the array.
                let raw = unsafe { CStr::from_ptr(info.name.as_ptr()) };
                raw.to_string_lossy().into_owned()
            };
            if info.flags & kAudioUnitParameterFlag_CFNameRelease != 0 {
                if has_cf {
                    release(info.cfNameString);
                }
                if info.unit == kAudioUnitParameterUnit_CustomUnit {
                    release(info.unitName);
                }
            }
            if info.flags & kAudioUnitParameterFlag_IsWritable == 0 {
                continue; // meters
            }
            if info.flags & kAudioUnitParameterFlag_ValuesHaveStrings != 0 {
                self.with_strings.push(id);
            }
            self.params.push(ParameterInfo {
                id: ParameterId(id),
                name: if name.is_empty() {
                    format!("Parameter {id}")
                } else {
                    name
                },
                min: f64::from(info.minValue),
                max: f64::from(info.maxValue.max(info.minValue)),
                default: f64::from(info.defaultValue),
                unit: match info.unit {
                    kAudioUnitParameterUnit_Decibels => ParameterUnit::Decibels,
                    kAudioUnitParameterUnit_Milliseconds => ParameterUnit::Milliseconds,
                    kAudioUnitParameterUnit_Hertz => ParameterUnit::Hertz,
                    kAudioUnitParameterUnit_SampleFrames => ParameterUnit::Samples,
                    _ => ParameterUnit::None,
                },
                automatable: info.flags & kAudioUnitParameterFlag_NonRealTime == 0,
                stepped: matches!(
                    info.unit,
                    kAudioUnitParameterUnit_Indexed | kAudioUnitParameterUnit_Boolean
                ),
            });
        }
    }

    /// Set the stream format of one bus to the first channel count it
    /// takes; returns that count (0: none).
    fn set_format(&self, scope: u32, rate: f64, choices: &[u32]) -> usize {
        for &ch in choices {
            let fmt = AudioStreamBasicDescription {
                mSampleRate: rate,
                mFormatID: kAudioFormatLinearPCM,
                mFormatFlags: kAudioFormatFlags_FloatNonInterleaved,
                mBytesPerPacket: 4,
                mFramesPerPacket: 1,
                mBytesPerFrame: 4,
                mChannelsPerFrame: ch,
                mBitsPerChannel: 32,
                mReserved: 0,
            };
            if self.set(kAudioUnitProperty_StreamFormat, scope, 0, &fmt) == 0 {
                return ch as usize;
            }
        }
        0
    }

    fn activate(&mut self, config: &ProcessConfig) -> Result<(), PluginError> {
        self.deactivate();
        let max = config.max_block_size.max(1);
        self.set(
            kAudioUnitProperty_MaximumFramesPerSlice,
            kAudioUnitScope_Global,
            0,
            &max,
        );
        let wanted = |ports: &[u16]| {
            ports
                .first()
                .map_or(0, |&c| u32::from(c))
                .min(MAX_CHANNELS as u32)
        };
        let out_ch = wanted(&self.scanned.audio_outputs).max(2);
        let outputs = self.set_format(kAudioUnitScope_Output, config.sample_rate, &[out_ch, 2, 1]);
        if outputs == 0 {
            return Err(PluginError::Failed(format!(
                "{}: no float output format at {} Hz",
                self.scanned.name, config.sample_rate
            )));
        }
        let has_input = self
            .get(
                kAudioUnitProperty_ElementCount,
                kAudioUnitScope_Input,
                0,
                0u32,
            )
            .unwrap_or(0)
            > 0;
        let inputs = if has_input {
            let in_ch = wanted(&self.scanned.audio_inputs).max(2);
            self.set_format(kAudioUnitScope_Input, config.sample_rate, &[in_ch, 2, 1])
        } else {
            0
        };
        let state = RtState::new(
            self.unit,
            inputs,
            outputs,
            max as usize,
            self.descriptor.note_inputs > 0,
        );
        if inputs > 0 {
            let cb = state.render_callback();
            let s = self.set(
                kAudioUnitProperty_SetRenderCallback,
                kAudioUnitScope_Input,
                0,
                &cb,
            );
            if s != 0 {
                return Err(failed(&format!("{}: input callback", self.scanned.name), s));
            }
        }
        // Optional (not every unit asks for the host's transport).
        let _ = self.set(
            kAudioUnitProperty_HostCallbacks,
            kAudioUnitScope_Global,
            0,
            &state.host_callbacks(),
        );
        // SAFETY: a configured, uninitialised unit.
        let status = unsafe { AudioUnitInitialize(self.unit) };
        if status != 0 {
            return Err(failed(
                &format!("{}: cannot initialise", self.scanned.name),
                status,
            ));
        }
        self.rt = Some(Arc::new(TryCell::new(state)));
        self.config = Some(*config);
        let seconds = |s: f64| (s.max(0.0) * config.sample_rate).round();
        self.latency = self
            .get(
                kAudioUnitProperty_Latency,
                kAudioUnitScope_Global,
                0,
                0.0f64,
            )
            .map_or(0, |s| seconds(s).min(f64::from(u32::MAX)) as u32);
        self.tail = match self.get(
            kAudioUnitProperty_TailTime,
            kAudioUnitScope_Global,
            0,
            0.0f64,
        ) {
            None => TailLength::None,
            Some(s) if s <= 0.0 => TailLength::None,
            Some(s) if s > 3600.0 => TailLength::Infinite,
            Some(s) => TailLength::Samples(seconds(s) as u32),
        };
        self.activations += 1;
        Ok(())
    }

    /// Take the processor from the graph and uninitialise the unit.
    pub fn deactivate(&mut self) {
        let Some(cell) = self.rt.take() else { return };
        self.config = None;
        // The audio thread holds the cell for at most one block.
        let reclaimed = match cell.lock_blocking(10_000) {
            Some(mut g) => {
                g.unit = None;
                true
            }
            None => false,
        };
        if !reclaimed {
            tracing::error!(
                "{}: cannot reclaim the processor; leaking it",
                self.scanned.name
            );
            std::mem::forget(cell);
            return;
        }
        // SAFETY: no thread renders any more (the cell says so).
        unsafe { AudioUnitUninitialize(self.unit) };
    }

    /// Run `f` while the audio thread keeps away from the unit.
    fn exclusive<R>(&self, f: impl FnOnce() -> R) -> R {
        let guard = self.rt.as_ref().and_then(|c| c.lock_blocking(10_000));
        let r = f();
        drop(guard);
        r
    }

    /// Tell the unit's own editor that values changed (not our listener).
    fn notify_ui(&self, parameter: u32) {
        let p = AudioUnitParameter {
            mAudioUnit: self.unit,
            mParameterID: parameter,
            mScope: kAudioUnitScope_Global,
            mElement: 0,
        };
        // SAFETY: plain notification; the sender (our listener, if any) is
        // excluded.
        unsafe { AUParameterListenerNotify(self.listener, std::ptr::null_mut(), &p) };
    }

    fn start_listening(&mut self) {
        if !self.listener.is_null() {
            return;
        }
        let mut listener: AUEventListenerRef = std::ptr::null_mut();
        // SAFETY: the edit queue lives until the listener is disposed;
        // events arrive on the main run loop (the UI thread).
        let status = unsafe {
            AUEventListenerCreate(
                on_event,
                self.edits.cast(),
                CFRunLoopGetMain(),
                kCFRunLoopDefaultMode,
                0.02,
                0.0,
                &mut listener,
            )
        };
        if status != 0 || listener.is_null() {
            return;
        }
        for p in &self.params {
            for kind in [
                kAudioUnitEvent_ParameterValueChange,
                kAudioUnitEvent_BeginParameterChangeGesture,
                kAudioUnitEvent_EndParameterChangeGesture,
            ] {
                let event = AudioUnitEvent {
                    mEventType: kind,
                    mParameter: AudioUnitParameter {
                        mAudioUnit: self.unit,
                        mParameterID: p.id.0,
                        mScope: kAudioUnitScope_Global,
                        mElement: 0,
                    },
                };
                // SAFETY: a live listener and unit.
                unsafe { AUEventListenerAddEventType(listener, std::ptr::null_mut(), &event) };
            }
        }
        self.listener = listener;
    }

    fn stop_listening(&mut self) {
        if !self.listener.is_null() {
            // SAFETY: created by `start_listening`, disposed once.
            unsafe { AUListenerDispose(self.listener) };
            self.listener = std::ptr::null_mut();
        }
    }

    fn preset_dirs(&self) -> Vec<PathBuf> {
        let sub = PathBuf::from(&self.scanned.vendor).join(&self.scanned.name);
        let mut dirs = vec![PathBuf::from("/Library/Audio/Presets").join(&sub)];
        if let Some(home) = std::env::var_os("HOME") {
            dirs.insert(
                0,
                PathBuf::from(home).join("Library/Audio/Presets").join(&sub),
            );
        }
        dirs
    }
}

impl Drop for AuInstance {
    fn drop(&mut self) {
        PluginEditor::close(self);
        self.deactivate();
        // SAFETY: removes the listeners registered in `new` before their
        // data is freed, then disposes the unit (nothing renders: the
        // processor was reclaimed above).
        unsafe {
            for id in WATCHED {
                AudioUnitRemovePropertyListenerWithUserData(
                    self.unit,
                    id,
                    on_property,
                    self.notices.cast(),
                );
            }
            AudioComponentInstanceDispose(self.unit);
            drop(Box::from_raw(self.notices));
            drop(Box::from_raw(self.edits));
        }
    }
}

/// Property-list bytes → a CF property list (owned; null if invalid).
fn plist_from(data: &[u8]) -> CFPropertyListRef {
    // SAFETY: plain CoreFoundation calls on a byte slice; the intermediate
    // CFData is released here.
    unsafe {
        let d = CFDataCreate(std::ptr::null(), data.as_ptr(), data.len() as CFIndex);
        if d.is_null() {
            return std::ptr::null();
        }
        let p = CFPropertyListCreateWithData(
            std::ptr::null(),
            d,
            kCFPropertyListImmutable,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        );
        release(d);
        p
    }
}

impl PluginInstance for AuInstance {
    fn descriptor(&self) -> &PluginDescriptor {
        &self.descriptor
    }

    fn parameters(&self) -> &[ParameterInfo] {
        &self.params
    }

    fn parameter(&mut self, id: ParameterId) -> Option<f64> {
        let mut v = 0.0f32;
        // SAFETY: plain query.
        let status =
            unsafe { AudioUnitGetParameter(self.unit, id.0, kAudioUnitScope_Global, 0, &mut v) };
        (status == 0).then_some(f64::from(v))
    }

    fn set_parameter(&mut self, id: ParameterId, value: f64) -> Result<(), PluginError> {
        let info = self
            .params
            .iter()
            .find(|p| p.id == id)
            .ok_or(PluginError::UnknownParameter(id))?;
        let v = info.clamp(value) as f32;
        // SAFETY: Audio Unit parameters may be set from any thread.
        let status =
            unsafe { AudioUnitSetParameter(self.unit, id.0, kAudioUnitScope_Global, 0, v, 0) };
        if status != 0 {
            return Err(failed("set parameter", status));
        }
        self.notify_ui(id.0);
        Ok(())
    }

    fn latency_samples(&self) -> u32 {
        self.latency
    }

    fn activation(&self) -> u64 {
        self.activations
    }

    fn tail(&self) -> TailLength {
        self.tail
    }

    fn save_state(&mut self) -> Result<Vec<u8>, PluginError> {
        let plist = self
            .get(
                kAudioUnitProperty_ClassInfo,
                kAudioUnitScope_Global,
                0,
                std::ptr::null::<c_void>(),
            )
            .filter(|p| !p.is_null())
            .ok_or_else(|| PluginError::Failed("the unit has no state".into()))?;
        // SAFETY: `plist` is ours (a Copy result); the data is released
        // after copying.
        let bytes = unsafe {
            let data = CFPropertyListCreateData(
                std::ptr::null(),
                plist,
                kCFPropertyListBinaryFormat_v1_0,
                0,
                std::ptr::null_mut(),
            );
            let bytes = cf_data(data);
            release(data);
            bytes
        };
        release(plist);
        if bytes.is_empty() {
            return Err(PluginError::Failed("cannot serialise the state".into()));
        }
        Ok(bytes)
    }

    fn load_state(&mut self, data: &[u8]) -> Result<(), PluginError> {
        let plist = plist_from(data);
        if plist.is_null() {
            return Err(PluginError::InvalidState("not a property list".into()));
        }
        let status = self.exclusive(|| {
            self.set(
                kAudioUnitProperty_ClassInfo,
                kAudioUnitScope_Global,
                0,
                &plist,
            )
        });
        release(plist);
        if status != 0 {
            return Err(PluginError::InvalidState(format!(
                "the unit refused the state (OSStatus {status})"
            )));
        }
        self.notify_ui(kAUParameterListener_AnyParameter);
        self.query_params();
        Ok(())
    }

    fn preset_files(&self) -> Vec<PathBuf> {
        let mut out = Vec::new();
        for dir in self.preset_dirs() {
            let Ok(entries) = std::fs::read_dir(&dir) else {
                continue;
            };
            for e in entries.flatten() {
                let p = e.path();
                if p.extension()
                    .is_some_and(|x| x.eq_ignore_ascii_case("aupreset"))
                {
                    out.push(p);
                }
            }
        }
        out.sort();
        out
    }

    fn state_from_preset_file(&self, data: &[u8]) -> Result<Vec<u8>, PluginError> {
        // An .aupreset is the unit's ClassInfo as a property list: the
        // state format itself.
        let plist = plist_from(data);
        if plist.is_null() {
            return Err(PluginError::InvalidState("not an .aupreset file".into()));
        }
        release(plist);
        Ok(data.to_vec())
    }

    fn take_editor_edits(&mut self) -> Vec<EditorEdit> {
        // SAFETY: the queue lives as long as the instance.
        let edits = unsafe { &*self.edits };
        let mut q = match edits.queue.try_borrow_mut() {
            Ok(q) => q,
            Err(_) => return Vec::new(),
        };
        let known = |id: ParameterId| self.params.iter().any(|p| p.id == id);
        q.drain(..)
            .filter(|e| match *e {
                EditorEdit::Begin(id) | EditorEdit::End(id) | EditorEdit::Value(id, _) => known(id),
            })
            .collect()
    }

    fn poll(&mut self) -> PluginPoll {
        // SAFETY: the notices live as long as the instance.
        let n = unsafe { &*self.notices };
        let restart = n.restart.swap(false, Ordering::AcqRel);
        let params_changed = n.params.swap(false, Ordering::AcqRel);
        if restart {
            self.needs_restart = true;
        }
        if params_changed {
            self.query_params();
        }
        PluginPoll {
            restart,
            params_changed,
            state_dirty: false,
        }
    }

    fn editor(&mut self) -> Option<&mut dyn PluginEditor> {
        Some(self)
    }

    fn format_parameter(&mut self, id: ParameterId, value: f64) -> Option<String> {
        if !self.with_strings.contains(&id.0) {
            return None;
        }
        let v = value as f32;
        let query = AudioUnitParameterStringFromValue {
            inParamID: id.0,
            inValue: &v,
            outString: std::ptr::null(),
        };
        let r = self.get(
            kAudioUnitProperty_ParameterStringFromValue,
            kAudioUnitScope_Global,
            0,
            query,
        )?;
        let text = cf_string(r.outString);
        release(r.outString);
        (!text.trim().is_empty()).then(|| text.trim().to_string())
    }

    fn create_processor(
        &mut self,
        config: &ProcessConfig,
    ) -> Result<Box<dyn PluginProcessor>, PluginError> {
        if self.config.as_ref() != Some(config) || self.rt.is_none() || self.needs_restart {
            self.needs_restart = false;
            self.activate(config)?;
        }
        let cell = self
            .rt
            .as_ref()
            .map(Arc::clone)
            .ok_or_else(|| PluginError::Failed("not active".into()))?;
        Ok(Box::new(AuProcessor { cell }))
    }
}

impl PluginEditor for AuInstance {
    fn can_embed(&mut self, api: WindowApi) -> bool {
        api == WindowApi::Cocoa
    }

    fn can_float(&mut self, _api: WindowApi) -> bool {
        false
    }

    fn open_embedded(&mut self, api: WindowApi, _scale: f64) -> Result<(u32, u32), PluginError> {
        if api != WindowApi::Cocoa {
            return Err(PluginError::Failed("Audio Units have Cocoa editors".into()));
        }
        self.close();
        let v = view::create(self.unit)
            .ok_or_else(|| PluginError::Failed("the unit has no editor view".into()))?;
        let size = view::size_of(&v);
        self.editor = Some(Editor { view: v, size });
        self.start_listening();
        Ok(size)
    }

    fn attach(&mut self, parent: ParentWindow) -> Result<(), PluginError> {
        let ed = self
            .editor
            .as_ref()
            .ok_or_else(|| PluginError::Failed("no editor".into()))?;
        if parent.api != WindowApi::Cocoa || !view::attach(&ed.view, parent.handle) {
            self.close();
            return Err(PluginError::Failed("cannot attach the editor view".into()));
        }
        Ok(())
    }

    fn open_floating(&mut self, _api: WindowApi, _title: &str) -> Result<(), PluginError> {
        Err(PluginError::Failed(
            "Audio Unit editors are embedded".into(),
        ))
    }

    fn close(&mut self) {
        if let Some(ed) = self.editor.take() {
            view::detach(&ed.view);
        }
        self.stop_listening();
    }

    fn is_open(&self) -> bool {
        self.editor.is_some()
    }

    fn can_resize(&mut self) -> bool {
        false
    }

    fn set_size(&mut self, width: u32, height: u32) -> Option<(u32, u32)> {
        let ed = self.editor.as_mut()?;
        view::set_size(&ed.view, (width, height));
        ed.size = view::size_of(&ed.view);
        Some(ed.size)
    }

    fn take_requests(&mut self) -> EditorRequests {
        let mut r = EditorRequests::default();
        // Views resize themselves (e.g. a plugin's zoom): follow.
        if let Some(ed) = self.editor.as_mut() {
            let now = view::size_of(&ed.view);
            if now != ed.size {
                ed.size = now;
                r.resize = Some(now);
            }
        }
        r
    }
}

/// Whether editors can be shown at all (CoreAudioKit is present).
pub fn has_generic_editor() -> bool {
    view::has_generic()
}
