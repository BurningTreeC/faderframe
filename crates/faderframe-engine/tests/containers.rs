//! Containers: parallel chains of devices, mixed, their latencies aligned.
#![allow(clippy::unwrap_used)]
mod common;
use common::TestProject;
use faderframe_core::{ChannelLayout, ParameterId, PluginInstanceId, TrackId, builtin, db_to_gain};
use faderframe_engine::{EngineConfig, offline::render_project};
use faderframe_project::container::Chain;
use faderframe_project::{PluginRef, PluginSlot, SavedParameter, TrackKind};
use faderframe_timeline::MusicalTime;

fn slot(tp: &mut TestProject, id: &str, set: &[(u32, f64)]) -> PluginSlot {
    PluginSlot {
        id: tp.project.ids.allocate(),
        plugin: PluginRef::builtin(id, id),
        bypass: false,
        parameters: set
            .iter()
            .map(|(id, value)| SavedParameter {
                id: ParameterId(*id),
                value: *value,
            })
            .collect(),
        state: None,
        sidechain: None,
    }
}

fn render(tp: &TestProject, frames: usize) -> Vec<f32> {
    render_project(
        &tp.project,
        &tp.sources,
        EngineConfig::default(),
        256,
        0,
        frames,
    )
    .unwrap()
    .swap_remove(0)
}

/// A track of a constant 0.5 with a container: chain "Down" (a utility at
/// −6 dB) and chain "Dry".
fn setup() -> (TestProject, TrackId, PluginInstanceId) {
    let mut tp = TestProject::new(48_000);
    let t = tp.track(TrackKind::Audio, "Tone", ChannelLayout::Stereo);
    let src = tp.dc(2, 0.5, 48_000);
    tp.clip(t, src, MusicalTime::ZERO, 48_000);
    let container = slot(&mut tp, builtin::CONTAINER, &[]);
    let down = slot(&mut tp, builtin::GAIN, &[(0, -6.0)]);
    let id = container.id;
    let track = tp.project.track_mut(t).unwrap();
    track.inserts.push(container);
    let mut a = Chain::new("Down");
    a.inserts.push(down);
    track.containers.insert(id, vec![a, Chain::new("Dry")]);
    (tp, t, id)
}

#[test]
fn chains_are_mixed_and_muted_soloed_and_levelled() {
    let (mut tp, t, c) = setup();
    let both = 0.5 * (1.0 + db_to_gain(-6.0));
    let out = render(&tp, 8_000);
    assert!((out[6_000] - both).abs() < 1e-3, "{}", out[6_000]);
    fn chains(tp: &mut TestProject, t: TrackId, c: PluginInstanceId) -> &mut Vec<Chain> {
        tp.project
            .track_mut(t)
            .unwrap()
            .containers
            .get_mut(&c)
            .unwrap()
    }
    chains(&mut tp, t, c)[1].mute = true;
    let out = render(&tp, 8_000);
    assert!((out[6_000] - 0.5 * db_to_gain(-6.0)).abs() < 1e-3);
    chains(&mut tp, t, c)[1].mute = false;
    chains(&mut tp, t, c)[1].solo = true;
    let out = render(&tp, 8_000);
    assert!((out[6_000] - 0.5).abs() < 1e-3, "solo: the dry chain alone");
    chains(&mut tp, t, c)[1].solo = false;
    chains(&mut tp, t, c)[1].gain_db = -6.0;
    let out = render(&tp, 8_000);
    assert!(
        (out[6_000] - db_to_gain(-6.0)).abs() < 1e-3,
        "{}",
        out[6_000]
    );
    // Bypassed: the dry signal.
    tp.project.track_mut(t).unwrap().inserts[0].bypass = true;
    let out = render(&tp, 8_000);
    assert!((out[6_000] - 0.5).abs() < 1e-3);
}

