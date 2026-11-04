//! The interactive score: edits as undo steps, the preview moving between
//! states on their sync points (real time on the dummy device), and the
//! export — stems with loop points on their exact frames, the manifest,
//! FMOD's script (checked by `node --check` when Node is here), Wwise's
//! importer list and WAAPI calls.
#![allow(clippy::unwrap_used)]

use faderframe_audio::dummy::DummyBackend;
use faderframe_audio_files::AudioData;
use faderframe_audio_files::markers::WavMarkers;
use faderframe_core::{ChannelLayout, TrackId};
use faderframe_engine::EngineConfig;
use faderframe_project::interactive::{RuleEnd, SyncPoint};
use faderframe_project::{
    AudioClip, AudioSource, Clip, ClipContent, ClipFades, Project, SourceSpec, StretchSettings,
    Track, TrackColor, TrackKind,
};
use faderframe_session::adaptive::AdaptiveOp;
use faderframe_session::{Action, AudioPreferences, Session};
use faderframe_timeline::MusicalTime;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

const SR: u32 = 48_000;

fn q(n: f64) -> MusicalTime {
    MusicalTime::from_quarters(n)
}

/// An audio track playing `audio` from the start.
fn audio_track(p: &mut Project, name: &str, audio: Vec<f32>, dir: &Path) -> TrackId {
    let path = dir.join(format!("{name}.wav"));
    faderframe_audio_files::write_wav(
        &path,
        &[audio.clone(), audio.clone()],
        SR,
        faderframe_audio_files::WavFormat::Float32,
        false,
    )
    .unwrap();
    let source = p.ids.allocate();
    p.sources.insert(
        source,
        AudioSource {
            id: source,
            name: name.into(),
            spec: SourceSpec::File {
                path,
                channels: 2,
                frames: audio.len() as i64,
                sample_rate: SR,
            },
        },
    );
    let track: TrackId = p.ids.allocate();
    let clip = p.ids.allocate();
    let mut t = Track::new(track, TrackKind::Audio, name, TrackColor::palette(0))
        .with_layout(ChannelLayout::Stereo);
    t.clips.push(clip);
    let at = p.tracks.len() - 1;
    p.tracks.insert(at, t);
    p.clips.insert(
        clip,
        Clip {
            id: clip,
            track,
            name: name.into(),
            color: None,
            start: MusicalTime::ZERO,
            muted: false,
            content: ClipContent::Audio(AudioClip {
                source,
                source_offset: 0,
                length: audio.len() as i64,
                gain_db: 0.0,
                fades: ClipFades::default(),
                stretch: StretchSettings::Off,
                reversed: false,
                warp: None,
                pitch: None,
                effects: None,
                spectral: None,
                envelope: None,
            }),
        },
    );
    let _ = AudioData::from_channels(SR, vec![audio]);
    track
}

