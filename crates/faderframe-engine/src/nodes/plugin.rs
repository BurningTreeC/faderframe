use super::{AUTOMATION_STEP, automation_at, ramp_step};
use crate::context::EngineContext;
use faderframe_audio_graph::{NodeIo, ProcessContext, Processor};
use faderframe_automation::ParameterEvent;
use faderframe_core::{ParameterId, PluginInstanceId, TrackId};
use faderframe_midi::{MidiBuffer, MidiEvent, TimedMidiEvent};
use faderframe_plugin_host::{
    NoteParamMod, ParamMod, PluginProcessContext, PluginProcessor, ProcessStatus,
};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering};

/// Most parameter events handed to a plugin per block.
const EVENT_CAPACITY: usize = 1024;
/// Most automated parameters tracked per plugin.
const MAX_AUTOMATED: usize = 128;
/// Most runs of the timeline one block's audio is read back in (a loop
/// shorter than the block wraps more often; the rest is skipped).
const MAX_RUNS: usize = 8;

/// Controllers whose letting go a plugin must not miss: the pedals
/// (sustain, portamento, sostenuto, soft, legato, hold 2) up, all sound
/// off, all notes off.
const RELEASES: [u8; 8] = [64, 65, 66, 67, 68, 69, 120, 123];

/// What the blocks a plugin's processor missed ([`ProcessStatus::Missed`])
/// would have ended — note-offs, pedals let go, all notes off — kept until
/// one of its blocks gets through, which then starts with them. Missed
/// note-ons stay missed. One per plugin instance, shared by its nodes in
/// every graph (a missed block is mostly another graph's turn with it):
/// atomic, never allocating.
pub struct MissedReleases {
    any: AtomicBool,
    /// A bit per channel and key.
    notes: [AtomicU64; 16 * 128 / 64],
    /// Per channel a bit per [`RELEASES`] controller.
    controls: [AtomicU64; 16 * RELEASES.len() / 64],
}

impl Default for MissedReleases {
    fn default() -> Self {
        Self {
            any: AtomicBool::new(false),
            notes: std::array::from_fn(|_| AtomicU64::new(0)),
            controls: std::array::from_fn(|_| AtomicU64::new(0)),
        }
    }
}

impl MissedReleases {
    fn set(words: &[AtomicU64], bit: usize) {
        if let Some(w) = words.get(bit / 64) {
            w.fetch_or(1 << (bit % 64), Ordering::AcqRel);
        }
    }

    /// Keep what a missed block's `events` end.
    pub fn keep(&self, events: &MidiBuffer) {
        let mut kept = false;
        for e in events.iter() {
            match e.event {
                MidiEvent::NoteOff { channel, key, .. }
                | MidiEvent::NoteOn {
                    channel,
                    key,
                    velocity: 0,
                } => {
                    Self::set(
                        &self.notes,
                        usize::from(channel & 15) * 128 + usize::from(key & 127),
                    );
                    kept = true;
                }
                MidiEvent::ControlChange {
                    channel,
                    controller,
                    value,
                } => {
                    if let Some(j) = RELEASES.iter().position(|c| *c == controller)
                        && (controller >= 120 || value < 64)
                    {
                        Self::set(
                            &self.controls,
                            usize::from(channel & 15) * RELEASES.len() + j,
                        );
                        kept = true;
                    }
                }
                _ => {}
            }
        }
        if kept {
            self.any.store(true, Ordering::Release);
        }
    }

    /// Anything kept?
    pub fn pending(&self) -> bool {
        self.any.load(Ordering::Acquire)
    }

