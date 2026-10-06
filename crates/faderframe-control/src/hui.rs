//! HUI (Mackie HUI and the surfaces that speak it).
//!
//! Switches and LEDs are addressed by zone and port: the surface sends
//! `B0 0F <zone>` then `B0 2F <0x40 | port>` (pressed) or `<port>`
//! (released); the host lights an LED with `B0 0C <zone>` `B0 2C <0x40 |
//! port>`. Strips are zones 0–7 (port 0 fader touch, 1 select, 2 mute,
//! 3 solo, 5 v-sel, 7 record ready); zone 0x0A banks and channels (0 ←
//! channel, 1 ← bank, 2 channel →, 3 bank →); zone 0x0E the transport
//! (1 rewind, 2 forward, 3 stop, 4 play, 5 record); zone 0x0F (0 return
//! to zero, 1 end, 3 loop); zone 0x08 (3 undo, 7 save). Faders are
//! `B0 0z <hi>` `B0 2z <lo>` (14 bits) both ways; pots `B0 4p <v>` (v >
//! 0x40: v − 0x40 clockwise ticks, else −v), the jog wheel `B0 0D <v>`;
//! pot rings `B0 1y <1…11>`. SysEx `F0 00 00 66 05 00 …`: 0x10 the
//! 4-character strip displays, 0x11 the timecode digits (rightmost first,
//! 0x10 adds the dot), 0x12 the 2×40 display in ten-character zones.
//! Meters are `A0 0y <side << 4 | segment>`. The host pings (`90 00 00`)
//! every second, or the surface goes off line and its faders stop.

use crate::{Button, Protocol, SurfaceInput, SurfaceState, abbreviate, fit, meter_level};

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
}

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
            (zone::KEYS, 3) => Button::Undo,
            (zone::KEYS, 7) => Button::Save,
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
            let ring = s.map_or(0, |s| {
                1 + ((s.pan.clamp(-1.0, 1.0) + 1.0) * 5.0).round() as u8
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
        self.led(zone::TIMECODE_LEDS, 2, true, out);
        // Bars (3), beats (2), sixteenths (1), ticks (2): rightmost first.
        let p = state.position;
        let text = format!(
            "{:03}{:02}{:01}{:02}",
            p.bar.clamp(0, 999),
            p.beat.clamp(0, 99),
            p.sixteenth.clamp(0, 9),
            (p.tick / 10).clamp(0, 99)
        );
        let digits: Vec<u8> = text
            .bytes()
            .rev()
            .enumerate()
            .map(|(k, c)| (c - b'0') | if matches!(k, 2 | 3 | 5) { 0x10 } else { 0 })
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

    fn reset(&mut self) {
        self.sent = Sent::default();
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
}
