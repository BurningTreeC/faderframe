//! The QC report: per delivered file its facts, levels, the checks
//! against the chosen spec and its SHA-256; the stems' null test; the
//! project, program and date. Serialised as JSON as it is
//! ([`QcReport::to_json`], schema [`SCHEMA`]) and laid out as a PDF by
//! [`crate::pdf`].
//!
//! Levels that do not exist (the peak of silence, the loudness of
//! something shorter than 400 ms) are `null` in the JSON, never a made-up
//! number.

use faderframe_analysis::levels::FileLevels;
use faderframe_analysis::qc::{QcLevels, Span};
use serde::{Deserialize, Serialize};

/// The JSON's schema name and version.
pub const SCHEMA: &str = "faderframe.qc/1";

/// How a file or the whole delivery fares.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Verdict {
    /// Measured; nothing is asked of it.
    Info,
    Pass,
    Warn,
    Fail,
}

impl Verdict {
    pub fn label(self) -> &'static str {
        match self {
            Verdict::Info => "info",
            Verdict::Pass => "pass",
            Verdict::Warn => "warn",
            Verdict::Fail => "FAIL",
        }
    }

    /// The worse of the verdicts (`Info` when there are only infos).
    pub fn worst(all: impl IntoIterator<Item = Verdict>) -> Verdict {
        all.into_iter().max().unwrap_or(Verdict::Info)
    }
}

/// What a file is in the delivery: judged as a programme against the
/// spec's loudness, or a part of one (a stem, a vinyl side, the nulling
/// stem) — whose loudness the spec does not judge.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    Programme,
    Stem,
    Part,
}

/// One check: what was measured against what is asked.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Check {
    /// Stable id ("loudness.integrated", "clipping.full_scale", …).
    pub id: String,
    pub label: String,
    pub verdict: Verdict,
    /// "−23.4 LUFS"; "3 runs (first at 1:02.250)".
    pub measured: String,
    /// "−23.0 ±0.5 LUFS"; "—" when nothing is asked.
    pub requirement: String,
    /// The measured number, when it is one (in `unit`).
    pub value: Option<f64>,
    pub unit: Option<String>,
}

/// A file's header facts.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Facts {
    /// "WAV", "RF64", "BWF (ADM)", "IAMF in MP4".
    pub container: String,
    /// "24-bit PCM", "32-bit float".
    pub encoding: String,
    pub sample_rate: u32,
    /// 16, 24 (32 with `float`); `None` for a coded file.
    pub bits: Option<u16>,
    pub float: bool,
    pub channels: usize,
    /// "Stereo", "5.1 (L R C LFE Ls Rs)".
    pub layout: String,
    pub frames: u64,
    pub seconds: f64,
    pub bytes: u64,
    /// The facts come from the file's header (false: the encoder's).
    pub from_file: bool,
}

/// A place in a file (seconds) and how long (seconds), on a channel.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Spot {
    pub at: f64,
    pub seconds: f64,
    pub channel: Option<usize>,
}

impl Spot {
    fn of(s: &Span, rate: f64) -> Self {
        Self {
            at: s.start as f64 / rate,
            seconds: s.frames as f64 / rate,
            channel: s.channel,
        }
    }
}

/// BS.1770 loudness of a file.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Loudness {
    pub integrated_lufs: Option<f64>,
    pub range_lu: f64,
    pub max_momentary_lufs: Option<f64>,
    pub max_short_term_lufs: Option<f64>,
    /// Peak to loudness ratio: true peak over integrated (LU).
    pub plr_lu: Option<f64>,
}

/// One channel's levels.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct ChannelFigures {
    pub sample_peak_dbfs: Option<f64>,
    pub true_peak_dbtp: Option<f64>,
    pub rms_dbfs: Option<f64>,
    /// The mean of the samples (linear) and as a level (dBFS).
    pub dc_offset: f64,
    pub dc_offset_dbfs: Option<f64>,
}

/// A stereo pair's phase.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Stereo {
    pub correlation: f64,
    pub min_correlation: Option<f64>,
    pub min_correlation_at: Option<f64>,
    /// Share (0–1) of the 400 ms windows loud enough to tell that lean
    /// out of phase.
    pub negative_share: f64,
    /// Integrated loudness of the mono fold-down at the stereo's level
    /// and how far it is from the stereo's (LU; −3 for unrelated sides).
    pub mono_lufs: Option<f64>,
    pub mono_change_lu: Option<f64>,
}

