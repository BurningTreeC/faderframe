//! The stretcher keeps pitch, honours its latency contract and never
//! allocates once configured.
#![allow(clippy::unwrap_used)]

use faderframe_stretch::{Preset, Stretcher};

#[cfg(ff_cpp_count)]
unsafe extern "C" {
    /// C++ heap allocations so far (the test-only `operator new`).
    fn ff_stretch_cpp_allocations() -> u64;
}

/// C++ allocations so far (always 0 where the counter is not linked, i.e.
/// Windows; the realtime check runs on the other platforms).
fn cpp_allocations() -> u64 {
    #[cfg(ff_cpp_count)]
    {
        // SAFETY: reads an atomic counter defined in count_new.cpp.
        unsafe { ff_stretch_cpp_allocations() }
    }
    #[cfg(not(ff_cpp_count))]
    {
        0
    }
}

const SR: f64 = 48_000.0;

fn sine(freq: f64, frames: usize) -> Vec<f32> {
    (0..frames)
        .map(|i| (i as f64 * freq * std::f64::consts::TAU / SR).sin() as f32 * 0.5)
        .collect()
}

/// Rising zero crossings per second over `x`.
fn frequency(x: &[f32]) -> f64 {
    let crossings = x.windows(2).filter(|w| w[0] < 0.0 && w[1] >= 0.0).count();
    crossings as f64 * SR / x.len() as f64
}

/// Stretch mono `input` by `factor` (output/input) in blocks of 256.
fn stretch(s: &mut Stretcher, input: &[f32], factor: f64) -> Vec<f32> {
    let li = s.input_latency();
    let lo = s.output_latency();
    s.reset();
    s.seek(&[&input[..li]], 1.0 / factor);
    let total = (input.len() as f64 * factor) as usize;
    let mut out = vec![0.0f32; total + lo];
    let mut consumed = li;
    let mut produced = 0;
    let block = 256;
    while produced < total + lo {
        let n = block.min(total + lo - produced);
        let target = li + ((produced + n) as f64 / factor) as usize;
        let take = target
            .saturating_sub(consumed)
            .min(input.len().saturating_sub(consumed));
        let zeros = vec![0.0f32; 4096];
        let chunk: &[f32] = if take > 0 {
            &input[consumed..consumed + take]
        } else {
            &zeros[..0]
        };
        let mut o = [&mut out[produced..produced + n]];
        s.process(&[chunk], chunk.len(), &mut o, n);
        consumed += take;
        produced += n;
    }
    out.drain(..lo);
    out
}

#[test]
fn stretching_keeps_the_pitch() {
    let mut s = Stretcher::new(1, SR, Preset::Polyphonic).unwrap();
    let input = sine(440.0, 96_000);
    for factor in [1.5, 0.75] {
        let out = stretch(&mut s, &input, factor);
        assert_eq!(out.len(), (input.len() as f64 * factor) as usize);
        // Away from the ends the tone is unchanged.
        let mid = &out[out.len() / 4..out.len() * 3 / 4];
        let f = frequency(mid);
        assert!((f - 440.0).abs() < 4.0, "{factor}×: {f} Hz");
        let rms = (mid.iter().map(|v| v * v).sum::<f32>() / mid.len() as f32).sqrt();
        assert!((rms - 0.354).abs() < 0.08, "{factor}×: level {rms}");
    }
}

#[test]
fn latency_contract_aligns_input_and_output() {
    // A click at input frame 20 000, played at the original speed, comes
    // out at output frame 20 000 once the output latency is removed.
    let mut s = Stretcher::new(1, SR, Preset::Rhythmic).unwrap();
    let mut input = vec![0.0f32; 48_000];
    for (k, v) in input[20_000..20_064].iter_mut().enumerate() {
        *v = (1.0 - k as f32 / 64.0) * if k % 2 == 0 { 1.0 } else { -1.0 };
    }
    let out = stretch(&mut s, &input, 1.0);
    let peak = out
        .iter()
        .enumerate()
        .max_by(|a, b| a.1.abs().total_cmp(&b.1.abs()))
        .unwrap()
        .0;
    assert!(
        (peak as i64 - 20_032).abs() < 200,
        "click at {peak}, expected near 20 032"
    );
}

#[test]
fn processing_never_allocates() {
    let mut stretchers = vec![
        Stretcher::new(2, SR, Preset::Polyphonic).unwrap(),
        Stretcher::new(2, SR, Preset::Rhythmic).unwrap(),
        Stretcher::new(1, 96_000.0, Preset::Polyphonic).unwrap(),
    ];
    let input: Vec<f32> = sine(220.0, 48_000)
        .iter()
        .zip(sine(331.0, 48_000))
        .map(|(a, b)| a + b * 0.3)
        .collect();
    let mut out_l = vec![0.0f32; 4096];
    let mut out_r = vec![0.0f32; 4096];
    let silence = vec![0.0f32; 48_000];
    let mut run = |s: &mut Stretcher, check: bool| {
        let before = cpp_allocations();
        s.reset();
        let seek = s.seek_length().min(input.len());
        s.seek(&[&input[..seek], &input[..seek]], 1.0);
        let mut pos = seek;
        for (i, ratio) in [0.5, 0.8, 1.0, 1.3, 2.0, 3.7]
            .iter()
            .cycle()
            .take(120)
            .enumerate()
        {
            let n = 64 + (i * 37) % 512;
            let take = ((n as f64 * ratio) as usize).min(4096);
            if pos + take > input.len() {
                pos = 0;
            }
            // Silence exercises the stretcher's silence path too.
            let src = if i % 17 == 0 { &silence } else { &input };
            let chunk = &src[pos..pos + take];
            let mut outs = [&mut out_l[..n], &mut out_r[..n]];
            s.process(&[chunk, chunk], take, &mut outs, n);
            pos += take;
            if i % 40 == 39 {
                s.reset();
                s.seek(&[&input[..seek], &input[..seek]], *ratio);
            }
        }
        if check {
            assert_eq!(
                cpp_allocations(),
                before,
                "{s:?} allocated while processing"
            );
        }
    };
    for s in &mut stretchers {
        // Warm up (first runs may touch lazily sized state), then check.
        run(s, false);
        run(s, true);
    }
    // Configuring does allocate (the counter works).
    if cfg!(ff_cpp_count) {
        let before = cpp_allocations();
        drop(Stretcher::new(2, SR, Preset::Polyphonic));
        assert!(cpp_allocations() > before);
    }
}
