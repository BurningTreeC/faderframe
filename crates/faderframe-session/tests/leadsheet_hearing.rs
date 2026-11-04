//! Lead sheets from polyphonic recordings, measured against what was
//! played: synthetic recordings whose every note is known — a piano-like
//! instrument playing block chords with a melody on top, a voice-like line
//! (a glottal saw through "ah" formants, scooped, with vibrato, a little
//! off pitch) over sustained chords, and a plucked guitar-like arpeggio —
//! heard and written, then scored:
//!
//! * note F1 of what was heard (onset within 50 ms, pitch exact), every
//!   note and the melody's;
//! * melody accuracy: the share of the melody's notes written where they
//!   were played (onset on the grid) at their pitch, and the share of the
//!   written notes that are right;
//! * chord accuracy: the share of beats whose chord symbol has the right
//!   root and quality (major or minor, MIREX's "majmin");
//! * the sheet: values shorter than the music has, ties and accidentals a
//!   bar;
//! * written as played: the share of every note written where it was
//!   played (note F1 against the sheet's notes, every staff).
//!
//! Each is also scored as the pipeline this replaced heard it: the
//! monophonic pitch tracker's notes and basic-pitch's raw notes.
//! `cargo test -p faderframe-session --test leadsheet_hearing -- --nocapture`
//! prints the scores; with `FADERFRAME_HEARING_DUMP=1` also basic-pitch's
//! raw notes, the notes heard (the melody's marked), each bar's chords and
//! the bars written as played.
#![allow(clippy::unwrap_used)]

use faderframe_audio_files::{WavFormat, write_wav};
use faderframe_engine::EngineConfig;
use faderframe_leadsheet::LeadSheet;
use faderframe_midi::theory::{Chord, Quality};
use faderframe_project::Project;
use faderframe_session::leadsheet::{Grid, LeadSheetOf, Notation, SheetStyle};
use faderframe_session::{Action, Session};
use faderframe_timeline::MusicalTime;
use std::f64::consts::TAU;

const SR: u32 = 48_000;
/// The project's tempo: a quarter is half a second.
const QUARTER: f64 = 0.5;

fn hz(key: f64) -> f64 {
    440.0 * 2f64.powf((key - 69.0) / 12.0)
}

/// A played note: quarters from the clip's start, its key.
#[derive(Clone, Copy, Debug)]
struct Played {
    start: f64,
    end: f64,
    key: u8,
}

/// What a recording plays: every note, the melody's, the chords (from a
/// quarter on).
struct Truth {
    notes: Vec<Played>,
    melody: Vec<Played>,
    chords: Vec<(f64, Chord)>,
    /// Quarters long.
    length: f64,
    /// The shortest value the melody has (quarters).
    finest: f64,
}

/// A piano-like tone: slightly inharmonic partials, each decaying (the
/// higher the faster), a hammer's attack, a damper's release.
fn piano(x: &mut [f32], key: u8, a: f64, b: f64, gain: f64) {
    let rate = f64::from(SR);
    let f0 = hz(f64::from(key));
    let inharmonic = 0.0004;
    let tau0 = 2.5 * (110.0 / f0).powf(0.4);
    let partials: Vec<(f64, f64, f64, f64)> = (1..=20)
        .map(|h| {
            let h = f64::from(h);
            let f = f0 * h * (1.0 + inharmonic * h * h).sqrt();
            (
                f,
                1.0 / h.powf(1.2),
                tau0 / (1.0 + 0.4 * (h - 1.0)),
                h * 1.3,
            )
        })
        .filter(|p| p.0 < 9_000.0)
        .collect();
    let release = 0.08;
    let from = (a * rate) as usize;
    let to = (((b + 5.0 * release) * rate) as usize).min(x.len());
    for (i, out) in x.iter_mut().enumerate().take(to).skip(from) {
        let t = i as f64 / rate - a;
        let attack = (t / 0.004).min(1.0);
        let damper = if t > b - a {
            (-(t - (b - a)) / release).exp()
        } else {
            1.0
        };
        let v: f64 = partials
            .iter()
            .map(|(f, amp, tau, phase)| amp * (-t / tau).exp() * (TAU * f * t + phase).sin())
            .sum();
        *out += (v * attack * damper * gain) as f32;
    }
}

