#![allow(clippy::unwrap_used)]
//! End to end: plugins in helper processes — this test binary, started
//! again as a helper — against the same plugins in process; a plugin that
//! crashes and one that hangs.

use faderframe_audio_graph::{AudioBuffer, NodeIo};
use faderframe_automation::ParameterEvent;
use faderframe_core::{ChannelLayout, ParameterId, builtin};
use faderframe_midi::{ExpressionValue, MidiBuffer, MidiEvent, NoteExpressionKind, TimedMidiEvent};
use faderframe_plugin_host::{
    AudioPortInfo, ParameterInfo, ParameterUnit, PluginCategory, PluginDescriptor, PluginError,
    PluginFactory, PluginFormat, PluginInstance, PluginProcessContext, PluginProcessor,
    PluginRegistry, ProcessConfig, ProcessStatus, TailLength,
};
use faderframe_plugin_sandbox::{Launcher, child, instantiate_sandboxed};
use faderframe_transport::TransportInfo;
use std::time::{Duration, Instant};

// --- plugins that misbehave (in the helper) -------------------------------------------

struct Broken;
struct BrokenInstance(PluginDescriptor);
/// Dies or hangs in `process`.
struct BrokenProcessor(bool);

fn broken_descriptor(id: &str) -> PluginDescriptor {
    let stereo = vec![AudioPortInfo {
        channels: 2,
        is_main: true,
    }];
    PluginDescriptor {
        format: PluginFormat::Vst3,
        id: id.into(),
        name: id.into(),
        vendor: "FaderFrame".into(),
        version: "1".into(),
        category: PluginCategory::Effect,
        audio_inputs: stereo.clone(),
        audio_outputs: stereo,
        note_inputs: 0,
        note_outputs: 0,
    }
}

impl PluginFactory for Broken {
    fn format(&self) -> PluginFormat {
        PluginFormat::Vst3
    }
    fn scan(&self) -> Vec<PluginDescriptor> {
        vec![
            broken_descriptor("test.crash"),
            broken_descriptor("test.hang"),
            broken_descriptor("test.sysex"),
        ]
    }
    fn instantiate(&self, id: &str) -> Result<Box<dyn PluginInstance>, PluginError> {
        Ok(Box::new(BrokenInstance(broken_descriptor(id))))
    }
}

impl PluginInstance for BrokenInstance {
    fn descriptor(&self) -> &PluginDescriptor {
        &self.0
    }
    fn parameters(&self) -> &[ParameterInfo] {
        &[]
    }
    fn parameter(&mut self, _id: ParameterId) -> Option<f64> {
        None
    }
    fn set_parameter(&mut self, id: ParameterId, _v: f64) -> Result<(), PluginError> {
        Err(PluginError::UnknownParameter(id))
    }
    fn latency_samples(&self) -> u32 {
        0
    }
    fn tail(&self) -> TailLength {
        TailLength::None
    }
    fn save_state(&mut self) -> Result<Vec<u8>, PluginError> {
        Ok(Vec::new())
    }
    fn load_state(&mut self, _data: &[u8]) -> Result<(), PluginError> {
        Ok(())
    }
    fn create_processor(
        &mut self,
        _c: &ProcessConfig,
    ) -> Result<Box<dyn PluginProcessor>, PluginError> {
        if self.0.id == "test.sysex" {
            return Ok(Box::new(SysexSum));
        }
        Ok(Box::new(BrokenProcessor(self.0.id == "test.crash")))
    }
}

impl PluginProcessor for BrokenProcessor {
    fn process(&mut self, _ctx: &PluginProcessContext<'_>, _io: &mut NodeIo<'_>) -> ProcessStatus {
        if self.0 {
            // Dies without a core dump: how fast a death is noticed must not
            // depend on the system's crash handler.
            std::process::exit(70);
        }
        loop {
            std::thread::sleep(Duration::from_secs(1));
        }
    }
    fn reset(&mut self) {}
}

/// Writes each SysEx message's byte sum to the left output at its offset.
struct SysexSum;

impl PluginProcessor for SysexSum {
    fn process(&mut self, _ctx: &PluginProcessContext<'_>, io: &mut NodeIo<'_>) -> ProcessStatus {
        let out = &mut io.audio_out[0];
        out.clear();
        if let Some(midi) = io.events_in.first() {
            for e in midi.iter() {
                if let MidiEvent::SysEx(r) = e.event {
                    let sum: u32 = midi
                        .sysex(&r)
                        .unwrap_or(&[])
                        .iter()
                        .map(|&b| u32::from(b))
                        .sum();
                    out.channel_mut(0)[e.sample_offset as usize] = sum as f32;
                }
            }
        }
        ProcessStatus::Continue
    }
    fn reset(&mut self) {}
}

