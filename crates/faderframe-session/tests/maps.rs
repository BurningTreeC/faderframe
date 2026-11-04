//! Articulation and drum maps in the session: made from templates, files
//! (FaderFrame's and Cubase's) and Drum Sampler pads, used by tracks,
//! edited and removed in one step each; notes take attributes, directions
//! go into the clip's lane; and a running session plays keyswitches
//! before their notes, chased on a locate.
#![allow(clippy::unwrap_used)]

use faderframe_audio::dummy::DummyBackend;
use faderframe_core::{ClipId, TrackId};
use faderframe_engine::EngineConfig;
use faderframe_project::articulation::ArticulationKind;
use faderframe_project::{MidiNote, MidiOutputRouting, PluginRef, Project, TrackKind};
use faderframe_session::maps::{self, DrumMapSource, MapOp};
use faderframe_session::{Action, AudioPreferences, Session, TransportAction};
use faderframe_timeline::MusicalTime;
use std::sync::Mutex;
use std::time::{Duration, Instant};

const SR: u32 = 48_000;

/// The user's maps folder is process-wide: tests that use it take turns.
static TURN: Mutex<()> = Mutex::new(());

fn q(n: f64) -> MusicalTime {
    MusicalTime::from_quarters(n)
}

fn scratch(name: &str) -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!("ff-maps-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// A MIDI track with a clip of three notes (beats 1, 2, 3).
fn with_notes(kind: TrackKind) -> (Session, TrackId, ClipId) {
    let mut s = Session::new(Project::new("Maps", SR), None, EngineConfig::default()).unwrap();
    let t = s.add_track(kind).unwrap();
    s.dispatch(Action::CreateMidiClip {
        track: t,
        start: MusicalTime::ZERO,
        length: q(8.0),
    })
    .unwrap();
    let clip = s.project().clips_of(t)[0].id;
    let notes = [(1.0, 60), (2.0, 62), (3.0, 64)]
        .into_iter()
        .map(|(at, key)| MidiNote {
            time_locked: false,
            id: faderframe_core::NoteId(0),
            start: q(at),
            length: q(0.25),
            key,
            velocity: 100,
            channel: 0,
            muted: false,
            release: None,
            articulation: None,
            fine: None,
        })
        .collect();
    s.dispatch(Action::AddNotes { clip, notes }).unwrap();
    (s, t, clip)
}

fn notes_of(s: &Session, clip: ClipId) -> Vec<MidiNote> {
    s.project()
        .clip(clip)
        .unwrap()
        .as_midi()
        .unwrap()
        .notes
        .clone()
}

#[test]
fn notes_take_attributes_and_directions_go_into_the_lane() {
    let (mut s, t, clip) = with_notes(TrackKind::Midi);
    s.use_articulation_map_named(t, "strings").unwrap();
    assert_eq!(s.history().undo_label(), Some("New Articulation Map"));
    let map = s.project().track_articulation_map(t).unwrap().clone();
    assert_eq!(map.name, "Strings");
    let stacc = map.find("Staccato").unwrap();
    let pizz = map.find("Pizzicato").unwrap();
    assert_eq!(stacc.kind, ArticulationKind::Attribute);
    let notes = notes_of(&s, clip);
    s.dispatch(Action::Maps(MapOp::SetNoteArticulation {
        clip,
        notes: vec![notes[1].id],
        articulation: Some(stacc.id),
    }))
    .unwrap();
    assert_eq!(s.history().undo_label(), Some("Set Articulation"));
    assert_eq!(notes_of(&s, clip)[1].articulation, Some(stacc.id));
    // A direction for the third note: into the lane where it starts.
    s.dispatch(Action::Maps(MapOp::SetNoteArticulation {
        clip,
        notes: vec![notes[2].id],
        articulation: Some(pizz.id),
    }))
    .unwrap();
    let m = s.project().clip(clip).unwrap().as_midi().unwrap().clone();
    assert_eq!(m.notes[2].articulation, None);
    assert_eq!(m.articulations.len(), 1);
    assert_eq!(
        (m.articulations[0].time, m.articulations[0].articulation),
        (q(3.0), pizz.id)
    );
    // Another of its group there replaces it; one of another group joins.
    let trem = map.find("Tremolo").unwrap().id;
    let legato = map.find("Legato").unwrap().id;
    for a in [trem, legato] {
        s.dispatch(Action::Maps(MapOp::SetDirection {
            clip,
            at: q(3.0),
            articulation: Some(a),
        }))
        .unwrap();
    }
    let lane = &s
        .project()
        .clip(clip)
        .unwrap()
        .as_midi()
        .unwrap()
        .articulations;
    let at_three: Vec<_> = lane.iter().map(|e| e.articulation).collect();
    assert_eq!(at_three, [trem, legato]);
    // The menu for the second note: none, then the map's, staccato ticked.
    let choices = s.articulation_choices(clip, &[notes[1].id]);
    assert_eq!(choices.len(), map.articulations.len() + 1);
    assert!(choices.iter().any(|c| c.checked && c.label == "Staccato"));
    // Removed from the lane; undone, back.
    s.dispatch(Action::Maps(MapOp::SetDirection {
        clip,
        at: q(3.0),
        articulation: None,
    }))
    .unwrap();
    assert!(
        s.project()
            .clip(clip)
            .unwrap()
            .as_midi()
            .unwrap()
            .articulations
            .is_empty()
    );
    s.dispatch(Action::Undo).unwrap();
    assert_eq!(
        s.project()
            .clip(clip)
            .unwrap()
            .as_midi()
            .unwrap()
            .articulations
            .len(),
        2
    );
    // Copies of a note keep its articulation (it is the note's).
    s.dispatch(Action::DuplicateNotes {
        clip,
        notes: vec![notes[1].id],
        offset: Some(q(4.0)),
        keys: 0,
    })
    .unwrap();
    let marked = notes_of(&s, clip)
        .iter()
        .filter(|n| n.articulation == Some(stacc.id))
        .count();
    assert_eq!(marked, 2);
}

#[test]
fn maps_are_removed_saved_and_read_back_in_one_step_each() {
    let _turn = TURN.lock().unwrap_or_else(|e| e.into_inner());
    let dir = scratch("files");
    maps::set_user_dir(Some(dir.clone()));
    let (mut s, t, _) = with_notes(TrackKind::Instrument);
    s.dispatch(Action::Maps(MapOp::NewArticulationMap {
        track: Some(t),
        template: Some("Brass".into()),
    }))
    .unwrap();
    let id = s.project().track(t).unwrap().articulation_map.unwrap();
    // Edited: renamed, one more articulation.
    let mut map = s.project().articulation_map(id).unwrap().clone();
    map.name = "My Brass".into();
    map.add(
        "Flutter",
        "flz.",
        0,
        ArticulationKind::Attribute,
        faderframe_project::articulation::ArticulationOutput::keyswitch(30),
    );
    s.dispatch(Action::Maps(MapOp::EditArticulationMap(Box::new(map))))
        .unwrap();
    assert_eq!(
        s.project()
            .articulation_map(id)
            .unwrap()
            .articulations
            .len(),
        6
    );
    // Saved as a user file, listed, offered to tracks.
    s.dispatch(Action::Maps(MapOp::SaveArticulationMap {
        map: id,
        path: None,
    }))
    .unwrap();
    assert!(dir.join("articulation-maps/My Brass.json").is_file());
    assert_eq!(maps::user_articulation_maps()[0].0, "My Brass");
    assert!(
        s.articulation_map_choices(t)
            .iter()
            .any(|c| c.label == "From File: My Brass")
    );
    // Removed: the track names none, in one step; undone, both back.
    s.dispatch(Action::Maps(MapOp::RemoveArticulationMap(id)))
        .unwrap();
    assert!(s.project().articulation_maps.is_empty());
    assert_eq!(s.project().track(t).unwrap().articulation_map, None);
    s.dispatch(Action::Undo).unwrap();
    assert_eq!(s.project().track(t).unwrap().articulation_map, Some(id));
    // Read back from the file by name: a new map with the same content.
    s.use_articulation_map_named(t, "My Brass").unwrap();
    assert_eq!(
        s.project().articulation_maps.len(),
        1,
        "the project's own by name"
    );
    let path = dir.join("articulation-maps/My Brass.json");
    s.dispatch(Action::Maps(MapOp::ImportArticulationMap {
        track: Some(t),
        path,
    }))
    .unwrap();
    assert_eq!(s.project().articulation_maps.len(), 2);
    let read = s.project().track_articulation_map(t).unwrap();
    assert_ne!(read.id, id);
    assert_eq!(
        read.articulations,
        s.project().articulation_map(id).unwrap().articulations
    );
    // A Cubase expression map.
    let cubase = dir.join("Legato.expressionmap");
    std::fs::write(
        &cubase,
        r#"<?xml version="1.0" encoding="utf-8"?>
<InstrumentMap>
   <string name="name" value="Legato Strings"/>
   <member name="slotvisuals"><list name="obj" type="obj">
      <obj class="USlotVisuals" ID="7">
         <int name="articulationtype" value="1"/>
         <string name="text" value="leg."/>
         <string name="description" value="Legato"/>
         <int name="group" value="0"/>
      </obj>
   </list></member>
   <member name="slots"><list name="obj" type="obj">
      <obj class="PSoundSlot" ID="8">
         <obj class="PSlotMidiAction" name="action" ID="9">
            <member name="midiMessages"><list name="obj" type="obj">
               <obj class="POutputEvent" ID="10">
                  <int name="status" value="176"/>
                  <int name="data1" value="68"/>
                  <int name="data2" value="127"/>
               </obj>
            </list></member>
         </obj>
         <member name="sv"><list name="s" type="obj"><obj class="USlotVisuals" ID="7"/></list></member>
         <member name="name"><string name="s" value="Legato"/></member>
      </obj>
   </list></member>
</InstrumentMap>"#,
    )
    .unwrap();
    s.dispatch(Action::Maps(MapOp::ImportArticulationMap {
        track: Some(t),
        path: cubase,
    }))
    .unwrap();
    let legato = s.project().track_articulation_map(t).unwrap();
    assert_eq!(legato.name, "Legato Strings");
    assert_eq!(legato.articulations[0].output.controls[0].controller, 68);
    // Saved with the project.
    let text = serde_json::to_string(s.project()).unwrap();
    let back: Project = serde_json::from_str(&text).unwrap();
    assert_eq!(back.articulation_maps, s.project().articulation_maps);
    assert_eq!(
        back.track(t).unwrap().articulation_map,
        s.project().track(t).unwrap().articulation_map
    );
    maps::set_user_dir(None);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn drum_maps_come_from_general_midi_pads_and_files() {
    let _turn = TURN.lock().unwrap_or_else(|e| e.into_inner());
    let dir = scratch("drums");
    maps::set_user_dir(Some(dir.clone()));
    let (mut s, t, _) = with_notes(TrackKind::Instrument);
    // No Drum Sampler yet: no pads offered.
    assert!(
        !s.drum_map_choices(t)
            .iter()
            .any(|c| c.label.contains("Pads"))
    );
    s.use_drum_map_named(t, "gm").unwrap();
    let gm = s.project().track_drum_map(t).unwrap().clone();
    assert_eq!(gm.sounds.len(), 47);
    assert_eq!(gm.name_of(38), Some("Acoustic Snare"));
    s.dispatch(Action::InsertPlugin {
        track: t,
        index: 0,
        plugin: PluginRef::builtin(faderframe_core::builtin::DRUMS, "Drum Sampler"),
    })
    .unwrap();
    assert!(
        s.drum_map_choices(t)
            .iter()
            .any(|c| c.label == "New: Drum Sampler Pads")
    );
    s.dispatch(Action::Maps(MapOp::NewDrumMap {
        track: Some(t),
        from: DrumMapSource::Pads,
    }))
    .unwrap();
    let pads = s.project().track_drum_map(t).unwrap().clone();
    assert_eq!(pads.sounds.len(), 16);
    assert_eq!(
        (pads.sounds[0].pitch, pads.sounds[0].name.as_str()),
        (36, "Pad 1")
    );
    // Edited (the kick plays another note), saved, read back by name.
    let mut edited = pads.clone();
    edited.name = "Kit".into();
    edited.sound_mut(36).unwrap().output_note = Some(35);
    s.dispatch(Action::Maps(MapOp::EditDrumMap(Box::new(edited))))
        .unwrap();
    s.dispatch(Action::Maps(MapOp::SaveDrumMap {
        map: pads.id,
        path: None,
    }))
    .unwrap();
    assert_eq!(maps::user_drum_maps()[0].0, "Kit");
    s.dispatch(Action::Maps(MapOp::UseDrumMap {
        track: t,
        map: None,
    }))
    .unwrap();
    s.use_drum_map_named(t, "Kit").unwrap();
    assert_eq!(
        s.project().track_drum_map(t).unwrap().id,
        pads.id,
        "the project's"
    );
    s.dispatch(Action::Maps(MapOp::RemoveDrumMap(gm.id)))
        .unwrap();
    assert_eq!(s.project().drum_maps.len(), 1);
    let file = dir.join("drum-maps/Kit.json");
    s.dispatch(Action::Maps(MapOp::ImportDrumMap {
        track: Some(t),
        path: file,
    }))
    .unwrap();
    let read = s.project().track_drum_map(t).unwrap();
    assert_eq!(read.sound(36).unwrap().output_note, Some(35));
    assert_ne!(read.id, pads.id);
    maps::set_user_dir(None);
    let _ = std::fs::remove_dir_all(&dir);
}

fn audio(s: &mut Session) {
    s.start_audio(
        vec![Box::new(DummyBackend::default())],
        &AudioPreferences {
            sample_rate: Some(SR),
            buffer_size: Some(64),
            threads: Some(1),
            ..Default::default()
        },
    )
    .unwrap();
}

fn note_ons(c: &faderframe_midi_io::Captured) -> Vec<Vec<u8>> {
    c.lock()
        .unwrap()
        .iter()
        .filter(|(_, b)| b.first().is_some_and(|s| s & 0xF0 == 0x90))
        .map(|(_, b)| b.clone())
        .collect()
}

#[test]
fn a_running_session_plays_keyswitches_before_their_notes_and_chases_them() {
    let (mut s, t, clip) = with_notes(TrackKind::Midi);
    let captured = s.add_virtual_midi_output("Strings");
    s.dispatch(Action::Edit(
        faderframe_project::Command::SetTrackMidiOutput {
            track: t,
            output: Some(MidiOutputRouting {
                port: "virtual:Strings".into(),
                channel: None,
            }),
        },
    ))
    .unwrap();
    s.use_articulation_map_named(t, "strings").unwrap();
    let map = s.project().track_articulation_map(t).unwrap().clone();
    let notes = notes_of(&s, clip);
    s.dispatch(Action::Maps(MapOp::SetNoteArticulation {
        clip,
        notes: vec![notes[1].id],
        articulation: Some(map.find("Staccato").unwrap().id),
    }))
    .unwrap();
    audio(&mut s);
    s.dispatch(Action::Transport(TransportAction::Play))
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    while note_ons(&captured).iter().filter(|b| b[1] >= 60).count() < 3 && Instant::now() < deadline
    {
        s.tick(0.005);
        std::thread::sleep(Duration::from_millis(5));
    }
    let ons = note_ons(&captured);
    let at = |b: &[u8]| ons.iter().position(|x| x.as_slice() == b).unwrap();
    // Sustain before the first note, staccato before the second, sustain
    // again before the third.
    assert!(at(&[0x90, 24, 100]) < at(&[0x90, 60, 100]), "{ons:02X?}");
    assert!(at(&[0x90, 60, 100]) < at(&[0x90, 25, 100]));
    assert!(at(&[0x90, 25, 100]) < at(&[0x90, 62, 100]));
    let last_sustain = ons
        .iter()
        .rposition(|x| x.as_slice() == [0x90, 24, 100])
        .unwrap();
    assert!(
        at(&[0x90, 62, 100]) < last_sustain && last_sustain < at(&[0x90, 64, 100]),
        "{ons:02X?}"
    );
    // A locate to beat 2.5 (after the staccato note): staccato is chased
    // first.
    let seen = ons.len();
    s.dispatch(Action::Transport(TransportAction::Locate(q(2.5))))
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    while note_ons(&captured).len() <= seen && Instant::now() < deadline {
        s.tick(0.005);
        std::thread::sleep(Duration::from_millis(5));
    }
    let after = note_ons(&captured)[seen..].to_vec();
    assert_eq!(
        after.first().map(|b| b.as_slice()),
        Some(&[0x90, 25, 100][..]),
        "{after:02X?}"
    );
    s.stop_audio();
}
