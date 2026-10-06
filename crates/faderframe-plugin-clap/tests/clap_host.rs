#![allow(clippy::unwrap_used)]
//! Hosting a real CLAP plugin (built with `clack-plugin`, loaded without a
//! shared library): parameters, state, activation and processing with
//! sample-accurate parameter events.

use clack_extensions::audio_ports::{
    AudioPortFlags, AudioPortInfo, AudioPortInfoWriter, AudioPortType, PluginAudioPorts,
    PluginAudioPortsImpl,
};
use clack_extensions::params::{
    ParamDisplayWriter, ParamInfo, ParamInfoFlags, ParamInfoWriter, PluginAudioProcessorParams,
    PluginMainThreadParams, PluginParams,
};
use clack_extensions::state::{PluginState, PluginStateImpl};
use clack_plugin::events::spaces::CoreEventSpace;
use clack_plugin::prelude::*;
use clack_plugin::stream::{InputStream, OutputStream};
use faderframe_audio_graph::{AudioBuffer, NodeIo};
use faderframe_automation::ParameterEvent;
use faderframe_core::{ChannelLayout, ParameterId, db_to_gain};
use faderframe_plugin_clap::ClapFactory;
use faderframe_plugin_clap::scan::ScannedPlugin;
use faderframe_plugin_host::{PluginFactory, PluginProcessContext, ProcessConfig};
use std::ffi::CStr;
use std::io::{Read, Write as _};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

/// Note events the test plugin received: (what, note id, key, value).
static NOTES: Mutex<Vec<(String, i32, i16, f64)>> = Mutex::new(Vec::new());
/// The sample width of the last block the test plugin processed (32/64).
static WIDTH: AtomicU64 = AtomicU64::new(0);
/// The tests share the statics above: one at a time.
static SERIAL: Mutex<()> = Mutex::new(());

// --- the test plugin -------------------------------------------------------------

struct TestGain;

pub struct GainShared {
    gain_db: AtomicU64,
    /// Modulation of every voice (dB on top of the gain).
    gain_mod: AtomicU64,
}

impl GainShared {
    fn get(&self) -> f64 {
        f64::from_bits(self.gain_db.load(Ordering::Relaxed))
    }
    fn modulation(&self) -> f64 {
        f64::from_bits(self.gain_mod.load(Ordering::Relaxed))
    }
    fn set(&self, v: f64) {
        self.gain_db
            .store(v.clamp(-60.0, 12.0).to_bits(), Ordering::Relaxed);
    }
    fn apply(&self, events: &InputEvents) {
        let note = |what: String, p: clack_plugin::events::Pckn, v: f64| {
            NOTES
                .lock()
                .unwrap()
                .push((what, p.raw_note_id(), p.raw_key(), v));
        };
        for e in events {
            match e.as_core_event() {
                Some(CoreEventSpace::ParamValue(p)) => self.set(p.value()),
                // Modulation: ("mod", note id, key, amount); a global one
                // moves the gain.
                Some(CoreEventSpace::ParamMod(m)) => {
                    note("mod".into(), m.pckn(), m.amount());
                    if m.pckn().raw_key() < 0 {
                        self.gain_mod.store(m.amount().to_bits(), Ordering::Relaxed);
                    }
                }
                Some(CoreEventSpace::NoteOn(n)) => note("on".into(), n.pckn(), n.velocity()),
                Some(CoreEventSpace::NoteOff(n)) => note("off".into(), n.pckn(), 0.0),
                Some(CoreEventSpace::NoteExpression(x)) => {
                    note(format!("{:?}", x.expression_type()), x.pckn(), x.value())
                }
                // SysEx: ("sysex", size, second byte, byte sum).
                Some(CoreEventSpace::MidiSysEx(m)) => {
                    // SAFETY: the host keeps the bytes alive for the call.
                    let data = unsafe { m.data() };
                    let sum: f64 = data.iter().map(|&b| f64::from(b)).sum();
                    NOTES.lock().unwrap().push((
                        "sysex".into(),
                        data.len() as i32,
                        data.get(1).copied().unwrap_or(0) as i16,
                        sum,
                    ));
                }
                _ => {}
            }
        }
    }
}

