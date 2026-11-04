//! "Preview on platforms": a rendered master measured, then coded and
//! decoded as each platform streams it, and what that does to it reported
//! — the gain the platform plays it at, the true peak after decoding
//! (with and without normalisation), the frames over full scale, the level
//! of the codec's artefacts — with advice.
//!
//! The master is read through a [`Source`] (a file read back piece by
//! piece: nothing holds the whole song), once to measure it and once per
//! platform, the platforms on threads of their own (as many as there are
//! cores, at most one per platform).

use crate::chain::{CHUNK, CodecChain};
use crate::meter::{CodecMeter, db};
use crate::platform::{Platform, PlatformGain};
use crate::{CodecError, CodecSettings, CoderKind};
use faderframe_analysis::{TruePeak, WeightedLoudness};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

/// Rendered audio read piece by piece (from any thread).
pub trait Source: Sync {
    fn channels(&self) -> usize;
    fn rate(&self) -> u32;
    fn frames(&self) -> u64;
    /// Frames `[start, start + out[c].len())` into `out` (silence past the
    /// end).
    fn read(&self, start: u64, out: &mut [&mut [f32]]) -> Result<(), String>;
}

/// Planar audio in memory as a [`Source`].
pub struct Planar<'a> {
    pub channels: &'a [Vec<f32>],
    pub rate: u32,
}

impl Source for Planar<'_> {
    fn channels(&self) -> usize {
        self.channels.len()
    }

    fn rate(&self) -> u32 {
        self.rate
    }

    fn frames(&self) -> u64 {
        self.channels.iter().map(Vec::len).min().unwrap_or(0) as u64
    }

    fn read(&self, start: u64, out: &mut [&mut [f32]]) -> Result<(), String> {
        for (o, c) in out.iter_mut().zip(self.channels) {
            let from = (start as usize).min(c.len());
            let n = o.len().min(c.len() - from);
            o[..n].copy_from_slice(&c[from..from + n]);
            o[n..].fill(0.0);
        }
        Ok(())
    }
}

/// What the master measures before any platform touches it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Master {
    /// Integrated loudness (LUFS, BS.1770 with the channels' weights).
    pub integrated: f64,
    /// Highest true peak (dBTP).
    pub true_peak: f64,
    pub frames: u64,
    pub rate: u32,
}

/// Measure the master: integrated loudness (`weights` per channel) and
/// true peak. `done` counts the frames read.
pub fn measure_master(
    src: &dyn Source,
    weights: &[f64],
    stop: &AtomicBool,
    done: &AtomicU64,
) -> Result<Master, CodecError> {
    let channels = src.channels();
    let frames = src.frames();
    let mut loudness = WeightedLoudness::new(src.rate(), weights);
    let mut tp = vec![TruePeak::new(); channels];
    let mut peak = 0.0f64;
    let mut bufs = vec![vec![0.0f32; CHUNK]; channels];
    let mut at = 0u64;
    while at < frames {
        if stop.load(Ordering::Relaxed) {
            return Err(CodecError::Cancelled);
        }
        let n = (frames - at).min(CHUNK as u64) as usize;
        {
            let mut views: Vec<&mut [f32]> = bufs.iter_mut().map(|b| &mut b[..n]).collect();
            src.read(at, &mut views).map_err(CodecError::Read)?;
        }
        let views: Vec<&[f32]> = bufs.iter().map(|b| &b[..n]).collect();
        loudness.add(&views);
        for (t, b) in tp.iter_mut().zip(&views) {
            for &x in *b {
                peak = peak.max(t.run(x));
            }
        }
        at += n as u64;
        done.fetch_add(n as u64, Ordering::Relaxed);
    }
    for t in &mut tp {
        for _ in 0..faderframe_analysis::TP_LATENCY {
            peak = peak.max(t.run(0.0));
        }
    }
    Ok(Master {
        integrated: loudness.integrated(),
        true_peak: db(peak),
        frames,
        rate: src.rate(),
    })
}

/// How likely the decoded master is to clip where the platform plays it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Risk {
    /// Its true peak stays a decibel or more under full scale.
    Safe,
    /// Within a decibel of full scale.
    Close,
    /// Over full scale: it clips on players.
    Clips,
}

