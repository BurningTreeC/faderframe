#![allow(clippy::unwrap_used)]
//! A device input heard on the control side: every callback's samples of
//! the tapped channel, in order, stamped with when the callback started on
//! the heard clock's time base — while stopped too.

mod common;

use faderframe_audio::{AudioCallback, DeviceBuffers, OwnedBuffers, StreamInfo};
use faderframe_engine::EngineConfig;
use faderframe_project::Impact;

#[test]
fn the_tapped_input_comes_through_in_order_with_its_callbacks_stamps() {
    const SR: u32 = 48_000;
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
            buffer_size: 128,
            input_channels: 2,
            output_channels: 2,
            input_latency: 0,
            output_latency: 0,
        },
    );
    c.sync(&tp.project, &tp.sources, Impact::Graph).unwrap();
    let mut tap = c.tap_input(1, 1.0).unwrap();
    let mut bufs = OwnedBuffers::new(2, 2, 128);
    let mut fed = Vec::new();
    let mut before = Vec::new();
    let mut after = Vec::new();
    for k in 0..50u32 {
        for (i, s) in bufs.input_mut(1).iter_mut().enumerate() {
            *s = (k * 128 + i as u32) as f32;
        }
        bufs.input_mut(0).fill(-1.0);
        fed.extend_from_slice(bufs.input(1));
        before.push(faderframe_engine::history::now_ns());
        p.process_device(&mut bufs);
        after.push(faderframe_engine::history::now_ns());
    }
    let (samples, stamps) = tap.drain();
    assert_eq!(samples, fed, "channel 1, every sample, in order");
    assert_eq!(stamps.len(), 50);
    for (k, (first, ns)) in stamps.iter().enumerate() {
        assert_eq!(*first, k * 128);
        assert!(*ns >= before[k] && *ns <= after[k], "callback {k}'s stamp");
    }
    // A channel the device does not have: silence, still timed.
    let mut none = c.tap_input(5, 1.0).unwrap();
    p.process_device(&mut bufs);
    c.collect_garbage();
    let (samples, stamps) = none.drain();
    assert_eq!(samples, vec![0.0; 128]);
    assert_eq!(stamps.len(), 1);
    assert!(tap.is_finished(), "the replaced tap's ends went back");
}
