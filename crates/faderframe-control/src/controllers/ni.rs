//! Native Instruments Komplete Kontrol S MK3 on its DAW port (the
//! keyboard's second USB-MIDI port; the A series, M32 and S MK2 reach a
//! host only through NI's helper, which Linux does not have).
//!
//! NI documents no MIDI for it; three implementations that agree —
//! DrivenByMoss (`controller/ni/kontrol/mkii`), ReaKontrol
//! (`src/niMidi.cpp`) and Ableton Live 12's `Komplete_Kontrol_S_Mk3` —
//! give the protocol: everything is CC on channel 16; the host says hello
//! (`BF 01 04`), the keyboard answers `BF 01 <version>`, and nothing else
//! goes out before; buttons send presses only; the knobs are relative
//! (two's complement) volume (CC 0x50–0x57) and pan (0x58–0x5F); select,
//! mute, solo, arm name their track (0–7) in the value; button lights
//! are the button's CC with 0/1; the track names, types and states go as
//! SysEx `F0 00 21 09 00 00 44 43 01 00 <cmd> <value> <index> [text] F7`.

use crate::mapped::{Control, Does, Encoding, Feedback, Msg, Profile, StripButton, ascii, sysex};
use crate::{Button, EditButton, SurfaceState};

const KONTROL: &[u8] = &[0xF0, 0x00, 0x21, 0x09, 0x00, 0x00, 0x44, 0x43, 0x01, 0x00];

const KK_CONTROLS: [Control; 15 + 16] = {
    let fixed: [Control; 15] = [
        (Msg::Cc(15, 0x10), Does::Button(Button::Play)),
        (Msg::Cc(15, 0x11), Does::Button(Button::Start)),
        (Msg::Cc(15, 0x12), Does::Button(Button::Record)),
        (Msg::Cc(15, 0x14), Does::Button(Button::Stop)),
        (Msg::Cc(15, 0x16), Does::Button(Button::Loop)),
        (Msg::Cc(15, 0x17), Does::Button(Button::Click)),
        (Msg::Cc(15, 0x20), Does::Button(Button::Undo)),
        (Msg::Cc(15, 0x21), Does::Button(Button::Redo)),
        (
            Msg::Cc(15, 0x22),
            Does::Button(Button::Edit(EditButton::Quantize)),
        ),
        // The 4-D encoder: tracks, banks, the playhead.
        (
            Msg::Cc(15, 0x30),
            Does::Wheel(Button::Down, Button::Up, Encoding::TwosComplement),
        ),
        (
            Msg::Cc(15, 0x31),
            Does::Wheel(
                Button::BankRight,
                Button::BankLeft,
                Encoding::TwosComplement,
            ),
        ),
        (Msg::Cc(15, 0x34), Does::Jog(Encoding::TwosComplement)),
        (Msg::Cc(15, 0x42), Does::StripByValue(StripButton::Select)),
        (Msg::Cc(15, 0x43), Does::StripByValue(StripButton::Mute)),
        (Msg::Cc(15, 0x44), Does::StripByValue(StripButton::Solo)),
    ];
    let mut c = [(Msg::Note(0, 0), Does::Fader(0)); 15 + 16];
    let mut i = 0;
    while i < 15 {
        c[i] = fixed[i];
        i += 1;
    }
    let mut k = 0;
    while k < 8 {
        c[15 + k] = (
            Msg::Cc(15, 0x50 + k as u8),
            Does::VolumeTicks(k, Encoding::TwosComplement),
        );
        c[23 + k] = (
            Msg::Cc(15, 0x58 + k as u8),
            Does::PanTicks(k, Encoding::TwosComplement),
        );
        k += 1;
    }
    c
};

/// The arm button names its track too (one more than the table holds).
const ARM: Control = (Msg::Cc(15, 0x45), Does::StripByValue(StripButton::Arm));

const fn kk_all() -> [Control; 15 + 16 + 1] {
    let mut c = [ARM; 15 + 16 + 1];
    let mut i = 0;
    while i < 15 + 16 {
        c[i] = KK_CONTROLS[i];
        i += 1;
    }
    c
}

static KK_ALL: [Control; 15 + 16 + 1] = kk_all();

