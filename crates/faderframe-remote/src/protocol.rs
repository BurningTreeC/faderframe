//! The remote's protocol: JSON text messages over the WebSocket, each an
//! object tagged by `"t"`.
//!
//! The web app sends `pair` (a code and its name) or `hello` (its token),
//! then `view` (the screen it shows: meters go only to screens that show
//! them) and [`RemoteInput`]s. The server answers `paired` (the token to
//! keep), `welcome` (the device's name and [`Role`]) or `refused` (why),
//! then `state` (the whole [`State`] as the role sees it), `patch`es (the
//! [`crate::diff`] since the last), `meters` and `notice`s; `bye` before it
//! closes a forgotten device.
//!
//! Faders, sends and pans are fader travel (0…1, the console law; the
//! state says where 0 dB is) and −1…1; a drag is `touch` on, the moves,
//! `touch` off — one undo step, as a control surface's. Ids are the
//! project's.

use crate::pairing::Role;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;

/// The longest message a client may send (bytes).
pub const MAX_MESSAGE: usize = 16 * 1024;

/// What the app shows (meters are sent only for those that show them).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Screen {
    #[default]
    Transport,
    Mixer,
    Cues,
    Setlist,
    Lyrics,
    Launcher,
}

impl Screen {
    /// The screen shows meters.
    pub fn metered(self) -> bool {
        matches!(self, Screen::Mixer | Screen::Cues)
    }
}

/// The transport's buttons.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TransportButton {
    Play,
    Stop,
    /// Play when stopped, stop when playing.
    PlayStop,
    Record,
    Loop,
    Click,
    /// The song's start and end.
    Start,
    End,
    /// A bar back or on.
    Rewind,
    Forward,
    Undo,
    Redo,
    /// A marker at the playhead.
    AddMarker,
}

/// Moves to the next or previous marker or section.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Step {
    MarkerNext,
    MarkerPrevious,
    SectionNext,
    SectionPrevious,
}

/// Show mode's controls.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ShowControl {
    Enter,
    Leave,
    Next,
    Previous,
    PlayStop,
}

/// Mix scene controls.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SceneStep {
    Next,
    Previous,
}

/// What a paired device asks for.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "snake_case")]
pub enum RemoteInput {
    Transport {
        button: TransportButton,
    },
    LocateMarker {
        id: u64,
    },
    LocateSection {
        id: u64,
    },
    Step {
        step: Step,
    },
    /// A fader, pan or send held (on) or let go: the moves between are one
    /// undo step.
    Touch {
        on: bool,
    },
    Fader {
        track: u64,
        travel: f32,
    },
    Pan {
        track: u64,
        pan: f32,
    },
    Mute {
        track: u64,
        on: bool,
    },
    Solo {
        track: u64,
        on: bool,
    },
    Arm {
        track: u64,
        on: bool,
    },
    Send {
        track: u64,
        send: u64,
        travel: f32,
    },
    SendOn {
        track: u64,
        send: u64,
        on: bool,
    },
    /// A cue mix's master level and mute.
    CueLevel {
        cue: u64,
        travel: f32,
    },
    CueMute {
        cue: u64,
        on: bool,
    },
    /// A track's send to a cue mix.
    CueSend {
        cue: u64,
        track: u64,
        travel: f32,
    },
    CuePan {
        cue: u64,
        track: u64,
        pan: f32,
    },
    CueOn {
        cue: u64,
        track: u64,
        on: bool,
    },
    Show {
        control: ShowControl,
    },
    /// Stand on a setlist song (`go`: and play it).
    ShowSong {
        song: usize,
        go: bool,
    },
    /// The clip launcher: a slot pressed and let go, a track's clip
    /// stopped, a scene, everything stopped, back to the arrangement.
    Launch {
        track: u64,
        scene: u64,
    },
    Release {
        track: u64,
        scene: u64,
    },
    StopTrack {
        track: u64,
    },
    LaunchScene {
        scene: u64,
    },
    StopClips,
    BackToArrangement,
    /// Mix scenes.
    MixScene {
        step: SceneStep,
    },
    RecallScene {
        scene: u64,
    },
    /// Run macro n.
    Macro {
        number: u16,
    },
}

impl RemoteInput {
    /// The cue mix this input changes, if it changes only one.
    pub fn cue(&self) -> Option<u64> {
        match self {
            RemoteInput::CueLevel { cue, .. }
            | RemoteInput::CueMute { cue, .. }
            | RemoteInput::CueSend { cue, .. }
            | RemoteInput::CuePan { cue, .. }
            | RemoteInput::CueOn { cue, .. } => Some(*cue),
            _ => None,
        }
    }

