//! The audio-thread side of a VST3 plugin.
//!
//! [`Active`] holds the processor interface with everything a block needs,
//! sized at activation: bus buffers, the host's `IParameterChanges` and
//! `IEventList` objects (fixed capacity, refilled per block) and the
//! process context. It sits in a [`TryCell`] shared by every graph node
//! that references the instance; if the cell is busy (the control thread
//! is reconfiguring the plugin) or empty (deactivated), the block is
//! silent. Nothing here allocates, locks or frees.

use faderframe_audio_graph::NodeIo;
use faderframe_midi::{MidiEvent, NoteExpressionKind, NoteIds, TimedMidiEvent};
use faderframe_plugin_host::emulated::{EmulatedMods, ModBases, delta_of};
use faderframe_plugin_host::{ParamMod, PluginProcessContext, PluginProcessor, ProcessStatus};
use faderframe_realtime::TryCell;
use std::cell::Cell;
use std::sync::Arc;
use vst3::Steinberg::Vst::{
    AudioBusBuffers, Event, IAudioProcessor, IAudioProcessorTrait, IEventList, IEventListTrait,
    IParamValueQueue, IParamValueQueueTrait, IParameterChanges, IParameterChangesTrait,
    NoteExpressionValueEvent, NoteOffEvent, NoteOnEvent, ParamID, ParamValue, PolyPressureEvent,
    ProcessContext, ProcessData, kNoParamId,
};
use vst3::Steinberg::{int32, kInvalidArgument, kResultFalse, kResultOk, kResultTrue, tresult};
use vst3::{Class, ComPtr, ComWrapper};

/// Parameters changed per block (beyond that, later changes wait or drop).
pub(crate) const PARAM_QUEUES: usize = 512;
/// Points per parameter and block; further points replace the last one.
pub(crate) const POINTS: usize = 32;
/// Events per block.
pub(crate) const EVENTS: usize = 2048;
/// MIDI controllers mapped through `IMidiMapping` (0–127, aftertouch,
/// pitch bend).
pub(crate) const CONTROLLERS: usize = 130;
const AFTERTOUCH: usize = 128;
const PITCH_BEND: usize = 129;

/// Stepped parameters (id, step count): FaderFrame shows them as integers
/// 0…steps, VST3 wants 0…1.
#[derive(Clone, Debug, Default)]
pub struct ParamMap {
    steps: Vec<(ParamID, u32)>,
}

impl ParamMap {
    pub fn new(mut steps: Vec<(ParamID, u32)>) -> Self {
        steps.sort_unstable();
        Self { steps }
    }

    fn steps(&self, id: ParamID) -> u32 {
        self.steps
            .binary_search_by_key(&id, |(i, _)| *i)
            .map_or(0, |i| self.steps[i].1)
    }

    pub fn normalized(&self, id: ParamID, value: f64) -> ParamValue {
        let s = self.steps(id);
        let n = if s > 0 { value / s as f64 } else { value };
        n.clamp(0.0, 1.0)
    }

    pub fn plain(&self, id: ParamID, n: ParamValue) -> f64 {
        let s = self.steps(id);
        if s > 0 {
            (n.clamp(0.0, 1.0) * s as f64).round()
        } else {
            n
        }
    }
}

/// `IMidiMapping` table: parameter per channel and controller (bus 0).
pub type MidiMap = [[ParamID; CONTROLLERS]; 16];

pub(crate) struct ParamQueue {
    id: Cell<ParamID>,
    len: Cell<usize>,
    points: Box<[Cell<(int32, ParamValue)>]>,
}

impl Class for ParamQueue {
    type Interfaces = (IParamValueQueue,);
}

impl ParamQueue {
    fn new() -> Self {
        Self {
            id: Cell::new(kNoParamId),
            len: Cell::new(0),
            points: (0..POINTS).map(|_| Cell::new((0, 0.0))).collect(),
        }
    }

    fn push(&self, offset: int32, value: ParamValue) -> usize {
        let n = self.len.get();
        // Offsets must not decrease.
        let offset = match n {
            0 => offset,
            _ => offset.max(self.points[n - 1].get().0),
        };
        if n < self.points.len() {
            self.points[n].set((offset, value));
            self.len.set(n + 1);
            n
        } else {
            self.points[n - 1].set((offset, value));
            n - 1
        }
    }

