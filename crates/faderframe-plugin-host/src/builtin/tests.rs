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
    assert_eq!(list.len(), 4);
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
