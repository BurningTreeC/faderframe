#![allow(clippy::unwrap_used)]
//! Pitch Correct on an audio track: a MIDI track playing the track gives
//! it its target notes (devices that take notes on audio tracks get the
//! notes of the MIDI tracks playing them), and its latency is
//! compensated like any other device's.

mod common;

use common::TestProject;
use faderframe_audio_files::AudioData;
use faderframe_core::{ChannelLayout, ParameterId, TrackId, builtin};
use faderframe_engine::EngineConfig;
use faderframe_engine::offline::OfflineRenderer;
use faderframe_plugin_host::devices::pitch_correct::{id, target};
use faderframe_project::{
    Clip, ClipContent, MidiClip, MidiNote, OutputRouting, PluginRef, PluginSlot, SavedParameter,
    TrackColor, TrackKind,
};
use faderframe_timeline::MusicalTime;

const SR: u32 = 48_000;

fn hz(note: f64) -> f64 {
    440.0 * ((note - 69.0) / 12.0).exp2()
}

/// A vocal track singing `note` (a sine) through Pitch Correct set to
/// follow MIDI notes at once.
fn vocal(tp: &mut TestProject, note: f64, seconds: f64) -> TrackId {
    let t = tp.track(TrackKind::Audio, "Vocal", ChannelLayout::Mono);
    let f = hz(note);
    let frames = (seconds * f64::from(SR)) as usize;
    let x: Vec<f32> = (0..frames)
        .map(|i| (0.4 * (std::f64::consts::TAU * f * i as f64 / f64::from(SR)).sin()) as f32)
        .collect();
    let src = tp.source(AudioData::from_channels(SR, vec![x]));
    tp.clip(t, src, MusicalTime::ZERO, frames as i64);
    let slot = PluginSlot {
        id: tp.project.ids.allocate(),
        plugin: PluginRef::builtin(builtin::PITCH_CORRECT, "Pitch Correct"),
        bypass: false,
        parameters: [
            (id::TARGET, target::MIDI as f64),
            (id::SPEED, 0.0),
            (id::VIBRATO, 0.0),
        ]
        .iter()
        .map(|(id, value)| SavedParameter {
            id: ParameterId(*id),
            value: *value,
        })
        .collect(),
        state: None,
        sidechain: None,
        mono_to_stereo: false,
        mix_origin: None,
    };
    tp.project.track_mut(t).unwrap().inserts = vec![slot];
    t
}

/// A MIDI track holding `key` from the start, playing `track`.
fn melody(tp: &mut TestProject, track: TrackId, key: u8) {
    let m = tp.track(TrackKind::Midi, "Melody", ChannelLayout::Stereo);
    tp.project.track_mut(m).unwrap().output = OutputRouting::Track { track };
    let note = MidiNote {
        fine: Default::default(),
        id: tp.project.ids.allocate(),
        start: MusicalTime::ZERO,
        length: MusicalTime::from_quarters(8.0),
        key,
        velocity: 100,
        channel: 0,
        muted: false,
        release: None,
        time_locked: false,
        articulation: None,
    };
    let id = tp.project.ids.allocate();
    tp.project.clips.insert(
        id,
        Clip {
            id,
            track: m,
            name: "Melody".into(),
            color: Some(TrackColor::palette(1)),
            start: MusicalTime::ZERO,
            muted: false,
            content: ClipContent::Midi(MidiClip {
                length: MusicalTime::from_quarters_i(8),
                notes: vec![note],
                controllers: Vec::new(),
                expressions: Vec::new(),
                sysex: Vec::new(),
                articulations: Vec::new(),
            }),
        },
    );
    tp.project.track_mut(m).unwrap().clips.push(id);
}

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

/// The note (MIDI, fractional) of a steady tone from its rising zero
/// crossings.
fn note_of(x: &[f32]) -> f64 {
    let ups: Vec<f64> = x
        .windows(2)
        .enumerate()
        .filter(|(_, w)| w[0] < 0.0 && w[1] >= 0.0)
        .map(|(i, w)| i as f64 + f64::from(-w[0] / (w[1] - w[0])))
        .collect();
    let period = (ups[ups.len() - 1] - ups[0]) / (ups.len() - 1) as f64;
    69.0 + 12.0 * (f64::from(SR) / period / 440.0).log2()
}

#[test]
fn a_midi_track_playing_an_audio_track_gives_pitch_correct_its_notes() {
    // Alone: no notes, so the voice is left as it is.
    let mut tp = TestProject::new(SR);
    vocal(&mut tp, 66.6, 1.0);
    let x = render(&tp, 40_000);
    assert!((note_of(&x[16_000..40_000]) - 66.6).abs() < 0.03);
    // A MIDI track holding G4 pulls it there.
    let mut tp = TestProject::new(SR);
    let t = vocal(&mut tp, 66.6, 1.0);
    melody(&mut tp, t, 67);
    let x = render(&tp, 40_000);
    let got = note_of(&x[16_000..40_000]);
    assert!((got - 67.0).abs() < 0.03, "{got}");
}
