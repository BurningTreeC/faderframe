//! Voice to MIDI measured: a corpus of sung phrases, whistling, a plucked
//! lead line, strummed and picked guitar chords and piano, each at input
//! levels from −40 to −6 dBFS (peak) in a quiet room, a noisy one and with
//! mains hum, through the monophonic and the polyphonic tracker, scored
//! note by note (see `score`).
//!
//! The table (release mode recommended):
//! `cargo test --release -p faderframe-analysis --test voice_corpus -- --ignored --nocapture`
//! (`VOICE_CORPUS=mono|poly`, `VOICE_RESPONSE=fast|balanced|stable|all`,
//! `VOICE_CALIBRATED=1` adds the tables after a calibration). The fast
//! test holds the accuracy reached on part of it.
#![allow(clippy::unwrap_used)]

mod score;
mod signals;

use faderframe_analysis::polyvoice::PolyTracker;
use faderframe_analysis::voice::{Meter, calibrate};
use faderframe_analysis::voice::{Responsiveness, VoiceConfig, VoiceEvent, VoiceTracker};
use score::{Tally, heard, score};
use signals::{Piece, RATE, Room, Vowel::*, at_level, breath, step};
use std::sync::Arc;

/// Input levels (peak dBFS).
const LEVELS: [f64; 5] = [-40.0, -30.0, -20.0, -12.0, -6.0];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Mode {
    Mono,
    Poly,
}

impl Mode {
    /// How far from a note's start a note may be placed to be right (s).
    fn tolerance(self) -> f64 {
        match self {
            Mode::Mono => 0.05,
            Mode::Poly => 0.07,
        }
    }
}

fn mono_pieces() -> Vec<Piece> {
    vec![
        signals::sung(
            "sung, man",
            &[
                step(50, 0.45, 0.12, Oh),
                step(55, 0.9, 0.0, Ah),
                step(59, 0.25, 0.0, Ee),
                step(55, 0.3, 0.0, Ah),
                step(59, 0.8, 0.15, Ee),
                step(57, 0.45, 0.0, Ah),
                breath(55, 1.4, 0.55, Ah),
                step(52, 0.45, 0.1, Eh),
                breath(50, 1.1, 0.6, Oo),
                step(45, 0.5, 0.08, Ah),
                step(48, 0.18, 0.0, Ah),
                step(50, 0.18, 0.0, Ah),
                step(52, 0.18, 0.0, Ah),
                step(53, 0.18, 0.0, Ah),
                step(55, 0.7, 0.2, Oh),
                step(43, 0.6, 0.1, Oo),
                step(47, 0.5, 0.0, Oh),
                step(50, 1.6, 0.4, Ah),
            ],
            false,
            1,
        ),
        signals::sung(
            "sung, woman",
            &[
                step(64, 0.4, 0.1, Ah),
                step(69, 0.8, 0.0, Ah),
                step(72, 0.3, 0.0, Ee),
                step(71, 0.3, 0.0, Eh),
                step(69, 0.9, 0.2, Ah),
                breath(76, 1.3, 0.6, Oh),
                step(74, 0.25, 0.0, Ee),
                step(72, 0.25, 0.0, Ah),
                step(71, 0.25, 0.0, Ah),
                step(69, 0.25, 0.0, Ah),
                step(67, 0.8, 0.15, Oo),
                step(62, 0.4, 0.1, Eh),
                step(64, 0.4, 0.0, Ah),
                step(66, 0.4, 0.0, Ah),
                step(67, 1.8, 0.4, Ah),
                step(79, 0.6, 0.1, Ah),
                step(77, 0.5, 0.0, Oh),
                step(74, 1.2, 0.3, Oo),
            ],
            true,
            2,
        ),
        signals::whistle(
            "whistle",
            &[
                (81, 0.35, 0.06),
                (84, 0.35, 0.0),
                (88, 0.5, 0.15),
                (86, 0.25, 0.0),
                (84, 0.25, 0.0),
                (81, 0.6, 0.3),
                (79, 0.3, 0.06),
                (83, 0.3, 0.06),
                (86, 0.3, 0.0),
                (91, 0.7, 0.3),
                (93, 0.4, 0.0),
                (91, 0.3, 0.0),
                (88, 0.8, 0.4),
            ],
            3,
        ),
        signals::lead_line(
            "plucked lead",
            &[
                (64, 0.3, 0.0),
                (67, 0.3, 0.0),
                (69, 0.6, 0.0),
                (72, 0.3, 0.0),
                (71, 0.3, 0.0),
                (69, 0.5, 0.1),
                (64, 0.25, 0.0),
                (62, 0.25, 0.0),
                (60, 0.6, 0.2),
                (57, 0.4, 0.0),
                (55, 0.4, 0.0),
                (52, 0.9, 0.4),
            ],
            4,
        ),
    ]
}

