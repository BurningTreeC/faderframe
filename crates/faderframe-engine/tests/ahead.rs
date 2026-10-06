//! Render-ahead (anticipative processing): tracks nobody plays live are
//! rendered ahead on a thread of their own and must sound exactly as when
//! the audio thread renders them — through playback, a locate and loop
//! wraps — without the audio thread ever waiting for them.
#![allow(clippy::unwrap_used)]

use faderframe_audio::OwnedBuffers;
use faderframe_core::{PluginInstanceId, TrackId, builtin};
use faderframe_engine::offline::OfflineRenderer;
use faderframe_engine::{EngineConfig, render_generated_sources};
use faderframe_project::demo::demo_project;
use faderframe_project::{Impact, PluginRef, PluginSlot, Project};
use faderframe_transport::{LoopRange, TransportCommand};
use std::collections::{HashMap, HashSet};
use std::time::Duration;

const SR: u32 = 48_000;
const BLOCK: usize = 256;

/// The demo with `plugin` on its audio tracks (something to render)
/// instead of their own devices, and without modulators (they keep their
/// tracks live). Free-running LFOs (the chorus's, the reverb's
/// modulation) are left out: playing with render-ahead starts once the
/// rings are primed, a few blocks later, so at the same song position
/// they would be a phase apart.
fn project_with(plugin: &str) -> Project {
    let mut p = demo_project(SR);
    for t in &mut p.tracks {
        t.modulators.clear();
        if ["Drums", "Bass", "Pluck", "Pad"].contains(&t.name.as_str()) {
            t.inserts.clear();
            t.preamp = None;
        }
        for slot in t
            .inserts
            .iter_mut()
            .filter(|s| s.plugin.id == builtin::REVERB)
        {
            // Its modulation.
            slot.parameters.push(faderframe_project::SavedParameter {
                id: faderframe_core::ParameterId(7),
                value: 0.0,
            });
        }
    }
    for (i, name) in ["Drums", "Bass", "Pluck", "Pad"].iter().enumerate() {
        let t = p.tracks.iter_mut().find(|t| t.name == *name).unwrap();
        t.inserts.push(PluginSlot {
            id: PluginInstanceId(9_000 + i as u64),
            plugin: PluginRef::builtin(plugin, "Effect"),
            bypass: false,
            parameters: Vec::new(),
            state: None,
            sidechain: None,
        });
    }
    p
}

fn project() -> Project {
    project_with(builtin::ECHO)
}

/// Output that depends on the position alone: gain inserts, nothing sent
/// to the echo aux. (Stateful effects have processed the lookahead's
/// worth of audio that a locate discards: their tails differ for a while.)
fn stateless_project() -> Project {
    let mut p = project_with(builtin::GAIN);
    for t in &mut p.tracks {
        t.sends.clear();
    }
    p
}

struct Run {
    r: OfflineRenderer,
    bufs: OwnedBuffers,
    /// Left master output by transport position, while playing.
    heard: HashMap<i64, f32>,
}

impl Run {
    fn new(project: &Project, ahead: bool) -> Self {
        let sources = render_generated_sources(project, SR);
        let config = EngineConfig {
            sample_rate: SR,
            max_block_size: BLOCK,
            ..EngineConfig::default()
        };
        let mut r = OfflineRenderer::new(project, &sources, config, BLOCK, 2).unwrap();
        if ahead {
            r.controller
                .set_render_ahead(Some(Duration::from_millis(150)), 2);
            r.controller.sync(project, &sources, Impact::Graph).unwrap();
        }
        Self {
            r,
            bufs: OwnedBuffers::new(2, 2, BLOCK),
            heard: HashMap::new(),
        }
    }

    /// `blocks` callbacks; with `paced` at the device's pace (the
    /// anticipator runs alongside in real time).
    fn run(&mut self, blocks: usize, paced: bool) {
        for _ in 0..blocks {
            self.r.processor.process_device(&mut self.bufs);
            let t = self.r.controller.transport_snapshot();
            if t.playing {
                let start = t.position - BLOCK as i64;
                for (i, s) in self.bufs.output_ref(0).iter().enumerate() {
                    self.heard.insert(start + i as i64, *s);
                }
            }
            if paced {
                std::thread::sleep(Duration::from_secs_f64(BLOCK as f64 / SR as f64));
            }
            self.r.controller.collect_garbage();
        }
    }

    fn command(&mut self, cmd: TransportCommand) {
        self.r.controller.transport(cmd).unwrap();
    }
}

/// Positions both runs heard, and how many samples differ.
fn compare(a: &Run, b: &Run) -> (usize, usize) {
    let mut both = 0;
    let mut differ = 0;
    for (p, x) in &a.heard {
        if let Some(y) = b.heard.get(p) {
            both += 1;
            if x != y {
                differ += 1;
            }
        }
    }
    (both, differ)
}

