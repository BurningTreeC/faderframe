//! Grid controllers playing the clip launcher: 8 × 8 pads (the slots of
//! eight tracks by eight scenes), scene launch buttons beside them, stop
//! buttons under them where the controller has a row for it, arrows (or
//! Shift and the track buttons) to move over tracks and scenes, the faders
//! of the APCs on the shown tracks' volumes.
//!
//! | | pads | scenes | stop | moves | modes |
//! |---|---|---|---|---|---|
//! | Launchpad Mini MK3, X, Pro MK3 (programmer mode) | notes 11–88 | CC 89…19 | an empty slot | CC 91–94 | Session (95): back to the arrangement; 96: stop all |
//! | APC mini | notes 0–63 | notes 82–89 | notes 64–71 | Shift (98) + 64–67 | Shift + 89: stop all |
//! | APC mini mk2 | notes 0–63 | notes 112–119 | notes 100–107 | Shift (122) + 100–103 | Shift + 119: stop all |
//! | Push 2 (user mode) | notes 36–99 | CC 43…36 | CC 20–27 | CC 44–47 | Stop Clip (29), Session (51), Play, Record |
//!
//! Lights: a clip in its colour (the Launchpads by RGB, the APC mini mk2
//! by the nearest palette colour, Push 2 by palette entries set to the
//! clips' colours; the first APC mini in yellow), green pulsing while it
//! plays, flashing while it waits to start or stop, red pulsing while it
//! records, dim red on an armed track's empty slots. Only what changed is
//! sent.

use crate::{
    Button, MASTER, Protocol, SceneLight, SlotKind, SlotState, SurfaceInput, SurfaceState,
};
use std::collections::HashMap;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum GridModel {
    LaunchpadMiniMk3,
    LaunchpadX,
    LaunchpadProMk3,
    ApcMini,
    ApcMiniMk2,
    Push2,
}

impl GridModel {
    fn launchpad_id(self) -> Option<u8> {
        match self {
            GridModel::LaunchpadMiniMk3 => Some(0x0D),
            GridModel::LaunchpadX => Some(0x0C),
            GridModel::LaunchpadProMk3 => Some(0x0E),
            _ => None,
        }
    }
}

/// The 128-colour palette the Launchpads, the APC mini mk2 (and many
/// others) share.
pub const PALETTE: [u32; 128] = [
    0x000000, 0x1E1E1E, 0x7F7F7F, 0xFFFFFF, 0xFF4C4C, 0xFF0000, 0x590000, 0x190000, 0xFFBD6C,
    0xFF5400, 0x591D00, 0x271B00, 0xFFFF4C, 0xFFFF00, 0x595900, 0x191900, 0x88FF4C, 0x54FF00,
    0x1D5900, 0x142B00, 0x4CFF4C, 0x00FF00, 0x005900, 0x001900, 0x4CFF5E, 0x00FF19, 0x00590D,
    0x001902, 0x4CFF88, 0x00FF55, 0x00591D, 0x001F12, 0x4CFFB7, 0x00FF99, 0x005935, 0x001912,
    0x4CC3FF, 0x00A9FF, 0x004152, 0x001019, 0x4C88FF, 0x0055FF, 0x001D59, 0x000819, 0x4C4CFF,
    0x0000FF, 0x000059, 0x000019, 0x874CFF, 0x5400FF, 0x190064, 0x0F0030, 0xFF4CFF, 0xFF00FF,
    0x590059, 0x190019, 0xFF4C87, 0xFF0054, 0x59001D, 0x220013, 0xFF1500, 0x993500, 0x795100,
    0x436400, 0x033900, 0x005735, 0x00547F, 0x0000FF, 0x00454F, 0x2500CC, 0x7F7F7F, 0x202020,
    0xBDFF2D, 0xAFED06, 0x64FF09, 0x108B00, 0x00FF87, 0x00A9FF, 0x002AFF, 0x3F00FF, 0x7A00FF,
    0xB21A7D, 0x402100, 0xFF4A00, 0x88E106, 0x72FF15, 0x00FF00, 0x3BFF26, 0x59FF71, 0x38FFCC,
    0x5B8AFF, 0x3151C6, 0x877FE9, 0xD31DFF, 0xFF005D, 0xFF7F00, 0xB9B000, 0x90FF00, 0x835D07,
    0x392B00, 0x144C10, 0x0D5038, 0x15152A, 0x16205A, 0x693C1C, 0xA8000A, 0xDE513D, 0xD86A1C,
    0xFFE126, 0x9EE12F, 0x67B50F, 0x1E1E30, 0xDCFF6B, 0x80FFBD, 0x9A99FF, 0x8E66FF, 0x404040,
    0x757575, 0xE0FFFF, 0xA00000, 0x350000, 0x1AD000, 0x074200, 0xB9B000, 0x3F3100, 0x3F3100,
    0xB35F00, 0x4B1502,
];

