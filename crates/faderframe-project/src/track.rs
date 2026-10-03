use faderframe_automation::AutomationSet;
use faderframe_core::{ChannelLayout, ClipId, ParameterId, PluginInstanceId, SendId, TrackId};
use serde::{Deserialize, Serialize};
use std::fmt;

/// What kind of channel a track is.
///
/// All kinds share one [`Track`] structure (fader, pan, mute/solo, inserts,
/// sends, routing, automation); kind-specific behaviour is decided by the
/// engine's graph builder.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TrackKind {
    /// Audio clips / hardware input.
    Audio,
    /// MIDI clips driving an instrument plugin; audio output.
    Instrument,
    /// MIDI clips / MIDI input routed to an instrument track or MIDI out.
    Midi,
    /// Subgroup receiving other tracks' outputs.
    Bus,
    /// Effect return receiving sends.
    Aux,
    /// The main output. Exactly one per project.
    Master,
}

impl TrackKind {
    pub fn label(self) -> &'static str {
        match self {
            TrackKind::Audio => "Audio",
            TrackKind::Instrument => "Instrument",
            TrackKind::Midi => "MIDI",
            TrackKind::Bus => "Bus",
            TrackKind::Aux => "Aux",
            TrackKind::Master => "Master",
        }
    }

    /// Holds clips on the timeline.
    pub fn has_clips(self) -> bool {
        matches!(
            self,
            TrackKind::Audio | TrackKind::Instrument | TrackKind::Midi
        )
    }

    /// Can be the destination of other tracks' outputs and sends.
    pub fn is_summing(self) -> bool {
        matches!(self, TrackKind::Bus | TrackKind::Aux | TrackKind::Master)
    }

    /// Produces audio (as opposed to MIDI-only tracks).
    pub fn has_audio(self) -> bool {
        !matches!(self, TrackKind::Midi)
    }
}

/// An sRGB colour, persisted as `"#rrggbb"`.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct TrackColor {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

impl TrackColor {
    pub const fn rgb(r: u8, g: u8, b: u8) -> Self {
        Self { r, g, b }
    }

    /// Muted studio palette used for new tracks.
    pub const PALETTE: [TrackColor; 12] = [
        TrackColor::rgb(0xd9, 0x6c, 0x4f), // brick
        TrackColor::rgb(0xe0, 0xa3, 0x4a), // amber
        TrackColor::rgb(0xc9, 0xc1, 0x5a), // olive gold
        TrackColor::rgb(0x7f, 0xb0, 0x69), // sage
        TrackColor::rgb(0x4f, 0xa8, 0x8f), // teal
        TrackColor::rgb(0x4d, 0x9a, 0xc4), // steel blue
        TrackColor::rgb(0x6a, 0x7f, 0xd1), // indigo
        TrackColor::rgb(0x9b, 0x6f, 0xc9), // violet
        TrackColor::rgb(0xc4, 0x6a, 0xa7), // plum
        TrackColor::rgb(0xb8, 0x8a, 0x6a), // tan
        TrackColor::rgb(0x8c, 0x96, 0xa3), // slate
        TrackColor::rgb(0xd0, 0x7a, 0x7a), // rose
    ];

    pub fn palette(index: usize) -> Self {
        Self::PALETTE[index % Self::PALETTE.len()]
    }

    pub fn to_hex(self) -> String {
        format!("#{:02x}{:02x}{:02x}", self.r, self.g, self.b)
    }

    pub fn from_hex(s: &str) -> Option<Self> {
        let s = s.strip_prefix('#').unwrap_or(s);
        if s.len() != 6 {
            return None;
        }
        let v = u32::from_str_radix(s, 16).ok()?;
        Some(Self::rgb((v >> 16) as u8, (v >> 8) as u8, v as u8))
    }
}

impl fmt::Debug for TrackColor {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_hex())
    }
}

impl Serialize for TrackColor {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.to_hex())
    }
}

impl<'de> Deserialize<'de> for TrackColor {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        TrackColor::from_hex(&s)
            .ok_or_else(|| serde::de::Error::custom(format!("invalid colour {s:?}")))
    }
}

