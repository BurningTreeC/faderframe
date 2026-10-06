//! OSC 1.0 (messages and bundles, int/float/string/blob/true/false/nil
//! arguments) and FaderFrame's OSC surface.
//!
//! Addresses (strips 1-based from the bank's start; buttons act on a
//! non-zero argument or none, so momentary and toggle buttons both work —
//! a toggle's value is compared with the state shown):
//!
//! | In | |
//! |---|---|
//! | `/transport/{play,stop,record,rewind,forward,loop,click,start,end}` | the transport |
//! | `/strip/N/fader f` | fader travel 0…1 (`/strip/N/touch i` around a move) |
//! | `/strip/N/pan f` | 0…1, 0.5 the centre |
//! | `/strip/N/{mute,solo,arm,select} i` | |
//! | `/master/fader f` | |
//! | `/bank/{left,right}`, `/channel/{left,right}` | move the bank |
//! | `/launcher/clip/N/S`, `/launcher/scene/S`, `/launcher/stop`, `/launcher/back` | the clip launcher (scene S 1-based) |
//! | `/jog i`, `/undo`, `/save`, `/marker`, `/refresh` | |
//! | `/flip`, `/page/pan`, `/page/send/K` | what faders and pots move |
//! | `/automation/{off,read,touch,latch,write}` | the selected track's mode |
//!
//! Out (only what changed): `/strip/N/{name s, fader f, level s, pan f,
//! mute i, solo i, arm i, select i, meter f}` (the meter 0…1),
//! `/master/fader f`, `/transport/{play,record,loop,click} i`,
//! `/transport/position s` ("bar.beat.sixteenth.tick"), `/bank/first i`,
//! `/page s` ("PN", "S1" …) and `/flip i`.

use crate::{Button, MASTER, Protocol, SurfaceInput, SurfaceState, meter_level};
use std::collections::HashMap;

#[derive(Clone, Debug, PartialEq)]
pub enum Arg {
    Int(i32),
    Float(f32),
    Str(String),
    Blob(Vec<u8>),
    Bool(bool),
    Nil,
}

