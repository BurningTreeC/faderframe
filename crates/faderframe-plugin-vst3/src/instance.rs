//! The control-thread side of a VST3 plugin instance.
//!
//! Set-up follows the VST3 host workflow: create and initialise the
//! component, find its edit controller (the same object, or a separate one
//! created from the controller class id and joined through connection
//! points), give the controller the component's state and the host's
//! component handler. Activation configures buses and processing and hands
//! the processor to the audio side ([`crate::processor`]).
//!
//! Parameters: VST3 values are normalised (0–1). FaderFrame shows
//! continuous ones as 0–1 and stepped ones as integers 0…steps (converted
//! at the boundary); the plugin formats values itself. Edits made in the
//! plugin's editor (`performEdit`) are forwarded to the processor, and
//! values the processor reports back are forwarded to the controller.

use crate::com::{Edit, HostApp, HostState, MemoryStream, context, stream_ptr};
use crate::module::Module;
use crate::processor::{
    Active, ActiveConfig, CONTROLLERS, MidiMap, ParamMap, SharedRt, Vst3Processor,
};
use crate::util::{parse_tuid, wstr};
use faderframe_core::ParameterId;
use faderframe_midi::NoteExpressionKind;
use faderframe_plugin_host::emulated::{ModBases, append_bases, read_bases};
use faderframe_plugin_host::scan::ScannedPlugin;
use faderframe_plugin_host::{
    EditorRequests, ParameterInfo, ParameterUnit, ParentWindow, PluginDescriptor, PluginEditor,
    PluginError, PluginEventSources, PluginFd, PluginFormat, PluginInstance as FfInstance,
    PluginPoll, PluginProcessor, ProcessConfig, TailLength, WindowApi,
};
use faderframe_realtime::TryCell;
use std::collections::HashMap;
use std::ffi::c_void;
use std::sync::Arc;
use vst3::Steinberg::Vst::{
    BusInfo, IAudioProcessor, IAudioProcessorTrait, IComponent, IComponentHandler, IComponentTrait,
    IConnectionPoint, IConnectionPointTrait, IEditController, IEditControllerTrait, IMidiMapping,
    IMidiMappingTrait, IUnitInfo, IUnitInfoTrait, ParamID, ParamValue, ProcessSetup,
    SpeakerArrangement, String128, UnitInfo, kNoParamId,
};
use vst3::Steinberg::{
    IPlugFrame, IPlugView, IPlugViewContentScaleSupport, IPlugViewContentScaleSupportTrait,
    IPlugViewTrait, IPluginBaseTrait, IPluginFactoryTrait, TUID, ViewRect, kPlatformTypeHWND,
    kPlatformTypeNSView, kPlatformTypeX11EmbedWindowID, kResultOk, kResultTrue,
};
use vst3::{ComPtr, ComWrapper, Interface};

/// Magic of FaderFrame's VST3 state blob: component state, then
/// controller state, each with a little-endian u32 length.
const STATE_MAGIC: &[u8; 4] = b"FFV3";

pub(crate) fn descriptor_of(p: &ScannedPlugin) -> PluginDescriptor {
    p.descriptor(PluginFormat::Vst3)
}

/// Create an object of class `cid` as interface `I`.
pub(crate) fn create<I: Interface>(module: &Module, cid: &TUID) -> Option<ComPtr<I>> {
    let iid = crate::util::tuid(&I::IID);
    let mut obj: *mut c_void = std::ptr::null_mut();
    // SAFETY: the factory is alive; on success `obj` holds a reference
    // owned by us.
    unsafe {
        let r = module
            .factory
            .createInstance(cid.as_ptr(), iid.as_ptr(), &mut obj);
        if r != kResultOk {
            return None;
        }
        ComPtr::from_raw(obj as *mut I)
    }
}

/// Bus channel counts (main bus first, as the plugin orders them).
pub(crate) fn bus_channels(component: &ComPtr<IComponent>, input: bool) -> Vec<u16> {
    use vst3::Steinberg::Vst::BusDirections_::{kInput, kOutput};
    use vst3::Steinberg::Vst::MediaTypes_::kAudio;
    let dir = if input { kInput } else { kOutput } as i32;
    let mut out = Vec::new();
    // SAFETY: component calls with valid out pointers.
    unsafe {
        for i in 0..component.getBusCount(kAudio as i32, dir).max(0) {
            let mut info: BusInfo = std::mem::zeroed();
            if component.getBusInfo(kAudio as i32, dir, i, &mut info) == kResultOk {
                out.push(info.channelCount.clamp(0, 64) as u16);
            }
        }
    }
    out
}

pub(crate) fn event_buses(component: &ComPtr<IComponent>, input: bool) -> u16 {
    use vst3::Steinberg::Vst::BusDirections_::{kInput, kOutput};
    use vst3::Steinberg::Vst::MediaTypes_::kEvent;
    let dir = if input { kInput } else { kOutput } as i32;
    // SAFETY: plain query.
    unsafe { component.getBusCount(kEvent as i32, dir).clamp(0, 16) as u16 }
}

