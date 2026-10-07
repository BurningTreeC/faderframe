//! Real LV2 bundles (opt-in): `FADERFRAME_TEST_LV2_PATH=<dirs>` with the
//! LV2 book's examples (eg-amp, eg-midigate, eg-fifths, eg-metro,
//! eg-sampler) and DPF's (d_parameters, d_states, d_latency) built there.
#![allow(clippy::unwrap_used)]

use faderframe_audio_graph::{AudioBuffer, NodeIo};
use faderframe_automation::ParameterEvent;
use faderframe_core::{ChannelLayout, ParameterId};
use faderframe_midi::{MidiBuffer, MidiEvent, TimedMidiEvent};
use faderframe_plugin_host::{
    PluginFactory, PluginInstance, PluginProcessContext, PluginProcessor, ProcessConfig,
};
use faderframe_plugin_lv2::Lv2Factory;
use faderframe_transport::TransportInfo;

const RATE: f64 = 44_100.0;
const BLOCK: usize = 256;

fn factory() -> Option<Lv2Factory> {
    let paths = std::env::var_os("FADERFRAME_TEST_LV2_PATH")?;
    let paths: Vec<_> = std::env::split_paths(&paths).collect();
    faderframe_plugin_lv2::set_catalog(faderframe_plugin_lv2::scan::scan(&paths));
    assert!(
        !faderframe_plugin_lv2::catalog().is_empty(),
        "no plugins on the path"
    );
    Some(Lv2Factory::new())
}

fn config() -> ProcessConfig {
    ProcessConfig {
        sample_rate: RATE,
        max_block_size: BLOCK as u32,
        sidechain: false,
        double_precision: false,
    }
}

struct Run {
    inputs: Vec<AudioBuffer>,
    outputs: Vec<AudioBuffer>,
    midi_in: Vec<MidiBuffer>,
    midi_out: Vec<MidiBuffer>,
    transport: TransportInfo,
}

impl Run {
    fn new(ins: usize, outs: usize) -> Run {
        let buf = |n: usize| {
            (n > 0)
                .then(|| AudioBuffer::new(ChannelLayout::from_channel_count(n), BLOCK))
                .into_iter()
                .collect::<Vec<_>>()
        };
        let mut inputs = buf(ins);
        for b in &mut inputs {
            b.set_len(BLOCK);
        }
        let mut outputs = buf(outs);
        for b in &mut outputs {
            b.set_len(BLOCK);
        }
        Run {
            inputs,
            outputs,
            midi_in: vec![MidiBuffer::with_capacity(64)],
            midi_out: vec![MidiBuffer::with_capacity(64)],
            transport: TransportInfo {
                sample_rate: RATE,
                ..TransportInfo::default()
            },
        }
    }

    /// A sine at 0.5 into every input channel.
    fn sine(&mut self) {
        for b in &mut self.inputs {
            for c in 0..b.num_channels() {
                for (i, s) in b.channel_mut(c).iter_mut().enumerate() {
                    *s = 0.5 * (i as f32 * 0.05).sin();
                }
            }
        }
    }

    fn block(&mut self, p: &mut dyn PluginProcessor, events: &[ParameterEvent]) {
        for m in &mut self.midi_out {
            m.clear();
        }
        let mut io = NodeIo {
            frames: BLOCK,
            audio_in: &self.inputs,
            audio_out: &mut self.outputs,
            events_in: &self.midi_in,
            events_out: &mut self.midi_out,
        };
        let ctx = PluginProcessContext {
            transport: &self.transport,
            param_events: events,
            harmony: &faderframe_plugin_host::NO_HARMONY,
            param_mods: &[],
            note_mods: &[],
        };
        p.process(&ctx, &mut io);
        if self.transport.playing {
            self.transport.sample_position += BLOCK as i64;
            self.transport.quarter_position += BLOCK as f64 / RATE * self.transport.tempo / 60.0;
        }
        self.midi_in[0].clear();
    }

    fn peak(&self) -> f32 {
        self.outputs
            .iter()
            .flat_map(|b| (0..b.num_channels()).flat_map(move |c| b.channel(c).iter()))
            .fold(0f32, |m, s| m.max(s.abs()))
    }

