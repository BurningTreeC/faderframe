//! What the rules report: findings with their measured evidence, the
//! severity and confidence they were given (and from what), and the
//! experiments they suggest.
//!
//! Findings name tracks by id; titles and explanations are written when
//! they are shown ([`Finding::title`]), with the names the tracks have
//! then — a renamed track keeps its findings.

use crate::bands::format_hz;
use faderframe_core::TrackId;

/// Which signal was measured.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SignalPoint {
    /// Each track as it reaches the master output, alone (solo in place):
    /// after its devices, automation, fader, pan, sends and the buses,
    /// returns and master chain it goes through.
    Processed,
    /// Each track's clips as they play (clip gain, fades, warp, an
    /// instrument's notes) before any audio device, the fader or pan.
    Source,
}

impl SignalPoint {
    pub fn label(self) -> &'static str {
        match self {
            SignalPoint::Processed => "Processed (as mixed)",
            SignalPoint::Source => "Source (before processing)",
        }
    }

    /// What the measurements stand for, for the evidence.
    pub fn describe(self) -> &'static str {
        match self {
            SignalPoint::Processed => {
                "Measured on each track as it reaches the master output on its own (solo in \
                 place, rendered offline with its devices, automation, fader, pan, sends, \
                 buses and master chain)."
            }
            SignalPoint::Source => {
                "Measured on each track's clips before any processing (no devices, fader or \
                 pan): this describes the material, not the mix."
            }
        }
    }
}

/// A stretch of time, seconds from the start of the analysed range.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TimeSpan {
    pub start: f64,
    pub end: f64,
}

impl TimeSpan {
    pub fn new(start: f64, end: f64) -> Self {
        Self {
            start: start.min(end),
            end: start.max(end),
        }
    }

    pub fn seconds(&self) -> f64 {
        self.end - self.start
    }

    /// The overlap with another span (seconds).
    pub fn overlap(&self, other: &TimeSpan) -> f64 {
        (self.end.min(other.end) - self.start.max(other.start)).max(0.0)
    }
}

/// A frequency range (Hz).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FrequencyRange {
    pub lo: f64,
    pub hi: f64,
}

impl FrequencyRange {
    pub fn centre(&self) -> f64 {
        (self.lo * self.hi).sqrt()
    }

    pub fn octaves(&self) -> f64 {
        (self.hi / self.lo.max(1e-9)).log2()
    }

    pub fn label(&self) -> String {
        format!("{}–{}", format_hz(self.lo), format_hz(self.hi))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Category {
    Masking,
    Levels,
    Stereo,
}

impl Category {
    pub const ALL: [Category; 3] = [Category::Masking, Category::Levels, Category::Stereo];

    pub fn label(self) -> &'static str {
        match self {
            Category::Masking => "Masking",
            Category::Levels => "Levels",
            Category::Stereo => "Stereo and Mono",
        }
    }
}

/// The rule that made a finding.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum RuleId {
    SpectralMasking,
    MasterTruePeak,
    ClippedSamples,
    OverFullScale,
    NonFiniteSamples,
    SectionLoudnessStep,
    MonoFoldDownLoss,
    InvertedChannel,
}

impl RuleId {
    pub fn category(self) -> Category {
        match self {
            RuleId::SpectralMasking => Category::Masking,
            RuleId::MasterTruePeak
            | RuleId::ClippedSamples
            | RuleId::OverFullScale
            | RuleId::NonFiniteSamples
            | RuleId::SectionLoudnessStep => Category::Levels,
            RuleId::MonoFoldDownLoss | RuleId::InvertedChannel => Category::Stereo,
        }
    }

    /// The rule's version (with [`crate::ALGORITHM_VERSION`]: which
    /// calculation a finding came from).
    pub fn version(self) -> u32 {
        1
    }