    /// What was kept into `out` at offset 0 (what does not fit stays kept).
    pub fn take_into(&self, out: &mut MidiBuffer) {
        if !self.any.swap(false, Ordering::AcqRel) {
            return;
        }
        let mut left = false;
        for (w, word) in self.notes.iter().enumerate() {
            let mut bits = word.swap(0, Ordering::AcqRel);
            while bits != 0 {
                let b = bits.trailing_zeros() as usize;
                bits &= bits - 1;
                let i = w * 64 + b;
                let off = MidiEvent::NoteOff {
                    channel: (i / 128) as u8,
                    key: (i % 128) as u8,
                    velocity: 0,
                };
                if out.push(TimedMidiEvent::new(0, off)).is_err() {
                    word.fetch_or(1 << b, Ordering::AcqRel);
                    left = true;
                }
            }
        }
        for (w, word) in self.controls.iter().enumerate() {
            let mut bits = word.swap(0, Ordering::AcqRel);
            while bits != 0 {
                let b = bits.trailing_zeros() as usize;
                bits &= bits - 1;
                let i = w * 64 + b;
                let up = MidiEvent::ControlChange {
                    channel: (i / RELEASES.len()) as u8,
                    controller: RELEASES[i % RELEASES.len()],
                    value: 0,
                };
                if out.push(TimedMidiEvent::new(0, up)).is_err() {
                    word.fetch_or(1 << b, Ordering::AcqRel);
                    left = true;
                }
            }
        }
        if left {
            self.any.store(true, Ordering::Release);
        }
    }
}

/// Where a plugin's event output came from in its last block.
const FROM_NOTHING: u8 = 0;
const FROM_PROCESSOR: u8 = 1;
const FROM_INPUT: u8 = 2;

/// A bit per channel and key.
type KeyBits = [AtomicU64; 16 * 128 / 64];

fn key_bit(channel: u8, key: u8) -> usize {
    usize::from(channel & 15) * 128 + usize::from(key & 127)
}

fn set_key(bits: &KeyBits, i: usize, on: bool) {
    if let Some(w) = bits.get(i / 64) {
        if on {
            w.fetch_or(1 << (i % 64), Ordering::AcqRel);
        } else {
            w.fetch_and(!(1 << (i % 64)), Ordering::AcqRel);
        }
    }
}

/// Note-ons and note-offs in `events` into `bits` (all notes off, all
/// sound off clear a channel).
fn follow_keys(bits: &KeyBits, events: &MidiBuffer) {
    for e in events.iter() {
        match e.event {
            MidiEvent::NoteOn {
                channel,
                key,
                velocity,
            } => set_key(bits, key_bit(channel, key), velocity > 0),
            MidiEvent::NoteOff { channel, key, .. } => set_key(bits, key_bit(channel, key), false),
            MidiEvent::ControlChange {
                channel,
                controller: 120 | 123,
                ..
            } => {
                let first = usize::from(channel & 15) * 2;
                for w in &bits[first..first + 2] {
                    w.store(0, Ordering::Release);
                }
            }
            _ => {}
        }
    }
}

/// Note-offs at offset 0 for every key in `bits` (cleared as they go;
/// what does not fit stays).
fn end_keys(bits: &KeyBits, out: &mut MidiBuffer) {
    for (w, word) in bits.iter().enumerate() {
        let mut keys = word.swap(0, Ordering::AcqRel);
        while keys != 0 {
            let b = keys.trailing_zeros() as usize;
            keys &= keys - 1;
            let i = w * 64 + b;
            let off = MidiEvent::NoteOff {
                channel: (i / 128) as u8,
                key: (i % 128) as u8,
                velocity: 0,
            };
            if out.push(TimedMidiEvent::new(0, off)).is_err() {
                word.fetch_or(1 << b, Ordering::AcqRel);
            }
        }
    }
}

/// What notes reach a plugin and leave it, so that switching between
/// playing it and passing its notes by (bypass, by hand or automated; a
/// failed plugin) leaves nothing sounding: what one side started, the
/// other cannot end. A MIDI effect passed by ends the notes it made and
/// sounds the keys held at its input; played again, it is told that what
/// it was given ended and given the keys held now, and the notes that
/// passed it end. An instrument played again likewise. One per plugin
/// instance, shared by its nodes in every graph (a bypass change builds
/// a new node): atomic, never allocating.
pub struct NoteLedger {
    /// Velocity per channel and key held at the plugin's event input
    /// (0 = up), whichever way the notes went.
    held: [AtomicU8; 16 * 128],
    /// Keys the processor was given and not let go.
    given: KeyBits,
    /// Keys sounding at the plugin's event output (a MIDI effect's).
    out: KeyBits,
    /// The processor got the last block's notes.
    fed: AtomicBool,
    /// Where the last block's event output came from (`FROM_*`).
    source: AtomicU8,
    /// What fed its notes in the last block (the node's `upstream`; 0:
    /// none ran yet).
    fed_by: AtomicU64,
}

