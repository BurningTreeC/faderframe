//! The null test of a stems delivery: the stems that make up the mix
//! (the tracks no other track feeds — a bus's or return's stem holds its
//! sources again) summed and compared with the mix rendered with the same
//! settings. The residual is told in numbers ([`NullFigures`]), as an
//! outcome against what rounding and dither alone would leave
//! ([`NullOutcome`], [`Precision::bound`]), and explained: the causes the
//! project shows (shared non-linear devices, sidechains, level processing
//! per stem, dither) and those the residual shows (an offset in time, a
//! difference only in the tail).

use crate::report::{QcFile, Verdict, finite, level, time};
use faderframe_analysis::qc::NullLevels;
use serde::{Deserialize, Serialize};

/// A residual peak under this nulls whatever the format (dBFS): the
/// rounding of floating-point arithmetic through a mix's devices.
pub const FLOAT_NULL_DB: f64 = -100.0;
/// A residual peak under this does not null but is inaudible within the
/// mix (dBFS).
pub const NEAR_NULL_DB: f64 = -60.0;

/// How the stems compare with the mix.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum NullOutcome {
    /// To the format's rounding (or floating-point arithmetic).
    Exact,
    /// To the dither every file carries.
    DitherFloor,
    /// Not a null, but more than 60 dB down: inaudible within the mix.
    Near,
    Fails,
}

impl NullOutcome {
    pub fn verdict(self) -> Verdict {
        match self {
            NullOutcome::Exact | NullOutcome::DitherFloor => Verdict::Pass,
            NullOutcome::Near => Verdict::Warn,
            NullOutcome::Fails => Verdict::Fail,
        }
    }
}

/// The dither the files were written with.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DitherKind {
    Off,
    Tpdf,
    Shaped,
}

/// What the files' format leaves of a perfect null.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Precision {
    /// Files compared: the stems summed and the mix.
    pub files: usize,
    /// Fixed-point bits (`None`: floating point).
    pub bits: Option<u16>,
    pub dither: DitherKind,
}

impl Precision {
    fn lsb(&self) -> Option<f64> {
        self.bits.map(|b| 2f64.powi(-(i32::from(b) - 1)))
    }

    /// The highest residual (linear) rounding alone leaves, and with the
    /// dither: each file's error at most half an LSB, TPDF dither adds an
    /// LSB, noise shaping about ten.
    pub fn bound(&self) -> (f64, f64) {
        let float = 10f64.powf(FLOAT_NULL_DB / 20.0);
        let Some(lsb) = self.lsb() else {
            return (float, float);
        };
        let rounding = (self.files.max(1) as f64 * 0.5 * lsb).max(float);
        (rounding, self.dither_peak().max(rounding))
    }

    /// The highest residual the files' rounding and dither alone leave
    /// (linear; 0 for floating point).
    pub fn dither_peak(&self) -> f64 {
        let per_file = match self.dither {
            DitherKind::Off => 0.5,
            DitherKind::Tpdf => 1.5,
            DitherKind::Shaped => 12.0,
        };
        self.lsb()
            .map_or(0.0, |lsb| self.files.max(1) as f64 * per_file * lsb)
    }
}

/// The residual in numbers (times in seconds into the file).
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct NullFigures {
    pub peak_dbfs: Option<f64>,
    pub rms_dbfs: Option<f64>,
    pub mix_rms_dbfs: Option<f64>,
    /// How far the residual's RMS is under the mix's (dB; `None`: it
    /// nulls exactly).
    pub depth_db: Option<f64>,
    /// Where the residual first rises above the format's bound and where
    /// it is worst (and on which channel, from 1).
    pub first_at: Option<f64>,
    pub worst_at: Option<f64>,
    pub worst_channel: usize,
    /// The highest residual within the range and in the tail after it.
    pub range_peak_dbfs: Option<f64>,
    pub tail_peak_dbfs: Option<f64>,
    pub seconds: f64,
}

