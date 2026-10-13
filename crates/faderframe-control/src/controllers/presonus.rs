//! PreSonus FaderPort 8, FaderPort 16 and FaderPort (2018) in their native
//! ("Studio One") mode: motorised faders (pitch bend), touch, buttons and
//! their lights as notes on channel 1 (the select buttons in RGB), the
//! scribble strips, value bars and meters; the host keeps the mode alive
//! with `A0 00 00` every second. Choose the mode on the unit: FaderPort
//! 8/16 hold Select 1 and 2 while switching on, then "Studio One"; the
//! FaderPort hold Next, then Solo.
//!
//! Source: *FaderPort 16 and FaderPort 8 Owner's Manual* (PreSonus, Part#
//! 70-52000065-E), Appendix 11.2 "FaderPort Multichannel Controller MIDI
//! Protocol", and the *FaderPort Owner's Manual* (2018) §8.2; the
//! irregular FaderPort 16 ids as printed there and in Ardour's
//! `fp8_strip.cc`; encoders bit 6 counter-clockwise as Ardour reads them.

use crate::mapped::{
    Control, Does, Encoding, Feedback, Msg, Profile, StripButton, ascii, fader_key, sysex,
};
use crate::{AutomationButton, Button, Modifier, SurfaceCommand, SurfaceInput, SurfaceState};

/// Buttons both the FaderPort 8 and 16 have.
const COMMON: [Control; 24] = [
    (Msg::Note(0, 0x01), Does::Button(Button::ClearSolo)),
    (Msg::Note(0, 0x03), Does::Button(Button::Bypass)),
    (
        Msg::Note(0, 0x06),
        Does::Button(Button::Modifier(Modifier::Shift)),
    ),
    (
        Msg::Note(0, 0x46),
        Does::Button(Button::Modifier(Modifier::Shift)),
    ),
    (Msg::Note(0, 0x29), Does::Button(Button::SendPage(None))),
    (Msg::Note(0, 0x2A), Does::Button(Button::PanPage)),
    (Msg::Note(0, 0x2E), Does::Button(Button::BankLeft)),
    (Msg::Note(0, 0x2F), Does::Button(Button::BankRight)),
    (Msg::Note(0, 0x3B), Does::Button(Button::Click)),
    (Msg::Note(0, 0x3D), Does::Button(Button::Marker)),
    (
        Msg::Note(0, 0x4A),
        Does::Button(Button::Automation(AutomationButton::Read)),
    ),
    (
        Msg::Note(0, 0x4B),
        Does::Button(Button::Automation(AutomationButton::Write)),
    ),
    (
        Msg::Note(0, 0x4D),
        Does::Button(Button::Automation(AutomationButton::Touch)),
    ),
    (
        Msg::Note(0, 0x4E),
        Does::Button(Button::Automation(AutomationButton::Latch)),
    ),
    (
        Msg::Note(0, 0x4F),
        Does::Button(Button::Automation(AutomationButton::Off)),
    ),
    (Msg::Note(0, 0x56), Does::Button(Button::Loop)),
    (Msg::Note(0, 0x5B), Does::Button(Button::Rewind)),
    (Msg::Note(0, 0x5C), Does::Button(Button::Forward)),
    (Msg::Note(0, 0x5D), Does::Button(Button::Stop)),
    (Msg::Note(0, 0x5E), Does::Button(Button::Play)),
    (Msg::Note(0, 0x5F), Does::Button(Button::Record)),
    (Msg::Note(0, 0x66), Does::Button(Button::Footswitch(0))),
    // Pan/Param: the selected track's pan; the session navigator: the
    // playhead.
    (Msg::Cc(0, 0x10), Does::SelectedPan(Encoding::SignedBit)),
    (Msg::Cc(0, 0x3C), Does::Jog(Encoding::SignedBit)),
];

/// A strip's ids: solo, mute, select, touch notes.
#[derive(Clone, Copy)]
struct StripIds {
    solo: u8,
    mute: u8,
    select: u8,
    touch: u8,
}