/// The palette entry nearest a colour (black only for black).
pub fn nearest(c: [u8; 3]) -> u8 {
    if c.iter().all(|v| *v < 8) {
        return 0;
    }
    let rgb = |e: u32| [(e >> 16) as i32, ((e >> 8) & 255) as i32, (e & 255) as i32];
    let [r, g, b] = c.map(i32::from);
    (1..128u8)
        .min_by_key(|&i| {
            let [pr, pg, pb] = rgb(PALETTE[usize::from(i)]);
            // Weighted for the eye.
            2 * (r - pr).pow(2) + 4 * (g - pg).pow(2) + 3 * (b - pb).pow(2)
        })
        .unwrap_or(0)
}

const GREEN: [u8; 3] = [0, 255, 0];
const RED: [u8; 3] = [255, 0, 0];
const WHITE: [u8; 3] = [255, 255, 255];
const AMBER: [u8; 3] = [255, 120, 0];

/// What a light shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Look {
    Off,
    Color([u8; 3]),
    Dim([u8; 3]),
    Pulse([u8; 3]),
    Flash([u8; 3]),
}

fn slot_look(s: Option<&SlotState>) -> Look {
    let Some(s) = s else { return Look::Off };
    match s.kind {
        SlotKind::Empty => Look::Off,
        SlotKind::Armed => Look::Dim(RED),
        SlotKind::Clip => Look::Color(s.color),
        SlotKind::Queued => Look::Flash(s.color),
        SlotKind::Playing => Look::Pulse(GREEN),
        SlotKind::Stopping => Look::Flash(GREEN),
        SlotKind::Recording => Look::Pulse(RED),
    }
}

fn scene_look(l: SceneLight) -> Look {
    match l {
        SceneLight::Off => Look::Off,
        SceneLight::Clips => Look::Dim(WHITE),
        SceneLight::Queued => Look::Flash(GREEN),
        SceneLight::Playing => Look::Color(GREEN),
    }
}

/// A light of the controller.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum Light {
    Pad(usize, usize),
    Scene(usize),
    Stop(usize),
    Button(u8),
}

/// A grid controller (see the module docs).
pub struct Grid {
    model: GridModel,
    shift: bool,
    started: bool,
    sent: HashMap<Light, Look>,
    /// Push 2: the palette entries set (pad entries 1–64).
    palette: HashMap<u8, [u8; 3]>,
}

impl Grid {
    pub fn new(model: GridModel) -> Self {
        Self {
            model,
            shift: false,
            started: false,
            sent: HashMap::new(),
            palette: HashMap::new(),
        }
    }

    pub fn model(&self) -> GridModel {
        self.model
    }

    /// The mode it is put in (programmer, user, session).
    fn hello(&self) -> Vec<Vec<u8>> {
        match self.model {
            m if m.launchpad_id().is_some() => {
                let id = m.launchpad_id().unwrap_or(0x0D);
                vec![vec![0xF0, 0x00, 0x20, 0x29, 0x02, id, 0x0E, 0x01, 0xF7]]
            }
            GridModel::ApcMiniMk2 => {
                vec![vec![0xF0, 0x47, 0x7F, 0x4F, 0x62, 0x00, 0x01, 0x00, 0xF7]]
            }
            GridModel::Push2 => vec![vec![0xF0, 0x00, 0x21, 0x1D, 0x01, 0x01, 0x0A, 0x01, 0xF7]],
            _ => Vec::new(),
        }
    }