    pub fn name(self) -> &'static str {
        match self {
            RuleId::SpectralMasking => "Spectral masking",
            RuleId::MasterTruePeak => "Master true peak",
            RuleId::ClippedSamples => "Clipped samples",
            RuleId::OverFullScale => "Over full scale",
            RuleId::NonFiniteSamples => "Invalid samples",
            RuleId::SectionLoudnessStep => "Section loudness step",
            RuleId::MonoFoldDownLoss => "Mono fold-down loss",
            RuleId::InvertedChannel => "Inverted channel",
        }
    }

    /// How the rule decides (its calculation, in words).
    pub fn method(self) -> &'static str {
        match self {
            RuleId::SpectralMasking => {
                "Per frame (Hann, half overlap) and third-octave band: the target's band counts \
                 when it is above the floor and within the prominence depth of its loudest band; \
                 the other track covers it by how close its band level comes (none 12 dB or more \
                 under, fully at the target's level or above), weighted by how alike their \
                 stereo placements are. Bands covered for a large enough share of the time are \
                 grouped into ranges; covered frames into events. Severity = role weight × \
                 √persistence × depth × importance; confidence from the signal measured, the \
                 roles known, how much evidence there is and how far above the floor it lies."
            }
            RuleId::MasterTruePeak => {
                "The master's true peak (4× oversampled, BS.1770 Annex 2) against the delivery \
                 ceiling (−1 dBTP: EBU R128, most streaming services)."
            }
            RuleId::ClippedSamples => {
                "Runs of three or more samples at full scale (overs) in the master's output."
            }
            RuleId::OverFullScale => {
                "Samples above 0 dBFS in a track's contribution at the master output (floating \
                 point keeps them; they clip where the signal is converted or limited)."
            }
            RuleId::NonFiniteSamples => {
                "Samples that are not numbers (NaN or infinity) in a rendered signal."
            }
            RuleId::SectionLoudnessStep => {
                "Integrated loudness (BS.1770) of each section of the master; adjacent sections \
                 differing by the step threshold or more."
            }
            RuleId::MonoFoldDownLoss => {
                "Per band, the mono fold-down ((L+R)/2) against what it would hold if left and \
                 right added without cancelling ((√L + √R)²/4): a panned source loses nothing, \
                 uncorrelated channels 3 dB, inverted ones everything. Bands within the \
                 relevance depth of the loudest band losing more than the threshold (half of it \
                 below the low-end frequency) for long enough are reported."
            }
            RuleId::InvertedChannel => {
                "Correlation of left and right over the frames where both carry the signal at \
                 similar levels, at or under the threshold."
            }
        }
    }

    /// What the rule cannot tell, and when it is known to be wrong.
    pub fn limitations(self) -> &'static [&'static str] {
        match self {
            RuleId::SpectralMasking => &[
                "Overlap of level and time in a band is evidence of a possible conflict, not a \
                 psychoacoustic masking model: whether it matters is a musical judgement.",
                "Two microphones on one source, doubled parts and layered sounds overlap by \
                 design; give the tracks roles to tell the analysis what they are.",
                "Below about 40 Hz third-octave bands blend into their neighbours at this \
                 resolution (the analysis window's main lobe is about 23 Hz wide).",
                "Processed signals are each rendered alone: a bus compressor or master limiter \
                 reacts to the full mix differently than to one track.",
                "Phase interaction between the tracks is not measured.",
            ],
            RuleId::MasterTruePeak => &[
                "A ceiling of −1 dBTP is a delivery convention; a premaster for further \
                 mastering may sit anywhere.",
            ],
            RuleId::ClippedSamples => {
                &["A deliberately clipped sound (or a square wave) reads as overs too."]
            }
            RuleId::OverFullScale => &[
                "FaderFrame mixes in floating point: nothing clips inside the mix itself; the \
                 master chain may bring it back under full scale.",
            ],
            RuleId::NonFiniteSamples => &["The device that produced them is not identified."],
            RuleId::SectionLoudnessStep => &[
                "Large steps between sections are often intended (a quiet bridge, a breakdown).",
                "Sections shorter than four seconds are left out.",
            ],
            RuleId::MonoFoldDownLoss => &[
                "Wide stereo (reverbs, wide synths) loses some level in mono by nature; a loss \
                 says how mono playback changes the band, not that the stereo image is wrong.",
                "Low correlation alone is not a fault.",
            ],
            RuleId::InvertedChannel => &[
                "A wide stereo effect can correlate negatively without either channel being \
                 inverted; check by flipping one channel's polarity and listening.",
            ],
        }
    }
}

