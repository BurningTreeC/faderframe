//! Articulation (expression) maps, as in Cubase.
//!
//! A map lists an instrument's articulations — name, the symbol shown on
//! notes, a group, and whether it is an *attribute* (set on a note: that
//! note only) or a *direction* (set in a clip's articulation lane: from
//! there on until the next one of its group) — and what each sends to
//! switch the instrument to it: keyswitch notes, a program change,
//! controller values, a channel the notes go out on, and changes to the
//! notes themselves (transpose, velocity and length in percent).
//!
//! One articulation of each group is in effect at a time (groups combine:
//! "legato" in group 1 with "con sordino" in group 2). Before any direction
//! a group plays its first articulation (the map's default). What a
//! combination sends is what its members send together, unless the map
//! has a [`Combination`] for exactly those (Cubase's sound slots that name
//! several articulations).
//!
//! Projects hold their maps ([`crate::Project::articulation_maps`]), tracks
//! name one ([`crate::Track::articulation_map`]); notes carry their
//! attribute ([`crate::MidiNote::articulation`]) and clips their
//! directions ([`crate::MidiClip::articulations`]). The engine's timeline
//! snapshot turns them into keyswitches before the notes (see
//! `faderframe_engine::snapshot`). Maps are also kept as user files
//! ([`ArticulationMap::to_file`]).

use faderframe_core::ArticulationMapId;
use faderframe_midi::MidiEvent;
use faderframe_timeline::MusicalTime;
use serde::{Deserialize, Serialize};

/// Groups an articulation can be in; one of each is in effect at a time.
pub const GROUPS: usize = 4;

/// What a user file holds (its `format` field).
const FILE_FORMAT: &str = "faderframe-articulation-map";

/// An articulation of a map (unique within its map, kept when the map is
/// edited; notes and lanes refer to it by this).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ArticulationId(pub u16);

/// How an articulation applies.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ArticulationKind {
    /// Set on a note: that note only (the ones after it go back).
    #[default]
    Attribute,
    /// Set in the clip's articulation lane: from there until the next
    /// direction of its group.
    Direction,
}

impl ArticulationKind {
    pub fn label(self) -> &'static str {
        match self {
            ArticulationKind::Attribute => "Attribute",
            ArticulationKind::Direction => "Direction",
        }
    }
}

fn full() -> u8 {
    100
}

fn percent() -> u8 {
    100
}

fn is_percent(v: &u8) -> bool {
    *v == 100
}

fn is_zero(v: &i8) -> bool {
    *v == 0
}

/// A keyswitch: a note pressed and let go right before the notes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Keyswitch {
    pub key: u8,
    #[serde(default = "full")]
    pub velocity: u8,
}

/// A controller value sent to switch.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ControlValue {
    pub controller: u8,
    pub value: u8,
}

/// What switching to an articulation sends, and what it does to its notes.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArticulationOutput {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub keyswitches: Vec<Keyswitch>,
    /// A program change (0-based).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub program: Option<u8>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub controls: Vec<ControlValue>,
    /// The notes (and what switches) go out on this channel (0-based;
    /// libraries with an articulation per channel).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub channel: Option<u8>,
    /// Semitones the notes move.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub transpose: i8,
    /// The notes' velocity in percent.
    #[serde(default = "percent", skip_serializing_if = "is_percent")]
    pub velocity: u8,
    /// The notes' length in percent.
    #[serde(default = "percent", skip_serializing_if = "is_percent")]
    pub length: u8,
}

impl Default for ArticulationOutput {
    fn default() -> Self {
        Self {
            keyswitches: Vec::new(),
            program: None,
            controls: Vec::new(),
            channel: None,
            transpose: 0,
            velocity: 100,
            length: 100,
        }
    }
}

impl ArticulationOutput {
    /// A single keyswitch.
    pub fn keyswitch(key: u8) -> Self {
        Self {
            keyswitches: vec![Keyswitch { key, velocity: 100 }],
            ..Self::default()
        }
    }

