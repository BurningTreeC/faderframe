use super::*;
use crate::{PluginProcessContext, PluginRegistry, ProcessStatus};
use faderframe_audio_graph::{AudioBuffer, NodeIo};
use faderframe_core::ChannelLayout;
use faderframe_midi::{MidiBuffer, MidiEvent, TimedMidiEvent};
use faderframe_transport::TransportInfo;

const SR: f64 = 48_000.0;
const BLOCK: usize = 128;

fn config() -> ProcessConfig {
    ProcessConfig {
        sample_rate: SR,
        max_block_size: BLOCK as u32,
        sidechain: false,
        double_precision: false,
    }
}

struct Rig {
    ins: Vec<AudioBuffer>,
    outs: Vec<AudioBuffer>,
    ev_in: Vec<MidiBuffer>,
    ev_out: Vec<MidiBuffer>,
}

impl Rig {
    fn new() -> Self {
        let mut ins = vec![AudioBuffer::new(ChannelLayout::Stereo, BLOCK)];
        let mut outs = vec![AudioBuffer::new(ChannelLayout::Stereo, BLOCK)];
        ins[0].set_len(BLOCK);
        outs[0].set_len(BLOCK);
        Self {
            ins,
            outs,
            ev_in: vec![MidiBuffer::with_capacity(64)],
            ev_out: vec![],
        }
    }

    fn run(&mut self, p: &mut dyn PluginProcessor) -> ProcessStatus {
        let transport = TransportInfo::default();
        let ctx = PluginProcessContext {
            transport: &transport,
            param_events: &[],
        };
        let mut io = NodeIo {
            frames: BLOCK,
            audio_in: &self.ins,
            audio_out: &mut self.outs,
            events_in: &self.ev_in,
            events_out: &mut self.ev_out,
        };
        p.process(&ctx, &mut io)
    }
}

#[test]
fn registry_lists_and_instantiates_builtins() {
    let reg = PluginRegistry::with_builtins();
    let list = reg.scan();
    assert_eq!(list.len(), 7);
    assert!(
        list.iter()
            .any(|d| d.category == PluginCategory::Instrument)
    );
    assert!(
        reg.instantiate(PluginFormat::Builtin, builtin::ECHO)
            .is_ok()
    );
    assert!(matches!(
        reg.instantiate(PluginFormat::Builtin, "nope"),
        Err(PluginError::NotFound(_))
    ));
    assert!(matches!(
        reg.instantiate(PluginFormat::Clap, "x"),
        Err(PluginError::UnsupportedFormat(PluginFormat::Clap))
    ));
}

#[test]
fn state_round_trip_and_parameter_clamping() {
    let f = BuiltinFactory;
    let mut a = f.instantiate(builtin::ECHO).unwrap();
    a.set_parameter(ParameterId(1), 5.0).unwrap(); // clamped to 0.95
    assert!((a.parameter(ParameterId(1)).unwrap() - 0.95).abs() < 1e-6);
    a.set_parameter(ParameterId(0), 250.0).unwrap();
    let state = a.save_state().unwrap();
    let mut b = f.instantiate(builtin::ECHO).unwrap();
    b.load_state(&state).unwrap();
    assert!((b.parameter(ParameterId(0)).unwrap() - 250.0).abs() < 1e-3);
    assert!(b.load_state(&[1, 2, 3]).is_err());
    assert!(matches!(
        b.set_parameter(ParameterId(77), 1.0),
        Err(PluginError::UnknownParameter(_))
    ));
}

#[test]
fn synth_renders_note_from_its_sample_offset_and_releases() {
    let mut inst = BuiltinFactory.instantiate(builtin::SYNTH).unwrap();
    let mut p = inst.create_processor(&config()).unwrap();
    let mut rig = Rig::new();
    rig.ev_in[0]
        .push(TimedMidiEvent::new(
            40,
            MidiEvent::NoteOn {
                channel: 0,
                key: 69,
                velocity: 120,
            },
        ))
        .unwrap();
    assert_eq!(rig.run(p.as_mut()), ProcessStatus::Continue);
    let l = rig.outs[0].channel(0);
    assert!(
        l[..40].iter().all(|&s| s == 0.0),
        "silent before the note-on offset"
    );
    assert!(
        l[41..].iter().any(|&s| s.abs() > 1e-4),
        "sound after the note-on"
    );
    // Release and run until the voice dies.
    rig.ev_in[0].clear();
    rig.ev_in[0]
        .push(TimedMidiEvent::new(
            0,
            MidiEvent::NoteOff {
                channel: 0,
                key: 69,
                velocity: 0,
            },
        ))
        .unwrap();
    rig.run(p.as_mut());
    rig.ev_in[0].clear();
    let mut status = ProcessStatus::Continue;
    for _ in 0..2000 {
        status = rig.run(p.as_mut());
        if status == ProcessStatus::Sleep {
            break;
        }
    }
    assert_eq!(status, ProcessStatus::Sleep);
    assert!(rig.outs[0].channel(0).iter().all(|s| s.is_finite()));
}

