//! Singing into MIDI live: voice ports.
//!
//! Every input channel of the audio device has a MIDI input port, "Voice ·
//! In n". A track that takes MIDI from one (its input routing names the
//! port) turns that input into notes: the engine copies the channel into a
//! ring ([`faderframe_engine::voice_tap`]), a thread runs the live pitch
//! tracker ([`faderframe_analysis::voice`]) on it and sends what it hears
//! into the MIDI input queue through the port, each message stamped with
//! when it was sung. From there it is MIDI like any keyboard's: it plays the
//! live instrument tracks, records (placed where it was sung), is captured,
//! reaches external synths. Glides go out as note expression (exact, for
//! hosted instruments), as pitch bend (±2 semitones), or not at all; notes
//! can snap to the project's key. One input listens at a time (the first
//! track's, in track order).

use crate::Session;
use faderframe_analysis::voice::{VoiceConfig, VoiceEvent, VoiceTracker};
use faderframe_midi::NoteExpressionKind;
use faderframe_midi_io::VirtualMidiInput;
use faderframe_project::InputRouting;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

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
}

impl Default for VoiceSettings {
    fn default() -> Self {
        Self {
            threshold_db: -45.0,
            glide: VoiceGlide::Off,
            in_key: false,
        }
    }
}

/// What the listening thread is told.
#[derive(Clone, Copy, Debug)]
struct Hearing {
    settings: VoiceSettings,
    scale: u16,
}

struct Listener {
    channel: u16,
    stop: Arc<AtomicBool>,
    tell: Sender<Hearing>,
    handle: Option<JoinHandle<()>>,
}

impl Drop for Listener {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

#[derive(Default)]
pub(crate) struct VoiceState {
    /// The ports made so far, by input channel (0-based).
    ports: Vec<VirtualMidiInput>,
    listener: Option<Listener>,
    pub(crate) settings: VoiceSettings,
    told: Option<(VoiceSettings, u16)>,
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
        self.voice.listener.as_ref().map(|l| l.channel)
    }

    /// Make the device's voice ports, start or stop listening as tracks
    /// ask, tell the listener what changed (from the MIDI tick).
    pub(crate) fn tick_voice(&mut self) {
        let inputs = self.engine.stream_inputs().min(64) as u16;
        let changed = (self.voice.ports.len() as u16) < inputs;
        while (self.voice.ports.len() as u16) < inputs {
            let n = self.voice.ports.len() as u16;
            self.voice.ports.push(
                self.midi
                    .hub
                    .virtual_input(&format!("{VOICE_PORT}{}", n + 1)),
            );
        }
        if changed {
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
            });
        if self.voice.listener.as_ref().map(|l| l.channel) != wanted {
            self.stop_voice();
            if let Some(c) = wanted {
                self.start_voice(c);
            }
        }
        let scale = if self.voice.settings.in_key {
            self.project.key_at(self.playhead()).map_or(0xFFF, |k| {
                (0..12u8)
                    .filter(|pc| k.contains(i32::from(*pc)))
                    .fold(0u16, |m, pc| m | 1 << pc)
            })
        } else {
            0xFFF
        };
        let now = (self.voice.settings, scale);
        if self.voice.told != Some(now)
            && let Some(l) = &self.voice.listener
        {
            let _ = l.tell.send(Hearing {
                settings: now.0,
                scale: now.1,
            });
            self.voice.told = Some(now);
        }
    }

    fn start_voice(&mut self, channel: u16) {
        let Some(port) = self.voice.ports.get(usize::from(channel)).cloned() else {
            return;
        };
        let shared = self.engine.voice_tap();
        shared.voice.set_channel(Some(channel));
        let stop = Arc::new(AtomicBool::new(false));
        let (tell, heard) = channel_of();
        let hearing = Hearing {
            settings: self.voice.settings,
            scale: 0xFFF,
        };
        let thread_stop = Arc::clone(&stop);
        let handle = std::thread::Builder::new()
            .name("faderframe-voice".into())
            .spawn(move || listen(shared, port, hearing, heard, thread_stop));
        match handle {
            Ok(handle) => {
                self.voice.told = None;
                self.voice.listener = Some(Listener {
                    channel,
                    stop,
                    tell,
                    handle: Some(handle),
                });
            }
            Err(e) => tracing::warn!("voice to MIDI: {e}"),
        }
    }

    pub(crate) fn stop_voice(&mut self) {
        if let Some(l) = self.voice.listener.take() {
            drop(l);
            self.engine.voice_tap().voice.set_channel(None);
        }
    }
}

