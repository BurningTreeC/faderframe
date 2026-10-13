//! Korg nanoKONTROL2 in its native mode: one fixed map on MIDI channel
//! 16 whatever the scene, and the host lights the buttons.
//!
//! Source: Korg's *nanoKONTROL2 MIDI Implementation* rev 1.00 (sections
//! 3-1(3) and 4): native mode on `F0 42 40 00 01 13 00 00 00 01 F7` (the
//! global channel 1), off with `… 00 F7`; sliders CC 0–7, knobs 16–23,
//! S/M/R 32–39/48–55/64–71, PLAY 41, STOP 42, REW 43, FF 44, REC 45,
//! CYCLE 46, TRACK ◀/▶ 58/59, MARKER SET/◀/▶ 60–62; LEDs: the same CC,
//! 0–40 off, 41–7F on.

use crate::mapped::{Control, Does, Encoding, Feedback, Msg, Profile, StripButton};
use crate::{Button, SurfaceState};

const NANO_CONTROLS: [Control; 8 * 5 + 11] = {
    let fixed: [Control; 11] = [
        (Msg::Cc(15, 41), Does::Button(Button::Play)),
        (Msg::Cc(15, 42), Does::Button(Button::Stop)),
        (Msg::Cc(15, 43), Does::Button(Button::Rewind)),
        (Msg::Cc(15, 44), Does::Button(Button::Forward)),
        (Msg::Cc(15, 45), Does::Button(Button::Record)),
        (Msg::Cc(15, 46), Does::Button(Button::Loop)),
        // TRACK ◀▶ move the bank; MARKER SET marks, ◀▶ move a strip.
        (Msg::Cc(15, 58), Does::Button(Button::BankLeft)),
        (Msg::Cc(15, 59), Does::Button(Button::BankRight)),
        (Msg::Cc(15, 60), Does::Button(Button::Marker)),
        (Msg::Cc(15, 61), Does::Button(Button::ChannelLeft)),
        (Msg::Cc(15, 62), Does::Button(Button::ChannelRight)),
    ];
    let mut c = [(Msg::Note(0, 0), Does::Fader(0)); 8 * 5 + 11];
    let mut i = 0;
    while i < 11 {
        c[i] = fixed[i];
        i += 1;
    }
    let mut k = 0;
    while k < 8 {
        let s = k as u8;
        c[i] = (Msg::Cc(15, s), Does::Fader(k));
        c[i + 1] = (Msg::Cc(15, 16 + s), Does::Knob(k, Encoding::Absolute));
        c[i + 2] = (Msg::Cc(15, 32 + s), Does::Strip(StripButton::Solo, k));
        c[i + 3] = (Msg::Cc(15, 48 + s), Does::Strip(StripButton::Mute, k));
        c[i + 4] = (Msg::Cc(15, 64 + s), Does::Strip(StripButton::Arm, k));
        i += 5;
        k += 1;
    }
    c
};

fn nano_feedback(state: &SurfaceState, f: &mut Feedback) {
    let on = |b: bool| if b { 0x7F } else { 0x00 };
    f.cc(15, 41, on(state.playing));
    f.cc(15, 42, on(!state.playing));
    f.cc(15, 45, on(state.recording));
    f.cc(15, 46, on(state.looping));
    for i in 0..8u8 {
        let s = state.strips.get(usize::from(i)).filter(|s| s.present);
        f.cc(15, 32 + i, on(s.is_some_and(|s| s.solo)));
        f.cc(15, 48 + i, on(s.is_some_and(|s| s.mute)));
        f.cc(15, 64 + i, on(s.is_some_and(|s| s.arm)));
    }
}

/// nanoKONTROL2 (native mode; global MIDI channel 1, the factory one).
pub static NANOKONTROL2: Profile = Profile {
    hello: &[&[
        0xF0, 0x42, 0x40, 0x00, 0x01, 0x13, 0x00, 0x00, 0x00, 0x01, 0xF7,
    ]],
    goodbye: &[&[
        0xF0, 0x42, 0x40, 0x00, 0x01, 0x13, 0x00, 0x00, 0x00, 0x00, 0xF7,
    ]],
    ..Profile::new("Korg nanoKONTROL2", 8, &NANO_CONTROLS, nano_feedback)
};

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mapped::Mapped;
    use crate::{Protocol, StripState, SurfaceInput};

    #[test]
    fn the_nanokontrol2_goes_native_and_lights_its_buttons() {
        let mut p = Mapped::new(&NANOKONTROL2);
        let mut strips = vec![StripState::default(); 8];
        strips[3] = StripState {
            present: true,
            mute: true,
            ..StripState::default()
        };
        let state = SurfaceState {
            strips,
            playing: true,
            ..SurfaceState::default()
        };
        let mut out = Vec::new();
        p.update(&state, 0.0, &mut out);
        assert_eq!(out[0][..7], [0xF0, 0x42, 0x40, 0x00, 0x01, 0x13, 0x00]);
        assert!(out.contains(&vec![0xBF, 41, 0x7F]));
        assert!(out.contains(&vec![0xBF, 48 + 3, 0x7F]));
        let mut inputs = Vec::new();
        p.receive(&[0xBF, 2, 127], &mut inputs);
        p.receive(&[0xBF, 19, 0], &mut inputs);
        p.receive(&[0xBF, 66, 127], &mut inputs);
        assert_eq!(
            inputs,
            [
                SurfaceInput::Fader {
                    strip: 2,
                    travel: 1.0
                },
                SurfaceInput::Knob {
                    strip: 3,
                    value: 0.0
                },
                SurfaceInput::Button {
                    button: Button::Arm(2),
                    pressed: true
                },
            ]
        );
    }
}