/// A sung note: a glottal saw (−12 dB an octave) through the formants of
/// "ah", scooped up from below at its start, vibrato on held notes, `cents`
/// off its key.
fn sung(x: &mut [f32], key: u8, a: f64, b: f64, gain: f64, cents: f64, scoop: f64) {
    let rate = f64::from(SR);
    let formants = [
        (730.0, 90.0, 1.0),
        (1090.0, 110.0, 0.5),
        (2440.0, 170.0, 0.25),
    ];
    let shape = |f: f64| -> f64 {
        formants
            .iter()
            .map(|(fc, bw, g)| g / (1.0 + ((f - fc) / (bw / 2.0)).powi(2)).sqrt())
            .sum::<f64>()
            + 0.02
    };
    let from = (a * rate) as usize;
    let to = ((b * rate) as usize).min(x.len());
    let mut phase = 0.0f64;
    let base = f64::from(key) + cents / 100.0;
    for (i, out) in x.iter_mut().enumerate().take(to).skip(from) {
        let t = i as f64 / rate - a;
        // The scoop: from `scoop` cents below, into the note in 90 ms.
        let glide = scoop / 100.0 * (1.0 - (t / 0.09).min(1.0)).powi(2);
        // Vibrato after 250 ms, ±35 cents at 5.5 Hz, eased in.
        let depth = 0.35 * ((t - 0.25) / 0.2).clamp(0.0, 1.0);
        let vib = depth * (TAU * 5.5 * t).sin();
        let f0 = hz(base - glide + vib);
        phase += TAU * f0 / rate;
        let env = (t / 0.03).min(1.0) * ((b - a - t) / 0.05).clamp(0.0, 1.0);
        let mut v = 0.0;
        let mut h = 1.0;
        while h * f0 < 8_000.0 {
            v += shape(h * f0) / (h * h) * (h * phase).sin();
            h += 1.0;
        }
        *out += (v * env * gain) as f32;
    }
}

/// A soft organ-like pad tone (a few harmonics, slow attack).
fn pad(x: &mut [f32], key: u8, a: f64, b: f64, gain: f64) {
    let rate = f64::from(SR);
    let f0 = hz(f64::from(key));
    let from = (a * rate) as usize;
    let to = ((b * rate) as usize).min(x.len());
    for (i, out) in x.iter_mut().enumerate().take(to).skip(from) {
        let t = i as f64 / rate - a;
        let env = (t / 0.04).min(1.0) * ((b - a - t) / 0.06).clamp(0.0, 1.0);
        let v: f64 = (1..=6)
            .map(|h| (TAU * f0 * f64::from(h) * t).sin() / f64::from(h * h))
            .sum();
        *out += (v * env * gain) as f32;
    }
}

/// A plucked string: partials shaped by the pluck's place, the higher
/// dying faster, ringing for `ring` seconds.
fn plucked(x: &mut [f32], key: u8, a: f64, ring: f64, gain: f64) {
    let rate = f64::from(SR);
    let f0 = hz(f64::from(key));
    let place = 0.18;
    let partials: Vec<(f64, f64, f64)> = (1..=24)
        .map(|h| {
            let h = f64::from(h);
            let amp = (std::f64::consts::PI * h * place).sin().abs() / h;
            (f0 * h, amp, 1.2 / (1.0 + 0.5 * (h - 1.0)))
        })
        .filter(|p| p.0 < 9_000.0)
        .collect();
    let from = (a * rate) as usize;
    let to = (((a + ring + 0.3) * rate) as usize).min(x.len());
    for (i, out) in x.iter_mut().enumerate().take(to).skip(from) {
        let t = i as f64 / rate - a;
        let damp = if t > ring {
            (-(t - ring) / 0.05).exp()
        } else {
            1.0
        };
        let v: f64 = partials
            .iter()
            .map(|(f, amp, tau)| amp * (-t / tau).exp() * (TAU * f * t).sin())
            .sum();
        *out += (v * damp * (t / 0.002).min(1.0) * gain) as f32;
    }
}

fn chord(name: &str) -> Chord {
    Chord::parse(name).unwrap()
}

/// Notes `(start, length, key)` in quarters.
fn played(notes: &[(f64, f64, u8)]) -> Vec<Played> {
    notes
        .iter()
        .map(|&(s, l, key)| Played {
            start: s,
            end: s + l,
            key,
        })
        .collect()
}

