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
