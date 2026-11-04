//! A device's header: A/B compare (a built-in with explicit values, a
//! sampler with a state of its own, a CLAP plugin whose own window changed
//! it) restores each side exactly, one undo step a switch; Copy; and the
//! device's own undo and redo beside the project's.
#![allow(clippy::unwrap_used)]

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
use faderframe_audio_graph::NodeIo;
use faderframe_core::{ParameterId, PluginInstanceId, TrackId, builtin, db_to_gain};
use faderframe_engine::EngineConfig;
use faderframe_plugin_clap::ClapFactory;
use faderframe_plugin_clap::scan::ScannedPlugin;
use faderframe_plugin_host::{
    AudioPortInfo as AudioPortInfoHost, EditorEdit, ParameterInfo, ParameterUnit, PluginCategory,
    PluginDescriptor as HostDescriptor, PluginError as HostError, PluginFactory, PluginFormat,
    PluginInstance, PluginProcessContext, PluginProcessor, ProcessConfig,
    ProcessStatus as HostStatus, TailLength,
};
use faderframe_project::device_compare::{DeviceSettings, Side};
use faderframe_project::{Command, PluginRef};
use faderframe_session::device_states::DeviceOp;
use faderframe_session::sampling::SampleTarget;
use faderframe_session::{Action, Session};
use faderframe_timeline::MusicalTime;
use std::ffi::CStr;
use std::io::{Read, Write as _};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

// --- a CLAP plugin with a state of its own ---------------------------------------

/// The test plugin's gain (dB): its own window changes it here.
static GAIN: AtomicU64 = AtomicU64::new(0);
/// The test using it runs alone.
static SERIAL: Mutex<()> = Mutex::new(());

fn gain() -> f64 {
    f64::from_bits(GAIN.load(Ordering::Relaxed))
}

fn set_gain(v: f64) {
    GAIN.store(v.clamp(-60.0, 12.0).to_bits(), Ordering::Relaxed);
}

struct TestGain;
pub struct Shared;
impl PluginShared<'_> for Shared {}

fn apply(events: &InputEvents) {
    for e in events {
        if let Some(CoreEventSpace::ParamValue(p)) = e.as_core_event() {
            set_gain(p.value());
        }
    }
}

pub struct Main;
impl PluginMainThread<'_, Shared> for Main {}

impl PluginMainThreadParams for Main {
    fn count(&self) -> u32 {
        1
    }