/// Block chords a bar (two half notes) under a melody on top.
fn piano_piece() -> (Vec<f32>, Truth) {
    let bars: [(&str, &[u8]); 8] = [
        ("C", &[48, 52, 55]),
        ("Am", &[45, 48, 52]),
        ("F", &[41, 45, 48]),
        ("G", &[43, 47, 50]),
        ("Em", &[40, 43, 47]),
        ("Am", &[45, 48, 52]),
        ("Dm", &[50, 53, 57]),
        ("C", &[48, 52, 55]),
    ];
    let melody = played(&[
        // C
        (0.0, 1.0, 76),
        (1.0, 1.0, 79),
        (2.0, 1.0, 76),
        (3.0, 1.0, 72),
        // Am
        (4.0, 2.0, 81),
        (6.0, 0.5, 79),
        (6.5, 0.5, 76),
        (7.0, 1.0, 72),
        // F
        (8.0, 1.0, 77),
        (9.0, 1.0, 81),
        (10.0, 2.0, 84),
        // G
        (12.0, 1.0, 83),
        (13.0, 0.5, 81),
        (13.5, 0.5, 79),
        (14.0, 2.0, 74),
        // Em
        (16.0, 1.0, 79),
        (17.0, 1.0, 76),
        (18.0, 1.0, 71),
        (19.0, 1.0, 76),
        // Am
        (20.0, 1.5, 81),
        (21.5, 0.5, 79),
        (22.0, 1.0, 76),
        (23.0, 1.0, 72),
        // Dm
        (24.0, 1.0, 77),
        (25.0, 1.0, 74),
        (26.0, 1.0, 69),
        (27.0, 1.0, 74),
        // C
        (28.0, 4.0, 72),
    ]);
    let mut x = vec![0.0f32; ((32.0 * QUARTER + 1.0) * f64::from(SR)) as usize];
    let mut notes = melody.clone();
    let mut chords = Vec::new();
    for (i, (name, keys)) in bars.iter().enumerate() {
        let at = 4.0 * i as f64;
        chords.push((at, chord(name)));
        for half in [0.0, 2.0] {
            for &k in keys.iter() {
                piano(
                    &mut x,
                    k,
                    (at + half) * QUARTER,
                    (at + half + 1.9) * QUARTER,
                    0.05,
                );
                notes.push(Played {
                    start: at + half,
                    end: at + half + 2.0,
                    key: k,
                });
            }
        }
    }
    for n in &melody {
        piano(
            &mut x,
            n.key,
            n.start * QUARTER,
            (n.end - 0.05) * QUARTER,
            0.11,
        );
    }
    (
        x,
        Truth {
            notes,
            melody,
            chords,
            length: 32.0,
            finest: 0.5,
        },
    )
}