fn poly_pieces() -> Vec<Piece> {
    vec![
        signals::strums(
            "strummed chords",
            &[
                (&[48, 52, 55, 60, 64], 1.6),
                (&[45, 52, 57, 60, 64], 1.6),
                (&[41, 48, 53, 57, 60, 65], 1.6),
                (&[43, 47, 50, 55, 59, 67], 1.6),
                (&[40, 47, 52, 55, 59, 64], 1.6),
                (&[48, 52, 55, 60], 2.0),
            ],
            0.012,
            11,
        ),
        signals::arpeggio(
            "picked arpeggio",
            &[
                &[45, 52, 57, 60, 64, 60, 57, 52],
                &[40, 47, 52, 56, 59, 64, 59, 56],
                &[48, 52, 55, 60, 64, 60, 55, 52],
                &[43, 47, 50, 55, 59, 67, 59, 55],
            ],
            0.22,
            12,
        ),
        signals::piano(
            "piano chords",
            &[
                (&[48, 55, 64, 67], 0.0, 0.9, 0.7),
                (&[45, 55, 60, 64], 1.0, 0.9, 0.85),
                (&[50, 57, 60, 65], 2.0, 0.9, 0.55),
                (&[43, 53, 59, 62], 3.0, 0.9, 0.9),
                (&[48, 55, 60, 64], 4.0, 1.6, 0.75),
            ],
            13,
        ),
        signals::piano(
            "piano, two hands",
            &[
                (&[36], 0.0, 0.8, 0.7),
                (&[43], 0.8, 0.8, 0.6),
                (&[41], 1.6, 0.8, 0.7),
                (&[43], 2.4, 0.8, 0.6),
                (&[36], 3.2, 1.2, 0.7),
                (&[72], 0.0, 0.38, 0.8),
                (&[74], 0.4, 0.38, 0.7),
                (&[76], 0.8, 0.38, 0.8),
                (&[79], 1.2, 0.38, 0.9),
                (&[77], 1.6, 0.38, 0.7),
                (&[76], 2.0, 0.38, 0.7),
                (&[74], 2.4, 0.38, 0.6),
                (&[72], 2.8, 0.38, 0.8),
                (&[71], 3.2, 0.38, 0.7),
                (&[72], 3.6, 0.8, 0.8),
            ],
            14,
        ),
    ]
}

/// A tracker of either kind.
enum Tracker {
    Mono(Box<VoiceTracker>),
    Poly(Box<PolyTracker>),
}

impl Tracker {
    fn new(mode: Mode, config: VoiceConfig) -> Self {
        match mode {
            Mode::Mono => Self::Mono(Box::new(VoiceTracker::new(RATE, config))),
            Mode::Poly => Self::Poly(Box::new(PolyTracker::new(RATE, config))),
        }
    }

    /// Counting what it hears into `meter`.
    fn metered(self, meter: Arc<Meter>) -> Self {
        match self {
            Self::Mono(t) => Self::Mono(Box::new(t.with_meter(meter))),
            Self::Poly(t) => Self::Poly(Box::new(t.with_meter(meter))),
        }
    }

