//! From the notes heard in a recording to the lines a sheet writes: what
//! a polyphonic transcription found (every note, chords too, each note's
//! pitch followed frame by frame) cleaned up, and the melody picked out of
//! it.
//!
//! 1. [`clean`]: notes hardly followed (a phantom of the transcription) go;
//!    each is written at its followed pitch (a sung note a little off its
//!    key, or named a semitone off, lands on the note sung); a note struck
//!    again while it still sounded (its level dropping and jumping back
//!    up) is split there; a louder note's overtones and subharmonics heard
//!    as notes go; fragments of one held note (the same key, a small gap,
//!    or overlapping, and no new attack where the second starts) are
//!    joined; ghost notes — much quieter than the notes sounding round
//!    them — go.
//! 2. [`melody`]: a dynamic-programming skyline. Time is cut where any
//!    note starts or ends; in each piece the melody is one of the notes
//!    sounding or a rest. A note's salience there is its level against the
//!    loudest note sounding, its height among them, a voice's marks
//!    (vibrato — not one borrowed from a louder note's shared partials —
//!    and a glide into the note) and its length; the line enters a note
//!    only at its onset (never in the middle of a held one), pays for
//!    leaps (past a whole tone, more again past an octave) and for leaving
//!    a note that still sounds into silence, and — in a second pass — for
//!    straying from the register the first pass sang in. A rest beats a
//!    weak note where notes sound together, never a note sounding alone: a
//!    solo line (a note at a time) is just its notes. The melody's notes
//!    end where the line moves on (legato).
//! 3. [`fix_octaves`]: a melody note an octave off both its neighbours
//!    (which are close to each other) moves by the octave its audio says it
//!    sounds in ([`Octave`]) — a transcription now and then names a note
//!    by its second harmonic or a strong subharmonic.
//!
//! Times are in any unit: [`Scale::per_second`] says how many make a
//! second (1 for seconds, the tempo's quarters a second for notes read
//! from MIDI), and the thresholds below are in seconds.

/// How many of the notes' time units make a second.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Scale {
    pub per_second: f64,
}

impl Scale {
    pub const SECONDS: Scale = Scale { per_second: 1.0 };

    fn of(self, seconds: f64) -> f64 {
        seconds * self.per_second
    }
}

/// A note heard: when it sounds, the key the transcription named, how it
/// was followed.
#[derive(Clone, Debug, PartialEq)]
pub struct Heard {
    pub start: f64,
    pub end: f64,
    /// The key the transcription named (MIDI).
    pub key: u8,
    /// Its pitch as followed (MIDI, fractional: the median of `track`);
    /// NaN where it was not followed (read from MIDI).
    pub pitch: f32,
    /// The transcription's activation (0…1), or a MIDI note's velocity.
    pub velocity: f32,
    /// How loud its harmonics were where followed (dB, any reference the
    /// same for every note of a recording); NaN: unknown (the velocity
    /// stands in).
    pub loudness: f32,
    /// Its pitch every `hop` from `start` (MIDI; NaN: not heard there).
    pub track: Vec<f32>,
    /// How strongly its harmonics are heard at each of `track`'s frames
    /// (linear, the reference of `loudness`; 0: not heard; empty: unknown).
    pub levels: Vec<f32>,
    pub hop: f64,
}

/// Where a note's audio says its fundamental is: how strongly the octave
/// below, the note itself and the octave above are heard (the weighted
/// harmonic sums a pitch follower compares; any common reference).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Octave {
    pub below: f32,
    pub here: f32,
    pub above: f32,
}

/// The vibrato's rates (Hz) a voice sings.
const VIBRATO_HZ: (f64, f64) = (3.5, 8.5);
/// Shortest note whose vibrato is measured (seconds).
const VIBRATO_NOTE: f64 = 0.35;
/// A vibrato's least autocorrelation at its period.
const PERIODIC: f64 = 0.3;
/// Frames under this share of a note's median level are not trusted for
/// its pitch's shape (an attack's noise, a tail).
const GATE: f32 = 0.4;
/// Least share of a note's frames that must be followed to keep it.
const LEAST_VOICED: f32 = 0.25;
/// Fragments of one note: joined across a gap this short (seconds) …
const FRAGMENT_GAP: f64 = 0.035;
/// … or across this one when either piece is shorter than
/// [`FRAGMENT_PIECE`].
const FRAGMENT_GAP_SHORT: f64 = 0.1;
const FRAGMENT_PIECE: f64 = 0.15;
/// A ghost: this many dB under the loudest note sounding with it or
/// within [`GHOST_NEAR`] of it …
const GHOST_DB: f32 = 20.0;
/// … or this many when it is also short ([`GHOST_SHORT`]) and weakly
/// activated.
const GHOST_SHORT_DB: f32 = 3.0;
const GHOST_SHORT: f64 = 0.25;
const GHOST_NEAR: f64 = 0.12;
/// The level range salience spreads over (dB).
const LEVEL_RANGE: f32 = 24.0;
/// Where a louder note's partials fall (semitones from it): its overtones
/// above, the subharmonics a transcription now and then hears below.
const PARTIALS: [i32; 9] = [12, 19, 24, 28, 31, 34, 36, -12, -24];
/// A partial heard as a note: this much quieter (activation) and sounding
/// mostly with the note.
const PARTIAL_SHARE: f32 = 0.65;
const PARTIAL_OVERLAP: f64 = 0.7;
/// A second piece of a note starting this much louder than the first
/// ended is a new attack, not a fragment.
const ATTACK: f32 = 1.6;
/// A note heard as one is split where its level jumps this much from a
/// trough under [`SPLIT_TROUGH`] of its usual level (a key struck again;
/// another note's attack on a shared partial lifts it less).
const SPLIT_RISE: f32 = 2.2;
const SPLIT_TROUGH: f32 = 0.45;

