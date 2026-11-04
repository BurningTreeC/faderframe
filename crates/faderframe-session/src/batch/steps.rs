//! The steps' processing on a file's audio in memory (plugins are in
//! [`super::plugins`]).

use super::{Channels, ConvertStep, FadeStep, LoudnessStep, NormalizeStep, PeakKind, TrimStep};
use faderframe_analysis::delivery::{self, PeakHandling};
use faderframe_analysis::silence::{Levels, regions};
use faderframe_analysis::{TP_LATENCY, TruePeak};
use std::sync::atomic::{AtomicBool, Ordering};

/// A file's audio as the steps pass it on: planar, at `rate`.
#[derive(Clone, Debug, Default, PartialEq)]
pub(super) struct Audio {
    pub channels: Vec<Vec<f32>>,
    pub rate: u32,
}

impl Audio {
    pub fn frames(&self) -> usize {
        self.channels.iter().map(Vec::len).min().unwrap_or(0)
    }

    pub fn seconds(&self) -> f64 {
        self.frames() as f64 / f64::from(self.rate.max(1))
    }
}

/// Why a step did not finish.
#[derive(Debug)]
pub(super) enum StepError {
    Cancelled,
    Failed(String),
}

/// A step's way to say how far it is and to hear of a cancel.
pub(super) struct Work<'a> {
    pub cancel: &'a AtomicBool,
    pub progress: &'a mut dyn FnMut(f32),
    /// What the step did worth saying (gain applied, time trimmed).
    pub notes: &'a mut Vec<String>,
}

impl Work<'_> {
    pub fn check(&self) -> Result<(), StepError> {
        if self.cancel.load(Ordering::Relaxed) {
            Err(StepError::Cancelled)
        } else {
            Ok(())
        }
    }

    pub fn report(&mut self, fraction: f32) {
        (self.progress)(fraction.clamp(0.0, 1.0));
    }

    pub fn note(&mut self, text: String) {
        self.notes.push(text);
    }
}

/// "+3.2 dB" / "−1.5 dB".
fn gain_text(db: f64) -> String {
    let t = format!("{db:+.1} dB");
    t.replacen('-', "\u{2212}", 1)
}

fn db(v: f64) -> f64 {
    if v > 0.0 {
        20.0 * v.log10()
    } else {
        f64::NEG_INFINITY
    }
}

/// The highest sample of any channel (linear).
pub(super) fn sample_peak(audio: &Audio) -> f64 {
    audio
        .channels
        .iter()
        .flatten()
        .fold(0.0f32, |m, s| m.max(s.abs()))
        .into()
}

/// The highest true peak of any channel (linear; 4× oversampled as
/// BS.1770 has it, never under the sample peak).
pub(super) fn true_peak(audio: &Audio) -> f64 {
    let mut peak = 0.0f64;
    for ch in &audio.channels {
        let mut tp = TruePeak::new();
        for &s in ch.iter().chain(std::iter::repeat_n(&0.0, TP_LATENCY)) {
            peak = peak.max(tp.run(s)).max(f64::from(s.abs()));
        }
    }
    peak
}

pub(super) fn normalize(
    audio: &mut Audio,
    step: &NormalizeStep,
    work: &mut Work<'_>,
) -> Result<(), StepError> {
    let peak = match step.peak {
        PeakKind::Sample => sample_peak(audio),
        PeakKind::True => true_peak(audio),
    };
    work.check()?;
    if peak < 1e-9 {
        work.note("silent: not normalised".into());
        return Ok(());
    }
    let gain_db = f64::from(step.ceiling_db) - db(peak);
    delivery::apply_gain(&mut audio.channels, gain_db);
    work.note(format!("normalised by {}", gain_text(gain_db)));
    work.report(1.0);
    Ok(())
}

pub(super) fn loudness(
    audio: &mut Audio,
    step: &LoudnessStep,
    work: &mut Work<'_>,
) -> Result<(), StepError> {
    if audio.channels.len() > 2 {
        work.note("loudness measured on the first two channels".into());
    }
    let done = delivery::normalize_loudness(
        &mut audio.channels,
        audio.rate,
        f64::from(step.target_lufs),
        f64::from(step.true_peak_db),
        if step.lower_gain {
            PeakHandling::LowerGain
        } else {
            PeakHandling::Limit
        },
    );
    work.check()?;
    if !done.before.integrated.is_finite() {
        work.note("too short or too quiet to measure its loudness: left as it was".into());
        return Ok(());
    }
    let mut text = format!(
        "loudness {:.1} → {:.1} LUFS ({})",
        done.before.integrated,
        done.after.integrated,
        gain_text(done.gain_db)
    );
    if done.limited_db > 0.05 {
        text.push_str(&format!(
            ", peaks limited by up to {:.1} dB",
            done.limited_db
        ));
    }
    work.note(text.replace('-', "\u{2212}"));
    work.report(1.0);
    Ok(())
}