#[test]
fn a_late_chain_is_aligned_with_the_others() {
    let mut tp = TestProject::new(48_000);
    let t = tp.track(TrackKind::Audio, "Click", ChannelLayout::Stereo);
    let src = tp.impulse(2, 4_096);
    tp.clip(t, src, MusicalTime::from_quarters(1.0), 4_096);
    let container = slot(&mut tp, builtin::CONTAINER, &[]);
    let late = slot(&mut tp, builtin::LATENCY_PROBE, &[(0, 300.0)]);
    let id = container.id;
    let track = tp.project.track_mut(t).unwrap();
    track.inserts.push(container);
    let mut a = Chain::new("Late");
    a.inserts.push(late);
    track.containers.insert(id, vec![a, Chain::new("Dry")]);
    let out = render(&tp, 48_000);
    // One click of both chains, not two.
    let hits: Vec<(usize, f32)> = out
        .iter()
        .enumerate()
        .filter(|(_, v)| v.abs() > 1e-3)
        .map(|(i, v)| (i, *v))
        .collect();
    assert_eq!(hits.len(), 1, "{hits:?}");
    assert!((hits[0].1 - 2.0).abs() < 1e-3, "{hits:?}");
}

#[test]
fn containers_nest() {
    let (mut tp, t, c) = setup();
    // In the dry chain: another container, both of whose chains are dry.
    let inner = slot(&mut tp, builtin::CONTAINER, &[]);
    let inner_id = inner.id;
    let track = tp.project.track_mut(t).unwrap();
    track.containers.get_mut(&c).unwrap()[1].inserts.push(inner);
    track
        .containers
        .insert(inner_id, vec![Chain::new("A"), Chain::new("B")]);
    let out = render(&tp, 8_000);
    let want = 0.5 * db_to_gain(-6.0) + 2.0 * 0.5;
    assert!((out[6_000] - want).abs() < 1e-3, "{}", out[6_000]);
    // The devices inside are the track's.
    let ids: Vec<_> = tp
        .project
        .track(t)
        .unwrap()
        .slots()
        .iter()
        .map(|s| s.id)
        .collect();
    assert_eq!(ids.len(), 3);
    assert!(ids.contains(&inner_id));
}

/// A plain sine synth (no filter movement, quick envelope).
fn sine(tp: &mut TestProject) -> PluginSlot {
    use faderframe_plugin_host::devices::synth::id::*;
    slot(
        tp,
        builtin::SYNTH,
        &[
            (OSC1_WAVE, 3.0),
            (OSC2_LEVEL, 0.0),
            (DETUNE, 0.0),
            (CUTOFF, 20_000.0),
            (RESONANCE, 0.0),
            (ENV_AMOUNT, 0.0),
            (ATTACK, 1.0),
            (SUSTAIN, 1.0),
            (RELEASE, 5.0),
            (WIDTH, 0.0),
        ],
    )
}

/// A MIDI clip on `track` holding `key` for a bar.
fn note(tp: &mut TestProject, track: TrackId, key: u8) {
    use faderframe_project::{Clip, ClipContent, MidiClip, MidiNote};
    let n = MidiNote {
        id: tp.project.ids.allocate(),
        start: MusicalTime::ZERO,
        length: MusicalTime::from_quarters(4.0),
        key,
        velocity: 100,
        channel: 0,
        muted: false,
        release: None,
    };
    let id = tp.project.ids.allocate();
    tp.project.clips.insert(
        id,
        Clip {
            id,
            track,
            name: "Note".into(),
            color: None,
            start: MusicalTime::ZERO,
            muted: false,
            content: ClipContent::Midi(MidiClip {
                length: MusicalTime::from_quarters(4.0),
                notes: vec![n],
                controllers: Vec::new(),
                expressions: Vec::new(),
                sysex: Vec::new(),
            }),
        },
    );
    tp.project.track_mut(track).unwrap().clips.push(id);
}

fn peak(x: &[f32]) -> f32 {
    x.iter().fold(0.0f32, |m, v| m.max(v.abs()))
}

