//! Mackie Control (MCU) and its clones (Behringer X-Touch, iCON, …).
//!
//! Buttons and LEDs are notes (velocity 127 pressed/on, 0 released/off):
//! record arm 0–7, solo 8–15, mute 16–23, select 24–31, pot press 32–39,
//! the assignment pages 40–45 (track, send, pan, plug-in, EQ,
//! instrument), bank ←/→ 46/47, channel ←/→ 48/49, flip 50, global view
//! 51, name/value 52, SMPTE/beats 53, F1–F8 54–61, the track types 62–69
//! (MIDI, inputs, audio, instruments, aux, buses, outputs, user), Shift,
//! Option, Control, Alt 70–73, automation read/off, write, trim, touch,
//! latch 74–78, group 79, save 80, undo 81 (Shift: redo), cancel 82,
//! enter 83, marker 84, nudge 85, cycle 86, drop 87 (punch), replace 88,
//! click 89, solo 90 (every solo off; lit while one is on), rewind 91,
//! forward 92 (Shift: the start and end), stop 93, play 94, record 95,
//! the arrows 96–99 (zoom: zooming, nudge: moving the selection, else
//! the selected track up and down and the channel), zoom 100, scrub 101
//! (the jog wheel scrubs), the footswitches 102/103 (play, record),
//! fader touch 104–112 (112 the master), the SMPTE and BEATS LEDs 113 and
//! 114, rude solo 115. Faders
//! are pitch bend (channels 0–7, 8 the master, 14 bits); pots CC 16–23
//! (bit 6 the direction, the rest ticks), the jog wheel CC 60, pot rings
//! CC 48–55 (mode in bits 4–5, position 1–11). The 2×56 LCD is SysEx
//! `F0 00 00 66 <device> 12 <offset> <text> F7`, 7 characters a strip:
//! the names above, the levels below (or the pots' values: name/value;
//! on the channel-strip pages the parameters' names and values). The
//! ten-digit display shows bars, beats, sixteenths and ticks or hours,
//! minutes, seconds and frames (CC 64–73 right to left, 0x40 adds the
//! dot), the two-digit one the page (CC 74–75). Meters are
//! channel pressure (strip in the high nibble, segment 0–12 in the low),
//! sent again every 100 ms (the surface lets them fall).
//!
//! Devices that speak it with a difference are a [`MackieProfile`]: what
//! puts them into their Mackie mode (and back), notes on channel 1 only (a
//! keyboard's pads share its DAW port), another display (Arturia's 2×16
//! LCD) or none, the X-Touch's coloured scribble strips, notes a device
//! sends for others, a single strip that follows the selection.

use crate::{
    AutomationButton, Button, MASTER, Modifier, Page, Protocol, SurfaceInput, SurfaceState,
    TrackFilter, abbreviate, fit, meter_level,
};

/// Device ids: the Mackie Control and its extender.
pub const MCU: u8 = 0x14;
pub const XT: u8 = 0x15;

const STRIPS: usize = 8;
const LCD_WIDTH: usize = 56;
const METER_EVERY: f64 = 0.1;

