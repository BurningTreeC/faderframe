//! Moves in a plugin's own editor write automation like FaderFrame's own
//! controls (a scripted plugin stands in for a hosted one).
#![allow(clippy::unwrap_used)]

use faderframe_audio::dummy::DummyBackend;
use faderframe_audio_graph::NodeIo;
use faderframe_automation::{AutomationCurve, AutomationLane, AutomationMode, AutomationTarget};
use faderframe_core::{ParameterId, TrackId};
use faderframe_engine::EngineConfig;
use faderframe_plugin_host::{
    AudioPortInfo, EditorEdit, ParameterInfo, ParameterUnit, PluginCategory, PluginDescriptor,
    PluginError, PluginFactory, PluginFormat, PluginInstance, PluginProcessContext,
    PluginProcessor, ProcessConfig, ProcessStatus, TailLength,
};
use faderframe_project::{Command, PluginRef};
use faderframe_session::{Action, AudioPreferences, Session, TransportAction};
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// Edits the scripted plugin's "editor" makes next.
static EDITS: Mutex<Vec<EditorEdit>> = Mutex::new(Vec::new());
const DRIVE: ParameterId = ParameterId(7);

struct Factory;
struct Instance {
    descriptor: PluginDescriptor,
    params: Vec<ParameterInfo>,
}
struct Through;

fn descriptor() -> PluginDescriptor {
    PluginDescriptor {
        format: PluginFormat::Clap,
        id: "test.editor".into(),
        name: "Editor Test".into(),
        vendor: "FaderFrame".into(),
        version: "1".into(),
        category: PluginCategory::Effect,
        audio_inputs: vec![AudioPortInfo {
            channels: 2,
            is_main: true,
        }],
        audio_outputs: vec![AudioPortInfo {
            channels: 2,
            is_main: true,
        }],
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
        if id != "test.editor" {
            return Err(PluginError::NotFound(id.into()));
        }
        Ok(Box::new(Instance {
            descriptor: descriptor(),
            params: vec![ParameterInfo {
                id: DRIVE,
                name: "Drive".into(),
                min: 0.0,
                max: 1.0,
                default: 0.5,
                unit: ParameterUnit::None,
                automatable: true,
                stepped: false,
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
        Some(0.5)
    }
    fn set_parameter(&mut self, _id: ParameterId, _value: f64) -> Result<(), PluginError> {
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

fn edit(s: &mut Session, e: EditorEdit, settle: f64) {
    EDITS.lock().unwrap().push(e);
    run(s, settle);
}

fn lane_points(s: &Session, t: TrackId, target: AutomationTarget) -> Vec<f64> {
    s.project()
        .track(t)
        .unwrap()
        .automation
        .lane(target)
        .unwrap()
        .curve
        .points()
        .iter()
        .map(|p| p.value)
        .collect()
}

#[test]
fn plugin_editor_moves_write_touch_and_latch_automation() {
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
            id: "test.editor".into(),
            name: "Editor Test".into(),
        },
    })
    .unwrap();
    let plugin = s.project().track(bass).unwrap().inserts[0].id;
    let target = AutomationTarget::PluginParameter {
        plugin,
        parameter: DRIVE,
    };
    let lane_id = faderframe_core::AutomationLaneId(4242);
    s.edit(Command::AddAutomationLane {
        track: bass,
        lane: Box::new(AutomationLane {
            id: lane_id,
            target,
            curve: AutomationCurve::default(),
            mode: AutomationMode::Touch,
            visible: true,
        }),
    })
    .unwrap();
    s.dispatch(Action::Transport(TransportAction::Play))
        .unwrap();
    run(&mut s, 0.2);
    // A gesture: begin, three values, end.
    edit(&mut s, EditorEdit::Begin(DRIVE), 0.05);
    // (Not on one line: a straight run is thinned to its ends.)
    for v in [0.6, 0.9, 0.7] {
        edit(&mut s, EditorEdit::Value(DRIVE, v), 0.08);
    }
    edit(&mut s, EditorEdit::End(DRIVE), 0.1);
    let pts = lane_points(&s, bass, target);
    for v in [0.6, 0.9, 0.7] {
        assert!(
            pts.iter().any(|p| (p - v).abs() < 1e-9),
            "{v} written: {pts:?}"
        );
    }
    // Plugins without gestures: Touch ends after a pause.
    edit(&mut s, EditorEdit::Value(DRIVE, 0.25), 1.2);
    let pts = lane_points(&s, bass, target);
    assert!(pts.iter().any(|p| (p - 0.25).abs() < 1e-9), "{pts:?}");
    // Nothing is written once stopped, even while the audio thread has
    // not taken the stop yet (moves right after it are not written).
    s.dispatch(Action::Transport(TransportAction::Stop))
        .unwrap();
    let before = lane_points(&s, bass, target).len();
    edit(&mut s, EditorEdit::Value(DRIVE, 0.9), 0.1);
    assert_eq!(lane_points(&s, bass, target).len(), before);
}