impl Heard {
    /// A note read from MIDI (no pitch followed): velocity 0…1.
    pub fn midi(start: f64, end: f64, key: u8, velocity: f32) -> Self {
        Self {
            start,
            end,
            key,
            pitch: f32::NAN,
            velocity,
            loudness: f32::NAN,
            track: Vec::new(),
            levels: Vec::new(),
            hop: 0.0,
        }
    }

    /// The key it is written as: its followed pitch rounded (the key named
    /// where it was not followed).
    pub fn written(&self) -> u8 {
        if self.pitch.is_finite() {
            self.pitch.round().clamp(0.0, 127.0) as u8
        } else {
            self.key
        }
    }

    /// The pitch it sounds at (fractional MIDI).
    pub fn sounding(&self) -> f32 {
        if self.pitch.is_finite() {
            self.pitch
        } else {
            f32::from(self.key)
        }
    }

    /// The share of its frames that were followed (1 for MIDI).
    pub fn voiced(&self) -> f32 {
        if self.track.is_empty() {
            return if self.pitch.is_finite() { 0.0 } else { 1.0 };
        }
        self.track.iter().filter(|p| p.is_finite()).count() as f32 / self.track.len() as f32
    }

    /// Its level in dB: as followed, else from the velocity (a MIDI
    /// velocity's usual curve, 40·log10).
    pub fn level_db(&self) -> f32 {
        if self.loudness.is_finite() {
            self.loudness
        } else {
            40.0 * self.velocity.max(1e-3).log10()
        }
    }

    /// Its pitch frame by frame where it is clearly heard: frames whose
    /// level is under [`GATE`] of its median level (an attack's noise, a
    /// tail) are NaN.
    fn gated(&self) -> Vec<f32> {
        if self.levels.len() != self.track.len() {
            return self.track.clone();
        }
        let floor = median(
            &self
                .levels
                .iter()
                .copied()
                .filter(|l| *l > 0.0)
                .collect::<Vec<_>>(),
        )
        .unwrap_or(0.0)
            * GATE;
        self.track
            .iter()
            .zip(&self.levels)
            .map(|(p, l)| if *l >= floor { *p } else { f32::NAN })
            .collect()
    }

    /// The depth (cents, peak) of a vibrato in its held part: the pitch's
    /// wobble about its 250 ms average (short gaps bridged, smoothed over
    /// 15 ms), counted only when it swings regularly at a voice's rate
    /// (its autocorrelation peaks at a period of 4–8 Hz).
    pub fn vibrato(&self, scale: Scale) -> f32 {
        if self.hop <= 0.0 || self.end - self.start < scale.of(VIBRATO_NOTE) {
            return 0.0;
        }
        let per_frame = self.hop / scale.per_second;
        let skip = (0.08 / per_frame).round() as usize;
        let track = self.gated();
        let held: Vec<f32> = track
            .iter()
            .skip(skip)
            .take(track.len().saturating_sub(2 * skip))
            .copied()
            .collect();
        let known = held.iter().filter(|p| p.is_finite()).count();
        if held.len() < 16 || known * 5 < held.len() * 4 {
            return 0.0;
        }
        // Gaps bridged by the frames either side.
        let mut filled = held.clone();
        let mut last: Option<(usize, f32)> = None;
        for i in 0..filled.len() {
            let here = filled[i];
            if here.is_finite() {
                if let Some((j, v)) = last
                    && i > j + 1
                {
                    for (k, slot) in filled.iter_mut().enumerate().take(i).skip(j + 1) {
                        let t = (k - j) as f32 / (i - j) as f32;
                        *slot = v + t * (here - v);
                    }
                }
                last = Some((i, here));
            }
        }
        let first = filled.iter().position(|p| p.is_finite()).unwrap_or(0);
        let end = filled
            .iter()
            .rposition(|p| p.is_finite())
            .map_or(0, |i| i + 1);
        let x: Vec<f64> = filled[first..end].iter().map(|v| f64::from(*v)).collect();
        if x.len() < 16 {
            return 0.0;
        }
        let mean_over = |v: &[f64], half: usize, i: usize| {
            let (a, b) = (i.saturating_sub(half), (i + half + 1).min(v.len()));
            v[a..b].iter().sum::<f64>() / (b - a) as f64
        };
        let smooth = ((0.0075 / per_frame).round() as usize).max(1);
        let x: Vec<f64> = (0..x.len()).map(|i| mean_over(&x, smooth, i)).collect();
        let half = ((0.125 / per_frame).round() as usize).max(1);
        let residual: Vec<f64> = (0..x.len())
            .map(|i| (x[i] - mean_over(&x, half, i)) * 100.0)
            .collect();
        let energy: f64 = residual.iter().map(|r| r * r).sum();
        if energy <= 0.0 {
            return 0.0;
        }
        let lags = (
            ((1.0 / VIBRATO_HZ.1) / per_frame).floor() as usize,
            ((1.0 / VIBRATO_HZ.0) / per_frame).ceil() as usize,
        );
        let periodic = (lags.0.max(1)..=lags.1.min(residual.len() / 2))
            .map(|lag| {
                residual
                    .iter()
                    .zip(&residual[lag..])
                    .map(|(a, b)| a * b)
                    .sum::<f64>()
                    / energy
            })
            .fold(f64::NEG_INFINITY, f64::max);
        if periodic < PERIODIC {
            return 0.0;
        }
        let rms = (energy / residual.len() as f64).sqrt();
        (rms * std::f64::consts::SQRT_2) as f32
    }