/// What a file measures (see `faderframe_analysis::{levels, qc}`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Levels {
    pub sample_peak_dbfs: Option<f64>,
    pub sample_peak_at: Option<f64>,
    pub true_peak_dbtp: Option<f64>,
    pub true_peak_at: Option<f64>,
    pub rms_dbfs: Option<f64>,
    /// `None`: not measured (object audio, an unknown layout).
    pub loudness: Option<Loudness>,
    pub channels: Vec<ChannelFigures>,
    /// Samples at (or past) the format's full scale, past 0 dBFS (float),
    /// and runs of three or more at full scale (clipped).
    pub full_scale_samples: u64,
    pub above_full_scale: u64,
    pub clipped_runs: u64,
    pub clip_runs: Vec<Spot>,
    pub longest_clip_run: Option<Spot>,
    /// Runs of the 4× reconstructed signal past 0 dBFS.
    pub inter_sample_overs: u64,
    pub first_inter_sample_over_at: Option<f64>,
    /// What counts as silence (dBFS), and the silence at either end
    /// (seconds; `None`: not measured).
    pub silence_floor_dbfs: Option<f64>,
    pub head_silence: Option<f64>,
    pub tail_silence: Option<f64>,
    pub silent: bool,
    /// Digital silence of a second or more inside (the first listed).
    pub gaps: Vec<Spot>,
    pub gap_count: u64,
    pub stereo: Option<Stereo>,
}

pub(crate) fn finite(v: f64) -> Option<f64> {
    v.is_finite().then_some(v)
}

fn db(v: f64) -> Option<f64> {
    (v > 0.0).then(|| 20.0 * v.log10())
}

impl Levels {
    /// From a file's levels and (when measured) its QC levels, at
    /// `sample_rate`.
    pub fn of(levels: &FileLevels, qc: Option<&QcLevels>, sample_rate: u32) -> Self {
        let rate = f64::from(sample_rate.max(1));
        let integrated = levels.loudness.and_then(|l| finite(l.integrated));
        let loudness = levels.loudness.map(|l| Loudness {
            integrated_lufs: finite(l.integrated),
            range_lu: l.range,
            max_momentary_lufs: finite(l.max_momentary),
            max_short_term_lufs: finite(l.max_short_term),
            plr_lu: integrated
                .zip(finite(levels.true_peak))
                .map(|(i, tp)| tp - i),
        });
        let sound = levels.sample_peak.is_finite();
        Self {
            sample_peak_dbfs: finite(levels.sample_peak),
            sample_peak_at: sound.then(|| levels.sample_peak_at as f64 / rate),
            true_peak_dbtp: finite(levels.true_peak),
            true_peak_at: sound.then(|| levels.true_peak_at as f64 / rate),
            rms_dbfs: finite(levels.rms),
            loudness,
            channels: levels
                .channels
                .iter()
                .map(|c| ChannelFigures {
                    sample_peak_dbfs: finite(c.sample_peak),
                    true_peak_dbtp: finite(c.true_peak),
                    rms_dbfs: finite(c.rms),
                    dc_offset: c.dc,
                    dc_offset_dbfs: db(c.dc.abs()),
                })
                .collect(),
            full_scale_samples: levels.full_scale_samples,
            above_full_scale: levels.above_full_scale,
            clipped_runs: levels.overs,
            clip_runs: qc.map_or_else(Vec::new, |q| {
                q.clip_runs.iter().map(|s| Spot::of(s, rate)).collect()
            }),
            longest_clip_run: qc.and_then(|q| q.longest_clip_run.map(|s| Spot::of(&s, rate))),
            inter_sample_overs: levels.true_overs,
            first_inter_sample_over_at: levels.first_true_over_at.map(|f| f as f64 / rate),
            silence_floor_dbfs: qc.and_then(|q| db(f64::from(q.floor))),
            head_silence: qc.map(|q| q.head_silence as f64 / rate),
            tail_silence: qc.map(|q| q.tail_silence as f64 / rate),
            silent: qc.map_or(!sound, |q| q.silent),
            gaps: qc.map_or_else(Vec::new, |q| {
                q.gaps.iter().map(|s| Spot::of(s, rate)).collect()
            }),
            gap_count: qc.map_or(0, |q| q.gap_count),
            stereo: qc.and_then(|q| q.stereo).map(|s| {
                let mono = finite(s.mono_integrated);
                Stereo {
                    correlation: s.correlation,
                    min_correlation: s.min_correlation.map(|(c, _)| c),
                    min_correlation_at: s.min_correlation.map(|(_, at)| at as f64 / rate),
                    negative_share: s.negative_share,
                    mono_lufs: mono,
                    mono_change_lu: mono.zip(integrated).map(|(m, i)| m - i),
                }
            }),
        }
    }
}