impl PluginShared<'_> for GainShared {}

pub struct GainMain<'a> {
    shared: &'a GainShared,
}

impl<'a> PluginMainThread<'a, GainShared> for GainMain<'a> {}

impl PluginMainThreadParams for GainMain<'_> {
    fn count(&self) -> u32 {
        1
    }

    fn get_info(&self, index: u32, info: &mut ParamInfoWriter) {
        if index == 0 {
            info.set(&ParamInfo {
                id: ClapId::new(0),
                flags: ParamInfoFlags::IS_AUTOMATABLE
                    | ParamInfoFlags::IS_MODULATABLE
                    | ParamInfoFlags::IS_MODULATABLE_PER_NOTE_ID
                    | ParamInfoFlags::IS_MODULATABLE_PER_KEY,
                cookie: Default::default(),
                name: b"Gain",
                module: b"",
                min_value: -60.0,
                max_value: 12.0,
                default_value: 0.0,
            });
        }
    }

    fn get_value(&self, id: ClapId) -> Option<f64> {
        (id == ClapId::new(0)).then(|| self.shared.get())
    }

    fn value_to_text(
        &self,
        _id: ClapId,
        value: f64,
        writer: &mut ParamDisplayWriter,
    ) -> std::fmt::Result {
        std::fmt::Write::write_fmt(writer, format_args!("{value:.1} dB"))
    }

    fn text_to_value(&self, _id: ClapId, text: &CStr) -> Option<f64> {
        text.to_str().ok()?.trim_end_matches(" dB").parse().ok()
    }

    fn flush(&self, input: &InputEvents, _output: &mut OutputEvents) {
        self.shared.apply(input);
    }
}

/// One stereo port each way, 64-bit capable.
impl PluginAudioPortsImpl for GainMain<'_> {
    fn count(&self, _is_input: bool) -> u32 {
        1
    }

    fn get(&self, index: u32, is_input: bool, writer: &mut AudioPortInfoWriter) {
        if index == 0 {
            writer.set(&AudioPortInfo {
                id: ClapId::new(0),
                name: if is_input { b"In" } else { b"Out" },
                channel_count: 2,
                flags: AudioPortFlags::IS_MAIN | AudioPortFlags::SUPPORTS_64BITS,
                port_type: Some(AudioPortType::STEREO),
                in_place_pair: None,
            });
        }
    }
}

impl PluginStateImpl for GainMain<'_> {
    fn save(&self, output: &mut OutputStream) -> Result<(), PluginError> {
        output.write_all(&self.shared.get().to_le_bytes())?;
        Ok(())
    }

    fn load(&self, input: &mut InputStream) -> Result<(), PluginError> {
        let mut b = [0u8; 8];
        input.read_exact(&mut b)?;
        self.shared.set(f64::from_le_bytes(b));
        Ok(())
    }
}

pub struct GainProcessor<'a> {
    shared: &'a GainShared,
}

impl<'a> PluginAudioProcessor<'a, GainShared, GainMain<'a>> for GainProcessor<'a> {
    fn activate(
        _host: HostAudioProcessorHandle<'a>,
        _main: &GainMain<'a>,
        shared: &'a GainShared,
        _config: PluginAudioConfiguration,
    ) -> Result<Self, PluginError> {
        Ok(Self { shared })
    }

