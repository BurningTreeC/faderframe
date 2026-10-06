//! HUI (Mackie HUI and the surfaces that speak it).
//!
//! Switches and LEDs are addressed by zone and port: the surface sends
//! `B0 0F <zone>` then `B0 2F <0x40 | port>` (pressed) or `<port>`
//! (released); the host lights an LED with `B0 0C <zone>` `B0 2C <0x40 |
//! port>`. Strips are zones 0–7 (port 0 fader touch, 1 select, 2 mute,
//! 3 solo, 5 v-sel, 7 record ready); zone 0x0A banks and channels (0 ←
//! channel, 1 ← bank, 2 channel →, 3 bank →); zone 0x0E the transport
//! (1 rewind, 2 forward, 3 stop, 4 play, 5 record); zone 0x0F (0 return
//! to zero, 1 end, 3 loop, 4 quick punch); zone 0x08 (0 control, 1
//! shift, 2 edit mode, 3 undo, 4 alt, 5 option, 6 edit tool, 7 save);
//! 0x09 the windows (0 mix, 1 edit, 2 transport: the launcher); 0x0C (6
//! record ready all); 0x0D the cursor keys (0 down, 1 left, 2 mode: zoom,
//! 3 right, 4 up, 5 scrub, 6 shuttle); 0x10 (1 pre-roll, 2 punch in, 3
//! punch out); 0x11/0x12 the monitor section (3 mute: the master's);
//! 0x13–0x15 the numeric keypad (digits and '.': locate to a bar, or
//! `.n.` to marker n; enter: a marker, or locate; clr; + and − a bar);
//! 0x19 (1 the selected track's input monitoring, 4 create a group); 0x1A
//! edit (0 paste, 1 cut, 2 capture, 3 delete, 4 copy, 5 separate); 0x1B
//! F1–F8; 0x1C parameter edit (0 ins/para: the plug-in page, 6 bypass).
//! The timecode LEDs (0x16: timecode, beats, rude solo) follow the
//! display and the solos. Faders are
//! `B0 0z <hi>` `B0 2z <lo>` (14 bits) both ways; pots `B0 4p <v>` (v >
//! 0x40: v − 0x40 clockwise ticks, else −v), the jog wheel `B0 0D <v>`;
//! pot rings `B0 1y <1…11>`. SysEx `F0 00 00 66 05 00 …`: 0x10 the
//! 4-character strip displays, 0x11 the timecode digits (rightmost first,
//! 0x10 adds the dot), 0x12 the 2×40 display in ten-character zones.
//! Meters are `A0 0y <side << 4 | segment>`. The host pings (`90 00 00`)
//! every second, or the surface goes off line and its faders stop; it
//! answers (`90 00 7F`), so a silent surface shows as off line.

use crate::{
    AutomationButton, Button, EditButton, Modifier, Protocol, SurfaceInput, SurfaceState, Window,
    abbreviate, fit, meter_level,
};

const STRIPS: usize = 8;
const HEADER: [u8; 6] = [0xF0, 0x00, 0x00, 0x66, 0x05, 0x00];
const PING_EVERY: f64 = 1.0;
const METER_EVERY: f64 = 0.1;

/// Zones and ports.
mod zone {
    pub const BANK: u8 = 0x0A;
    pub const TRANSPORT: u8 = 0x0E;
    pub const LOCATE: u8 = 0x0F;
    pub const KEYS: u8 = 0x08;
    pub const TIMECODE_LEDS: u8 = 0x16;
    pub const ASSIGN: u8 = 0x0B;
    pub const AUTO_MODE: u8 = 0x18;
    pub const WINDOW: u8 = 0x09;
    pub const ASSIGN2: u8 = 0x0C;
    pub const CURSOR: u8 = 0x0D;
    pub const PUNCH: u8 = 0x10;
    pub const MONITOR: u8 = 0x11;
    pub const MONITOR2: u8 = 0x12;
    pub const PAD1: u8 = 0x13;
    pub const PAD2: u8 = 0x14;
    pub const PAD3: u8 = 0x15;
    pub const STATUS: u8 = 0x19;
    pub const EDIT: u8 = 0x1A;
    pub const FUNCTION: u8 = 0x1B;
    pub const PARAM: u8 = 0x1C;
}

