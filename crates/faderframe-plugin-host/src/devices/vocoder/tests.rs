#![allow(clippy::unwrap_used)]

use super::*;
use crate::devices::rig::{BLOCK, Rig, SR, bin_db, config, silence, tone, white};
use faderframe_audio_graph::AudioBuffer;
use faderframe_core::ChannelLayout;
use faderframe_midi::{MidiBuffer, TimedMidiEvent};
use faderframe_transport::TransportInfo;

fn rig(set: &[(u32, f64)]) -> Rig<VocoderProcessor> {
    Rig::with(parameters(), TAP_VALUES, set, VocoderProcessor::new)
}

fn rms_db(x: &[f32]) -> f64 {
    let ms = x.iter().map(|v| f64::from(*v).powi(2)).sum::<f64>() / x.len().max(1) as f64;
    10.0 * ms.max(1e-30).log10()
}

/// A key's frequency.
fn hz(key: f64) -> f64 {
    440.0 * 2f64.powf((key - 69.0) / 12.0)
}

/// A unison saw on C3 (130.8 Hz): its harmonics are where the bands are.
fn saw_carrier() -> Vec<(u32, f64)> {
    vec![
        (id::CHORD, 0.0),
        (id::ROOT, 48.0),
        (id::WAVE, 0.0),
        (id::DETUNE, 0.0),
        (id::NOISE, 0.0),
        (id::UNVOICED, 0.0),
        (id::WIDTH, 0.0),
    ]
}

#[test]
fn the_modulators_spectrum_shapes_the_carrier() {
    let mut r = rig(&saw_carrier());
    let (l, _) = r.run(1.5, tone(1_000.0, 0.3), silence);
    let f0 = hz(48.0);
    let near = bin_db(&l, 8.0 * f0);
    let far = bin_db(&l, 31.0 * f0);
    let low = bin_db(&l, 2.0 * f0);
    assert!(near > far + 25.0, "near {near:.1}, far {far:.1}");
    assert!(near > low + 20.0, "near {near:.1}, low {low:.1}");
    // About as loud as the voice (Enhance).
    let level = rms_db(&l[24_000..]);
    assert!(
        (level - rms_db(&[0.3 / 2f32.sqrt(); 10])).abs() < 10.0,
        "{level:.1}"
    );
    // The modulator's band published.
    let bands = band_count(get(&r.params, id::BANDS));
    let loudest = (0..bands)
        .max_by(|a, b| {
            r.tap
                .value(value::BAND + a)
                .total_cmp(&r.tap.value(value::BAND + b))
        })
        .unwrap();
    let centre = bands_of(&r)[loudest].0;
    assert!(
        (centre / 1_000.0).log2().abs() < 0.3,
        "loudest band at {centre:.0} Hz"
    );
}

fn bands_of(r: &Rig<VocoderProcessor>) -> Vec<(f64, f64)> {
    bands(
        band_count(get(&r.params, id::BANDS)),
        get(&r.params, id::LOW),
        get(&r.params, id::HIGH),
        get(&r.params, id::SPACING) as usize,
    )
}

#[test]
fn silence_in_silence_out() {
    let mut r = rig(&saw_carrier());
    let (l, rr) = r.run(1.0, silence, silence);
    assert!(rms_db(&l[4_800..]) < -100.0 && rms_db(&rr[4_800..]) < -100.0);
}

#[test]
fn formant_moves_the_voice_up_the_carrier() {
    let mut set = saw_carrier();
    set.push((id::FORMANT, 12.0));
    let mut r = rig(&set);
    let (l, _) = r.run(1.5, tone(1_000.0, 0.3), silence);
    let f0 = hz(48.0);
    // An octave up: the 15th–16th harmonics (about 2 kHz), not the 8th.
    let up = bin_db(&l, 16.0 * f0).max(bin_db(&l, 15.0 * f0));
    let home = bin_db(&l, 8.0 * f0);
    assert!(up > home + 15.0, "up {up:.1}, home {home:.1}");
}

