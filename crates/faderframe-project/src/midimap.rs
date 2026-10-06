//! Controller mappings ("MIDI learn"): a knob, fader, wheel or pad of a MIDI
//! controller drives a parameter or a transport function.

use faderframe_automation::AutomationTarget;
use faderframe_core::{MidiMappingId, SceneId, TrackId};
use serde::{Deserialize, Serialize};

/// Which control of a MIDI device.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "type")]
pub enum MidiControl {
    /// Control change (absolute 0–127).
    Cc {
        number: u8,
    },
    /// Pitch bend (14 bit).
    PitchBend,
    ChannelPressure,
    /// A key or pad: toggles a switch or triggers a transport function.
    Note {
        key: u8,
    },
}

impl MidiControl {
    pub fn label(&self) -> String {
        match self {
            MidiControl::Cc { number } => format!("CC {number}"),
            MidiControl::PitchBend => "Pitch Bend".into(),
            MidiControl::ChannelPressure => "Aftertouch".into(),
            MidiControl::Note { key } => format!("Note {key}"),
        }
    }
}

/// Where mapped events come from.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct MidiSource {
    /// Port key (see `InputRouting::Midi`); `None` = any port.
    #[serde(default)]
    pub port: Option<String>,
    /// MIDI channel 0–15.
    pub channel: u8,
    pub control: MidiControl,
}

impl MidiSource {
    pub fn matches(&self, port: Option<&str>, channel: u8, control: MidiControl) -> bool {
        self.channel == channel
            && self.control == control
            && self.port.as_deref().is_none_or(|p| Some(p) == port)
    }

    pub fn label(&self) -> String {
        let port = self
            .port
            .as_deref()
            .map_or("any input", crate::midi_port_display);
        format!(
            "{} · Ch {} · {port}",
            self.control.label(),
            self.channel + 1
        )
    }
}

/// Transport functions a controller can trigger.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TransportControl {
    PlayStop,
    Stop,
    Record,
    Loop,
    ToStart,
}

impl TransportControl {
    pub const ALL: [TransportControl; 5] = [
        TransportControl::PlayStop,
        TransportControl::Stop,
        TransportControl::Record,
        TransportControl::Loop,
        TransportControl::ToStart,
    ];

    pub fn label(&self) -> &'static str {
        match self {
            TransportControl::PlayStop => "Play / Stop",
            TransportControl::Stop => "Stop",
            TransportControl::Record => "Record",
            TransportControl::Loop => "Loop",
            TransportControl::ToStart => "Go to Start",
        }
    }
}

/// What a mapping drives.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "type")]
pub enum MappingTarget {
    /// Any automatable parameter of a track.
    Parameter {
        track: TrackId,
        target: AutomationTarget,
    },
    Transport {
        control: TransportControl,
    },
    /// A launcher slot's button: a press launches it (as its launch mode
    /// says), letting go releases it.
    LauncherSlot {
        track: TrackId,
        scene: SceneId,
    },
    /// A scene's launch button.
    LauncherScene {
        scene: SceneId,
    },
    /// Stop a track's launched clip (`None`: every track's).
    LauncherStop {
        #[serde(default)]
        track: Option<TrackId>,
    },
}

impl MappingTarget {
    /// A button (pads and keys fit it), not a value.
    pub fn is_button(&self) -> bool {
        !matches!(self, MappingTarget::Parameter { .. })
    }
}

/// How a control's values drive the target.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MappingMode {
    /// The parameter jumps to the control's position.
    #[default]
    Absolute,
    /// Soft takeover: the parameter follows only once the control has
    /// reached its current value (no jumps after the mouse moved it).
    Pickup,
    /// Endless encoder, 1–63 up / 65–127 down (two's complement, 127 = −1).
    RelativeTwosComplement,
    /// Endless encoder around 64 (65 = +1, 63 = −1).
    RelativeBinaryOffset,
    /// Endless encoder, bit 6 = down (1 = +1, 65 = −1).
    RelativeSignMagnitude,
}

impl MappingMode {
    pub const ALL: [MappingMode; 5] = [
        MappingMode::Absolute,
        MappingMode::Pickup,
        MappingMode::RelativeTwosComplement,
        MappingMode::RelativeBinaryOffset,
        MappingMode::RelativeSignMagnitude,
    ];

    pub fn label(self) -> &'static str {
        match self {
            MappingMode::Absolute => "Absolute",
            MappingMode::Pickup => "Soft Takeover",
            MappingMode::RelativeTwosComplement => "Relative (2's complement)",
            MappingMode::RelativeBinaryOffset => "Relative (offset 64)",
            MappingMode::RelativeSignMagnitude => "Relative (sign bit)",
        }
    }

    pub fn is_relative(self) -> bool {
        matches!(
            self,
            MappingMode::RelativeTwosComplement
                | MappingMode::RelativeBinaryOffset
                | MappingMode::RelativeSignMagnitude
        )
    }

    /// Encoder ticks of a relative CC value (0 for absolute modes).
    pub fn ticks(self, value: u8) -> i32 {
        let v = (value & 127) as i32;
        match self {
            MappingMode::RelativeTwosComplement => {
                if v < 64 {
                    v
                } else {
                    v - 128
                }
            }
            MappingMode::RelativeBinaryOffset => v - 64,
            MappingMode::RelativeSignMagnitude => {
                if v & 64 != 0 {
                    -(v & 63)
                } else {
                    v & 63
                }
            }
            _ => 0,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MidiMapping {
    pub id: MidiMappingId,
    pub source: MidiSource,
    pub target: MappingTarget,
    #[serde(default)]
    pub mode: MappingMode,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sources_match_port_channel_and_control() {
        let any = MidiSource {
            port: None,
            channel: 0,
            control: MidiControl::Cc { number: 7 },
        };
        let keys = MidiSource {
            port: Some("MPK:MPK MIDI 1".into()),
            ..any.clone()
        };
        let cc7 = MidiControl::Cc { number: 7 };
        assert!(any.matches(Some("Other:Port"), 0, cc7));
        assert!(any.matches(None, 0, cc7));
        assert!(!any.matches(None, 1, cc7));
        assert!(!any.matches(None, 0, MidiControl::Cc { number: 8 }));
        assert!(keys.matches(Some("MPK:MPK MIDI 1"), 0, cc7));
        assert!(!keys.matches(Some("Other:Port"), 0, cc7));
        assert_eq!(keys.label(), "CC 7 · Ch 1 · MPK MIDI 1");
    }

    #[test]
    fn mappings_round_trip_through_json() {
        let m = MidiMapping {
            id: MidiMappingId(4),
            source: MidiSource {
                port: None,
                channel: 9,
                control: MidiControl::Note { key: 36 },
            },
            target: MappingTarget::Transport {
                control: TransportControl::PlayStop,
            },
            mode: MappingMode::Absolute,
        };
        let json = serde_json::to_string(&m).unwrap();
        assert_eq!(serde_json::from_str::<MidiMapping>(&json).unwrap(), m);
    }

    #[test]
    fn relative_encodings() {
        use MappingMode::*;
        assert_eq!(RelativeTwosComplement.ticks(1), 1);
        assert_eq!(RelativeTwosComplement.ticks(127), -1);
        assert_eq!(RelativeTwosComplement.ticks(120), -8);
        assert_eq!(RelativeBinaryOffset.ticks(65), 1);
        assert_eq!(RelativeBinaryOffset.ticks(60), -4);
        assert_eq!(RelativeSignMagnitude.ticks(3), 3);
        assert_eq!(RelativeSignMagnitude.ticks(67), -3);
        assert_eq!(Absolute.ticks(100), 0);
    }
}
