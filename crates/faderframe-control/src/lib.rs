//! Control surfaces: the protocols, without I/O.
//!
//! A surface shows a bank of channel strips (fader, pan pot, mute, solo,
//! record arm, select, name, meter), the master fader, the transport and
//! the song position, and sends what is done on it. The session builds a
//! [`SurfaceState`] of what should be shown (every tick) and hands it to a
//! [`Protocol`], which sends only what changed; what arrives from the
//! surface becomes [`SurfaceInput`]s for the session to act on.
//!
//! Protocols: [`mackie::Mackie`] (Mackie Control Universal and its many
//! clones: notes for buttons and LEDs, pitch bend for faders, SysEx for the
//! LCD), [`hui::Hui`] (zone/port switches, 14-bit faders over two CCs, a
//! ping each second) — both MIDI — and [`osc::OscSurface`] (FaderFrame's
//! own OSC address space over UDP, see [`osc`]). Grid controllers
//! ([`grid::Grid`]: Launchpad Mini MK3 / X / Pro MK3, APC mini and mk2,
//! Push 2) play the clip launcher: their pads are its slots, their
//! columns the tracks that hold clips (a bank of their own), lit in the
//! clips' colours as the [`LauncherView`] says.

#![forbid(unsafe_code)]

pub mod grid;
pub mod hui;
pub mod mackie;
pub mod osc;

/// The master fader's strip number in [`SurfaceInput`]s.
pub const MASTER: usize = usize::MAX;

/// What is shown on a channel strip.
#[derive(Clone, Debug, PartialEq)]
pub struct StripState {
    /// A track is there (an empty strip shows nothing).
    pub present: bool,
    pub name: String,
    /// Fader travel (0…1, the console law).
    pub fader: f32,
    /// The level as text ("-3.0", "-inf").
    pub level: String,
    /// -1 (left) … 1 (right).
    pub pan: f32,
    /// What the pot's ring shows (0…1): the pan, a send level, or (flipped)
    /// the volume.
    pub pot: f32,
    /// The ring is a dot from the middle (pan) rather than a bar from the
    /// left (levels).
    pub pot_bipolar: bool,
    pub mute: bool,
    pub solo: bool,
    pub arm: bool,
    pub selected: bool,
    /// Peak level (dBFS) since the last state.
    pub meter_db: f32,
}

impl Default for StripState {
    fn default() -> Self {
        Self {
            present: false,
            name: String::new(),
            fader: 0.0,
            level: String::new(),
            pan: 0.0,
            pot: 0.5,
            pot_bipolar: true,
            mute: false,
            solo: false,
            arm: false,
            selected: false,
            meter_db: f32::NEG_INFINITY,
        }
    }
}

/// The song position as bars, beats, sixteenths and ticks (1-based like
/// the transport display).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Position {
    pub bar: i32,
    pub beat: i32,
    pub sixteenth: i32,
    pub tick: i32,
}

/// Everything a surface shows.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SurfaceState {
    /// The bank (as many strips as the surface has).
    pub strips: Vec<StripState>,
    /// The master fader's travel (no master: `None`).
    pub master: Option<f32>,
    pub playing: bool,
    pub recording: bool,
    pub looping: bool,
    pub click: bool,
    pub position: Position,
    /// The first strip's track number (1-based).
    pub first_track: usize,
    /// What the pots do (and the faders, flipped).
    pub page: Page,
    /// Faders and pots swapped.
    pub flip: bool,
    /// The selected track's automation mode.
    pub automation: Option<AutomationButton>,
    /// The launcher's slots of the shown strips (for surfaces that show
    /// it).
    pub launcher: LauncherView,
}

/// What a launcher slot shows.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SlotKind {
    #[default]
    Empty,
    /// Empty, on a track armed to record into it.
    Armed,
    /// A clip, not playing.
    Clip,
    /// Launched, waiting for its launch position.
    Queued,
    Playing,
    /// Playing, stopping at the next launch position.
    Stopping,
    /// Being recorded into.
    Recording,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SlotState {
    pub kind: SlotKind,
    /// The clip's colour.
    pub color: [u8; 3],
    pub name: String,
}

/// What a scene's launch button shows.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SceneLight {
    /// No scene (or no clips in it).
    #[default]
    Off,
    /// It has clips.
    Clips,
    /// One of its clips is waiting to start.
    Queued,
    /// Its clips play.
    Playing,
}

/// The launcher as a surface shows it: the strips' tracks by `rows` scenes
/// from `first_scene`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct LauncherView {
    pub first_scene: usize,
    /// Scenes in all.
    pub scene_count: usize,
    /// The shown scenes' names (empty: no scene there).
    pub scene_names: Vec<String>,
    pub scenes: Vec<SceneLight>,
    /// Per strip, a slot per shown scene.
    pub slots: Vec<Vec<SlotState>>,
    /// Per strip: a clip plays on its track (its stop button lights).
    pub playing: Vec<bool>,
}

impl LauncherView {
    pub fn slot(&self, strip: usize, row: usize) -> Option<&SlotState> {
        self.slots.get(strip)?.get(row)
    }
}

/// What the pots control.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Page {
    #[default]
    Pan,
    /// A send (0-based).
    Send(usize),
}

impl Page {
    /// Two characters for the assignment display.
    pub fn short(self) -> String {
        match self {
            Page::Pan => "PN".into(),
            Page::Send(n) => format!("S{}", (n + 1).min(9)),
        }
    }
}

