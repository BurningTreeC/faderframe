//! ARA inserts in the engine, against the mock ARA plug-in: an insert
//! renders its track's clips from its own model (the clip player leaves
//! them out), follows moves and removals, carries its edits into renders
//! through the archive, lines up behind latency, and gives back notes.
#![allow(clippy::unwrap_used)]

mod common;

use common::TestProject;
use faderframe_audio_files::AudioData;
use faderframe_core::{ChannelLayout, ClipId, ParameterId, PluginInstanceId, TrackId, builtin};
use faderframe_engine::EngineConfig;
use faderframe_engine::offline::OfflineRenderer;
use faderframe_plugin_host::ara_mock::{self, AraMockFactory};
use faderframe_project::{Impact, PluginFormat, PluginRef, PluginSlot, SavedParameter, TrackKind};
use faderframe_timeline::MusicalTime;

const RATE: u32 = 48_000;

fn registry() {
    faderframe_plugin_host::set_default_registry(|| {
        let mut r = faderframe_plugin_host::PluginRegistry::with_builtins();
        r.add_factory(Box::new(AraMockFactory));
        r
    });
}

fn config() -> EngineConfig {
    EngineConfig {
        sample_rate: RATE,
        max_block_size: 512,
        ..EngineConfig::default()
    }
}

/// Half a second of A440, then silence.
fn tone() -> AudioData {
    let mut x: Vec<f32> = (0..RATE as usize / 2)
        .map(|i| {
            0.5 * (2.0 * std::f64::consts::PI * 440.0 * i as f64 / f64::from(RATE)).sin() as f32
        })
        .collect();
    x.extend(std::iter::repeat_n(0.0, RATE as usize / 4));
    AudioData::from_channels(RATE, vec![x.clone(), x])
}

struct Song {
    tp: TestProject,
    track: TrackId,
    clip: ClipId,
    slot: PluginInstanceId,
    source: faderframe_core::AudioSourceId,
}

/// A stereo track with one clip of the tone at quarter 4 (two seconds at
/// 120 BPM) and the mock as an ARA insert, in a project named `name` (its
/// ARA document's name: tests find their mock instances by it).
fn song(name: &str) -> Song {
    registry();
    let mut tp = TestProject::new(RATE);
    tp.project.name = name.into();
    let track = tp.track(TrackKind::Audio, "Vocals", ChannelLayout::Stereo);
    let source = tp.source(tone());
    tp.clip(
        track,
        source,
        MusicalTime::from_quarters(4.0),
        RATE as i64 / 2,
    );
    let clip = *tp.project.track(track).unwrap().clips.last().unwrap();
    let slot = PluginSlot {
        id: tp.project.ids.allocate(),
        plugin: PluginRef {
            format: PluginFormat::Clap,
            id: ara_mock::ID.into(),
            name: ara_mock::NAME.into(),
        },
        bypass: false,
        parameters: Vec::new(),
        state: None,
        sidechain: None,
        mono_to_stereo: false,
        mix_origin: None,
    };
    let id = slot.id;
    tp.project.track_mut(track).unwrap().inserts.push(slot);
    tp.project.ara.slots.insert(id);
    Song {
        tp,
        track,
        clip,
        slot: id,
        source,
    }
}

fn render(s: &Song) -> (OfflineRenderer, Vec<f32>) {
    let mut r = OfflineRenderer::new(&s.tp.project, &s.tp.sources, config(), 512, 2).unwrap();
    r.play_from(0).unwrap();
    let out = r.render(RATE as usize * 3);
    (r, out[0].clone())
}

fn peak(x: &[f32]) -> f32 {
    x.iter().fold(0.0, |m, v| m.max(v.abs()))
}

/// The tone as the clip player plays it (the mock's insert left out).
fn plain(s: &Song) -> Vec<f32> {
    let mut p = s.tp.project.clone();
    p.track_mut(s.track).unwrap().inserts.clear();
    let mut r = OfflineRenderer::new(&p, &s.tp.sources, config(), 512, 2).unwrap();
    r.play_from(0).unwrap();
    r.render(RATE as usize * 3)[0].clone()
}

#[test]
fn an_ara_insert_renders_its_tracks_clips() {
    let s = song("renders");
    let want = plain(&s);
    assert!(peak(&want[2 * RATE as usize..]) > 0.3);
    let (r, out) = render(&s);
    assert!(r.controller.is_ara_renderer(s.slot));
    let probe = ara_mock::probe_of("renders").unwrap();
    assert_eq!(probe.summary().regions, 1);
    assert!(probe.violations().is_empty(), "{:?}", probe.violations());
    // Silent before the clip, the tone where it is (the clip player left
    // it out: no doubling, nothing through the ignored input).
    assert!(peak(&out[..2 * RATE as usize - 64]) < 1e-6);
    for (i, (a, b)) in out.iter().zip(&want).enumerate() {
        assert!((a - b).abs() < 1e-5, "frame {i}: {a} vs {b}");
    }
}