/// Note numbers of the buttons (and their LEDs).
mod note {
    pub const REC: u8 = 0x00;
    pub const SOLO: u8 = 0x08;
    pub const MUTE: u8 = 0x10;
    pub const SELECT: u8 = 0x18;
    pub const POT: u8 = 0x20;
    pub const TRACK: u8 = 0x28;
    pub const SEND: u8 = 0x29;
    pub const PAN: u8 = 0x2A;
    pub const PLUGIN: u8 = 0x2B;
    pub const EQ: u8 = 0x2C;
    pub const INSTRUMENT: u8 = 0x2D;
    pub const FLIP: u8 = 0x32;
    pub const GLOBAL: u8 = 0x33;
    pub const NAME_VALUE: u8 = 0x34;
    pub const SMPTE_BEATS: u8 = 0x35;
    pub const F1: u8 = 0x36;
    pub const TYPES: u8 = 0x3E;
    pub const SHIFT: u8 = 0x46;
    pub const TRIM: u8 = 0x4C;
    pub const GROUP: u8 = 0x4F;
    pub const CANCEL: u8 = 0x52;
    pub const ENTER: u8 = 0x53;
    pub const NUDGE: u8 = 0x55;
    pub const DROP: u8 = 0x57;
    pub const REPLACE: u8 = 0x58;
    pub const SOLO_ALL: u8 = 0x5A;
    pub const UP: u8 = 0x60;
    pub const DOWN: u8 = 0x61;
    pub const ZOOM: u8 = 0x64;
    pub const SCRUB: u8 = 0x65;
    pub const FOOT: u8 = 0x66;
    pub const SMPTE: u8 = 0x71;
    pub const RUDE_SOLO: u8 = 0x73;
    pub const READ: u8 = 0x4A;
    pub const WRITE: u8 = 0x4B;
    pub const AUTO_TOUCH: u8 = 0x4D;
    pub const LATCH: u8 = 0x4E;
    pub const BANK_LEFT: u8 = 0x2E;
    pub const BANK_RIGHT: u8 = 0x2F;
    pub const CHANNEL_LEFT: u8 = 0x30;
    pub const CHANNEL_RIGHT: u8 = 0x31;
    pub const SAVE: u8 = 0x50;
    pub const UNDO: u8 = 0x51;
    pub const MARKER: u8 = 0x54;
    pub const CYCLE: u8 = 0x56;
    pub const CLICK: u8 = 0x59;
    pub const REWIND: u8 = 0x5B;
    pub const FORWARD: u8 = 0x5C;
    pub const STOP: u8 = 0x5D;
    pub const PLAY: u8 = 0x5E;
    pub const RECORD: u8 = 0x5F;
    pub const LEFT: u8 = 0x62;
    pub const RIGHT: u8 = 0x63;
    pub const TOUCH: u8 = 0x68;
    pub const TOUCH_MASTER: u8 = 0x70;
    pub const BEATS: u8 = 0x72;
}

struct Sent {
    lcd: [Option<Vec<u8>>; 2],
    faders: [Option<u16>; STRIPS + 1],
    rings: [Option<u8>; STRIPS],
    /// LED states by note.
    leds: [Option<bool>; 128],
    digits: [Option<u8>; 12],
    meters: [u8; STRIPS],
}

impl Default for Sent {
    fn default() -> Self {
        Self {
            lcd: [None, None],
            faders: [None; STRIPS + 1],
            rings: [None; STRIPS],
            leds: [None; 128],
            digits: [None; 12],
            meters: [0; STRIPS],
        }
    }
}

/// What a device shows text on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Display {
    /// The Mackie LCD (2 × 56).
    Mackie,
    /// Arturia's keyboards' 2 × 16 LCD (`F0 00 20 6B 7F 42 04 00 60 …`):
    /// the selected track, its level, the position and the transport.
    Arturia,
    None,
}

/// How a device departs from the Mackie Control (see the module docs).
#[derive(Clone, Copy, Debug)]
pub struct MackieProfile {
    /// The SysEx device id (LCD, colours).
    pub device: u8,
    /// Sent when it is opened (into its Mackie mode) and when it is closed.
    pub hello: &'static [&'static [u8]],
    pub goodbye: &'static [&'static [u8]],
    /// Notes count on channel 1 only.
    pub channel_1: bool,
    pub display: Display,
    /// The time display and the meters.
    pub timecode: bool,
    pub meters: bool,
    /// Scribble strips in the tracks' colours (X-Touch).
    pub colours: bool,
    /// (the device's note, the Mackie note it means), both ways.
    pub remap: &'static [(u8, u8)],
    /// Channel strips (1: it follows the selection).
    pub strips: usize,
}