impl Default for NoteLedger {
    fn default() -> Self {
        Self {
            held: std::array::from_fn(|_| AtomicU8::new(0)),
            given: std::array::from_fn(|_| AtomicU64::new(0)),
            out: std::array::from_fn(|_| AtomicU64::new(0)),
            fed: AtomicBool::new(true),
            source: AtomicU8::new(FROM_NOTHING),
            fed_by: AtomicU64::new(0),
        }
    }
}

impl NoteLedger {
    /// Keys held at the input after `events`.
    fn follow_input(&self, events: &MidiBuffer) {
        for e in events.iter() {
            match e.event {
                MidiEvent::NoteOn {
                    channel,
                    key,
                    velocity,
                } => self.held[key_bit(channel, key)].store(velocity, Ordering::Release),
                MidiEvent::NoteOff { channel, key, .. } => {
                    self.held[key_bit(channel, key)].store(0, Ordering::Release);
                }
                MidiEvent::ControlChange {
                    channel,
                    controller: 120 | 123,
                    ..
                } => {
                    let first = usize::from(channel & 15) * 128;
                    for k in &self.held[first..first + 128] {
                        k.store(0, Ordering::Release);
                    }
                }
                _ => {}
            }
        }
    }

    /// Note-ons at offset 0 for the keys held at the input, but those the
    /// block's own `input` starts or ends at offset 0.
    fn sound_held(&self, input: Option<&MidiBuffer>, out: &mut MidiBuffer) {
        let mut own = [0u64; 16 * 128 / 64];
        for e in input.into_iter().flat_map(|i| i.range(0, 1)) {
            if let MidiEvent::NoteOn { channel, key, .. }
            | MidiEvent::NoteOff { channel, key, .. } = e.event
            {
                let i = key_bit(channel, key);
                own[i / 64] |= 1 << (i % 64);
            }
        }
        for (i, held) in self.held.iter().enumerate() {
            let velocity = held.load(Ordering::Acquire);
            if velocity == 0 || own[i / 64] & (1 << (i % 64)) != 0 {
                continue;
            }
            let on = MidiEvent::NoteOn {
                channel: (i / 128) as u8,
                key: (i % 128) as u8,
                velocity,
            };
            let _ = out.push(TimedMidiEvent::new(0, on));
        }
    }

    /// A node fed by `upstream` plays now: `true` when the last block was
    /// fed by something else (a MIDI effect before it removed, added or
    /// moved; a MIDI track playing it gone) — what that started, nothing
    /// will end.
    fn rewired(&self, upstream: u64) -> bool {
        if self.fed_by.load(Ordering::Acquire) == upstream {
            return false;
        }
        self.fed_by.swap(upstream, Ordering::AcqRel) != 0
    }

    /// Rewired: what the processor was given ends (into `out`, before the
    /// block's notes), and the keys held at the input are forgotten (they
    /// came from what fed it before).
    fn start_over(&self, out: &mut MidiBuffer) {
        end_keys(&self.given, out);
        self.forget_held();
    }

    fn forget_held(&self) {
        for k in &self.held {
            k.store(0, Ordering::Release);
        }
    }

    /// The processor gets this block's notes after blocks it did not get
    /// (`true`: it is to be told the news first, [`Self::catch_up`]).
    fn feeds(&self) -> bool {
        !self.fed.swap(true, Ordering::AcqRel)
    }

    /// Blocks went by the processor.
    fn starve(&self) {
        self.fed.store(false, Ordering::Release);
    }

    /// For a processor played again: what it was given ends, the keys
    /// held now begin (into `out`, before the block's notes).
    fn catch_up(&self, input: Option<&MidiBuffer>, out: &mut MidiBuffer) {
        end_keys(&self.given, out);
        self.sound_held(input, out);
    }

    /// What the processor was given (it did not miss the block).
    fn follow_given(&self, events: &MidiBuffer) {
        follow_keys(&self.given, events);
    }