    fn pad_of(&self, note: u8) -> Option<(usize, usize)> {
        let n = usize::from(note);
        match self.model {
            m if m.launchpad_id().is_some() => {
                let (row, col) = (n / 10, n % 10);
                ((1..=8).contains(&row) && (1..=8).contains(&col)).then(|| (col - 1, 8 - row))
            }
            GridModel::ApcMini | GridModel::ApcMiniMk2 => (n < 64).then(|| (n % 8, 7 - n / 8)),
            GridModel::Push2 => (36..100)
                .contains(&n)
                .then(|| ((n - 36) % 8, 7 - (n - 36) / 8)),
            _ => None,
        }
    }

    fn pad_note(&self, strip: usize, row: usize) -> u8 {
        let (s, r) = (strip as u8, row as u8);
        match self.model {
            m if m.launchpad_id().is_some() => 10 * (8 - r) + s + 1,
            GridModel::Push2 => 36 + (7 - r) * 8 + s,
            _ => (7 - r) * 8 + s,
        }
    }

    fn pad(&self, strip: usize, row: usize, pressed: bool, out: &mut Vec<SurfaceInput>) {
        out.push(if pressed {
            SurfaceInput::LaunchClip { strip, scene: row }
        } else {
            SurfaceInput::ReleaseClip { strip, scene: row }
        });
    }

    fn button(button: Button, pressed: bool, out: &mut Vec<SurfaceInput>) {
        out.push(SurfaceInput::Button { button, pressed });
    }

    /// A move (or mode) button of the controller.
    fn moves(code: u8, pressed: bool, out: &mut Vec<SurfaceInput>) -> bool {
        let button = match code {
            0 => Button::SceneUp,
            1 => Button::SceneDown,
            2 => Button::ChannelLeft,
            3 => Button::ChannelRight,
            _ => return false,
        };
        Self::button(button, pressed, out);
        true
    }

    fn receive_launchpad(&mut self, msg: &[u8], out: &mut Vec<SurfaceInput>) {
        match *msg {
            [s, note, v] if s & 0xF0 == 0x90 || s & 0xF0 == 0x80 => {
                let pressed = s & 0xF0 == 0x90 && v > 0;
                if let Some((strip, row)) = self.pad_of(note) {
                    self.pad(strip, row, pressed, out);
                }
            }
            [s, cc, v] if s & 0xF0 == 0xB0 => {
                let pressed = v > 0;
                match cc {
                    91..=94 => {
                        Self::moves(cc - 91, pressed, out);
                    }
                    95 if pressed => out.push(SurfaceInput::BackToArrangement),
                    96 if pressed => out.push(SurfaceInput::StopClips),
                    19..=89 if cc % 10 == 9 && pressed => {
                        out.push(SurfaceInput::LaunchScene(usize::from((89 - cc) / 10)));
                    }
                    _ => {}
                }
            }
            _ => {}
        }
    }

    fn receive_apc(&mut self, msg: &[u8], out: &mut Vec<SurfaceInput>) {
        let mk2 = self.model == GridModel::ApcMiniMk2;
        let (tracks, scenes, shift) = if mk2 {
            (100u8, 112u8, 122u8)
        } else {
            (64, 82, 98)
        };
        match *msg {
            [s, note, v] if s & 0xF0 == 0x90 || s & 0xF0 == 0x80 => {
                let pressed = s & 0xF0 == 0x90 && v > 0;
                if note == shift {
                    self.shift = pressed;
                } else if let Some((strip, row)) = self.pad_of(note) {
                    self.pad(strip, row, pressed, out);
                } else if (tracks..tracks + 8).contains(&note) {
                    let i = note - tracks;
                    if self.shift {
                        Self::moves(i, pressed, out);
                    } else if pressed {
                        out.push(SurfaceInput::StopTrack(usize::from(i)));
                    }
                } else if (scenes..scenes + 8).contains(&note) && pressed {
                    let i = note - scenes;
                    if self.shift && i == 7 {
                        out.push(SurfaceInput::StopClips);
                    } else {
                        out.push(SurfaceInput::LaunchScene(usize::from(i)));
                    }
                }
            }
            [s, cc @ 48..=56, v] if s & 0xF0 == 0xB0 => {
                let strip = if cc == 56 {
                    MASTER
                } else {
                    usize::from(cc - 48)
                };
                out.push(SurfaceInput::Fader {
                    strip,
                    travel: f32::from(v) / 127.0,
                });
            }
            _ => {}
        }
    }