pub struct Vst3Instance {
    descriptor: PluginDescriptor,
    scanned: ScannedPlugin,
    params: Vec<ParameterInfo>,
    map: Arc<ParamMap>,
    rt: Option<SharedRt>,
    config: Option<ProcessConfig>,
    to_rt: Option<rtrb::Producer<(ParamID, ParamValue)>>,
    from_rt: Option<rtrb::Consumer<(ParamID, ParamValue)>>,
    /// The controller's values after a state load (bases for modulation).
    bases_tx: Option<rtrb::Producer<(ParamID, ParamValue)>>,
    /// The bases of the parameters the processor modulates now.
    mod_bases: Arc<ModBases>,
    /// Parameters modulation may move (continuous, automatable).
    modulatable: Vec<ParamID>,
    /// The plugin's gain-reduction meter (a read-only parameter): its id,
    /// its values in dB, and the cell the processor sets.
    reduction: Option<(ParamID, Arc<faderframe_plugin_host::ReductionTable>)>,
    reduction_cell: Arc<faderframe_plugin_host::Reduction>,
    /// Changes made while inactive, sent to the processor on activation.
    pending: Vec<(ParamID, ParamValue)>,
    latency: u32,
    tail: TailLength,
    needs_restart: bool,
    /// Output buses the graph takes, and how many the active processor
    /// switched on.
    output_buses: usize,
    active_outputs: usize,
    /// Activations so far (see `PluginInstance::activation`).
    activations: u64,
    /// Editor gestures and values for automation writing.
    editor_edits: Vec<faderframe_plugin_host::EditorEdit>,
    /// The note expressions the plugin lists (queried once).
    note_expressions: Vec<NoteExpressionKind>,
    /// The program-change parameter (and its step count) with the names of
    /// its programs.
    program: Option<(ParamID, u32)>,
    programs: Vec<String>,
    view: Option<ComPtr<IPlugView>>,
    view_open: bool,
    /// Editor edits forwarded since the last poll (for "dirty").
    edited: bool,
    state: Arc<HostState>,
    connection: Option<(ComPtr<IConnectionPoint>, ComPtr<IConnectionPoint>)>,
    controller: Option<ComPtr<IEditController>>,
    separate_controller: bool,
    processor: ComPtr<IAudioProcessor>,
    component: ComPtr<IComponent>,
    host: ComWrapper<HostApp>,
    _module: &'static Module,
}

impl Vst3Instance {
    pub fn new(module: &'static Module, scanned: &ScannedPlugin) -> Result<Self, PluginError> {
        let fail = |what: &str| PluginError::Failed(format!("{}: {what}", scanned.name));
        let cid = parse_tuid(&scanned.id).ok_or_else(|| fail("invalid class id"))?;
        let host = HostApp::new();
        let state = Arc::clone(&host.state);
        let component: ComPtr<IComponent> =
            create(module, &cid).ok_or_else(|| fail("cannot create the component"))?;
        // SAFETY: standard VST3 set-up calls on live objects (control
        // thread), in the order the SDK's own host uses.
        unsafe {
            if component.initialize(context(&host)) != kResultOk {
                return Err(fail("the component failed to initialise"));
            }
        }
        let processor: ComPtr<IAudioProcessor> = match component.cast() {
            Some(p) => p,
            None => {
                // SAFETY: as above.
                unsafe { component.terminate() };
                return Err(fail("the component has no audio processor"));
            }
        };
        let (controller, separate) = match component.cast::<IEditController>() {
            Some(c) => (Some(c), false),
            None => {
                let mut ccid: TUID = [0; 16];
                // SAFETY: as above.
                let c = unsafe {
                    (component.getControllerClassId(&mut ccid) == kResultOk)
                        .then(|| create::<IEditController>(module, &ccid))
                        .flatten()
                        .filter(|c| c.initialize(context(&host)) == kResultOk)
                };
                (c, true)
            }
        };
        let mut s = Self {
            descriptor: descriptor_of(scanned),
            scanned: scanned.clone(),
            params: Vec::new(),
            map: Arc::default(),
            rt: None,
            config: None,
            to_rt: None,
            from_rt: None,
            bases_tx: None,
            mod_bases: Arc::default(),
            modulatable: Vec::new(),
            reduction: None,
            reduction_cell: faderframe_plugin_host::Reduction::new(),
            pending: Vec::new(),
            latency: 0,
            tail: TailLength::None,
            needs_restart: false,
            output_buses: 1,
            active_outputs: 1,
            activations: 0,
            editor_edits: Vec::new(),
            note_expressions: Vec::new(),
            program: None,
            programs: Vec::new(),
            view: None,
            view_open: false,
            edited: false,
            state,
            connection: None,
            controller,
            separate_controller: separate,
            processor,
            component,
            host,
            _module: module,
        };
        if let Some(ctrl) = s.controller.clone() {
            if separate
                && let (Some(a), Some(b)) = (
                    s.component.cast::<IConnectionPoint>(),
                    ctrl.cast::<IConnectionPoint>(),
                )
            {
                // SAFETY: both connection points are alive while connected.
                unsafe {
                    a.connect(b.as_ptr());
                    b.connect(a.as_ptr());
                }
                s.connection = Some((a, b));
            }
            if let Some(handler) = s.host.as_com_ref::<IComponentHandler>() {
                // SAFETY: the host object outlives the controller.
                unsafe { ctrl.setComponentHandler(handler.as_ptr()) };
            }
            // The controller starts from the component's state.
            if let Some(bytes) = s.component_state() {
                let stream = MemoryStream::with_data(bytes);
                // SAFETY: stream alive for the call.
                unsafe { ctrl.setComponentState(stream_ptr(&stream)) };
            }
        }
        s.query_params();
        s.note_expressions = s.listed_note_expressions();
        Ok(s)
    }

    fn component_state(&self) -> Option<Vec<u8>> {
        let stream = MemoryStream::empty();
        // SAFETY: stream alive for the call.
        let ok = unsafe { self.component.getState(stream_ptr(&stream)) } == kResultOk;
        ok.then(|| stream.data())
    }

    fn units(&self) -> HashMap<i32, String> {
        let mut out = HashMap::new();
        let Some(units) = self.controller.as_ref().and_then(|c| c.cast::<IUnitInfo>()) else {
            return out;
        };
        // SAFETY: plain queries with valid out pointers.
        unsafe {
            for i in 0..units.getUnitCount().clamp(0, 4096) {
                let mut info: UnitInfo = std::mem::zeroed();
                if units.getUnitInfo(i, &mut info) == kResultOk && info.id != 0 {
                    out.insert(info.id, wstr(&info.name));
                }
            }
        }
        out
    }