    /// Its events over `x` in 256-frame callbacks (absolute frames from
    /// `base`: decided, placed).
    fn listen(&mut self, x: &[f32], base: i64, out: &mut Vec<(i64, i64, VoiceEvent)>) {
        for (b, chunk) in x.chunks(256).enumerate() {
            let base = base + (b * 256) as i64;
            let mut emit = |now: isize, at: isize, e: VoiceEvent| {
                out.push((base + now as i64, base + at as i64, e))
            };
            match self {
                Self::Mono(t) => t.process(chunk, &mut emit),
                Self::Poly(t) => t.process(chunk, &mut emit),
            }
        }
    }

    /// Release what sounds at frame `at`.
    fn reset(&mut self, at: i64, out: &mut Vec<(i64, i64, VoiceEvent)>) {
        match self {
            Self::Mono(t) => {
                if let Some(e) = t.reset() {
                    out.push((at, at, e));
                }
            }
            Self::Poly(t) => t.reset(|e| out.push((at, at, e))),
        }
    }
}

/// The tracker's events over `x`, reset at the end.
fn listen(x: &[f32], config: VoiceConfig, mode: Mode) -> Vec<(i64, i64, VoiceEvent)> {
    let mut out = Vec::new();
    let mut t = Tracker::new(mode, config);
    t.listen(x, 0, &mut out);
    t.reset(x.len() as i64, &mut out);
    out
}

/// How a case is heard: the default settings of a responsiveness, or
/// calibrated first (the room alone for three seconds, then the performer
/// playing the piece once, as Preferences → MIDI → Calibrate does).
#[derive(Clone, Copy, Debug)]
enum Setup {
    Default(Responsiveness),
    Calibrated(Responsiveness),
}

impl Setup {
    fn config(self, piece: &Piece, level: f64, room: Room, mode: Mode, seed: u64) -> VoiceConfig {
        match self {
            Setup::Default(r) => VoiceConfig::default().with(r),
            Setup::Calibrated(r) => {
                let base = VoiceConfig::default().with(r);
                let meter = Arc::new(Meter::new());
                let mut t = Tracker::new(mode, base).metered(meter.clone());
                let mut out = Vec::new();
                t.listen(&signals::room(3.0, level, room, seed + 1), 0, &mut out);
                let quiet = meter.take();
                t.listen(&at_level(piece, level, room, seed + 2), 0, &mut out);
                let played = meter.take();
                match calibrate(&quiet, &played) {
                    Ok(c) => VoiceConfig {
                        threshold_db: c.threshold_db,
                        dynamics: Some(c.dynamics),
                        ..base
                    },
                    Err(e) => {
                        eprintln!("{} @ {level} dB, {}: {e:?}", piece.name, room.label());
                        base
                    }
                }
            }
        }
    }
}

/// One case: a piece at a level in a room.
struct Case {
    piece: usize,
    level: f64,
    room: Room,
    tally: Tally,
}

fn measure(
    pieces: &[Piece],
    mode: Mode,
    levels: &[f64],
    rooms: &[Room],
    setup: Setup,
) -> Vec<Case> {
    let mut cases = Vec::new();
    for (p, piece) in pieces.iter().enumerate() {
        for &level in levels {
            for &room in rooms {
                let seed = p as u64 * 31 + 7;
                let x = at_level(piece, level, room, seed);
                let config = setup.config(piece, level, room, mode, seed);
                let notes = heard(&listen(&x, config, mode), x.len());
                cases.push(Case {
                    piece: p,
                    level,
                    room,
                    tally: score(&piece.truth, &notes, mode.tolerance()),
                });
            }
        }
    }
    cases
}

fn sum<'a>(cases: impl Iterator<Item = &'a Case>) -> Tally {
    let mut t = Tally::default();
    for c in cases {
        t.add(&c.tally);
    }
    t
}