    fn note(&mut self, at: u32, on: bool, key: u8) {
        let ev = if on {
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
        self.midi_in[0].push(TimedMidiEvent::new(at, ev)).unwrap();
    }
}

fn open(f: &Lv2Factory, uri: &str) -> Option<Box<dyn PluginInstance>> {
    match f.instantiate(uri) {
        Ok(i) => {
            eprintln!("testing {uri}");
            Some(i)
        }
        Err(e) => {
            eprintln!("skipping {uri}: {e}");
            None
        }
    }
}

#[test]
#[ignore = "needs LV2 bundles"]
fn the_example_plugins_play() {
    let Some(f) = factory() else { return };
    // eg-amp: gain in dB, sample-accurate changes inside a block.
    if let Some(mut amp) = open(&f, "http://lv2plug.in/plugins/eg-amp") {
        assert_eq!(amp.parameters().len(), 1);
        assert_eq!(amp.parameters()[0].name, "Gain");
        let mut p = amp.create_processor(&config()).unwrap();
        let mut r = Run::new(1, 1);
        r.sine();
        r.block(p.as_mut(), &[]);
        let unity = r.peak();
        assert!((unity - 0.5).abs() < 0.01, "unity gain: {unity}");
        r.block(
            p.as_mut(),
            &[ParameterEvent {
                parameter: ParameterId(0),
                value: -6.0206,
                sample_offset: 128,
            }],
        );
        let out = r.outputs[0].channel(0);
        let first = out[..128].iter().fold(0f32, |m, s| m.max(s.abs()));
        let second = out[128..].iter().fold(0f32, |m, s| m.max(s.abs()));
        assert!(
            first > 0.45 && (second - 0.25).abs() < 0.01,
            "{first} {second}"
        );
        assert_eq!(amp.parameter(ParameterId(0)).map(|v| v.round()), Some(-6.0));
        // State: the port by symbol.
        let saved = amp.save_state().unwrap();
        amp.set_parameter(ParameterId(0), 3.0).unwrap();
        amp.load_state(&saved).unwrap();
        assert_eq!(amp.parameter(ParameterId(0)).map(|v| v.round()), Some(-6.0));
    }
    // eg-midigate: audio only while a note is held.
    if let Some(mut gate) = open(&f, "http://lv2plug.in/plugins/eg-midigate") {
        let mut p = gate.create_processor(&config()).unwrap();
        let mut r = Run::new(1, 1);
        r.sine();
        r.block(p.as_mut(), &[]);
        assert_eq!(r.peak(), 0.0, "closed");
        r.note(64, true, 60);
        r.block(p.as_mut(), &[]);
        let out = r.outputs[0].channel(0);
        assert!(out[..64].iter().all(|s| *s == 0.0));
        assert!(
            out[64..].iter().any(|s| s.abs() > 0.1),
            "open from the note on"
        );
        r.note(0, false, 60);
        r.block(p.as_mut(), &[]);
        assert_eq!(r.peak(), 0.0, "closed again");
    }
    // eg-fifths: each note comes out with its fifth.
    if let Some(mut fifths) = open(&f, "http://lv2plug.in/plugins/eg-fifths") {
        let mut p = fifths.create_processor(&config()).unwrap();
        let mut r = Run::new(0, 0);
        r.note(10, true, 60);
        r.block(p.as_mut(), &[]);
        let keys: Vec<(u32, u8)> = r.midi_out[0]
            .iter()
            .filter_map(|e| match e.event {
                MidiEvent::NoteOn { key, .. } => Some((e.sample_offset, key)),
                _ => None,
            })
            .collect();
        assert_eq!(keys, [(10, 60), (10, 67)]);
    }
    // eg-metro: clicks while the transport plays.
    if let Some(mut metro) = open(&f, "http://lv2plug.in/plugins/eg-metro") {
        let mut p = metro.create_processor(&config()).unwrap();
        let mut r = Run::new(0, 1);
        r.block(p.as_mut(), &[]);
        assert_eq!(r.peak(), 0.0, "stopped: silent");
        r.transport.playing = true;
        let mut heard = 0f32;
        for _ in 0..8 {
            r.block(p.as_mut(), &[]);
            heard = heard.max(r.peak());
        }
        assert!(heard > 0.05, "playing: clicks ({heard})");
    }
    // eg-sampler: the default state names click.wav, loaded by the worker.
    if let Some(mut sampler) = open(&f, "http://lv2plug.in/plugins/eg-sampler") {
        let mut p = sampler.create_processor(&config()).unwrap();
        let mut r = Run::new(0, 1);
        let mut heard = 0f32;
        for i in 0..40 {
            if i == 4 {
                r.note(0, true, 60);
            }
            r.block(p.as_mut(), &[]);
            heard = heard.max(r.peak());
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        assert!(heard > 0.05, "the sample plays ({heard})");
        let state = sampler.save_state().unwrap();
        let text = String::from_utf8_lossy(&state);
        assert!(text.contains("click.wav"), "its sample is in the state");
        sampler.load_state(&state).unwrap();
    }
}

#[test]
#[ignore = "needs LV2 bundles"]
fn dpf_plugins_report_latency_presets_and_state() {
    let Some(f) = factory() else { return };
    if let Some(mut lat) = open(&f, "http://distrho.sf.net/examples/Latency") {
        let _p = lat.create_processor(&config()).unwrap();
        // Its "latency" parameter is 1 s by default.
        assert_eq!(lat.latency_samples(), RATE as u32);
    }
    if let Some(mut params) = open(&f, "http://distrho.sf.net/examples/Parameters") {
        let names = params.programs();
        assert_eq!(names, ["Custom", "Default"]);
        let _p = params.create_processor(&config()).unwrap();
        params.select_program(0).unwrap();
        let values: Vec<f64> = params
            .parameters()
            .iter()
            .map(|p| p.id)
            .collect::<Vec<_>>()
            .into_iter()
            .filter_map(|id| params.parameter(id))
            .collect();
        assert_eq!(&values[..3], &[1.0, 1.0, 0.0], "{values:?}");
        assert_eq!(params.current_program(), Some(0));
        params.select_program(1).unwrap();
        let first = params.parameter(params.parameters()[0].id);
        assert_eq!(first, Some(0.0));
        assert!(params.editor().is_some(), "it has an X11 UI");
    }
    if let Some(mut states) = open(&f, "http://distrho.sf.net/examples/States") {
        let _p = states.create_processor(&config()).unwrap();
        let before = states.save_state().unwrap();
        states.select_program(0).unwrap();
        let after = states.save_state().unwrap();
        assert_ne!(before, after, "the preset changed its state");
        states.load_state(&before).unwrap();
        assert_eq!(states.save_state().unwrap(), before);
    }
}