/// Strips 1–8 (both), then the FaderPort 16's 9–16 (as printed).
const IDS: [StripIds; 16] = {
    const SOLO_HI: [u8; 8] = [0x50, 0x51, 0x52, 0x58, 0x54, 0x55, 0x59, 0x57];
    const SELECT_HI: [u8; 8] = [0x07, 0x21, 0x22, 0x23, 0x24, 0x25, 0x26, 0x27];
    let mut ids = [StripIds {
        solo: 0,
        mute: 0,
        select: 0,
        touch: 0,
    }; 16];
    let mut i = 0;
    while i < 8 {
        ids[i] = StripIds {
            solo: 0x08 + i as u8,
            mute: 0x10 + i as u8,
            select: 0x18 + i as u8,
            touch: 0x68 + i as u8,
        };
        ids[8 + i] = StripIds {
            solo: SOLO_HI[i],
            mute: 0x78 + i as u8,
            select: SELECT_HI[i],
            touch: 0x70 + i as u8,
        };
        i += 1;
    }
    ids
};

const fn controls<const N: usize>(strips: usize) -> [Control; N] {
    let mut c = [(Msg::Note(0, 0), Does::Fader(0)); N];
    let mut i = 0;
    while i < COMMON.len() {
        c[i] = COMMON[i];
        i += 1;
    }
    let mut k = 0;
    while k < strips {
        let id = IDS[k];
        c[i] = (Msg::Bend(k as u8), Does::Fader(k));
        c[i + 1] = (Msg::Note(0, id.touch), Does::Touch(k));
        c[i + 2] = (Msg::Note(0, id.solo), Does::Strip(StripButton::Solo, k));
        c[i + 3] = (Msg::Note(0, id.mute), Does::Strip(StripButton::Mute, k));
        c[i + 4] = (Msg::Note(0, id.select), Does::Strip(StripButton::Select, k));
        i += 5;
        k += 1;
    }
    c
}

static FP8_CONTROLS: [Control; 24 + 8 * 5] = controls(8);
static FP16_CONTROLS: [Control; 24 + 16 * 5] = controls(16);

/// A light (note) on, off or flashing.
fn led(f: &mut Feedback, note: u8, on: bool) {
    f.note(0, note, if on { 0x7F } else { 0x00 });
}

/// The select button in a colour (`91/92/93 id value`).
fn rgb(f: &mut Feedback, id: u8, [r, g, b]: [u8; 3]) {
    for (k, v) in [(1u8, r), (2, g), (3, b)] {
        f.set(
            0x0700_0000 | u32::from(k) << 8 | u32::from(id),
            vec![0x90 | k, id, v >> 1],
        );
    }
}