    /// Sends nothing and leaves the notes as they are.
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }

    /// In range (keys and values 0–127, channels 0–15, percents 1–400).
    fn tidy(&mut self) {
        for k in &mut self.keyswitches {
            k.key = k.key.min(127);
            k.velocity = k.velocity.clamp(1, 127);
        }
        self.program = self.program.map(|p| p.min(127));
        for c in &mut self.controls {
            c.controller = c.controller.min(127);
            c.value = c.value.min(127);
        }
        self.channel = self.channel.map(|c| c.min(15));
        self.transpose = self.transpose.clamp(-48, 48);
        self.velocity = self.velocity.clamp(1, 250);
        self.length = self.length.clamp(1, 250);
    }

    /// A short description ("C0 · PC 3 · CC 1=64 · ch 2").
    pub fn describe(&self) -> String {
        let mut parts: Vec<String> = self
            .keyswitches
            .iter()
            .map(|k| crate::articulation::key_name(k.key))
            .collect();
        if let Some(p) = self.program {
            parts.push(format!("PC {}", u16::from(p) + 1));
        }
        parts.extend(
            self.controls
                .iter()
                .map(|c| format!("CC {}={}", c.controller, c.value)),
        );
        if let Some(c) = self.channel {
            parts.push(format!("ch {}", c + 1));
        }
        if self.transpose != 0 {
            parts.push(format!("{:+} st", self.transpose));
        }
        if self.velocity != 100 {
            parts.push(format!("vel {}%", self.velocity));
        }
        if self.length != 100 {
            parts.push(format!("len {}%", self.length));
        }
        if parts.is_empty() {
            "nothing".into()
        } else {
            parts.join(" · ")
        }
    }
}

/// "C0" style key name (middle C = C4 = 60, as the piano roll names keys).
pub fn key_name(key: u8) -> String {
    const NAMES: [&str; 12] = [
        "C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B",
    ];
    format!(
        "{}{}",
        NAMES[usize::from(key % 12)],
        i32::from(key) / 12 - 1
    )
}

/// One articulation of a map.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Articulation {
    pub id: ArticulationId,
    pub name: String,
    /// What notes and the lane show (Cubase's text), e.g. "stacc.".
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub symbol: String,
    /// 0 … [`GROUPS`] − 1.
    #[serde(default)]
    pub group: u8,
    #[serde(default)]
    pub kind: ArticulationKind,
    #[serde(default)]
    pub output: ArticulationOutput,
}

impl Articulation {
    /// The symbol, else the name.
    pub fn short(&self) -> &str {
        if self.symbol.is_empty() {
            &self.name
        } else {
            &self.symbol
        }
    }
}

/// Articulations of different groups together sending something of their
/// own (instead of what each sends).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Combination {
    pub articulations: Vec<ArticulationId>,
    #[serde(default)]
    pub output: ArticulationOutput,
}

/// An articulation per group (`None`: the map has none in that group).
pub type ArticulationState = [Option<ArticulationId>; GROUPS];

/// What a state sends and does to its notes.
#[derive(Clone, Debug, PartialEq)]
pub struct Resolved {
    pub keyswitches: Vec<Keyswitch>,
    pub program: Option<u8>,
    pub controls: Vec<ControlValue>,
    pub channel: Option<u8>,
    pub transpose: i32,
    /// Factors (1 = as written).
    pub velocity: f64,
    pub length: f64,
}

impl Resolved {
    fn add(&mut self, o: &ArticulationOutput) {
        self.keyswitches.extend(o.keyswitches.iter().copied());
        if o.program.is_some() {
            self.program = o.program;
        }
        self.controls.extend(o.controls.iter().copied());
        if o.channel.is_some() {
            self.channel = o.channel;
        }
        self.transpose += i32::from(o.transpose);
        self.velocity *= f64::from(o.velocity) / 100.0;
        self.length *= f64::from(o.length) / 100.0;
    }

    /// The messages that switch to it, on `channel` unless it has its own:
    /// controllers, the program, then the keyswitch note-ons (their
    /// note-offs are [`Resolved::releases`]).
    pub fn switch_events(&self, channel: u8) -> Vec<MidiEvent> {
        let channel = self.channel.unwrap_or(channel) & 15;
        let mut out: Vec<MidiEvent> = self
            .controls
            .iter()
            .map(|c| MidiEvent::ControlChange {
                channel,
                controller: c.controller & 127,
                value: c.value & 127,
            })
            .collect();
        if let Some(program) = self.program {
            out.push(MidiEvent::ProgramChange {
                channel,
                program: program & 127,
            });
        }
        out.extend(self.keyswitches.iter().map(|k| MidiEvent::NoteOn {
            channel,
            key: k.key & 127,
            velocity: k.velocity.clamp(1, 127),
        }));
        out
    }