fn print_table(title: &str, pieces: &[Piece], cases: &[Case]) {
    eprintln!("\n=== {title}");
    eprintln!("{}", Tally::HEADER);
    for (p, piece) in pieces.iter().enumerate() {
        for &level in &LEVELS {
            let t = sum(cases.iter().filter(|c| c.piece == p && c.level == level));
            eprintln!("{}", t.row(&format!("{} @ {level:.0} dB", piece.name)));
        }
    }
    eprintln!("{}", "-".repeat(Tally::HEADER.len()));
    for &level in &LEVELS {
        let t = sum(cases.iter().filter(|c| c.level == level));
        eprintln!("{}", t.row(&format!("all @ {level:.0} dBFS")));
    }
    for room in Room::ALL {
        let t = sum(cases.iter().filter(|c| c.room == room));
        eprintln!("{}", t.row(&format!("all, {}", room.label())));
    }
    eprintln!("{}", sum(cases.iter()).row("all"));
}

fn setups() -> Vec<Setup> {
    let responses = match std::env::var("VOICE_RESPONSE").as_deref() {
        Ok("fast") => vec![Responsiveness::Fast],
        Ok("stable") => vec![Responsiveness::Stable],
        Ok("all") => Responsiveness::ALL.to_vec(),
        _ => vec![Responsiveness::Balanced],
    };
    let mut out: Vec<Setup> = responses.iter().map(|r| Setup::Default(*r)).collect();
    if std::env::var("VOICE_CALIBRATED").is_ok() {
        out.extend(responses.iter().map(|r| Setup::Calibrated(*r)));
    }
    out
}

#[test]
#[ignore = "the corpus table: run with --release --nocapture"]
fn voice_corpus_table() {
    let which = std::env::var("VOICE_CORPUS").unwrap_or_default();
    for setup in setups() {
        if which != "poly" {
            let pieces = mono_pieces();
            let cases = measure(&pieces, Mode::Mono, &LEVELS, &Room::ALL, setup);
            print_table(&format!("monophonic, {setup:?}"), &pieces, &cases);
        }
        if which != "mono" {
            let pieces = poly_pieces();
            let cases = measure(&pieces, Mode::Poly, &LEVELS, &Room::ALL, setup);
            print_table(&format!("polyphonic, {setup:?}"), &pieces, &cases);
        }
    }
}

/// Part of the corpus, quiet and loud, in a quiet room and with hum: the
/// accuracy reached holds (the whole table at Balanced: F1 79 %
/// monophonic, 68 % polyphonic, from 47 % and 52 % before levels were
/// followed).
#[test]
fn voice_corpus_holds_its_accuracy() {
    let setup = Setup::Default(Responsiveness::Balanced);
    let some = |pieces: Vec<Piece>, names: &[&str]| -> Vec<Piece> {
        pieces
            .into_iter()
            .filter(|p| names.contains(&p.name))
            .collect()
    };
    let mono = some(mono_pieces(), &["sung, man", "whistle"]);
    let cases = measure(
        &mono,
        Mode::Mono,
        &[-40.0, -12.0],
        &[Room::Quiet, Room::Hum],
        setup,
    );
    let t = sum(cases.iter());
    eprintln!("{}\n{}", Tally::HEADER, t.row("monophonic"));
    assert!(t.f1() >= 0.80, "monophonic F1 {:.2}", t.f1());
    assert!(t.recall() >= 0.85, "monophonic recall {:.2}", t.recall());
    assert!(
        t.in_silence <= 2,
        "monophonic notes in silence: {}",
        t.in_silence
    );
    // Velocities follow the performer, not the input's gain.
    let quiet = sum(cases.iter().filter(|c| c.level == -40.0));
    let loud = sum(cases.iter().filter(|c| c.level == -12.0));
    assert!(
        (quiet.velocity_median() - loud.velocity_median()).abs() <= 12.0,
        "velocities {} at −40 dB, {} at −12 dB",
        quiet.velocity_median(),
        loud.velocity_median()
    );
    let poly = some(poly_pieces(), &["strummed chords", "piano chords"]);
    let cases = measure(&poly, Mode::Poly, &[-40.0, -12.0], &[Room::Quiet], setup);
    let t = sum(cases.iter());
    eprintln!("{}", t.row("polyphonic"));
    assert!(t.f1() >= 0.64, "polyphonic F1 {:.2}", t.f1());
    assert!(t.recall() >= 0.58, "polyphonic recall {:.2}", t.recall());
    assert!(
        t.in_silence == 0,
        "polyphonic notes in silence: {}",
        t.in_silence
    );
}

