//! The shared memory of one activation ([`Block`]): a [`Header`] — sync
//! words, the block's size and buffer shapes, transport, parameter events,
//! MIDI events both ways — followed by the audio, [`MAX_BUFFERS`] buffers
//! of [`MAX_CHANNELS`] channels of `max_frames` samples each way.
//!
//! Every value crosses as a plain fixed-layout record (`Wire*`), never as
//! a Rust enum, and the host decodes what the helper wrote defensively:
//! counts are clamped, unknown event kinds dropped, non-finite samples
//! zeroed. Ordering: the host writes the request, then `seq` (release); the
//! helper reads `seq` (acquire), processes, writes the response, then
//! `done` (release); the host reads `done` (acquire), then the response.

use crate::sys::Shm;
use crate::wire::{expression_index, expression_kind};
use faderframe_audio_graph::AudioBuffer;
use faderframe_automation::ParameterEvent;
use faderframe_core::{ChannelLayout, ParameterId};
use faderframe_midi::{ExpressionValue, MidiBuffer, MidiEvent, TimedMidiEvent};
use faderframe_plugin_host::ProcessStatus;
use faderframe_timeline::TimeSignature;
use faderframe_transport::{LoopRange, TransportInfo};
use std::io;
use std::ptr::{addr_of, addr_of_mut};
use std::sync::atomic::{AtomicU32, AtomicU64};

pub const MAX_BUFFERS: usize = 4;
pub const MAX_CHANNELS: usize = 16;
pub const MAX_EVENTS: usize = 1024;
pub const MAX_PARAMS: usize = 1024;
const MAGIC: u32 = 0x4646_5348;
const VERSION: u32 = 1;

/// A MIDI event or note expression at a frame offset.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct WireEvent {
    pub offset: u32,
    pub kind: u8,
    pub channel: u8,
    pub a: u8,
    pub b: u8,
    pub value: i32,
}

impl WireEvent {
    pub fn encode(e: &TimedMidiEvent) -> Self {
        let mut w = WireEvent {
            offset: e.sample_offset,
            channel: e.event.channel(),
            ..Self::default()
        };
        match e.event {
            MidiEvent::NoteOn { key, velocity, .. } => (w.kind, w.a, w.b) = (1, key, velocity),
            MidiEvent::NoteOff { key, velocity, .. } => (w.kind, w.a, w.b) = (2, key, velocity),
            MidiEvent::PolyPressure { key, pressure, .. } => {
                (w.kind, w.a, w.b) = (3, key, pressure)
            }
            MidiEvent::ControlChange {
                controller, value, ..
            } => (w.kind, w.a, w.b) = (4, controller, value),
            MidiEvent::ProgramChange { program, .. } => (w.kind, w.a) = (5, program),
            MidiEvent::ChannelPressure { pressure, .. } => (w.kind, w.a) = (6, pressure),
            MidiEvent::PitchBend { value, .. } => (w.kind, w.value) = (7, value as i32),
            MidiEvent::NoteExpression {
                key, kind, value, ..
            } => {
                (w.kind, w.a, w.b, w.value) = (8, key, expression_index(kind), value.raw());
            }
        }
        w
    }