    /// The keyswitches' note-offs.
    pub fn releases(&self, channel: u8) -> Vec<MidiEvent> {
        let channel = self.channel.unwrap_or(channel) & 15;
        self.keyswitches
            .iter()
            .map(|k| MidiEvent::NoteOff {
                channel,
                key: k.key & 127,
                velocity: 0,
            })
            .collect()
    }

    /// A note as it goes out: (channel, key, velocity, length).
    pub fn note(
        &self,
        channel: u8,
        key: u8,
        velocity: u8,
        length: MusicalTime,
    ) -> (u8, u8, u8, MusicalTime) {
        let key = (i32::from(key) + self.transpose).clamp(0, 127) as u8;
        let velocity = (f64::from(velocity) * self.velocity)
            .round()
            .clamp(1.0, 127.0) as u8;
        let length = MusicalTime(((length.ticks() as f64 * self.length).round() as i64).max(1));
        (self.channel.unwrap_or(channel) & 15, key, velocity, length)
    }

    /// Leaves the notes as they are.
    pub fn keeps_notes(&self) -> bool {
        self.channel.is_none() && self.transpose == 0 && self.velocity == 1.0 && self.length == 1.0
    }
}

/// An instrument's articulations and what each sends.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArticulationMap {
    pub id: ArticulationMapId,
    pub name: String,
    #[serde(default)]
    pub articulations: Vec<Articulation>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub combinations: Vec<Combination>,
}

/// A user file: the map without its project id.
#[derive(Serialize, Deserialize)]
struct MapFile {
    format: String,
    version: u32,
    name: String,
    #[serde(default)]
    articulations: Vec<Articulation>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    combinations: Vec<Combination>,
}

impl ArticulationMap {
    pub fn new(id: ArticulationMapId, name: impl Into<String>) -> Self {
        Self {
            id,
            name: name.into(),
            articulations: Vec::new(),
            combinations: Vec::new(),
        }
    }

    pub fn get(&self, id: ArticulationId) -> Option<&Articulation> {
        self.articulations.iter().find(|a| a.id == id)
    }

    pub fn get_mut(&mut self, id: ArticulationId) -> Option<&mut Articulation> {
        self.articulations.iter_mut().find(|a| a.id == id)
    }

    /// An articulation by name (case-insensitive) or symbol.
    pub fn find(&self, name: &str) -> Option<&Articulation> {
        let n = name.trim();
        self.articulations
            .iter()
            .find(|a| a.name.eq_ignore_ascii_case(n))
            .or_else(|| {
                self.articulations
                    .iter()
                    .find(|a| !a.symbol.is_empty() && a.symbol.eq_ignore_ascii_case(n))
            })
    }

    /// An id no articulation of the map has.
    pub fn next_id(&self) -> ArticulationId {
        ArticulationId(
            self.articulations
                .iter()
                .map(|a| a.id.0)
                .max()
                .map_or(1, |m| m.saturating_add(1)),
        )
    }

    /// Add an articulation (its id is the map's next).
    pub fn add(
        &mut self,
        name: impl Into<String>,
        symbol: impl Into<String>,
        group: u8,
        kind: ArticulationKind,
        output: ArticulationOutput,
    ) -> ArticulationId {
        let id = self.next_id();
        self.articulations.push(Articulation {
            id,
            name: name.into(),
            symbol: symbol.into(),
            group: group.min(GROUPS as u8 - 1),
            kind,
            output,
        });
        id
    }

    /// The groups articulations are in, in order.
    pub fn groups(&self) -> Vec<u8> {
        let mut g: Vec<u8> = self.articulations.iter().map(|a| a.group).collect();
        g.sort_unstable();
        g.dedup();
        g
    }

    /// Each group's first articulation: what plays before any direction.
    pub fn default_state(&self) -> ArticulationState {
        let mut s = [None; GROUPS];
        for a in &self.articulations {
            let g = usize::from(a.group).min(GROUPS - 1);
            if s[g].is_none() {
                s[g] = Some(a.id);
            }
        }
        s
    }

    /// A note's state: the directions in effect with the note's attribute
    /// over its group's.
    pub fn state(
        &self,
        directions: &ArticulationState,
        attribute: Option<ArticulationId>,
    ) -> ArticulationState {
        let mut s = *directions;
        if let Some(a) = attribute.and_then(|id| self.get(id)) {
            s[usize::from(a.group).min(GROUPS - 1)] = Some(a.id);
        }
        s
    }

