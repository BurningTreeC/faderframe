//! Novation's keyboards in DAW mode, on their "DAW" port: the Launchkey
//! MK3 (25/37/49/61, the 88), the Launchkey Mini MK3, the Launchkey MK4
//! (and Mini MK4), and the SL MkIII's InControl port. Pots or encoders
//! and faders on the mixer's strips, the transport, the session pads (two
//! rows) on the clip launcher, lit from the shared 128-colour palette
//! ([`crate::grid::nearest`]) — static on channel 1, flashing on 2,
//! pulsing on 3 — and the display (the selected track, the position).
//!
//! Sources: Novation's *Launchkey [MK3] Programmer's Reference Guide* v1,
//! *Launchkey MK4 Programmer's Reference Guide* v2.0 and *SL MkIII
//! Programmer's Reference Guide*; for the Mini MK3 (no vendor guide)
//! Ableton Live 12's `Launchkey_Mini_MK3`, DrivenByMoss and Bitwig's
//! extension, which agree. Track ◄/► on the Launchkeys follow the scripts
//! (103 left, 102 right), not the guides' drawing; on the SL MkIII the
//! guide's (0x66 left).

use crate::grid::nearest;
use crate::mapped::{
    Control, Does, Encoding, Feedback, Msg, Profile, ascii, position_text, shown_strip, sysex,
};
use crate::{
    Button, EditButton, MASTER, Modifier, SlotKind, SlotState, SurfaceInput, SurfaceState,
};

/// How a launcher slot is lit: palette entry and channel (0 static, 1
/// flashing, 2 pulsing).
pub fn slot_light(s: Option<&SlotState>) -> (u8, u8) {
    const GREEN: u8 = 21;
    const RED: u8 = 5;
    let Some(s) = s else { return (0, 0) };
    match s.kind {
        SlotKind::Empty => (0, 0),
        SlotKind::Armed => (7, 0),
        SlotKind::Clip => (nearest(s.color).max(1), 0),
        SlotKind::Queued => (nearest(s.color).max(1), 1),
        SlotKind::Playing => (GREEN, 2),
        SlotKind::Stopping => (GREEN, 1),
        SlotKind::Recording => (RED, 2),
    }
}

/// The session pads' notes: top row then bottom row (scene 0, 1).
const PAD_ROWS: [u8; 2] = [0x60, 0x70];

/// Light the session pads (notes on channel 1–3 by how they are lit).
fn pads(state: &SurfaceState, f: &mut Feedback) {
    for (row, base) in PAD_ROWS.iter().enumerate() {
        for strip in 0..8 {
            let (colour, ch) = slot_light(state.launcher.slot(strip, row));
            let note = base + strip as u8;
            // One key per pad: the channel may change.
            f.set(0x4000 | u32::from(note), vec![0x90 | ch, note, colour]);
        }
    }
}

const fn pad_controls() -> [Control; 16] {
    let mut c = [(Msg::Note(0, 0), Does::Slot(0, 0)); 16];
    let mut i = 0;
    while i < 8 {
        c[i] = (Msg::Note(0, 0x60 + i as u8), Does::Slot(i, 0));
        c[8 + i] = (Msg::Note(0, 0x70 + i as u8), Does::Slot(i, 1));
        i += 1;
    }
    c
}

const PADS: [Control; 16] = pad_controls();

// --- Launchkey MK3 -----------------------------------------------------------

/// DAW mode on, then off (`9F 0C 7F/00`).
const DAW_ON: &[u8] = &[0x9F, 0x0C, 0x7F];
const DAW_OFF: &[u8] = &[0x9F, 0x0C, 0x00];

