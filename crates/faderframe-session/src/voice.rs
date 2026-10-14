//! Singing into MIDI live: voice ports.
//!
//! Every input channel of the audio device has a MIDI input port, "Voice ·
//! In n", and one port takes them all, "Voice · All Inputs" (input n on
//! MIDI channel n + 1, as an MPE lower zone has its members: one singer,
//! string or player per channel, up to 15). A track that takes MIDI from
//! one (its input routing names the port) turns those inputs into notes:
//! the engine runs a live pitch tracker ([`faderframe_analysis::voice`] or
//! [`faderframe_analysis::polyvoice`]) on each input inside its callback,
//! every input any track takes at once. The notes join that callback's
//! live MIDI on the port ([`faderframe_engine::voice`]) — played at once,
//! recorded where they were sung. A copy of each comes back here for
//! Capture MIDI and the activity lights. From there it is MIDI like any
//! keyboard's: it plays the live instrument tracks, records, reaches
//! external synths. Glides go out as note expression (per key, for hosted
//! instruments), as pitch bend (±2 semitones on the input's channel,
//! monophonic only), or not at all; notes can snap to the project's key.
//!
//! The trackers follow the input's own noise floor and the performer's
//! loud notes, so any gain works without settings. A calibration
//! ([`Session::start_voice_calibration`]) listens to the room alone for a
//! few seconds, then to the performer, and sets the sensitivity between
//! the room (and any hum) and the soft notes, and the dynamics velocities
//! span.

use crate::Session;
pub use faderframe_analysis::voice::{Advice, Calibration, CalibrationError};
use faderframe_analysis::voice::{
    DEFAULT_THRESHOLD_DB, Dynamics, Levels, Meter, Responsiveness, VoiceConfig, calibrate,
};
use faderframe_engine::voice::{Glide, VoiceRun};
use faderframe_midi::MidiInputEvent;
use faderframe_midi_io::VirtualMidiInput;
use faderframe_project::InputRouting;
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Port names: this and the 1-based input channel.
pub const VOICE_PORT: &str = "Voice · In ";
/// The port of every input, each on a channel of its own.
pub const VOICE_ALL_PORT: &str = "Voice · All Inputs";
/// Inputs the all-inputs port carries (MIDI channels 2–16).
pub const ALL_INPUTS: u16 = 15;

/// How sung pitch moves between notes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VoiceGlide {
    /// Steady notes.
    #[default]
    Off,
    /// The note's own tuning (CLAP, VST3 and the built-in instruments).
    Expression,
    /// Channel pitch bend, ±2 semitones (any instrument, external synths).
    PitchBend,
}

impl VoiceGlide {
    pub const ALL: [VoiceGlide; 3] = [
        VoiceGlide::Off,
        VoiceGlide::Expression,
        VoiceGlide::PitchBend,
    ];

    pub fn label(self) -> &'static str {
        match self {
            VoiceGlide::Off => "Steady notes",
            VoiceGlide::Expression => "Glide (note expression)",
            VoiceGlide::PitchBend => "Glide (pitch bend ±2, monophonic)",
        }
    }

    fn engine(self) -> Glide {
        match self {
            VoiceGlide::Off => Glide::Off,
            VoiceGlide::Expression => Glide::Expression,
            VoiceGlide::PitchBend => Glide::PitchBend,
        }
    }
}

/// How quickly notes follow the voice (see [`Responsiveness`]).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VoiceSpeed {
    /// A note sounds a couple of periods after it is sung (for playing).
    Fast,
    #[default]
    Balanced,
    /// Notes hold a little before they start or change (clean takes).
    Stable,
}

impl VoiceSpeed {
    pub const ALL: [VoiceSpeed; 3] = [VoiceSpeed::Fast, VoiceSpeed::Balanced, VoiceSpeed::Stable];

