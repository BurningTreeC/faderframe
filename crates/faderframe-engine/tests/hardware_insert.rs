#![allow(clippy::unwrap_used)]
//! Hardware inserts on a device whose outputs come back on its inputs
//! through a cable (one buffer plus `CABLE` frames): the ping measures
//! exactly that round trip, and with it set the return lines up with the
//! dry signal — an impulse mixed half and half comes out as one impulse;
//! without it, as two a round trip apart.

mod common;

use common::{TestProject, hits};
use faderframe_audio::{AudioCallback, DeviceBuffers, OwnedBuffers, StreamInfo};
use faderframe_core::{ChannelLayout, ParameterId, builtin};
use faderframe_engine::{EngineConfig, EngineController, EngineProcessor};
use faderframe_plugin_host::devices::hardware_insert::id;
use faderframe_project::{Impact, PluginRef, PluginSlot, SavedParameter, TrackKind};
use faderframe_timeline::MusicalTime;
use faderframe_transport::TransportCommand;
use std::collections::VecDeque;

const SR: u32 = 48_000;
const BLOCK: usize = 64;
const CABLE: usize = 37;
const CHANNELS: usize = 4;

/// The device: four channels, output `n` cabled to input `n`.
struct Device {
    bufs: OwnedBuffers,
    cable: Vec<VecDeque<f32>>,
}

impl Device {
    fn new() -> Self {
        Self {
            bufs: OwnedBuffers::new(CHANNELS, CHANNELS, BLOCK),
            cable: (0..CHANNELS)
                .map(|_| std::iter::repeat_n(0.0, BLOCK + CABLE).collect())
                .collect(),
        }
    }

    /// One callback; the first output's samples.
    fn run(&mut self, p: &mut EngineProcessor, c: &mut EngineController) -> Vec<f32> {
        self.bufs.set_frames(BLOCK);
        for ch in 0..CHANNELS {
            for s in self.bufs.input_mut(ch).iter_mut() {
                *s = self.cable[ch].pop_front().unwrap_or(0.0);
            }
        }
        p.process_device(&mut self.bufs);
        c.collect_garbage();
        for ch in 0..CHANNELS {
            let out = self.bufs.output(ch).to_vec();
            self.cable[ch].extend(out);
        }
        self.bufs.output(0).to_vec()
    }
}

fn engine(tp: &TestProject) -> (EngineController, EngineProcessor) {
    let config = EngineConfig {
        sample_rate: SR,
        ..EngineConfig::default()
    };
    let (mut c, mut p) = faderframe_engine::create(config);
    AudioCallback::prepare(
        &mut p,
        &StreamInfo {
            backend: "test",
            device: "cabled".into(),
            sample_rate: SR,
            buffer_size: BLOCK as u32,
            input_channels: CHANNELS as u16,
            output_channels: CHANNELS as u16,
            input_latency: 0,
            output_latency: 0,
        },
    );
    c.sync(&tp.project, &tp.sources, Impact::Graph).unwrap();
    (c, p)
}

/// A mono track playing one impulse at frame 2000 through a hardware
/// insert (send output 3, return input 3) at half mix.
fn project(round_trip: Option<u32>) -> TestProject {
    let mut tp = TestProject::new(SR);
    let t = tp.track(TrackKind::Audio, "Vox", ChannelLayout::Mono);
    let src = tp.impulse(1, 4_000);
    let at = tp.project.timeline.to_musical(2_000, f64::from(SR));
    tp.clip(t, src, at, 4_000);
    let mut parameters = vec![SavedParameter {
        id: ParameterId(id::MIX),
        value: 0.5,
    }];
    if let Some(r) = round_trip {
        parameters.push(SavedParameter {
            id: ParameterId(id::ROUND_TRIP),
            value: f64::from(r),
        });
    }
    let slot = PluginSlot {
        id: tp.project.ids.allocate(),
        plugin: PluginRef::builtin(builtin::HARDWARE_INSERT, "Hardware Insert"),
        bypass: false,
        parameters,
        state: None,
        sidechain: None,
    };
    tp.project.track_mut(t).unwrap().inserts.push(slot);
    tp
}

#[test]
fn the_ping_measures_the_round_trip() {
    let tp = project(None);
    let (mut c, mut p) = engine(&tp);
    let mut dev = Device::new();
    for _ in 0..4 {
        dev.run(&mut p, &mut c);
    }
    c.ping(2, 2);
    assert_eq!(c.ping_result(), None);
    for _ in 0..8 {
        dev.run(&mut p, &mut c);
    }
    assert_eq!(c.ping_result(), Some(Ok((BLOCK + CABLE) as u32)));
    // Nothing on the return: lost after a second.
    c.ping(2, 3);
    for _ in 0..(SR as usize / BLOCK + 4) {
        dev.run(&mut p, &mut c);
    }
    assert_eq!(c.ping_result(), Some(Err(())));
}

/// The master's first output while the impulse plays.
fn played(round_trip: Option<u32>) -> Vec<f32> {
    let tp = project(round_trip);
    let (mut c, mut p) = engine(&tp);
    let mut dev = Device::new();
    c.transport(TransportCommand::Locate(0)).unwrap();
    c.transport(TransportCommand::Play).unwrap();
    let mut out = Vec::new();
    for _ in 0..(10_000 / BLOCK) {
        out.extend(dev.run(&mut p, &mut c));
    }
    out
}

#[test]
fn the_return_lines_up_with_the_dry_signal() {
    let trip = (BLOCK + CABLE) as u32;
    // Unmeasured: the dry half and, a round trip later, the returned one.
    let late = played(None);
    let h = hits(&late, 0.1);
    assert_eq!(h.len(), 2, "{h:?}");
    assert_eq!(h[1] - h[0], trip as usize);
    // Measured: one impulse, the halves added up (the mono track's level
    // in the master as the dry impulse alone at full mix would have).
    let on_time = played(Some(trip));
    let h2 = hits(&on_time, 0.1);
    assert_eq!(h2.len(), 1, "{h2:?}");
    let peak = on_time[h2[0]];
    assert!(
        (peak - 2.0 * late[h[0]]).abs() < 1e-4,
        "{peak} = 2 × {}",
        late[h[0]]
    );
    // Later by the round trip (the whole mix waits for the return).
    assert_eq!(h2[0], h[0] + trip as usize);
    let _ = MusicalTime::ZERO;
}