const MK3_CONTROLS: &[Control] = &[
    (Msg::Cc(15, 21), Does::Knob(0, Encoding::Absolute)),
    (Msg::Cc(15, 22), Does::Knob(1, Encoding::Absolute)),
    (Msg::Cc(15, 23), Does::Knob(2, Encoding::Absolute)),
    (Msg::Cc(15, 24), Does::Knob(3, Encoding::Absolute)),
    (Msg::Cc(15, 25), Does::Knob(4, Encoding::Absolute)),
    (Msg::Cc(15, 26), Does::Knob(5, Encoding::Absolute)),
    (Msg::Cc(15, 27), Does::Knob(6, Encoding::Absolute)),
    (Msg::Cc(15, 28), Does::Knob(7, Encoding::Absolute)),
    (Msg::Cc(15, 53), Does::Fader(0)),
    (Msg::Cc(15, 54), Does::Fader(1)),
    (Msg::Cc(15, 55), Does::Fader(2)),
    (Msg::Cc(15, 56), Does::Fader(3)),
    (Msg::Cc(15, 57), Does::Fader(4)),
    (Msg::Cc(15, 58), Does::Fader(5)),
    (Msg::Cc(15, 59), Does::Fader(6)),
    (Msg::Cc(15, 60), Does::Fader(7)),
    (Msg::Cc(15, 61), Does::Fader(MASTER)),
    // The buttons under the faders select their tracks.
    (
        Msg::Cc(15, 37),
        Does::Strip(crate::mapped::StripButton::Select, 0),
    ),
    (
        Msg::Cc(15, 38),
        Does::Strip(crate::mapped::StripButton::Select, 1),
    ),
    (
        Msg::Cc(15, 39),
        Does::Strip(crate::mapped::StripButton::Select, 2),
    ),
    (
        Msg::Cc(15, 40),
        Does::Strip(crate::mapped::StripButton::Select, 3),
    ),
    (
        Msg::Cc(15, 41),
        Does::Strip(crate::mapped::StripButton::Select, 4),
    ),
    (
        Msg::Cc(15, 42),
        Does::Strip(crate::mapped::StripButton::Select, 5),
    ),
    (
        Msg::Cc(15, 43),
        Does::Strip(crate::mapped::StripButton::Select, 6),
    ),
    (
        Msg::Cc(15, 44),
        Does::Strip(crate::mapped::StripButton::Select, 7),
    ),
    (
        Msg::Cc(15, 74),
        Does::Button(Button::Edit(EditButton::Capture)),
    ),
    (
        Msg::Cc(15, 75),
        Does::Button(Button::Edit(EditButton::Quantize)),
    ),
    (Msg::Cc(15, 76), Does::Button(Button::Click)),
    (Msg::Cc(15, 77), Does::Button(Button::Undo)),
    (Msg::Cc(15, 115), Does::Button(Button::Play)),
    (Msg::Cc(15, 116), Does::Button(Button::Stop)),
    (Msg::Cc(15, 117), Does::Button(Button::Record)),
    (Msg::Cc(15, 118), Does::Button(Button::Loop)),
    (Msg::Cc(15, 103), Does::Button(Button::ChannelLeft)),
    (Msg::Cc(15, 102), Does::Button(Button::ChannelRight)),
    (Msg::Cc(15, 106), Does::Button(Button::SceneUp)),
    (Msg::Cc(15, 107), Does::Button(Button::SceneDown)),
    (Msg::Cc(0, 104), Does::Input(SurfaceInput::LaunchScene(0))),
    (Msg::Cc(0, 105), Does::Input(SurfaceInput::StopClips)),
    (
        Msg::Cc(0, 108),
        Does::Button(Button::Modifier(Modifier::Shift)),
    ),
];

/// The Launchkey MK3's controls and its session pads.
const fn mk3_all() -> [Control; MK3_CONTROLS.len() + 16] {
    let mut c = [(Msg::Note(0, 0), Does::Slot(0, 0)); MK3_CONTROLS.len() + 16];
    let mut i = 0;
    while i < MK3_CONTROLS.len() {
        c[i] = MK3_CONTROLS[i];
        i += 1;
    }
    let mut k = 0;
    while k < 16 {
        c[i + k] = PADS[k];
        k += 1;
    }
    c
}

static MK3_ALL: [Control; MK3_CONTROLS.len() + 16] = mk3_all();

/// The 16 × 2 LCD's default display (`<hdr> 04 <row> <text>`).
fn mk3_lcd(state: &SurfaceState, f: &mut Feedback, header: &[u8]) {
    let shown = shown_strip(state);
    let top = shown.map_or_else(|| "FaderFrame".to_string(), |s| s.name.clone());
    for (row, text) in [(0u8, top), (1, position_text(state))] {
        let mut m = vec![0x04, row];
        m.extend(ascii(&text, 16));
        f.set(0x2000 | u32::from(row), sysex(header, &m));
    }
}