/// Units of measured values.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Unit {
    /// A level difference.
    Db,
    /// A band level: mean-square power in dB re full scale.
    DbBand,
    Dbfs,
    Dbtp,
    Lufs,
    Lu,
    Hz,
    Seconds,
    /// 0 to 1, shown as a percentage.
    Share,
    Count,
    /// −1 to 1.
    Correlation,
    Octaves,
}

/// One measured value behind a finding.
#[derive(Clone, Debug, PartialEq)]
pub struct Evidence {
    pub label: String,
    pub value: f64,
    pub unit: Unit,
    /// The track it was measured on (`None`: the pair, the mix).
    pub track: Option<TrackId>,
}

impl Evidence {
    pub fn new(label: impl Into<String>, value: f64, unit: Unit, track: Option<TrackId>) -> Self {
        Self {
            label: label.into(),
            value: if value.is_finite() { value } else { 0.0 },
            unit,
            track,
        }
    }

    /// The value with its unit, rounded to what the measurement can tell.
    pub fn value_text(&self) -> String {
        let v = self.value;
        match self.unit {
            Unit::Db => format!("{v:+.1} dB"),
            Unit::DbBand => format!("{v:.1} dB"),
            Unit::Dbfs => format!("{v:.1} dBFS"),
            Unit::Dbtp => format!("{v:+.1} dBTP"),
            Unit::Lufs => format!("{v:.1} LUFS"),
            Unit::Lu => format!("{v:.1} LU"),
            Unit::Hz => format_hz(v),
            Unit::Seconds => format!("{v:.1} s"),
            Unit::Share => format!("{:.0} %", v * 100.0),
            Unit::Count => format!("{v:.0}"),
            Unit::Correlation => format!("{v:+.2}"),
            Unit::Octaves => format!("{v:.2} oct"),
        }
    }
}

/// A term of a severity or confidence calculation (shown with it).
#[derive(Clone, Debug, PartialEq)]
pub struct Term {
    pub name: &'static str,
    pub value: f64,
}

/// Three steps a score is shown in (not a measurement of quality).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Grade {
    Low,
    Moderate,
    High,
}

impl Grade {
    /// Severity: high from 0.5, moderate from 0.25.
    pub fn of_severity(v: f64) -> Self {
        if v >= 0.5 {
            Grade::High
        } else if v >= 0.25 {
            Grade::Moderate
        } else {
            Grade::Low
        }
    }

    /// Confidence: high from 0.75, moderate from 0.5.
    pub fn of_confidence(v: f64) -> Self {
        if v >= 0.75 {
            Grade::High
        } else if v >= 0.5 {
            Grade::Moderate
        } else {
            Grade::Low
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Grade::Low => "Low",
            Grade::Moderate => "Moderate",
            Grade::High => "High",
        }
    }
}

/// How an interaction is spread over time.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Pattern {
    /// Most of the time the source plays there.
    Persistent,
    /// Three or more separate events.
    Recurring,
    /// One or two events.
    Isolated,
}

impl Pattern {
    pub fn label(self) -> &'static str {
        match self {
            Pattern::Persistent => "persistent",
            Pattern::Recurring => "recurring",
            Pattern::Isolated => "isolated",
        }
    }
}

/// Band levels behind a finding (dB per band, per track), the range it
/// is about highlighted.
#[derive(Clone, Debug, PartialEq)]
pub struct SpectrumEvidence {
    pub centres: Vec<f64>,
    pub series: Vec<(TrackId, Vec<f64>)>,
    /// Bands `a..=b`.
    pub highlight: (usize, usize),
}