/// A sung line over sustained chords (the chords' top often in the
/// voice's range, once above it).
fn voice_piece() -> (Vec<f32>, Truth) {
    let bars: [(&str, &[u8]); 8] = [
        ("C", &[48, 55, 60, 64]),
        ("Am", &[45, 57, 60, 64]),
        ("F", &[41, 57, 60, 65]),
        ("G", &[43, 55, 59, 62]),
        ("C", &[48, 55, 60, 64]),
        ("F", &[41, 57, 60, 65]),
        ("G", &[43, 55, 59, 67]),
        ("C", &[48, 55, 60, 64]),
    ];
    // (start, length, key, cents off, scoop cents)
    let line: [(f64, f64, u8, f64, f64); 21] = [
        (0.0, 1.0, 67, 8.0, 60.0),
        (1.0, 1.0, 72, -10.0, 0.0),
        (2.0, 2.0, 76, 5.0, 80.0),
        (4.0, 1.0, 76, -12.0, 0.0),
        (5.0, 1.0, 72, 6.0, 40.0),
        (6.0, 2.0, 69, -8.0, 0.0),
        (8.0, 1.0, 69, 10.0, 70.0),
        (9.0, 1.0, 72, -5.0, 0.0),
        (10.0, 2.0, 77, 12.0, 90.0),
        (12.0, 1.5, 74, -6.0, 50.0),
        (13.5, 0.5, 72, 4.0, 0.0),
        (14.0, 2.0, 71, -10.0, 0.0),
        (16.0, 1.0, 72, 6.0, 60.0),
        (17.0, 1.0, 76, -4.0, 0.0),
        (18.0, 2.0, 79, 10.0, 70.0),
        (20.0, 1.0, 77, -8.0, 0.0),
        (21.0, 1.0, 76, 5.0, 0.0),
        (22.0, 2.0, 74, -6.0, 40.0),
        (24.0, 1.0, 74, 8.0, 0.0),
        (25.0, 1.0, 71, -10.0, 0.0),
        (26.0, 6.0 - 0.0, 72, 6.0, 80.0),
    ];
    let mut x = vec![0.0f32; ((32.0 * QUARTER + 1.0) * f64::from(SR)) as usize];
    let mut notes = Vec::new();
    let mut chords = Vec::new();
    for (i, (name, keys)) in bars.iter().enumerate() {
        let at = 4.0 * i as f64;
        chords.push((at, chord(name)));
        for &k in keys.iter() {
            pad(&mut x, k, at * QUARTER, (at + 3.95) * QUARTER, 0.045);
            notes.push(Played {
                start: at,
                end: at + 4.0,
                key: k,
            });
        }
    }
    let mut melody = Vec::new();
    for &(s, l, key, cents, scoop) in &line {
        sung(
            &mut x,
            key,
            s * QUARTER,
            (s + l - 0.08) * QUARTER,
            0.16,
            cents,
            scoop,
        );
        melody.push(Played {
            start: s,
            end: s + l,
            key,
        });
    }
    notes.extend(melody.iter().copied());
    (
        x,
        Truth {
            notes,
            melody,
            chords,
            length: 32.0,
            finest: 0.5,
        },
    )
}

/// A plucked arpeggio in eighths, each string left ringing.
fn guitar_piece() -> (Vec<f32>, Truth) {
    let bars: [(&str, [u8; 8]); 8] = [
        ("C", [48, 52, 55, 60, 64, 60, 55, 52]),
        ("Am", [45, 52, 57, 60, 64, 60, 57, 52]),
        ("F", [41, 48, 53, 57, 60, 57, 53, 48]),
        ("G", [43, 47, 50, 55, 59, 55, 50, 47]),
        ("C", [48, 52, 55, 60, 64, 60, 55, 52]),
        ("Am", [45, 52, 57, 60, 64, 60, 57, 52]),
        ("F", [41, 48, 53, 57, 60, 57, 53, 48]),
        ("G", [43, 47, 50, 55, 59, 55, 50, 47]),
    ];
    let mut x = vec![0.0f32; ((32.0 * QUARTER + 1.5) * f64::from(SR)) as usize];
    let mut notes = Vec::new();
    let mut chords = Vec::new();
    for (i, (name, keys)) in bars.iter().enumerate() {
        let at = 4.0 * i as f64;
        chords.push((at, chord(name)));
        for (j, &k) in keys.iter().enumerate() {
            let s = at + 0.5 * j as f64;
            plucked(&mut x, k, s * QUARTER, 0.7, 0.09);
            notes.push(Played {
                start: s,
                end: s + 0.5,
                key: k,
            });
        }
    }
    let melody = notes.clone();
    (
        x,
        Truth {
            notes,
            melody,
            chords,
            length: 32.0,
            finest: 0.5,
        },
    )
}

/// Note F1: found notes matched one to one to played ones with the same
/// key and an onset within `tolerance` quarters.
fn f1(found: &[Played], truth: &[Played], tolerance: f64) -> (f64, f64, f64) {
    let mut used = vec![false; found.len()];
    let mut hits = 0usize;
    for t in truth {
        let best = found
            .iter()
            .enumerate()
            .filter(|(i, f)| !used[*i] && f.key == t.key && (f.start - t.start).abs() <= tolerance)
            .min_by(|a, b| {
                (a.1.start - t.start)
                    .abs()
                    .total_cmp(&(b.1.start - t.start).abs())
            })
            .map(|(i, _)| i);
        if let Some(i) = best {
            used[i] = true;
            hits += 1;
        }
    }
    let p = hits as f64 / found.len().max(1) as f64;
    let r = hits as f64 / truth.len().max(1) as f64;
    let f = if p + r > 0.0 {
        2.0 * p * r / (p + r)
    } else {
        0.0
    };
    (p, r, f)
}