/// No answer to the pings this long: off line.
const ONLINE_FOR: f64 = 3.0;

mod port {
    pub const TOUCH: u8 = 0;
    pub const SELECT: u8 = 1;
    pub const MUTE: u8 = 2;
    pub const SOLO: u8 = 3;
    pub const VSEL: u8 = 5;
    pub const ARM: u8 = 7;
}

#[derive(Default)]
struct Sent {
    scribbles: [Option<Vec<u8>>; STRIPS],
    display: [Option<Vec<u8>>; 8],
    faders: [Option<u16>; STRIPS],
    rings: [Option<u8>; STRIPS],
    /// LED states by zone and port.
    leds: Vec<((u8, u8), bool)>,
    digits: Option<Vec<u8>>,
    meters: [u8; STRIPS],
}

pub struct Hui {
    sent: Sent,
    /// The zone the surface selected last.
    zone: Option<u8>,
    /// Fader high bytes waiting for their low bytes.
    high: [Option<u8>; STRIPS],
    touched: [bool; STRIPS],
    ping_at: f64,
    meters_at: f64,
    /// When the surface last answered a ping, and when we last looked.
    answered: f64,
    now: f64,
    /// The first ping.
    since: Option<f64>,
}

impl Default for Hui {
    fn default() -> Self {
        Self::new()
    }
}

impl Hui {
    pub fn new() -> Self {
        Self {
            sent: Sent::default(),
            zone: None,
            high: [None; STRIPS],
            touched: [false; STRIPS],
            ping_at: f64::NEG_INFINITY,
            meters_at: f64::NEG_INFINITY,
            answered: f64::NEG_INFINITY,
            now: 0.0,
            since: None,
        }
    }

    fn led(&mut self, zone: u8, port: u8, on: bool, out: &mut Vec<Vec<u8>>) {
        let key = (zone, port);
        match self.sent.leds.iter_mut().find(|(k, _)| *k == key) {
            Some((_, v)) if *v == on => return,
            Some((_, v)) => *v = on,
            None => self.sent.leds.push((key, on)),
        }
        out.push(vec![
            0xB0,
            0x0C,
            zone,
            0x2C,
            port | if on { 0x40 } else { 0 },
        ]);
    }

    fn sysex(body: &[u8]) -> Vec<u8> {
        let mut m = HEADER.to_vec();
        m.extend_from_slice(body);
        m.push(0xF7);
        m
    }