    /// This block's event output came from `source`; when the last one's
    /// came from elsewhere, what that left sounding ends first, and a
    /// MIDI effect passed by sounds the keys held at its input.
    fn switch_output(&self, source: u8, input: Option<&MidiBuffer>, out: &mut MidiBuffer) {
        let was = self.source.swap(source, Ordering::AcqRel);
        if was == source || was == FROM_NOTHING {
            return;
        }
        end_keys(&self.out, out);
        if source == FROM_INPUT {
            self.sound_held(input, out);
        }
    }

    /// What the event output sounds after `events`.
    fn follow_output(&self, events: &MidiBuffer) {
        follow_keys(&self.out, events);
    }

    /// A reset processor holds nothing, and what it sent is silenced.
    fn forget(&self) {
        for w in self.given.iter().chain(self.out.iter()) {
            w.store(0, Ordering::Release);
        }
    }
}

/// Hosts a plugin processor (insert or instrument) inside the graph.
///
/// Bypassed or failed plugins pass audio through (instruments go silent).
/// The reported latency is the instance's latency at build time; a bypassed
/// plugin reports zero (the graph is rebuilt when bypass changes).
///
/// Automated parameters reach the plugin as sample-accurate
/// [`ParameterEvent`]s: one wherever the curve has a breakpoint and one
/// every [`AUTOMATION_STEP`] frames while the value changes. Automated
/// bypass is a *soft* bypass: the plugin keeps running (so its latency and
/// state stay consistent), and the output crossfades to the input delayed
/// by the plugin's latency, keeping every summing point aligned.
///
/// A block the processor missed (its shared processor another graph's
/// turn, its helper process late) loses no note-off: [`MissedReleases`]
/// keeps them for the plugin's next block, in whichever graph.
pub struct PluginNode {
    track: TrackId,
    plugin: PluginInstanceId,
    processor: Box<dyn PluginProcessor>,
    latency: u32,
    bypass: bool,
    failed: Arc<AtomicBool>,
    events: Vec<ParameterEvent>,
    /// Last value sent per automated parameter (to skip repeats).
    sent: Vec<(ParameterId, f32)>,
    /// This block's modulation of the plugin's parameters, and of its
    /// single voices (from the notes it gets).
    mods: Vec<ParamMod>,
    note_mods: Vec<NoteParamMod>,
    voices: Box<crate::modulation::NoteVoices>,
    /// Soft-bypass mix: 0 = plugin output, 1 = (delayed) input.
    dry: f32,
    /// Latency-matching delay of the dry signal, per channel.
    delay: Vec<Box<[f32]>>,
    delay_pos: usize,
    /// How late its input is (its automation is read that much earlier).
    input_latency: u32,
    /// What blocks its processor missed would have ended (the plugin's).
    missed: Arc<MissedReleases>,
    /// What notes reached the plugin and left it (the plugin's).
    notes: Arc<NoteLedger>,
    /// What feeds its notes (a signature of the MIDI effects before it and
    /// the MIDI tracks playing its track; part of its node key).
    upstream: u64,
    /// A block's events after those (a node that takes events: one
    /// buffer, else none).
    merged: Vec<MidiBuffer>,
}