/// When started as a helper this "test" serves the host, then exits.
#[test]
fn helper_entry() {
    if !child::is_helper() {
        return;
    }
    let mut registry = PluginRegistry::with_builtins();
    registry.add_factory(Box::new(Broken));
    std::process::exit(child::run(registry));
}

fn launcher() -> Launcher {
    Launcher {
        exe: std::env::current_exe().unwrap(),
        args: ["helper_entry", "--exact", "--nocapture", "--test-threads=1"]
            .map(String::from)
            .to_vec(),
        env: Vec::new(),
    }
}

// --- the rig ---------------------------------------------------------------------------

const FRAMES: usize = 256;
const CONFIG: ProcessConfig = ProcessConfig {
    sample_rate: 48_000.0,
    max_block_size: FRAMES as u32,
    sidechain: false,
    double_precision: false,
};

/// Run `blocks` blocks of a sine (and `events` in the first) through `p`;
/// returns the left output and the last status.
fn run(
    p: &mut dyn PluginProcessor,
    blocks: usize,
    events: &[TimedMidiEvent],
    params: &[ParameterEvent],
) -> (Vec<f32>, ProcessStatus) {
    let mut out_l = Vec::new();
    let mut status = ProcessStatus::Continue;
    for b in 0..blocks {
        let mut input = AudioBuffer::new(ChannelLayout::Stereo, FRAMES);
        input.set_len(FRAMES);
        for c in 0..2 {
            for (i, s) in input.channel_mut(c).iter_mut().enumerate() {
                *s = (((b * FRAMES + i) as f32) * 0.03).sin() * 0.5;
            }
        }
        let mut output = AudioBuffer::new(ChannelLayout::Stereo, FRAMES);
        output.set_len(FRAMES);
        let mut midi = MidiBuffer::with_capacity(64);
        if b == 0 {
            for e in events {
                midi.push(*e).unwrap();
            }
        }
        let inputs = [input];
        let mut outputs = [output];
        let events_in = [midi];
        let mut events_out = [MidiBuffer::with_capacity(64)];
        let transport = TransportInfo {
            playing: true,
            sample_position: (b * FRAMES) as i64,
            ..TransportInfo::default()
        };
        let ctx = PluginProcessContext {
            transport: &transport,
            param_events: if b == 0 { params } else { &[] },
            harmony: &faderframe_plugin_host::NO_HARMONY,
        };
        let mut io = NodeIo {
            frames: FRAMES,
            audio_in: &inputs,
            audio_out: &mut outputs,
            events_in: &events_in,
            events_out: &mut events_out,
        };
        status = p.process(&ctx, &mut io);
        out_l.extend_from_slice(outputs[0].channel(0));
    }
    (out_l, status)
}

fn local(id: &str) -> Box<dyn PluginInstance> {
    PluginRegistry::with_builtins()
        .instantiate(PluginFormat::Builtin, id)
        .unwrap()
}

fn remote(format: PluginFormat, id: &str) -> Box<dyn PluginInstance> {
    instantiate_sandboxed(&launcher(), format, id).unwrap()
}

// --- the tests -------------------------------------------------------------------------

#[test]
fn sysex_crosses_into_the_helper() {
    let mut inst = remote(PluginFormat::Vst3, "test.sysex");
    let mut p = inst.create_processor(&CONFIG).unwrap();
    let small = [0xF0, 1, 2, 3, 0xF7];
    let mut big = vec![0x10u8; 9000];
    (big[0], big[8999]) = (0xF0, 0xF7);
    let mut midi = MidiBuffer::with_capacity(16);
    midi.push_sysex(10, &small).unwrap();
    midi.push(TimedMidiEvent::new(
        20,
        MidiEvent::NoteOn {
            channel: 0,
            key: 60,
            velocity: 100,
        },
    ))
    .unwrap();
    midi.push_sysex(30, &big).unwrap();
    let mut input = AudioBuffer::new(ChannelLayout::Stereo, FRAMES);
    input.set_len(FRAMES);
    let mut output = AudioBuffer::new(ChannelLayout::Stereo, FRAMES);
    output.set_len(FRAMES);
    let inputs = [input];
    let mut outputs = [output];
    let events = [midi];
    let transport = TransportInfo::default();
    let mut io = NodeIo {
        frames: FRAMES,
        audio_in: &inputs,
        audio_out: &mut outputs,
        events_in: &events,
        events_out: &mut [],
    };
    p.process(
        &PluginProcessContext {
            transport: &transport,
            param_events: &[],
            harmony: &faderframe_plugin_host::NO_HARMONY,
        },
        &mut io,
    );
    let out = outputs[0].channel(0);
    let sum = |b: &[u8]| b.iter().map(|&x| x as u32).sum::<u32>() as f32;
    assert_eq!(out[10], sum(&small));
    assert_eq!(out[30], sum(&big));
    assert_eq!(out.iter().filter(|v| **v != 0.0).count(), 2);
}