    fn switch(&mut self, zone: u8, port: u8, pressed: bool, out: &mut Vec<SurfaceInput>) {
        let strip = zone as usize;
        let button = match (zone, port) {
            (0..=7, port::TOUCH) => {
                self.touched[strip] = pressed;
                if !pressed {
                    self.sent.faders[strip] = None;
                }
                out.push(SurfaceInput::Touch {
                    strip,
                    touched: pressed,
                });
                return;
            }
            (0..=7, port::SELECT) => Button::Select(strip),
            (0..=7, port::MUTE) => Button::Mute(strip),
            (0..=7, port::SOLO) => Button::Solo(strip),
            (0..=7, port::VSEL) => Button::PotPress(strip),
            (0..=7, port::ARM) => Button::Arm(strip),
            (zone::BANK, 0) => Button::ChannelLeft,
            (zone::BANK, 1) => Button::BankLeft,
            (zone::BANK, 2) => Button::ChannelRight,
            (zone::BANK, 3) => Button::BankRight,
            (zone::TRANSPORT, 1) => Button::Rewind,
            (zone::TRANSPORT, 2) => Button::Forward,
            (zone::TRANSPORT, 3) => Button::Stop,
            (zone::TRANSPORT, 4) => Button::Play,
            (zone::TRANSPORT, 5) => Button::Record,
            (zone::LOCATE, 0) => Button::Start,
            (zone::LOCATE, 1) => Button::End,
            (zone::LOCATE, 3) => Button::Loop,
            (zone::ASSIGN, 2) => Button::PanPage,
            // send a … e
            (zone::ASSIGN, 3..=7) => Button::SendPage(Some((7 - port) as usize)),
            (zone::AUTO_MODE, 1) => Button::Automation(AutomationButton::Latch),
            (zone::AUTO_MODE, 2) => Button::Automation(AutomationButton::Read),
            (zone::AUTO_MODE, 3) => Button::Automation(AutomationButton::Off),
            (zone::AUTO_MODE, 4) => Button::Automation(AutomationButton::Write),
            (zone::AUTO_MODE, 5) => Button::Automation(AutomationButton::Touch),
            (zone::LOCATE, 4) => Button::Drop,
            (zone::KEYS, 0) => Button::Modifier(Modifier::Control),
            (zone::KEYS, 1) => Button::Modifier(Modifier::Shift),
            (zone::KEYS, 2) => Button::EditMode,
            (zone::KEYS, 3) => Button::Undo,
            (zone::KEYS, 4) => Button::Modifier(Modifier::Alt),
            (zone::KEYS, 5) => Button::Modifier(Modifier::Option),
            (zone::KEYS, 6) => Button::EditTool,
            (zone::KEYS, 7) => Button::Save,
            (zone::WINDOW, 0) => Button::Window(Window::Mixer),
            (zone::WINDOW, 1) => Button::Window(Window::Editor),
            (zone::WINDOW, 2) => Button::Window(Window::Launcher),
            (zone::ASSIGN2, 6) => Button::ArmAll,
            (zone::CURSOR, 0) => Button::Down,
            (zone::CURSOR, 1) => Button::Left,
            (zone::CURSOR, 2) => Button::Zoom,
            (zone::CURSOR, 3) => Button::Right,
            (zone::CURSOR, 4) => Button::Up,
            (zone::CURSOR, 5) => Button::Scrub,
            (zone::CURSOR, 6) => Button::Shuttle,
            (zone::PUNCH, 1) => Button::PreRoll,
            (zone::PUNCH, 2) => Button::PunchIn,
            (zone::PUNCH, 3) => Button::PunchOut,
            (zone::MONITOR | zone::MONITOR2, 3) => Button::MonitorMute,
            (zone::PAD1, p) => {
                Button::Numpad(['0', '1', '4', '2', '5', '.', '3', '6'][p as usize & 7])
            }
            (zone::PAD2, 0) => Button::Numpad('E'),
            (zone::PAD2, 1) => Button::Numpad('+'),
            (zone::PAD3, 0) => Button::Numpad('7'),
            (zone::PAD3, 1) => Button::Numpad('8'),
            (zone::PAD3, 2) => Button::Numpad('9'),
            (zone::PAD3, 3) => Button::Numpad('-'),
            (zone::PAD3, 4) => Button::Numpad('C'),
            (zone::STATUS, 1) => Button::InputMonitor,
            (zone::STATUS, 4) => Button::Group,
            (zone::EDIT, 0) => Button::Edit(EditButton::Paste),
            (zone::EDIT, 1) => Button::Edit(EditButton::Cut),
            (zone::EDIT, 2) => Button::Edit(EditButton::Capture),
            (zone::EDIT, 3) => Button::Edit(EditButton::Delete),
            (zone::EDIT, 4) => Button::Edit(EditButton::Copy),
            (zone::EDIT, 5) => Button::Edit(EditButton::Separate),
            (zone::FUNCTION, f) => Button::Function(f),
            (zone::PARAM, 0) => Button::PluginPage,
            (zone::PARAM, 6) => Button::Bypass,
            _ => return,
        };
        out.push(SurfaceInput::Button { button, pressed });
    }
}

/// Signed ticks of a HUI encoder.
fn ticks(v: u8) -> i32 {
    if v > 0x40 {
        i32::from(v - 0x40)
    } else {
        -i32::from(v)
    }
}

impl Protocol for Hui {
    fn strips(&self) -> usize {
        STRIPS
    }

