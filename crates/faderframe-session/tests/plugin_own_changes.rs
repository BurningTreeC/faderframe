//! A value the plugin changes itself (in its own editor, e.g. its bypass
//! button) stays: later syncs do not push the slot's older value back, and
//! saving takes the plugin's value over.
#![allow(clippy::unwrap_used)]

use faderframe_audio::dummy::DummyBackend;
use faderframe_audio_graph::NodeIo;
use faderframe_core::ParameterId;
use faderframe_engine::EngineConfig;
use faderframe_plugin_host::{
    AudioPortInfo, EditorEdit, ParameterInfo, ParameterUnit, PluginCategory, PluginDescriptor,
    PluginError, PluginFactory, PluginFormat, PluginInstance, PluginProcessContext,
    PluginProcessor, ProcessConfig, ProcessStatus, TailLength,
};
use faderframe_project::{Command, PluginRef};
use faderframe_session::{Action, AudioPreferences, Session};
use std::sync::Mutex;
use std::time::{Duration, Instant};

const BYPASS: ParameterId = ParameterId(37);

/// The plugin's own bypass value, and its editor's pending edits.
static VALUE: Mutex<f64> = Mutex::new(0.0);
static EDITS: Mutex<Vec<EditorEdit>> = Mutex::new(Vec::new());

struct Factory;
struct Instance {
    descriptor: PluginDescriptor,
    params: Vec<ParameterInfo>,
}
struct Through;

fn descriptor() -> PluginDescriptor {
    let stereo = vec![AudioPortInfo {
        channels: 2,
        is_main: true,
    }];
    PluginDescriptor {
        format: PluginFormat::Clap,
        id: "test.own".into(),
        name: "Own Changes".into(),
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
    fn instantiate(&self, id: &str) -> Result<Box<dyn PluginInstance>, PluginError> {
        if id != "test.own" {
            return Err(PluginError::NotFound(id.into()));
        }
        Ok(Box::new(Instance {
            descriptor: descriptor(),
            params: vec![ParameterInfo {
                id: BYPASS,
                name: "Bypass".into(),
                min: 0.0,
                max: 1.0,
                default: 0.0,
                unit: ParameterUnit::None,
                automatable: true,
                stepped: true,
            }],
        }))
    }
}

impl PluginInstance for Instance {
    fn descriptor(&self) -> &PluginDescriptor {
        &self.descriptor
    }
    fn parameters(&self) -> &[ParameterInfo] {
        &self.params
    }
    fn parameter(&mut self, _id: ParameterId) -> Option<f64> {
        Some(*VALUE.lock().unwrap())
    }
    fn set_parameter(&mut self, _id: ParameterId, value: f64) -> Result<(), PluginError> {
        *VALUE.lock().unwrap() = value;
        Ok(())
    }
    fn latency_samples(&self) -> u32 {
        0
    }
    fn take_editor_edits(&mut self) -> Vec<EditorEdit> {
        std::mem::take(&mut *EDITS.lock().unwrap())
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
        _config: &ProcessConfig,
    ) -> Result<Box<dyn PluginProcessor>, PluginError> {
        Ok(Box::new(Through))
    }
}

impl PluginProcessor for Through {
    fn process(&mut self, _ctx: &PluginProcessContext<'_>, io: &mut NodeIo<'_>) -> ProcessStatus {
        if let (Some(i), Some(o)) = (io.audio_in.first(), io.audio_out.first_mut()) {
            o.copy_from(i);
        }
        ProcessStatus::Continue
    }
    fn reset(&mut self) {}
}

fn run(s: &mut Session, seconds: f64) {
    let end = Instant::now() + Duration::from_secs_f64(seconds);
    while Instant::now() < end {
        s.tick(0.016);
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn a_plugins_own_change_is_not_overwritten() {
    faderframe_plugin_host::set_default_registry(|| {
        let mut r = faderframe_plugin_host::PluginRegistry::with_builtins();
        r.add_factory(Box::new(Factory));
        r
    });
    let mut s = Session::demo(EngineConfig::default()).unwrap();
    s.start_audio(
        vec![Box::new(DummyBackend::default())],
        &AudioPreferences::default(),
    )
    .unwrap();
    let bass = s
        .project()
        .tracks
        .iter()
        .find(|t| t.name == "Bass")
        .unwrap()
        .id;
    s.dispatch(Action::InsertPlugin {
        track: bass,
        index: 0,
        plugin: PluginRef {
            format: faderframe_project::PluginFormat::Clap,
            id: "test.own".into(),
            name: "Own Changes".into(),
        },
    })
    .unwrap();
    let plugin = s.project().track(bass).unwrap().inserts[0].id;
    // An explicit value from FaderFrame's side (its parameter window).
    s.edit(Command::SetPluginParameter {
        track: bass,
        plugin,
        parameter: BYPASS,
        value: Some(0.0),
    })
    .unwrap();
    run(&mut s, 0.1);
    assert_eq!(*VALUE.lock().unwrap(), 0.0);
    // The plugin's editor bypasses it.
    *VALUE.lock().unwrap() = 1.0;
    EDITS.lock().unwrap().extend([
        EditorEdit::Begin(BYPASS),
        EditorEdit::Value(BYPASS, 1.0),
        EditorEdit::End(BYPASS),
    ]);
    run(&mut s, 0.1);
    // Unrelated edits re-sync parameters and rebuild the graph.
    s.dispatch(Action::Edit(Command::RenameTrack {
        track: bass,
        name: "Bass 2".into(),
    }))
    .unwrap();
    s.dispatch(Action::InsertPlugin {
        track: bass,
        index: 1,
        plugin: PluginRef {
            format: faderframe_project::PluginFormat::Builtin,
            id: faderframe_core::builtin::GAIN.into(),
            name: "Gain".into(),
        },
    })
    .unwrap();
    run(&mut s, 0.2);
    assert_eq!(*VALUE.lock().unwrap(), 1.0, "still bypassed");
    // Saving takes the plugin's value into the slot.
    s.capture_plugin_states();
    let slot = &s.project().track(bass).unwrap().inserts[0];
    assert_eq!(slot.parameters[0].value, 1.0);
}