/// Frames resampled at a time.
const CHUNK: usize = 16_384;

pub(super) fn convert(
    audio: &mut Audio,
    step: &ConvertStep,
    work: &mut Work<'_>,
) -> Result<(), StepError> {
    if step.channels == Channels::Mono && audio.channels.len() > 1 {
        let n = audio.channels.len() as f32;
        let frames = audio.frames();
        let mono: Vec<f32> = (0..frames)
            .map(|i| audio.channels.iter().map(|c| c[i]).sum::<f32>() / n)
            .collect();
        work.note(format!("{} channels mixed to mono", audio.channels.len()));
        audio.channels = vec![mono];
    }
    work.check()?;
    if let Some(rate) = step.sample_rate.filter(|r| *r != audio.rate) {
        resample(audio, rate, work)?;
    }
    if step.channels == Channels::Stereo {
        match audio.channels.len() {
            1 => {
                let copy = audio.channels[0].clone();
                audio.channels.push(copy);
                work.note("mono made stereo".into());
            }
            2 => {}
            n => {
                audio.channels.truncate(2);
                work.note(format!("the first two of {n} channels kept"));
            }
        }
    }
    work.report(1.0);
    Ok(())
}

/// To `rate` with the importer's resampler.
fn resample(audio: &mut Audio, rate: u32, work: &mut Work<'_>) -> Result<(), StepError> {
    use faderframe_audio_files::resample::StreamResampler;
    let channels = audio.channels.len().max(1);
    let frames = audio.frames();
    let from = audio.rate;
    let mut r =
        StreamResampler::new(from, rate, channels).map_err(|e| StepError::Failed(e.to_string()))?;
    let expected = (frames as f64 * f64::from(rate) / f64::from(from.max(1))).round() as usize;
    let mut out: Vec<Vec<f32>> = vec![Vec::with_capacity(expected + 1); channels];
    let mut chunk: Vec<Vec<f32>> = vec![Vec::with_capacity(CHUNK); channels];
    let mut sink =
        |planes: &[&[f32]], n: usize| -> Result<(), faderframe_audio_files::import::ImportError> {
            for (o, p) in out.iter_mut().zip(planes) {
                o.extend_from_slice(&p[..n.min(p.len())]);
            }
            Ok(())
        };
    let mut at = 0;
    while at < frames {
        work.check()?;
        let n = CHUNK.min(frames - at);
        for (c, ch) in chunk.iter_mut().enumerate() {
            ch.clear();
            ch.extend_from_slice(&audio.channels[c][at..at + n]);
        }
        r.push(&chunk, n, &mut sink)
            .map_err(|e| StepError::Failed(e.to_string()))?;
        at += n;
        work.report(at as f32 / frames.max(1) as f32);
    }
    r.finish(&mut sink)
        .map_err(|e| StepError::Failed(e.to_string()))?;
    audio.channels = out;
    audio.rate = rate;
    work.note(format!("{from} Hz → {rate} Hz"));
    Ok(())
}

pub(super) fn trim(
    audio: &mut Audio,
    step: &TrimStep,
    work: &mut Work<'_>,
) -> Result<(), StepError> {
    if !step.start && !step.end {
        return Ok(());
    }
    let frames = audio.frames();
    let slices: Vec<&[f32]> = audio.channels.iter().map(|c| &c[..frames]).collect();
    let levels = Levels::measure(&slices);
    work.check()?;
    let spans = regions(&levels, f64::from(audio.rate), &step.settings());
    let (Some(first), Some(last)) = (spans.first(), spans.last()) else {
        work.note(format!(
            "nothing above {:.0} dBFS: not trimmed",
            step.threshold_db
        ));
        return Ok(());
    };
    let start = if step.start {
        first.start.clamp(0, frames as i64) as usize
    } else {
        0
    };
    let end = if step.end {
        (last.end.clamp(0, frames as i64) as usize).max(start)
    } else {
        frames
    };
    if start == 0 && end == frames {
        return Ok(());
    }
    for ch in &mut audio.channels {
        ch.truncate(end);
        ch.drain(..start);
    }
    let rate = f64::from(audio.rate.max(1));
    work.note(format!(
        "trimmed {:.2} s from the start, {:.2} s from the end",
        start as f64 / rate,
        (frames - end) as f64 / rate
    ));
    work.report(1.0);
    Ok(())
}