    /// What a state sends: a combination naming exactly its articulations,
    /// else each group's articulation in group order.
    pub fn resolve(&self, state: &ArticulationState) -> Resolved {
        let mut r = Resolved {
            keyswitches: Vec::new(),
            program: None,
            controls: Vec::new(),
            channel: None,
            transpose: 0,
            velocity: 1.0,
            length: 1.0,
        };
        let members: Vec<ArticulationId> = state.iter().flatten().copied().collect();
        let combined = self.combinations.iter().find(|c| {
            c.articulations.len() == members.len()
                && c.articulations.iter().all(|a| members.contains(a))
        });
        match combined {
            Some(c) => r.add(&c.output),
            None => {
                for id in members {
                    if let Some(a) = self.get(id) {
                        r.add(&a.output);
                    }
                }
            }
        }
        r
    }

    /// Kept sane: unique ids, groups in range, outputs in range,
    /// combinations of two or more articulations there are, one per group.
    pub fn tidy(&mut self) {
        let mut seen = std::collections::BTreeSet::new();
        let mut next = self.next_id().0;
        for a in &mut self.articulations {
            if !seen.insert(a.id) {
                a.id = ArticulationId(next);
                next = next.saturating_add(1);
                seen.insert(a.id);
            }
            a.group = a.group.min(GROUPS as u8 - 1);
            a.output.tidy();
        }
        let groups: std::collections::BTreeMap<ArticulationId, u8> =
            self.articulations.iter().map(|a| (a.id, a.group)).collect();
        for c in &mut self.combinations {
            c.articulations.retain(|a| groups.contains_key(a));
            let mut taken = [false; GROUPS];
            c.articulations.retain(|a| {
                let g = usize::from(groups[a]);
                !std::mem::replace(&mut taken[g], true)
            });
            c.output.tidy();
        }
        self.combinations.retain(|c| c.articulations.len() >= 2);
    }

    /// The map as a user file (JSON; without its project id).
    pub fn to_file(&self) -> String {
        let f = MapFile {
            format: FILE_FORMAT.into(),
            version: 1,
            name: self.name.clone(),
            articulations: self.articulations.clone(),
            combinations: self.combinations.clone(),
        };
        serde_json::to_string_pretty(&f).unwrap_or_default()
    }

    /// A map read from a user file, given `id`.
    pub fn from_file(id: ArticulationMapId, text: &str) -> Result<Self, String> {
        let f: MapFile = serde_json::from_str(text).map_err(|e| e.to_string())?;
        if f.format != FILE_FORMAT {
            return Err(format!("not an articulation map ({})", f.format));
        }
        let mut m = Self {
            id,
            name: f.name,
            articulations: f.articulations,
            combinations: f.combinations,
        };
        m.tidy();
        Ok(m)
    }

    /// Built-in starting points, by name: "Strings" (sustain, staccato,
    /// pizzicato, tremolo, spiccato on keyswitches from C0, legato as a
    /// group 2 direction on CC 68) and "Brass" (sustain, staccato,
    /// marcato, sforzando, fall from C0).
    pub fn template(id: ArticulationMapId, name: &str) -> Option<Self> {
        use ArticulationKind::*;
        let mut m = Self::new(id, name.to_string());
        match name.to_ascii_lowercase().as_str() {
            "strings" => {
                m.name = "Strings".into();
                m.add(
                    "Sustain",
                    "sus.",
                    0,
                    Direction,
                    ArticulationOutput::keyswitch(24),
                );
                m.add(
                    "Staccato",
                    "stacc.",
                    0,
                    Attribute,
                    ArticulationOutput::keyswitch(25),
                );
                m.add(
                    "Pizzicato",
                    "pizz.",
                    0,
                    Direction,
                    ArticulationOutput::keyswitch(26),
                );
                m.add(
                    "Tremolo",
                    "trem.",
                    0,
                    Direction,
                    ArticulationOutput::keyswitch(27),
                );
                m.add(
                    "Spiccato",
                    "spicc.",
                    0,
                    Attribute,
                    ArticulationOutput::keyswitch(28),
                );
                m.add(
                    "Non legato",
                    "non leg.",
                    1,
                    Direction,
                    ArticulationOutput {
                        controls: vec![ControlValue {
                            controller: 68,
                            value: 0,
                        }],
                        ..ArticulationOutput::default()
                    },
                );
                m.add(
                    "Legato",
                    "leg.",
                    1,
                    Direction,
                    ArticulationOutput {
                        controls: vec![ControlValue {
                            controller: 68,
                            value: 127,
                        }],
                        ..ArticulationOutput::default()
                    },
                );
            }
            "brass" => {
                m.name = "Brass".into();
                for (i, (n, s, kind)) in [
                    ("Sustain", "sus.", Direction),
                    ("Staccato", "stacc.", Attribute),
                    ("Marcato", "marc.", Attribute),
                    ("Sforzando", "sfz", Attribute),
                    ("Fall", "fall", Attribute),
                ]
                .into_iter()
                .enumerate()
                {
                    m.add(n, s, 0, kind, ArticulationOutput::keyswitch(24 + i as u8));
                }
            }
            _ => return None,
        }
        Some(m)
    }