fn feedback(state: &SurfaceState, f: &mut Feedback, header: &[u8], strips: usize) {
    led(f, 0x5E, state.playing);
    led(f, 0x5D, !state.playing);
    led(f, 0x5F, state.recording);
    led(f, 0x56, state.looping);
    led(f, 0x3B, state.click);
    for (b, note) in [
        (AutomationButton::Read, 0x4A),
        (AutomationButton::Write, 0x4B),
        (AutomationButton::Touch, 0x4D),
        (AutomationButton::Latch, 0x4E),
        (AutomationButton::Off, 0x4F),
    ] {
        led(f, note, state.automation == Some(b));
    }
    for (i, id) in IDS.iter().enumerate().take(strips) {
        let s = state.strips.get(i).filter(|s| s.present);
        led(f, id.solo, s.is_some_and(|s| s.solo));
        led(f, id.mute, s.is_some_and(|s| s.mute));
        // The track's colour, bright when selected.
        let colour = s.map_or([0, 0, 0], |s| {
            let c = if s.color == [0, 0, 0] {
                [200, 200, 200]
            } else {
                s.color
            };
            if s.selected { c } else { c.map(|v| v / 4) }
        });
        rgb(f, id.select, colour);
        // The fader (not while it is held).
        if !f.touched(i) {
            let v = (s.map_or(0.0, |s| s.fader).clamp(0.0, 1.0) * 16383.0).round() as u16;
            f.set(
                fader_key(i),
                vec![0xE0 | i as u8, (v & 0x7F) as u8, (v >> 7) as u8],
            );
        }
        // The value bar: the pot (bipolar for pan).
        let (bar, mode) = if i < 8 {
            (0x30 + i as u8, 0x38 + i as u8)
        } else {
            (0x40 + i as u8 - 8, 0x48 + i as u8 - 8)
        };
        let (value, bipolar) = s.map_or((0, false), |s| {
            ((s.pot.clamp(0.0, 1.0) * 127.0).round() as u8, s.pot_bipolar)
        });
        f.cc(0, mode, if s.is_none() { 4 } else { u8::from(bipolar) });
        f.cc(0, bar, value);
        // The scribble strip: default mode, the name and the level.
        let strip = i as u8;
        f.set(
            0x0800_0000 | u32::from(strip),
            sysex(header, &[0x13, strip, 0x00]),
        );
        for (line, text) in [
            (0u8, s.map_or(String::new(), |s| s.name.clone())),
            (1, s.map_or(String::new(), |s| s.level.clone())),
        ] {
            let mut m = vec![0x12, strip, line, 0x00];
            m.extend(ascii(&text, 9));
            f.set(
                0x0900_0000 | u32::from(strip) << 4 | u32::from(line),
                sysex(header, &m),
            );
        }
    }
    // Meters (peaks the unit lets fall), ten times a second.
    if f.every(0x0A00_0000, 0.1) {
        for i in 0..strips {
            let db = state
                .strips
                .get(i)
                .filter(|s| s.present)
                .map_or(f32::NEG_INFINITY, |s| s.meter_db);
            let v = (((db + 60.0) / 60.0).clamp(0.0, 1.0) * 127.0).round() as u8;
            if v > 0 {
                f.send(if i < 8 {
                    vec![0xD0 | i as u8, v]
                } else {
                    vec![0xC0 | (i - 8) as u8, v]
                });
            }
        }
    }
}

const FP8: &[u8] = &[0xF0, 0x00, 0x01, 0x06, 0x02];
const FP16: &[u8] = &[0xF0, 0x00, 0x01, 0x06, 0x16];

fn fp8_feedback(state: &SurfaceState, f: &mut Feedback) {
    feedback(state, f, FP8, 8);
}

fn fp16_feedback(state: &SurfaceState, f: &mut Feedback) {
    feedback(state, f, FP16, 16);
}

const KEEP_ALIVE: (f64, &[u8]) = (1.0, &[0xA0, 0x00, 0x00]);

/// FaderPort 8, native mode.
pub static FADERPORT_8: Profile = Profile {
    keep_alive: Some(KEEP_ALIVE),
    ..Profile::new("PreSonus FaderPort 8", 8, &FP8_CONTROLS, fp8_feedback)
};