impl MackieProfile {
    /// The Mackie Control itself (or an extender: [`XT`]).
    pub const fn plain(device: u8) -> Self {
        Self {
            device,
            hello: &[],
            goodbye: &[],
            channel_1: false,
            display: Display::Mackie,
            timecode: true,
            meters: true,
            colours: false,
            remap: &[],
            strips: STRIPS,
        }
    }
}

pub struct Mackie {
    profile: MackieProfile,
    sent: Sent,
    touched: [bool; STRIPS + 1],
    meters_at: f64,
    greeted: bool,
    colours: Option<Vec<u8>>,
}

impl Mackie {
    /// A surface with `device` id ([`MCU`] or [`XT`]).
    pub fn new(device: u8) -> Self {
        Self::with_profile(MackieProfile::plain(device))
    }

    pub fn with_profile(profile: MackieProfile) -> Self {
        Self {
            profile,
            sent: Sent::default(),
            touched: [false; STRIPS + 1],
            meters_at: f64::NEG_INFINITY,
            greeted: false,
            colours: None,
        }
    }

    fn led(&mut self, note: u8, on: bool, out: &mut Vec<Vec<u8>>) {
        let slot = &mut self.sent.leds[note as usize & 0x7F];
        if *slot != Some(on) {
            *slot = Some(on);
            // The device's own note for it.
            let note = self
                .profile
                .remap
                .iter()
                .find(|(_, m)| *m == note)
                .map_or(note, |(d, _)| *d);
            out.push(vec![0x90, note, if on { 0x7F } else { 0x00 }]);
        }
    }

    /// Arturia's 2 × 16 LCD: the selected track (else the first) above,
    /// its level, the position and the transport below.
    fn arturia_lcd(&mut self, state: &SurfaceState, out: &mut Vec<Vec<u8>>) {
        let shown = state
            .strips
            .iter()
            .find(|s| s.present && s.selected)
            .or_else(|| state.strips.iter().find(|s| s.present));
        let top = shown.map_or_else(|| "FaderFrame".to_string(), |s| s.name.clone());
        let transport = if state.recording {
            "REC"
        } else if state.playing {
            "PLAY"
        } else {
            "STOP"
        };
        let p = state.position;
        let bottom = format!(
            "{:<7}{:>9}",
            shown.map_or(String::new(), |s| s.level.clone()),
            format!("{}.{} {transport}", p.bar, p.beat)
        );
        let text = [fit(&top, 16), fit(&bottom, 16)].concat();
        if self.sent.lcd[0].as_ref() == Some(&text) {
            return;
        }
        let mut msg = vec![0xF0, 0x00, 0x20, 0x6B, 0x7F, 0x42, 0x04, 0x00, 0x60, 0x01];
        msg.extend_from_slice(&text[..16]);
        msg.extend_from_slice(&[0x00, 0x02]);
        msg.extend_from_slice(&text[16..]);
        msg.extend_from_slice(&[0x00, 0xF7]);
        out.push(msg);
        self.sent.lcd[0] = Some(text);
    }

    /// The X-Touch's scribble strips in the tracks' colours (bit 0 red,
    /// 1 green, 2 blue; black shows white, an empty strip none).
    fn colours(&mut self, state: &SurfaceState, out: &mut Vec<Vec<u8>>) {
        let codes: Vec<u8> = (0..STRIPS)
            .map(|i| match state.strips.get(i).filter(|s| s.present) {
                None => 0,
                Some(s) => {
                    let [r, g, b] = s.color;
                    let c = u8::from(r > 0x7F) | u8::from(g > 0x7F) << 1 | u8::from(b > 0x7F) << 2;
                    if c == 0 { 7 } else { c }
                }
            })
            .collect();
        if self.colours.as_ref() == Some(&codes) {
            return;
        }
        let mut msg = vec![0xF0, 0x00, 0x00, 0x66, self.profile.device, 0x72];
        msg.extend_from_slice(&codes);
        msg.push(0xF7);
        out.push(msg);
        self.colours = Some(codes);
    }

