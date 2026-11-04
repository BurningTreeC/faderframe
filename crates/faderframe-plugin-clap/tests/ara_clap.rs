#![allow(clippy::unwrap_used)]
//! ARA through CLAP: a plugin built with `clack-plugin` that carries the ARA
//! plug-in extension (its ARA side is `faderframe_ara::mock`), found as an
//! ARA plugin, bound to a document before activation, and rendering its
//! playback region at the song position the host's transport gives it.

use clack_plugin::prelude::*;
use faderframe_ara::mock;
use faderframe_ara::{
    Assignment, Context, ControllerRef, Document, Model, Modification, Region, Renderer, Roles,
    Samples, Sequence, Source,
};
use faderframe_audio_graph::{AudioBuffer, NodeIo};
use faderframe_core::ChannelLayout;
use faderframe_plugin_clap::ClapFactory;
use faderframe_plugin_clap::scan::ScannedPlugin;
use faderframe_plugin_host::{PluginFactory, PluginProcessContext, ProcessConfig};
use std::collections::BTreeSet;
use std::ffi::{CStr, c_void};
use std::sync::{Arc, Mutex};

const ID: &str = "org.faderframe.test-ara";
const BUNDLE: &CStr = c"/faderframe-test/test-ara.clap";
const RATE: f64 = 48_000.0;

/// The probe of the newest instance (one test at a time).
static PROBE: Mutex<Option<mock::Probe>> = Mutex::new(None);

struct TestAra;

/// The mock's companion instance (its extension struct is boxed inside;
/// the plugin keeps it on the main thread, rendering goes through the
/// `Send` renderer).
struct Held(mock::Instance);
// SAFETY: only the main thread touches the instance (binding, activation
// state); the audio thread uses `renderer`.
unsafe impl Send for Held {}
// SAFETY: as above.
unsafe impl Sync for Held {}

pub struct AraShared {
    held: Mutex<Held>,
    renderer: mock::Renderer,
}

impl PluginShared<'_> for AraShared {}

pub struct AraMain;

impl<'a> PluginMainThread<'a, AraShared> for AraMain {}

pub struct AraProcessor<'a> {
    shared: &'a AraShared,
    rate: f64,
}

impl<'a> PluginAudioProcessor<'a, AraShared, AraMain> for AraProcessor<'a> {
    fn activate(
        _host: HostAudioProcessorHandle<'a>,
        _main: &AraMain,
        shared: &'a AraShared,
        config: PluginAudioConfiguration,
    ) -> Result<Self, PluginError> {
        shared.held.lock().unwrap().0.set_active(true);
        Ok(Self {
            shared,
            rate: config.sample_rate,
        })
    }

    fn process(
        &mut self,
        process: Process,
        mut audio: Audio,
        _events: Events,
    ) -> Result<ProcessStatus, PluginError> {
        let (position, playing) = process.transport.map_or((0, false), |t| {
            (
                (t.song_pos_seconds.to_float() * self.rate).round() as i64,
                t.flags
                    .contains(clack_plugin::events::event_types::TransportFlags::IS_PLAYING),
            )
        });
        let mut port = audio.port_pair(0).ok_or(PluginError::Message("no port"))?;
        let mut channels = port
            .channels()?
            .into_f32()
            .ok_or(PluginError::Message("not f32"))?;
        for (c, pair) in channels.iter_mut().enumerate() {
            let out = match pair {
                ChannelPair::InputOutput(_, o) | ChannelPair::OutputOnly(o) => o,
                ChannelPair::InPlace(b) => b,
                ChannelPair::InputOnly(_) => continue,
            };
            self.shared
                .renderer
                .render_channel(position, self.rate, playing, c, out);
        }
        Ok(ProcessStatus::Continue)
    }

    fn deactivate(self, _main: &AraMain) {
        self.shared.held.lock().unwrap().0.set_active(false);
    }
}

/// `clap_ara_plugin_extension_t` (ARACLAP.h).
#[repr(C)]
#[derive(Clone, Copy)]
#[allow(non_camel_case_types)]
struct clap_ara_plugin_extension {
    get_factory: Option<
        unsafe extern "C" fn(
            plugin: *const clack_plugin::extensions::prelude::clap_plugin,
        ) -> *const c_void,
    >,
    bind_to_document_controller: Option<
        unsafe extern "C" fn(
            plugin: *const clack_plugin::extensions::prelude::clap_plugin,
            controller: *mut c_void,
            known: i32,
            assigned: i32,
        ) -> *const c_void,
    >,
}