    /// How far (cents) it glides into its pitch: the mean offset of its
    /// first 60 ms clearly heard, when they lie on one side of it (a
    /// voice's scoop up or fall into the note; an attack's noise is not).
    pub fn glide(&self, scale: Scale) -> f32 {
        if self.hop <= 0.0 || !self.pitch.is_finite() {
            return 0.0;
        }
        let per_frame = self.hop / scale.per_second;
        let n = ((0.06 / per_frame).round() as usize).max(2);
        let track = self.gated();
        let Some(from) = track.iter().position(|p| p.is_finite()) else {
            return 0.0;
        };
        let first: Vec<f32> = track[from..]
            .iter()
            .take(n)
            .copied()
            .filter(|p| p.is_finite())
            .map(|p| p - self.pitch)
            .collect();
        if first.len() < 2 {
            return 0.0;
        }
        let mean = first.iter().sum::<f32>() / first.len() as f32;
        let one_side = first.iter().filter(|d| d.signum() == mean.signum()).count();
        if one_side * 4 < first.len() * 3 {
            return 0.0;
        }
        (mean * 100.0).abs()
    }

    /// How much it sounds like a voice (0…1): its vibrato and its glide.
    pub fn voice(&self, scale: Scale) -> f32 {
        voice_of(self.vibrato(scale), self.glide(scale))
    }
}

/// A voice's marks (0…1) from a vibrato's depth and a glide (cents).
fn voice_of(vibrato: f32, glide: f32) -> f32 {
    let v = (vibrato / 18.0).clamp(0.0, 1.0);
    let g = ((glide - 15.0) / 45.0).clamp(0.0, 1.0);
    (0.75 * v + 0.45 * g).min(1.0)
}

/// Each note's voice marks ([`Heard::voice`]), a vibrato borrowed left
/// out: a note sharing partials with a note sounding with it (an octave,
/// a twelfth … apart) whose vibrato is deeper wobbles with that note's
/// partials, not its own.
fn voices(notes: &[Heard], scale: Scale) -> Vec<f32> {
    let vib: Vec<f32> = notes.iter().map(|n| n.vibrato(scale)).collect();
    notes
        .iter()
        .enumerate()
        .map(|(i, n)| {
            let borrowed = vib[i] > 0.0
                && notes.iter().enumerate().any(|(j, m)| {
                    let interval = (i32::from(n.written()) - i32::from(m.written())).abs();
                    j != i
                        && vib[j] > vib[i]
                        && PARTIALS.contains(&interval)
                        && n.end.min(m.end) - n.start.max(m.start) > 0.5 * (n.end - n.start)
                });
            voice_of(if borrowed { 0.0 } else { vib[i] }, n.glide(scale))
        })
        .collect()
}