impl PluginNode {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        track: TrackId,
        plugin: PluginInstanceId,
        processor: Box<dyn PluginProcessor>,
        latency: u32,
        bypass: bool,
        failed: Arc<AtomicBool>,
        channels: usize,
        missed: Arc<MissedReleases>,
        notes: Arc<NoteLedger>,
        takes_events: bool,
        upstream: u64,
    ) -> Self {
        let latency = if bypass { 0 } else { latency };
        Self {
            track,
            plugin,
            processor,
            latency,
            bypass,
            failed,
            events: Vec::with_capacity(EVENT_CAPACITY),
            sent: Vec::with_capacity(MAX_AUTOMATED),
            mods: Vec::with_capacity(crate::modulation::MAX_PARAM_MODS),
            note_mods: Vec::with_capacity(crate::modulation::MAX_NOTE_MODS),
            voices: Box::default(),
            dry: 0.0,
            delay: (0..channels)
                .map(|_| vec![0.0; latency as usize].into_boxed_slice())
                .collect(),
            delay_pos: 0,
            input_latency: 0,
            missed,
            notes,
            upstream,
            merged: if takes_events {
                vec![MidiBuffer::with_capacity(MidiBuffer::DEFAULT_CAPACITY)]
            } else {
                Vec::new()
            },
        }
    }

    fn last_sent(&mut self, id: ParameterId) -> Option<&mut f32> {
        self.sent.iter_mut().find(|(p, _)| *p == id).map(|(_, v)| v)
    }

    /// Fill `self.events` from the automation of this plugin.
    fn collect_events(&mut self, cx: &ProcessContext<'_, EngineContext>, frames: usize) {
        self.events.clear();
        let Some(auto) = cx.data.timeline.automation(self.track) else {
            return;
        };
        let latency = self.input_latency;
        // The runs of the timeline this block's audio was played in: the
        // block itself, or — behind latency — what the play history says
        // (split where a loop wrapped or a locate jumped back then).
        let mut runs = [(0usize, 0usize, crate::history::Played::default()); MAX_RUNS];
        let mut count = 0;
        if latency == 0 {
            runs[0] = (
                0,
                frames,
                crate::history::Played {
                    pos: cx.data.transport.sample_position,
                    playing: cx.data.transport.playing,
                    jumps: 0,
                    start: 0,
                    count_in: 0,
                },
            );
            count = 1;
        } else {
            cx.data
                .history
                .pieces(latency, 0, frames, |off, n, played| {
                    if count < MAX_RUNS {
                        runs[count] = (off, n, played);
                        count += 1;
                    }
                });
        }
        for (id, lane) in auto.plugin_params(self.plugin) {
            let emit = |node: &mut Self, offset: usize, at: i64| {
                let Some(v) = lane.value_at(at) else { return };
                let v = v as f32;
                match node.last_sent(id) {
                    Some(last) if *last == v => return,
                    Some(last) => *last = v,
                    None => {
                        if node.sent.len() < MAX_AUTOMATED {
                            node.sent.push((id, v));
                        }
                    }
                }
                if node.events.len() < EVENT_CAPACITY {
                    node.events.push(ParameterEvent {
                        parameter: id,
                        value: v,
                        sample_offset: offset as u32,
                    });
                }
            };
            emit(self, 0, automation_at(cx, latency, 0));
            for &(first, n, played) in &runs[..count] {
                if first > 0 {
                    emit(self, first, played.pos);
                }
                if !played.playing {
                    continue;
                }
                let start = played.pos;
                // Breakpoints inside the run, exactly.
                let mut s = start;
                while let Some(at) = lane.next_point_in(s, start + n as i64) {
                    emit(self, first + (at - start) as usize, at);
                    s = at;
                }
                // Ramps between them.
                let mut off = first.div_ceil(AUTOMATION_STEP).max(1) * AUTOMATION_STEP;
                while off < first + n {
                    emit(self, off, start + (off - first) as i64);
                    off += AUTOMATION_STEP;
                }
            }
        }
        // By offset, in place: an unstable sort (a stable one takes heap
        // scratch past a few hundred events). Events tied on offset and
        // parameter come from the same lane at the same position: the same
        // value, so their order does not matter.
        self.events
            .sort_unstable_by_key(|e| (e.sample_offset, e.parameter.0));
    }

    /// Mix towards the dry input as the bypass lane says; `true` when a MIDI
    /// effect's notes are to pass it by (unchanged) this block.
    fn soft_bypass(&mut self, cx: &ProcessContext<'_, EngineContext>, io: &mut NodeIo<'_>) -> bool {
        let target = cx
            .data
            .timeline
            .automation(self.track)
            .and_then(|a| a.bypass(self.plugin))
            .and_then(|l| l.value_at(automation_at(cx, self.input_latency, io.frames)))
            .map_or(0.0, |v| if v >= 0.5 { 1.0 } else { 0.0 });
        // A MIDI effect (no audio): bypassed, its notes pass unchanged.
        if io.audio_out.is_empty() {
            self.dry = target;
            return target >= 0.5;
        }
        let n = io.frames;
        let latency = self.latency as usize;
        let start_pos = self.delay_pos;
        let from = self.dry;
        let step = ramp_step(from, target, n);
        let Some(out) = io.audio_out.first_mut() else {
            return false;
        };
        for c in 0..out.num_channels() {
            let input = io
                .audio_in
                .first()
                .map(|i| i.channel(c.min(i.num_channels().saturating_sub(1))));
            let mut delay = self.delay.get_mut(c);
            let mut g = from;
            let mut pos = start_pos;
            for (i, o) in out.channel_mut(c).iter_mut().enumerate() {
                g += step;
                let x = input.map_or(0.0, |inp| inp[i]);
                let dry = match delay.as_deref_mut() {
                    Some(d) if latency > 0 => {
                        let y = d[pos];
                        d[pos] = x;
                        pos = (pos + 1) % latency;
                        y
                    }
                    _ => x,
                };
                *o = *o * (1.0 - g) + dry * g;
            }
        }
        if latency > 0 {
            self.delay_pos = (start_pos + n) % latency;
        }
        self.dry = target;
        false
    }

    /// The event outputs after the block: the processor's, or (`passed`) the
    /// inputs unchanged — a bypassed MIDI effect's; other bypassed plugins
    /// have none. A change of side ends what the other left sounding.
    fn notes_out(&self, io: &mut NodeIo<'_>, passed: bool, rewired: bool) {
        let source = if passed { FROM_INPUT } else { FROM_PROCESSOR };
        for (i, out) in io.events_out.iter_mut().enumerate() {
            let input = io.events_in.get(i);
            if passed {
                out.clear();
            }
            if i == 0 {
                // Rewired: what it had sounding came from what fed it
                // before.
                if rewired {
                    end_keys(&self.notes.out, out);
                }
                self.notes.switch_output(source, input, out);
            }
            if passed && let Some(input) = input {
                out.merge_from(input, 0);
            }
            if i == 0 {
                self.notes.follow_output(out);
            }
        }
    }

    /// The keys held at the input after the block (a node that takes
    /// events).
    fn notes_in(&self, io: &NodeIo<'_>) {
        if !self.merged.is_empty()
            && let Some(input) = io.events_in.first()
        {
            self.notes.follow_input(input);
        }
    }
}

