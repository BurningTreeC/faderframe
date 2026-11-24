#![allow(clippy::unwrap_used)]
//! The media browser's previews: a file heard beside the mix (at once, or
//! from a bar in step with the transport, looped in phase), at its volume,
//! fading only where it must; a MIDI file played into the auditioned
//! instrument, its notes ended when it stops.

mod common;

use common::TestProject;
use faderframe_audio::{DeviceBuffers, OwnedBuffers};
use faderframe_core::{ChannelLayout, builtin};
use faderframe_engine::EngineConfig;
use faderframe_engine::midi_preview::MidiPreview;
use faderframe_engine::nodes::{PreviewAudio, PreviewSound, PreviewStart};
use faderframe_engine::offline::OfflineRenderer;
use faderframe_project::{PluginRef, PluginSlot, TrackKind};
use faderframe_timeline::MusicalTime;
use faderframe_transport::{LoopRange, TransportCommand};
use std::sync::Arc;
use std::sync::atomic::Ordering;

const RATE: u32 = 48_000;
const BLOCK: usize = 256;
/// A preview frame's value: exact in f32.
const SCALE: f32 = 1.0 / 1_048_576.0;
const MIX: f32 = 0.25;

/// A preview whose left channel counts its frames from 1 (the right one
/// negated), so a frame read is known by its value.
fn counting(frames: usize) -> Arc<PreviewAudio> {
    let l: Vec<f32> = (0..frames).map(|i| (i + 1) as f32 * SCALE).collect();
    let r: Vec<f32> = l.iter().map(|v| -v).collect();
    PreviewAudio::from_planes(RATE, &[l, r])
}

fn frame_of(v: f32) -> i64 {
    ((v - MIX) / SCALE).round() as i64 - 1
}

/// A project whose mix is a steady `MIX` on both sides.
fn renderer() -> OfflineRenderer {
    let mut p = TestProject::new(RATE);
    let t = p.track(TrackKind::Audio, "A", ChannelLayout::Stereo);
    let dc = p.dc(2, MIX, RATE as usize * 20);
    p.clip(t, dc, MusicalTime::ZERO, RATE as i64 * 20);
    let config = EngineConfig {
        sample_rate: RATE,
        ..EngineConfig::default()
    };
    OfflineRenderer::new(&p.project, &p.sources, config, BLOCK, 2).unwrap()
}

fn block(r: &mut OfflineRenderer, bufs: &mut OwnedBuffers) -> (Vec<f32>, Vec<f32>) {
    r.processor.process_device(bufs);
    r.controller.collect_garbage();
    (bufs.output(0).to_vec(), bufs.output(1).to_vec())
}

#[test]
fn a_preview_plays_beside_the_mix_at_once_and_stops_with_a_fade() {
    let mut r = renderer();
    let mut bufs = OwnedBuffers::new(2, 2, BLOCK);
    let link = Arc::clone(r.controller.listening().link.preview());
    let shared = Arc::clone(link.shared());
    r.play_from(0).unwrap();
    for _ in 0..4 {
        block(&mut r, &mut bufs);
    }
    let total = BLOCK * 12;
    let generation = link.set(Some(PreviewSound {
        audio: counting(total),
        start: PreviewStart::Now,
        looped: false,
        loop_frames: 0,
    }));
    // From its first frame, unfaded (a drum's attack is kept), the mix
    // beside it.
    let (l, rr) = block(&mut r, &mut bufs);
    assert_eq!(shared.installed(), generation);
    for (i, (a, b)) in l.iter().zip(&rr).enumerate() {
        assert_eq!(frame_of(*a), i as i64, "frame {i}");
        assert!((a + b - 2.0 * MIX).abs() < 1e-6, "the right side negated");
    }
    // The volume ramps over a block to where it is set.
    shared.set_gain(0.5);
    let (l, _) = block(&mut r, &mut bufs);
    let last = (l[BLOCK - 1] - MIX) / ((2 * BLOCK) as f32 * SCALE);
    assert!((last - 0.5).abs() < 0.01, "{last}");
    shared.set_gain(1.0);
    // Played out: the mix alone, the sound noted as ended.
    for _ in 0..12 {
        block(&mut r, &mut bufs);
    }
    let (l, _) = block(&mut r, &mut bufs);
    assert!(l.iter().all(|v| (*v - MIX).abs() < 1e-6));
    assert!(shared.has_ended(generation));

    // Another, stopped halfway: it fades out over 5 ms, no jump.
    let generation = link.set(Some(PreviewSound {
        audio: counting(RATE as usize),
        start: PreviewStart::Now,
        looped: true,
        loop_frames: 0,
    }));
    for _ in 0..4 {
        block(&mut r, &mut bufs);
    }
    assert!(!shared.has_ended(generation));
    let stop = link.set(None);
    let fade = (f64::from(RATE) * faderframe_engine::nodes::file_preview::FADE_SECONDS) as usize;
    let mut out = Vec::new();
    while out.len() < fade + BLOCK {
        out.extend(block(&mut r, &mut bufs).0);
    }
    for w in out.windows(2) {
        assert!((w[1] - w[0]).abs() < 0.01, "a jump: {w:?}");
    }
    assert!(out[fade + 8..].iter().all(|v| (*v - MIX).abs() < 1e-6));
    assert!(shared.has_ended(generation) && shared.has_ended(stop));
    link.collect();
}

