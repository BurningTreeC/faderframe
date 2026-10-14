//! Arturia's keyboards in their DAW program: the KeyLab Essential mk3 and
//! the MiniLab 3 speak it on their main "MIDI" port (the keys' port too —
//! the profile claims its controls, the keys still play), after a
//! connection handshake that makes their knobs and faders send CCs there
//! and lets the host light their buttons and write their screens.
//!
//! Sources: Arturia's own DAW scripts (the KeyLab Essential mk3's for FL
//! Studio — `KLEss3Process.py`, `LedIds.py`, `Displays.py`; the MiniLab
//! 3's for Reason and FL Studio), Bitwig's official extensions
//! (`controllers/arturia/keylab/essentialMk3`, `minilab3`, MIT) and
//! Ableton Live 12's scripts (`KeyLab_Essential_mk3`, `MiniLab_3`), which
//! agree; the program switch from the community guide
//! github.com/PrzemekBarski/arturia-keylab-essential-mk3-programming-guide.
//! The KeyLab Essential (mkI) and the KeyLab mkII speak Mackie Control on
//! their DAW port: see [`super::KEYLAB_ESSENTIAL`] and
//! [`super::KEYLAB_MK2`].

use crate::mapped::{
    Control, Does, Encoding, Feedback, Msg, Profile, ascii, position_text, shown_strip, sysex,
};
use crate::{Button, EditButton, MASTER, Modifier, SurfaceState};

/// Arturia's keyboard SysEx header.
const ARTURIA: [u8; 6] = [0xF0, 0x00, 0x20, 0x6B, 0x7F, 0x42];

/// Whether `m` is Arturia SysEx (program and bank reports).
fn arturia_sysex(m: &[u8]) -> bool {
    m.starts_with(&ARTURIA)
}

// --- KeyLab Essential mk3 --------------------------------------------------

/// All on MIDI channel 1 (index 0); knobs and faders absolute (the
/// endless knobs keep a value of their own, which the host re-seeds).
const KLE3_CONTROLS: &[Control] = &[
    (Msg::Cc(0, 20), Does::Button(Button::Stop)),
    (Msg::Cc(0, 21), Does::Button(Button::Play)),
    (Msg::Cc(0, 22), Does::Button(Button::Record)),
    (Msg::Cc(0, 24), Does::Button(Button::Loop)),
    (Msg::Cc(0, 25), Does::Button(Button::Rewind)),
    (Msg::Cc(0, 26), Does::Button(Button::Forward)),
    (Msg::Cc(0, 27), Does::Button(Button::Click)),
    (Msg::Cc(0, 40), Does::Button(Button::Save)),
    (
        Msg::Cc(0, 41),
        Does::Button(Button::Edit(EditButton::Quantize)),
    ),
    (Msg::Cc(0, 42), Does::Button(Button::Undo)),
    (Msg::Cc(0, 43), Does::Button(Button::Redo)),
    // The four buttons under the screen (labelled there).
    (Msg::Cc(0, 44), Does::Button(Button::BankLeft)),
    (Msg::Cc(0, 45), Does::Button(Button::BankRight)),
    (Msg::Cc(0, 46), Does::Button(Button::PanPage)),
    (Msg::Cc(0, 47), Does::Button(Button::SendPage(None))),
    // The main encoder moves the playhead; its press marks.
    (Msg::Cc(0, 116), Does::Jog(Encoding::Offset64)),
    (Msg::Cc(0, 117), Does::Button(Button::Marker)),
    (Msg::Cc(0, 118), Does::Button(Button::ChannelRight)),
    (Msg::Cc(0, 96), Does::Knob(0, Encoding::Absolute)),
    (Msg::Cc(0, 97), Does::Knob(1, Encoding::Absolute)),
    (Msg::Cc(0, 98), Does::Knob(2, Encoding::Absolute)),
    (Msg::Cc(0, 99), Does::Knob(3, Encoding::Absolute)),
    (Msg::Cc(0, 100), Does::Knob(4, Encoding::Absolute)),
    (Msg::Cc(0, 101), Does::Knob(5, Encoding::Absolute)),
    (Msg::Cc(0, 102), Does::Knob(6, Encoding::Absolute)),
    (Msg::Cc(0, 103), Does::Knob(7, Encoding::Absolute)),
    (Msg::Cc(0, 105), Does::Fader(0)),
    (Msg::Cc(0, 106), Does::Fader(1)),
    (Msg::Cc(0, 107), Does::Fader(2)),
    (Msg::Cc(0, 108), Does::Fader(3)),
    (Msg::Cc(0, 109), Does::Fader(4)),
    (Msg::Cc(0, 110), Does::Fader(5)),
    (Msg::Cc(0, 111), Does::Fader(6)),
    (Msg::Cc(0, 112), Does::Fader(7)),
    (Msg::Cc(0, 113), Does::Fader(MASTER)),
];

