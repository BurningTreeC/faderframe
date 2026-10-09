#![allow(clippy::unwrap_used)]
//! Voice to MIDI through the engine: a sung note on the input plays the
//! live instrument within milliseconds — in the callback it is heard, a
//! couple of its periods after it begins.

mod common;

use common::TestProject;
use faderframe_analysis::voice::{Responsiveness, VoiceConfig};
use faderframe_audio::OwnedBuffers;
use faderframe_core::{ChannelLayout, builtin};
use faderframe_engine::EngineConfig;
use faderframe_engine::offline::OfflineRenderer;
use faderframe_engine::voice::{Glide, VoiceRun};
use faderframe_midi::midi_input_queue;
use faderframe_project::{Impact, InputRouting, PluginRef, PluginSlot, TrackKind};
use std::collections::HashSet;

const SR: u32 = 48_000;
const BLOCK: usize = 64;
/// The note starts here (frames).
const ONSET: usize = 4800;

/// How long (ms) after a sung `key` begins the instrument sounds.
fn voice_to_sound(key: f64, r: Responsiveness) -> f64 {
    let mut tp = TestProject::new(SR);
    let t = tp.track(TrackKind::Instrument, "Voice", ChannelLayout::Stereo);
    let slot = PluginSlot {
        id: tp.project.ids.allocate(),
        plugin: PluginRef::builtin(builtin::SYNTH, "Synth"),
        bypass: false,
        parameters: Vec::new(),
        state: None,
        sidechain: None,
    };
    let track = tp.project.track_mut(t).unwrap();
    track.instrument = Some(slot);
    track.input = InputRouting::all_midi();
    let config = EngineConfig {
        sample_rate: SR,
        ..EngineConfig::default()
    };
    let mut rd = OfflineRenderer::new(&tp.project, &tp.sources, config, BLOCK, 2).unwrap();
    let (_tx, q, _feed) = midi_input_queue(64);
    rd.controller.set_midi_input(q).unwrap();
    rd.controller.set_midi_live(HashSet::from([t]));
    rd.controller
        .sync(&tp.project, &tp.sources, Impact::Params)
        .unwrap();
    let (run, _heard) = VoiceRun::new(0, 0, SR, VoiceConfig::default().with(r), Glide::Off);
    rd.controller.set_voice(Some(run)).unwrap();
    let f = 440.0 * 2f64.powf((key - 69.0) / 12.0);
    let mut bufs = OwnedBuffers::new(1, 2, BLOCK);
    let mut phase = 0.0f64;
    for b in 0..(SR as usize / BLOCK) {
        // A glottal-ish saw from ONSET with a 5 ms attack.
        for (i, v) in bufs.input_mut(0).iter_mut().enumerate() {
            let n = b * BLOCK + i;
            *v = if n < ONSET {
                0.0
            } else {
                let t = (n - ONSET) as f64 / f64::from(SR);
                phase = (phase + f / f64::from(SR)).fract();
                let mut s = 0.0;
                for h in 1..12 {
                    s += (2.0 * std::f64::consts::PI * phase * h as f64).sin() / (h * h) as f64;
                }
                (s * 0.3 * (t / 0.005).min(1.0)) as f32
            };
        }
        rd.processor.process_device(&mut bufs);
        if let Some(i) = bufs.output_ref(0).iter().position(|v| v.abs() > 1e-4) {
            let n = b * BLOCK + i;
            assert!(n >= ONSET, "sound before the voice");
            return (n - ONSET) as f64 * 1000.0 / f64::from(SR);
        }
    }
    panic!("the instrument never sounded for {key}");
}

#[test]
fn a_sung_note_sounds_within_milliseconds() {
    for (key, fast, balanced) in [(69.0, 8.0, 11.5), (57.0, 13.5, 17.0)] {
        let f = voice_to_sound(key, Responsiveness::Fast);
        let b = voice_to_sound(key, Responsiveness::Balanced);
        eprintln!("key {key}: fast {f:.1} ms, balanced {b:.1} ms (+ device buffers)");
        assert!(f <= fast, "{key}: fast {f}");
        assert!(b <= balanced, "{key}: balanced {b}");
    }
}

#[test]
fn a_chord_plays_records_and_releases_when_listening_stops() {
    chord_in_engine(Glide::Expression, false);
}