impl Risk {
    fn of(true_peak: f64) -> Self {
        if true_peak > 0.0 {
            Risk::Clips
        } else if true_peak > -1.0 {
            Risk::Close
        } else {
            Risk::Safe
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Risk::Safe => "safe",
            Risk::Close => "close to clipping",
            Risk::Clips => "clips",
        }
    }
}

/// What one platform does to the master.
#[derive(Clone, Debug, PartialEq)]
pub struct PlatformPreview {
    pub platform: &'static Platform,
    pub master: Master,
    pub gain: PlatformGain,
    /// Decoded, as it plays with normalisation off (dBTP) …
    pub decoded_true_peak: f64,
    /// … and at the platform's gain (dBTP).
    pub heard_true_peak: f64,
    /// Frames over full scale after decoding, at the platform's gain and
    /// with normalisation off.
    pub overs: u64,
    pub overs_unnormalised: u64,
    /// What the codec removed or added against the master (dB).
    pub artefacts_db: Option<f64>,
    /// At the platform's gain.
    pub risk: Risk,
    pub advice: Vec<String>,
}

/// Rounded down to a tenth of a decibel ("−1.4").
fn tenth_down(v: f64) -> f64 {
    (v * 10.0).floor() / 10.0
}

fn signed(v: f64) -> String {
    format!("{v:+.1}").replace('-', "−")
}

/// The advice for a preview (what to change before delivering).
fn advise(p: &PlatformPreview) -> Vec<String> {
    let name = p.platform.name;
    let mut out = Vec::new();
    // Peaks: aim for −1 dBTP decoded, played either way.
    let worst = p.heard_true_peak.max(p.decoded_true_peak);
    if worst > -1.0 && p.master.true_peak.is_finite() {
        let ceiling = tenth_down((p.master.true_peak - (worst + 1.0)).min(-1.0));
        let how = if p.heard_true_peak > 0.0 {
            format!(
                "decoded at {name}'s level its true peak reaches {} dBTP ({} frames over full scale)",
                signed(p.heard_true_peak),
                p.overs
            )
        } else if p.decoded_true_peak > 0.0 {
            format!(
                "decoded with normalisation off its true peak reaches {} dBTP ({} frames over full scale)",
                signed(p.decoded_true_peak),
                p.overs_unnormalised
            )
        } else {
            format!(
                "decoded its true peak comes within a decibel of full scale ({} dBTP)",
                signed(worst)
            )
        };
        out.push(format!(
            "The codec raises the peaks: {how}. Lower the limiter's ceiling to {} dBTP.",
            signed(ceiling)
        ));
    } else if p.decoded_true_peak - p.master.true_peak > 0.5 {
        out.push(format!(
            "The codec raises the true peak by {:.1} dB, still under −1 dBTP.",
            p.decoded_true_peak - p.master.true_peak
        ));
    }
    let g = p.gain;
    if g.db <= -2.0 {
        out.push(format!(
            "{name} turns this master down by {:.1} dB: it plays no louder than one at {:.0} \
             LUFS, so the loudness bought with limiting buys nothing there.",
            -g.db, p.platform.target_lufs
        ));
    } else if g.peak_limited {
        out.push(format!(
            "{name} would turn it up by {:.1} dB but stops at {:.1} dB, where its peaks reach \
             the ceiling: it plays {:.1} dB quieter than louder masters.",
            g.wanted_db,
            g.db,
            g.wanted_db - g.db
        ));
    } else if g.wanted_db >= 1.0 && g.db == 0.0 {
        out.push(format!(
            "{name} does not turn quiet masters up: this one plays {:.1} dB under its {:.0} LUFS.",
            g.wanted_db, p.platform.target_lufs
        ));
    }
    if let Some(a) = p.artefacts_db
        && a > -24.0
    {
        out.push(format!(
            "What the codec changes is loud ({} dB against the mix): listen to it with \
             Codec Monitor → Difference (dense highs, wide reverbs and noise cost bits).",
            signed(a)
        ));
    }
    if !p.platform.native {
        out.push(format!(
            "{} has no encoder under a permissive licence: Opus at {} kb/s stands in; the \
             artefacts differ in kind, their level and the peaks compare.",
            p.platform.codec, p.platform.kbps
        ));
    }
    out
}

