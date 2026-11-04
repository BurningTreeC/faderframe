//! `faderframe_ara::mock` as a plugin (feature `ara-mock`): a CLAP-format
//! factory with one ARA plugin, so engine and session tests host ARA
//! without a real plug-in.

use crate::{
    AudioPortInfo, ParameterInfo, PluginCategory, PluginDescriptor, PluginError, PluginFactory,
    PluginFormat, PluginInstance, PluginProcessContext, PluginProcessor, ProcessConfig,
    ProcessStatus, TailLength,
};
use faderframe_ara::mock;
use faderframe_audio_graph::NodeIo;
use faderframe_core::ParameterId;

/// The mock's plugin id (CLAP format).
pub const ID: &str = "org.faderframe.ara-mock";
pub const NAME: &str = "ARA Mock";

/// Lists and instantiates the mock ARA plugin.
pub struct AraMockFactory;

pub fn descriptor() -> PluginDescriptor {
    let stereo = vec![AudioPortInfo {
        channels: 2,
        is_main: true,
    }];
    PluginDescriptor {
        format: PluginFormat::Clap,
        id: ID.into(),
        name: NAME.into(),
        vendor: "FaderFrame".into(),
        version: "1.0.0".into(),
        category: PluginCategory::Effect,
        audio_inputs: stereo.clone(),
        audio_outputs: stereo,
        note_inputs: 0,
        note_outputs: 0,
    }
}

impl PluginFactory for AraMockFactory {
    fn format(&self) -> PluginFormat {
        PluginFormat::Clap
    }

    fn scan(&self) -> Vec<PluginDescriptor> {
        vec![descriptor()]
    }

    fn instantiate(&self, id: &str) -> Result<Box<dyn PluginInstance>, PluginError> {
        if id != ID {
            return Err(PluginError::NotFound(id.into()));
        }
        Ok(Box::new(AraMockPlugin {
            descriptor: descriptor(),
            instance: mock::Instance::new(),
            activations: 0,
            active: false,
            bound: false,
        }))
    }

    fn ara_plugins(&self) -> Vec<String> {
        vec![ID.into()]
    }
}

/// One instance of the mock ARA plugin.
pub struct AraMockPlugin {
    descriptor: PluginDescriptor,
    instance: mock::Instance,
    activations: u64,
    active: bool,
    bound: bool,
}

impl AraMockPlugin {
    /// What tests read of the instance and its document.
    pub fn probe(&self) -> mock::Probe {
        self.instance.probe()
    }
}

/// Probes of every mock instance made, newest last (tests find theirs by
/// order of instantiation).
static PROBES: std::sync::Mutex<Vec<mock::Probe>> = std::sync::Mutex::new(Vec::new());

/// Every mock instance's probe so far.
pub fn probes() -> Vec<mock::Probe> {
    PROBES.lock().map(|p| p.clone()).unwrap_or_default()
}

/// The newest mock instance bound to a document of this name (the
/// project's: give each test's project a name of its own).
pub fn probe_of(document: &str) -> Option<mock::Probe> {
    probes()
        .into_iter()
        .rev()
        .find(|p| p.summary().document == document)
}

/// An edit in a mock instance's editor (see `mock::Instance::edit`).
pub fn edit(probe: &mock::Probe, modification: &str, gain: f32) -> bool {
    probe.edit(modification, gain)
}

impl PluginInstance for AraMockPlugin {
    fn descriptor(&self) -> &PluginDescriptor {
        &self.descriptor
    }

    fn parameters(&self) -> &[ParameterInfo] {
        &[]
    }

    fn parameter(&mut self, _id: ParameterId) -> Option<f64> {
        None
    }

    fn set_parameter(&mut self, id: ParameterId, _value: f64) -> Result<(), PluginError> {
        Err(PluginError::UnknownParameter(id))
    }

    fn latency_samples(&self) -> u32 {
        0
    }

    fn tail(&self) -> TailLength {
        TailLength::None
    }

    fn activation(&self) -> u64 {
        self.activations
    }

    fn save_state(&mut self) -> Result<Vec<u8>, PluginError> {
        // The edits live in the ARA document's archive.
        Ok(b"ara-mock".to_vec())
    }

    fn load_state(&mut self, _data: &[u8]) -> Result<(), PluginError> {
        if !self.bound {
            tracing::warn!("ARA mock: state loaded before binding");
        }
        Ok(())
    }

    fn create_processor(
        &mut self,
        _config: &ProcessConfig,
    ) -> Result<Box<dyn PluginProcessor>, PluginError> {
        if !self.active {
            self.active = true;
            self.activations += 1;
            self.instance.set_active(true);
        }
        Ok(Box::new(AraMockProcessor {
            renderer: self.instance.renderer(),
        }))
    }

    fn ara_factory(&mut self) -> Option<faderframe_ara::Factory> {
        Some(mock::factory())
    }

    fn bind_ara(
        &mut self,
        controller: faderframe_ara::ControllerRef,
        roles: faderframe_ara::Roles,
    ) -> Result<faderframe_ara::Extension, PluginError> {
        let ext = self
            .instance
            .bind_extension(controller, faderframe_ara::Roles::ALL, roles)
            .ok_or_else(|| PluginError::Failed("ARA mock: binding failed".into()))?;
        self.bound = true;
        if let Ok(mut p) = PROBES.lock() {
            p.push(self.instance.probe());
        }
        Ok(ext)
    }

    fn deactivate(&mut self) {
        if self.active {
            self.active = false;
            self.instance.set_active(false);
        }
    }
}

struct AraMockProcessor {
    renderer: mock::Renderer,
}

impl PluginProcessor for AraMockProcessor {
    fn process(&mut self, ctx: &PluginProcessContext<'_>, io: &mut NodeIo<'_>) -> ProcessStatus {
        let t = ctx.transport;
        if let Some(out) = io.audio_out.first_mut() {
            for c in 0..out.num_channels() {
                let ch = &mut out.channel_mut(c)[..io.frames];
                self.renderer
                    .render_channel(t.sample_position, t.sample_rate, t.playing, c, ch);
            }
        }
        ProcessStatus::Continue
    }

    fn reset(&mut self) {}
}
