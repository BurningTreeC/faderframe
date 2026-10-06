//! MIDI keyboards and controllers: input devices, live play, controller
//! mappings ("MIDI learn") and MIDI recording.
//!
//! * Devices: a [`MidiHub`] connects the system's MIDI inputs once the shell
//!   calls [`Session::start_midi`] (tests never touch real devices); a
//!   virtual input — the computer/on-screen keyboard — always exists. Every
//!   message goes to the engine (live play, recording) and, as a copy, to
//!   the session (learn, mappings, activity), which also works without an
//!   audio stream.
//! * Live play: instrument and MIDI tracks with a MIDI input take it while
//!   monitoring is `Input`, or in `Auto` while armed — or selected when no
//!   track is armed. The session decides; the engine reads a flag per track.
//! * Mappings: a mapped control drives a parameter through the same
//!   [`Command`]s as the mouse, inside a gesture (one undo step per twist,
//!   Touch/Latch automation writing works), or triggers a transport
//!   function.

use crate::{Action, InputChoice, NoticeLevel, Session, TransportAction};
use faderframe_automation::AutomationTarget;
use faderframe_core::TrackId;
use faderframe_engine::midi::{MidiFilter, MidiRecordTarget, NO_PORT, RecordedMidi};
use faderframe_midi::{MidiControlFeed, MidiEvent, MidiInputEvent, MidiInputSender};
use faderframe_midi_io::{MidiHub, VirtualMidiInput};
use faderframe_project::{
    Command, InputRouting, MappingTarget, MidiControl, MidiMapping, MidiSource, MonitorMode,
    TrackKind, TransportControl,
};
use faderframe_timeline::MusicalTime;
use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

/// Port name of the built-in virtual keyboard input.
pub const KEYBOARD_PORT: &str = "FaderFrame Keyboard";
/// How often devices are rescanned (hotplug).
const RESCAN: Duration = Duration::from_secs(2);
/// A controller gesture ends after this long without movement.
const GESTURE_QUIET: Duration = Duration::from_millis(400);
/// MIDI learn gives up after this long.
const LEARN_TIMEOUT: Duration = Duration::from_secs(30);
/// A port shows as active this long after a message.
const ACTIVITY_HOLD: Duration = Duration::from_millis(180);

/// Which MIDI devices FaderFrame uses (saved by the shell).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MidiPreferences {
    /// Input port keys not to connect.
    pub disabled_inputs: Vec<String>,
    /// Output port keys not to connect.
    pub disabled_outputs: Vec<String>,
    /// Output port keys that get MIDI clock.
    pub clock_outputs: Vec<String>,
}

/// A MIDI output as shown in preferences and menus.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MidiOutputStatus {
    pub key: String,
    pub name: String,
    pub enabled: bool,
    pub connected: bool,
    pub is_virtual: bool,
    /// Sends MIDI clock (24 ppqn, start/stop/song position).
    pub clock: bool,
}

/// Step input: notes played on a MIDI keyboard are entered at the cursor
/// of the clip open in the piano roll (a chord when keys overlap), then the
/// cursor moves on by one step.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StepInput {
    pub clip: faderframe_core::ClipId,
    /// Clip-relative.
    pub cursor: faderframe_timeline::MusicalTime,
    pub step: faderframe_timeline::MusicalTime,
}

/// A MIDI input as shown in preferences and menus.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MidiPortStatus {
    pub key: String,
    pub name: String,
    pub enabled: bool,
    pub connected: bool,
    pub is_virtual: bool,
    /// A message arrived just now.
    pub active: bool,
}

/// A note being recorded (for drawing it while recording).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LiveNote {
    /// Timeline samples (latency-compensated, where it will land).
    pub start: i64,
    /// `None` while the key is held.
    pub end: Option<i64>,
    pub key: u8,
    pub velocity: u8,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct RecNote {
    pub start: i64,
    pub end: i64,
    pub key: u8,
    pub velocity: u8,
    pub channel: u8,
    pub pass: u32,
}

/// MPE recording: the member-channel pitch bend, pressure and CC 74 that
/// arrive while a note sounds (and just before it, for its initial values)
/// become that note's expression; such controller moves are removed from
/// `ccs` (member-channel expression outside notes is dropped). `ids` are
/// the notes' ids, `rate` the sample rate, `to_note_time` converts a
/// (position, note start) pair to the time from the note's start.
pub(crate) fn mpe_expressions(
    notes: &[RecNote],
    ids: &[faderframe_core::NoteId],
    ccs: &mut Vec<RecController>,
    bend_range: u8,
    rate: f64,
    to_note_time: impl Fn(i64, i64) -> faderframe_timeline::MusicalTime,
) -> Vec<faderframe_project::NoteExpression> {
    use faderframe_project::{ExpressionKind, ExpressionPoint, MidiController, NoteExpression};
    let kind = |c: MidiController| match c {
        MidiController::PitchBend => Some(ExpressionKind::Pitch),
        MidiController::ChannelPressure => Some(ExpressionKind::Pressure),
        MidiController::Cc { number: 74 } => Some(ExpressionKind::Timbre),
        _ => None,
    };
    let lead = (rate * 0.02) as i64;
    let mut out: Vec<NoteExpression> = Vec::new();
    let mut taken = vec![false; ccs.len()];
    for (n, &id) in notes.iter().zip(ids) {
        if n.channel == 0 {
            continue;
        }
        let mut e = NoteExpression::new(id);
        for (i, &(pos, c, ch, v, _)) in ccs.iter().enumerate() {
            let Some(k) = kind(c) else { continue };
            if taken[i] || ch != n.channel || pos < n.start - lead || pos > n.end {
                continue;
            }
            taken[i] = true;
            let value = match k {
                ExpressionKind::Pitch => (v as f32 - 8192.0) / 8192.0 * bend_range.max(1) as f32,
                _ => v.min(127) as f32 / 127.0,
            };
            e.curve_mut(k).push(ExpressionPoint {
                time: to_note_time(pos.max(n.start), n.start),
                value,
            });
        }
        for k in ExpressionKind::ALL {
            let c = e.curve_mut(k);
            c.sort_by_key(|p| p.time);
            // Later values at the same time win (initial values arrive in a
            // burst before the note).
            c.reverse();
            c.dedup_by_key(|p| p.time);
            c.reverse();
            // Keep the points where the curve bends.
            let tol = if k == ExpressionKind::Pitch {
                0.01
            } else {
                0.4 / 127.0
            };
            let mut kept: Vec<ExpressionPoint> = Vec::with_capacity(c.len());
            for (j, p) in c.iter().enumerate() {
                let next = c.get(j + 1);
                let redundant = match (kept.last(), next) {
                    (Some(a), Some(b)) => {
                        let span = (b.time - a.time).ticks().max(1) as f32;
                        let f = (p.time - a.time).ticks() as f32 / span;
                        (a.value + (b.value - a.value) * f - p.value).abs() <= tol
                    }
                    _ => false,
                };
                if !redundant {
                    kept.push(*p);
                }
            }
            *c = kept;
        }
        if !e.is_empty() {
            out.push(e);
        }
    }
    let mut i = 0;
    ccs.retain(|&(_, c, ch, _, _)| {
        let keep = !taken[i] && !(ch != 0 && kind(c).is_some());
        i += 1;
        keep
    });
    out
}