/// FaderPort 16, native mode.
pub static FADERPORT_16: Profile = Profile {
    keep_alive: Some(KEEP_ALIVE),
    ..Profile::new("PreSonus FaderPort 16", 16, &FP16_CONTROLS, fp16_feedback)
};

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mapped::Mapped;
    use crate::{Protocol, StripState, SurfaceInput};

    fn state() -> SurfaceState {
        let mut strips = vec![StripState::default(); 16];
        strips[0] = StripState {
            present: true,
            name: "Kick".into(),
            fader: 1.0,
            color: [250, 10, 10],
            selected: true,
            ..StripState::default()
        };
        strips[9] = StripState {
            present: true,
            name: "Pad".into(),
            solo: true,
            ..StripState::default()
        };
        SurfaceState {
            strips,
            ..SurfaceState::default()
        }
    }

    #[test]
    fn the_faderport_8_moves_its_faders_but_not_while_held() {
        let mut p = Mapped::new(&FADERPORT_8);
        let mut out = Vec::new();
        p.update(&state(), 0.0, &mut out);
        assert!(out.contains(&vec![0xA0, 0x00, 0x00]), "kept alive");
        assert!(out.contains(&vec![0xE0, 0x7F, 0x7F]), "fader 1 up");
        // The select button red (bright: selected).
        assert!(out.contains(&vec![0x91, 0x18, 125]));
        // The name on its scribble strip.
        assert!(out.iter().any(|m| m.windows(4).any(|w| w == b"Kick")));
        // Held: the motor stays put; let go, it goes where it should.
        let mut inputs = Vec::new();
        p.receive(&[0x90, 0x68, 0x7F], &mut inputs);
        let mut moved = state();
        moved.strips[0].fader = 0.25;
        out.clear();
        p.update(&moved, 0.05, &mut out);
        assert!(!out.iter().any(|m| m[0] == 0xE0), "{out:?}");
        p.receive(&[0x90, 0x68, 0x00], &mut inputs);
        out.clear();
        p.update(&moved, 0.06, &mut out);
        assert!(out.iter().any(|m| m[0] == 0xE0));
        assert_eq!(
            inputs,
            [
                SurfaceInput::Touch {
                    strip: 0,
                    touched: true
                },
                SurfaceInput::Touch {
                    strip: 0,
                    touched: false
                },
            ]
        );
    }

    #[test]
    fn the_faderport_16_uses_its_own_ids_for_the_strips_past_8() {
        let mut p = Mapped::new(&FADERPORT_16);
        let mut out = Vec::new();
        p.update(&state(), 0.0, &mut out);
        // Strip 10's solo is note 0x51.
        assert!(out.contains(&vec![0x90, 0x51, 0x7F]));
        let mut inputs = Vec::new();
        p.receive(&[0x90, 0x58, 0x7F], &mut inputs);
        p.receive(&[0xE9, 0x00, 0x40], &mut inputs);
        assert_eq!(
            inputs[0],
            SurfaceInput::Button {
                button: Button::Solo(11),
                pressed: true
            }
        );
        assert!(matches!(inputs[1], SurfaceInput::Fader { strip: 9, .. }));
    }
}

// --- FaderPort (2018), the navigator in full --------------------------------

/// What the FaderPort's session navigator (its encoder, its push, Prev and
/// Next) does, chosen by the lit mode button (manual §2.4).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NavigatorMode {
    /// The encoder and Prev/Next move the selection (the parameter under
    /// the mouse has no counterpart here).
    Link,
    /// The encoder moves the master's level (push: 0 dB).
    Master,
    /// The encoder pans the selected track (push: centre); Prev/Next move
    /// the selection.
    Pan,
    /// The encoder and Prev/Next move the selection.
    Channel,
    /// The encoder moves the playhead (push: the whole project in view);
    /// with Shift + Scroll it zooms instead.
    Scroll,
    /// The encoder nudges the selected clips; Prev/Next go through the
    /// sections.
    Section,
    /// The encoder moves the playhead; Prev/Next go through the markers;
    /// push drops one.
    Marker,
}

/// Button ids of the FaderPort (2018) (manual §8.2.4).
mod fp1 {
    pub const ARM: u8 = 0x00;
    pub const BYPASS: u8 = 0x03;
    pub const LINK: u8 = 0x05;
    pub const SOLO: u8 = 0x08;
    pub const MUTE: u8 = 0x10;
    pub const PUSH: u8 = 0x20;
    pub const PAN: u8 = 0x2A;
    pub const PREV: u8 = 0x2E;
    pub const NEXT: u8 = 0x2F;
    pub const CHANNEL: u8 = 0x36;
    pub const SCROLL: u8 = 0x38;
    pub const MASTER: u8 = 0x3A;
    pub const CLICK: u8 = 0x3B;
    pub const SECTION: u8 = 0x3C;
    pub const MARKER: u8 = 0x3D;
    pub const SHIFT: u8 = 0x46;
    pub const READ: u8 = 0x4A;
    pub const WRITE: u8 = 0x4B;
    pub const TOUCH: u8 = 0x4D;
    pub const LOOP: u8 = 0x56;
    pub const REWIND: u8 = 0x5B;
    pub const FORWARD: u8 = 0x5C;
    pub const STOP: u8 = 0x5D;
    pub const PLAY: u8 = 0x5E;
    pub const RECORD: u8 = 0x5F;
    pub const FOOT: u8 = 0x66;
    pub const FADER_TOUCH: u8 = 0x68;
}