#[test]
fn moves_and_removals_reach_the_document() {
    let mut s = song("moves");
    let (mut r, _) = render(&s);
    // The clip one second earlier: only the region's properties change.
    let at = MusicalTime::from_quarters(2.0);
    s.tp.project.clips.get_mut(&s.clip).unwrap().start = at;
    r.controller
        .sync(&s.tp.project, &s.tp.sources, Impact::Timeline)
        .unwrap();
    r.play_from(0).unwrap();
    let out = r.render(RATE as usize * 3)[0].clone();
    assert!(peak(&out[RATE as usize..RATE as usize + RATE as usize / 2]) > 0.3);
    assert!(peak(&out[2 * RATE as usize..]) < 1e-6);
    // Removed: the renderer loses its region (deactivated, the graph
    // rebuilt).
    s.tp.project.clips.remove(&s.clip);
    s.tp.project.track_mut(s.track).unwrap().clips.clear();
    r.controller
        .sync(&s.tp.project, &s.tp.sources, Impact::Timeline)
        .unwrap();
    r.play_from(0).unwrap();
    let out = r.render(RATE as usize * 3)[0].clone();
    assert!(peak(&out) < 1e-6);
    let probe = ara_mock::probe_of("moves").unwrap();
    assert!(probe.violations().is_empty(), "{:?}", probe.violations());
    assert_eq!(probe.summary().regions, 0);
}

#[test]
fn the_plugins_edits_reach_a_render_through_the_archive() {
    let mut s = song("archive");
    let (mut r, _) = render(&s);
    let modification = format!("mod:{}:{}", s.track.raw(), s.source.raw());
    let probe = ara_mock::probe_of("archive").unwrap();
    assert!(probe.edit(&modification, 0.5), "an edit in its editor");
    let poll = r.controller.poll_plugins();
    assert!(poll.state_dirty);
    assert!(r.controller.ara_unsaved());
    let archives = r.controller.capture_ara_archives();
    assert_eq!(archives.len(), 1);
    assert_eq!(
        archives[0].factory,
        faderframe_plugin_host::ara::mock::FACTORY_ID
    );
    assert!(!r.controller.ara_unsaved());
    s.tp.project.ara.archives = archives;
    // A render (an engine of its own) restores the edits.
    let want = plain(&s);
    let (_, out) = render(&s);
    let at = 2 * RATE as usize + 1000;
    assert!(
        (out[at] - 0.5 * want[at]).abs() < 1e-5,
        "{} vs {}",
        out[at],
        want[at]
    );
}

#[test]
fn a_bypassed_ara_insert_lets_the_clips_play() {
    let mut s = song("bypassed");
    s.tp.project.track_mut(s.track).unwrap().inserts[0].bypass = true;
    let want = plain(&s);
    let (r, out) = render(&s);
    assert!(r.controller.ara_rendered_tracks().is_empty());
    for (a, b) in out.iter().zip(&want) {
        assert!((a - b).abs() < 1e-5);
    }
}

#[test]
fn an_ara_insert_behind_latency_lines_up() {
    let mut s = song("latency");
    // A plugin with latency before the ARA insert, another track with the
    // same clip as the reference.
    let probe = PluginSlot {
        id: s.tp.project.ids.allocate(),
        plugin: PluginRef::builtin(builtin::LATENCY_PROBE, "Latency"),
        bypass: false,
        parameters: vec![SavedParameter {
            id: ParameterId(0),
            value: 1500.0,
        }],
        state: None,
        sidechain: None,
        mono_to_stereo: false,
        mix_origin: None,
    };
    s.tp.project
        .track_mut(s.track)
        .unwrap()
        .inserts
        .insert(0, probe);
    let reference =
        s.tp.track(TrackKind::Audio, "Reference", ChannelLayout::Stereo);
    s.tp.clip(
        reference,
        s.source,
        MusicalTime::from_quarters(4.0),
        RATE as i64 / 2,
    );
    s.tp.project.track_mut(reference).unwrap().phase_invert = true;
    // The ARA track plus the inverted reference cancel when aligned.
    let (_, out) = render(&s);
    assert!(peak(&out) < 1e-4, "residual {}", peak(&out));
}

#[test]
fn notes_come_back_from_the_plugin() {
    let s = song("notes");
    let (r, _) = render(&s);
    let notes = r.controller.ara_notes(s.clip, s.track).unwrap();
    assert_eq!(notes.len(), 1, "{notes:?}");
    assert_eq!(notes[0].pitch, Some(69));
    assert!((notes[0].start - 2.0).abs() < 1e-3, "song time");
}