    fn query_params(&mut self) {
        use vst3::Steinberg::Vst::ParameterInfo_::ParameterFlags_::*;
        let Some(ctrl) = self.controller.clone() else {
            self.params.clear();
            return;
        };
        let units = self.units();
        // Proxy parameters that only exist as MIDI controller targets
        // (JUCE and others add 16 × 130 of them) are not shown.
        let proxies: std::collections::HashSet<ParamID> = self
            .midi_map()
            .map(|m| m.iter().flatten().copied().collect())
            .unwrap_or_default();
        let mut out = Vec::new();
        let mut steps = Vec::new();
        let mut modulatable = Vec::new();
        // A gain-reduction meter: (id, units).
        let mut meter = None;
        // The program-change parameter (often hidden): (id, unit, steps).
        let mut program = None;
        // SAFETY: plain queries with valid out pointers.
        unsafe {
            for i in 0..ctrl.getParameterCount().clamp(0, 65536) {
                let mut info: vst3::Steinberg::Vst::ParameterInfo = std::mem::zeroed();
                if ctrl.getParameterInfo(i, &mut info) != kResultOk {
                    continue;
                }
                if info.flags & kIsProgramChange != 0 && program.is_none() {
                    program = Some((info.id, info.unitId, info.stepCount.max(0) as u32));
                }
                if info.flags & kIsReadOnly != 0
                    && meter.is_none()
                    && (faderframe_plugin_host::names_gain_reduction(&wstr(&info.title))
                        || faderframe_plugin_host::names_gain_reduction(&wstr(&info.shortTitle)))
                {
                    meter = Some((info.id, wstr(&info.units).to_ascii_lowercase()));
                }
                if info.flags & kIsHidden != 0
                    || (info.flags & kCanAutomate == 0 && proxies.contains(&info.id))
                {
                    continue;
                }
                let name = wstr(&info.title);
                let name = match units.get(&info.unitId) {
                    Some(u) if !u.is_empty() => format!("{u}/{name}"),
                    _ => name,
                };
                let s = info.stepCount.max(0) as u32;
                if s > 0 {
                    steps.push((info.id, s));
                }
                // Modulation (sent as changes) for continuous automatable
                // parameters; never bypass or program switches.
                if s == 0
                    && info.flags & kCanAutomate != 0
                    && info.flags & (kIsReadOnly | kIsBypass | kIsProgramChange) == 0
                {
                    modulatable.push(info.id);
                }
                let def = info.defaultNormalizedValue.clamp(0.0, 1.0);
                out.push(ParameterInfo {
                    id: ParameterId(info.id),
                    name,
                    min: 0.0,
                    max: if s > 0 { s as f64 } else { 1.0 },
                    default: if s > 0 { (def * s as f64).round() } else { def },
                    unit: ParameterUnit::None,
                    automatable: info.flags & kCanAutomate != 0 && info.flags & kIsReadOnly == 0,
                    stepped: s > 0,
                });
            }
        }
        self.params = out;
        self.reduction =
            meter.map(|(id, units)| (id, Arc::new(reduction_table(&ctrl, id, &units))));
        modulatable.sort_unstable();
        self.modulatable = modulatable;
        self.map = Arc::new(ParamMap::new(steps));
        self.programs = program.map_or_else(Vec::new, |(id, unit, steps)| {
            self.program_names(id, unit, steps)
        });
        self.program = program
            .filter(|_| !self.programs.is_empty())
            .map(|(id, _, steps)| (id, steps));
    }

    /// The names of the programs that parameter `id` (of `unit`, with
    /// `steps` steps) selects: the unit's program list, else the
    /// parameter's own value texts.
    fn program_names(&self, id: ParamID, unit: i32, steps: u32) -> Vec<String> {
        use vst3::Steinberg::Vst::{ProgramListInfo, kNoProgramListId};
        let Some(ctrl) = self.controller.as_ref() else {
            return Vec::new();
        };
        if let Some(units) = ctrl.cast::<IUnitInfo>() {
            // SAFETY: plain queries with valid out pointers.
            unsafe {
                let mut list = None;
                for i in 0..units.getUnitCount().clamp(0, 4096) {
                    let mut info: UnitInfo = std::mem::zeroed();
                    if units.getUnitInfo(i, &mut info) == kResultOk && info.id == unit {
                        list =
                            (info.programListId != kNoProgramListId).then_some(info.programListId);
                        break;
                    }
                }
                let lists = units.getProgramListCount().clamp(0, 1024);
                for i in 0..lists {
                    let mut info: ProgramListInfo = std::mem::zeroed();
                    if units.getProgramListInfo(i, &mut info) != kResultOk {
                        continue;
                    }
                    // The unit's list, or the only one there is.
                    if list.is_some_and(|l| l != info.id) || (list.is_none() && lists != 1) {
                        continue;
                    }
                    let names: Vec<String> = (0..info.programCount.clamp(0, 16384))
                        .map(|p| {
                            let mut name: String128 = std::mem::zeroed();
                            match units.getProgramName(info.id, p, &mut name) {
                                r if r == kResultOk => wstr(&name),
                                _ => format!("Program {}", p + 1),
                            }
                        })
                        .collect();
                    if !names.is_empty() {
                        return names;
                    }
                }
            }
        }
        // No list: what the parameter calls its values.
        (0..=steps.min(16383))
            .filter(|_| steps > 0)
            .map(|i| {
                let n = f64::from(i) / f64::from(steps);
                // SAFETY: plain query into a valid string buffer.
                unsafe {
                    let mut text: String128 = std::mem::zeroed();
                    match ctrl.getParamStringByValue(id, n, &mut text) {
                        r if r == kResultOk => wstr(&text),
                        _ => format!("Program {}", i + 1),
                    }
                }
            })
            .collect()
    }