impl NullFigures {
    pub fn of(l: &NullLevels, sample_rate: u32) -> Self {
        let rate = f64::from(sample_rate.max(1));
        let peak = finite(l.peak);
        Self {
            peak_dbfs: peak,
            rms_dbfs: finite(l.rms),
            mix_rms_dbfs: finite(l.mix_rms),
            depth_db: finite(l.depth()),
            first_at: l.first_at.map(|f| f as f64 / rate),
            worst_at: peak.map(|_| l.peak_at as f64 / rate),
            worst_channel: l.peak_channel + 1,
            range_peak_dbfs: finite(l.range_peak),
            tail_peak_dbfs: finite(l.tail_peak),
            seconds: l.frames as f64 / rate,
        }
    }
}

/// The outcome of a residual for files of `precision`.
pub fn outcome(l: &NullLevels, precision: &Precision) -> NullOutcome {
    let peak = if l.peak.is_finite() {
        10f64.powf(l.peak / 20.0)
    } else {
        0.0
    };
    let (rounding, dither) = precision.bound();
    if peak <= rounding * 1.000_001 {
        NullOutcome::Exact
    } else if precision.dither != DitherKind::Off && peak <= dither * 1.000_001 {
        NullOutcome::DitherFloor
    } else if l.peak <= NEAR_NULL_DB {
        NullOutcome::Near
    } else {
        NullOutcome::Fails
    }
}

/// Why stems would not (or did not) null.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum CauseKind {
    /// A non-linear device several stems pass through (a limiter or
    /// compressor on the master, saturation on a bus).
    NonLinear,
    /// A device FaderFrame cannot see into (a plug-in) where several
    /// stems pass.
    MaybeNonLinear,
    /// A device keyed by a bus or return: in a stem the solo silences
    /// what feeds the key.
    Sidechain,
    /// Normalisation or loudness finishing per stem.
    LevelPerStem,
    /// Each file's dither (expected; far under audibility).
    Dither,
    /// A track fed by others that plays clips of its own: in no source's
    /// stem.
    FedTrack,
    /// The residual lies in the tail after the range.
    Tail,
    /// The stems are offset in time against the mix.
    Latency,
    /// Something renders differently each time (random or free-running
    /// modulation in a device).
    Unexplained,
}

/// One cause: what, and what to do about it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Cause {
    pub kind: CauseKind,
    pub text: String,
    pub fix: Option<String>,
}

impl Cause {
    /// `device` (what it is: "a limiter") on `track` processes what
    /// `shared` stems sum there.
    pub fn non_linear(device: &str, what: &str, track: &str, shared: usize) -> Self {
        Self {
            kind: CauseKind::NonLinear,
            text: format!(
                "{device} on {track} is {what}: it reacts to what reaches it, and {shared} stems \
                 reach it alone in their renders but together in the mix"
            ),
            fix: Some(format!(
                "Bypass {device} for the stems, or deliver them with the nulling stem (Shared \
                 Processing)"
            )),
        }
    }

    pub fn maybe_non_linear(device: &str, track: &str, shared: usize) -> Self {
        Self {
            kind: CauseKind::MaybeNonLinear,
            text: format!(
                "{device} on {track} is a plug-in FaderFrame cannot see into; if it compresses, \
                 limits or saturates, the {shared} stems that reach it do not sum to the mix"
            ),
            fix: None,
        }
    }

    pub fn sidechain(device: &str, track: &str, key: &str) -> Self {
        Self {
            kind: CauseKind::Sidechain,
            text: format!(
                "{device} on {track} is keyed by {key}, which other tracks feed: in a stem the \
                 solo silences what reaches {key}, so it acts differently than in the mix"
            ),
            fix: Some(format!(
                "Key {device} from a track of its own (keys are taken before the fader, mute and \
                 solo) or bypass it"
            )),
        }
    }