    /// Meters: again every 100 ms while they show anything.
    fn meters(&mut self, state: &SurfaceState, now: f64, out: &mut Vec<Vec<u8>>) {
        if !self.profile.meters || now - self.meters_at < METER_EVERY {
            return;
        }
        self.meters_at = now;
        for i in 0..STRIPS {
            let level = state
                .strips
                .get(i)
                .filter(|s| s.present)
                .map_or(0, |s| meter_level(s.meter_db));
            if level > 0 || self.sent.meters[i] > 0 {
                out.push(vec![0xD0, (i as u8) << 4 | level]);
            }
            self.sent.meters[i] = level;
        }
    }

    fn lcd_line(&mut self, row: usize, text: Vec<u8>, out: &mut Vec<Vec<u8>>) {
        if self.sent.lcd[row].as_ref() == Some(&text) {
            return;
        }
        let mut msg = vec![
            0xF0,
            0x00,
            0x00,
            0x66,
            self.profile.device,
            0x12,
            (row * LCD_WIDTH) as u8,
        ];
        msg.extend_from_slice(&text);
        msg.push(0xF7);
        out.push(msg);
        self.sent.lcd[row] = Some(text);
    }
}

/// The 7-segment code of a character (0x40 adds the dot).
fn segment(c: u8, dot: bool) -> u8 {
    let v = match c {
        b'0'..=b'9' | b' ' | b'-' => c,
        b'A'..=b'Z' => c - 0x40,
        b'a'..=b'z' => c - 0x60,
        _ => b' ',
    };
    v | if dot { 0x40 } else { 0 }
}

/// The track-type buttons in order.
const FILTERS: [TrackFilter; 8] = [
    TrackFilter::Midi,
    TrackFilter::Inputs,
    TrackFilter::Audio,
    TrackFilter::Instruments,
    TrackFilter::Aux,
    TrackFilter::Buses,
    TrackFilter::Outputs,
    TrackFilter::User,
];

/// Signed ticks of a relative encoder (bit 6 counter-clockwise).
fn ticks(v: u8) -> i32 {
    let n = i32::from(v & 0x3F);
    if v & 0x40 != 0 { -n } else { n }
}

impl Protocol for Mackie {
    fn strips(&self) -> usize {
        self.profile.strips
    }

    fn follows_selection(&self) -> bool {
        self.profile.strips == 1
    }

    fn goodbye(&self) -> Vec<Vec<u8>> {
        self.profile.goodbye.iter().map(|m| m.to_vec()).collect()
    }