/// One platform's run: the codec over the whole source, measured at the
/// platform's gain and at unity.
fn run_platform(
    src: &dyn Source,
    groups: &[(usize, usize)],
    master: &Master,
    platform: &'static Platform,
    kind: CoderKind,
    stop: &AtomicBool,
    done: &AtomicU64,
) -> Result<PlatformPreview, CodecError> {
    let channels = src.channels();
    let gain = platform.gain(master.integrated, master.true_peak);
    let g = faderframe_core::db_to_gain(gain.db as f32);
    let mut chain = CodecChain::new(
        channels,
        src.rate(),
        groups,
        CodecSettings {
            kind,
            kbps: platform.kbps,
            vbr: platform.vbr,
        },
    )?;
    let delay = chain.delay() as u64;
    let mut heard = CodecMeter::new(channels);
    let mut plain = CodecMeter::new(channels);
    let mut bufs = vec![vec![0.0f32; CHUNK]; channels];
    let total = master.frames + delay;
    let mut at = 0u64;
    while at < total {
        if stop.load(Ordering::Relaxed) {
            return Err(CodecError::Cancelled);
        }
        let n = (total - at).min(CHUNK as u64) as usize;
        {
            let mut views: Vec<&mut [f32]> = bufs.iter_mut().map(|b| &mut b[..n]).collect();
            src.read(at, &mut views).map_err(CodecError::Read)?;
        }
        chain.process(n, |c| &bufs[c][..n]);
        // The first `delay` frames out are the codec's start, not the
        // master.
        let skip = delay.saturating_sub(at).min(n as u64) as usize;
        if skip < n {
            let k = n - skip;
            heard.add(
                k,
                |c| &chain.wet(c)[skip..],
                |c| &chain.dry(c)[skip..],
                g,
                g,
            );
            plain.add(
                k,
                |c| &chain.wet(c)[skip..],
                |c| &chain.dry(c)[skip..],
                1.0,
                1.0,
            );
        }
        at += n as u64;
        done.fetch_add(n as u64, Ordering::Relaxed);
    }
    let heard_true_peak = heard.peak_dbtp();
    let mut p = PlatformPreview {
        platform,
        master: *master,
        gain,
        decoded_true_peak: plain.peak_dbtp(),
        heard_true_peak,
        overs: heard.overs,
        overs_unnormalised: plain.overs,
        artefacts_db: heard.difference_db(),
        risk: Risk::of(heard_true_peak),
        advice: Vec::new(),
    };
    p.advice = advise(&p);
    Ok(p)
}

/// The frames a preview of `platforms` reads in all (for its progress:
/// the master once, each platform once with the codec's delay).
pub fn work(master_frames: u64, platforms: usize) -> u64 {
    master_frames * (1 + platforms as u64) + platforms as u64 * 4096
}