    pub fn level_per_stem(what: &str) -> Self {
        Self {
            kind: CauseKind::LevelPerStem,
            text: format!("Each stem is {what} on its own, so their levels no longer add up"),
            fix: Some(
                "Render the stems with the level unchanged (or with the nulling stem, which \
                       applies the mix's gain to every stem)"
                    .into(),
            ),
        }
    }

    pub fn dither(files: usize, floor_db: f64) -> Self {
        Self {
            kind: CauseKind::Dither,
            text: format!(
                "Each of the {files} files carries its own dither: up to {} remains in the sum — \
                 far under audibility",
                level(Some(floor_db), "dBFS")
            ),
            fix: None,
        }
    }

    pub fn fed_track(track: &str) -> Self {
        Self {
            kind: CauseKind::FedTrack,
            text: format!(
                "{track} is fed by other tracks and plays clips of its own: its clips are in no \
                 source's stem"
            ),
            fix: Some(format!("Move the clips of {track} to a track of their own")),
        }
    }

    pub fn tail(range_end: f64) -> Self {
        Self {
            kind: CauseKind::Tail,
            text: format!(
                "The difference lies in the tail after the range ({}): devices ring on \
                 differently once the music stops",
                time(range_end)
            ),
            fix: Some("Render with a longer tail, or trim the files at the range's end".into()),
        }
    }

    pub fn latency(frames: i64, sample_rate: u32) -> Self {
        let ms = frames as f64 * 1000.0 / f64::from(sample_rate.max(1));
        Self {
            kind: CauseKind::Latency,
            text: format!(
                "The stems are {} {} frames ({:.2} ms) against the mix: a device's latency is \
                 not compensated the same way in both",
                if frames > 0 { "late by" } else { "early by" },
                frames.abs(),
                ms.abs()
            ),
            fix: Some("Check plug-ins whose reported latency changes".into()),
        }
    }

    pub fn unexplained(candidates: &[String]) -> Self {
        Self {
            kind: CauseKind::Unexplained,
            text: if candidates.is_empty() {
                "Something renders differently each time — random or free-running modulation in \
                 a device"
                    .into()
            } else {
                format!(
                    "Something renders differently each time — random or free-running \
                     modulation in a device; candidates: {}",
                    candidates.join(", ")
                )
            },
            fix: None,
        }
    }
}

/// A stem left out of the sum, and why.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LeftOut {
    pub name: String,
    pub why: String,
}

/// The null test as reported (see the module).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct NullTest {
    /// The files summed.
    pub summed: Vec<String>,
    pub left_out: Vec<LeftOut>,
    /// The nulling stem among them, when the stems were made to null.
    pub shared_stem: Option<String>,
    /// What the mix was.
    pub reference: String,
    pub residual: NullFigures,
    /// What rounding and dither alone would leave at most (dBFS).
    pub floor_dbfs: Option<f64>,
    pub outcome: NullOutcome,
    pub verdict: Verdict,
    /// One sentence.
    pub summary: String,
    pub causes: Vec<Cause>,
    /// An offset that would null them better (frames; positive: late).
    pub lag_frames: Option<i64>,
    /// The mix itself, measured and checked as the programme.
    pub mix: Option<QcFile>,
}

fn db(v: f64) -> f64 {
    20.0 * v.max(1e-300).log10()
}