    /// The normalised value that selects program `index`.
    fn program_value(&self, index: usize) -> Option<(ParamID, ParamValue)> {
        let (id, steps) = self.program?;
        let last = if steps > 0 {
            steps as usize
        } else {
            self.programs.len().saturating_sub(1)
        };
        (index <= last && index < self.programs.len().max(1)).then(|| {
            (
                id,
                if last == 0 {
                    0.0
                } else {
                    index as f64 / last as f64
                },
            )
        })
    }

    /// The standard note expressions the plugin lists (bus 0, channel 0),
    /// plus pressure (always as poly pressure).
    fn listed_note_expressions(&self) -> Vec<NoteExpressionKind> {
        use vst3::Steinberg::Vst::NoteExpressionTypeIDs_::*;
        use vst3::Steinberg::Vst::{
            INoteExpressionController, INoteExpressionControllerTrait, NoteExpressionTypeInfo,
        };
        let mut out = vec![NoteExpressionKind::Pressure];
        let Some(nec) = self
            .controller
            .as_ref()
            .and_then(|c| c.cast::<INoteExpressionController>())
        else {
            return out;
        };
        // SAFETY: plain queries with valid out pointers.
        let count = unsafe { nec.getNoteExpressionCount(0, 0) };
        for i in 0..count.max(0) {
            // SAFETY: plain C struct, filled by the plugin.
            let mut info: NoteExpressionTypeInfo = unsafe { std::mem::zeroed() };
            if unsafe { nec.getNoteExpressionInfo(0, 0, i, &mut info) } != kResultOk {
                continue;
            }
            let kind = match info.typeId as u32 {
                t if t == kVolumeTypeID as u32 => NoteExpressionKind::Volume,
                t if t == kPanTypeID as u32 => NoteExpressionKind::Pan,
                t if t == kTuningTypeID as u32 => NoteExpressionKind::Tuning,
                t if t == kVibratoTypeID as u32 => NoteExpressionKind::Vibrato,
                t if t == kExpressionTypeID as u32 => NoteExpressionKind::Expression,
                t if t == kBrightnessTypeID as u32 => NoteExpressionKind::Brightness,
                _ => continue,
            };
            if !out.contains(&kind) {
                out.push(kind);
            }
        }
        out
    }

    fn midi_map(&self) -> Option<Box<MidiMap>> {
        let mapping = self.controller.as_ref()?.cast::<IMidiMapping>()?;
        let mut map = Box::new([[kNoParamId; CONTROLLERS]; 16]);
        let mut any = false;
        for (ch, row) in map.iter_mut().enumerate() {
            for (cc, slot) in row.iter_mut().enumerate() {
                let mut id: ParamID = kNoParamId;
                // SAFETY: plain query with a valid out pointer.
                let r = unsafe {
                    mapping.getMidiControllerAssignment(0, ch as i16, cc as i16, &mut id)
                };
                if r == kResultOk || r == kResultTrue {
                    *slot = id;
                    any = true;
                }
            }
        }
        any.then_some(map)
    }

    /// Activate the main buses, the default-active ones, the output buses
    /// the graph takes and, with a sidechain, the second input bus.
    fn set_bus_states(&self, sidechain: bool) -> (Vec<u16>, Vec<u16>, bool) {
        use vst3::Steinberg::Vst::BusDirections_::{kInput, kOutput};
        use vst3::Steinberg::Vst::BusInfo_::BusFlags_::kDefaultActive;
        use vst3::Steinberg::Vst::BusTypes_::kMain;
        use vst3::Steinberg::Vst::MediaTypes_::{kAudio, kEvent};
        let c = &self.component;
        let inputs = bus_channels(c, true);
        let outputs = bus_channels(c, false);
        let events = event_buses(c, true) > 0;
        // SAFETY: bus configuration calls while inactive.
        unsafe {
            for (dir, count) in [(kInput, inputs.len()), (kOutput, outputs.len())] {
                for i in 0..count as i32 {
                    let mut info: BusInfo = std::mem::zeroed();
                    let side = sidechain && dir == kInput && i == 1;
                    let taken = dir == kOutput && (i as usize) < self.output_buses;
                    let active = c.getBusInfo(kAudio as i32, dir as i32, i, &mut info) == kResultOk
                        && (info.busType == kMain as i32
                            || info.flags as u32 & kDefaultActive as u32 != 0
                            || side
                            || taken);
                    c.activateBus(kAudio as i32, dir as i32, i, active as u8);
                }
            }
            if events {
                c.activateBus(kEvent as i32, kInput as i32, 0, 1);
            }
            // Keep the plugin's own arrangements, but tell it they are set.
            let arr = |dir: i32, n: usize| -> Vec<SpeakerArrangement> {
                (0..n as i32)
                    .map(|i| {
                        let mut a: SpeakerArrangement = 0;
                        self.processor.getBusArrangement(dir, i, &mut a);
                        a
                    })
                    .collect()
            };
            let mut ins = arr(kInput as i32, inputs.len());
            let mut outs = arr(kOutput as i32, outputs.len());
            self.processor.setBusArrangements(
                ins.as_mut_ptr(),
                ins.len() as i32,
                outs.as_mut_ptr(),
                outs.len() as i32,
            );
        }
        // The arrangement may have changed the channel counts.
        (bus_channels(c, true), bus_channels(c, false), events)
    }