    /// Whether a device in `role` may send this (`cue`: the id of the cue
    /// mix a performer's role names, when the project has it).
    pub fn allowed(&self, role: &Role, cue: Option<u64>) -> bool {
        match role {
            Role::Full => true,
            Role::Viewer => false,
            Role::Performer { .. } => {
                matches!(self, RemoteInput::Touch { .. })
                    || (self.cue().is_some() && self.cue() == cue)
            }
        }
    }
}

/// What a client sends.
#[derive(Clone, Debug, PartialEq)]
pub enum ClientMessage {
    Pair { code: String, name: String },
    Hello { token: String },
    View { screen: Screen },
    Input(RemoteInput),
}

/// Read a client's message.
pub fn parse(text: &str) -> Result<ClientMessage, String> {
    #[derive(Deserialize)]
    #[serde(tag = "t", rename_all = "snake_case")]
    enum Auth {
        Pair {
            code: String,
            #[serde(default)]
            name: String,
        },
        Hello {
            token: String,
        },
        View {
            screen: Screen,
        },
    }
    let v: Value = serde_json::from_str(text).map_err(|e| e.to_string())?;
    let tag = v.get("t").and_then(Value::as_str).unwrap_or("");
    if matches!(tag, "pair" | "hello" | "view") {
        let a: Auth = serde_json::from_value(v).map_err(|e| e.to_string())?;
        return Ok(match a {
            Auth::Pair { code, name } => ClientMessage::Pair { code, name },
            Auth::Hello { token } => ClientMessage::Hello { token },
            Auth::View { screen } => ClientMessage::View { screen },
        });
    }
    serde_json::from_value(v)
        .map(ClientMessage::Input)
        .map_err(|e| e.to_string())
}

/// A server message as text.
pub fn message(t: &str, fields: Value) -> String {
    let mut m = match fields {
        Value::Object(m) => m,
        _ => serde_json::Map::new(),
    };
    m.insert("t".into(), Value::String(t.into()));
    Value::Object(m).to_string()
}

// --- the state ---------------------------------------------------------------

/// The role as the app sees it.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct RoleView {
    /// "full", "performer" or "viewer".
    pub kind: String,
    /// A performer's cue mix: its name, and its id when the project has
    /// it.
    pub cue_name: Option<String>,
    pub cue: Option<u64>,
}

/// The transport.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct TransportView {
    pub playing: bool,
    pub recording: bool,
    pub looping: bool,
    pub click: bool,
    /// The heard position in seconds (the app moves it on between updates
    /// while playing).
    pub seconds: f64,
    /// "12.3.1" (bar, beat, sixteenth).
    pub bars: String,
    /// "1:23.4".
    pub time: String,
    pub bpm: f64,
    pub signature: String,
    /// Show mode is counting down to a song (seconds).
    pub countdown: Option<f64>,
}

/// A send of a track.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct SendView {
    pub id: u64,
    /// Where it goes.
    pub name: String,
    pub travel: f32,
    pub level: String,
    pub on: bool,
}

/// A track's mixer strip.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct TrackView {
    pub id: u64,
    pub name: String,
    /// "audio", "instrument", "bus", "aux", "vca", "master".
    pub kind: String,
    /// "#rrggbb".
    pub color: String,
    pub fader: f32,
    /// The level as text ("-3.0", "-inf").
    pub level: String,
    pub pan: f32,
    /// It pans (not a VCA).
    pub pans: bool,
    pub mute: bool,
    pub solo: bool,
    pub arm: bool,
    /// It records (audio, instrument).
    pub armable: bool,
    pub sends: Vec<SendView>,
}

/// A track's send to a cue mix.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct CueSendView {
    pub travel: f32,
    pub level: String,
    pub pan: f32,
    pub on: bool,
}

/// A cue mix.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct CueView {
    pub id: u64,
    pub name: String,
    pub color: String,
    pub travel: f32,
    pub level: String,
    pub mute: bool,
    /// By track id (the tracks that feed cue mixes, in `cue_order`).
    pub sends: BTreeMap<String, CueSendView>,
}

/// A marker.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct MarkerView {
    pub id: u64,
    pub name: String,
    pub seconds: f64,
    pub bars: String,
}

/// A section.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct SectionView {
    pub id: u64,
    pub name: String,
    pub start: f64,
    pub end: f64,
    pub color: String,
}

/// A song of the setlist.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct SongView {
    pub name: String,
    /// Its length.
    pub seconds: f64,
    /// Where it starts.
    pub start: f64,
    /// What happens after it.
    pub then: String,
    pub notes: String,
}

/// The setlist and show mode.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct SetlistView {
    pub songs: Vec<SongView>,
    pub show: bool,
    /// The song stood on or playing (show mode).
    pub current: Option<usize>,
}

/// A line of the lyrics (seconds).
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct LyricView {
    pub start: f64,
    pub end: f64,
    pub text: String,
}