    fn get_info(&self, index: u32, info: &mut ParamInfoWriter) {
        if index == 0 {
            info.set(&ParamInfo {
                id: ClapId::new(0),
                flags: ParamInfoFlags::IS_AUTOMATABLE,
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
        (id == ClapId::new(0)).then(gain)
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
        apply(input);
    }
}

impl PluginAudioPortsImpl for Main {
    fn count(&self, _is_input: bool) -> u32 {
        1
    }

    fn get(&self, index: u32, is_input: bool, writer: &mut AudioPortInfoWriter) {
        if index == 0 {
            writer.set(&AudioPortInfo {
                id: ClapId::new(0),
                name: if is_input { b"In" } else { b"Out" },
                channel_count: 2,
                flags: AudioPortFlags::IS_MAIN,
                port_type: Some(AudioPortType::STEREO),
                in_place_pair: None,
            });
        }
    }
}

/// The state: the gain, then a tail the plugin keeps (so the state is
/// more than its one parameter).
impl PluginStateImpl for Main {
    fn save(&self, output: &mut OutputStream) -> Result<(), PluginError> {
        output.write_all(&gain().to_le_bytes())?;
        output.write_all(b"test-gain")?;
        Ok(())
    }

    fn load(&self, input: &mut InputStream) -> Result<(), PluginError> {
        let mut b = [0u8; 8];
        input.read_exact(&mut b)?;
        set_gain(f64::from_le_bytes(b));
        Ok(())
    }
}

pub struct Processor;

impl<'a> PluginAudioProcessor<'a, Shared, Main> for Processor {
    fn activate(
        _host: HostAudioProcessorHandle<'a>,
        _main: &Main,
        _shared: &'a Shared,
        _config: PluginAudioConfiguration,
    ) -> Result<Self, PluginError> {
        Ok(Self)
    }

    fn process(
        &mut self,
        _process: Process,
        mut audio: Audio,
        events: Events,
    ) -> Result<ProcessStatus, PluginError> {
        apply(events.input);
        let g = db_to_gain(gain() as f32);
        let mut port = audio.port_pair(0).ok_or(PluginError::Message("no port"))?;
        let mut channels = port
            .channels()?
            .into_f32()
            .ok_or(PluginError::Message("not f32"))?;
        for pair in channels.iter_mut() {
            match pair {
                ChannelPair::InputOutput(i, o) => {
                    for (o, i) in o.iter_mut().zip(i.iter()) {
                        *o = *i * g;
                    }
                }
                ChannelPair::InPlace(b) => b.iter_mut().for_each(|s| *s *= g),
                ChannelPair::OutputOnly(o) => o.fill(0.0),
                ChannelPair::InputOnly(_) => {}
            }
        }
        Ok(ProcessStatus::Continue)
    }
}

impl PluginAudioProcessorParams for Processor {
    fn flush(&mut self, input: &InputEvents, _output: &mut OutputEvents) {
        apply(input);
    }
}

impl Plugin for TestGain {
    type AudioProcessor<'a> = Processor;
    type Shared<'a> = Shared;
    type MainThread<'a> = Main;

    fn declare_extensions(builder: &mut PluginExtensions<Self>, _shared: Option<&Shared>) {
        builder
            .register::<PluginParams>()
            .register::<PluginState>()
            .register::<PluginAudioPorts>();
    }
}

impl DefaultPluginFactory for TestGain {
    fn get_descriptor() -> PluginDescriptor {
        use clack_plugin::plugin::features::*;
        PluginDescriptor::new("org.faderframe.test-ab", "Test A/B")
            .with_features([AUDIO_EFFECT, STEREO])
    }

    fn new_shared(_host: HostSharedHandle<'_>) -> Result<Shared, PluginError> {
        Ok(Shared)
    }

    fn new_main_thread<'a>(
        _host: HostMainThreadHandle<'a>,
        _shared: &'a Shared,
    ) -> Result<Main, PluginError> {
        Ok(Main)
    }
}

const BUNDLE: &CStr = c"/faderframe-test/test-ab.clap";

fn clap_factory() -> ClapFactory {
    let entry =
        clack_host::prelude::PluginEntry::load_from_clack::<SinglePluginEntry<TestGain>>(BUNDLE)
            .unwrap();
    faderframe_plugin_clap::set_catalog(vec![ScannedPlugin {
        id: "org.faderframe.test-ab".into(),
        name: "Test A/B".into(),
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

// --- a plugin whose own window reports its moves ------------------------------------

/// Its value (its whole state) and its window's moves not taken yet.
static OWN: Mutex<f64> = Mutex::new(0.0);
static OWN_EDITS: Mutex<Vec<EditorEdit>> = Mutex::new(Vec::new());
const OWN_PARAM: ParameterId = ParameterId(7);

struct OwnFactory;
struct OwnInstance {
    descriptor: HostDescriptor,
    params: Vec<ParameterInfo>,
}
struct Through;

fn own_descriptor() -> HostDescriptor {
    let stereo = vec![AudioPortInfoHost {
        channels: 2,
        is_main: true,
    }];
    HostDescriptor {
        format: PluginFormat::Vst3,
        id: "test.own-window".into(),
        name: "Own Window".into(),
        vendor: "FaderFrame".into(),
        version: "1".into(),
        category: PluginCategory::Effect,
        audio_inputs: stereo.clone(),
        audio_outputs: stereo,
        note_inputs: 0,
        note_outputs: 0,
    }
}

impl PluginFactory for OwnFactory {
    fn format(&self) -> PluginFormat {
        PluginFormat::Vst3
    }
    fn scan(&self) -> Vec<HostDescriptor> {
        vec![own_descriptor()]
    }
    fn instantiate(&self, id: &str) -> Result<Box<dyn PluginInstance>, HostError> {
        if id != "test.own-window" {
            return Err(HostError::NotFound(id.into()));
        }
        Ok(Box::new(OwnInstance {
            descriptor: own_descriptor(),
            params: vec![ParameterInfo {
                id: OWN_PARAM,
                name: "Amount".into(),
                min: 0.0,
                max: 1.0,
                default: 0.0,
                unit: ParameterUnit::None,
                automatable: true,
                stepped: false,
            }],
        }))
    }
}

impl PluginInstance for OwnInstance {
    fn descriptor(&self) -> &HostDescriptor {
        &self.descriptor
    }
    fn parameters(&self) -> &[ParameterInfo] {
        &self.params
    }
    fn parameter(&mut self, _id: ParameterId) -> Option<f64> {
        Some(*OWN.lock().unwrap())
    }
    fn set_parameter(&mut self, _id: ParameterId, value: f64) -> Result<(), HostError> {
        *OWN.lock().unwrap() = value;
        Ok(())
    }
    fn latency_samples(&self) -> u32 {
        0
    }
    fn take_editor_edits(&mut self) -> Vec<EditorEdit> {
        std::mem::take(&mut *OWN_EDITS.lock().unwrap())
    }
    fn tail(&self) -> TailLength {
        TailLength::None
    }
    fn save_state(&mut self) -> Result<Vec<u8>, HostError> {
        Ok(OWN.lock().unwrap().to_le_bytes().to_vec())
    }
    fn load_state(&mut self, data: &[u8]) -> Result<(), HostError> {
        let b: [u8; 8] = data
            .try_into()
            .map_err(|_| HostError::InvalidState("eight bytes".into()))?;
        *OWN.lock().unwrap() = f64::from_le_bytes(b);
        Ok(())
    }
    fn create_processor(
        &mut self,
        _config: &ProcessConfig,
    ) -> Result<Box<dyn PluginProcessor>, HostError> {
        Ok(Box::new(Through))
    }
}

impl PluginProcessor for Through {
    fn process(&mut self, _ctx: &PluginProcessContext<'_>, io: &mut NodeIo<'_>) -> HostStatus {
        if let (Some(i), Some(o)) = (io.audio_in.first(), io.audio_out.first_mut()) {
            o.copy_from(i);
        }
        HostStatus::Continue
    }
    fn reset(&mut self) {}
}

/// The plugin's window moves its value (a gesture).
fn move_in_window(v: f64) {
    *OWN.lock().unwrap() = v;
    OWN_EDITS.lock().unwrap().extend([
        EditorEdit::Begin(OWN_PARAM),
        EditorEdit::Value(OWN_PARAM, v),
        EditorEdit::End(OWN_PARAM),
    ]);
}

fn registry() {
    faderframe_plugin_host::set_default_registry(|| {
        let mut r = faderframe_plugin_host::PluginRegistry::with_builtins();
        r.add_factory(Box::new(clap_factory()));
        r.add_factory(Box::new(OwnFactory));
        r
    });
}

// --- helpers ------------------------------------------------------------------------

fn track(s: &Session, name: &str) -> TrackId {
    s.project()
        .tracks
        .iter()
        .find(|t| t.name == name)
        .unwrap()
        .id
}

fn settings(s: &Session, plugin: PluginInstanceId) -> DeviceSettings {
    DeviceSettings::of(s.plugin_owner(plugin).unwrap().1)
}

/// A built-in's parameter as it plays.
fn live(s: &Session, plugin: PluginInstanceId, id: u32) -> f64 {
    let tap = s.plugin_tap(plugin).unwrap();
    let i = tap
        .params
        .infos()
        .iter()
        .position(|p| p.id.0 == id)
        .unwrap();
    f64::from(tap.params.get(i))
}

fn set(s: &mut Session, track: TrackId, plugin: PluginInstanceId, id: u32, v: f64) {
    s.dispatch(Action::Edit(Command::SetPluginParameter {
        track,
        plugin,
        parameter: ParameterId(id),
        value: Some(v),
    }))
    .unwrap();
    s.tick(0.016);
}

fn device(s: &mut Session, op: DeviceOp) {
    s.dispatch(Action::Device(op)).unwrap();
    s.tick(0.016);
}

/// The demo bass's compressor (explicit values over no state of its own).
fn compressor(s: &Session) -> (TrackId, PluginInstanceId) {
    let bass = track(s, "Bass");
    let slot = s
        .project()
        .track(bass)
        .unwrap()
        .inserts
        .iter()
        .find(|p| p.plugin.id == builtin::COMPRESSOR)
        .unwrap()
        .id;
    (bass, slot)
}

const THRESHOLD: u32 = 0;
const RATIO: u32 = 1;

// --- A/B ------------------------------------------------------------------------------

/// A built-in: B starts as a copy of A, each side keeps its values, a
/// switch is one undo step that takes the slot and its compare back
/// together, and Copy makes the sides the same.
#[test]
fn a_built_in_switches_between_two_complete_settings() {
    registry();
    let mut s = Session::demo(EngineConfig::default()).unwrap();
    let (bass, comp) = compressor(&s);
    let status = s.device_status(comp).unwrap();
    assert_eq!((status.heard, status.compared), (Side::A, false));
    set(&mut s, bass, comp, THRESHOLD, -30.0);
    let a = settings(&s, comp);
    device(&mut s, DeviceOp::Switch(comp));
    let status = s.device_status(comp).unwrap();
    assert_eq!(status.heard, Side::B);
    assert!(status.compared && !status.differ, "B starts as A");
    assert_eq!(settings(&s, comp), a);
    assert_eq!(
        s.history_steps().0.last().unwrap(),
        "Compare B (FaderFrame Compressor)"
    );
    // B's own values.
    set(&mut s, bass, comp, THRESHOLD, -10.0);
    set(&mut s, bass, comp, RATIO, 8.0);
    let b = settings(&s, comp);
    assert!(s.device_status(comp).unwrap().differ);
    device(
        &mut s,
        DeviceOp::Hear {
            plugin: comp,
            side: Side::A,
        },
    );
    assert_eq!(settings(&s, comp), a, "A exactly");
    assert_eq!(live(&s, comp, THRESHOLD), -30.0);
    assert_eq!(live(&s, comp, RATIO), 3.0, "the demo's ratio, as A had it");
    device(&mut s, DeviceOp::Switch(comp));
    assert_eq!(settings(&s, comp), b, "B exactly");
    assert_eq!(
        (live(&s, comp, THRESHOLD), live(&s, comp, RATIO)),
        (-10.0, 8.0)
    );
    // Undo takes the switch back: A plays, and is heard as A.
    s.dispatch(Action::Undo).unwrap();
    assert_eq!(settings(&s, comp), a);
    assert_eq!(s.device_status(comp).unwrap().heard, Side::A);
    assert_eq!(live(&s, comp, THRESHOLD), -30.0);
    s.dispatch(Action::Redo).unwrap();
    assert_eq!(s.device_status(comp).unwrap().heard, Side::B);
    // Copy B to A: both the same.
    device(&mut s, DeviceOp::CopyToOther(comp));
    assert_eq!(
        s.history_steps().0.last().unwrap(),
        "Copy B to A (FaderFrame Compressor)"
    );
    assert!(!s.device_status(comp).unwrap().differ);
    device(&mut s, DeviceOp::Switch(comp));
    assert_eq!(settings(&s, comp), b);
}

/// A sampler: each side its own state (samples, slices), one going back
/// to none (how the device was made) restores that too.
#[test]
fn a_samplers_states_switch_whole() {
    registry();
    let mut s = Session::demo(EngineConfig::default()).unwrap();
    let bass = track(&s, "Bass");
    let clip = s.project().clips_of(bass)[0].clone();
    s.dispatch(Action::MakeSample {
        track: bass,
        start: clip.start,
        end: clip.start + MusicalTime::from_quarters(2.0),
        target: SampleTarget::Sampler,
    })
    .unwrap();
    let end = Instant::now() + Duration::from_secs(60);
    while !s.sampling().is_empty() {
        assert!(Instant::now() < end);
        s.tick(0.016);
        std::thread::sleep(Duration::from_millis(5));
    }
    let t = track(&s, "Bass Sample");
    let plugin = s.instrument_slot(s.project().track(t).unwrap()).unwrap().id;
    let a = settings(&s, plugin);
    assert!(a.state.is_some());
    device(&mut s, DeviceOp::Switch(plugin));
    // B: the sample cleared (the state of a sampler with nothing loaded).
    s.dispatch(Action::LoadDeviceSamples {
        plugin,
        slot: 0,
        files: Vec::new(),
    })
    .unwrap();
    let b = settings(&s, plugin);
    assert_ne!(a, b);
    device(&mut s, DeviceOp::Switch(plugin));
    assert_eq!(settings(&s, plugin), a);
    s.wait_for_samples();
    let contents = s
        .plugin_tap(plugin)
        .unwrap()
        .assets::<faderframe_plugin_host::devices::samples::Contents>()
        .unwrap();
    assert!(contents.set.slot(0).is_some(), "A's sample plays again");
    device(&mut s, DeviceOp::Switch(plugin));
    assert_eq!(settings(&s, plugin), b);
}

/// A CLAP plugin whose own window changed it: the switch takes its live
/// state first, so each side comes back exactly as it was left.
#[test]
fn a_clap_plugins_own_changes_are_kept_on_each_side() {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    registry();
    set_gain(0.0);
    let mut s = Session::demo(EngineConfig::default()).unwrap();
    let pad = track(&s, "Pad");
    s.dispatch(Action::InsertPlugin {
        track: pad,
        index: 0,
        plugin: PluginRef {
            format: faderframe_project::PluginFormat::Clap,
            id: "org.faderframe.test-ab".into(),
            name: "Test A/B".into(),
        },
    })
    .unwrap();
    let plugin = s.project().track(pad).unwrap().inserts[0].id;
    // Its window turns it down: the host does not know yet.
    set_gain(-12.0);
    device(&mut s, DeviceOp::Switch(plugin));
    let a_state = s
        .project()
        .device_compare(plugin)
        .unwrap()
        .other
        .state
        .clone()
        .unwrap();
    let a_bytes = faderframe_engine::decode_state(&a_state).unwrap();
    assert_eq!(
        a_bytes[..8],
        (-12.0f64).to_le_bytes(),
        "A as its window left it"
    );
    assert_eq!(&a_bytes[8..], b"test-gain");
    // B: its window turns it up.
    set_gain(6.0);
    device(&mut s, DeviceOp::Switch(plugin));
    assert_eq!(gain(), -12.0, "A's state loaded");
    assert_eq!(s.device_status(plugin).unwrap().heard, Side::A);
    device(&mut s, DeviceOp::Switch(plugin));
    assert_eq!(gain(), 6.0, "B's state loaded");
    // Undo of the switch: A again.
    s.dispatch(Action::Undo).unwrap();
    assert_eq!(gain(), -12.0);
}

// --- the device's own undo ------------------------------------------------------------

/// Undo in the device takes back its own last change, whatever was done
/// elsewhere since, as a step of the project's history; redo brings it
/// back; the project's undo of that step moves the device's history on
/// again; a drag is one step.
#[test]
fn a_device_undoes_its_own_changes_and_leaves_the_rest() {
    registry();
    let mut s = Session::demo(EngineConfig::default()).unwrap();
    let (bass, comp) = compressor(&s);
    let pad = track(&s, "Pad");
    // Only while a window shows it.
    assert!(s.dispatch(Action::Device(DeviceOp::Undo(comp))).is_err());
    device(
        &mut s,
        DeviceOp::Watch {
            plugin: comp,
            on: true,
        },
    );
    assert!(s.device_status(comp).unwrap().undo.is_none());
    let original = settings(&s, comp);
    set(&mut s, bass, comp, THRESHOLD, -30.0);
    // A drag: one step.
    s.dispatch(Action::BeginGesture("Ratio".into())).unwrap();
    for v in [4.0, 5.0, 6.0] {
        set(&mut s, bass, comp, RATIO, v);
    }
    s.dispatch(Action::EndGesture).unwrap();
    s.tick(0.016);
    let after = settings(&s, comp);
    // Something else entirely.
    s.dispatch(Action::Edit(Command::SetTrackVolume {
        track: pad,
        db: -20.0,
    }))
    .unwrap();
    s.tick(0.016);
    let status = s.device_status(comp).unwrap();
    assert_eq!(
        status.undo.as_deref(),
        Some("Ratio"),
        "the drag, as it was named"
    );
    device(&mut s, DeviceOp::Undo(comp));
    assert_eq!(live(&s, comp, RATIO), 3.0, "the drag taken back");
    assert_eq!(live(&s, comp, THRESHOLD), -30.0);
    assert_eq!(
        s.project().track(pad).unwrap().volume_db,
        -20.0,
        "the rest stays"
    );
    assert!(
        s.history_steps()
            .0
            .last()
            .unwrap()
            .starts_with("Undo in FaderFrame Compressor: Ratio"),
        "{:?}",
        s.history_steps().0.last()
    );
    device(&mut s, DeviceOp::Undo(comp));
    assert_eq!(settings(&s, comp), original);
    assert!(s.device_status(comp).unwrap().undo.is_none());
    assert!(s.dispatch(Action::Device(DeviceOp::Undo(comp))).is_err());
    device(&mut s, DeviceOp::Redo(comp));
    device(&mut s, DeviceOp::Redo(comp));
    assert_eq!(settings(&s, comp), after);
    assert!(s.device_status(comp).unwrap().redo.is_none());
    // The project's undo of the device's redo: a move back, not a change.
    s.dispatch(Action::Undo).unwrap();
    s.tick(0.016);
    let status = s.device_status(comp).unwrap();
    assert!(status.redo.is_some(), "{status:?}");
    assert_eq!(live(&s, comp, RATIO), 3.0);
    // A new change drops what could be redone.
    set(&mut s, bass, comp, RATIO, 2.0);
    assert!(s.device_status(comp).unwrap().redo.is_none());
    // Closed: its history goes.
    device(
        &mut s,
        DeviceOp::Watch {
            plugin: comp,
            on: false,
        },
    );
    assert!(!s.device_watched(comp));
}

/// A/B switches are steps of the device's history too: its undo goes
/// back to the side heard before, compare and all.
#[test]
fn a_devices_undo_takes_a_switch_back_with_its_compare() {
    registry();
    let mut s = Session::demo(EngineConfig::default()).unwrap();
    let (bass, comp) = compressor(&s);
    device(
        &mut s,
        DeviceOp::Watch {
            plugin: comp,
            on: true,
        },
    );
    device(&mut s, DeviceOp::Switch(comp));
    set(&mut s, bass, comp, THRESHOLD, -5.0);
    device(&mut s, DeviceOp::Switch(comp));
    assert_eq!(s.device_status(comp).unwrap().heard, Side::A);
    device(&mut s, DeviceOp::Undo(comp));
    let status = s.device_status(comp).unwrap();
    assert_eq!(status.heard, Side::B);
    assert_eq!(live(&s, comp, THRESHOLD), -5.0);
}

/// Level matching: on, then off again (listening only: nothing in the
/// project).
#[test]
fn level_matching_turns_on_and_off_without_touching_the_project() {
    registry();
    let mut s = Session::demo(EngineConfig::default()).unwrap();
    let (_, comp) = compressor(&s);
    let steps = s.history_steps().0.len();
    device(
        &mut s,
        DeviceOp::MatchLevels {
            plugin: comp,
            on: true,
        },
    );
    let status = s.device_status(comp).unwrap();
    assert!(status.matching);
    let levels = status.levels.unwrap();
    assert_eq!(
        (levels.a_db, levels.b_db, levels.gain_db),
        (None, None, 0.0)
    );
    device(
        &mut s,
        DeviceOp::MatchLevels {
            plugin: comp,
            on: false,
        },
    );
    assert!(!s.device_status(comp).unwrap().matching);
    assert_eq!(s.history_steps().0.len(), steps);
}

/// A plugin whose own window moved it: once the moves end its state is a
/// step of the device's history, and the device's undo loads the state
/// before.
#[test]
fn moves_in_a_plugins_own_window_are_undone_in_the_device() {
    registry();
    *OWN.lock().unwrap() = 0.25;
    let mut s = Session::demo(EngineConfig::default()).unwrap();
    let pad = track(&s, "Pad");
    s.dispatch(Action::InsertPlugin {
        track: pad,
        index: 0,
        plugin: PluginRef {
            format: faderframe_project::PluginFormat::Vst3,
            id: "test.own-window".into(),
            name: "Own Window".into(),
        },
    })
    .unwrap();
    let plugin = s.project().track(pad).unwrap().inserts[0].id;
    device(&mut s, DeviceOp::Watch { plugin, on: true });
    move_in_window(0.75);
    // The plugin's window is the plugin's: no step of the project's.
    let steps = s.history_steps().0.len();
    let end = Instant::now() + Duration::from_secs(5);
    while s.device_status(plugin).unwrap().undo.is_none() {
        assert!(Instant::now() < end, "the window's move was not taken in");
        s.tick(0.016);
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(s.history_steps().0.len(), steps);
    assert_eq!(
        s.device_status(plugin).unwrap().undo.as_deref(),
        Some("Plugin Window Edit")
    );
    device(&mut s, DeviceOp::Undo(plugin));
    assert_eq!(*OWN.lock().unwrap(), 0.25, "the state before loaded");
    device(&mut s, DeviceOp::Redo(plugin));
    assert_eq!(*OWN.lock().unwrap(), 0.75);
    // Undo straight after a move takes the move in first.
    move_in_window(0.5);
    s.tick(0.016);
    device(&mut s, DeviceOp::Undo(plugin));
    assert_eq!(*OWN.lock().unwrap(), 0.75);
}
