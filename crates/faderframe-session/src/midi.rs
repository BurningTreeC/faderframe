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

/// Which inputs FaderFrame uses (saved by the shell).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MidiPreferences {
    /// Port keys not to connect.
    pub disabled_inputs: Vec<String>,
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
    pub last: i64,
}

impl MidiTake {
    pub(crate) fn new(rx: rtrb::Consumer<RecordedMidi>, tracks: Vec<TrackId>, shift: i64) -> Self {
        let n = tracks.len();
        Self {
            rx,
            tracks,
            shift,
            open: vec![HashMap::new(); n],
            notes: vec![Vec::new(); n],
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
}

impl MidiState {
    pub(crate) fn new() -> (Self, faderframe_midi::MidiInputQueue) {
        let (sender, queue, feed) = faderframe_midi::midi_input_queue(4096);
        let mut hub = MidiHub::new(sender.clone());
        let keyboard = hub.virtual_input(KEYBOARD_PORT);
        (
            Self {
                hub,
                sender,
                feed,
                keyboard,
                last_scan: None,
                activity: HashMap::new(),
                learn: None,
                gesture: None,
                cc_last: HashMap::new(),
                live: HashSet::new(),
            },
            queue,
        )
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

    /// Connect the system's MIDI inputs (the shell calls this at start-up).
    pub fn start_midi(&mut self, prefs: &MidiPreferences) {
        self.midi
            .hub
            .set_disabled(prefs.disabled_inputs.iter().cloned());
        self.midi.hub.start_system();
        if let Some(e) = self.midi.hub.error() {
            let e = e.to_string();
            self.notify(NoticeLevel::Warning, e);
        }
        self.midi.last_scan = Some(Instant::now());
        self.midi_ports_changed();
    }

    pub fn stop_midi(&mut self) {
        self.midi.hub.stop_system();
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
                .filter(|p| !p.enabled)
                .map(|p| p.key)
                .collect(),
        }
    }

    /// Use (or ignore) an input device.
    pub fn set_midi_input_enabled(&mut self, key: &str, enabled: bool) {
        let mut disabled = self.midi_preferences().disabled_inputs;
        disabled.retain(|k| k != key);
        if !enabled {
            disabled.push(key.to_string());
        }
        self.midi.hub.set_disabled(disabled);
        self.midi_ports_changed();
        self.revision += 1;
    }

    /// The virtual keyboard input (computer keyboard, on-screen keys, tests).
    pub fn midi_keyboard(&self) -> &VirtualMidiInput {
        &self.midi.keyboard
    }

    fn midi_ports_changed(&mut self) {
        let map = self.midi.port_map();
        if map == *self.engine.midi_ports() {
            return;
        }
        self.engine.set_midi_ports(map);
        let named = self
            .project
            .tracks
            .iter()
            .any(|t| matches!(t.input, InputRouting::Midi { port: Some(_), .. }));
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
            if self.midi.hub.refresh() {
                self.midi_ports_changed();
                self.revision += 1;
            }
        }
        let live = self.compute_midi_live();
        if live != self.midi.live {
            self.midi.live = live.clone();
            self.engine.set_midi_live(live);
            if let Err(e) = self.engine.update_params(&self.project) {
                self.notify(NoticeLevel::Error, e.to_string());
            }
            self.revision += 1;
        }
        let events = self.midi.feed.drain();
        if !events.is_empty() {
            for ev in &events {
                self.midi.activity.insert(ev.port, now);
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
            let mapped: Vec<MappingTarget> = self
                .project
                .midi_mappings
                .iter()
                .filter(|m| m.source.matches(port_key.as_deref(), channel, control))
                .map(|m| m.target)
                .collect();
            for target in mapped {
                match target {
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
                        } else {
                            values.retain(|(t, a, _)| !(*t == track && *a == target));
                            values.push((track, target, value));
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
            mapping: MidiMapping { id, source, target },
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
            if self.record.loop_mode == crate::LoopRecordMode::LastPass
                && let Some(last) = notes.iter().map(|n| n.pass).max()
            {
                notes.retain(|n| n.pass == last);
            }
            if notes.is_empty() {
                continue;
            }
            notes.sort_by_key(|n| (n.start, n.key));
            let ids: Vec<faderframe_core::NoteId> =
                notes.iter().map(|_| self.project.ids.allocate()).collect();
            let to_musical = |s: i64| self.engine.samples_to_musical(&self.project, s.max(0));
            let meter = &self.project.timeline.meter;
            let first = notes.iter().map(|n| n.start).min().unwrap_or(0);
            let last = notes.iter().map(|n| n.end).max().unwrap_or(first);
            let start = meter.bar_start(meter.bar_at(to_musical(first)));
            let end = meter.bar_start(meter.bar_at(to_musical(last)) + 1);
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
                    }
                })
                .collect();
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
                    }),
                }),
            });
            placed.push(id);
        }
        (commands, placed)
    }
}
