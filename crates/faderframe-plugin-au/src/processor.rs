//! The audio-thread side of an Audio Unit.
//!
//! `AudioUnitRender` pulls the unit's input through a render callback that
//! reads the block's input from a preallocated feed, and asks the host for
//! the transport through host callbacks that read a per-block copy of it.
//! Both live in their own heap allocations (raw pointers, so nothing else
//! holds a reference to them while the unit reads). Automation becomes
//! scheduled parameter events, MIDI goes through `MusicDeviceMIDIEvent`.
//! The state sits in a `TryCell` shared with the instance, which takes it
//! before uninitialising the unit; a busy or deactivated cell gives
//! silence.

use crate::ffi::*;
use faderframe_audio_graph::NodeIo;
use faderframe_plugin_host::emulated::{EmulatedMods, ModBases, delta_of};
use faderframe_plugin_host::{ParamMod, PluginProcessContext, PluginProcessor, ProcessStatus};
use faderframe_realtime::TryCell;
use faderframe_transport::TransportInfo;
use std::ffi::c_void;
use std::sync::Arc;

/// Most channels per bus handled.
pub(crate) const MAX_CHANNELS: usize = 16;
/// Scheduled parameter events per block.
const EVENT_CAPACITY: usize = 512;

/// The block's input, read by the unit's render callback.
pub(crate) struct InputFeed {
    bufs: Vec<Vec<f32>>,
    frames: usize,
}

/// The block's transport, read by the unit's host callbacks.
pub(crate) struct HostTransport {
    info: TransportInfo,
    changed: bool,
    was_playing: bool,
}

/// A unit pointer the audio thread may use.
#[derive(Clone, Copy)]
pub(crate) struct UnitPtr(pub AudioUnit);

// SAFETY: an initialised unit may render on any one thread at a time; the
// TryCell serialises the calls, and the instance takes the cell before
// uninitialising or disposing the unit.
unsafe impl Send for UnitPtr {}

pub(crate) struct RtState {
    /// `None` once the instance deactivated the unit.
    pub unit: Option<UnitPtr>,
    feed: *mut InputFeed,
    transport: *mut HostTransport,
    outputs: Vec<Vec<f32>>,
    list: AudioBufferList<MAX_CHANNELS>,
    events: Vec<AudioUnitParameterEvent>,
    sample_time: f64,
    max_frames: usize,
    midi: bool,
    /// Modulation set on the unit (Audio Units have none of their own),
    /// with the parameters' ranges, and the values set elsewhere (the
    /// UI, the unit's editor, a loaded state) as bases.
    emu: EmulatedMods,
    ranges: Box<[(u32, f32, f32)]>,
    bases_rx: rtrb::Consumer<(u32, f64)>,
}

/// What the processor needs to modulate (see [`RtState`]'s `emu`).
pub(crate) struct Modulation {
    /// Every parameter: id, value now, range.
    pub params: Vec<(u32, f64, f32, f32)>,
    pub bases: Arc<ModBases>,
    pub bases_rx: rtrb::Consumer<(u32, f64)>,
}

/// A modulation in the parameter's own units.
fn amount(m: &ParamMod) -> f64 {
    f64::from(m.amount)
}

/// `v` within the parameter's range.
fn clamp_in(ranges: &[(u32, f32, f32)], id: u32, v: f64) -> f64 {
    match ranges.binary_search_by_key(&id, |r| r.0) {
        Ok(i) => v.clamp(f64::from(ranges[i].1), f64::from(ranges[i].2)),
        Err(_) => v,
    }
}

// SAFETY: the raw pointers are owned allocations of this state (freed in
// Drop) that only the thread holding the cell, and the unit while it
// renders on that thread, touch.
unsafe impl Send for RtState {}

impl RtState {
    pub fn new(
        unit: AudioUnit,
        inputs: usize,
        outputs: usize,
        max_frames: usize,
        midi: bool,
        modulation: Modulation,
    ) -> Self {
        let feed = Box::into_raw(Box::new(InputFeed {
            bufs: (0..inputs).map(|_| vec![0.0; max_frames]).collect(),
            frames: 0,
        }));
        let transport = Box::into_raw(Box::new(HostTransport {
            info: TransportInfo::default(),
            changed: false,
            was_playing: false,
        }));
        Self {
            unit: Some(UnitPtr(unit)),
            feed,
            transport,
            outputs: (0..outputs.clamp(1, MAX_CHANNELS))
                .map(|_| vec![0.0; max_frames])
                .collect(),
            list: AudioBufferList::new(),
            events: Vec::with_capacity(EVENT_CAPACITY),
            sample_time: 0.0,
            max_frames,
            midi,
            emu: EmulatedMods::new(
                modulation.params.iter().map(|p| (p.0, p.1)).collect(),
                modulation.bases,
            ),
            ranges: {
                let mut r: Vec<(u32, f32, f32)> =
                    modulation.params.iter().map(|p| (p.0, p.2, p.3)).collect();
                r.sort_unstable_by_key(|p| p.0);
                r.into_boxed_slice()
            },
            bases_rx: modulation.bases_rx,
        }
    }