    fn process(
        &mut self,
        _process: Process,
        mut audio: Audio,
        events: Events,
    ) -> Result<ProcessStatus, PluginError> {
        self.shared.apply(events.input);
        let gain = db_to_gain((self.shared.get() + self.shared.modulation()) as f32);
        let mut port = audio.port_pair(0).ok_or(PluginError::Message("no port"))?;
        let channels = port.channels()?;
        let mut channels = match channels.into_f64() {
            Some(mut channels) => {
                WIDTH.store(64, Ordering::Relaxed);
                let gain = f64::from(gain);
                for pair in channels.iter_mut() {
                    match pair {
                        ChannelPair::InputOutput(i, o) => {
                            for (o, i) in o.iter_mut().zip(i.iter()) {
                                *o = *i * gain;
                            }
                        }
                        ChannelPair::InPlace(b) => {
                            for s in b.iter_mut() {
                                *s *= gain;
                            }
                        }
                        ChannelPair::OutputOnly(o) => o.fill(0.0),
                        ChannelPair::InputOnly(_) => {}
                    }
                }
                return Ok(ProcessStatus::Continue);
            }
            None => port
                .channels()?
                .into_f32()
                .ok_or(PluginError::Message("not f32"))?,
        };
        WIDTH.store(32, Ordering::Relaxed);
        for pair in channels.iter_mut() {
            match pair {
                ChannelPair::InputOutput(i, o) => {
                    for (o, i) in o.iter_mut().zip(i.iter()) {
                        *o = *i * gain;
                    }
                }
                ChannelPair::InPlace(b) => {
                    for s in b.iter_mut() {
                        *s *= gain;
                    }
                }
                ChannelPair::OutputOnly(o) => o.fill(0.0),
                ChannelPair::InputOnly(_) => {}
            }
        }
        Ok(ProcessStatus::Continue)
    }
}

impl PluginAudioProcessorParams for GainProcessor<'_> {
    fn flush(&mut self, input: &InputEvents, _output: &mut OutputEvents) {
        self.shared.apply(input);
    }
}

impl Plugin for TestGain {
    type AudioProcessor<'a> = GainProcessor<'a>;
    type Shared<'a> = GainShared;
    type MainThread<'a> = GainMain<'a>;

    fn declare_extensions(builder: &mut PluginExtensions<Self>, _shared: Option<&GainShared>) {
        builder
            .register::<PluginParams>()
            .register::<PluginState>()
            .register::<PluginAudioPorts>();
    }
}

impl DefaultPluginFactory for TestGain {
    fn get_descriptor() -> PluginDescriptor {
        use clack_plugin::plugin::features::*;
        PluginDescriptor::new("org.faderframe.test-gain", "Test Gain")
            .with_features([AUDIO_EFFECT, STEREO])
    }

    fn new_shared(_host: HostSharedHandle<'_>) -> Result<GainShared, PluginError> {
        Ok(GainShared {
            gain_db: AtomicU64::new(0f64.to_bits()),
            gain_mod: AtomicU64::new(0f64.to_bits()),
        })
    }

    fn new_main_thread<'a>(
        _host: HostMainThreadHandle<'a>,
        shared: &'a GainShared,
    ) -> Result<GainMain<'a>, PluginError> {
        Ok(GainMain { shared })
    }
}

// --- the tests --------------------------------------------------------------------------

const BUNDLE: &CStr = c"/faderframe-test/test-gain.clap";

fn factory() -> ClapFactory {
    let entry =
        clack_host::prelude::PluginEntry::load_from_clack::<SinglePluginEntry<TestGain>>(BUNDLE)
            .unwrap();
    faderframe_plugin_clap::set_catalog(vec![ScannedPlugin {
        id: "org.faderframe.test-gain".into(),
        name: "Test Gain".into(),
        vendor: "FaderFrame".into(),
        version: "1".into(),
        features: vec!["audio-effect".into(), "stereo".into()],
        bundle: BUNDLE.to_str().unwrap().into(),
        audio_inputs: vec![2],
        audio_outputs: vec![2],
        note_inputs: 0,
        note_outputs: 0,
    }]);
    ClapFactory::new().with_entry(BUNDLE.to_str().unwrap().into(), entry)
}