impl NullTest {
    /// The null test of a residual: its figures, outcome and causes —
    /// `causes` what the project shows; what the residual shows
    /// (`lag`: `best_lag`'s answer at the worst place; the range's end in
    /// frames) is added here.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        levels: &NullLevels,
        sample_rate: u32,
        precision: Precision,
        range_end: u64,
        lag: Option<(isize, f64)>,
        mut causes: Vec<Cause>,
        summed: Vec<String>,
        left_out: Vec<LeftOut>,
        reference: String,
    ) -> Self {
        let outcome = outcome(levels, &precision);
        let (rounding, dither) = precision.bound();
        let floor = if precision.dither == DitherKind::Off {
            rounding
        } else {
            dither
        };
        let figures = NullFigures::of(levels, sample_rate);
        // What the residual itself shows.
        let mut lag_frames = None;
        if outcome != NullOutcome::Exact {
            if let Some((frames, left)) = lag
                && frames != 0
                && left < 0.25
            {
                lag_frames = Some(frames as i64);
                causes.insert(0, Cause::latency(frames as i64, sample_rate));
            }
            let in_tail = levels.tail_peak > levels.range_peak + 6.0
                && levels.range_peak <= 20.0 * floor.log10() + 0.5;
            if in_tail {
                causes.push(Cause::tail(
                    range_end as f64 / f64::from(sample_rate.max(1)),
                ));
            }
        }
        if precision.dither != DitherKind::Off
            && precision.bits.is_some()
            && !causes.iter().any(|c| c.kind == CauseKind::Dither)
        {
            causes.push(Cause::dither(precision.files, db(precision.dither_peak())));
        }
        let explained = causes
            .iter()
            .any(|c| !matches!(c.kind, CauseKind::Dither | CauseKind::Unexplained));
        if matches!(outcome, NullOutcome::Near | NullOutcome::Fails) && !explained {
            causes.push(Cause::unexplained(&[]));
        }
        let summary = summary(outcome, &figures, summed.len());
        Self {
            summed,
            left_out,
            shared_stem: None,
            reference,
            residual: figures,
            floor_dbfs: Some(db(floor)),
            outcome,
            verdict: outcome.verdict(),
            summary,
            causes,
            lag_frames,
            mix: None,
        }
    }

    /// Only the causes that explain why it does not null (the dither
    /// note aside) — what to show when it nulls.
    pub fn problems(&self) -> impl Iterator<Item = &Cause> {
        self.causes.iter().filter(|c| c.kind != CauseKind::Dither)
    }
}

