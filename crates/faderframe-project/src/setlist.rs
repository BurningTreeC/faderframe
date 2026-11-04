//! A setlist: the songs of a show in the order they are played, each a
//! range of the project (one project holds the show, a song a stretch of
//! it — usually a section), with what happens after it and the
//! performer's notes. Show mode (in the session) plays it.
//!
//! A song can change external gear ([`SongGear`]): program changes with
//! bank selects, controller snapshots and SysEx patch dumps sent to MIDI
//! outputs when the show stands on the song (or on its first frame; see
//! [`GearTiming`]). Each change can wait before it (gear that needs time
//! after a patch change); the session spaces the messages as a MIDI cable
//! carries them.

use faderframe_timeline::MusicalTime;
use serde::{Deserialize, Serialize};

/// What happens when a song ends.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "type")]
pub enum AfterSong {
    /// Stop on its last frame and stand on the next song.
    #[default]
    Stop,
    /// Stop, then play the next song after `gap` seconds.
    Next { gap: f32 },
    /// Play straight on (the next song follows in the project).
    Continue,
}

impl AfterSong {
    pub fn label(self) -> String {
        match self {
            AfterSong::Stop => "Stop".into(),
            AfterSong::Next { gap } if gap <= 0.0 => "Next song at once".into(),
            AfterSong::Next { gap } => format!("Next song after {gap:.0} s"),
            AfterSong::Continue => "Play on".into(),
        }
    }
}

/// One song of the show.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SetSong {
    pub name: String,
    /// Where it is in the project.
    pub start: MusicalTime,
    pub end: MusicalTime,
    #[serde(default)]
    pub then: AfterSong,
    /// The performer's notes (shown in show mode).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub notes: String,
    /// The mix scene recalled when show mode stands on the song (its own
    /// crossfade, else the project's; see [`crate::scenes`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scene: Option<faderframe_core::MixSceneId>,
    /// What the song sends to external gear.
    #[serde(default, skip_serializing_if = "SongGear::is_empty")]
    pub gear: SongGear,
}

/// The show's songs in order.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Setlist {
    #[serde(default)]
    pub songs: Vec<SetSong>,
}

impl Setlist {
    pub fn is_empty(&self) -> bool {
        self.songs.is_empty()
    }

    /// Ranges kept sane (start before end, not before the project start).
    pub fn tidy(&mut self) {
        for s in &mut self.songs {
            s.start = s.start.max(MusicalTime::ZERO);
            if s.end < s.start {
                std::mem::swap(&mut s.start, &mut s.end);
            }
            if let AfterSong::Next { gap } = &mut s.then {
                *gap = gap.clamp(0.0, 600.0);
            }
            s.gear.tidy();
        }
    }
}

/// One message for a piece of gear.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "type")]
pub enum GearMessage {
    /// A program change (0-based) on `channel` (0-based), after a bank
    /// select when `bank` is set (CC 0 = its upper seven bits, CC 32 = its
    /// lower seven).
    Program {
        channel: u8,
        program: u8,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        bank: Option<u16>,
    },
    /// One controller value (a snapshot is several).
    Control {
        channel: u8,
        controller: u8,
        value: u8,
    },
    /// A system exclusive message (`F0 … F7`), e.g. a patch dump; `name`
    /// says what it is (the file it came from).
    #[serde(rename = "sysex")]
    SysEx {
        #[serde(default, skip_serializing_if = "String::is_empty")]
        name: String,
        data: Vec<u8>,
    },
}

impl GearMessage {
    /// The MIDI messages it is, in the order they go out.
    pub fn messages(&self) -> Vec<Vec<u8>> {
        match self {
            GearMessage::Program {
                channel,
                program,
                bank,
            } => {
                let ch = channel & 15;
                let mut v = Vec::new();
                if let Some(b) = bank {
                    let b = b & 0x3FFF;
                    v.push(vec![0xB0 | ch, 0, (b >> 7) as u8]);
                    v.push(vec![0xB0 | ch, 32, (b & 0x7F) as u8]);
                }
                v.push(vec![0xC0 | ch, program & 127]);
                v
            }
            GearMessage::Control {
                channel,
                controller,
                value,
            } => vec![vec![0xB0 | (channel & 15), controller & 127, value & 127]],
            GearMessage::SysEx { data, .. } => vec![data.clone()],
        }
    }