fn run_block(
    proc: &mut dyn faderframe_plugin_host::PluginProcessor,
    events: &[ParameterEvent],
    frames: usize,
) -> Vec<f32> {
    run_block_modulated(proc, events, &[], frames)
}

fn run_block_modulated(
    proc: &mut dyn faderframe_plugin_host::PluginProcessor,
    events: &[ParameterEvent],
    mods: &[faderframe_plugin_host::ParamMod],
    frames: usize,
) -> Vec<f32> {
    let mut input = AudioBuffer::new(ChannelLayout::Stereo, frames);
    input.set_len(frames);
    for c in 0..2 {
        input.channel_mut(c).fill(0.5);
    }
    let mut output = AudioBuffer::new(ChannelLayout::Stereo, frames);
    output.set_len(frames);
    let inputs = [input];
    let mut outputs = [output];
    let mut io = NodeIo {
        frames,
        audio_in: &inputs,
        audio_out: &mut outputs,
        events_in: &[],
        events_out: &mut [],
    };
    let transport = faderframe_transport::TransportInfo::default();
    let ctx = PluginProcessContext {
        transport: &transport,
        param_events: events,
        harmony: &faderframe_plugin_host::NO_HARMONY,
        param_mods: mods,
        note_mods: &[],
    };
    let status = proc.process(&ctx, &mut io);
    assert_ne!(status, faderframe_plugin_host::ProcessStatus::Error);
    outputs[0].channel(0).to_vec()
}

#[test]
fn notes_carry_ids_and_note_expressions_reach_their_keys() {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    use faderframe_midi::{
        ExpressionValue, MidiBuffer, MidiEvent, NoteExpressionKind, TimedMidiEvent,
    };
    let f = factory();
    let mut inst = f.instantiate("org.faderframe.test-gain").unwrap();
    let mut proc = inst
        .create_processor(&ProcessConfig {
            sample_rate: 48_000.0,
            max_block_size: 64,
            sidechain: false,
            double_precision: false,
        })
        .unwrap();
    NOTES.lock().unwrap().clear();
    let expr = |at, key, kind, v| {
        TimedMidiEvent::new(
            at,
            MidiEvent::NoteExpression {
                channel: 0,
                key,
                kind,
                value: ExpressionValue::new(v),
            },
        )
    };
    let mut midi = MidiBuffer::with_capacity(16);
    for e in [
        TimedMidiEvent::new(
            0,
            MidiEvent::NoteOn {
                channel: 0,
                key: 60,
                velocity: 127,
            },
        ),
        TimedMidiEvent::new(
            0,
            MidiEvent::NoteOn {
                channel: 0,
                key: 62,
                velocity: 127,
            },
        ),
        expr(0, 60, NoteExpressionKind::Tuning, 2.0),
        expr(10, 60, NoteExpressionKind::Volume, -6.0),
        expr(10, 62, NoteExpressionKind::Pan, 0.5),
        TimedMidiEvent::new(
            20,
            MidiEvent::NoteOff {
                channel: 0,
                key: 60,
                velocity: 0,
            },
        ),
    ] {
        midi.push(e).unwrap();
    }
    midi.push_sysex(3, &[0xF0, 0x7E, 0x7F, 0x06, 0x01, 0xF7])
        .unwrap();
    let input = {
        let mut b = AudioBuffer::new(ChannelLayout::Stereo, 64);
        b.set_len(64);
        b
    };
    let mut outputs = [{
        let mut b = AudioBuffer::new(ChannelLayout::Stereo, 64);
        b.set_len(64);
        b
    }];
    let inputs = [input];
    let events = [midi];
    let mut io = NodeIo {
        frames: 64,
        audio_in: &inputs,
        audio_out: &mut outputs,
        events_in: &events,
        events_out: &mut [],
    };
    let transport = faderframe_transport::TransportInfo::default();
    proc.process(
        &PluginProcessContext {
            transport: &transport,
            param_events: &[],
            harmony: &faderframe_plugin_host::NO_HARMONY,
            param_mods: &[],
            note_mods: &[],
        },
        &mut io,
    );
    let seen = NOTES.lock().unwrap().clone();
    let id_of = |what: &str, key: i16| {
        seen.iter()
            .find(|n| n.0 == what && n.2 == key)
            .unwrap_or_else(|| panic!("{what} {key}: {seen:?}"))
            .clone()
    };
    let (a, b) = (id_of("on", 60).1, id_of("on", 62).1);
    assert!(a >= 0 && b >= 0 && a != b, "{seen:?}");
    // Expressions address their note by key and channel (note id −1).
    let tuning = id_of("Some(Tuning)", 60);
    assert_eq!((tuning.1, tuning.3), (-1, 2.0));
    let volume = id_of("Some(Volume)", 60);
    assert!(
        (volume.3 - db_to_gain(-6.0) as f64).abs() < 1e-3,
        "{volume:?}"
    );
    let pan = id_of("Some(Pan)", 62);
    assert_eq!((pan.1, pan.3), (-1, 0.75));
    assert_eq!(id_of("off", 60).1, a);
    // SysEx with its bytes (an identity request).
    let sysex = id_of("sysex", 0x7E);
    assert_eq!(
        (sysex.1, sysex.3),
        (6, f64::from(0xF0u32 + 0x7E + 0x7F + 0x06 + 0x01 + 0xF7))
    );
}

