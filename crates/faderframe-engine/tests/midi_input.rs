#![allow(clippy::unwrap_used)]
//! Live MIDI input: played through to instruments while the track is live,
//! filtered by port and channel, recorded inside the record window.

mod common;

use common::TestProject;
use faderframe_core::{ChannelLayout, builtin};
use faderframe_engine::EngineConfig;
use faderframe_engine::midi::{MidiFilter, MidiRecordTarget};
use faderframe_engine::offline::OfflineRenderer;
use faderframe_midi::{MidiEvent, midi_input_queue};
use faderframe_project::{Impact, InputRouting, PluginRef, PluginSlot, TrackKind};
use faderframe_transport::TransportCommand;
use std::collections::{HashMap, HashSet};

const SR: u32 = 48_000;
const BLOCK: usize = 256;

fn peak(r: &mut OfflineRenderer, blocks: usize) -> f32 {
    let mut p = 0.0f32;
    for _ in 0..blocks {
        let out = r.step();
        p = out.output_ref(0).iter().fold(p, |m, v| m.max(v.abs()));
    }
    p
}

fn synth_project(input: InputRouting) -> (TestProject, faderframe_core::TrackId) {
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
    let track = tp.project.track_mut(t).unwrap();
    track.instrument = Some(slot);
    track.input = input;
    (tp, t)
}

#[test]
fn live_input_plays_the_instrument_only_while_live() {
    let (tp, t) = synth_project(InputRouting::all_midi());
    let config = EngineConfig {
        sample_rate: SR,
        ..EngineConfig::default()
    };
    let mut r = OfflineRenderer::new(&tp.project, &tp.sources, config, BLOCK, 2).unwrap();
    let (tx, q, _feed) = midi_input_queue(64);
    r.controller.set_midi_input(q).unwrap();
    assert!(peak(&mut r, 4) < 1e-6, "silence before any note");

    // Not live: notes are ignored.
    tx.send(0, &[0x90, 60, 110]);
    assert!(peak(&mut r, 20) < 1e-6, "not live");

    // Live: the synth sounds.
    r.controller.set_midi_live(HashSet::from([t]));
    r.controller
        .sync(&tp.project, &tp.sources, Impact::Params)
        .unwrap();
    tx.send(0, &[0x90, 64, 110]);
    assert!(peak(&mut r, 20) > 0.01, "live note plays");

    // While live, the held note keeps sounding (sustain) …
    peak(&mut r, 400);
    assert!(peak(&mut r, 20) > 0.01, "held while live");
    // … going non-live releases it (no hanging notes).
    r.controller.set_midi_live(HashSet::new());
    r.controller
        .sync(&tp.project, &tp.sources, Impact::Params)
        .unwrap();
    peak(&mut r, 600);
    assert!(peak(&mut r, 20) < 1e-4, "released after going non-live");
    let (dropped, _) = r.controller.midi_counters();
    assert_eq!(dropped, 0);
}

#[test]
fn port_and_channel_filters_apply() {
    let (tp, t) = synth_project(InputRouting::Midi {
        port: Some("Keys:Keys 1".into()),
        channel: Some(9),
    });
    let config = EngineConfig {
        sample_rate: SR,
        ..EngineConfig::default()
    };
    let mut r = OfflineRenderer::new(&tp.project, &tp.sources, config, BLOCK, 2).unwrap();
    r.controller
        .set_midi_ports(HashMap::from([("Keys:Keys 1".to_string(), 3)]));
    r.controller.set_midi_live(HashSet::from([t]));
    r.controller
        .sync(&tp.project, &tp.sources, Impact::Graph)
        .unwrap();
    let (tx, q, _feed) = midi_input_queue(64);
    r.controller.set_midi_input(q).unwrap();
    peak(&mut r, 2);
    tx.send(0, &[0x99, 60, 110]); // wrong port
    tx.send(3, &[0x90, 60, 110]); // wrong channel
    assert!(peak(&mut r, 20) < 1e-6);
    tx.send(3, &[0x99, 62, 110]); // port 3, channel 10
    assert!(peak(&mut r, 20) > 0.01);
}

#[test]
fn recording_captures_armed_input_inside_the_window() {
    let (tp, t) = synth_project(InputRouting::all_midi());
    let config = EngineConfig {
        sample_rate: SR,
        ..EngineConfig::default()
    };
    let mut r = OfflineRenderer::new(&tp.project, &tp.sources, config, BLOCK, 2).unwrap();
    let (tx, q, _feed) = midi_input_queue(64);
    r.controller.set_midi_input(q).unwrap();
    let mut rx = r
        .controller
        .begin_midi_recording(
            vec![MidiRecordTarget {
                track: t,
                filter: MidiFilter {
                    port: None,
                    channel: None,
                },
            }],
            0,
            i64::MAX,
        )
        .unwrap();
    r.controller
        .transport(TransportCommand::SetRecording(true))
        .unwrap();
    r.play_from(0).unwrap();
    peak(&mut r, 4);
    tx.send(0, &[0x90, 60, 100]);
    peak(&mut r, 4);
    tx.send(0, &[0x80, 60, 0]);
    peak(&mut r, 2);
    let got: Vec<_> = std::iter::from_fn(|| rx.pop().ok()).collect();
    assert_eq!(got.len(), 2, "{got:?}");
    assert!(matches!(got[0].event, MidiEvent::NoteOn { key: 60, .. }));
    assert!(matches!(got[1].event, MidiEvent::NoteOff { key: 60, .. }));
    // Placed by arrival: inside the blocks that followed the send.
    assert!(got[0].position >= 4 * BLOCK as i64 && got[0].position < 5 * BLOCK as i64);
    assert!(got[1].position >= 8 * BLOCK as i64 && got[1].position < 9 * BLOCK as i64);
    r.controller.end_midi_recording().unwrap();
}

