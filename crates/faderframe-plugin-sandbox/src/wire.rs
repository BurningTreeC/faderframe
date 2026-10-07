//! The control protocol between FaderFrame and a plugin helper: a
//! [`Request`] and its [`Response`] as JSON with an optional binary payload
//! (plugin states, preset files). A frame is `MAGIC`, the JSON length and
//! the payload length (`u32` little endian each), then the bytes. Frames
//! over [`MAX_FRAME`] are refused: the peer is broken or hostile.

use faderframe_core::ParameterId;
use faderframe_midi::NoteExpressionKind;
use faderframe_plugin_host::{
    AudioPortInfo, EditorEdit, EditorRequests, ParameterInfo, ParameterUnit, PluginCategory,
    PluginDescriptor, PluginFormat, TailLength, WindowApi,
};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use std::io::{self, Read, Write};
use std::path::PathBuf;

const MAGIC: u32 = 0x4246_5346; // "FSFB"
/// The largest JSON part or payload accepted.
pub const MAX_FRAME: usize = 256 << 20;

/// Write one frame.
pub fn write_frame(w: &mut impl Write, json: &[u8], payload: &[u8]) -> io::Result<()> {
    if json.len() > MAX_FRAME || payload.len() > MAX_FRAME {
        return Err(io::Error::other("frame too large"));
    }
    let mut head = [0u8; 12];
    head[..4].copy_from_slice(&MAGIC.to_le_bytes());
    head[4..8].copy_from_slice(&(json.len() as u32).to_le_bytes());
    head[8..].copy_from_slice(&(payload.len() as u32).to_le_bytes());
    w.write_all(&head)?;
    w.write_all(json)?;
    w.write_all(payload)?;
    w.flush()
}

/// Read one frame: (JSON, payload).
pub fn read_frame(r: &mut impl Read) -> io::Result<(Vec<u8>, Vec<u8>)> {
    let mut head = [0u8; 12];
    r.read_exact(&mut head)?;
    let word = |i: usize| u32::from_le_bytes([head[i], head[i + 1], head[i + 2], head[i + 3]]);
    if word(0) != MAGIC {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "not a frame"));
    }
    let (json_len, payload_len) = (word(4) as usize, word(8) as usize);
    if json_len > MAX_FRAME || payload_len > MAX_FRAME {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "frame too large",
        ));
    }
    let mut json = vec![0u8; json_len];
    r.read_exact(&mut json)?;
    let mut payload = vec![0u8; payload_len];
    r.read_exact(&mut payload)?;
    Ok((json, payload))
}

/// Send a message with a payload.
pub fn send<T: Serialize>(w: &mut impl Write, msg: &T, payload: &[u8]) -> io::Result<()> {
    let json = serde_json::to_vec(msg).map_err(io::Error::other)?;
    write_frame(w, &json, payload)
}

