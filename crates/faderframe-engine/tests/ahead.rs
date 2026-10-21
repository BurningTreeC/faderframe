//! Render-ahead (anticipative processing): tracks nobody plays live are
//! rendered ahead on a thread of their own and must sound exactly as when
//! the audio thread renders them — through playback, a locate and loop
//! wraps — without the audio thread ever waiting for them.
#![allow(clippy::unwrap_used)]
mod common;

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
    /// The transport's position after each callback (playing or not).
    positions: Vec<i64>,
    /// [`Self::heard`] of the callbacks that ran on from the one before
    /// (no wrap or jump in them: positions known for sure).
    ran_on: HashMap<i64, f32>,
}

impl Run {
    fn new(project: &Project, ahead: bool) -> Self {
        Self::with(project, ahead, false)
    }

    /// With render-ahead and `buses` rendered ahead too.
    fn with(project: &Project, ahead: bool, buses: bool) -> Self {
        Self::with_sources(project, render_generated_sources(project, SR), ahead, buses)
    }

    fn with_sources(
        project: &Project,
        sources: faderframe_engine::SourceMap,
        ahead: bool,
        buses: bool,
    ) -> Self {
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
            positions: Vec::new(),
            ran_on: HashMap::new(),
        }
    }

    /// `blocks` callbacks; with `paced` at the device's pace (the
    /// anticipator runs alongside in real time).
    fn run(&mut self, blocks: usize, paced: bool) {
        for _ in 0..blocks {
            self.r.processor.process_device(&mut self.bufs);
            let t = self.r.controller.transport_snapshot();
            let ran_on = self.positions.last() == Some(&(t.position - BLOCK as i64));
            self.positions.push(t.position);
            if t.playing {
                let start = t.position - BLOCK as i64;
                for (i, s) in self.bufs.output_ref(0).iter().enumerate() {
                    self.heard.insert(start + i as i64, *s);
                    if ran_on {
                        self.ran_on.insert(start + i as i64, *s);
                    }
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

/// Paced runs share the machine with the other tests: a hosted runner can
/// keep the anticipator from a deadline once, which plays a block of
/// silence (a miss) where nothing else differs. `attempt` plays one paced
/// comparison and returns how often it was late, asserting the audio only
/// when it never was; a late one is tried again, three times at most.
fn on_time(mut attempt: impl FnMut() -> u64) {
    for tries in 1..=3 {
        let misses = attempt();
        if misses == 0 {
            return;
        }
        eprintln!("try {tries}: {misses} blocks late (a busy machine): again");
    }
    panic!("late in every try: the anticipator does not keep up here");
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
    on_time(|| {
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
        let misses = ahead.r.controller.ahead_misses();
        if misses > 0 {
            return misses;
        }
        let (both, differ) = compare(&plain, &ahead);
        assert!(both > SR as usize, "heard together: {both}");
        assert_eq!(differ, 0, "{differ} of {both} samples differ");
        // The audio is not silent (the comparison means something).
        assert!(ahead.heard.values().any(|s| s.abs() > 0.01));
        0
    });
}

#[test]
fn a_locate_and_loop_wraps_continue_seamlessly() {
    let project = stateless_project();
    on_time(|| {
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
        let misses = ahead.r.controller.ahead_misses();
        if misses > 0 {
            return misses;
        }
        let (both, differ) = compare(&plain, &ahead);
        assert!(both > SR as usize, "heard together: {both}");
        assert_eq!(differ, 0, "{differ} of {both} samples differ");
        0
    });
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
    on_time(|| {
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
        let misses = ahead.r.controller.ahead_misses();
        if misses > 0 {
            return misses;
        }
        let (both, differ) = compare(&plain, &ahead);
        assert!(both > SR as usize, "heard together: {both}");
        assert_eq!(differ, 0, "{differ} of {both} samples differ");
        0
    });
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
    on_time(|| {
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
        let misses = ahead.r.controller.ahead_misses();
        if misses > 0 {
            return misses;
        }
        let (both, differ) = compare(&plain, &ahead);
        assert!(both > SR as usize, "heard together: {both}");
        assert_eq!(differ, 0, "{differ} of {both} samples differ");
        0
    });
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
    on_time(|| {
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
        let misses = ahead.r.controller.ahead_misses();
        if misses > 0 {
            return misses;
        }
        let (both, differ) = compare(&plain, &ahead);
        assert!(both > SR as usize, "heard together: {both}");
        assert_eq!(differ, 0, "{differ} of {both} samples differ");
        assert!(ahead.heard.values().any(|s| s.abs() > 0.01));
        // The drums' meter, from its echo on the audio thread.
        let meter = ahead.r.controller.take_meter(id("Drums")).unwrap();
        assert!(meter.left.peak > 0.01, "metered: {:?}", meter.left);
        0
    });
}

/// A folder bus is rendered ahead as a bus is (it and the strips of what
/// it holds in the shallow tier) and sounds exactly as it plays live.
#[test]
fn a_folder_bus_renders_ahead_as_it_plays() {
    use faderframe_core::{ChannelLayout, TrackId};
    use faderframe_project::{Track, TrackColor, TrackKind};
    let mut project = project();
    project
        .tracks
        .retain(|t| !["Chords", "Arp Synth", "Arpeggios"].contains(&t.name.as_str()));
    let id = |p: &Project, name: &str| p.tracks.iter().find(|t| t.name == name).unwrap().id;
    let keys: TrackId = project.ids.allocate();
    let mut folder = Track::new(keys, TrackKind::Bus, "Keys", TrackColor::palette(2))
        .with_layout(ChannelLayout::Stereo);
    folder.folder_bus = true;
    folder.inserts.push(PluginSlot {
        id: PluginInstanceId(9_400),
        plugin: PluginRef::builtin(builtin::GAIN, "Utility"),
        bypass: false,
        parameters: vec![faderframe_project::SavedParameter {
            id: faderframe_core::ParameterId(0),
            value: -3.0,
        }],
        state: None,
        sidechain: None,
    });
    folder.volume_db = -2.0;
    folder.pan = 0.3;
    let at = project.tracks.len() - 1;
    project.tracks.insert(at, folder);
    for name in ["Pluck", "Pad"] {
        let t = id(&project, name);
        let t = project.track_mut(t).unwrap();
        t.folder = Some(keys);
        t.output = faderframe_project::OutputRouting::Master;
    }
    on_time(|| {
        let mut plain = Run::new(&project, false);
        let mut ahead = Run::with(&project, true, true);
        let c = &ahead.r.controller;
        assert!(c.ahead_tracks().contains(&keys), "the folder bus ahead");
        for name in ["Pluck", "Pad", "Keys"] {
            assert!(
                c.ahead_strips().contains(&id(&project, name)),
                "{name}'s strip ahead"
            );
        }
        ahead.run(20, true);
        for run in [&mut plain, &mut ahead] {
            run.command(TransportCommand::Play);
        }
        let blocks = 2 * SR as usize / BLOCK;
        plain.run(blocks, false);
        ahead.run(blocks, true);
        let misses = ahead.r.controller.ahead_misses();
        if misses > 0 {
            return misses;
        }
        let (both, differ) = compare(&plain, &ahead);
        assert!(both > SR as usize, "heard together: {both}");
        assert_eq!(differ, 0, "{differ} of {both} samples differ");
        assert!(ahead.heard.values().any(|s| s.abs() > 0.01));
        0
    });
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
    on_time(|| {
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
        let misses = ahead.r.controller.ahead_misses();
        if misses > 0 {
            return misses;
        }
        let (both, differ) = compare(&plain, &ahead);
        assert!(both > SR as usize, "heard together: {both}");
        assert_eq!(differ, 0, "{differ} of {both} samples differ");
        0
    });
}

/// With buses rendered ahead, closing the fader of a track that reaches
/// one is heard after the shallow tier's few milliseconds, not the deep
/// tier's lookahead (150 ms here).
#[test]
fn faders_reaching_buses_rendered_ahead_answer_within_milliseconds() {
    use faderframe_core::ChannelLayout;
    use faderframe_project::{OutputRouting, TrackKind};
    let mut tp = common::TestProject::new(SR);
    let track = tp.track(TrackKind::Audio, "Tone", ChannelLayout::Stereo);
    let bus = tp.track(TrackKind::Bus, "Bus", ChannelLayout::Stereo);
    let tone: Vec<f32> = (0..30 * SR as usize)
        .map(|n| 0.25 * (n as f32 * std::f32::consts::TAU * 220.0 / SR as f32).sin())
        .collect();
    let src = tp.source(faderframe_audio_files::AudioData::from_channels(
        SR,
        vec![tone.clone(), tone],
    ));
    tp.clip(
        track,
        src,
        faderframe_timeline::MusicalTime::ZERO,
        30 * SR as i64,
    );
    let gain = |id| PluginSlot {
        id: PluginInstanceId(id),
        plugin: PluginRef::builtin(builtin::GAIN, "Utility"),
        bypass: false,
        parameters: Vec::new(),
        state: None,
        sidechain: None,
    };
    let t = tp.project.track_mut(track).unwrap();
    t.output = OutputRouting::Track { track: bus };
    t.inserts.push(gain(9_300));
    tp.project.track_mut(bus).unwrap().inserts.push(gain(9_301));
    let mut project = tp.project.clone();
    for buses in [false, true] {
        on_time(|| {
            let mut run = Run::with_sources(&project, tp.sources.clone(), true, buses);
            assert_eq!(run.r.controller.ahead_strips().contains(&track), buses);
            assert_eq!(run.r.controller.ahead_tracks().contains(&bus), buses);
            run.run(20, true);
            run.command(TransportCommand::Play);
            run.run(SR as usize / BLOCK, true);
            assert!(run.heard.values().any(|s| s.abs() > 0.1), "sounding");
            // Close the fader.
            project.track_mut(track).unwrap().volume_db = -144.0;
            let at = run.r.controller.transport_snapshot().position;
            run.r.controller.update_params(&project).unwrap();
            run.run(SR as usize / 2 / BLOCK, true);
            project.track_mut(track).unwrap().volume_db = 0.0;
            // Late (a busy machine): the timing says nothing; again.
            let misses = run.r.controller.ahead_misses();
            if misses > 0 {
                return misses;
            }
            // Silent from there: a whole cycle of the tone below -80 dB.
            let silent = (at..at + SR as i64 / 2)
                .find(|p| (0..240).all(|i| run.heard.get(&(p + i)).is_some_and(|s| s.abs() < 1e-4)))
                .unwrap();
            let ms = (silent - at) as f64 * 1000.0 / f64::from(SR);
            eprintln!("buses {buses}: the fader is heard after {ms:.1} ms");
            assert!(ms < 30.0, "buses {buses}: heard after {ms:.1} ms");
            0
        });
    }
}

/// A tone track (a utility on it: rendered ahead), with `bus` through a
/// bus with a utility (rendered ahead by the shallow tier), and nothing
/// else that keeps state: every sample heard is a function of its
/// position alone, whatever path the transport took to get there.
fn tone_project(bus: bool) -> (Project, faderframe_engine::SourceMap) {
    use faderframe_core::ChannelLayout;
    use faderframe_project::{OutputRouting, TrackKind};
    let mut tp = common::TestProject::new(SR);
    let track = tp.track(TrackKind::Audio, "Tone", ChannelLayout::Stereo);
    // A tone that rises: no two positions sound alike.
    let tone: Vec<f32> = (0..20 * SR as usize)
        .map(|n| {
            let t = n as f32 / SR as f32;
            0.25 * (std::f32::consts::TAU * (220.0 + 10.0 * t) * t).sin()
        })
        .collect();
    let src = tp.source(faderframe_audio_files::AudioData::from_channels(
        SR,
        vec![tone.clone(), tone],
    ));
    tp.clip(
        track,
        src,
        faderframe_timeline::MusicalTime::ZERO,
        20 * SR as i64,
    );
    let utility = |id| PluginSlot {
        id: PluginInstanceId(id),
        plugin: PluginRef::builtin(builtin::GAIN, "Utility"),
        bypass: false,
        parameters: Vec::new(),
        state: None,
        sidechain: None,
    };
    tp.project
        .track_mut(track)
        .unwrap()
        .inserts
        .push(utility(9_400));
    if bus {
        let b = tp.track(TrackKind::Bus, "Bus", ChannelLayout::Stereo);
        tp.project.track_mut(track).unwrap().output = OutputRouting::Track { track: b };
        tp.project
            .track_mut(b)
            .unwrap()
            .inserts
            .push(utility(9_401));
    }
    (tp.project, tp.sources)
}

/// What a run of callbacks did: `(index, from, to)` of the callbacks
/// after which the transport neither moved on by a callback's frames nor
/// wrapped exactly at the end of one of `loops` (a repeat, a skip), and of
/// those that wrapped.
struct Steps {
    irregular: Vec<(usize, i64, i64)>,
    wraps: Vec<(usize, i64, i64)>,
}

fn steps(positions: &[i64], loops: &[(i64, i64)]) -> Steps {
    let mut out = Steps {
        irregular: Vec::new(),
        wraps: Vec::new(),
    };
    let block = BLOCK as i64;
    for (i, w) in positions.windows(2).enumerate() {
        let (a, b) = (w[0], w[1]);
        if b == a + block {
            continue;
        }
        let wrapped = loops
            .iter()
            .any(|&(start, end)| a < end && a + block >= end && b == start + (a + block - end));
        if wrapped {
            out.wraps.push((i + 1, a, b));
        } else {
            out.irregular.push((i + 1, a, b));
        }
    }
    out
}

/// Positions heard by both runs (on callbacks that ran on), and how many
/// samples differ: with [`tone_project`], none may, whatever the paths.
fn compare_ran_on(a: &Run, b: &Run) -> (usize, usize) {
    let mut both = 0;
    let mut differ = 0;
    for (p, x) in &a.ran_on {
        if let Some(y) = b.ran_on.get(p) {
            both += 1;
            if x != y {
                differ += 1;
            }
        }
    }
    (both, differ)
}

fn set_loop(runs: &mut [&mut Run], start: i64, end: i64) {
    for run in runs.iter_mut() {
        run.command(TransportCommand::SetLoopRange(LoopRange::new(start, end)));
    }
}

/// Dragging a loop's end while playing (every few callbacks a new range,
/// far from the playhead) was a repeat of the last callback per change —
/// each change started a new sequence of the anticipators and the switch
/// put the playhead back to where the change was made. Now the transport
/// goes on frame by frame, nothing repeats or drops out, it sounds exactly
/// as without rendering ahead, and the loop wraps at the end it was given
/// last.
#[test]
fn resizing_the_loop_while_playing_neither_repeats_nor_drops_out() {
    let (project, sources) = tone_project(false);
    on_time(|| {
        let mut plain = Run::with_sources(&project, sources.clone(), false, false);
        let mut ahead = Run::with_sources(&project, sources.clone(), true, false);
        assert!(!ahead.r.controller.ahead_tracks().is_empty());
        ahead.run(20, true);
        let (start, mut end) = (24_000i64, 24_000 + 2 * SR as i64);
        let mut loops = vec![(start, end)];
        for run in [&mut plain, &mut ahead] {
            run.command(TransportCommand::SetLoopRange(LoopRange::new(start, end)));
            run.command(TransportCommand::SetLoopEnabled(true));
            run.command(TransportCommand::Locate(SR as i64));
            run.command(TransportCommand::Play);
        }
        plain.run(40, false);
        ahead.run(40, true);
        let (first_plain, first) = (plain.positions.len(), ahead.positions.len());
        for _ in 0..40 {
            end += 480;
            loops.push((start, end));
            set_loop(&mut [&mut plain, &mut ahead], start, end);
            plain.run(4, false);
            ahead.run(4, true);
        }
        // On to the wrap.
        plain.run(200, false);
        ahead.run(200, true);
        let misses = ahead.r.controller.ahead_misses();
        let s = steps(&ahead.positions[first..], &loops);
        eprintln!(
            "rendered ahead: {} irregular steps in {} callbacks, {misses} misses, wraps {:?}: {:?}",
            s.irregular.len(),
            ahead.positions.len() - first,
            s.wraps,
            &s.irregular[..s.irregular.len().min(12)]
        );
        if misses > 0 {
            return misses;
        }
        assert!(
            s.irregular.is_empty(),
            "the transport jumped: {:?}",
            s.irregular
        );
        let p = steps(&plain.positions[first_plain..], &loops);
        assert!(p.irregular.is_empty(), "{:?}", p.irregular);
        // Once, at the end as dragged to last.
        for wraps in [&s.wraps, &p.wraps] {
            assert_eq!(wraps.len(), 1, "{wraps:?}");
            let (_, a, b) = wraps[0];
            assert!(a < end && a + BLOCK as i64 >= end, "wrapped at {a}");
            assert_eq!(b, start + a + BLOCK as i64 - end);
        }
        let (both, differ) = compare_ran_on(&plain, &ahead);
        assert!(both > SR as usize, "heard together: {both}");
        assert_eq!(differ, 0, "{differ} of {both} samples differ");
        assert!(ahead.ran_on.values().any(|s| s.abs() > 0.1), "sounding");
        0
    });
}

/// Where a loop change lands while playing, rendering ahead: beyond what
/// the anticipator has rendered (here 150 ms and two blocks), from there
/// exactly as asked. A loop end pulled in well ahead of the playhead wraps
/// there; one pushed out just before the playhead reaches the old end
/// wraps at the old end once (that pass was rendered), then at the new
/// one; an end pulled in behind the playhead lets it play on. Never a
/// repeat or a gap.
#[test]
fn loop_changes_near_the_playhead_land_beyond_the_lookahead() {
    let (project, sources) = tone_project(false);
    on_time(|| {
        let mut ahead = Run::with_sources(&project, sources.clone(), true, false);
        ahead.run(20, true);
        let start = 24_000i64;
        let mut loops = vec![(start, 10 * SR as i64)];
        ahead.command(TransportCommand::SetLoopRange(LoopRange::new(
            start,
            10 * SR as i64,
        )));
        ahead.command(TransportCommand::SetLoopEnabled(true));
        ahead.command(TransportCommand::Locate(SR as i64));
        ahead.command(TransportCommand::Play);
        ahead.run(40, true);
        let first = ahead.positions.len();
        // 1. Pulled in to 0.3 s ahead: wraps there.
        let at = ahead.r.controller.transport_snapshot().position;
        let near = at + (0.3 * f64::from(SR)) as i64;
        loops.push((start, near));
        ahead.command(TransportCommand::SetLoopRange(LoopRange::new(start, near)));
        ahead.run(80, true);
        // 2. Pushed out 20 ms before the playhead gets to the end: the
        // pass rendered still wraps there, the next at the new end.
        let short = (start, start + SR as i64 / 2);
        loops.push(short);
        ahead.command(TransportCommand::SetLoopRange(LoopRange::new(
            short.0, short.1,
        )));
        let mut guard = 0;
        while ahead.r.controller.transport_snapshot().position < short.1 - SR as i64 / 50 {
            ahead.run(1, true);
            guard += 1;
            assert!(guard < 400, "never got near the end");
        }
        let wraps_before = steps(&ahead.positions[first..], &loops).wraps.len();
        let long = (start, start + SR as i64);
        loops.push(long);
        ahead.command(TransportCommand::SetLoopRange(LoopRange::new(
            long.0, long.1,
        )));
        ahead.run(4 * SR as usize / BLOCK / 2, true);
        // 3. Pulled in behind the playhead, early in a pass (no wrap of
        // the old range before it lands): it plays on past it.
        let mut guard = 0;
        let at = loop {
            let at = ahead.r.controller.transport_snapshot().position;
            if at > start + 9_600 && at < long.1 - 2 * SR as i64 / 5 {
                break at;
            }
            ahead.run(1, true);
            guard += 1;
            assert!(guard < 400, "never early in a pass");
        };
        let behind = (start, start + 4_800);
        loops.push(behind);
        ahead.command(TransportCommand::SetLoopRange(LoopRange::new(
            behind.0, behind.1,
        )));
        ahead.run(60, true);
        let misses = ahead.r.controller.ahead_misses();
        if misses > 0 {
            return misses;
        }
        let s = steps(&ahead.positions[first..], &loops);
        assert!(
            s.irregular.is_empty(),
            "the transport jumped: {:?}",
            s.irregular
        );
        let ends: Vec<i64> = s
            .wraps
            .iter()
            .map(|&(_, a, b)| a + BLOCK as i64 - (b - start))
            .collect();
        eprintln!("wrapped at the ends {ends:?}");
        assert_eq!(ends[0], near, "the end pulled in");
        assert_eq!(ends[wraps_before], short.1, "the pass already rendered");
        assert_eq!(ends[wraps_before + 1], long.1, "then the end pushed out");
        assert!(ends[wraps_before + 1..].iter().all(|e| *e == long.1));
        // Played on past the end behind it.
        let last = *ahead.positions.last().unwrap();
        assert!(last > at + 50 * BLOCK as i64, "played on: {last}");
        0
    });
}

/// Loop changes in quick succession while playing — the range moved,
/// resized, crossed (no range), switched off and on, around the playhead
/// and through its wraps — keep rendering ahead seamless: no repeat, no
/// skip, no late block, and every sample is the one of its position.
#[test]
fn rapid_loop_changes_while_playing_stay_seamless() {
    rapid_loop_changes(false);
    // With a bus rendered ahead by the shallow tier too.
    rapid_loop_changes(true);
}

fn rapid_loop_changes(buses: bool) {
    let (project, sources) = tone_project(buses);
    on_time(|| {
        let mut plain = Run::with_sources(&project, sources.clone(), false, false);
        let mut ahead = Run::with_sources(&project, sources.clone(), true, buses);
        if buses {
            let bus = project.tracks.iter().find(|t| t.name == "Bus").unwrap().id;
            assert!(
                ahead.r.controller.ahead_tracks().contains(&bus),
                "the bus ahead"
            );
        }
        ahead.run(20, true);
        let mut loops = vec![(24_000i64, 72_000i64)];
        for run in [&mut plain, &mut ahead] {
            run.command(TransportCommand::SetLoopRange(LoopRange::new(
                24_000, 72_000,
            )));
            run.command(TransportCommand::SetLoopEnabled(true));
            run.command(TransportCommand::Locate(30_000));
            run.command(TransportCommand::Play);
        }
        plain.run(10, false);
        ahead.run(10, true);
        let (first_plain, first) = (plain.positions.len(), ahead.positions.len());
        // A fixed pseudo-random walk.
        let mut seed = 0x2545_f491_4f6c_dd1du64;
        let mut next = |n: u64| {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed % n
        };
        // A loop of about a second that the playhead keeps wrapping in,
        // its ends nudged by up to 25 ms either way all the time; now and
        // then switched off and back on, crossed (no range) and given back,
        // moved to wherever the playhead is.
        let (mut ls, mut le) = (24_000i64, 72_000i64);
        let mut enabled = true;
        for _ in 0..400 {
            let pos = ahead.r.controller.transport_snapshot().position;
            let mut cmds = Vec::new();
            match next(20) {
                0 | 1 => {
                    enabled = !enabled;
                    cmds.push(TransportCommand::SetLoopEnabled(enabled));
                }
                2 => {
                    // Crossed, then back.
                    cmds.push(TransportCommand::SetLoopRange(LoopRange::new(le, ls)));
                    cmds.push(TransportCommand::SetLoopRange(LoopRange::new(ls, le)));
                }
                3 => {
                    // Around the playhead, its end well beyond the
                    // lookahead.
                    ls = (pos - next(SR as u64 / 2) as i64).max(0);
                    le = pos + 12_000 + next(SR as u64 / 2) as i64;
                    cmds.push(TransportCommand::SetLoopRange(LoopRange::new(ls, le)));
                }
                4..=6 => {
                    ls = (ls + next(2_401) as i64 - 1_200).max(0);
                    le = le.max(ls + 12_000);
                    cmds.push(TransportCommand::SetLoopRange(LoopRange::new(ls, le)));
                }
                _ => {
                    le = (le + next(2_401) as i64 - 1_200).max(ls + 12_000);
                    cmds.push(TransportCommand::SetLoopRange(LoopRange::new(ls, le)));
                }
            }
            loops.push((ls, le));
            let wait = 1 + next(3) as usize;
            for (k, cmd) in cmds.into_iter().enumerate() {
                if k > 0 {
                    plain.run(1, false);
                    ahead.run(1, true);
                }
                for run in [&mut plain, &mut ahead] {
                    run.command(cmd);
                }
            }
            plain.run(wait, false);
            ahead.run(wait, true);
            // Switched off for long: back on.
            if !enabled && next(2) == 0 {
                enabled = true;
                for run in [&mut plain, &mut ahead] {
                    run.command(TransportCommand::SetLoopEnabled(true));
                }
            }
        }
        let misses = ahead.r.controller.ahead_misses();
        if misses > 0 {
            return misses;
        }
        let s = steps(&ahead.positions[first..], &loops);
        eprintln!(
            "buses {buses}: {} wraps, {} irregular steps in {} callbacks",
            s.wraps.len(),
            s.irregular.len(),
            ahead.positions.len() - first
        );
        assert!(
            s.irregular.is_empty(),
            "the transport jumped: {:?}",
            s.irregular
        );
        assert!(s.wraps.len() >= 2, "wrapped: {}", s.wraps.len());
        let p = steps(&plain.positions[first_plain..], &loops);
        assert!(p.irregular.is_empty(), "{:?}", p.irregular);
        let (both, differ) = compare_ran_on(&plain, &ahead);
        assert!(both > SR as usize / 4, "heard together: {both}");
        assert_eq!(differ, 0, "{differ} of {both} samples differ");
        0
    });
}

/// Stopped, a loop change is in force at once: playing from there wraps
/// at the new end on the first pass.
#[test]
fn a_loop_changed_while_stopped_holds_from_the_first_pass() {
    let (project, sources) = tone_project(false);
    on_time(|| {
        let mut ahead = Run::with_sources(&project, sources.clone(), true, false);
        ahead.run(20, true);
        ahead.command(TransportCommand::SetLoopRange(LoopRange::new(
            24_000, 120_000,
        )));
        ahead.command(TransportCommand::SetLoopEnabled(true));
        ahead.command(TransportCommand::Locate(30_000));
        ahead.run(20, true);
        ahead.command(TransportCommand::SetLoopRange(LoopRange::new(
            24_000, 40_000,
        )));
        ahead.run(3, true);
        let first = ahead.positions.len();
        ahead.command(TransportCommand::Play);
        ahead.run(60, true);
        let misses = ahead.r.controller.ahead_misses();
        if misses > 0 {
            return misses;
        }
        let s = steps(&ahead.positions[first..], &[(24_000, 40_000)]);
        // The first step is the start (from where it stood).
        assert!(s.irregular.len() <= 1, "{:?}", s.irregular);
        let (_, a, _) = s.wraps[0];
        assert!(a < 40_000 && a + BLOCK as i64 >= 40_000, "{:?}", s.wraps);
        0
    });
}

/// Automation behind latency, rendered ahead: a utility's mute after a
/// 4000-frame latent device (the deep tier) and the fader of that track
/// (its strip in the shallow tier when buses render ahead) read where
/// their audio was played — the play history, which the anticipators
/// keep as the audio thread does — and sound bit for bit as on the audio
/// thread. Without it they read position 0 for ever (a mute that never
/// comes, a fader shut at its first value).
#[test]
fn automation_behind_latency_renders_ahead_as_it_plays() {
    use faderframe_automation::{
        AutomationCurve, AutomationLane, AutomationMode, AutomationPoint, AutomationTarget,
        CurveShape,
    };
    use faderframe_core::{ChannelLayout, ParameterId};
    use faderframe_project::{OutputRouting, SavedParameter, TrackKind};
    use faderframe_timeline::MusicalTime;
    const Q: usize = 24_000;
    let mut tp = common::TestProject::new(SR);
    let track = tp.track(TrackKind::Audio, "A", ChannelLayout::Mono);
    let bus = tp.track(TrackKind::Bus, "Mix", ChannelLayout::Stereo);
    let src = tp.dc(1, 0.5, Q * 8);
    tp.clip(track, src, MusicalTime::ZERO, (Q * 8) as i64);
    let slot = |id: u64, plugin: &str, parameters: Vec<SavedParameter>| PluginSlot {
        id: PluginInstanceId(id),
        plugin: PluginRef::builtin(plugin, "Device"),
        bypass: false,
        parameters,
        state: None,
        sidechain: None,
    };
    let utility = PluginInstanceId(9_401);
    {
        let t = tp.project.track_mut(track).unwrap();
        t.output = OutputRouting::Track { track: bus };
        t.inserts.push(slot(
            9_400,
            builtin::LATENCY_PROBE,
            vec![SavedParameter {
                id: ParameterId(0),
                value: 4000.0,
            }],
        ));
        t.inserts.push(slot(utility.0, builtin::GAIN, Vec::new()));
    }
    tp.project
        .track_mut(bus)
        .unwrap()
        .inserts
        .push(slot(9_402, builtin::GAIN, Vec::new()));
    let lane = |target: AutomationTarget, points: &[(f64, f64)]| AutomationLane {
        id: faderframe_core::AutomationLaneId(0),
        target,
        curve: AutomationCurve::from_points(
            points
                .iter()
                .map(|&(q, value)| AutomationPoint {
                    time: MusicalTime::from_quarters(q),
                    value,
                    shape: CurveShape::Step,
                })
                .collect(),
        ),
        mode: AutomationMode::Read,
        visible: true,
    };
    let mute = lane(
        AutomationTarget::PluginParameter {
            plugin: utility,
            parameter: ParameterId(faderframe_plugin_host::devices::utility::id::MUTE),
        },
        &[(0.0, 0.0), (1.0, 1.0), (1.5, 0.0), (2.5, 1.0), (3.0, 0.0)],
    );
    let fader = lane(
        AutomationTarget::TrackVolume,
        &[(0.0, -144.0), (0.5, 0.0), (2.0, -144.0), (2.25, 0.0)],
    );
    for (i, mut l) in [mute, fader].into_iter().enumerate() {
        l.id = faderframe_core::AutomationLaneId(9_500 + i as u64);
        tp.project
            .track_mut(track)
            .unwrap()
            .automation
            .lanes
            .push(l);
    }
    let project = tp.project.clone();
    for buses in [false, true] {
        on_time(|| {
            let mut plain = Run::with_sources(&project, tp.sources.clone(), false, false);
            let mut ahead = Run::with_sources(&project, tp.sources.clone(), true, buses);
            assert!(ahead.r.controller.ahead_tracks().contains(&track));
            assert_eq!(ahead.r.controller.ahead_strips().contains(&track), buses);
            ahead.run(20, true);
            for run in [&mut plain, &mut ahead] {
                run.command(TransportCommand::Play);
            }
            let blocks = 2 * SR as usize / BLOCK;
            plain.run(blocks, false);
            ahead.run(blocks, true);
            let misses = ahead.r.controller.ahead_misses();
            if misses > 0 {
                return misses;
            }
            // The automation acts in the plain run: open and shut stretches.
            let loud = plain.heard.values().filter(|s| s.abs() > 0.1).count();
            let quiet = plain.heard.values().filter(|s| s.abs() < 1e-6).count();
            assert!(
                loud > SR as usize / 4 && quiet > SR as usize / 4,
                "{loud} / {quiet}"
            );
            let (both, differ) = compare(&plain, &ahead);
            assert!(both > SR as usize, "heard together: {both}");
            assert_eq!(
                differ, 0,
                "buses {buses}: {differ} of {both} samples differ"
            );
            0
        });
    }
}
