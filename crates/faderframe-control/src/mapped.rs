//! Controllers described by tables: keyboards and small surfaces with a
//! vendor DAW mode (Arturia's DAW program, Novation's DAW mode, Korg's
//! native mode, NI's Kontrol protocol, M-Audio's, PreSonus's), whose
//! controls are a fixed set of CCs, notes and pitch bends.
//!
//! A [`Profile`] lists what each incoming message does ([`Does`]), the
//! messages that put the device into its DAW mode and back, and a
//! function writing its feedback (lights, displays) through a
//! [`Feedback`], which sends only what changed. Knobs that say where they
//! are become [`SurfaceInput::Knob`]s, relative ones [`SurfaceInput::Pot`]
//! ticks in the encoding they use ([`Encoding`]).
//!
//! The profiles themselves are in [`crate::controllers`], each with its
//! sources.

use crate::{Button, MASTER, Protocol, SurfaceInput, SurfaceState};
use std::collections::HashMap;

/// An incoming message, by kind, channel (0–15) and number.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Msg {
    Cc(u8, u8),
    Note(u8, u8),
    /// Pitch bend on a channel (a 14-bit fader).
    Bend(u8),
}

/// How a knob or wheel says it moved.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Encoding {
    /// Where it is, 0–127.
    Absolute,
    /// Bit 6 counter-clockwise, the rest ticks (Mackie V-pots).
    SignedBit,
    /// 7-bit two's complement: 1… clockwise, 127… counter-clockwise.
    TwosComplement,
    /// Around 64: 65… clockwise, 63… counter-clockwise.
    Offset64,
}

impl Encoding {
    /// Ticks of a relative value (`None` for absolute ones).
    pub fn ticks(self, v: u8) -> Option<i32> {
        let v = i32::from(v & 0x7F);
        match self {
            Encoding::Absolute => None,
            Encoding::SignedBit => Some(if v & 0x40 != 0 { -(v & 0x3F) } else { v }),
            Encoding::TwosComplement => Some(if v >= 64 { v - 128 } else { v }),
            Encoding::Offset64 => Some(v - 64),
        }
    }
}

/// What a strip button does (some devices send the strip as the value).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StripButton {
    Mute,
    Solo,
    Arm,
    Select,
}

impl StripButton {
    fn button(self, strip: usize) -> Button {
        match self {
            StripButton::Mute => Button::Mute(strip),
            StripButton::Solo => Button::Solo(strip),
            StripButton::Arm => Button::Arm(strip),
            StripButton::Select => Button::Select(strip),
        }
    }
}

/// What an incoming message does.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Does {
    /// Pressed while the value (or velocity) is not 0.
    Button(Button),
    /// A strip's button.
    Strip(StripButton, usize),
    /// The strip is the value (NI: "select track 3" is CC 42 = 3).
    StripByValue(StripButton),
    /// A fader of a strip ([`MASTER`] for the master): 7-bit CC or pitch
    /// bend.
    Fader(usize),
    /// A strip's knob.
    Knob(usize, Encoding),
    /// The jog wheel (the position).
    Jog(Encoding),
    /// A wheel pressing one button per tick clockwise, the other
    /// counter-clockwise (the selection up and down, the bank).
    Wheel(Button, Button, Encoding),
    /// A strip's volume turned (ticks of a relative knob).
    VolumeTicks(usize, Encoding),
    /// A strip's pan turned (ticks).
    PanTicks(usize, Encoding),
    /// The selected track's pan (a single encoder).
    SelectedPan(Encoding),
    /// A strip's fader touched (value: touched).
    Touch(usize),
    /// A launcher pad: strip and row of the shown scenes.
    Slot(usize, usize),
    /// Sends this as it is (pressed: value not 0).
    Input(SurfaceInput),
}

/// One control: the message and what it does.
pub type Control = (Msg, Does);

/// Feedback written by a profile: messages under keys, each sent again
/// only when it changes.
pub struct Feedback<'a> {
    sent: &'a mut HashMap<u32, Vec<u8>>,
    out: &'a mut Vec<Vec<u8>>,
    touched: &'a [bool],
    now: f64,
}

/// The key a motorised fader's position goes under ([`Feedback::set`]):
/// forgotten when the fader is let go, so it goes where it should be.
pub const fn fader_key(strip: usize) -> u32 {
    0x0F00_0000 | strip as u32
}

