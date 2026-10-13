//! How long each built-in's `reset()` takes after it has played (a panic
//! resets every device in one go, on the audio thread): every built-in at
//! 192 kHz, the EQ at its heaviest. Diagnostic; run with
//! `cargo test -p faderframe-plugin-host --release --test reset_cost -- --ignored --nocapture`.

#![allow(clippy::unwrap_used)]

use faderframe_audio_graph::{AudioBuffer, NodeIo};
use faderframe_core::ChannelLayout;
use faderframe_plugin_host::builtin::BuiltinFactory;
use faderframe_plugin_host::eq::{Field, band_id, global, global_id};
use faderframe_plugin_host::{NO_HARMONY, PluginFactory, PluginProcessContext, ProcessConfig};
use faderframe_transport::TransportInfo;
use std::time::Instant;

const SR: f64 = 192_000.0;
const BLOCK: usize = 512;
/// Output buses offered (multi-output devices take what they have).
const OUTS: usize = 8;

/// The slowest reset of `id` (after playing noise each time), µs.
fn reset_cost(id: &str, set: &[(faderframe_core::ParameterId, f64)], live: bool) -> Option<f64> {
    let mut inst = BuiltinFactory.instantiate(id).ok()?;
    // Live: workers where a device has them (as in a playing session).
    inst.configure_realtime(live);
    for &(p, v) in set {
        inst.set_parameter(p, v).ok()?;
    }
    let config = ProcessConfig {
        sample_rate: SR,
        max_block_size: BLOCK as u32,
        sidechain: true,
        double_precision: false,
    };
    let mut p = inst.create_processor(&config).ok()?;
    let mut ins = [
        AudioBuffer::new(ChannelLayout::Stereo, BLOCK),
        AudioBuffer::new(ChannelLayout::Stereo, BLOCK),
    ];
    let mut outs: Vec<AudioBuffer> = (0..OUTS)
        .map(|_| AudioBuffer::new(ChannelLayout::Stereo, BLOCK))
        .collect();
    for b in ins.iter_mut().chain(outs.iter_mut()) {
        b.set_len(BLOCK);
    }
    let transport = TransportInfo::default();
    let mut seed = 1u32;
    let mut slowest = 0f64;
    for _ in 0..3 {
        // Half a second of noise.
        for _ in 0..(SR as usize / 2 / BLOCK) {
            for b in ins.iter_mut() {
                for c in 0..2 {
                    for s in b.channel_mut(c).iter_mut() {
                        seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                        *s = (seed >> 9) as f32 / (1u32 << 23) as f32 - 0.5;
                    }
                }
            }
            let ctx = PluginProcessContext {
                transport: &transport,
                param_events: &[],
                harmony: &NO_HARMONY,
                param_mods: &[],
                note_mods: &[],
            };
            let mut io = NodeIo {
                frames: BLOCK,
                audio_in: &ins,
                audio_out: &mut outs,
                events_in: &[],
                events_out: &mut [],
            };
            p.process(&ctx, &mut io);
        }
        let t0 = Instant::now();
        p.reset();
        slowest = slowest.max(t0.elapsed().as_secs_f64() * 1e6);
    }
    Some(slowest)
}

#[test]
#[ignore]
fn every_reset_is_quick() {
    let mut rows = Vec::new();
    for d in BuiltinFactory.scan() {
        let id = d.id.clone();
        match std::panic::catch_unwind(|| reset_cost(&id, &[], true)) {
            Ok(Some(us)) => rows.push((us, id.clone())),
            Ok(None) => eprintln!("{id}: not run here"),
            Err(_) => eprintln!("{id}: panicked"),
        }
    }
    // The EQ at its heaviest: linear phase at the highest resolution with
    // a spectral band.
    let eq = [
        (global_id(global::PHASE), 1.0),
        (global_id(global::QUALITY), 4.0),
        (band_id(0, Field::Enabled), 1.0),
        (band_id(0, Field::Spectral), 1.0),
    ];
    if let Some(us) = reset_cost(faderframe_core::builtin::EQ, &eq, true) {
        rows.push((us, "EQ, linear maximum + spectral".into()));
    }
    // Rendering (no workers): the Guitar Station resets inline.
    if let Some(us) = reset_cost(faderframe_core::builtin::GUITAR_STATION, &[], false) {
        rows.push((us, "Guitar Station, offline".into()));
    }
    rows.sort_by(|a, b| b.0.total_cmp(&a.0));
    for (us, id) in rows {
        eprintln!("{us:>10.1} µs  {id}");
    }
}