    fn receive(&mut self, msg: &[u8], out: &mut Vec<SurfaceInput>) {
        let [status, a, b, ..] = *msg else {
            return;
        };
        match status & 0xF0 {
            0x90 | 0x80 if self.profile.channel_1 && status & 0x0F != 0 => {}
            0x90 | 0x80 => {
                let a = self
                    .profile
                    .remap
                    .iter()
                    .find(|(d, _)| *d == a)
                    .map_or(a, |(_, m)| *m);
                let pressed = status & 0xF0 == 0x90 && b > 0;
                let strip = |base: u8| (a - base) as usize;
                let button = match a {
                    0x00..=0x07 => Button::Arm(strip(note::REC)),
                    0x08..=0x0F => Button::Solo(strip(note::SOLO)),
                    0x10..=0x17 => Button::Mute(strip(note::MUTE)),
                    0x18..=0x1F => Button::Select(strip(note::SELECT)),
                    0x20..=0x27 => Button::PotPress(strip(note::POT)),
                    note::TRACK => Button::TrackPage,
                    note::SEND => Button::SendPage(None),
                    note::PAN => Button::PanPage,
                    note::PLUGIN => Button::PluginPage,
                    note::EQ => Button::EqPage,
                    note::INSTRUMENT => Button::InstrumentPage,
                    note::FLIP => Button::Flip,
                    note::GLOBAL => Button::GlobalView,
                    note::NAME_VALUE => Button::NameValue,
                    note::SMPTE_BEATS => Button::TimeDisplay,
                    0x36..=0x3D => Button::Function(a - note::F1),
                    0x3E..=0x45 => Button::TrackType(FILTERS[(a - note::TYPES) as usize]),
                    0x46..=0x49 => Button::Modifier(
                        [
                            Modifier::Shift,
                            Modifier::Option,
                            Modifier::Control,
                            Modifier::Alt,
                        ][(a - note::SHIFT) as usize],
                    ),
                    note::TRIM => Button::Automation(AutomationButton::Off),
                    note::GROUP => Button::Group,
                    note::CANCEL => Button::Cancel,
                    note::ENTER => Button::Enter,
                    note::NUDGE => Button::Nudge,
                    note::DROP => Button::Drop,
                    note::REPLACE => Button::Replace,
                    note::SOLO_ALL => Button::ClearSolo,
                    note::UP => Button::Up,
                    note::DOWN => Button::Down,
                    note::ZOOM => Button::Zoom,
                    note::SCRUB => Button::Scrub,
                    0x66 | 0x67 => Button::Footswitch(a - note::FOOT),
                    note::READ => Button::Automation(AutomationButton::Read),
                    note::WRITE => Button::Automation(AutomationButton::Write),
                    note::AUTO_TOUCH => Button::Automation(AutomationButton::Touch),
                    note::LATCH => Button::Automation(AutomationButton::Latch),
                    note::BANK_LEFT => Button::BankLeft,
                    note::BANK_RIGHT => Button::BankRight,
                    note::CHANNEL_LEFT => Button::ChannelLeft,
                    note::CHANNEL_RIGHT => Button::ChannelRight,
                    note::LEFT => Button::Left,
                    note::RIGHT => Button::Right,
                    note::SAVE => Button::Save,
                    note::UNDO => Button::Undo,
                    note::MARKER => Button::Marker,
                    note::CYCLE => Button::Loop,
                    note::CLICK => Button::Click,
                    note::REWIND => Button::Rewind,
                    note::FORWARD => Button::Forward,
                    note::STOP => Button::Stop,
                    note::PLAY => Button::Play,
                    note::RECORD => Button::Record,
                    note::TOUCH..=note::TOUCH_MASTER => {
                        let i = (a - note::TOUCH) as usize;
                        self.touched[i] = pressed;
                        if !pressed {
                            // Where the motor should go now is sent again.
                            self.sent.faders[i] = None;
                        }
                        out.push(SurfaceInput::Touch {
                            strip: if i == STRIPS { MASTER } else { i },
                            touched: pressed,
                        });
                        return;
                    }
                    _ => return,
                };
                out.push(SurfaceInput::Button { button, pressed });
            }
            0xE0 => {
                let ch = (status & 0x0F) as usize;
                if ch > STRIPS {
                    return;
                }
                let v = u16::from(a & 0x7F) | u16::from(b & 0x7F) << 7;
                // The surface is where it was moved to.
                self.sent.faders[ch] = Some(v);
                out.push(SurfaceInput::Fader {
                    strip: if ch == STRIPS { MASTER } else { ch },
                    travel: f32::from(v) / 16383.0,
                });
            }
            0xB0 => match a {
                0x10..=0x17 => out.push(SurfaceInput::Pot {
                    strip: (a - 0x10) as usize,
                    delta: ticks(b),
                }),
                0x3C => out.push(SurfaceInput::Jog(ticks(b))),
                _ => {}
            },
            _ => {}
        }
    }

