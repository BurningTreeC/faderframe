//! Modulators: sources that move a track's parameters on their own — an
//! LFO, an envelope follower, a step sequence, a random walk, or a macro
//! knob — each routed to any number of targets (a device's or a CLAP
//! plugin's parameter on the track, its volume or pan) with a depth.
//!
//! Modulation never changes a parameter's own value: the engine adds
//! `depth × output` (a share of the target's whole range) to it while
//! playing, so automation, presets and saved state keep the value the
//! user set. Bipolar sources (LFO, steps, random) swing around it,
//! unipolar ones (follower, macro) move it one way.
//!
//! Per-note sources (velocity, key, a note envelope, a note LFO, a note
//! random) have a value for every sounding note: they move the parameters
//! of the devices that play the notes, each voice its own where the plugin
//! takes modulation per note (CLAP's polyphonic modulation), else as the
//! newest note has it.
//!
//! The shape functions here are the ones the engine plays and the editor
//! draws.

use faderframe_core::{ModulatorId, ParameterId, PluginInstanceId, TrackId};
use serde::{Deserialize, Serialize};

/// Most modulators a track can have.
pub const MAX_MODULATORS: usize = 16;
/// Most steps of a step sequence.
pub const MAX_STEPS: usize = 32;

/// How fast a modulator runs.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "type")]
pub enum ModRate {
    /// Cycles (or steps) a second.
    Hz { hz: f32 },
    /// A cycle (or step) every `beats` quarter notes, locked to the song.
    Sync { beats: f64 },
}