fn kk_feedback(state: &SurfaceState, f: &mut Feedback) {
    let on = |b: bool| u8::from(b);
    f.cc(15, 0x10, on(state.playing));
    f.cc(15, 0x12, on(state.recording));
    f.cc(15, 0x16, on(state.looping));
    f.cc(15, 0x17, on(state.click));
    f.cc(15, 0x22, on(state.input_quantize));
    f.cc(15, 0x14, on(!state.playing));
    // Navigation possible both ways.
    f.cc(15, 0x30, 3);
    f.cc(15, 0x31, 3);
    for (i, s) in state.strips.iter().take(8).enumerate() {
        let idx = i as u8;
        let mut cmd = |cmd: u8, value: u8, text: &str| {
            let mut m = vec![cmd, value, idx];
            m.extend(ascii(text, 32));
            f.set(
                0x1000 | u32::from(cmd) << 4 | u32::from(idx),
                sysex(KONTROL, &m),
            );
        };
        // Generic track, or an empty slot.
        cmd(0x40, if s.present { 1 } else { 0 }, "");
        cmd(0x48, 0, if s.present { &s.name } else { "" });
        cmd(0x42, u8::from(s.present && s.selected), "");
        cmd(0x43, u8::from(s.mute), "");
        cmd(0x44, u8::from(s.solo), "");
        cmd(0x45, u8::from(s.arm), "");
        cmd(0x46, 0, &s.level);
        // The knobs' rings: volume and pan.
        f.cc(
            15,
            0x50 + idx,
            (s.fader.clamp(0.0, 1.0) * 127.0).round() as u8,
        );
        f.cc(
            15,
            0x58 + idx,
            ((s.pan.clamp(-1.0, 1.0) + 1.0) * 63.5).round() as u8,
        );
    }
}

/// Komplete Kontrol S49/61/88 MK3, DAW port.
pub static KOMPLETE_KONTROL_MK3: Profile = Profile {
    hello: &[&[0xBF, 0x01, 0x04]],
    goodbye: &[&[0xBF, 0x02, 0x00]],
    answer: Some(&[0xBF, 0x01]),
    press_only: true,
    ..Profile::new(
        "Native Instruments Komplete Kontrol S MK3",
        8,
        &KK_ALL,
        kk_feedback,
    )
};

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mapped::Mapped;
    use crate::{Protocol, StripState, SurfaceInput};

    #[test]
    fn the_kontrol_waits_for_its_answer_then_shows_the_tracks() {
        let mut p = Mapped::new(&KOMPLETE_KONTROL_MK3);
        let mut strips = vec![StripState::default(); 8];
        strips[0] = StripState {
            present: true,
            name: "Keys".into(),
            fader: 0.5,
            ..StripState::default()
        };
        let state = SurfaceState {
            strips,
            ..SurfaceState::default()
        };
        let mut out = Vec::new();
        p.update(&state, 0.0, &mut out);
        assert_eq!(out, vec![vec![0xBF, 0x01, 0x04]], "only the hello");
        let mut inputs = Vec::new();
        p.receive(&[0xBF, 0x01, 0x04], &mut inputs);
        assert_eq!(inputs, [SurfaceInput::Refresh]);
        out.clear();
        p.update(&state, 0.1, &mut out);
        assert!(out.iter().any(|m| m.windows(4).any(|w| w == b"Keys")));
        assert!(out.contains(&vec![0xBF, 0x50, 64]));
        // Buttons: presses only (a release made up); a track by value.
        inputs.clear();
        p.receive(&[0xBF, 0x10, 1], &mut inputs);
        p.receive(&[0xBF, 0x43, 2], &mut inputs);
        p.receive(&[0xBF, 0x50, 0x02], &mut inputs);
        assert_eq!(
            inputs,
            [
                SurfaceInput::Button {
                    button: Button::Play,
                    pressed: true
                },
                SurfaceInput::Button {
                    button: Button::Play,
                    pressed: false
                },
                SurfaceInput::Button {
                    button: Button::Mute(2),
                    pressed: true
                },
                SurfaceInput::Button {
                    button: Button::Mute(2),
                    pressed: false
                },
                SurfaceInput::Fader {
                    strip: 0,
                    travel: 0.51
                },
            ]
        );
    }

    /// Its Quantize button reaches the host, and its light shows the
    /// transport's Q (input quantize).
    #[test]
    fn the_quantize_light_follows_input_quantize() {
        let mut p = Mapped::new(&KOMPLETE_KONTROL_MK3);
        let mut inputs = Vec::new();
        p.receive(&[0xBF, 0x01, 0x04], &mut inputs);
        inputs.clear();
        p.receive(&[0xBF, 0x22, 1], &mut inputs);
        assert_eq!(
            inputs[0],
            SurfaceInput::Button {
                button: Button::Edit(EditButton::Quantize),
                pressed: true
            }
        );
        let mut out = Vec::new();
        let on = SurfaceState {
            input_quantize: true,
            ..SurfaceState::default()
        };
        p.update(&on, 0.1, &mut out);
        assert!(out.contains(&vec![0xBF, 0x22, 1]), "lit");
        out.clear();
        p.update(&SurfaceState::default(), 0.2, &mut out);
        assert!(out.contains(&vec![0xBF, 0x22, 0]), "dark");
    }
}