    fn update(&mut self, state: &SurfaceState, now: f64, out: &mut Vec<Vec<u8>>) {
        if !self.greeted {
            self.greeted = true;
            out.extend(self.profile.hello.iter().map(|m| m.to_vec()));
        }
        let strip = |i: usize| state.strips.get(i).filter(|s| s.present);
        match self.profile.display {
            // The LCD: names above, levels below.
            Display::Mackie => {
                let mut names = Vec::with_capacity(LCD_WIDTH);
                let mut levels = Vec::with_capacity(LCD_WIDTH);
                for i in 0..STRIPS {
                    let (n, l) = strip(i).map_or((String::new(), String::new()), |s| {
                        (abbreviate(&s.name, 6), s.level.clone())
                    });
                    names.extend(fit(&n, 6));
                    names.push(b' ');
                    levels.extend(fit(&format!("{l:>6}"), 6));
                    levels.push(b' ');
                }
                self.lcd_line(0, names, out);
                self.lcd_line(1, levels, out);
            }
            Display::Arturia => self.arturia_lcd(state, out),
            Display::None => {}
        }
        if self.profile.colours {
            self.colours(state, out);
        }
        // Faders (not while held), pot rings, strip LEDs.
        for i in 0..=STRIPS {
            let travel = if i == STRIPS {
                state.master.unwrap_or(0.0)
            } else {
                strip(i).map_or(0.0, |s| s.fader)
            };
            let v = (travel.clamp(0.0, 1.0) * 16383.0).round() as u16;
            if !self.touched[i] && self.sent.faders[i] != Some(v) {
                self.sent.faders[i] = Some(v);
                out.push(vec![0xE0 | i as u8, (v & 0x7F) as u8, (v >> 7) as u8]);
            }
        }
        for i in 0..STRIPS {
            let s = strip(i);
            // Mode 0 (a dot, 1…11 left to right) for pan, mode 2 (a bar
            // from the left) for levels.
            let ring = s.map_or(0, |s| {
                let v = s.pot.clamp(0.0, 1.0);
                if s.pot_bipolar {
                    1 + (v * 10.0).round() as u8
                } else {
                    0x20 | (v * 11.0).round() as u8
                }
            });
            if self.sent.rings[i] != Some(ring) {
                self.sent.rings[i] = Some(ring);
                out.push(vec![0xB0, 0x30 + i as u8, ring]);
            }
            let i8 = i as u8;
            self.led(note::REC + i8, s.is_some_and(|s| s.arm), out);
            self.led(note::SOLO + i8, s.is_some_and(|s| s.solo), out);
            self.led(note::MUTE + i8, s.is_some_and(|s| s.mute), out);
            self.led(note::SELECT + i8, s.is_some_and(|s| s.selected), out);
        }
        self.led(note::PLAY, state.playing, out);
        self.led(note::STOP, !state.playing, out);
        self.led(note::RECORD, state.recording, out);
        self.led(note::CYCLE, state.looping, out);
        self.led(note::CLICK, state.click, out);
        self.led(note::BEATS, state.timecode.is_none(), out);
        self.led(note::PAN, state.page == Page::Pan, out);
        self.led(note::SEND, matches!(state.page, Page::Send(_)), out);
        self.led(note::TRACK, state.page == Page::Track, out);
        self.led(note::PLUGIN, state.page == Page::Plugin, out);
        self.led(note::EQ, state.page == Page::Eq, out);
        self.led(note::INSTRUMENT, state.page == Page::Instrument, out);
        self.led(note::FLIP, state.flip, out);
        let m = state.modes;
        self.led(note::GLOBAL, m.filter.is_none(), out);
        for (k, f) in FILTERS.iter().enumerate() {
            self.led(note::TYPES + k as u8, m.filter == Some(*f), out);
        }
        self.led(note::NAME_VALUE, m.values, out);
        self.led(note::NUDGE, m.nudge, out);
        self.led(note::DROP, m.punch, out);
        self.led(note::REPLACE, m.replace, out);
        self.led(note::SOLO_ALL, m.any_solo, out);
        self.led(note::RUDE_SOLO, m.any_solo, out);
        self.led(note::ZOOM, m.zoom, out);
        self.led(note::SCRUB, m.scrub, out);
        for (k, on) in m.modifiers.iter().enumerate() {
            self.led(note::SHIFT + k as u8, *on, out);
        }
        self.led(note::SMPTE, state.timecode.is_some(), out);
        for (n, b) in [
            (note::READ, AutomationButton::Read),
            (note::WRITE, AutomationButton::Write),
            (note::AUTO_TOUCH, AutomationButton::Touch),
            (note::LATCH, AutomationButton::Latch),
        ] {
            self.led(n, state.automation == Some(b), out);
        }
        if !self.profile.timecode {
            self.meters(state, now, out);
            return;
        }
        // Bars, beats, sixteenths, ticks (or hours, minutes, seconds,
        // frames); the page.
        let p = state.position;
        let text = match state.timecode {
            Some([h, mi, se, f]) => format!(
                "{:>3}{:02}{:02}{:>3}",
                h.clamp(0, 999),
                mi.clamp(0, 59),
                se.clamp(0, 59),
                f.clamp(0, 999)
            ),
            None => format!(
                "{:>3}{:>2}{:>2}{:>3}",
                p.bar.clamp(-99, 999),
                p.beat.clamp(0, 99),
                p.sixteenth.clamp(0, 99),
                p.tick.clamp(0, 999)
            ),
        };
        let assign = state.page.short();
        let digits: Vec<u8> = text.bytes().chain(assign.bytes()).collect();
        for (k, c) in digits.iter().enumerate() {
            // Digit k from the left; CC 0x40 is the rightmost of the ten,
            // 0x4A/0x4B the assignment digits (right, left).
            let (cc, dot) = match k {
                0..=9 => (0x49 - k as u8, matches!(k, 2 | 4 | 6)),
                10 => (0x4B, false),
                _ => (0x4A, false),
            };
            let v = segment(*c, dot);
            let slot = &mut self.sent.digits[(cc - 0x40) as usize];
            if *slot != Some(v) {
                *slot = Some(v);
                out.push(vec![0xB0, cc, v]);
            }
        }
        self.meters(state, now, out);
    }

