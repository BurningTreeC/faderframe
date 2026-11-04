//! The whole way through the codec: the project's channels resampled to
//! 48 kHz, coded and decoded frame by frame (mono and coupled stereo
//! substreams), resampled back — and, beside it, the input delayed by
//! exactly as much, so the decoded signal, the original and their
//! difference line up to the sample.
//!
//! **Delay.** Each substream gathers [`FRAME`] samples, codes them and
//! appends what the decoder gives back to its output ring; the ring starts
//! primed so that it never runs dry. With Opus' lookahead `L` (the decoder
//! gives frame `k` back delayed by it), every substream delays by exactly
//! `FRAME + L` samples at 48 kHz, padded by [`RatePlan::delay_for`] so the
//! whole way is a whole number of project samples ([`CodecChain::delay`]).
//! Substreams start their frames at staggered points (the first frame of
//! substream `k` of `S` is shorter by `k·FRAME/S`, its ring primed by as
//! much more): the delay is the same, but a 7.1.4 bed's six encoders do
//! their work in six different device callbacks instead of one.
//!
//! **Realtime.** Everything is allocated in [`CodecChain::new`] (control
//! thread); [`CodecChain::process`] and [`CodecChain::set_settings`]
//! allocate nothing (libopus is built with variable-length stack arrays,
//! its `ctl` calls only set fields) — the chain may live on the audio
//! thread, and must be dropped off it.

use crate::resample::{RatePlan, Resampler};
use crate::{CodecError, CodecSettings, CoderKind};

/// Samples per frame at 48 kHz (20 ms: what music is coded with).
pub const FRAME: usize = 960;
/// Most project frames one [`CodecChain::process`] call takes.
pub const CHUNK: usize = 1024;
/// The identity coder's lookahead: Opus' (2.5 ms + 4 ms) at 48 kHz.
pub const IDENTITY_LOOKAHEAD: usize = 312;
/// Most channels a chain codes (a 9.1.6 bed).
pub const MAX_CHANNELS: usize = 16;

/// A fixed ring of samples (one channel).
#[derive(Debug)]
struct Ring {
    buf: Box<[f32]>,
    head: usize,
    len: usize,
}

impl Ring {
    fn new(capacity: usize) -> Self {
        Self {
            buf: vec![0.0; capacity.max(1)].into_boxed_slice(),
            head: 0,
            len: 0,
        }
    }

    fn push(&mut self, x: &[f32]) {
        let cap = self.buf.len();
        for &v in x {
            if self.len == cap {
                // Never happens with the sizes made; the oldest goes.
                self.head = (self.head + 1) % cap;
                self.len -= 1;
            }
            self.buf[(self.head + self.len) % cap] = v;
            self.len += 1;
        }
    }

    fn push_zeros(&mut self, n: usize) {
        for _ in 0..n {
            self.push(&[0.0]);
        }
    }

    /// Fill `out` from the oldest (silence once empty).
    fn pop_into(&mut self, out: &mut [f32]) {
        let cap = self.buf.len();
        for o in out.iter_mut() {
            if self.len == 0 {
                *o = 0.0;
                continue;
            }
            *o = self.buf[self.head];
            self.head = (self.head + 1) % cap;
            self.len -= 1;
        }
    }

    fn clear(&mut self) {
        self.head = 0;
        self.len = 0;
    }
}

/// A delay line of a fixed length (one channel).
#[derive(Debug)]
struct DelayLine {
    buf: Box<[f32]>,
    at: usize,
}

impl DelayLine {
    fn new(delay: usize) -> Self {
        Self {
            buf: vec![0.0; delay + 1].into_boxed_slice(),
            at: 0,
        }
    }

    fn process(&mut self, x: &[f32], out: &mut [f32]) {
        let len = self.buf.len();
        for (o, &v) in out.iter_mut().zip(x) {
            self.buf[self.at] = v;
            self.at = (self.at + 1) % len;
            // The oldest: `len − 1` samples ago.
            *o = self.buf[self.at];
        }
    }

    fn clear(&mut self) {
        self.buf.fill(0.0);
    }
}