    /// Stop processing and deactivate (graphs still holding the shared
    /// cell go silent until they get a fresh processor).
    pub fn deactivate(&mut self) {
        let Some(cell) = self.rt.take() else { return };
        self.to_rt = None;
        self.from_rt = None;
        self.bases_tx = None;
        self.config = None;
        let Some(mut guard) = cell.lock_blocking(10_000) else {
            tracing::error!(
                "{}: cannot reclaim the processor; leaking it",
                self.scanned.name
            );
            std::mem::forget(cell);
            return;
        };
        if let Some(active) = guard.take() {
            // SAFETY: holding the cell, no block is running; processing
            // state calls are allowed on the control thread.
            unsafe {
                active.processor.setProcessing(0);
                self.component.setActive(0);
            }
            drop(active);
        }
    }

    /// Every listed parameter's normalised value in the controller.
    fn controller_values(&self) -> Vec<(ParamID, ParamValue)> {
        let Some(ctrl) = &self.controller else {
            return Vec::new();
        };
        self.params
            .iter()
            // SAFETY: plain query.
            .map(|p| (p.id.0, unsafe { ctrl.getParamNormalized(p.id.0) }))
            .collect()
    }

    /// The processor's bases follow the controller (a loaded state, the
    /// plugin's own preset); modulated parameters keep theirs (the
    /// controller may show their modulated values).
    fn refresh_bases(&mut self) {
        let modulated: Vec<u32> = self
            .mod_bases
            .snapshot()
            .iter()
            .map(|(id, _)| *id)
            .collect();
        let values = self.controller_values();
        if let Some(tx) = self.bases_tx.as_mut() {
            for (id, n) in values {
                if !modulated.contains(&id) {
                    let _ = tx.push((id, n));
                }
            }
        }
    }

    fn send(&mut self, id: ParamID, n: ParamValue) {
        match self.to_rt.as_mut() {
            Some(tx) => {
                if tx.push((id, n)).is_err() {
                    tracing::warn!("{}: parameter queue full", self.scanned.name);
                }
            }
            None => {
                self.pending.retain(|(p, _)| *p != id);
                self.pending.push((id, n));
            }
        }
    }

    fn drop_view(&mut self) {
        if let Some(view) = self.view.take() {
            // SAFETY: detach the editor before releasing it.
            unsafe {
                if self.view_open {
                    view.removed();
                }
                view.setFrame(std::ptr::null_mut());
            }
        }
        self.view_open = false;
    }

    fn ensure_view(&mut self) -> Option<ComPtr<IPlugView>> {
        if self.view.is_none() {
            let ctrl = self.controller.as_ref()?;
            // SAFETY: createView returns a new reference (or null).
            let view = unsafe {
                ComPtr::from_raw(ctrl.createView(vst3::Steinberg::Vst::ViewType::kEditor))
            }?;
            self.view = Some(view);
        }
        self.view.clone()
    }

    pub fn scanned(&self) -> &ScannedPlugin {
        &self.scanned
    }
}

impl FfInstance for Vst3Instance {
    fn configure_outputs(&mut self, buses: usize) {
        self.output_buses = buses.max(1);
    }

    fn output_bus_names(&mut self) -> Vec<String> {
        use vst3::Steinberg::Vst::BusDirections_::kOutput;
        use vst3::Steinberg::Vst::MediaTypes_::kAudio;
        let c = &self.component;
        let mut out = Vec::new();
        // SAFETY: plain queries with valid out pointers.
        unsafe {
            for i in 0..c.getBusCount(kAudio as i32, kOutput as i32).max(0) {
                let mut info: BusInfo = std::mem::zeroed();
                out.push(
                    if c.getBusInfo(kAudio as i32, kOutput as i32, i, &mut info) == kResultOk {
                        crate::util::wstr(&info.name)
                    } else {
                        String::new()
                    },
                );
            }
        }
        out
    }

    fn descriptor(&self) -> &PluginDescriptor {
        &self.descriptor
    }

    fn parameters(&self) -> &[ParameterInfo] {
        &self.params
    }

    fn reduction(&self) -> Option<Arc<faderframe_plugin_host::Reduction>> {
        self.reduction
            .as_ref()
            .map(|_| Arc::clone(&self.reduction_cell))
    }

    fn modulatable(&self, id: ParameterId) -> bool {
        self.modulatable.binary_search(&id.0).is_ok()
    }

    fn parameter(&mut self, id: ParameterId) -> Option<f64> {
        let ctrl = self.controller.as_ref()?;
        // Modulated: the value as set (the controller may show the
        // modulated one the processor reported).
        let n = match self
            .mod_bases
            .snapshot()
            .into_iter()
            .find(|(p, _)| *p == id.0)
        {
            Some((_, base)) => base,
            // SAFETY: plain query.
            None => unsafe { ctrl.getParamNormalized(id.0) },
        };
        Some(self.map.plain(id.0, n))
    }

    fn set_parameter(&mut self, id: ParameterId, value: f64) -> Result<(), PluginError> {
        let Some(info) = self.params.iter().find(|p| p.id == id) else {
            return Err(PluginError::UnknownParameter(id));
        };
        let n = self.map.normalized(id.0, info.clamp(value));
        if let Some(ctrl) = &self.controller {
            // SAFETY: plain call.
            unsafe { ctrl.setParamNormalized(id.0, n) };
        }
        self.send(id.0, n);
        Ok(())
    }

    fn latency_samples(&self) -> u32 {
        self.latency
    }

    fn preset_files(&self) -> Vec<std::path::PathBuf> {
        crate::presets::files(&self.scanned)
    }

    fn state_from_preset_file(&self, data: &[u8]) -> Result<Vec<u8>, PluginError> {
        crate::presets::state(data, &self.scanned.id)
    }

    fn programs(&self) -> Vec<String> {
        self.programs.clone()
    }

    fn current_program(&self) -> Option<usize> {
        let (id, steps) = self.program?;
        let last = if steps > 0 {
            steps as usize
        } else {
            self.programs.len().saturating_sub(1)
        };
        // SAFETY: plain query.
        let n = unsafe { self.controller.as_ref()?.getParamNormalized(id) };
        Some(
            ((n.clamp(0.0, 1.0) * last as f64).round() as usize)
                .min(self.programs.len().saturating_sub(1)),
        )
    }