    /// The event, if this is a valid one (data bytes are masked to 7 bits).
    pub fn decode(&self) -> Option<TimedMidiEvent> {
        let channel = self.channel & 15;
        let (a, b) = (self.a & 127, self.b & 127);
        let event = match self.kind {
            1 => MidiEvent::NoteOn {
                channel,
                key: a,
                velocity: b,
            },
            2 => MidiEvent::NoteOff {
                channel,
                key: a,
                velocity: b,
            },
            3 => MidiEvent::PolyPressure {
                channel,
                key: a,
                pressure: b,
            },
            4 => MidiEvent::ControlChange {
                channel,
                controller: a,
                value: b,
            },
            5 => MidiEvent::ProgramChange {
                channel,
                program: a,
            },
            6 => MidiEvent::ChannelPressure {
                channel,
                pressure: a,
            },
            7 => MidiEvent::PitchBend {
                channel,
                value: self.value.clamp(0, 16_383) as u16,
            },
            8 => MidiEvent::NoteExpression {
                channel,
                key: a,
                kind: expression_kind(self.b)?,
                value: ExpressionValue::from_raw(self.value),
            },
            _ => return None,
        };
        Some(TimedMidiEvent::new(self.offset, event))
    }
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct WireParam {
    pub parameter: u32,
    pub offset: u32,
    pub value: f32,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct WireTransport {
    pub playing: u8,
    pub recording: u8,
    pub looping: u8,
    pub has_loop: u8,
    pub sig_numerator: u8,
    pub sig_denominator: u8,
    pub _pad: [u8; 2],
    pub bar_index: i32,
    pub sample_position: i64,
    pub sample_rate: f64,
    pub quarter_position: f64,
    pub tempo: f64,
    pub bar_start_quarters: f64,
    pub loop_start: i64,
    pub loop_end: i64,
}

impl WireTransport {
    pub fn encode(t: &TransportInfo) -> Self {
        let lr = t.loop_range.unwrap_or(LoopRange { start: 0, end: 0 });
        Self {
            playing: t.playing as u8,
            recording: t.recording as u8,
            looping: t.looping as u8,
            has_loop: t.loop_range.is_some() as u8,
            sig_numerator: t.time_signature.numerator,
            sig_denominator: t.time_signature.denominator,
            _pad: [0; 2],
            bar_index: t.bar_index,
            sample_position: t.sample_position,
            sample_rate: t.sample_rate,
            quarter_position: t.quarter_position,
            tempo: t.tempo,
            bar_start_quarters: t.bar_start_quarters,
            loop_start: lr.start,
            loop_end: lr.end,
        }
    }

    pub fn decode(&self) -> TransportInfo {
        let finite = |v: f64, or: f64| if v.is_finite() { v } else { or };
        let signature = if self.sig_numerator > 0 && self.sig_denominator.is_power_of_two() {
            TimeSignature {
                numerator: self.sig_numerator,
                denominator: self.sig_denominator,
            }
        } else {
            TimeSignature::FOUR_FOUR
        };
        TransportInfo {
            playing: self.playing != 0,
            recording: self.recording != 0,
            looping: self.looping != 0,
            sample_position: self.sample_position,
            sample_rate: finite(self.sample_rate, 48_000.0).max(1.0),
            quarter_position: finite(self.quarter_position, 0.0),
            tempo: finite(self.tempo, 120.0).max(1.0),
            time_signature: signature,
            bar_index: self.bar_index,
            bar_start_quarters: finite(self.bar_start_quarters, 0.0),
            loop_range: (self.has_loop != 0).then_some(LoopRange {
                start: self.loop_start,
                end: self.loop_end.max(self.loop_start),
            }),
        }
    }
}

#[repr(C)]
pub struct Header {
    pub magic: u32,
    pub version: u32,
    /// The request the host posted last.
    pub seq: AtomicU32,
    /// The request the helper finished last.
    pub done: AtomicU32,
    /// The helper's audio thread should stop.
    pub quit: AtomicU32,
    /// Reset the processor before the next block.
    pub reset: AtomicU32,
    /// The host audio thread's scheduling (`thread_scheduling`, 0 unknown).
    pub sched: AtomicU64,
    pub sched_extra: AtomicU64,
    pub max_frames: u32,
    pub frames: u32,
    /// 0 continue, 1 sleep, 2 error.
    pub status: u32,
    pub n_in: u32,
    pub n_out: u32,
    pub in_channels: [u32; MAX_BUFFERS],
    pub out_channels: [u32; MAX_BUFFERS],
    pub has_events_in: u32,
    pub has_events_out: u32,
    pub n_events_in: u32,
    pub n_events_out: u32,
    pub n_params: u32,
    pub transport: WireTransport,
    pub params: [WireParam; MAX_PARAMS],
    pub events_in: [WireEvent; MAX_EVENTS],
    pub events_out: [WireEvent; MAX_EVENTS],
}

/// Bytes of the header, rounded up to a cache line.
const fn header_size() -> usize {
    std::mem::size_of::<Header>().div_ceil(64) * 64
}

/// Bytes of a block for `max_frames`.
pub const fn block_size(max_frames: usize) -> usize {
    header_size() + 2 * MAX_BUFFERS * MAX_CHANNELS * max_frames * 4
}

/// What the host posts for one block.
pub struct BlockIn<'a> {
    pub frames: usize,
    pub transport: &'a TransportInfo,
    pub params: &'a [ParameterEvent],
    pub audio_in: &'a [AudioBuffer],
    pub events_in: Option<&'a MidiBuffer>,
    /// Channels of each output buffer.
    pub out_channels: &'a [usize],
    /// The node takes output events.
    pub events_out: bool,
}

/// One activation's shared memory.
pub struct Block {
    shm: Shm,
    max_frames: usize,
}

impl Block {
    /// The host's side: a new block.
    pub fn create(max_frames: usize) -> io::Result<Self> {
        let max_frames = max_frames.clamp(1, 1 << 16);
        let shm = Shm::create(block_size(max_frames))?;
        let b = Self { shm, max_frames };
        let h = b.h();
        // SAFETY: the fresh mapping is ours alone until its name is sent.
        unsafe {
            addr_of_mut!((*h).magic).write(MAGIC);
            addr_of_mut!((*h).version).write(VERSION);
            addr_of_mut!((*h).max_frames).write(max_frames as u32);
        }
        Ok(b)
    }