impl Feedback<'_> {
    /// Whether a strip's fader is held (motorised faders stay put).
    pub fn touched(&self, strip: usize) -> bool {
        self.touched.get(strip).copied().unwrap_or(false)
    }

    /// Seconds since the surface started (meters sent again in time).
    pub fn now(&self) -> f64 {
        self.now
    }

    /// `msg` now, whatever was sent before (meters the surface lets fall).
    pub fn send(&mut self, msg: Vec<u8>) {
        self.out.push(msg);
    }

    /// True once per `period` seconds for `key` (meters refreshed in time).
    pub fn every(&mut self, key: u32, period: f64) -> bool {
        let stamp = ((self.now / period).floor() as i64).to_le_bytes().to_vec();
        if self.sent.get(&key) == Some(&stamp) {
            return false;
        }
        self.sent.insert(key, stamp);
        true
    }

    /// `msg` under `key` (sent when it differs from the last one there).
    pub fn set(&mut self, key: u32, msg: Vec<u8>) {
        if self.sent.get(&key) != Some(&msg) {
            self.out.push(msg.clone());
            self.sent.insert(key, msg);
        }
    }

    /// A CC (channel 0–15) as a light: `on`/`off` values.
    pub fn cc(&mut self, ch: u8, cc: u8, value: u8) {
        self.set(
            0x0100_0000 | u32::from(ch) << 8 | u32::from(cc),
            vec![0xB0 | ch, cc, value],
        );
    }

    /// A note-on as a light.
    pub fn note(&mut self, ch: u8, note: u8, velocity: u8) {
        self.set(
            0x0200_0000 | u32::from(ch) << 8 | u32::from(note),
            vec![0x90 | ch, note, velocity],
        );
    }
}

/// A controller described by tables (see the module docs).
pub struct Profile {
    pub name: &'static str,
    /// Channel strips (faders, knobs) it has.
    pub strips: usize,
    pub controls: &'static [Control],
    /// Sent when it is opened (into its DAW mode) and closed.
    pub hello: &'static [&'static [u8]],
    pub goodbye: &'static [&'static [u8]],
    /// Lights and displays.
    pub feedback: fn(&SurfaceState, &mut Feedback),
    /// Sent every `.0` seconds (a device that leaves its mode otherwise).
    pub keep_alive: Option<(f64, &'static [u8])>,
    /// Buttons send a press only (no release): one is made up.
    pub press_only: bool,
    /// Its port carries the keyboard too: the messages it takes.
    pub claims: Option<fn(&[u8]) -> bool>,
    /// One strip, the selected track.
    pub follows_selection: bool,
    /// Rows of launcher slots its pads show.
    pub scenes: usize,
    /// It answers its hello with a message starting with these bytes;
    /// until then it gets the hello again each second and nothing else.
    pub answer: Option<&'static [u8]>,
}

impl Profile {
    /// A profile with the controls and nothing else to it.
    pub const fn new(
        name: &'static str,
        strips: usize,
        controls: &'static [Control],
        feedback: fn(&SurfaceState, &mut Feedback),
    ) -> Self {
        Self {
            name,
            strips,
            controls,
            hello: &[],
            goodbye: &[],
            feedback,
            keep_alive: None,
            press_only: false,
            claims: None,
            follows_selection: false,
            scenes: 0,
            answer: None,
        }
    }
}

impl Profile {
    /// What `msg` is (channel messages; the controls' first match).
    fn find(&self, msg: &[u8]) -> Option<(Does, u8)> {
        let [status, a, ..] = *msg else {
            return None;
        };
        let ch = status & 0x0F;
        let b = msg.get(2).copied().unwrap_or(0);
        let (key, value) = match status & 0xF0 {
            0xB0 => (Msg::Cc(ch, a), b),
            0x90 => (Msg::Note(ch, a), b),
            0x80 => (Msg::Note(ch, a), 0),
            0xE0 => (Msg::Bend(ch), b),
            _ => return None,
        };
        self.controls
            .iter()
            .find(|(m, _)| *m == key)
            .map(|(_, d)| (*d, value))
    }

    /// Whether `msg` is one of its controls (what a shared port keeps from
    /// the instruments).
    pub fn takes(&self, msg: &[u8]) -> bool {
        self.find(msg).is_some()
    }
}