/// Step 1 (see the module docs): notes hardly followed out, partials,
/// fragments joined, ghosts out; sorted by start.
pub fn clean(notes: Vec<Heard>, scale: Scale) -> Vec<Heard> {
    let mut notes: Vec<Heard> = notes
        .into_iter()
        .filter(|n| n.end > n.start && n.voiced() >= LEAST_VOICED)
        .flat_map(|n| split_attacks(n, scale))
        .collect();
    notes.sort_by(|a, b| a.start.total_cmp(&b.start).then(a.key.cmp(&b.key)));
    drop_partials(&mut notes);
    // Fragments of one note.
    let mut joined: Vec<Heard> = Vec::with_capacity(notes.len());
    for n in notes {
        let key = n.written();
        let piece = |x: &Heard| x.end - x.start < scale.of(FRAGMENT_PIECE);
        let to = joined.iter_mut().rev().find(|p| {
            if p.written() != key {
                return false;
            }
            let gap = n.start - p.end;
            (gap <= scale.of(FRAGMENT_GAP)
                || (gap <= scale.of(FRAGMENT_GAP_SHORT) && (piece(p) || piece(&n))))
                && !attacks(p, &n, scale)
        });
        match to {
            Some(p) => join(p, n),
            None => joined.push(n),
        }
    }
    drop_partials(&mut joined);
    // Ghosts: quiet against the notes round them.
    let level: Vec<f32> = joined.iter().map(Heard::level_db).collect();
    let keep: Vec<bool> = joined
        .iter()
        .enumerate()
        .map(|(i, n)| {
            let near = scale.of(GHOST_NEAR);
            let loudest = joined
                .iter()
                .enumerate()
                .filter(|(j, m)| *j != i && m.start < n.end + near && m.end > n.start - near)
                .map(|(j, _)| level[j])
                .fold(f32::NEG_INFINITY, f32::max);
            let below = loudest - level[i];
            let short = n.end - n.start < scale.of(GHOST_SHORT);
            !(below > GHOST_DB || (short && n.velocity < 0.45 && below > GHOST_SHORT_DB))
        })
        .collect();
    let mut i = 0;
    joined.retain(|_| {
        i += 1;
        keep[i - 1]
    });
    joined.sort_by(|a, b| a.start.total_cmp(&b.start).then(a.key.cmp(&b.key)));
    joined
}

/// A note cut where it is struck again: where its level, having fallen
/// well under its usual level, jumps [`SPLIT_RISE`] times above the lowest of
/// the 60 ms before and back to most of its usual level (a voice's
/// vibrato swells less), each piece at least [`FRAGMENT_PIECE`] long and
/// at the note's pitch.
fn split_attacks(n: Heard, scale: Scale) -> Vec<Heard> {
    if n.hop <= 0.0 || n.levels.len() != n.track.len() || n.levels.is_empty() {
        return vec![n];
    }
    let heard: Vec<f32> = n.levels.iter().copied().filter(|l| *l > 0.0).collect();
    let usual = median(&heard).unwrap_or(0.0);
    let back = ((scale.of(0.06) / n.hop).round() as usize).max(2);
    let rise = ((scale.of(0.03) / n.hop).round() as usize).max(1);
    let piece = ((scale.of(FRAGMENT_PIECE) / n.hop).round() as usize).max(1);
    let mut cuts: Vec<usize> = Vec::new();
    let mut k = piece;
    while k + piece < n.levels.len() {
        let (from, to) = (k.saturating_sub(back), (k + rise).min(n.levels.len()));
        let (peak_at, peak) =
            n.levels[k..to]
                .iter()
                .enumerate()
                .fold(
                    (k, 0.0f32),
                    |acc, (i, l)| {
                        if *l > acc.1 { (k + i, *l) } else { acc }
                    },
                );
        // The trough before the rise.
        let (low_at, low) =
            n.levels[from..peak_at]
                .iter()
                .enumerate()
                .fold((from, f32::MAX), |acc, (i, l)| {
                    if *l < acc.1 { (from + i, *l) } else { acc }
                });
        let last = cuts.last().copied().unwrap_or(0);
        if peak >= 0.7 * usual
            && low <= SPLIT_TROUGH * usual
            && peak > SPLIT_RISE * low.max(1e-9)
            && low_at >= last + piece
        {
            cuts.push(low_at + 1);
            k = low_at + 1 + piece;
        } else {
            k += 1;
        }
    }
    if cuts.is_empty() {
        return vec![n];
    }
    let mut out = Vec::with_capacity(cuts.len() + 1);
    let mut from = 0;
    for cut in cuts.into_iter().chain([n.track.len()]) {
        let track = n.track[from..cut].to_vec();
        let levels = n.levels[from..cut].to_vec();
        let loud: Vec<f32> = levels.iter().copied().filter(|l| *l > 0.0).collect();
        out.push(Heard {
            start: n.start + from as f64 * n.hop,
            end: if cut == n.track.len() {
                n.end
            } else {
                n.start + cut as f64 * n.hop
            },
            key: n.key,
            pitch: n.pitch,
            velocity: n.velocity,
            loudness: median(&loud).map_or(n.loudness, |l| 20.0 * l.log10()),
            track,
            levels,
            hop: n.hop,
        });
        from = cut;
    }
    out
}

/// Leave out notes that are a louder note's partial: on one of its
/// overtones or subharmonics, sounding mostly with it (or with it struck
/// again), much less strongly activated.
fn drop_partials(notes: &mut Vec<Heard>) {
    let keep: Vec<bool> = notes
        .iter()
        .map(|n| {
            !PARTIALS.iter().any(|interval| {
                let with: f64 = notes
                    .iter()
                    .filter(|m| {
                        i32::from(n.written()) - i32::from(m.written()) == *interval
                            && n.velocity < PARTIAL_SHARE * m.velocity
                    })
                    .map(|m| (n.end.min(m.end) - n.start.max(m.start)).max(0.0))
                    .sum();
                with >= PARTIAL_OVERLAP * (n.end - n.start)
            })
        })
        .collect();
    let mut i = 0;
    notes.retain(|_| {
        i += 1;
        keep[i - 1]
    });
}

