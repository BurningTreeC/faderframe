//! Akai MPK mini Plus: no DAW mode, but its transport sends Akai's CC
//! convention (Rewind 115, Fast Forward 116, Stop 117, Play 118, Record
//! 119 on channel 1, lit by notes of the same number) and its knobs CC
//! 70–77 — on the keys' port, so the profile claims those and the keys
//! still play.
//!
//! Sources: Akai's *MPD and MPK Series – Transport Controls MIDI Details*
//! (the CC convention), the *MPK mini Plus User Guide* v1.2 (transport
//! "On/Off": 127 pressed, 0 released), Bitwig's
//! `MpkMiniPlusControllerExtension` and Ardour's `AKAI_MPKminiplus.map`
//! (the knobs, the lights). The knobs' factory mode is absolute.

use crate::mapped::{Control, Does, Encoding, Feedback, Msg, Profile};
use crate::{Button, SurfaceState};

const MPK_CONTROLS: &[Control] = &[
    (Msg::Cc(0, 115), Does::Button(Button::Rewind)),
    (Msg::Cc(0, 116), Does::Button(Button::Forward)),
    (Msg::Cc(0, 117), Does::Button(Button::Stop)),
    (Msg::Cc(0, 118), Does::Button(Button::Play)),
    (Msg::Cc(0, 119), Does::Button(Button::Record)),
    (Msg::Cc(0, 70), Does::Knob(0, Encoding::Absolute)),
    (Msg::Cc(0, 71), Does::Knob(1, Encoding::Absolute)),
    (Msg::Cc(0, 72), Does::Knob(2, Encoding::Absolute)),
    (Msg::Cc(0, 73), Does::Knob(3, Encoding::Absolute)),
    (Msg::Cc(0, 74), Does::Knob(4, Encoding::Absolute)),
    (Msg::Cc(0, 75), Does::Knob(5, Encoding::Absolute)),
    (Msg::Cc(0, 76), Does::Knob(6, Encoding::Absolute)),
    (Msg::Cc(0, 77), Does::Knob(7, Encoding::Absolute)),
];

fn mpk_feedback(state: &SurfaceState, f: &mut Feedback) {
    let on = |b: bool| if b { 127 } else { 0 };
    f.note(0, 118, on(state.playing));
    f.note(0, 117, on(!state.playing));
    f.note(0, 119, on(state.recording));
}

pub fn mpk_claims(m: &[u8]) -> bool {
    MPK_MINI_PLUS.takes(m)
}

/// MPK mini Plus, its MIDI port (shared with the keys).
pub static MPK_MINI_PLUS: Profile = Profile {
    claims: Some(mpk_claims),
    ..Profile::new("Akai MPK mini Plus", 8, MPK_CONTROLS, mpk_feedback)
};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_mpk_takes_its_transport_and_knobs_and_leaves_the_keys() {
        assert!(mpk_claims(&[0xB0, 118, 127]));
        assert!(mpk_claims(&[0xB0, 70, 3]));
        assert!(!mpk_claims(&[0x90, 60, 100]));
        assert!(!mpk_claims(&[0xB0, 1, 64]));
        assert!(!mpk_claims(&[0xB1, 118, 127]), "other channels play");
    }
}