/// The selected track's fader button lit white, the others off.
fn fader_buttons(state: &SurfaceState, f: &mut Feedback) {
    for i in 0..8 {
        let on = state.strips.get(i).is_some_and(|s| s.present && s.selected);
        f.cc(0, 37 + i as u8, if on { 3 } else { 0 });
    }
}

fn mk3_feedback(state: &SurfaceState, f: &mut Feedback) {
    pads(state, f);
    fader_buttons(state, f);
    mk3_lcd(state, f, &[0xF0, 0x00, 0x20, 0x29, 0x02, 0x0F]);
}

fn mk3_88_feedback(state: &SurfaceState, f: &mut Feedback) {
    pads(state, f);
    fader_buttons(state, f);
    mk3_lcd(state, f, &[0xF0, 0x00, 0x20, 0x29, 0x02, 0x12]);
}

/// Launchkey MK3 25/37/49/61 (the faders on 49/61).
pub static LAUNCHKEY_MK3: Profile = Profile {
    hello: &[DAW_ON],
    goodbye: &[DAW_OFF],
    scenes: 2,
    ..Profile::new("Novation Launchkey MK3", 8, &MK3_ALL, mk3_feedback)
};

/// Launchkey 88 MK3 (another SysEx header for its display).
pub static LAUNCHKEY_MK3_88: Profile = Profile {
    hello: &[DAW_ON],
    goodbye: &[DAW_OFF],
    scenes: 2,
    ..Profile::new("Novation Launchkey 88 MK3", 8, &MK3_ALL, mk3_88_feedback)
};

// --- Launchkey Mini MK3 ------------------------------------------------------

const MINI_MK3_CONTROLS: [Control; 13 + 16] = {
    let head: [Control; 13] = [
        (Msg::Cc(15, 21), Does::Knob(0, Encoding::Absolute)),
        (Msg::Cc(15, 22), Does::Knob(1, Encoding::Absolute)),
        (Msg::Cc(15, 23), Does::Knob(2, Encoding::Absolute)),
        (Msg::Cc(15, 24), Does::Knob(3, Encoding::Absolute)),
        (Msg::Cc(15, 25), Does::Knob(4, Encoding::Absolute)),
        (Msg::Cc(15, 26), Does::Knob(5, Encoding::Absolute)),
        (Msg::Cc(15, 27), Does::Knob(6, Encoding::Absolute)),
        (Msg::Cc(15, 28), Does::Knob(7, Encoding::Absolute)),
        (Msg::Cc(15, 115), Does::Button(Button::Play)),
        (Msg::Cc(15, 117), Does::Button(Button::Record)),
        (Msg::Cc(0, 104), Does::Input(SurfaceInput::LaunchScene(0))),
        (Msg::Cc(0, 105), Does::Input(SurfaceInput::StopClips)),
        (
            Msg::Cc(0, 108),
            Does::Button(Button::Modifier(Modifier::Shift)),
        ),
    ];
    let mut c = [(Msg::Note(0, 0), Does::Slot(0, 0)); 13 + 16];
    let mut i = 0;
    while i < 13 {
        c[i] = head[i];
        i += 1;
    }
    let mut k = 0;
    while k < 16 {
        c[13 + k] = PADS[k];
        k += 1;
    }
    c
};

fn mini_mk3_feedback(state: &SurfaceState, f: &mut Feedback) {
    pads(state, f);
}

/// Launchkey Mini MK3 (no display, no faders).
pub static LAUNCHKEY_MINI_MK3: Profile = Profile {
    hello: &[DAW_ON],
    goodbye: &[DAW_OFF],
    scenes: 2,
    ..Profile::new(
        "Novation Launchkey Mini MK3",
        8,
        &MINI_MK3_CONTROLS,
        mini_mk3_feedback,
    )
};

// --- Launchkey MK4 -----------------------------------------------------------

