//! Singing into MIDI live: voice ports.
//!
//! Every input channel of the audio device has a MIDI input port, "Voice ·
//! In n". A track that takes MIDI from one (its input routing names the
//! port) turns that input into notes: the engine runs the live pitch tracker
//! ([`faderframe_analysis::voice`]) on the channel inside its callback and
//! the notes join that callback's live MIDI on the port
//! ([`faderframe_engine::voice`]) — played at once, recorded where they
//! were sung. A copy of each comes back here for Capture MIDI and the
//! activity lights. From there it is MIDI like any keyboard's: it plays the
//! live instrument tracks, records, reaches external synths. Glides go out
//! as note expression (exact, for hosted instruments), as pitch bend (±2
//! semitones), or not at all; notes can snap to the project's key. One
//! input listens at a time (the first track's, in track order).

use crate::Session;
use faderframe_analysis::voice::{Responsiveness, VoiceConfig};
use faderframe_engine::voice::{Glide, VoiceRun};
use faderframe_midi::MidiInputEvent;
use faderframe_midi_io::VirtualMidiInput;
use faderframe_project::InputRouting;

/// Port names: this and the 1-based input channel.
pub const VOICE_PORT: &str = "Voice · In ";

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
            VoiceGlide::PitchBend => "Glide (pitch bend ±2)",
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

#[derive(Default)]
pub(crate) struct VoiceState {
    /// The ports made so far, by input channel (0-based).
    ports: Vec<VirtualMidiInput>,
    /// The input listened to and the device rate it was set up for.
    listening: Option<(u16, u32)>,
    /// The engine's copies of what it heard.
    feed: Option<rtrb::Consumer<MidiInputEvent>>,
    pub(crate) settings: VoiceSettings,
    /// What the engine was last told: settings and scale.
    told: Option<(VoiceSettings, u16)>,
}

impl VoiceState {
    /// What the engine heard since the last call (for the MIDI tick).
    pub(crate) fn take_feed(&mut self) -> Vec<MidiInputEvent> {
        let mut out = Vec::new();
        if let Some(rx) = self.feed.as_mut() {
            while let Ok(e) = rx.pop() {
                out.push(e);
            }
        }
        out
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

impl Session {
    pub fn voice_settings(&self) -> VoiceSettings {
        self.voice.settings
    }

    pub fn set_voice_settings(&mut self, settings: VoiceSettings) {
        self.voice.settings = settings;
        self.tick_voice();
    }

    /// The input channel a voice port listens to now.
    pub fn voice_listening(&self) -> Option<u16> {
        self.voice.listening.map(|l| l.0)
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
        let made = (self.voice.ports.len() as u16) < inputs;
        while (self.voice.ports.len() as u16) < inputs {
            let n = self.voice.ports.len() as u16;
            self.voice.ports.push(
                self.midi
                    .hub
                    .virtual_input(&format!("{VOICE_PORT}{}", n + 1)),
            );
        }
        if made {
            // The engine's port map takes the new ports.
            self.midi_ports_changed();
            self.revision += 1;
        }
        // The first track (in order) taking a voice port.
        let wanted = self
            .project
            .folder_order()
            .into_iter()
            .find_map(|t| match &t.input {
                InputRouting::Midi { port: Some(p), .. } => {
                    voice_channel(p).filter(|c| *c < inputs)
                }
                _ => None,
            })
            .filter(|_| rate > 0)
            .map(|c| (c, rate));
        let scale = self.voice_scale();
        if self.voice.listening != wanted {
            self.voice.listening = None;
            self.voice.feed = None;
            self.voice.told = None;
            let run = wanted.and_then(|(c, rate)| {
                let port = self.voice.ports.get(usize::from(c))?.port();
                let s = self.voice.settings;
                let (run, rx) = VoiceRun::new(c, port, rate, s.config(scale), s.glide.engine());
                self.voice.feed = Some(rx);
                self.voice.told = Some((s, scale));
                Some(run)
            });
            let on = run.is_some();
            if let Err(e) = self.engine.set_voice(run) {
                tracing::warn!("voice to MIDI: {e}");
                return;
            }
            if on {
                self.voice.listening = wanted;
            }
        }
        let now = (self.voice.settings, scale);
        if self.voice.listening.is_some() && self.voice.told != Some(now) {
            if let Err(e) = self
                .engine
                .set_voice_config(now.0.config(now.1), now.0.glide.engine())
            {
                tracing::warn!("voice to MIDI: {e}");
            }
            self.voice.told = Some(now);
        }
    }

    pub(crate) fn stop_voice(&mut self) {
        if self.voice.listening.take().is_some() {
            let _ = self.engine.set_voice(None);
            self.voice.feed = None;
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
    }
}