impl Processor<EngineContext> for PluginNode {
    fn preferred_block_size(&self) -> usize {
        if self.bypass {
            usize::MAX
        } else {
            self.processor.preferred_block_size()
        }
    }

    fn latency(&self) -> u32 {
        self.latency
    }

    fn set_input_latency(&mut self, frames: u32) {
        self.input_latency = frames;
    }

    fn process(&mut self, cx: &ProcessContext<'_, EngineContext>, io: &mut NodeIo<'_>) {
        let takes = !self.merged.is_empty();
        let rewired = takes && self.notes.rewired(self.upstream);
        if self.bypass || self.failed.load(Ordering::Relaxed) {
            for (i, out) in io.audio_out.iter_mut().enumerate() {
                match io.audio_in.get(i) {
                    Some(input) => out.copy_from(input),
                    None => out.clear(),
                }
            }
            if takes {
                self.notes.starve();
            }
            if rewired {
                // The held keys came from what fed it before (what the
                // processor was given ends when it is played again).
                self.notes.forget_held();
            }
            self.notes_out(io, true, rewired);
            self.notes_in(io);
            return;
        }
        self.collect_events(cx, io.frames);
        self.note_mods.clear();
        match cx.data.modulation.track(self.track) {
            Some(m) => {
                // The modulation its audio passed the modulators with.
                let came_in = cx.data.history.chunk_start() - i64::from(self.input_latency);
                m.plugin_mods_at(self.plugin, &mut self.mods, m.bus.slot_at(came_in));
                if m.moves_notes_of(self.plugin) {
                    self.voices.process(
                        m,
                        self.plugin,
                        io.events_in.first(),
                        io.frames,
                        &cx.data.transport,
                        cx.data.discontinuity,
                        &mut self.mods,
                        &mut self.note_mods,
                    );
                }
            }
            None => self.mods.clear(),
        }
        let events = std::mem::take(&mut self.events);
        let mods = std::mem::take(&mut self.mods);
        let note_mods = std::mem::take(&mut self.note_mods);
        let ctx = PluginProcessContext {
            transport: &cx.data.transport,
            param_events: &events,
            harmony: &cx.data.timeline.harmony,
            param_mods: &mods,
            note_mods: &note_mods,
        };
        self.processor
            .set_callback_deadline(cx.data.callback_deadline);
        // Played again after blocks that went by it, it hears first what
        // ended and what is held now; then what blocks it missed would have
        // ended (in this graph or another).
        let catch_up = takes && self.notes.feeds();
        let held = takes && (rewired || catch_up || self.missed.pending());
        if held && let Some(merged) = self.merged.first_mut() {
            merged.clear();
            if rewired {
                self.notes.start_over(merged);
            } else if catch_up {
                self.notes.catch_up(io.events_in.first(), merged);
            }
            self.missed.take_into(merged);
            if let Some(input) = io.events_in.first() {
                merged.merge_from(input, 0);
            }
        }
        let status = if held {
            let mut with = NodeIo {
                frames: io.frames,
                audio_in: io.audio_in,
                audio_out: &mut *io.audio_out,
                events_in: &self.merged,
                events_out: &mut *io.events_out,
            };
            self.processor.process(&ctx, &mut with)
        } else {
            self.processor.process(&ctx, io)
        };
        let given = if held {
            self.merged.first()
        } else {
            io.events_in.first()
        };
        if let Some(events) = given.filter(|_| takes) {
            if status == ProcessStatus::Missed {
                // Its events never reached it: what they end waits for its
                // next block.
                self.missed.keep(events);
            } else {
                self.notes.follow_given(events);
            }
        }
        let underruns = self.processor.take_underruns();
        if underruns != 0 {
            cx.data
                .worker_underruns
                .fetch_add(underruns, Ordering::Relaxed);
        }
        self.events = events;
        self.mods = mods;
        self.note_mods = note_mods;
        if status == ProcessStatus::Error {
            // Reported to the control side through the shared flag.
            self.failed.store(true, Ordering::Relaxed);
            for out in io.audio_out.iter_mut() {
                out.clear();
            }
            self.notes_out(io, false, rewired);
            self.notes_in(io);
            return;
        }
        let automated_bypass = cx
            .data
            .timeline
            .automation(self.track)
            .is_some_and(|a| a.bypass(self.plugin).is_some());
        let passed = (automated_bypass || self.dry > 0.0) && self.soft_bypass(cx, io);
        self.notes_out(io, passed, rewired);
        self.notes_in(io);
    }