    pub fn label(self) -> &'static str {
        match self {
            VoiceSpeed::Fast => "Fast (for playing)",
            VoiceSpeed::Balanced => "Balanced",
            VoiceSpeed::Stable => "Stable (for clean takes)",
        }
    }

    fn responsiveness(self) -> Responsiveness {
        match self {
            VoiceSpeed::Fast => Responsiveness::Fast,
            VoiceSpeed::Balanced => Responsiveness::Balanced,
            VoiceSpeed::Stable => Responsiveness::Stable,
        }
    }
}

/// How the voice ports hear (Preferences → MIDI).
#[derive(Clone, Copy, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct VoiceSettings {
    /// Listen for chords rather than one melodic line.
    pub polyphonic: bool,
    /// Quieter than this (dBFS) is silence (the trackers' own gate over
    /// the room's noise may be higher).
    #[serde(deserialize_with = "threshold")]
    pub threshold_db: f32,
    pub glide: VoiceGlide,
    /// Snap notes to the project's key at the playhead.
    pub in_key: bool,
    pub speed: VoiceSpeed,
    /// The performer's soft and loud levels a calibration measured
    /// (velocities span them); none: velocities follow the loud notes.
    pub dynamics: Option<VoiceDynamics>,
}

/// A performer's soft and loud levels (dBFS of the analysis frames).
#[derive(Clone, Copy, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct VoiceDynamics {
    pub soft_db: f32,
    pub loud_db: f32,
}

/// The sensitivity before the trackers followed the room themselves.
const OLD_DEFAULT_THRESHOLD_DB: f32 = -45.0;

/// Preferences saved with the old default sensitivity take the new one
/// (the old one was set before the trackers kept over the room by
/// themselves, and missed quiet inputs).
fn threshold<'de, D: serde::Deserializer<'de>>(d: D) -> Result<f32, D::Error> {
    let db = <f32 as serde::Deserialize>::deserialize(d)?;
    Ok(if db == OLD_DEFAULT_THRESHOLD_DB {
        DEFAULT_THRESHOLD_DB
    } else {
        db
    })
}

impl Default for VoiceSettings {
    fn default() -> Self {
        Self {
            polyphonic: false,
            threshold_db: DEFAULT_THRESHOLD_DB,
            glide: VoiceGlide::Off,
            in_key: false,
            speed: VoiceSpeed::Balanced,
            dynamics: None,
        }
    }
}

impl VoiceSettings {
    fn config(self, scale: u16) -> VoiceConfig {
        VoiceConfig {
            threshold_db: self.threshold_db,
            dynamics: self.dynamics.map(|d| Dynamics {
                soft_db: d.soft_db,
                loud_db: d.loud_db,
            }),
            scale,
            ..VoiceConfig::default().with(self.speed.responsiveness())
        }
    }
}

/// How long a calibration listens to the room alone, then to the
/// performance.
pub const CALIBRATION_ROOM: Duration = Duration::from_secs(3);
pub const CALIBRATION_PLAY: Duration = Duration::from_secs(8);

/// Where a calibration is (see [`Session::voice_calibration`]).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum VoiceCalibration {
    /// Waiting for the input to be listened to (the audio device starts).
    Starting,
    /// Listening to the room alone (stay quiet): seconds left and the
    /// input's level now (dBFS).
    Room { left: f32, level_db: f32 },
    /// Listening to the performance (sing or play, soft and loud).
    Playing { left: f32, level_db: f32 },
    /// What it found and set, or why it could not.
    Done(Result<Calibration, CalibrationError>),
}

/// A calibration under way.
struct Calibrating {
    input: u16,
    room_for: Duration,
    play_for: Duration,
    /// The listener's meter once there, and when the phase began.
    meter: Option<Arc<Meter>>,
    since: Instant,
    /// The room's levels once heard.
    room: Option<Levels>,
}