/// Whether `b`, the same key as `a` just after it, starts with an attack
/// of its own: its first 60 ms [`ATTACK`] times louder than the quietest
/// of `a`'s last 100 ms — a note struck again (its predecessor fading or
/// damped first), not a piece of one held.
fn attacks(a: &Heard, b: &Heard, scale: Scale) -> bool {
    if a.levels.is_empty() || b.levels.is_empty() || a.hop <= 0.0 {
        return false;
    }
    let frames = |secs: f64| ((scale.of(secs) / a.hop).round() as usize).max(1);
    // `a` before `b` starts.
    let a_end = ((b.start.min(a.end) - a.start) / a.hop).round().max(0.0) as usize;
    let a_end = a_end.min(a.levels.len());
    let tail = &a.levels[a_end.saturating_sub(frames(0.1))..a_end];
    let before = tail
        .iter()
        .copied()
        .filter(|l| *l > 0.0)
        .fold(f32::MAX, f32::min);
    let after = b.levels[..frames(0.06).min(b.levels.len())]
        .iter()
        .copied()
        .fold(0.0f32, f32::max);
    before < f32::MAX && after > ATTACK * before
}

/// `b` joined into `a` (the same note): its span, the louder level, the
/// pitch track across both (the gap unheard), the median again.
fn join(a: &mut Heard, b: Heard) {
    let (start, end) = (a.start.min(b.start), a.end.max(b.end));
    if a.hop > 0.0 && b.hop > 0.0 {
        let frames = ((end - start) / a.hop).round() as usize + 1;
        let mut track = vec![f32::NAN; frames];
        let mut levels = vec![0.0f32; frames];
        for n in [&*a, &b] {
            let off = ((n.start - start) / a.hop).round() as usize;
            for (k, p) in n.track.iter().enumerate() {
                if let Some(slot) = track.get_mut(off + k)
                    && p.is_finite()
                {
                    *slot = *p;
                }
            }
            for (k, l) in n.levels.iter().enumerate() {
                if let Some(slot) = levels.get_mut(off + k) {
                    *slot = slot.max(*l);
                }
            }
        }
        a.track = track;
        if !a.levels.is_empty() || !b.levels.is_empty() {
            a.levels = levels;
        }
        if let Some(m) = median(&a.track) {
            a.pitch = m;
        }
    }
    a.start = start;
    a.end = end;
    a.velocity = a.velocity.max(b.velocity);
    a.loudness = if a.loudness.is_finite() && b.loudness.is_finite() {
        a.loudness.max(b.loudness)
    } else if a.loudness.is_finite() {
        a.loudness
    } else {
        b.loudness
    };
}

/// The median of the finite values.
pub fn median(v: &[f32]) -> Option<f32> {
    let mut f: Vec<f32> = v.iter().copied().filter(|x| x.is_finite()).collect();
    if f.is_empty() {
        return None;
    }
    f.sort_by(f32::total_cmp);
    Some(f[f.len() / 2])
}

/// A melody note: which heard note, where the line has it, its key.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Sung {
    pub index: usize,
    pub start: f64,
    pub end: f64,
    pub key: u8,
}

/// A rest's cost a second where notes sound together (salience is 0…1: a
/// note is the melody when it is salient enough); where a note sounds
/// alone the rest costs a full second's worth and skipping its onset
/// [`SKIP`] more (a solo line keeps all its notes).
const REST: f64 = 0.62;
const SKIP: f64 = 0.3;
/// Entering a note after a rest.
const ENTER: f64 = 0.04;
/// Leaps: a semitone's cost past a whole tone, and once more past an
/// octave.
const LEAP: f64 = 0.012;
const OCTAVE_LEAP: f64 = 0.12;
/// Leaving a note into a rest while it still sounds.
const ABANDON: f64 = 0.35;
/// The register pass: a semitone's cost past a fifth from it.
const REGISTER: f64 = 0.012;

/// Step 2 (see the module docs): the melody's notes, in order.
pub fn melody(notes: &[Heard], scale: Scale) -> Vec<Sung> {
    let first = skyline(notes, scale, None);
    if first.is_empty() {
        return first;
    }
    // The register sung: each note's median of the melody within 3 s.
    let reach = scale.of(3.0);
    let register: Vec<(f64, f32)> = first
        .iter()
        .map(|s| {
            let near: Vec<f32> = first
                .iter()
                .filter(|o| (o.start - s.start).abs() <= reach)
                .map(|o| notes[o.index].sounding())
                .collect();
            (s.start, median(&near).unwrap_or(notes[s.index].sounding()))
        })
        .collect();
    skyline(notes, scale, Some(&register))
}

