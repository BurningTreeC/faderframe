//! Every standard sample rate × every common device buffer size must give
//! identical, sample-exact results (clip timing, loop wrap, transport
//! position). Large device buffers exercise the engine's internal chunking.

mod common;

use common::{TestProject, hits};
use faderframe_audio::STANDARD_SAMPLE_RATES;
use faderframe_core::ChannelLayout;
use faderframe_engine::EngineConfig;
use faderframe_engine::offline::OfflineRenderer;
use faderframe_project::{MusicalRange, TrackKind};
use faderframe_timeline::MusicalTime;

const BUFFER_SIZES: [usize; 9] = [32, 64, 128, 256, 512, 1024, 2048, 4096, 441];

#[test]
fn sample_rate_and_buffer_size_matrix() {
    for &sr in &STANDARD_SAMPLE_RATES {
        for &block in &BUFFER_SIZES {
            let mut tp = TestProject::new(sr);
            let track = tp.track(TrackKind::Audio, "Click", ChannelLayout::Stereo);
            let imp = tp.impulse(2, 32);
            // 120 BPM: quarter 1 = 0.5 s; loop 0..1.5 quarters = 0.75 s.
            tp.clip(track, imp, MusicalTime::QUARTER, 32);
            tp.project.loop_range =
                MusicalRange::new(MusicalTime::ZERO, MusicalTime::from_quarters(1.5));
            tp.project.loop_enabled = true;

            let config = EngineConfig {
                sample_rate: sr,
                ..EngineConfig::default()
            };
            let mut r = OfflineRenderer::new(&tp.project, &tp.sources, config, block, 2).unwrap();
            r.play_from(0).unwrap();
            let frames = (sr as f64 * 1.6) as usize;
            let out = r.render(frames);

            let click = sr as usize / 2;
            let loop_len = (sr as f64 * 0.75).round() as usize;
            let expected = vec![click, click + loop_len];
            assert_eq!(hits(&out[0], 1e-6), expected, "sr={sr} block={block}");
            assert_eq!(hits(&out[1], 1e-6), expected, "sr={sr} block={block}");
            assert_eq!(out[0][click], 1.0);

            // Transport position: total rendered (whole callbacks) minus one
            // loop length (one wrap happened).
            let rendered = frames.div_ceil(block) * block;
            let mut pos = rendered as i64;
            while pos >= loop_len as i64 {
                pos -= loop_len as i64;
            }
            assert_eq!(
                r.controller.transport_snapshot().position,
                pos,
                "sr={sr} block={block}"
            );
            let m = r.controller.metrics();
            assert_eq!(m.callbacks as usize, frames.div_ceil(block));
        }
    }
}