    fn select_program(&mut self, index: usize) -> Result<(), PluginError> {
        let Some((id, n)) = self.program_value(index) else {
            return Err(PluginError::Failed(format!(
                "{}: no program {}",
                self.scanned.name,
                index + 1
            )));
        };
        if let Some(ctrl) = &self.controller {
            // SAFETY: plain call.
            unsafe { ctrl.setParamNormalized(id, n) };
        }
        self.send(id, n);
        Ok(())
    }

    fn changes_pending(&self) -> bool {
        self.to_rt
            .as_ref()
            .is_some_and(|tx| tx.slots() < tx.buffer().capacity())
    }

    fn take_editor_edits(&mut self) -> Vec<faderframe_plugin_host::EditorEdit> {
        std::mem::take(&mut self.editor_edits)
    }

    fn note_expressions(&self) -> Option<Vec<NoteExpressionKind>> {
        Some(self.note_expressions.clone())
    }

    fn activation(&self) -> u64 {
        self.activations
    }

    fn tail(&self) -> TailLength {
        self.tail
    }

    fn save_state(&mut self) -> Result<Vec<u8>, PluginError> {
        let comp = self
            .component_state()
            .ok_or_else(|| PluginError::Failed("the component did not save".into()))?;
        let ctrl = self
            .controller
            .as_ref()
            .filter(|_| self.separate_controller)
            .and_then(|c| {
                let stream = MemoryStream::empty();
                // SAFETY: stream alive for the call.
                (unsafe { c.getState(stream_ptr(&stream)) } == kResultOk).then(|| stream.data())
            })
            .unwrap_or_default();
        let mut out = Vec::with_capacity(12 + comp.len() + ctrl.len());
        out.extend_from_slice(STATE_MAGIC);
        out.extend_from_slice(&(comp.len() as u32).to_le_bytes());
        out.extend_from_slice(&comp);
        out.extend_from_slice(&(ctrl.len() as u32).to_le_bytes());
        out.extend_from_slice(&ctrl);
        // The component saved modulated values: the values as set follow.
        append_bases(&mut out, &self.mod_bases.snapshot());
        Ok(out)
    }

    fn load_state(&mut self, data: &[u8]) -> Result<(), PluginError> {
        let bad = || PluginError::InvalidState("truncated VST3 state".into());
        // Foreign blobs (no magic) are taken as component state.
        let (comp, ctrl, bases) = match data.strip_prefix(STATE_MAGIC) {
            Some(rest) => {
                let take = |b: &[u8]| -> Option<(Vec<u8>, usize)> {
                    let n = u32::from_le_bytes(b.get(..4)?.try_into().ok()?) as usize;
                    Some((b.get(4..4 + n)?.to_vec(), 4 + n))
                };
                let (comp, used) = take(rest).ok_or_else(bad)?;
                let (ctrl, more) = take(&rest[used..]).unwrap_or_default();
                (comp, ctrl, read_bases(&rest[used + more..]))
            }
            None => (data.to_vec(), Vec::new(), Vec::new()),
        };
        let stream = MemoryStream::with_data(comp);
        // SAFETY: streams alive for the calls.
        unsafe {
            if self.component.setState(stream_ptr(&stream)) != kResultOk {
                return Err(PluginError::InvalidState(
                    "the plugin rejected the state".into(),
                ));
            }
            if let Some(c) = &self.controller {
                stream.rewind();
                c.setComponentState(stream_ptr(&stream));
                if self.separate_controller && !ctrl.is_empty() {
                    let s2 = MemoryStream::with_data(ctrl);
                    c.setState(stream_ptr(&s2));
                }
            }
        }
        self.query_params();
        // Parameters saved while modulated: back to their values as set.
        for (id, n) in bases {
            if let Some(c) = &self.controller {
                // SAFETY: plain call.
                unsafe { c.setParamNormalized(id, n) };
            }
            self.send(id, n);
        }
        self.refresh_bases();
        Ok(())
    }

    fn poll(&mut self) -> PluginPoll {
        use vst3::Steinberg::Vst::RestartFlags_::*;
        let mut poll = PluginPoll::default();
        let flags = self.state.take_restart();
        if flags & (kParamTitlesChanged | kParamValuesChanged) != 0 {
            self.query_params();
            self.refresh_bases();
            poll.params_changed = true;
        }
        if flags & (kReloadComponent | kIoChanged | kMidiCCAssignmentChanged) != 0 {
            self.needs_restart = true;
            poll.restart = true;
        }
        if flags & kLatencyChanged != 0 && self.rt.is_some() {
            // SAFETY: plain query.
            let new = unsafe { self.processor.getLatencySamples() };
            if new != self.latency {
                self.needs_restart = true;
                poll.restart = true;
            }
        }
        // Editor edits reach the processor (and automation writing).
        use faderframe_plugin_host::EditorEdit as E;
        for e in self.state.take_edits() {
            match e {
                Edit::Perform(id, n) => {
                    self.send(id, n);
                    self.edited = true;
                    let plain = self.map.plain(id, n);
                    self.editor_edits.push(E::Value(ParameterId(id), plain));
                }
                Edit::Begin(id) => self.editor_edits.push(E::Begin(ParameterId(id))),
                Edit::End(id) => self.editor_edits.push(E::End(ParameterId(id))),
            }
        }
        // Values the processor changed go to the controller.
        let mut changes = Vec::new();
        if let Some(rx) = self.from_rt.as_mut() {
            while let Ok(c) = rx.pop() {
                changes.push(c);
            }
        }
        if let Some(ctrl) = &self.controller {
            for (id, n) in changes {
                // SAFETY: plain call.
                unsafe { ctrl.setParamNormalized(id, n) };
            }
        }
        poll.state_dirty = std::mem::take(&mut self.edited) | self.state.take_dirty();
        poll
    }

