//! PultEQFx's response and latency tests on the ported circuit, and the
//! processor as FaderFrame runs it.
#![allow(clippy::unwrap_used)]

use super::*;
use faderframe_audio_graph::AudioBuffer;
use faderframe_core::ChannelLayout;
use faderframe_transport::TransportInfo;

const FS: f64 = 96_000.0;
const IR_LEN: usize = 1 << 18;

fn impulse_response(controls: Controls) -> Vec<f64> {
    let mut eq = PassiveNetwork::new(FS);
    eq.set_controls(controls);
    let mut ir = Vec::with_capacity(IR_LEN);
    ir.push(eq.process(1.0));
    for _ in 1..IR_LEN {
        ir.push(eq.process(0.0));
    }
    ir
}

fn magnitude_db_at(ir: &[f64], freq: f64, sample_rate: f64) -> f64 {
    let w = std::f64::consts::TAU * freq / sample_rate;
    let (mut re, mut im) = (0.0, 0.0);
    for (n, &x) in ir.iter().enumerate() {
        let phase = w * n as f64;
        re += x * phase.cos();
        im -= x * phase.sin();
    }
    20.0 * (re * re + im * im).sqrt().log10()
}

const SWEEP: [f64; 18] = [
    20.0, 30.0, 50.0, 80.0, 100.0, 150.0, 200.0, 300.0, 500.0, 800.0, 1000.0, 2000.0, 3000.0,
    5000.0, 8000.0, 10000.0, 16000.0, 20000.0,
];

fn curve(controls: Controls) -> Vec<f64> {
    let ir = impulse_response(controls);
    SWEEP.iter().map(|&f| magnitude_db_at(&ir, f, FS)).collect()
}

fn at(db: &[f64], freq: f64) -> f64 {
    db[SWEEP.iter().position(|&f| f == freq).unwrap()]
}

#[test]
fn flat_when_every_knob_is_down() {
    for d in curve(Controls::default()) {
        assert!(d.abs() < 0.5);
    }
}

#[test]
fn the_low_shelves_match_the_published_curves() {
    for (freq, boost, atten) in [
        (20.0, 11.7, -15.1),
        (30.0, 14.9, -17.8),
        (60.0, 16.1, -18.9),
        (100.0, 16.3, -19.1),
    ] {
        let b = curve(Controls {
            low_boost: 1.0,
            low_freq: freq,
            ..Controls::default()
        });
        assert!((at(&b, 20.0) - boost).abs() < 1.0, "boost @ {freq}");
        assert!(at(&b, 3000.0).abs() < 0.5);
        let a = curve(Controls {
            low_atten: 1.0,
            low_freq: freq,
            ..Controls::default()
        });
        assert!((at(&a, 20.0) - atten).abs() < 1.0, "atten @ {freq}");
    }
}

#[test]
fn the_high_boost_peaks_at_eighteen_decibels_on_its_frequency() {
    for freq in [3e3, 5e3, 8e3, 10e3, 16e3] {
        let sharp = curve(Controls {
            high_boost: 1.0,
            high_boost_freq: freq,
            bandwidth: 0.0,
            ..Controls::default()
        });
        let broad = curve(Controls {
            high_boost: 1.0,
            high_boost_freq: freq,
            bandwidth: 1.0,
            ..Controls::default()
        });
        let peak = |db: &[f64]| db.iter().cloned().fold(f64::MIN, f64::max);
        assert!((peak(&sharp) - 18.0).abs() < 1.0, "sharp @ {freq}");
        assert!(peak(&broad) < peak(&sharp) - 3.0);
        let best = sharp
            .iter()
            .enumerate()
            .max_by(|a, b| a.1.total_cmp(b.1))
            .unwrap()
            .0;
        assert_eq!(SWEEP[best], freq, "the bell sits on {freq}");
    }
}

#[test]
fn the_high_atten_reaches_sixteen_decibels() {
    for freq in [5e3, 10e3, 20e3] {
        let db = curve(Controls {
            high_atten: 1.0,
            high_atten_freq: freq,
            ..Controls::default()
        });
        assert!((at(&db, freq) + 16.0).abs() < 1.2, "atten @ {freq}");
    }
}