/// A written part's notes (each chord note; a tied note once) in quarters
/// from the first bar.
fn written(sheet: &LeadSheet, part: usize) -> Vec<Played> {
    let mut out = Vec::new();
    let mut at = 0.0;
    for m in &sheet.parts[part].measures {
        for e in &m.events {
            if !e.tied_from {
                for p in e.pitches() {
                    out.push(Played {
                        start: at + f64::from(e.at) / 12.0,
                        end: at + f64::from(e.end()) / 12.0,
                        key: p.midi,
                    });
                }
            }
        }
        at += f64::from(m.length()) / 12.0;
    }
    out
}

/// MIREX's majmin class of a quality: 0 major, 1 minor, 2 neither.
fn majmin(q: Quality) -> u8 {
    use Quality::*;
    match q {
        Major | Major6 | Major7 | Dominant7 | Add9 | Dominant9 | Major9 | Dominant11
        | Dominant13 => 0,
        Minor | Minor6 | Minor7 | MinorMajor7 | MinorAdd9 | Minor9 => 1,
        _ => 2,
    }
}

/// The share of beats whose written chord (the last symbol at or before
/// it, over the top part) has the played chord's root and majmin class.
fn chord_accuracy(sheet: &LeadSheet, truth: &Truth) -> f64 {
    let mut written: Vec<(f64, u8, u8)> = Vec::new();
    let mut at = 0.0;
    for m in &sheet.parts[0].measures {
        for c in &m.chords {
            let pc = [0u8, 2, 4, 5, 7, 9, 11][usize::from(c.root.0)] as i32 + i32::from(c.root.1);
            let q = Quality::ALL
                .iter()
                .copied()
                .find(|q| q.symbol().replace('b', "♭").replace('#', "♯") == c.suffix)
                .unwrap_or(Quality::Power);
            written.push((
                at + f64::from(c.at) / 12.0,
                pc.rem_euclid(12) as u8,
                majmin(q),
            ));
        }
        at += f64::from(m.length()) / 12.0;
    }
    let beats = truth.length as usize;
    let mut right = 0;
    for b in 0..beats {
        let t = b as f64 + 0.01;
        let want = truth
            .chords
            .iter()
            .rev()
            .find(|(s, _)| *s <= t)
            .map(|(_, c)| (c.root, majmin(c.quality)));
        let got = written
            .iter()
            .rev()
            .find(|(s, _, _)| *s <= t)
            .map(|(_, r, q)| (*r, *q));
        if want.is_some() && want == got {
            right += 1;
        }
    }
    right as f64 / beats.max(1) as f64
}

/// The sheet's hygiene: written values shorter than `finest` quarters, the
/// most ties in a bar and on average, accidentals a bar.
fn hygiene(sheet: &LeadSheet, part: usize, finest: f64) -> (usize, usize, f64, f64) {
    let p = &sheet.parts[part];
    let mut short = 0;
    let mut most_ties = 0;
    let (mut ties, mut accidentals) = (0usize, 0usize);
    for m in &p.measures {
        let mut t = 0;
        for e in &m.events {
            if f64::from(e.duration.divisions()) / 12.0 < finest - 1e-9 && !e.bar_rest {
                short += 1;
            }
            if e.tie {
                t += 1;
            }
            accidentals += e.pitches().filter(|p| p.accidental.is_some()).count();
        }
        most_ties = most_ties.max(t);
        ties += t;
    }
    let bars = p.measures.len().max(1) as f64;
    (
        short,
        most_ties,
        ties as f64 / bars,
        accidentals as f64 / bars,
    )
}

/// The monophonic tracker's notes (what lead sheets heard a staff with
/// before), in quarters.
fn monophonic(x: &[f32]) -> Vec<Played> {
    use faderframe_analysis::melody;
    let tr = melody::track(x, f64::from(SR));
    melody::notes(&tr)
        .into_iter()
        .map(|n| Played {
            start: (n.start * tr.hop) as f64 / f64::from(SR) / QUARTER,
            end: (n.end * tr.hop) as f64 / f64::from(SR) / QUARTER,
            key: n.pitch.round() as u8,
        })
        .collect()
}