/// One input listened to for one port.
struct Listener {
    /// What it hears and sends: input channel (0-based), engine port, MIDI
    /// channel (0-based).
    hears: Hears,
    /// The engine's copies of what it heard.
    feed: rtrb::Consumer<MidiInputEvent>,
    /// What it hears, counted by level.
    meter: Arc<Meter>,
}

/// An input channel, the port its notes come from and their MIDI channel.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct Hears {
    input: u16,
    port: u16,
    channel: u8,
}

#[derive(Default)]
pub(crate) struct VoiceState {
    /// The ports made so far, by input channel (0-based), and the port of
    /// every input.
    ports: Vec<VirtualMidiInput>,
    all: Option<VirtualMidiInput>,
    /// The inputs listened to, and the device rate and mode their
    /// listeners were made for.
    listeners: Vec<Listener>,
    made_for: Option<(u32, bool)>,
    /// Keep replaced listeners until their final releases have arrived.
    retiring: Vec<rtrb::Consumer<MidiInputEvent>>,
    pub(crate) settings: VoiceSettings,
    /// What the engine was last told: settings and scale.
    told: Option<(VoiceSettings, u16)>,
    /// A calibration under way, and the last one's outcome.
    calibrating: Option<Calibrating>,
    calibrated: Option<Result<Calibration, CalibrationError>>,
}

impl VoiceState {
    /// What the engine heard since the last call (for the MIDI tick).
    pub(crate) fn take_feed(&mut self) -> Vec<MidiInputEvent> {
        let mut out = Vec::new();
        self.retiring.retain_mut(|rx| {
            // Observe abandonment before draining: a producer can publish
            // its last note-off immediately before it is dropped.
            let finished = rx.is_abandoned();
            while let Ok(e) = rx.pop() {
                out.push(e);
            }
            !finished
        });
        for l in &mut self.listeners {
            while let Ok(e) = l.feed.pop() {
                out.push(e);
            }
        }
        out
    }

    /// Stop keeping a listener: its feed drains its last note-offs.
    fn retire(&mut self, l: Listener) {
        self.retiring.push(l.feed);
    }
}

/// The input channel (0-based) a port key listens to.
pub fn voice_channel(key: &str) -> Option<u16> {
    let n: u16 = key
        .strip_prefix("virtual:")?
        .strip_prefix(VOICE_PORT)?
        .parse()
        .ok()?;
    n.checked_sub(1)
}

/// The key of input channel `channel`'s (0-based) voice port.
pub fn voice_port_key(channel: u16) -> String {
    format!("virtual:{VOICE_PORT}{}", channel + 1)
}

/// The key of the port of every input.
pub fn voice_all_port_key() -> String {
    format!("virtual:{VOICE_ALL_PORT}")
}

impl Session {
    pub fn voice_settings(&self) -> VoiceSettings {
        self.voice.settings
    }

    pub fn set_voice_settings(&mut self, settings: VoiceSettings) {
        self.voice.settings = settings;
        self.tick_voice();
    }

    /// The input channels listened to now (0-based, in order).
    pub fn voice_listening(&self) -> Vec<u16> {
        let mut inputs: Vec<u16> = self.voice.listeners.iter().map(|l| l.hears.input).collect();
        inputs.sort_unstable();
        inputs.dedup();
        inputs
    }

    /// The key scale notes snap to (all twelve when not in key).
    fn voice_scale(&self) -> u16 {
        if !self.voice.settings.in_key {
            return 0xFFF;
        }
        self.project.key_at(self.playhead()).map_or(0xFFF, |k| {
            (0..12u8)
                .filter(|pc| k.contains(i32::from(*pc)))
                .fold(0u16, |m, pc| m | 1 << pc)
        })
    }