/// What codes a substream's frames.
enum Coder {
    #[cfg(not(faderframe_check_only))]
    Opus {
        encoder: Box<opus::Encoder>,
        decoder: Box<opus::Decoder>,
        packet: Box<[u8]>,
    },
    /// The frame back unchanged, `IDENTITY_LOOKAHEAD` samples late.
    Identity { tail: Box<[f32]> },
}

impl Coder {
    fn new(kind: CoderKind, channels: usize) -> Result<Self, CodecError> {
        match kind {
            CoderKind::Identity => Ok(Coder::Identity {
                tail: vec![0.0; IDENTITY_LOOKAHEAD * channels].into_boxed_slice(),
            }),
            #[cfg(faderframe_check_only)]
            CoderKind::Opus => Err(CodecError::Unavailable),
            #[cfg(not(faderframe_check_only))]
            CoderKind::Opus => {
                let layout = if channels == 2 {
                    opus::Channels::Stereo
                } else {
                    opus::Channels::Mono
                };
                let err = |e: opus::Error| CodecError::Opus(e.to_string());
                let mut encoder = opus::Encoder::new(
                    crate::resample::CODEC_RATE,
                    layout,
                    opus::Application::Audio,
                )
                .map_err(err)?;
                encoder.set_complexity(10).map_err(err)?;
                let decoder =
                    opus::Decoder::new(crate::resample::CODEC_RATE, layout).map_err(err)?;
                Ok(Coder::Opus {
                    encoder: Box::new(encoder),
                    decoder: Box::new(decoder),
                    packet: vec![0; 1500 * channels].into_boxed_slice(),
                })
            }
        }
    }

    /// Back to its start (the identity coder's tail; Opus carries on).
    fn restart(&mut self) {
        match self {
            #[cfg(not(faderframe_check_only))]
            Coder::Opus { .. } => {}
            Coder::Identity { tail } => tail.fill(0.0),
        }
    }

    /// The decoder's delay (48 kHz samples).
    fn lookahead(&mut self) -> usize {
        match self {
            #[cfg(not(faderframe_check_only))]
            Coder::Opus { encoder, .. } => encoder
                .get_lookahead()
                .map_or(IDENTITY_LOOKAHEAD, |l| l.max(0) as usize),
            Coder::Identity { .. } => IDENTITY_LOOKAHEAD,
        }
    }

    /// Bitrate (bits per second for these channels) and VBR; `false` when
    /// libopus refused.
    fn configure(&mut self, bits: u32, vbr: bool) -> bool {
        match self {
            #[cfg(not(faderframe_check_only))]
            Coder::Opus { encoder, .. } => {
                let rate = encoder.set_bitrate(opus::Bitrate::Bits(bits as i32));
                let mode = encoder.set_vbr(vbr);
                // Unconstrained VBR is how files are coded.
                let constraint = encoder.set_vbr_constraint(false);
                rate.is_ok() && mode.is_ok() && constraint.is_ok()
            }
            Coder::Identity { .. } => {
                let _ = (bits, vbr);
                true
            }
        }
    }

    /// One frame (`input`: interleaved, `FRAME × channels`) coded and
    /// decoded into `out`; `false` (and silence) when the codec failed.
    fn code(&mut self, input: &[f32], out: &mut [f32], channels: usize) -> bool {
        match self {
            #[cfg(not(faderframe_check_only))]
            Coder::Opus {
                encoder,
                decoder,
                packet,
            } => {
                let ok = encoder
                    .encode_float(input, packet)
                    .and_then(|len| decoder.decode_float(&packet[..len], out, false));
                match ok {
                    Ok(n) if n == FRAME => true,
                    Ok(n) => {
                        let from = (n * channels).min(out.len());
                        out[from..].fill(0.0);
                        false
                    }
                    Err(_) => {
                        out.fill(0.0);
                        false
                    }
                }
            }
            Coder::Identity { tail } => {
                let _ = channels;
                let t = tail.len();
                let n = input.len();
                out[..t].copy_from_slice(tail);
                out[t..].copy_from_slice(&input[..n - t]);
                tail.copy_from_slice(&input[n - t..]);
                true
            }
        }
    }
}

