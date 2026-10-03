#![allow(clippy::unwrap_used)]
//! MIDI clips: controller lanes play and are chased on locate, muted notes
//! stay silent.

mod common;

use common::TestProject;
use faderframe_core::{ChannelLayout, builtin};
use faderframe_engine::EngineConfig;
use faderframe_engine::offline::OfflineRenderer;
use faderframe_project::{
    Clip, ClipContent, ControllerLane, ControllerPoint, MidiClip, MidiController, MidiNote,
    PluginRef, PluginSlot, TrackColor, TrackKind,
};
use faderframe_timeline::MusicalTime;

const SR: u32 = 48_000;

fn energy(r: &mut OfflineRenderer, blocks: usize) -> f32 {
    let mut e = 0.0;
    for _ in 0..blocks {
        let out = r.step();
        e += out.output_ref(0).iter().map(|v| v * v).sum::<f32>();
    }
    e
}

fn project(muted: bool, sustain: bool) -> TestProject {
    let mut tp = TestProject::new(SR);
    let t = tp.track(TrackKind::Instrument, "Keys", ChannelLayout::Stereo);
    let slot = PluginSlot {
        id: tp.project.ids.allocate(),
        plugin: PluginRef::builtin(builtin::SYNTH, "Synth"),
        bypass: false,
        parameters: Vec::new(),
        state: None,
        sidechain: None,
    };
    tp.project.track_mut(t).unwrap().instrument = Some(slot);
    let mut clip = MidiClip {
        length: MusicalTime::from_quarters_i(8),
        notes: vec![MidiNote {
            id: tp.project.ids.allocate(),
            start: MusicalTime::ZERO,
            length: MusicalTime::from_quarters(0.25),
            key: 60,
            velocity: 110,
            channel: 0,
            muted,
        }],
        controllers: Vec::new(),
        expressions: Vec::new(),
        sysex: Vec::new(),
    };
    if sustain {
        let mut lane = ControllerLane::new(MidiController::SUSTAIN, 0);
        lane.points = vec![ControllerPoint {
            time: MusicalTime::ZERO,
            value: 127,
        }];
        clip.controllers.push(lane);
    }
    let id = tp.project.ids.allocate();
    tp.project.clips.insert(
        id,
        Clip {
            id,
            track: t,
            name: "Keys".into(),
            color: Some(TrackColor::palette(1)),
            start: MusicalTime::ZERO,
            muted: false,
            content: ClipContent::Midi(clip),
        },
    );
    tp.project.track_mut(t).unwrap().clips.push(id);
    tp
}

#[test]
fn muted_notes_are_silent() {
    let tp = project(true, false);
    let config = EngineConfig {
        sample_rate: SR,
        ..EngineConfig::default()
    };
    let mut r = OfflineRenderer::new(&tp.project, &tp.sources, config, 256, 2).unwrap();
    r.play_from(0).unwrap();
    assert!(energy(&mut r, 40) < 1e-9);
    let tp = project(false, false);
    let mut r = OfflineRenderer::new(&tp.project, &tp.sources, config, 256, 2).unwrap();
    r.play_from(0).unwrap();
    assert!(energy(&mut r, 40) > 1e-3);
}

#[test]
fn a_sustained_note_keeps_ringing_and_is_released_on_stop() {
    let config = EngineConfig {
        sample_rate: SR,
        ..EngineConfig::default()
    };
    // Without the pedal the short note dies away; with it, it rings on
    // well past its end (a quarter of a beat).
    let mut plain = OfflineRenderer::new(
        &project(false, false).project,
        &Default::default(),
        config,
        256,
        2,
    )
    .unwrap();
    let tp = project(false, true);
    let mut pedal = OfflineRenderer::new(&tp.project, &tp.sources, config, 256, 2).unwrap();
    for r in [&mut plain, &mut pedal] {
        r.play_from(0).unwrap();
        energy(r, 200); // ~1 s
    }
    let (p, s) = (energy(&mut plain, 20), energy(&mut pedal, 20));
    assert!(s > p * 10.0 + 1e-6, "sustain holds the note: {s} vs {p}");
    // Stop: the pedal goes up, the note releases.
    pedal
        .controller
        .transport(faderframe_transport::TransportCommand::Stop)
        .unwrap();
    energy(&mut pedal, 600);
    assert!(energy(&mut pedal, 20) < 1e-6, "released after stop");
}
