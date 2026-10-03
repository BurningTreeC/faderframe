#![allow(clippy::unwrap_used)]
//! Input capture on the audio thread: window gating, loop passes,
//! overruns, no allocation, and the metronome.

mod common;

use common::TestProject;
use faderframe_audio::{DeviceBuffers, OwnedBuffers};
use faderframe_core::{ChannelLayout, TrackId};
use faderframe_engine::offline::OfflineRenderer;
use faderframe_engine::{EngineConfig, MetronomeMode, RecordBlock, RecordStreams, RecordTarget};
use faderframe_project::TrackKind;
use faderframe_transport::{LoopRange, TransportCommand};

const SR: u32 = 48_000;
const BLOCK: usize = 100;

fn renderer() -> (OfflineRenderer, TrackId) {
    let mut tp = TestProject::new(SR);
    let t = tp.track(TrackKind::Audio, "Vox", ChannelLayout::Stereo);
    let config = EngineConfig {
        sample_rate: SR,
        ..EngineConfig::default()
    };
    (
        OfflineRenderer::new(&tp.project, &tp.sources, config, BLOCK, 2).unwrap(),
        t,
    )
}

/// Drive `blocks` callbacks; input channel c carries `device_frame * 10 + c`.
fn run(r: &mut OfflineRenderer, bufs: &mut OwnedBuffers, blocks: usize, device_frame: &mut i64) {
    for _ in 0..blocks {
        bufs.set_frames(BLOCK);
        for c in 0..2 {
            for (i, s) in bufs.input_mut(c).iter_mut().enumerate() {
                *s = ((*device_frame + i as i64) * 10 + c as i64) as f32;
            }
        }
        r.processor.process_device(bufs);
        r.controller.collect_garbage();
        *device_frame += BLOCK as i64;
    }
}

fn drain(s: &mut RecordStreams) -> Vec<(RecordBlock, Vec<f32>)> {
    let ch = s.channels();
    let mut out = Vec::new();
    while let Ok(h) = s.headers.pop() {
        let mut data = vec![0.0; h.frames as usize * ch];
        s.data.pop_entire_slice(&mut data).unwrap();
        out.push((h, data));
    }
    out
}

fn target(track: TrackId) -> Vec<RecordTarget> {
    vec![RecordTarget {
        track,
        first_channel: 0,
        channels: 2,
    }]
}

#[test]
fn captures_exactly_the_record_window() {
    let (mut r, t) = renderer();
    let mut s = r
        .controller
        .begin_recording(target(t), 1_234, 5_678, 2.0)
        .unwrap();
    r.controller
        .transport(TransportCommand::SetRecording(true))
        .unwrap();
    r.play_from(0).unwrap();
    let mut bufs = OwnedBuffers::new(2, 2, BLOCK);
    let mut frame = 0;
    run(&mut r, &mut bufs, 80, &mut frame);
    let blocks = drain(&mut s);
    assert_eq!(blocks.first().unwrap().0.position, 1_234);
    let total: u32 = blocks.iter().map(|(h, _)| h.frames).sum();
    assert_eq!(total, 5_678 - 1_234);
    let mut expect = 1_234i64;
    for (h, data) in &blocks {
        assert_eq!(h.position, expect, "contiguous");
        assert_eq!(h.pass, 0);
        let n = h.frames as usize;
        // Timeline position == device frame here (play started at 0).
        assert_eq!(data[0], (h.position * 10) as f32, "channel 0 first sample");
        assert_eq!(
            data[n],
            (h.position * 10 + 1) as f32,
            "channel 1 follows channel 0"
        );
        expect += n as i64;
    }
    // Ending recording lets the audio thread drop its ends.
    r.controller.end_recording().unwrap();
    run(&mut r, &mut bufs, 1, &mut frame);
    assert!(s.is_finished());
    assert_eq!(r.controller.record_counters().1, 5_678 - 1_234);
}