/// The FaderPort (2018): one motorised fader on the selected track, its
/// channel buttons, transport, automation, and the session navigator with
/// its modes — Shift + Prev/Next undo and redo, Shift + Master, Click,
/// Section, Marker are F1–F4, Shift + Pan puts the pan on the fader,
/// Shift + Scroll zooms, Shift + Arm arms every track, Shift + Solo/Mute
/// clear them, Shift + Touch latches, Shift + Read turns automation off.
/// Native ("Studio One") mode, kept alive with `A0 00 00` every second.
pub struct FaderPortOne {
    mode: NavigatorMode,
    shift: bool,
    zoom: bool,
    /// Shift + Pan: the fader pans.
    pan_on_fader: bool,
    touched: bool,
    sent: std::collections::HashMap<u32, Vec<u8>>,
    alive_at: f64,
    /// What the last state showed: the selected track's pan, the master,
    /// the travel of 0 dB.
    pan: f32,
    master: f32,
    unity: f32,
}

impl Default for FaderPortOne {
    fn default() -> Self {
        Self::new()
    }
}

impl FaderPortOne {
    pub fn new() -> Self {
        Self {
            mode: NavigatorMode::Channel,
            shift: false,
            zoom: false,
            pan_on_fader: false,
            touched: false,
            sent: std::collections::HashMap::new(),
            alive_at: f64::NEG_INFINITY,
            pan: 0.0,
            master: 0.0,
            unity: 0.0,
        }
    }

    pub fn mode(&self) -> NavigatorMode {
        self.mode
    }

    fn set(&mut self, key: u32, msg: Vec<u8>, out: &mut Vec<Vec<u8>>) {
        if self.sent.get(&key) != Some(&msg) {
            out.push(msg.clone());
            self.sent.insert(key, msg);
        }
    }

    fn led(&mut self, id: u8, on: bool, out: &mut Vec<Vec<u8>>) {
        self.set(
            u32::from(id),
            vec![0x90, id, if on { 0x7F } else { 0x00 }],
            out,
        );
    }

    /// An RGB button: lit in `colour` (0–127 per channel) or off.
    fn rgb(&mut self, id: u8, on: bool, [r, g, b]: [u8; 3], out: &mut Vec<Vec<u8>>) {
        self.led(id, on, out);
        for (k, v) in [(1u8, r), (2, g), (3, b)] {
            self.set(
                0x100 * u32::from(k) + u32::from(id),
                vec![0x90 | k, id, v.min(127)],
                out,
            );
        }
    }

    fn command(c: SurfaceCommand, out: &mut Vec<SurfaceInput>) {
        out.push(SurfaceInput::Command(c));
    }

    fn press(button: Button, out: &mut Vec<SurfaceInput>) {
        out.push(SurfaceInput::Button {
            button,
            pressed: true,
        });
        out.push(SurfaceInput::Button {
            button,
            pressed: false,
        });
    }