#[derive(Copy, Clone)]
#[allow(dead_code)]
struct AraExtension(
    clack_plugin::extensions::prelude::RawExtension<
        clack_plugin::extensions::prelude::PluginExtensionSide,
        clap_ara_plugin_extension,
    >,
);

// SAFETY: the extension's repr(C) struct under ARACLAP.h's identifier.
unsafe impl clack_plugin::extensions::prelude::Extension for AraExtension {
    const IDENTIFIERS: &[&CStr] = &[c"org.ara-audio.ara.pluginextension/2"];
    type ExtensionSide = clack_plugin::extensions::prelude::PluginExtensionSide;

    unsafe fn from_raw(
        raw: clack_plugin::extensions::prelude::RawExtension<Self::ExtensionSide>,
    ) -> Self {
        // SAFETY: the caller guarantees the pointer is this extension's.
        Self(unsafe { raw.cast() })
    }
}

// SAFETY: the implementation is the extension's struct.
unsafe impl clack_plugin::extensions::prelude::ExtensionImplementation<TestAra> for AraExtension {
    const IMPLEMENTATION: clack_plugin::extensions::prelude::RawExtensionImplementation =
        clack_plugin::extensions::prelude::RawExtensionImplementation::new(
            &clap_ara_plugin_extension {
                get_factory: Some(get_factory),
                bind_to_document_controller: Some(bind),
            },
        );
}

unsafe extern "C" fn get_factory(
    _plugin: *const clack_plugin::extensions::prelude::clap_plugin,
) -> *const c_void {
    mock::factory_ptr()
}

unsafe extern "C" fn bind(
    plugin: *const clack_plugin::extensions::prelude::clap_plugin,
    controller: *mut c_void,
    known: i32,
    assigned: i32,
) -> *const c_void {
    // SAFETY: called by the host with this plugin's pointer, main thread.
    let raw = unsafe {
        clack_plugin::extensions::prelude::PluginWrapper::<TestAra>::handle(plugin, |p| {
            let mut held = p.shared().held.lock().unwrap();
            Ok(held.0.bind(
                ControllerRef::from_raw(controller),
                Roles(known),
                Roles(assigned),
            ) as usize)
        })
    };
    raw.unwrap_or(0) as *const c_void
}

impl Plugin for TestAra {
    type AudioProcessor<'a> = AraProcessor<'a>;
    type Shared<'a> = AraShared;
    type MainThread<'a> = AraMain;

    fn declare_extensions(builder: &mut PluginExtensions<Self>, _shared: Option<&AraShared>) {
        builder.register::<AraExtension>();
    }
}

impl DefaultPluginFactory for TestAra {
    fn get_descriptor() -> PluginDescriptor {
        use clack_plugin::plugin::features::*;
        PluginDescriptor::new(ID, "Test ARA").with_features([AUDIO_EFFECT, STEREO])
    }

    fn new_shared(_host: HostSharedHandle<'_>) -> Result<AraShared, PluginError> {
        let instance = mock::Instance::new();
        *PROBE.lock().unwrap() = Some(instance.probe());
        Ok(AraShared {
            renderer: instance.renderer(),
            held: Mutex::new(Held(instance)),
        })
    }

    fn new_main_thread<'a>(
        _host: HostMainThreadHandle<'a>,
        _shared: &'a AraShared,
    ) -> Result<AraMain, PluginError> {
        Ok(AraMain)
    }
}

fn factory() -> ClapFactory {
    let entry =
        clack_host::prelude::PluginEntry::load_from_clack::<SinglePluginEntry<TestAra>>(BUNDLE)
            .unwrap();
    faderframe_plugin_clap::set_catalog(vec![ScannedPlugin {
        id: ID.into(),
        name: "Test ARA".into(),
        vendor: "FaderFrame".into(),
        version: "1".into(),
        features: vec![
            "audio-effect".into(),
            "stereo".into(),
            faderframe_plugin_clap::ara::FEATURE.into(),
        ],
        bundle: BUNDLE.to_str().unwrap().into(),
        audio_inputs: vec![2],
        audio_outputs: vec![2],
        note_inputs: 0,
        note_outputs: 0,
    }]);
    ClapFactory::new().with_entry(BUNDLE.to_str().unwrap().into(), entry)
}

