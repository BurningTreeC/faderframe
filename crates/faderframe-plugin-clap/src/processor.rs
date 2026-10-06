//! The audio-thread side of a CLAP plugin.
//!
//! The live clack processor sits in a [`TryCell`] shared by every graph
//! node that references the instance (graph rebuilds keep the same
//! processor). Per block, the node builds the plugin's input events
//! (queued UI parameter changes, automation events, notes, MIDI),
//! copies its input into preallocated port buffers and calls `process`.
//! Nothing here allocates: buffers and event storage are sized at
//! activation. If the cell is busy (the control thread is reconfiguring
//! the plugin) or empty (deactivated), the block is silent.

use crate::host::FfHost;
use clack_host::events::event_types::{
    MidiEvent as ClapMidi, MidiSysExEvent, NoteExpressionEvent, NoteExpressionType, NoteOffEvent,
    NoteOnEvent, ParamGestureBeginEvent, ParamGestureEndEvent, ParamModEvent, ParamValueEvent,
    TransportEvent, TransportFlags,
};
use clack_host::events::{EventFlags, EventHeader, Match, Pckn};
use clack_host::prelude::*;
use clack_host::utils::{BeatTime, SecondsTime};
use faderframe_audio_graph::NodeIo;
use faderframe_midi::{MidiEvent, NoteExpressionKind, NoteIds};
use faderframe_plugin_host::{PluginProcessContext, PluginProcessor, ProcessStatus as FfStatus};
use faderframe_realtime::TryCell;
use std::sync::Arc;

/// Events per block handed to the plugin (beyond that, events are dropped).
pub(crate) const EVENT_CAPACITY: usize = 2048;

pub(crate) enum RtProc {
    Stopped(StoppedPluginAudioProcessor<FfHost>),
    Started(StartedPluginAudioProcessor<FfHost>),
}

/// Everything the audio thread needs for one instance.
pub(crate) struct RtState {
    pub proc: Option<RtProc>,
    in_bufs: Vec<Vec<Vec<f32>>>,
    out_bufs: Vec<Vec<Vec<f32>>>,
    /// The port buffers in 64-bit (processing in double precision; the
    /// 32-bit ones are then empty).
    in_bufs64: Vec<Vec<Vec<f64>>>,
    out_bufs64: Vec<Vec<Vec<f64>>>,
    double: bool,
    ports_in: AudioPorts,
    ports_out: AudioPorts,
    events_in: EventBuffer,
    events_out: EventBuffer,
    params_rx: rtrb::Consumer<(u32, f64)>,
    /// Parameter moves the plugin reports (its editor): (0 begin, 1 value,
    /// 2 end, id, value) for automation writing.
    edits_tx: rtrb::Producer<(u8, u32, f64)>,
    steady: u64,
    max_frames: usize,
    /// Ids of the sounding notes (note expressions address them).
    note_ids: Box<NoteIds>,
    /// Modulation sent and still in effect: (parameter, amount).
    mods: Box<[(u32, f32); MAX_MODS]>,
    mod_count: usize,
    /// Parameters whose voices are addressed by note id, not by key
    /// (sorted).
    by_note_id: Box<[u32]>,
}

/// Most parameters modulated at once.
const MAX_MODS: usize = 64;

/// A note expression as CLAP has it (volume: linear gain up to 4 = +12 dB;
/// pan: 0 left … 0.5 centre … 1 right; tuning in semitones).
fn clap_expression(kind: NoteExpressionKind, plain: f64) -> (NoteExpressionType, f64) {
    match kind {
        NoteExpressionKind::Volume => (
            NoteExpressionType::Volume,
            10f64.powf(plain / 20.0).clamp(0.0, 4.0),
        ),
        NoteExpressionKind::Pan => (
            NoteExpressionType::Pan,
            ((plain + 1.0) / 2.0).clamp(0.0, 1.0),
        ),
        NoteExpressionKind::Tuning => (NoteExpressionType::Tuning, plain.clamp(-120.0, 120.0)),
        NoteExpressionKind::Vibrato => (NoteExpressionType::Vibrato, plain.clamp(0.0, 1.0)),
        NoteExpressionKind::Expression => (NoteExpressionType::Expression, plain.clamp(0.0, 1.0)),
        NoteExpressionKind::Brightness => (NoteExpressionType::Brightness, plain.clamp(0.0, 1.0)),
        NoteExpressionKind::Pressure => (NoteExpressionType::Pressure, plain.clamp(0.0, 1.0)),
    }
}