    fn button(&mut self, id: u8, out: &mut Vec<SurfaceInput>) {
        use NavigatorMode as M;
        let shift = self.shift;
        match id {
            fp1::ARM if shift => Self::press(Button::ArmAll, out),
            fp1::ARM => Self::press(Button::Arm(0), out),
            fp1::SOLO if shift => Self::press(Button::ClearSolo, out),
            fp1::SOLO => Self::press(Button::Solo(0), out),
            fp1::MUTE if shift => Self::command(SurfaceCommand::ClearMutes, out),
            fp1::MUTE => Self::press(Button::Mute(0), out),
            fp1::BYPASS => Self::command(SurfaceCommand::BypassTrack(0), out),
            fp1::PREV if shift => Self::press(Button::Undo, out),
            fp1::NEXT if shift => Self::press(Button::Redo, out),
            fp1::PREV | fp1::NEXT => {
                let next = id == fp1::NEXT;
                let c = match self.mode {
                    M::Section if next => SurfaceCommand::SectionNext,
                    M::Section => SurfaceCommand::SectionPrevious,
                    M::Marker if next => SurfaceCommand::MarkerNext,
                    M::Marker => SurfaceCommand::MarkerPrevious,
                    _ if next => SurfaceCommand::SelectNext,
                    _ => SurfaceCommand::SelectPrevious,
                };
                Self::command(c, out);
            }
            fp1::PUSH => match self.mode {
                M::Master => out.push(SurfaceInput::Fader {
                    strip: crate::MASTER,
                    travel: self.unity,
                }),
                M::Pan => out.push(SurfaceInput::Pan { strip: 0, pan: 0.0 }),
                M::Scroll => Self::command(SurfaceCommand::ZoomFit, out),
                M::Marker => Self::press(Button::Marker, out),
                _ => {}
            },
            fp1::LINK if !shift => self.mode = M::Link,
            fp1::PAN if shift => self.pan_on_fader = !self.pan_on_fader,
            fp1::PAN => self.mode = M::Pan,
            fp1::CHANNEL if !shift => self.mode = M::Channel,
            fp1::SCROLL => {
                self.zoom = shift && !(self.mode == M::Scroll && self.zoom);
                self.mode = M::Scroll;
            }
            fp1::MASTER if shift => Self::press(Button::Function(0), out),
            fp1::MASTER => self.mode = M::Master,
            fp1::CLICK if shift => Self::press(Button::Function(1), out),
            fp1::CLICK => Self::press(Button::Click, out),
            fp1::SECTION if shift => Self::press(Button::Function(2), out),
            fp1::SECTION => self.mode = M::Section,
            fp1::MARKER if shift => Self::press(Button::Function(3), out),
            fp1::MARKER => self.mode = M::Marker,
            fp1::READ if shift => Self::press(Button::Automation(AutomationButton::Off), out),
            fp1::READ => Self::press(Button::Automation(AutomationButton::Read), out),
            fp1::WRITE => Self::press(Button::Automation(AutomationButton::Write), out),
            fp1::TOUCH if shift => Self::press(Button::Automation(AutomationButton::Latch), out),
            fp1::TOUCH => Self::press(Button::Automation(AutomationButton::Touch), out),
            fp1::LOOP => Self::press(Button::Loop, out),
            fp1::REWIND => Self::press(Button::Rewind, out),
            fp1::FORWARD => Self::press(Button::Forward, out),
            fp1::STOP => Self::press(Button::Stop, out),
            fp1::PLAY => Self::press(Button::Play, out),
            fp1::RECORD => Self::press(Button::Record, out),
            fp1::FOOT => Self::press(Button::Footswitch(0), out),
            _ => {}
        }
    }

    fn encoder(&mut self, ticks: i32, out: &mut Vec<SurfaceInput>) {
        use NavigatorMode as M;
        if ticks == 0 {
            return;
        }
        let each = |c_up: SurfaceCommand, c_down: SurfaceCommand, out: &mut Vec<SurfaceInput>| {
            let c = if ticks > 0 { c_up } else { c_down };
            for _ in 0..ticks.unsigned_abs().min(8) {
                Self::command(c, out);
            }
        };
        match self.mode {
            M::Link | M::Channel => each(
                SurfaceCommand::SelectNext,
                SurfaceCommand::SelectPrevious,
                out,
            ),
            M::Master => {
                self.master = (self.master + ticks as f32 * 0.005).clamp(0.0, 1.0);
                out.push(SurfaceInput::Fader {
                    strip: crate::MASTER,
                    travel: self.master,
                });
            }
            M::Pan => {
                self.pan = (self.pan + ticks as f32 * 0.01).clamp(-1.0, 1.0);
                out.push(SurfaceInput::Pan {
                    strip: 0,
                    pan: self.pan,
                });
            }
            M::Section => each(
                SurfaceCommand::Nudge { forward: true },
                SurfaceCommand::Nudge { forward: false },
                out,
            ),
            M::Scroll if self.zoom => each(SurfaceCommand::ZoomIn, SurfaceCommand::ZoomOut, out),
            M::Scroll | M::Marker => out.push(SurfaceInput::Jog(ticks)),
        }
    }
}

impl crate::Protocol for FaderPortOne {
    fn strips(&self) -> usize {
        1
    }

    fn follows_selection(&self) -> bool {
        true
    }