/// MK4: relative encoders (`B6 45 7F`: CC 85–92 around 64), faders CC
/// 5–13, Shift on channel 7.
const MK4_CONTROLS: &[Control] = &[
    (Msg::Cc(15, 85), Does::Knob(0, Encoding::Offset64)),
    (Msg::Cc(15, 86), Does::Knob(1, Encoding::Offset64)),
    (Msg::Cc(15, 87), Does::Knob(2, Encoding::Offset64)),
    (Msg::Cc(15, 88), Does::Knob(3, Encoding::Offset64)),
    (Msg::Cc(15, 89), Does::Knob(4, Encoding::Offset64)),
    (Msg::Cc(15, 90), Does::Knob(5, Encoding::Offset64)),
    (Msg::Cc(15, 91), Does::Knob(6, Encoding::Offset64)),
    (Msg::Cc(15, 92), Does::Knob(7, Encoding::Offset64)),
    (Msg::Cc(15, 5), Does::Fader(0)),
    (Msg::Cc(15, 6), Does::Fader(1)),
    (Msg::Cc(15, 7), Does::Fader(2)),
    (Msg::Cc(15, 8), Does::Fader(3)),
    (Msg::Cc(15, 9), Does::Fader(4)),
    (Msg::Cc(15, 10), Does::Fader(5)),
    (Msg::Cc(15, 11), Does::Fader(6)),
    (Msg::Cc(15, 12), Does::Fader(7)),
    (Msg::Cc(15, 13), Does::Fader(MASTER)),
    (
        Msg::Cc(15, 37),
        Does::Strip(crate::mapped::StripButton::Select, 0),
    ),
    (
        Msg::Cc(15, 38),
        Does::Strip(crate::mapped::StripButton::Select, 1),
    ),
    (
        Msg::Cc(15, 39),
        Does::Strip(crate::mapped::StripButton::Select, 2),
    ),
    (
        Msg::Cc(15, 40),
        Does::Strip(crate::mapped::StripButton::Select, 3),
    ),
    (
        Msg::Cc(15, 41),
        Does::Strip(crate::mapped::StripButton::Select, 4),
    ),
    (
        Msg::Cc(15, 42),
        Does::Strip(crate::mapped::StripButton::Select, 5),
    ),
    (
        Msg::Cc(15, 43),
        Does::Strip(crate::mapped::StripButton::Select, 6),
    ),
    (
        Msg::Cc(15, 44),
        Does::Strip(crate::mapped::StripButton::Select, 7),
    ),
    (Msg::Cc(15, 51), Does::Button(Button::BankLeft)),
    (Msg::Cc(15, 52), Does::Button(Button::BankRight)),
    (
        Msg::Cc(15, 74),
        Does::Button(Button::Edit(EditButton::Capture)),
    ),
    (
        Msg::Cc(15, 75),
        Does::Button(Button::Edit(EditButton::Quantize)),
    ),
    (Msg::Cc(15, 76), Does::Button(Button::Click)),
    (Msg::Cc(15, 77), Does::Button(Button::Undo)),
    (Msg::Cc(15, 115), Does::Button(Button::Play)),
    (Msg::Cc(15, 116), Does::Button(Button::Stop)),
    (Msg::Cc(15, 117), Does::Button(Button::Record)),
    (Msg::Cc(15, 118), Does::Button(Button::Loop)),
    (Msg::Cc(15, 103), Does::Button(Button::ChannelLeft)),
    (Msg::Cc(15, 102), Does::Button(Button::ChannelRight)),
    (Msg::Cc(15, 106), Does::Button(Button::SceneUp)),
    (Msg::Cc(15, 107), Does::Button(Button::SceneDown)),
    (Msg::Cc(15, 104), Does::Input(SurfaceInput::LaunchScene(0))),
    (Msg::Cc(15, 105), Does::Input(SurfaceInput::StopClips)),
    (
        Msg::Cc(6, 63),
        Does::Button(Button::Modifier(Modifier::Shift)),
    ),
];

const fn mk4_all() -> [Control; MK4_CONTROLS.len() + 16] {
    let mut c = [(Msg::Note(0, 0), Does::Slot(0, 0)); MK4_CONTROLS.len() + 16];
    let mut i = 0;
    while i < MK4_CONTROLS.len() {
        c[i] = MK4_CONTROLS[i];
        i += 1;
    }
    let mut k = 0;
    while k < 16 {
        c[i + k] = PADS[k];
        k += 1;
    }
    c
}