/// One mono or coupled-stereo substream.
struct Substream {
    first: usize,
    count: usize,
    coder: Coder,
    /// Interleaved samples gathered for the next frame.
    acc: Box<[f32]>,
    fill: usize,
    /// Interleaved: what the decoder gave back.
    decoded: Box<[f32]>,
    /// Per channel: decoded samples waiting to go out.
    out: Vec<Ring>,
    /// Where its frames start: the first frame is `FRAME − offset` short
    /// and the rings primed by `offset` (+ the padding).
    offset: usize,
    pad: usize,
}

impl Substream {
    fn start(&mut self) {
        self.acc.fill(0.0);
        self.fill = FRAME - self.offset;
        for r in &mut self.out {
            r.clear();
            r.push_zeros(self.offset + self.pad);
        }
        self.coder.restart();
    }

    /// Code `m` samples of `buf` in place (decoded out, `FRAME + L + pad`
    /// late).
    fn run(&mut self, buf: &mut [Box<[f32]>], m: usize, failures: &mut u64) {
        let ch = self.count;
        let mut i = 0;
        while i < m {
            let take = (FRAME - self.fill).min(m - i);
            for j in 0..take {
                for c in 0..ch {
                    self.acc[(self.fill + j) * ch + c] = buf[self.first + c][i + j];
                }
            }
            self.fill += take;
            i += take;
            if self.fill == FRAME {
                self.fill = 0;
                if !self.coder.code(&self.acc, &mut self.decoded, ch) {
                    *failures += 1;
                }
                for (c, ring) in self.out.iter_mut().enumerate() {
                    for k in 0..FRAME {
                        ring.push(&[self.decoded[k * ch + c]]);
                    }
                }
            }
        }
        for (c, ring) in self.out.iter_mut().enumerate() {
            ring.pop_into(&mut buf[self.first + c][..m]);
        }
    }
}

/// The codec stage for `channels` at a project rate: the decoded signal
/// and the original, both delayed by [`Self::delay`].
pub struct CodecChain {
    channels: usize,
    plan: RatePlan,
    settings: CodecSettings,
    to: Vec<Resampler>,
    from: Vec<Resampler>,
    subs: Vec<Substream>,
    /// Per channel: 48 kHz samples of the chunk.
    mid: Vec<Box<[f32]>>,
    /// Per channel: project-rate samples back from 48 kHz, before they go.
    back: Vec<Ring>,
    scratch: Box<[f32]>,
    dry_lines: Vec<DelayLine>,
    wet: Vec<Box<[f32]>>,
    dry: Vec<Box<[f32]>>,
    delay: usize,
    failures: u64,
    /// Frames of the last [`Self::process`] call.
    last: usize,
}

impl std::fmt::Debug for CodecChain {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CodecChain")
            .field("channels", &self.channels)
            .field("rate", &self.plan.rate())
            .field("settings", &self.settings)
            .field("delay", &self.delay)
            .finish()
    }
}

/// Bits per second for a substream of `count` channels at `kbps` a pair.
fn bits_for(kbps: u32, count: usize) -> u32 {
    let k = crate::clamp_kbps(kbps);
    if count == 2 { k * 1000 } else { k * 500 }
}