    fn receive(&mut self, msg: &[u8], out: &mut Vec<SurfaceInput>) {
        let [status, a, ..] = *msg else { return };
        let b = msg.get(2).copied().unwrap_or(0);
        match status {
            0x90 | 0x80 => {
                let pressed = status == 0x90 && b > 0;
                match a {
                    // Every Shift combination is the protocol's own (the
                    // session's Shift would turn F1–F4 into marker jumps).
                    fp1::SHIFT => self.shift = pressed,
                    fp1::FADER_TOUCH => {
                        self.touched = pressed;
                        if !pressed {
                            self.sent.remove(&fader_key(0));
                        }
                        out.push(SurfaceInput::Touch {
                            strip: 0,
                            touched: pressed,
                        });
                    }
                    _ if pressed => self.button(a, out),
                    _ => {}
                }
            }
            0xE0 => {
                let v = u16::from(a & 0x7F) | u16::from(b & 0x7F) << 7;
                let travel = f32::from(v) / 16383.0;
                if self.pan_on_fader {
                    out.push(SurfaceInput::Pan {
                        strip: 0,
                        pan: travel * 2.0 - 1.0,
                    });
                } else {
                    out.push(SurfaceInput::Fader { strip: 0, travel });
                }
            }
            // The encoder: bit 6 counter-clockwise (as Ardour reads it).
            0xB0 if a == 0x3C || a == 0x10 => {
                let t = Encoding::SignedBit.ticks(b).unwrap_or(0);
                self.encoder(t, out);
            }
            _ => {}
        }
    }

    fn update(&mut self, state: &SurfaceState, now: f64, out: &mut Vec<Vec<u8>>) {
        use NavigatorMode as M;
        if now - self.alive_at >= KEEP_ALIVE.0 {
            self.alive_at = now;
            out.push(KEEP_ALIVE.1.to_vec());
        }
        self.unity = state.unity;
        let s = state.strips.first().filter(|s| s.present);
        self.pan = s.map_or(0.0, |s| s.pan);
        self.master = state.master.unwrap_or(0.0);
        self.led(fp1::ARM, s.is_some_and(|s| s.arm), out);
        self.led(fp1::SOLO, s.is_some_and(|s| s.solo), out);
        self.led(fp1::MUTE, s.is_some_and(|s| s.mute), out);
        self.led(fp1::SHIFT, self.shift, out);
        self.led(fp1::PREV, true, out);
        self.led(fp1::NEXT, true, out);
        let mode = self.mode;
        self.rgb(fp1::LINK, mode == M::Link, [127, 127, 127], out);
        self.rgb(
            fp1::PAN,
            mode == M::Pan || self.pan_on_fader,
            [0, 110, 127],
            out,
        );
        self.rgb(fp1::CHANNEL, mode == M::Channel, [0, 40, 127], out);
        let scroll = if self.zoom {
            [127, 90, 0]
        } else {
            [0, 127, 40]
        };
        self.rgb(fp1::SCROLL, mode == M::Scroll, scroll, out);
        self.led(fp1::MASTER, mode == M::Master, out);
        self.led(fp1::SECTION, mode == M::Section, out);
        self.led(fp1::MARKER, mode == M::Marker, out);
        self.led(fp1::CLICK, state.click, out);
        let auto = state.automation;
        self.rgb(
            fp1::READ,
            auto == Some(AutomationButton::Read),
            [0, 127, 0],
            out,
        );
        self.rgb(
            fp1::WRITE,
            auto == Some(AutomationButton::Write),
            [127, 0, 0],
            out,
        );
        let touch = matches!(
            auto,
            Some(AutomationButton::Touch | AutomationButton::Latch)
        );
        self.rgb(fp1::TOUCH, touch, [127, 90, 0], out);
        self.led(fp1::PLAY, state.playing, out);
        self.led(fp1::STOP, !state.playing, out);
        self.led(fp1::RECORD, state.recording, out);
        self.led(fp1::LOOP, state.looping, out);
        // The motor: the level, or the pan on the fader (not while held).
        if !self.touched {
            let travel = if self.pan_on_fader {
                (s.map_or(0.0, |s| s.pan) + 1.0) / 2.0
            } else {
                s.map_or(0.0, |s| s.fader)
            };
            let v = (travel.clamp(0.0, 1.0) * 16383.0).round() as u16;
            self.set(
                fader_key(0),
                vec![0xE0, (v & 0x7F) as u8, (v >> 7) as u8],
                out,
            );
        }
    }