    /// The input render callback to install (refcon: the feed).
    pub fn render_callback(&self) -> AURenderCallbackStruct {
        AURenderCallbackStruct {
            inputProc: Some(render_input),
            inputProcRefCon: self.feed.cast(),
        }
    }

    /// The transport callbacks to install.
    pub fn host_callbacks(&self) -> HostCallbackInfo {
        HostCallbackInfo {
            hostUserData: self.transport.cast(),
            beatAndTempoProc: Some(beat_and_tempo),
            musicalTimeLocationProc: Some(musical_time_location),
            transportStateProc: Some(transport_state),
            transportStateProc2: Some(transport_state2),
        }
    }
}

impl Drop for RtState {
    fn drop(&mut self) {
        // SAFETY: allocated in `new` with Box::into_raw, freed once here;
        // the unit no longer renders with them (deactivated, or the
        // instance re-installed newer ones before rendering again).
        unsafe {
            drop(Box::from_raw(self.feed));
            drop(Box::from_raw(self.transport));
        }
    }
}

unsafe extern "C" fn render_input(
    ref_con: *mut c_void,
    _flags: *mut u32,
    _time: *const AudioTimeStamp,
    _bus: u32,
    frames: u32,
    data: *mut AudioBufferList<1>,
) -> OSStatus {
    if ref_con.is_null() || data.is_null() {
        return 0;
    }
    // SAFETY: the refcon is the live feed of the rendering state (see
    // `RtState::render_callback`); `data` holds `mNumberBuffers` buffers.
    unsafe {
        let feed = &mut *ref_con.cast::<InputFeed>();
        let count = (*data).mNumberBuffers as usize;
        let buffers = std::ptr::addr_of_mut!((*data).mBuffers).cast::<AudioBuffer>();
        let frames = frames as usize;
        for i in 0..count {
            let b = &mut *buffers.add(i);
            if feed.bufs.is_empty() {
                if !b.mData.is_null() {
                    std::ptr::write_bytes(b.mData.cast::<f32>(), 0, frames);
                }
                continue;
            }
            let last = feed.bufs.len() - 1;
            let src = &mut feed.bufs[i.min(last)];
            let n = frames.min(feed.frames).min(src.len());
            src[n..].fill(0.0);
            if b.mData.is_null() {
                // The unit asks for our buffer.
                b.mData = src.as_mut_ptr().cast();
            } else {
                let dst = std::slice::from_raw_parts_mut(b.mData.cast::<f32>(), frames);
                let m = frames.min(src.len());
                dst[..m].copy_from_slice(&src[..m]);
                dst[m..].fill(0.0);
            }
            b.mDataByteSize = (frames * 4) as u32;
        }
    }
    0
}

/// SAFETY (all host callbacks): `user` is the live transport of the
/// rendering state; out-pointers may be null.
unsafe fn transport<'a>(user: *mut c_void) -> Option<&'a HostTransport> {
    // SAFETY: see above.
    unsafe { user.cast::<HostTransport>().as_ref() }
}

unsafe fn put<T>(p: *mut T, v: T) {
    if !p.is_null() {
        // SAFETY: a non-null out-pointer from the unit.
        unsafe { *p = v };
    }
}

fn beats_at(t: &TransportInfo, sample: i64) -> f64 {
    t.quarter_position
        + (sample - t.sample_position) as f64 / t.sample_rate.max(1.0) * t.tempo / 60.0
}

unsafe extern "C" fn beat_and_tempo(
    user: *mut c_void,
    beat: *mut f64,
    tempo: *mut f64,
) -> OSStatus {
    // SAFETY: host callback contract (see `transport`).
    unsafe {
        let Some(t) = transport(user) else { return -1 };
        put(beat, t.info.quarter_position);
        put(tempo, t.info.tempo);
    }
    0
}