/// The note a note id addresses (any note on the key when there is none).
fn note_match(id: i32) -> Match<u32> {
    u32::try_from(id).map_or(Match::All, Match::Specific)
}

impl RtState {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        proc: StoppedPluginAudioProcessor<FfHost>,
        inputs: &[u16],
        outputs: &[u16],
        max_frames: usize,
        double: bool,
        params_rx: rtrb::Consumer<(u32, f64)>,
        edits_tx: rtrb::Producer<(u8, u32, f64)>,
        by_note_id: Box<[u32]>,
    ) -> Self {
        let (n32, n64) = if double {
            (0, max_frames)
        } else {
            (max_frames, 0)
        };
        let bufs = |ports: &[u16]| -> Vec<Vec<Vec<f32>>> {
            ports
                .iter()
                .map(|&c| (0..c).map(|_| vec![0.0; n32]).collect())
                .collect()
        };
        let bufs64 = |ports: &[u16]| -> Vec<Vec<Vec<f64>>> {
            ports
                .iter()
                .map(|&c| (0..c).map(|_| vec![0.0; n64]).collect())
                .collect()
        };
        let total = |ports: &[u16]| ports.iter().map(|&c| c as usize).sum::<usize>();
        Self {
            proc: Some(RtProc::Stopped(proc)),
            in_bufs: bufs(inputs),
            out_bufs: bufs(outputs),
            in_bufs64: bufs64(inputs),
            out_bufs64: bufs64(outputs),
            double,
            ports_in: AudioPorts::with_capacity(total(inputs), inputs.len()),
            ports_out: AudioPorts::with_capacity(total(outputs), outputs.len()),
            events_in: EventBuffer::with_capacity(EVENT_CAPACITY),
            events_out: EventBuffer::with_capacity(EVENT_CAPACITY),
            params_rx,
            edits_tx,
            steady: 0,
            max_frames,
            note_ids: Box::new(NoteIds::new()),
            mods: Box::new([(0, 0.0); MAX_MODS]),
            mod_count: 0,
            by_note_id,
        }
    }
}

pub(crate) type SharedRt = Arc<TryCell<RtState>>;

/// The graph's handle on a CLAP plugin's processor.
pub struct ClapProcessor {
    pub(crate) cell: SharedRt,
}

fn silence(io: &mut NodeIo<'_>) {
    for out in io.audio_out.iter_mut() {
        out.clear();
    }
}

fn transport_event(t: &faderframe_transport::TransportInfo) -> TransportEvent {
    let mut flags = TransportFlags::HAS_TEMPO
        | TransportFlags::HAS_BEATS_TIMELINE
        | TransportFlags::HAS_SECONDS_TIMELINE
        | TransportFlags::HAS_TIME_SIGNATURE;
    if t.playing {
        flags |= TransportFlags::IS_PLAYING;
    }
    if t.recording {
        flags |= TransportFlags::IS_RECORDING;
    }
    if t.looping {
        flags |= TransportFlags::IS_LOOP_ACTIVE;
    }
    let secs = |s: i64| SecondsTime::from_float(s as f64 / t.sample_rate.max(1.0));
    let (ls, le) = t.loop_range.map_or((0, 0), |r| (r.start, r.end));
    // Beats of the loop points, assuming the tempo at the playhead.
    let beats = |s: i64| {
        BeatTime::from_float(
            t.quarter_position
                + (s - t.sample_position) as f64 / t.sample_rate.max(1.0) * t.tempo / 60.0,
        )
    };
    TransportEvent {
        header: EventHeader::new_core(0, EventFlags::empty()),
        flags,
        song_pos_beats: BeatTime::from_float(t.quarter_position),
        song_pos_seconds: secs(t.sample_position),
        tempo: t.tempo,
        tempo_inc: 0.0,
        loop_start_beats: beats(ls),
        loop_end_beats: beats(le),
        loop_start_seconds: secs(ls),
        loop_end_seconds: secs(le),
        bar_start: BeatTime::from_float(t.bar_start_quarters),
        bar_number: t.bar_index,
        time_signature_numerator: t.time_signature.numerator as u16,
        time_signature_denominator: t.time_signature.denominator as u16,
    }
}

