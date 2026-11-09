#![allow(clippy::unwrap_used)]
//! A plugin whose output buses the host paired (LV2 ports without port
//! groups: DrumGizmo's sixteen microphones, #52): "Create Output Tracks"
//! makes a mono track per channel, as nothing says which channels belong
//! together — never two microphones hard left and right on one track.

use faderframe_audio_graph::NodeIo;
use faderframe_core::{ChannelLayout, ParameterId};
use faderframe_engine::EngineConfig;
use faderframe_plugin_host::{
    AudioPortInfo, ParameterInfo, PluginCategory, PluginDescriptor, PluginError, PluginFactory,
    PluginFormat, PluginInstance, PluginProcessContext, PluginProcessor, PluginRegistry,
    ProcessConfig, ProcessStatus, TailLength,
};
use faderframe_project::{InputRouting, OutputRouting, PluginRef, Project, TrackKind};
use faderframe_session::{Action, PluginTarget, Session};

const ID: &str = "org.faderframe.test.mics";

struct Factory;

struct Instance(PluginDescriptor);

struct Silent;

fn descriptor() -> PluginDescriptor {
    let pair = AudioPortInfo {
        channels: 2,
        is_main: false,
    };
    PluginDescriptor {
        format: PluginFormat::Clap,
        id: ID.into(),
        name: "Mics".into(),
        vendor: "FaderFrame".into(),
        version: "1".into(),
        category: PluginCategory::Instrument,
        audio_inputs: Vec::new(),
        audio_outputs: vec![
            AudioPortInfo {
                is_main: true,
                ..pair
            },
            pair,
            pair,
        ],
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
    fn output_bus_names(&mut self) -> Vec<String> {
        vec!["Out 1/2".into(), "Out 3/4".into(), "Out 5/6".into()]
    }
    fn output_channel_names(&mut self) -> Vec<Vec<String>> {
        (0..3)
            .map(|b| vec![format!("Out{}", 2 * b + 1), format!("Out{}", 2 * b + 2)])
            .collect()
    }
    /// Paired by the host.
    fn output_buses_declared(&mut self) -> bool {
        false
    }
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
        Ok(Box::new(Silent))
    }
}

impl PluginProcessor for Silent {
    fn process(&mut self, _ctx: &PluginProcessContext<'_>, io: &mut NodeIo<'_>) -> ProcessStatus {
        for out in io.audio_out.iter_mut() {
            out.clear();
        }
        ProcessStatus::Continue
    }
    fn reset(&mut self) {}
}

fn registry() -> PluginRegistry {
    let mut r = PluginRegistry::with_builtins();
    r.add_factory(Box::new(Factory));
    r
}

#[test]
fn outputs_the_host_paired_get_a_mono_track_each() {
    faderframe_plugin_host::set_default_registry(registry);
    let mut s = Session::new(Project::new("Mics", 48_000), None, EngineConfig::default()).unwrap();
    let t = s.add_track(TrackKind::Instrument).unwrap();
    s.place_plugin(
        t,
        PluginTarget::Instrument,
        PluginRef {
            format: faderframe_project::PluginFormat::Clap,
            id: ID.into(),
            name: "Mics".into(),
        },
    )
    .unwrap();
    let plugin = s.project().track(t).unwrap().inserts[0].id;
    let buses = s.plugin_output_buses(plugin);
    assert_eq!(buses.len(), 3);
    assert!(buses.iter().all(|b| !b.declared));
    // A track per channel, and no second entry offering the same.
    assert_eq!(s.missing_output_tracks(plugin, false), 6);
    assert!(!s.plugin_has_wide_outputs(plugin));
    s.dispatch(Action::CreateOutputTracks {
        plugin,
        buses: None,
        mono: false,
    })
    .unwrap();
    let p = s.project();
    let outs = p.plugin_output_tracks(plugin);
    assert_eq!(outs.len(), 6);
    for (i, o) in outs.iter().enumerate() {
        assert_eq!(o.layout, ChannelLayout::Mono, "{}", o.name);
        assert_eq!(
            o.input,
            InputRouting::Plugin {
                plugin,
                bus: (i / 2) as u16,
                channel: Some((i % 2) as u16),
            }
        );
    }
    // Named after the plugin's ports (a number: the track's name first).
    let host = p.track(t).unwrap();
    assert_eq!(outs[0].name, format!("{} Out1", host.name));
    assert_eq!(outs[5].name, format!("{} Out6", host.name));
    // The main has its tracks: the plugin's own track is silent.
    assert_eq!(host.output, OutputRouting::None);
    assert_eq!(s.missing_output_tracks(plugin, false), 0);
}