    pub(crate) fn last(&self) -> Option<ParamValue> {
        let n = self.len.get();
        (n > 0).then(|| self.points[n - 1].get().1)
    }
}

impl IParamValueQueueTrait for ParamQueue {
    unsafe fn getParameterId(&self) -> ParamID {
        self.id.get()
    }

    unsafe fn getPointCount(&self) -> int32 {
        self.len.get() as int32
    }

    unsafe fn getPoint(&self, index: int32, offset: *mut int32, value: *mut ParamValue) -> tresult {
        let i = index.max(0) as usize;
        if index < 0 || i >= self.len.get() || offset.is_null() || value.is_null() {
            return kInvalidArgument;
        }
        let (o, v) = self.points[i].get();
        // SAFETY: checked for null.
        unsafe {
            *offset = o;
            *value = v;
        }
        kResultTrue
    }

    unsafe fn addPoint(&self, offset: int32, value: ParamValue, index: *mut int32) -> tresult {
        let i = self.push(offset, value);
        if !index.is_null() {
            // SAFETY: checked for null.
            unsafe { *index = i as int32 };
        }
        kResultTrue
    }
}

/// The host's `IParameterChanges` (input or output of one block).
pub(crate) struct ParamChanges {
    queues: Box<[ComWrapper<ParamQueue>]>,
    count: Cell<usize>,
}

impl Class for ParamChanges {
    type Interfaces = (IParameterChanges,);
}

impl ParamChanges {
    pub(crate) fn new() -> Self {
        Self {
            queues: (0..PARAM_QUEUES)
                .map(|_| ComWrapper::new(ParamQueue::new()))
                .collect(),
            count: Cell::new(0),
        }
    }

    pub(crate) fn clear(&self) {
        self.count.set(0);
    }

    /// The queue for `id` (new if needed); `None` when all are in use.
    fn queue(&self, id: ParamID) -> Option<(usize, &ComWrapper<ParamQueue>)> {
        let n = self.count.get();
        if let Some(i) = self.queues[..n].iter().position(|q| q.id.get() == id) {
            return Some((i, &self.queues[i]));
        }
        let q = self.queues.get(n)?;
        q.id.set(id);
        q.len.set(0);
        self.count.set(n + 1);
        Some((n, q))
    }

    pub(crate) fn add(&self, id: ParamID, offset: int32, value: ParamValue) -> bool {
        match self.queue(id) {
            Some((_, q)) => {
                q.push(offset, value);
                true
            }
            None => false,
        }
    }

    pub(crate) fn has_room_for(&self, id: ParamID) -> bool {
        let n = self.count.get();
        n < self.queues.len() || self.queues[..n].iter().any(|q| q.id.get() == id)
    }

    pub(crate) fn changed(&self) -> impl Iterator<Item = &ComWrapper<ParamQueue>> {
        self.queues[..self.count.get()].iter()
    }

    fn ptr_of(q: &ComWrapper<ParamQueue>) -> *mut IParamValueQueue {
        q.as_com_ref::<IParamValueQueue>()
            .map_or(std::ptr::null_mut(), |r| r.as_ptr())
    }
}

impl IParameterChangesTrait for ParamChanges {
    unsafe fn getParameterCount(&self) -> int32 {
        self.count.get() as int32
    }

    unsafe fn getParameterData(&self, index: int32) -> *mut IParamValueQueue {
        match usize::try_from(index) {
            Ok(i) if i < self.count.get() => Self::ptr_of(&self.queues[i]),
            _ => std::ptr::null_mut(),
        }
    }

    unsafe fn addParameterData(
        &self,
        id: *const ParamID,
        index: *mut int32,
    ) -> *mut IParamValueQueue {
        // SAFETY: the plugin passes a valid id pointer.
        let Some(&id) = (unsafe { id.as_ref() }) else {
            return std::ptr::null_mut();
        };
        let Some((i, q)) = self.queue(id) else {
            return std::ptr::null_mut();
        };
        if !index.is_null() {
            // SAFETY: checked for null.
            unsafe { *index = i as int32 };
        }
        Self::ptr_of(q)
    }
}

/// The host's `IEventList`.
pub(crate) struct EventList {
    events: Box<[Cell<Event>]>,
    len: Cell<usize>,
}

impl Class for EventList {
    type Interfaces = (IEventList,);
}

fn blank_event() -> Event {
    // SAFETY: `Event` is a plain C struct (with a union); all-zero is a
    // valid note-on event.
    unsafe { std::mem::zeroed() }
}