#[test]
fn loop_wraps_start_new_passes_and_nothing_is_captured_unless_recording() {
    let (mut r, t) = renderer();
    let mut s = r
        .controller
        .begin_recording(target(t), 0, i64::MAX, 2.0)
        .unwrap();
    r.controller
        .transport(TransportCommand::SetLoopRange(LoopRange::new(500, 1_500)))
        .unwrap();
    r.controller
        .transport(TransportCommand::SetLoopEnabled(true))
        .unwrap();
    let mut bufs = OwnedBuffers::new(2, 2, BLOCK);
    let mut frame = 0;
    // Playing but not recording: nothing.
    r.play_from(500).unwrap();
    run(&mut r, &mut bufs, 5, &mut frame);
    assert!(drain(&mut s).is_empty());
    r.controller
        .transport(TransportCommand::Locate(500))
        .unwrap();
    r.controller
        .transport(TransportCommand::SetRecording(true))
        .unwrap();
    run(&mut r, &mut bufs, 35, &mut frame); // 3500 frames = 3.5 passes
    let blocks = drain(&mut s);
    let passes: Vec<u32> = blocks.iter().map(|(h, _)| h.pass).collect();
    assert_eq!(*passes.last().unwrap() - passes[0], 3, "{passes:?}");
    for p in passes[0]..passes[0] + 3 {
        let frames: u32 = blocks
            .iter()
            .filter(|(h, _)| h.pass == p)
            .map(|(h, _)| h.frames)
            .sum();
        assert_eq!(frames, 1_000, "pass {p} covers the loop exactly");
        let first = blocks.iter().find(|(h, _)| h.pass == p).unwrap();
        assert_eq!(first.0.position, 500);
    }
}

#[test]
fn full_rings_count_overruns_instead_of_blocking() {
    let (mut r, t) = renderer();
    // 0.5 s minimum capacity; never drained.
    let s = r
        .controller
        .begin_recording(target(t), 0, i64::MAX, 0.0)
        .unwrap();
    r.controller
        .transport(TransportCommand::SetRecording(true))
        .unwrap();
    r.play_from(0).unwrap();
    let mut bufs = OwnedBuffers::new(2, 2, BLOCK);
    let mut frame = 0;
    run(&mut r, &mut bufs, 400, &mut frame); // 40 000 frames > 24 000 capacity
    let (overruns, captured) = r.controller.record_counters();
    assert_eq!(captured, 24_000);
    assert_eq!(overruns, 16_000);
    drop(s);
}

#[test]
fn metronome_clicks_on_beats_only_when_enabled() {
    let (mut r, _) = renderer();
    let mut bufs = OwnedBuffers::new(2, 2, BLOCK);
    let mut frame = 0;
    r.play_from(0).unwrap();
    run(&mut r, &mut bufs, 10, &mut frame);
    assert!(
        bufs.output_ref(0).iter().all(|v| *v == 0.0),
        "off by default"
    );
    r.controller.metronome().set_mode(MetronomeMode::Always);
    // 120 bpm at 48 kHz: one beat every 24 000 samples. Collect a second.
    r.controller.transport(TransportCommand::Locate(0)).unwrap();
    let mut onsets = Vec::new();
    let mut silent_run = 1_000;
    for b in 0..480 {
        bufs.set_frames(BLOCK);
        r.processor.process_device(&mut bufs);
        for (i, v) in bufs.output_ref(0).iter().enumerate() {
            if v.abs() > 1e-4 {
                if silent_run > 500 {
                    onsets.push(b * BLOCK + i);
                }
                silent_run = 0;
            } else {
                silent_run += 1;
            }
        }
    }
    // Each click starts at its beat with sin(0) = 0, so the first audible
    // sample is one later.
    assert_eq!(onsets, vec![1, 24_001], "clicks on beats 1 and 2");
    r.controller.metronome().set_mode(MetronomeMode::Recording);
    run(&mut r, &mut bufs, 300, &mut frame);
    assert!(
        bufs.output_ref(0).iter().all(|v| *v == 0.0),
        "recording-only mode is silent here"
    );
    assert_eq!(bufs.output_channels(), 2);
}