/// One pass of the skyline (`register`: the first pass's register by
/// time).
fn skyline(notes: &[Heard], scale: Scale, register: Option<&[(f64, f32)]>) -> Vec<Sung> {
    if notes.is_empty() {
        return Vec::new();
    }
    let mut cuts: Vec<f64> = notes.iter().flat_map(|n| [n.start, n.end]).collect();
    cuts.sort_by(f64::total_cmp);
    cuts.dedup_by(|a, b| (*a - *b).abs() < 1e-9);
    let pieces = cuts.len().saturating_sub(1);
    if pieces == 0 {
        return Vec::new();
    }
    let level: Vec<f32> = notes.iter().map(Heard::level_db).collect();
    let voice = voices(notes, scale);
    let register_at = |t: f64| -> Option<f32> {
        let r = register?;
        let i = r.partition_point(|(s, _)| *s <= t);
        r.get(i.saturating_sub(1)).map(|(_, p)| *p)
    };
    // The states of each piece: a rest (None) or a note sounding then.
    let sounding: Vec<Vec<usize>> = (0..pieces)
        .map(|k| {
            let (a, b) = (cuts[k], cuts[k + 1]);
            (0..notes.len())
                .filter(|&i| notes[i].start <= a + 1e-9 && notes[i].end >= b - 1e-9)
                .collect()
        })
        .collect();
    // Viterbi: cost and the state it came from, per piece and state
    // (state 0 the rest, s + 1 the s-th note sounding).
    let mut cost: Vec<Vec<f64>> = Vec::with_capacity(pieces);
    let mut from: Vec<Vec<usize>> = Vec::with_capacity(pieces);
    for k in 0..pieces {
        let (a, b) = (cuts[k], cuts[k + 1]);
        let secs = (b - a) / scale.per_second;
        let here = &sounding[k];
        let loudest = here
            .iter()
            .map(|&i| level[i])
            .fold(f32::NEG_INFINITY, f32::max);
        let mut by_pitch: Vec<usize> = here.clone();
        by_pitch.sort_by(|x, y| notes[*x].sounding().total_cmp(&notes[*y].sounding()));
        let emit = |i: usize| -> f64 {
            let n = &notes[i];
            let rel = (1.0 - (loudest - level[i]) / LEVEL_RANGE).clamp(0.0, 1.0);
            let height = if here.len() <= 1 {
                1.0
            } else {
                by_pitch.iter().position(|x| *x == i).unwrap_or(0) as f32 / (here.len() - 1) as f32
            };
            let length = (((n.end - n.start) / scale.per_second) / 0.5).min(1.0) as f32;
            let salience = 0.35 * rel + 0.4 * height + 0.2 * voice[i] + 0.05 * length;
            f64::from(1.0 - salience.min(1.0)) * secs
        };
        let rest = match here.as_slice() {
            [] => 0.0,
            [one] => {
                secs + if (notes[*one].start - a).abs() < 1e-9 {
                    SKIP
                } else {
                    0.0
                }
            }
            _ => REST * secs,
        };
        let states = here.len() + 1;
        let mut c = vec![f64::INFINITY; states];
        let mut f = vec![0usize; states];
        for (s, slot) in c.iter_mut().enumerate() {
            let note = s.checked_sub(1).map(|j| here[j]);
            let own = match note {
                None => rest,
                Some(i) => emit(i),
            };
            let enters = note.is_some_and(|i| (notes[i].start - a).abs() < 1e-9);
            let reg = match (note, register_at(a)) {
                (Some(i), Some(r)) if enters => {
                    REGISTER * f64::from(((notes[i].sounding() - r).abs() - 7.0).max(0.0))
                }
                _ => 0.0,
            };
            if k == 0 {
                // A note sounding from the start is entered there.
                *slot = own + if note.is_some() { ENTER + reg } else { 0.0 };
                continue;
            }
            let prev = &sounding[k - 1];
            let mut best = (f64::INFINITY, 0usize);
            for (ps, pc) in cost[k - 1].iter().enumerate() {
                if !pc.is_finite() {
                    continue;
                }
                let was = ps.checked_sub(1).map(|j| prev[j]);
                let step = match (was, note) {
                    (None, None) => 0.0,
                    (Some(p), None) => {
                        if notes[p].end > a + 1e-9 {
                            ABANDON
                        } else {
                            0.0
                        }
                    }
                    (Some(p), Some(i)) if p == i => 0.0,
                    (_, Some(_)) if !enters => f64::INFINITY,
                    (None, Some(_)) => ENTER + reg,
                    (Some(p), Some(i)) => {
                        let leap = f64::from((notes[i].sounding() - notes[p].sounding()).abs());
                        LEAP * (leap - 2.0).max(0.0)
                            + if leap > 12.5 { OCTAVE_LEAP } else { 0.0 }
                            + reg
                    }
                };
                let total = pc + step;
                if total < best.0 {
                    best = (total, ps);
                }
            }
            *slot = best.0 + own;
            f[s] = best.1;
        }
        cost.push(c);
        from.push(f);
    }
    // Back through the pieces.
    let mut states = vec![0usize; pieces];
    let mut s = cost[pieces - 1]
        .iter()
        .enumerate()
        .min_by(|a, b| a.1.total_cmp(b.1))
        .map_or(0, |(i, _)| i);
    for k in (0..pieces).rev() {
        states[k] = s;
        s = from[k][s];
    }
    let mut out: Vec<Sung> = Vec::new();
    let mut current: Option<usize> = None;
    for k in 0..pieces {
        let note = states[k].checked_sub(1).map(|j| sounding[k][j]);
        if note != current {
            if let Some(last) = out.last_mut()
                && current.is_some()
            {
                last.end = cuts[k];
            }
            if let Some(i) = note {
                out.push(Sung {
                    index: i,
                    start: notes[i].start,
                    end: notes[i].end,
                    key: notes[i].written(),
                });
            }
            current = note;
        }
    }
    if let (Some(last), Some(_)) = (out.last_mut(), current) {
        last.end = cuts[pieces];
    }
    out
}