/// Automation mode buttons.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum AutomationButton {
    Off,
    Read,
    Touch,
    Latch,
    Write,
}

/// Buttons a surface has.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Button {
    Mute(usize),
    Solo(usize),
    Arm(usize),
    Select(usize),
    /// A strip's pan pot pressed (centres it).
    PotPress(usize),
    Play,
    Stop,
    Record,
    Rewind,
    Forward,
    Loop,
    Click,
    /// The song's start and end.
    Start,
    End,
    BankLeft,
    BankRight,
    ChannelLeft,
    ChannelRight,
    Undo,
    Save,
    Marker,
    /// Faders and pots swap.
    Flip,
    /// The pots control pan.
    PanPage,
    /// The pots control a send (`None`: the next one).
    SendPage(Option<usize>),
    /// The selected track's automation mode.
    Automation(AutomationButton),
    /// The launcher's scene bank (a surface showing it).
    SceneUp,
    SceneDown,
}

/// What a surface sends.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum SurfaceInput {
    /// A fader moved (travel 0…1; strip [`MASTER`] for the master).
    Fader {
        strip: usize,
        travel: f32,
    },
    /// A fader touched or let go.
    Touch {
        strip: usize,
        touched: bool,
    },
    /// A pan pot turned (ticks, clockwise positive).
    Pot {
        strip: usize,
        delta: i32,
    },
    /// The jog wheel turned (ticks).
    Jog(i32),
    Button {
        button: Button,
        pressed: bool,
    },
    /// A pan position set directly (-1…1; OSC).
    Pan {
        strip: usize,
        pan: f32,
    },
    /// Launch a clip-launcher slot (track and scene, 0-based from the
    /// bank's and the scene bank's start).
    LaunchClip {
        strip: usize,
        scene: usize,
    },
    /// The slot's button let go (gate and repeat clips stop).
    ReleaseClip {
        strip: usize,
        scene: usize,
    },
    /// Stop the strip's track's clip.
    StopTrack(usize),
    LaunchScene(usize),
    StopClips,
    BackToArrangement,
    /// Send everything again.
    Refresh,
}

/// A surface protocol (see the crate docs).
pub trait Protocol {
    /// How many channel strips the surface has.
    fn strips(&self) -> usize;
    /// Bytes from the surface (one MIDI message) to inputs.
    fn receive(&mut self, msg: &[u8], out: &mut Vec<SurfaceInput>);
    /// What changed since the last call as messages to send; `now` in
    /// seconds (pings, meter refreshes).
    fn update(&mut self, state: &SurfaceState, now: f64, out: &mut Vec<Vec<u8>>);
    /// Forget what was sent: the next update sends everything.
    fn reset(&mut self);
    /// Rows of launcher slots it shows (0: none).
    fn scenes(&self) -> usize {
        0
    }
    /// Its strips are the launcher's columns (the tracks that hold clips)
    /// with a bank of their own, not the mixer's.
    fn launcher_columns(&self) -> bool {
        false
    }
    /// What it sends when it is closed (back to its own mode).
    fn goodbye(&self) -> Vec<Vec<u8>> {
        Vec::new()
    }
}

/// The meter segment (0…12) of the Mackie/HUI meters for a peak level.
pub fn meter_level(db: f32) -> u8 {
    const STEPS: [(f32, u8); 12] = [
        (0.0, 12),
        (-2.0, 11),
        (-4.0, 10),
        (-6.0, 9),
        (-8.0, 8),
        (-10.0, 7),
        (-14.0, 6),
        (-20.0, 5),
        (-30.0, 4),
        (-40.0, 3),
        (-50.0, 2),
        (-60.0, 1),
    ];
    STEPS
        .iter()
        .find(|(at, _)| db >= *at)
        .map_or(0, |(_, level)| *level)
}

/// `text` cut or padded to `width` characters of 7-bit ASCII (others
/// become '?').
pub fn fit(text: &str, width: usize) -> Vec<u8> {
    let mut out: Vec<u8> = text
        .chars()
        .take(width)
        .map(|c| {
            if c.is_ascii() && !c.is_ascii_control() {
                c as u8
            } else {
                b'?'
            }
        })
        .collect();
    out.resize(width, b' ');
    out
}

/// A short name for a narrow display: vowels after the first letter go
/// first, then it is cut.
pub fn abbreviate(name: &str, width: usize) -> String {
    let name = name.trim();
    if name.chars().count() <= width {
        return name.to_string();
    }
    let mut chars: Vec<char> = name.chars().filter(|c| *c != ' ').collect();
    let mut i = chars.len();
    while chars.len() > width && i > 1 {
        i -= 1;
        if "aeiouAEIOU".contains(chars[i]) {
            chars.remove(i);
        }
    }
    chars.into_iter().take(width).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn meters_and_text() {
        assert_eq!(meter_level(3.0), 12);
        assert_eq!(meter_level(-5.0), 9);
        assert_eq!(meter_level(-70.0), 0);
        assert_eq!(meter_level(f32::NEG_INFINITY), 0);
        assert_eq!(fit("Bässe", 7), b"B?sse  ");
        assert_eq!(abbreviate("Lead Synth", 6), "LdSynt");
        assert_eq!(abbreviate("Drums", 6), "Drums");
    }
}