impl EventList {
    pub(crate) fn new() -> Self {
        Self {
            events: (0..EVENTS).map(|_| Cell::new(blank_event())).collect(),
            len: Cell::new(0),
        }
    }

    pub(crate) fn clear(&self) {
        self.len.set(0);
    }

    pub(crate) fn push(&self, e: Event) -> bool {
        let n = self.len.get();
        match self.events.get(n) {
            Some(slot) => {
                slot.set(e);
                self.len.set(n + 1);
                true
            }
            None => false,
        }
    }

    pub(crate) fn iter(&self) -> impl Iterator<Item = Event> + '_ {
        self.events[..self.len.get()].iter().map(Cell::get)
    }
}

impl IEventListTrait for EventList {
    unsafe fn getEventCount(&self) -> int32 {
        self.len.get() as int32
    }

    unsafe fn getEvent(&self, index: int32, e: *mut Event) -> tresult {
        match usize::try_from(index) {
            Ok(i) if i < self.len.get() && !e.is_null() => {
                // SAFETY: checked for null.
                unsafe { *e = self.events[i].get() };
                kResultOk
            }
            _ => kInvalidArgument,
        }
    }

    unsafe fn addEvent(&self, e: *mut Event) -> tresult {
        // SAFETY: the plugin passes a valid event.
        match unsafe { e.as_ref() } {
            Some(e) if self.push(*e) => kResultOk,
            _ => kResultFalse,
        }
    }
}

/// Buffers of all buses of one direction, with the pointer arrays VST3
/// reads them through: 32-bit, or 64-bit when the plugin processes in
/// double precision (the graph's audio is converted in and out).
pub(crate) struct Buses {
    bufs: Vec<Vec<Vec<f32>>>,
    ptrs: Vec<Vec<*mut f32>>,
    bufs64: Vec<Vec<Vec<f64>>>,
    ptrs64: Vec<Vec<*mut f64>>,
    abb: Vec<AudioBusBuffers>,
    double: bool,
}

impl Buses {
    pub(crate) fn new(channels: &[u16], max_frames: usize, double: bool) -> Self {
        let shape = |len: usize| -> Vec<Vec<usize>> {
            channels.iter().map(|&c| vec![len; c as usize]).collect()
        };
        let (n32, n64) = if double {
            (0, max_frames)
        } else {
            (max_frames, 0)
        };
        let mut bufs: Vec<Vec<Vec<f32>>> = shape(n32)
            .into_iter()
            .map(|bus| bus.into_iter().map(|n| vec![0.0; n]).collect())
            .collect();
        let mut bufs64: Vec<Vec<Vec<f64>>> = shape(n64)
            .into_iter()
            .map(|bus| bus.into_iter().map(|n| vec![0.0; n]).collect())
            .collect();
        let ptrs: Vec<Vec<*mut f32>> = bufs
            .iter_mut()
            .map(|bus| bus.iter_mut().map(|ch| ch.as_mut_ptr()).collect())
            .collect();
        let ptrs64: Vec<Vec<*mut f64>> = bufs64
            .iter_mut()
            .map(|bus| bus.iter_mut().map(|ch| ch.as_mut_ptr()).collect())
            .collect();
        let abb = channels
            .iter()
            .map(|&c| {
                // SAFETY: plain C struct; the buffer pointers are set before
                // every block (`arm`).
                let mut b: AudioBusBuffers = unsafe { std::mem::zeroed() };
                b.numChannels = c as int32;
                b
            })
            .collect();
        // The heap blocks behind the buffers and pointer arrays never move
        // from here on (the vectors are never resized), so the pointers
        // stay valid.
        Self {
            bufs,
            ptrs,
            bufs64,
            ptrs64,
            abb,
            double,
        }
    }

    fn count(&self) -> int32 {
        self.abb.len() as int32
    }

    fn channels(&self, bus: usize) -> usize {
        self.abb.get(bus).map_or(0, |b| b.numChannels as usize)
    }