fn channel_of() -> (Sender<Hearing>, Receiver<Hearing>) {
    channel()
}

fn config_of(h: Hearing) -> VoiceConfig {
    VoiceConfig {
        threshold_db: h.settings.threshold_db,
        scale: h.scale,
        ..VoiceConfig::default()
    }
}

/// The listening thread: the ring → the tracker → the port.
fn listen(
    shared: Arc<faderframe_engine::EngineShared>,
    port: VirtualMidiInput,
    mut hearing: Hearing,
    heard: Receiver<Hearing>,
    stop: Arc<AtomicBool>,
) {
    let tap = &shared.voice;
    let mut rate = tap.rate();
    let mut tracker = VoiceTracker::new(f64::from(rate), config_of(hearing));
    let mut from = tap.ring.written();
    let (mut left, mut right) = (Vec::with_capacity(8192), Vec::with_capacity(8192));
    let sounding = Mutex::new(None::<u8>);
    let send = |time: u64, e: VoiceEvent, glide: VoiceGlide| match e {
        VoiceEvent::NoteOn { key, velocity } => {
            if glide == VoiceGlide::PitchBend {
                port.send_at(time, &[0xE0, 0x00, 0x40]);
            }
            port.send_at(time, &[0x90, key & 0x7F, velocity.clamp(1, 127)]);
            if let Ok(mut s) = sounding.lock() {
                *s = Some(key);
            }
        }
        VoiceEvent::NoteOff { key } => {
            port.send_at(time, &[0x80, key & 0x7F, 0]);
            if let Ok(mut s) = sounding.lock() {
                *s = None;
            }
        }
        VoiceEvent::Bend { semitones } => match glide {
            VoiceGlide::Off => {}
            VoiceGlide::Expression => {
                if let Some(key) = sounding.lock().ok().and_then(|s| *s) {
                    port.send_expression_at(
                        time,
                        0,
                        key,
                        NoteExpressionKind::Tuning,
                        f64::from(semitones),
                    );
                }
            }
            VoiceGlide::PitchBend => {
                let v = (8192.0 + f64::from(semitones) / 2.0 * 8192.0).clamp(0.0, 16383.0) as u16;
                port.send_at(time, &[0xE0, (v & 0x7F) as u8, (v >> 7) as u8]);
            }
        },
    };
    while !stop.load(Ordering::Relaxed) {
        while let Ok(h) = heard.try_recv() {
            hearing = h;
            tracker.set_config(config_of(h));
        }
        if tap.rate() != rate {
            rate = tap.rate();
            if let Some(e) = tracker.reset() {
                send(port.clock().now_ns(), e, hearing.settings.glide);
            }
            tracker = VoiceTracker::new(f64::from(rate), config_of(hearing));
        }
        left.clear();
        right.clear();
        let (end, _lost) = tap.ring.read_since(from, &mut left, &mut right);
        let base = end - left.len() as u64;
        from = end;
        let glide = hearing.settings.glide;
        tracker.process(&left, |at, e| {
            let frame = (base as i64 + at as i64).max(0) as u64;
            send(tap.time_of(frame), e, glide);
        });
        std::thread::sleep(Duration::from_millis(2));
    }
    if let Some(e) = tracker.reset() {
        send(port.clock().now_ns(), e, hearing.settings.glide);
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
