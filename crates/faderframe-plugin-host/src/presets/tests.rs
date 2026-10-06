#![allow(clippy::unwrap_used)]

use super::*;
use crate::builtin::BuiltinFactory;
use crate::{PluginFactory, PluginProcessContext, ProcessConfig};
use faderframe_audio_graph::{AudioBuffer, NodeIo};
use faderframe_core::ChannelLayout;
use faderframe_midi::{MidiBuffer, MidiEvent, TimedMidiEvent};
use faderframe_transport::TransportInfo;

const SR: f64 = 48_000.0;
const BLOCK: usize = 256;

/// The MIDI effects (their presets are played, not heard).
const MIDI_EFFECTS: [&str; 4] = [
    builtin::ARPEGGIATOR,
    builtin::CHORD,
    builtin::SCALE,
    builtin::NOTE_ECHO,
];

/// Every built-in with factory presets.
const WITH_PRESETS: [&str; 16] = [
    builtin::ARPEGGIATOR,
    builtin::CHORD,
    builtin::SCALE,
    builtin::NOTE_ECHO,
    builtin::COMPRESSOR,
    builtin::LIMITER,
    builtin::GATE,
    builtin::DEESSER,
    builtin::SATURATOR,
    builtin::ECHO,
    builtin::REVERB,
    builtin::MODULATION,
    builtin::GAIN,
    builtin::EQ,
    builtin::PROGRAM_EQ,
    builtin::SYNTH,
];

#[test]
fn every_effect_and_the_synth_have_ten_to_fifteen_sound_presets() {
    for id in WITH_PRESETS {
        let presets = factory_presets(id);
        assert!(
            (10..=15).contains(&presets.len()),
            "{id}: {} presets",
            presets.len()
        );
        let infos = parameters(id);
        assert!(!infos.is_empty(), "{id}");
        let mut names: Vec<&str> = presets.iter().map(|p| p.name).collect();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), presets.len(), "{id}: names repeat");
        for p in &presets {
            for (pid, v) in p.set() {
                let info = infos
                    .iter()
                    .find(|i| i.id.0 == *pid)
                    .unwrap_or_else(|| panic!("{id} '{}': no parameter {pid}", p.name));
                assert!(
                    (info.min..=info.max).contains(v),
                    "{id} '{}': {} = {v} outside {}..{}",
                    p.name,
                    info.name,
                    info.min,
                    info.max
                );
                if info.stepped {
                    assert_eq!(v.fract(), 0.0, "{id} '{}': {}", p.name, info.name);
                }
            }
            let mut ids: Vec<u32> = p.set().iter().map(|(i, _)| *i).collect();
            ids.sort_unstable();
            let n = ids.len();
            ids.dedup();
            assert_eq!(ids.len(), n, "{id} '{}' sets a parameter twice", p.name);
            // Complete: every parameter, once.
            assert_eq!(p.values(&infos).len(), infos.len());
        }
        // Distinct: no two presets the same.
        for (i, a) in presets.iter().enumerate() {
            for b in &presets[i + 1..] {
                assert_ne!(
                    a.values(&infos),
                    b.values(&infos),
                    "{id}: '{}' = '{}'",
                    a.name,
                    b.name
                );
            }
        }
    }
    for id in [
        builtin::SAMPLER,
        builtin::DRUMS,
        builtin::TUNER,
        builtin::LATENCY_PROBE,
    ] {
        assert!(factory_presets(id).is_empty(), "{id}");
    }
    assert_eq!(
        factory_preset_values(builtin::REVERB, 0).map(|v| v.len()),
        Some(parameters(builtin::REVERB).len())
    );
    assert!(factory_preset_values(builtin::REVERB, 99).is_none());
}