#[test]
fn polyphonic_pitch_bend_is_suppressed_and_missing_input_releases() {
    chord_in_engine(Glide::PitchBend, true);
}

fn chord_in_engine(glide: Glide, disconnect_input: bool) {
    use faderframe_engine::midi::{MidiFilter, MidiRecordTarget};
    use faderframe_midi::MidiEvent;
    use faderframe_transport::TransportCommand;

    let mut tp = TestProject::new(SR);
    let t = tp.track(TrackKind::Instrument, "Chords", ChannelLayout::Stereo);
    let id = tp.project.ids.allocate();
    let track = tp.project.track_mut(t).unwrap();
    track.instrument = Some(PluginSlot {
        id,
        plugin: PluginRef::builtin(builtin::SYNTH, "Synth"),
        bypass: false,
        parameters: Vec::new(),
        state: None,
        sidechain: None,
    });
    track.input = InputRouting::all_midi();
    let mut rd = OfflineRenderer::new(
        &tp.project,
        &tp.sources,
        EngineConfig {
            sample_rate: SR,
            ..EngineConfig::default()
        },
        BLOCK,
        2,
    )
    .unwrap();
    let (_tx, q, _feed) = midi_input_queue(64);
    rd.controller.set_midi_input(q).unwrap();
    rd.controller.set_midi_live(HashSet::from([t]));
    rd.controller
        .sync(&tp.project, &tp.sources, Impact::Params)
        .unwrap();
    let (run, mut heard) = VoiceRun::new_polyphonic(0, 7, SR, VoiceConfig::default(), glide);
    rd.controller.set_voice(Some(run)).unwrap();
    let mut recorded = rd
        .controller
        .begin_midi_recording(
            vec![MidiRecordTarget {
                track: t,
                filter: MidiFilter {
                    port: Some(7),
                    channel: Some(0),
                },
            }],
            0,
            i64::MAX,
        )
        .unwrap();
    rd.controller
        .transport(TransportCommand::SetRecording(true))
        .unwrap();
    rd.play_from(0).unwrap();
    let mut bufs = OwnedBuffers::new(1, 2, BLOCK);
    let mut peak = 0.0f32;
    for b in 0..600 {
        for (i, v) in bufs.input_mut(0).iter_mut().enumerate() {
            let n = b * BLOCK + i;
            let time = n.saturating_sub(ONSET) as f64 / f64::from(SR);
            // A slightly detuned triad so expression is exercised for each key.
            *v = if n < ONSET {
                0.0
            } else {
                [48.15, 51.85, 55.2]
                    .iter()
                    .map(|k| {
                        let f = 440.0 * 2f64.powf((k - 69.0) / 12.0);
                        (1..=10)
                            .map(|h| (std::f64::consts::TAU * f * h as f64 * time).sin() / h as f64)
                            .sum::<f64>()
                            * 0.12
                            * (time / 0.005).min(1.0)
                    })
                    .sum::<f64>() as f32
            };
        }
        rd.processor.process_device(&mut bufs);
        peak = bufs.output_ref(0).iter().fold(peak, |p, v| p.max(v.abs()));
    }
    assert!(peak > 0.01, "the chord reaches the synth");
    let events: Vec<_> = std::iter::from_fn(|| recorded.pop().ok()).collect();
    let mut keys = Vec::new();
    for e in &events {
        if let MidiEvent::NoteOn { key, .. } = e.event {
            keys.push(key);
            assert!(
                (e.position - ONSET as i64).abs() < (SR / 20) as i64,
                "note {key} recorded at {} rather than its onset",
                e.position
            );
        }
    }
    keys.sort_unstable();
    assert_eq!(keys, [48, 52, 55]);
    let copies: Vec<_> = std::iter::from_fn(|| heard.pop().ok()).collect();
    assert!(
        !copies
            .iter()
            .any(|e| matches!(e.event(), Some(MidiEvent::PitchBend { .. })))
    );
    if glide == Glide::Expression {
        for key in keys {
            assert!(
                copies.iter().any(|e| e.port == 7
                    && matches!(e.event(),
            Some(MidiEvent::NoteExpression { key: k, .. }) if k == key)),
                "per-note tuning for {key}"
            );
        }
    }

    // The stop is consumed in a zero-frame control pump. Releases must
    // survive until a real callback and reach both capture and the synth.
    if disconnect_input {
        bufs = OwnedBuffers::new(0, 2, BLOCK);
    } else {
        rd.controller.set_voice(None).unwrap();
    }
    let mut idle = OwnedBuffers::new(if disconnect_input { 0 } else { 1 }, 2, 0);
    rd.processor.process_device(&mut idle);
    rd.processor.process_device(&mut bufs);
    let mut released: Vec<_> = std::iter::from_fn(|| recorded.pop().ok())
        .filter_map(|e| {
            if let MidiEvent::NoteOff { key, .. } = e.event {
                Some(key)
            } else {
                None
            }
        })
        .collect();
    released.sort_unstable();
    assert_eq!(released, [48, 52, 55]);
    for _ in 0..1600 {
        rd.processor.process_device(&mut bufs);
    }
    assert!(
        bufs.output_ref(0).iter().all(|v| v.abs() < 1e-4),
        "no stuck notes"
    );
    assert_eq!(rd.controller.midi_counters(), (0, 0));
}

