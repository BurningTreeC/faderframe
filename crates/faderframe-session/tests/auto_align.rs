//! Auto-Align: a drum kit's microphones (kick, snare bottom, overheads,
//! room) recorded at their distances, found to a fraction of a sample,
//! set as the tracks' own delays and polarities in one undo step — and
//! then heard together (the master's sum gains what an ideal alignment
//! does); unrelated tracks are left alone; the delays are saved.
#![allow(clippy::unwrap_used)]

use faderframe_analysis::fft;
use faderframe_audio_files::{WavFormat, write_wav};
use faderframe_core::TrackId;
use faderframe_engine::EngineConfig;
use faderframe_engine::offline::OfflineRenderer;
use faderframe_project::{Project, TrackKind};
use faderframe_session::align::AlignOp;
use faderframe_session::{Action, SelectMode, Session, UiRequest};
use faderframe_timeline::MusicalTime;
use std::f64::consts::PI;
use std::path::PathBuf;

const SR: u32 = 48_000;
const RATE: f64 = SR as f64;
const SECONDS: f64 = 6.0;

struct Noise(u64);

impl Noise {
    fn next(&mut self) -> f64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        (self.0 >> 11) as f64 / (1u64 << 53) as f64 * 2.0 - 1.0
    }
}

/// Kicks, snares and hats with silence between.
fn drums(seed: u64) -> Vec<f64> {
    let len = (SECONDS * RATE) as usize;
    let mut x = vec![0.0; len];
    let mut noise = Noise(seed);
    let mut t = (0.1 * RATE) as usize;
    let mut i = 0;
    while t < len {
        let vel = 0.6 + 0.4 * noise.next().abs();
        for k in 0..(RATE * 0.3) as usize {
            if t + k >= len {
                break;
            }
            let s = k as f64 / RATE;
            x[t + k] += vel
                * match i % 4 {
                    0 => {
                        (2.0 * PI * (50.0 + 90.0 * (-s * 30.0).exp()) * s).sin() * (-s * 12.0).exp()
                            + 0.3 * noise.next() * (-s * 400.0).exp()
                    }
                    2 => {
                        noise.next() * 0.6 * (-s * 25.0).exp()
                            + 0.4 * (2.0 * PI * 190.0 * s).sin() * (-s * 30.0).exp()
                    }
                    _ => noise.next() * 0.25 * (-s * 60.0).exp(),
                };
        }
        t += (RATE * 0.25) as usize + (noise.next() * 400.0) as usize;
        i += 1;
    }
    x
}

/// `x` `d` samples later (fractional) through `gain(hz)`.
fn shifted(x: &[f64], d: f64, gain: impl Fn(f64) -> f64) -> Vec<f64> {
    let n = (x.len() + 8192).next_power_of_two();
    let (mut re, mut im) = (vec![0.0; n], vec![0.0; n]);
    re[..x.len()].copy_from_slice(x);
    fft(&mut re, &mut im);
    for k in 0..n {
        let signed = if k <= n / 2 {
            k as f64
        } else {
            k as f64 - n as f64
        };
        let w = -2.0 * PI * signed * d / n as f64;
        let g = gain(signed.abs() * RATE / n as f64);
        let (c, s) = (w.cos(), if k == n / 2 { 0.0 } else { w.sin() });
        let (r, i) = (re[k], im[k]);
        re[k] = g * (r * c - i * s);
        im[k] = g * (r * s + i * c);
    }
    im.iter_mut().for_each(|v| *v = -*v);
    fft(&mut re, &mut im);
    re.truncate(x.len());
    re.iter_mut().for_each(|v| *v /= n as f64);
    re
}

/// A microphone: where it hears the source (samples), how loud, which
/// way up, its colour and its room.
struct Mic {
    name: &'static str,
    delay: f64,
    level: f64,
    inverted: bool,
    filter: fn(f64) -> f64,
    reflections: &'static [(f64, f64)],
}