static MK4_ALL: [Control; MK4_CONTROLS.len() + 16] = mk4_all();

/// The OLED's stationary display (target 0x20): name and value fields.
fn mk4_display(state: &SurfaceState, f: &mut Feedback, header: &[u8]) {
    let shown = shown_strip(state);
    let top = shown.map_or_else(|| "FaderFrame".to_string(), |s| s.name.clone());
    for (field, text) in [(0u8, top), (1, position_text(state))] {
        let mut m = vec![0x06, 0x20, field];
        m.extend(ascii(&text, 16));
        f.set(0x2000 | u32::from(field), sysex(header, &m));
    }
}

const MK4: &[u8] = &[0xF0, 0x00, 0x20, 0x29, 0x02, 0x14];
const MINI_MK4: &[u8] = &[0xF0, 0x00, 0x20, 0x29, 0x02, 0x13];

fn mk4_feedback(state: &SurfaceState, f: &mut Feedback) {
    pads(state, f);
    fader_buttons(state, f);
    mk4_display(state, f, MK4);
}

fn mini_mk4_feedback(state: &SurfaceState, f: &mut Feedback) {
    pads(state, f);
    mk4_display(state, f, MINI_MK4);
}

/// Launchkey MK4 25/37/49/61/88: DAW mode, relative encoders, the
/// stationary display arranged as name and value.
pub static LAUNCHKEY_MK4: Profile = Profile {
    hello: &[
        DAW_ON,
        &[0xB6, 0x45, 0x7F],
        &[0xF0, 0x00, 0x20, 0x29, 0x02, 0x14, 0x04, 0x20, 0x61, 0xF7],
    ],
    goodbye: &[DAW_OFF],
    scenes: 2,
    ..Profile::new("Novation Launchkey MK4", 8, &MK4_ALL, mk4_feedback)
};

/// The Mini MK4: the MK4's controls it has (no faders), its own header.
pub static LAUNCHKEY_MINI_MK4: Profile = Profile {
    hello: &[
        DAW_ON,
        &[0xB6, 0x45, 0x7F],
        &[0xF0, 0x00, 0x20, 0x29, 0x02, 0x13, 0x04, 0x20, 0x61, 0xF7],
    ],
    goodbye: &[DAW_OFF],
    scenes: 2,
    ..Profile::new(
        "Novation Launchkey Mini MK4",
        8,
        &MK4_ALL,
        mini_mk4_feedback,
    )
};

// --- SL MkIII ----------------------------------------------------------------

/// Fixed controls of the SL MkIII (before the strips' and the pads').
const FIXED: usize = 17;

/// InControl: knobs relative (two's complement), faders absolute, the
/// soft buttons under the screens select, the right-hand rows mute and
/// solo; pads on channel 16.
const SL_CONTROLS: &[Control] = &{
    use crate::mapped::StripButton::{Mute, Select, Solo};
    let fixed: [Control; FIXED] = [
        (Msg::Cc(15, 0x66), Does::Button(Button::ChannelLeft)),
        (Msg::Cc(15, 0x67), Does::Button(Button::ChannelRight)),
        (Msg::Cc(15, 0x70), Does::Button(Button::Rewind)),
        (Msg::Cc(15, 0x71), Does::Button(Button::Forward)),
        (Msg::Cc(15, 0x72), Does::Button(Button::Stop)),
        (Msg::Cc(15, 0x73), Does::Button(Button::Play)),
        (Msg::Cc(15, 0x74), Does::Button(Button::Loop)),
        (Msg::Cc(15, 0x75), Does::Button(Button::Record)),
        (
            Msg::Cc(15, 0x5B),
            Does::Button(Button::Modifier(Modifier::Shift)),
        ),
        (Msg::Cc(15, 0x51), Does::Button(Button::BankLeft)),
        (Msg::Cc(15, 0x52), Does::Button(Button::BankRight)),
        (Msg::Cc(15, 0x55), Does::Button(Button::SceneUp)),
        (Msg::Cc(15, 0x56), Does::Button(Button::SceneDown)),
        (Msg::Cc(15, 0x53), Does::Input(SurfaceInput::LaunchScene(0))),
        (Msg::Cc(15, 0x54), Does::Input(SurfaceInput::LaunchScene(1))),
        // Duplicate and Clear.
        (
            Msg::Cc(15, 0x5C),
            Does::Button(Button::Edit(EditButton::Copy)),
        ),
        (
            Msg::Cc(15, 0x5D),
            Does::Button(Button::Edit(EditButton::Delete)),
        ),
    ];
    let mut c = [(Msg::Note(0, 0), Does::Slot(0, 0)); FIXED + 8 * 5 + 16];
    let mut i = 0;
    while i < FIXED {
        c[i] = fixed[i];
        i += 1;
    }
    let mut k = 0;
    while k < 8 {
        let s = k as u8;
        c[i] = (
            Msg::Cc(15, 0x15 + s),
            Does::Knob(k, Encoding::TwosComplement),
        );
        c[i + 1] = (Msg::Cc(15, 0x29 + s), Does::Fader(k));
        c[i + 2] = (Msg::Cc(15, 0x33 + s), Does::Strip(Select, k));
        c[i + 3] = (Msg::Cc(15, 0x3B + s), Does::Strip(Mute, k));
        c[i + 4] = (Msg::Cc(15, 0x43 + s), Does::Strip(Solo, k));
        i += 5;
        k += 1;
    }
    let mut p = 0;
    while p < 8 {
        c[i + p] = (Msg::Note(15, 0x60 + p as u8), Does::Slot(p, 0));
        c[i + 8 + p] = (Msg::Note(15, 0x70 + p as u8), Does::Slot(p, 1));
        p += 1;
    }
    c
};