    fn receive_push(&mut self, msg: &[u8], out: &mut Vec<SurfaceInput>) {
        match *msg {
            [s, note, v] if s & 0xF0 == 0x90 || s & 0xF0 == 0x80 => {
                let pressed = s & 0xF0 == 0x90 && v > 0;
                if let Some((strip, row)) = self.pad_of(note) {
                    self.pad(strip, row, pressed, out);
                }
            }
            [s, cc, v] if s & 0xF0 == 0xB0 => {
                let pressed = v > 0;
                match cc {
                    36..=43 if pressed => out.push(SurfaceInput::LaunchScene(usize::from(43 - cc))),
                    20..=27 if pressed => out.push(SurfaceInput::StopTrack(usize::from(cc - 20))),
                    44 => {
                        Self::moves(2, pressed, out);
                    }
                    45 => {
                        Self::moves(3, pressed, out);
                    }
                    46 => {
                        Self::moves(0, pressed, out);
                    }
                    47 => {
                        Self::moves(1, pressed, out);
                    }
                    49 => self.shift = pressed,
                    29 if pressed => out.push(SurfaceInput::StopClips),
                    51 if pressed => out.push(SurfaceInput::BackToArrangement),
                    85 => Self::button(Button::Play, pressed, out),
                    86 => Self::button(Button::Record, pressed, out),
                    _ => {}
                }
            }
            _ => {}
        }
    }

    /// What every light should show now.
    fn looks(&self, state: &SurfaceState) -> Vec<(Light, Look)> {
        let l = &state.launcher;
        let mut out = Vec::with_capacity(96);
        for strip in 0..8 {
            for row in 0..8 {
                out.push((Light::Pad(strip, row), slot_look(l.slot(strip, row))));
            }
            let playing = l.playing.get(strip).copied().unwrap_or(false);
            out.push((
                Light::Stop(strip),
                if playing { Look::Color(RED) } else { Look::Off },
            ));
        }
        for row in 0..8 {
            out.push((
                Light::Scene(row),
                scene_look(l.scenes.get(row).copied().unwrap_or_default()),
            ));
        }
        if self.model.launchpad_id().is_some() {
            for cc in 91..=94 {
                out.push((Light::Button(cc), Look::Dim(WHITE)));
            }
            out.push((Light::Button(95), Look::Color(AMBER)));
            out.push((Light::Button(96), Look::Color(RED)));
        }
        out
    }

    fn update_launchpad(&mut self, changed: &[(Light, Look)], out: &mut Vec<Vec<u8>>) {
        let id = self.model.launchpad_id().unwrap_or(0x0D);
        let mut msg = vec![0xF0, 0x00, 0x20, 0x29, 0x02, id, 0x03];
        let seven = |c: [u8; 3], shift: u8| c.map(|v| v >> shift);
        for &(light, look) in changed {
            let led = match light {
                Light::Pad(s, r) => self.pad_note(s, r),
                Light::Scene(r) => 89 - 10 * r as u8,
                Light::Button(cc) => cc,
                Light::Stop(_) => continue,
            };
            match look {
                Look::Off => msg.extend([0, led, 0]),
                Look::Color(c) => {
                    let [r, g, b] = seven(c, 1);
                    msg.extend([3, led, r, g, b]);
                }
                Look::Dim(c) => {
                    let [r, g, b] = seven(c, 3);
                    msg.extend([3, led, r, g, b]);
                }
                Look::Pulse(c) => msg.extend([2, led, nearest(c)]),
                Look::Flash(c) => msg.extend([1, led, 0, nearest(c)]),
            }
        }
        if msg.len() > 7 {
            msg.push(0xF7);
            out.push(msg);
        }
    }