/// One delivered file in the report.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct QcFile {
    pub path: String,
    pub name: String,
    pub role: Role,
    /// Its SHA-256 (hex), the render receipt (`None`: it could not be
    /// read — see `notes`).
    pub sha256: Option<String>,
    pub facts: Option<Facts>,
    pub levels: Option<Levels>,
    pub checks: Vec<Check>,
    pub verdict: Verdict,
    pub notes: Vec<String>,
}

/// A delivered file that is not audio (a cue sheet, a DDP fileset's
/// files, a cutting sheet): its receipt only.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Receipt {
    pub path: String,
    pub name: String,
    pub bytes: u64,
    pub sha256: Option<String>,
}

/// Where the report comes from.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ProjectInfo {
    pub name: String,
    /// The project file (`None`: never saved).
    pub path: Option<String>,
    /// "FaderFrame 0.36.0".
    pub program: String,
    /// The project file format's version.
    pub file_version: u32,
}

/// The spec the files were checked against.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SpecInfo {
    pub id: String,
    pub name: String,
    pub source: String,
    pub notes: Vec<String>,
}

/// One setting of the render ("Range", "Bars 1–32").
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Setting {
    pub name: String,
    pub value: String,
}

/// How many checks came out how.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Counts {
    pub pass: usize,
    pub warn: usize,
    pub fail: usize,
    pub info: usize,
}

/// The report (see the module).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct QcReport {
    pub schema: String,
    /// "Stems", "Master mix", "Album “Live”".
    pub title: String,
    pub project: ProjectInfo,
    /// ISO 8601 (UTC) and as read.
    pub created: String,
    pub created_text: String,
    pub spec: SpecInfo,
    pub settings: Vec<Setting>,
    pub verdict: Verdict,
    pub counts: Counts,
    pub files: Vec<QcFile>,
    pub receipts: Vec<Receipt>,
    pub null_test: Option<crate::null::NullTest>,
    /// How things were measured.
    pub method: Vec<String>,
}

impl QcReport {
    /// A report of `files` (checked already) for `title`; the verdict and
    /// counts follow from the files and the null test.
    pub fn new(
        title: &str,
        project: ProjectInfo,
        created: crate::date::Utc,
        spec: &crate::spec::Spec,
        files: Vec<QcFile>,
    ) -> Self {
        let mut notes: Vec<String> = spec.notes.iter().map(|n| n.to_string()).collect();
        if let Some(a) = spec.loudness.and_then(|l| l.approximate) {
            notes.insert(0, format!("{a}."));
        }
        let mut r = Self {
            schema: SCHEMA.into(),
            title: title.into(),
            project,
            created: created.iso(),
            created_text: created.text(),
            spec: SpecInfo {
                id: spec.id.into(),
                name: spec.name.into(),
                source: spec.source.into(),
                notes,
            },
            settings: Vec::new(),
            verdict: Verdict::Info,
            counts: Counts::default(),
            files,
            receipts: Vec::new(),
            null_test: None,
            method: method(),
        };
        r.tally();
        r
    }

    /// Count the checks and take the worst verdict again (after files or
    /// the null test changed).
    pub fn tally(&mut self) {
        let mut counts = Counts::default();
        let mut all = Vec::new();
        let null_checks = self
            .null_test
            .as_ref()
            .and_then(|n| n.mix.as_ref())
            .map(|m| m.checks.iter())
            .into_iter()
            .flatten();
        for c in self.files.iter().flat_map(|f| &f.checks).chain(null_checks) {
            match c.verdict {
                Verdict::Pass => counts.pass += 1,
                Verdict::Warn => counts.warn += 1,
                Verdict::Fail => counts.fail += 1,
                Verdict::Info => counts.info += 1,
            }
            all.push(c.verdict);
        }
        if let Some(n) = &self.null_test {
            all.push(n.verdict);
            match n.verdict {
                Verdict::Pass => counts.pass += 1,
                Verdict::Warn => counts.warn += 1,
                Verdict::Fail => counts.fail += 1,
                Verdict::Info => counts.info += 1,
            }
        }
        for f in self
            .files
            .iter_mut()
            .chain(self.null_test.as_mut().and_then(|n| n.mix.as_mut()))
        {
            f.verdict = Verdict::worst(f.checks.iter().map(|c| c.verdict));
        }
        self.counts = counts;
        self.verdict = Verdict::worst(all);
    }

