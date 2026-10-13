//! M-Audio Oxygen Pro (25/49/61) and Oxygen MKV in their vendor DAW mode
//! on the DAW port ("Mackie/HUI": the Pro's third port, the MKV's
//! second), switched on by SysEx: faders, knobs, the transport, the
//! fader buttons, two pad rows on the clip launcher.
//!
//! Sources: Ableton Live 12's `Oxygen_Pro` and `Oxygen_5th_Gen` scripts
//! and Bitwig's official `controllers/maudio/oxygenpro` (MIT), which send
//! the same set-up (`F0 00 01 05 7F 00 00 6D 00 01 02 F7` — `… 00 F7` on
//! the MKV — then the control and LED modes) and read the same CCs. The
//! encoder (CC 103) is read as Bitwig does (above 64 clockwise); Ableton
//! reads it the other way: check on the hardware.

use crate::mapped::{Control, Does, Encoding, Feedback, Msg, Profile, StripButton};
use crate::{Button, MASTER, Modifier, SlotKind, SurfaceInput, SurfaceState};

/// Pads: row 1 (scene 0) then row 2, notes on channel 1.
const PAD_NOTES: [[u8; 8]; 2] = [
    [40, 41, 42, 43, 48, 49, 50, 51],
    [36, 37, 38, 39, 44, 45, 46, 47],
];

const OXYGEN_CONTROLS: [Control; 13 + 8 * 3 + 16] = {
    let fixed: [Control; 13] = [
        (Msg::Cc(0, 41), Does::Fader(MASTER)),
        (
            Msg::Cc(0, 103),
            Does::Wheel(Button::Down, Button::Up, Encoding::Offset64),
        ),
        (Msg::Cc(0, 104), Does::Button(Button::Undo)),
        (
            Msg::Cc(12, 105),
            Does::Button(Button::Modifier(Modifier::Shift)),
        ),
        (Msg::Cc(0, 106), Does::Button(Button::Click)),
        (Msg::Cc(0, 107), Does::Input(SurfaceInput::LaunchScene(0))),
        (Msg::Cc(0, 108), Does::Input(SurfaceInput::LaunchScene(1))),
        (Msg::Cc(0, 110), Does::Button(Button::BankLeft)),
        (Msg::Cc(0, 111), Does::Button(Button::BankRight)),
        (Msg::Cc(0, 114), Does::Button(Button::Loop)),
        (Msg::Cc(0, 115), Does::Button(Button::Rewind)),
        (Msg::Cc(0, 116), Does::Button(Button::Forward)),
        (Msg::Cc(0, 117), Does::Button(Button::Stop)),
    ];
    let mut c = [(Msg::Cc(0, 118), Does::Button(Button::Play)); 13 + 8 * 3 + 16];
    let mut i = 0;
    while i < 13 {
        c[i] = fixed[i];
        i += 1;
    }
    let mut k = 0;
    while k < 8 {
        let s = k as u8;
        c[i] = (Msg::Cc(0, 12 + s), Does::Fader(k));
        c[i + 1] = (Msg::Cc(0, 22 + s), Does::Knob(k, Encoding::Absolute));
        c[i + 2] = (Msg::Cc(0, 32 + s), Does::Strip(StripButton::Select, k));
        i += 3;
        k += 1;
    }
    let mut row = 0;
    while row < 2 {
        let mut p = 0;
        while p < 8 {
            c[i] = (Msg::Note(0, PAD_NOTES[row][p]), Does::Slot(p, row));
            i += 1;
            p += 1;
        }
        row += 1;
    }
    c
};

/// Play and Record come last (the table's filler is Play).
static OXYGEN_ALL: [Control; 13 + 8 * 3 + 16 + 1] = {
    let mut c = [(Msg::Cc(0, 119), Does::Button(Button::Record)); 13 + 8 * 3 + 16 + 1];
    let mut i = 0;
    while i < 13 + 8 * 3 + 16 {
        c[i] = OXYGEN_CONTROLS[i];
        i += 1;
    }
    c
};

/// Pad colours (velocities): green, red, amber, white, and blinking.
fn pad_velocity(kind: SlotKind) -> u8 {
    match kind {
        SlotKind::Empty => 0,
        SlotKind::Armed => 3,
        SlotKind::Clip => 11,
        SlotKind::Queued => 76,
        SlotKind::Playing => 12,
        SlotKind::Stopping => 76,
        SlotKind::Recording => 67,
    }
}