/// A launcher slot.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct SlotView {
    /// "empty", "armed", "clip", "queued", "playing", "stopping",
    /// "recording".
    pub kind: String,
    pub name: String,
    pub color: String,
}

/// A launcher column: a track's slots, a scene each.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct ColumnView {
    pub track: u64,
    pub name: String,
    pub color: String,
    pub playing: bool,
    pub slots: Vec<SlotView>,
}

/// A launcher scene.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct SceneView {
    pub id: u64,
    pub name: String,
    /// "off", "clips", "queued", "playing".
    pub light: String,
}

/// The clip launcher.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct LauncherView {
    pub scenes: Vec<SceneView>,
    pub columns: Vec<ColumnView>,
}

/// A mix scene.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct MixSceneView {
    pub id: u64,
    pub name: String,
    /// Recalled last.
    pub current: bool,
}

/// A macro.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct MacroView {
    pub number: u16,
    pub name: String,
}

/// Everything the remote shows (as the engineer sees it; [`State::for_role`]
/// leaves out what a role does not see).
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct State {
    pub project: String,
    pub role: RoleView,
    pub transport: TransportView,
    /// Fader travel of 0 dB.
    pub unity: f32,
    /// Mixer strips by id, in `order`; the master's id in `master`.
    pub tracks: BTreeMap<String, TrackView>,
    pub order: Vec<u64>,
    pub master: Option<u64>,
    pub cues: Vec<CueView>,
    /// The tracks that feed cue mixes, in order.
    pub cue_order: Vec<u64>,
    pub markers: Vec<MarkerView>,
    pub sections: Vec<SectionView>,
    pub setlist: SetlistView,
    pub lyrics: Vec<LyricView>,
    pub launcher: LauncherView,
    pub mix_scenes: Vec<MixSceneView>,
    pub macros: Vec<MacroView>,
}

impl State {
    /// The state as a device in `role` sees it (`cue`: the performer's cue
    /// mix's id, when the project has it). A performer sees their cue mix
    /// only (with the names and colours of the tracks in it), the
    /// transport, markers, sections, the setlist and the lyrics; a viewer
    /// sees no mixer at all.
    pub fn for_role(&self, role: &Role, cue: Option<u64>) -> Value {
        let mut s = self.clone();
        s.role = RoleView {
            kind: role.kind().into(),
            cue_name: match role {
                Role::Performer { cue } => Some(cue.clone()),
                _ => None,
            },
            cue,
        };
        match role {
            Role::Full => {}
            Role::Performer { .. } => {
                s.cues.retain(|c| Some(c.id) == cue);
                let shown: Vec<u64> = s.cue_order.clone();
                s.tracks = s
                    .tracks
                    .into_iter()
                    .filter(|(_, t)| shown.contains(&t.id))
                    .map(|(k, t)| {
                        (
                            k,
                            TrackView {
                                id: t.id,
                                name: t.name,
                                kind: t.kind,
                                color: t.color,
                                ..TrackView::default()
                            },
                        )
                    })
                    .collect();
                s.order.retain(|id| shown.contains(id));
                s.master = None;
                s.launcher = LauncherView::default();
                s.mix_scenes.clear();
                s.macros.clear();
            }
            Role::Viewer => {
                s.tracks.clear();
                s.order.clear();
                s.master = None;
                s.cues.clear();
                s.cue_order.clear();
                s.launcher = LauncherView::default();
                s.mix_scenes.clear();
                s.macros.clear();
            }
        }
        serde_json::to_value(&s).unwrap_or(Value::Null)
    }
}