/// Held keys of one track: (channel, key) → (start, velocity, pass).
type HeldNotes = HashMap<(u8, u8), (i64, u8, u32)>;

/// The MIDI half of a recording: events from the engine, paired into notes.
pub(crate) struct MidiTake {
    rx: rtrb::Consumer<RecordedMidi>,
    pub tracks: Vec<TrackId>,
    /// Subtracted from engine positions (input scheduling + output latency).
    pub shift: i64,
    open: Vec<HeldNotes>,
    pub notes: Vec<Vec<RecNote>>,
    /// Controller moves per track: (position, controller, channel, value,
    /// pass).
    pub controllers: Vec<Vec<RecController>>,
    /// SysEx per track: (position, message).
    pub sysex: Vec<Vec<(i64, Vec<u8>)>>,
    pub last: i64,
}

/// A recorded controller move: (position, controller, channel, value, pass).
pub(crate) type RecController = (i64, faderframe_project::MidiController, u8, u16, u32);

impl MidiTake {
    pub(crate) fn new(rx: rtrb::Consumer<RecordedMidi>, tracks: Vec<TrackId>, shift: i64) -> Self {
        let n = tracks.len();
        Self {
            rx,
            tracks,
            shift,
            open: vec![HashMap::new(); n],
            notes: vec![Vec::new(); n],
            controllers: vec![Vec::new(); n],
            sysex: vec![Vec::new(); n],
            last: i64::MIN,
        }
    }

