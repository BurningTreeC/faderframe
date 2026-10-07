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
        Self::with(project, ahead, false)
    }

    /// With render-ahead and `buses` rendered ahead too.
    fn with(project: &Project, ahead: bool, buses: bool) -> Self {
        let sources = render_generated_sources(project, SR);
        let config = EngineConfig {
            sample_rate: SR,
            max_block_size: BLOCK,
            ..EngineConfig::default()
        };
        let mut r = OfflineRenderer::new(project, &sources, config, BLOCK, 2).unwrap();
        if ahead {
            r.controller.set_render_ahead_buses(buses);
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

/// Rendered ahead, a container's chains and modulators that follow the
/// song position sound as they do live. (While stopped an LFO keeps
/// moving, for as long as each graph happened to run stopped: the first
/// block after Play glides from there in each. The master's limiter, whose
/// release would carry that block on, is left out.)
#[test]
fn containers_and_synced_modulators_render_ahead_alike() {
    use faderframe_project::container::Chain;
    use faderframe_project::modulation::{
        LfoShape, ModRate, ModRoute, ModSource, ModTarget, Modulator,
    };
    let mut project = stateless_project();
    let pluck = project
        .tracks
        .iter()
        .position(|t| t.name == "Pluck")
        .unwrap();
    // A container (a utility beside the dry signal) and synced LFOs on the
    // utilities in and outside it.
    let utility = |id: u64| PluginSlot {
        id: PluginInstanceId(id),
        plugin: PluginRef::builtin(builtin::GAIN, "Utility"),
        bypass: false,
        parameters: Vec::new(),
        state: None,
        sidechain: None,
    };
    let container = PluginSlot {
        id: PluginInstanceId(9_100),
        plugin: PluginRef::builtin(builtin::CONTAINER, "Container"),
        bypass: false,
        parameters: Vec::new(),
        state: None,
        sidechain: None,
    };
    let mut wet = Chain::new("Wet");
    wet.inserts.push(utility(9_101));
    wet.gain_db = -6.0;
    let gain = |plugin: u64| ModTarget::Plugin {
        plugin: PluginInstanceId(plugin),
        parameter: faderframe_core::ParameterId(0),
    };
    let lfo = |id: u64, shape, beats, target| {
        let mut m = Modulator::new(
            faderframe_core::ModulatorId(id),
            ModSource::Lfo {
                shape,
                rate: ModRate::Sync { beats },
                phase: 0.0,
            },
        );
        m.routes = vec![ModRoute { target, depth: 0.1 }];
        m
    };
    for t in &mut project.tracks {
        if t.kind == faderframe_project::TrackKind::Master {
            t.inserts.clear();
        }
    }
    let t = &mut project.tracks[pluck];
    t.containers
        .insert(container.id, vec![Chain::new("Dry"), wet]);
    t.inserts.push(container);
    t.modulators = vec![
        lfo(1, LfoShape::Sine, 1.0, gain(9_002)),
        lfo(2, LfoShape::Triangle, 0.5, gain(9_101)),
    ];
    let id = project.tracks[pluck].id;
    let mut plain = Run::new(&project, false);
    let mut ahead = Run::new(&project, true);
    assert!(
        ahead.r.controller.ahead_tracks().contains(&id),
        "rendered ahead"
    );
    ahead.run(20, true);
    for run in [&mut plain, &mut ahead] {
        run.command(TransportCommand::Play);
    }
    let blocks = 2 * SR as usize / BLOCK;
    plain.run(blocks, false);
    ahead.run(blocks, true);
    for run in [&mut plain, &mut ahead] {
        run.heard.retain(|p, _| *p >= 2 * BLOCK as i64);
    }
    let (both, differ) = compare(&plain, &ahead);
    assert!(both > SR as usize, "heard together: {both}");
    assert_eq!(differ, 0, "{differ} of {both} samples differ");
    assert_eq!(ahead.r.controller.ahead_misses(), 0, "never late");
    // Modulating the fader keeps the track live.
    project.tracks[pluck].modulators[0].routes[0].target = ModTarget::Volume;
    let live = Run::new(&project, true);
    assert!(!live.r.controller.ahead_tracks().contains(&id));
}

/// Buses rendered ahead sound as they do live: the drum bus, the auxes and
/// the master's chain, with every strip and send that reaches them
/// rendered ahead too. Their meters still move, as the audio is heard.
#[test]
fn buses_rendered_ahead_sound_the_same() {
    let mut project = project();
    // The arpeggios' synth plays a MIDI track's notes (on the audio
    // thread): without them, everything reaches the master from ahead.
    project
        .tracks
        .retain(|t| !["Chords", "Arp Synth", "Arpeggios"].contains(&t.name.as_str()));
    let id = |name: &str| project.tracks.iter().find(|t| t.name == name).unwrap().id;
    let mut plain = Run::new(&project, false);
    let mut ahead = Run::with(&project, true, true);
    let c = &ahead.r.controller;
    for name in ["Drum Bus", "Echo", "Space", "Master"] {
        assert!(
            c.ahead_tracks().contains(&id(name)),
            "{name} rendered ahead"
        );
    }
    for name in [
        "Drums",
        "Bass",
        "Pluck",
        "Pad",
        "Lead Synth",
        "Drum Bus",
        "Echo",
        "Space",
    ] {
        assert!(c.ahead_strips().contains(&id(name)), "{name}'s strip ahead");
    }
    // The master's strip plays live (to the device).
    assert!(!c.ahead_strips().contains(&id("Master")));
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
    assert!(ahead.heard.values().any(|s| s.abs() > 0.01));
    // The drums' meter, from its echo on the audio thread.
    let meter = ahead.r.controller.take_meter(id("Drums")).unwrap();
    assert!(meter.left.peak > 0.01, "metered: {:?}", meter.left);
}

/// A bus is rendered ahead only when everything that reaches it is; a
/// track that plays live keeps its buses live, and while playing a strip
/// that has to come back brings its chain (no gap), and its buses theirs.
#[test]
fn a_live_input_keeps_its_buses_live() {
    let project = project();
    let id = |name: &str| project.tracks.iter().find(|t| t.name == name).unwrap().id;
    let mut ahead = Run::with(&project, true, true);
    let c = &ahead.r.controller;
    // The drum bus: only the drums reach it.
    assert!(c.ahead_tracks().contains(&id("Drum Bus")));
    assert!(c.ahead_strips().contains(&id("Drums")));
    // The arpeggios' synth plays on the audio thread and reaches the
    // master: the master stays live, so do the strips reaching it (the lead
    // synth's), and the auxes they send to.
    assert!(!c.ahead_tracks().contains(&id("Master")));
    assert!(!c.ahead_strips().contains(&id("Lead Synth")));
    assert!(
        c.ahead_tracks().contains(&id("Lead Synth")),
        "its chain still is"
    );
    assert!(!c.ahead_tracks().contains(&id("Echo")));
    // Playing: the drums go live; the drum bus comes back, and with it its
    // strip's chain (the drums are back whole).
    ahead.run(20, true);
    ahead.command(TransportCommand::Play);
    ahead.run(10, true);
    let drums = id("Drums");
    ahead.r.controller.set_midi_live(HashSet::from([drums]));
    ahead.r.controller.update_params(&project).unwrap();
    let c = &ahead.r.controller;
    assert!(!c.ahead_tracks().contains(&drums));
    assert!(!c.ahead_tracks().contains(&id("Drum Bus")));
    // Nothing joins while playing.
    ahead.r.controller.set_midi_live(HashSet::new());
    ahead.r.controller.update_params(&project).unwrap();
    assert!(!ahead.r.controller.ahead_tracks().contains(&id("Drum Bus")));
    ahead.run(10, true);
    ahead.command(TransportCommand::Stop);
    ahead.run(4, true);
    assert!(ahead.r.controller.ahead_wants_rebuild(&project));
    ahead.r.controller.update_params(&project).unwrap();
    assert!(ahead.r.controller.ahead_tracks().contains(&id("Drum Bus")));
}

/// Latency rendered ahead (a limiter's lookahead on the drums, before the
/// drum bus) is compensated where it meets live tracks at the master.
#[test]
fn latency_reaching_a_bus_rendered_ahead_is_compensated() {
    let mut project = project();
    let drums = project
        .tracks
        .iter_mut()
        .find(|t| t.name == "Drums")
        .unwrap();
    drums.inserts.push(PluginSlot {
        id: PluginInstanceId(9_200),
        plugin: PluginRef::builtin(builtin::LIMITER, "Limiter"),
        bypass: false,
        parameters: Vec::new(),
        state: None,
        sidechain: None,
    });
    let drums = drums.id;
    let bus = project
        .tracks
        .iter()
        .find(|t| t.name == "Drum Bus")
        .unwrap()
        .id;
    let mut plain = Run::new(&project, false);
    let mut ahead = Run::with(&project, true, true);
    assert!(ahead.r.controller.ahead_strips().contains(&drums));
    assert!(ahead.r.controller.ahead_tracks().contains(&bus));
    assert!(!ahead.r.controller.ahead_strips().contains(&bus));
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