/// A processing experiment a finding suggests (see [`Recommendation`]).
#[derive(Clone, Debug, PartialEq)]
pub enum Experiment {
    /// A bell on the built-in EQ, inserted at the end of the track's
    /// devices.
    EqCut {
        track: TrackId,
        freq_hz: f64,
        gain_db: f64,
        q: f64,
    },
    /// A dynamic bell on the built-in EQ that cuts up to `range_db` only
    /// while `key` plays in its region (the EQ's sidechain from `key`).
    DynamicEqCut {
        track: TrackId,
        key: TrackId,
        freq_hz: f64,
        range_db: f64,
        q: f64,
    },
    /// The track's fader moved by `change_db`.
    Level { track: TrackId, change_db: f64 },
    /// A low cut on the side signal (the built-in EQ's band on Side, 24
    /// dB/oct): the low end below `freq_hz` made mono.
    SideLowCut { track: TrackId, freq_hz: f64 },
    /// Something to check or try by hand (nothing is changed).
    Manual { text: String },
}

/// Limits experiments are kept within (conservative by design).
pub const MAX_CUT_DB: f64 = 6.0;
pub const MAX_LEVEL_DB: f64 = 6.0;

impl Experiment {
    /// The track it changes.
    pub fn track(&self) -> Option<TrackId> {
        match self {
            Experiment::EqCut { track, .. }
            | Experiment::DynamicEqCut { track, .. }
            | Experiment::Level { track, .. }
            | Experiment::SideLowCut { track, .. } => Some(*track),
            Experiment::Manual { .. } => None,
        }
    }

    /// Every track it involves (a dynamic cut's key too).
    pub fn tracks(&self) -> Vec<TrackId> {
        match self {
            Experiment::DynamicEqCut { track, key, .. } => vec![*track, *key],
            _ => self.track().into_iter().collect(),
        }
    }

    /// It can be auditioned and applied (not a manual suggestion).
    pub fn previewable(&self) -> bool {
        !matches!(self, Experiment::Manual { .. })
    }

    /// Check the values are within the conservative limits and the
    /// frequency below `nyquist`.
    pub fn validate(&self, nyquist: f64) -> Result<(), String> {
        let freq = |f: f64| {
            if f.is_finite() && (20.0..nyquist.min(20_000.0)).contains(&f) {
                Ok(())
            } else {
                Err(format!("{f:.0} Hz is outside the audible range"))
            }
        };
        let q = |q: f64| {
            if q.is_finite() && (0.3..=10.0).contains(&q) {
                Ok(())
            } else {
                Err(format!("a Q of {q:.2} is outside 0.3 to 10"))
            }
        };
        let cut = |g: f64| {
            if g.is_finite() && (-MAX_CUT_DB..0.0).contains(&g) {
                Ok(())
            } else {
                Err(format!(
                    "a cut of {g:.1} dB is outside 0 to −{MAX_CUT_DB} dB"
                ))
            }
        };
        match self {
            Experiment::EqCut {
                freq_hz,
                gain_db,
                q: qv,
                ..
            } => freq(*freq_hz).and(q(*qv)).and(cut(*gain_db)),
            Experiment::DynamicEqCut {
                track,
                key,
                freq_hz,
                range_db,
                q: qv,
            } => {
                if track == key {
                    return Err("a track cannot key its own cut".into());
                }
                freq(*freq_hz).and(q(*qv)).and(cut(*range_db))
            }
            Experiment::Level { change_db, .. } => {
                if change_db.is_finite() && change_db.abs() <= MAX_LEVEL_DB && *change_db != 0.0 {
                    Ok(())
                } else {
                    Err(format!(
                        "a level change of {change_db:.1} dB is not allowed"
                    ))
                }
            }
            Experiment::SideLowCut { freq_hz, .. } => {
                if freq_hz.is_finite() && (30.0..=300.0).contains(freq_hz) {
                    Ok(())
                } else {
                    Err(format!(
                        "a side low cut at {freq_hz:.0} Hz is outside 30 to 300 Hz"
                    ))
                }
            }
            Experiment::Manual { .. } => Ok(()),
        }
    }