const SL: &[u8] = &[0xF0, 0x00, 0x20, 0x29, 0x02, 0x0A, 0x01];

fn sl_feedback(state: &SurfaceState, f: &mut Feedback) {
    // The knob layout, a column per strip: the track's name, its level,
    // the knob where the pot is.
    f.set(0x1000, sysex(SL, &[0x01, 0x01]));
    for (i, s) in state.strips.iter().take(8).enumerate() {
        let col = i as u8;
        let (name, level) = if s.present {
            (s.name.clone(), s.level.clone())
        } else {
            (String::new(), String::new())
        };
        for (obj, text) in [(0u8, name), (1, level)] {
            let mut m = vec![0x02, col, 0x01, obj];
            m.extend(ascii(&text, 9));
            m.push(0x00);
            f.set(0x2000 | u32::from(col) << 4 | u32::from(obj), sysex(SL, &m));
        }
        // The knob's value (ch 16, the knob's own CC).
        let v = if s.present {
            (s.pot.clamp(0.0, 1.0) * 127.0).round() as u8
        } else {
            0
        };
        f.cc(15, 0x15 + col, v);
        // The LEDs: the track's colour over its fader; select, mute, solo.
        let colour = if s.present {
            nearest(s.color).max(3)
        } else {
            0
        };
        f.cc(15, 0x29 + col, colour);
        f.cc(15, 0x33 + col, if s.present && s.selected { 3 } else { 0 });
        f.cc(15, 0x3B + col, if s.mute { 9 } else { 0 });
        f.cc(15, 0x43 + col, if s.solo { 13 } else { 0 });
    }
    f.cc(15, 0x73, if state.playing { 21 } else { 1 });
    f.cc(15, 0x75, if state.recording { 5 } else { 1 });
    f.cc(15, 0x74, if state.looping { 9 } else { 1 });
    // The pads (channel 16 static, 2 flashing, 3 pulsing).
    for (row, base) in PAD_ROWS.iter().enumerate() {
        for strip in 0..8usize {
            let (colour, ch) = slot_light(state.launcher.slot(strip, row));
            let note = base + strip as u8;
            let ch = if ch == 0 { 15 } else { ch };
            f.set(0x4000 | u32::from(note), vec![0x90 | ch, note, colour]);
        }
    }
}

