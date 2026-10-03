//! The engine renders the same audio on one thread and on a worker pool.

use faderframe_engine::offline::OfflineRenderer;
use faderframe_engine::{EngineConfig, render_generated_sources};
use faderframe_project::demo::demo_project;
use faderframe_realtime::{PoolConfig, WorkerPool};
use std::sync::Arc;

#[test]
fn demo_renders_bit_identically_in_parallel() {
    const SR: u32 = 48_000;
    let project = demo_project(SR);
    let sources = render_generated_sources(&project, SR);
    let config = EngineConfig {
        sample_rate: SR,
        max_block_size: 128,
        parallel_min_ns: 0,
        ..EngineConfig::default()
    };
    let render = |pool: Option<Arc<WorkerPool>>| {
        let mut r = OfflineRenderer::new(&project, &sources, config, 300, 2).unwrap();
        r.processor.set_worker_pool(pool);
        r.play_from(0).unwrap();
        // Ten seconds, including the loop wrap of the demo.
        r.render(SR as usize * 10)
    };
    let serial = render(None);
    let parallel = render(Some(Arc::new(WorkerPool::new(PoolConfig::new(4)))));
    assert!(serial[0].iter().any(|s| s.abs() > 1e-3), "audible");
    for (c, (a, b)) in serial.iter().zip(&parallel).enumerate() {
        assert_eq!(a.len(), b.len());
        let first = a
            .iter()
            .zip(b)
            .position(|(x, y)| x.to_bits() != y.to_bits());
        assert_eq!(first, None, "channel {c} differs");
    }
}