/// Melody notes that are an octave off both their neighbours, which are
/// close to each other (index into `line`, and which way the octave would
/// go: −1 down, +1 up).
pub fn octave_suspects(line: &[Sung]) -> Vec<(usize, i8)> {
    let mut out = Vec::new();
    for i in 1..line.len().saturating_sub(1) {
        let (p, n, q) = (
            i32::from(line[i - 1].key),
            i32::from(line[i].key),
            i32::from(line[i + 1].key),
        );
        let (up, down) = (n - p, n - q);
        if up.abs() >= 10 && down.abs() >= 10 && up.signum() == down.signum() && (p - q).abs() <= 7
        {
            out.push((i, if up > 0 { -1 } else { 1 }));
        }
    }
    out
}

/// Step 3: each suspect moved by an octave where its audio says so (the
/// octave it would move to heard at least as strongly as the note itself;
/// a note named by its second harmonic has its fundamental's odd harmonics
/// an octave below, one named a subharmonic low has hardly any of them).
pub fn fix_octaves(line: &mut [Sung], heard: &[(usize, i8, Octave)]) {
    for &(i, way, o) in heard {
        let Some(s) = line.get_mut(i) else {
            continue;
        };
        let other = if way < 0 { o.below } else { o.above };
        // A note's octave below always hears its even harmonics (about 0.7
        // of its own sum); the octave above, its even ones (about half):
        // past those it is the octave sounding.
        let need = if way < 0 { 0.95 } else { 1.1 };
        if o.here > 0.0 && other >= need * o.here {
            s.key = (i32::from(s.key) + 12 * i32::from(way)).clamp(0, 127) as u8;
        }
    }
}

/// A part's note as written from: when it sounds, its key, how loud
/// (0…1) and whether it is the melody's.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Voiced {
    pub start: f64,
    pub end: f64,
    pub key: u8,
    pub level: f32,
    pub melody: bool,
}

