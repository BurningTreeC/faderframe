#![allow(clippy::unwrap_used)]
//! MIDI effects before the instrument: the track's notes go through them
//! in insert order, the instrument plays what they made of them; they
//! follow the key track; bypassed they pass the notes as they are; a MIDI
//! track's effects shape what it sends to an instrument.

mod common;

use common::TestProject;
use faderframe_core::{ChannelLayout, ParameterId, TrackId, builtin};
use faderframe_engine::EngineConfig;
use faderframe_engine::offline::OfflineRenderer;
use faderframe_plugin_host::devices::{arpeggiator, chord, scale, synth};
use faderframe_project::harmony::{Key, Scale};
use faderframe_project::{
    Clip, ClipContent, KeyChange, MidiClip, MidiNote, OutputRouting, PluginRef, PluginSlot,
    SavedParameter, TrackColor, TrackKind,
};
use faderframe_timeline::MusicalTime;

const SR: u32 = 48_000;

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

/// A plain sine, no filter movement: each note one clean partial.
fn sine(tp: &mut TestProject) -> PluginSlot {
    use synth::id::*;
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

/// Notes held from the start for `quarters`.
fn clip(tp: &mut TestProject, track: TrackId, keys: &[u8], quarters: f64) {
    let notes = keys
        .iter()
        .map(|k| MidiNote {
            id: tp.project.ids.allocate(),
            start: MusicalTime::ZERO,
            length: MusicalTime::from_quarters(quarters),
            key: *k,
            velocity: 100,
            channel: 0,
            muted: false,
        })
        .collect();
    let id = tp.project.ids.allocate();
    tp.project.clips.insert(
        id,
        Clip {
            id,
            track,
            name: "Notes".into(),
            color: Some(TrackColor::palette(1)),
            start: MusicalTime::ZERO,
            muted: false,
            content: ClipContent::Midi(MidiClip {
                length: MusicalTime::from_quarters_i(8),
                notes,
                controllers: Vec::new(),
                expressions: Vec::new(),
                sysex: Vec::new(),
            }),
        },
    );
    tp.project.track_mut(track).unwrap().clips.push(id);
}

/// `samples` of the render's left channel.
fn render(tp: &TestProject, samples: usize) -> Vec<f32> {
    let config = EngineConfig {
        sample_rate: SR,
        ..EngineConfig::default()
    };
    let mut r = OfflineRenderer::new(&tp.project, &tp.sources, config, 256, 2).unwrap();
    r.play_from(0).unwrap();
    let mut out = Vec::new();
    while out.len() < samples {
        out.extend_from_slice(r.step().output_ref(0));
    }
    out.truncate(samples);
    out
}

/// The level of a MIDI key's pitch in `x` (Goertzel, Hann window).
fn level(x: &[f32], key: u8) -> f64 {
    let f = 440.0 * 2f64.powf((f64::from(key) - 69.0) / 12.0);
    let w = std::f64::consts::TAU * f / f64::from(SR);
    let n = x.len() as f64;
    let (mut re, mut im, mut sum) = (0.0, 0.0, 0.0);
    for (i, v) in x.iter().enumerate() {
        let win = 0.5 - 0.5 * (std::f64::consts::TAU * i as f64 / n).cos();
        re += f64::from(*v) * win * (w * i as f64).cos();
        im += f64::from(*v) * win * (w * i as f64).sin();
        sum += win;
    }
    2.0 * re.hypot(im) / sum
}

fn sounding(x: &[f32], keys: &[u8]) -> Vec<u8> {
    let loudest = keys.iter().map(|k| level(x, *k)).fold(0.0, f64::max);
    keys.iter()
        .copied()
        .filter(|k| level(x, *k) > loudest * 0.3 && loudest > 1e-4)
        .collect()
}

#[test]
fn a_chord_device_turns_a_note_into_a_chord_and_bypassed_passes_it() {
    let mut tp = TestProject::new(SR);
    let t = tp.track(TrackKind::Instrument, "Keys", ChannelLayout::Stereo);
    let fx = slot(&mut tp, builtin::CHORD, &[]);
    let synth = sine(&mut tp);
    tp.project.track_mut(t).unwrap().inserts = vec![fx, synth];
    clip(&mut tp, t, &[60], 4.0);
    let x = render(&tp, 24_000);
    assert_eq!(sounding(&x[4800..], &[60, 62, 64, 65, 67]), [60, 64, 67]);
    // Bypassed: the note alone.
    tp.project.track_mut(t).unwrap().inserts[0].bypass = true;
    let x = render(&tp, 24_000);
    assert_eq!(sounding(&x[4800..], &[60, 64, 67]), [60]);
    // After the synth it does nothing to it.
    let ins = &mut tp.project.track_mut(t).unwrap().inserts;
    ins[0].bypass = false;
    ins.swap(0, 1);
    let x = render(&tp, 24_000);
    assert_eq!(sounding(&x[4800..], &[60, 64, 67]), [60]);
}

#[test]
fn the_arpeggiator_plays_a_held_chord_one_note_a_sixteenth() {
    let mut tp = TestProject::new(SR);
    let t = tp.track(TrackKind::Instrument, "Arp", ChannelLayout::Stereo);
    let fx = slot(
        &mut tp,
        builtin::ARPEGGIATOR,
        &[(arpeggiator::id::GATE, 0.9)],
    );
    let synth = sine(&mut tp);
    tp.project.track_mut(t).unwrap().inserts = vec![fx, synth];
    clip(&mut tp, t, &[60, 64, 67], 4.0);
    // 120 BPM: a sixteenth is 6000 samples.
    tp.project.timeline.tempo = faderframe_timeline::TempoMap::new(120.0);
    let x = render(&tp, 6000 * 4);
    let step = |k: usize| sounding(&x[k * 6000 + 600..k * 6000 + 5000], &[60, 64, 67]);
    assert_eq!(
        [step(0), step(1), step(2), step(3)],
        [vec![60], vec![64], vec![67], vec![60]]
    );
}

#[test]
fn the_scale_device_follows_the_key_track_and_a_midi_track_sends_through_its_effects() {
    let mut tp = TestProject::new(SR);
    let t = tp.track(TrackKind::Instrument, "Keys", ChannelLayout::Stereo);
    let fx = slot(&mut tp, builtin::SCALE, &[]);
    let synth = sine(&mut tp);
    tp.project.track_mut(t).unwrap().inserts = vec![fx, synth];
    clip(&mut tp, t, &[65], 4.0);
    // D major: F lies between E and F#, and a tie goes down: E.
    tp.project.keys = vec![KeyChange {
        at: MusicalTime::ZERO,
        key: Key::new(2, Scale::Major),
    }];
    let x = render(&tp, 24_000);
    assert_eq!(sounding(&x[4800..], &[64, 65, 66]), [64]);
    let _ = scale::id::MODE;
    // A MIDI track, its Chord device, into the instrument track.
    let mut tp = TestProject::new(SR);
    let keys = tp.track(TrackKind::Instrument, "Keys", ChannelLayout::Stereo);
    let synth = sine(&mut tp);
    tp.project.track_mut(keys).unwrap().inserts = vec![synth];
    let m = tp.track(TrackKind::Midi, "Notes", ChannelLayout::Stereo);
    let fx = slot(
        &mut tp,
        builtin::CHORD,
        &[(chord::id::SHIFT, 7.0), (chord::id::SHIFT + 1, 0.0)],
    );
    let mt = tp.project.track_mut(m).unwrap();
    mt.inserts = vec![fx];
    mt.output = OutputRouting::Track { track: keys };
    clip(&mut tp, m, &[60], 4.0);
    let x = render(&tp, 24_000);
    assert_eq!(sounding(&x[4800..], &[60, 64, 67]), [60, 67]);
}

/// A MIDI track reaches every instrument of the track it plays, as that
/// track's own notes do: with an old-style instrument slot followed by a
/// synth insert (which replaces what comes in), only the insert is heard.
#[test]
fn a_midi_track_plays_every_instrument_of_its_track() {
    let mut tp = TestProject::new(SR);
    let keys = tp.track(TrackKind::Instrument, "Keys", ChannelLayout::Stereo);
    let legacy = sine(&mut tp);
    let insert = sine(&mut tp);
    let kt = tp.project.track_mut(keys).unwrap();
    kt.instrument = Some(legacy);
    kt.inserts = vec![insert];
    let m = tp.track(TrackKind::Midi, "Notes", ChannelLayout::Stereo);
    tp.project.track_mut(m).unwrap().output = OutputRouting::Track { track: keys };
    clip(&mut tp, m, &[69], 4.0);
    let x = render(&tp, 24_000);
    assert_eq!(sounding(&x[4800..], &[60, 69]), [69]);
}

/// A MIDI track's mute silences it (it has no strip): by itself, by a
/// folder it is in, or by another track's solo.
#[test]
fn muting_a_midi_track_or_its_folder_silences_it() {
    let build = |setup: &dyn Fn(&mut TestProject, TrackId, TrackId, TrackId)| {
        let mut tp = TestProject::new(SR);
        let keys = tp.track(TrackKind::Instrument, "Keys", ChannelLayout::Stereo);
        let synth = sine(&mut tp);
        tp.project.track_mut(keys).unwrap().inserts = vec![synth];
        let m = tp.track(TrackKind::Midi, "Notes", ChannelLayout::Stereo);
        tp.project.track_mut(m).unwrap().output = OutputRouting::Track { track: keys };
        clip(&mut tp, m, &[69], 4.0);
        let folder = tp.track(TrackKind::Folder, "Folder", ChannelLayout::Stereo);
        tp.project.track_mut(m).unwrap().folder = Some(folder);
        setup(&mut tp, keys, m, folder);
        sounding(&render(&tp, 24_000)[4800..], &[69])
    };
    assert_eq!(build(&|_, _, _, _| {}), [69], "plays");
    assert!(build(&|tp, _, m, _| tp.project.track_mut(m).unwrap().mute = true).is_empty());
    assert!(
        build(&|tp, _, _, f| tp.project.track_mut(f).unwrap().mute = true).is_empty(),
        "the folder's mute reaches it"
    );
    // Another MIDI track soloed: this one is out.
    assert!(
        build(&|tp, keys, _, _| {
            let other = tp.track(TrackKind::Midi, "Other", ChannelLayout::Stereo);
            let o = tp.project.track_mut(other).unwrap();
            o.output = OutputRouting::Track { track: keys };
            o.solo = true;
        })
        .is_empty()
    );
    // The folder soloed: what is in it plays.
    assert_eq!(
        build(&|tp, _, _, f| tp.project.track_mut(f).unwrap().solo = true),
        [69]
    );
}

/// A folder's mute reaches the audio tracks in it (and folders in it).
#[test]
fn a_folders_mute_reaches_the_tracks_in_it() {
    let build = |mute_outer: bool| {
        let mut tp = TestProject::new(SR);
        let keys = tp.track(TrackKind::Instrument, "Keys", ChannelLayout::Stereo);
        let synth = sine(&mut tp);
        tp.project.track_mut(keys).unwrap().inserts = vec![synth];
        clip(&mut tp, keys, &[69], 4.0);
        let outer = tp.track(TrackKind::Folder, "Outer", ChannelLayout::Stereo);
        let inner = tp.track(TrackKind::Folder, "Inner", ChannelLayout::Stereo);
        tp.project.track_mut(inner).unwrap().folder = Some(outer);
        tp.project.track_mut(keys).unwrap().folder = Some(inner);
        tp.project.track_mut(outer).unwrap().mute = mute_outer;
        sounding(&render(&tp, 24_000)[4800..], &[69])
    };
    assert_eq!(build(false), [69]);
    assert!(build(true).is_empty());
}