#[test]
fn factory_programs_replace_all_settings_and_round_trip() {
    for id in WITH_PRESETS {
        let mut inst = BuiltinFactory.instantiate(id).unwrap();
        let presets = factory_presets(id);
        assert_eq!(
            inst.programs(),
            presets.iter().map(|p| p.name).collect::<Vec<_>>()
        );
        let infos = inst.parameters().to_vec();
        for (index, preset) in presets.iter().enumerate() {
            // Old settings, including values absent from this preset,
            // must not leak through a program change.
            for p in &infos {
                inst.set_parameter(p.id, p.max).unwrap();
            }
            inst.select_program(index).unwrap();
            assert_eq!(inst.current_program(), Some(index));
            let expected = preset.values(&infos);
            for &(pid, value) in &expected {
                assert_eq!(
                    inst.parameter(pid),
                    Some(f64::from(value as f32)),
                    "{id}: {}",
                    preset.name
                );
            }
            let state = inst.save_state().unwrap();
            assert!(inst.select_program(presets.len()).is_err());
            assert_eq!(inst.save_state().unwrap(), state);
            let mut restored = BuiltinFactory.instantiate(id).unwrap();
            restored.load_state(&state).unwrap();
            assert_eq!(restored.save_state().unwrap(), state);
        }
    }
    let mut inst = BuiltinFactory.instantiate(builtin::PROGRAM_EQ).unwrap();
    let initial = inst.save_state().unwrap();
    assert_eq!(inst.programs()[0], "Low End Punch");
    inst.select_program(0).unwrap();
    for &(pid, value) in crate::program_eq::PRESETS[0].1 {
        assert_eq!(inst.parameter(ParameterId(pid as u32)), Some(value));
    }
    inst.load_state(&initial).unwrap();
    assert_eq!(inst.current_program(), None);
    for id in [
        builtin::SAMPLER,
        builtin::DRUMS,
        builtin::TUNER,
        builtin::LATENCY_PROBE,
    ] {
        let mut inst = BuiltinFactory.instantiate(id).unwrap();
        assert!(inst.programs().is_empty());
        assert!(inst.select_program(0).is_err());
    }
}

// --- rendering every preset ------------------------------------------------------

/// A small deterministic noise source.
struct Noise(u32);

impl Noise {
    fn next(&mut self) -> f32 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 17;
        self.0 ^= self.0 << 5;
        self.0 as f32 / u32::MAX as f32 * 2.0 - 1.0
    }
}

/// Test material: a drum pattern (kick, snare, hats) under a sung line
/// with sibilant onsets, the hats and the voice off centre; peaks near
/// −6 dBFS. Also the kick alone (a sidechain key).
pub(crate) fn material(seconds: f64) -> ([Vec<f32>; 2], Vec<f32>) {
    let n = (seconds * SR) as usize;
    let mut noise = Noise(0x9e37_79b9);
    let (mut l, mut r, mut kick) = (vec![0.0; n], vec![0.0; n], vec![0.0; n]);
    let tau = std::f64::consts::TAU;
    let mut phase = 0.0f64;
    for i in 0..n {
        let t = i as f64 / SR;
        // Kick on every beat (120 BPM), snare on 2 and 4, hats in eighths.
        let beat = t % 0.5;
        let k =
            (tau * (50.0 + 90.0 * (-beat * 30.0).exp()) * beat).sin() * (-beat * 9.0).exp() * 0.45;
        let bar = t % 1.0;
        let sn = if bar >= 0.5 { bar - 0.5 } else { 9.0 };
        let snare = if sn < 0.3 {
            (f64::from(noise.next()) * 0.6 + (tau * 190.0 * sn).sin() * 0.4)
                * (-sn * 22.0).exp()
                * 0.3
        } else {
            0.0
        };
        let ht = t % 0.25;
        let hat = f64::from(noise.next()) * (-ht * 90.0).exp() * 0.07;
        // The voice: 1.4 s phrases, a glottal-ish pulse at 170–200 Hz with
        // vibrato, an "s" at each start.
        let pt = t % 1.75;
        let f0 = 185.0 + 12.0 * (tau * 5.2 * t).sin() + 15.0 * (tau * 0.3 * t).sin();
        phase = (phase + f0 / SR).fract();
        let pulse: f64 = (1..12)
            .map(|h| (tau * phase * h as f64).sin() / (h as f64).powf(1.3))
            .sum();
        let env = if pt < 1.4 {
            (pt / 0.05).min(1.0) * ((1.4 - pt) / 0.1).min(1.0)
        } else {
            0.0
        };
        let ess = if (0.0..0.12).contains(&pt) {
            let s = f64::from(noise.next());
            s * (1.0 - pt / 0.12) * 0.12
        } else {
            0.0
        };
        let voice = pulse * env * 0.16 + ess;
        l[i] = (k + snare + hat * 0.6 + voice * 1.1) as f32;
        r[i] = (k + snare + hat * 1.4 + voice * 0.9) as f32;
        kick[i] = k as f32;
    }
    ([l, r], kick)
}