/// Code `src` (measured as `master`) as each of `platforms` streams it,
/// with substreams `groups` ([`crate::substreams`]). `done` counts frames
/// read.
pub fn preview(
    src: &dyn Source,
    groups: &[(usize, usize)],
    master: &Master,
    platforms: &[&'static Platform],
    kind: CoderKind,
    stop: &AtomicBool,
    done: &AtomicU64,
) -> Result<Vec<PlatformPreview>, CodecError> {
    let workers = std::thread::available_parallelism()
        .map_or(2, std::num::NonZero::get)
        .clamp(1, platforms.len().max(1));
    let mut out = Vec::with_capacity(platforms.len());
    for batch in platforms.chunks(workers) {
        let results: Vec<Result<PlatformPreview, CodecError>> = std::thread::scope(|s| {
            let handles: Vec<_> = batch
                .iter()
                .map(|&p| s.spawn(move || run_platform(src, groups, master, p, kind, stop, done)))
                .collect();
            handles
                .into_iter()
                .map(|h| {
                    h.join()
                        .unwrap_or_else(|_| Err(CodecError::Opus("a coding thread failed".into())))
                })
                .collect()
        });
        for r in results {
            out.push(r?);
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::platform::platform;

    /// A quiet master (about −18 LUFS) whose samples stay under full
    /// scale but whose waveform overshoots between them: 5 ms bursts of a
    /// tone at a quarter of the rate, 45° off its peaks, once a second
    /// over a bed of tones.
    fn overshooting(rate: u32, seconds: f64) -> Vec<Vec<f32>> {
        let n = (f64::from(rate) * seconds) as usize;
        let l: Vec<f32> = (0..n)
            .map(|i| {
                let t = i as f64 / f64::from(rate);
                let burst = if t.fract() < 0.005 {
                    1.0 * (std::f64::consts::FRAC_PI_2 * i as f64 + std::f64::consts::FRAC_PI_4)
                        .sin()
                } else {
                    0.0
                };
                (burst
                    + 0.08 * (std::f64::consts::TAU * 220.0 * t).sin()
                    + 0.04 * (std::f64::consts::TAU * 3_100.0 * t).sin()) as f32
            })
            .collect();
        vec![l.clone(), l]
    }

    #[test]
    fn identity_coding_keeps_the_master_and_reports_its_gain() {
        let audio = overshooting(48_000, 4.0);
        let src = Planar {
            channels: &audio,
            rate: 48_000,
        };
        let stop = AtomicBool::new(false);
        let done = AtomicU64::new(0);
        let master = measure_master(&src, &[1.0, 1.0], &stop, &done).unwrap();
        // The samples stay under full scale, the waveform does not.
        let sample_peak = audio[0].iter().fold(0.0f32, |m, v| m.max(v.abs()));
        assert!(sample_peak < 0.97);
        assert!(master.true_peak > 0.0, "{}", master.true_peak);
        let ps: Vec<&'static Platform> = ["youtube", "tidal"]
            .iter()
            .filter_map(|id| platform(id))
            .collect();
        let out = preview(
            &src,
            &[(0, 2)],
            &master,
            &ps,
            CoderKind::Identity,
            &stop,
            &done,
        )
        .unwrap();
        assert_eq!(out.len(), 2);
        for p in &out {
            // Nothing coded: the decoded peak is the master's, the
            // difference nothing.
            assert!((p.decoded_true_peak - master.true_peak).abs() < 0.01);
            assert_eq!(p.artefacts_db, Some(f64::NEG_INFINITY));
            let g = p.platform.gain(master.integrated, master.true_peak);
            assert_eq!(p.gain, g);
            assert!((p.heard_true_peak - (master.true_peak + g.db)).abs() < 0.02);
        }
    }

    #[cfg(not(faderframe_check_only))]
    #[test]
    fn decoding_a_hot_master_finds_its_overs_and_says_to_lower_the_ceiling() {
        let audio = overshooting(44_100, 3.0);
        let src = Planar {
            channels: &audio,
            rate: 44_100,
        };
        let stop = AtomicBool::new(false);
        let done = AtomicU64::new(0);
        let master = measure_master(&src, &[1.0, 1.0], &stop, &done).unwrap();
        let ps = [platform("deezer").unwrap()];
        let out = preview(&src, &[(0, 2)], &master, &ps, CoderKind::Opus, &stop, &done).unwrap();
        let p = &out[0];
        // Quiet: Deezer leaves it as it is, so the decoded overs stay.
        assert_eq!(p.gain.db, 0.0);
        assert!(p.decoded_true_peak > 0.0, "{}", p.decoded_true_peak);
        assert!(p.overs > 0 && p.overs == p.overs_unnormalised);
        assert_eq!(p.risk, Risk::Clips);
        assert!(
            p.advice
                .iter()
                .any(|a| a.contains("Lower the limiter's ceiling")),
            "{:?}",
            p.advice
        );
        assert!(p.advice.iter().any(|a| a.contains("stands in")));
        let a = p.artefacts_db.unwrap();
        assert!(a < -6.0 && a > -80.0, "artefacts {a:.1} dB");
    }

    #[test]
    fn a_preview_stops_when_asked() {
        let audio = overshooting(48_000, 1.0);
        let src = Planar {
            channels: &audio,
            rate: 48_000,
        };
        let stop = AtomicBool::new(true);
        let done = AtomicU64::new(0);
        assert!(matches!(
            measure_master(&src, &[1.0, 1.0], &stop, &done),
            Err(CodecError::Cancelled)
        ));
    }
}