    fn reset(&mut self) {
        self.processor.reset();
        self.notes.forget();
        self.sent.clear();
        self.voices.reset();
        for d in &mut self.delay {
            d.fill(0.0);
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn missed_blocks_keep_what_ends_notes_and_nothing_else() {
        let missed = MissedReleases::default();
        let mut block = MidiBuffer::with_capacity(16);
        let cc = |channel, controller, value| MidiEvent::ControlChange {
            channel,
            controller,
            value,
        };
        for e in [
            MidiEvent::NoteOn {
                channel: 3,
                key: 70,
                velocity: 90,
            },
            MidiEvent::NoteOff {
                channel: 0,
                key: 60,
                velocity: 30,
            },
            MidiEvent::NoteOn {
                channel: 15,
                key: 127,
                velocity: 0,
            },
            // Pedals down and the mod wheel stay missed.
            cc(1, 64, 127),
            cc(1, 1, 0),
            cc(2, 66, 0),
            cc(9, 123, 0),
        ] {
            block.push(TimedMidiEvent::new(5, e)).unwrap();
        }
        assert!(!missed.pending());
        missed.keep(&block);
        assert!(missed.pending());
        let mut next = MidiBuffer::with_capacity(16);
        missed.take_into(&mut next);
        assert!(!missed.pending());
        let got: Vec<(u32, MidiEvent)> = next.iter().map(|e| (e.sample_offset, e.event)).collect();
        let off = |channel, key| MidiEvent::NoteOff {
            channel,
            key,
            velocity: 0,
        };
        assert_eq!(
            got,
            [
                (0, off(0, 60)),
                (0, off(15, 127)),
                (0, cc(2, 66, 0)),
                (0, cc(9, 123, 0)),
            ]
        );
        // What does not fit waits for the next block.
        missed.keep(&block);
        let mut small = MidiBuffer::with_capacity(1);
        missed.take_into(&mut small);
        assert_eq!(small.len(), 1);
        assert!(missed.pending());
        next.clear();
        missed.take_into(&mut next);
        assert_eq!(next.len(), 3);
    }
}