#[test]
fn modulation_moves_the_plugin_not_its_value_and_reaches_single_voices() {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    use faderframe_midi::{MidiBuffer, MidiEvent, TimedMidiEvent};
    use faderframe_plugin_host::{NoteParamMod, ParamMod};
    let f = factory();
    let mut inst = f.instantiate("org.faderframe.test-gain").unwrap();
    let gain = ParameterId(0);
    assert!(inst.modulatable(gain) && inst.modulatable_per_note(gain));
    let mut proc = inst
        .create_processor(&ProcessConfig {
            sample_rate: 48_000.0,
            max_block_size: 64,
            sidechain: false,
            double_precision: false,
        })
        .unwrap();
    NOTES.lock().unwrap().clear();
    // Every voice: −6 dB on top of the gain, which stays where it was.
    let m = ParamMod {
        parameter: gain,
        share: -6.0 / 72.0,
        amount: -6.0,
    };
    let out = run_block_modulated(proc.as_mut(), &[], &[m], 64);
    assert!(
        (out[10] - 0.5 * db_to_gain(-6.0)).abs() < 1e-4,
        "{}",
        out[10]
    );
    assert_eq!(inst.parameter(gain), Some(0.0));
    // The same again is not sent again; gone, it is reset to nothing.
    run_block_modulated(proc.as_mut(), &[], &[m], 64);
    let out = run_block(proc.as_mut(), &[], 64);
    assert!((out[10] - 0.5).abs() < 1e-4);
    let mods: Vec<f64> = NOTES
        .lock()
        .unwrap()
        .iter()
        .filter(|n| n.0 == "mod")
        .map(|n| n.3)
        .collect();
    assert_eq!(mods, [-6.0, 0.0]);
    // One voice: its modulation follows its note-on, by key.
    NOTES.lock().unwrap().clear();
    let mut midi = MidiBuffer::with_capacity(4);
    midi.push(TimedMidiEvent::new(
        10,
        MidiEvent::NoteOn {
            channel: 0,
            key: 64,
            velocity: 100,
        },
    ))
    .unwrap();
    let input = {
        let mut b = AudioBuffer::new(ChannelLayout::Stereo, 64);
        b.set_len(64);
        b
    };
    let mut outputs = [{
        let mut b = AudioBuffer::new(ChannelLayout::Stereo, 64);
        b.set_len(64);
        b
    }];
    let (inputs, events) = ([input], [midi]);
    let mut io = NodeIo {
        frames: 64,
        audio_in: &inputs,
        audio_out: &mut outputs,
        events_in: &events,
        events_out: &mut [],
    };
    let transport = faderframe_transport::TransportInfo::default();
    proc.process(
        &PluginProcessContext {
            transport: &transport,
            param_events: &[],
            harmony: &faderframe_plugin_host::NO_HARMONY,
            param_mods: &[],
            note_mods: &[NoteParamMod {
                parameter: gain,
                channel: 0,
                key: 64,
                amount: 3.0,
                sample_offset: 10,
            }],
        },
        &mut io,
    );
    let seen = NOTES.lock().unwrap().clone();
    let at = |what: &str| seen.iter().position(|n| n.0 == what).unwrap();
    assert!(at("on") < at("mod"), "{seen:?}");
    assert_eq!(seen[at("mod")], ("mod".into(), -1, 64, 3.0));
}

