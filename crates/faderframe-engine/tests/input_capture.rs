#![allow(clippy::unwrap_used)]
//! Capture Audio's audio-thread side: the kept input channels of every
//! chunk processed, playing or stopped, stamped with the engine's frame
//! counter, the timeline position while playing (a loop wrap shows as a
//! jump back) and whether the recorder took the chunk; a full ring loses
//! blocks in place, never waits.

mod common;

use faderframe_audio::{AudioCallback, DeviceBuffers, OwnedBuffers, StreamInfo};
use faderframe_engine::input_capture::{CaptureBlock, CaptureStreams};
use faderframe_engine::{EngineConfig, EngineController, EngineProcessor, RecordTarget};
use faderframe_project::Impact;
use faderframe_transport::{LoopRange, TransportCommand};

const SR: u32 = 48_000;
const BLOCK: usize = 128;

fn engine() -> (EngineController, EngineProcessor) {
    let tp = common::TestProject::new(SR);
    let (mut c, mut p) = faderframe_engine::create(EngineConfig {
        sample_rate: SR,
        ..EngineConfig::default()
    });
    AudioCallback::prepare(
        &mut p,
        &StreamInfo {
            backend: "test",
            device: "two".into(),
            sample_rate: SR,
            buffer_size: BLOCK as u32,
            input_channels: 2,
            output_channels: 2,
            input_latency: 0,
            output_latency: 0,
        },
    );
    c.sync(&tp.project, &tp.sources, Impact::Graph).unwrap();
    (c, p)
}

/// Callbacks with input channel 1 counting up (channel 0 negative).
fn feed(p: &mut EngineProcessor, bufs: &mut OwnedBuffers, from: u32, callbacks: u32) -> Vec<f32> {
    let mut fed = Vec::new();
    for k in from..from + callbacks {
        for (i, s) in bufs.input_mut(1).iter_mut().enumerate() {
            *s = (k * BLOCK as u32 + i as u32) as f32;
        }
        bufs.input_mut(0).fill(-1.0);
        fed.extend_from_slice(bufs.input(1));
        p.process_device(bufs);
    }
    fed
}

/// Every header and each channel's samples.
fn drain(cap: &mut CaptureStreams) -> (Vec<CaptureBlock>, Vec<Vec<f32>>) {
    let n = cap.channels.len();
    let mut headers = Vec::new();
    let mut channels = vec![Vec::new(); n];
    while let Ok(h) = cap.headers.pop() {
        if !h.lost {
            for ch in &mut channels {
                for _ in 0..h.frames {
                    ch.push(cap.data.pop().unwrap());
                }
            }
        }
        headers.push(h);
    }
    (headers, channels)
}

#[test]
fn the_inputs_are_kept_for_every_chunk_playing_or_stopped() {
    let (mut c, mut p) = engine();
    // Channel 5 is not on the device: silence.
    let mut cap = c.capture_inputs(vec![1, 5], 1.0).unwrap();
    let mut bufs = OwnedBuffers::new(2, 2, BLOCK);
    let mut fed = feed(&mut p, &mut bufs, 0, 3);
    // Playing round a loop of 300 frames from 1 000.
    c.transport(TransportCommand::SetLoopRange(LoopRange::new(1_000, 1_300)))
        .unwrap();
    c.transport(TransportCommand::SetLoopEnabled(true)).unwrap();
    c.transport(TransportCommand::Locate(1_000)).unwrap();
    c.transport(TransportCommand::Play).unwrap();
    fed.extend(feed(&mut p, &mut bufs, 3, 6));
    c.transport(TransportCommand::Stop).unwrap();
    fed.extend(feed(&mut p, &mut bufs, 9, 2));
    let (headers, channels) = drain(&mut cap);
    assert_eq!(channels[0], fed, "channel 1, every sample in order");
    assert!(channels[1].iter().all(|s| *s == 0.0));
    assert_eq!(channels[1].len(), fed.len());
    // Frames follow each other on the engine's counter.
    let mut next = headers[0].frame;
    for h in &headers {
        assert_eq!(h.frame, next, "{headers:?}");
        assert!(!h.lost && !h.recording);
        next += u64::from(h.frames);
    }
    assert_eq!(next - headers[0].frame, 11 * BLOCK as u64);
    // Stopped, then playing from 1 000 with the wraps at 1 300, then
    // stopped again.
    let positions: Vec<Option<i64>> = headers.iter().map(|h| h.position).collect();
    assert!(positions[..3].iter().all(Option::is_none), "{positions:?}");
    assert!(positions.last().unwrap().is_none());
    let playing: Vec<&CaptureBlock> = headers.iter().filter(|h| h.position.is_some()).collect();
    assert_eq!(playing[0].position, Some(1_000));
    let mut wraps = 0;
    for w in playing.windows(2) {
        let (a, b) = (w[0], w[1]);
        let end = a.position.unwrap() + i64::from(a.frames);
        if b.position == Some(1_000) && end == 1_300 {
            wraps += 1;
        } else {
            assert_eq!(b.position, Some(end), "{a:?} {b:?}");
        }
        assert!(b.position.unwrap() < 1_300);
    }
    assert_eq!(wraps, 2, "six callbacks of 128 round 300 frames");
    // Ended: the engine lets go of its ends.
    c.end_input_capture().unwrap();
    p.process_device(&mut bufs);
    c.collect_garbage();
    assert!(cap.is_finished());
}

#[test]
fn a_full_ring_loses_blocks_in_place() {
    let (mut c, mut p) = engine();
    // A quarter of a second of one channel; nobody reads.
    let mut cap = c.capture_inputs(vec![1], 0.25).unwrap();
    let mut bufs = OwnedBuffers::new(2, 2, BLOCK);
    let fed = feed(&mut p, &mut bufs, 0, 200);
    let (headers, channels) = drain(&mut cap);
    assert_eq!(headers.len(), 200, "every block has its header");
    let kept = headers.iter().take_while(|h| !h.lost).count();
    assert!(kept > 0 && kept < 200);
    assert!(headers[kept..].iter().all(|h| h.lost));
    assert_eq!(channels[0], fed[..kept * BLOCK]);
    for (k, h) in headers.iter().enumerate() {
        assert_eq!(h.frame, headers[0].frame + (k * BLOCK) as u64);
    }
}

#[test]
fn chunks_the_recorder_took_are_marked() {
    let (mut c, mut p) = engine();
    let mut cap = c.capture_inputs(vec![0], 1.0).unwrap();
    let _rec = c
        .begin_recording(
            vec![RecordTarget {
                track: faderframe_core::TrackId(1),
                first_channel: 0,
                channels: 1,
            }],
            256,
            768,
            1.0,
        )
        .unwrap();
    c.transport(TransportCommand::SetRecording(true)).unwrap();
    c.transport(TransportCommand::Play).unwrap();
    let mut bufs = OwnedBuffers::new(2, 2, BLOCK);
    feed(&mut p, &mut bufs, 0, 8);
    let (headers, _) = drain(&mut cap);
    // Only the chunks with frames inside the record window (256..768).
    let marked: Vec<i64> = headers
        .iter()
        .filter(|h| h.recording)
        .filter_map(|h| h.position)
        .collect();
    assert_eq!(marked, [256, 384, 512, 640]);
}