pub(crate) struct Rendered {
    pub out: [Vec<f32>; 2],
}

/// Run `id` with preset `index` over `[l, r]` (the key on the
/// sidechain), or, for instruments, play `notes` (key, start, end in
/// seconds) for `seconds`.
pub(crate) fn render(
    id: &str,
    index: usize,
    input: &[Vec<f32>; 2],
    key: &[f32],
    notes: &[(u8, f64, f64)],
    seconds: f64,
) -> Rendered {
    let mut inst = BuiltinFactory.instantiate(id).unwrap();
    inst.select_program(index).unwrap();
    // Only presets explicitly asking for an external key get a sidechain.
    // Otherwise the dynamics must hear the material they are processing.
    let preset = &factory_presets(id)[index];
    let sidechain = preset.set().iter().any(|&(pid, value)| {
        value > 0.5
            && match id {
                builtin::COMPRESSOR => pid == crate::devices::compressor::id::EXTERNAL,
                builtin::GATE => pid == crate::devices::gate::id::EXTERNAL,
                builtin::EQ => (0..crate::eq::BANDS)
                    .any(|b| pid == crate::eq::band_id(b, crate::eq::Field::Key).0),
                _ => false,
            }
    });
    let config = ProcessConfig {
        sample_rate: SR,
        max_block_size: BLOCK as u32,
        sidechain,
        double_precision: false,
    };
    let mut p = inst.create_processor(&config).unwrap();
    let instrument = !notes.is_empty();
    let mut ins = if instrument {
        Vec::new()
    } else {
        (0..if sidechain { 2 } else { 1 })
            .map(|_| AudioBuffer::new(ChannelLayout::Stereo, BLOCK))
            .collect()
    };
    let mut outs = vec![AudioBuffer::new(ChannelLayout::Stereo, BLOCK)];
    for b in ins.iter_mut().chain(outs.iter_mut()) {
        b.set_len(BLOCK);
    }
    let mut ev_in = vec![MidiBuffer::with_capacity(64)];
    let mut ev_out: Vec<MidiBuffer> = Vec::new();
    let mut transport = TransportInfo {
        playing: true,
        sample_rate: SR,
        tempo: 120.0,
        ..TransportInfo::default()
    };
    let frames = (seconds * SR) as usize;
    let mut out = [Vec::with_capacity(frames), Vec::with_capacity(frames)];
    let mut at = 0;
    while at < frames {
        if !instrument {
            for (c, input) in input.iter().enumerate() {
                for i in 0..BLOCK {
                    ins[0].channel_mut(c)[i] = input.get(at + i).copied().unwrap_or(0.0);
                    if sidechain {
                        ins[1].channel_mut(c)[i] = key.get(at + i).copied().unwrap_or(0.0);
                    }
                }
            }
        }
        ev_in[0].clear();
        for &(k, start, end) in notes {
            let (s, e) = ((start * SR) as usize, (end * SR) as usize);
            if (at..at + BLOCK).contains(&s) {
                ev_in[0]
                    .push(TimedMidiEvent::new(
                        (s - at) as u32,
                        MidiEvent::NoteOn {
                            channel: 0,
                            key: k,
                            velocity: 100,
                        },
                    ))
                    .unwrap();
            }
            if (at..at + BLOCK).contains(&e) {
                ev_in[0]
                    .push(TimedMidiEvent::new(
                        (e - at) as u32,
                        MidiEvent::NoteOff {
                            channel: 0,
                            key: k,
                            velocity: 0,
                        },
                    ))
                    .unwrap();
            }
        }
        transport.quarter_position = at as f64 / SR * 2.0;
        transport.sample_position = at as i64;
        let ctx = PluginProcessContext {
            transport: &transport,
            param_events: &[],
            harmony: &crate::NO_HARMONY,
        };
        let mut io = NodeIo {
            frames: BLOCK,
            audio_in: &ins,
            audio_out: &mut outs,
            events_in: &ev_in,
            events_out: &mut ev_out,
        };
        p.process(&ctx, &mut io);
        for (c, o) in out.iter_mut().enumerate() {
            o.extend_from_slice(outs[0].channel(c));
        }
        at += BLOCK;
    }
    for o in &mut out {
        o.truncate(frames);
    }
    Rendered { out }
}

