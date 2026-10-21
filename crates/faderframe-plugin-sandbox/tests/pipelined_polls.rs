#![allow(clippy::unwrap_used)]
//! Polls to a helper are pipelined: sent at one UI tick, answered at a
//! later one. An answer describes the helper as it was when it read the
//! poll; what FaderFrame changed after sending it must survive the answer.
//! (Issue #12: a poll answered "editor closed" just before the editor
//! opened; FaderFrame believed it, destroyed the window the plugin's GUI
//! sat in, and the plugin — on Xlib — died of a BadWindow error.)

use faderframe_audio_graph::NodeIo;
use faderframe_core::ParameterId;
use faderframe_plugin_host::{
    AudioPortInfo, EditorRequests, ParameterInfo, ParameterUnit, ParentWindow, PluginCategory,
    PluginDescriptor, PluginEditor, PluginError, PluginFactory, PluginFormat, PluginInstance,
    PluginPoll, PluginProcessContext, PluginProcessor, PluginRegistry, ProcessConfig,
    ProcessStatus, TailLength, WindowApi,
};
use faderframe_plugin_sandbox::{Launcher, child, instantiate_sandboxed};

const SIZE: (u32, u32) = (300, 200);
/// What the plugin reports while active.
const LATENCY: u32 = 64;
/// The values the plugin's "GUI" turns Drive to at its first and its
/// sixth poll.
const TURNED: f64 = 0.25;
const TURNED_LATER: f64 = 0.9;

struct Factory;

fn descriptor() -> PluginDescriptor {
    let stereo = vec![AudioPortInfo {
        channels: 2,
        is_main: true,
    }];
    PluginDescriptor {
        format: PluginFormat::Clap,
        id: "test.editor".into(),
        name: "Editor".into(),
        vendor: "FaderFrame".into(),
        version: "1".into(),
        category: PluginCategory::Effect,
        audio_inputs: stereo.clone(),
        audio_outputs: stereo,
        note_inputs: 0,
        note_outputs: 0,
    }
}

impl PluginFactory for Factory {
    fn format(&self) -> PluginFormat {
        PluginFormat::Clap
    }
    fn scan(&self) -> Vec<PluginDescriptor> {
        vec![descriptor()]
    }
    fn instantiate(&self, _id: &str) -> Result<Box<dyn PluginInstance>, PluginError> {
        Ok(Box::new(Plugin {
            descriptor: descriptor(),
            params: vec![ParameterInfo {
                id: ParameterId(0),
                name: "Drive".into(),
                min: 0.0,
                max: 1.0,
                default: 0.5,
                unit: ParameterUnit::None,
                automatable: true,
                stepped: false,
            }],
            drive: 0.5,
            polls: 0,
            active: false,
            open: false,
            state: b"first".to_vec(),
        }))
    }
}

/// A plugin with an editor (no real windows), a latency while active and
/// a state.
struct Plugin {
    descriptor: PluginDescriptor,
    params: Vec<ParameterInfo>,
    drive: f64,
    polls: u32,
    active: bool,
    open: bool,
    state: Vec<u8>,
}

struct Silent;

impl PluginProcessor for Silent {
    fn process(&mut self, _ctx: &PluginProcessContext<'_>, _io: &mut NodeIo<'_>) -> ProcessStatus {
        ProcessStatus::Continue
    }
    fn reset(&mut self) {}
}