impl CodecChain {
    /// `channels` at `rate`, coded as `groups` say (each `(first, count)`:
    /// a mono or coupled-stereo substream; [`crate::substreams`] for a
    /// layout). Control thread.
    pub fn new(
        channels: usize,
        rate: u32,
        groups: &[(usize, usize)],
        settings: CodecSettings,
    ) -> Result<Self, CodecError> {
        if channels == 0 || channels > MAX_CHANNELS {
            return Err(CodecError::Channels(channels));
        }
        let covered: usize = groups.iter().map(|g| g.1).sum();
        if covered != channels
            || groups
                .iter()
                .any(|&(first, count)| !(1..=2).contains(&count) || first + count > channels)
        {
            return Err(CodecError::Channels(channels));
        }
        let plan = RatePlan::new(rate)?;
        let mut coders = groups
            .iter()
            .map(|&(_, count)| Coder::new(settings.kind, count))
            .collect::<Result<Vec<_>, _>>()?;
        let lookahead = coders
            .iter_mut()
            .map(Coder::lookahead)
            .max()
            .unwrap_or(IDENTITY_LOOKAHEAD);
        let (pad, delay) = plan.delay_for(FRAME + lookahead);
        let mid_cap = plan.max_codec_frames(CHUNK);
        let back_cap = plan.max_project_frames(mid_cap);
        let s = groups.len();
        let subs = groups
            .iter()
            .zip(coders)
            .enumerate()
            .map(|(k, (&(first, count), mut coder))| {
                if !coder.configure(bits_for(settings.kbps, count), settings.vbr) {
                    return Err(CodecError::Opus("the encoder refused its settings".into()));
                }
                let mut sub = Substream {
                    first,
                    count,
                    coder,
                    acc: vec![0.0; FRAME * count].into_boxed_slice(),
                    fill: 0,
                    decoded: vec![0.0; FRAME * count].into_boxed_slice(),
                    out: (0..count)
                        .map(|_| Ring::new(2 * FRAME + pad + mid_cap + 8))
                        .collect(),
                    offset: FRAME - k * FRAME / s,
                    pad,
                };
                sub.start();
                Ok(sub)
            })
            .collect::<Result<Vec<_>, CodecError>>()?;
        let to: Vec<Resampler> = (0..channels).filter_map(|_| plan.to_codec()).collect();
        let from: Vec<Resampler> = (0..channels).filter_map(|_| plan.from_codec()).collect();
        let buffer = |n: usize| vec![0.0f32; n].into_boxed_slice();
        Ok(Self {
            channels,
            settings,
            to,
            from,
            subs,
            mid: (0..channels).map(|_| buffer(mid_cap)).collect(),
            back: (0..channels)
                .map(|_| Ring::new(back_cap + CHUNK + 8))
                .collect(),
            scratch: buffer(back_cap),
            dry_lines: (0..channels).map(|_| DelayLine::new(delay)).collect(),
            wet: (0..channels).map(|_| buffer(CHUNK)).collect(),
            dry: (0..channels).map(|_| buffer(CHUNK)).collect(),
            delay,
            failures: 0,
            last: 0,
            plan,
        })
    }

    pub fn channels(&self) -> usize {
        self.channels
    }

    pub fn rate(&self) -> u32 {
        self.plan.rate()
    }

    /// How late both outputs are behind the input (project frames).
    pub fn delay(&self) -> usize {
        self.delay
    }

    pub fn settings(&self) -> CodecSettings {
        self.settings
    }

    /// Frames the codec could not code (each heard as silence).
    pub fn failures(&self) -> u64 {
        self.failures
    }

    /// A new bitrate or VBR, from the next frame on (allocation free; the
    /// coder's kind stays as made).
    pub fn set_settings(&mut self, settings: CodecSettings) {
        let s = CodecSettings {
            kind: self.settings.kind,
            ..settings
        };
        if s == self.settings {
            return;
        }
        for sub in &mut self.subs {
            if !sub.coder.configure(bits_for(s.kbps, sub.count), s.vbr) {
                self.failures += 1;
            }
        }
        self.settings = s;
    }

    /// Back to silence (the delay lines and rings emptied, the frames
    /// restarted; the codec's own state carries on).
    pub fn clear(&mut self) {
        for d in &mut self.dry_lines {
            d.clear();
        }
        for s in &mut self.subs {
            s.start();
        }
        for b in &mut self.back {
            b.clear();
        }
    }

