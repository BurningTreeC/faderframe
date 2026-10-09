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

use crate::Session;
use faderframe_analysis::voice::{Responsiveness, VoiceConfig};
use faderframe_engine::voice::{Glide, VoiceRun};
use faderframe_midi::MidiInputEvent;
use faderframe_midi_io::VirtualMidiInput;
use faderframe_project::InputRouting;

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
    /// Quieter than this (dBFS) is silence.
    pub threshold_db: f32,
    pub glide: VoiceGlide,
    /// Snap notes to the project's key at the playhead.
    pub in_key: bool,
    pub speed: VoiceSpeed,
}

impl Default for VoiceSettings {
    fn default() -> Self {
        Self {
            polyphonic: false,
            threshold_db: -45.0,
            glide: VoiceGlide::Off,
            in_key: false,
            speed: VoiceSpeed::Balanced,
        }
    }
}

impl VoiceSettings {
    fn config(self, scale: u16) -> VoiceConfig {
        VoiceConfig {
            threshold_db: self.threshold_db,
            scale,
            ..VoiceConfig::default().with(self.speed.responsiveness())
        }
    }
}

/// One input listened to for one port.
struct Listener {
    /// What it hears and sends: input channel (0-based), engine port, MIDI
    /// channel (0-based).
    hears: Hears,
    /// The engine's copies of what it heard.
    feed: rtrb::Consumer<MidiInputEvent>,
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
            if let Err(e) = self.engine.add_voice(run.on_channel(h.channel)) {
                tracing::warn!("voice to MIDI: {e}");
                return;
            }
            self.voice.listeners.push(Listener { hears: h, feed });
            // The new one hears as told now; the others may still be on
            // what they were told.
            if self.voice.told.is_none() {
                self.voice.told = Some((s, scale));
            }
        }
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
        wanted.sort_unstable();
        wanted.dedup();
        wanted
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