fn oxygen_feedback(state: &SurfaceState, f: &mut Feedback) {
    let on = |b: bool| if b { 127 } else { 0 };
    f.cc(0, 118, on(state.playing));
    f.cc(0, 119, on(state.recording));
    f.cc(0, 114, on(state.looping));
    f.cc(0, 106, on(state.click));
    for i in 0..8 {
        let sel = state.strips.get(i).is_some_and(|s| s.present && s.selected);
        f.cc(0, 32 + i as u8, on(sel));
    }
    for (row, notes) in PAD_NOTES.iter().enumerate() {
        for (strip, note) in notes.iter().enumerate() {
            let v = state
                .launcher
                .slot(strip, row)
                .map_or(0, |s| pad_velocity(s.kind));
            f.note(0, *note, v);
        }
    }
}

/// Firmware mode, control modes, LEDs from the host.
macro_rules! oxygen_hello {
    ($mode:expr) => {
        &[
            &[
                0xF0, 0x00, 0x01, 0x05, 0x7F, 0x00, 0x00, 0x6D, 0x00, 0x01, $mode, 0xF7,
            ],
            &[
                0xF0, 0x00, 0x01, 0x05, 0x7F, 0x00, 0x00, 0x6E, 0x00, 0x01, 0x02, 0xF7,
            ],
            &[
                0xF0, 0x00, 0x01, 0x05, 0x7F, 0x00, 0x00, 0x6E, 0x00, 0x01, 0x07, 0xF7,
            ],
            &[
                0xF0, 0x00, 0x01, 0x05, 0x7F, 0x00, 0x00, 0x6B, 0x00, 0x01, 0x01, 0xF7,
            ],
            &[
                0xF0, 0x00, 0x01, 0x05, 0x7F, 0x00, 0x00, 0x6C, 0x00, 0x01, 0x03, 0xF7,
            ],
        ]
    };
}

/// LEDs back to the keyboard.
const OXYGEN_GOODBYE: &[&[u8]] = &[&[
    0xF0, 0x00, 0x01, 0x05, 0x7F, 0x00, 0x00, 0x6C, 0x00, 0x01, 0x00, 0xF7,
]];

/// Oxygen Pro 25/49/61, its third ("Mackie/HUI") port.
pub static OXYGEN_PRO: Profile = Profile {
    hello: oxygen_hello!(0x02),
    goodbye: OXYGEN_GOODBYE,
    scenes: 2,
    ..Profile::new("M-Audio Oxygen Pro", 8, &OXYGEN_ALL, oxygen_feedback)
};

/// Oxygen 25/49/61 MKV, its second port.
pub static OXYGEN_MKV: Profile = Profile {
    hello: oxygen_hello!(0x00),
    goodbye: OXYGEN_GOODBYE,
    scenes: 2,
    ..Profile::new("M-Audio Oxygen MKV", 8, &OXYGEN_ALL, oxygen_feedback)
};

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Protocol;
    use crate::mapped::Mapped;

    #[test]
    fn the_oxygen_pro_switches_to_daw_mode_and_reads_its_controls() {
        let mut p = Mapped::new(&OXYGEN_PRO);
        let mut out = Vec::new();
        p.update(&SurfaceState::default(), 0.0, &mut out);
        assert_eq!(out[0][7..11], [0x6D, 0x00, 0x01, 0x02]);
        let mut inputs = Vec::new();
        p.receive(&[0xB0, 13, 127], &mut inputs);
        p.receive(&[0xB0, 41, 0], &mut inputs);
        p.receive(&[0xB0, 119, 127], &mut inputs);
        p.receive(&[0x90, 36, 90], &mut inputs);
        assert_eq!(
            inputs,
            [
                SurfaceInput::Fader {
                    strip: 1,
                    travel: 1.0
                },
                SurfaceInput::Fader {
                    strip: MASTER,
                    travel: 0.0
                },
                SurfaceInput::Button {
                    button: Button::Record,
                    pressed: true
                },
                SurfaceInput::LaunchClip { strip: 0, scene: 1 },
            ]
        );
    }
}