unsafe extern "C" fn musical_time_location(
    user: *mut c_void,
    delta: *mut u32,
    numerator: *mut f32,
    denominator: *mut u32,
    down_beat: *mut f64,
) -> OSStatus {
    // SAFETY: host callback contract (see `transport`).
    unsafe {
        let Some(t) = transport(user) else { return -1 };
        let i = &t.info;
        let to_next = i.quarter_position.ceil() - i.quarter_position;
        let samples = to_next * 60.0 / i.tempo.max(1.0) * i.sample_rate;
        put(delta, samples.max(0.0) as u32);
        put(numerator, f32::from(i.time_signature.numerator));
        put(denominator, u32::from(i.time_signature.denominator));
        put(down_beat, i.bar_start_quarters);
    }
    0
}

unsafe extern "C" fn transport_state(
    user: *mut c_void,
    playing: *mut Boolean,
    changed: *mut Boolean,
    sample: *mut f64,
    cycling: *mut Boolean,
    cycle_start: *mut f64,
    cycle_end: *mut f64,
) -> OSStatus {
    // SAFETY: host callback contract.
    unsafe {
        transport_state2(
            user,
            playing,
            std::ptr::null_mut(),
            changed,
            sample,
            cycling,
            cycle_start,
            cycle_end,
        )
    }
}

#[allow(clippy::too_many_arguments)]
unsafe extern "C" fn transport_state2(
    user: *mut c_void,
    playing: *mut Boolean,
    recording: *mut Boolean,
    changed: *mut Boolean,
    sample: *mut f64,
    cycling: *mut Boolean,
    cycle_start: *mut f64,
    cycle_end: *mut f64,
) -> OSStatus {
    // SAFETY: host callback contract (see `transport`).
    unsafe {
        let Some(t) = transport(user) else { return -1 };
        let i = &t.info;
        put(playing, Boolean::from(i.playing));
        put(recording, Boolean::from(i.recording));
        put(changed, Boolean::from(t.changed));
        put(sample, i.sample_position as f64);
        put(cycling, Boolean::from(i.looping));
        let (s, e) = i.loop_range.map_or((0, 0), |r| (r.start, r.end));
        put(cycle_start, beats_at(i, s));
        put(cycle_end, beats_at(i, e));
    }
    0
}

pub(crate) type SharedRt = Arc<TryCell<RtState>>;

/// The graph's handle on an Audio Unit.
pub struct AuProcessor {
    pub(crate) cell: SharedRt,
}

fn silence(io: &mut NodeIo<'_>) {
    for out in io.audio_out.iter_mut() {
        out.clear();
    }
}