/// One case note by note: `VOICE_DUMP=<piece name>@<level>@<quiet|noisy|hum>`
/// (`VOICE_CORPUS=poly` for the polyphonic pieces).
#[test]
#[ignore = "a diagnostic"]
fn voice_corpus_dump() {
    let Ok(spec) = std::env::var("VOICE_DUMP") else {
        return;
    };
    let mut parts = spec.split('@');
    let name = parts.next().unwrap();
    let level: f64 = parts.next().map_or(-12.0, |l| l.parse().unwrap());
    let room = match parts.next() {
        Some("noisy") => Room::Noisy,
        Some("hum") => Room::Hum,
        _ => Room::Quiet,
    };
    let mode = if std::env::var("VOICE_CORPUS").as_deref() == Ok("poly") {
        Mode::Poly
    } else {
        Mode::Mono
    };
    let pieces = if mode == Mode::Poly {
        poly_pieces()
    } else {
        mono_pieces()
    };
    let (p, piece) = pieces
        .iter()
        .enumerate()
        .find(|(_, p)| p.name == name)
        .unwrap();
    for setup in setups() {
        let seed = p as u64 * 31 + 7;
        let x = at_level(piece, level, room, seed);
        let config = setup.config(piece, level, room, mode, seed);
        let notes = heard(&listen(&x, config, mode), x.len());
        eprintln!(
            "--- {name} @ {level} dB, {} ({setup:?}: {config:?})",
            room.label()
        );
        for n in &piece.truth {
            eprintln!("truth {:>3} {:.3}–{:.3}", n.key, n.on, n.off);
        }
        for h in &notes {
            eprintln!(
                "heard {:>3} {:.3}–{:.3} (decided {:.3}) v{}",
                h.key, h.at, h.end, h.decided, h.velocity
            );
        }
        eprintln!("{}", Tally::HEADER);
        eprintln!(
            "{}",
            score(&piece.truth, &notes, mode.tolerance()).row(name)
        );
    }
}

/// What listening costs on the audio thread: every piece at −12 dB in a
/// noisy room, in 64-frame callbacks (run in release mode).
#[test]
#[ignore = "a measurement: run with --release --nocapture"]
fn voice_corpus_cost() {
    for (mode, pieces) in [(Mode::Mono, mono_pieces()), (Mode::Poly, poly_pieces())] {
        for r in Responsiveness::ALL {
            let mut times = Vec::new();
            for (p, piece) in pieces.iter().enumerate() {
                let x = at_level(piece, -12.0, Room::Noisy, p as u64);
                let mut t = Tracker::new(mode, VoiceConfig::default().with(r));
                let mut out = Vec::with_capacity(4096);
                for chunk in x.chunks(64) {
                    let start = std::time::Instant::now();
                    t.listen(chunk, 0, &mut out);
                    times.push(start.elapsed().as_secs_f64() * 1e6);
                }
            }
            times.sort_by(f64::total_cmp);
            let mean = times.iter().sum::<f64>() / times.len() as f64;
            eprintln!(
                "{mode:?} {r:?}: mean {mean:.1} µs, p99 {:.1} µs, max {:.1} µs per 64 frames ({:.0} µs of audio)",
                times[times.len() * 99 / 100],
                times[times.len() - 1],
                64e6 / RATE
            );
        }
    }
}