pub(crate) fn rms_db(x: &[f32]) -> f64 {
    let ms = x.iter().map(|v| f64::from(*v).powi(2)).sum::<f64>() / x.len().max(1) as f64;
    10.0 * ms.max(1e-20).log10()
}

pub(crate) fn peak_db(x: &[f32]) -> f64 {
    20.0 * x
        .iter()
        .fold(0.0f64, |m, v| m.max(f64::from(v.abs())))
        .max(1e-10)
        .log10()
}

fn stereo_rms_db(x: &[Vec<f32>; 2]) -> f64 {
    let p = |c: &[f32]| 10f64.powf(rms_db(c) / 10.0);
    10.0 * (0.5 * (p(&x[0]) + p(&x[1]))).max(1e-20).log10()
}

/// A chord (C3 E3 G3 C4) held long enough for the slowest pad/riser
/// attack to finish, then a low C alone.
pub(crate) const CHORD: [(u8, f64, f64); 5] = [
    (48, 0.0, 5.0),
    (52, 0.0, 5.0),
    (55, 0.0, 5.0),
    (60, 0.0, 5.0),
    (36, 5.6, 6.4),
];

#[test]
fn every_preset_sounds_and_stays_in_bounds() {
    let (input, key) = material(4.0);
    let in_db = stereo_rms_db(&input);
    for id in WITH_PRESETS
        .into_iter()
        .filter(|id| !MIDI_EFFECTS.contains(id))
    {
        for (i, preset) in factory_presets(id).iter().enumerate() {
            let name = preset.name;
            let instrument = id == builtin::SYNTH;
            let r = if instrument {
                render(id, i, &input, &key, &CHORD, 7.0)
            } else {
                render(id, i, &input, &key, &[], 4.0)
            };
            for c in &r.out {
                assert!(c.iter().all(|v| v.is_finite()), "{id} '{name}': not finite");
            }
            let peak = peak_db(&r.out[0]).max(peak_db(&r.out[1]));
            let level = stereo_rms_db(&r.out);
            if instrument {
                assert!(peak <= -1.0, "{id} '{name}': peak {peak:.1} dBFS");
                // Plucks and bells intentionally decay to silence while
                // the key is held. Check their loudest 100 ms rather
                // than averaging that silence into their audible level.
                let active_level = r
                    .out
                    .iter()
                    .flat_map(|c| c.chunks((0.1 * SR) as usize))
                    .map(rms_db)
                    .fold(f64::NEG_INFINITY, f64::max);
                assert!(
                    active_level > -36.0,
                    "{id} '{name}': {active_level:.1} dB RMS while sounding"
                );
                continue;
            }
            assert!(level > -50.0, "{id} '{name}': silent ({level:.1} dB)");
            assert!(peak < 6.0, "{id} '{name}': peak {peak:.1} dBFS");
            if id != builtin::GAIN {
                assert!(
                    (level - in_db).abs() < 12.0,
                    "{id} '{name}': {in_db:.1} → {level:.1} dB"
                );
            }
            if id == builtin::LIMITER {
                let ceiling = preset
                    .values(&parameters(id))
                    .iter()
                    .find(|(p, _)| p.0 == crate::devices::limiter::id::CEILING)
                    .map(|(_, v)| *v)
                    .unwrap();
                let unity = preset
                    .set()
                    .iter()
                    .any(|(p, v)| *p == crate::devices::limiter::id::UNITY && *v > 0.5);
                if !unity {
                    assert!(
                        peak <= ceiling + 0.05,
                        "{id} '{name}': {peak:.2} over {ceiling}"
                    );
                }
            }
        }
    }
}