#[test]
fn rendered_ahead_sounds_the_same() {
    let mut project = project();
    project.crosstalk = true;
    let slot = faderframe_project::PluginSlot {
        id: project.ids.allocate(),
        plugin: faderframe_project::PluginRef::builtin(
            faderframe_core::builtin::PREAMPS[1].0,
            "American 312",
        ),
        bypass: false,
        parameters: vec![],
        state: None,
        sidechain: None,
    };
    project
        .tracks
        .iter_mut()
        .find(|t| t.name == "Pluck")
        .unwrap()
        .preamp = Some(slot);

    for track in &mut project.tracks {
        if let Some(instrument) = track.instrument.take() {
            track.inserts.insert(0, instrument);
        }
    }
    let mut plain = Run::new(&project, false);
    let mut ahead = Run::new(&project, true);
    // Everything that can be rendered ahead is: the audio tracks with
    // their echo and the instrument track.
    let tracks: HashSet<TrackId> = ahead.r.controller.ahead_tracks().clone();
    for name in ["Drums", "Bass", "Pluck", "Pad", "Lead Synth"] {
        let id = project.tracks.iter().find(|t| t.name == name).unwrap().id;
        assert!(tracks.contains(&id), "{name} is rendered ahead");
    }
    // Let the anticipator start, then play two seconds.
    ahead.run(20, true);
    for run in [&mut plain, &mut ahead] {
        run.command(TransportCommand::Play);
    }
    let blocks = 2 * SR as usize / BLOCK;
    plain.run(blocks, false);
    ahead.run(blocks, true);
    let (both, differ) = compare(&plain, &ahead);
    assert!(both > SR as usize, "heard together: {both}");
    assert_eq!(differ, 0, "{differ} of {both} samples differ");
    assert_eq!(ahead.r.controller.ahead_misses(), 0, "never late");
    // The audio is not silent (the comparison means something).
    assert!(ahead.heard.values().any(|s| s.abs() > 0.01));
}

#[test]
fn a_locate_and_loop_wraps_continue_seamlessly() {
    let project = stateless_project();
    let mut plain = Run::new(&project, false);
    let mut ahead = Run::new(&project, true);
    ahead.run(20, true);
    // A loop of one second from 0.5 s, playing from 3 s, then a locate
    // into the loop.
    let range = LoopRange::new(24_000, 72_000);
    for run in [&mut plain, &mut ahead] {
        run.command(TransportCommand::SetLoopRange(range));
        run.command(TransportCommand::SetLoopEnabled(true));
        run.command(TransportCommand::Locate(3 * SR as i64));
        run.command(TransportCommand::Play);
    }
    let blocks = SR as usize / 2 / BLOCK;
    plain.run(blocks, false);
    ahead.run(blocks, true);
    for run in [&mut plain, &mut ahead] {
        run.command(TransportCommand::Locate(30_000));
    }
    // Three passes of the loop.
    let blocks = 3 * SR as usize / BLOCK;
    plain.run(blocks, false);
    ahead.run(blocks, true);
    let (both, differ) = compare(&plain, &ahead);
    assert!(both > SR as usize, "heard together: {both}");
    assert_eq!(differ, 0, "{differ} of {both} samples differ");
    assert_eq!(ahead.r.controller.ahead_misses(), 0, "never late");
}

#[test]
fn a_track_played_live_leaves_the_ahead_graph() {
    let project = project();
    let mut ahead = Run::new(&project, true);
    let synth = project
        .tracks
        .iter()
        .find(|t| t.name == "Lead Synth")
        .unwrap()
        .id;
    assert!(ahead.r.controller.ahead_tracks().contains(&synth));
    ahead.r.controller.set_midi_live(HashSet::from([synth]));
    ahead.r.controller.update_params(&project).unwrap();
    assert!(!ahead.r.controller.ahead_tracks().contains(&synth));
    // Not live any more: back once stopped (it is).
    ahead.r.controller.set_midi_live(HashSet::new());
    assert!(ahead.r.controller.ahead_wants_rebuild(&project));
    ahead.r.controller.update_params(&project).unwrap();
    assert!(ahead.r.controller.ahead_tracks().contains(&synth));
    ahead.run(4, false);
}

#[test]
fn changing_the_lookahead_keeps_every_track_sounding() {
    let project = stateless_project();
    let sources = render_generated_sources(&project, SR);
    let mut plain = Run::new(&project, false);
    let mut ahead = Run::new(&project, true);
    ahead.run(20, true);
    let tracks = ahead.r.controller.ahead_tracks().len();
    assert!(tracks > 0);
    // A longer lookahead while stopped: new rings, a new anticipator, the
    // same tracks rendered ahead.
    ahead
        .r
        .controller
        .set_render_ahead(Some(Duration::from_millis(300)), 2);
    ahead
        .r
        .controller
        .sync(&project, &sources, Impact::Graph)
        .unwrap();
    assert_eq!(ahead.r.controller.ahead_tracks().len(), tracks);
    ahead.run(20, true);
    for run in [&mut plain, &mut ahead] {
        run.command(TransportCommand::Play);
    }
    let blocks = 2 * SR as usize / BLOCK;
    plain.run(blocks, false);
    ahead.run(blocks, true);
    let (both, differ) = compare(&plain, &ahead);
    assert!(both > SR as usize, "heard together: {both}");
    assert_eq!(differ, 0, "{differ} of {both} samples differ");
    assert_eq!(ahead.r.controller.ahead_misses(), 0, "never late");
}