/// RGB light ids (Arturia's `LedIds.py`).
mod kle3_led {
    pub const LOOP: u8 = 0x10;
    pub const REWIND: u8 = 0x11;
    pub const FORWARD: u8 = 0x12;
    pub const METRONOME: u8 = 0x13;
    pub const STOP: u8 = 0x14;
    pub const PLAY: u8 = 0x15;
    pub const RECORD: u8 = 0x16;
    pub const SAVE: u8 = 0x0C;
    pub const QUANT: u8 = 0x0D;
    pub const UNDO: u8 = 0x0E;
    pub const REDO: u8 = 0x0F;
}

fn kle3_rgb(f: &mut Feedback, id: u8, [r, g, b]: [u8; 3]) {
    f.set(
        0x1000 | u32::from(id),
        sysex(&ARTURIA, &[0x04, 0x01, 0x16, id, r, g, b]),
    );
}

/// Full and dim colours (0–127 per channel).
const DIM: [u8; 3] = [8, 8, 8];

fn lit(on: bool, colour: [u8; 3]) -> [u8; 3] {
    if on { colour } else { DIM }
}

fn kle3_feedback(state: &SurfaceState, f: &mut Feedback) {
    use kle3_led::*;
    kle3_rgb(f, PLAY, lit(state.playing, [0, 127, 0]));
    kle3_rgb(f, STOP, lit(!state.playing, [100, 100, 100]));
    kle3_rgb(f, RECORD, lit(state.recording, [127, 0, 0]));
    kle3_rgb(f, LOOP, lit(state.looping, [127, 90, 0]));
    kle3_rgb(f, METRONOME, lit(state.click, [0, 60, 127]));
    kle3_rgb(f, QUANT, lit(state.input_quantize, [127, 60, 0]));
    for id in [REWIND, FORWARD, SAVE, UNDO, REDO] {
        kle3_rgb(f, id, DIM);
    }
    // The screen: the selected track and its level, the position.
    let shown = shown_strip(state);
    let top = shown.map_or_else(|| "FaderFrame".to_string(), |s| s.name.clone());
    let bottom = format!(
        "{} {}",
        shown.map_or(String::new(), |s| s.level.clone()),
        position_text(state)
    );
    let mut screen = vec![0x04, 0x01, 0x60, 0x12, 0x01];
    screen.extend(ascii(&top, 16));
    screen.extend([0x00, 0x02]);
    screen.extend(ascii(&bottom, 16));
    screen.push(0x00);
    f.set(0x2000, sysex(&ARTURIA, &screen));
    // The endless knobs carry on from the values shown (a pan moved with
    // the mouse turns on from there).
    for (i, s) in state.strips.iter().take(8).enumerate() {
        let v = if s.present {
            (s.pot.clamp(0.0, 1.0) * 127.0).round() as u8
        } else {
            0
        };
        f.set(
            0x3000 | i as u32,
            sysex(&ARTURIA, &[0x02, 0x0F, 0x40, 3 + i as u8, v]),
        );
    }
}

/// What the KeyLab Essential mk3's DAW program takes from its port.
pub fn kle3_claims(m: &[u8]) -> bool {
    arturia_sysex(m) || KEYLAB_ESSENTIAL_MK3.takes(m)
}