    fn reset(&mut self) {
        self.sent = Sent::default();
        self.meters_at = f64::NEG_INFINITY;
        self.greeted = false;
        self.colours = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Position, StripState};

    fn state() -> SurfaceState {
        let mut strips = vec![StripState::default(); 8];
        strips[0] = StripState {
            present: true,
            name: "Drums".into(),
            fader: 0.75,
            level: "0.0".into(),
            pan: -1.0,
            pot: 0.0,
            mute: true,
            meter_db: -5.0,
            ..StripState::default()
        };
        SurfaceState {
            strips,
            master: Some(0.5),
            playing: true,
            position: Position {
                bar: 12,
                beat: 3,
                sixteenth: 2,
                tick: 40,
            },
            first_track: 1,
            ..SurfaceState::default()
        }
    }

    #[test]
    fn it_shows_the_state_and_only_what_changed() {
        let mut m = Mackie::new(MCU);
        let mut out = Vec::new();
        m.update(&state(), 0.0, &mut out);
        let has = |out: &[Vec<u8>], msg: &[u8]| out.iter().any(|m| m == msg);
        // The names on the LCD's upper line.
        let lcd = out
            .iter()
            .find(|m| m[..7] == [0xF0, 0, 0, 0x66, 0x14, 0x12, 0])
            .unwrap();
        assert_eq!(&lcd[7..14], b"Drums  ");
        // Fader 1 at 3/4 (14 bits), the master at half.
        let v = (0.75f32 * 16383.0).round() as u16;
        assert!(has(&out, &[0xE0, (v & 0x7F) as u8, (v >> 7) as u8]));
        assert!(has(&out, &[0xE8, 0x00, 0x40]));
        // Mute 1 lit, play lit, the pan ring hard left, a meter.
        assert!(has(&out, &[0x90, 0x10, 0x7F]));
        assert!(has(&out, &[0x90, 0x5E, 0x7F]));
        assert!(has(&out, &[0xB0, 0x30, 1]));
        assert!(has(&out, &[0xD0, 9]));
        // Bar 12: '1' '2' (with the dot) in the 2nd and 3rd digits.
        assert!(has(&out, &[0xB0, 0x48, b'1']));
        assert!(has(&out, &[0xB0, 0x47, b'2' | 0x40]));
        // Nothing changed: only the meters again (after 100 ms).
        out.clear();
        m.update(&state(), 0.05, &mut out);
        assert!(out.is_empty(), "{out:?}");
        m.update(&state(), 0.2, &mut out);
        assert_eq!(out, vec![vec![0xD0, 9]]);
    }

