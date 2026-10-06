//! Whisper's input: the log-mel spectrogram of 16 kHz audio, as its
//! feature extractor makes it — 400-point Hann-windowed frames every 160
//! samples (centred, reflect-padded), the power spectrum through the
//! checkpoint's 80 mel filters, log10 (floor 1e-10), no more than 8 below
//! the loudest, then (x + 4) / 4.

/// Samples a second.
pub const RATE: usize = 16_000;
pub const N_FFT: usize = 400;
pub const HOP: usize = 160;
pub const MELS: usize = 80;

/// The log-mel spectrogram of `audio` (`MELS` rows of `frames` values,
/// frame-major: `out[f * MELS + m]`), and the number of frames.
pub fn log_mel(audio: &[f32], filters: &[f32]) -> (Vec<f32>, usize) {
    let bins = N_FFT / 2 + 1;
    let pad = N_FFT / 2;
    let n = audio.len();
    // Centred frames: the signal reflected at both ends.
    let at = |i: isize| -> f32 {
        if n == 0 {
            return 0.0;
        }
        let mut j = i;
        let last = n as isize - 1;
        if last == 0 {
            return audio[0];
        }
        while j < 0 || j > last {
            if j < 0 {
                j = -j;
            }
            if j > last {
                j = 2 * last - j;
            }
        }
        audio[j as usize]
    };
    // torch.stft gives 1 + n / HOP frames; Whisper drops the last.
    let frames = n / HOP;
    let window: Vec<f32> = (0..N_FFT)
        .map(|i| (0.5 - 0.5 * (std::f64::consts::TAU * i as f64 / N_FFT as f64).cos()) as f32)
        .collect();
    let (cos, sin): (Vec<f32>, Vec<f32>) = (0..bins * N_FFT)
        .map(|k| {
            let (b, i) = (k / N_FFT, k % N_FFT);
            let a = std::f64::consts::TAU * (b * i % N_FFT) as f64 / N_FFT as f64;
            (a.cos() as f32, a.sin() as f32)
        })
        .unzip();
    let mut out = vec![0.0f32; frames * MELS];
    let mut frame = vec![0.0f32; N_FFT];
    let mut power = vec![0.0f32; bins];
    for f in 0..frames {
        let start = (f * HOP) as isize - pad as isize;
        for (i, v) in frame.iter_mut().enumerate() {
            *v = at(start + i as isize) * window[i];
        }
        for (b, p) in power.iter_mut().enumerate() {
            let (c, s) = (
                &cos[b * N_FFT..(b + 1) * N_FFT],
                &sin[b * N_FFT..(b + 1) * N_FFT],
            );
            let (mut re, mut im) = (0.0f32, 0.0f32);
            for i in 0..N_FFT {
                re += frame[i] * c[i];
                im -= frame[i] * s[i];
            }
            *p = re * re + im * im;
        }
        for m in 0..MELS {
            let w = &filters[m * bins..(m + 1) * bins];
            let v: f32 = w.iter().zip(&power).map(|(a, b)| a * b).sum();
            out[f * MELS + m] = v.max(1e-10).log10();
        }
    }
    let top = out.iter().copied().fold(f32::NEG_INFINITY, f32::max);
    for v in &mut out {
        *v = ((*v).max(top - 8.0) + 4.0) / 4.0;
    }
    (out, frames)
}

/// The checkpoint's mel filters (`preprocessor_config.json`), as `MELS`
/// rows of `N_FFT / 2 + 1`, whichever way round they are stored.
pub fn filters(config: &serde_json::Value) -> Option<Vec<f32>> {
    let rows = config["mel_filters"].as_array()?;
    let bins = N_FFT / 2 + 1;
    let get = |r: &serde_json::Value| -> Option<Vec<f32>> {
        r.as_array()?
            .iter()
            .map(|v| v.as_f64().map(|x| x as f32))
            .collect()
    };
    let m: Vec<Vec<f32>> = rows.iter().map(get).collect::<Option<_>>()?;
    match (m.len(), m.first().map(Vec::len)) {
        (MELS, Some(b)) if b == bins => Some(m.concat()),
        (b, Some(MELS)) if b == bins => Some(
            (0..MELS)
                .flat_map(|i| m.iter().map(move |r| r[i]))
                .collect(),
        ),
        _ => None,
    }
}
