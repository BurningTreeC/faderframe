//! Hosting Apple's built-in Audio Units (present on every Mac): listing,
//! rendering, parameters and sample-accurate automation, state, MIDI into
//! an instrument and the editor view's absence of a window requirement.
#![cfg(target_os = "macos")]
#![allow(clippy::unwrap_used)]

use faderframe_audio_graph::{AudioBuffer, NodeIo};
use faderframe_automation::ParameterEvent;
use faderframe_core::{ChannelLayout, ParameterId};
use faderframe_midi::{MidiBuffer, MidiEvent, TimedMidiEvent};
use faderframe_plugin_au::{AuFactory, catalog, scan};
use faderframe_plugin_host::{
    ParamMod, PluginFactory, PluginInstance, PluginProcessContext, PluginProcessor, ProcessConfig,
    TailLength,
};
use faderframe_transport::TransportInfo;

const BLOCK: usize = 256;
const CONFIG: ProcessConfig = ProcessConfig {
    sample_rate: 48_000.0,
    max_block_size: BLOCK as u32,
    sidechain: false,
    double_precision: false,
};

const DELAY: &str = "aufx:dely:appl";
const LOWPASS: &str = "aufx:lpas:appl";
const DLS: &str = "aumu:dls :appl";
const DYNAMICS: &str = "aufx:dcmp:appl";

struct Rig {
    input: Vec<AudioBuffer>,
    output: Vec<AudioBuffer>,
    events: Vec<MidiBuffer>,
    events_out: Vec<MidiBuffer>,
    transport: TransportInfo,
}

impl Rig {
    fn new() -> Self {
        let buf = || {
            let mut b = AudioBuffer::new(ChannelLayout::Stereo, BLOCK);
            b.set_len(BLOCK);
            b
        };
        Self {
            input: vec![buf()],
            output: vec![buf()],
            events: vec![MidiBuffer::with_capacity(64)],
            events_out: vec![MidiBuffer::with_capacity(64)],
            transport: TransportInfo::default(),
        }
    }

    fn run(&mut self, p: &mut dyn PluginProcessor, params: &[ParameterEvent]) -> Vec<f32> {
        self.run_with(p, params, &[])
    }

    fn run_with(
        &mut self,
        p: &mut dyn PluginProcessor,
        params: &[ParameterEvent],
        mods: &[ParamMod],
    ) -> Vec<f32> {
        let ctx = PluginProcessContext {
            transport: &self.transport,
            param_events: params,
            harmony: &faderframe_plugin_host::NO_HARMONY,
            param_mods: mods,
            note_mods: &[],
        };
        let mut io = NodeIo {
            frames: BLOCK,
            audio_in: &self.input,
            audio_out: &mut self.output,
            events_in: &self.events,
            events_out: &mut self.events_out,
        };
        p.process(&ctx, &mut io);
        self.events[0].clear();
        self.transport.sample_position += BLOCK as i64;
        self.output[0].channel(0).to_vec()
    }

    fn input(&mut self, f: impl Fn(usize) -> f32) {
        for c in 0..2 {
            for (i, s) in self.input[0].channel_mut(c).iter_mut().enumerate() {
                *s = f(i);
            }
        }
    }
}

fn instantiate(id: &str) -> Box<dyn PluginInstance> {
    AuFactory::new().instantiate(id).unwrap()
}

fn param(inst: &dyn PluginInstance, name: &str) -> ParameterId {
    inst.parameters()
        .iter()
        .find(|p| p.name.to_lowercase().contains(name))
        .unwrap_or_else(|| panic!("no parameter like {name}: {:?}", inst.parameters()))
        .id
}

#[test]
fn apples_units_are_listed() {
    let all = catalog();
    for id in [DELAY, LOWPASS, DLS] {
        let p = all
            .iter()
            .find(|p| p.id == id)
            .unwrap_or_else(|| panic!("{id} missing"));
        assert_eq!(p.vendor, "Apple");
        assert_eq!(scan::id_of(&scan::parse_id(id).unwrap()), id);
    }
    let dls = all.iter().find(|p| p.id == DLS).unwrap();
    assert!(dls.is_instrument());
    assert!(dls.audio_inputs.is_empty());
    assert_eq!(dls.note_inputs, 1);
    let delay = all.iter().find(|p| p.id == DELAY).unwrap();
    assert_eq!(delay.audio_inputs, vec![2]);
}