pub(super) fn fade(
    audio: &mut Audio,
    step: &FadeStep,
    work: &mut Work<'_>,
) -> Result<(), StepError> {
    let frames = audio.frames();
    let rate = f64::from(audio.rate.max(1));
    let mut fade_in = (step.fade_in.max(0.0) * rate).round() as usize;
    let mut fade_out = (step.fade_out.max(0.0) * rate).round() as usize;
    // Fades longer than the file share it in their proportion.
    if fade_in + fade_out > frames {
        let total = (fade_in + fade_out) as f64;
        fade_in = (frames as f64 * fade_in as f64 / total).floor() as usize;
        fade_out = frames - fade_in;
        work.note("the fades were shortened to fit the file".into());
    }
    for ch in &mut audio.channels {
        for (i, s) in ch.iter_mut().take(fade_in).enumerate() {
            *s *= step.shape_in.gain(i as f32 / fade_in as f32);
        }
        let len = ch.len().min(frames);
        for (k, s) in ch[len - fade_out..len].iter_mut().enumerate() {
            // The last frame silent.
            *s *= step
                .shape_out
                .gain((fade_out - 1 - k) as f32 / fade_out as f32);
        }
    }
    work.report(1.0);
    Ok(())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    fn work<'a>(
        cancel: &'a AtomicBool,
        progress: &'a mut dyn FnMut(f32),
        notes: &'a mut Vec<String>,
    ) -> Work<'a> {
        Work {
            cancel,
            progress,
            notes,
        }
    }

    fn tone(rate: u32, seconds: f64, amp: f32) -> Vec<f32> {
        (0..(seconds * f64::from(rate)) as usize)
            .map(|i| amp * (i as f32 * 997.0 * std::f32::consts::TAU / rate as f32).sin())
            .collect()
    }

    #[test]
    fn a_fade_starts_and_ends_in_silence() {
        let mut audio = Audio {
            channels: vec![vec![1.0; 1000]],
            rate: 1000,
        };
        let (cancel, mut notes) = (AtomicBool::new(false), Vec::new());
        let mut p = |_| {};
        let step = FadeStep {
            fade_in: 0.1,
            fade_out: 0.2,
            ..FadeStep::default()
        };
        fade(&mut audio, &step, &mut work(&cancel, &mut p, &mut notes)).unwrap();
        let c = &audio.channels[0];
        assert_eq!(c[0], 0.0);
        assert!((c[50] - 0.5).abs() < 1e-6);
        assert_eq!(c[100], 1.0);
        assert_eq!(c[799], 1.0);
        assert_eq!(c[999], 0.0);
        assert!(c[800] < 1.0 && c[800] > 0.99);
    }

    #[test]
    fn fades_longer_than_the_file_share_it() {
        let mut audio = Audio {
            channels: vec![vec![1.0; 100]],
            rate: 1000,
        };
        let (cancel, mut notes) = (AtomicBool::new(false), Vec::new());
        let mut p = |_| {};
        let step = FadeStep {
            fade_in: 1.0,
            fade_out: 1.0,
            ..FadeStep::default()
        };
        fade(&mut audio, &step, &mut work(&cancel, &mut p, &mut notes)).unwrap();
        assert_eq!(audio.channels[0][0], 0.0);
        assert_eq!(audio.channels[0][99], 0.0);
        assert_eq!(notes.len(), 1);
    }

    #[test]
    fn the_true_peak_is_found_between_samples() {
        // A tone at a quarter of the rate, sampled 45° off its peaks:
        // samples at 0.707, the wave at 1.
        let x: Vec<f32> = (0..4800)
            .map(|i| (std::f32::consts::FRAC_PI_2 * i as f32 + std::f32::consts::FRAC_PI_4).sin())
            .collect();
        let audio = Audio {
            channels: vec![x],
            rate: 48_000,
        };
        assert!((sample_peak(&audio) - std::f64::consts::FRAC_1_SQRT_2).abs() < 1e-3);
        assert!(true_peak(&audio) > 0.95);
    }

    #[test]
    fn resampling_keeps_the_length_and_the_tone() {
        let mut audio = Audio {
            channels: vec![tone(48_000, 1.0, 0.5), tone(48_000, 1.0, 0.25)],
            rate: 48_000,
        };
        let (cancel, mut notes) = (AtomicBool::new(false), Vec::new());
        let mut p = |_| {};
        let step = ConvertStep {
            sample_rate: Some(44_100),
            channels: Channels::Keep,
        };
        convert(&mut audio, &step, &mut work(&cancel, &mut p, &mut notes)).unwrap();
        assert_eq!(audio.rate, 44_100);
        assert_eq!(audio.channels.len(), 2);
        assert_eq!(audio.frames(), 44_100);
        let mid = &audio.channels[0][10_000..30_000];
        let peak = mid.iter().fold(0.0f32, |m, s| m.max(s.abs()));
        assert!((peak - 0.5).abs() < 0.01, "{peak}");
    }
}