#[test]
fn the_low_end_trick_gives_a_bump_and_a_dip() {
    let db = curve(Controls {
        low_boost: 1.0,
        low_atten: 1.0,
        low_freq: 30.0,
        ..Controls::default()
    });
    assert!(at(&db, 80.0) > 3.0);
    assert!(at(&db, 200.0) < -2.5);
    assert!(at(&db, 1000.0).abs() < 1.0);
}

#[test]
fn the_low_end_punch_preset_has_punch() {
    let (_, values) = PRESETS[0];
    let v = |i: usize| values.iter().find(|(p, _)| *p == i).unwrap().1;
    let db = curve(Controls {
        low_boost: v(param::LOW_BOOST) / 10.0,
        low_atten: v(param::LOW_ATTEN) / 10.0,
        high_boost: v(param::HIGH_BOOST) / 10.0,
        high_atten: v(param::HIGH_ATTEN) / 10.0,
        bandwidth: v(param::BANDWIDTH) / 10.0,
        low_freq: f64::from(LOW_FREQS[v(param::LOW_FREQ) as usize]),
        high_boost_freq: f64::from(HIGH_BOOST_FREQS[v(param::HIGH_BOOST_FREQ) as usize]),
        high_atten_freq: f64::from(HIGH_ATTEN_FREQS[v(param::HIGH_ATTEN_FREQ) as usize]),
    });
    assert!(at(&db, 50.0) > 5.0);
    assert!(at(&db, 100.0) > 4.0);
    assert!(at(&db, 10000.0) > 3.0);
}

#[test]
fn the_oversampling_setting_does_not_change_the_tone() {
    let analog = |f: f64| {
        let coupling = (f / 3.0) / (1.0 + (f / 3.0).powi(2)).sqrt();
        let transformer = 1.0 / (1.0 + (f / 60e3).powi(2)).sqrt();
        20.0 * (coupling * transformer).log10()
    };
    for fs in [44_100.0, 48_000.0, 96_000.0] {
        for factor in [1, 2, 4, 8] {
            let mut channel = Channel::new(fs, factor);
            channel.set_drive(0.0);
            let ir: Vec<f64> = (0..1 << 16)
                .map(|n| {
                    f64::from(channel.process(if n == 0 { 1e-3 } else { 0.0 }, false, true)) * 1e3
                })
                .collect();
            for f in [20.0, 100.0, 1e3, 10e3, 16e3, 20e3] {
                let e = magnitude_db_at(&ir, f, fs) - analog(f);
                assert!(e.abs() < 0.1, "{fs} Hz {factor}x: {f} Hz off by {e:+.3}");
            }
        }
    }
}

#[test]
fn power_off_holds_the_dry_signal_back_by_the_latency() {
    for factor in [1, 2, 4, 8] {
        let mut channel = Channel::new(48_000.0, factor);
        for n in 0..256 {
            let got = channel.process(if n == 0 { 1.0 } else { 0.0 }, true, false);
            assert_eq!(got, if n == LATENCY as usize { 1.0 } else { 0.0 });
        }
        let mut channel = Channel::new(48_000.0, factor);
        let response: Vec<f32> = (0..256)
            .map(|n| channel.process(if n == 0 { 1e-3 } else { 0.0 }, false, true))
            .collect();
        let peak = response
            .iter()
            .enumerate()
            .max_by(|a, b| a.1.abs().total_cmp(&b.1.abs()))
            .unwrap()
            .0;
        assert_eq!(peak, LATENCY as usize, "the processed path at {factor}x");
    }
}

fn processor() -> (ParamValues, Arc<AnalysisTap>, ProgramEqProcessor) {
    let params = ParamValues::new(parameters());
    let tap = Arc::new(AnalysisTap::new(params.clone(), 0));
    let config = ProcessConfig {
        sample_rate: 48_000.0,
        max_block_size: 256,
        sidechain: false,
        double_precision: false,
    };
    let p = ProgramEqProcessor::new(params.clone(), Arc::clone(&tap), &config);
    (params, tap, p)
}