/// Input monitoring behaviour.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MonitorMode {
    /// Never monitor the input.
    #[default]
    Off,
    /// Always monitor the input.
    Input,
    /// Tape style: monitor the input while armed, except during playback
    /// outside of recording.
    Auto,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "type")]
pub enum InputRouting {
    #[default]
    None,
    /// Hardware input channels starting at `first_channel` (count = track layout).
    Hardware { first_channel: u16 },
    /// Live MIDI (instrument and MIDI tracks): one input port by its stable
    /// name, or every port; one channel (0–15) or all.
    Midi {
        #[serde(default)]
        port: Option<String>,
        #[serde(default)]
        channel: Option<u8>,
    },
}

/// An external MIDI device a track plays (MIDI tracks).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MidiOutputRouting {
    /// Output port key (stable name, like input keys).
    pub port: String,
    /// Send on this channel (0–15) instead of the notes' own.
    #[serde(default)]
    pub channel: Option<u8>,
}

/// The shown part of a MIDI port key ("Device:Port" → "Port").
pub fn midi_port_display(key: &str) -> &str {
    match key.split_once(':') {
        Some((_, port)) if !port.is_empty() => port,
        _ => key,
    }
}

impl InputRouting {
    /// Every MIDI input, every channel.
    pub fn all_midi() -> Self {
        InputRouting::Midi {
            port: None,
            channel: None,
        }
    }

    pub fn is_midi(&self) -> bool {
        matches!(self, InputRouting::Midi { .. })
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "type")]
pub enum OutputRouting {
    /// The project's master track.
    #[default]
    Master,
    /// A bus/aux/master track — or, for MIDI tracks, an instrument track.
    Track { track: TrackId },
    /// Hardware outputs starting at `first_channel` (direct out / master out).
    Hardware { first_channel: u16 },
    /// Disconnected.
    None,
}

/// Where along the channel strip a send taps the signal.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SendTap {
    /// Before the insert chain.
    PreFx,
    /// After inserts, before the fader.
    PreFader,
    /// After fader and pan.
    #[default]
    PostFader,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AuxSend {
    pub id: SendId,
    pub target: TrackId,
    pub level_db: f32,
    #[serde(default)]
    pub tap: SendTap,
    #[serde(default = "default_true")]
    pub enabled: bool,
}

fn default_true() -> bool {
    true
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PluginFormat {
    Builtin,
    Clap,
    Vst3,
    AudioUnit,
}

/// Which plugin a slot holds.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct PluginRef {
    pub format: PluginFormat,
    /// Format-specific id (CLAP id, VST3 class id, builtin id).
    pub id: String,
    /// Display name at the time it was inserted.
    pub name: String,
}