#[test]
fn the_sidechain_can_be_the_carrier_or_the_voice() {
    // Carrier on the sidechain: its 2 kHz comes through where the noise
    // (the voice) has energy.
    let mut r = rig(&[(id::CARRIER, 2.0), (id::UNVOICED, 0.0)]);
    let noise = |n: usize| {
        let v = (white(n, 4) * 0.1) as f32;
        (v, v)
    };
    let (l, _) = r.run(1.0, noise, tone(2_000.0, 0.3));
    assert!(bin_db(&l, 2_000.0) > -40.0);
    assert!(bin_db(&l, 2_000.0) > bin_db(&l, 700.0) + 20.0);
    // The voice on the sidechain, the carrier the input.
    let mut r = rig(&[
        (id::CARRIER, 3.0),
        (id::MODULATOR, 1.0),
        (id::UNVOICED, 0.0),
    ]);
    let (l, _) = r.run(1.0, tone(2_000.0, 0.3), noise);
    assert!(bin_db(&l, 2_000.0) > -40.0);
    // No voice there: silence.
    let (l, _) = r.run(1.0, tone(2_000.0, 0.3), silence);
    assert!(rms_db(&l[24_000..]) < -80.0);
}

#[test]
fn follow_sings_along_with_the_voice() {
    let mut r = rig(&[(id::PITCH, 2.0), (id::TRANSPOSE, 12.0)]);
    // A buzzy voice on A3 (key 57).
    let voice = |n: usize| {
        let t = n as f64 / SR;
        let v: f64 = (1..10)
            .map(|h| (std::f64::consts::TAU * 220.0 * h as f64 * t).sin() / h as f64)
            .sum();
        ((0.2 * v) as f32, (0.2 * v) as f32)
    };
    r.run(1.0, voice, silence);
    let pitch = f64::from(r.tap.value(value::PITCH));
    assert!((pitch - 69.0).abs() < 0.5, "{pitch:.2}");
}

/// Run blocks with MIDI events (key, on) at frames.
fn run_midi(
    p: &mut VocoderProcessor,
    seconds: f64,
    notes: &[(usize, u8, bool)],
    input: impl Fn(usize) -> (f32, f32),
) -> Vec<f32> {
    let mut ins = vec![
        AudioBuffer::new(ChannelLayout::Stereo, BLOCK),
        AudioBuffer::new(ChannelLayout::Stereo, BLOCK),
    ];
    let mut outs = vec![AudioBuffer::new(ChannelLayout::Stereo, BLOCK)];
    for b in ins.iter_mut().chain(outs.iter_mut()) {
        b.set_len(BLOCK);
    }
    let transport = TransportInfo::default();
    let mut left = Vec::new();
    let blocks = (seconds * SR / BLOCK as f64).ceil() as usize;
    for k in 0..blocks {
        let at = k * BLOCK;
        let mut events = vec![MidiBuffer::with_capacity(16)];
        for &(f, key, on) in notes {
            if (at..at + BLOCK).contains(&f) {
                let event = if on {
                    MidiEvent::NoteOn {
                        channel: 0,
                        key,
                        velocity: 100,
                    }
                } else {
                    MidiEvent::NoteOff {
                        channel: 0,
                        key,
                        velocity: 0,
                    }
                };
                events[0]
                    .push(TimedMidiEvent::new((f - at) as u32, event))
                    .unwrap();
            }
        }
        for i in 0..BLOCK {
            let (a, b) = input(at + i);
            ins[0].channel_mut(0)[i] = a;
            ins[0].channel_mut(1)[i] = b;
        }
        let ctx = PluginProcessContext {
            transport: &transport,
            param_events: &[],
            harmony: &crate::NO_HARMONY,
            param_mods: &[],
            note_mods: &[],
        };
        let mut io = NodeIo {
            frames: BLOCK,
            audio_in: &ins,
            audio_out: &mut outs,
            events_in: &events,
            events_out: &mut [],
        };
        p.process(&ctx, &mut io);
        left.extend_from_slice(outs[0].channel(0));
    }
    left
}