/// SL MkIII, InControl (press InControl on the keyboard).
pub static SL_MK3: Profile = Profile {
    scenes: 2,
    ..Profile::new("Novation SL MkIII", 8, SL_CONTROLS, sl_feedback)
};

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mapped::Mapped;
    use crate::{LauncherView, Protocol, StripState};

    fn state() -> SurfaceState {
        let mut strips = vec![StripState::default(); 8];
        strips[2] = StripState {
            present: true,
            name: "Bass".into(),
            selected: true,
            ..StripState::default()
        };
        let mut slots = vec![vec![SlotState::default(); 2]; 8];
        slots[0][0] = SlotState {
            kind: SlotKind::Playing,
            color: [255, 0, 0],
            name: String::new(),
        };
        SurfaceState {
            strips,
            launcher: LauncherView {
                slots,
                ..LauncherView::default()
            },
            ..SurfaceState::default()
        }
    }

    #[test]
    fn a_launchkey_mk3_enters_daw_mode_and_plays_the_launcher() {
        let mut p = Mapped::new(&LAUNCHKEY_MK3);
        let mut out = Vec::new();
        p.update(&state(), 0.0, &mut out);
        assert_eq!(out[0], [0x9F, 0x0C, 0x7F]);
        // Playing: green, pulsing (channel 3).
        assert!(out.contains(&vec![0x92, 0x60, 21]));
        // The selected track's fader button.
        assert!(out.contains(&vec![0xB0, 39, 3]));
        // The display.
        assert!(out.iter().any(|m| m.windows(4).any(|w| w == b"Bass")));
        let mut inputs = Vec::new();
        p.receive(&[0x90, 0x71, 100], &mut inputs);
        p.receive(&[0x90, 0x71, 0], &mut inputs);
        p.receive(&[0xBF, 21, 0], &mut inputs);
        p.receive(&[0xBF, 102, 127], &mut inputs);
        assert_eq!(
            inputs,
            [
                SurfaceInput::LaunchClip { strip: 1, scene: 1 },
                SurfaceInput::ReleaseClip { strip: 1, scene: 1 },
                SurfaceInput::Knob {
                    strip: 0,
                    value: 0.0
                },
                SurfaceInput::Button {
                    button: Button::ChannelRight,
                    pressed: true
                },
            ]
        );
        assert_eq!(p.goodbye(), vec![vec![0x9F, 0x0C, 0x00]]);
    }

    #[test]
    fn a_launchkey_mk4_turns_its_encoders_relative() {
        let mut p = Mapped::new(&LAUNCHKEY_MK4);
        let mut out = Vec::new();
        p.update(&state(), 0.0, &mut out);
        assert_eq!(out[1], [0xB6, 0x45, 0x7F]);
        let mut inputs = Vec::new();
        p.receive(&[0xBF, 85, 0x41], &mut inputs);
        p.receive(&[0xBF, 92, 0x3E], &mut inputs);
        p.receive(&[0xB6, 63, 127], &mut inputs);
        assert_eq!(
            inputs,
            [
                SurfaceInput::Pot { strip: 0, delta: 1 },
                SurfaceInput::Pot {
                    strip: 7,
                    delta: -2
                },
                SurfaceInput::Button {
                    button: Button::Modifier(Modifier::Shift),
                    pressed: true
                },
            ]
        );
    }

    #[test]
    fn the_sl_mk3_reads_its_knobs_and_strips() {
        let mut p = Mapped::new(&SL_MK3);
        let mut inputs = Vec::new();
        p.receive(&[0xBF, 0x15, 0x7F], &mut inputs);
        p.receive(&[0xBF, 0x2B, 64], &mut inputs);
        p.receive(&[0xBF, 0x3C, 127], &mut inputs);
        p.receive(&[0x9F, 0x61, 90], &mut inputs);
        assert_eq!(
            inputs,
            [
                SurfaceInput::Pot {
                    strip: 0,
                    delta: -1
                },
                SurfaceInput::Fader {
                    strip: 2,
                    travel: 64.0 / 127.0
                },
                SurfaceInput::Button {
                    button: Button::Mute(1),
                    pressed: true
                },
                SurfaceInput::LaunchClip { strip: 1, scene: 0 },
            ]
        );
        let mut out = Vec::new();
        p.update(&state(), 0.0, &mut out);
        // The knob layout and the selected track's name on its column.
        assert_eq!(
            out[0],
            [0xF0, 0x00, 0x20, 0x29, 0x02, 0x0A, 0x01, 0x01, 0x01, 0xF7]
        );
        assert!(out.iter().any(|m| m.windows(4).any(|w| w == b"Bass")));
    }
}