    fn receive(&mut self, msg: &[u8], out: &mut Vec<SurfaceInput>) {
        let [status, a, b, ..] = *msg else {
            return;
        };
        // The answer to a ping: on line.
        if status == 0x90 && a == 0 && b == 0x7F {
            self.answered = self.now;
            return;
        }
        if status != 0xB0 {
            return;
        }
        match a {
            0x0F => self.zone = Some(b),
            0x2F => {
                if let Some(zone) = self.zone {
                    self.switch(zone, b & 0x07, b & 0x40 != 0, out);
                }
            }
            0x00..=0x07 => self.high[a as usize] = Some(b & 0x7F),
            0x20..=0x27 => {
                let i = (a - 0x20) as usize;
                if let Some(hi) = self.high[i].take() {
                    let v = u16::from(hi) << 7 | u16::from(b & 0x7F);
                    self.sent.faders[i] = Some(v);
                    out.push(SurfaceInput::Fader {
                        strip: i,
                        travel: f32::from(v) / 16383.0,
                    });
                }
            }
            0x40..=0x47 => out.push(SurfaceInput::Pot {
                strip: (a - 0x40) as usize,
                delta: ticks(b),
            }),
            0x0D => out.push(SurfaceInput::Jog(ticks(b))),
            _ => {}
        }
    }

    fn update(&mut self, state: &SurfaceState, now: f64, out: &mut Vec<Vec<u8>>) {
        self.now = now;
        self.since.get_or_insert(now);
        if now - self.ping_at >= PING_EVERY {
            self.ping_at = now;
            out.push(vec![0x90, 0x00, 0x00]);
        }
        let strip = |i: usize| state.strips.get(i).filter(|s| s.present);
        for i in 0..STRIPS {
            let s = strip(i);
            let name = fit(&s.map_or(String::new(), |s| abbreviate(&s.name, 4)), 4);
            if self.sent.scribbles[i].as_ref() != Some(&name) {
                let mut body = vec![0x10, i as u8];
                body.extend_from_slice(&name);
                out.push(Self::sysex(&body));
                self.sent.scribbles[i] = Some(name);
            }
            let v = (s.map_or(0.0, |s| s.fader).clamp(0.0, 1.0) * 16383.0).round() as u16;
            if !self.touched[i] && self.sent.faders[i] != Some(v) {
                self.sent.faders[i] = Some(v);
                out.push(vec![
                    0xB0,
                    i as u8,
                    (v >> 7) as u8,
                    0x20 + i as u8,
                    (v & 0x7F) as u8,
                ]);
            }
            // A dot (1…11) for pan, a bar from the left (0x21…0x2B) for
            // levels.
            let ring = s.map_or(0, |s| {
                let v = s.pot.clamp(0.0, 1.0);
                if s.pot_bipolar {
                    1 + (v * 10.0).round() as u8
                } else {
                    0x21 + (v * 10.0).round() as u8
                }
            });
            if self.sent.rings[i] != Some(ring) {
                self.sent.rings[i] = Some(ring);
                out.push(vec![0xB0, 0x10 + i as u8, ring]);
            }
            let z = i as u8;
            self.led(z, port::SELECT, s.is_some_and(|s| s.selected), out);
            self.led(z, port::MUTE, s.is_some_and(|s| s.mute), out);
            self.led(z, port::SOLO, s.is_some_and(|s| s.solo), out);
            self.led(z, port::ARM, s.is_some_and(|s| s.arm), out);
        }
        // The big display: levels above, names below, 5 characters a strip.
        for row in 0..2 {
            for zone in 0..4 {
                let mut text = Vec::with_capacity(10);
                for k in 0..2 {
                    let i = zone * 2 + k;
                    let t = strip(i).map_or(String::new(), |s| {
                        if row == 0 {
                            format!("{:>4}", s.level)
                        } else {
                            abbreviate(&s.name, 4)
                        }
                    });
                    text.extend(fit(&t, 4));
                    text.push(b' ');
                }
                let z = row * 4 + zone;
                if self.sent.display[z].as_ref() != Some(&text) {
                    let mut body = vec![0x12, z as u8];
                    body.extend_from_slice(&text);
                    out.push(Self::sysex(&body));
                    self.sent.display[z] = Some(text);
                }
            }
        }
        self.led(zone::TRANSPORT, 4, state.playing, out);
        self.led(zone::TRANSPORT, 3, !state.playing, out);
        self.led(zone::TRANSPORT, 5, state.recording, out);
        self.led(zone::LOCATE, 3, state.looping, out);
        self.led(zone::TIMECODE_LEDS, 0, state.timecode.is_some(), out);
        self.led(zone::TIMECODE_LEDS, 2, state.timecode.is_none(), out);
        self.led(zone::TIMECODE_LEDS, 3, state.modes.any_solo, out);
        self.led(zone::LOCATE, 4, state.modes.punch, out);
        self.led(zone::CURSOR, 2, state.modes.zoom, out);
        self.led(zone::CURSOR, 5, state.modes.scrub, out);
        self.led(zone::CURSOR, 6, state.modes.shuttle, out);
        self.led(zone::PARAM, 0, state.page.is_channel_strip(), out);
        self.led(zone::ASSIGN, 2, state.page == crate::Page::Pan, out);
        for k in 0..5u8 {
            let on = state.page == crate::Page::Send(k as usize);
            self.led(zone::ASSIGN, 7 - k, on, out);
        }
        for (port, b) in [
            (1, AutomationButton::Latch),
            (2, AutomationButton::Read),
            (3, AutomationButton::Off),
            (4, AutomationButton::Write),
            (5, AutomationButton::Touch),
        ] {
            self.led(zone::AUTO_MODE, port, state.automation == Some(b), out);
        }
        // Bars (3), beats (2), sixteenths (1), ticks (2) — or hours (2),
        // minutes, seconds, frames — rightmost first.
        let p = state.position;
        let text = match state.timecode {
            Some([h, m, s, f]) => format!(
                "{:02}{:02}{:02}{:02}",
                h.clamp(0, 99),
                m.clamp(0, 59),
                s.clamp(0, 59),
                f.clamp(0, 99)
            ),
            None => format!(
                "{:03}{:02}{:01}{:02}",
                p.bar.clamp(0, 999),
                p.beat.clamp(0, 99),
                p.sixteenth.clamp(0, 9),
                (p.tick / 10).clamp(0, 99)
            ),
        };
        let dots: &[usize] = if state.timecode.is_some() {
            &[2, 4, 6]
        } else {
            &[2, 3, 5]
        };
        let digits: Vec<u8> = text
            .bytes()
            .rev()
            .enumerate()
            .map(|(k, c)| (c - b'0') | if dots.contains(&k) { 0x10 } else { 0 })
            .collect();
        if self.sent.digits.as_ref() != Some(&digits) {
            let mut body = vec![0x11];
            body.extend_from_slice(&digits);
            out.push(Self::sysex(&body));
            self.sent.digits = Some(digits);
        }
        if now - self.meters_at >= METER_EVERY {
            self.meters_at = now;
            for i in 0..STRIPS {
                let level = strip(i).map_or(0, |s| meter_level(s.meter_db));
                if level > 0 || self.sent.meters[i] > 0 {
                    for side in 0..2u8 {
                        out.push(vec![0xA0, i as u8, side << 4 | level]);
                    }
                }
                self.sent.meters[i] = level;
            }
        }
    }