    /// Make the device's voice ports, start or stop listening as tracks
    /// ask, tell the engine what changed (from the session tick).
    pub(crate) fn tick_voice(&mut self) {
        let inputs = self.engine.stream_inputs().min(64) as u16;
        let rate = self.engine.stream_rate();
        let mut made = (self.voice.ports.len() as u16) < inputs;
        while (self.voice.ports.len() as u16) < inputs {
            let n = self.voice.ports.len() as u16;
            self.voice.ports.push(
                self.midi
                    .hub
                    .virtual_input(&format!("{VOICE_PORT}{}", n + 1)),
            );
        }
        if inputs > 0 && self.voice.all.is_none() {
            self.voice.all = Some(self.midi.hub.virtual_input(VOICE_ALL_PORT));
            made = true;
        }
        if made {
            // The engine's port map takes the new ports.
            self.midi_ports_changed();
            self.revision += 1;
        }
        let wanted = if rate > 0 {
            self.voice_wanted(inputs)
        } else {
            Vec::new()
        };
        let mode = (rate, self.voice.settings.polyphonic);
        let scale = self.voice_scale();
        // Another device rate or mode: every listener anew.
        if self.voice.made_for.is_some_and(|m| m != mode) && !self.voice.listeners.is_empty() {
            if let Err(e) = self.engine.set_voice(None) {
                tracing::warn!("voice to MIDI: {e}");
                return;
            }
            for l in std::mem::take(&mut self.voice.listeners) {
                self.voice.retire(l);
            }
        }
        // Inputs no track takes any more.
        let mut i = 0;
        while i < self.voice.listeners.len() {
            let h = self.voice.listeners[i].hears;
            if wanted.contains(&h) {
                i += 1;
                continue;
            }
            if let Err(e) = self.engine.remove_voice(h.input, h.port) {
                tracing::warn!("voice to MIDI: {e}");
                return;
            }
            let l = self.voice.listeners.swap_remove(i);
            self.voice.retire(l);
        }
        // Inputs a track takes now.
        let s = self.voice.settings;
        for h in wanted {
            if self.voice.listeners.iter().any(|l| l.hears == h) {
                continue;
            }
            if self.voice.listeners.len() >= faderframe_engine::voice::MAX_VOICES {
                tracing::warn!("voice to MIDI: more inputs than can be listened to");
                break;
            }
            let make = if s.polyphonic {
                VoiceRun::new_polyphonic
            } else {
                VoiceRun::new
            };
            let (run, feed) = make(h.input, h.port, rate, s.config(scale), s.glide.engine());
            let meter = run.meter();
            if let Err(e) = self.engine.add_voice(run.on_channel(h.channel)) {
                tracing::warn!("voice to MIDI: {e}");
                return;
            }
            self.voice.listeners.push(Listener {
                hears: h,
                feed,
                meter,
            });
            // The new one hears as told now; the others may still be on
            // what they were told.
            if self.voice.told.is_none() {
                self.voice.told = Some((s, scale));
            }
        }
        self.tick_calibration(inputs);
        if self.voice.listeners.is_empty() {
            self.voice.made_for = None;
            self.voice.told = None;
            return;
        }
        self.voice.made_for = Some(mode);
        let now = (self.voice.settings, scale);
        if self.voice.told != Some(now) {
            if let Err(e) = self
                .engine
                .set_voice_config(now.0.config(now.1), now.0.glide.engine())
            {
                tracing::warn!("voice to MIDI: {e}");
            } else {
                self.voice.told = Some(now);
            }
        }
    }