#[test]
fn a_sandboxed_gain_matches_the_one_in_process() {
    let mut a = local(builtin::GAIN);
    let mut b = remote(PluginFormat::Builtin, builtin::GAIN);
    assert_eq!(a.descriptor(), b.descriptor());
    assert_eq!(a.parameters(), b.parameters());
    let gain = ParameterId(0);
    for inst in [&mut a, &mut b] {
        inst.set_parameter(gain, -6.0).unwrap();
    }
    assert_eq!(b.parameter(gain), Some(-6.0));
    let auto = [ParameterEvent {
        parameter: gain,
        value: 3.0,
        sample_offset: 100,
    }];
    let (pa, pb) = (
        a.create_processor(&CONFIG).unwrap(),
        b.create_processor(&CONFIG).unwrap(),
    );
    let (mut pa, mut pb) = (pa, pb);
    let (xa, sa) = run(pa.as_mut(), 8, &[], &auto);
    let (xb, sb) = run(pb.as_mut(), 8, &[], &auto);
    assert_eq!(sa, sb);
    assert_eq!(xa, xb, "bit-identical through the helper");
    assert!(xb.iter().any(|v| v.abs() > 0.1));
    // State round trip, and the helper's own values come back with a poll.
    let state = b.save_state().unwrap();
    b.set_parameter(gain, -20.0).unwrap();
    b.load_state(&state).unwrap();
    b.poll();
    assert_eq!(b.parameter(gain), a.parameter(gain));
    assert_eq!(
        b.format_parameter(gain, -6.0),
        a.format_parameter(gain, -6.0)
    );
    // A new configuration activates anew; the old processor falls silent.
    let before = b.activation();
    let mut pc = b
        .create_processor(&ProcessConfig {
            max_block_size: 512,
            ..CONFIG
        })
        .unwrap();
    assert!(b.activation() > before);
    let (old, _) = run(pb.as_mut(), 1, &[], &[]);
    assert!(old.iter().all(|v| *v == 0.0));
    let (new, status) = run(pc.as_mut(), 2, &[], &[]);
    assert_eq!(status, ProcessStatus::Continue);
    assert!(new.iter().any(|v| v.abs() > 0.01));
}

#[test]
fn a_sandboxed_synth_plays_notes_and_note_expressions_like_in_process() {
    let notes = [
        TimedMidiEvent::new(
            10,
            MidiEvent::NoteOn {
                channel: 0,
                key: 57,
                velocity: 110,
            },
        ),
        TimedMidiEvent::new(
            10,
            MidiEvent::NoteExpression {
                channel: 0,
                key: 57,
                kind: NoteExpressionKind::Tuning,
                value: ExpressionValue::new(7.0),
            },
        ),
    ];
    let mut a = local(builtin::SYNTH);
    let mut b = remote(PluginFormat::Builtin, builtin::SYNTH);
    assert_eq!(b.note_expressions(), a.note_expressions());
    let (mut pa, mut pb) = (
        a.create_processor(&CONFIG).unwrap(),
        b.create_processor(&CONFIG).unwrap(),
    );
    let (xa, _) = run(pa.as_mut(), 20, &notes, &[]);
    let (xb, _) = run(pb.as_mut(), 20, &notes, &[]);
    assert!(xb.iter().any(|v| v.abs() > 0.05), "it plays");
    assert_eq!(xa, xb);
    // Latency is reported through the helper too.
    let mut probe = remote(PluginFormat::Builtin, builtin::LATENCY_PROBE);
    assert_eq!(probe.latency_samples(), 256);
    probe.set_parameter(ParameterId(0), 100.0).unwrap();
    let mut p = probe.create_processor(&CONFIG).unwrap();
    run(p.as_mut(), 1, &[], &[]);
    assert_eq!(probe.latency_samples(), 100);
}