/// KeyLab Essential mk3 (49/61/88), DAW program on the "MIDI" port.
pub static KEYLAB_ESSENTIAL_MK3: Profile = Profile {
    // The DAW connection, then the DAW program.
    hello: &[
        &[
            0xF0, 0x00, 0x20, 0x6B, 0x7F, 0x42, 0x02, 0x0F, 0x40, 0x5A, 0x01, 0xF7,
        ],
        &[
            0xF0, 0x00, 0x20, 0x6B, 0x7F, 0x42, 0x21, 0x11, 0x40, 0x02, 0x00, 0x01, 0xF7,
        ],
    ],
    goodbye: &[&[
        0xF0, 0x00, 0x20, 0x6B, 0x7F, 0x42, 0x02, 0x0F, 0x40, 0x5A, 0x00, 0xF7,
    ]],
    claims: Some(kle3_claims),
    ..Profile::new(
        "Arturia KeyLab Essential mk3",
        8,
        KLE3_CONTROLS,
        kle3_feedback,
    )
};

// --- MiniLab 3 ---------------------------------------------------------------

const MINILAB3_CONTROLS: &[Control] = &[
    (Msg::Cc(0, 86), Does::Knob(0, Encoding::Absolute)),
    (Msg::Cc(0, 87), Does::Knob(1, Encoding::Absolute)),
    (Msg::Cc(0, 89), Does::Knob(2, Encoding::Absolute)),
    (Msg::Cc(0, 90), Does::Knob(3, Encoding::Absolute)),
    (Msg::Cc(0, 110), Does::Knob(4, Encoding::Absolute)),
    (Msg::Cc(0, 111), Does::Knob(5, Encoding::Absolute)),
    (Msg::Cc(0, 116), Does::Knob(6, Encoding::Absolute)),
    (Msg::Cc(0, 117), Does::Knob(7, Encoding::Absolute)),
    (Msg::Cc(0, 14), Does::Fader(0)),
    (Msg::Cc(0, 15), Does::Fader(1)),
    (Msg::Cc(0, 30), Does::Fader(2)),
    (Msg::Cc(0, 31), Does::Fader(3)),
    // The main encoder moves the playhead; with Shift the selection.
    (Msg::Cc(0, 28), Does::Jog(Encoding::Offset64)),
    (
        Msg::Cc(0, 29),
        Does::Wheel(Button::Down, Button::Up, Encoding::Offset64),
    ),
    (
        Msg::Cc(0, 27),
        Does::Button(Button::Modifier(Modifier::Shift)),
    ),
    (Msg::Cc(0, 105), Does::Button(Button::Loop)),
    (Msg::Cc(0, 106), Does::Button(Button::Stop)),
    (Msg::Cc(0, 107), Does::Button(Button::Play)),
    (Msg::Cc(0, 108), Does::Button(Button::Record)),
];

fn minilab3_rgb(f: &mut Feedback, id: u8, [r, g, b]: [u8; 3]) {
    f.set(
        0x1000 | u32::from(id),
        sysex(&ARTURIA, &[0x02, 0x02, 0x16, id, r, g, b]),
    );
}

fn minilab3_feedback(state: &SurfaceState, f: &mut Feedback) {
    minilab3_rgb(f, 0x57, lit(state.looping, [127, 90, 0]));
    minilab3_rgb(f, 0x58, lit(!state.playing, [100, 100, 100]));
    minilab3_rgb(f, 0x59, lit(state.playing, [0, 127, 0]));
    minilab3_rgb(f, 0x5A, lit(state.recording, [127, 0, 0]));
    let shown = shown_strip(state);
    let top = shown.map_or_else(|| "FaderFrame".to_string(), |s| s.name.clone());
    let mut screen = vec![0x04, 0x02, 0x60, 0x01];
    screen.extend(ascii(&top, 10));
    screen.extend([0x00, 0x02]);
    screen.extend(ascii(&position_text(state), 18));
    screen.push(0x00);
    f.set(0x2000, sysex(&ARTURIA, &screen));
}