    /// The templates' names.
    pub const TEMPLATES: [&'static str; 2] = ["Strings", "Brass"];
}

/// A direction in a clip's articulation lane.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArticulationEvent {
    /// From the clip start.
    pub time: MusicalTime,
    pub articulation: ArticulationId,
}

/// The directions along a stretch of music (a track's clips in time
/// order): per group where each starts.
#[derive(Clone, Debug, Default)]
pub struct Directions {
    starts: [Vec<(MusicalTime, ArticulationId)>; GROUPS],
    defaults: ArticulationState,
}

impl Directions {
    /// From directions at absolute times (any order). Articulations the
    /// map does not have, or that are attributes, are left out.
    pub fn new(
        map: &ArticulationMap,
        events: impl IntoIterator<Item = (MusicalTime, ArticulationId)>,
    ) -> Self {
        let mut d = Self {
            starts: Default::default(),
            defaults: map.default_state(),
        };
        for (t, id) in events {
            let Some(a) = map.get(id) else { continue };
            if a.kind != ArticulationKind::Direction {
                continue;
            }
            d.starts[usize::from(a.group).min(GROUPS - 1)].push((t, id));
        }
        for g in &mut d.starts {
            // Stable: of two at one time, the later listed wins.
            g.sort_by_key(|(t, _)| *t);
        }
        d
    }

