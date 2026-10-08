//! MIDI tools: transformations of selected notes and generators of new
//! ones (pure; the piano roll previews them, the session applies them).
//!
//! Transformations take the selected notes and return what replaces them:
//! notes they keep (with their ids) and new ones (id 0). Generators fill a
//! time range. Both see the clip's place in the song, the meter, the
//! scale at a time and the chord track ([`ToolContext`]), and are
//! deterministic (random ones by their seed), so a preview is exactly
//! what is applied.

use crate::MidiNote;
use crate::harmony::ChordEvent;
use crate::midi_ops::{Rng, Scale};
use faderframe_core::NoteId;
use faderframe_midi::theory::{Chord, Key};
use faderframe_timeline::{MusicalTime, TimeSignatureMap};

/// Note values the tools step in (name, quarters).
pub const STEPS: [(&str, f64); 13] = [
    ("1/128", 0.031_25),
    ("1/64", 0.0625),
    ("1/32T", 1.0 / 12.0),
    ("1/32", 0.125),
    ("1/16T", 1.0 / 6.0),
    ("1/16", 0.25),
    ("1/8T", 1.0 / 3.0),
    ("1/8", 0.5),
    ("1/4T", 2.0 / 3.0),
    ("1/4", 1.0),
    ("1/2", 2.0),
    ("1/1", 4.0),
    ("2/1", 8.0),
];

/// A step's length.
pub fn step(i: u8) -> MusicalTime {
    MusicalTime::from_quarters(STEPS[usize::from(i).min(STEPS.len() - 1)].1)
}

const S64: u8 = 1;
const S16: u8 = 5;
const S8: u8 = 7;
const S4: u8 = 9;
const S2: u8 = 10;

/// Factors Time Scale offers.
pub const FACTORS: [(&str, f64); 6] = [
    ("×½ (double time)", 0.5),
    ("×⅔", 2.0 / 3.0),
    ("×¾", 0.75),
    ("×1½", 1.5),
    ("×2 (half time)", 2.0),
    ("×3", 3.0),
];

/// The tools, in the order the panel lists them.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Tool {
    #[default]
    Strum,
    Chop,
    Join,
    Connect,
    Arpeggiate,
    Recombine,
    Conform,
    Accent,
    TimeScale,
    Ornament,
    Euclid,
    Seed,
    Chords,
    Bassline,
    Drums,
}

impl Tool {
    pub const TRANSFORMS: [Tool; 10] = [
        Tool::Strum,
        Tool::Chop,
        Tool::Join,
        Tool::Connect,
        Tool::Arpeggiate,
        Tool::Recombine,
        Tool::Conform,
        Tool::Accent,
        Tool::TimeScale,
        Tool::Ornament,
    ];
    pub const GENERATORS: [Tool; 5] = [
        Tool::Euclid,
        Tool::Seed,
        Tool::Chords,
        Tool::Bassline,
        Tool::Drums,
    ];

    pub fn is_generator(self) -> bool {
        Self::GENERATORS.contains(&self)
    }