    /// A short description ("PC 5 bank 1 ch 1").
    pub fn describe(&self) -> String {
        match self {
            GearMessage::Program {
                channel,
                program,
                bank,
            } => match bank {
                Some(b) => format!("PC {} bank {b} ch {}", u16::from(*program) + 1, channel + 1),
                None => format!("PC {} ch {}", u16::from(*program) + 1, channel + 1),
            },
            GearMessage::Control {
                channel,
                controller,
                value,
            } => format!("CC {controller}={value} ch {}", channel + 1),
            GearMessage::SysEx { name, data } if name.is_empty() => {
                format!("SysEx ({} bytes)", data.len())
            }
            GearMessage::SysEx { name, data } => format!("SysEx {name} ({} bytes)", data.len()),
        }
    }
}

/// A change for one piece of gear: a message to an output.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GearChange {
    /// The output's port key (as [`crate::MidiOutputRouting::port`]).
    pub output: String,
    /// Milliseconds to wait before it (after the change before it).
    #[serde(default, skip_serializing_if = "is_zero")]
    pub wait_ms: u16,
    pub message: GearMessage,
}

fn is_zero(v: &u16) -> bool {
    *v == 0
}

/// When a song's gear changes are sent.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GearTiming {
    /// When the show stands on the song (cued, before it plays: the gear
    /// has the time to load); a song played into straight on from the one
    /// before gets them on its first frame.
    #[default]
    Cue,
    /// On the song's first frame, whenever playback crosses it.
    Start,
}

impl GearTiming {
    pub fn label(self) -> &'static str {
        match self {
            GearTiming::Cue => "When the Show Stands on It",
            GearTiming::Start => "On Its First Frame",
        }
    }
}

/// What a song sends to external gear.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SongGear {
    #[serde(default)]
    pub changes: Vec<GearChange>,
    #[serde(default)]
    pub timing: GearTiming,
}

/// A MIDI cable carries 3125 bytes a second (31250 baud, 10 bits a byte).
pub const WIRE_NS_PER_BYTE: u64 = 320_000;

impl SongGear {
    pub fn is_empty(&self) -> bool {
        self.changes.is_empty() && self.timing == GearTiming::Cue
    }

    /// The messages to send: (output, nanoseconds after the first, bytes),
    /// each after the one before it has gone down a MIDI cable and after
    /// its change's wait.
    pub fn schedule(&self) -> Vec<(&str, u64, Vec<u8>)> {
        let mut out = Vec::new();
        let mut at = 0u64;
        for c in &self.changes {
            at += u64::from(c.wait_ms) * 1_000_000;
            for m in c.message.messages() {
                let len = m.len() as u64;
                out.push((c.output.as_str(), at, m));
                at += len * WIRE_NS_PER_BYTE;
            }
        }
        out
    }

    /// Kept sane: channels, programs and values in range, banks 14-bit,
    /// SysEx complete (`F0 … F7`), outputs named.
    pub fn tidy(&mut self) {
        self.changes.retain_mut(|c| {
            match &mut c.message {
                GearMessage::Program {
                    channel,
                    program,
                    bank,
                } => {
                    *channel = (*channel).min(15);
                    *program = (*program).min(127);
                    *bank = bank.map(|b| b.min(0x3FFF));
                }
                GearMessage::Control {
                    channel,
                    controller,
                    value,
                } => {
                    *channel = (*channel).min(15);
                    *controller = (*controller).min(127);
                    *value = (*value).min(127);
                }
                GearMessage::SysEx { data, .. } => {
                    if data.len() < 2 || data[0] != 0xF0 || data[data.len() - 1] != 0xF7 {
                        return false;
                    }
                }
            }
            !c.output.trim().is_empty()
        });
    }

    /// The changes as text, one a line: `pc 5 bank 1 ch 1`, `cc 7=100 ch 2`,
    /// `sysex F0 43 … F7`, a `wait 200` (ms) before a change that waits,
    /// and `on "<output>"` wherever the output changes. [`SongGear::parse`]
    /// reads it back.
    pub fn to_text(&self) -> String {
        let mut lines = Vec::new();
        let mut output: Option<&str> = None;
        for c in &self.changes {
            if output != Some(c.output.as_str()) {
                lines.push(format!("on \"{}\"", c.output));
                output = Some(&c.output);
            }
            if c.wait_ms > 0 {
                lines.push(format!("wait {}", c.wait_ms));
            }
            lines.push(match &c.message {
                GearMessage::Program {
                    channel,
                    program,
                    bank,
                } => match bank {
                    Some(b) => {
                        format!("pc {} bank {b} ch {}", u16::from(*program) + 1, channel + 1)
                    }
                    None => format!("pc {} ch {}", u16::from(*program) + 1, channel + 1),
                },
                GearMessage::Control {
                    channel,
                    controller,
                    value,
                } => format!("cc {controller}={value} ch {}", channel + 1),
                GearMessage::SysEx { data, .. } => format!(
                    "sysex {}",
                    data.iter()
                        .map(|b| format!("{b:02X}"))
                        .collect::<Vec<_>>()
                        .join(" ")
                ),
            });
        }
        lines.join("\n")
    }