#[test]
fn a_crashing_plugin_takes_only_its_own_process_down() {
    let mut inst = remote(PluginFormat::Vst3, "test.crash");
    let mut p = inst.create_processor(&CONFIG).unwrap();
    let t = Instant::now();
    let (out, status) = run(p.as_mut(), 1, &[], &[]);
    assert_eq!(status, ProcessStatus::Error);
    assert!(out.iter().all(|v| *v == 0.0));
    assert!(
        t.elapsed() < Duration::from_secs(1),
        "a death is noticed long before the deadline"
    );
    // From now on immediately.
    let t = Instant::now();
    assert_eq!(run(p.as_mut(), 3, &[], &[]).1, ProcessStatus::Error);
    assert!(t.elapsed() < Duration::from_millis(100));
    // The instance keeps answering (from what it knows) and goes quietly.
    assert_eq!(inst.poll(), Default::default());
    assert!(inst.set_parameter(ParameterId(1), 0.5).is_err());
    assert!(inst.create_processor(&CONFIG).is_err());
    assert_eq!(inst.descriptor().id, "test.crash");
    drop(p);
    drop(inst);
}

#[test]
fn a_hanging_plugin_is_given_up_after_the_deadline() {
    let mut inst = remote(PluginFormat::Vst3, "test.hang");
    let mut p = inst.create_processor(&CONFIG).unwrap();
    let t = Instant::now();
    assert_eq!(run(p.as_mut(), 1, &[], &[]).1, ProcessStatus::Error);
    let waited = t.elapsed();
    assert!(
        waited >= Duration::from_millis(200) && waited < Duration::from_secs(2),
        "{waited:?}"
    );
    let t = Instant::now();
    assert_eq!(run(p.as_mut(), 1, &[], &[]).1, ProcessStatus::Error);
    assert!(t.elapsed() < Duration::from_millis(100));
    // The poll stops the hung process; dropping does not wait for it.
    inst.poll();
    let t = Instant::now();
    drop(p);
    drop(inst);
    assert!(t.elapsed() < Duration::from_secs(1));
}

#[test]
fn unknown_plugins_fail_cleanly() {
    let err = instantiate_sandboxed(&launcher(), PluginFormat::Builtin, "nope");
    assert!(matches!(err, Err(PluginError::Failed(_))));
    // A helper that cannot start.
    let bad = Launcher {
        exe: "/nonexistent/faderframe".into(),
        args: Vec::new(),
        env: Vec::new(),
    };
    assert!(instantiate_sandboxed(&bad, PluginFormat::Builtin, builtin::GAIN).is_err());
}

#[test]
fn units_and_categories_cross_unchanged() {
    // Parameter units of every built-in survive the trip.
    for id in [builtin::SYNTH, builtin::ECHO, builtin::COMPRESSOR] {
        let a = local(id);
        let b = remote(PluginFormat::Builtin, id);
        assert_eq!(a.parameters(), b.parameters(), "{id}");
        assert!(a.parameters().iter().any(|p| p.unit != ParameterUnit::None));
    }
}

/// `cargo test --release -p faderframe-plugin-sandbox --test sandbox
/// round_trip -- --ignored --nocapture`: what a block through a helper
/// costs on top of processing it in process.
#[test]
#[ignore = "a measurement"]
fn round_trip_cost() {
    for frames in [64usize, 256] {
        let config = ProcessConfig {
            max_block_size: frames as u32,
            ..CONFIG
        };
        let mut a = local(builtin::GAIN);
        let mut b = remote(PluginFormat::Builtin, builtin::GAIN);
        let mut pa = a.create_processor(&config).unwrap();
        let mut pb = b.create_processor(&config).unwrap();
        let mut input = AudioBuffer::new(ChannelLayout::Stereo, frames);
        input.set_len(frames);
        let mut output = AudioBuffer::new(ChannelLayout::Stereo, frames);
        output.set_len(frames);
        let inputs = [input];
        let mut outputs = [output];
        let transport = TransportInfo::default();
        let mut time = |p: &mut Box<dyn PluginProcessor>| {
            let n = 20_000;
            let t = Instant::now();
            for _ in 0..n {
                let mut io = NodeIo {
                    frames,
                    audio_in: &inputs,
                    audio_out: &mut outputs,
                    events_in: &[],
                    events_out: &mut [],
                };
                p.process(
                    &PluginProcessContext {
                        transport: &transport,
                        param_events: &[],
                        harmony: &faderframe_plugin_host::NO_HARMONY,
                    },
                    &mut io,
                );
            }
            t.elapsed().as_secs_f64() / n as f64 * 1e6
        };
        let (local_us, remote_us) = (time(&mut pa), time(&mut pb));
        eprintln!(
            "{frames} frames: in process {local_us:.2} µs, sandboxed {remote_us:.2} µs per block ({:.2} % of the block's {:.0} µs)",
            100.0 * (remote_us - local_us) / (frames as f64 / 48_000.0 * 1e6),
            frames as f64 / 48_000.0 * 1e6
        );
    }
}