/// Receive a message and its payload.
pub fn recv<T: DeserializeOwned>(r: &mut impl Read) -> io::Result<(T, Vec<u8>)> {
    let (json, payload) = read_frame(r)?;
    let msg =
        serde_json::from_slice(&json).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    Ok((msg, payload))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Format {
    Builtin,
    Clap,
    Vst3,
    AudioUnit,
    Lv2,
}

impl From<PluginFormat> for Format {
    fn from(f: PluginFormat) -> Self {
        match f {
            PluginFormat::Builtin => Format::Builtin,
            PluginFormat::Clap => Format::Clap,
            PluginFormat::Vst3 => Format::Vst3,
            PluginFormat::AudioUnit => Format::AudioUnit,
            PluginFormat::Lv2 => Format::Lv2,
        }
    }
}

impl From<Format> for PluginFormat {
    fn from(f: Format) -> Self {
        match f {
            Format::Builtin => PluginFormat::Builtin,
            Format::Clap => PluginFormat::Clap,
            Format::Vst3 => PluginFormat::Vst3,
            Format::AudioUnit => PluginFormat::AudioUnit,
            Format::Lv2 => PluginFormat::Lv2,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Api {
    X11,
    Win32,
    Cocoa,
}

impl From<WindowApi> for Api {
    fn from(a: WindowApi) -> Self {
        match a {
            WindowApi::X11 => Api::X11,
            WindowApi::Win32 => Api::Win32,
            WindowApi::Cocoa => Api::Cocoa,
        }
    }
}

impl From<Api> for WindowApi {
    fn from(a: Api) -> Self {
        match a {
            Api::X11 => WindowApi::X11,
            Api::Win32 => WindowApi::Win32,
            Api::Cocoa => WindowApi::Cocoa,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Param {
    pub id: u32,
    pub name: String,
    pub min: f64,
    pub max: f64,
    pub default: f64,
    pub unit: u8,
    pub automatable: bool,
    pub stepped: bool,
    /// Takes modulation (that leaves its value), and per note.
    #[serde(default)]
    pub modulatable: bool,
    #[serde(default)]
    pub per_note: bool,
}

const UNITS: [ParameterUnit; 6] = [
    ParameterUnit::None,
    ParameterUnit::Decibels,
    ParameterUnit::Milliseconds,
    ParameterUnit::Hertz,
    ParameterUnit::Percent,
    ParameterUnit::Samples,
];

impl From<&ParameterInfo> for Param {
    fn from(p: &ParameterInfo) -> Self {
        Self {
            id: p.id.0,
            name: p.name.clone(),
            min: p.min,
            max: p.max,
            default: p.default,
            unit: UNITS.iter().position(|u| *u == p.unit).unwrap_or(0) as u8,
            automatable: p.automatable,
            stepped: p.stepped,
            modulatable: false,
            per_note: false,
        }
    }
}

impl Param {
    /// The parameter with what the instance says of its modulation.
    pub fn of(p: &ParameterInfo, inst: &dyn faderframe_plugin_host::PluginInstance) -> Self {
        Self {
            modulatable: inst.modulatable(p.id),
            per_note: inst.modulatable_per_note(p.id),
            ..Self::from(p)
        }
    }
}

impl From<&Param> for ParameterInfo {
    fn from(p: &Param) -> Self {
        Self {
            id: ParameterId(p.id),
            name: p.name.clone(),
            min: p.min,
            max: p.max.max(p.min),
            default: p.default,
            unit: UNITS
                .get(p.unit as usize)
                .copied()
                .unwrap_or(ParameterUnit::None),
            automatable: p.automatable,
            stepped: p.stepped,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Descriptor {
    pub format: Format,
    pub id: String,
    pub name: String,
    pub vendor: String,
    pub version: String,
    /// Effect, instrument, analyzer, utility.
    pub category: u8,
    /// (channels, main) per port.
    pub audio_inputs: Vec<(u16, bool)>,
    pub audio_outputs: Vec<(u16, bool)>,
    pub note_inputs: u16,
    pub note_outputs: u16,
}

const CATEGORIES: [PluginCategory; 5] = [
    PluginCategory::Effect,
    PluginCategory::Instrument,
    PluginCategory::Analyzer,
    PluginCategory::Utility,
    PluginCategory::MidiEffect,
];

impl From<&PluginDescriptor> for Descriptor {
    fn from(d: &PluginDescriptor) -> Self {
        let ports = |p: &[AudioPortInfo]| p.iter().map(|p| (p.channels, p.is_main)).collect();
        Self {
            format: d.format.into(),
            id: d.id.clone(),
            name: d.name.clone(),
            vendor: d.vendor.clone(),
            version: d.version.clone(),
            category: CATEGORIES
                .iter()
                .position(|c| *c == d.category)
                .unwrap_or(0) as u8,
            audio_inputs: ports(&d.audio_inputs),
            audio_outputs: ports(&d.audio_outputs),
            note_inputs: d.note_inputs,
            note_outputs: d.note_outputs,
        }
    }
}

impl From<&Descriptor> for PluginDescriptor {
    fn from(d: &Descriptor) -> Self {
        let ports = |p: &[(u16, bool)]| {
            p.iter()
                .map(|&(channels, is_main)| AudioPortInfo { channels, is_main })
                .collect()
        };
        Self {
            format: d.format.into(),
            id: d.id.clone(),
            name: d.name.clone(),
            vendor: d.vendor.clone(),
            version: d.version.clone(),
            category: CATEGORIES
                .get(d.category as usize)
                .copied()
                .unwrap_or(PluginCategory::Effect),
            audio_inputs: ports(&d.audio_inputs),
            audio_outputs: ports(&d.audio_outputs),
            note_inputs: d.note_inputs,
            note_outputs: d.note_outputs,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Tail {
    None,
    Samples(u32),
    Infinite,
}

impl From<TailLength> for Tail {
    fn from(t: TailLength) -> Self {
        match t {
            TailLength::None => Tail::None,
            TailLength::Samples(n) => Tail::Samples(n),
            TailLength::Infinite => Tail::Infinite,
        }
    }
}

impl From<Tail> for TailLength {
    fn from(t: Tail) -> Self {
        match t {
            Tail::None => TailLength::None,
            Tail::Samples(n) => TailLength::Samples(n),
            Tail::Infinite => TailLength::Infinite,
        }
    }
}

/// Note expression kinds by their place in [`NoteExpressionKind::ALL`].
pub fn expression_index(k: NoteExpressionKind) -> u8 {
    NoteExpressionKind::ALL
        .iter()
        .position(|x| *x == k)
        .unwrap_or(0) as u8
}

pub fn expression_kind(i: u8) -> Option<NoteExpressionKind> {
    NoteExpressionKind::ALL.get(i as usize).copied()
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub enum Edit {
    Begin(u32),
    Value(u32, f64),
    End(u32),
}

impl From<EditorEdit> for Edit {
    fn from(e: EditorEdit) -> Self {
        match e {
            EditorEdit::Begin(p) => Edit::Begin(p.0),
            EditorEdit::Value(p, v) => Edit::Value(p.0, v),
            EditorEdit::End(p) => Edit::End(p.0),
        }
    }
}

impl From<Edit> for EditorEdit {
    fn from(e: Edit) -> Self {
        match e {
            Edit::Begin(p) => EditorEdit::Begin(ParameterId(p)),
            Edit::Value(p, v) => EditorEdit::Value(ParameterId(p), v),
            Edit::End(p) => EditorEdit::End(ParameterId(p)),
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct EditorReq {
    pub resize: Option<(u32, u32)>,
    pub show: bool,
    pub hide: bool,
    pub closed: bool,
}

impl From<EditorRequests> for EditorReq {
    fn from(r: EditorRequests) -> Self {
        Self {
            resize: r.resize,
            show: r.show,
            hide: r.hide,
            closed: r.closed,
        }
    }
}

impl From<EditorReq> for EditorRequests {
    fn from(r: EditorReq) -> Self {
        Self {
            resize: r.resize,
            show: r.show,
            hide: r.hide,
            closed: r.closed,
        }
    }
}

/// What an instantiated plugin is.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Instantiated {
    pub descriptor: Descriptor,
    pub params: Vec<Param>,
    pub values: Vec<(u32, f64)>,
    pub latency: u32,
    pub tail: Tail,
    /// Indexes into `NoteExpressionKind::ALL` (`None`: the plugin does not
    /// say).
    pub note_expressions: Option<Vec<u8>>,
    pub has_editor: bool,
    /// The plugin's program list.
    #[serde(default)]
    pub programs: Vec<String>,
}

/// What happened in the helper since the last poll.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Polled {
    pub restart: bool,
    pub params_changed: bool,
    pub state_dirty: bool,
    pub edits: Vec<Edit>,
    pub editor: EditorReq,
    pub editor_open: bool,
    /// Parameter values that changed.
    pub values: Vec<(u32, f64)>,
    /// The parameter list, when it changed.
    pub params: Option<Vec<Param>>,
    pub latency: u32,
    pub tail: Option<Tail>,
    /// The selected program.
    #[serde(default)]
    pub program: Option<usize>,
    /// The program list, when the parameters changed.
    #[serde(default)]
    pub programs: Option<Vec<String>>,
    /// Changes the processor has not taken yet.
    #[serde(default)]
    pub pending: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum EditorCall {
    CanEmbed(Api),
    CanFloat(Api),
    OpenEmbedded {
        api: Api,
        scale: f64,
    },
    Attach {
        api: Api,
        handle: u64,
    },
    OpenFloating {
        api: Api,
        title: String,
    },
    Close,
    /// Bring the editor's window to the front (one the helper owns).
    Raise,
    CanResize,
    SetSize {
        width: u32,
        height: u32,
    },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum Request {
    Instantiate {
        format: Format,
        id: String,
    },
    SetParameter {
        id: u32,
        value: f64,
    },
    FormatParameter {
        id: u32,
        value: f64,
    },
    SaveState,
    /// Payload: the state.
    LoadState,
    PresetFiles,
    /// Payload: the file.
    StateFromPresetFile,
    SelectProgram {
        index: usize,
    },
    Poll,
    /// Activate for processing with the shared memory block `shm`.
    Activate {
        sample_rate: f64,
        max_block: u32,
        sidechain: bool,
        shm: String,
        shm_size: u64,
        #[serde(default)]
        double_precision: bool,
    },
    Deactivate,
    Editor(EditorCall),
    Quit,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum Response {
    Done,
    Failed(String),
    Instantiated(Box<Instantiated>),
    Text(Option<String>),
    /// Payload: the bytes.
    Bytes,
    Paths(Vec<PathBuf>),
    Polled(Box<Polled>),
    Activated {
        latency: u32,
    },
    Flag(bool),
    Size(Option<(u32, u32)>),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frames_round_trip_and_reject_garbage() {
        let mut buf = Vec::new();
        send(&mut buf, &Request::SetParameter { id: 7, value: 0.5 }, b"").unwrap();
        send(&mut buf, &Request::LoadState, &[1, 2, 3]).unwrap();
        let mut r = buf.as_slice();
        let (a, p): (Request, _) = recv(&mut r).unwrap();
        assert_eq!(a, Request::SetParameter { id: 7, value: 0.5 });
        assert!(p.is_empty());
        let (b, p): (Request, _) = recv(&mut r).unwrap();
        assert_eq!((b, p), (Request::LoadState, vec![1, 2, 3]));
        // Wrong magic, oversized lengths and truncation are errors.
        let mut bad = buf.clone();
        bad[0] ^= 0xFF;
        assert!(recv::<Request>(&mut bad.as_slice()).is_err());
        let mut huge = buf.clone();
        huge[4..8].copy_from_slice(&u32::MAX.to_le_bytes());
        assert!(recv::<Request>(&mut huge.as_slice()).is_err());
        assert!(recv::<Request>(&mut &buf[..10]).is_err());
    }

    #[test]
    fn host_types_survive_the_trip() {
        let d = PluginDescriptor {
            format: PluginFormat::Clap,
            id: "a.b".into(),
            name: "AB".into(),
            vendor: "V".into(),
            version: "1".into(),
            category: PluginCategory::Instrument,
            audio_inputs: vec![],
            audio_outputs: vec![AudioPortInfo {
                channels: 2,
                is_main: true,
            }],
            note_inputs: 1,
            note_outputs: 0,
        };
        assert_eq!(PluginDescriptor::from(&Descriptor::from(&d)), d);
        let p = ParameterInfo {
            id: ParameterId(3),
            name: "Cutoff".into(),
            min: 20.0,
            max: 20_000.0,
            default: 1_000.0,
            unit: ParameterUnit::Hertz,
            automatable: true,
            stepped: false,
        };
        assert_eq!(ParameterInfo::from(&Param::from(&p)), p);
        for k in NoteExpressionKind::ALL {
            assert_eq!(expression_kind(expression_index(k)), Some(k));
        }
    }
}