const KIT: [Mic; 4] = [
    Mic {
        name: "Kick In",
        delay: 40.0,
        level: 1.0,
        inverted: false,
        filter: |hz| 1.0 / (1.0 + (hz / 3000.0).powi(2)).sqrt() + 0.3,
        reflections: &[],
    },
    Mic {
        name: "Snare Bottom",
        delay: 51.37,
        level: 0.5,
        inverted: true,
        filter: |hz| (hz / 400.0).min(1.0) * 0.9 + 0.1,
        reflections: &[],
    },
    Mic {
        name: "Overheads",
        delay: 199.62,
        level: 0.35,
        inverted: false,
        filter: |hz| 0.3 + (hz / 2000.0).min(1.0),
        reflections: &[(150.0, 0.45)],
    },
    Mic {
        name: "Room",
        delay: 978.81,
        level: 0.2,
        inverted: false,
        filter: |hz| 1.0 / (1.0 + (hz / 1500.0).powi(2)).sqrt(),
        reflections: &[(210.5, 0.6), (390.25, 0.4)],
    },
];

impl Mic {
    /// What it records, heard at `delay` (its own unless aligning).
    fn record(&self, source: &[f64], delay: f64, seed: u64) -> Vec<f64> {
        let mut y = shifted(source, delay, self.filter);
        for (d, g) in self.reflections {
            let r = shifted(source, delay + d, |hz| {
                (self.filter)(hz) * 0.7f64.powf(hz / 4000.0)
            });
            y.iter_mut().zip(r).for_each(|(a, b)| *a += g * b);
        }
        let mut noise = Noise(seed);
        let sign = if self.inverted { -1.0 } else { 1.0 };
        y.iter()
            .map(|v| sign * self.level * v + 0.0003 * noise.next())
            .collect()
    }
}