fn run(p: &mut ProgramEqProcessor, input: impl Fn(usize) -> f32, blocks: usize) -> Vec<f32> {
    let mut ins = vec![AudioBuffer::new(ChannelLayout::Stereo, 256)];
    let mut outs = vec![AudioBuffer::new(ChannelLayout::Stereo, 256)];
    ins[0].set_len(256);
    outs[0].set_len(256);
    let transport = TransportInfo::default();
    let mut out = Vec::new();
    for b in 0..blocks {
        for i in 0..256 {
            let v = input(b * 256 + i);
            ins[0].channel_mut(0)[i] = v;
            ins[0].channel_mut(1)[i] = v;
        }
        let ctx = PluginProcessContext {
            transport: &transport,
            param_events: &[],
            harmony: &crate::NO_HARMONY,
            param_mods: &[],
        };
        let mut io = NodeIo {
            frames: 256,
            audio_in: &ins,
            audio_out: &mut outs,
            events_in: &[],
            events_out: &mut [],
        };
        p.process(&ctx, &mut io);
        out.extend_from_slice(outs[0].channel(0));
    }
    out
}

#[test]
fn the_processor_boosts_and_meters() {
    let (params, tap, mut p) = processor();
    let sine = |n: usize| (0.1 * (std::f64::consts::TAU * 30.0 * n as f64 / 48_000.0).sin()) as f32;
    let flat = run(&mut p, sine, 200);
    params
        .set_by_id(ParameterId(param::LOW_BOOST as u32), 10.0)
        .unwrap();
    let boosted = run(&mut p, sine, 400);
    let rms = |x: &[f32]| (x.iter().map(|v| v * v).sum::<f32>() / x.len() as f32).sqrt();
    let lift = 20.0 * (rms(&boosted[boosted.len() / 2..]) / rms(&flat[flat.len() / 2..])).log10();
    assert!(lift > 12.0, "the low boost lifts 30 Hz by {lift:.1} dB");
    assert!(tap.meter_in.held(0) > 0.09);
    assert!(tap.meter_out.held(0) > tap.meter_in.held(0));
    // Switching the oversampling while running keeps the signal going.
    params
        .set_by_id(ParameterId(param::OVERSAMPLING as u32), 0.0)
        .unwrap();
    let after = run(&mut p, sine, 100);
    assert!(rms(&after[after.len() / 2..]) > 0.3);
    assert_eq!(
        format(ParameterId(param::LOW_FREQ as u32), 1.0).unwrap(),
        "30 Hz"
    );
    assert_eq!(
        format(ParameterId(param::POWER as u32), 0.0).unwrap(),
        "OFF"
    );
    assert_eq!(
        format(ParameterId(param::HIGH_BOOST as u32), 3.25).unwrap(),
        "3.2"
    );
}

#[test]
fn mono_lights_both_meters_while_stereo_keeps_independent_channels() {
    for layout in [ChannelLayout::Mono, ChannelLayout::Stereo] {
        let (_, tap, mut p) = processor();
        let mut input = AudioBuffer::new(layout, 256);
        let mut output = AudioBuffer::new(layout, 256);
        input.set_len(256);
        output.set_len(256);
        input.channel_mut(0).fill(0.25);
        let transport = TransportInfo::default();
        for _ in 0..20 {
            p.process(
                &PluginProcessContext {
                    transport: &transport,
                    param_events: &[],
                    harmony: &crate::NO_HARMONY,
                    param_mods: &[],
                },
                &mut NodeIo {
                    frames: 256,
                    audio_in: std::slice::from_ref(&input),
                    audio_out: std::slice::from_mut(&mut output),
                    events_in: &[],
                    events_out: &mut [],
                },
            );
        }
        for meter in [&tap.meter_in, &tap.meter_out] {
            assert!(meter.held(0) > 0.1);
            if layout == ChannelLayout::Mono {
                assert_eq!(meter.held(0), meter.held(1));
                assert_eq!(meter.mean_square(0), meter.mean_square(1));
                assert_eq!(meter.figure(0), meter.figure(1));
            } else {
                assert_eq!(meter.held(1), 0.0);
            }
        }
    }
}