/// Meters: peak levels (dBFS, left and right, to half a decibel) by strip
/// or cue mix id.
pub fn meters_message(levels: &[(u64, f32, f32)]) -> String {
    let mut m = serde_json::Map::new();
    for (id, l, r) in levels {
        let q = |v: f32| -> Value {
            let v = (v.max(-90.0) * 2.0).round() / 2.0;
            serde_json::Number::from_f64(f64::from(v)).map_or(Value::Null, Value::Number)
        };
        m.insert(id.to_string(), Value::Array(vec![q(*l), q(*r)]));
    }
    message("meters", serde_json::json!({ "m": m }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn client_messages_read() {
        assert_eq!(
            parse(r#"{"t":"pair","code":"123456","name":"Phone"}"#).unwrap(),
            ClientMessage::Pair {
                code: "123456".into(),
                name: "Phone".into()
            }
        );
        assert_eq!(
            parse(r#"{"t":"hello","token":"ab"}"#).unwrap(),
            ClientMessage::Hello { token: "ab".into() }
        );
        assert_eq!(
            parse(r#"{"t":"view","screen":"mixer"}"#).unwrap(),
            ClientMessage::View {
                screen: Screen::Mixer
            }
        );
        assert_eq!(
            parse(r#"{"t":"fader","track":12,"travel":0.5}"#).unwrap(),
            ClientMessage::Input(RemoteInput::Fader {
                track: 12,
                travel: 0.5
            })
        );
        assert_eq!(
            parse(r#"{"t":"transport","button":"play_stop"}"#).unwrap(),
            ClientMessage::Input(RemoteInput::Transport {
                button: TransportButton::PlayStop
            })
        );
        assert_eq!(
            parse(r#"{"t":"stop_clips"}"#).unwrap(),
            ClientMessage::Input(RemoteInput::StopClips)
        );
        for bad in [
            "",
            "[]",
            r#"{"t":"fader","track":"x","travel":0.5}"#,
            r#"{"t":"explode"}"#,
            r#"{"t":"pair"}"#,
        ] {
            assert!(parse(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn roles_allow_what_they_should() {
        let fader = RemoteInput::Fader {
            track: 1,
            travel: 0.5,
        };
        let mine = RemoteInput::CueSend {
            cue: 7,
            track: 1,
            travel: 0.5,
        };
        let theirs = RemoteInput::CueSend {
            cue: 8,
            track: 1,
            travel: 0.5,
        };
        let touch = RemoteInput::Touch { on: true };
        let performer = Role::Performer {
            cue: "Drums".into(),
        };
        assert!(fader.allowed(&Role::Full, None));
        assert!(theirs.allowed(&Role::Full, None));
        assert!(!fader.allowed(&performer, Some(7)));
        assert!(mine.allowed(&performer, Some(7)));
        assert!(!theirs.allowed(&performer, Some(7)));
        assert!(!mine.allowed(&performer, None));
        assert!(touch.allowed(&performer, Some(7)));
        assert!(!touch.allowed(&Role::Viewer, None));
        assert!(!mine.allowed(&Role::Viewer, None));
    }

    fn sample() -> State {
        let track = |id: u64, name: &str| TrackView {
            id,
            name: name.into(),
            kind: "audio".into(),
            color: "#112233".into(),
            fader: 0.7,
            level: "0.0".into(),
            mute: true,
            ..TrackView::default()
        };
        let mut tracks = BTreeMap::new();
        tracks.insert("1".into(), track(1, "Drums"));
        tracks.insert("2".into(), track(2, "Bass"));
        tracks.insert("9".into(), track(9, "Master"));
        let cue = |id: u64, name: &str| CueView {
            id,
            name: name.into(),
            sends: [("1".to_string(), CueSendView::default())].into(),
            ..CueView::default()
        };
        State {
            project: "Song".into(),
            tracks,
            order: vec![1, 2, 9],
            master: Some(9),
            cues: vec![cue(20, "Drums"), cue(21, "Vox")],
            cue_order: vec![1, 2],
            lyrics: vec![LyricView {
                start: 1.0,
                end: 2.0,
                text: "la".into(),
            }],
            macros: vec![MacroView {
                number: 1,
                name: "x".into(),
            }],
            ..State::default()
        }
    }

    #[test]
    fn a_performer_sees_only_their_cue_mix() {
        let s = sample();
        let full = s.for_role(&Role::Full, None);
        assert_eq!(full["cues"].as_array().unwrap().len(), 2);
        assert_eq!(full["role"]["kind"], json!("full"));
        let p = s.for_role(&Role::Performer { cue: "Vox".into() }, Some(21));
        assert_eq!(
            p["role"],
            json!({"kind": "performer", "cue_name": "Vox", "cue": 21})
        );
        let cues = p["cues"].as_array().unwrap();
        assert_eq!(cues.len(), 1);
        assert_eq!(cues[0]["name"], json!("Vox"));
        // The tracks in the cue mixes, by name only; not the master.
        assert_eq!(p["order"], json!([1, 2]));
        assert_eq!(p["tracks"]["1"]["name"], json!("Drums"));
        assert_eq!(p["tracks"]["1"]["mute"], json!(false));
        assert!(p["tracks"].get("9").is_none());
        assert_eq!(p["master"], json!(null));
        assert_eq!(p["macros"], json!([]));
        assert_eq!(p["lyrics"][0]["text"], json!("la"));
        // Their cue mix is not in this project.
        let gone = s.for_role(&Role::Performer { cue: "Keys".into() }, None);
        assert_eq!(gone["cues"], json!([]));
        let v = s.for_role(&Role::Viewer, None);
        assert_eq!(v["tracks"], json!({}));
        assert_eq!(v["cues"], json!([]));
        assert_eq!(v["lyrics"][0]["text"], json!("la"));
    }

    #[test]
    fn meters_are_rounded_and_floored() {
        let m = meters_message(&[(3, -3.24, f32::NEG_INFINITY)]);
        let v: Value = serde_json::from_str(&m).unwrap();
        assert_eq!(v["t"], json!("meters"));
        assert_eq!(v["m"]["3"], json!([-3.0, -90.0]));
    }
}