    /// Fill channel `c` of bus `b` from `src` (`None`: silence).
    fn load(&mut self, b: usize, c: usize, src: Option<&[f32]>, n: usize) {
        if self.double {
            let dst = &mut self.bufs64[b][c][..n];
            match src {
                Some(src) => {
                    for (d, s) in dst.iter_mut().zip(&src[..n]) {
                        *d = f64::from(*s);
                    }
                }
                None => dst.fill(0.0),
            }
        } else {
            let dst = &mut self.bufs[b][c][..n];
            match src {
                Some(src) => dst.copy_from_slice(&src[..n]),
                None => dst.fill(0.0),
            }
        }
    }

    /// Copy channel `c` of bus `b` to `dst`.
    fn store(&self, b: usize, c: usize, dst: &mut [f32], n: usize) {
        if self.double {
            for (d, s) in dst[..n].iter_mut().zip(&self.bufs64[b][c][..n]) {
                *d = *s as f32;
            }
        } else {
            dst[..n].copy_from_slice(&self.bufs[b][c][..n]);
        }
    }

    /// Point the bus structs at the buffers (the plugin may have changed
    /// the pointers or flags last block).
    fn arm(&mut self) {
        for (b, abb) in self.abb.iter_mut().enumerate() {
            abb.silenceFlags = 0;
            if self.double {
                abb.__field0.channelBuffers64 = self.ptrs64[b].as_mut_ptr();
            } else {
                abb.__field0.channelBuffers32 = self.ptrs[b].as_mut_ptr();
            }
        }
    }

    fn as_mut_ptr(&mut self) -> *mut AudioBusBuffers {
        if self.abb.is_empty() {
            std::ptr::null_mut()
        } else {
            self.abb.as_mut_ptr()
        }
    }
}

/// Everything the audio thread needs for one active instance.
pub(crate) struct Active {
    pub processor: ComPtr<IAudioProcessor>,
    inputs: Buses,
    outputs: Buses,
    in_params: ComWrapper<ParamChanges>,
    pub(crate) out_params: ComWrapper<ParamChanges>,
    in_events: ComWrapper<EventList>,
    out_events: ComWrapper<EventList>,
    has_event_input: bool,
    context: ProcessContext,
    params_rx: rtrb::Consumer<(ParamID, ParamValue)>,
    out_tx: rtrb::Producer<(ParamID, ParamValue)>,
    /// Values the controller has now (a loaded state): bases only.
    bases_rx: rtrb::Consumer<(ParamID, ParamValue)>,
    /// Modulation sent as parameter changes (VST3 has none of its own).
    emu: EmulatedMods,
    map: Arc<ParamMap>,
    midi: Option<Box<MidiMap>>,
    /// Ids of the sounding notes (note expressions address them).
    note_ids: Box<NoteIds>,
    /// The note expressions the plugin lists.
    note_expressions: [bool; 7],
    /// The program-change parameter and its last program's index.
    program: Option<(ParamID, u32)>,
    continuous: i64,
    max_frames: usize,
    /// The gain-reduction meter (see `ActiveConfig::reduction`).
    reduction: Option<(
        ParamID,
        Arc<faderframe_plugin_host::ReductionTable>,
        Arc<faderframe_plugin_host::Reduction>,
    )>,
}

// SAFETY: `Active` is used by one thread at a time (it lives in a TryCell;
// the audio thread holds it during a block, the control thread only while
// reconfiguring). Its COM objects use `Cell`s and raw buffer pointers that
// are only touched by the holder; reference counts are atomic.
unsafe impl Send for Active {}

pub(crate) struct ActiveConfig {
    /// Process in 64-bit floating point.
    pub double: bool,
    pub inputs: Vec<u16>,
    pub outputs: Vec<u16>,
    pub has_event_input: bool,
    pub max_frames: usize,
    pub map: Arc<ParamMap>,
    pub midi: Option<Box<MidiMap>>,
    /// The note expressions the plugin lists (others are not sent).
    pub note_expressions: Vec<NoteExpressionKind>,
    /// The program-change parameter and its last program's index (MIDI
    /// program changes select programs).
    pub program: Option<(ParamID, u32)>,
    /// Every parameter's value now (normalised), and where the bases of
    /// modulated ones are published.
    pub values: Vec<(ParamID, ParamValue)>,
    pub bases: Arc<ModBases>,
    /// The gain-reduction meter: its parameter, its values in dB, the cell.
    pub reduction: Option<(
        ParamID,
        Arc<faderframe_plugin_host::ReductionTable>,
        Arc<faderframe_plugin_host::Reduction>,
    )>,
}

/// A modulation's share of the range: VST3's normalised offset.
fn share(m: &ParamMod) -> f64 {
    f64::from(m.share)
}

