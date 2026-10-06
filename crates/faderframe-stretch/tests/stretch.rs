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

/// The allocation counter counts the whole process: tests that configure
/// stretchers run one at a time, so none counts another's.
fn serial() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    LOCK.lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
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
    let _serial = serial();
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
    let _serial = serial();
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
    let _serial = serial();
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
            // Transposing as it goes (the samplers' Keep Length, bends,
            // pitch editing with its formants kept or moved).
            s.set_transpose([0.5, 1.0, 1.26, 2.0][i % 4]);
            s.set_formant([1.0, 1.0, 0.84, 1.19, 1.0][i % 5], i % 3 != 0);
            s.set_formant_base([0.0, 220.0][i % 2]);
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

/// The spectral centroid of `x` (Hz), by a plain DFT over 2048 frames.
fn centroid(x: &[f32]) -> f64 {
    let n = 2048;
    let x = &x[x.len() / 2..x.len() / 2 + n];
    let (mut num, mut den) = (0.0, 0.0);
    for k in 1..n / 2 {
        let (mut re, mut im) = (0.0, 0.0);
        for (i, v) in x.iter().enumerate() {
            let w = 0.5 - 0.5 * (std::f64::consts::TAU * i as f64 / n as f64).cos();
            let a = std::f64::consts::TAU * (k * i) as f64 / n as f64;
            re += f64::from(*v) * w * a.cos();
            im -= f64::from(*v) * w * a.sin();
        }
        let m = (re * re + im * im).sqrt();
        num += m * k as f64 * SR / n as f64;
        den += m;
    }
    num / den
}

#[test]
fn transposing_can_keep_the_formants() {
    let _serial = serial();
    // A buzz (all harmonics of 150 Hz) through a resonance near 1 kHz:
    // a vowel-like spectrum.
    let mut lp = 0.0f64;
    let mut bp = 0.0f64;
    let (f, q) = (2.0 * (std::f64::consts::PI * 1_000.0 / SR).sin(), 0.2);
    let input: Vec<f32> = (0..96_000)
        .map(|i| {
            let phase = (i as f64 * 150.0 / SR).fract();
            let saw = 2.0 * phase - 1.0;
            let hp = saw - lp - q * bp;
            bp += f * hp;
            lp += f * bp;
            (bp * 0.3) as f32
        })
        .collect();
    let shifted = |keep: bool| {
        let mut s = Stretcher::new(1, SR, Preset::Polyphonic).unwrap();
        s.set_transpose(2f32.powf(5.0 / 12.0));
        s.set_formant(1.0, keep);
        stretch(&mut s, &input, 1.0)
    };
    let moved = centroid(&shifted(false));
    let kept = centroid(&shifted(true));
    let original = centroid(&input);
    // Up a fourth: without keeping them the formants rise with the pitch,
    // kept they stay near the original's.
    assert!(moved > original * 1.15, "{moved} vs {original}");
    assert!(
        (kept / original - 1.0).abs() < (moved / original - 1.0).abs() / 2.0,
        "kept {kept}, moved {moved}, original {original}"
    );
}