#[test]
fn midi_notes_play_the_synth() {
    let mut set = saw_carrier();
    set.push((id::PITCH, 1.0));
    set.push((id::GLIDE, 0.0));
    let r = rig(&set);
    let tap = Arc::clone(&r.tap);
    let mut p = VocoderProcessor::new(r.params.clone(), Arc::clone(&tap), &config());
    let noise = |n: usize| {
        let v = (white(n, 2) * 0.3) as f32;
        (v, v)
    };
    // E3 held from 0.1 s, G3 added at 0.5 s: two voices, the newest leads.
    let l = run_midi(&mut p, 1.0, &[(4_800, 52, true), (24_000, 55, true)], noise);
    assert_eq!(tap.value(value::VOICES), 2.0);
    assert!((tap.value(value::PITCH) - 55.0).abs() < 1e-3);
    // Its harmonics, not what lies between them.
    let on = bin_db(&l, hz(52.0) * 4.0);
    let between = bin_db(&l, hz(52.0) * 4.5);
    assert!(on > between + 15.0, "{on:.1} vs {between:.1}");
    // Let go: nothing sounding (the voice still in, the carrier gone).
    let l = run_midi(&mut p, 1.0, &[(10, 52, false), (10, 55, false)], noise);
    assert_eq!(tap.value(value::VOICES), 0.0);
    assert!(rms_db(&l[28_800..]) < -80.0, "{}", rms_db(&l[28_800..]));
}

#[test]
fn consonants_pass_their_highs() {
    // Hiss over 5 kHz: unvoiced.
    let mut hp = [OnePole::new(5_000.0, SR), OnePole::new(5_000.0, SR)];
    let hiss: Vec<f32> = (0..50_000)
        .map(|n| {
            let a = hp[0].high(white(n, 8) * 0.2);
            hp[1].high(a) as f32
        })
        .collect();
    let h = hiss.clone();
    let mut r = rig(&[(id::PITCH, 1.0), (id::UNVOICED, 1.0)]);
    let (l, _) = r.run(1.0, move |n| (h[n], h[n]), silence);
    assert!(r.tap.value(value::UNVOICED) > 0.9);
    // No notes, so no carrier: what comes out is the hiss let through.
    assert!(rms_db(&l[24_000..]) > rms_db(&hiss[24_000..]) - 6.0);
    // A voiced tone is not.
    let mut r = rig(&[(id::UNVOICED, 1.0)]);
    r.run(1.0, tone(200.0, 0.3), silence);
    assert!(r.tap.value(value::UNVOICED) < 0.1);
    // Unvoiced off: nothing let through.
    let h = hiss.clone();
    let mut r = rig(&[(id::PITCH, 1.0), (id::UNVOICED, 0.0)]);
    let (l, _) = r.run(1.0, move |n| (h[n], h[n]), silence);
    assert!(rms_db(&l[24_000..]) < -80.0);
}

#[test]
fn mix_brings_the_input_back() {
    let mut r = rig(&[(id::MIX, 0.0)]);
    let x = tone(440.0, 0.4);
    let (l, _) = r.run(0.3, &x, silence);
    for (n, v) in l.iter().enumerate() {
        assert!((v - x(n).0).abs() < 1e-6);
    }
}

#[test]
fn the_bank_is_spaced_between_low_and_high() {
    for spacing in 0..SPACINGS.len() {
        for n in BAND_COUNTS {
            let b = bands(n, 100.0, 8_000.0, spacing);
            assert_eq!(b.len(), n);
            assert!((b[0].0 - 100.0).abs() < 1e-6);
            assert!((b[n - 1].0 - 8_000.0).abs() < 1e-3);
            assert!(b.windows(2).all(|w| w[1].0 > w[0].0 && w[0].1 > 0.0));
        }
    }
    for (i, p) in parameters().iter().enumerate() {
        assert_eq!(p.id.0 as usize, i, "{}", p.name);
    }
    assert_eq!(format(ParameterId(id::ROOT), 48.0).unwrap(), "C3");
    assert_eq!(format(ParameterId(id::CHORD), 4.0).unwrap(), "Minor");
    assert_eq!(format(ParameterId(id::BANDS), 6.0).unwrap(), "40");
}