impl Active {
    pub(crate) fn new(
        processor: ComPtr<IAudioProcessor>,
        c: ActiveConfig,
        params_rx: rtrb::Consumer<(ParamID, ParamValue)>,
        out_tx: rtrb::Producer<(ParamID, ParamValue)>,
        bases_rx: rtrb::Consumer<(ParamID, ParamValue)>,
    ) -> Self {
        Self {
            emu: EmulatedMods::new(c.values, c.bases),
            bases_rx,
            processor,
            inputs: Buses::new(&c.inputs, c.max_frames, c.double),
            outputs: Buses::new(&c.outputs, c.max_frames, c.double),
            in_params: ComWrapper::new(ParamChanges::new()),
            out_params: ComWrapper::new(ParamChanges::new()),
            in_events: ComWrapper::new(EventList::new()),
            out_events: ComWrapper::new(EventList::new()),
            has_event_input: c.has_event_input,
            // SAFETY: plain C struct; filled per block.
            context: unsafe { std::mem::zeroed() },
            params_rx,
            out_tx,
            map: c.map,
            midi: c.midi,
            note_ids: Box::new(NoteIds::new()),
            note_expressions: NoteExpressionKind::ALL.map(|k| c.note_expressions.contains(&k)),
            program: c.program,
            continuous: 0,
            max_frames: c.max_frames,
            reduction: c.reduction,
        }
    }
}

/// A note expression as VST3 has it: the standard type id and the
/// normalised value (volume: 0.25 = 0 dB, 1 = +12 dB; pan and tuning: 0.5 =
/// centre, tuning ±120 semitones). Pressure is a poly-pressure event
/// instead (`None`).
fn vst3_expression(kind: NoteExpressionKind, plain: f64) -> Option<(u32, f64)> {
    use vst3::Steinberg::Vst::NoteExpressionTypeIDs_::*;
    let (id, norm) = match kind {
        NoteExpressionKind::Volume => (kVolumeTypeID, 10f64.powf(plain / 20.0) / 4.0),
        NoteExpressionKind::Pan => (kPanTypeID, (plain + 1.0) / 2.0),
        NoteExpressionKind::Tuning => (kTuningTypeID, 0.5 + plain / 240.0),
        NoteExpressionKind::Vibrato => (kVibratoTypeID, plain),
        NoteExpressionKind::Expression => (kExpressionTypeID, plain),
        NoteExpressionKind::Brightness => (kBrightnessTypeID, plain),
        NoteExpressionKind::Pressure => return None,
    };
    Some((id as _, norm.clamp(0.0, 1.0)))
}

pub(crate) type SharedRt = Arc<TryCell<Option<Active>>>;

/// The graph's handle on a VST3 plugin's processor.
pub struct Vst3Processor {
    pub(crate) cell: SharedRt,
}

fn silence(io: &mut NodeIo<'_>) {
    for out in io.audio_out.iter_mut() {
        out.clear();
    }
}

fn fill_context(c: &mut ProcessContext, t: &faderframe_transport::TransportInfo, cont: i64) {
    use vst3::Steinberg::Vst::ProcessContext_::StatesAndFlags_::*;
    let mut state =
        kTempoValid | kTimeSigValid | kProjectTimeMusicValid | kBarPositionValid | kContTimeValid;
    if t.playing {
        state |= kPlaying;
    }
    if t.recording {
        state |= kRecording;
    }
    c.cycleStartMusic = 0.0;
    c.cycleEndMusic = 0.0;
    if let Some(r) = t.loop_range {
        state |= kCycleValid;
        if t.looping {
            state |= kCycleActive;
        }
        // Quarters of the loop points, assuming the tempo at the playhead.
        let q = |s: i64| {
            t.quarter_position
                + (s - t.sample_position) as f64 / t.sample_rate.max(1.0) * t.tempo / 60.0
        };
        c.cycleStartMusic = q(r.start);
        c.cycleEndMusic = q(r.end);
    }
    // Constant types differ between platforms' bindings.
    c.state = state as _;
    c.sampleRate = t.sample_rate;
    c.projectTimeSamples = t.sample_position;
    c.continousTimeSamples = cont;
    c.projectTimeMusic = t.quarter_position;
    c.barPositionMusic = t.bar_start_quarters;
    c.tempo = t.tempo;
    c.timeSigNumerator = t.time_signature.numerator as i32;
    c.timeSigDenominator = t.time_signature.denominator as i32;
}