    fn editor(&mut self) -> Option<&mut dyn PluginEditor> {
        self.controller.as_ref()?;
        Some(self)
    }

    fn format_parameter(&mut self, id: ParameterId, value: f64) -> Option<String> {
        let ctrl = self.controller.as_ref()?;
        let n = self.map.normalized(id.0, value);
        let mut text: String128 = [0; 128];
        // SAFETY: valid out buffer.
        let ok = unsafe { ctrl.getParamStringByValue(id.0, n, &mut text) } == kResultOk;
        let s = wstr(&text);
        (ok && !s.is_empty()).then_some(s)
    }

    fn event_sources(&self) -> PluginEventSources {
        PluginEventSources {
            fds: self
                .state
                .fds()
                .into_iter()
                .map(|fd| PluginFd {
                    fd,
                    read: true,
                    write: false,
                    error: false,
                })
                .collect(),
            timers: self.state.timers(),
        }
    }

    fn on_fd(&mut self, fd: PluginFd) {
        self.state.fd_ready(fd.fd);
    }

    fn on_timer(&mut self, id: u32) {
        self.state.timer_fired(id);
    }

    fn create_processor(
        &mut self,
        config: &ProcessConfig,
    ) -> Result<Box<dyn PluginProcessor>, PluginError> {
        if self.config.as_ref() != Some(config)
            || self.rt.is_none()
            || self.needs_restart
            || self.active_outputs != self.output_buses
        {
            self.needs_restart = false;
            self.active_outputs = self.output_buses;
            self.deactivate();
            let fail = |what: &str| PluginError::Failed(format!("{}: {what}", self.scanned.name));
            use vst3::Steinberg::Vst::ProcessModes_::kRealtime;
            use vst3::Steinberg::Vst::SymbolicSampleSizes_::{kSample32, kSample64};
            let max = config.max_block_size.max(1);
            // SAFETY: plain queries on the control thread while inactive (no
            // processor is running).
            let can = |size: u32| unsafe { self.processor.canProcessSampleSize(size as i32) } == kResultOk;
            let (single, double) = (can(kSample32 as u32), can(kSample64 as u32));
            if !single && !double {
                return Err(fail("neither 32- nor 64-bit float processing is supported"));
            }
            // 64-bit when asked for (or the only choice).
            let double = double && (config.double_precision || !single);
            tracing::debug!(
                "{}: processing in {}-bit floating point",
                self.scanned.name,
                if double { 64 } else { 32 }
            );
            let (inputs, outputs, events) = self.set_bus_states(config.sidechain);
            let mut setup = ProcessSetup {
                processMode: kRealtime as i32,
                symbolicSampleSize: if double { kSample64 } else { kSample32 } as i32,
                maxSamplesPerBlock: max as i32,
                sampleRate: config.sample_rate,
            };
            // SAFETY: as above.
            unsafe {
                if self.processor.setupProcessing(&mut setup) != kResultOk {
                    return Err(fail("setupProcessing failed"));
                }
                if self.component.setActive(1) != kResultOk {
                    return Err(fail("activation failed"));
                }
                self.processor.setProcessing(1);
                self.latency = self.processor.getLatencySamples();
                self.tail = match self.processor.getTailSamples() {
                    0 => TailLength::None,
                    vst3::Steinberg::Vst::kInfiniteTail => TailLength::Infinite,
                    n => TailLength::Samples(n),
                };
            }
            let (mut tx, rx) = rtrb::RingBuffer::new(4096);
            let (out_tx, out_rx) = rtrb::RingBuffer::new(4096);
            let (bases_tx, bases_rx) = rtrb::RingBuffer::new((self.params.len() * 2).max(256));
            let values = self.controller_values();
            for (id, n) in self.pending.drain(..) {
                let _ = tx.push((id, n));
            }
            let active = Active::new(
                self.processor.clone(),
                ActiveConfig {
                    double,
                    inputs,
                    outputs,
                    has_event_input: events,
                    max_frames: max as usize,
                    map: Arc::clone(&self.map),
                    midi: self.midi_map(),
                    note_expressions: self.note_expressions.clone(),
                    program: self.program.map(|(id, steps)| {
                        let last = if steps > 0 {
                            steps
                        } else {
                            self.programs.len().saturating_sub(1) as u32
                        };
                        (id, last)
                    }),
                    values,
                    bases: Arc::clone(&self.mod_bases),
                    reduction: self.reduction.as_ref().map(|(id, table)| {
                        (*id, Arc::clone(table), Arc::clone(&self.reduction_cell))
                    }),
                },
                rx,
                out_tx,
                bases_rx,
            );
            self.rt = Some(Arc::new(TryCell::new(Some(active))));
            self.activations += 1;
            self.to_rt = Some(tx);
            self.from_rt = Some(out_rx);
            self.bases_tx = Some(bases_tx);
            self.config = Some(*config);
        }
        let cell = self
            .rt
            .as_ref()
            .map(Arc::clone)
            .ok_or_else(|| PluginError::Failed("not active".into()))?;
        Ok(Box::new(Vst3Processor { cell }))
    }
}

impl Drop for Vst3Instance {
    fn drop(&mut self) {
        self.drop_view();
        self.deactivate();
        // SAFETY: tear-down in the reverse order of set-up.
        unsafe {
            if let Some((a, b)) = self.connection.take() {
                a.disconnect(b.as_ptr());
                b.disconnect(a.as_ptr());
            }
            if let Some(c) = self.controller.take() {
                c.setComponentHandler(std::ptr::null_mut());
                if self.separate_controller {
                    c.terminate();
                }
            }
            self.component.terminate();
        }
        self.state.clear_run_loop();
    }
}