/// A [`Profile`] running.
pub struct Mapped {
    profile: &'static Profile,
    sent: HashMap<u32, Vec<u8>>,
    greeted: bool,
    alive_at: f64,
    /// It answered its hello (or needs none).
    answered: bool,
    hello_at: f64,
    /// What the last state showed: faders' travel and pans by strip, the
    /// selected strip (relative volume and pan knobs move from there).
    faders: Vec<f32>,
    pans: Vec<f32>,
    selected: Option<usize>,
    /// Faders held (by strip).
    touched: Vec<bool>,
}

impl Mapped {
    pub fn new(profile: &'static Profile) -> Self {
        Self {
            profile,
            sent: HashMap::new(),
            greeted: false,
            alive_at: f64::NEG_INFINITY,
            answered: profile.answer.is_none(),
            hello_at: f64::NEG_INFINITY,
            faders: Vec::new(),
            pans: Vec::new(),
            selected: None,
            touched: vec![false; profile.strips],
        }
    }

    pub fn profile(&self) -> &'static Profile {
        self.profile
    }
}

impl Protocol for Mapped {
    fn strips(&self) -> usize {
        self.profile.strips
    }

    fn receive(&mut self, msg: &[u8], out: &mut Vec<SurfaceInput>) {
        if let Some(answer) = self.profile.answer
            && msg.starts_with(answer)
        {
            if !self.answered {
                self.answered = true;
                // Everything again, now that it listens.
                self.sent.clear();
                out.push(SurfaceInput::Refresh);
            }
            return;
        }
        let Some((does, value)) = self.profile.find(msg) else {
            return;
        };
        let press_only = self.profile.press_only;
        let press = |button: Button, out: &mut Vec<SurfaceInput>| {
            let pressed = value > 0;
            out.push(SurfaceInput::Button { button, pressed });
            if pressed && press_only {
                out.push(SurfaceInput::Button {
                    button,
                    pressed: false,
                });
            }
        };
        match does {
            Does::Button(b) => press(b, out),
            Does::Strip(kind, strip) => press(kind.button(strip), out),
            Does::StripByValue(kind) => {
                out.push(SurfaceInput::Button {
                    button: kind.button(usize::from(value)),
                    pressed: true,
                });
                out.push(SurfaceInput::Button {
                    button: kind.button(usize::from(value)),
                    pressed: false,
                });
            }
            Does::Fader(strip) => {
                let travel = if msg[0] & 0xF0 == 0xE0 {
                    let v = u16::from(msg[1] & 0x7F) | u16::from(value & 0x7F) << 7;
                    f32::from(v) / 16383.0
                } else {
                    f32::from(value) / 127.0
                };
                out.push(SurfaceInput::Fader { strip, travel });
            }
            Does::Knob(strip, enc) => match enc.ticks(value) {
                Some(0) => {}
                Some(delta) => out.push(SurfaceInput::Pot { strip, delta }),
                None => out.push(SurfaceInput::Knob {
                    strip,
                    value: f32::from(value) / 127.0,
                }),
            },
            Does::Jog(enc) => match enc.ticks(value) {
                Some(0) | None => {}
                Some(t) => out.push(SurfaceInput::Jog(t)),
            },
            Does::Wheel(cw, ccw, enc) => {
                let t = enc.ticks(value).unwrap_or(0);
                let button = if t > 0 { cw } else { ccw };
                for _ in 0..t.unsigned_abs().min(8) {
                    out.push(SurfaceInput::Button {
                        button,
                        pressed: true,
                    });
                    out.push(SurfaceInput::Button {
                        button,
                        pressed: false,
                    });
                }
            }
            Does::VolumeTicks(strip, enc) => {
                let t = enc.ticks(value).unwrap_or(0);
                if let Some(v) = self.faders.get_mut(strip) {
                    *v = (*v + t as f32 * 0.005).clamp(0.0, 1.0);
                    out.push(SurfaceInput::Fader { strip, travel: *v });
                }
            }
            Does::PanTicks(strip, enc) => {
                let t = enc.ticks(value).unwrap_or(0);
                if let Some(p) = self.pans.get_mut(strip) {
                    *p = (*p + t as f32 * 0.01).clamp(-1.0, 1.0);
                    out.push(SurfaceInput::Pan { strip, pan: *p });
                }
            }
            Does::SelectedPan(enc) => {
                if let (Some(strip), Some(t)) = (self.selected, enc.ticks(value))
                    && t != 0
                {
                    out.push(SurfaceInput::Pot { strip, delta: t });
                }
            }
            Does::Touch(strip) => {
                let touched = value > 0;
                if let Some(t) = self.touched.get_mut(strip) {
                    *t = touched;
                }
                if !touched {
                    // Where the motor should go now is sent again.
                    self.sent.remove(&fader_key(strip));
                }
                out.push(SurfaceInput::Touch { strip, touched });
            }
            Does::Slot(strip, scene) => out.push(if value > 0 {
                SurfaceInput::LaunchClip { strip, scene }
            } else {
                SurfaceInput::ReleaseClip { strip, scene }
            }),
            Does::Input(input) => {
                if value > 0 {
                    out.push(input);
                }
            }
        }
    }