fn note_event(offset: i32, kind: u16, set: impl FnOnce(&mut Event)) -> Event {
    let mut e = blank_event();
    e.busIndex = 0;
    e.sampleOffset = offset;
    e.r#type = kind;
    set(&mut e);
    e
}

impl Active {
    /// Notes become events; controllers become parameter changes through the
    /// plugin's MIDI mapping (VST3 has no CC events).
    /// A value set this block (`id` at `t`): the new base, and what the
    /// plugin gets with the parameter's modulation on top.
    fn set_param(&mut self, mods: &[ParamMod], id: ParamID, t: int32, v: ParamValue) -> bool {
        self.emu.set(id, v);
        let v = delta_of(mods, id, share).map_or(v, |d| (v + d).clamp(0.0, 1.0));
        self.in_params.add(id, t, v)
    }

    fn midi_in(&mut self, io: &NodeIo<'_>, last: i32, mods: &[ParamMod]) {
        use vst3::Steinberg::Vst::Event_::EventTypes_::*;
        let Some(midi) = io.events_in.first() else {
            return;
        };
        for ev in midi.iter() {
            let t = (ev.sample_offset as i32).min(last);
            let mapped = |ch: u8, ctrl: usize| -> Option<ParamID> {
                let id = self.midi.as_ref()?[ch as usize & 15][ctrl];
                (id != kNoParamId).then_some(id)
            };
            match ev.event {
                MidiEvent::NoteOn {
                    channel,
                    key,
                    velocity,
                } if velocity > 0 => {
                    let id = self.note_ids.start(channel, key);
                    self.in_events.push(note_event(t, kNoteOnEvent as u16, |e| {
                        e.__field0.noteOn = NoteOnEvent {
                            channel: channel as i16,
                            pitch: key as i16,
                            tuning: 0.0,
                            velocity: velocity as f32 / 127.0,
                            length: 0,
                            noteId: id,
                        }
                    }));
                }
                MidiEvent::NoteOn { channel, key, .. }
                | MidiEvent::NoteOff { channel, key, .. } => {
                    let velocity = match ev.event {
                        MidiEvent::NoteOff { velocity, .. } => velocity,
                        _ => 64,
                    };
                    let id = self.note_ids.end(channel, key);
                    self.in_events
                        .push(note_event(t, kNoteOffEvent as u16, |e| {
                            e.__field0.noteOff = NoteOffEvent {
                                channel: channel as i16,
                                pitch: key as i16,
                                velocity: velocity as f32 / 127.0,
                                noteId: id,
                                tuning: 0.0,
                            }
                        }));
                }
                MidiEvent::PolyPressure {
                    channel,
                    key,
                    pressure,
                } => {
                    self.in_events
                        .push(note_event(t, kPolyPressureEvent as u16, |e| {
                            e.__field0.polyPressure = PolyPressureEvent {
                                channel: channel as i16,
                                pitch: key as i16,
                                pressure: pressure as f32 / 127.0,
                                noteId: self.note_ids.get(channel, key),
                            }
                        }));
                }
                MidiEvent::NoteExpression {
                    channel,
                    key,
                    kind,
                    value,
                } => {
                    let id = self.note_ids.get(channel, key);
                    let listed = NoteExpressionKind::ALL
                        .iter()
                        .position(|k| *k == kind)
                        .is_some_and(|i| self.note_expressions[i]);
                    if id == NoteIds::NONE || !listed {
                        continue;
                    }
                    let event = match vst3_expression(kind, value.get()) {
                        Some((type_id, norm)) => {
                            note_event(t, kNoteExpressionValueEvent as u16, |e| {
                                e.__field0.noteExpressionValue = NoteExpressionValueEvent {
                                    typeId: type_id,
                                    noteId: id,
                                    value: norm,
                                }
                            })
                        }
                        None => note_event(t, kPolyPressureEvent as u16, |e| {
                            e.__field0.polyPressure = PolyPressureEvent {
                                channel: channel as i16,
                                pitch: key as i16,
                                pressure: value.get().clamp(0.0, 1.0) as f32,
                                noteId: id,
                            }
                        }),
                    };
                    self.in_events.push(event);
                }
                MidiEvent::ControlChange {
                    channel,
                    controller,
                    value,
                } => {
                    if let Some(id) = mapped(channel, controller as usize & 127) {
                        self.set_param(mods, id, t, value as f64 / 127.0);
                    }
                }
                MidiEvent::ChannelPressure { channel, pressure } => {
                    if let Some(id) = mapped(channel, AFTERTOUCH) {
                        self.set_param(mods, id, t, pressure as f64 / 127.0);
                    }
                }
                MidiEvent::PitchBend { channel, value } => {
                    if let Some(id) = mapped(channel, PITCH_BEND) {
                        self.set_param(mods, id, t, value.min(16383) as f64 / 16383.0);
                    }
                }
                // SysEx: a data event pointing at the bytes in the graph's
                // buffer (valid for this block).
                MidiEvent::SysEx(r) => {
                    use vst3::Steinberg::Vst::DataEvent_::DataTypes_::kMidiSysEx;
                    if let Some(bytes) = midi.sysex(&r) {
                        self.in_events.push(note_event(t, kDataEvent as u16, |e| {
                            e.__field0.data = vst3::Steinberg::Vst::DataEvent {
                                size: bytes.len() as u32,
                                r#type: kMidiSysEx as _,
                                bytes: bytes.as_ptr(),
                            }
                        }));
                    }
                }
                // VST3 has no program change events: it selects the
                // program-change parameter's program (and the controller
                // follows, so menus show it).
                MidiEvent::ProgramChange { program, .. } => {
                    if let Some((id, last)) = self.program
                        && last > 0
                        && u32::from(program) <= last
                    {
                        let n = f64::from(program) / f64::from(last);
                        self.set_param(mods, id, t, n);
                        let _ = self.out_tx.push((id, n));
                    }
                }
            }
        }
    }