impl PluginProcessor for AuProcessor {
    fn process(&mut self, ctx: &PluginProcessContext<'_>, io: &mut NodeIo<'_>) -> ProcessStatus {
        let Some(mut guard) = self.cell.try_lock() else {
            silence(io);
            return ProcessStatus::Continue;
        };
        let st = &mut *guard;
        let Some(UnitPtr(unit)) = st.unit else {
            silence(io);
            return ProcessStatus::Continue;
        };
        let n = io.frames.min(st.max_frames);

        // Input and transport for the unit's callbacks.
        // SAFETY: owned allocations of this state; nothing else uses them
        // until the render below, on this thread.
        unsafe {
            let feed = &mut *st.feed;
            for (c, buf) in feed.bufs.iter_mut().enumerate() {
                match io.audio_in.first() {
                    Some(inp) if inp.num_channels() > 0 => {
                        let s = inp.channel(c.min(inp.num_channels() - 1));
                        buf[..n].copy_from_slice(&s[..n]);
                    }
                    _ => buf[..n].fill(0.0),
                }
            }
            feed.frames = n;
            let t = &mut *st.transport;
            t.info = *ctx.transport;
            t.changed = t.info.playing != t.was_playing;
            t.was_playing = t.info.playing;
        }

        // Automation, sample-accurate, with any modulation on top; then
        // the block's modulation from its start.
        st.events.clear();
        let last = n.saturating_sub(1) as u32;
        let mods = ctx.param_mods;
        while let Ok((id, v)) = st.bases_rx.pop() {
            st.emu.set(id, v);
        }
        let event = |parameter: u32, offset: u32, value: f64| AudioUnitParameterEvent {
            scope: kAudioUnitScope_Global,
            element: 0,
            parameter,
            eventType: kParameterEvent_Immediate,
            bufferOffset: offset,
            value: value as f32,
            _ramp_rest: [0; 2],
        };
        for e in ctx.param_events.iter().take(EVENT_CAPACITY) {
            let id = e.parameter.0;
            st.emu.set(id, f64::from(e.value));
            let v = match delta_of(mods, id, amount) {
                Some(d) => clamp_in(&st.ranges, id, f64::from(e.value) + d),
                None => f64::from(e.value),
            };
            st.events.push(event(id, e.sample_offset.min(last), v));
        }
        {
            let RtState {
                emu,
                events,
                ranges,
                ..
            } = st;
            // Automated parameters had their modulation with each point.
            let automated = |id: u32| ctx.param_events.iter().any(|e| e.parameter.0 == id);
            emu.block(
                mods,
                amount,
                |id, v| clamp_in(ranges, id, v),
                |id, v| {
                    if !automated(id) && events.len() < events.capacity() {
                        events.push(event(id, 0, v));
                    }
                },
            );
        }
        // SAFETY: an initialised unit (see `UnitPtr`); the event slice and
        // MIDI bytes are valid for the calls.
        unsafe {
            if !st.events.is_empty()
                && AudioUnitScheduleParameters(unit, st.events.as_ptr(), st.events.len() as u32)
                    != 0
            {
                // Units that do not schedule: set at the block start.
                for e in &st.events {
                    AudioUnitSetParameter(unit, e.parameter, e.scope, e.element, e.value, 0);
                }
            }
            if st.midi
                && let Some(midi) = io.events_in.first()
            {
                for ev in midi.iter() {
                    if let faderframe_midi::MidiEvent::SysEx(r) = ev.event {
                        // No sample offset: it applies from the block start.
                        if let Some(bytes) = midi.sysex(&r) {
                            MusicDeviceSysEx(unit, bytes.as_ptr(), bytes.len() as u32);
                        }
                        continue;
                    }
                    let (b, len) = ev.event.to_bytes();
                    if len == 0 {
                        continue;
                    }
                    MusicDeviceMIDIEvent(
                        unit,
                        u32::from(b[0]),
                        u32::from(b[1]),
                        u32::from(b[2]),
                        ev.sample_offset.min(last),
                    );
                }
            }
        }

        // Render into our output buffers.
        let channels = st.outputs.len();
        st.list.mNumberBuffers = channels as u32;
        for (b, out) in st.list.mBuffers.iter_mut().zip(st.outputs.iter_mut()) {
            *b = AudioBuffer {
                mNumberChannels: 1,
                mDataByteSize: (n * 4) as u32,
                mData: out.as_mut_ptr().cast(),
            };
        }
        let stamp = AudioTimeStamp {
            mSampleTime: st.sample_time,
            mFlags: kAudioTimeStampSampleTimeValid,
            ..Default::default()
        };
        let mut flags = 0u32;
        // SAFETY: as above; the list describes `channels` buffers of at
        // least `n` frames.
        let status = unsafe {
            AudioUnitRender(
                unit,
                &mut flags,
                &stamp,
                0,
                n as u32,
                (&mut st.list as *mut AudioBufferList<MAX_CHANNELS>).cast(),
            )
        };
        st.sample_time += n as f64;
        if status != 0 {
            silence(io);
            return ProcessStatus::Error;
        }
        if let Some(out) = io.audio_out.first_mut() {
            let got = (st.list.mNumberBuffers as usize).min(channels);
            for c in 0..out.num_channels() {
                if got == 0 {
                    out.channel_mut(c)[..n].fill(0.0);
                    continue;
                }
                let b = st.list.mBuffers[c.min(got - 1)];
                if b.mData.is_null() {
                    out.channel_mut(c)[..n].fill(0.0);
                    continue;
                }
                // SAFETY: the unit rendered `n` frames into this buffer
                // (ours, or one of its own it pointed us to).
                let src = unsafe { std::slice::from_raw_parts(b.mData.cast::<f32>(), n) };
                out.channel_mut(c)[..n].copy_from_slice(src);
            }
        }
        ProcessStatus::Continue
    }

    fn reset(&mut self) {
        if let Some(g) = self.cell.try_lock()
            && let Some(UnitPtr(unit)) = g.unit
        {
            // SAFETY: an initialised unit, called from the thread holding
            // the cell.
            unsafe { AudioUnitReset(unit, kAudioUnitScope_Global, 0) };
        }
    }
}