#[test]
fn a_preview_from_the_next_bar_follows_the_transport_in_phase() {
    let mut r = renderer();
    let mut bufs = OwnedBuffers::new(2, 2, BLOCK);
    let link = Arc::clone(r.controller.listening().link.preview());
    let shared = Arc::clone(link.shared());
    r.play_from(0).unwrap();
    for _ in 0..2 {
        block(&mut r, &mut bufs);
    }
    // Starts at a frame inside the fourth block, loops every 1000 frames.
    let at = (3 * BLOCK + 100) as i64;
    let generation = link.set(Some(PreviewSound {
        audio: counting(4000),
        start: PreviewStart::At(at),
        looped: true,
        loop_frames: 1000,
    }));
    let mut out = Vec::new();
    for _ in 0..30 {
        out.extend(block(&mut r, &mut bufs).0);
    }
    let start = 2 * BLOCK;
    for (i, v) in out.iter().enumerate() {
        let pos = (start + i) as i64;
        if pos < at {
            assert!((v - MIX).abs() < 1e-6, "silent before its start: {pos}");
        } else {
            assert_eq!(frame_of(*v), (pos - at) % 1000, "at {pos}");
        }
    }
    // A locate: still in phase with where it started.
    r.controller
        .transport(TransportCommand::Locate(at + 2500))
        .unwrap();
    let (l, _) = block(&mut r, &mut bufs);
    let now = r.controller.transport_snapshot().position - BLOCK as i64;
    for (i, v) in l.iter().enumerate() {
        assert_eq!(frame_of(*v), (now + i as i64 - at) % 1000);
    }
    // A loop wrap keeps it in phase too.
    r.controller
        .transport(TransportCommand::SetLoopRange(LoopRange::new(
            at,
            at + 3000,
        )))
        .unwrap();
    r.controller
        .transport(TransportCommand::SetLoopEnabled(true))
        .unwrap();
    for _ in 0..40 {
        let (l, _) = block(&mut r, &mut bufs);
        assert!(l.iter().all(|v| frame_of(*v) >= 0 && frame_of(*v) < 1000));
    }
    // A stop ends it.
    r.controller.transport(TransportCommand::Stop).unwrap();
    for _ in 0..4 {
        block(&mut r, &mut bufs);
    }
    assert!(shared.has_ended(generation));
    assert_eq!(shared.position(), None);
}