    fn process(&mut self, ctx: &PluginProcessContext<'_>, io: &mut NodeIo<'_>) -> ProcessStatus {
        use vst3::Steinberg::Vst::ProcessModes_::kRealtime;
        use vst3::Steinberg::Vst::SymbolicSampleSizes_::{kSample32, kSample64};
        let n = io.frames.min(self.max_frames);
        let last = n.saturating_sub(1) as i32;
        self.in_params.clear();
        self.out_params.clear();
        self.in_events.clear();
        self.out_events.clear();

        // Parameter changes: from the UI and the plugin's editor first (they
        // wait in the queue when no slot is free), then automation, then
        // mapped MIDI controllers; modulation rides on all of them, and
        // reaches its block's value at the end of the block.
        let mods = ctx.param_mods;
        while let Ok((id, v)) = self.bases_rx.pop() {
            self.emu.set(id, v);
        }
        while let Ok(&(id, v)) = self.params_rx.peek() {
            if !self.in_params.has_room_for(id) {
                break;
            }
            self.set_param(mods, id, 0, v);
            let _ = self.params_rx.pop();
        }
        for e in ctx.param_events {
            let v = self.map.normalized(e.parameter.0, e.value as f64);
            self.set_param(mods, e.parameter.0, (e.sample_offset as i32).min(last), v);
        }
        if self.has_event_input {
            self.midi_in(io, last, mods);
        }
        {
            let inp = &self.in_params;
            self.emu.block(
                mods,
                share,
                |_, v| v.clamp(0.0, 1.0),
                |id, v| {
                    inp.add(id, last, v);
                },
            );
        }

        // Audio: graph input `b` into bus `b` (the main input, then the
        // sidechain when connected), other buses silent.
        for b in 0..self.inputs.abb.len() {
            for c in 0..self.inputs.channels(b) {
                let src = match io.audio_in.get(b) {
                    Some(inp) if inp.num_channels() > 0 => {
                        Some(inp.channel(c.min(inp.num_channels() - 1)))
                    }
                    _ => None,
                };
                self.inputs.load(b, c, src, n);
            }
        }
        self.inputs.arm();
        self.outputs.arm();
        fill_context(&mut self.context, ctx.transport, self.continuous);

        let as_changes = |c: &ComWrapper<ParamChanges>| {
            c.as_com_ref::<IParameterChanges>()
                .map_or(std::ptr::null_mut(), |r| r.as_ptr())
        };
        let as_events = |c: &ComWrapper<EventList>| {
            c.as_com_ref::<IEventList>()
                .map_or(std::ptr::null_mut(), |r| r.as_ptr())
        };
        let mut data = ProcessData {
            processMode: kRealtime as int32,
            symbolicSampleSize: if self.inputs.double {
                kSample64
            } else {
                kSample32
            } as int32,
            numSamples: n as int32,
            numInputs: self.inputs.count(),
            numOutputs: self.outputs.count(),
            inputs: self.inputs.as_mut_ptr(),
            outputs: self.outputs.as_mut_ptr(),
            inputParameterChanges: as_changes(&self.in_params),
            outputParameterChanges: as_changes(&self.out_params),
            inputEvents: if self.has_event_input {
                as_events(&self.in_events)
            } else {
                std::ptr::null_mut()
            },
            outputEvents: as_events(&self.out_events),
            processContext: &mut self.context,
        };
        // SAFETY: every pointer in `data` refers to buffers and objects owned
        // by `self`, valid for the duration of the call.
        let result = unsafe { self.processor.process(&mut data) };
        self.continuous += n as i64;

        // Values the processor changed itself go to the controller (and are
        // new bases, unless they are a modulated parameter's echo).
        for q in self.out_params.changed() {
            if let Some(v) = q.last() {
                let id = q.id.get();
                // The gain-reduction meter, for the mixer's.
                if let Some((meter, table, cell)) = &self.reduction
                    && *meter == id
                {
                    cell.set(table.db(v));
                }
                if !self.emu.modulated(id) {
                    self.emu.set(id, v);
                }
                let _ = self.out_tx.push((id, v));
            }
        }

        // Notes the plugin sent (MIDI effects) to the graph.
        if let Some(out) = io.events_out.first_mut() {
            use vst3::Steinberg::Vst::Event_::EventTypes_::{kNoteOffEvent, kNoteOnEvent};
            for e in self.out_events.iter() {
                let at = e.sampleOffset.clamp(0, last) as u32;
                // SAFETY: the union member matches the event type.
                let event = unsafe {
                    match e.r#type as u32 {
                        t if t == kNoteOnEvent as u32 => {
                            let n = e.__field0.noteOn;
                            MidiEvent::NoteOn {
                                channel: (n.channel & 15) as u8,
                                key: n.pitch.clamp(0, 127) as u8,
                                velocity: (n.velocity * 127.0).round().clamp(1.0, 127.0) as u8,
                            }
                        }
                        t if t == kNoteOffEvent as u32 => {
                            let n = e.__field0.noteOff;
                            MidiEvent::NoteOff {
                                channel: (n.channel & 15) as u8,
                                key: n.pitch.clamp(0, 127) as u8,
                                velocity: (n.velocity * 127.0).round().clamp(0.0, 127.0) as u8,
                            }
                        }
                        _ => continue,
                    }
                };
                let _ = out.push(TimedMidiEvent {
                    sample_offset: at,
                    event,
                });
            }
        }