impl PluginRef {
    pub fn builtin(id: &str, name: &str) -> Self {
        Self {
            format: PluginFormat::Builtin,
            id: id.into(),
            name: name.into(),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct SavedParameter {
    pub id: ParameterId,
    pub value: f64,
}

/// An insert or instrument slot.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PluginSlot {
    pub id: PluginInstanceId,
    pub plugin: PluginRef,
    #[serde(default)]
    pub bypass: bool,
    /// Parameter values (plain units).
    #[serde(default)]
    pub parameters: Vec<SavedParameter>,
    /// Opaque plugin state (base64 when written by a real plugin format).
    #[serde(default)]
    pub state: Option<String>,
}

/// A channel of the project: audio/instrument/MIDI track, bus, aux or master.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Track {
    pub id: TrackId,
    pub kind: TrackKind,
    pub name: String,
    pub color: TrackColor,
    pub layout: ChannelLayout,

    /// Clips on this track (order not significant).
    #[serde(default)]
    pub clips: Vec<ClipId>,

    #[serde(default)]
    pub instrument: Option<PluginSlot>,
    #[serde(default)]
    pub inserts: Vec<PluginSlot>,
    #[serde(default)]
    pub sends: Vec<AuxSend>,

    #[serde(default)]
    pub input: InputRouting,
    #[serde(default)]
    pub output: OutputRouting,

    /// Static fader level; automation in Read mode overrides it.
    pub volume_db: f32,
    /// Static pan (-1..1); automation in Read mode overrides it.
    pub pan: f32,
    #[serde(default)]
    pub phase_invert: bool,

    #[serde(default)]
    pub mute: bool,
    #[serde(default)]
    pub solo: bool,
    #[serde(default)]
    pub record_arm: bool,
    /// External MIDI device (MIDI tracks): what plays goes out there too.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub midi_output: Option<MidiOutputRouting>,
    /// The instrument (or MIDI output) speaks MPE: notes get member
    /// channels and play their per-note expression.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mpe: Option<crate::MpeConfig>,
    #[serde(default)]
    pub monitor: MonitorMode,

    #[serde(default)]
    pub automation: AutomationSet,
}

impl Track {
    pub fn new(id: TrackId, kind: TrackKind, name: impl Into<String>, color: TrackColor) -> Self {
        let layout = match kind {
            TrackKind::Audio => ChannelLayout::Mono,
            _ => ChannelLayout::Stereo,
        };
        Self {
            id,
            kind,
            name: name.into(),
            color,
            layout,
            clips: Vec::new(),
            instrument: None,
            inserts: Vec::new(),
            sends: Vec::new(),
            // Instruments play what the keyboard plays.
            input: match kind {
                TrackKind::Instrument | TrackKind::Midi => InputRouting::all_midi(),
                _ => InputRouting::None,
            },
            output: match kind {
                TrackKind::Master => OutputRouting::Hardware { first_channel: 0 },
                _ => OutputRouting::Master,
            },
            volume_db: 0.0,
            pan: 0.0,
            phase_invert: false,
            mute: false,
            solo: false,
            record_arm: false,
            midi_output: None,
            mpe: None,
            monitor: match kind {
                TrackKind::Instrument | TrackKind::Midi => MonitorMode::Auto,
                _ => MonitorMode::Off,
            },
            automation: AutomationSet::default(),
        }
    }

    pub fn with_layout(mut self, layout: ChannelLayout) -> Self {
        self.layout = layout;
        self
    }

    pub fn send(&self, id: SendId) -> Option<&AuxSend> {
        self.sends.iter().find(|s| s.id == id)
    }

    pub fn send_mut(&mut self, id: SendId) -> Option<&mut AuxSend> {
        self.sends.iter_mut().find(|s| s.id == id)
    }

    /// Insert or instrument slot with `id`.
    pub fn plugin_mut(&mut self, id: PluginInstanceId) -> Option<&mut PluginSlot> {
        self.inserts
            .iter_mut()
            .chain(self.instrument.iter_mut())
            .find(|p| p.id == id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn colours_round_trip_as_hex() {
        let c = TrackColor::rgb(0x12, 0xab, 0xef);
        assert_eq!(c.to_hex(), "#12abef");
        assert_eq!(TrackColor::from_hex("#12ABEF"), Some(c));
        assert_eq!(TrackColor::from_hex("nope"), None);
        let json = serde_json::to_string(&c).unwrap();
        assert_eq!(json, "\"#12abef\"");
        assert!(serde_json::from_str::<TrackColor>("\"#zzzzzz\"").is_err());
    }

    #[test]
    fn new_tracks_have_sensible_defaults() {
        let audio = Track::new(TrackId(1), TrackKind::Audio, "Vox", TrackColor::palette(0));
        assert_eq!(audio.layout, ChannelLayout::Mono);
        assert_eq!(audio.output, OutputRouting::Master);
        let master = Track::new(
            TrackId(2),
            TrackKind::Master,
            "Master",
            TrackColor::palette(1),
        );
        assert_eq!(master.output, OutputRouting::Hardware { first_channel: 0 });
        let synth = Track::new(
            TrackId(3),
            TrackKind::Instrument,
            "Synth",
            TrackColor::palette(2),
        );
        assert_eq!(synth.input, InputRouting::all_midi());
        assert_eq!(synth.monitor, MonitorMode::Auto);
        let json = serde_json::to_string(&InputRouting::Midi {
            port: Some("MPK mini 3:MPK mini 3 MIDI 1".into()),
            channel: Some(9),
        })
        .unwrap();
        assert!(json.contains("\"type\":\"midi\""), "{json}");
        let back: InputRouting = serde_json::from_str(r#"{"type":"midi"}"#).unwrap();
        assert_eq!(back, InputRouting::all_midi());
        assert!(TrackKind::Bus.is_summing() && !TrackKind::Audio.is_summing());
    }
}