    /// Changes from text (see [`SongGear::to_text`]; lines or `;`
    /// separate them, programs are 1–128 and channels 1–16 as written,
    /// `ch` defaults to 1). Outputs not named go to `default_output`; a
    /// SysEx change named in `self` keeps its name when its bytes are the
    /// same. The timing is kept.
    pub fn parse(&self, text: &str, default_output: &str) -> Result<SongGear, String> {
        let mut changes = Vec::new();
        let mut output = default_output.to_string();
        let mut wait = 0u16;
        for raw in text.split(['\n', ';']) {
            let line = raw.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let (word, rest) = line.split_once(char::is_whitespace).unwrap_or((line, ""));
            let rest = rest.trim();
            let word = word.to_ascii_lowercase();
            let message = match word.as_str() {
                "on" | "to" | "out" => {
                    let name = rest.trim_matches('"').trim();
                    if name.is_empty() {
                        return Err(format!("'{line}': which output?"));
                    }
                    output = name.to_string();
                    continue;
                }
                "wait" => {
                    let ms = rest.trim_end_matches("ms").trim();
                    wait = ms
                        .parse::<u16>()
                        .map_err(|_| format!("'{line}': wait how many milliseconds?"))?;
                    continue;
                }
                "pc" | "program" => {
                    let mut words = rest.split_whitespace();
                    let program = number(words.next(), 1, 128, line)? - 1;
                    let (mut bank, mut channel) = (None, 0u8);
                    while let Some(w) = words.next() {
                        match w.to_ascii_lowercase().as_str() {
                            "bank" => bank = Some(number(words.next(), 0, 0x3FFF, line)? as u16),
                            "ch" | "channel" => {
                                channel = (number(words.next(), 1, 16, line)? - 1) as u8
                            }
                            _ => return Err(format!("'{line}': what is '{w}'?")),
                        }
                    }
                    GearMessage::Program {
                        channel,
                        program: program as u8,
                        bank,
                    }
                }
                "cc" | "control" => {
                    let mut words = rest.split_whitespace();
                    let pair = words.next().unwrap_or("");
                    let (cc, value) = pair
                        .split_once('=')
                        .ok_or_else(|| format!("'{line}': write cc <number>=<value>"))?;
                    let controller = number(Some(cc), 0, 127, line)? as u8;
                    let value = number(Some(value), 0, 127, line)? as u8;
                    let mut channel = 0u8;
                    while let Some(w) = words.next() {
                        match w.to_ascii_lowercase().as_str() {
                            "ch" | "channel" => {
                                channel = (number(words.next(), 1, 16, line)? - 1) as u8
                            }
                            _ => return Err(format!("'{line}': what is '{w}'?")),
                        }
                    }
                    GearMessage::Control {
                        channel,
                        controller,
                        value,
                    }
                }
                "sysex" | "syx" => {
                    let data: Vec<u8> = rest
                        .split(|c: char| c.is_whitespace() || c == ',')
                        .filter(|w| !w.is_empty())
                        .map(|w| {
                            u8::from_str_radix(w.trim_start_matches("0x"), 16)
                                .map_err(|_| format!("'{line}': '{w}' is not a hex byte"))
                        })
                        .collect::<Result<_, _>>()?;
                    if data.len() < 2 || data[0] != 0xF0 || data[data.len() - 1] != 0xF7 {
                        return Err(format!("'{line}': SysEx runs from F0 to F7"));
                    }
                    let name = self
                        .changes
                        .iter()
                        .find_map(|c| match &c.message {
                            GearMessage::SysEx { name, data: d } if *d == data => {
                                Some(name.clone())
                            }
                            _ => None,
                        })
                        .unwrap_or_default();
                    GearMessage::SysEx { name, data }
                }
                _ => return Err(format!("'{line}': pc, cc, sysex, wait or on?")),
            };
            if output.trim().is_empty() {
                return Err("no MIDI output to send to (on \"<output>\")".into());
            }
            changes.push(GearChange {
                output: output.clone(),
                wait_ms: std::mem::take(&mut wait),
                message,
            });
        }
        Ok(SongGear {
            changes,
            timing: self.timing,
        })
    }

    /// A short summary ("PC 5 bank 1 ch 1 · 2 more").
    pub fn summary(&self) -> String {
        match self.changes.as_slice() {
            [] => String::new(),
            [one] => one.message.describe(),
            [first, rest @ ..] => format!("{} · {} more", first.message.describe(), rest.len()),
        }
    }
}

