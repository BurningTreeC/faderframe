//! DAWproject 1.0: the document model, written in the schema's element
//! order ([`write_project`], [`write_metadata`]) and read back
//! ([`read_project`], [`read_metadata`]), and the container ([`file`]).
//!
//! The model mirrors `Project.xsd` (vendored in `data/dawproject`): a
//! project is an application, a transport (tempo and time signature), a
//! structure of tracks and channels (tracks nest: folders hold tracks), an
//! arrangement of timelines and clip launcher scenes. Timelines nest
//! freely — lanes hold timelines, clips hold one timeline each, warps map
//! one timeline's time onto their own. Times are in beats (quarter notes)
//! or seconds, whichever the nearest `timeUnit` above says (beats when
//! none does).
//!
//! What the schema calls `Nameable` (name, colour, comment) and
//! `Referenceable` (those and an `id`) is [`Meta`]; elements that are only
//! nameable (clips, markers) leave its `id` out when written.

mod read;
mod write;

pub mod file;

pub use read::{read_metadata, read_project};
pub use write::{write_metadata, write_project};

/// The DAWproject version written.
pub const VERSION: &str = "1.0";

macro_rules! names {
    ($(#[$m:meta])* $name:ident { $($variant:ident = $text:literal),* $(,)? }) => {
        $(#[$m])*
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
        pub enum $name {
            $($variant),*
        }

        impl $name {
            pub const ALL: &'static [$name] = &[$($name::$variant),*];

            pub fn as_str(self) -> &'static str {
                match self {
                    $($name::$variant => $text),*
                }
            }

            pub fn parse(text: &str) -> Option<Self> {
                match text.trim() {
                    $($text => Some($name::$variant),)*
                    _ => None,
                }
            }
        }

        impl std::fmt::Display for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str(self.as_str())
            }
        }
    };
}

names!(
    /// The time unit of a timeline (beats = quarter notes).
    TimeUnit { Beats = "beats", Seconds = "seconds" }
);

names!(
    /// The unit of a real parameter's values.
    Unit {
        Linear = "linear",
        Normalized = "normalized",
        Percent = "percent",
        Decibel = "decibel",
        Hertz = "hertz",
        Semitones = "semitones",
        Seconds = "seconds",
        Beats = "beats",
        Bpm = "bpm",
    }
);

names!(
    /// What a track's timelines hold.
    ContentType {
        Audio = "audio",
        Automation = "automation",
        Notes = "notes",
        Video = "video",
        Markers = "markers",
        Tracks = "tracks",
    }
);

names!(
    /// A mixer channel's role.
    MixerRole {
        Regular = "regular",
        Master = "master",
        Effect = "effect",
        Submix = "submix",
        Vca = "vca",
    }
);

names!(
    /// Where a send taps.
    SendType { Pre = "pre", Post = "post" }
);

names!(
    DeviceRole {
        Instrument = "instrument",
        NoteFx = "noteFX",
        AudioFx = "audioFX",
        Analyzer = "analyzer",
    }
);

names!(
    /// How a real point's segment goes to the next point (hold when
    /// unsaid).
    Interpolation { Hold = "hold", Linear = "linear" }
);

names!(
    /// What an expression timeline moves.
    ExpressionType {
        Gain = "gain",
        Pan = "pan",
        Transpose = "transpose",
        Timbre = "timbre",
        Formant = "formant",
        Pressure = "pressure",
        ChannelController = "channelController",
        ChannelPressure = "channelPressure",
        PolyPressure = "polyPressure",
        PitchBend = "pitchBend",
        ProgramChange = "programChange",
    }
);

names!(
    EqBandType {
        HighPass = "highPass",
        LowPass = "lowPass",
        BandPass = "bandPass",
        HighShelf = "highShelf",
        LowShelf = "lowShelf",
        Bell = "bell",
        Notch = "notch",
    }
);

/// Name, colour (`#rrggbb`) and comment, and an id for elements others
/// refer to.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Meta {
    pub id: Option<String>,
    pub name: Option<String>,
    pub color: Option<String>,
    pub comment: Option<String>,
}

impl Meta {
    pub fn id(id: impl Into<String>) -> Self {
        Self {
            id: Some(id.into()),
            ..Self::default()
        }
    }