#[test]
fn parameters_state_and_processing() {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let f = factory();
    assert_eq!(f.scan().len(), 1);
    let mut inst = f.instantiate("org.faderframe.test-gain").unwrap();
    let params = inst.parameters().to_vec();
    assert_eq!(params.len(), 1);
    assert_eq!(params[0].name, "Gain");
    assert!(params[0].automatable);
    assert_eq!((params[0].min, params[0].max), (-60.0, 12.0));

    // Inactive: parameter changes are flushed.
    inst.set_parameter(ParameterId(0), -6.0).unwrap();
    assert_eq!(inst.parameter(ParameterId(0)), Some(-6.0));

    // State round trip into a second instance.
    let state = inst.save_state().unwrap();
    let mut other = f.instantiate("org.faderframe.test-gain").unwrap();
    other.load_state(&state).unwrap();
    assert_eq!(other.parameter(ParameterId(0)), Some(-6.0));

    // Processing: the parameter value, then an automation event.
    let config = ProcessConfig {
        sample_rate: 48_000.0,
        max_block_size: 256,
        sidechain: false,
        double_precision: false,
    };
    let mut proc = inst.create_processor(&config).unwrap();
    let out = run_block(proc.as_mut(), &[], 256);
    assert!(
        (out[100] - 0.5 * db_to_gain(-6.0)).abs() < 1e-5,
        "{}",
        out[100]
    );
    let ev = [ParameterEvent {
        parameter: ParameterId(0),
        value: -20.0,
        sample_offset: 0,
    }];
    let out = run_block(proc.as_mut(), &ev, 256);
    assert!(
        (out[100] - 0.5 * db_to_gain(-20.0)).abs() < 1e-5,
        "{}",
        out[100]
    );

    // A second processor handle shares the live plugin (graph rebuilds).
    let mut again = inst.create_processor(&config).unwrap();
    let out = run_block(again.as_mut(), &[], 128);
    assert!((out[10] - 0.5 * db_to_gain(-20.0)).abs() < 1e-5);

    // Parameter changes from the UI while active reach the processor.
    inst.set_parameter(ParameterId(0), 0.0).unwrap();
    let out = run_block(proc.as_mut(), &[], 64);
    assert!((out[10] - 0.5).abs() < 1e-5, "{}", out[10]);

    // A new configuration re-activates; old handles go silent, safely.
    let mut fresh = inst
        .create_processor(&ProcessConfig {
            sample_rate: 96_000.0,
            max_block_size: 512,
            sidechain: false,
            double_precision: false,
        })
        .unwrap();
    let out = run_block(proc.as_mut(), &[], 64);
    assert!(out.iter().all(|v| *v == 0.0), "stale handle is silent");
    let out = run_block(fresh.as_mut(), &[], 512);
    assert!((out[300] - 0.5).abs() < 1e-5);
    drop(inst);
    let out = run_block(fresh.as_mut(), &[], 64);
    assert!(
        out.iter().all(|v| *v == 0.0),
        "after the instance is gone the node is silent"
    );
}