    fn reset(&mut self) {
        self.sent.clear();
        self.alive_at = f64::NEG_INFINITY;
    }
}

#[cfg(test)]
mod faderport_one_tests {
    use super::*;
    use crate::{Protocol, StripState};

    fn state() -> SurfaceState {
        SurfaceState {
            strips: vec![StripState {
                present: true,
                name: "Vocal".into(),
                fader: 0.5,
                pan: 0.2,
                mute: true,
                ..StripState::default()
            }],
            master: Some(0.7),
            unity: 0.75,
            ..SurfaceState::default()
        }
    }

    fn feed(p: &mut FaderPortOne, msgs: &[&[u8]]) -> Vec<SurfaceInput> {
        let mut out = Vec::new();
        for m in msgs {
            p.receive(m, &mut out);
        }
        out
    }

    #[test]
    fn its_modes_give_the_encoder_and_prev_next_their_jobs() {
        let mut p = FaderPortOne::new();
        let mut sent = Vec::new();
        p.update(&state(), 0.0, &mut sent);
        assert!(sent.contains(&vec![0xA0, 0x00, 0x00]), "kept alive");
        assert!(sent.contains(&vec![0x90, fp1::MUTE, 0x7F]));
        assert!(sent.contains(&vec![0xE0, 0x00, 0x40]), "the fader half way");
        // Channel (the start): the encoder and Next move the selection.
        let got = feed(&mut p, &[&[0xB0, 0x3C, 0x02], &[0x90, fp1::NEXT, 0x7F]]);
        assert_eq!(
            got,
            vec![
                SurfaceInput::Command(SurfaceCommand::SelectNext),
                SurfaceInput::Command(SurfaceCommand::SelectNext),
                SurfaceInput::Command(SurfaceCommand::SelectNext),
            ]
        );
        // Master: the encoder moves the master from where it is; push: 0 dB.
        feed(&mut p, &[&[0x90, fp1::MASTER, 0x7F]]);
        assert_eq!(p.mode(), NavigatorMode::Master);
        let got = feed(&mut p, &[&[0xB0, 0x3C, 0x41], &[0x90, fp1::PUSH, 0x7F]]);
        assert_eq!(
            got,
            vec![
                SurfaceInput::Fader {
                    strip: crate::MASTER,
                    travel: 0.695
                },
                SurfaceInput::Fader {
                    strip: crate::MASTER,
                    travel: 0.75
                },
            ]
        );
        // Marker: Prev goes back a marker, push drops one.
        feed(&mut p, &[&[0x90, fp1::MARKER, 0x7F]]);
        let got = feed(
            &mut p,
            &[&[0x90, fp1::PREV, 0x7F], &[0x90, fp1::PUSH, 0x7F]],
        );
        assert_eq!(
            got[0],
            SurfaceInput::Command(SurfaceCommand::MarkerPrevious)
        );
        assert_eq!(
            got[1],
            SurfaceInput::Button {
                button: Button::Marker,
                pressed: true
            }
        );
        // Shift + Prev: undo; Shift + Pan: the fader pans.
        let got = feed(
            &mut p,
            &[
                &[0x90, fp1::SHIFT, 0x7F],
                &[0x90, fp1::PREV, 0x7F],
                &[0x90, fp1::PAN, 0x7F],
                &[0x90, fp1::SHIFT, 0x00],
                &[0xE0, 0x7F, 0x7F],
            ],
        );
        assert!(got.contains(&SurfaceInput::Button {
            button: Button::Undo,
            pressed: true
        }));
        assert_eq!(got.last(), Some(&SurfaceInput::Pan { strip: 0, pan: 1.0 }));
        // Shift + Scroll zooms with the encoder.
        feed(
            &mut p,
            &[
                &[0x90, fp1::SHIFT, 0x7F],
                &[0x90, fp1::SCROLL, 0x7F],
                &[0x90, fp1::SHIFT, 0x00],
            ],
        );
        let got = feed(&mut p, &[&[0xB0, 0x3C, 0x41]]);
        assert_eq!(got, vec![SurfaceInput::Command(SurfaceCommand::ZoomOut)]);
    }
}