#[test]
fn echo_repeats_an_impulse_after_the_delay_time() {
    let mut inst = BuiltinFactory.instantiate(builtin::ECHO).unwrap();
    inst.set_parameter(ParameterId(0), 10.0).unwrap(); // 480 samples
    inst.set_parameter(ParameterId(4), 0.0).unwrap(); // no ping-pong
    inst.set_parameter(ParameterId(2), 0.0).unwrap(); // no damping
    let mut p = inst.create_processor(&config()).unwrap();
    let mut rig = Rig::new();
    let mut out = Vec::new();
    for b in 0..8 {
        rig.ins[0].clear();
        if b == 0 {
            rig.ins[0].channel_mut(0)[0] = 1.0;
            rig.ins[0].channel_mut(1)[0] = 1.0;
        }
        rig.run(p.as_mut());
        out.extend_from_slice(rig.outs[0].channel(0));
    }
    let peak = out
        .iter()
        .enumerate()
        .max_by(|a, b| a.1.abs().total_cmp(&b.1.abs()))
        .unwrap()
        .0;
    assert!((475..=485).contains(&peak), "echo peak at {peak}");
}

#[test]
fn latency_probe_reports_and_applies_its_latency() {
    let mut inst = BuiltinFactory.instantiate(builtin::LATENCY_PROBE).unwrap();
    inst.set_parameter(ParameterId(0), 100.0).unwrap();
    assert_eq!(inst.latency_samples(), 100);
    let mut p = inst.create_processor(&config()).unwrap();
    let mut rig = Rig::new();
    // Impulse at frame 60 appears 100 frames later: frame 32 of block 2.
    rig.ins[0].channel_mut(0)[60] = 1.0;
    rig.run(p.as_mut());
    assert!(rig.outs[0].channel(0).iter().all(|&s| s == 0.0));
    rig.ins[0].clear();
    rig.run(p.as_mut());
    let out = rig.outs[0].channel(0);
    assert_eq!(out[60 + 100 - BLOCK], 1.0);
    assert_eq!(out.iter().filter(|&&s| s != 0.0).count(), 1);

    let mut zero = BuiltinFactory.instantiate(builtin::LATENCY_PROBE).unwrap();
    zero.set_parameter(ParameterId(0), 0.0).unwrap();
    let mut p = zero.create_processor(&config()).unwrap();
    let mut rig = Rig::new();
    rig.ins[0].channel_mut(0)[3] = 1.0;
    rig.run(p.as_mut());
    assert_eq!(
        rig.outs[0].channel(0)[3],
        1.0,
        "zero latency is a passthrough"
    );
}

/// Render `blocks` blocks and return the left channel.
fn record(p: &mut dyn PluginProcessor, rig: &mut Rig, blocks: usize) -> Vec<f32> {
    let mut out = Vec::with_capacity(blocks * BLOCK);
    for _ in 0..blocks {
        rig.run(p);
        rig.ev_in[0].clear();
        out.extend_from_slice(rig.outs[0].channel(0));
    }
    out
}

/// Fundamental by autocorrelation (smallest lag within 90 % of the best).
fn fundamental(x: &[f32]) -> f64 {
    let corr = |lag: usize| -> f64 {
        x.iter()
            .zip(&x[lag..])
            .map(|(a, b)| (*a as f64) * (*b as f64))
            .sum()
    };
    let lags = 30..1500;
    let c: Vec<f64> = lags.clone().map(corr).collect();
    let best = c.iter().cloned().fold(f64::MIN, f64::max);
    // Skip the zero-lag lobe, then take the first peak near the best.
    let mut i = c.iter().position(|&v| v < 0.0).unwrap_or(0);
    while i + 1 < c.len() && !(c[i] >= 0.9 * best && c[i] >= c[i + 1] && c[i] >= c[i - 1]) {
        i += 1;
    }
    SR / (lags.start + i) as f64
}