    #[test]
    fn it_reads_buttons_faders_and_pots() {
        let mut m = Mackie::new(MCU);
        let mut out = Vec::new();
        m.receive(&[0x90, 0x5E, 0x7F], &mut out);
        m.receive(&[0x90, 0x5E, 0x00], &mut out);
        m.receive(&[0x90, 0x69, 0x7F], &mut out);
        m.receive(&[0xE1, 0x7F, 0x7F], &mut out);
        m.receive(&[0xB0, 0x12, 0x43], &mut out);
        m.receive(&[0xB0, 0x3C, 0x01], &mut out);
        assert_eq!(
            out,
            [
                SurfaceInput::Button {
                    button: Button::Play,
                    pressed: true
                },
                SurfaceInput::Button {
                    button: Button::Play,
                    pressed: false
                },
                SurfaceInput::Touch {
                    strip: 1,
                    touched: true
                },
                SurfaceInput::Fader {
                    strip: 1,
                    travel: 1.0
                },
                SurfaceInput::Pot {
                    strip: 2,
                    delta: -3
                },
                SurfaceInput::Jog(1),
            ]
        );
        // A held fader is not moved by the host.
        let mut sent = Vec::new();
        let mut s = state();
        s.strips[1].present = true;
        s.strips[1].fader = 0.2;
        m.update(&s, 0.0, &mut sent);
        assert!(!sent.iter().any(|m| m[0] == 0xE1));
        m.receive(&[0x90, 0x69, 0x00], &mut out);
        sent.clear();
        m.update(&s, 0.0, &mut sent);
        assert!(sent.iter().any(|m| m[0] == 0xE1));
    }

    #[test]
    fn pages_flip_and_automation() {
        let mut m = Mackie::new(MCU);
        let mut out = Vec::new();
        for n in [0x29, 0x2A, 0x32, 0x4D] {
            m.receive(&[0x90, n, 0x7F], &mut out);
        }
        let buttons: Vec<Button> = out
            .iter()
            .filter_map(|i| match i {
                SurfaceInput::Button { button, .. } => Some(*button),
                _ => None,
            })
            .collect();
        assert_eq!(
            buttons,
            [
                Button::SendPage(None),
                Button::PanPage,
                Button::Flip,
                Button::Automation(AutomationButton::Touch)
            ]
        );
        // A send page: a level bar on the ring, "S2" on the display, the
        // send and flip LEDs, Touch lit.
        let mut s = state();
        s.strips[0].pot = 1.0;
        s.strips[0].pot_bipolar = false;
        s.page = crate::Page::Send(1);
        s.flip = true;
        s.automation = Some(AutomationButton::Touch);
        let mut sent = Vec::new();
        m.update(&s, 0.0, &mut sent);
        let has = |msg: &[u8]| sent.iter().any(|m| m == msg);
        assert!(has(&[0xB0, 0x30, 0x20 | 11]));
        assert!(has(&[0xB0, 0x4B, b'S' - 0x40]));
        assert!(has(&[0xB0, 0x4A, b'2']));
        assert!(has(&[0x90, 0x29, 0x7F]) && has(&[0x90, 0x2A, 0x00]));
        assert!(has(&[0x90, 0x32, 0x7F]) && has(&[0x90, 0x4D, 0x7F]));
    }
}