#[test]
fn the_delay_echoes_an_impulse() {
    let mut inst = instantiate(DELAY);
    assert!(!inst.parameters().is_empty());
    // Fully wet, 100 ms, no feedback.
    let wet = param(&*inst, "dry/wet");
    let time = param(&*inst, "delay time");
    let feedback = param(&*inst, "feedback");
    inst.set_parameter(wet, 100.0).unwrap();
    inst.set_parameter(time, 0.1).unwrap();
    inst.set_parameter(feedback, 0.0).unwrap();
    assert!((inst.parameter(time).unwrap() - 0.1).abs() < 1e-4);
    let mut p = inst.create_processor(&CONFIG).unwrap();
    assert_eq!(inst.activation(), 1);
    assert!(matches!(
        inst.tail(),
        TailLength::Samples(_) | TailLength::Infinite
    ));
    let mut rig = Rig::new();
    rig.input(|i| if i == 0 { 1.0 } else { 0.0 });
    let mut out = rig.run(&mut *p, &[]);
    rig.input(|_| 0.0);
    for _ in 0..40 {
        out.extend(rig.run(&mut *p, &[]));
    }
    let peak = out
        .iter()
        .enumerate()
        .max_by(|a, b| a.1.abs().total_cmp(&b.1.abs()))
        .unwrap();
    assert!(peak.1.abs() > 0.3, "no echo: {}", peak.1);
    let expected = 4800;
    assert!(
        peak.0.abs_diff(expected) < 64,
        "echo at {} (expected about {expected})",
        peak.0
    );
}

#[test]
fn automation_reaches_the_unit() {
    let mut inst = instantiate(LOWPASS);
    let cutoff = param(&*inst, "cutoff");
    let mut p = inst.create_processor(&CONFIG).unwrap();
    let mut rig = Rig::new();
    // A 6 kHz tone (eight samples a period: continuous from block to block,
    // no DC) through the filter.
    rig.input(|i| (i as f32 * std::f32::consts::TAU / 8.0).sin());
    let open = ParameterEvent {
        parameter: cutoff,
        value: 20_000.0,
        sample_offset: 0,
    };
    let closed = ParameterEvent {
        parameter: cutoff,
        value: 40.0,
        sample_offset: 0,
    };
    let energy = |v: &[f32]| v.iter().map(|s| s * s).sum::<f32>();
    let mut loud = 0.0;
    for _ in 0..8 {
        loud = energy(&rig.run(&mut *p, &[open]));
    }
    let mut quiet = 0.0;
    for _ in 0..8 {
        quiet = energy(&rig.run(&mut *p, &[closed]));
    }
    // Apple's filter is gentle at 6 kHz (energy falls about tenfold), so
    // this checks that the automation arrives, not the filter's slope.
    let range = inst
        .parameters()
        .iter()
        .find(|p| p.id == cutoff)
        .map(|p| (p.min, p.max));
    assert!(
        quiet < loud * 0.5,
        "cutoff automation had no effect: {loud} → {quiet} (range {range:?})"
    );
}

/// Modulators move an Audio Unit's parameter by setting the modulated
/// value on the unit; the value as set stays what the host shows and
/// saves, and comes back when the modulation ends.
#[test]
fn modulation_moves_the_unit_and_keeps_the_value_as_set() {
    let mut inst = instantiate(LOWPASS);
    let cutoff = param(&*inst, "cutoff");
    assert!(inst.modulatable(cutoff));
    inst.set_parameter(cutoff, 40.0).unwrap();
    let info = inst
        .parameters()
        .iter()
        .find(|p| p.id == cutoff)
        .cloned()
        .unwrap();
    let mut p = inst.create_processor(&CONFIG).unwrap();
    let mut rig = Rig::new();
    rig.input(|i| (i as f32 * std::f32::consts::TAU / 8.0).sin());
    let energy = |v: &[f32]| v.iter().map(|s| s * s).sum::<f32>();
    let mut quiet = 0.0;
    for _ in 0..8 {
        quiet = energy(&rig.run(&mut *p, &[]));
    }
    let open = [ParamMod {
        parameter: cutoff,
        share: 1.0,
        amount: (info.max - info.min) as f32,
    }];
    let mut loud = 0.0;
    for _ in 0..8 {
        loud = energy(&rig.run_with(&mut *p, &[], &open));
    }
    assert!(
        quiet < loud * 0.5,
        "modulation had no effect: {quiet} → {loud}"
    );
    assert!(
        (inst.parameter(cutoff).unwrap() - 40.0).abs() < 1e-3,
        "the value as set"
    );
    let state = inst.save_state().unwrap();
    assert!(state.starts_with(b"FFAU"), "saved with the values as set");
    let mut again = 0.0;
    for _ in 0..8 {
        again = energy(&rig.run(&mut *p, &[]));
    }
    assert!(again < loud * 0.5, "back to the value as set: {again}");
    let mut b = instantiate(LOWPASS);
    b.load_state(&state).unwrap();
    assert!((b.parameter(cutoff).unwrap() - 40.0).abs() < 1e-3);
}