    /// What it does, with the names tracks have now.
    pub fn title(&self, name: &dyn Fn(TrackId) -> String) -> String {
        match self {
            Experiment::EqCut {
                track,
                freq_hz,
                gain_db,
                q,
            } => format!(
                "EQ cut on {}: {gain_db:.1} dB at {} (Q {q:.1})",
                name(*track),
                format_hz(*freq_hz)
            ),
            Experiment::DynamicEqCut {
                track,
                key,
                freq_hz,
                range_db,
                q,
            } => format!(
                "Dynamic EQ on {}: up to {range_db:.1} dB at {} (Q {q:.1}) while {} plays",
                name(*track),
                format_hz(*freq_hz),
                name(*key)
            ),
            Experiment::Level { track, change_db } => {
                format!("Level of {}: {change_db:+.1} dB", name(*track))
            }
            Experiment::SideLowCut { track, freq_hz } => format!(
                "Mono low end on {}: side signal cut below {}",
                name(*track),
                format_hz(*freq_hz)
            ),
            Experiment::Manual { text } => format!("Check by hand: {text}"),
        }
    }
}

/// A suggested experiment and why (see [`Experiment`]). Never a promise
/// that the mix gets better: something to listen to.
#[derive(Clone, Debug, PartialEq)]
pub struct Recommendation {
    pub experiment: Experiment,
    /// What it is meant to do.
    pub purpose: String,
    /// What else it does, and what is uncertain.
    pub side_effects: String,
}

/// What a finding is about.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Subject {
    /// `masker` may cover `target` (the one that suffers).
    Pair { target: TrackId, masker: TrackId },
    /// One track's signal.
    Track(TrackId),
    /// The master output (its track, when known).
    Master(Option<TrackId>),
}

/// A finding (see the module docs). Ids are stable: the same rule finding
/// the same thing on the same tracks in the same band gets the same id in
/// the next analysis (dismissals stay dismissed).
#[derive(Clone, Debug, PartialEq)]
pub struct Finding {
    pub id: u64,
    pub rule: RuleId,
    pub signal: SignalPoint,
    pub subject: Subject,
    /// Tracks it involves (the one that suffers first).
    pub tracks: Vec<TrackId>,
    /// From the first event's start to the last's end.
    pub time: Option<TimeSpan>,
    /// Where it happens (merged; at most [`MAX_EVENTS`]).
    pub events: Vec<TimeSpan>,
    pub pattern: Option<Pattern>,
    pub frequency: Option<FrequencyRange>,
    /// 0 to 1 (see [`Grade::of_severity`]); `severity_terms` say from what.
    pub severity: f64,
    pub severity_terms: Vec<Term>,
    /// 0 to 1 (see [`Grade::of_confidence`]).
    pub confidence: f64,
    pub confidence_terms: Vec<Term>,
    /// Measured values (facts, with units).
    pub evidence: Vec<Evidence>,
    pub spectrum: Option<SpectrumEvidence>,
    /// Sections (by name) most of the events fall in.
    pub sections: Vec<String>,
    pub recommendations: Vec<Recommendation>,
    /// Things to keep in mind about this one (beyond the rule's
    /// limitations).
    pub caveats: Vec<String>,
}

/// Events kept per finding (the rest merged into the last).
pub const MAX_EVENTS: usize = 64;

impl Finding {
    pub fn category(&self) -> Category {
        self.rule.category()
    }

    /// How findings are ranked: severity, weighed by confidence
    /// (`severity × (0.5 + 0.5 × confidence)`).
    pub fn relevance(&self) -> f64 {
        self.severity * (0.5 + 0.5 * self.confidence)
    }

    fn evidence_value(&self, label: &str) -> Option<f64> {
        self.evidence
            .iter()
            .find(|e| e.label == label)
            .map(|e| e.value)
    }