impl PluginInstance for Plugin {
    fn descriptor(&self) -> &PluginDescriptor {
        &self.descriptor
    }
    fn parameters(&self) -> &[ParameterInfo] {
        &self.params
    }
    fn parameter(&mut self, id: ParameterId) -> Option<f64> {
        (id.0 == 0).then_some(self.drive)
    }
    fn set_parameter(&mut self, id: ParameterId, v: f64) -> Result<(), PluginError> {
        if id.0 != 0 {
            return Err(PluginError::UnknownParameter(id));
        }
        self.drive = v;
        Ok(())
    }
    fn latency_samples(&self) -> u32 {
        if self.active { LATENCY } else { 0 }
    }
    fn tail(&self) -> TailLength {
        TailLength::None
    }
    fn save_state(&mut self) -> Result<Vec<u8>, PluginError> {
        Ok(self.state.clone())
    }
    fn load_state(&mut self, data: &[u8]) -> Result<(), PluginError> {
        self.state = data.to_vec();
        Ok(())
    }
    fn create_processor(
        &mut self,
        _c: &ProcessConfig,
    ) -> Result<Box<dyn PluginProcessor>, PluginError> {
        self.active = true;
        Ok(Box::new(Silent))
    }
    fn poll(&mut self) -> PluginPoll {
        self.polls += 1;
        // The user turned it in the plugin's own GUI.
        match self.polls {
            1 => self.drive = TURNED,
            6 => self.drive = TURNED_LATER,
            _ => {}
        }
        PluginPoll::default()
    }
    fn editor(&mut self) -> Option<&mut dyn PluginEditor> {
        Some(self)
    }
}

/// Its editor embeds into a parent window (X11, Win32) and, on macOS,
/// floats in a window of its own (the helper's own windows need the
/// helper's main thread, which a test helper does not run on).
impl PluginEditor for Plugin {
    fn can_embed(&mut self, api: WindowApi) -> bool {
        api != WindowApi::Cocoa
    }
    fn can_float(&mut self, api: WindowApi) -> bool {
        api == WindowApi::Cocoa
    }
    fn open_embedded(&mut self, _api: WindowApi, _scale: f64) -> Result<(u32, u32), PluginError> {
        Ok(SIZE)
    }
    fn attach(&mut self, _parent: ParentWindow) -> Result<(), PluginError> {
        self.open = true;
        Ok(())
    }
    fn open_floating(&mut self, api: WindowApi, _title: &str) -> Result<(), PluginError> {
        if api != WindowApi::Cocoa {
            return Err(PluginError::Failed("floats on macOS only".into()));
        }
        self.open = true;
        Ok(())
    }
    fn close(&mut self) {
        self.open = false;
    }
    fn is_open(&self) -> bool {
        self.open
    }
    fn can_resize(&mut self) -> bool {
        false
    }
    fn set_size(&mut self, _width: u32, _height: u32) -> Option<(u32, u32)> {
        None
    }
    fn take_requests(&mut self) -> EditorRequests {
        EditorRequests::default()
    }
}

/// When started as a helper this "test" serves the host, then exits.
#[test]
fn helper_entry() {
    if !child::is_helper() {
        return;
    }
    let mut registry = PluginRegistry::with_builtins();
    registry.add_factory(Box::new(Factory));
    std::process::exit(child::run(registry));
}

fn sandboxed() -> Box<dyn PluginInstance> {
    let launcher = Launcher {
        exe: std::env::current_exe().unwrap(),
        args: ["helper_entry", "--exact", "--nocapture", "--test-threads=1"]
            .map(String::from)
            .to_vec(),
        env: Vec::new(),
    };
    let inst = instantiate_sandboxed(&launcher, PluginFormat::Clap, "test.editor").unwrap();
    assert!(inst.sandboxed());
    inst
}

/// A few UI ticks: each takes in the last answer and asks again.
fn ticks(inst: &mut dyn PluginInstance) -> PluginPoll {
    let mut all = PluginPoll::default();
    for _ in 0..4 {
        all = all.merge(inst.poll());
        inst.settle();
    }
    all
}

/// UI ticks as FaderFrame's: after each poll the editor windows are
/// looked after — an editor that says it is not open loses its window.
fn editor_ticks(inst: &mut dyn PluginInstance, open: bool, why: &str) {
    for tick in 0..4 {
        inst.poll();
        assert_eq!(inst.editor().unwrap().is_open(), open, "tick {tick}: {why}");
        inst.settle();
    }
}