impl ModRate {
    /// The tempo-synced divisions offered (name, quarters).
    pub const DIVISIONS: [(&'static str, f64); 15] = [
        ("8 bars", 32.0),
        ("4 bars", 16.0),
        ("2 bars", 8.0),
        ("1 bar", 4.0),
        ("1/2", 2.0),
        ("1/2 T", 4.0 / 3.0),
        ("1/4 D", 1.5),
        ("1/4", 1.0),
        ("1/4 T", 2.0 / 3.0),
        ("1/8 D", 0.75),
        ("1/8", 0.5),
        ("1/8 T", 1.0 / 3.0),
        ("1/16", 0.25),
        ("1/16 T", 1.0 / 6.0),
        ("1/32", 0.125),
    ];

    /// Cycles a second at `bpm`.
    pub fn hz(self, bpm: f64) -> f64 {
        match self {
            ModRate::Hz { hz } => f64::from(hz).max(0.0),
            ModRate::Sync { beats } => bpm.max(1.0) / 60.0 / beats.max(1e-3),
        }
    }

    pub fn label(self) -> String {
        match self {
            ModRate::Hz { hz } if hz >= 10.0 => format!("{hz:.0} Hz"),
            ModRate::Hz { hz } => format!("{hz:.2} Hz"),
            ModRate::Sync { beats } => Self::DIVISIONS
                .iter()
                .find(|(_, b)| (b - beats).abs() < 1e-6)
                .map_or_else(|| format!("{beats} beats"), |(n, _)| (*n).to_string()),
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LfoShape {
    #[default]
    Sine,
    Triangle,
    /// Rising.
    Saw,
    /// Falling.
    Ramp,
    Square,
}

impl LfoShape {
    pub const ALL: [LfoShape; 5] = [
        LfoShape::Sine,
        LfoShape::Triangle,
        LfoShape::Saw,
        LfoShape::Ramp,
        LfoShape::Square,
    ];

    pub fn label(self) -> &'static str {
        match self {
            LfoShape::Sine => "Sine",
            LfoShape::Triangle => "Triangle",
            LfoShape::Saw => "Saw Up",
            LfoShape::Ramp => "Saw Down",
            LfoShape::Square => "Square",
        }
    }

    /// The shape at `phase` (0..1): −1..1.
    pub fn at(self, phase: f64) -> f32 {
        let p = phase.rem_euclid(1.0);
        (match self {
            LfoShape::Sine => (std::f64::consts::TAU * p).sin(),
            LfoShape::Triangle => 1.0 - 4.0 * ((p + 0.25).rem_euclid(1.0) - 0.5).abs(),
            LfoShape::Saw => 2.0 * p - 1.0,
            LfoShape::Ramp => 1.0 - 2.0 * p,
            LfoShape::Square => {
                if p < 0.5 {
                    1.0
                } else {
                    -1.0
                }
            }
        }) as f32
    }
}

/// What an envelope follower listens to.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "type")]
pub enum FollowSource {
    /// The track's own signal, before its devices.
    #[default]
    Input,
    /// Another track's signal (after its devices, before its fader).
    Track { track: TrackId },
}

/// What a modulator is.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "type")]
pub enum ModSource {
    /// A repeating shape (−1..1).
    Lfo {
        shape: LfoShape,
        rate: ModRate,
        /// Where in the cycle it starts (0..1).
        #[serde(default)]
        phase: f32,
    },
    /// How loud a signal is (0..1, from −60 dBFS to 0 dBFS after the gain).
    Follower {
        #[serde(default)]
        source: FollowSource,
        attack_ms: f32,
        release_ms: f32,
        #[serde(default)]
        gain_db: f32,
    },
    /// A sequence of values (−1..1), one a step.
    Steps {
        steps: Vec<f32>,
        rate: ModRate,
        /// How far each step slides into the next (0: none, 1: all the
        /// way).
        #[serde(default)]
        glide: f32,
    },
    /// A new random value (−1..1) at the rate, smoothed.
    Random {
        rate: ModRate,
        #[serde(default)]
        smooth: f32,
    },
    /// A knob (0..1) that moves many parameters at once.
    Macro { value: f32 },
    /// Per note: how hard it was played (0..1).
    Velocity,
    /// Per note: where it is on the keyboard (−1 at C−1, 0 at C4, +1 at
    /// C9).
    Key,
    /// Per note: an envelope from the note's start (0..1).
    NoteEnvelope {
        attack_ms: f32,
        decay_ms: f32,
        sustain: f32,
        release_ms: f32,
    },
    /// Per note: an LFO that starts with the note (−1..1).
    NoteLfo {
        shape: LfoShape,
        rate: ModRate,
        #[serde(default)]
        phase: f32,
    },
    /// Per note: a random value of its own (−1..1).
    NoteRandom,
}

impl ModSource {
    pub fn kind_label(&self) -> &'static str {
        match self {
            ModSource::Lfo { .. } => "LFO",
            ModSource::Follower { .. } => "Envelope Follower",
            ModSource::Steps { .. } => "Steps",
            ModSource::Random { .. } => "Random",
            ModSource::Macro { .. } => "Macro",
            ModSource::Velocity => "Velocity",
            ModSource::Key => "Key",
            ModSource::NoteEnvelope { .. } => "Note Envelope",
            ModSource::NoteLfo { .. } => "Note LFO",
            ModSource::NoteRandom => "Note Random",
        }
    }

    /// Swings both ways (−1..1) rather than one (0..1).
    pub fn bipolar(&self) -> bool {
        matches!(
            self,
            ModSource::Lfo { .. }
                | ModSource::Steps { .. }
                | ModSource::Random { .. }
                | ModSource::Key
                | ModSource::NoteLfo { .. }
                | ModSource::NoteRandom
        )
    }

    /// Has a value per sounding note.
    pub fn per_note(&self) -> bool {
        matches!(
            self,
            ModSource::Velocity
                | ModSource::Key
                | ModSource::NoteEnvelope { .. }
                | ModSource::NoteLfo { .. }
                | ModSource::NoteRandom
        )
    }

    /// New modulators of each kind, as they start (the per-note ones
    /// last).
    pub fn defaults() -> Vec<ModSource> {
        vec![
            ModSource::Lfo {
                shape: LfoShape::Sine,
                rate: ModRate::Sync { beats: 1.0 },
                phase: 0.0,
            },
            ModSource::Follower {
                source: FollowSource::Input,
                attack_ms: 10.0,
                release_ms: 150.0,
                gain_db: 0.0,
            },
            ModSource::Steps {
                steps: vec![1.0, 0.0, -0.5, 0.5, -1.0, 0.25, 0.75, -0.25],
                rate: ModRate::Sync { beats: 0.25 },
                glide: 0.0,
            },
            ModSource::Random {
                rate: ModRate::Sync { beats: 0.5 },
                smooth: 0.3,
            },
            ModSource::Macro { value: 0.0 },
            ModSource::Velocity,
            ModSource::Key,
            ModSource::NoteEnvelope {
                attack_ms: 5.0,
                decay_ms: 300.0,
                sustain: 0.4,
                release_ms: 300.0,
            },
            ModSource::NoteLfo {
                shape: LfoShape::Sine,
                rate: ModRate::Hz { hz: 5.0 },
                phase: 0.0,
            },
            ModSource::NoteRandom,
        ]
    }
}

/// A note envelope's level `t_ms` after the note started, released (if it
/// was) `held_ms` after its start and `released_ms` ago.
pub fn note_envelope(
    (attack_ms, decay_ms, sustain, release_ms): (f32, f32, f32, f32),
    t_ms: f32,
    released: Option<(f32, f32)>,
) -> f32 {
    let s = sustain.clamp(0.0, 1.0);
    let held = |t: f32| {
        if t < attack_ms {
            t / attack_ms.max(1e-3)
        } else if t < attack_ms + decay_ms {
            1.0 - (1.0 - s) * (t - attack_ms) / decay_ms.max(1e-3)
        } else {
            s
        }
    };
    match released {
        None => held(t_ms),
        Some((held_ms, since)) => held(held_ms) * (1.0 - since / release_ms.max(1e-3)).max(0.0),
    }
}

/// The key source's value for `key`.
pub fn key_value(key: u8) -> f32 {
    ((f32::from(key) - 60.0) / 60.0).clamp(-1.0, 1.0)
}

/// The step sequence at `position` (in steps, wrapping), with glide.
pub fn steps_at(steps: &[f32], position: f64, glide: f32) -> f32 {
    let n = steps.len();
    if n == 0 {
        return 0.0;
    }
    let p = position.rem_euclid(n as f64);
    let i = p.floor() as usize % n;
    let frac = (p - p.floor()) as f32;
    let here = steps[i];
    let g = glide.clamp(0.0, 1.0);
    if g <= 0.0 {
        return here;
    }
    // Slide over the last `glide` of the step into the next one.
    let start = 1.0 - g;
    if frac <= start {
        here
    } else {
        let t = (frac - start) / g;
        here + (steps[(i + 1) % n] - here) * t
    }
}

/// What a route moves.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "type")]
pub enum ModTarget {
    /// The track's fader (in fader travel).
    Volume,
    Pan,
    /// A parameter of a device or plugin on the track.
    Plugin {
        plugin: PluginInstanceId,
        parameter: ParameterId,
    },
}

/// A modulator's effect on one target.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct ModRoute {
    pub target: ModTarget,
    /// Share of the target's whole range at full output (−1..1).
    pub depth: f32,
}

/// One modulator of a track.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Modulator {
    pub id: ModulatorId,
    pub name: String,
    pub source: ModSource,
    #[serde(default)]
    pub routes: Vec<ModRoute>,
    #[serde(default = "on")]
    pub enabled: bool,
}