/// `notes` with the melody's marked: every note as heard (written at its
/// pitch), the melody's at the key the line gives it (an octave slip
/// fixed).
pub fn voiced(notes: &[Heard], line: &[Sung]) -> Vec<Voiced> {
    let mut out: Vec<Voiced> = notes
        .iter()
        .map(|n| Voiced {
            start: n.start,
            end: n.end,
            key: n.written(),
            level: n.velocity,
            melody: false,
        })
        .collect();
    for s in line {
        if let Some(v) = out.get_mut(s.index) {
            v.melody = true;
            v.key = s.key;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn note(start: f64, end: f64, key: u8, db: f32) -> Heard {
        Heard {
            start,
            end,
            key,
            pitch: f32::from(key),
            velocity: 0.7,
            loudness: db,
            track: vec![f32::from(key); ((end - start) / 0.005) as usize + 1],
            levels: vec![10f32.powf(db / 20.0); ((end - start) / 0.005) as usize + 1],
            hop: 0.005,
        }
    }

    fn keys(line: &[Sung]) -> Vec<u8> {
        line.iter().map(|s| s.key).collect()
    }

    #[test]
    fn a_solo_line_is_its_notes() {
        let notes = vec![
            note(0.0, 0.5, 60, -20.0),
            note(0.5, 1.0, 64, -22.0),
            note(1.0, 1.5, 67, -21.0),
            note(2.0, 3.0, 72, -24.0),
        ];
        let line = melody(&clean(notes, Scale::SECONDS), Scale::SECONDS);
        assert_eq!(keys(&line), [60, 64, 67, 72]);
        assert!((line[3].end - 3.0).abs() < 1e-9);
    }

    #[test]
    fn the_top_voice_over_block_chords_is_the_melody() {
        let mut notes = Vec::new();
        for (i, top) in [76u8, 79, 77, 74].iter().enumerate() {
            let t = i as f64 * 0.5;
            notes.push(note(t, t + 0.5, *top, -18.0));
        }
        for k in [48u8, 52, 55] {
            notes.push(note(0.0, 1.0, k, -24.0));
            notes.push(note(1.0, 2.0, k + 5, -24.0));
        }
        let line = melody(&clean(notes, Scale::SECONDS), Scale::SECONDS);
        assert_eq!(keys(&line), [76, 79, 77, 74]);
    }

    #[test]
    fn a_sung_line_wins_over_the_chords_top() {
        // The pad's top (E5) above the voice (C5…D5); the voice louder,
        // with vibrato.
        let mut notes = vec![note(0.0, 2.0, 76, -26.0), note(0.0, 2.0, 67, -26.0)];
        for (i, k) in [72u8, 74, 72].iter().enumerate() {
            let t = i as f64 * 0.66;
            let mut v = note(t, t + 0.66, *k, -18.0);
            for (j, p) in v.track.iter_mut().enumerate() {
                *p += 0.3 * (std::f32::consts::TAU * 5.5 * j as f32 * 0.005).sin();
            }
            notes.push(v);
        }
        let line = melody(&clean(notes, Scale::SECONDS), Scale::SECONDS);
        assert_eq!(keys(&line), [72, 74, 72]);
    }

    #[test]
    fn fragments_join_and_ghosts_go() {
        let notes = vec![
            note(0.0, 0.4, 67, -20.0),
            // The same note again after 20 ms: a fragment.
            note(0.42, 0.9, 67, -21.0),
            // A real repeat after a breath.
            note(1.0, 1.5, 67, -20.0),
            // A ghost a semitone up, 25 dB down.
            note(0.2, 0.35, 68, -45.0),
        ];
        let c = clean(notes, Scale::SECONDS);
        let spans: Vec<(f64, f64, u8)> = c.iter().map(|n| (n.start, n.end, n.key)).collect();
        assert_eq!(spans, [(0.0, 0.9, 67), (1.0, 1.5, 67)]);
        // A key struck again (its level jumping back up where the second
        // piece starts) stays two notes; a much weaker octave above and
        // below a note sounding with it are its partials.
        let mut decayed = note(2.0, 2.52, 48, -20.0);
        for (k, l) in decayed.levels.iter_mut().enumerate() {
            *l *= (-(k as f32) * 0.005 / 0.2).exp();
        }
        let mut over = note(2.0, 2.5, 60, -30.0);
        over.velocity = 0.3;
        let mut under = note(2.1, 2.4, 36, -30.0);
        under.velocity = 0.3;
        let notes = vec![decayed, note(2.5, 3.0, 48, -20.0), over, under];
        let c = clean(notes, Scale::SECONDS);
        let spans: Vec<(f64, f64, u8)> = c.iter().map(|n| (n.start, n.end, n.key)).collect();
        assert_eq!(spans, [(2.0, 2.52, 48), (2.5, 3.0, 48)]);
        // Heard as one note, but struck again half way: two.
        let mut held = note(4.0, 5.0, 52, -20.0);
        for (k, l) in held.levels.iter_mut().enumerate() {
            let t = (k as f32 * 0.005) % 0.5;
            *l *= (-t / 0.15).exp();
        }
        let c = clean(vec![held], Scale::SECONDS);
        let spans: Vec<(f64, f64)> = c.iter().map(|n| (n.start, n.end)).collect();
        assert_eq!(spans.len(), 2, "{spans:?}");
        assert!((spans[1].0 - 4.5).abs() < 0.011 && (spans[1].1 - 5.0).abs() < 1e-9);
    }

    #[test]
    fn notes_are_written_at_their_pitch_and_a_voice_is_recognised() {
        let mut n = note(0.0, 1.0, 61, -20.0);
        n.pitch = 60.2;
        for (j, p) in n.track.iter_mut().enumerate() {
            let t = j as f32 * 0.005;
            *p = 60.2 + 0.3 * (std::f32::consts::TAU * 5.5 * t).sin() - (1.0 - t * 12.0).max(0.0);
        }
        assert_eq!(n.written(), 60);
        assert!(
            n.vibrato(Scale::SECONDS) > 15.0,
            "{}",
            n.vibrato(Scale::SECONDS)
        );
        assert!(n.glide(Scale::SECONDS) > 30.0);
        assert!(n.voice(Scale::SECONDS) > 0.7);
        let steady = note(0.0, 1.0, 60, -20.0);
        assert_eq!(steady.vibrato(Scale::SECONDS), 0.0);
        assert_eq!(steady.voice(Scale::SECONDS), 0.0);
    }

    #[test]
    fn an_octave_slip_moves_back_when_the_audio_says_so() {
        let notes = vec![
            note(0.0, 0.5, 67, -20.0),
            note(0.5, 1.0, 81, -20.0),
            note(1.0, 1.5, 69, -20.0),
        ];
        let mut line = melody(&notes, Scale::SECONDS);
        let suspects = octave_suspects(&line);
        assert_eq!(suspects, [(1, -1)]);
        // Heard an octave below as strongly as where it was named: moved.
        let mut kept = line.clone();
        fix_octaves(
            &mut line,
            &[(
                1,
                -1,
                Octave {
                    below: 1.2,
                    here: 1.0,
                    above: 0.4,
                },
            )],
        );
        assert_eq!(keys(&line), [67, 69, 69]);
        // A real leap (the octave below only its even harmonics): kept.
        fix_octaves(
            &mut kept,
            &[(
                1,
                -1,
                Octave {
                    below: 0.7,
                    here: 1.0,
                    above: 0.5,
                },
            )],
        );
        assert_eq!(keys(&kept), [67, 81, 69]);
    }
}
