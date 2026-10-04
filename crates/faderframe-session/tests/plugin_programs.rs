//! A plugin's own programs: selecting one is one undo step that records the
//! plugin's new state once its processor has taken the program.
#![allow(clippy::unwrap_used)]

use faderframe_audio::dummy::DummyBackend;
use faderframe_audio_graph::NodeIo;
use faderframe_core::ParameterId;
use faderframe_engine::EngineConfig;
use faderframe_plugin_host::{
    AudioPortInfo, ParameterInfo, PluginCategory, PluginDescriptor, PluginError, PluginFactory,
    PluginFormat, PluginInstance, PluginProcessContext, PluginProcessor, ProcessConfig,
    ProcessStatus, TailLength,
};
use faderframe_project::PluginRef;
use faderframe_session::{Action, AudioPreferences, Session, TransportAction};
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

const PROGRAMS: [&str; 3] = ["Dark", "Warm", "Bright"];
const NONE: u32 = u32::MAX;

struct Factory;

/// The program the processor runs, and one sent to it but not taken yet.
#[derive(Default)]
struct Shared {
    program: AtomicUsize,
    pending: AtomicU32,
}

struct Instance {
    descriptor: PluginDescriptor,
    shared: Arc<Shared>,
}

struct Processor(Arc<Shared>);

fn descriptor() -> PluginDescriptor {
    let stereo = vec![AudioPortInfo {
        channels: 2,
        is_main: true,
    }];
    PluginDescriptor {
        format: PluginFormat::Clap,
        id: "test.programs".into(),
        name: "Programs".into(),
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
        let shared = Arc::new(Shared::default());
        shared.pending.store(NONE, Ordering::SeqCst);
        Ok(Box::new(Instance {
            descriptor: descriptor(),
            shared,
        }))
    }
}

impl PluginInstance for Instance {
    fn descriptor(&self) -> &PluginDescriptor {
        &self.descriptor
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
    fn programs(&self) -> Vec<String> {
        PROGRAMS.map(String::from).to_vec()
    }
    fn current_program(&self) -> Option<usize> {
        Some(self.shared.program.load(Ordering::SeqCst))
    }
    fn select_program(&mut self, index: usize) -> Result<(), PluginError> {
        if index >= PROGRAMS.len() {
            return Err(PluginError::Failed("no such program".into()));
        }
        self.shared.pending.store(index as u32, Ordering::SeqCst);
        Ok(())
    }
    fn changes_pending(&self) -> bool {
        self.shared.pending.load(Ordering::SeqCst) != NONE
    }
    /// The state is the program the processor runs.
    fn save_state(&mut self) -> Result<Vec<u8>, PluginError> {
        Ok(vec![self.shared.program.load(Ordering::SeqCst) as u8])
    }
    fn load_state(&mut self, data: &[u8]) -> Result<(), PluginError> {
        let p = *data
            .first()
            .ok_or(PluginError::InvalidState("empty".into()))?;
        self.shared.program.store(p as usize, Ordering::SeqCst);
        Ok(())
    }
    fn create_processor(
        &mut self,
        _c: &ProcessConfig,
    ) -> Result<Box<dyn PluginProcessor>, PluginError> {
        Ok(Box::new(Processor(Arc::clone(&self.shared))))
    }
}

impl PluginProcessor for Processor {
    fn process(&mut self, _ctx: &PluginProcessContext<'_>, io: &mut NodeIo<'_>) -> ProcessStatus {
        let p = self.0.pending.swap(NONE, Ordering::SeqCst);
        if p != NONE {
            self.0.program.store(p as usize, Ordering::SeqCst);
        }
        if let (Some(i), Some(o)) = (io.audio_in.first(), io.audio_out.first_mut()) {
            o.copy_from(i);
        }
        ProcessStatus::Continue
    }
    fn reset(&mut self) {}
}

fn run(s: &mut Session, secs: f64) {
    let end = Instant::now() + Duration::from_secs_f64(secs);
    while Instant::now() < end {
        s.tick(0.016);
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn selecting_a_program_is_one_undo_step() {
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
            id: "test.programs".into(),
            name: "Programs".into(),
        },
    })
    .unwrap();
    let plugin = s.project().track(bass).unwrap().inserts[0].id;
    s.dispatch(Action::Transport(TransportAction::Play))
        .unwrap();
    run(&mut s, 0.1);
    assert_eq!(s.plugin_programs(plugin), PROGRAMS.to_vec());
    assert_eq!(s.plugin_current_program(plugin), Some(0));
    let slot_state = |s: &Session| s.project().track(bass).unwrap().inserts[0].state.clone();

    s.dispatch(Action::SelectPluginProgram { plugin, index: 2 })
        .unwrap();
    let begun = Instant::now();
    while s.history().undo_label() != Some("Select Program") && begun.elapsed().as_secs() < 3 {
        run(&mut s, 0.02);
    }
    assert_eq!(s.history().undo_label(), Some("Select Program"));
    assert_eq!(s.plugin_current_program(plugin), Some(2));
    let bright = slot_state(&s);
    assert!(bright.is_some());

    // Undo loads the state from before: program 0 again.
    s.dispatch(Action::Undo).unwrap();
    run(&mut s, 0.05);
    assert_eq!(s.plugin_current_program(plugin), Some(0));
    assert_ne!(slot_state(&s), bright);
    // Redo: program 2.
    s.dispatch(Action::Redo).unwrap();
    run(&mut s, 0.05);
    assert_eq!(s.plugin_current_program(plugin), Some(2));
    assert_eq!(slot_state(&s), bright);

    // A program the plugin does not have is refused.
    assert!(
        s.dispatch(Action::SelectPluginProgram { plugin, index: 7 })
            .is_err()
    );
    s.stop_audio();
}