#[test]
fn auditioning_plays_the_track_even_when_not_live() {
    use faderframe_engine::midi::AUDITION_PORT;
    use std::sync::atomic::Ordering;
    let (tp, t) = synth_project(faderframe_project::InputRouting::None);
    let config = EngineConfig {
        sample_rate: SR,
        ..EngineConfig::default()
    };
    let mut r = OfflineRenderer::new(&tp.project, &tp.sources, config, BLOCK, 2).unwrap();
    let (tx, q, _feed) = midi_input_queue(64);
    r.controller.set_midi_input(q).unwrap();
    tx.send(AUDITION_PORT, &[0x90, 60, 110]);
    assert!(peak(&mut r, 20) < 1e-6, "not the auditioned track");
    r.controller
        .midi_shared()
        .audition_track
        .store(t.raw(), Ordering::Relaxed);
    tx.send(AUDITION_PORT, &[0x90, 64, 110]);
    assert!(peak(&mut r, 20) > 0.01, "audition sounds");
}

#[test]
fn mapped_controls_do_not_reach_the_instrument() {
    use faderframe_engine::midi::ConsumedControl;
    let (tp, t) = synth_project(InputRouting::all_midi());
    let config = EngineConfig {
        sample_rate: SR,
        ..EngineConfig::default()
    };
    let mut r = OfflineRenderer::new(&tp.project, &tp.sources, config, BLOCK, 2).unwrap();
    let (tx, q, _feed) = midi_input_queue(64);
    r.controller.set_midi_input(q).unwrap();
    r.controller.set_midi_live(HashSet::from([t]));
    r.controller
        .sync(&tp.project, &tp.sources, Impact::Params)
        .unwrap();
    // A pad (note 36 on channel 10) mapped to something: it must not play.
    r.controller
        .midi_shared()
        .consumed
        .set(&[(None, 9, ConsumedControl::Note(36))]);
    tx.send(5, &[0x99, 36, 120]);
    assert!(peak(&mut r, 20) < 1e-6, "consumed");
    tx.send(5, &[0x99, 38, 120]);
    assert!(peak(&mut r, 20) > 0.01, "other pads still play");
}

#[test]
fn midi_tracks_play_external_devices_in_time() {
    use faderframe_project::{MidiOutputRouting, TrackKind};
    let mut tp = TestProject::new(SR);
    let t = tp.track(TrackKind::Midi, "Ext", ChannelLayout::Stereo);
    tp.project.track_mut(t).unwrap().midi_output = Some(MidiOutputRouting {
        port: "Synth:Synth In".into(),
        channel: Some(3),
    });
    let config = EngineConfig {
        sample_rate: SR,
        ..EngineConfig::default()
    };
    let mut r = OfflineRenderer::new(&tp.project, &tp.sources, config, BLOCK, 2).unwrap();
    r.controller
        .set_midi_output_ports(HashMap::from([("Synth:Synth In".to_string(), 2)]));
    r.controller.set_midi_live(HashSet::from([t]));
    r.controller
        .sync(&tp.project, &tp.sources, Impact::Graph)
        .unwrap();
    let (tx, q, _feed) = midi_input_queue(64);
    let clock = tx.clock();
    r.controller.set_midi_input(q).unwrap();
    let (oq, mut orx) = faderframe_midi::midi_output_queue(256, clock);
    r.controller.set_midi_output(oq).unwrap();
    // Clock on port 2 too.
    r.controller
        .midi_shared()
        .clock_ports
        .store(1 << 2, std::sync::atomic::Ordering::Relaxed);
    peak(&mut r, 2);
    tx.send(0, &[0x90, 60, 100]);
    let before = clock.now_ns();
    peak(&mut r, 2);
    let got: Vec<_> = std::iter::from_fn(|| orx.pop().ok()).collect();
    let note = got.iter().find(|e| e.bytes()[0] & 0xF0 == 0x90).unwrap();
    assert_eq!(note.port, 2);
    assert_eq!(note.bytes(), &[0x93, 60, 100], "remapped to channel 4");
    assert!(note.due_ns >= before, "scheduled ahead (latency)");
    // Playing: MIDI clock goes out.
    r.play_from(0).unwrap();
    peak(&mut r, 20);
    let clock_msgs: Vec<_> = std::iter::from_fn(|| orx.pop().ok())
        .filter(|e| e.port == 2)
        .collect();
    assert!(clock_msgs.iter().any(|e| e.bytes() == [0xFA]), "start");
    let pulses = clock_msgs.iter().filter(|e| e.bytes() == [0xF8]).count();
    // 20 blocks of 256 frames at 120 BPM ≈ 0.107 s ≈ 5 pulses (24/quarter).
    assert!((4..=6).contains(&pulses), "{pulses} pulses");
    // Start comes before the first pulse. (Due times follow the wall clock
    // of each callback; offline rendering runs faster than real time, so
    // they are only ordered within a callback here.)
    let start = clock_msgs.iter().position(|e| e.bytes() == [0xFA]).unwrap();
    let first_pulse = clock_msgs.iter().position(|e| e.bytes() == [0xF8]).unwrap();
    assert!(start < first_pulse);
}