        // Output bus `b` to graph output `b` (the main one first; no
        // output bus: pass through).
        for (b, out) in io.audio_out.iter_mut().enumerate() {
            match self.outputs.channels(b) {
                bus if bus > 0 => {
                    for c in 0..out.num_channels() {
                        self.outputs.store(b, c.min(bus - 1), out.channel_mut(c), n);
                    }
                }
                _ if b == 0 => match io.audio_in.first() {
                    Some(inp) => out.copy_from(inp),
                    None => out.clear(),
                },
                _ => out.clear(),
            }
        }
        if result == kResultOk || result == kResultTrue {
            ProcessStatus::Continue
        } else {
            ProcessStatus::Error
        }
    }
}

impl PluginProcessor for Vst3Processor {
    fn process(&mut self, ctx: &PluginProcessContext<'_>, io: &mut NodeIo<'_>) -> ProcessStatus {
        let Some(mut guard) = self.cell.try_lock() else {
            silence(io);
            return ProcessStatus::Continue;
        };
        match guard.as_mut() {
            Some(active) => active.process(ctx, io),
            None => {
                silence(io);
                ProcessStatus::Continue
            }
        }
    }

    fn reset(&mut self) {
        // VST3 has no reset: a processing off/on cycle clears tails (the
        // spec allows it on the audio thread, without allocation).
        if let Some(mut g) = self.cell.try_lock()
            && let Some(a) = g.as_mut()
        {
            // SAFETY: the processor is alive and activated.
            unsafe {
                a.processor.setProcessing(0);
                a.processor.setProcessing(1);
            }
        }
    }
}