/// Two singers on two inputs: two listeners side by side, each on its
/// own port and MIDI channel; one stops, the other sings on.
#[test]
fn two_inputs_are_listened_to_at_once() {
    use faderframe_midi::MidiEvent;

    let mut tp = TestProject::new(SR);
    let t = tp.track(TrackKind::Instrument, "Choir", ChannelLayout::Stereo);
    tp.project.track_mut(t).unwrap().input = InputRouting::all_midi();
    let mut rd = OfflineRenderer::new(
        &tp.project,
        &tp.sources,
        EngineConfig {
            sample_rate: SR,
            ..EngineConfig::default()
        },
        BLOCK,
        2,
    )
    .unwrap();
    let (_tx, q, _feed) = midi_input_queue(64);
    rd.controller.set_midi_input(q).unwrap();
    let (a, mut heard_a) = VoiceRun::new(0, 3, SR, VoiceConfig::default(), Glide::Off);
    let (b, mut heard_b) = VoiceRun::new(1, 4, SR, VoiceConfig::default(), Glide::Off);
    rd.controller.add_voice(a.on_channel(1)).unwrap();
    rd.controller.add_voice(b.on_channel(2)).unwrap();
    let mut bufs = OwnedBuffers::new(2, 2, BLOCK);
    let mut phases = [0.0f64; 2];
    let mut sing = |bufs: &mut OwnedBuffers, blocks: usize| {
        for _ in 0..blocks {
            // A3 on input 1, E4 on input 2.
            for (c, key) in [57.0f64, 64.0].iter().enumerate() {
                let f = 440.0 * 2f64.powf((key - 69.0) / 12.0);
                for v in bufs.input_mut(c).iter_mut() {
                    phases[c] = (phases[c] + f / f64::from(SR)).fract();
                    *v = (0.3 * (std::f64::consts::TAU * phases[c]).sin()) as f32;
                }
            }
            rd.processor.process_device(bufs);
        }
    };
    sing(&mut bufs, 150);
    let ons = |rx: &mut rtrb::Consumer<faderframe_midi::MidiInputEvent>| {
        std::iter::from_fn(|| rx.pop().ok())
            .filter_map(|e| match e.event() {
                Some(MidiEvent::NoteOn { channel, key, .. }) => Some((e.port, channel, key)),
                _ => None,
            })
            .collect::<Vec<_>>()
    };
    assert_eq!(
        ons(&mut heard_a),
        [(3, 1, 57)],
        "input 1 on port 3, channel 2"
    );
    assert_eq!(
        ons(&mut heard_b),
        [(4, 2, 64)],
        "input 2 on port 4, channel 3"
    );
    // The second singer leaves: its note ends, the first sings on.
    rd.controller.remove_voice(1, 4).unwrap();
    sing(&mut bufs, 20);
    let offs: Vec<_> = std::iter::from_fn(|| heard_b.pop().ok())
        .filter_map(|e| match e.event() {
            Some(MidiEvent::NoteOff { channel, key, .. }) => Some((channel, key)),
            _ => None,
        })
        .collect();
    assert_eq!(offs, [(2, 64)]);
    assert!(
        std::iter::from_fn(|| heard_a.pop().ok())
            .all(|e| !matches!(e.event(), Some(MidiEvent::NoteOff { .. }))),
        "the first singer's note goes on"
    );
}