/// The VST3 platform type of a window system.
fn platform_type(api: WindowApi) -> vst3::Steinberg::FIDString {
    match api {
        WindowApi::X11 => kPlatformTypeX11EmbedWindowID,
        WindowApi::Win32 => kPlatformTypeHWND,
        WindowApi::Cocoa => kPlatformTypeNSView,
    }
}

impl PluginEditor for Vst3Instance {
    fn can_embed(&mut self, api: WindowApi) -> bool {
        self.ensure_view().is_some_and(|v| {
            // SAFETY: plain query.
            unsafe { v.isPlatformTypeSupported(platform_type(api)) == kResultTrue }
        })
    }

    fn can_float(&mut self, _api: WindowApi) -> bool {
        false
    }

    fn open_embedded(&mut self, _api: WindowApi, scale: f64) -> Result<(u32, u32), PluginError> {
        if self.view_open {
            self.close();
        }
        let view = self
            .ensure_view()
            .ok_or_else(|| PluginError::Failed("no editor".into()))?;
        let mut rect = ViewRect {
            left: 0,
            top: 0,
            right: 640,
            bottom: 420,
        };
        // SAFETY: the host object (the frame) outlives the view.
        unsafe {
            if let Some(frame) = self.host.as_com_ref::<IPlugFrame>() {
                view.setFrame(frame.as_ptr());
            }
            // Windows editors scale themselves when told (optional
            // interface; macOS and X11 take it from the system).
            if cfg!(windows)
                && let Some(s) = view.cast::<IPlugViewContentScaleSupport>()
            {
                s.setContentScaleFactor(scale as f32);
            }
            view.getSize(&mut rect);
        }
        Ok((
            (rect.right - rect.left).max(1) as u32,
            (rect.bottom - rect.top).max(1) as u32,
        ))
    }

    fn attach(&mut self, parent: ParentWindow) -> Result<(), PluginError> {
        let view = self
            .view
            .clone()
            .ok_or_else(|| PluginError::Failed("no editor".into()))?;
        // SAFETY: the parent window outlives the editor: the host calls
        // `removed` (close) before destroying its window.
        let r = unsafe {
            view.attached(
                parent.handle as usize as *mut c_void,
                platform_type(parent.api),
            )
        };
        if r != kResultOk {
            self.drop_view();
            return Err(PluginError::Failed("the editor did not attach".into()));
        }
        self.view_open = true;
        Ok(())
    }

    fn open_floating(&mut self, _api: WindowApi, _title: &str) -> Result<(), PluginError> {
        Err(PluginError::Failed("VST3 editors are embedded only".into()))
    }

    fn close(&mut self) {
        self.drop_view();
    }

    fn is_open(&self) -> bool {
        self.view_open
    }

    fn can_resize(&mut self) -> bool {
        self.view.as_ref().is_some_and(|v| {
            // SAFETY: plain query.
            unsafe { v.canResize() == kResultTrue }
        })
    }

    fn set_size(&mut self, width: u32, height: u32) -> Option<(u32, u32)> {
        let view = self.view.clone()?;
        let mut rect = ViewRect {
            left: 0,
            top: 0,
            right: width as i32,
            bottom: height as i32,
        };
        // SAFETY: plain calls with a valid rectangle.
        unsafe {
            view.checkSizeConstraint(&mut rect);
            view.onSize(&mut rect);
        }
        Some((
            (rect.right - rect.left).max(1) as u32,
            (rect.bottom - rect.top).max(1) as u32,
        ))
    }

    fn take_requests(&mut self) -> EditorRequests {
        let resize = self.state.take_resize();
        // VST3: the host answers resizeView with onSize.
        if let (Some((w, h)), Some(view)) = (resize, self.view.clone()) {
            let mut rect = ViewRect {
                left: 0,
                top: 0,
                right: w as i32,
                bottom: h as i32,
            };
            // SAFETY: plain call with a valid rectangle.
            unsafe { view.onSize(&mut rect) };
        }
        EditorRequests {
            resize,
            show: false,
            hide: false,
            closed: false,
        }
    }
}

/// A gain-reduction meter's values in dB taken off, from the plugin's own
/// conversion (main thread): its plain value when its unit is dB, else the
/// dB its text shows, else a plain 0…1 as a linear gain, else the plain
/// value as dB.
fn reduction_table(
    ctrl: &ComPtr<IEditController>,
    id: ParamID,
    units: &str,
) -> faderframe_plugin_host::ReductionTable {
    let mut t = [0.0f32; 65];
    // SAFETY: plain queries with valid out pointers.
    let plain = |n: f64| unsafe { ctrl.normalizedParamToPlain(id, n) };
    let unit_range = (0.0..=1.0).contains(&plain(0.0)) && (0.0..=1.0).contains(&plain(1.0));
    for (k, v) in t.iter_mut().enumerate() {
        let n = k as f64 / 64.0;
        let p = plain(n);
        // SAFETY: a plain query into a valid string buffer.
        let text = unsafe {
            let mut text: String128 = std::mem::zeroed();
            (ctrl.getParamStringByValue(id, n, &mut text) == kResultOk).then(|| wstr(&text))
        };
        let shown = text
            .as_deref()
            .filter(|t| t.to_ascii_lowercase().contains("db"))
            .and_then(faderframe_plugin_host::leading_number);
        *v = if units.contains("db") {
            p.abs() as f32
        } else if let Some(db) = shown {
            db.abs()
        } else if unit_range {
            if p > 0.0 {
                (-20.0 * p.log10()).clamp(0.0, 90.0) as f32
            } else {
                90.0
            }
        } else {
            p.abs() as f32
        };
    }
    faderframe_plugin_host::ReductionTable(t)
}