    /// The helper's side: open the block `name` of `size` bytes.
    pub fn open(name: &str, size: usize) -> io::Result<Self> {
        if size < header_size() {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "not a block"));
        }
        let shm = Shm::open(name, size)?;
        let h = shm.ptr().cast::<Header>();
        // SAFETY: the mapping holds at least a header (checked by size).
        let (magic, version, max_frames) = unsafe {
            (
                addr_of!((*h).magic).read_volatile(),
                addr_of!((*h).version).read_volatile(),
                addr_of!((*h).max_frames).read_volatile() as usize,
            )
        };
        if magic != MAGIC || version != VERSION {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "not a block"));
        }
        if block_size(max_frames) > size {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "block too small",
            ));
        }
        Ok(Self { shm, max_frames })
    }

    pub fn name(&self) -> &str {
        self.shm.name()
    }

    pub fn size(&self) -> usize {
        self.shm.len()
    }

    /// Remove the block's name once the helper has opened it.
    pub fn unlink(&mut self) {
        self.shm.unlink();
    }

    pub fn max_frames(&self) -> usize {
        self.max_frames
    }

    fn h(&self) -> *mut Header {
        self.shm.ptr().cast()
    }

    pub fn header(&self) -> &Header {
        // SAFETY: the mapping holds a header; the fields read through this
        // reference are atomics (the rest is accessed by copying).
        unsafe { &*self.h() }
    }

    /// The samples of channel `ch` of buffer `buf` (`out`: the outputs).
    fn audio(&self, out: bool, buf: usize, ch: usize) -> *mut f32 {
        let index = (out as usize * MAX_BUFFERS + buf) * MAX_CHANNELS + ch;
        // SAFETY: within the mapping: block_size covers every buffer and
        // channel below the maxima for max_frames.
        unsafe {
            self.shm
                .ptr()
                .add(header_size() + index * self.max_frames * 4)
                .cast()
        }
    }

    // --- the host ---------------------------------------------------------------

    /// Post a block's input (before `seq`).
    pub fn write_request(&self, r: &BlockIn<'_>) {
        let BlockIn {
            frames,
            transport,
            params,
            audio_in,
            events_in,
            out_channels,
            events_out,
        } = *r;
        let frames = frames.min(self.max_frames);
        let h = self.h();
        let n_in = audio_in.len().min(MAX_BUFFERS);
        let n_out = out_channels.len().min(MAX_BUFFERS);
        // SAFETY: the host owns the request part until it bumps `seq`; all
        // writes stay within the header and the audio areas.
        unsafe {
            addr_of_mut!((*h).frames).write_volatile(frames as u32);
            addr_of_mut!((*h).n_in).write_volatile(n_in as u32);
            addr_of_mut!((*h).n_out).write_volatile(n_out as u32);
            for (i, b) in audio_in.iter().take(n_in).enumerate() {
                let chans = b.num_channels().min(MAX_CHANNELS);
                addr_of_mut!((*h).in_channels[i]).write_volatile(chans as u32);
                for c in 0..chans {
                    let src = b.channel(c);
                    let n = frames.min(src.len());
                    std::ptr::copy_nonoverlapping(src.as_ptr(), self.audio(false, i, c), n);
                }
            }
            for (i, &chans) in out_channels.iter().take(n_out).enumerate() {
                addr_of_mut!((*h).out_channels[i]).write_volatile(chans.min(MAX_CHANNELS) as u32);
            }
            addr_of_mut!((*h).transport).write_volatile(WireTransport::encode(transport));
            let np = params.len().min(MAX_PARAMS);
            for (i, p) in params.iter().take(np).enumerate() {
                addr_of_mut!((*h).params[i]).write_volatile(WireParam {
                    parameter: p.parameter.0,
                    offset: p.sample_offset,
                    value: p.value,
                });
            }
            addr_of_mut!((*h).n_params).write_volatile(np as u32);
            let mut ne = 0;
            if let Some(ev) = events_in {
                for e in ev.iter().take(MAX_EVENTS) {
                    addr_of_mut!((*h).events_in[ne]).write_volatile(WireEvent::encode(e));
                    ne += 1;
                }
            }
            addr_of_mut!((*h).has_events_in).write_volatile(events_in.is_some() as u32);
            addr_of_mut!((*h).n_events_in).write_volatile(ne as u32);
            addr_of_mut!((*h).has_events_out).write_volatile(events_out as u32);
        }
    }

    /// Read a finished block's output (after `done`): the helper's samples
    /// (non-finite ones zeroed), its output events and status.
    pub fn read_response(
        &self,
        frames: usize,
        audio_out: &mut [AudioBuffer],
        events_out: Option<&mut MidiBuffer>,
    ) -> ProcessStatus {
        let frames = frames.min(self.max_frames);
        let h = self.h();
        // SAFETY: the helper finished (`done`); reads stay within the
        // mapping, counts are clamped before use.
        unsafe {
            for (i, out) in audio_out.iter_mut().take(MAX_BUFFERS).enumerate() {
                for c in 0..out.num_channels().min(MAX_CHANNELS) {
                    let dst = out.channel_mut(c);
                    let n = frames.min(dst.len());
                    std::ptr::copy_nonoverlapping(self.audio(true, i, c), dst.as_mut_ptr(), n);
                    for s in &mut dst[..n] {
                        if !s.is_finite() {
                            *s = 0.0;
                        }
                    }
                }
            }
            if let Some(out) = events_out {
                let n = (addr_of!((*h).n_events_out).read_volatile() as usize).min(MAX_EVENTS);
                for i in 0..n {
                    let w = addr_of!((*h).events_out[i]).read_volatile();
                    if let Some(mut e) = w.decode() {
                        e.sample_offset = e.sample_offset.min(frames.saturating_sub(1) as u32);
                        let _ = out.push(e);
                    }
                }
            }
            match addr_of!((*h).status).read_volatile() {
                1 => ProcessStatus::Sleep,
                2 => ProcessStatus::Error,
                _ => ProcessStatus::Continue,
            }
        }
    }

    // --- the helper -------------------------------------------------------------

    /// Read a posted block into `io` (after `seq`); returns the frame count.
    pub fn read_request(&self, io: &mut HelperIo) -> usize {
        let h = self.h();
        // SAFETY: the host posted the request; reads stay within the
        // mapping, counts are clamped.
        unsafe {
            let frames = (addr_of!((*h).frames).read_volatile() as usize).min(self.max_frames);
            let n_in = (addr_of!((*h).n_in).read_volatile() as usize).min(MAX_BUFFERS);
            let n_out = (addr_of!((*h).n_out).read_volatile() as usize).min(MAX_BUFFERS);
            let mut ins = [0usize; MAX_BUFFERS];
            let mut outs = [0usize; MAX_BUFFERS];
            for (i, c) in ins.iter_mut().enumerate().take(n_in) {
                *c = (addr_of!((*h).in_channels[i]).read_volatile() as usize).min(MAX_CHANNELS);
            }
            for (i, c) in outs.iter_mut().enumerate().take(n_out) {
                *c = (addr_of!((*h).out_channels[i]).read_volatile() as usize).min(MAX_CHANNELS);
            }
            io.shape(&ins[..n_in], &outs[..n_out], self.max_frames);
            for (i, b) in io.ins.iter_mut().enumerate() {
                b.set_len(frames);
                for c in 0..b.num_channels() {
                    std::ptr::copy_nonoverlapping(
                        self.audio(false, i, c),
                        b.channel_mut(c).as_mut_ptr(),
                        frames,
                    );
                }
            }
            for b in io.outs.iter_mut() {
                b.set_len(frames);
                b.clear();
            }
            io.transport = addr_of!((*h).transport).read_volatile().decode();
            io.params.clear();
            let np = (addr_of!((*h).n_params).read_volatile() as usize).min(MAX_PARAMS);
            for i in 0..np {
                let p = addr_of!((*h).params[i]).read_volatile();
                io.params.push(ParameterEvent {
                    parameter: ParameterId(p.parameter),
                    value: p.value,
                    sample_offset: p.offset.min(frames.saturating_sub(1) as u32),
                });
            }
            io.events_in.clear();
            if addr_of!((*h).has_events_in).read_volatile() != 0 {
                let mut buf = io
                    .spare_events
                    .take()
                    .unwrap_or_else(|| MidiBuffer::with_capacity(MAX_EVENTS));
                buf.clear();
                let n = (addr_of!((*h).n_events_in).read_volatile() as usize).min(MAX_EVENTS);
                for i in 0..n {
                    if let Some(e) = addr_of!((*h).events_in[i]).read_volatile().decode() {
                        let _ = buf.push(e);
                    }
                }
                io.events_in.push(buf);
            }
            io.events_out.clear();
            if addr_of!((*h).has_events_out).read_volatile() != 0 {
                let mut buf = io
                    .spare_out_events
                    .take()
                    .unwrap_or_else(|| MidiBuffer::with_capacity(MAX_EVENTS));
                buf.clear();
                io.events_out.push(buf);
            }
            frames
        }
    }

    /// Write the block's output (before `done`).
    pub fn write_response(&self, frames: usize, status: ProcessStatus, io: &mut HelperIo) {
        let h = self.h();
        // SAFETY: the helper owns the response part until it sets `done`;
        // writes stay within the mapping.
        unsafe {
            for (i, b) in io.outs.iter().take(MAX_BUFFERS).enumerate() {
                for c in 0..b.num_channels().min(MAX_CHANNELS) {
                    let src = b.channel(c);
                    let n = frames.min(src.len()).min(self.max_frames);
                    std::ptr::copy_nonoverlapping(src.as_ptr(), self.audio(true, i, c), n);
                }
            }
            let mut n = 0;
            if let Some(ev) = io.events_out.first() {
                for e in ev.iter().take(MAX_EVENTS) {
                    addr_of_mut!((*h).events_out[n]).write_volatile(WireEvent::encode(e));
                    n += 1;
                }
            }
            addr_of_mut!((*h).n_events_out).write_volatile(n as u32);
            addr_of_mut!((*h).status).write_volatile(match status {
                ProcessStatus::Continue => 0,
                ProcessStatus::Sleep => 1,
                ProcessStatus::Error => 2,
            });
        }
        // Keep the buffers for the next block.
        if let Some(b) = io.events_in.pop() {
            io.spare_events = Some(b);
        }
        if let Some(b) = io.events_out.pop() {
            io.spare_out_events = Some(b);
        }
    }
}

