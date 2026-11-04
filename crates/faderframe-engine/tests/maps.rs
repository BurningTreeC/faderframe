#![allow(clippy::unwrap_used)]
//! Articulation maps and drum maps on the way out: keyswitches go out a
//! sample before their notes and are chased on locate; drum sounds play
//! on their own note and channel (clips and live input) and sounds with a
//! port of their own go there.

mod common;

use common::TestProject;
use faderframe_core::{ArticulationMapId, ChannelLayout, DrumMapId, TrackId};
use faderframe_engine::EngineConfig;
use faderframe_engine::offline::OfflineRenderer;
use faderframe_midi::{MidiOutputEvent, midi_input_queue, midi_output_queue};
use faderframe_project::articulation::{ArticulationEvent, ArticulationMap};
use faderframe_project::drum_map::DrumMap;
use faderframe_project::{
    Clip, ClipContent, Impact, MidiClip, MidiNote, MidiOutputRouting, TrackColor, TrackKind,
};
use faderframe_timeline::MusicalTime;
use faderframe_transport::TransportCommand;
use std::collections::{HashMap, HashSet};

const SR: u32 = 48_000;
/// One callback holds four seconds (eight beats): due times within it
/// differ by the frames between the events.
const BLOCK: usize = 4 * SR as usize;
const NS_PER_FRAME: f64 = 1e9 / SR as f64;

fn note(tp: &mut TestProject, quarter: f64, key: u8) -> MidiNote {
    MidiNote {
        time_locked: false,
        id: tp.project.ids.allocate(),
        start: MusicalTime::from_quarters(quarter),
        length: MusicalTime::from_quarters(0.25),
        key,
        velocity: 100,
        channel: 0,
        muted: false,
        release: None,
        articulation: None,
        fine: None,
    }
}

/// A MIDI track sending to port 2, with one clip.
fn midi_track(tp: &mut TestProject, clip: MidiClip) -> TrackId {
    let t = tp.track(TrackKind::Midi, "Ext", ChannelLayout::Stereo);
    tp.project.track_mut(t).unwrap().midi_output = Some(MidiOutputRouting {
        port: "Synth".into(),
        channel: None,
    });
    let id = tp.project.ids.allocate();
    tp.project.clips.insert(
        id,
        Clip {
            id,
            track: t,
            name: "Line".into(),
            color: Some(TrackColor::palette(1)),
            start: MusicalTime::ZERO,
            muted: false,
            content: ClipContent::Midi(clip),
        },
    );
    tp.project.track_mut(t).unwrap().clips.push(id);
    t
}

fn renderer(tp: &TestProject) -> (OfflineRenderer, rtrb::Consumer<MidiOutputEvent>) {
    let config = EngineConfig {
        sample_rate: SR,
        ..EngineConfig::default()
    };
    let mut r = OfflineRenderer::new(&tp.project, &tp.sources, config, BLOCK, 2).unwrap();
    r.controller.set_midi_output_ports(HashMap::from([
        ("Synth".to_string(), 2),
        ("Other".to_string(), 3),
    ]));
    r.controller
        .sync(&tp.project, &tp.sources, Impact::Graph)
        .unwrap();
    let (_tx, q, _feed) = midi_input_queue(64);
    r.controller.set_midi_input(q).unwrap();
    let (oq, orx) = midi_output_queue(4096, faderframe_midi::MidiClock::new());
    r.controller.set_midi_output(oq).unwrap();
    (r, orx)
}

fn drain(rx: &mut rtrb::Consumer<MidiOutputEvent>) -> Vec<MidiOutputEvent> {
    std::iter::from_fn(|| rx.pop().ok())
        .filter(|e| e.bytes().first().is_some_and(|b| *b < 0xF0))
        .collect()
}

fn frames_apart(a: &MidiOutputEvent, b: &MidiOutputEvent) -> i64 {
    ((b.due_ns as f64 - a.due_ns as f64) / NS_PER_FRAME).round() as i64
}