    /// Take `n` frames (at most [`CHUNK`]) of each channel from `input`;
    /// [`Self::wet`] and [`Self::dry`] then hold `n` frames each.
    pub fn process<'a>(&mut self, n: usize, input: impl Fn(usize) -> &'a [f32]) {
        let n = n.min(CHUNK);
        self.last = n;
        let mut m = n;
        for c in 0..self.channels {
            let x = &input(c)[..n];
            self.dry_lines[c].process(x, &mut self.dry[c][..n]);
            match self.to.get_mut(c) {
                Some(r) => m = r.process(x, &mut self.mid[c]),
                None => self.mid[c][..n].copy_from_slice(x),
            }
        }
        for s in &mut self.subs {
            s.run(&mut self.mid, m, &mut self.failures);
        }
        for c in 0..self.channels {
            match self.from.get_mut(c) {
                Some(r) => {
                    let k = r.process(&self.mid[c][..m], &mut self.scratch);
                    self.back[c].push(&self.scratch[..k]);
                    self.back[c].pop_into(&mut self.wet[c][..n]);
                }
                None => self.wet[c][..n].copy_from_slice(&self.mid[c][..n]),
            }
        }
    }

    /// The decoded signal of channel `c` for the last call.
    pub fn wet(&self, c: usize) -> &[f32] {
        &self.wet[c][..self.last]
    }

    /// The input of channel `c`, delayed alike, for the last call.
    pub fn dry(&self, c: usize) -> &[f32] {
        &self.dry[c][..self.last]
    }
}

#[cfg(test)]
#[allow(clippy::needless_range_loop)]
mod tests {
    use super::*;

    fn tone(rate: u32, n: usize, phase: f64) -> Vec<f32> {
        (0..n)
            .map(|i| {
                let t = i as f64 / f64::from(rate);
                (0.3 * (std::f64::consts::TAU * 440.0 * t + phase).sin()
                    + 0.2 * (std::f64::consts::TAU * 5_300.0 * t).sin()) as f32
            })
            .collect()
    }

    fn run(
        chain: &mut CodecChain,
        input: &[Vec<f32>],
        block: usize,
    ) -> (Vec<Vec<f32>>, Vec<Vec<f32>>) {
        let channels = input.len();
        let n = input[0].len();
        let mut wet = vec![Vec::with_capacity(n); channels];
        let mut dry = vec![Vec::with_capacity(n); channels];
        let mut at = 0;
        while at < n {
            let k = block.min(n - at).min(CHUNK);
            chain.process(k, |c| &input[c][at..at + k]);
            for c in 0..channels {
                wet[c].extend_from_slice(chain.wet(c));
                dry[c].extend_from_slice(chain.dry(c));
            }
            at += k;
        }
        (wet, dry)
    }

    fn identity(kbps: u32) -> CodecSettings {
        CodecSettings {
            kind: CoderKind::Identity,
            kbps,
            vbr: true,
        }
    }

    #[test]
    fn at_48_khz_the_identity_coder_is_the_input_delayed_exactly() {
        let rate = 48_000;
        let input: Vec<Vec<f32>> = (0..3).map(|c| tone(rate, 20_000, c as f64)).collect();
        let mut chain = CodecChain::new(3, rate, &[(0, 2), (2, 1)], identity(128)).unwrap();
        assert_eq!(chain.delay(), FRAME + IDENTITY_LOOKAHEAD);
        let d = chain.delay();
        for block in [64, 1000, 37] {
            chain.clear();
            let (wet, dry) = run(&mut chain, &input, block);
            for c in 0..3 {
                for i in 0..input[c].len() {
                    let want = if i >= d { input[c][i - d] } else { 0.0 };
                    assert_eq!(wet[c][i], want, "wet {c} {i} (block {block})");
                    assert_eq!(dry[c][i], want, "dry {c} {i} (block {block})");
                }
            }
        }
    }

    #[test]
    fn at_other_rates_the_decoded_and_the_original_line_up() {
        for rate in [44_100u32, 96_000, 88_200] {
            let n = rate as usize;
            let input = vec![tone(rate, n, 0.0), tone(rate, n, 1.0)];
            let mut chain = CodecChain::new(2, rate, &[(0, 2)], identity(128)).unwrap();
            let d = chain.delay();
            let (wet, dry) = run(&mut chain, &input, 256);
            let (mut err, mut sig) = (0.0f64, 0.0f64);
            for c in 0..2 {
                for i in d + 4000..n {
                    assert_eq!(dry[c][i], input[c][i - d]);
                    err += f64::from(wet[c][i] - dry[c][i]).powi(2);
                    sig += f64::from(dry[c][i]).powi(2);
                }
            }
            let db = 10.0 * (err / sig).log10();
            assert!(
                db < -80.0,
                "{rate} Hz: the difference is {db:.1} dB (delay {d})"
            );
        }
    }