fn temp(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("ff-adaptive-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// 240 BPM (a bar a second): "Explore" bars 0–2, "Combat" bars 4–6; a
/// drums track with clicks on Combat's loop start (bar 5: a bar of
/// pickup first) and a pad track.
fn project(dir: &Path) -> (Project, TrackId, TrackId) {
    let mut p = Project::new("Game", SR);
    p.timeline.tempo.set_initial_bpm(240.0);
    let mut drums = vec![0.0f32; 8 * SR as usize];
    for bar in 0..8 {
        drums[bar * SR as usize] = 0.5;
    }
    let pad = vec![0.1f32; 8 * SR as usize];
    let d = audio_track(&mut p, "Drums", drums, dir);
    let pad = audio_track(&mut p, "Pad", pad, dir);
    (p, d, pad)
}

fn session(p: Project) -> Session {
    let mut s = Session::new(p, None, EngineConfig::default()).unwrap();
    for (a, b) in [(0.0, 8.0), (16.0, 24.0)] {
        s.dispatch(Action::AddSection {
            start: q(a),
            end: q(b),
        })
        .unwrap();
    }
    let names = ["Explore", "Combat"];
    let ids: Vec<_> = s.project().sections.iter().map(|x| x.id).collect();
    for (id, name) in ids.into_iter().zip(names) {
        let mut sec = s
            .project()
            .sections
            .iter()
            .find(|x| x.id == id)
            .unwrap()
            .clone();
        sec.name = name.into();
        s.dispatch(Action::Edit(faderframe_project::Command::UpdateSection {
            section: sec,
        }))
        .unwrap();
    }
    s
}

#[test]
fn the_score_is_edited_in_undo_steps_with_ids_the_session_gives() {
    let dir = temp("edits");
    let (p, drums, pad) = project(&dir);
    let mut s = session(p);
    let steps = s.history().undo_labels().len();
    s.dispatch(Action::Adaptive(AdaptiveOp::StatesFromSections))
        .unwrap();
    let score = s.interactive().clone();
    assert_eq!(score.states.len(), 2);
    assert_eq!(score.states[1].name, "Combat");
    assert_eq!(s.history().undo_labels().len(), steps + 1);
    // Again: nothing new.
    s.dispatch(Action::Adaptive(AdaptiveOp::StatesFromSections))
        .unwrap();
    assert_eq!(s.history().undo_labels().len(), steps + 1);
    s.dispatch(Action::Adaptive(AdaptiveOp::AddLayer {
        name: Some("Drums".into()),
        tracks: vec![drums],
    }))
    .unwrap();
    s.dispatch(Action::Adaptive(AdaptiveOp::AddLayer {
        name: Some("Pad".into()),
        tracks: vec![pad],
    }))
    .unwrap();
    s.dispatch(Action::Adaptive(AdaptiveOp::AddParameter { name: None }))
        .unwrap();
    let p = &s.interactive().parameters[0];
    assert_eq!(p.name, "Intensity");
    assert_eq!(p.curves.len(), 1, "the second layer comes in with it");
    let combat = s.interactive().states[1].id;
    s.dispatch(Action::Adaptive(AdaptiveOp::AddRule {
        from: RuleEnd::Any,
        to: RuleEnd::State { state: combat },
    }))
    .unwrap();
    let mut rule = s.interactive().rules[0].clone();
    rule.sync = SyncPoint::Beat;
    s.dispatch(Action::Adaptive(AdaptiveOp::SetRule(rule.clone())))
        .unwrap();
    assert_eq!(s.interactive().rules[0].sync, SyncPoint::Beat);
    assert_eq!(
        *s.history().undo_labels().last().unwrap(),
        "Edit Transition Rule"
    );
    // Removing a state takes its rules along; undo brings both back.
    s.dispatch(Action::Adaptive(AdaptiveOp::RemoveState(combat)))
        .unwrap();
    assert!(s.interactive().rules.is_empty());
    s.dispatch(Action::Undo).unwrap();
    assert_eq!(s.interactive().rules, vec![rule]);
    assert_eq!(s.interactive().states.len(), 2);
    // Ids are the project's own, never reused.
    let mut ids: Vec<u64> = s
        .interactive()
        .states
        .iter()
        .map(|x| x.id.raw())
        .chain(s.interactive().layers.iter().map(|l| l.id.raw()))
        .collect();
    ids.sort_unstable();
    ids.dedup();
    assert_eq!(ids.len(), 4);
    let _ = std::fs::remove_dir_all(&dir);
}

fn wait(s: &mut Session, mut done: impl FnMut(&Session) -> bool, what: &str) {
    let deadline = Instant::now() + Duration::from_secs(20);
    while Instant::now() < deadline {
        s.tick(0.005);
        if done(s) {
            return;
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    panic!(
        "timed out waiting for {what}: {:?}, processing at {}",
        s.adaptive_status(),
        s.processing_position()
    );
}

#[test]
fn the_preview_moves_to_another_state_on_its_rules_sync_point() {
    let dir = temp("preview");
    let (p, _, _) = project(&dir);
    let mut s = session(p);
    s.dispatch(Action::Adaptive(AdaptiveOp::StatesFromSections))
        .unwrap();
    let (explore, combat) = (s.interactive().states[0].id, s.interactive().states[1].id);
    // Into Combat on the next bar, after its pickup bar (entry cue at
    // one bar in).
    let mut state = s.interactive().states[1].clone();
    state.entry = q(4.0);
    s.dispatch(Action::Adaptive(AdaptiveOp::SetState(state)))
        .unwrap();
    s.start_audio(
        vec![Box::new(DummyBackend::default())],
        &AudioPreferences {
            sample_rate: Some(SR),
            buffer_size: Some(128),
            ..Default::default()
        },
    )
    .unwrap();
    s.dispatch(Action::Adaptive(AdaptiveOp::Go(explore)))
        .unwrap();
    wait(
        &mut s,
        |s| s.transport().playing && s.processing_position() > 2_000,
        "the preview to play",
    );
    assert_eq!(s.adaptive_status().current, Some(explore));
    s.dispatch(Action::Adaptive(AdaptiveOp::Go(combat)))
        .unwrap();
    let status = s.adaptive_status();
    let at = status.queued_at.unwrap();
    assert_eq!(status.queued.as_ref().unwrap().0, combat);
    // A bar line (a second at 240 BPM), inside Explore's loop (0–2 s; its
    // end is the loop's start again).
    assert_eq!(at % i64::from(SR), 0, "{at}");
    assert!((1..=2).contains(&(at / i64::from(SR))), "{at}");
    wait(
        &mut s,
        |s| s.adaptive_status().current == Some(combat),
        "the move to Combat",
    );
    // Playing Combat from its start (bar 4), looping its loop (bars 5–6).
    let pos = s.processing_position();
    assert!(
        (4 * i64::from(SR)..6 * i64::from(SR)).contains(&pos),
        "{pos}"
    );
    // Round its loop: past its end, back at its start.
    let mut near_end = false;
    wait(
        &mut s,
        |s| {
            let p = s.processing_position();
            near_end |= p > 6 * i64::from(SR) - 8_000;
            near_end && p < 5 * i64::from(SR) + 20_000
        },
        "Combat to loop round",
    );
    assert_eq!(s.adaptive_status().current, Some(combat));
    s.dispatch(Action::Adaptive(AdaptiveOp::Preview(false)))
        .unwrap();
    assert!(!s.adaptive_previewing());
    s.stop_audio();
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn an_export_writes_stems_with_loop_points_and_the_metadata() {
    let dir = temp("export");
    let (p, drums, pad) = project(&dir);
    let mut s = session(p);
    s.dispatch(Action::Adaptive(AdaptiveOp::StatesFromSections))
        .unwrap();
    let combat = s.interactive().states[1].clone();
    let mut state = combat.clone();
    state.entry = q(4.0);
    s.dispatch(Action::Adaptive(AdaptiveOp::SetState(state)))
        .unwrap();
    for (name, t) in [("Drums", drums), ("Pad", pad)] {
        s.dispatch(Action::Adaptive(AdaptiveOp::AddLayer {
            name: Some(name.into()),
            tracks: vec![t],
        }))
        .unwrap();
    }
    s.dispatch(Action::Adaptive(AdaptiveOp::AddParameter { name: None }))
        .unwrap();
    s.dispatch(Action::Adaptive(AdaptiveOp::AddRule {
        from: RuleEnd::Any,
        to: RuleEnd::State { state: combat.id },
    }))
    .unwrap();
    let mut export = s.interactive().export.clone();
    export.tail_seconds = 0.5;
    s.dispatch(Action::Adaptive(AdaptiveOp::SetExport(export)))
        .unwrap();
    let out = dir.join("out");
    s.dispatch(Action::Adaptive(AdaptiveOp::Export(out.clone())))
        .unwrap();
    assert!(s.wait_for_score_export(Duration::from_secs(120)));
    let report = s.adaptive_status().last_export.unwrap().unwrap();
    // Two states × two layers, and five metadata files.
    assert_eq!(report.files.len(), 4 + 5, "{:?}", report.files);
    // Combat's drums: two bars (2 s) and the tail; the loop from its
    // entry (one bar in) to its end, a click right on the loop's start.
    let file = out.join("Audio/Combat - Drums.wav");
    let wav = faderframe_audio_files::read_wav(&file).unwrap();
    assert_eq!(wav.channels[0].len(), 2 * SR as usize + SR as usize / 2);
    let markers = WavMarkers::read(&file).unwrap();
    assert_eq!(markers.loops.len(), 1);
    assert_eq!(markers.loops[0].start, SR);
    assert_eq!(markers.loops[0].end, 2 * SR);
    assert!(
        markers
            .cues
            .iter()
            .any(|c| c.label == "Entry" && c.frame == SR)
    );
    let peak = wav.channels[0]
        .iter()
        .enumerate()
        .skip(SR as usize / 2)
        .fold(
            (0, 0.0f32),
            |m, (i, v)| if v.abs() > m.1 { (i, v.abs()) } else { m },
        );
    assert_eq!(peak.0, SR as usize, "the click on the loop's first frame");
    // The pad's stem holds no clicks: a layer is its tracks alone.
    let pad_wav = faderframe_audio_files::read_wav(&out.join("Audio/Combat - Pad.wav")).unwrap();
    assert!(pad_wav.channels[0][SR as usize].abs() < 0.2);
    // The manifest.
    let manifest: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&report.manifest).unwrap()).unwrap();
    assert_eq!(manifest["schema"], "faderframe.interactive-score/1");
    assert_eq!(manifest["sample_rate"], SR);
    let states = manifest["states"].as_array().unwrap();
    assert_eq!(states.len(), 2);
    let c = &states[1];
    assert_eq!(c["name"], "Combat");
    assert_eq!(c["loop"]["start"], SR);
    assert_eq!(c["loop"]["end"], 2 * SR);
    assert_eq!(c["tempo"]["bpm"], 240.0);
    assert_eq!(c["beats"].as_array().unwrap().len(), 8);
    assert_eq!(c["beats"][4]["frame"], SR);
    assert_eq!(c["beats"][4]["bar"], 2);
    assert_eq!(c["stems"].as_array().unwrap().len(), 2);
    assert_eq!(manifest["layers"][0]["tracks"][0], "Drums");
    assert_eq!(manifest["parameters"][0]["name"], "Intensity");
    assert_eq!(manifest["transitions"].as_array().unwrap().len(), 2);
    assert_eq!(manifest["transitions"][0]["to"], "Combat");
    assert_eq!(manifest["transitions"][0]["sync"]["type"], "bar");
    // FMOD's script: valid JavaScript (when Node is here to say so),
    // calling only the documented functions it names.
    let js = std::fs::read_to_string(out.join("Game.fmod.js")).unwrap();
    for call in [
        "studio.project.workspace.addEvent(",
        ".addGameParameter(",
        ".addGroupTrack(",
        ".addMarkerTrack()",
        ".addSound(event.timeline, \"SingleSound\"",
        ".addNamedMarker(",
        ".addRegion(",
        ".addTransitionRegion(",
        ".addTransitionMarker(",
        ".addParameterCondition(",
        ".addAutomator(\"volume\")",
        ".addAutomationCurve(",
        ".addAutomationPoint(",
        "studio.project.importAudioFile(",
        "studio.menu.addMenuItem(",
    ] {
        assert!(js.contains(call), "{call}");
    }
    if let Ok(o) = std::process::Command::new("node")
        .arg("--check")
        .arg(out.join("Game.fmod.js"))
        .output()
    {
        assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    }
    // Wwise: the importer list and the WAAPI calls.
    let tsv = std::fs::read_to_string(out.join("Game.wwise.tsv")).unwrap();
    let mut lines = tsv.lines();
    assert_eq!(
        lines.next().unwrap(),
        "Audio File\tObject Path\tObject Type\tNotes"
    );
    let row: Vec<&str> = lines.next().unwrap().split('\t').collect();
    assert_eq!(row.len(), 4);
    assert!(row[1].ends_with(
        "<Music Switch Container>Game\\<Music Playlist Container>Explore\\<Music Segment>Explore\\<Music Track>Drums"
    ));
    let waapi: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(out.join("Game.waapi.json")).unwrap())
            .unwrap();
    let calls = waapi["calls"].as_array().unwrap();
    assert!(calls.iter().all(|c| c["uri"] == "ak.wwise.core.object.set"));
    let music = &calls[1]["args"]["objects"][0]["children"][0];
    assert_eq!(music["type"], "MusicSwitchContainer");
    assert_eq!(music["@Entries"].as_array().unwrap().len(), 2);
    let segment = &music["children"][1]["children"][0];
    assert_eq!(segment["type"], "MusicSegment");
    assert_eq!(segment["@Cues"][0]["@TimeMs"], 1000.0);
    assert_eq!(segment["@Cues"][1]["@TimeMs"], 2000.0);
    let transitions = &calls.last().unwrap()["args"]["objects"][0]["@TransitionRoot"]["children"];
    assert_eq!(
        transitions.as_array().unwrap().len(),
        2,
        "the default and the rule"
    );
    assert_eq!(transitions[1]["@ExitSourceAt"], 2, "next bar");
    if let Ok(o) = std::process::Command::new("python3")
        .args(["-m", "py_compile"])
        .arg(out.join("Game.waapi.py"))
        .output()
    {
        assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    }
    let _ = std::fs::remove_dir_all(&dir);
}