pub fn minilab3_claims(m: &[u8]) -> bool {
    arturia_sysex(m) || MINILAB_3.takes(m)
}

/// MiniLab 3, DAW program (Shift + pad 3) on the "MIDI" port.
pub static MINILAB_3: Profile = Profile {
    hello: &[&[
        0xF0, 0x00, 0x20, 0x6B, 0x7F, 0x42, 0x02, 0x02, 0x40, 0x6A, 0x21, 0xF7,
    ]],
    goodbye: &[&[
        0xF0, 0x00, 0x20, 0x6B, 0x7F, 0x42, 0x02, 0x02, 0x40, 0x6A, 0x20, 0xF7,
    ]],
    claims: Some(minilab3_claims),
    ..Profile::new("Arturia MiniLab 3", 8, MINILAB3_CONTROLS, minilab3_feedback)
};

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mapped::Mapped;
    use crate::{Protocol, StripState, SurfaceInput};

    fn state() -> SurfaceState {
        let mut strips = vec![StripState::default(); 8];
        strips[0] = StripState {
            present: true,
            name: "Vocals".into(),
            level: "-3.0".into(),
            pot: 0.5,
            selected: true,
            ..StripState::default()
        };
        SurfaceState {
            strips,
            playing: true,
            ..SurfaceState::default()
        }
    }

    #[test]
    fn the_keylab_essential_mk3_connects_and_plays_the_transport() {
        let mut p = Mapped::new(&KEYLAB_ESSENTIAL_MK3);
        let mut out = Vec::new();
        p.update(&state(), 0.0, &mut out);
        // The connection handshake first, then the DAW program.
        assert_eq!(
            out[0],
            [
                0xF0, 0x00, 0x20, 0x6B, 0x7F, 0x42, 0x02, 0x0F, 0x40, 0x5A, 0x01, 0xF7
            ]
        );
        // Play lit green.
        assert!(out.contains(&vec![
            0xF0, 0x00, 0x20, 0x6B, 0x7F, 0x42, 0x04, 0x01, 0x16, 0x15, 0, 127, 0, 0xF7
        ]));
        // The screen names the selected track.
        assert!(out.iter().any(|m| m.windows(6).any(|w| w == b"Vocals")));
        // Nothing again unless something changes.
        let mut again = Vec::new();
        p.update(&state(), 0.1, &mut again);
        assert!(again.is_empty(), "{again:?}");
        let mut inputs = Vec::new();
        p.receive(&[0xB0, 21, 127], &mut inputs);
        p.receive(&[0xB0, 105, 64], &mut inputs);
        p.receive(&[0xB0, 96, 127], &mut inputs);
        p.receive(&[0xB0, 116, 66], &mut inputs);
        assert_eq!(
            inputs,
            [
                SurfaceInput::Button {
                    button: Button::Play,
                    pressed: true
                },
                SurfaceInput::Fader {
                    strip: 0,
                    travel: 64.0 / 127.0
                },
                SurfaceInput::Knob {
                    strip: 0,
                    value: 1.0
                },
                SurfaceInput::Jog(2),
            ]
        );
    }

    #[test]
    fn the_keys_on_its_port_still_play() {
        // Notes, the pads, the mod wheel: the instruments'.
        assert!(!kle3_claims(&[0x90, 60, 100]));
        assert!(!kle3_claims(&[0x9A, 36, 100]));
        assert!(!kle3_claims(&[0xB0, 1, 64]));
        // Its controls and its SysEx: the surface's.
        assert!(kle3_claims(&[0xB0, 21, 127]));
        assert!(kle3_claims(&[
            0xF0, 0x00, 0x20, 0x6B, 0x7F, 0x42, 0x21, 0x11, 0x40, 0x02, 0x00, 0x01, 0xF7
        ]));
        assert!(minilab3_claims(&[0xB0, 107, 127]));
        assert!(!minilab3_claims(&[0x99, 36, 100]));
    }
}