#[test]
fn double_precision_where_the_ports_take_it() {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let f = factory();
    let mut inst = f.instantiate("org.faderframe.test-gain").unwrap();
    inst.set_parameter(ParameterId(0), -6.0).unwrap();
    let config = ProcessConfig {
        sample_rate: 48_000.0,
        max_block_size: 256,
        sidechain: false,
        double_precision: true,
    };
    let mut proc = inst.create_processor(&config).unwrap();
    let out = run_block(proc.as_mut(), &[], 256);
    assert_eq!(WIDTH.load(Ordering::Relaxed), 64);
    assert!((out[100] - 0.5 * db_to_gain(-6.0)).abs() < 1e-6);
    // Back to 32-bit: a new activation.
    let mut proc = inst
        .create_processor(&ProcessConfig {
            double_precision: false,
            ..config
        })
        .unwrap();
    run_block(proc.as_mut(), &[], 256);
    assert_eq!(WIDTH.load(Ordering::Relaxed), 32);
}

/// Host a real installed plugin end to end (opt-in):
/// `FADERFRAME_TEST_CLAP=~/.clap/Vendor/Plugin.clap cargo test -p faderframe-plugin-clap -- --ignored`
#[test]
#[ignore = "needs FADERFRAME_TEST_CLAP pointing at an installed CLAP bundle"]
fn hosts_an_installed_plugin() {
    let Some(bundle) = std::env::var_os("FADERFRAME_TEST_CLAP") else {
        return;
    };
    let bundle = std::path::PathBuf::from(bundle);
    let plugins = faderframe_plugin_clap::scan::describe_bundle(&bundle).unwrap();
    assert!(!plugins.is_empty());
    faderframe_plugin_clap::set_catalog(plugins.clone());
    let f = ClapFactory::new();
    for p in plugins.iter().filter(|p| p.is_instrument()) {
        play_an_installed_instrument(&f, p);
    }
    for p in plugins.iter().filter(|p| !p.is_instrument()) {
        let mut inst = f.instantiate(&p.id).unwrap();
        eprintln!(
            "{}: {} parameters, latency {}",
            p.name,
            inst.parameters().len(),
            inst.latency_samples()
        );
        let state = inst.save_state().unwrap();
        inst.load_state(&state).unwrap();
        let mut proc = inst
            .create_processor(&ProcessConfig {
                sample_rate: 48_000.0,
                max_block_size: 512,
                sidechain: false,
                double_precision: false,
            })
            .unwrap();
        let mut peak = 0.0f32;
        for _ in 0..50 {
            let out = run_block(proc.as_mut(), &[], 512);
            peak = out.iter().fold(peak, |m, v| m.max(v.abs()));
            assert!(out.iter().all(|v| v.is_finite()));
        }
        eprintln!("{}: output peak {peak} for a 0.5 DC input", p.name);
        // Modulation: the parameters that take it, moved without changing
        // their values.
        let infos = inst.parameters().to_vec();
        let modulated: Vec<_> = infos.iter().filter(|i| inst.modulatable(i.id)).collect();
        eprintln!(
            "{}: {} of {} parameters take modulation ({} per note)",
            p.name,
            modulated.len(),
            infos.len(),
            infos
                .iter()
                .filter(|i| inst.modulatable_per_note(i.id))
                .count()
        );
        if let Some(info) = modulated.first() {
            let before = inst.parameter(info.id);
            let range = (info.max - info.min) as f32;
            let mut peak = 0.0f32;
            for i in 0..50 {
                let share = (i as f32 / 10.0).sin() * 0.5;
                let m = faderframe_plugin_host::ParamMod {
                    parameter: info.id,
                    share,
                    amount: share * range,
                };
                let out = run_block_modulated(proc.as_mut(), &[], &[m], 512);
                peak = out.iter().fold(peak, |m, v| m.max(v.abs()));
                assert!(out.iter().all(|v| v.is_finite()));
            }
            run_block(proc.as_mut(), &[], 512);
            assert_eq!(
                inst.parameter(info.id),
                before,
                "modulation left {}",
                info.name
            );
            eprintln!("{}: '{}' modulated, output peak {peak}", p.name, info.name);
        }
    }
}