/// basic-pitch's notes as they come (what fretted staves and the chords
/// were heard with before), in quarters.
fn transcribed(x: &[f32]) -> Vec<Played> {
    let mut r = faderframe_audio_files::resample::StreamResampler::new(SR, 22_050, 1).unwrap();
    let mut audio = Vec::new();
    let mut sink = |planes: &[&[f32]], n: usize| {
        audio.extend_from_slice(&planes[0][..n]);
        Ok(())
    };
    r.push(&[x.to_vec()], x.len(), &mut sink).unwrap();
    r.finish(&mut sink).unwrap();
    faderframe_transcribe::transcribe(&audio, &faderframe_transcribe::Settings::default(), 4)
        .unwrap()
        .into_iter()
        .map(|n| Played {
            start: n.start / QUARTER,
            end: n.end / QUARTER,
            key: n.key,
        })
        .collect()
}

/// A session with `x` recorded into one clip at the start (on a track
/// named `name`); the sheet made of it.
fn sheet_of(name: &str, x: &[f32]) -> (Session, LeadSheet) {
    let dir = std::env::temp_dir().join(format!("ff-lead-hearing-{}-{name}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join(format!("{name}.wav"));
    write_wav(&file, &[x.to_vec()], SR, WavFormat::Float32, false).unwrap();
    let mut s = Session::new(Project::new(name, SR), None, EngineConfig::default()).unwrap();
    s.dispatch(Action::ImportFiles {
        files: vec![file],
        track: None,
        at: MusicalTime::ZERO,
    })
    .unwrap();
    s.wait_for_imports();
    let clip = *s.project().clips.keys().next().unwrap();
    let track = s.project().clip(clip).unwrap().track;
    s.dispatch(Action::Edit(faderframe_project::Command::RenameTrack {
        track,
        name: name.into(),
    }))
    .unwrap();
    s.dispatch(Action::MakeLeadSheet {
        of: LeadSheetOf::Clip(clip),
        grid: Grid::Auto,
    })
    .unwrap();
    s.wait_for_lead_sheet();
    let sheet = s.lead_sheet().unwrap().sheet.clone();
    let _ = std::fs::remove_dir_all(&dir);
    (s, sheet)
}

/// What a scenario scored (the fields not asserted on are read through
/// `Debug`, printed).
#[derive(Debug, Default)]
#[allow(dead_code)]
struct Scores {
    heard_f1: f64,
    heard_f1_old_mono: f64,
    heard_f1_old_raw: f64,
    melody_f1: f64,
    melody_f1_old: f64,
    melody_accuracy: f64,
    melody_precision: f64,
    chords: f64,
    short_values: usize,
    most_ties: usize,
    ties: f64,
    accidentals: f64,
    as_played_f1: f64,
    as_played_parts: usize,
    as_played_short: usize,
    as_played_most_ties: usize,
}

fn score(name: &str, x: &[f32], truth: &Truth) -> Scores {
    let tol = 0.05 / QUARTER;
    let mono = monophonic(x);
    let raw = transcribed(x);
    if std::env::var_os("FADERFRAME_HEARING_DUMP").is_some() {
        for n in &raw {
            println!("{name} raw {:7.3}–{:7.3} {:3}", n.start, n.end, n.key);
        }
    }
    let (s, sheet) = sheet_of(name, x);
    let heard: Vec<Played> = s
        .lead_sheet_notes()
        .into_iter()
        .flatten()
        .map(|v| Played {
            start: v.start,
            end: v.end,
            key: v.key,
        })
        .collect();
    let sung: Vec<Played> = s
        .lead_sheet_notes()
        .into_iter()
        .flatten()
        .filter(|v| v.melody)
        .map(|v| Played {
            start: v.start,
            end: v.end,
            key: v.key,
        })
        .collect();
    if std::env::var_os("FADERFRAME_HEARING_DUMP").is_some() {
        for v in s.lead_sheet_notes().into_iter().flatten() {
            println!(
                "{name} heard {:7.3}–{:7.3} {:3} {:.2}{}",
                v.start,
                v.end,
                v.key,
                v.level,
                if v.melody { " melody" } else { "" }
            );
        }
        for m in &sheet.parts[0].measures {
            println!(
                "{name} bar chords {:?}",
                m.chords.iter().map(|c| c.text()).collect::<Vec<_>>()
            );
        }
    }
    let mel = written(&sheet, 0);
    let (precision, accuracy, _) = f1(&mel, &truth.melody, 1e-6);
    let (short, most_ties, ties, accidentals) = hygiene(&sheet, 0, truth.finest);
    // Every note, as played.
    let mut session = s;
    session
        .dispatch(Action::SetSheetStyle(SheetStyle {
            notation: Notation::AsPlayed,
            ..session.sheet_style()
        }))
        .unwrap();
    assert!(
        !session.making_lead_sheet(),
        "written again without listening"
    );
    let played_sheet = session.lead_sheet().unwrap().sheet.clone();
    let all: Vec<Played> = (0..played_sheet.parts.len())
        .flat_map(|i| written(&played_sheet, i))
        .collect();
    if std::env::var_os("FADERFRAME_HEARING_DUMP").is_some() {
        for (pi, part) in played_sheet.parts.iter().enumerate() {
            for (mi, m) in part.measures.iter().enumerate() {
                let ev: Vec<String> = m
                    .events
                    .iter()
                    .map(|e| {
                        format!(
                            "{}{}:{:?}",
                            e.at,
                            if e.is_rest() { "r" } else { "" },
                            e.pitches().map(|p| p.midi).collect::<Vec<_>>()
                        ) + &format!("/{}", e.duration.divisions())
                    })
                    .collect();
                println!("{name} played part {pi} bar {mi}: {}", ev.join(" "));
            }
        }
    }
    let (mut played_short, mut played_ties) = (0, 0);
    for i in 0..played_sheet.parts.len() {
        let (short, most, _, _) = hygiene(&played_sheet, i, truth.finest);
        played_short += short;
        played_ties = played_ties.max(most);
    }
    let s = Scores {
        as_played_f1: f1(&all, &truth.notes, 1e-6).2,
        as_played_parts: played_sheet.parts.len(),
        as_played_short: played_short,
        as_played_most_ties: played_ties,
        heard_f1: f1(&heard, &truth.notes, tol).2,
        melody_f1: f1(&sung, &truth.melody, tol).2,
        heard_f1_old_mono: f1(&mono, &truth.notes, tol).2,
        heard_f1_old_raw: f1(&raw, &truth.notes, tol).2,
        melody_f1_old: f1(&mono, &truth.melody, tol).2,
        melody_accuracy: accuracy,
        melody_precision: precision,
        chords: chord_accuracy(&sheet, truth),
        short_values: short,
        most_ties,
        ties,
        accidentals,
    };
    println!("{name}: {s:#?}");
    s
}

#[test]
fn polyphonic_recordings_become_clean_lead_sheets() {
    let (x, truth) = piano_piece();
    let piano = score("piano", &x, &truth);
    let (x, truth) = voice_piece();
    let voice = score("voice", &x, &truth);
    let (x, truth) = guitar_piece();
    let guitar = score("guitar", &x, &truth);
    // Thresholds the monophonic pipeline fails (it scored melody
    // accuracy 0.21, 0.00 and 0.03, chords 0, heard F1 0.11, 0.00 and
    // 0.04, and wrote 71, 29 and 11 values shorter than the music has).
    for s in [&piano, &voice, &guitar] {
        assert!(s.heard_f1 >= 0.8, "{s:?}");
        assert!(s.chords >= 0.85, "{s:?}");
        assert!(
            s.heard_f1 > s.heard_f1_old_raw,
            "cleaner than the raw transcription: {s:?}"
        );
    }
    assert!(
        piano.melody_accuracy >= 0.9 && piano.melody_precision >= 0.9,
        "{piano:?}"
    );
    assert!(
        voice.melody_accuracy >= 0.85 && voice.melody_precision >= 0.85,
        "{voice:?}"
    );
    for s in [&piano, &voice] {
        assert_eq!(s.short_values, 0, "{s:?}");
        assert!(s.most_ties <= 2 && s.accidentals < 0.25, "{s:?}");
    }
    // As played: the piano on a grand staff, every note where it was.
    assert_eq!(piano.as_played_parts, 2, "{piano:?}");
    assert_eq!(guitar.as_played_parts, 1, "{guitar:?}");
    for s in [&piano, &guitar] {
        assert!(s.as_played_f1 >= 0.75, "{s:?}");
        assert_eq!(s.as_played_short, 0, "{s:?}");
        assert!(s.as_played_most_ties <= 3, "{s:?}");
    }
}