    /// Take what the engine recorded so far.
    pub(crate) fn drain(&mut self) {
        while let Ok(r) = self.rx.pop() {
            let t = r.target as usize;
            if t >= self.tracks.len() {
                continue;
            }
            let pos = r.position - self.shift;
            self.last = self.last.max(pos);
            match r.event {
                MidiEvent::NoteOn {
                    channel,
                    key,
                    velocity,
                } if velocity > 0 => {
                    // A retrigger closes the previous note first.
                    if let Some((start, vel, pass)) = self.open[t].remove(&(channel, key)) {
                        self.close(t, start, pos, key, vel, channel, pass);
                    }
                    self.open[t].insert((channel, key), (pos, velocity, r.pass));
                }
                ev @ (MidiEvent::ControlChange { .. }
                | MidiEvent::PitchBend { .. }
                | MidiEvent::ChannelPressure { .. }) => {
                    if let Some((c, ch, v)) = faderframe_project::MidiController::of_event(ev) {
                        self.controllers[t].push((pos, c, ch, v, r.pass));
                    }
                }
                MidiEvent::NoteOn { channel, key, .. }
                | MidiEvent::NoteOff { channel, key, .. } => {
                    if let Some((start, vel, pass)) = self.open[t].remove(&(channel, key)) {
                        // A loop wrap while held ends the note at the wrap.
                        let end = if pass == r.pass {
                            pos
                        } else {
                            self.last.max(start + 1)
                        };
                        self.close(t, start, end, key, vel, channel, pass);
                    }
                }
                _ => {}
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn close(
        &mut self,
        t: usize,
        start: i64,
        end: i64,
        key: u8,
        velocity: u8,
        channel: u8,
        pass: u32,
    ) {
        self.notes[t].push(RecNote {
            start,
            end: end.max(start + 1),
            key,
            velocity,
            channel,
            pass,
        });
    }

    /// End held notes at `at` (recording stopped).
    pub(crate) fn close_all(&mut self, at: i64) {
        for t in 0..self.tracks.len() {
            let open: Vec<_> = self.open[t].drain().collect();
            for ((channel, key), (start, vel, pass)) in open {
                self.close(t, start, at.max(start + 1), key, vel, channel, pass);
            }
        }
    }

    pub(crate) fn live_notes(&self, track: TrackId) -> Vec<LiveNote> {
        let Some(t) = self.tracks.iter().position(|x| *x == track) else {
            return Vec::new();
        };
        self.notes[t]
            .iter()
            .map(|n| LiveNote {
                start: n.start,
                end: Some(n.end),
                key: n.key,
                velocity: n.velocity,
            })
            .chain(
                self.open[t]
                    .iter()
                    .map(|(&(_, key), &(start, velocity, _))| LiveNote {
                        start,
                        end: None,
                        key,
                        velocity,
                    }),
            )
            .collect()
    }
}

pub(crate) struct MidiState {
    pub(crate) hub: MidiHub,
    pub(crate) outputs: faderframe_midi_io::MidiOutputs,
    clock_outputs: HashSet<String>,
    sender: MidiInputSender,
    feed: MidiControlFeed,
    keyboard: VirtualMidiInput,
    last_scan: Option<Instant>,
    activity: HashMap<u16, Instant>,
    learn: Option<(MappingTarget, Instant)>,
    /// Our controller gesture is open; last movement.
    gesture: Option<Instant>,
    /// Last CC value per (port, channel, controller), for button edges.
    cc_last: HashMap<(u16, u8, u8), u8>,
    live: HashSet<TrackId>,
    /// Keys held on the inputs, per channel (for keyboards on screen).
    held: [u128; 16],
    /// The note being auditioned: (track, channel, key).
    audition: Option<(TrackId, u8, u8)>,
    /// Soft takeover: per mapping, whether the control has picked the
    /// parameter up, and the value it last set.
    pickup: HashMap<faderframe_core::MidiMappingId, (bool, f64)>,
    /// Mappings the engine's consumed-controls table was built from
    /// (`None`: not built for this engine yet).
    consumed_for: Option<Vec<MidiMapping>>,
    step: Option<StepInput>,
    /// SysEx playback scheduling.
    pub(crate) sysex: crate::sysex::SysexPlayback,
    /// Keys of the chord being entered by step input (and how many are down).
    step_chord: Vec<(u8, u8, u8)>,
    step_down: usize,
    /// Input and output ports control surfaces have (kept from the hub and
    /// the track outputs, and out of the preferences' disabled lists).
    surface_inputs: Vec<String>,
    surface_outputs: Vec<String>,
}

impl MidiState {
    pub(crate) fn new() -> (Self, faderframe_midi::MidiInputQueue) {
        let (sender, queue, feed) = faderframe_midi::midi_input_queue(4096);
        let mut hub = MidiHub::new(sender.clone());
        let keyboard = hub.virtual_input(KEYBOARD_PORT);
        let outputs = faderframe_midi_io::MidiOutputs::new(sender.clock());
        (
            Self {
                hub,
                outputs,
                clock_outputs: HashSet::new(),
                sender,
                feed,
                keyboard,
                last_scan: None,
                activity: HashMap::new(),
                learn: None,
                gesture: None,
                cc_last: HashMap::new(),
                live: HashSet::new(),
                held: [0; 16],
                audition: None,
                pickup: HashMap::new(),
                consumed_for: None,
                step: None,
                sysex: crate::sysex::SysexPlayback::default(),
                step_chord: Vec::new(),
                step_down: 0,
                surface_inputs: Vec::new(),
                surface_outputs: Vec::new(),
            },
            queue,
        )
    }

    /// An output queue for a new engine.
    pub(crate) fn renew_output_queue(&self) -> faderframe_midi::MidiOutputQueue {
        self.outputs.renew_queue()
    }

    pub(crate) fn output_port_map(&self) -> HashMap<String, u16> {
        self.outputs
            .ports()
            .into_iter()
            .map(|p| (p.key, p.index))
            .collect()
    }

    /// A new engine has empty tables: rebuild them on the next tick.
    pub(crate) fn reset_engine_tables(&mut self) {
        self.consumed_for = None;
    }

    /// Bit mask of output indices that send clock.
    pub(crate) fn clock_mask(&self) -> u64 {
        self.outputs
            .ports()
            .iter()
            .filter(|p| p.index < 64 && self.clock_outputs.contains(&p.key))
            .fold(0, |m, p| m | (1u64 << p.index))
    }

    /// A queue for a new engine (every input keeps sending, into it).
    pub(crate) fn renew_queue(&self) -> faderframe_midi::MidiInputQueue {
        self.sender.renew()
    }

    pub(crate) fn port_map(&self) -> HashMap<String, u16> {
        self.hub
            .ports()
            .into_iter()
            .map(|p| (p.key, p.index))
            .collect()
    }

    pub(crate) fn live(&self) -> &HashSet<TrackId> {
        &self.live
    }
}

/// The part of a control event mappings look at.
fn control_of(ev: MidiEvent) -> Option<(u8, MidiControl, f64)> {
    match ev {
        MidiEvent::ControlChange {
            channel,
            controller,
            value,
        } => Some((
            channel,
            MidiControl::Cc { number: controller },
            value as f64 / 127.0,
        )),
        MidiEvent::PitchBend { channel, value } => {
            Some((channel, MidiControl::PitchBend, value as f64 / 16383.0))
        }
        MidiEvent::ChannelPressure { channel, pressure } => Some((
            channel,
            MidiControl::ChannelPressure,
            pressure as f64 / 127.0,
        )),
        MidiEvent::NoteOn {
            channel,
            key,
            velocity,
        } if velocity > 0 => Some((channel, MidiControl::Note { key }, 1.0)),
        _ => None,
    }
}

impl Session {
    // --- devices ----------------------------------------------------------------------

    /// Connect the system's MIDI devices (the shell calls this at start-up).
    pub fn start_midi(&mut self, prefs: &MidiPreferences) {
        let surfaces = &self.midi.surface_inputs;
        self.midi.hub.set_disabled(
            prefs
                .disabled_inputs
                .iter()
                .chain(surfaces.iter())
                .cloned()
                .collect::<Vec<_>>(),
        );
        self.midi.hub.start_system();
        if let Some(e) = self.midi.hub.error() {
            let e = e.to_string();
            self.notify(NoticeLevel::Warning, e);
        }
        let surfaces = &self.midi.surface_outputs;
        self.midi.outputs.set_disabled(
            prefs
                .disabled_outputs
                .iter()
                .chain(surfaces.iter())
                .cloned()
                .collect::<Vec<_>>(),
        );
        self.midi.outputs.start_system();
        self.midi.clock_outputs = prefs.clock_outputs.iter().cloned().collect();
        self.midi.last_scan = Some(Instant::now());
        self.midi_ports_changed();
    }

    pub fn stop_midi(&mut self) {
        self.midi.hub.stop_system();
        self.midi.outputs.stop_system();
    }

    /// Every input known so far (the virtual keyboard first).
    pub fn midi_ports(&self) -> Vec<MidiPortStatus> {
        let now = Instant::now();
        self.midi
            .hub
            .ports()
            .into_iter()
            .map(|p| MidiPortStatus {
                active: self
                    .midi
                    .activity
                    .get(&p.index)
                    .is_some_and(|t| now.duration_since(*t) < ACTIVITY_HOLD),
                key: p.key,
                name: p.name,
                enabled: p.enabled,
                connected: p.connected,
                is_virtual: p.is_virtual,
            })
            .collect()
    }

    /// Every output known so far.
    pub fn midi_outputs(&self) -> Vec<MidiOutputStatus> {
        self.midi
            .outputs
            .ports()
            .into_iter()
            .map(|p| MidiOutputStatus {
                clock: self.midi.clock_outputs.contains(&p.key),
                key: p.key,
                name: p.name,
                enabled: p.enabled,
                connected: p.connected,
                is_virtual: p.is_virtual,
            })
            .collect()
    }

    /// Keep the ports control surfaces use from the hub and the track
    /// outputs (and give back the ones they no longer use).
    pub(crate) fn apply_surface_ports(&mut self) {
        let prefs = self.midi_preferences();
        let (ins, outs) = self.surface_ports();
        self.midi.surface_inputs = ins;
        self.midi.surface_outputs = outs;
        let mut inputs = prefs.disabled_inputs;
        inputs.extend(self.midi.surface_inputs.iter().cloned());
        self.midi.hub.set_disabled(inputs);
        let mut outputs = prefs.disabled_outputs;
        outputs.extend(self.midi.surface_outputs.iter().cloned());
        self.midi.outputs.set_disabled(outputs);
        self.midi_ports_changed();
    }

    /// Is this MIDI port a control surface's?
    pub fn is_surface_port(&self, key: &str) -> bool {
        self.midi.surface_inputs.iter().any(|k| k == key)
            || self.midi.surface_outputs.iter().any(|k| k == key)
    }

    /// Any input received something just now.
    pub fn midi_active(&self) -> bool {
        let now = Instant::now();
        self.midi
            .activity
            .values()
            .any(|t| now.duration_since(*t) < ACTIVITY_HOLD)
    }

    pub fn midi_preferences(&self) -> MidiPreferences {
        MidiPreferences {
            disabled_inputs: self
                .midi
                .hub
                .ports()
                .into_iter()
                .filter(|p| !p.enabled && !self.midi.surface_inputs.contains(&p.key))
                .map(|p| p.key)
                .collect(),
            disabled_outputs: self
                .midi
                .outputs
                .ports()
                .into_iter()
                .filter(|p| !p.enabled && !self.midi.surface_outputs.contains(&p.key))
                .map(|p| p.key)
                .collect(),
            clock_outputs: {
                let mut v: Vec<String> = self.midi.clock_outputs.iter().cloned().collect();
                v.sort();
                v
            },
        }
    }

    /// Use (or ignore) an input device.
    pub fn set_midi_input_enabled(&mut self, key: &str, enabled: bool) {
        let mut disabled = self.midi_preferences().disabled_inputs;
        disabled.retain(|k| k != key);
        if !enabled {
            disabled.push(key.to_string());
        }
        disabled.extend(self.midi.surface_inputs.iter().cloned());
        self.midi.hub.set_disabled(disabled);
        self.midi_ports_changed();
        self.revision += 1;
    }

    /// Use (or ignore) an output device.
    pub fn set_midi_output_enabled(&mut self, key: &str, enabled: bool) {
        let mut disabled = self.midi_preferences().disabled_outputs;
        disabled.retain(|k| k != key);
        if !enabled {
            disabled.push(key.to_string());
        }
        disabled.extend(self.midi.surface_outputs.iter().cloned());
        self.midi.outputs.set_disabled(disabled);
        self.midi_ports_changed();
        self.revision += 1;
    }

    /// Send MIDI clock to an output (or stop).
    pub fn set_midi_clock_output(&mut self, key: &str, on: bool) {
        if on {
            self.midi.clock_outputs.insert(key.to_string());
        } else {
            self.midi.clock_outputs.remove(key);
        }
        self.engine
            .midi_shared()
            .clock_ports
            .store(self.midi.clock_mask(), std::sync::atomic::Ordering::Relaxed);
        self.revision += 1;
    }

    /// The virtual keyboard input (computer keyboard, on-screen keys, tests).
    pub fn midi_keyboard(&self) -> &VirtualMidiInput {
        &self.midi.keyboard
    }

    /// A capture output (tests, monitoring): every message sent to it.
    pub fn add_virtual_midi_output(&mut self, name: &str) -> faderframe_midi_io::Captured {
        let (_, captured) = self.midi.outputs.virtual_output(name);
        self.midi_ports_changed();
        captured
    }

    /// Port maps and clock outputs to the engine (graph rebuild when a
    /// track uses a named port).
    pub(crate) fn midi_ports_changed(&mut self) {
        self.engine
            .midi_shared()
            .clock_ports
            .store(self.midi.clock_mask(), std::sync::atomic::Ordering::Relaxed);
        let inputs = self.midi.port_map();
        let outputs = self.midi.output_port_map();
        if inputs == *self.engine.midi_ports() && outputs == *self.engine.midi_output_ports() {
            return;
        }
        self.engine.set_midi_ports(inputs);
        self.engine.set_midi_output_ports(outputs);
        let named = self.project.tracks.iter().any(|t| {
            matches!(t.input, InputRouting::Midi { port: Some(_), .. }) || t.midi_output.is_some()
        });
        if named
            && let Err(e) = self.engine.sync(
                &self.project,
                &self.sources,
                faderframe_project::Impact::Graph,
            )
        {
            self.notify(NoticeLevel::Error, e.to_string());
        }
    }

    /// The instrument a MIDI track plays: none, or one of the instrument
    /// tracks (its notes and live input go through the MIDI track's MIDI
    /// effects to that track's instrument).
    pub fn midi_instrument_choices(&self, track: TrackId) -> Vec<InputChoice> {
        use faderframe_project::OutputRouting;
        let Some(t) = self.project.track(track) else {
            return Vec::new();
        };
        if t.kind != TrackKind::Midi {
            return Vec::new();
        }
        let set = |output| Action::Edit(Command::SetTrackOutput { track, output });
        let current = match t.output {
            OutputRouting::Track { track } => Some(track),
            _ => None,
        };
        let mut out = vec![InputChoice {
            label: "None".into(),
            action: set(OutputRouting::None),
            checked: current.is_none(),
            group_start: true,
        }];
        for (i, inst) in self
            .project
            .tracks
            .iter()
            .filter(|d| d.kind == TrackKind::Instrument)
            .enumerate()
        {
            let plays = self
                .instrument_slot(inst)
                .map(|s| format!(" ({})", s.plugin.name.trim_start_matches("FaderFrame ")))
                .unwrap_or_else(|| " (no instrument yet)".into());
            out.push(InputChoice {
                label: format!("{}{plays}", inst.name),
                action: set(OutputRouting::Track { track: inst.id }),
                checked: current == Some(inst.id),
                group_start: i == 0,
            });
        }
        out
    }

    /// MIDI output choices of a MIDI track: none, or a device (and channel).
    pub fn midi_output_choices(&self, track: TrackId) -> Vec<InputChoice> {
        use faderframe_project::MidiOutputRouting;
        let Some(t) = self.project.track(track) else {
            return Vec::new();
        };
        let cur = t.midi_output.clone();
        let set = |output: Option<MidiOutputRouting>| {
            Action::Edit(Command::SetTrackMidiOutput { track, output })
        };
        let mut out = vec![InputChoice {
            label: "No External MIDI Output".into(),
            action: set(None),
            checked: cur.is_none(),
            group_start: true,
        }];
        let mut ports: Vec<(String, String)> = self
            .midi_outputs()
            .into_iter()
            .filter(|p| p.enabled && !p.is_virtual)
            .map(|p| (p.key, p.name))
            .collect();
        if let Some(c) = &cur
            && !ports.iter().any(|(k, _)| *k == c.port)
        {
            ports.push((
                c.port.clone(),
                format!(
                    "{} (not connected)",
                    faderframe_project::midi_port_display(&c.port)
                ),
            ));
        }
        let channel = cur.as_ref().and_then(|c| c.channel);
        for (i, (key, name)) in ports.into_iter().enumerate() {
            out.push(InputChoice {
                label: name,
                action: set(Some(MidiOutputRouting {
                    port: key.clone(),
                    channel,
                })),
                checked: cur.as_ref().is_some_and(|c| c.port == key),
                group_start: i == 0,
            });
        }
        if let Some(c) = cur {
            for ch in std::iter::once(None).chain((0..16u8).map(Some)) {
                out.push(InputChoice {
                    label: ch.map_or_else(
                        || "Channel: as played".into(),
                        |n| format!("Channel {}", n + 1),
                    ),
                    action: set(Some(MidiOutputRouting {
                        port: c.port.clone(),
                        channel: ch,
                    })),
                    checked: c.channel == ch,
                    group_start: ch.is_none(),
                });
            }
        }
        out
    }

    // --- auditioning and the on-screen keyboard -------------------------------------

    /// Play `key` on `track`'s instrument (piano roll keys, drawn notes),
    /// whatever its live state; stops the previous audition note.
    pub fn audition(&mut self, track: TrackId, key: u8, velocity: u8, channel: u8) {
        self.audition_off();
        self.engine
            .midi_shared()
            .audition_track
            .store(track.raw(), std::sync::atomic::Ordering::Relaxed);
        let ch = channel & 15;
        self.midi.sender.send(
            faderframe_engine::midi::AUDITION_PORT,
            &[0x90 | ch, key & 127, velocity.clamp(1, 127)],
        );
        self.midi.audition = Some((track, ch, key));
    }

    pub fn audition_off(&mut self) {
        if let Some((_, ch, key)) = self.midi.audition.take() {
            self.midi
                .sender
                .send(faderframe_engine::midi::AUDITION_PORT, &[0x80 | ch, key, 0]);
        }
    }

    /// Keys held on the MIDI inputs right now (any channel).
    pub fn held_midi_keys(&self) -> u128 {
        self.midi.held.iter().fold(0, |m, c| m | c)
    }

    /// The keys a MIDI or instrument track plays right now (a bit per
    /// key): the notes of its clips under the playhead while the transport
    /// runs, and the keys held on its MIDI input while it plays live.
    pub fn sounding_keys(&self, track: TrackId) -> u128 {
        let Some(t) = self.project.track(track) else {
            return 0;
        };
        let mut keys = 0u128;
        if self.transport.playing && !t.mute {
            let now = self.playhead();
            for c in self.project.clips_of(track) {
                let Some(m) = c.as_midi().filter(|_| !c.muted) else {
                    continue;
                };
                let rel = now - c.start;
                if rel < MusicalTime::ZERO || rel >= m.length {
                    continue;
                }
                for n in m.notes.iter().filter(|n| !n.muted) {
                    if n.start <= rel && rel < n.start + n.length {
                        keys |= 1u128 << (n.key & 127);
                    }
                }
            }
        }
        if self.midi.live.contains(&track) {
            keys |= match t.input {
                InputRouting::Midi {
                    channel: Some(ch), ..
                } => self.midi.held[usize::from(ch & 15)],
                _ => self.held_midi_keys(),
            };
        }
        keys
    }

    // --- step input ------------------------------------------------------------------

    pub fn step_input(&self) -> Option<StepInput> {
        self.midi.step
    }

    /// Turn step input on for the clip in the editor (cursor at `cursor`,
    /// notes `step` long), or off.
    pub fn set_step_input(&mut self, step: Option<StepInput>) {
        self.midi.step = step;
        self.midi.step_chord.clear();
        self.midi.step_down = 0;
        self.revision += 1;
    }

    fn step_input_event(&mut self, ev: MidiEvent) {
        let Some(st) = self.midi.step else { return };
        match ev {
            MidiEvent::NoteOn {
                channel,
                key,
                velocity,
            } if velocity > 0 => {
                self.midi.step_down += 1;
                self.midi.step_chord.push((channel, key, velocity));
            }
            MidiEvent::NoteOn { .. } | MidiEvent::NoteOff { .. } => {
                self.midi.step_down = self.midi.step_down.saturating_sub(1);
                if self.midi.step_down == 0 && !self.midi.step_chord.is_empty() {
                    let chord = std::mem::take(&mut self.midi.step_chord);
                    let fits = self
                        .project
                        .clip(st.clip)
                        .and_then(|c| c.as_midi())
                        .is_some_and(|m| st.cursor < m.length);
                    if !fits {
                        return;
                    }
                    let notes: Vec<faderframe_project::MidiNote> = chord
                        .into_iter()
                        .map(|(channel, key, velocity)| faderframe_project::MidiNote {
                            id: faderframe_core::NoteId(0),
                            start: st.cursor,
                            length: st.step,
                            key,
                            velocity,
                            channel,
                            muted: false,
                        })
                        .collect();
                    if let Err(e) = self.add_notes(st.clip, &notes) {
                        self.notify(NoticeLevel::Warning, format!("step input: {e}"));
                    }
                    self.midi.step = Some(StepInput {
                        cursor: st.cursor + st.step,
                        ..st
                    });
                }
            }
            _ => {}
        }
    }

    /// Engine table of controls taken by mappings (rebuilt when mappings
    /// change).
    fn update_consumed(&mut self) {
        if self.midi.consumed_for.as_ref() == Some(&self.project.midi_mappings) {
            return;
        }
        use faderframe_engine::midi::ConsumedControl as C;
        let list: Vec<(Option<u16>, u8, C)> = self
            .project
            .midi_mappings
            .iter()
            .map(|m| {
                let port = m.source.port.as_ref().map(|k| {
                    self.midi
                        .hub
                        .port_index(k)
                        .unwrap_or(faderframe_engine::midi::NO_PORT)
                });
                let c = match m.source.control {
                    MidiControl::Cc { number } => C::Cc(number),
                    MidiControl::Note { key } => C::Note(key),
                    MidiControl::PitchBend => C::PitchBend,
                    MidiControl::ChannelPressure => C::ChannelPressure,
                };
                (port, m.source.channel, c)
            })
            .collect();
        self.engine.midi_shared().consumed.set(&list);
        self.midi.consumed_for = Some(self.project.midi_mappings.clone());
    }

    // --- live play -------------------------------------------------------------------

    /// Tracks that play what the keyboard plays right now.
    pub fn midi_live_tracks(&self) -> &HashSet<TrackId> {
        self.midi.live()
    }

    fn compute_midi_live(&self) -> HashSet<TrackId> {
        let takes = |t: &&faderframe_project::Track| {
            matches!(t.kind, TrackKind::Instrument | TrackKind::Midi) && t.input.is_midi()
        };
        let any_armed = self
            .project
            .tracks
            .iter()
            .filter(takes)
            .any(|t| t.record_arm);
        self.project
            .tracks
            .iter()
            .filter(takes)
            .filter(|t| match t.monitor {
                MonitorMode::Off => false,
                MonitorMode::Input => true,
                MonitorMode::Auto => {
                    t.record_arm || (!any_armed && self.selection.tracks.contains(&t.id))
                }
            })
            .map(|t| t.id)
            .collect()
    }

    pub(crate) fn tick_midi(&mut self) {
        let now = Instant::now();
        if self.midi.hub.system_started()
            && self
                .midi
                .last_scan
                .is_none_or(|t| now.duration_since(t) >= RESCAN)
        {
            self.midi.last_scan = Some(now);
            let inputs = self.midi.hub.refresh();
            let outputs = self.midi.outputs.refresh();
            if inputs || outputs {
                self.midi_ports_changed();
                self.revision += 1;
            }
        }
        self.update_consumed();
        let live = self.compute_midi_live();
        if live != self.midi.live {
            self.midi.live = live.clone();
            self.engine.set_midi_live(live);
            if let Err(e) = self.engine.update_params(&self.project) {
                self.notify(NoticeLevel::Error, e.to_string());
            }
            self.revision += 1;
        }
        let system = self.midi.feed.drain_system();
        for ev in &system {
            self.midi.activity.insert(ev.port, now);
            // SysEx to the plugins of the tracks playing live from the port.
            if let faderframe_midi::SystemMessage::SysEx(data) = &ev.message
                && !self.midi.live.is_empty()
                && !self.engine.send_live_sysex(ev.port, data)
            {
                tracing::debug!("live SysEx of {} bytes dropped", data.len());
            }
        }
        let clock_now = self.midi.sender.clock().now_ns();
        self.tick_sync(&system, clock_now);
        self.record_sysex(&system);
        self.tick_sysex(clock_now);
        let events: Vec<MidiInputEvent> = self
            .midi
            .feed
            .drain()
            .into_iter()
            .filter(|e| e.port != faderframe_engine::midi::AUDITION_PORT)
            .collect();
        self.feed_capture(&events);
        if !events.is_empty() {
            for ev in &events {
                self.midi.activity.insert(ev.port, now);
                if let Some(e) = ev.event() {
                    let ch = (e.channel() & 15) as usize;
                    match e {
                        MidiEvent::NoteOn { key, velocity, .. } if velocity > 0 => {
                            self.midi.held[ch] |= 1u128 << (key & 127);
                        }
                        MidiEvent::NoteOn { key, .. } | MidiEvent::NoteOff { key, .. } => {
                            self.midi.held[ch] &= !(1u128 << (key & 127));
                        }
                        _ => {}
                    }
                    if self.midi.step.is_some() {
                        self.step_input_event(e);
                    }
                }
            }
            self.handle_midi_controls(&events);
            self.revision += 1;
        }
        if let Some(t) = self.midi.gesture
            && now.duration_since(t) >= GESTURE_QUIET
        {
            self.midi.gesture = None;
            let _ = self.dispatch(Action::EndGesture);
        }
        if let Some((_, t)) = self.midi.learn
            && now.duration_since(t) >= LEARN_TIMEOUT
        {
            self.midi.learn = None;
            self.notify(NoticeLevel::Info, "MIDI learn timed out");
        }
    }

    // --- learn and mappings ----------------------------------------------------------

    /// Wait for the next control of a MIDI device to map it to `target`.
    pub fn start_midi_learn(&mut self, target: MappingTarget) {
        self.midi.learn = Some((target, Instant::now()));
        let what = self.mapping_target_label(&target);
        self.notify(
            NoticeLevel::Info,
            format!("MIDI learn: move a control for {what} (Esc cancels)"),
        );
        self.revision += 1;
    }

    pub fn cancel_midi_learn(&mut self) {
        if self.midi.learn.take().is_some() {
            self.notify(NoticeLevel::Info, "MIDI learn cancelled");
            self.revision += 1;
        }
    }

    pub fn midi_learning(&self) -> Option<MappingTarget> {
        self.midi.learn.map(|(t, _)| t)
    }

    /// Mappings driving `target`.
    pub fn midi_mappings_for(&self, target: MappingTarget) -> Vec<&MidiMapping> {
        self.project
            .midi_mappings
            .iter()
            .filter(|m| m.target == target)
            .collect()
    }

    /// "Drums · Volume", "Transport · Play / Stop".
    pub fn mapping_target_label(&self, target: &MappingTarget) -> String {
        match target {
            MappingTarget::Parameter { track, target } => {
                let t = self.project.track(*track);
                let track_name = t.map_or("(removed track)", |t| t.name.as_str());
                let param = self
                    .automation_param(*track, *target)
                    .map_or_else(|| target.label(), |p| p.name);
                format!("{track_name} · {param}")
            }
            MappingTarget::Transport { control } => format!("Transport · {}", control.label()),
        }
    }

    /// Menu entries for mapping `target`: learn, and remove existing ones.
    pub fn midi_learn_menu(&self, target: MappingTarget) -> Vec<(String, Action)> {
        let mut out = vec![("MIDI Learn…".to_string(), Action::MidiLearn(target))];
        for m in self.midi_mappings_for(target) {
            out.push((
                format!("Remove MIDI Mapping ({})", m.source.label()),
                Action::Edit(Command::RemoveMidiMapping { mapping: m.id }),
            ));
        }
        out
    }

    fn is_toggle_target(&self, target: &MappingTarget) -> bool {
        match target {
            MappingTarget::Transport { .. } => true,
            MappingTarget::Parameter { track, target } => self
                .automation_param(*track, *target)
                .is_some_and(|p| p.kind == crate::ParamKind::Toggle),
        }
    }

    fn handle_midi_controls(&mut self, events: &[MidiInputEvent]) {
        // Latest value per parameter target within this tick; triggers in
        // order.
        let mut values: Vec<(TrackId, AutomationTarget, f64)> = Vec::new();
        let mut toggles: Vec<(TrackId, AutomationTarget)> = Vec::new();
        let mut transport: Vec<TransportControl> = Vec::new();
        for raw in events {
            let Some(ev) = raw.event() else { continue };
            let Some((channel, control, value)) = control_of(ev) else {
                continue;
            };
            let port_key = self.midi.hub.port_key(raw.port).map(str::to_string);
            // Button edge for CCs (pads/switches sending 127/0).
            let pressed = match control {
                MidiControl::Note { .. } => true,
                MidiControl::Cc { number } => {
                    let v = (value * 127.0).round() as u8;
                    let prev = self
                        .midi
                        .cc_last
                        .insert((raw.port, channel, number), v)
                        .unwrap_or(0);
                    prev < 64 && v >= 64
                }
                _ => false,
            };
            if let Some((target, _)) = self.midi.learn {
                let toggle = self.is_toggle_target(&target);
                let fits = match control {
                    MidiControl::Note { .. } => toggle,
                    _ => true,
                };
                if fits {
                    self.midi.learn = None;
                    self.learn_mapping(
                        target,
                        MidiSource {
                            port: port_key,
                            channel,
                            control,
                        },
                    );
                }
                continue;
            }
            let mapped: Vec<MidiMapping> = self
                .project
                .midi_mappings
                .iter()
                .filter(|m| m.source.matches(port_key.as_deref(), channel, control))
                .cloned()
                .collect();
            for m in mapped {
                match m.target {
                    MappingTarget::Transport { control: tc } => {
                        if pressed {
                            transport.push(tc);
                        }
                    }
                    MappingTarget::Parameter { track, target } => {
                        let toggle =
                            self.is_toggle_target(&MappingTarget::Parameter { track, target });
                        if toggle && matches!(control, MidiControl::Note { .. }) {
                            toggles.push((track, target));
                            continue;
                        }
                        // Where the parameter is now (pending moves of this
                        // tick included), as a lane position.
                        let current = values
                            .iter()
                            .find(|(t, a, _)| *t == track && *a == target)
                            .map(|(_, _, v)| *v)
                            .or_else(|| {
                                let p = self.automation_param(track, target)?;
                                Some(p.to_normal(self.display_value(track, target)?))
                            })
                            .unwrap_or(0.0);
                        let next = match (m.mode, control) {
                            (mode, MidiControl::Cc { .. }) if mode.is_relative() => {
                                let ticks = mode.ticks((value * 127.0).round() as u8);
                                // 1/128 of the range per encoder tick.
                                Some((current + ticks as f64 / 128.0).clamp(0.0, 1.0))
                            }
                            (faderframe_project::MappingMode::Pickup, _) => {
                                let (picked, last) = self
                                    .midi
                                    .pickup
                                    .get(&m.id)
                                    .copied()
                                    .unwrap_or((false, -1.0));
                                // Moved elsewhere since we last set it: pick
                                // it up again.
                                let picked = picked && (current - last).abs() < 0.01;
                                let near = (value - current).abs() < 0.03;
                                if picked || near {
                                    self.midi.pickup.insert(m.id, (true, value));
                                    Some(value)
                                } else {
                                    self.midi.pickup.insert(m.id, (false, last));
                                    None
                                }
                            }
                            _ => Some(value),
                        };
                        if let Some(n) = next {
                            values.retain(|(t, a, _)| !(*t == track && *a == target));
                            values.push((track, target, n));
                        }
                    }
                }
            }
        }
        if !values.is_empty() || !toggles.is_empty() {
            self.apply_mapped(values, toggles);
        }
        for tc in transport {
            let action = match tc {
                TransportControl::PlayStop => TransportAction::TogglePlay,
                TransportControl::Stop => TransportAction::Stop,
                TransportControl::Record => TransportAction::ToggleRecord,
                TransportControl::Loop => TransportAction::ToggleLoop,
                TransportControl::ToStart => TransportAction::ReturnToStart,
            };
            if let Err(e) = self.dispatch(Action::Transport(action)) {
                self.notify(NoticeLevel::Error, e.to_string());
            }
        }
    }

    fn learn_mapping(&mut self, target: MappingTarget, source: MidiSource) {
        let mut commands: Vec<Command> = self
            .project
            .midi_mappings
            .iter()
            .filter(|m| m.source == source || m.target == target)
            .map(|m| Command::RemoveMidiMapping { mapping: m.id })
            .collect();
        let id = self.project.ids.allocate();
        let label = format!(
            "MIDI learn: {} → {}",
            source.label(),
            self.mapping_target_label(&target)
        );
        commands.push(Command::AddMidiMapping {
            index: usize::MAX,
            mapping: MidiMapping {
                id,
                source,
                target,
                mode: faderframe_project::MappingMode::Absolute,
            },
        });
        match self.edit(Command::Batch {
            label: "MIDI Learn".into(),
            commands,
        }) {
            Ok(()) => self.notify(NoticeLevel::Info, label),
            Err(e) => self.notify(NoticeLevel::Error, e.to_string()),
        }
    }

    /// The command that sets `target` to the plain value `v`.
    pub fn command_for(&self, track: TrackId, target: AutomationTarget, v: f64) -> Option<Command> {
        Some(match target {
            AutomationTarget::TrackVolume => Command::SetTrackVolume {
                track,
                db: v as f32,
            },
            AutomationTarget::TrackPan => Command::SetTrackPan {
                track,
                pan: v as f32,
            },
            AutomationTarget::TrackMute => Command::SetTrackMute {
                track,
                on: v >= 0.5,
            },
            AutomationTarget::SendLevel(send) => Command::SetSendLevel {
                track,
                send,
                db: v as f32,
            },
            AutomationTarget::PluginParameter { plugin, parameter } => {
                Command::SetPluginParameter {
                    track,
                    plugin,
                    parameter,
                    value: Some(v),
                }
            }
            AutomationTarget::PluginBypass(plugin) => Command::SetPluginBypass {
                track,
                plugin,
                bypass: v >= 0.5,
            },
        })
    }

    fn apply_mapped(
        &mut self,
        values: Vec<(TrackId, AutomationTarget, f64)>,
        toggles: Vec<(TrackId, AutomationTarget)>,
    ) {
        if self.midi.gesture.is_none() && !self.history.in_gesture() {
            let _ = self.dispatch(Action::BeginGesture("MIDI Controller".into()));
            self.midi.gesture = Some(Instant::now());
        } else if self.midi.gesture.is_some() {
            self.midi.gesture = Some(Instant::now());
        }
        let mut commands = Vec::new();
        for (track, target, n) in values {
            if let Some(p) = self.automation_param(track, target)
                && let Some(c) = self.command_for(track, target, p.from_normal(n))
            {
                commands.push(c);
            }
        }
        for (track, target) in toggles {
            let on = self.display_value(track, target).is_some_and(|v| v >= 0.5);
            if let Some(c) = self.command_for(track, target, if on { 0.0 } else { 1.0 }) {
                commands.push(c);
            }
        }
        for c in commands {
            if let Err(e) = self.dispatch(Action::Edit(c)) {
                self.notify(NoticeLevel::Warning, format!("MIDI controller: {e}"));
            }
        }
    }

    // --- inputs ------------------------------------------------------------------------

    /// MIDI input choices of an instrument/MIDI track for menus: no input,
    /// every input or one, every channel or one.
    pub fn midi_input_choices(&self, track: TrackId) -> Vec<InputChoice> {
        let Some(t) = self.project.track(track) else {
            return Vec::new();
        };
        let (cur_port, cur_channel) = match &t.input {
            InputRouting::Midi { port, channel } => (Some(port.clone()), *channel),
            _ => (None, None),
        };
        let set = |input: InputRouting| Action::Edit(Command::SetTrackInput { track, input });
        let mut out = vec![InputChoice {
            label: "No MIDI Input".into(),
            action: set(InputRouting::None),
            checked: t.input == InputRouting::None,
            group_start: true,
        }];
        let mut ports: Vec<(Option<String>, String)> = vec![(None, "All MIDI Inputs".into())];
        ports.extend(
            self.midi_ports()
                .into_iter()
                .filter(|p| p.enabled)
                .map(|p| (Some(p.key), p.name)),
        );
        // A routed port that is absent right now stays visible.
        if let Some(Some(k)) = &cur_port
            && !ports.iter().any(|(p, _)| p.as_ref() == Some(k))
        {
            ports.push((
                Some(k.clone()),
                format!(
                    "{} (not connected)",
                    faderframe_project::midi_port_display(k)
                ),
            ));
        }
        for (i, (port, name)) in ports.into_iter().enumerate() {
            out.push(InputChoice {
                label: name,
                action: set(InputRouting::Midi {
                    port: port.clone(),
                    channel: cur_channel,
                }),
                checked: cur_port.as_ref() == Some(&port),
                group_start: i == 0,
            });
        }
        let port = cur_port.flatten();
        for ch in std::iter::once(None).chain((0..16u8).map(Some)) {
            out.push(InputChoice {
                label: ch.map_or_else(|| "All Channels".into(), |c| format!("Channel {}", c + 1)),
                action: set(InputRouting::Midi {
                    port: port.clone(),
                    channel: ch,
                }),
                checked: t.input.is_midi() && cur_channel == ch,
                group_start: ch.is_none(),
            });
        }
        out
    }

    // --- recording ---------------------------------------------------------------------

    /// Armed instrument/MIDI tracks with a MIDI input.
    pub(crate) fn midi_record_targets(&self) -> Vec<MidiRecordTarget> {
        let ports = self.engine.midi_ports();
        self.project
            .tracks
            .iter()
            .filter(|t| t.record_arm && matches!(t.kind, TrackKind::Instrument | TrackKind::Midi))
            .filter_map(|t| match &t.input {
                InputRouting::Midi { port, channel } => Some(MidiRecordTarget {
                    track: t.id,
                    filter: MidiFilter {
                        port: port
                            .as_ref()
                            .map(|k| ports.get(k).copied().unwrap_or(NO_PORT)),
                        channel: *channel,
                    },
                }),
                _ => None,
            })
            .collect()
    }

    /// Notes recorded on `track` so far (while recording).
    pub fn live_midi_notes(&self, track: TrackId) -> Vec<LiveNote> {
        self.recording
            .as_ref()
            .and_then(|r| r.midi.as_ref())
            .map(|m| m.live_notes(track))
            .unwrap_or_default()
    }
}

impl Session {
    /// Clips for a finished MIDI take (one per track with notes), and the
    /// carving Replace mode does first.
    pub(crate) fn midi_take_commands(
        &mut self,
        take: &MidiTake,
    ) -> (Vec<Command>, Vec<faderframe_core::ClipId>) {
        use faderframe_project::{Clip, ClipContent, MidiClip, MidiNote};
        let mut commands = Vec::new();
        let mut placed = Vec::new();
        for (i, track) in take.tracks.iter().enumerate() {
            let Some(name) = self.project.track(*track).map(|t| t.name.clone()) else {
                continue;
            };
            let mut notes = take.notes[i].clone();
            let mut ccs = take.controllers[i].clone();
            let sysex_rec = take.sysex[i].clone();
            let last_pass = notes
                .iter()
                .map(|n| n.pass)
                .chain(ccs.iter().map(|c| c.4))
                .max();
            if self.record.loop_mode == crate::LoopRecordMode::LastPass
                && let Some(last) = last_pass
            {
                notes.retain(|n| n.pass == last);
                ccs.retain(|c| c.4 == last);
            }
            if notes.is_empty() && ccs.is_empty() && sysex_rec.is_empty() {
                continue;
            }
            notes.sort_by_key(|n| (n.start, n.key));
            let ids: Vec<faderframe_core::NoteId> =
                notes.iter().map(|_| self.project.ids.allocate()).collect();
            let to_musical = |s: i64| self.engine.samples_to_musical(&self.project, s.max(0));
            // MPE: member-channel expression goes with the notes.
            let expressions = match self.project.track(*track).and_then(|t| t.mpe) {
                Some(cfg) => mpe_expressions(
                    &notes,
                    &ids,
                    &mut ccs,
                    cfg.bend_range,
                    self.engine.sample_rate() as f64,
                    |pos, start| to_musical(pos) - to_musical(start),
                ),
                None => Vec::new(),
            };
            let meter = &self.project.timeline.meter;
            let first = notes
                .iter()
                .map(|n| n.start)
                .chain(ccs.iter().map(|c| c.0))
                .chain(sysex_rec.iter().map(|x| x.0))
                .min()
                .unwrap_or(0);
            let last = notes
                .iter()
                .map(|n| n.end)
                .chain(ccs.iter().map(|c| c.0))
                .chain(sysex_rec.iter().map(|x| x.0))
                .max()
                .unwrap_or(first);
            let start = meter.bar_start(meter.bar_at(to_musical(first)));
            let end = meter.bar_start(meter.bar_at(to_musical(last)) + 1);
            let sysex_events: Vec<faderframe_project::SysexEvent> = sysex_rec
                .iter()
                .map(|(pos, data)| faderframe_project::SysexEvent {
                    time: (to_musical(*pos) - start).max(faderframe_timeline::MusicalTime::ZERO),
                    data: data.clone(),
                })
                .collect();
            let midi_notes: Vec<MidiNote> = notes
                .iter()
                .zip(ids)
                .map(|(n, id)| {
                    let s = to_musical(n.start);
                    MidiNote {
                        id,
                        start: s - start,
                        length: (to_musical(n.end) - s).max(faderframe_timeline::MusicalTime(1)),
                        key: n.key,
                        velocity: n.velocity.max(1),
                        channel: n.channel,
                        muted: false,
                    }
                })
                .collect();
            // Controller moves become lanes (one per controller and channel).
            let mut lanes: Vec<faderframe_project::ControllerLane> = Vec::new();
            for &(pos, c, ch, v, _) in &ccs {
                let time = (to_musical(pos) - start).max(faderframe_timeline::MusicalTime::ZERO);
                let i = match lanes
                    .iter()
                    .position(|l| l.controller == c && l.channel == ch)
                {
                    Some(i) => i,
                    None => {
                        lanes.push(faderframe_project::ControllerLane::new(c, ch));
                        lanes.len() - 1
                    }
                };
                lanes[i]
                    .points
                    .push(faderframe_project::ControllerPoint { time, value: v });
            }
            for l in &mut lanes {
                l.normalize();
            }
            lanes.sort_by_key(|l| (l.controller, l.channel));
            if self.record.mode == crate::RecordMode::Replace {
                commands.extend(crate::record::carve_midi(
                    &mut self.project,
                    *track,
                    start,
                    end,
                ));
            }
            let takes = self
                .project
                .clips_of(*track)
                .iter()
                .filter(|c| c.name.starts_with(&format!("{name} Take")))
                .count();
            let id = self.project.ids.allocate();
            commands.push(Command::AddClip {
                clip: Box::new(Clip {
                    id,
                    track: *track,
                    name: format!("{name} Take {}", takes + 1),
                    color: None,
                    start,
                    muted: false,
                    content: ClipContent::Midi(MidiClip {
                        length: end - start,
                        notes: midi_notes,
                        controllers: lanes,
                        expressions,
                        sysex: sysex_events,
                    }),
                }),
            });
            placed.push(id);
        }
        (commands, placed)
    }
}

#[cfg(test)]
mod mpe_tests {
    use super::*;
    use faderframe_project::{ExpressionKind, MidiController};
    use faderframe_timeline::MusicalTime;

    #[test]
    fn member_channel_expression_becomes_note_expression() {
        let rate = 48_000.0;
        let ms = |m: i64| m * 48;
        // Two notes on member channels 2 and 3, a sustain pedal and a mod
        // wheel on the master channel, a stray bend after note 1 ended.
        let notes = vec![
            RecNote {
                start: ms(100),
                end: ms(600),
                key: 60,
                velocity: 90,
                channel: 1,
                pass: 0,
            },
            RecNote {
                start: ms(200),
                end: ms(700),
                key: 64,
                velocity: 90,
                channel: 2,
                pass: 0,
            },
        ];
        let ids = [faderframe_core::NoteId(10), faderframe_core::NoteId(11)];
        let pb = |pos, ch, v| (pos, MidiController::PitchBend, ch, v, 0u32);
        let mut ccs = vec![
            // Initial values just before note 1.
            pb(ms(95), 1, 8192),
            (ms(95), MidiController::ChannelPressure, 1, 0, 0),
            pb(ms(300), 1, 8192 + 4096), // +24 st of ±48
            (ms(400), MidiController::ChannelPressure, 1, 127, 0),
            (ms(250), MidiController::Cc { number: 74 }, 2, 100, 0),
            pb(ms(800), 1, 0),
            (ms(150), MidiController::Cc { number: 64 }, 0, 127, 0),
            (ms(150), MidiController::Cc { number: 1 }, 0, 64, 0),
        ];
        let to_time = |pos: i64, start: i64| MusicalTime((pos - start) * 1000);
        let e = mpe_expressions(&notes, &ids, &mut ccs, 48, rate, to_time);
        assert_eq!(e.len(), 2);
        let first = e.iter().find(|x| x.note == ids[0]).unwrap();
        assert_eq!(first.pitch.first().unwrap().time, MusicalTime::ZERO);
        assert_eq!(first.pitch.last().unwrap().value, 24.0);
        assert_eq!(first.pressure.last().unwrap().value, 1.0);
        let second = e.iter().find(|x| x.note == ids[1]).unwrap();
        assert!(
            (second.value_at(ExpressionKind::Timbre, MusicalTime(ms(100) * 1000)) - 100.0 / 127.0)
                .abs()
                < 1e-6
        );
        // Master-channel controllers stay lanes; member expression is gone.
        assert_eq!(ccs.len(), 2);
        assert!(ccs.iter().all(|c| c.2 == 0));
    }
}