/// An installed instrument: its modulation flags, and a note played with
/// and without a per-voice offset on a parameter (a filter cutoff if it
/// has one).
fn play_an_installed_instrument(f: &ClapFactory, p: &ScannedPlugin) {
    let inst = f.instantiate(&p.id).unwrap();
    let infos = inst.parameters().to_vec();
    let per_note: Vec<_> = infos
        .iter()
        .filter(|i| inst.modulatable_per_note(i.id))
        .collect();
    eprintln!(
        "{}: {} parameters, {} take modulation, {} per note",
        p.name,
        infos.len(),
        infos.iter().filter(|i| inst.modulatable(i.id)).count(),
        per_note.len()
    );
    if std::env::var_os("FADERFRAME_TEST_LIST").is_some() {
        for i in &per_note {
            eprintln!("  per note: {} ({}..{})", i.name, i.min, i.max);
        }
    }
    drop(inst);
    let target = per_note
        .iter()
        .find(|i| i.name.contains("Cutoff") || i.name == "VCF1/Frequency")
        .or(per_note.first())
        .map(|i| (i.id, (i.max - i.min) as f32, i.name.clone()));
    let plain = play_a_note(f, p, None);
    eprintln!("{}: a note, rms {plain}", p.name);
    if let Some((id, range, name)) = target {
        let moved = play_a_note(f, p, Some((id, -0.4 * range)));
        eprintln!("{}: '{name}' −40 % on its voice, rms {moved}", p.name);
    }
}

/// The rms of a second of A3 (with a per-voice offset on a parameter).
fn play_a_note(f: &ClapFactory, p: &ScannedPlugin, offset: Option<(ParameterId, f32)>) -> f32 {
    use faderframe_midi::{MidiBuffer, MidiEvent, TimedMidiEvent};
    use faderframe_plugin_host::NoteParamMod;
    let mut inst = f.instantiate(&p.id).unwrap();
    let mut proc = inst
        .create_processor(&ProcessConfig {
            sample_rate: 48_000.0,
            max_block_size: 512,
            sidechain: false,
            double_precision: false,
        })
        .unwrap();
    let (mut sum, mut n) = (0.0f64, 0usize);
    for b in 0..94u32 {
        let mut midi = MidiBuffer::with_capacity(4);
        if b == 0 {
            midi.push(TimedMidiEvent::new(
                0,
                MidiEvent::NoteOn {
                    channel: 0,
                    key: 57,
                    velocity: 110,
                },
            ))
            .unwrap();
        }
        let note_mods: Vec<NoteParamMod> = offset
            .iter()
            .map(|(id, amount)| NoteParamMod {
                parameter: *id,
                channel: 0,
                key: 57,
                amount: *amount,
                sample_offset: 0,
            })
            .collect();
        let mut outputs = [{
            let mut o = AudioBuffer::new(ChannelLayout::Stereo, 512);
            o.set_len(512);
            o
        }];
        let events = [midi];
        let mut io = NodeIo {
            frames: 512,
            audio_in: &[],
            audio_out: &mut outputs,
            events_in: &events,
            events_out: &mut [],
        };
        let transport = faderframe_transport::TransportInfo::default();
        let status = proc.process(
            &PluginProcessContext {
                transport: &transport,
                param_events: &[],
                harmony: &faderframe_plugin_host::NO_HARMONY,
                param_mods: &[],
                note_mods: &note_mods,
            },
            &mut io,
        );
        assert_ne!(status, faderframe_plugin_host::ProcessStatus::Error);
        for v in outputs[0].channel(0) {
            assert!(v.is_finite());
            if b >= 20 {
                sum += f64::from(*v) * f64::from(*v);
                n += 1;
            }
        }
    }
    (sum / n.max(1) as f64).sqrt() as f32
}