fn temp_dir(what: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("ff-auto-align-{what}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// A session with one track per recording, in order.
fn session_with(recordings: &[(&str, Vec<f64>)], dir: &std::path::Path) -> (Session, Vec<TrackId>) {
    let mut s = Session::new(Project::new("Kit", SR), None, EngineConfig::default()).unwrap();
    for (name, audio) in recordings {
        let file = dir.join(format!("{name}.wav"));
        let samples: Vec<f32> = audio.iter().map(|v| *v as f32).collect();
        write_wav(&file, &[samples], SR, WavFormat::Float32, false).unwrap();
        s.dispatch(Action::ImportFiles {
            files: vec![file],
            track: None,
            at: MusicalTime::ZERO,
        })
        .unwrap();
    }
    s.wait_for_imports();
    let tracks: Vec<TrackId> = s
        .project()
        .tracks
        .iter()
        .filter(|t| t.kind == TrackKind::Audio)
        .map(|t| t.id)
        .collect();
    assert_eq!(tracks.len(), recordings.len());
    (s, tracks)
}

/// The master's energy (both sides) over the whole recording.
fn master_energy(s: &Session) -> f64 {
    let p = s.project();
    let mut sources = faderframe_engine::render_generated_sources(p, SR);
    faderframe_session::media::open_file_sources(p, None, &mut sources);
    let config = EngineConfig {
        sample_rate: SR,
        ..EngineConfig::default()
    };
    let mut r = OfflineRenderer::new(p, &sources, config, 1024, 2).unwrap();
    r.play_from(0).unwrap();
    let out = r.render((SECONDS * RATE) as usize + 8192);
    out.iter()
        .flat_map(|c| c.iter())
        .map(|v| f64::from(*v).powi(2))
        .sum()
}

fn db(ratio: f64) -> f64 {
    10.0 * ratio.log10()
}

#[test]
fn a_kit_is_aligned_in_time_and_polarity_in_one_step() {
    let dir = temp_dir("kit");
    let source = drums(7);
    let recordings: Vec<(&str, Vec<f64>)> = KIT
        .iter()
        .enumerate()
        .map(|(i, m)| (m.name, m.record(&source, m.delay, 100 + i as u64)))
        .collect();
    let (mut s, tracks) = session_with(&recordings, &dir);
    s.dispatch(Action::SelectTracks {
        tracks: tracks.clone(),
        mode: SelectMode::Replace,
    })
    .unwrap();
    // The track menus offer it.
    let menu = s.auto_align_menu(tracks[2]);
    assert_eq!(menu[0].label, "Auto-Align Microphones…");
    let before = master_energy(&s);
    let steps = s.history_steps().0.len();
    s.dispatch(menu[0].action.clone().unwrap()).unwrap();
    assert!(s.take_ui_requests().contains(&UiRequest::AutoAlign));
    assert!(s.auto_aligning());
    s.wait_for_auto_align();
    let view = s.auto_align().unwrap();
    assert!(!view.analysing);
    // The whole recording (where every clip plays).
    let (a, b) = view.span.unwrap();
    assert!(a.abs() < 1e-9 && (b - SECONDS).abs() < 0.01, "{a}–{b}");
    // The kick microphone hears it first: the reference.
    assert_eq!(view.effective, Some(tracks[0]));
    for (row, mic) in view.rows.iter().zip(&KIT) {
        let want = mic.delay - KIT[0].delay;
        let lag = row.lag_samples.unwrap();
        assert!((lag - want).abs() < 0.05, "{}: {lag} for {want}", mic.name);
        assert!((row.lag_ms.unwrap() - want / 48.0).abs() < 1e-3);
        assert_eq!(row.inverted, mic.inverted, "{}", mic.name);
        assert!(row.reference || row.confidence > 0.8, "{row:?}");
        assert_eq!(row.include, !row.reference);
    }
    assert!(view.can_apply);
    assert!(
        view.status.contains("3 tracks found against ‘Kick In’"),
        "{}",
        view.status
    );
    // What the window shows.
    let snare = &view.rows[1];
    assert_eq!(snare.lag_text(), "+0.237 ms");
    assert_eq!(snare.samples_text(), "+11.37");
    assert_eq!(snare.polarity_text(), "inverted");
    assert_eq!(view.rows[0].lag_text(), "reference");

    s.dispatch(Action::AutoAlign(AlignOp::Apply)).unwrap();
    assert_eq!(s.history_steps().0.len(), steps + 1, "one undo step");
    assert_eq!(
        s.history_steps().0.last().map(String::as_str),
        Some("Auto-Align Microphones")
    );
    for (id, mic) in tracks.iter().zip(&KIT) {
        let t = s.project().track(*id).unwrap();
        let want = -(mic.delay - KIT[0].delay) / 48.0;
        assert!(
            (t.delay_ms - want).abs() < 0.05 / 48.0,
            "{}: {}",
            mic.name,
            t.delay_ms
        );
        assert_eq!(t.phase_invert, mic.inverted, "{}", mic.name);
    }
    assert!(!s.auto_align().unwrap().can_apply, "applied already");
    // Heard together: the sum gains what the microphones aligned by hand
    // (each recorded at the kick's distance, turned the right way up) do.
    let after = master_energy(&s);
    let ideal: Vec<Vec<f64>> = KIT
        .iter()
        .enumerate()
        .map(|(i, m)| {
            let mut y = m.record(&source, KIT[0].delay, 100 + i as u64);
            if m.inverted {
                y.iter_mut().for_each(|v| *v = -*v);
            }
            y
        })
        .collect();
    let sum = |v: &[Vec<f64>]| -> f64 {
        (0..v[0].len())
            .map(|i| v.iter().map(|x| x[i]).sum::<f64>().powi(2))
            .sum()
    };
    let ideal = sum(&ideal);
    // The master hears each mono track at −3 dB a side: both sides
    // together are the mono sum.
    let gain = db(after / before);
    eprintln!(
        "sum before {:.2} dB, after {:.2} dB, ideal {:.2} dB: {gain:.2} dB gained",
        db(before),
        db(after),
        db(ideal)
    );
    // (The snare bottom's polarity and the room's lag cost this kit
    // about 2.8 dB before.)
    assert!(gain > 2.5, "{gain} dB");
    assert!(
        db(after / ideal).abs() < 0.05,
        "{} dB from ideal",
        db(after / ideal)
    );

    // Saved and opened again.
    let file = dir.join("Kit.ffproj");
    s.save_as(&file).unwrap();
    let mut t = Session::new(Project::new("x", SR), None, EngineConfig::default()).unwrap();
    t.open(&file).unwrap();
    for id in &tracks {
        let (a, b) = (
            s.project().track(*id).unwrap(),
            t.project().track(*id).unwrap(),
        );
        assert_eq!(a.delay_ms, b.delay_ms);
        assert_eq!(a.phase_invert, b.phase_invert);
    }
    // The menu resets a delay; one undo takes the alignment back whole.
    let reset = s.auto_align_menu(tracks[3]);
    assert!(
        reset[1].label.starts_with("Reset Track Delay (−")
            || reset[1].label.starts_with("Reset Track Delay (-")
    );
    s.dispatch(Action::Undo).unwrap();
    for id in &tracks {
        let t = s.project().track(*id).unwrap();
        assert_eq!((t.delay_ms, t.phase_invert), (0.0, false));
    }
    s.dispatch(Action::AutoAlign(AlignOp::Close)).unwrap();
    assert!(s.auto_align().is_none());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn another_reference_and_a_track_left_out() {
    let dir = temp_dir("reference");
    let source = drums(9);
    let recordings: Vec<(&str, Vec<f64>)> = KIT
        .iter()
        .enumerate()
        .map(|(i, m)| (m.name, m.record(&source, m.delay, 200 + i as u64)))
        .collect();
    let (mut s, tracks) = session_with(&recordings, &dir);
    // Aligned to the overheads, applied straight away (a script).
    s.dispatch(Action::AutoAlign(AlignOp::Open {
        tracks: tracks.clone(),
        reference: Some(tracks[2]),
    }))
    .unwrap();
    s.dispatch(Action::AutoAlign(AlignOp::Include {
        track: tracks[3],
        on: false,
    }))
    .unwrap();
    s.dispatch(Action::AutoAlign(AlignOp::Apply)).unwrap();
    s.wait_for_auto_align();
    let p = s.project();
    // The close microphones wait for the overheads; the room is left out.
    for (i, id) in tracks.iter().enumerate() {
        let t = p.track(*id).unwrap();
        let want = match i {
            3 | 2 => 0.0,
            _ => (KIT[2].delay - KIT[i].delay) / 48.0,
        };
        assert!(
            (t.delay_ms - want).abs() < 0.05 / 48.0,
            "{i}: {}",
            t.delay_ms
        );
        assert_eq!(t.phase_invert, KIT[i].inverted && i != 3);
    }
    // The reference's own delay and polarity stay what the rest follows.
    let view = s.auto_align().unwrap();
    assert_eq!(view.effective, Some(tracks[2]));
    assert!(!view.rows[3].include);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn unrelated_tracks_are_left_alone() {
    let dir = temp_dir("unrelated");
    let a = drums(1);
    let b = drums(2);
    let (mut s, tracks) = session_with(&[("One", a), ("Other", b)], &dir);
    s.dispatch(Action::AutoAlign(AlignOp::Open {
        tracks: tracks.clone(),
        reference: None,
    }))
    .unwrap();
    s.wait_for_auto_align();
    let view = s.auto_align().unwrap();
    let other = view.rows.iter().find(|r| !r.reference).unwrap();
    assert_eq!(other.lag_ms, None);
    assert!(!other.include);
    assert_eq!(other.confidence_text(), "no common signal");
    assert!(!view.can_apply);
    let steps = s.history_steps().0.len();
    s.dispatch(Action::AutoAlign(AlignOp::Apply)).unwrap();
    assert_eq!(s.history_steps().0.len(), steps, "nothing to set");
    // One track alone has nothing to align with.
    s.dispatch(Action::AutoAlign(AlignOp::Open {
        tracks: vec![tracks[0]],
        reference: None,
    }))
    .unwrap();
    assert!(!s.auto_aligning());
    assert!(s.auto_align().unwrap().status.contains("two or more"));
    let _ = std::fs::remove_dir_all(&dir);
}