    /// The directions in effect at `t` (a direction at `t` included).
    pub fn at(&self, t: MusicalTime) -> ArticulationState {
        let mut s = self.defaults;
        for (g, starts) in self.starts.iter().enumerate() {
            let n = starts.partition_point(|(at, _)| *at <= t);
            if n > 0 {
                s[g] = Some(starts[n - 1].1);
            }
        }
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn map() -> ArticulationMap {
        ArticulationMap::template(ArticulationMapId(1), "strings").unwrap()
    }

    #[test]
    fn notes_take_the_directions_in_effect_and_their_own_attribute() {
        let m = map();
        let sus = m.find("Sustain").unwrap().id;
        let stacc = m.find("stacc.").unwrap().id;
        let pizz = m.find("Pizzicato").unwrap().id;
        let legato = m.find("Legato").unwrap().id;
        let nonleg = m.find("Non legato").unwrap().id;
        // Before any direction: each group's first.
        assert_eq!(m.default_state(), [Some(sus), Some(nonleg), None, None]);
        let q = MusicalTime::from_quarters_i;
        let d = Directions::new(
            &m,
            [
                (q(4), pizz),
                (q(2), legato),
                // An attribute in the lane is not a direction.
                (q(1), stacc),
            ],
        );
        assert_eq!(d.at(q(0)), [Some(sus), Some(nonleg), None, None]);
        assert_eq!(d.at(q(2)), [Some(sus), Some(legato), None, None]);
        assert_eq!(d.at(q(5)), [Some(pizz), Some(legato), None, None]);
        // A staccato note over pizzicato: staccato for it alone.
        let s = m.state(&d.at(q(5)), Some(stacc));
        assert_eq!(s, [Some(stacc), Some(legato), None, None]);
        let r = m.resolve(&s);
        assert_eq!(
            r.keyswitches,
            vec![Keyswitch {
                key: 25,
                velocity: 100
            }]
        );
        assert_eq!(
            r.controls,
            vec![ControlValue {
                controller: 68,
                value: 127
            }]
        );
        // Controllers, then the keyswitch note-on.
        let ev = r.switch_events(3);
        assert!(matches!(
            ev[0],
            MidiEvent::ControlChange {
                channel: 3,
                controller: 68,
                value: 127
            }
        ));
        assert!(matches!(
            ev[1],
            MidiEvent::NoteOn {
                channel: 3,
                key: 25,
                ..
            }
        ));
        assert_eq!(r.releases(3).len(), 1);
    }

    #[test]
    fn a_combination_sends_its_own_and_notes_change_as_the_output_says() {
        let mut m = map();
        let sus = m.find("Sustain").unwrap().id;
        let legato = m.find("Legato").unwrap().id;
        m.combinations.push(Combination {
            articulations: vec![legato, sus],
            output: ArticulationOutput {
                channel: Some(4),
                transpose: -12,
                velocity: 50,
                length: 200,
                ..ArticulationOutput::keyswitch(36)
            },
        });
        let r = m.resolve(&[Some(sus), Some(legato), None, None]);
        assert_eq!(r.keyswitches.len(), 1);
        assert_eq!(r.keyswitches[0].key, 36);
        assert!(r.controls.is_empty(), "the combination's, not each one's");
        let (ch, key, vel, len) = r.note(0, 60, 100, MusicalTime(480));
        assert_eq!((ch, key, vel, len), (4, 48, 50, MusicalTime(960)));
        assert!(!r.keeps_notes());
        assert!(m.resolve(&m.default_state()).keeps_notes());
    }

    #[test]
    fn tidy_keeps_ids_unique_and_combinations_sane() {
        let mut m = map();
        let first = m.articulations[0].id;
        m.articulations[1].id = first;
        m.articulations[2].group = 9;
        m.combinations.push(Combination {
            articulations: vec![first, ArticulationId(999)],
            output: ArticulationOutput::default(),
        });
        m.tidy();
        let ids: std::collections::BTreeSet<_> = m.articulations.iter().map(|a| a.id).collect();
        assert_eq!(ids.len(), m.articulations.len());
        assert_eq!(m.articulations[2].group, GROUPS as u8 - 1);
        assert!(
            m.combinations.is_empty(),
            "one articulation is no combination"
        );
    }

    #[test]
    fn a_map_round_trips_through_its_file() {
        let m = map();
        let text = m.to_file();
        assert!(text.contains(FILE_FORMAT));
        let back = ArticulationMap::from_file(ArticulationMapId(7), &text).unwrap();
        assert_eq!(back.id, ArticulationMapId(7));
        assert_eq!(back.articulations, m.articulations);
        assert_eq!(back.name, m.name);
        assert!(
            ArticulationMap::from_file(
                ArticulationMapId(7),
                "{\"format\":\"x\",\"version\":1,\"name\":\"a\"}"
            )
            .is_err()
        );
    }

    #[test]
    fn maps_are_added_named_by_tracks_and_kept_while_named() {
        use crate::{Command, Project, Track, TrackColor, TrackKind};
        let mut p = Project::new("Maps", 48_000);
        let id: faderframe_core::TrackId = p.ids.allocate();
        let track = Track::new(id, TrackKind::Instrument, "Strings", TrackColor::palette(0));
        Command::AddTrack {
            track: Box::new(track),
            index: 0,
        }
        .apply(&mut p)
        .unwrap();
        let map_id: ArticulationMapId = p.ids.allocate();
        let mut m = ArticulationMap::template(map_id, "strings").unwrap();
        m.articulations[0].group = 7;
        let undo_add = Command::SetArticulationMap {
            map: map_id,
            value: Some(Box::new(m)),
            index: 0,
        }
        .apply(&mut p)
        .unwrap();
        assert_eq!(p.articulation_maps.len(), 1);
        assert_eq!(p.articulation_maps[0].articulations[0].group, 3, "tidied");
        let undo_use = Command::SetTrackArticulationMap {
            track: id,
            map: Some(map_id),
        }
        .apply(&mut p)
        .unwrap();
        assert_eq!(p.track_articulation_map(id).map(|m| m.id), Some(map_id));
        // Named by a track: not removed.
        assert!(
            Command::SetArticulationMap {
                map: map_id,
                value: None,
                index: 0,
            }
            .apply(&mut p)
            .is_err()
        );
        assert!(
            Command::SetTrackArticulationMap {
                track: id,
                map: Some(ArticulationMapId(9999)),
            }
            .apply(&mut p)
            .is_err(),
            "a map that is not there"
        );
        undo_use.apply(&mut p).unwrap();
        assert_eq!(p.track(id).unwrap().articulation_map, None);
        let redo = undo_add.apply(&mut p).unwrap();
        assert!(p.articulation_maps.is_empty());
        redo.apply(&mut p).unwrap();
        assert_eq!(p.articulation_maps.len(), 1);
        assert_eq!(
            Command::SetTrackArticulationMap {
                track: id,
                map: None
            }
            .impact(),
            crate::Impact::Timeline
        );
    }
}