    /// A short title from the evidence, with the names tracks have now.
    pub fn title(&self, name: &dyn Fn(TrackId) -> String) -> String {
        let range = self.frequency.map(|f| f.label()).unwrap_or_default();
        let on = |s: &Subject| match s {
            Subject::Track(t) => name(*t),
            Subject::Master(Some(t)) => name(*t),
            Subject::Master(None) => "the master".into(),
            Subject::Pair { target, .. } => name(*target),
        };
        match self.rule {
            RuleId::SpectralMasking => match self.subject {
                Subject::Pair { target, masker } => {
                    format!("{} may mask {} at {range}", name(masker), name(target))
                }
                _ => format!("Possible masking at {range}"),
            },
            RuleId::MasterTruePeak => format!(
                "{}: true peak {:+.1} dBTP",
                on(&self.subject),
                self.evidence_value(labels::TRUE_PEAK).unwrap_or(0.0)
            ),
            RuleId::ClippedSamples => format!(
                "{}: {:.0} clipped runs",
                on(&self.subject),
                self.evidence_value(labels::OVERS).unwrap_or(0.0)
            ),
            RuleId::OverFullScale => format!("{} goes over 0 dBFS", on(&self.subject)),
            RuleId::NonFiniteSamples => format!("{} has invalid samples", on(&self.subject)),
            RuleId::SectionLoudnessStep => format!(
                "{}: {:.1} LU step between sections",
                on(&self.subject),
                self.evidence_value(labels::STEP).unwrap_or(0.0).abs()
            ),
            RuleId::MonoFoldDownLoss => format!(
                "{}: {:.1} dB lost in mono at {range}",
                on(&self.subject),
                -self.evidence_value(labels::FOLD_LOSS).unwrap_or(0.0)
            ),
            RuleId::InvertedChannel => {
                format!("{}: left and right look inverted", on(&self.subject))
            }
        }
    }

    /// The interpretation: what the evidence suggests (worded as a
    /// possibility, never as a verdict).
    pub fn explanation(&self, name: &dyn Fn(TrackId) -> String) -> String {
        let pct = |v: f64| format!("{:.0} %", v * 100.0);
        let range = self.frequency.map(|f| f.label()).unwrap_or_default();
        match (self.rule, self.subject) {
            (RuleId::SpectralMasking, Subject::Pair { target, masker }) => {
                let pattern = self.pattern.map_or("", |p| p.label());
                let secs = self.evidence_value(labels::CONFLICT_SECONDS).unwrap_or(0.0);
                let persistence = self.evidence_value(labels::PERSISTENCE).unwrap_or(0.0);
                let margin = self.evidence_value(labels::MARGIN).unwrap_or(0.0);
                let spatial = self.evidence_value(labels::SPATIAL).unwrap_or(1.0);
                let sections = if self.sections.is_empty() {
                    String::new()
                } else {
                    format!(" (mostly in {})", self.sections.join(", "))
                };
                let rel = if margin >= 0.0 {
                    format!("{margin:.1} dB above")
                } else {
                    format!("{:.1} dB under", -margin)
                };
                format!(
                    "For {secs:.1} s{sections} — {} of the time {t} is prominent at {range} — \
                     {m} sounds in the same band at {rel} {t}'s level, placed {} alike in \
                     stereo. The overlap is {pattern}. This is a potential masking conflict: \
                     {m} may make {t} less distinct there. Whether it matters is for \
                     listening to decide.",
                    pct(persistence),
                    pct(spatial),
                    t = name(target),
                    m = name(masker),
                )
            }
            (RuleId::MasterTruePeak, _) => format!(
                "The highest true peak is above the {:+.1} dBTP ceiling most delivery \
                 specifications set: encoding to a lossy format or converting may clip.",
                self.evidence_value(labels::CEILING).unwrap_or(-1.0)
            ),
            (RuleId::ClippedSamples, _) => {
                "Runs of samples sit at full scale: the waveform is likely flattened (clipped) \
                 there, unless that is the intended sound."
                    .into()
            }
            (RuleId::OverFullScale, _) => {
                "This track alone goes above 0 dBFS at the master output. Inside FaderFrame's \
                 floating-point mix nothing clips, but a fixed-point export or a device that \
                 clips will."
                    .into()
            }
            (RuleId::NonFiniteSamples, _) => {
                "The signal holds samples that are not numbers. A device on the way produced \
                 them; they were measured as silence and can make outputs fail."
                    .into()
            }
            (RuleId::SectionLoudnessStep, _) => {
                "Adjacent sections differ a lot in loudness. Often intended (a breakdown, a \
                 quiet bridge); worth a listen if not."
                    .into()
            }
            (RuleId::MonoFoldDownLoss, _) => format!(
                "Folded to mono, {range} is quieter than in stereo by the measured amount: its \
                 left and right partly cancel. Wide stereo loses some level in mono by nature; \
                 a large loss low down changes how the mix sounds on mono playback."
            ),
            (RuleId::InvertedChannel, _) => {
                "Left and right are strongly negatively correlated where both carry the \
                 signal at similar levels, as if one channel's polarity were inverted: in \
                 mono most of it cancels."
                    .into()
            }
            _ => String::new(),
        }
    }
}