/// An instrument track whose container splits the keyboard: a synth in
/// "Low" (up to B3) and one in "High" (C4 up).
fn split(tp: &mut TestProject) -> (TrackId, PluginInstanceId) {
    let t = tp.track(TrackKind::Instrument, "Keys", ChannelLayout::Stereo);
    let container = slot(tp, builtin::CONTAINER, &[]);
    let (lo_synth, hi_synth) = (sine(tp), sine(tp));
    let id = container.id;
    let track = tp.project.track_mut(t).unwrap();
    track.inserts.push(container);
    let mut low = Chain::new("Low");
    low.key_high = 59;
    low.inserts.push(lo_synth);
    let mut high = Chain::new("High");
    high.key_low = 60;
    high.inserts.push(hi_synth);
    track.containers.insert(id, vec![low, high]);
    (t, id)
}

#[test]
fn chains_play_the_notes_of_their_key_range() {
    for (key, low_heard) in [(48u8, true), (72u8, false)] {
        let mut tp = TestProject::new(48_000);
        let (t, c) = split(&mut tp);
        note(&mut tp, t, key);
        let both = peak(&render(&tp, 12_000)[2_000..]);
        assert!(both > 0.01, "{key}: {both}");
        // The low chain muted: only a high note still sounds.
        tp.project
            .track_mut(t)
            .unwrap()
            .containers
            .get_mut(&c)
            .unwrap()[0]
            .mute = true;
        let without_low = peak(&render(&tp, 12_000)[2_000..]);
        if low_heard {
            assert!(
                without_low < 1e-4,
                "{key}: the high chain kept out: {without_low}"
            );
        } else {
            assert!(
                (without_low - both).abs() < 1e-4,
                "{key}: {without_low} vs {both}"
            );
        }
    }
}

#[test]
fn a_midi_track_reaches_the_instruments_in_containers() {
    let mut tp = TestProject::new(48_000);
    let (t, _) = split(&mut tp);
    let midi = tp.track(TrackKind::Midi, "Notes", ChannelLayout::Stereo);
    tp.project.track_mut(midi).unwrap().output =
        faderframe_project::OutputRouting::Track { track: t };
    note(&mut tp, midi, 72);
    let out = render(&tp, 12_000);
    assert!(
        peak(&out[2_000..]) > 0.01,
        "the high chain plays the MIDI track's note"
    );
}

#[test]
fn a_device_in_a_chain_is_keyed_from_another_track() {
    use faderframe_plugin_host::devices::gate::id as gate;
    let mut tp = TestProject::new(48_000);
    // The key: loud for a quarter of a second, then silent; not heard.
    let key = tp.track(TrackKind::Audio, "Key", ChannelLayout::Stereo);
    let src = tp.dc(2, 0.5, 12_000);
    tp.clip(key, src, MusicalTime::ZERO, 12_000);
    tp.project.track_mut(key).unwrap().output = faderframe_project::OutputRouting::None;
    let t = tp.track(TrackKind::Audio, "Tone", ChannelLayout::Stereo);
    let src = tp.dc(2, 0.25, 48_000);
    tp.clip(t, src, MusicalTime::ZERO, 48_000);
    let container = slot(&mut tp, builtin::CONTAINER, &[]);
    let mut gate_slot = slot(&mut tp, builtin::GATE, &[(gate::RELEASE, 20.0)]);
    gate_slot.sidechain = Some(key);
    let id = container.id;
    let track = tp.project.track_mut(t).unwrap();
    track.inserts.push(container);
    let mut gated = Chain::new("Gated");
    gated.inserts.push(gate_slot);
    track.containers.insert(id, vec![gated]);
    let out = render(&tp, 36_000);
    assert!(peak(&out[4_000..10_000]) > 0.2, "open while the key plays");
    assert!(
        peak(&out[24_000..]) < 1e-3,
        "closed after: {}",
        peak(&out[24_000..])
    );
}