/// Notes at beats 1, 2 (staccato) and 3 (after a pizzicato direction at
/// beat 2.5) on the Strings template's keyswitches.
fn articulated() -> (TestProject, TrackId) {
    let mut tp = TestProject::new(SR);
    let map_id: ArticulationMapId = tp.project.ids.allocate();
    let map = ArticulationMap::template(map_id, "strings").unwrap();
    let stacc = map.find("Staccato").unwrap().id;
    let pizz = map.find("Pizzicato").unwrap().id;
    let mut notes = vec![
        note(&mut tp, 1.0, 60),
        note(&mut tp, 2.0, 62),
        note(&mut tp, 3.0, 64),
    ];
    notes[1].articulation = Some(stacc);
    let clip = MidiClip {
        length: MusicalTime::from_quarters_i(8),
        notes,
        articulations: vec![ArticulationEvent {
            time: MusicalTime::from_quarters(2.5),
            articulation: pizz,
        }],
        ..MidiClip::default()
    };
    let t = midi_track(&mut tp, clip);
    tp.project.articulation_maps.push(map);
    tp.project.track_mut(t).unwrap().articulation_map = Some(map_id);
    (tp, t)
}

#[test]
fn keyswitches_go_out_a_sample_before_their_notes() {
    let (tp, _) = articulated();
    let (mut r, mut rx) = renderer(&tp);
    r.play_from(0).unwrap();
    r.step();
    let got = drain(&mut rx);
    let bytes: Vec<&[u8]> = got.iter().map(|e| e.bytes()).collect();
    // Sustain (C0 = 24) and non-legato (CC 68 = 0) before the first note
    // (chased at the start too: the state the first note wants), staccato
    // (C#0) before the second, back to sustain before nothing (the third
    // note is pizzicato's: D0).
    let pos = |b: &[u8]| bytes.iter().position(|x| *x == b).unwrap();
    let first = pos(&[0x90, 60, 100]);
    let second = pos(&[0x90, 62, 100]);
    let third = pos(&[0x90, 64, 100]);
    let stacc = pos(&[0x90, 25, 100]);
    let pizz = pos(&[0x90, 26, 100]);
    assert!(
        bytes[..first].contains(&&[0x90u8, 24, 100][..]),
        "{bytes:02X?}"
    );
    assert!(
        bytes[..first].contains(&&[0xB0u8, 68, 0][..]),
        "{bytes:02X?}"
    );
    assert!(first < stacc && stacc < second, "{bytes:02X?}");
    assert!(second < pizz && pizz < third, "{bytes:02X?}");
    // A sample before the note, its note-off on the note's sample, sent
    // before the note-on there.
    assert_eq!(frames_apart(&got[stacc], &got[second]), 1);
    let stacc_off = bytes.iter().position(|x| *x == [0x80, 25, 0]).unwrap();
    assert!(stacc_off < second);
    assert_eq!(frames_apart(&got[stacc_off], &got[second]), 0);
    assert_eq!(frames_apart(&got[pizz], &got[third]), 1);
    // Every keyswitch let go.
    for k in [24u8, 25, 26] {
        let ons = bytes.iter().filter(|b| **b == [0x90, k, 100]).count();
        let offs = bytes.iter().filter(|b| **b == [0x80, k, 0]).count();
        assert_eq!(ons, offs, "keyswitch {k}: {bytes:02X?}");
    }
}

/// Starting mid-clip sends the state playback from the start would have
/// left the instrument in (switches go out before notes, so that is the
/// last note's), then the next note's switch before it.
#[test]
fn a_locate_chases_the_articulation_in_effect() {
    let (tp, _) = articulated();
    let (mut r, mut rx) = renderer(&tp);
    // Beat 2.75: after the staccato note, past the pizzicato direction,
    // before the third note.
    let from = SR as i64 * 11 / 8;
    r.play_from(from).unwrap();
    r.step();
    let got = drain(&mut rx);
    let bytes: Vec<&[u8]> = got.iter().map(|e| e.bytes()).collect();
    assert_eq!(bytes[0], &[0xB0, 68, 0], "{bytes:02X?}");
    assert_eq!(
        bytes[1],
        &[0x90, 25, 100],
        "staccato chased first: {bytes:02X?}"
    );
    assert_eq!(bytes[2], &[0x80, 25, 0], "and let go a sample later");
    assert_eq!(frames_apart(&got[1], &got[2]), 1);
    let pizz = bytes.iter().position(|b| *b == [0x90, 26, 100]).unwrap();
    let third = bytes.iter().position(|b| *b == [0x90, 64, 100]).unwrap();
    assert!(pizz < third);
    assert_eq!(frames_apart(&got[pizz], &got[third]), 1);
    assert_eq!(
        frames_apart(&got[1], &got[third]),
        SR as i64 * 3 / 2 - from,
        "the chase at the locate"
    );
    assert!(!bytes.contains(&&[0x90u8, 24, 100][..]), "not sustain");
    // A locate while playing chases again: to beat 1.5, after the first
    // note (sustain).
    let _ = drain(&mut rx);
    r.controller
        .transport(TransportCommand::Locate(SR as i64 * 3 / 4))
        .unwrap();
    r.step();
    let again: Vec<Vec<u8>> = drain(&mut rx).iter().map(|e| e.bytes().to_vec()).collect();
    assert_eq!(
        again.iter().position(|b| *b == [0x90, 24, 100]),
        again
            .iter()
            .position(|b| b.first().is_some_and(|s| s & 0xF0 == 0x90)),
        "sustain is the first note-on after the jump: {again:02X?}"
    );
}