/// Evidence labels the rules use (and titles read).
pub mod labels {
    pub const CONFLICT_SECONDS: &str = "Time covered";
    pub const ACTIVE_SECONDS: &str = "Time the target is prominent in the range";
    pub const PERSISTENCE: &str = "Share of that time covered";
    pub const EVENTS: &str = "Separate events";
    pub const TARGET_LEVEL: &str = "Target band level (while covered)";
    pub const MASKER_LEVEL: &str = "Masker band level (while covered)";
    pub const MARGIN: &str = "Masker relative to target";
    pub const SPATIAL: &str = "Stereo placement alike";
    pub const SHARE: &str = "Share of the target's energy in the range";
    pub const DEPTH: &str = "Mean coverage";
    pub const TRUE_PEAK: &str = "True peak";
    pub const SAMPLE_PEAK: &str = "Sample peak";
    pub const CEILING: &str = "Ceiling";
    pub const OVERS: &str = "Overs (3+ samples at full scale)";
    pub const FULL_SCALE: &str = "Samples at full scale";
    pub const ABOVE: &str = "Samples above 0 dBFS";
    pub const NON_FINITE: &str = "Invalid samples";
    pub const INTEGRATED: &str = "Integrated loudness";
    pub const STEP: &str = "Step between sections";
    pub const FOLD_LOSS: &str = "Mono fold-down change";
    pub const CORRELATION: &str = "Correlation";
    pub const BAND_LEVEL: &str = "Band level";
    pub const CREST: &str = "Crest factor (peak to RMS)";
    pub const RMS: &str = "RMS";
    pub const LRA: &str = "Loudness range";
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn experiments_are_validated() {
        let t = TrackId(1);
        let ok = Experiment::EqCut {
            track: t,
            freq_hz: 80.0,
            gain_db: -3.0,
            q: 1.4,
        };
        assert!(ok.validate(24_000.0).is_ok());
        let boost = Experiment::EqCut {
            track: t,
            freq_hz: 80.0,
            gain_db: 3.0,
            q: 1.4,
        };
        assert!(boost.validate(24_000.0).is_err(), "cuts only");
        let deep = Experiment::EqCut {
            track: t,
            freq_hz: 80.0,
            gain_db: -12.0,
            q: 1.4,
        };
        assert!(deep.validate(24_000.0).is_err());
        let high = Experiment::EqCut {
            track: t,
            freq_hz: 30_000.0,
            gain_db: -2.0,
            q: 1.4,
        };
        assert!(high.validate(24_000.0).is_err());
        let own = Experiment::DynamicEqCut {
            track: t,
            key: t,
            freq_hz: 80.0,
            range_db: -3.0,
            q: 1.0,
        };
        assert!(own.validate(24_000.0).is_err());
        let nan = Experiment::Level {
            track: t,
            change_db: f64::NAN,
        };
        assert!(nan.validate(24_000.0).is_err());
        assert!(
            Experiment::Manual { text: "x".into() }
                .validate(24_000.0)
                .is_ok()
        );
        let name = |id: TrackId| format!("T{}", id.0);
        assert_eq!(ok.title(&name), "EQ cut on T1: -3.0 dB at 80 Hz (Q 1.4)");
    }

    #[test]
    fn evidence_rounds_and_never_holds_nan() {
        let e = Evidence::new("x", f64::NAN, Unit::Db, None);
        assert_eq!(e.value, 0.0);
        assert_eq!(
            Evidence::new("x", 0.4567, Unit::Share, None).value_text(),
            "46 %"
        );
        assert_eq!(
            Evidence::new("x", -3.04, Unit::Db, None).value_text(),
            "-3.0 dB"
        );
        assert_eq!(Grade::of_severity(0.6), Grade::High);
        assert_eq!(Grade::of_confidence(0.6), Grade::Moderate);
    }
}