    fn update_apc(&mut self, changed: &[(Light, Look)], out: &mut Vec<Vec<u8>>) {
        let mk2 = self.model == GridModel::ApcMiniMk2;
        let (tracks, scenes) = if mk2 { (100u8, 112u8) } else { (64, 82) };
        for &(light, look) in changed {
            let single = |look: Look| match look {
                Look::Off => 0,
                Look::Flash(_) => 2,
                _ => 1,
            };
            let msg = match light {
                Light::Pad(s, r) => {
                    let note = self.pad_note(s, r);
                    if mk2 {
                        let (ch, v) = match look {
                            Look::Off => (0, 0),
                            Look::Color(c) => (6, nearest(c)),
                            Look::Dim(c) => (1, nearest(c)),
                            Look::Pulse(c) => (10, nearest(c)),
                            Look::Flash(c) => (14, nearest(c)),
                        };
                        vec![0x90 | ch, note, v]
                    } else {
                        // Green, red, yellow (each steady or blinking).
                        let v = match look {
                            Look::Off => 0,
                            Look::Dim(_) => 0,
                            Look::Color(_) => 5,
                            Look::Pulse(c) if c == RED => 3,
                            Look::Pulse(_) => 1,
                            Look::Flash(c) if c == GREEN => 6,
                            Look::Flash(_) => 2,
                        };
                        vec![0x90, note, v]
                    }
                }
                Light::Stop(s) => vec![0x90, tracks + s as u8, single(look)],
                Light::Scene(r) => vec![0x90, scenes + r as u8, single(look)],
                Light::Button(_) => continue,
            };
            out.push(msg);
        }
    }

    fn update_push(&mut self, changed: &[(Light, Look)], out: &mut Vec<Vec<u8>>) {
        // Pads in their clips' colours: palette entry 1 + pad.
        let mut entries: Vec<(u8, [u8; 3])> = Vec::new();
        let mut notes = Vec::new();
        for &(light, look) in changed {
            match light {
                Light::Pad(s, r) => {
                    let note = self.pad_note(s, r);
                    let entry = 1 + (r * 8 + s) as u8;
                    let (ch, v) = match look {
                        Look::Off => (0, 0),
                        Look::Pulse(c) => (8, if c == RED { 127 } else { 126 }),
                        Look::Color(c) | Look::Dim(c) | Look::Flash(c) => {
                            let c = if matches!(look, Look::Dim(_)) {
                                c.map(|v| v / 4)
                            } else {
                                c
                            };
                            if self.palette.get(&entry) != Some(&c) {
                                entries.push((entry, c));
                            }
                            (
                                if matches!(look, Look::Flash(_)) {
                                    13
                                } else {
                                    0
                                },
                                entry,
                            )
                        }
                    };
                    notes.push(vec![0x90 | ch, note, v]);
                }
                Light::Scene(r) => {
                    let (ch, v) = match look {
                        Look::Off => (0, 0),
                        Look::Flash(_) => (13, 126),
                        Look::Color(_) => (0, 126),
                        _ => (0, 122),
                    };
                    notes.push(vec![0xB0 | ch, 43 - r as u8, v]);
                }
                Light::Stop(s) => {
                    let v = if look == Look::Off { 0 } else { 127 };
                    notes.push(vec![0xB0, 20 + s as u8, v]);
                }
                Light::Button(_) => {}
            }
        }
        if !entries.is_empty() {
            for (i, c) in entries {
                let w = c.into_iter().max().unwrap_or(0);
                let split = |v: u8| [v & 0x7F, v >> 7];
                let mut m = vec![0xF0, 0x00, 0x21, 0x1D, 0x01, 0x01, 0x03, i];
                for v in [c[0], c[1], c[2], w] {
                    m.extend(split(v));
                }
                m.push(0xF7);
                out.push(m);
                self.palette.insert(i, c);
            }
            out.push(vec![0xF0, 0x00, 0x21, 0x1D, 0x01, 0x01, 0x05, 0xF7]);
        }
        out.extend(notes);
    }
}

impl Protocol for Grid {
    fn strips(&self) -> usize {
        8
    }