    pub fn named(id: impl Into<String>, name: impl Into<String>) -> Self {
        Self {
            id: Some(id.into()),
            name: Some(name.into()),
            ..Self::default()
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct RealParameter {
    pub meta: Meta,
    pub parameter_id: Option<i32>,
    pub unit: Unit,
    pub min: Option<f64>,
    pub max: Option<f64>,
    pub value: Option<f64>,
}

impl RealParameter {
    pub fn new(meta: Meta, unit: Unit, value: f64) -> Self {
        Self {
            meta,
            parameter_id: None,
            unit,
            min: None,
            max: None,
            value: Some(value),
        }
    }

    pub fn range(mut self, min: f64, max: f64) -> Self {
        self.min = Some(min);
        self.max = Some(max);
        self
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BoolParameter {
    pub meta: Meta,
    pub parameter_id: Option<i32>,
    pub value: Option<bool>,
}

impl BoolParameter {
    pub fn new(meta: Meta, value: bool) -> Self {
        Self {
            meta,
            parameter_id: None,
            value: Some(value),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IntegerParameter {
    pub meta: Meta,
    pub parameter_id: Option<i32>,
    pub min: Option<i64>,
    pub max: Option<i64>,
    pub value: Option<i64>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EnumParameter {
    pub meta: Meta,
    pub parameter_id: Option<i32>,
    pub count: i64,
    pub labels: Vec<String>,
    pub value: Option<i64>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TimeSignatureParameter {
    pub meta: Meta,
    pub parameter_id: Option<i32>,
    pub numerator: i64,
    pub denominator: i64,
}

/// A device's parameter (what automation targets by its id).
#[derive(Clone, Debug, PartialEq)]
pub enum Parameter {
    Real(RealParameter),
    Bool(BoolParameter),
    Integer(IntegerParameter),
    Enum(EnumParameter),
    TimeSignature(TimeSignatureParameter),
}

impl Parameter {
    pub fn meta(&self) -> &Meta {
        match self {
            Parameter::Real(p) => &p.meta,
            Parameter::Bool(p) => &p.meta,
            Parameter::Integer(p) => &p.meta,
            Parameter::Enum(p) => &p.meta,
            Parameter::TimeSignature(p) => &p.meta,
        }
    }

    pub fn parameter_id(&self) -> Option<i32> {
        match self {
            Parameter::Real(p) => p.parameter_id,
            Parameter::Bool(p) => p.parameter_id,
            Parameter::Integer(p) => p.parameter_id,
            Parameter::Enum(p) => p.parameter_id,
            Parameter::TimeSignature(p) => p.parameter_id,
        }
    }

    /// Its value as a number (booleans 0/1).
    pub fn number(&self) -> Option<f64> {
        match self {
            Parameter::Real(p) => p.value,
            Parameter::Bool(p) => p.value.map(|b| f64::from(u8::from(b))),
            Parameter::Integer(p) => p.value.map(|v| v as f64),
            Parameter::Enum(p) => p.value.map(|v| v as f64),
            Parameter::TimeSignature(_) => None,
        }
    }
}

/// A file in the container (or, `external`, beside it).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileReference {
    pub path: String,
    pub external: Option<bool>,
}

impl FileReference {
    pub fn inside(path: impl Into<String>) -> Self {
        Self {
            path: path.into(),
            external: None,
        }
    }
}

/// Which element a device is written as.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum DeviceKind {
    Device,
    Vst2,
    Vst3,
    Clap,
    Au,
    Builtin,
    Equalizer,
    Compressor,
    NoiseGate,
    Limiter,
}

impl DeviceKind {
    pub const ALL: [DeviceKind; 10] = [
        DeviceKind::Device,
        DeviceKind::Vst2,
        DeviceKind::Vst3,
        DeviceKind::Clap,
        DeviceKind::Au,
        DeviceKind::Builtin,
        DeviceKind::Equalizer,
        DeviceKind::Compressor,
        DeviceKind::NoiseGate,
        DeviceKind::Limiter,
    ];

    pub fn element(self) -> &'static str {
        match self {
            DeviceKind::Device => "Device",
            DeviceKind::Vst2 => "Vst2Plugin",
            DeviceKind::Vst3 => "Vst3Plugin",
            DeviceKind::Clap => "ClapPlugin",
            DeviceKind::Au => "AuPlugin",
            DeviceKind::Builtin => "BuiltinDevice",
            DeviceKind::Equalizer => "Equalizer",
            DeviceKind::Compressor => "Compressor",
            DeviceKind::NoiseGate => "NoiseGate",
            DeviceKind::Limiter => "Limiter",
        }
    }

    pub fn of_element(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|k| k.element() == name)
    }

    /// A plug-in format's (they carry a `pluginVersion`).
    pub fn is_plugin(self) -> bool {
        matches!(
            self,
            DeviceKind::Vst2 | DeviceKind::Vst3 | DeviceKind::Clap | DeviceKind::Au
        )
    }

    /// The parameters a generic built-in device has of its own, in the
    /// schema's order.
    pub fn own_parameters(self) -> &'static [&'static str] {
        match self {
            DeviceKind::Equalizer => &["InputGain", "OutputGain"],
            DeviceKind::Compressor => &[
                "Attack",
                "AutoMakeup",
                "InputGain",
                "OutputGain",
                "Ratio",
                "Release",
                "Threshold",
            ],
            DeviceKind::NoiseGate => &["Attack", "Range", "Ratio", "Release", "Threshold"],
            DeviceKind::Limiter => &["Attack", "InputGain", "OutputGain", "Release", "Threshold"],
            _ => &[],
        }
    }
}

/// A band of the generic equalizer.
#[derive(Clone, Debug, PartialEq)]
pub struct EqBand {
    pub kind: EqBandType,
    pub order: Option<i64>,
    pub freq: RealParameter,
    pub gain: Option<RealParameter>,
    pub q: Option<RealParameter>,
    pub enabled: Option<BoolParameter>,
}

/// A device or plug-in on a channel.
#[derive(Clone, Debug, PartialEq)]
pub struct Device {
    pub kind: DeviceKind,
    pub meta: Meta,
    pub role: DeviceRole,
    pub loaded: Option<bool>,
    pub device_name: String,
    /// CLAP: its id; VST3: its class as a UUID; VST2: its id in decimal.
    pub device_id: Option<String>,
    pub vendor: Option<String>,
    pub plugin_version: Option<String>,
    /// Parameters given ids (for automation; the generic devices' own are
    /// in `own`).
    pub parameters: Vec<Parameter>,
    pub enabled: Option<BoolParameter>,
    /// Its state in the plug-in's native format.
    pub state: Option<FileReference>,
    /// A generic device's own parameters by element name (see
    /// [`DeviceKind::own_parameters`]).
    pub own: Vec<(String, Parameter)>,
    /// The generic equalizer's bands.
    pub bands: Vec<EqBand>,
}

impl Device {
    pub fn new(kind: DeviceKind, meta: Meta, role: DeviceRole, name: impl Into<String>) -> Self {
        Self {
            kind,
            meta,
            role,
            loaded: Some(true),
            device_name: name.into(),
            device_id: None,
            vendor: None,
            plugin_version: None,
            parameters: Vec::new(),
            enabled: None,
            state: None,
            own: Vec::new(),
            bands: Vec::new(),
        }
    }

    /// One of its own parameters (`InputGain`, `Threshold`, …).
    pub fn own(&self, name: &str) -> Option<&Parameter> {
        self.own.iter().find(|(n, _)| n == name).map(|(_, p)| p)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Send {
    pub meta: Meta,
    pub destination: Option<String>,
    pub kind: Option<SendType>,
    pub enable: Option<BoolParameter>,
    pub pan: Option<RealParameter>,
    pub volume: RealParameter,
}

/// A mixer channel.
#[derive(Clone, Debug, PartialEq)]
pub struct Channel {
    pub meta: Meta,
    pub role: Option<MixerRole>,
    pub audio_channels: Option<i64>,
    pub solo: Option<bool>,
    /// The channel it plays into.
    pub destination: Option<String>,
    pub devices: Vec<Device>,
    pub mute: Option<BoolParameter>,
    pub pan: Option<RealParameter>,
    pub sends: Vec<Send>,
    pub volume: Option<RealParameter>,
}

impl Channel {
    pub fn new(meta: Meta) -> Self {
        Self {
            meta,
            role: None,
            audio_channels: None,
            solo: None,
            destination: None,
            devices: Vec::new(),
            mute: None,
            pan: None,
            sends: Vec::new(),
            volume: None,
        }
    }
}

/// A track: what its timelines hold, its channel, the tracks it holds.
#[derive(Clone, Debug, PartialEq)]
pub struct Track {
    pub meta: Meta,
    pub content_types: Vec<ContentType>,
    pub loaded: Option<bool>,
    pub channel: Option<Channel>,
    pub tracks: Vec<Track>,
}

/// An entry of the structure: a track, or a channel of its own.
#[derive(Clone, Debug, PartialEq)]
pub enum Lane {
    Track(Track),
    Channel(Channel),
}

/// What every timeline has: its meta, its time unit, the track it is
/// local to.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TimelineMeta {
    pub meta: Meta,
    pub time_unit: Option<TimeUnit>,
    pub track: Option<String>,
}

impl TimelineMeta {
    pub fn id(id: impl Into<String>) -> Self {
        Self {
            meta: Meta::id(id),
            ..Self::default()
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum Timeline {
    Lanes(Lanes),
    Clips(Clips),
    Notes(Notes),
    Points(Points),
    Warps(Warps),
    Audio(MediaFile),
    Video(MediaFile),
    Markers(Markers),
    ClipSlot(ClipSlot),
}

impl Timeline {
    pub fn common(&self) -> &TimelineMeta {
        match self {
            Timeline::Lanes(t) => &t.common,
            Timeline::Clips(t) => &t.common,
            Timeline::Notes(t) => &t.common,
            Timeline::Points(t) => &t.common,
            Timeline::Warps(t) => &t.common,
            Timeline::Audio(t) | Timeline::Video(t) => &t.common,
            Timeline::Markers(t) => &t.common,
            Timeline::ClipSlot(t) => &t.common,
        }
    }

    /// Its element name.
    pub fn element(&self) -> &'static str {
        match self {
            Timeline::Lanes(_) => "Lanes",
            Timeline::Clips(_) => "Clips",
            Timeline::Notes(_) => "Notes",
            Timeline::Points(_) => "Points",
            Timeline::Warps(_) => "Warps",
            Timeline::Audio(_) => "Audio",
            Timeline::Video(_) => "Video",
            // The schema names it in lower case where timelines nest.
            Timeline::Markers(_) => "markers",
            Timeline::ClipSlot(_) => "ClipSlot",
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Lanes {
    pub common: TimelineMeta,
    pub lanes: Vec<Timeline>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Clips {
    pub common: TimelineMeta,
    pub clips: Vec<Clip>,
}

/// A clipped view onto a timeline (its content, or the one it refers to).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Clip {
    pub meta: Meta,
    pub time: f64,
    pub duration: Option<f64>,
    pub content_time_unit: Option<TimeUnit>,
    pub play_start: Option<f64>,
    pub play_stop: Option<f64>,
    pub loop_start: Option<f64>,
    pub loop_end: Option<f64>,
    pub fade_time_unit: Option<TimeUnit>,
    pub fade_in_time: Option<f64>,
    pub fade_out_time: Option<f64>,
    /// Plays (true when unsaid).
    pub enable: Option<bool>,
    /// A linked (alias) clip's timeline.
    pub reference: Option<String>,
    pub content: Option<Box<Timeline>>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Notes {
    pub common: TimelineMeta,
    pub notes: Vec<Note>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Note {
    pub time: f64,
    pub duration: f64,
    pub channel: i64,
    pub key: i64,
    /// 0…1.
    pub vel: Option<f64>,
    pub rel: Option<f64>,
    /// Per-note expression (relative to the note's start).
    pub content: Option<Box<Timeline>>,
}

/// What automation or expression moves.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AutomationTarget {
    pub parameter: Option<String>,
    pub expression: Option<ExpressionType>,
    pub channel: Option<i64>,
    pub key: Option<i64>,
    pub controller: Option<i64>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Point {
    Real {
        time: f64,
        value: f64,
        interpolation: Option<Interpolation>,
    },
    Bool {
        time: f64,
        value: bool,
    },
    Integer {
        time: f64,
        value: i64,
    },
    Enum {
        time: f64,
        value: i64,
    },
    TimeSignature {
        time: f64,
        numerator: i64,
        denominator: i64,
    },
}

impl Point {
    pub fn time(&self) -> f64 {
        match *self {
            Point::Real { time, .. }
            | Point::Bool { time, .. }
            | Point::Integer { time, .. }
            | Point::Enum { time, .. }
            | Point::TimeSignature { time, .. } => time,
        }
    }

    /// Its value as a number (booleans 0/1; a time signature has none).
    pub fn number(&self) -> Option<f64> {
        match *self {
            Point::Real { value, .. } => Some(value),
            Point::Bool { value, .. } => Some(f64::from(u8::from(value))),
            Point::Integer { value, .. } | Point::Enum { value, .. } => Some(value as f64),
            Point::TimeSignature { .. } => None,
        }
    }

    /// The segment to the next point is a line (only real points' can be).
    pub fn is_linear(&self) -> bool {
        matches!(
            self,
            Point::Real {
                interpolation: Some(Interpolation::Linear),
                ..
            }
        )
    }
}

/// Automation or expression points.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Points {
    pub common: TimelineMeta,
    pub unit: Option<Unit>,
    pub target: AutomationTarget,
    pub points: Vec<Point>,
}

/// One timeline's time mapped onto this one's by warp points (linear
/// between them).
#[derive(Clone, Debug, PartialEq)]
pub struct Warps {
    pub common: TimelineMeta,
    pub content_time_unit: TimeUnit,
    pub content: Option<Box<Timeline>>,
    pub warps: Vec<Warp>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Warp {
    pub time: f64,
    pub content_time: f64,
}

/// An audio (or video) file as a timeline in seconds.
#[derive(Clone, Debug, PartialEq)]
pub struct MediaFile {
    pub common: TimelineMeta,
    /// Seconds, the whole file.
    pub duration: f64,
    pub file: FileReference,
    pub algorithm: Option<String>,
    pub channels: Option<i64>,
    pub sample_rate: Option<i64>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Markers {
    pub common: TimelineMeta,
    pub markers: Vec<Marker>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Marker {
    pub meta: Meta,
    pub time: f64,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct ClipSlot {
    pub common: TimelineMeta,
    pub has_stop: Option<bool>,
    pub clip: Option<Clip>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Arrangement {
    pub meta: Meta,
    pub lanes: Option<Lanes>,
    pub markers: Option<Markers>,
    pub tempo_automation: Option<Points>,
    pub time_signature_automation: Option<Points>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Scene {
    pub meta: Meta,
    pub content: Option<Timeline>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Transport {
    pub tempo: Option<RealParameter>,
    pub time_signature: Option<TimeSignatureParameter>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Application {
    pub name: String,
    pub version: String,
}

/// `project.xml`.
#[derive(Clone, Debug, PartialEq)]
pub struct Project {
    pub version: String,
    pub application: Application,
    pub transport: Option<Transport>,
    pub structure: Vec<Lane>,
    pub arrangement: Option<Arrangement>,
    pub scenes: Vec<Scene>,
}

impl Project {
    /// Every track, depth first (a folder before what it holds).
    pub fn tracks(&self) -> Vec<&Track> {
        fn walk<'a>(t: &'a Track, out: &mut Vec<&'a Track>) {
            out.push(t);
            for c in &t.tracks {
                walk(c, out);
            }
        }
        let mut out = Vec::new();
        for l in &self.structure {
            if let Lane::Track(t) = l {
                walk(t, &mut out);
            }
        }
        out
    }

    /// Every channel: the tracks' and those standing alone.
    pub fn channels(&self) -> Vec<&Channel> {
        let mut out: Vec<&Channel> = self
            .tracks()
            .into_iter()
            .filter_map(|t| t.channel.as_ref())
            .collect();
        out.extend(self.structure.iter().filter_map(|l| match l {
            Lane::Channel(c) => Some(c),
            Lane::Track(_) => None,
        }));
        out
    }
}

/// `metadata.xml`: the song's details (all optional).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MetaData {
    pub title: Option<String>,
    pub artist: Option<String>,
    pub album: Option<String>,
    pub original_artist: Option<String>,
    pub composer: Option<String>,
    pub songwriter: Option<String>,
    pub producer: Option<String>,
    pub arranger: Option<String>,
    pub year: Option<String>,
    pub genre: Option<String>,
    pub copyright: Option<String>,
    pub website: Option<String>,
    pub comment: Option<String>,
}

impl MetaData {
    /// The fields with their element names, in the schema's order.
    pub fn fields(&self) -> [(&'static str, &Option<String>); 13] {
        [
            ("Title", &self.title),
            ("Artist", &self.artist),
            ("Album", &self.album),
            ("OriginalArtist", &self.original_artist),
            ("Composer", &self.composer),
            ("Songwriter", &self.songwriter),
            ("Producer", &self.producer),
            ("Arranger", &self.arranger),
            ("Year", &self.year),
            ("Genre", &self.genre),
            ("Copyright", &self.copyright),
            ("Website", &self.website),
            ("Comment", &self.comment),
        ]
    }

    pub(crate) fn field_mut(&mut self, name: &str) -> Option<&mut Option<String>> {
        Some(match name {
            "Title" => &mut self.title,
            "Artist" => &mut self.artist,
            "Album" => &mut self.album,
            "OriginalArtist" => &mut self.original_artist,
            "Composer" => &mut self.composer,
            "Songwriter" => &mut self.songwriter,
            "Producer" => &mut self.producer,
            "Arranger" => &mut self.arranger,
            "Year" => &mut self.year,
            "Genre" => &mut self.genre,
            "Copyright" => &mut self.copyright,
            "Website" => &mut self.website,
            "Comment" => &mut self.comment,
            _ => return None,
        })
    }
}

#[cfg(test)]
mod tests;