impl PluginProcessor for ClapProcessor {
    fn process(&mut self, ctx: &PluginProcessContext<'_>, io: &mut NodeIo<'_>) -> FfStatus {
        let _audio = crate::host::AudioThreadScope::enter();
        let Some(mut guard) = self.cell.try_lock() else {
            silence(io);
            return FfStatus::Continue;
        };
        let st = &mut *guard;
        let n = io.frames.min(st.max_frames);
        let started = match st.proc.take() {
            None => {
                silence(io);
                return FfStatus::Continue;
            }
            Some(RtProc::Started(s)) => s,
            // CLAP: start_processing happens on the audio thread.
            Some(RtProc::Stopped(s)) => match s.start_processing() {
                Ok(s) => {
                    // Started again: send all the modulation anew.
                    st.mod_count = 0;
                    s
                }
                Err(e) => {
                    st.proc = Some(RtProc::Stopped(e.into_stopped_processor()));
                    silence(io);
                    return FfStatus::Error;
                }
            },
        };
        let mut started = started;

        // Events: UI parameter changes, automation, notes.
        st.events_in.clear();
        st.events_out.clear();
        let mut room = EVENT_CAPACITY;
        while room > 0
            && let Ok((id, value)) = st.params_rx.pop()
        {
            st.events_in.push(&ParamValueEvent::new(
                0,
                ClapId::new(id),
                Pckn::match_all(),
                value,
            ));
            room -= 1;
        }
        let last = n.saturating_sub(1) as u32;
        for e in ctx.param_events.iter().take(room) {
            st.events_in.push(&ParamValueEvent::new(
                e.sample_offset.min(last),
                ClapId::new(e.parameter.0),
                Pckn::match_all(),
                e.value as f64,
            ));
            room -= 1;
        }
        // Modulation (leaves the values as they are): what changed, and 0
        // for what went away.
        let mut i = 0;
        while i < st.mod_count {
            let (id, _) = st.mods[i];
            if room > 0 && !ctx.param_mods.iter().any(|m| m.parameter.0 == id) {
                st.events_in.push(&ParamModEvent::new(
                    0,
                    ClapId::new(id),
                    Pckn::match_all(),
                    0.0,
                ));
                room -= 1;
                st.mod_count -= 1;
                st.mods[i] = st.mods[st.mod_count];
            } else {
                i += 1;
            }
        }
        for m in ctx.param_mods {
            if room == 0 {
                break;
            }
            let known = st.mods[..st.mod_count]
                .iter()
                .position(|(id, _)| *id == m.parameter.0);
            if known.is_some_and(|k| st.mods[k].1 == m.amount) {
                continue;
            }
            match known {
                Some(k) => st.mods[k].1 = m.amount,
                None if st.mod_count < MAX_MODS => {
                    st.mods[st.mod_count] = (m.parameter.0, m.amount);
                    st.mod_count += 1;
                }
                None => continue,
            }
            st.events_in.push(&ParamModEvent::new(
                0,
                ClapId::new(m.parameter.0),
                Pckn::match_all(),
                f64::from(m.amount),
            ));
            room -= 1;
        }
        if let Some(midi) = io.events_in.first() {
            for ev in midi.iter().take(room) {
                let t = ev.sample_offset.min(last);
                match ev.event {
                    MidiEvent::NoteOn {
                        channel,
                        key,
                        velocity,
                    } => {
                        let id = note_match(st.note_ids.start(channel, key));
                        let pckn = Pckn::new(0u16, channel as u16, key as u16, id);
                        st.events_in
                            .push(&NoteOnEvent::new(t, pckn, velocity as f64 / 127.0));
                    }
                    MidiEvent::NoteOff {
                        channel,
                        key,
                        velocity,
                    } => {
                        let id = note_match(st.note_ids.end(channel, key));
                        let pckn = Pckn::new(0u16, channel as u16, key as u16, id);
                        st.events_in
                            .push(&NoteOffEvent::new(t, pckn, velocity as f64 / 127.0));
                    }
                    MidiEvent::NoteExpression {
                        channel,
                        key,
                        kind,
                        value,
                    } => {
                        // Addressed by key and channel (note id −1): plugins
                        // that do not keep the host's note ids (u-he Diva)
                        // only match those, and FaderFrame never has two
                        // notes on one key and channel.
                        let pckn = Pckn::new(0u16, channel as u16, key as u16, Match::All);
                        let (kind, value) = clap_expression(kind, value.get());
                        st.events_in
                            .push(&NoteExpressionEvent::new(t, pckn, kind, value));
                    }
                    MidiEvent::SysEx(r) => {
                        if let Some(bytes) = midi.sysex(&r) {
                            // SAFETY: the bytes live in the graph's input
                            // buffer, borrowed for this whole call; the
                            // event list is cleared before the next block.
                            let e = unsafe { MidiSysExEvent::new(t, 0, bytes) };
                            st.events_in.push(&e);
                        }
                    }
                    other => {
                        let (bytes, _) = other.to_bytes();
                        st.events_in.push(&ClapMidi::new(t, 0, bytes));
                    }
                }
            }
        }
        // Single voices' modulation, after their notes (the sort keeps the
        // order at one time), addressed by key and channel like note
        // expressions, or by note id where the plugin takes only that.
        let room = EVENT_CAPACITY.saturating_sub(st.events_in.len() as usize);
        for m in ctx.note_mods.iter().take(room) {
            let id = if st.by_note_id.binary_search(&m.parameter.0).is_ok() {
                note_match(st.note_ids.get(m.channel, m.key))
            } else {
                Match::All
            };
            let pckn = Pckn::new(0u16, u16::from(m.channel), u16::from(m.key), id);
            st.events_in.push(&ParamModEvent::new(
                m.sample_offset.min(last),
                ClapId::new(m.parameter.0),
                pckn,
                f64::from(m.amount),
            ));
        }
        st.events_in.sort();

        // Audio: graph input `p` into port `p` (the main input, then the
        // sidechain when connected), other ports silent.
        let ports = st.in_bufs.len().max(st.in_bufs64.len());
        for p in 0..ports {
            let channels = if st.double {
                st.in_bufs64[p].len()
            } else {
                st.in_bufs[p].len()
            };
            for c in 0..channels {
                let src = match io.audio_in.get(p) {
                    Some(inp) if inp.num_channels() > 0 => {
                        Some(&inp.channel(c.min(inp.num_channels() - 1))[..n])
                    }
                    _ => None,
                };
                if st.double {
                    let ch = &mut st.in_bufs64[p][c][..n];
                    match src {
                        Some(s) => {
                            for (d, s) in ch.iter_mut().zip(s) {
                                *d = f64::from(*s);
                            }
                        }
                        None => ch.fill(0.0),
                    }
                } else {
                    let ch = &mut st.in_bufs[p][c][..n];
                    match src {
                        Some(s) => ch.copy_from_slice(s),
                        None => ch.fill(0.0),
                    }
                }
            }
        }
        let transport = transport_event(ctx.transport);
        let status = {
            let RtState {
                in_bufs,
                out_bufs,
                in_bufs64,
                out_bufs64,
                double,
                ports_in,
                ports_out,
                events_in,
                events_out,
                steady,
                ..
            } = st;
            if *double {
                let inputs = ports_in.with_input_buffers(in_bufs64.iter_mut().map(|port| {
                    AudioPortBuffer {
                        latency: 0,
                        channels: AudioPortBufferType::f64_input_only(
                            port.iter_mut()
                                .map(|ch| InputChannel::variable(&mut ch[..n])),
                        ),
                    }
                }));
                let mut outputs =
                    ports_out.with_output_buffers(out_bufs64.iter_mut().map(|port| {
                        AudioPortBuffer {
                            latency: 0,
                            channels: AudioPortBufferType::f64_output_only(
                                port.iter_mut().map(|ch| &mut ch[..n]),
                            ),
                        }
                    }));
                started.process(
                    &inputs,
                    &mut outputs,
                    &events_in.as_input(),
                    &mut events_out.as_output(),
                    Some(*steady),
                    Some(&transport),
                )
            } else {
                let inputs = ports_in.with_input_buffers(in_bufs.iter_mut().map(|port| {
                    AudioPortBuffer {
                        latency: 0,
                        channels: AudioPortBufferType::f32_input_only(
                            port.iter_mut()
                                .map(|ch| InputChannel::variable(&mut ch[..n])),
                        ),
                    }
                }));
                let mut outputs = ports_out.with_output_buffers(out_bufs.iter_mut().map(|port| {
                    AudioPortBuffer {
                        latency: 0,
                        channels: AudioPortBufferType::f32_output_only(
                            port.iter_mut().map(|ch| &mut ch[..n]),
                        ),
                    }
                }));
                started.process(
                    &inputs,
                    &mut outputs,
                    &events_in.as_input(),
                    &mut events_out.as_output(),
                    Some(*steady),
                    Some(&transport),
                )
            }
        };
        st.steady += n as u64;
        // Parameter moves the plugin made itself (its editor), for the host.
        for e in st.events_out.iter() {
            let edit = if let Some(v) = e.as_event::<ParamValueEvent>() {
                v.param_id().map(|id| (1u8, id.get(), v.value()))
            } else if let Some(b) = e.as_event::<ParamGestureBeginEvent>() {
                b.param_id().map(|id| (0u8, id.get(), 0.0))
            } else if let Some(b) = e.as_event::<ParamGestureEndEvent>() {
                b.param_id().map(|id| (2u8, id.get(), 0.0))
            } else {
                None
            };
            if let Some(edit) = edit {
                // Full queue: dropped (the next moves follow).
                let _ = st.edits_tx.push(edit);
            }
        }

        // Main output port to the graph (no output ports: pass through).
        if let Some(out) = io.audio_out.first_mut() {
            let main = if st.double {
                st.out_bufs64.first().map_or(0, Vec::len)
            } else {
                st.out_bufs.first().map_or(0, Vec::len)
            };
            match main {
                main if main > 0 => {
                    for c in 0..out.num_channels() {
                        let dst = &mut out.channel_mut(c)[..n];
                        let src = c.min(main - 1);
                        if st.double {
                            for (d, s) in dst.iter_mut().zip(&st.out_bufs64[0][src][..n]) {
                                *d = *s as f32;
                            }
                        } else {
                            dst.copy_from_slice(&st.out_bufs[0][src][..n]);
                        }
                    }
                }
                _ => match io.audio_in.first() {
                    Some(inp) => out.copy_from(inp),
                    None => out.clear(),
                },
            }
        }
        st.proc = Some(RtProc::Started(started));
        match status {
            Ok(ProcessStatus::Sleep) => FfStatus::Sleep,
            Ok(_) => FfStatus::Continue,
            Err(_) => FfStatus::Error,
        }
    }

    fn reset(&mut self) {
        let _audio = crate::host::AudioThreadScope::enter();
        if let Some(mut g) = self.cell.try_lock()
            && let Some(RtProc::Started(s)) = g.proc.as_mut()
        {
            s.reset();
        }
    }
}