#[test]
fn synth_bend_range_follows_rpn_and_the_mpe_zone() {
    let cc = |channel, controller, value| MidiEvent::ControlChange {
        channel,
        controller,
        value,
    };
    let pitch = |events: &[MidiEvent]| {
        let mut inst = BuiltinFactory.instantiate(builtin::SYNTH).unwrap();
        let mut p = inst.create_processor(&config()).unwrap();
        let mut rig = Rig::new();
        for &e in events {
            rig.ev_in[0].push(TimedMidiEvent::new(0, e)).unwrap();
        }
        rig.ev_in[0]
            .push(TimedMidiEvent::new(
                1,
                MidiEvent::NoteOn {
                    channel: 1,
                    key: 45,
                    velocity: 120,
                },
            ))
            .unwrap();
        // Skip the attack, then measure.
        record(p.as_mut(), &mut rig, 40);
        fundamental(&record(p.as_mut(), &mut rig, 64))
    };
    let full_bend = MidiEvent::PitchBend {
        channel: 1,
        value: 16383,
    };
    let plain = pitch(&[]);
    assert!((plain - 110.0).abs() < 2.0, "A2 ≈ 110 Hz: {plain}");
    // Default range: +2 semitones.
    let two = pitch(&[full_bend]);
    assert!((two / plain - 2f64.powf(2.0 / 12.0)).abs() < 0.03, "{two}");
    // RPN 0 = 12 semitones: an octave up.
    let twelve = pitch(&[cc(1, 101, 0), cc(1, 100, 0), cc(1, 6, 12), full_bend]);
    assert!((twelve / plain - 2.0).abs() < 0.03, "{twelve}");
    // MPE zone on the master channel: members bend ±48 — a quarter of the
    // way up is an octave.
    let mpe = pitch(&[
        cc(0, 101, 0),
        cc(0, 100, 6),
        cc(0, 6, 15),
        MidiEvent::PitchBend {
            channel: 1,
            value: 8192 + 2048,
        },
    ]);
    assert!((mpe / plain - 2.0).abs() < 0.03, "{mpe}");
}

#[test]
fn synth_voices_follow_their_note_expressions() {
    use faderframe_midi::{ExpressionValue, NoteExpressionKind};
    // Play A2 with expressions addressed to it (or to another key) right
    // after the note-on; returns the left channel and its RMS.
    let play = |exprs: &[(u8, NoteExpressionKind, f64)]| {
        let mut inst = BuiltinFactory.instantiate(builtin::SYNTH).unwrap();
        let mut p = inst.create_processor(&config()).unwrap();
        let mut rig = Rig::new();
        rig.ev_in[0]
            .push(TimedMidiEvent::new(
                1,
                MidiEvent::NoteOn {
                    channel: 0,
                    key: 45,
                    velocity: 120,
                },
            ))
            .unwrap();
        for &(key, kind, v) in exprs {
            rig.ev_in[0]
                .push(TimedMidiEvent::new(
                    1,
                    MidiEvent::NoteExpression {
                        channel: 0,
                        key,
                        kind,
                        value: ExpressionValue::new(v),
                    },
                ))
                .unwrap();
        }
        record(p.as_mut(), &mut rig, 40);
        let x = record(p.as_mut(), &mut rig, 64);
        let rms = (x.iter().map(|s| (*s as f64).powi(2)).sum::<f64>() / x.len() as f64).sqrt();
        (x, rms)
    };
    let (x, level) = play(&[]);
    let plain = fundamental(&x);
    assert!((plain - 110.0).abs() < 2.0, "{plain}");
    // Tuning: an octave up — for this note only.
    let up = fundamental(&play(&[(45, NoteExpressionKind::Tuning, 12.0)]).0);
    assert!((up / plain - 2.0).abs() < 0.03, "{up}");
    let other = fundamental(&play(&[(47, NoteExpressionKind::Tuning, 12.0)]).0);
    assert!((other - plain).abs() < 1.0, "{other}");
    // Volume −12 dB: a quarter of the level.
    let (_, quiet) = play(&[(45, NoteExpressionKind::Volume, -12.0)]);
    assert!((quiet / level - 0.25).abs() < 0.03, "{}", quiet / level);
    // Pan hard right: the left side falls silent; hard left keeps it.
    let (_, right) = play(&[(45, NoteExpressionKind::Pan, 1.0)]);
    let (_, left) = play(&[(45, NoteExpressionKind::Pan, -1.0)]);
    assert!(right < level * 0.01, "{right}");
    assert!((left / level - 1.0).abs() < 0.02, "{left}");
}