    /// The report as JSON (pretty-printed).
    pub fn to_json(&self) -> String {
        serde_json::to_string_pretty(self).unwrap_or_else(|e| format!("{{\"error\": \"{e}\"}}"))
    }

    /// One line: "FAIL — 3 failed, 2 warnings, 41 passed".
    pub fn summary(&self) -> String {
        let c = self.counts;
        let mut parts = Vec::new();
        if c.fail > 0 {
            parts.push(format!("{} failed", c.fail));
        }
        if c.warn > 0 {
            parts.push(format!(
                "{} warning{}",
                c.warn,
                if c.warn == 1 { "" } else { "s" }
            ));
        }
        parts.push(format!("{} passed", c.pass));
        let head = match self.verdict {
            Verdict::Fail => "FAIL",
            Verdict::Warn => "Passed with warnings",
            Verdict::Pass => "Passed",
            Verdict::Info => "Measured",
        };
        format!("{head} — {}", parts.join(", "))
    }
}

/// How the figures were measured (the report's last section).
pub fn method() -> Vec<String> {
    vec![
        "Every file is read back from disk as written (format, dither and all) and measured \
         sample by sample; nothing comes from the live mix."
            .into(),
        "Loudness: ITU-R BS.1770-4 / EBU R 128 — K-weighted, gated integrated loudness, loudness \
         range (EBU Tech 3342), momentary (400 ms) and short-term (3 s) maxima; surround \
         channels weighted by their place, the LFE left out."
            .into(),
        "True peak: 4× oversampled (BS.1770 Annex 2, 48-tap windowed sinc). Inter-sample overs \
         are runs of the reconstructed signal past 0 dBFS."
            .into(),
        "Clipping: runs of three or more samples at the format's largest code (or past 0 dBFS in \
         floating point)."
            .into(),
        "Silence: every channel within the dither floor (16 LSB of a fixed-point file, −120 dBFS \
         in floating point); gaps are digital silence of 1 s or more between the first and last \
         sound."
            .into(),
        "Phase: correlation of the first two channels over the file and in 400 ms windows louder \
         than −50 dBFS; mono compatibility is the BS.1770 loudness of the fold-down \
         (L + R) / 2 on both sides against the stereo's."
            .into(),
        "DC offset: the mean of each channel's samples.".into(),
        "Receipts: SHA-256 (FIPS 180-4) of each file's bytes, as `sha256sum` prints it.".into(),
    ]
}

/// "−23.4 LUFS", "—" for a level that does not exist.
pub fn level(v: Option<f64>, unit: &str) -> String {
    match v {
        Some(v) => format!("{v:.1} {unit}").replace('-', "−"),
        None => format!("−∞ {unit}"),
    }
}

/// "1:02.250" or "12.500 s".
pub fn time(seconds: f64) -> String {
    if seconds >= 60.0 {
        let m = (seconds / 60.0).floor();
        format!("{m:.0}:{:06.3}", seconds - m * 60.0)
    } else {
        format!("{seconds:.3} s")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn verdicts_order_and_texts() {
        assert_eq!(
            Verdict::worst([Verdict::Pass, Verdict::Info, Verdict::Warn]),
            Verdict::Warn
        );
        assert_eq!(Verdict::worst([]), Verdict::Info);
        assert_eq!(
            Verdict::worst([Verdict::Fail, Verdict::Pass]),
            Verdict::Fail
        );
        assert_eq!(level(Some(-23.04), "LUFS"), "−23.0 LUFS");
        assert_eq!(level(None, "dBFS"), "−∞ dBFS");
        assert_eq!(time(62.25), "1:02.250");
        assert_eq!(time(1.5), "1.500 s");
    }
}