#[test]
fn drum_sounds_play_on_their_note_and_channel_and_port() {
    let mut tp = TestProject::new(SR);
    let notes = vec![note(&mut tp, 0.5, 36), note(&mut tp, 1.0, 38)];
    let t = midi_track(
        &mut tp,
        MidiClip {
            length: MusicalTime::from_quarters_i(4),
            notes,
            ..MidiClip::default()
        },
    );
    let id: DrumMapId = tp.project.ids.allocate();
    let mut map = DrumMap::gm(id);
    let kick = map.sound_mut(36).unwrap();
    kick.output_note = Some(48);
    kick.channel = Some(9);
    map.sound_mut(38).unwrap().port = Some("Other".into());
    tp.project.drum_maps.push(map);
    tp.project.track_mut(t).unwrap().drum_map = Some(id);
    let (mut r, mut rx) = renderer(&tp);
    r.controller.set_midi_live(HashSet::from([t]));
    r.controller
        .sync(&tp.project, &tp.sources, Impact::Params)
        .unwrap();
    r.play_from(0).unwrap();
    r.step();
    let got = drain(&mut rx);
    let kick = got
        .iter()
        .find(|e| e.bytes()[0] & 0xF0 == 0x90 && e.bytes()[1] == 48)
        .unwrap();
    assert_eq!(kick.bytes(), &[0x99, 48, 100], "its note on channel 10");
    assert_eq!(kick.port, 2);
    let snare: Vec<_> = got.iter().filter(|e| e.bytes()[1] == 38).collect();
    assert!(!snare.is_empty());
    assert!(
        snare.iter().all(|e| e.port == 3),
        "the snare goes to its own port"
    );
    assert!(
        got.iter()
            .filter(|e| e.port == 3)
            .all(|e| e.bytes()[1] == 38),
        "nothing else does"
    );
}

#[test]
fn live_drums_are_mapped_too() {
    let mut tp = TestProject::new(SR);
    let t = midi_track(&mut tp, MidiClip::default());
    let id: DrumMapId = tp.project.ids.allocate();
    let mut map = DrumMap::gm(id);
    map.sound_mut(42).unwrap().output_note = Some(70);
    tp.project.drum_maps.push(map);
    tp.project.track_mut(t).unwrap().drum_map = Some(id);
    let config = EngineConfig {
        sample_rate: SR,
        ..EngineConfig::default()
    };
    let mut r = OfflineRenderer::new(&tp.project, &tp.sources, config, 256, 2).unwrap();
    r.controller
        .set_midi_output_ports(HashMap::from([("Synth".to_string(), 2)]));
    r.controller.set_midi_live(HashSet::from([t]));
    r.controller
        .sync(&tp.project, &tp.sources, Impact::Graph)
        .unwrap();
    let (tx, q, _feed) = midi_input_queue(64);
    let clock = tx.clock();
    r.controller.set_midi_input(q).unwrap();
    let (oq, mut orx) = midi_output_queue(256, clock);
    r.controller.set_midi_output(oq).unwrap();
    r.step();
    tx.send(0, &[0x90, 42, 90]);
    r.step();
    r.step();
    tx.send(0, &[0x80, 42, 0]);
    r.step();
    r.step();
    let got: Vec<Vec<u8>> = drain(&mut orx).iter().map(|e| e.bytes().to_vec()).collect();
    assert!(got.contains(&vec![0x90, 70, 90]), "{got:02X?}");
    assert!(got.contains(&vec![0x80, 70, 0]), "released where it played");
    assert!(!got.iter().any(|b| b.get(1) == Some(&42)), "{got:02X?}");
}