/// What each preset does to the test material (run by hand:
/// `cargo test -p faderframe-plugin-host --release audition -- --ignored --nocapture`).
#[test]
#[ignore]
fn audition() {
    let (input, key) = material(4.0);
    let in_db = stereo_rms_db(&input);
    let in_side = side_db(&input);
    println!(
        "material: {in_db:.1} dB RMS, peak {:.1}, side {in_side:.1}, centroid {:.0} Hz",
        peak_db(&input[0]),
        centroid(&input[0])
    );
    for id in WITH_PRESETS
        .into_iter()
        .filter(|id| !MIDI_EFFECTS.contains(id))
    {
        println!("== {id}");
        for (i, preset) in factory_presets(id).iter().enumerate() {
            let instrument = id == builtin::SYNTH;
            let r = if instrument {
                render(id, i, &input, &key, &CHORD, 8.0)
            } else {
                render(id, i, &input, &key, &[], 6.0)
            };
            let level = stereo_rms_db(&r.out);
            let peak = peak_db(&r.out[0]).max(peak_db(&r.out[1]));
            // The tail after the input stops (instruments: after release).
            let tail_from = if instrument { 6.4 } else { 4.0 };
            let tail = &r.out[0][(tail_from * SR) as usize..];
            println!(
                "{:32} {:+6.1} dB  peak {:6.1}  side {:6.1}  centroid {:5.0} Hz  crest {:4.1}  tail {:6.1} dB",
                preset.name,
                level - if instrument { 0.0 } else { in_db },
                peak,
                side_db(&r.out),
                centroid(&r.out[0]),
                peak - level,
                rms_db(&tail[..(0.5 * SR) as usize]) - level,
            );
        }
    }
}

fn side_db(x: &[Vec<f32>; 2]) -> f64 {
    let side: Vec<f32> = x[0].iter().zip(&x[1]).map(|(a, b)| 0.5 * (a - b)).collect();
    rms_db(&side) - stereo_rms_db(x)
}

/// The spectral centroid (Hz) of a signal: zero crossings are not good
/// enough, so a coarse DFT over 4096-sample frames.
fn centroid(x: &[f32]) -> f64 {
    let n = 2048;
    let (mut num, mut den) = (0.0, 0.0);
    for frame in x.chunks_exact(n).step_by(8) {
        for k in (1..n / 2).step_by(4) {
            let w = std::f64::consts::TAU * k as f64 / n as f64;
            let (mut re, mut im) = (0.0, 0.0);
            for (j, v) in frame.iter().enumerate() {
                let win = 0.5 - 0.5 * (std::f64::consts::TAU * j as f64 / n as f64).cos();
                re += f64::from(*v) * win * (w * j as f64).cos();
                im += f64::from(*v) * win * (w * j as f64).sin();
            }
            let m = re.hypot(im);
            num += m * k as f64 * SR / n as f64;
            den += m;
        }
    }
    num / den.max(1e-20)
}

/// The notes a Synth preset is judged by: a monophonic patch plays one note
/// (a bass C2, a lead C4), a polyphonic one a four-note chord, each held
/// until its attack is over and a second more.
fn synth_notes(preset: &FactoryPreset) -> (Vec<(u8, f64, f64)>, f64) {
    use crate::devices::synth::id;
    let values = preset.values(&parameters(builtin::SYNTH));
    let get = |pid: u32| {
        values
            .iter()
            .find(|(p, _)| p.0 == pid)
            .map_or(0.0, |(_, v)| *v)
    };
    let hold = 1.0 + get(id::ATTACK) * 0.001;
    let notes = if get(id::VOICE_MODE) >= 0.5 {
        let key = if preset.name.contains("Bass") { 36 } else { 60 };
        vec![(key, 0.0, hold)]
    } else {
        [48, 52, 55, 60].iter().map(|&k| (k, 0.0, hold)).collect()
    };
    (notes, hold + 0.5)
}

/// The loudest momentary loudness of a render (BS.1770 K-weighting at
/// 48 kHz, 400 ms windows every 100 ms; LUFS).
fn momentary_max(out: &[Vec<f32>; 2]) -> f64 {
    let weighted: Vec<Vec<f64>> = out
        .iter()
        .map(|c| {
            // The head's shelf, then the RLB high pass.
            let stages = [
                (
                    [
                        1.535_124_859_586_97,
                        -2.691_696_189_406_38,
                        1.198_392_810_852_85,
                    ],
                    [-1.690_659_293_182_41, 0.732_480_774_215_85],
                ),
                (
                    [1.0, -2.0, 1.0],
                    [-1.990_047_454_833_98, 0.990_072_250_366_21],
                ),
            ];
            let mut x: Vec<f64> = c.iter().map(|v| f64::from(*v)).collect();
            for (b, a) in stages {
                let (mut x1, mut x2, mut y1, mut y2) = (0.0, 0.0, 0.0, 0.0);
                for v in &mut x {
                    let y = b[0] * *v + b[1] * x1 + b[2] * x2 - a[0] * y1 - a[1] * y2;
                    (x2, x1, y2, y1) = (x1, *v, y1, y);
                    *v = y;
                }
            }
            x
        })
        .collect();
    let (w, hop) = ((0.4 * SR) as usize, (0.1 * SR) as usize);
    let mut best = f64::NEG_INFINITY;
    let mut a = 0;
    while a + w <= weighted[0].len() {
        let power: f64 = weighted
            .iter()
            .map(|c| c[a..a + w].iter().map(|v| v * v).sum::<f64>() / w as f64)
            .sum();
        best = best.max(-0.691 + 10.0 * power.max(1e-20).log10());
        a += hop;
    }
    best
}