    fn scenes(&self) -> usize {
        8
    }

    fn launcher_columns(&self) -> bool {
        true
    }

    fn receive(&mut self, msg: &[u8], out: &mut Vec<SurfaceInput>) {
        match self.model {
            m if m.launchpad_id().is_some() => self.receive_launchpad(msg, out),
            GridModel::ApcMini | GridModel::ApcMiniMk2 => self.receive_apc(msg, out),
            _ => self.receive_push(msg, out),
        }
    }

    fn update(&mut self, state: &SurfaceState, _now: f64, out: &mut Vec<Vec<u8>>) {
        if !self.started {
            out.extend(self.hello());
            self.started = true;
        }
        let changed: Vec<(Light, Look)> = self
            .looks(state)
            .into_iter()
            .filter(|(light, look)| self.sent.get(light) != Some(look))
            .collect();
        if changed.is_empty() {
            return;
        }
        match self.model {
            m if m.launchpad_id().is_some() => self.update_launchpad(&changed, out),
            GridModel::ApcMini | GridModel::ApcMiniMk2 => self.update_apc(&changed, out),
            _ => self.update_push(&changed, out),
        }
        self.sent.extend(changed);
    }

    fn reset(&mut self) {
        self.sent.clear();
        self.palette.clear();
        self.started = false;
        self.shift = false;
    }

