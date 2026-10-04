//! A plugin that stops working: one notice, the plugin marked, and Reload
//! Plugin starting it again from its slot.
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
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

/// Instances made so far, and whether processing fails.
static MADE: AtomicUsize = AtomicUsize::new(0);
static FAIL: AtomicBool = AtomicBool::new(false);

struct Factory;
struct Instance(PluginDescriptor);
struct Processor;

fn descriptor() -> PluginDescriptor {
    let stereo = vec![AudioPortInfo {
        channels: 2,
        is_main: true,
    }];
    PluginDescriptor {
        format: PluginFormat::Clap,
        id: "test.fragile".into(),
        name: "Fragile".into(),
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
        MADE.fetch_add(1, Ordering::SeqCst);
        Ok(Box::new(Instance(descriptor())))
    }
}

impl PluginInstance for Instance {
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
        Ok(b"state".to_vec())
    }
    fn load_state(&mut self, _data: &[u8]) -> Result<(), PluginError> {
        Ok(())
    }
    fn create_processor(
        &mut self,
        _c: &ProcessConfig,
    ) -> Result<Box<dyn PluginProcessor>, PluginError> {
        Ok(Box::new(Processor))
    }
}

impl PluginProcessor for Processor {
    fn process(&mut self, _ctx: &PluginProcessContext<'_>, io: &mut NodeIo<'_>) -> ProcessStatus {
        if FAIL.load(Ordering::SeqCst) {
            return ProcessStatus::Error;
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
fn a_failed_plugin_is_reported_once_and_reloads() {
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
            id: "test.fragile".into(),
            name: "Fragile".into(),
        },
    })
    .unwrap();
    let plugin = s.project().track(bass).unwrap().inserts[0].id;
    s.dispatch(Action::Transport(TransportAction::Play))
        .unwrap();
    run(&mut s, 0.2);
    assert!(!s.plugin_failed(plugin));
    let made = MADE.load(Ordering::SeqCst);

    FAIL.store(true, Ordering::SeqCst);
    run(&mut s, 0.3);
    assert!(s.plugin_failed(plugin));
    let notices: Vec<String> = s
        .notices()
        .map(|n| n.text.clone())
        .filter(|t| t.contains("Fragile"))
        .collect();
    assert_eq!(notices.len(), 1, "one notice: {notices:?}");
    run(&mut s, 0.2);
    assert_eq!(
        s.notices().filter(|n| n.text.contains("Fragile")).count(),
        1,
        "not repeated"
    );

    // Reloaded from its slot: a new instance, working again.
    FAIL.store(false, Ordering::SeqCst);
    s.dispatch(Action::ReloadPlugin(plugin)).unwrap();
    assert_eq!(MADE.load(Ordering::SeqCst), made + 1);
    run(&mut s, 0.3);
    assert!(!s.plugin_failed(plugin));
    assert!(
        s.notices()
            .any(|n| n.text.contains("Fragile started again"))
    );
    s.stop_audio();
}