fn number(w: Option<&str>, lo: u32, hi: u32, line: &str) -> Result<u32, String> {
    let w = w.ok_or_else(|| format!("'{line}': a number is missing"))?;
    let n: u32 = w
        .parse()
        .map_err(|_| format!("'{line}': '{w}' is not a number"))?;
    if n < lo || n > hi {
        return Err(format!("'{line}': {n} is not within {lo}–{hi}"));
    }
    Ok(n)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn change(output: &str, message: GearMessage) -> GearChange {
        GearChange {
            output: output.into(),
            wait_ms: 0,
            message,
        }
    }

    #[test]
    fn a_program_change_goes_after_its_bank_select() {
        let m = GearMessage::Program {
            channel: 2,
            program: 4,
            bank: Some(130),
        };
        assert_eq!(
            m.messages(),
            vec![vec![0xB2, 0, 1], vec![0xB2, 32, 2], vec![0xC2, 4]]
        );
        assert_eq!(m.describe(), "PC 5 bank 130 ch 3");
    }

    #[test]
    fn messages_are_spaced_as_a_cable_carries_them_and_wait_when_asked() {
        let gear = SongGear {
            changes: vec![
                change(
                    "a",
                    GearMessage::SysEx {
                        name: String::new(),
                        data: vec![0xF0, 1, 2, 3, 0xF7],
                    },
                ),
                GearChange {
                    wait_ms: 100,
                    ..change(
                        "b",
                        GearMessage::Control {
                            channel: 0,
                            controller: 7,
                            value: 90,
                        },
                    )
                },
            ],
            timing: GearTiming::Cue,
        };
        let s = gear.schedule();
        assert_eq!(s.len(), 2);
        assert_eq!((s[0].0, s[0].1), ("a", 0));
        assert_eq!(s[1].0, "b");
        assert_eq!(s[1].1, 5 * WIRE_NS_PER_BYTE + 100_000_000);
    }

    #[test]
    fn the_text_form_reads_back() {
        let gear = SongGear {
            changes: vec![
                change(
                    "Synth A",
                    GearMessage::Program {
                        channel: 0,
                        program: 4,
                        bank: Some(1),
                    },
                ),
                GearChange {
                    wait_ms: 250,
                    ..change(
                        "Synth A",
                        GearMessage::Control {
                            channel: 1,
                            controller: 7,
                            value: 100,
                        },
                    )
                },
                change(
                    "Rack",
                    GearMessage::SysEx {
                        name: "patch.syx".into(),
                        data: vec![0xF0, 0x43, 0x10, 0xF7],
                    },
                ),
            ],
            timing: GearTiming::Start,
        };
        let text = gear.to_text();
        assert!(text.contains("pc 5 bank 1 ch 1"), "{text}");
        assert!(text.contains("on \"Rack\""), "{text}");
        let back = gear.parse(&text, "unused").unwrap();
        assert_eq!(back, gear);
        // A line of its own, with semicolons, on the default output.
        let quick = SongGear::default()
            .parse("pc 12; cc 64=127 ch 10", "Out")
            .unwrap();
        assert_eq!(quick.changes.len(), 2);
        assert_eq!(quick.changes[0].output, "Out");
        assert_eq!(
            quick.changes[1].message,
            GearMessage::Control {
                channel: 9,
                controller: 64,
                value: 127
            }
        );
        for bad in [
            "pc 0",
            "pc 129",
            "cc 7",
            "sysex 43 10",
            "bogus 1",
            "pc 5 ch 17",
        ] {
            assert!(SongGear::default().parse(bad, "Out").is_err(), "{bad}");
        }
        assert!(SongGear::default().parse("pc 1", "").is_err(), "no output");
    }

    #[test]
    fn tidy_drops_broken_sysex_and_unnamed_outputs() {
        let mut gear = SongGear {
            changes: vec![
                change(
                    "a",
                    GearMessage::SysEx {
                        name: String::new(),
                        data: vec![0xF0, 1],
                    },
                ),
                change(
                    " ",
                    GearMessage::Control {
                        channel: 0,
                        controller: 1,
                        value: 1,
                    },
                ),
                change(
                    "a",
                    GearMessage::Program {
                        channel: 40,
                        program: 200,
                        bank: Some(u16::MAX),
                    },
                ),
            ],
            timing: GearTiming::Cue,
        };
        gear.tidy();
        assert_eq!(gear.changes.len(), 1);
        assert_eq!(
            gear.changes[0].message,
            GearMessage::Program {
                channel: 15,
                program: 127,
                bank: Some(0x3FFF)
            }
        );
    }
}