    fn online(&self) -> Option<bool> {
        // Not judged before the pings had time to be answered.
        let judged = self.since.is_some_and(|t| self.now - t >= ONLINE_FOR);
        judged.then_some(self.now - self.answered < ONLINE_FOR)
    }

    fn reset(&mut self) {
        self.sent = Sent::default();
        self.since = None;
        self.ping_at = f64::NEG_INFINITY;
        self.meters_at = f64::NEG_INFINITY;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Position, StripState};

    #[test]
    fn switches_faders_and_pots_arrive() {
        let mut h = Hui::new();
        let mut out = Vec::new();
        // Solo on strip 5 pressed and released, play, a fader, a pot.
        for m in [
            [0xB0, 0x0F, 0x04],
            [0xB0, 0x2F, 0x43],
            [0xB0, 0x0F, 0x04],
            [0xB0, 0x2F, 0x03],
            [0xB0, 0x0F, 0x0E],
            [0xB0, 0x2F, 0x44],
            [0xB0, 0x02, 0x7F],
            [0xB0, 0x22, 0x60],
            [0xB0, 0x41, 0x01],
            [0xB0, 0x0D, 0x42],
        ] {
            h.receive(&m, &mut out);
        }
        assert_eq!(
            out,
            [
                SurfaceInput::Button {
                    button: Button::Solo(4),
                    pressed: true
                },
                SurfaceInput::Button {
                    button: Button::Solo(4),
                    pressed: false
                },
                SurfaceInput::Button {
                    button: Button::Play,
                    pressed: true
                },
                SurfaceInput::Fader {
                    strip: 2,
                    travel: f32::from(0x7Fu16 << 7 | 0x60) / 16383.0
                },
                SurfaceInput::Pot {
                    strip: 1,
                    delta: -1
                },
                SurfaceInput::Jog(2),
            ]
        );
    }