    fn update(&mut self, state: &SurfaceState, now: f64, out: &mut Vec<Vec<u8>>) {
        self.faders = state.strips.iter().map(|s| s.fader).collect();
        self.pans = state.strips.iter().map(|s| s.pan).collect();
        self.selected = state.strips.iter().position(|s| s.present && s.selected);
        if !self.answered {
            // Hello until it answers.
            if now - self.hello_at >= 1.0 {
                self.hello_at = now;
                out.extend(self.profile.hello.iter().map(|m| m.to_vec()));
            }
            return;
        }
        if !self.greeted {
            self.greeted = true;
            if self.profile.answer.is_none() {
                out.extend(self.profile.hello.iter().map(|m| m.to_vec()));
            }
        }
        if let Some((every, msg)) = self.profile.keep_alive
            && now - self.alive_at >= every
        {
            self.alive_at = now;
            out.push(msg.to_vec());
        }
        let mut f = Feedback {
            sent: &mut self.sent,
            out,
            touched: &self.touched,
            now,
        };
        (self.profile.feedback)(state, &mut f);
    }

    fn reset(&mut self) {
        self.sent.clear();
        self.greeted = false;
        self.alive_at = f64::NEG_INFINITY;
        self.answered = self.profile.answer.is_none();
        self.hello_at = f64::NEG_INFINITY;
    }

    fn scenes(&self) -> usize {
        self.profile.scenes
    }

    fn goodbye(&self) -> Vec<Vec<u8>> {
        self.profile.goodbye.iter().map(|m| m.to_vec()).collect()
    }

    fn claims(&self) -> Option<fn(&[u8]) -> bool> {
        self.profile.claims
    }

    fn follows_selection(&self) -> bool {
        self.profile.follows_selection
    }
}

/// The master's strip number for a [`Does::Fader`] of the master.
pub const MASTER_FADER: usize = MASTER;

/// `bytes` with the header in front and `F7` after (SysEx).
pub fn sysex(header: &[u8], bytes: &[u8]) -> Vec<u8> {
    let mut m = Vec::with_capacity(header.len() + bytes.len() + 1);
    m.extend_from_slice(header);
    m.extend_from_slice(bytes);
    m.push(0xF7);
    m
}

/// `text` as 7-bit ASCII cut to `width` (no padding).
pub fn ascii(text: &str, width: usize) -> Vec<u8> {
    text.chars()
        .take(width)
        .map(|c| {
            if c.is_ascii() && !c.is_ascii_control() {
                c as u8
            } else {
                b'?'
            }
        })
        .collect()
}

/// The selected strip (else the first present one).
pub fn shown_strip(state: &SurfaceState) -> Option<&crate::StripState> {
    state
        .strips
        .iter()
        .find(|s| s.present && s.selected)
        .or_else(|| state.strips.iter().find(|s| s.present))
}

/// "12.3 PLAY": the bar and beat and what the transport does.
pub fn position_text(state: &SurfaceState) -> String {
    let transport = if state.recording {
        "REC"
    } else if state.playing {
        "PLAY"
    } else {
        "STOP"
    };
    format!("{}.{} {transport}", state.position.bar, state.position.beat)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relative_encodings_read_their_ticks() {
        assert_eq!(Encoding::SignedBit.ticks(0x01), Some(1));
        assert_eq!(Encoding::SignedBit.ticks(0x41), Some(-1));
        assert_eq!(Encoding::SignedBit.ticks(0x45), Some(-5));
        assert_eq!(Encoding::TwosComplement.ticks(3), Some(3));
        assert_eq!(Encoding::TwosComplement.ticks(127), Some(-1));
        assert_eq!(Encoding::TwosComplement.ticks(65), Some(-63));
        assert_eq!(Encoding::Offset64.ticks(65), Some(1));
        assert_eq!(Encoding::Offset64.ticks(61), Some(-3));
        assert_eq!(Encoding::Absolute.ticks(5), None);
    }
}