#[test]
fn a_preview_still_being_decoded_waits_at_its_edge() {
    let mut r = renderer();
    let mut bufs = OwnedBuffers::new(2, 2, BLOCK);
    let link = Arc::clone(r.controller.listening().link.preview());
    r.play_from(0).unwrap();
    let (audio, mut writer) = PreviewAudio::new(RATE, 1, RATE as usize);
    let first: Vec<f32> = (0..BLOCK / 2).map(|i| (i + 1) as f32 * SCALE).collect();
    writer.push(&[&first], first.len());
    link.set(Some(PreviewSound {
        audio: Arc::clone(&audio),
        start: PreviewStart::Now,
        looped: false,
        loop_frames: 0,
    }));
    // Nothing published yet (a partial chunk): it waits, it does not skip.
    let (l, _) = block(&mut r, &mut bufs);
    assert!(l.iter().all(|v| (*v - MIX).abs() < 1e-6));
    let rest: Vec<f32> = (BLOCK / 2..BLOCK * 2)
        .map(|i| (i + 1) as f32 * SCALE)
        .collect();
    writer.push(&[&rest], rest.len());
    writer.finish();
    // Then from its first frame (a mono preview on both sides).
    let (l, rr) = block(&mut r, &mut bufs);
    for (i, (a, b)) in l.iter().zip(&rr).enumerate() {
        assert_eq!(frame_of(*a), i as i64);
        assert_eq!(a, b);
    }
}

#[test]
fn a_midi_file_plays_the_auditioned_instrument_until_stopped() {
    let mut tp = TestProject::new(RATE);
    let t = tp.track(TrackKind::Instrument, "Keys", ChannelLayout::Stereo);
    let slot = PluginSlot {
        id: tp.project.ids.allocate(),
        plugin: PluginRef::builtin(builtin::SYNTH, "Synth"),
        bypass: false,
        parameters: Vec::new(),
        state: None,
        sidechain: None,
        mono_to_stereo: false,
        mix_origin: None,
    };
    tp.project.track_mut(t).unwrap().instrument = Some(slot);
    let config = EngineConfig {
        sample_rate: RATE,
        ..EngineConfig::default()
    };
    let mut r = OfflineRenderer::new(&tp.project, &tp.sources, config, BLOCK, 2).unwrap();
    let peak = |r: &mut OfflineRenderer, blocks: usize| {
        let mut p = 0.0f32;
        for _ in 0..blocks {
            let out = r.step();
            p = out.output_ref(0).iter().fold(p, |m, v| m.max(v.abs()));
        }
        p
    };
    assert!(peak(&mut r, 4) < 1e-6);
    // A held chord: nothing plays while no track is auditioned.
    let chord = MidiPreview {
        events: vec![
            (0, [0x90, 60, 100]),
            (0, [0x90, 64, 100]),
            (i64::from(RATE) * 10, [0x80, 60, 0]),
            (i64::from(RATE) * 10, [0x80, 64, 0]),
        ],
        length: i64::from(RATE) * 10,
        looped: false,
        start: Some(PreviewStart::Now),
    };
    let status = r
        .controller
        .set_midi_preview(Some(chord.clone()))
        .unwrap()
        .unwrap();
    assert!(peak(&mut r, 20) < 1e-6, "not the auditioned track");
    r.controller.set_midi_preview(None).unwrap();
    r.controller
        .midi_shared()
        .audition_track
        .store(t.raw(), Ordering::Relaxed);
    let status2 = r.controller.set_midi_preview(Some(chord)).unwrap().unwrap();
    assert!(peak(&mut r, 20) > 0.01, "the chord sounds");
    assert!(status2.position().is_some_and(|p| p > 0));
    assert!(!status2.has_ended());
    // Stopped: the notes end (the synth's release dies away).
    r.controller.set_midi_preview(None).unwrap();
    peak(&mut r, (RATE as usize * 3) / BLOCK);
    assert!(peak(&mut r, 8) < 1e-4, "released");
    drop(status);
}

#[test]
fn a_one_shot_midi_preview_ends_with_its_length() {
    let mut r = renderer();
    let status = r
        .controller
        .set_midi_preview(Some(MidiPreview {
            events: vec![(10, [0x90, 60, 100]), (100, [0x80, 60, 0])],
            length: (BLOCK * 3) as i64,
            looped: false,
            start: Some(PreviewStart::Now),
        }))
        .unwrap()
        .unwrap();
    for _ in 0..2 {
        r.step();
    }
    assert!(!status.has_ended());
    for _ in 0..3 {
        r.step();
    }
    assert!(status.has_ended());
}