fn on() -> bool {
    true
}

impl Modulator {
    pub fn new(id: ModulatorId, source: ModSource) -> Self {
        Self {
            id,
            name: source.kind_label().to_string(),
            source,
            routes: Vec::new(),
            enabled: true,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shapes_swing_between_minus_one_and_one() {
        for s in LfoShape::ALL {
            for i in 0..100 {
                let v = s.at(i as f64 / 100.0);
                assert!((-1.0..=1.0).contains(&v), "{s:?} {v}");
            }
        }
        assert!((LfoShape::Sine.at(0.25) - 1.0).abs() < 1e-6);
        assert!((LfoShape::Triangle.at(0.25) - 1.0).abs() < 1e-6);
        assert!((LfoShape::Triangle.at(0.75) + 1.0).abs() < 1e-6);
        assert!((LfoShape::Triangle.at(0.0)).abs() < 1e-6);
        assert_eq!(LfoShape::Saw.at(0.0), -1.0);
        assert_eq!(LfoShape::Square.at(0.75), -1.0);
    }

    #[test]
    fn rates_follow_the_tempo() {
        assert_eq!(ModRate::Sync { beats: 1.0 }.hz(120.0), 2.0);
        assert_eq!(ModRate::Hz { hz: 3.0 }.hz(90.0), 3.0);
        assert_eq!(ModRate::Sync { beats: 0.5 }.label(), "1/8");
    }

    #[test]
    fn note_envelopes_attack_decay_hold_and_release() {
        let adsr = (10.0, 100.0, 0.5, 200.0);
        assert_eq!(note_envelope(adsr, 5.0, None), 0.5);
        assert_eq!(note_envelope(adsr, 10.0, None), 1.0);
        assert!((note_envelope(adsr, 60.0, None) - 0.75).abs() < 1e-6);
        assert_eq!(note_envelope(adsr, 1_000.0, None), 0.5);
        // Released at the sustain: down to nothing over the release.
        assert_eq!(note_envelope(adsr, 1_100.0, Some((1_000.0, 100.0))), 0.25);
        assert_eq!(note_envelope(adsr, 1_300.0, Some((1_000.0, 300.0))), 0.0);
        // Released in the attack: from where it was.
        assert_eq!(note_envelope(adsr, 5.0, Some((5.0, 0.0))), 0.5);
        assert_eq!(key_value(60), 0.0);
        assert_eq!(key_value(0), -1.0);
        assert!(ModSource::Velocity.per_note() && !ModSource::Macro { value: 0.0 }.per_note());
    }

    #[test]
    fn steps_glide_into_the_next() {
        let s = [0.0, 1.0];
        assert_eq!(steps_at(&s, 0.2, 0.0), 0.0);
        assert_eq!(steps_at(&s, 1.2, 0.0), 1.0);
        assert_eq!(steps_at(&s, 2.2, 0.0), 0.0, "wraps");
        // Half glide: the second half of step 0 slides to 1.
        assert_eq!(steps_at(&s, 0.25, 0.5), 0.0);
        assert!((steps_at(&s, 0.75, 0.5) - 0.5).abs() < 1e-6);
    }
}