#[test]
fn state_round_trips_through_class_info() {
    let mut a = instantiate(DELAY);
    let time = param(&*a, "delay time");
    a.set_parameter(time, 0.37).unwrap();
    let state = a.save_state().unwrap();
    assert!(state.starts_with(b"bplist"), "binary property list");
    let mut b = instantiate(DELAY);
    assert!((b.parameter(time).unwrap() - 0.37).abs() > 1e-3);
    b.load_state(&state).unwrap();
    assert!((b.parameter(time).unwrap() - 0.37).abs() < 1e-4);
    // An .aupreset is the same property list (in XML; any format loads).
    assert_eq!(b.state_from_preset_file(&state).unwrap(), state);
    assert!(b.load_state(b"not a plist").is_err());
}

#[test]
fn notes_play_the_dls_synth() {
    let mut inst = instantiate(DLS);
    let mut p = inst.create_processor(&CONFIG).unwrap();
    let mut rig = Rig::new();
    let silent = rig.run(&mut *p, &[]);
    assert!(silent.iter().all(|s| s.abs() < 1e-6));
    rig.events[0]
        .push(TimedMidiEvent {
            sample_offset: 10,
            event: MidiEvent::NoteOn {
                channel: 0,
                key: 60,
                velocity: 110,
            },
        })
        .unwrap();
    let mut out = Vec::new();
    for _ in 0..20 {
        out.extend(rig.run(&mut *p, &[]));
    }
    let peak = out.iter().fold(0.0f32, |m, s| m.max(s.abs()));
    assert!(peak > 0.01, "the synth stayed silent ({peak})");
}

#[test]
fn reactivation_follows_the_configuration() {
    let mut inst = instantiate(DELAY);
    let _p = inst.create_processor(&CONFIG).unwrap();
    let _same = inst.create_processor(&CONFIG).unwrap();
    assert_eq!(inst.activation(), 1, "same configuration: same activation");
    let other = ProcessConfig {
        sample_rate: 44_100.0,
        ..CONFIG
    };
    let mut p = inst.create_processor(&other).unwrap();
    assert_eq!(inst.activation(), 2);
    let mut rig = Rig::new();
    rig.input(|_| 0.5);
    rig.run(&mut *p, &[]);
    assert!(inst.latency_samples() < 48_000);
}

/// AUSplitter (a format converter, not in the catalog): one input, two
/// output elements carrying it. The graph's second output is element 1.
#[test]
fn extra_output_elements_reach_the_graph() {
    const SPLITTER: &str = "aufc:splt:appl";
    let p = faderframe_plugin_host::scan::ScannedPlugin {
        id: SPLITTER.into(),
        name: "AUSplitter".into(),
        vendor: "Apple".into(),
        version: String::new(),
        features: Vec::new(),
        bundle: std::path::PathBuf::new(),
        audio_inputs: vec![2],
        audio_outputs: vec![2],
        note_inputs: 0,
        note_outputs: 0,
    };
    let mut inst = match faderframe_plugin_au::AuInstance::new(&p) {
        Ok(i) => i,
        Err(e) => {
            eprintln!("no AUSplitter here: {e}");
            return;
        }
    };
    assert_eq!(
        inst.descriptor().audio_outputs.len(),
        2,
        "both output elements are described"
    );
    assert_eq!(inst.output_bus_names().len(), 2);
    inst.configure_outputs(2);
    let mut proc = inst.create_processor(&CONFIG).unwrap();
    let mut rig = Rig::new();
    rig.output.push({
        let mut b = AudioBuffer::new(ChannelLayout::Stereo, BLOCK);
        b.set_len(BLOCK);
        b
    });
    rig.input(|i| 0.5 * (i as f32 * 0.05).sin());
    rig.run(proc.as_mut(), &[]);
    let input = rig.input[0].channel(0).to_vec();
    for (bus, out) in rig.output.iter().enumerate() {
        let got = out.channel(0);
        let err = got
            .iter()
            .zip(&input)
            .fold(0f32, |m, (a, b)| m.max((a - b).abs()));
        assert!(err < 1e-5, "output {bus} carries the input (error {err})");
    }
    // Back to the main element only: the processor is made again.
    let before = inst.activation();
    inst.configure_outputs(1);
    let _ = inst.create_processor(&CONFIG).unwrap();
    assert!(inst.activation() > before);
}

/// Apple's Dynamics Processor reports its "Compression Amount" meter; it
/// reaches the host after each render, for the mixer's gain-reduction
/// meter.
#[test]
fn the_dynamics_processor_reports_its_gain_reduction() {
    let mut inst = instantiate(DYNAMICS);
    let threshold = param(&*inst, "threshold");
    inst.set_parameter(threshold, -40.0).unwrap();
    let mut p = inst.create_processor(&CONFIG).unwrap();
    let cell = inst.reduction().expect("a compression meter");
    let mut rig = Rig::new();
    rig.input(|i| 0.5 * (i as f32 * 0.05).sin());
    for _ in 0..40 {
        rig.run(p.as_mut(), &[]);
    }
    let r = cell.get().expect("reported");
    assert!(r > 1.0, "{r} dB");
}