/// A ramp: sample k is k / 100 000.
struct Ramp;

impl Samples for Ramp {
    fn read(&self, start: i64, out: &mut [&mut [f32]]) -> bool {
        for o in out.iter_mut() {
            for (i, v) in o.iter_mut().enumerate() {
                let k = start + i as i64;
                *v = if (0..48_000).contains(&k) {
                    k as f32 / 100_000.0
                } else {
                    0.0
                };
            }
        }
        true
    }
}

fn model() -> Model {
    Model {
        name: Some("clap".into()),
        context: Context {
            tempo: Context::tempo_entries(&[], 0.5),
            ..Context::default()
        },
        sequences: vec![Sequence {
            key: "t:1".into(),
            name: None,
            order: 0,
            color: None,
        }],
        sources: vec![Source {
            key: "src:1".into(),
            name: None,
            frames: 48_000,
            sample_rate: RATE,
            channels: 2,
            samples: Arc::new(Ramp),
        }],
        modifications: vec![Modification {
            key: "mod:1:1".into(),
            source: "src:1".into(),
            name: None,
        }],
        regions: vec![Region {
            key: "r:1.0@mod:1:1".into(),
            modification: "mod:1:1".into(),
            sequence: "t:1".into(),
            start_in_modification: 0.0,
            duration_in_modification: 1.0,
            start_in_playback: 2.0,
            duration_in_playback: 1.0,
            transformation: 0,
            name: None,
            color: None,
        }],
    }
}

#[test]
fn an_ara_clap_plugin_is_bound_and_renders_at_the_song_position() {
    let f = factory();
    assert_eq!(f.ara_plugins(), vec![ID.to_string()]);
    let mut instance = f.instantiate(ID).unwrap();
    let ara = instance.ara_factory().expect("an ARA factory");
    assert_eq!(ara.id(), mock::FACTORY_ID);
    let mut doc = Document::new(ara, Some("clap")).unwrap();
    let m = model();
    doc.sync(&m, &mut [], None);
    let ext = instance.bind_ara(doc.controller(), Roles::ALL).unwrap();
    assert!(ext.plays() && ext.previews() && ext.views());
    let mut renderer = Renderer::new(ext);
    doc.sync(
        &m,
        &mut [Assignment {
            renderer: &mut renderer,
            regions: BTreeSet::from(["r:1.0@mod:1:1".to_string()]),
            sequences: BTreeSet::from(["t:1".to_string()]),
        }],
        None,
    );
    // Bound once, before activation: a second binding is refused.
    let mut processor = instance
        .create_processor(&ProcessConfig {
            sample_rate: RATE,
            max_block_size: 256,
            sidechain: false,
            double_precision: false,
        })
        .unwrap();
    assert!(instance.bind_ara(doc.controller(), Roles::ALL).is_err());
    let frames = 256;
    let mut run = |at: i64| {
        let mut input = AudioBuffer::new(ChannelLayout::Stereo, frames);
        input.set_len(frames);
        input.channel_mut(0).fill(0.9);
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
        let transport = faderframe_transport::TransportInfo {
            playing: true,
            sample_position: at,
            sample_rate: RATE,
            ..Default::default()
        };
        let ctx = PluginProcessContext {
            transport: &transport,
            param_events: &[],
            harmony: &faderframe_plugin_host::NO_HARMONY,
            param_mods: &[],
            note_mods: &[],
        };
        processor.process(&ctx, &mut io);
        outputs[0].channel(0).to_vec()
    };
    // Two seconds in: the region's first frames (its input ignored).
    let out = run(2 * RATE as i64);
    for (k, v) in out.iter().enumerate() {
        assert!((v - k as f32 / 100_000.0).abs() < 1e-7, "frame {k}: {v}");
    }
    assert!(
        run(RATE as i64).iter().all(|v| *v == 0.0),
        "before the region"
    );
    let probe = PROBE.lock().unwrap().clone().unwrap();
    assert_eq!(probe.region_count(), 1);
    assert!(probe.violations().is_empty(), "{:?}", probe.violations());
    drop(processor);
    drop(renderer);
    drop(instance);
    drop(doc);
    assert!(probe.summary().destroyed);
}