    fn goodbye(&self) -> Vec<Vec<u8>> {
        match self.model {
            m if m.launchpad_id().is_some() => {
                let id = m.launchpad_id().unwrap_or(0x0D);
                vec![vec![0xF0, 0x00, 0x20, 0x29, 0x02, id, 0x0E, 0x00, 0xF7]]
            }
            GridModel::Push2 => vec![vec![0xF0, 0x00, 0x21, 0x1D, 0x01, 0x01, 0x0A, 0x00, 0xF7]],
            _ => Vec::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::LauncherView;

    fn state() -> SurfaceState {
        let slot = |kind, color| SlotState {
            kind,
            color,
            name: String::new(),
        };
        let mut slots = vec![vec![SlotState::default(); 8]; 8];
        slots[0][0] = slot(SlotKind::Clip, [200, 40, 40]);
        slots[1][0] = slot(SlotKind::Playing, [0, 0, 0]);
        slots[2][7] = slot(SlotKind::Queued, [40, 40, 220]);
        SurfaceState {
            launcher: LauncherView {
                first_scene: 0,
                scene_count: 8,
                scene_names: vec!["A".into(); 8],
                scenes: vec![SceneLight::Clips; 8],
                slots,
                playing: vec![false, true, false, false, false, false, false, false],
            },
            ..SurfaceState::default()
        }
    }

    #[test]
    fn the_palette_finds_colours() {
        assert_eq!(nearest([255, 0, 0]), 5);
        assert_eq!(nearest([0, 255, 0]), 21);
        assert_eq!(nearest([0, 0, 0]), 0);
        assert_eq!(nearest([255, 255, 255]), 3);
    }

    #[test]
    fn a_launchpad_lights_its_pads_and_sends_its_presses() {
        let mut g = Grid::new(GridModel::LaunchpadMiniMk3);
        let mut out = Vec::new();
        g.update(&state(), 0.0, &mut out);
        // Programmer mode, then one SysEx with every light.
        assert_eq!(
            out[0],
            vec![0xF0, 0x00, 0x20, 0x29, 0x02, 0x0D, 0x0E, 0x01, 0xF7]
        );
        let lights = &out[1];
        assert_eq!(&lights[..7], &[0xF0, 0x00, 0x20, 0x29, 0x02, 0x0D, 0x03]);
        // Top-left pad (note 81) in the clip's colour (7 bit).
        let at = lights.windows(5).position(|w| w == [3, 81, 100, 20, 20]);
        assert!(at.is_some(), "{lights:?}");
        // Its neighbour pulses green.
        assert!(lights.windows(3).any(|w| w == [2, 82, 21]));
        // Unchanged: nothing more.
        let mut again = Vec::new();
        g.update(&state(), 0.1, &mut again);
        assert!(again.is_empty());
        let mut inputs = Vec::new();
        g.receive(&[0x90, 81, 127], &mut inputs);
        g.receive(&[0x90, 81, 0], &mut inputs);
        g.receive(&[0xB0, 89, 127], &mut inputs);
        g.receive(&[0xB0, 91, 127], &mut inputs);
        assert_eq!(
            inputs,
            vec![
                SurfaceInput::LaunchClip { strip: 0, scene: 0 },
                SurfaceInput::ReleaseClip { strip: 0, scene: 0 },
                SurfaceInput::LaunchScene(0),
                SurfaceInput::Button {
                    button: Button::SceneUp,
                    pressed: true
                },
            ]
        );
    }

    #[test]
    fn apc_minis_use_their_buttons_and_faders() {
        for (model, tracks, scenes, shift) in [
            (GridModel::ApcMini, 64u8, 82u8, 98u8),
            (GridModel::ApcMiniMk2, 100, 112, 122),
        ] {
            let mut g = Grid::new(model);
            let mut out = Vec::new();
            g.update(&state(), 0.0, &mut out);
            // The top-left pad is note 56; the second track's stop lit.
            assert!(
                out.iter().any(|m| m[1] == 56 && m[2] != 0),
                "{model:?} {out:?}"
            );
            assert!(out.contains(&vec![0x90, tracks + 1, 1]));
            let mut inputs = Vec::new();
            g.receive(&[0x90, 0, 127], &mut inputs);
            g.receive(&[0x80, 0, 0], &mut inputs);
            g.receive(&[0x90, tracks + 2, 127], &mut inputs);
            g.receive(&[0x90, scenes, 127], &mut inputs);
            g.receive(&[0x90, shift, 127], &mut inputs);
            g.receive(&[0x90, tracks, 127], &mut inputs);
            g.receive(&[0x90, scenes + 7, 127], &mut inputs);
            g.receive(&[0x80, shift, 0], &mut inputs);
            g.receive(&[0xB0, 50, 64], &mut inputs);
            assert_eq!(
                inputs,
                vec![
                    SurfaceInput::LaunchClip { strip: 0, scene: 7 },
                    SurfaceInput::ReleaseClip { strip: 0, scene: 7 },
                    SurfaceInput::StopTrack(2),
                    SurfaceInput::LaunchScene(0),
                    SurfaceInput::Button {
                        button: Button::SceneUp,
                        pressed: true
                    },
                    SurfaceInput::StopClips,
                    SurfaceInput::Fader {
                        strip: 2,
                        travel: 64.0 / 127.0
                    },
                ],
                "{model:?}"
            );
        }
    }

    #[test]
    fn push_sets_palette_entries_for_clip_colours() {
        let mut g = Grid::new(GridModel::Push2);
        let mut out = Vec::new();
        g.update(&state(), 0.0, &mut out);
        assert_eq!(
            out[0],
            vec![0xF0, 0x00, 0x21, 0x1D, 0x01, 0x01, 0x0A, 0x01, 0xF7]
        );
        // Entry 1 (the top-left pad) set to the clip's colour, the palette
        // reapplied, the pad (note 92) lit with it.
        assert!(out.contains(&vec![
            0xF0, 0x00, 0x21, 0x1D, 0x01, 0x01, 0x03, 1, 72, 1, 40, 0, 40, 0, 72, 1, 0xF7
        ]));
        assert!(out.contains(&vec![0xF0, 0x00, 0x21, 0x1D, 0x01, 0x01, 0x05, 0xF7]));
        assert!(out.contains(&vec![0x90, 92, 1]));
        assert!(out.contains(&vec![0x98, 93, 126]), "playing pulses green");
        let mut inputs = Vec::new();
        g.receive(&[0x90, 36, 100], &mut inputs);
        g.receive(&[0xB0, 43, 127], &mut inputs);
        g.receive(&[0xB0, 21, 127], &mut inputs);
        assert_eq!(
            inputs,
            vec![
                SurfaceInput::LaunchClip { strip: 0, scene: 7 },
                SurfaceInput::LaunchScene(0),
                SurfaceInput::StopTrack(1),
            ]
        );
    }
}