    /// What the tracks ask to hear: every input a track's MIDI input port
    /// names (each in order once).
    fn voice_wanted(&self, inputs: u16) -> Vec<Hears> {
        let all_key = voice_all_port_key();
        let mut wanted = Vec::new();
        for t in self.project.folder_order() {
            let InputRouting::Midi { port: Some(p), .. } = &t.input else {
                continue;
            };
            if *p == all_key {
                let Some(all) = &self.voice.all else { continue };
                for c in 0..inputs.min(ALL_INPUTS) {
                    wanted.push(Hears {
                        input: c,
                        port: all.port(),
                        channel: (c + 1) as u8,
                    });
                }
            } else if let Some(c) = voice_channel(p).filter(|c| *c < inputs)
                && let Some(port) = self.voice.ports.get(usize::from(c))
            {
                wanted.push(Hears {
                    input: c,
                    port: port.port(),
                    channel: 0,
                });
            }
        }
        // A calibration hears its input (on that input's own port) when no
        // track does.
        if let Some(cal) = &self.voice.calibrating
            && self.audio.is_some()
            && cal.input < inputs
            && !wanted.iter().any(|h| h.input == cal.input)
            && let Some(port) = self.voice.ports.get(usize::from(cal.input))
        {
            wanted.push(Hears {
                input: cal.input,
                port: port.port(),
                channel: 0,
            });
        }
        wanted.sort_unstable();
        wanted.dedup();
        wanted
    }

    /// Calibrate Voice to MIDI on input `input` (0-based): listen to the
    /// room alone for [`CALIBRATION_ROOM`], then to the performer singing
    /// or playing (soft and loud) for [`CALIBRATION_PLAY`]; then set the
    /// sensitivity (over the room and any hum, under the soft notes) and
    /// the dynamics velocities span ([`Self::voice_calibration`] follows
    /// it). The input is listened to whether a track takes it or not.
    pub fn start_voice_calibration(&mut self, input: u16) {
        self.start_voice_calibration_for(input, CALIBRATION_ROOM, CALIBRATION_PLAY);
    }

    /// [`Self::start_voice_calibration`] listening as long as given.
    pub fn start_voice_calibration_for(&mut self, input: u16, room: Duration, play: Duration) {
        self.voice.calibrating = Some(Calibrating {
            input,
            room_for: room,
            play_for: play,
            meter: None,
            since: Instant::now(),
            room: None,
        });
        self.voice.calibrated = None;
        self.tick_voice();
    }

    /// Stop calibrating (nothing changes).
    pub fn cancel_voice_calibration(&mut self) {
        if self.voice.calibrating.take().is_some() {
            self.tick_voice();
        }
    }

    /// Back to the default sensitivity and velocities that follow the loud
    /// notes.
    pub fn forget_voice_calibration(&mut self) {
        self.voice.calibrated = None;
        self.set_voice_settings(VoiceSettings {
            threshold_db: DEFAULT_THRESHOLD_DB,
            dynamics: None,
            ..self.voice.settings
        });
    }

    /// The calibration under way or the last one's outcome.
    pub fn voice_calibration(&self) -> Option<VoiceCalibration> {
        let Some(cal) = &self.voice.calibrating else {
            return self.voice.calibrated.map(VoiceCalibration::Done);
        };
        let Some(meter) = &cal.meter else {
            return Some(VoiceCalibration::Starting);
        };
        let level_db = meter.level_db();
        let left = |of: Duration| of.saturating_sub(cal.since.elapsed()).as_secs_f32();
        Some(match cal.room {
            None => VoiceCalibration::Room {
                left: left(cal.room_for),
                level_db,
            },
            Some(_) => VoiceCalibration::Playing {
                left: left(cal.play_for),
                level_db,
            },
        })
    }