impl Arg {
    /// A number (ints, floats, booleans).
    pub fn number(&self) -> Option<f32> {
        match self {
            Arg::Int(v) => Some(*v as f32),
            Arg::Float(v) => Some(*v),
            Arg::Bool(b) => Some(if *b { 1.0 } else { 0.0 }),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Message {
    pub addr: String,
    pub args: Vec<Arg>,
}

impl Message {
    pub fn new(addr: impl Into<String>, args: Vec<Arg>) -> Self {
        Self {
            addr: addr.into(),
            args,
        }
    }
}

fn pad(out: &mut Vec<u8>) {
    while !out.len().is_multiple_of(4) {
        out.push(0);
    }
}

fn put_str(out: &mut Vec<u8>, s: &str) {
    out.extend_from_slice(s.as_bytes());
    out.push(0);
    pad(out);
}

/// One message as a packet.
pub fn encode(m: &Message) -> Vec<u8> {
    let mut out = Vec::with_capacity(32);
    put_str(&mut out, &m.addr);
    let mut tags = String::from(",");
    for a in &m.args {
        tags.push(match a {
            Arg::Int(_) => 'i',
            Arg::Float(_) => 'f',
            Arg::Str(_) => 's',
            Arg::Blob(_) => 'b',
            Arg::Bool(true) => 'T',
            Arg::Bool(false) => 'F',
            Arg::Nil => 'N',
        });
    }
    put_str(&mut out, &tags);
    for a in &m.args {
        match a {
            Arg::Int(v) => out.extend_from_slice(&v.to_be_bytes()),
            Arg::Float(v) => out.extend_from_slice(&v.to_be_bytes()),
            Arg::Str(s) => put_str(&mut out, s),
            Arg::Blob(b) => {
                out.extend_from_slice(&(b.len() as i32).to_be_bytes());
                out.extend_from_slice(b);
                pad(&mut out);
            }
            Arg::Bool(_) | Arg::Nil => {}
        }
    }
    out
}

/// Reads a padded string at `*at`.
fn get_str(p: &[u8], at: &mut usize) -> Option<String> {
    let rest = p.get(*at..)?;
    let end = rest.iter().position(|b| *b == 0)?;
    let s = std::str::from_utf8(&rest[..end]).ok()?.to_string();
    *at += (end + 4) & !3;
    Some(s)
}

fn get_u32(p: &[u8], at: &mut usize) -> Option<[u8; 4]> {
    let b: [u8; 4] = p.get(*at..*at + 4)?.try_into().ok()?;
    *at += 4;
    Some(b)
}

fn decode_message(p: &[u8]) -> Option<Message> {
    let mut at = 0;
    let addr = get_str(p, &mut at)?;
    if !addr.starts_with('/') {
        return None;
    }
    let mut args = Vec::new();
    if at >= p.len() {
        return Some(Message { addr, args });
    }
    let tags = get_str(p, &mut at)?;
    let tags = tags.strip_prefix(',')?;
    for t in tags.chars() {
        args.push(match t {
            'i' => Arg::Int(i32::from_be_bytes(get_u32(p, &mut at)?)),
            'f' => Arg::Float(f32::from_be_bytes(get_u32(p, &mut at)?)),
            's' | 'S' => Arg::Str(get_str(p, &mut at)?),
            'b' => {
                let n = i32::from_be_bytes(get_u32(p, &mut at)?).max(0) as usize;
                let b = p.get(at..at + n)?.to_vec();
                at += (n + 3) & !3;
                Arg::Blob(b)
            }
            'h' | 't' | 'd' => {
                // 64-bit values: as a float where it makes sense.
                let b: [u8; 8] = p.get(at..at + 8)?.try_into().ok()?;
                at += 8;
                match t {
                    'd' => Arg::Float(f64::from_be_bytes(b) as f32),
                    _ => Arg::Int(i64::from_be_bytes(b) as i32),
                }
            }
            'T' => Arg::Bool(true),
            'F' => Arg::Bool(false),
            'N' | 'I' => Arg::Nil,
            _ => return None,
        });
    }
    Some(Message { addr, args })
}

/// The messages of a packet (bundles are opened; their times ignored).
pub fn decode(p: &[u8]) -> Vec<Message> {
    let mut out = Vec::new();
    decode_into(p, &mut out, 0);
    out
}

fn decode_into(p: &[u8], out: &mut Vec<Message>, depth: usize) {
    if p.starts_with(b"#bundle\0") {
        if depth > 8 {
            return;
        }
        let mut at = 16;
        while let Some(n) = get_u32(p, &mut at) {
            let n = i32::from_be_bytes(n).max(0) as usize;
            let Some(inner) = p.get(at..at + n) else {
                return;
            };
            decode_into(inner, out, depth + 1);
            at += n;
        }
    } else if let Some(m) = decode_message(p) {
        out.push(m);
    }
}

/// FaderFrame's OSC surface (see the module docs).
pub struct OscSurface {
    strips: usize,
    sent: HashMap<String, Arg>,
}

impl OscSurface {
    pub fn new(strips: usize) -> Self {
        Self {
            strips: strips.max(1),
            sent: HashMap::new(),
        }
    }

    /// Send `arg` at `addr` unless it was sent last.
    fn put(&mut self, addr: String, arg: Arg, out: &mut Vec<Vec<u8>>) {
        if self.sent.get(&addr) == Some(&arg) {
            return;
        }
        out.push(encode(&Message::new(addr.clone(), vec![arg.clone()])));
        self.sent.insert(addr, arg);
    }

    /// The state last shown at `addr` (toggles compare with it).
    fn shown(&self, addr: &str) -> bool {
        matches!(self.sent.get(addr), Some(Arg::Int(v)) if *v != 0)
    }

    fn message(&mut self, m: &Message, out: &mut Vec<SurfaceInput>) {
        let value = m.args.first().and_then(Arg::number);
        let on = value.is_none_or(|v| v != 0.0);
        let parts: Vec<&str> = m.addr.trim_matches('/').split('/').collect();
        let press = |button: Button, out: &mut Vec<SurfaceInput>| {
            out.push(SurfaceInput::Button {
                button,
                pressed: true,
            });
            out.push(SurfaceInput::Button {
                button,
                pressed: false,
            });
        };
        let index = |s: &str| s.parse::<usize>().ok().filter(|n| *n >= 1).map(|n| n - 1);
        match parts.as_slice() {
            ["transport", what] if on => {
                let button = match *what {
                    "play" => Button::Play,
                    "stop" => Button::Stop,
                    "record" => Button::Record,
                    "rewind" => Button::Rewind,
                    "forward" => Button::Forward,
                    "loop" => Button::Loop,
                    "click" => Button::Click,
                    "start" => Button::Start,
                    "end" => Button::End,
                    _ => return,
                };
                press(button, out);
            }
            ["strip", n, what] => {
                let Some(strip) = index(n).filter(|s| *s < self.strips) else {
                    return;
                };
                match (*what, value) {
                    ("fader", Some(v)) => out.push(SurfaceInput::Fader {
                        strip,
                        travel: v.clamp(0.0, 1.0),
                    }),
                    ("touch", _) => out.push(SurfaceInput::Touch { strip, touched: on }),
                    ("pan", Some(v)) => out.push(SurfaceInput::Pan {
                        strip,
                        pan: (v.clamp(0.0, 1.0) * 2.0 - 1.0),
                    }),
                    (toggle @ ("mute" | "solo" | "arm" | "select"), _) => {
                        // A toggle's new state, or a momentary press.
                        let shown = self.shown(&format!("/strip/{}/{toggle}", strip + 1));
                        if value.is_some() && on == shown {
                            return;
                        }
                        let button = match toggle {
                            "mute" => Button::Mute(strip),
                            "solo" => Button::Solo(strip),
                            "arm" => Button::Arm(strip),
                            _ => Button::Select(strip),
                        };
                        press(button, out);
                    }
                    _ => {}
                }
            }
            ["master", "fader"] => {
                if let Some(v) = value {
                    out.push(SurfaceInput::Fader {
                        strip: MASTER,
                        travel: v.clamp(0.0, 1.0),
                    });
                }
            }
            ["bank", side] | ["channel", side] if on => {
                let bank = parts[0] == "bank";
                let button = match (*side, bank) {
                    ("left", true) => Button::BankLeft,
                    ("right", true) => Button::BankRight,
                    ("left", false) => Button::ChannelLeft,
                    ("right", false) => Button::ChannelRight,
                    _ => return,
                };
                press(button, out);
            }
            ["launcher", "clip", n, s] if on => {
                if let (Some(strip), Some(scene)) = (index(n), index(s)) {
                    out.push(SurfaceInput::LaunchClip { strip, scene });
                }
            }
            ["launcher", "scene", s] if on => {
                if let Some(scene) = index(s) {
                    out.push(SurfaceInput::LaunchScene(scene));
                }
            }
            ["launcher", "stop"] if on => out.push(SurfaceInput::StopClips),
            ["launcher", "back"] if on => out.push(SurfaceInput::BackToArrangement),
            ["jog"] => {
                if let Some(v) = value {
                    out.push(SurfaceInput::Jog(v as i32));
                }
            }
            ["flip"] if on => press(Button::Flip, out),
            ["page", "pan"] if on => press(Button::PanPage, out),
            ["page", "send", k] if on => {
                if let Some(k) = index(k) {
                    press(Button::SendPage(Some(k)), out);
                }
            }
            ["automation", mode] if on => {
                use crate::AutomationButton as A;
                let b = match *mode {
                    "off" => A::Off,
                    "read" => A::Read,
                    "touch" => A::Touch,
                    "latch" => A::Latch,
                    "write" => A::Write,
                    _ => return,
                };
                press(Button::Automation(b), out);
            }
            ["undo"] if on => press(Button::Undo, out),
            ["save"] if on => press(Button::Save, out),
            ["marker"] if on => press(Button::Marker, out),
            ["refresh"] => out.push(SurfaceInput::Refresh),
            _ => {}
        }
    }
}

impl Protocol for OscSurface {
    fn strips(&self) -> usize {
        self.strips
    }

    /// One packet.
    fn receive(&mut self, msg: &[u8], out: &mut Vec<SurfaceInput>) {
        for m in decode(msg) {
            self.message(&m, out);
        }
    }

    fn update(&mut self, state: &SurfaceState, _now: f64, out: &mut Vec<Vec<u8>>) {
        for i in 0..self.strips {
            let s = state.strips.get(i).filter(|s| s.present);
            let base = format!("/strip/{}", i + 1);
            let flag = |v: Option<bool>| Arg::Int(i32::from(v.unwrap_or(false)));
            self.put(
                format!("{base}/name"),
                Arg::Str(s.map_or(String::new(), |s| s.name.clone())),
                out,
            );
            self.put(
                format!("{base}/fader"),
                Arg::Float(s.map_or(0.0, |s| s.fader)),
                out,
            );
            self.put(
                format!("{base}/level"),
                Arg::Str(s.map_or(String::new(), |s| s.level.clone())),
                out,
            );
            self.put(
                format!("{base}/pan"),
                Arg::Float(s.map_or(0.5, |s| (s.pan + 1.0) / 2.0)),
                out,
            );
            self.put(format!("{base}/mute"), flag(s.map(|s| s.mute)), out);
            self.put(format!("{base}/solo"), flag(s.map(|s| s.solo)), out);
            self.put(format!("{base}/arm"), flag(s.map(|s| s.arm)), out);
            self.put(format!("{base}/select"), flag(s.map(|s| s.selected)), out);
            self.put(
                format!("{base}/meter"),
                Arg::Float(s.map_or(0.0, |s| f32::from(meter_level(s.meter_db)) / 12.0)),
                out,
            );
        }
        if let Some(m) = state.master {
            self.put("/master/fader".into(), Arg::Float(m), out);
        }
        for (what, on) in [
            ("play", state.playing),
            ("record", state.recording),
            ("loop", state.looping),
            ("click", state.click),
        ] {
            self.put(format!("/transport/{what}"), Arg::Int(i32::from(on)), out);
        }
        let p = state.position;
        self.put(
            "/transport/position".into(),
            Arg::Str(format!(
                "{}.{}.{}.{:03}",
                p.bar, p.beat, p.sixteenth, p.tick
            )),
            out,
        );
        self.put(
            "/bank/first".into(),
            Arg::Int(state.first_track as i32),
            out,
        );
        self.put("/page".into(), Arg::Str(state.page.short()), out);
        self.put("/flip".into(), Arg::Int(i32::from(state.flip)), out);
    }

    fn reset(&mut self) {
        self.sent.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::StripState;

    #[test]
    fn messages_and_bundles_round_trip() {
        let m = Message::new(
            "/strip/1/name",
            vec![
                Arg::Str("Drums".into()),
                Arg::Int(-3),
                Arg::Float(0.5),
                Arg::Blob(vec![1, 2, 3]),
                Arg::Bool(true),
                Arg::Nil,
            ],
        );
        let p = encode(&m);
        assert_eq!(p.len() % 4, 0);
        assert_eq!(&p[..16], b"/strip/1/name\0\0\0");
        assert_eq!(decode(&p), vec![m.clone()]);
        // A bundle of two.
        let other = Message::new("/transport/play", vec![]);
        let mut bundle = b"#bundle\0".to_vec();
        bundle.extend_from_slice(&[0, 0, 0, 0, 0, 0, 0, 1]);
        for e in [&m, &other] {
            let e = encode(e);
            bundle.extend_from_slice(&(e.len() as i32).to_be_bytes());
            bundle.extend_from_slice(&e);
        }
        assert_eq!(decode(&bundle), vec![m, other]);
        assert!(decode(b"garbage").is_empty());
    }

    #[test]
    fn the_surface_reads_and_shows() {
        let mut s = OscSurface::new(8);
        let mut out = Vec::new();
        let send = |s: &mut OscSurface, m: Message, out: &mut Vec<SurfaceInput>| {
            s.receive(&encode(&m), out);
        };
        send(
            &mut s,
            Message::new("/strip/2/fader", vec![Arg::Float(0.25)]),
            &mut out,
        );
        send(
            &mut s,
            Message::new("/transport/play", vec![Arg::Float(1.0)]),
            &mut out,
        );
        send(
            &mut s,
            Message::new("/transport/play", vec![Arg::Float(0.0)]),
            &mut out,
        );
        send(&mut s, Message::new("/launcher/clip/3/2", vec![]), &mut out);
        assert_eq!(
            out,
            [
                SurfaceInput::Fader {
                    strip: 1,
                    travel: 0.25
                },
                SurfaceInput::Button {
                    button: Button::Play,
                    pressed: true
                },
                SurfaceInput::Button {
                    button: Button::Play,
                    pressed: false
                },
                SurfaceInput::LaunchClip { strip: 2, scene: 1 },
            ]
        );
        // Shown: the state, then only changes.
        let mut state = SurfaceState {
            strips: vec![StripState::default(); 8],
            ..SurfaceState::default()
        };
        state.strips[0] = StripState {
            present: true,
            name: "Bass".into(),
            mute: true,
            ..StripState::default()
        };
        let mut sent = Vec::new();
        s.update(&state, 0.0, &mut sent);
        let msgs: Vec<Message> = sent.iter().flat_map(|p| decode(p)).collect();
        assert!(msgs.contains(&Message::new(
            "/strip/1/name",
            vec![Arg::Str("Bass".into())]
        )));
        assert!(msgs.contains(&Message::new("/strip/1/mute", vec![Arg::Int(1)])));
        sent.clear();
        s.update(&state, 0.1, &mut sent);
        assert!(sent.is_empty());
        // A toggle already in that state does nothing; another state flips it.
        out.clear();
        send(
            &mut s,
            Message::new("/strip/1/mute", vec![Arg::Int(1)]),
            &mut out,
        );
        assert!(out.is_empty());
        send(
            &mut s,
            Message::new("/strip/1/mute", vec![Arg::Int(0)]),
            &mut out,
        );
        assert_eq!(out.len(), 2);
    }
}