/// Open the editor as FaderFrame does here: embedded into a window of
/// FaderFrame's (`parent`), or on macOS — where a view cannot go into
/// another process's window, so a sandboxed editor never embeds — floating.
fn open_editor(inst: &mut dyn PluginInstance, parent: u64) {
    let ed = inst.editor().unwrap();
    if cfg!(target_os = "macos") {
        assert!(!ed.can_embed(WindowApi::Cocoa), "sandboxed editors float");
        assert!(ed.can_float(WindowApi::Cocoa));
        ed.open_floating(WindowApi::Cocoa, "Editor").unwrap();
    } else {
        assert!(ed.can_embed(WindowApi::X11));
        assert_eq!(ed.open_embedded(WindowApi::X11, 1.0).unwrap(), SIZE);
        ed.attach(ParentWindow {
            api: WindowApi::X11,
            handle: parent,
        })
        .unwrap();
    }
    assert!(ed.is_open());
}

#[test]
fn an_editor_opened_while_a_poll_is_on_its_way_stays_open() {
    let mut inst = sandboxed();
    // A tick: the poll is answered as the helper is now — editor closed.
    inst.poll();
    open_editor(inst.as_mut(), 0x160_0009);
    editor_ticks(
        inst.as_mut(),
        true,
        "an answer from before the editor opened must not close it (FaderFrame would \
         destroy the window the plugin's GUI is in)",
    );
    // And the other way round: asked while open, answered after closing.
    inst.poll();
    inst.editor().unwrap().close();
    editor_ticks(inst.as_mut(), false, "closed stays closed");
    // Open again: the helper's own answers say so.
    inst.poll();
    open_editor(inst.as_mut(), 0x160_0010);
    editor_ticks(inst.as_mut(), true, "opened again");
}

#[test]
fn a_value_set_while_a_poll_is_on_its_way_is_kept() {
    let mut inst = sandboxed();
    // The answer to this poll brings the plugin GUI's own move…
    inst.poll();
    // … but FaderFrame sets the parameter after asking.
    inst.set_parameter(ParameterId(0), 0.75).unwrap();
    ticks(inst.as_mut());
    assert_eq!(inst.parameter(ParameterId(0)), Some(0.75));
    // What the plugin changes later still arrives.
    ticks(inst.as_mut());
    assert_eq!(inst.parameter(ParameterId(0)), Some(TURNED_LATER));
}

#[test]
fn a_latency_reported_before_the_activation_restarts_nothing() {
    let mut inst = sandboxed();
    // Asked while inactive (no latency)…
    inst.poll();
    let config = ProcessConfig {
        sample_rate: 48_000.0,
        max_block_size: 64,
        sidechain: false,
        double_precision: false,
    };
    let _processor = inst.create_processor(&config).unwrap();
    assert_eq!(inst.latency_samples(), LATENCY);
    // … answered after it: no restart for a latency change that is none.
    let polled = ticks(inst.as_mut());
    assert!(!polled.restart, "a stale latency asked for a restart");
    assert_eq!(inst.latency_samples(), LATENCY);
    let activation = inst.activation();
    let _again = inst.create_processor(&config).unwrap();
    assert_eq!(inst.activation(), activation, "not activated again");
}

#[test]
fn a_state_saved_before_a_load_is_not_taken_for_the_plugins() {
    let mut inst = sandboxed();
    assert!(inst.request_state());
    // The save goes out with this tick…
    inst.poll();
    // … and a state is loaded before its answer is read.
    inst.load_state(b"second").unwrap();
    let mut taken = None;
    for _ in 0..6 {
        inst.poll();
        inst.settle();
        if let Some(s) = inst.take_state() {
            taken = Some(s);
            break;
        }
    }
    assert_eq!(taken.as_deref(), Some(&b"second"[..]));
}
