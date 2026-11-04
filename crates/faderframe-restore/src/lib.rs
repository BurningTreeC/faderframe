//! Audio restoration: the DSP of FaderFrame's restoration devices (De-noise,
//! De-click, De-hum, De-clip, De-reverb in `faderframe_plugin_host::devices`)
//! and of the spectral editor's "Learn Noise and Remove"
//! (`faderframe_spectral`), so a profile learned in one place and heard in
//! the other sounds the same.
//!
//! * [`stft`]: a streaming short-time Fourier transform (square-root Hann,
//!   75 % overlap, a frame of latency); [`blocks`]: windows with context
//!   on both sides for the time-domain repairs.
//! * [`denoise`] with [`profile`]: noise profiles in sixth-octave bands,
//!   learned or tracked (minima-controlled averaging), and the
//!   decision-directed Wiener gains that keep musical noise down.
//! * [`ar`]: autoregressive models and least-squares interpolation, which
//!   [`declick`] (clicks and crackle found in the model's matched residual)
//!   and [`declip`] (clipped runs rebuilt beyond the clip level) fill gaps
//!   with.
//! * [`dehum`]: notches at the mains and its harmonics, following its
//!   drift; [`dereverb`]: late reverb predicted from the decay and taken
//!   down, and the decay estimated from the sound.
//!
//! Everything is allocation free once made, so the devices run it on the
//! audio thread.

#![forbid(unsafe_code)]

pub mod ar;
pub mod blocks;
pub mod declick;
pub mod declip;
pub mod dehum;
pub mod denoise;
pub mod dereverb;
pub mod profile;
pub mod stft;

/// A frame size of about `seconds` at `rate`: a power of two.
pub fn frame_for(seconds: f64, rate: f64) -> usize {
    ((seconds * rate).round().max(64.0) as usize).next_power_of_two()
}

/// A deterministic noise of unit variance (nearly Gaussian: four uniform
/// draws summed), for tests, dither and the devices' own noise.
#[derive(Clone, Copy, Debug)]
pub struct Noise(u32);

impl Noise {
    pub const fn new(seed: u32) -> Self {
        Self(if seed == 0 { 0x9e37_79b9 } else { seed })
    }

    #[inline]
    fn uniform(&mut self) -> f64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        self.0 = x;
        f64::from(x) / f64::from(u32::MAX) * 2.0 - 1.0
    }

    /// The next value.
    #[inline]
    #[allow(clippy::should_implement_trait)]
    pub fn next(&mut self) -> f64 {
        // Four uniforms on ±1 sum to a variance of 4/3.
        (self.uniform() + self.uniform() + self.uniform() + self.uniform())
            * 0.866_025_403_784_438_6
    }
}