/// One sentence for the outcome.
pub fn summary(outcome: NullOutcome, f: &NullFigures, stems: usize) -> String {
    let peak = level(f.peak_dbfs, "dBFS");
    match outcome {
        NullOutcome::Exact if f.peak_dbfs.is_none() => {
            format!("The {stems} stems sum to the mix exactly: nothing is left")
        }
        NullOutcome::Exact => format!(
            "The {stems} stems sum to the mix: the residual peaks at {peak} — the format's \
             rounding"
        ),
        NullOutcome::DitherFloor => {
            format!("The {stems} stems sum to the mix to the dither: the residual peaks at {peak}")
        }
        NullOutcome::Near => format!(
            "The {stems} stems sum to the mix within {} (residual peak {peak}, {:.0} dB under \
             the mix) — inaudible, but not a null",
            level(Some(NEAR_NULL_DB), "dBFS"),
            f.depth_db.unwrap_or(0.0)
        ),
        NullOutcome::Fails => format!(
            "The {stems} stems do not sum to the mix: the residual peaks at {peak}{}, {} RMS \
             ({:.0} dB under the mix)",
            f.worst_at
                .map_or_else(String::new, |t| format!(" at {}", time(t))),
            level(f.rms_dbfs, "dBFS"),
            f.depth_db.unwrap_or(0.0)
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn levels(peak: f64, range: f64, tail: f64) -> NullLevels {
        NullLevels {
            frames: 480_000,
            peak,
            peak_at: 96_000,
            peak_channel: 1,
            rms: peak - 10.0,
            mix_rms: -18.0,
            first_at: peak.is_finite().then_some(48_000),
            range_peak: range,
            tail_peak: tail,
        }
    }

    #[test]
    fn outcomes_follow_the_formats_precision() {
        let float = Precision {
            files: 5,
            bits: None,
            dither: DitherKind::Off,
        };
        assert_eq!(
            outcome(
                &levels(f64::NEG_INFINITY, f64::NEG_INFINITY, f64::NEG_INFINITY),
                &float
            ),
            NullOutcome::Exact
        );
        assert_eq!(
            outcome(&levels(-130.0, -130.0, -140.0), &float),
            NullOutcome::Exact
        );
        assert_eq!(
            outcome(&levels(-70.0, -70.0, -80.0), &float),
            NullOutcome::Near
        );
        assert_eq!(
            outcome(&levels(-12.0, -12.0, -20.0), &float),
            NullOutcome::Fails
        );
        // 16-bit, TPDF: five files' dither is about −72 dBFS at most.
        let pcm16 = Precision {
            files: 5,
            bits: Some(16),
            dither: DitherKind::Tpdf,
        };
        let (rounding, dither) = pcm16.bound();
        assert!((db(rounding) - db(2.5 / 32_768.0)).abs() < 1e-9);
        assert!((db(dither) - db(7.5 / 32_768.0)).abs() < 1e-9);
        assert_eq!(
            outcome(&levels(-80.0, -80.0, -90.0), &pcm16),
            NullOutcome::DitherFloor
        );
        assert_eq!(
            outcome(&levels(-65.0, -65.0, -90.0), &pcm16),
            NullOutcome::Near
        );
        assert_eq!(NullOutcome::Fails.verdict(), Verdict::Fail);
    }

    #[test]
    fn the_residual_explains_itself() {
        let p = Precision {
            files: 3,
            bits: None,
            dither: DitherKind::Off,
        };
        // Late stems.
        let t = NullTest::new(
            &levels(-20.0, -20.0, -30.0),
            48_000,
            p,
            470_000,
            Some((12, 0.01)),
            Vec::new(),
            vec!["a".into(), "b".into()],
            Vec::new(),
            "the mix".into(),
        );
        assert_eq!(t.outcome, NullOutcome::Fails);
        assert_eq!(t.lag_frames, Some(12));
        assert_eq!(t.causes[0].kind, CauseKind::Latency);
        assert!(t.summary.contains("do not sum"), "{}", t.summary);
        assert_eq!(t.residual.worst_channel, 2);
        assert_eq!(t.residual.worst_at, Some(2.0));
        // Only in the tail.
        let t = NullTest::new(
            &levels(-40.0, -140.0, -40.0),
            48_000,
            p,
            470_000,
            None,
            Vec::new(),
            vec!["a".into()],
            Vec::new(),
            "the mix".into(),
        );
        assert!(t.causes.iter().any(|c| c.kind == CauseKind::Tail));
        // Nothing to blame: unexplained.
        let t = NullTest::new(
            &levels(-30.0, -30.0, -30.0),
            48_000,
            p,
            470_000,
            None,
            Vec::new(),
            vec!["a".into()],
            Vec::new(),
            "the mix".into(),
        );
        assert_eq!(
            t.causes.last().map(|c| c.kind),
            Some(CauseKind::Unexplained)
        );
        // A known cause stands alone.
        let t = NullTest::new(
            &levels(-10.0, -10.0, -30.0),
            48_000,
            p,
            470_000,
            None,
            vec![Cause::non_linear("Limiter 1", "a limiter", "the master", 4)],
            vec!["a".into()],
            Vec::new(),
            "the mix".into(),
        );
        assert_eq!(t.causes.len(), 1);
        assert!(t.causes[0].text.contains("4 stems"));
        // A dithered null names the dither and nothing else wrong.
        let t = NullTest::new(
            &levels(-82.0, -82.0, -95.0),
            48_000,
            Precision {
                files: 3,
                bits: Some(16),
                dither: DitherKind::Tpdf,
            },
            470_000,
            None,
            Vec::new(),
            vec!["a".into(), "b".into()],
            Vec::new(),
            "the mix".into(),
        );
        assert_eq!(t.outcome, NullOutcome::DitherFloor);
        assert_eq!(t.causes.len(), 1);
        assert_eq!(t.problems().count(), 0);
        assert!(
            t.causes[0].text.contains("−77.2 dBFS"),
            "{}",
            t.causes[0].text
        );
    }
}