    /// Move a calibration on (from the voice tick): find its listener's
    /// meter, count the room, then the performance, then set what it found.
    fn tick_calibration(&mut self, inputs: u16) {
        let Some(cal) = &mut self.voice.calibrating else {
            return;
        };
        let meter = self
            .voice
            .listeners
            .iter()
            .find(|l| l.hears.input == cal.input)
            .map(|l| l.meter.clone());
        let same = match (&cal.meter, &meter) {
            (Some(a), Some(b)) => Arc::ptr_eq(a, b),
            _ => false,
        };
        if !same {
            // Its listener came (or was made anew: another mode or rate):
            // the room again from now.
            if let Some(m) = &meter {
                m.take();
            }
            cal.meter = meter;
            cal.room = None;
            cal.since = Instant::now();
        }
        let Some(m) = cal.meter.clone() else {
            // No input to hear (no device, or not that many inputs).
            if self.audio.is_none() || self.engine.stream_rate() == 0 || cal.input >= inputs {
                self.voice.calibrating = None;
                self.voice.calibrated = Some(Err(CalibrationError::NoInput));
            }
            return;
        };
        match cal.room.take() {
            None if cal.since.elapsed() >= cal.room_for => {
                cal.room = Some(m.take());
                cal.since = Instant::now();
            }
            Some(room) if cal.since.elapsed() >= cal.play_for => {
                let found = calibrate(&room, &m.take());
                self.voice.calibrating = None;
                self.voice.calibrated = Some(found);
                if let Ok(c) = found {
                    self.voice.settings.threshold_db = c.threshold_db.round();
                    self.voice.settings.dynamics = Some(VoiceDynamics {
                        soft_db: c.dynamics.soft_db,
                        loud_db: c.dynamics.loud_db,
                    });
                }
                self.revision += 1;
            }
            room => cal.room = room,
        }
    }

    pub(crate) fn stop_voice(&mut self) {
        if !self.voice.listeners.is_empty() && self.engine.set_voice(None).is_ok() {
            for l in std::mem::take(&mut self.voice.listeners) {
                self.voice.retire(l);
            }
            self.voice.made_for = None;
            self.voice.told = None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn voice_port_keys_name_their_channel() {
        assert_eq!(voice_port_key(0), "virtual:Voice · In 1");
        assert_eq!(voice_channel("virtual:Voice · In 2"), Some(1));
        assert_eq!(voice_channel("virtual:FaderFrame Keyboard"), None);
        assert_eq!(voice_channel("MPK mini 3:MPK mini 3 MIDI 1"), None);
        assert_eq!(voice_all_port_key(), "virtual:Voice · All Inputs");
        assert_eq!(voice_channel(&voice_all_port_key()), None);
    }

    #[test]
    fn old_voice_preferences_keep_monophonic_mode() {
        let old: VoiceSettings = serde_json::from_str(
            r#"{"threshold_db":-36.0,"glide":"expression","in_key":true,"speed":"fast"}"#,
        )
        .expect("old preferences");
        assert!(!old.polyphonic);
        let poly = VoiceSettings {
            polyphonic: true,
            ..old
        };
        let json = serde_json::to_string(&poly).expect("serialize settings");
        assert_eq!(
            serde_json::from_str::<VoiceSettings>(&json).expect("new preferences"),
            poly
        );
        // The old default sensitivity reads as the new one; a calibration's
        // dynamics are kept.
        let old: VoiceSettings =
            serde_json::from_str(r#"{"threshold_db":-45.0}"#).expect("old default");
        assert_eq!(old.threshold_db, DEFAULT_THRESHOLD_DB);
        let calibrated = VoiceSettings {
            threshold_db: -57.0,
            dynamics: Some(VoiceDynamics {
                soft_db: -40.0,
                loud_db: -18.0,
            }),
            ..VoiceSettings::default()
        };
        let json = serde_json::to_string(&calibrated).expect("serialize settings");
        assert_eq!(
            serde_json::from_str::<VoiceSettings>(&json).expect("calibrated preferences"),
            calibrated
        );
    }

    #[test]
    fn retired_listener_delivers_its_final_release() {
        let (mut tx, rx) = rtrb::RingBuffer::new(8);
        let mut state = VoiceState {
            retiring: vec![rx],
            ..VoiceState::default()
        };
        assert!(state.take_feed().is_empty());
        assert_eq!(state.retiring.len(), 1);
        tx.push(MidiInputEvent::new(7, 100, &[0x80, 60, 0]).expect("note off"))
            .expect("room");
        drop(tx);
        assert_eq!(state.take_feed().len(), 1);
        assert!(state.retiring.is_empty());
    }
}