    pub fn label(self) -> &'static str {
        match self {
            Tool::Strum => "Strum",
            Tool::Chop => "Chop",
            Tool::Join => "Join",
            Tool::Connect => "Connect",
            Tool::Arpeggiate => "Arpeggiate",
            Tool::Recombine => "Recombine",
            Tool::Conform => "Conform to Chords",
            Tool::Accent => "Accent",
            Tool::TimeScale => "Time Scale",
            Tool::Ornament => "Ornament",
            Tool::Euclid => "Euclidean Rhythm",
            Tool::Seed => "Seed Melody",
            Tool::Chords => "Chords",
            Tool::Bassline => "Bassline",
            Tool::Drums => "Drum Pattern",
        }
    }

    /// What it does, in a line.
    pub fn about(self) -> &'static str {
        match self {
            Tool::Strum => "Notes starting together start one after another, ending together",
            Tool::Chop => "Each note cut into equal parts (ratchets), louder or softer as they go",
            Tool::Join => "Repeated notes of the same key joined into one",
            Tool::Connect => "Gaps in the melody filled with a run through the scale",
            Tool::Arpeggiate => "Chords played as arpeggios over their length",
            Tool::Recombine => {
                "Pitches, velocities or lengths shuffled, rotated or reversed; the rhythm stays"
            }
            Tool::Conform => {
                "Notes moved to the chord track's chord tones (the scale where there is no chord)"
            }
            Tool::Accent => "Notes on the downbeats, beats or offbeats louder, the others softer",
            Tool::TimeScale => "Faster or slower: starts and lengths scaled from the first note",
            Tool::Ornament => "A grace note, flam or mordent before each note",
            Tool::Euclid => "Hits spread as evenly as can be over the steps, repeated",
            Tool::Seed => "A melody in the key, from a seed: the same seed, the same melody",
            Tool::Chords => {
                "The chord track's chords as voiced chords, led smoothly from one to the next"
            }
            Tool::Bassline => "A bassline on the chord track's roots",
            Tool::Drums => "A drum pattern on the General MIDI keys (the Drum Sampler's pads)",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Direction {
    #[default]
    Up,
    Down,
    Alternate,
}

impl Direction {
    pub const ALL: [Direction; 3] = [Direction::Up, Direction::Down, Direction::Alternate];
    pub fn label(self) -> &'static str {
        match self {
            Direction::Up => "Up",
            Direction::Down => "Down",
            Direction::Alternate => "Alternate",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ArpOrder {
    #[default]
    Up,
    Down,
    UpDown,
    Random,
}

impl ArpOrder {
    pub const ALL: [ArpOrder; 4] = [
        ArpOrder::Up,
        ArpOrder::Down,
        ArpOrder::UpDown,
        ArpOrder::Random,
    ];
    pub fn label(self) -> &'static str {
        match self {
            ArpOrder::Up => "Up",
            ArpOrder::Down => "Down",
            ArpOrder::UpDown => "Up-Down",
            ArpOrder::Random => "Random",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Recombine {
    #[default]
    ShufflePitches,
    RotatePitches,
    ReversePitches,
    ShuffleVelocities,
    ShuffleLengths,
}

impl Recombine {
    pub const ALL: [Recombine; 5] = [
        Recombine::ShufflePitches,
        Recombine::RotatePitches,
        Recombine::ReversePitches,
        Recombine::ShuffleVelocities,
        Recombine::ShuffleLengths,
    ];
    pub fn label(self) -> &'static str {
        match self {
            Recombine::ShufflePitches => "Shuffle Pitches",
            Recombine::RotatePitches => "Rotate Pitches",
            Recombine::ReversePitches => "Reverse Pitches",
            Recombine::ShuffleVelocities => "Shuffle Velocities",
            Recombine::ShuffleLengths => "Shuffle Lengths",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum AccentOn {
    #[default]
    Downbeats,
    Beats,
    Offbeats,
}

impl AccentOn {
    pub const ALL: [AccentOn; 3] = [AccentOn::Downbeats, AccentOn::Beats, AccentOn::Offbeats];
    pub fn label(self) -> &'static str {
        match self {
            AccentOn::Downbeats => "Downbeats",
            AccentOn::Beats => "Beats",
            AccentOn::Offbeats => "Offbeats",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Ornament {
    #[default]
    GraceBelow,
    GraceAbove,
    Flam,
    Mordent,
}

impl Ornament {
    pub const ALL: [Ornament; 4] = [
        Ornament::GraceBelow,
        Ornament::GraceAbove,
        Ornament::Flam,
        Ornament::Mordent,
    ];
    pub fn label(self) -> &'static str {
        match self {
            Ornament::GraceBelow => "Grace Note Below",
            Ornament::GraceAbove => "Grace Note Above",
            Ornament::Flam => "Flam",
            Ornament::Mordent => "Mordent",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Lengths {
    Short,
    #[default]
    Mixed,
    Long,
}

impl Lengths {
    pub const ALL: [Lengths; 3] = [Lengths::Short, Lengths::Mixed, Lengths::Long];
    pub fn label(self) -> &'static str {
        match self {
            Lengths::Short => "Short",
            Lengths::Mixed => "Mixed",
            Lengths::Long => "Long",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum BassPattern {
    #[default]
    Roots,
    RootFifth,
    Octaves,
    Walking,
    Pulse,
}

impl BassPattern {
    pub const ALL: [BassPattern; 5] = [
        BassPattern::Roots,
        BassPattern::RootFifth,
        BassPattern::Octaves,
        BassPattern::Walking,
        BassPattern::Pulse,
    ];
    pub fn label(self) -> &'static str {
        match self {
            BassPattern::Roots => "Roots",
            BassPattern::RootFifth => "Root and Fifth",
            BassPattern::Octaves => "Octaves",
            BassPattern::Walking => "Walking",
            BassPattern::Pulse => "Pulse",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum DrumStyle {
    #[default]
    FourOnTheFloor,
    Backbeat,
    Breakbeat,
    HalfTime,
    Trap,
}

impl DrumStyle {
    pub const ALL: [DrumStyle; 5] = [
        DrumStyle::FourOnTheFloor,
        DrumStyle::Backbeat,
        DrumStyle::Breakbeat,
        DrumStyle::HalfTime,
        DrumStyle::Trap,
    ];
    pub fn label(self) -> &'static str {
        match self {
            DrumStyle::FourOnTheFloor => "Four on the Floor",
            DrumStyle::Backbeat => "Backbeat",
            DrumStyle::Breakbeat => "Breakbeat",
            DrumStyle::HalfTime => "Half-Time",
            DrumStyle::Trap => "Trap",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Hats {
    Off,
    #[default]
    Eighths,
    Sixteenths,
}

impl Hats {
    pub const ALL: [Hats; 3] = [Hats::Off, Hats::Eighths, Hats::Sixteenths];
    pub fn label(self) -> &'static str {
        match self {
            Hats::Off => "No Hats",
            Hats::Eighths => "Eighth Hats",
            Hats::Sixteenths => "Sixteenth Hats",
        }
    }
}

/// Every tool's settings (each tool keeps its own while others are used).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ToolSettings {
    pub strum_step: u8,
    pub strum_direction: Direction,
    pub chop_parts: u8,
    /// Velocity added per part (negative: fading).
    pub chop_ramp: i8,
    /// Join notes at most this far apart (0: touching).
    pub join_gap: u8,
    pub connect_step: u8,
    /// The run's velocity, a share of the note before (%).
    pub connect_velocity: u8,
    pub arp_order: ArpOrder,
    pub arp_step: u8,
    /// Each note's length, a share of the step (%).
    pub arp_gate: u8,
    pub arp_octaves: u8,
    pub recombine: Recombine,
    /// Notes in the key stay; only the others move to chord tones.
    pub conform_keep_key: bool,
    pub accent_on: AccentOn,
    pub accent_amount: u8,
    pub time_factor: u8,
    pub ornament: Ornament,
    pub ornament_length: u8,
    /// The grace note's velocity, a share of the note's (%).
    pub ornament_velocity: u8,
    pub euclid_hits: u8,
    pub euclid_steps: u8,
    pub euclid_rotate: u8,
    pub euclid_step: u8,
    pub euclid_key: u8,
    pub euclid_accent: u8,
    pub seed_density: u8,
    pub seed_low: u8,
    pub seed_high: u8,
    pub seed_step: u8,
    pub seed_lengths: Lengths,
    pub chords_rhythm: u8,
    pub chords_register: u8,
    pub chords_lead: bool,
    pub bass_pattern: BassPattern,
    pub bass_step: u8,
    pub bass_octave: u8,
    pub drum_style: DrumStyle,
    pub drum_hats: Hats,
    /// Velocity of generated notes.
    pub velocity: u8,
    /// Random tools' seed.
    pub seed: u32,
    /// Generators replace the notes in their range (else add to them).
    pub replace: bool,
}

impl Default for ToolSettings {
    fn default() -> Self {
        Self {
            strum_step: S64,
            strum_direction: Direction::Up,
            chop_parts: 4,
            chop_ramp: -8,
            join_gap: 0,
            connect_step: S16,
            connect_velocity: 75,
            arp_order: ArpOrder::Up,
            arp_step: S16,
            arp_gate: 80,
            arp_octaves: 1,
            recombine: Recombine::ShufflePitches,
            conform_keep_key: false,
            accent_on: AccentOn::Beats,
            accent_amount: 20,
            time_factor: 0,
            ornament: Ornament::GraceBelow,
            ornament_length: S64 + 2,
            ornament_velocity: 70,
            euclid_hits: 5,
            euclid_steps: 8,
            euclid_rotate: 0,
            euclid_step: S16,
            euclid_key: 36,
            euclid_accent: 20,
            seed_density: 60,
            seed_low: 60,
            seed_high: 79,
            seed_step: S8,
            seed_lengths: Lengths::Mixed,
            chords_rhythm: S2,
            chords_register: 60,
            chords_lead: true,
            bass_pattern: BassPattern::RootFifth,
            bass_step: S4,
            bass_octave: 2,
            drum_style: DrumStyle::FourOnTheFloor,
            drum_hats: Hats::Eighths,
            velocity: 100,
            seed: 1,
            replace: false,
        }
    }
}

/// Where the tools work: the clip's start in the song, the meter, the
/// scale at a time and the chord track (all in project time).
pub struct ToolContext<'a> {
    pub clip_start: MusicalTime,
    pub meter: &'a TimeSignatureMap,
    pub scale_at: &'a dyn Fn(MusicalTime) -> Scale,
    pub key_at: &'a dyn Fn(MusicalTime) -> Option<Key>,
    pub chords: &'a [ChordEvent],
}

impl ToolContext<'_> {
    fn chord_at(&self, rel: MusicalTime) -> Option<Chord> {
        let t = self.clip_start + rel;
        self.chords
            .iter()
            .find(|c| c.start <= t && t < c.end)
            .map(|c| c.chord)
    }
}

/// Notes closer together than this start "together" (a chord).
const TOGETHER: MusicalTime = MusicalTime(faderframe_timeline::TICKS_PER_QUARTER / 32);
/// The shortest note a tool makes.
const SHORTEST: MusicalTime = MusicalTime(faderframe_timeline::TICKS_PER_QUARTER / 64);

fn new_note(
    start: MusicalTime,
    length: MusicalTime,
    key: i32,
    velocity: i32,
    like: &MidiNote,
) -> Option<MidiNote> {
    (0..=127).contains(&key).then(|| MidiNote {
        id: NoteId(0),
        start: start.max(MusicalTime::ZERO),
        length: length.max(SHORTEST),
        key: key as u8,
        velocity: velocity.clamp(1, 127) as u8,
        channel: like.channel,
        muted: like.muted,
        release: None,
    })
}

/// Notes in groups starting together, each group's notes low to high.
fn chords_of(notes: &[MidiNote]) -> Vec<Vec<MidiNote>> {
    let mut sorted = notes.to_vec();
    sorted.sort_by_key(|n| (n.start, n.key));
    let mut out: Vec<Vec<MidiNote>> = Vec::new();
    for n in sorted {
        match out.last_mut() {
            Some(g) if n.start - g[0].start < TOGETHER => g.push(n),
            _ => out.push(vec![n]),
        }
    }
    for g in &mut out {
        g.sort_by_key(|n| n.key);
    }
    out
}

fn scaled(t: MusicalTime, f: f64) -> MusicalTime {
    MusicalTime((t.ticks() as f64 * f).round() as i64)
}

/// The notes replacing `notes` (the selection) after transformation `tool`.
pub fn transform(
    tool: Tool,
    s: &ToolSettings,
    notes: &[MidiNote],
    ctx: &ToolContext<'_>,
) -> Vec<MidiNote> {
    let mut out: Vec<MidiNote> = Vec::with_capacity(notes.len());
    match tool {
        Tool::Strum => {
            let d = step(s.strum_step);
            for (gi, group) in chords_of(notes).into_iter().enumerate() {
                let down = match s.strum_direction {
                    Direction::Up => false,
                    Direction::Down => true,
                    Direction::Alternate => gi % 2 == 1,
                };
                let n = group.len();
                for (i, mut note) in group.into_iter().enumerate() {
                    let place = if down { n - 1 - i } else { i };
                    let off = MusicalTime(d.ticks() * place as i64);
                    let end = note.end();
                    note.start += off;
                    note.length = (end - note.start).max(SHORTEST);
                    out.push(note);
                }
            }
        }
        Tool::Chop => {
            let parts = i64::from(s.chop_parts.clamp(2, 16));
            for n in notes {
                let part = MusicalTime(n.length.ticks() / parts);
                if part < SHORTEST {
                    out.push(*n);
                    continue;
                }
                for k in 0..parts {
                    let v = i32::from(n.velocity) + k as i32 * i32::from(s.chop_ramp);
                    let mut piece = MidiNote {
                        start: n.start + MusicalTime(part.ticks() * k),
                        length: part,
                        velocity: v.clamp(1, 127) as u8,
                        ..*n
                    };
                    if k > 0 {
                        piece.id = NoteId(0);
                    }
                    out.push(piece);
                }
            }
        }
        Tool::Join => {
            let gap = if s.join_gap == 0 {
                MusicalTime::ZERO
            } else {
                step(s.join_gap)
            };
            let mut sorted = notes.to_vec();
            sorted.sort_by_key(|n| (n.channel, n.key, n.start));
            for n in sorted {
                match out.last_mut() {
                    Some(last)
                        if last.channel == n.channel
                            && last.key == n.key
                            && n.start <= last.end() + gap =>
                    {
                        let end = last.end().max(n.end());
                        last.length = end - last.start;
                    }
                    _ => out.push(n),
                }
            }
        }
        Tool::Connect => {
            out.extend_from_slice(notes);
            // The top line: each group's highest note.
            let line: Vec<MidiNote> = chords_of(notes)
                .into_iter()
                .filter_map(|g| g.last().copied())
                .collect();
            let d = step(s.connect_step);
            for w in line.windows(2) {
                let (a, b) = (w[0], w[1]);
                let gap_start = a.end();
                if b.start - gap_start < d || a.key == b.key {
                    continue;
                }
                let scale = (ctx.scale_at)(ctx.clip_start + gap_start);
                let up = b.key > a.key;
                let mut key = a.key;
                let mut t = gap_start;
                while t + d <= b.start {
                    let next = if scale.is_chromatic() {
                        if up {
                            key.saturating_add(1)
                        } else {
                            key.saturating_sub(1)
                        }
                    } else {
                        scale.step(key, if up { 1 } else { -1 })
                    };
                    if next == b.key || (up && next > b.key) || (!up && next < b.key) {
                        break;
                    }
                    key = next;
                    let v = i32::from(a.velocity) * i32::from(s.connect_velocity) / 100;
                    out.extend(new_note(t, d, i32::from(key), v, &a));
                    t += d;
                }
            }
        }
        Tool::Arpeggiate => {
            let d = step(s.arp_step);
            let mut rng = Rng::new(u64::from(s.seed) * 0x9e37_79b9);
            for group in chords_of(notes) {
                if group.len() < 2 {
                    out.extend(group);
                    continue;
                }
                let start = group[0].start;
                let end = group.iter().map(MidiNote::end).max().unwrap_or(start);
                let octaves = i32::from(s.arp_octaves.clamp(1, 4));
                let mut seq: Vec<(i32, u8)> = Vec::new();
                for o in 0..octaves {
                    for n in &group {
                        seq.push((i32::from(n.key) + 12 * o, n.velocity));
                    }
                }
                let n = seq.len();
                let mut t = start;
                let mut k = 0usize;
                while t < end {
                    let i = match s.arp_order {
                        ArpOrder::Up => k % n,
                        ArpOrder::Down => n - 1 - k % n,
                        ArpOrder::UpDown => {
                            let p = k % (2 * n - 2).max(1);
                            if p < n { p } else { 2 * n - 2 - p }
                        }
                        ArpOrder::Random => ((rng.signed() + 1.0) * 0.5 * n as f64) as usize % n,
                    };
                    let len = scaled(d, f64::from(s.arp_gate.clamp(10, 150)) / 100.0).min(end - t);
                    out.extend(new_note(t, len, seq[i].0, i32::from(seq[i].1), &group[0]));
                    t += d;
                    k += 1;
                }
            }
        }
        Tool::Recombine => {
            let mut sorted = notes.to_vec();
            sorted.sort_by_key(|n| (n.start, n.key));
            let mut rng = Rng::new(u64::from(s.seed) * 0x2545_f491);
            let mut shuffle = |v: &mut Vec<u8>| {
                for i in (1..v.len()).rev() {
                    let j = ((rng.signed() + 1.0) * 0.5 * (i + 1) as f64) as usize % (i + 1);
                    v.swap(i, j);
                }
            };
            match s.recombine {
                Recombine::ShufflePitches
                | Recombine::RotatePitches
                | Recombine::ReversePitches => {
                    let mut keys: Vec<u8> = sorted.iter().map(|n| n.key).collect();
                    match s.recombine {
                        Recombine::ShufflePitches => shuffle(&mut keys),
                        Recombine::RotatePitches if !keys.is_empty() => keys.rotate_left(1),
                        Recombine::ReversePitches => keys.reverse(),
                        _ => {}
                    }
                    for (n, k) in sorted.iter_mut().zip(keys) {
                        n.key = k;
                    }
                }
                Recombine::ShuffleVelocities => {
                    let mut v: Vec<u8> = sorted.iter().map(|n| n.velocity).collect();
                    shuffle(&mut v);
                    for (n, x) in sorted.iter_mut().zip(v) {
                        n.velocity = x;
                    }
                }
                Recombine::ShuffleLengths => {
                    let mut order: Vec<u8> = (0..sorted.len().min(255) as u8).collect();
                    shuffle(&mut order);
                    let lengths: Vec<MusicalTime> = sorted.iter().map(|n| n.length).collect();
                    for (n, i) in sorted.iter_mut().zip(order) {
                        n.length = lengths[usize::from(i)];
                    }
                }
            }
            out = sorted;
        }
        Tool::Conform => {
            for n in notes {
                let at = ctx.clip_start + n.start;
                let scale = (ctx.scale_at)(at);
                let k = i32::from(n.key);
                let key = match ctx.chord_at(n.start) {
                    Some(_)
                        if s.conform_keep_key && !scale.is_chromatic() && scale.contains(n.key) =>
                    {
                        k
                    }
                    Some(c) => (0..12)
                        .flat_map(|d| [k - d, k + d])
                        .find(|x| c.contains(*x))
                        .unwrap_or(k),
                    None if !scale.is_chromatic() => i32::from(scale.nearest(n.key)),
                    None => k,
                };
                out.push(MidiNote {
                    key: key.clamp(0, 127) as u8,
                    ..*n
                });
            }
        }
        Tool::Accent => {
            let amount = i32::from(s.accent_amount);
            for n in notes {
                let at = ctx.clip_start + n.start;
                let bar = ctx.meter.bar_at(at);
                let from_bar = at - ctx.meter.bar_start(bar);
                let beat = ctx.meter.signature_of_bar(bar).beat_length();
                let on = |unit: MusicalTime, offset: MusicalTime| {
                    let r = (from_bar - offset).ticks().rem_euclid(unit.ticks().max(1));
                    r < TOGETHER.ticks() || unit.ticks() - r < TOGETHER.ticks()
                };
                let hit = match s.accent_on {
                    AccentOn::Downbeats => from_bar < TOGETHER,
                    AccentOn::Beats => on(beat, MusicalTime::ZERO),
                    AccentOn::Offbeats => {
                        on(beat, MusicalTime(beat.ticks() / 2)) && !on(beat, MusicalTime::ZERO)
                    }
                };
                let v = i32::from(n.velocity) + if hit { amount } else { -amount / 2 };
                out.push(MidiNote {
                    velocity: v.clamp(1, 127) as u8,
                    ..*n
                });
            }
        }
        Tool::TimeScale => {
            let f = FACTORS[usize::from(s.time_factor).min(FACTORS.len() - 1)].1;
            let anchor = notes
                .iter()
                .map(|n| n.start)
                .min()
                .unwrap_or(MusicalTime::ZERO);
            for n in notes {
                out.push(MidiNote {
                    start: anchor + scaled(n.start - anchor, f),
                    length: scaled(n.length, f).max(SHORTEST),
                    ..*n
                });
            }
        }
        Tool::Ornament => {
            let d = step(s.ornament_length);
            for n in notes {
                let scale = (ctx.scale_at)(ctx.clip_start + n.start);
                let neighbour = |up: bool| -> i32 {
                    if scale.is_chromatic() {
                        i32::from(n.key) + if up { 2 } else { -1 }
                    } else {
                        i32::from(scale.step(n.key, if up { 1 } else { -1 }))
                    }
                };
                let v = i32::from(n.velocity) * i32::from(s.ornament_velocity) / 100;
                match s.ornament {
                    Ornament::Mordent if n.length > MusicalTime(d.ticks() * 3) => {
                        out.extend(new_note(
                            n.start,
                            d,
                            i32::from(n.key),
                            i32::from(n.velocity),
                            n,
                        ));
                        out.extend(new_note(n.start + d, d, neighbour(true), v, n));
                        out.push(MidiNote {
                            start: n.start + MusicalTime(d.ticks() * 2),
                            length: n.length - MusicalTime(d.ticks() * 2),
                            ..*n
                        });
                    }
                    Ornament::Mordent => out.push(*n),
                    _ => {
                        // Before the beat (as far as the clip allows).
                        let start = if n.start >= d {
                            n.start - d
                        } else {
                            MusicalTime::ZERO
                        };
                        let len = (n.start - start).max(SHORTEST);
                        let key = match s.ornament {
                            Ornament::GraceAbove => neighbour(true),
                            Ornament::GraceBelow => neighbour(false),
                            _ => i32::from(n.key),
                        };
                        if n.start > start {
                            out.extend(new_note(start, len, key, v, n));
                        }
                        out.push(*n);
                    }
                }
            }
        }
        _ => out.extend_from_slice(notes),
    }
    out.retain(|n| n.length > MusicalTime::ZERO);
    out
}

/// Whether step `i` of a Euclidean rhythm (hits over steps) is a hit.
pub fn euclid_hit(hits: u32, steps: u32, i: u32) -> bool {
    let (k, n) = (hits.min(steps), steps.max(1));
    (i % n) * k % n < k
}

/// The notes generator `tool` makes in `from..to` (clip time).
pub fn generate(
    tool: Tool,
    s: &ToolSettings,
    from: MusicalTime,
    to: MusicalTime,
    ctx: &ToolContext<'_>,
) -> Vec<MidiNote> {
    let like = MidiNote {
        id: NoteId(0),
        start: MusicalTime::ZERO,
        length: MusicalTime::QUARTER,
        key: 60,
        velocity: s.velocity,
        channel: 0,
        muted: false,
        release: None,
    };
    let velocity = i32::from(s.velocity);
    let mut out = Vec::new();
    if to <= from {
        return out;
    }
    match tool {
        Tool::Euclid => {
            let d = step(s.euclid_step);
            let steps = u32::from(s.euclid_steps.clamp(2, 32));
            let hits = u32::from(s.euclid_hits).min(steps);
            let mut k = 0u32;
            let mut t = from;
            while t < to {
                let i = (k + u32::from(s.euclid_rotate)) % steps;
                if euclid_hit(hits, steps, i) {
                    let accent = if k.is_multiple_of(steps) {
                        i32::from(s.euclid_accent)
                    } else {
                        0
                    };
                    let len = scaled(d, 0.8).min(to - t);
                    out.extend(new_note(
                        t,
                        len,
                        i32::from(s.euclid_key),
                        velocity + accent,
                        &like,
                    ));
                }
                t += d;
                k += 1;
            }
        }
        Tool::Seed => {
            let d = step(s.seed_step);
            let mut rng = Rng::new(u64::from(s.seed).wrapping_mul(0x5851_f42d_4c95_7f2d) ^ 0xa5a5);
            let unit = |r: &mut Rng| (r.signed() + 1.0) * 0.5;
            let (low, high) = (s.seed_low.min(s.seed_high), s.seed_low.max(s.seed_high));
            let mut key = (u32::from(low) + u32::from(high)) / 2;
            let mut t = from;
            while t < to {
                let scale = (ctx.scale_at)(ctx.clip_start + t);
                let span = match s.seed_lengths {
                    Lengths::Short => 1,
                    Lengths::Mixed => 1 + (unit(&mut rng) * 3.0) as i64,
                    Lengths::Long => 2 + (unit(&mut rng) * 3.0) as i64,
                };
                if unit(&mut rng) * 100.0 < f64::from(s.seed_density) {
                    // A walk: mostly steps, sometimes leaps, kept in range.
                    let r = unit(&mut rng);
                    let steps: i32 = if r < 0.35 {
                        1
                    } else if r < 0.7 {
                        -1
                    } else if r < 0.82 {
                        2
                    } else if r < 0.94 {
                        -2
                    } else if r < 0.97 {
                        4
                    } else {
                        -4
                    };
                    let k = if scale.is_chromatic() {
                        (key as i32 + steps).clamp(0, 127) as u8
                    } else {
                        scale.step(scale.nearest(key as u8), steps)
                    };
                    let k = if k < low || k > high {
                        // Turn back into the range.
                        if scale.is_chromatic() {
                            (key as i32 - steps).clamp(0, 127) as u8
                        } else {
                            scale.step(scale.nearest(key as u8), -steps)
                        }
                    } else {
                        k
                    };
                    let k = k.clamp(low, high);
                    let k = if scale.is_chromatic() || scale.contains(k) {
                        k
                    } else {
                        scale.nearest(k)
                    };
                    key = u32::from(k);
                    let len = MusicalTime(d.ticks() * span).min(to - t);
                    let v = velocity - 20 + (unit(&mut rng) * 30.0) as i32;
                    out.extend(new_note(t, len, key as i32, v, &like));
                }
                t += MusicalTime(d.ticks() * span);
            }
        }
        Tool::Chords => {
            let pulse = step(s.chords_rhythm);
            let mut previous: Option<Vec<i32>> = None;
            for c in ctx.chords {
                let (start, end) = (c.start - ctx.clip_start, c.end - ctx.clip_start);
                let (a, b) = (start.max(from), end.min(to));
                if b <= a {
                    continue;
                }
                let voicing = voice(
                    c.chord,
                    i32::from(s.chords_register),
                    previous.as_deref(),
                    s.chords_lead,
                );
                let mut t = a;
                while t < b {
                    let len = pulse.min(b - t);
                    for k in &voicing {
                        out.extend(new_note(t, scaled(len, 0.95), *k, velocity, &like));
                    }
                    t += pulse;
                }
                previous = Some(voicing);
            }
        }
        Tool::Bassline => {
            let d = step(s.bass_step);
            let base = 12 * (i32::from(s.bass_octave.clamp(0, 5)) + 1);
            // The roots: the chord track's, or the key's tonic each bar.
            let mut spans: Vec<(MusicalTime, MusicalTime, i32, bool)> = Vec::new();
            for c in ctx.chords {
                let (a, b) = (
                    (c.start - ctx.clip_start).max(from),
                    (c.end - ctx.clip_start).min(to),
                );
                if b > a {
                    let minor = c.chord.quality.intervals().contains(&3)
                        && !c.chord.quality.intervals().contains(&4);
                    spans.push((a, b, i32::from(c.chord.root), minor));
                }
            }
            if spans.is_empty() {
                let key = (ctx.key_at)(ctx.clip_start + from);
                let root = key.map_or(0, |k| i32::from(k.root));
                spans.push((from, to, root, key.is_some_and(|k| k.scale.is_minor())));
            }
            for (i, &(a, b, root, minor)) in spans.iter().enumerate() {
                let r = base + root;
                let next_root = spans.get(i + 1).map(|s| base + s.2);
                let mut t = a;
                let mut k = 0;
                while t < b {
                    let len = d.min(b - t);
                    let last = t + d >= b;
                    let key = match s.bass_pattern {
                        BassPattern::Roots | BassPattern::Pulse => r,
                        BassPattern::RootFifth => {
                            if k % 2 == 0 {
                                r
                            } else {
                                r + 7
                            }
                        }
                        BassPattern::Octaves => {
                            if k % 2 == 0 {
                                r
                            } else {
                                r + 12
                            }
                        }
                        BassPattern::Walking => match (last, next_root) {
                            // A chromatic approach into the next root.
                            (true, Some(n)) if k > 0 => n + if n > r { -1 } else { 1 },
                            _ => r + [0, if minor { 3 } else { 4 }, 7, 9][k % 4],
                        },
                    };
                    let len = if s.bass_pattern == BassPattern::Pulse {
                        scaled(len, 0.5)
                    } else {
                        scaled(len, 0.92)
                    };
                    out.extend(new_note(
                        t,
                        len,
                        key,
                        velocity - if k % 2 == 1 { 12 } else { 0 },
                        &like,
                    ));
                    t += d;
                    k += 1;
                }
            }
        }
        Tool::Drums => {
            const KICK: i32 = 36;
            const SNARE: i32 = 38;
            const CLAP: i32 = 39;
            const HAT: i32 = 42;
            const OPEN: i32 = 46;
            let mut bar = ctx.meter.bar_at(ctx.clip_start + from);
            loop {
                let bar_start = ctx.meter.bar_start(bar) - ctx.clip_start;
                if bar_start >= to {
                    break;
                }
                let sig = ctx.meter.signature_of_bar(bar);
                let sixteenth = MusicalTime(sig.beat_length().ticks() / 4);
                let steps = (sig.bar_length().ticks() / sixteenth.ticks().max(1)) as usize;
                // Hits as (step, key, velocity share).
                let mut hits: Vec<(usize, i32, f64)> = Vec::new();
                let beats: Vec<usize> = (0..steps).step_by(4).collect();
                let last_beat = beats.len().saturating_sub(1);
                match s.drum_style {
                    DrumStyle::FourOnTheFloor => {
                        hits.extend(beats.iter().map(|b| (*b, KICK, 1.0)));
                        hits.extend(beats.iter().skip(1).step_by(2).map(|b| (*b, CLAP, 0.95)));
                        hits.extend(beats.iter().map(|b| (b + 2, OPEN, 0.6)));
                    }
                    DrumStyle::Backbeat => {
                        hits.extend(beats.iter().step_by(2).map(|b| (*b, KICK, 1.0)));
                        if beats.len() > 2 {
                            hits.push((beats[2] + 2, KICK, 0.8));
                        }
                        hits.extend(beats.iter().skip(1).step_by(2).map(|b| (*b, SNARE, 1.0)));
                    }
                    DrumStyle::Breakbeat => {
                        hits.extend([
                            (0, KICK, 1.0),
                            (10, KICK, 0.9),
                            (4, SNARE, 1.0),
                            (12, SNARE, 1.0),
                        ]);
                        hits.extend([(7, SNARE, 0.45), (9, SNARE, 0.4), (15, SNARE, 0.5)]);
                    }
                    DrumStyle::HalfTime => {
                        hits.extend([(0, KICK, 1.0), (14, KICK, 0.7)]);
                        if beats.len() > 2 {
                            hits.push((beats[2], SNARE, 1.0));
                        }
                    }
                    DrumStyle::Trap => {
                        hits.extend([(0, KICK, 1.0), (7, KICK, 0.85), (10, KICK, 0.9)]);
                        if beats.len() > 2 {
                            hits.push((beats[2], CLAP, 1.0));
                        }
                    }
                }
                let hat_step = match (s.drum_hats, s.drum_style) {
                    (Hats::Off, _) => 0,
                    (_, DrumStyle::Trap) | (Hats::Sixteenths, _) => 1,
                    (Hats::Eighths, _) => 2,
                };
                if hat_step > 0 {
                    for i in (0..steps).step_by(hat_step) {
                        if s.drum_style == DrumStyle::FourOnTheFloor && i % 4 == 2 {
                            continue; // the open hat is there
                        }
                        let accent = if i % 4 == 0 {
                            0.8
                        } else if i % 2 == 0 {
                            0.62
                        } else {
                            0.48
                        };
                        hits.push((i, HAT, accent));
                    }
                }
                let _ = last_beat;
                for (i, key, share) in hits {
                    if i >= steps {
                        continue;
                    }
                    let t = bar_start + MusicalTime(sixteenth.ticks() * i as i64);
                    if t < from || t >= to {
                        continue;
                    }
                    let v = (f64::from(s.velocity) * share).round() as i32;
                    out.extend(new_note(t, scaled(sixteenth, 0.5), key, v, &like));
                }
                bar += 1;
            }
        }
        _ => {}
    }
    out
}

/// A chord's notes round `register`, led from `previous` (the voicing
/// that moves least) when `lead`.
pub fn voice(chord: Chord, register: i32, previous: Option<&[i32]>, lead: bool) -> Vec<i32> {
    let base = chord.voicing(register);
    let Some(prev) = previous.filter(|_| lead) else {
        return base;
    };
    // Every inversion within an octave either way: the least movement
    // from the last voicing (by sorted note), then the nearest centre.
    let n = base.len();
    let mut best = base.clone();
    let mut best_cost = i32::MAX;
    for inv in 0..n {
        for shift in [-12, 0, 12] {
            let mut v: Vec<i32> = base
                .iter()
                .enumerate()
                .map(|(i, k)| k + shift + if i < inv { 12 } else { 0 })
                .collect();
            v.sort_unstable();
            let mut p = prev.to_vec();
            p.sort_unstable();
            let cost: i32 = v
                .iter()
                .zip(
                    p.iter()
                        .chain(std::iter::repeat(p.last().unwrap_or(&register))),
                )
                .map(|(a, b)| (a - b).abs())
                .sum::<i32>()
                + (v.iter().sum::<i32>() / n as i32 - register).abs() / 4;
            if cost < best_cost && v.iter().all(|k| (0..=127).contains(k)) {
                best_cost = cost;
                best = v;
            }
        }
    }
    best
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::midi_ops::ScaleKind;
    use faderframe_midi::theory::{Quality, Scale as KeyScale};
    use faderframe_timeline::{TimeSignature, TimeSignatureMap};

    fn q(x: f64) -> MusicalTime {
        MusicalTime::from_quarters(x)
    }

    fn note(id: u64, start: f64, len: f64, key: u8) -> MidiNote {
        MidiNote {
            id: NoteId(id),
            start: q(start),
            length: q(len),
            key,
            velocity: 100,
            channel: 0,
            muted: false,
            release: None,
        }
    }

    fn with<R>(chords: &[ChordEvent], f: impl FnOnce(&ToolContext<'_>) -> R) -> R {
        let meter = TimeSignatureMap::new(TimeSignature::FOUR_FOUR);
        let c_major = |_| Scale::new(0, ScaleKind::Major);
        let key = |_| Some(Key::new(0, KeyScale::Major));
        let ctx = ToolContext {
            clip_start: MusicalTime::ZERO,
            meter: &meter,
            scale_at: &c_major,
            key_at: &key,
            chords,
        };
        f(&ctx)
    }

    fn keys(n: &[MidiNote]) -> Vec<u8> {
        let mut v = n.to_vec();
        v.sort_by_key(|n| (n.start, n.key));
        v.iter().map(|n| n.key).collect()
    }

    #[test]
    fn strum_chop_join_and_time_scale() {
        let s = ToolSettings::default();
        let chord = [
            note(1, 0.0, 1.0, 60),
            note(2, 0.0, 1.0, 64),
            note(3, 0.0, 1.0, 67),
        ];
        let out = with(&[], |c| transform(Tool::Strum, &s, &chord, c));
        let starts: Vec<f64> = keys(&out)
            .iter()
            .map(|k| out.iter().find(|n| n.key == *k).unwrap().start.quarters())
            .collect();
        assert_eq!(starts, [0.0, 0.0625, 0.125]);
        assert!(
            out.iter().all(|n| (n.end().quarters() - 1.0).abs() < 1e-9),
            "they end together"
        );
        assert!(out.iter().all(|n| n.id.0 != 0), "the notes keep their ids");
        // Chop into four, each softer.
        let out = with(&[], |c| {
            transform(Tool::Chop, &s, &[note(1, 0.0, 1.0, 60)], c)
        });
        assert_eq!(out.len(), 4);
        assert_eq!(
            out.iter().map(|n| n.velocity).collect::<Vec<_>>(),
            [100, 92, 84, 76]
        );
        assert_eq!(out.iter().filter(|n| n.id.0 != 0).count(), 1);
        // Join a repeated key; another key stays apart.
        let rep = [
            note(1, 0.0, 0.5, 60),
            note(2, 0.5, 0.5, 60),
            note(3, 1.0, 0.5, 62),
        ];
        let out = with(&[], |c| transform(Tool::Join, &s, &rep, c));
        assert_eq!(out.len(), 2);
        assert_eq!(out.iter().find(|n| n.key == 60).unwrap().length, q(1.0));
        // Double time from the first note.
        let line = [note(1, 2.0, 1.0, 60), note(2, 4.0, 1.0, 62)];
        let out = with(&[], |c| transform(Tool::TimeScale, &s, &line, c));
        assert_eq!((out[1].start, out[1].length), (q(3.0), q(0.5)));
    }

    #[test]
    fn connect_arpeggiate_conform_accent_and_ornaments() {
        let s = ToolSettings::default();
        // C … G a bar apart: D E F fill the gap in sixteenths.
        let line = [note(1, 0.0, 0.25, 60), note(2, 1.0, 0.25, 67)];
        let out = with(&[], |c| transform(Tool::Connect, &s, &line, c));
        assert_eq!(keys(&out), [60, 62, 64, 65, 67]);
        // A C major chord over a beat: arpeggiated in sixteenths, up.
        let chord = [
            note(1, 0.0, 1.0, 60),
            note(2, 0.0, 1.0, 64),
            note(3, 0.0, 1.0, 67),
        ];
        let out = with(&[], |c| transform(Tool::Arpeggiate, &s, &chord, c));
        assert_eq!(keys(&out), [60, 64, 67, 60]);
        assert!(out.iter().all(|n| n.length == q(0.2)));
        // Conform to an A minor chord: B moves to C (a semitone), D to C
        // (two down, the tie with E going down).
        let am = [ChordEvent {
            start: MusicalTime::ZERO,
            end: q(4.0),
            chord: Chord::new(9, Quality::Minor),
        }];
        let out = with(&am, |c| {
            transform(
                Tool::Conform,
                &s,
                &[note(1, 0.0, 1.0, 71), note(2, 1.0, 1.0, 62)],
                c,
            )
        });
        assert_eq!(keys(&out), [72, 60]);
        // Accent the beats: on the beat louder, off it softer.
        let hits = [note(1, 0.0, 0.25, 60), note(2, 0.25, 0.25, 60)];
        let out = with(&[], |c| transform(Tool::Accent, &s, &hits, c));
        assert_eq!(
            out.iter().map(|n| n.velocity).collect::<Vec<_>>(),
            [120, 90]
        );
        // A grace note below in the scale, before the note.
        let out = with(&[], |c| {
            transform(Tool::Ornament, &s, &[note(1, 1.0, 1.0, 64)], c)
        });
        assert_eq!(keys(&out), [62, 64]);
        assert!(out.iter().any(|n| n.key == 62 && n.end() == q(1.0)));
    }

    #[test]
    fn recombine_keeps_the_rhythm_and_the_seed_decides() {
        let mut s = ToolSettings::default();
        let line: Vec<MidiNote> = (0..8)
            .map(|i| note(i + 1, i as f64 * 0.5, 0.5, 60 + i as u8))
            .collect();
        let a = with(&[], |c| transform(Tool::Recombine, &s, &line, c));
        let b = with(&[], |c| transform(Tool::Recombine, &s, &line, c));
        assert_eq!(a, b, "the same seed, the same result");
        let mut k = keys(&a);
        k.sort_unstable();
        assert_eq!(k, (60..68).collect::<Vec<u8>>(), "the same pitches");
        let starts: Vec<_> = a.iter().map(|n| n.start).collect();
        assert_eq!(starts, line.iter().map(|n| n.start).collect::<Vec<_>>());
        s.seed = 2;
        assert_ne!(with(&[], |c| transform(Tool::Recombine, &s, &line, c)), a);
        s.recombine = Recombine::ReversePitches;
        assert_eq!(
            keys(&with(&[], |c| transform(Tool::Recombine, &s, &line, c))),
            (60..68).rev().collect::<Vec<u8>>()
        );
    }

    #[test]
    fn euclid_seed_chords_bass_and_drums() {
        let mut s = ToolSettings::default();
        // Tresillo: 3 over 8.
        let pattern: Vec<bool> = (0..8).map(|i| euclid_hit(3, 8, i)).collect();
        assert_eq!(pattern.iter().filter(|h| **h).count(), 3);
        s.euclid_hits = 3;
        let out = with(&[], |c| {
            generate(Tool::Euclid, &s, MusicalTime::ZERO, q(4.0), c)
        });
        assert_eq!(out.len(), 6, "two cycles of eight sixteenths");
        assert!(out.iter().all(|n| n.key == 36));
        // A seeded melody: in C major, in range, the same each time.
        let a = with(&[], |c| {
            generate(Tool::Seed, &s, MusicalTime::ZERO, q(16.0), c)
        });
        let b = with(&[], |c| {
            generate(Tool::Seed, &s, MusicalTime::ZERO, q(16.0), c)
        });
        assert_eq!(a, b);
        assert!(!a.is_empty());
        let c_major = Scale::new(0, ScaleKind::Major);
        assert!(
            a.iter()
                .all(|n| c_major.contains(n.key) && (60..=79).contains(&n.key))
        );
        // Chords from the chord track, a half note each, led smoothly.
        let track = [
            ChordEvent {
                start: MusicalTime::ZERO,
                end: q(4.0),
                chord: Chord::new(0, Quality::Major),
            },
            ChordEvent {
                start: q(4.0),
                end: q(8.0),
                chord: Chord::new(5, Quality::Major),
            },
        ];
        let out = with(&track, |c| {
            generate(Tool::Chords, &s, MusicalTime::ZERO, q(8.0), c)
        });
        assert_eq!(out.len(), 4 * 3);
        let f: Vec<u8> = out
            .iter()
            .filter(|n| n.start == q(4.0))
            .map(|n| n.key)
            .collect();
        let c: Vec<u8> = out
            .iter()
            .filter(|n| n.start == q(0.0))
            .map(|n| n.key)
            .collect();
        let moved: i32 = c
            .iter()
            .zip(&f)
            .map(|(a, b)| (i32::from(*a) - i32::from(*b)).abs())
            .sum();
        assert!(moved <= 4, "C to F moves {moved} semitones: {c:?} {f:?}");
        // Root and fifth on the roots.
        let out = with(&track, |c| {
            generate(Tool::Bassline, &s, MusicalTime::ZERO, q(8.0), c)
        });
        assert_eq!(keys(&out), [36, 43, 36, 43, 41, 48, 41, 48]);
        // Four on the floor: four kicks, two claps, a bar.
        let out = with(&[], |c| {
            generate(Tool::Drums, &s, MusicalTime::ZERO, q(4.0), c)
        });
        assert_eq!(out.iter().filter(|n| n.key == 36).count(), 4);
        assert_eq!(out.iter().filter(|n| n.key == 39).count(), 2);
        assert_eq!(out.iter().filter(|n| n.key == 46).count(), 4);
    }
}