/// What the helper's audio thread hands its processor, reused per block.
pub struct HelperIo {
    pub ins: Vec<AudioBuffer>,
    pub outs: Vec<AudioBuffer>,
    pub events_in: Vec<MidiBuffer>,
    pub events_out: Vec<MidiBuffer>,
    pub params: Vec<ParameterEvent>,
    pub transport: TransportInfo,
    spare_events: Option<MidiBuffer>,
    spare_out_events: Option<MidiBuffer>,
}

impl Default for HelperIo {
    fn default() -> Self {
        Self {
            ins: Vec::new(),
            outs: Vec::new(),
            events_in: Vec::with_capacity(1),
            events_out: Vec::with_capacity(1),
            params: Vec::with_capacity(MAX_PARAMS),
            transport: TransportInfo::default(),
            spare_events: None,
            spare_out_events: None,
        }
    }
}

impl HelperIo {
    /// Buffers of these channel counts (reallocated only when they change).
    fn shape(&mut self, ins: &[usize], outs: &[usize], frames: usize) {
        let fits = |have: &[AudioBuffer], want: &[usize]| {
            have.len() == want.len()
                && have
                    .iter()
                    .zip(want)
                    .all(|(b, &c)| b.num_channels() == c && b.capacity() >= frames)
        };
        let make = |want: &[usize]| -> Vec<AudioBuffer> {
            want.iter()
                .map(|&c| AudioBuffer::new(ChannelLayout::from_channel_count(c), frames))
                .collect()
        };
        if !fits(&self.ins, ins) {
            self.ins = make(ins);
        }
        if !fits(&self.outs, outs) {
            self.outs = make(outs);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use faderframe_midi::NoteExpressionKind;

    #[test]
    fn events_and_transport_cross_intact_and_garbage_is_dropped() {
        let events = [
            MidiEvent::NoteOn {
                channel: 3,
                key: 60,
                velocity: 100,
            },
            MidiEvent::PitchBend {
                channel: 1,
                value: 12_345,
            },
            MidiEvent::NoteExpression {
                channel: 0,
                key: 61,
                kind: NoteExpressionKind::Tuning,
                value: ExpressionValue::new(-2.5),
            },
            MidiEvent::ControlChange {
                channel: 15,
                controller: 74,
                value: 1,
            },
        ];
        for (i, e) in events.into_iter().enumerate() {
            let t = TimedMidiEvent::new(i as u32, e);
            assert_eq!(WireEvent::encode(&t).decode(), Some(t));
        }
        let garbage = WireEvent {
            kind: 99,
            ..WireEvent::default()
        };
        assert_eq!(garbage.decode(), None);
        let bad_expression = WireEvent {
            kind: 8,
            b: 200,
            ..WireEvent::default()
        };
        assert_eq!(bad_expression.decode(), None);

        let t = TransportInfo {
            playing: true,
            tempo: 97.5,
            loop_range: Some(LoopRange { start: 10, end: 99 }),
            ..TransportInfo::default()
        };
        assert_eq!(WireTransport::encode(&t).decode(), t);
        let nonsense = WireTransport {
            tempo: f64::NAN,
            sig_denominator: 3,
            sig_numerator: 7,
            ..WireTransport::default()
        };
        let d = nonsense.decode();
        assert_eq!(d.tempo, 120.0);
        assert_eq!(d.time_signature, TimeSignature::FOUR_FOUR);
    }

    #[test]
    fn a_block_carries_audio_events_and_status_both_ways() {
        let frames = 64;
        let host = Block::create(frames).unwrap();
        let helper = Block::open(host.name(), host.size()).unwrap();
        let mut input = AudioBuffer::new(ChannelLayout::Stereo, frames);
        input.set_len(frames);
        input.channel_mut(1).fill(0.25);
        let mut midi = MidiBuffer::with_capacity(8);
        midi.push(TimedMidiEvent::new(
            5,
            MidiEvent::NoteOn {
                channel: 0,
                key: 64,
                velocity: 90,
            },
        ))
        .unwrap();
        let params = [ParameterEvent {
            parameter: ParameterId(4),
            value: 0.5,
            sample_offset: 9,
        }];
        host.write_request(&BlockIn {
            frames,
            transport: &TransportInfo::default(),
            params: &params,
            audio_in: std::slice::from_ref(&input),
            events_in: Some(&midi),
            out_channels: &[2],
            events_out: true,
        });
        let mut io = HelperIo::default();
        assert_eq!(helper.read_request(&mut io), frames);
        assert_eq!(io.ins[0].channel(1)[3], 0.25);
        assert_eq!(io.outs[0].num_channels(), 2);
        assert_eq!(io.params, params);
        assert_eq!(io.events_in[0].iter().count(), 1);
        // The "plugin": output = input × 2, a NaN, an event back.
        for c in 0..2 {
            let src = io.ins[0].channel(c).to_vec();
            for (o, i) in io.outs[0].channel_mut(c).iter_mut().zip(src) {
                *o = i * 2.0;
            }
        }
        io.outs[0].channel_mut(0)[7] = f32::NAN;
        io.events_out[0]
            .push(TimedMidiEvent::new(
                3,
                MidiEvent::NoteOff {
                    channel: 0,
                    key: 64,
                    velocity: 0,
                },
            ))
            .unwrap();
        helper.write_response(frames, ProcessStatus::Sleep, &mut io);
        let mut out = AudioBuffer::new(ChannelLayout::Stereo, frames);
        out.set_len(frames);
        let mut back = MidiBuffer::with_capacity(8);
        let status = host.read_response(frames, std::slice::from_mut(&mut out), Some(&mut back));
        assert_eq!(status, ProcessStatus::Sleep);
        assert_eq!(out.channel(1)[0], 0.5);
        assert_eq!(out.channel(0)[7], 0.0, "non-finite samples are zeroed");
        assert_eq!(back.iter().count(), 1);
    }
}