    #[cfg(not(faderframe_check_only))]
    #[test]
    fn opus_is_heard_in_step_with_the_original() {
        let rate = 48_000;
        let n = 48_000;
        let input = vec![tone(rate, n, 0.0), tone(rate, n, 0.7)];
        let settings = CodecSettings {
            kind: CoderKind::Opus,
            kbps: 128,
            vbr: true,
        };
        let mut chain = CodecChain::new(2, rate, &[(0, 2)], settings).unwrap();
        let d = chain.delay();
        assert_eq!(d, FRAME + 312, "Opus' lookahead at 48 kHz");
        let (wet, dry) = run(&mut chain, &input, 128);
        // The best lag between the decoded and the original is none.
        let lag_err = |lag: i64| {
            let mut e = 0.0f64;
            for i in d + 2000..n - 200 {
                let j = (i as i64 + lag) as usize;
                e += f64::from(wet[0][j] - dry[0][i]).powi(2);
            }
            e
        };
        let at_zero = lag_err(0);
        for lag in [-3i64, -2, -1, 1, 2, 3] {
            assert!(at_zero < lag_err(lag), "lag {lag} fits better than none");
        }
        let sig: f64 = (d + 2000..n - 200)
            .map(|i| f64::from(dry[0][i]).powi(2))
            .sum();
        let db = 10.0 * (at_zero / sig).log10();
        assert!(db < -15.0, "a clean tone at 128 kb/s differs by {db:.1} dB");
        assert_eq!(chain.failures(), 0);
        // Changing the bitrate on the way allocates no new chain and keeps
        // the timing.
        chain.set_settings(CodecSettings {
            kbps: 48,
            vbr: false,
            ..settings
        });
        assert_eq!(chain.settings().kbps, 48);
        assert_eq!(chain.delay(), d);
    }

    /// What a chain costs in real time (release: `cargo test -p
    /// faderframe-codec --release --lib codec_cost -- --ignored
    /// --nocapture`).
    #[cfg(not(faderframe_check_only))]
    #[test]
    #[ignore]
    fn codec_cost() {
        for (rate, channels, groups) in [
            (48_000u32, 2usize, vec![(0usize, 2usize)]),
            (44_100, 2, vec![(0, 2)]),
            (96_000, 2, vec![(0, 2)]),
            (48_000, 6, vec![(0, 2), (2, 1), (3, 1), (4, 2)]),
        ] {
            let seconds = 10;
            let n = rate as usize * seconds;
            let input: Vec<Vec<f32>> = (0..channels).map(|c| tone(rate, n, c as f64)).collect();
            let mut chain =
                CodecChain::new(channels, rate, &groups, CodecSettings::opus(320, true)).unwrap();
            let t = std::time::Instant::now();
            let mut worst = std::time::Duration::ZERO;
            let mut at = 0;
            while at < n {
                let k = 256.min(n - at);
                let b = std::time::Instant::now();
                chain.process(k, |c| &input[c][at..at + k]);
                worst = worst.max(b.elapsed());
                at += k;
            }
            let took = t.elapsed().as_secs_f64();
            println!(
                "{rate} Hz, {channels} ch: {:.2} % of a core, worst 256-frame block {:.0} µs \
                 (of {:.0} µs)",
                100.0 * took / seconds as f64,
                worst.as_secs_f64() * 1e6,
                256.0 / f64::from(rate) * 1e6
            );
        }
    }

    #[test]
    fn groups_must_cover_the_channels() {
        assert!(CodecChain::new(3, 48_000, &[(0, 2)], identity(128)).is_err());
        assert!(CodecChain::new(2, 48_000, &[(0, 3)], identity(128)).is_err());
        assert!(CodecChain::new(0, 48_000, &[], identity(128)).is_err());
    }
}