/// Patches sit at one loudness (momentary, K-weighted), played as they are
/// meant to be, so switching from a pad to a bass or a lead does not jump
/// by 15 dB; and they keep a decibel of headroom.
#[test]
fn the_synth_presets_are_balanced() {
    use crate::devices::synth::id;
    const TARGET: f64 = -18.0;
    let (input, key) = material(0.1);
    let mut wrong = Vec::new();
    for (i, preset) in factory_presets(builtin::SYNTH).iter().enumerate() {
        let (notes, seconds) = synth_notes(preset);
        let r = render(builtin::SYNTH, i, &input, &key, &notes, seconds);
        let level = momentary_max(&r.out);
        let peak = peak_db(&r.out[0]).max(peak_db(&r.out[1]));
        let volume = preset
            .values(&parameters(builtin::SYNTH))
            .iter()
            .find(|(p, _)| p.0 == id::VOLUME)
            .map_or(0.0, |(_, v)| *v);
        println!(
            "{:20} {level:6.1} LUFS  peak {peak:6.1}  volume {volume:+5.1} → {:+5.1}",
            preset.name,
            volume + TARGET - level
        );
        if (level - TARGET).abs() > 1.5 || peak > -1.0 {
            wrong.push(preset.name);
        }
    }
    assert!(wrong.is_empty(), "unbalanced: {wrong:?}");
}

/// Every MIDI effect preset, played a chord through (held, released,
/// then a run of single notes over a key change): it plays notes, none of
/// them stuck, none out of range.
#[test]
fn every_midi_effect_preset_plays_and_lets_go() {
    use crate::Harmony;
    use crate::devices::midi_fx::rig::{balanced, off, on, ons, run};
    use faderframe_midi::theory::{Chord, Key, Quality, Scale};
    let harmony = Harmony {
        keys: vec![
            (0, Key::new(0, Scale::Major)),
            (96_000, Key::new(9, Scale::Minor)),
        ],
        chords: vec![
            (0, 96_000, Chord::new(0, Quality::Major)),
            (96_000, 192_000, Chord::new(9, Quality::Minor)),
        ],
    };
    let mut input = vec![on(0, 60, 100), on(10, 64, 90), on(20, 67, 80)];
    input.extend([off(48_000, 60), off(48_000, 64), off(48_000, 67)]);
    for (k, key) in [62u8, 65, 69, 71].iter().enumerate() {
        let t = 96_000 + k as u64 * 12_000;
        input.extend([on(t, *key, 100), off(t + 9_000, *key)]);
    }
    for id in MIDI_EFFECTS {
        for (i, preset) in factory_presets(id).iter().enumerate() {
            let mut inst = BuiltinFactory.instantiate(id).unwrap();
            for (pid, v) in factory_preset_values(id, i).unwrap() {
                inst.set_parameter(pid, v).unwrap();
            }
            let config = ProcessConfig {
                sample_rate: SR,
                max_block_size: BLOCK as u32,
                sidechain: false,
                double_precision: false,
            };
            let mut p = inst.create_processor(&config).unwrap();
            // Long enough for echoes and latched arpeggios to finish
            // (hold presets keep playing: they are let go by a reset).
            let mut got = run(p.as_mut(), &input, SR as u64 * 12, 120.0, true, &harmony);
            let latched = preset.set().iter().any(|(p, v)| {
                id == builtin::ARPEGGIATOR
                    && *p == crate::devices::arpeggiator::id::HOLD
                    && *v > 0.5
            });
            if latched {
                p.reset();
                got.extend(run(p.as_mut(), &[], 4096, 120.0, true, &harmony));
            }
            let notes = ons(&got);
            assert!(
                notes.len() >= 3,
                "{id} '{}': {} notes",
                preset.name,
                notes.len()
            );
            assert!(balanced(&got), "{id} '{}': a note left on", preset.name);
        }
    }
}