    #[test]
    fn it_pings_and_shows_the_state() {
        let mut h = Hui::new();
        let mut strips = vec![StripState::default(); 8];
        strips[2] = StripState {
            present: true,
            name: "Vocals".into(),
            fader: 1.0,
            level: "+12".into(),
            solo: true,
            ..StripState::default()
        };
        let state = SurfaceState {
            strips,
            playing: true,
            position: Position {
                bar: 5,
                beat: 2,
                sixteenth: 1,
                tick: 0,
            },
            ..SurfaceState::default()
        };
        let mut out = Vec::new();
        h.update(&state, 0.0, &mut out);
        let has = |out: &[Vec<u8>], msg: &[u8]| out.iter().any(|m| m == msg);
        assert!(has(&out, &[0x90, 0x00, 0x00]), "ping");
        assert!(
            has(&out, &[0xB0, 0x02, 0x7F, 0x22, 0x7F]),
            "fader 3 at the top"
        );
        assert!(has(&out, &[0xB0, 0x0C, 0x02, 0x2C, 0x43]), "solo 3 lit");
        assert!(has(&out, &[0xB0, 0x0C, 0x0E, 0x2C, 0x44]), "play lit");
        let scribble = HEADER
            .iter()
            .copied()
            .chain([0x10, 2])
            .chain(*b"Vcls")
            .chain([0xF7])
            .collect::<Vec<u8>>();
        assert!(has(&out, &scribble));
        // 005.02.1.00, rightmost digit first.
        let tc = HEADER
            .iter()
            .copied()
            .chain([0x11, 0, 0, 0x11, 0x12, 0, 5 | 0x10, 0, 0])
            .chain([0xF7])
            .collect::<Vec<u8>>();
        assert!(has(&out, &tc), "{out:x?}");
        out.clear();
        h.update(&state, 0.5, &mut out);
        assert!(out.is_empty(), "nothing changed: {out:x?}");
        h.update(&state, 1.2, &mut out);
        assert_eq!(out, vec![vec![0x90, 0x00, 0x00]]);
    }

    #[test]
    fn the_other_zones_and_the_answer_to_pings() {
        let mut h = Hui::new();
        let mut out = Vec::new();
        let press = |h: &mut Hui, zone: u8, port: u8, out: &mut Vec<SurfaceInput>| {
            h.receive(&[0xB0, 0x0F, zone], out);
            h.receive(&[0xB0, 0x2F, 0x40 | port], out);
            h.receive(&[0xB0, 0x2F, port], out);
        };
        press(&mut h, 0x1B, 2, &mut out);
        press(&mut h, 0x1A, 5, &mut out);
        press(&mut h, 0x13, 4, &mut out);
        press(&mut h, 0x0D, 5, &mut out);
        press(&mut h, 0x09, 0, &mut out);
        let pressed: Vec<Button> = out
            .iter()
            .filter_map(|i| match i {
                SurfaceInput::Button {
                    button,
                    pressed: true,
                } => Some(*button),
                _ => None,
            })
            .collect();
        assert_eq!(
            pressed,
            vec![
                Button::Function(2),
                Button::Edit(EditButton::Separate),
                Button::Numpad('5'),
                Button::Scrub,
                Button::Window(Window::Mixer),
            ]
        );
        // Silent: off line after a few seconds; answering: on line.
        let state = SurfaceState::default();
        let mut sent = Vec::new();
        h.update(&state, 0.0, &mut sent);
        assert_eq!(h.online(), None);
        h.update(&state, 4.0, &mut sent);
        assert_eq!(h.online(), Some(false));
        h.receive(&[0x90, 0x00, 0x7F], &mut out);
        h.update(&state, 4.5, &mut sent);
        assert_eq!(h.online(), Some(true));
    }
}
