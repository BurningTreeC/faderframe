//! Clip SysEx reaches the track's plugins (on time, bytes intact) through
//! the realtime path.
#![allow(clippy::unwrap_used)]

use faderframe_audio::dummy::DummyBackend;
use faderframe_audio_graph::NodeIo;
use faderframe_core::ParameterId;
use faderframe_engine::EngineConfig;
use faderframe_midi::MidiEvent;
use faderframe_plugin_host::{
    AudioPortInfo, ParameterInfo, PluginCategory, PluginDescriptor, PluginError, PluginFactory,
    PluginFormat, PluginInstance, PluginProcessContext, PluginProcessor, ProcessConfig,
    ProcessStatus, TailLength,
};
use faderframe_project::{PluginRef, Project, TrackKind};
use faderframe_session::{Action, AudioPreferences, Session, TransportAction};
use faderframe_timeline::MusicalTime;
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// SysEx the test instrument received: (transport sample, bytes).
static GOT: Mutex<Vec<(i64, Vec<u8>)>> = Mutex::new(Vec::new());

struct Factory;
struct Instance(PluginDescriptor);
struct Processor;

fn descriptor() -> PluginDescriptor {
    PluginDescriptor {
        format: PluginFormat::Clap,
        id: "test.sysex".into(),
        name: "SysEx Listener".into(),
        vendor: "FaderFrame".into(),
        version: "1".into(),
        category: PluginCategory::Instrument,
        audio_inputs: Vec::new(),
        audio_outputs: vec![AudioPortInfo {
            channels: 2,
            is_main: true,
        }],
        note_inputs: 1,
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
        Ok(Vec::new())
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
    fn process(&mut self, ctx: &PluginProcessContext<'_>, io: &mut NodeIo<'_>) -> ProcessStatus {
        if let Some(midi) = io.events_in.first() {
            for e in midi.iter() {
                if let MidiEvent::SysEx(r) = e.event {
                    let bytes = midi.sysex(&r).unwrap().to_vec();
                    let at = ctx.transport.sample_position + i64::from(e.sample_offset);
                    GOT.lock().unwrap().push((at, bytes));
                }
            }
        }
        for o in io.audio_out.iter_mut() {
            o.clear();
        }
        ProcessStatus::Continue
    }
    fn reset(&mut self) {}
}

fn registry() {
    faderframe_plugin_host::set_default_registry(|| {
        let mut r = faderframe_plugin_host::PluginRegistry::with_builtins();
        r.add_factory(Box::new(Factory));
        r
    });
}

/// One test at a time: they share what the plugin received.
static SERIAL: Mutex<()> = Mutex::new(());

#[test]
fn clip_sysex_reaches_the_tracks_instrument() {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    GOT.lock().unwrap().clear();
    registry();
    let mut s = Session::new(Project::new("SysEx", 48_000), None, EngineConfig::default()).unwrap();
    let t = s.add_track(TrackKind::Instrument).unwrap();
    s.dispatch(Action::SetInstrumentPlugin {
        track: t,
        plugin: Some(PluginRef {
            format: faderframe_project::PluginFormat::Clap,
            id: "test.sysex".into(),
            name: "SysEx Listener".into(),
        }),
    })
    .unwrap();
    assert!(s.instrument_slot(s.project().track(t).unwrap()).is_some());
    s.dispatch(Action::CreateMidiClip {
        track: t,
        start: MusicalTime::ZERO,
        length: MusicalTime::from_quarters_i(8),
    })
    .unwrap();
    let clip = s.project().clips_of(t)[0].id;
    let dump = vec![0xF0, 0x43, 0x00, 0x09, 0x20, 0x00, 0x11, 0x22, 0xF7];
    s.dispatch(Action::AddSysex {
        clip,
        at: MusicalTime::from_quarters_i(1),
        messages: vec![dump.clone()],
    })
    .unwrap();
    s.start_audio(
        vec![Box::new(DummyBackend::default())],
        &AudioPreferences::default(),
    )
    .unwrap();
    s.dispatch(Action::Transport(TransportAction::Play))
        .unwrap();
    let begun = Instant::now();
    while GOT.lock().unwrap().is_empty() && begun.elapsed() < Duration::from_secs(5) {
        s.tick(0.01);
        std::thread::sleep(Duration::from_millis(10));
    }
    s.stop_audio();
    let got = GOT.lock().unwrap().clone();
    // 120 BPM: beat 1 is half a second in.
    assert_eq!(got, vec![(24_000, dump)]);
}

#[test]
fn live_sysex_reaches_the_live_instrument() {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    GOT.lock().unwrap().clear();
    registry();
    let mut s = Session::new(Project::new("Live", 48_000), None, EngineConfig::default()).unwrap();
    let t = s.add_track(TrackKind::Instrument).unwrap();
    s.dispatch(Action::SetInstrumentPlugin {
        track: t,
        plugin: Some(PluginRef {
            format: faderframe_project::PluginFormat::Clap,
            id: "test.sysex".into(),
            name: "SysEx Listener".into(),
        }),
    })
    .unwrap();
    s.start_audio(
        vec![Box::new(DummyBackend::default())],
        &AudioPreferences::default(),
    )
    .unwrap();
    s.dispatch(Action::SelectTracks {
        tracks: vec![t],
        mode: faderframe_session::SelectMode::Replace,
    })
    .unwrap();
    let begun = Instant::now();
    while !s.midi_live_tracks().contains(&t) && begun.elapsed() < Duration::from_secs(2) {
        s.tick(0.01);
    }
    assert!(s.midi_live_tracks().contains(&t));
    // A patch dump from the keyboard: to the live instrument's plugin.
    let dump = vec![0xF0, 0x43, 0x00, 0x00, 0x01, 0x1B, 0x55, 0xF7];
    assert!(s.midi_keyboard().send(&dump));
    let begun = Instant::now();
    while GOT.lock().unwrap().is_empty() && begun.elapsed() < Duration::from_secs(5) {
        s.tick(0.01);
        std::thread::sleep(Duration::from_millis(10));
    }
    s.stop_audio();
    let got: Vec<Vec<u8>> = GOT.lock().unwrap().iter().map(|(_, b)| b.clone()).collect();
    assert_eq!(got, vec![dump]);
}
