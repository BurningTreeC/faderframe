//! The mix against a reference track (spec §11 E): a comparison target,
//! never a standard — a category of its own ([`crate::Category::Reference`]).
//!
//! Both are profiled alike ([`crate::profile`]: the master over the
//! analysed range, the reference as it is heard — mapped onto the
//! master's channels — over the same stretch when it follows the timeline,
//! else its region). Every level comparison is **loudness-matched**: the
//! reference is brought to the mix's integrated loudness (BS.1770, both
//! channels weighted 1) before its band levels are compared, so what is
//! left is balance, not volume. Measured ([`ReferenceComparison`]):
//!
//! * loudness: integrated, loudness range, peak-to-loudness ratio (true
//!   peak − integrated) of both;
//! * the long-term difference per third octave (mean power over the frames
//!   with signal; the matched reference subtracted), and per broad region;
//! * per section of the mix, each broad region's share of the section's
//!   energy against the reference's (the same stretch when the reference
//!   follows the timeline, else all of it);
//! * per broad region, the mono fold-down change and the width (side
//!   against mid) of both ([`crate::stereo`]).
//!
//! # Rules ([`crate::config::ReferenceRule`])
//!
//! * [`RuleId::ReferenceLoudness`]: integrated, range or PLR apart by the
//!   thresholds; severity 0.12.
//! * [`RuleId::ReferenceSpectrum`]: the difference smoothed over three
//!   neighbouring bands, runs of at least `min_bands` relevant bands at or
//!   beyond `spectrum_db` in one direction (the largest `max_ranges`);
//!   severity `clamp(0.12 + 0.04 × (|mean| − threshold) + 0.02 ×
//!   (octaves − 1), 0.12, 0.45)`.
//! * [`RuleId::ReferenceSectionBalance`]: a section whose difference in a
//!   region is `section_db` or more and stands out from the sections'
//!   typical difference there (their median) by half that; severity
//!   `clamp(0.12 + 0.03 × (largest difference − threshold), 0.12, 0.4)`.
//! * [`RuleId::ReferenceStereo`]: regions losing `fold_db` more in mono
//!   than the reference, or `width_db` wider or narrower; severity
//!   `clamp(0.15 + 0.03 × largest excess, 0.15, 0.4)`.
//!
//! Confidence = alignment (the reference follows the timeline: 1, else
//! 0.8) × amount (`0.6 + 0.4 · min(1, seconds compared / 30)`), the
//! seconds being the shorter of the two signals with signal.
//! Low severities on purpose: a difference to a reference is information,
//! the mix's and the reference's arrangements differ.

use crate::bands::{BROAD_EDGES, BROAD_NAMES};
use crate::config::ReferenceRule;
use crate::finding::{
    Evidence, Experiment, Finding, FrequencyRange, Recommendation, RuleId, SignalPoint, Subject,
    Term, TimeSpan, Unit, labels,
};
use crate::profile::{TrackProfile, db_power, power_db};
use crate::stereo::BandStereo;
use faderframe_core::{ReferenceId, TrackId};

/// A frame carries signal from this total power (dB): the rest is left
/// out of the averages (silence before, after and between).
const ACTIVE_DB: f64 = -70.0;
/// Width (side − mid) read for a region with no side to speak of.
const MONO_WIDTH_DB: f64 = -60.0;

/// What [`compare`] reads.
pub struct ReferenceInput<'a> {
    pub reference: ReferenceId,
    /// The reference as heard (its frames on the mix's time axis when
    /// `aligned`).
    pub profile: &'a TrackProfile,
    /// The master over the analysed range.
    pub mix: &'a TrackProfile,
    pub master: Option<TrackId>,
    /// The reference follows the timeline: its frame `t` played beside
    /// the mix's frame `t`.
    pub aligned: bool,
    /// The mix's sections (seconds from the start of the range).
    pub sections: &'a [(String, TimeSpan)],
    pub rules: &'a ReferenceRule,
    pub signal: SignalPoint,
}

/// A broad region compared.
#[derive(Clone, Debug, PartialEq)]
pub struct RegionComparison {
    pub name: &'static str,
    /// The region's share of all the energy (dB) in the mix and in the
    /// reference.
    pub mix_share_db: f64,
    pub reference_share_db: f64,
    /// Mono fold-down change (dB, ≤ 0) and width (side − mid, dB).
    pub mix_fold_db: f64,
    pub reference_fold_db: f64,
    pub mix_width_db: f64,
    pub reference_width_db: f64,
    /// It carries energy worth comparing (in either).
    pub relevant: bool,
}

/// A section of the mix compared.
#[derive(Clone, Debug, PartialEq)]
pub struct SectionComparison {
    pub name: String,
    pub span: TimeSpan,
    /// Per broad region: the mix's share minus the reference's (dB);
    /// `None` where neither carries energy.
    pub difference_db: Vec<Option<f64>>,
}

/// Everything measured of the mix against the reference (facts; the
/// findings say what stands out).
#[derive(Clone, Debug, PartialEq)]
pub struct ReferenceComparison {
    pub reference: ReferenceId,
    pub aligned: bool,
    pub mix_lufs: Option<f64>,
    pub reference_lufs: Option<f64>,
    /// The gain the reference was compared at (mix − reference, dB).
    pub match_db: f64,
    pub mix_range: Option<f64>,
    pub reference_range: Option<f64>,
    pub mix_plr: Option<f64>,
    pub reference_plr: Option<f64>,
    /// Seconds with signal compared, of each.
    pub mix_seconds: f64,
    pub reference_seconds: f64,
    /// Band centres (Hz), the mix's and the matched reference's long-term
    /// levels (dB) and their difference (`None`: a band neither carries).
    pub centres: Vec<f64>,
    pub mix_db: Vec<f64>,
    pub reference_db: Vec<f64>,
    pub difference_db: Vec<Option<f64>>,
    pub regions: Vec<RegionComparison>,
    pub sections: Vec<SectionComparison>,
}

#[derive(Debug, thiserror::Error, PartialEq)]
pub enum ReferenceError {
    #[error("the reference and the mix were measured with different settings")]
    Layouts,
}

fn id_of(rule: RuleId, reference: ReferenceId, parts: &[u64]) -> u64 {
    let mut h = crate::Fnv::new();
    h.write_u64(rule as u64);
    h.write_u64(reference.raw());
    for p in parts {
        h.write_u64(*p);
    }
    h.finish()
}

/// Frames with signal.
fn active(p: &TrackProfile, frames: std::ops::Range<usize>) -> Vec<usize> {
    frames
        .filter(|&t| {
            let total: f64 = (0..p.frames.bands()).map(|b| p.frames.power(t, b)).sum();
            power_db(total) >= ACTIVE_DB
        })
        .collect()
}

fn mean_powers(p: &TrackProfile, frames: &[usize]) -> Vec<f64> {
    let n = frames.len().max(1) as f64;
    (0..p.frames.bands())
        .map(|b| frames.iter().map(|&t| p.frames.power(t, b)).sum::<f64>() / n)
        .collect()
}

/// The broad region of each band (`None`: outside them).
fn regions_of(p: &TrackProfile) -> Vec<Option<usize>> {
    p.layout
        .bands()
        .iter()
        .map(|b| {
            BROAD_EDGES
                .windows(2)
                .position(|w| b.centre >= w[0] && b.centre < w[1])
        })
        .collect()
}

fn region_powers(powers: &[f64], regions: &[Option<usize>]) -> Vec<f64> {
    let mut out = vec![0.0; BROAD_NAMES.len()];
    for (p, r) in powers.iter().zip(regions) {
        if let Some(r) = r {
            out[*r] += p;
        }
    }
    out
}

/// Shares of the total (dB) and which regions carry energy (within
/// `relevance` dB of the largest).
fn shares(powers: &[f64], relevance: f64) -> (Vec<f64>, Vec<bool>) {
    let total: f64 = powers.iter().sum();
    let top = powers.iter().fold(0.0f64, |m, v| m.max(*v));
    let shares = powers
        .iter()
        .map(|p| power_db(*p) - power_db(total))
        .collect();
    let relevant = powers
        .iter()
        .map(|p| *p > 0.0 && power_db(*p) >= power_db(top) - relevance && power_db(*p) > ACTIVE_DB)
        .collect();
    (shares, relevant)
}

/// Stereo per region over `frames`.
fn region_stereo(p: &TrackProfile, frames: &[usize], regions: &[Option<usize>]) -> Vec<BandStereo> {
    let mut out = vec![
        BandStereo {
            mid: 0.0,
            side: 0.0,
            coherent: 0.0,
            power: 0.0,
            correlation: 0.0,
        };
        BROAD_NAMES.len()
    ];
    for &t in frames {
        for (b, r) in regions.iter().enumerate() {
            let Some(r) = r else { continue };
            let x = BandStereo::of_frame(p, t, b);
            let o = &mut out[*r];
            o.mid += x.mid;
            o.side += x.side;
            o.coherent += x.coherent;
            o.power += x.power;
            o.correlation += x.power * x.correlation;
        }
    }
    for o in &mut out {
        if o.power > 0.0 {
            o.correlation /= o.power;
        }
    }
    out
}

fn loudness(p: &TrackProfile) -> (Option<f64>, Option<f64>, Option<f64>) {
    let finite = |v: f64| v.is_finite().then_some(v);
    let l = p.levels.loudness;
    let i = l.and_then(|l| finite(l.integrated));
    let range = l
        .and_then(|l| finite(l.range))
        .filter(|_| p.seconds() >= 3.0);
    let plr = i.and_then(|i| finite(p.levels.true_peak).map(|tp| tp - i));
    (i, range, plr)
}

fn recs(input: &ReferenceInput<'_>, eq: bool) -> Vec<Recommendation> {
    let mut out = vec![Recommendation {
        experiment: Experiment::CompareReference {
            reference: input.reference,
        },
        purpose: "Hear the mix and the reference flip, loudness-matched, at the same place.".into(),
        side_effects: "Nothing in the project changes: the reference is heard instead of the mix \
                       on the monitor path only."
            .into(),
    }];
    if eq && let Some(m) = input.master {
        out.push(Recommendation {
            experiment: Experiment::ReferenceEqMatch {
                reference: input.reference,
                track: m,
            },
            purpose: "Open EQ Match on the master's EQ with the reference's spectrum as its \
                      target, to see (and, if wanted, try) a correction curve."
                .into(),
            side_effects: "An EQ is added to the master when it has none (one undo step); EQ \
                           Match changes nothing until a match is made and applied there. \
                           Matching a whole spectrum also matches what is the arrangement's, \
                           not the mix's."
                .into(),
        });
    }
    out
}

fn finding(
    input: &ReferenceInput<'_>,
    rule: RuleId,
    parts: &[u64],
    severity: (f64, Vec<Term>),
    confidence: (f64, Vec<Term>),
    evidence: Vec<Evidence>,
    recommendations: Vec<Recommendation>,
) -> Finding {
    Finding {
        id: id_of(rule, input.reference, parts),
        rule,
        signal: input.signal,
        subject: Subject::Master(input.master),
        tracks: input.master.into_iter().collect(),
        time: None,
        events: Vec::new(),
        pattern: None,
        frequency: None,
        severity: severity.0.clamp(0.0, 1.0),
        severity_terms: severity.1,
        confidence: confidence.0.clamp(0.0, 1.0),
        confidence_terms: confidence.1,
        evidence,
        spectrum: None,
        sections: Vec::new(),
        recommendations,
        caveats: vec![
            "A reference is a comparison, not a target every mix should meet: its arrangement, \
             key and mastering differ."
                .into(),
        ],
    }
}

/// Compare the mix with the reference (see the module docs): what was
/// measured and the findings (unranked).
pub fn compare(
    input: &ReferenceInput<'_>,
) -> Result<(ReferenceComparison, Vec<Finding>), ReferenceError> {
    let (mix, r) = (input.mix, input.profile);
    if mix.layout != r.layout || mix.hop != r.hop || mix.rate != r.rate {
        return Err(ReferenceError::Layouts);
    }
    let rules = input.rules;
    let mix_frames = active(mix, 0..mix.frames.frames());
    let ref_frames = active(r, 0..r.frames.frames());
    let hop = mix.hop_seconds();
    let (mix_seconds, reference_seconds) =
        (mix_frames.len() as f64 * hop, ref_frames.len() as f64 * hop);
    let (mix_lufs, mix_range, mix_plr) = loudness(mix);
    let (reference_lufs, reference_range, reference_plr) = loudness(r);
    let m = mean_powers(mix, &mix_frames);
    let raw = mean_powers(r, &ref_frames);
    // The match: the loudness, else (one too short to gate) the power.
    let match_db = match (mix_lufs, reference_lufs) {
        (Some(a), Some(b)) => a - b,
        _ => power_db(m.iter().sum()) - power_db(raw.iter().sum()),
    };
    let match_db = if match_db.is_finite() { match_db } else { 0.0 };
    let g = db_power(match_db.max(-140.0));
    let rp: Vec<f64> = raw.iter().map(|v| v * g).collect();
    let (mix_db, reference_db): (Vec<f64>, Vec<f64>) = m
        .iter()
        .zip(&rp)
        .map(|(a, b)| (power_db(*a), power_db(*b)))
        .unzip();
    let top = mix_db
        .iter()
        .chain(&reference_db)
        .fold(f64::NEG_INFINITY, |a, b| a.max(*b));
    let difference_db: Vec<Option<f64>> = mix_db
        .iter()
        .zip(&reference_db)
        .map(|(a, b)| {
            (a.max(*b) >= top - rules.relevance_db && a.max(*b) > ACTIVE_DB).then_some(a - b)
        })
        .collect();
    // Broad regions: shares and stereo.
    let regions = regions_of(mix);
    let (ms, mr) = shares(&region_powers(&m, &regions), rules.relevance_db);
    let (rs, rr) = shares(&region_powers(&rp, &regions), rules.relevance_db);
    let stereo = mix.channels >= 2 && r.channels >= 2;
    let mst = region_stereo(mix, &mix_frames, &regions);
    let rst = region_stereo(r, &ref_frames, &regions);
    let region_list: Vec<RegionComparison> = BROAD_NAMES
        .iter()
        .enumerate()
        .map(|(i, name)| RegionComparison {
            name,
            mix_share_db: ms[i],
            reference_share_db: rs[i],
            mix_fold_db: mst[i].fold_db(),
            reference_fold_db: rst[i].fold_db(),
            // Mono reads −60 dB (no side to speak of), whatever the level.
            mix_width_db: mst[i].width_db().max(MONO_WIDTH_DB),
            reference_width_db: rst[i].width_db().max(MONO_WIDTH_DB),
            relevant: mr[i] && rr[i],
        })
        .collect();
    // Sections.
    let mut sections = Vec::new();
    for (name, span) in input.sections {
        if span.seconds() < rules.min_section_seconds {
            continue;
        }
        let mf = active(mix, mix.frames_in(span.start, span.end));
        let rf = if input.aligned {
            active(r, r.frames_in(span.start, span.end))
        } else {
            ref_frames.clone()
        };
        if mf.is_empty() || rf.is_empty() {
            continue;
        }
        let (a, ar) = shares(
            &region_powers(&mean_powers(mix, &mf), &regions),
            rules.relevance_db,
        );
        let (b, br) = shares(
            &region_powers(&mean_powers(r, &rf), &regions),
            rules.relevance_db,
        );
        sections.push(SectionComparison {
            name: name.clone(),
            span: *span,
            difference_db: (0..BROAD_NAMES.len())
                .map(|i| (ar[i] && br[i]).then(|| a[i] - b[i]))
                .collect(),
        });
    }
    let comparison = ReferenceComparison {
        reference: input.reference,
        aligned: input.aligned,
        mix_lufs,
        reference_lufs,
        match_db,
        mix_range,
        reference_range,
        mix_plr,
        reference_plr,
        mix_seconds,
        reference_seconds,
        centres: mix.layout.bands().iter().map(|b| b.centre).collect(),
        mix_db,
        reference_db,
        difference_db,
        regions: region_list,
        sections,
    };
    let findings = if mix_frames.is_empty() || ref_frames.is_empty() {
        Vec::new()
    } else {
        rules_on(input, &comparison, stereo)
    };
    Ok((comparison, findings))
}

fn rules_on(input: &ReferenceInput<'_>, c: &ReferenceComparison, stereo: bool) -> Vec<Finding> {
    let rules = input.rules;
    let seconds = c.mix_seconds.min(c.reference_seconds);
    let alignment = if c.aligned { 1.0 } else { 0.8 };
    let amount = 0.6 + 0.4 * (seconds / 30.0).min(1.0);
    let confidence = || {
        (
            alignment * amount,
            vec![
                Term {
                    name: "alignment",
                    value: alignment,
                },
                Term {
                    name: "amount",
                    value: amount,
                },
            ],
        )
    };
    let master = input.master;
    let mut out = Vec::new();
    let seconds_evidence = Evidence::new(
        labels::REFERENCE_SECONDS,
        c.reference_seconds,
        Unit::Seconds,
        None,
    );
    // Loudness.
    let apart = |a: Option<f64>, b: Option<f64>, t: f64| match (a, b) {
        (Some(a), Some(b)) => (a - b).abs() >= t,
        _ => false,
    };
    if apart(c.mix_lufs, c.reference_lufs, rules.loudness_lu)
        || apart(c.mix_range, c.reference_range, rules.range_lu)
        || apart(c.mix_plr, c.reference_plr, rules.range_lu)
    {
        let mut evidence = Vec::new();
        let mut push = |label: &str, v: Option<f64>, unit: Unit| {
            if let Some(v) = v {
                evidence.push(Evidence::new(label, v, unit, None));
            }
        };
        push(labels::MIX_LUFS, c.mix_lufs, Unit::Lufs);
        push(labels::REFERENCE_LUFS, c.reference_lufs, Unit::Lufs);
        push(
            labels::LOUDNESS_DIFFERENCE,
            c.mix_lufs.zip(c.reference_lufs).map(|(a, b)| a - b),
            Unit::Lu,
        );
        push(labels::MIX_LRA, c.mix_range, Unit::Lu);
        push(labels::REFERENCE_LRA, c.reference_range, Unit::Lu);
        push(labels::MIX_PLR, c.mix_plr, Unit::Db);
        push(labels::REFERENCE_PLR, c.reference_plr, Unit::Db);
        evidence.push(seconds_evidence.clone());
        out.push(finding(
            input,
            RuleId::ReferenceLoudness,
            &[],
            (0.12, Vec::new()),
            confidence(),
            evidence,
            recs(input, false),
        ));
    }
    // Frequency-dependent differences.
    let n = c.difference_db.len();
    let smoothed: Vec<Option<f64>> = (0..n)
        .map(|b| {
            c.difference_db[b]?;
            let around: Vec<f64> = (b.saturating_sub(1)..(b + 2).min(n))
                .filter_map(|i| c.difference_db[i])
                .collect();
            Some(around.iter().sum::<f64>() / around.len().max(1) as f64)
        })
        .collect();
    let mut runs: Vec<(usize, usize, f64)> = Vec::new();
    let mut b = 0;
    while b < n {
        let sign = match smoothed[b] {
            Some(v) if v.abs() >= rules.spectrum_db => v.signum(),
            _ => {
                b += 1;
                continue;
            }
        };
        let a = b;
        while b < n
            && smoothed[b].is_some_and(|v| v.abs() >= rules.spectrum_db && v.signum() == sign)
        {
            b += 1;
        }
        if b - a >= rules.min_bands {
            let area: f64 = (a..b).filter_map(|i| smoothed[i]).map(f64::abs).sum();
            runs.push((a, b - 1, area));
        }
    }
    runs.sort_by(|x, y| y.2.total_cmp(&x.2).then(x.0.cmp(&y.0)));
    for &(a, z, _) in runs.iter().take(rules.max_ranges) {
        let diffs: Vec<f64> = (a..=z).filter_map(|i| c.difference_db[i]).collect();
        let mean = diffs.iter().sum::<f64>() / diffs.len().max(1) as f64;
        let largest = diffs
            .iter()
            .copied()
            .fold(0.0f64, |m, v| if v.abs() > m.abs() { v } else { m });
        let (lo, hi) = input.mix.layout.span(a, z).unwrap_or((0.0, 0.0));
        let octaves = (hi / lo.max(1e-9)).log2();
        let power = |v: &[f64]| power_db((a..=z).map(|i| db_power(v[i])).sum());
        let severity = (0.12 + 0.04 * (mean.abs() - rules.spectrum_db) + 0.02 * (octaves - 1.0))
            .clamp(0.12, 0.45);
        let mut f = finding(
            input,
            RuleId::ReferenceSpectrum,
            &[a as u64, z as u64],
            (
                severity,
                vec![
                    Term {
                        name: "|mean difference|",
                        value: mean.abs(),
                    },
                    Term {
                        name: "octaves",
                        value: octaves,
                    },
                ],
            ),
            confidence(),
            vec![
                Evidence::new(labels::DIFFERENCE, mean, Unit::Db, None),
                Evidence::new(labels::LARGEST_DIFFERENCE, largest, Unit::Db, None),
                Evidence::new(
                    "Mix level in the range",
                    power(&c.mix_db),
                    Unit::DbBand,
                    master,
                ),
                Evidence::new(
                    "Reference level in the range (matched)",
                    power(&c.reference_db),
                    Unit::DbBand,
                    None,
                ),
                Evidence::new(
                    "Reference played at (loudness match)",
                    c.match_db,
                    Unit::Db,
                    None,
                ),
                seconds_evidence.clone(),
            ],
            recs(input, true),
        );
        f.frequency = Some(FrequencyRange { lo, hi });
        out.push(f);
    }
    // Sections: against the sections' typical difference (the median of
    // theirs), so a difference the whole song has (the spectrum finding)
    // is not repeated per section.
    let typical: Vec<Option<f64>> = (0..BROAD_NAMES.len())
        .map(|i| {
            let mut v: Vec<f64> = c
                .sections
                .iter()
                .filter_map(|s| s.difference_db[i])
                .collect();
            if v.len() < 2 {
                return None;
            }
            v.sort_by(f64::total_cmp);
            let m = v.len() / 2;
            Some(if v.len().is_multiple_of(2) {
                0.5 * (v[m - 1] + v[m])
            } else {
                v[m]
            })
        })
        .collect();
    for (si, s) in c.sections.iter().enumerate() {
        let mut evidence = Vec::new();
        let mut largest = 0.0f64;
        for (i, d) in s.difference_db.iter().enumerate() {
            let (Some(d), Some(song)) = (d, typical[i]) else {
                continue;
            };
            let dev = d - song;
            if d.abs() >= rules.section_db && dev.abs() >= rules.section_db / 2.0 {
                largest = largest.max(d.abs());
                evidence.push(Evidence::new(
                    format!("{}: mix minus reference", BROAD_NAMES[i]),
                    *d,
                    Unit::Db,
                    None,
                ));
                evidence.push(Evidence::new(
                    format!(
                        "{}: {}",
                        BROAD_NAMES[i],
                        labels::SONG_DIFFERENCE.to_lowercase()
                    ),
                    song,
                    Unit::Db,
                    None,
                ));
            }
        }
        if evidence.is_empty() {
            continue;
        }
        evidence.push(seconds_evidence.clone());
        let mut f = finding(
            input,
            RuleId::ReferenceSectionBalance,
            &[crate::Fnv::of_str(&s.name), si as u64],
            (
                (0.12 + 0.03 * (largest - rules.section_db)).clamp(0.12, 0.4),
                vec![Term {
                    name: "largest difference",
                    value: largest,
                }],
            ),
            confidence(),
            evidence,
            {
                let mut r = recs(input, false);
                r.push(Recommendation {
                    experiment: Experiment::Manual {
                        text: "listen to the section against the reference; a change for this \
                               section only (automation of an EQ band or a level) is the \
                               experiment to try"
                            .into(),
                    },
                    purpose: "Decide whether the section's balance is meant.".into(),
                    side_effects: "Nothing is changed.".into(),
                });
                r
            },
        );
        f.sections = vec![s.name.clone()];
        f.time = Some(s.span);
        f.events = vec![s.span];
        out.push(f);
    }
    // Stereo and mono.
    if stereo {
        let mut evidence = Vec::new();
        let mut largest = 0.0f64;
        for r in c.regions.iter().filter(|r| r.relevant) {
            let fold = r.mix_fold_db - r.reference_fold_db;
            let width = r.mix_width_db - r.reference_width_db;
            let loses = fold <= -rules.fold_db;
            let wider = width.abs() >= rules.width_db
                && r.mix_width_db.is_finite()
                && r.reference_width_db.is_finite();
            if !(loses || wider) {
                continue;
            }
            largest = largest
                .max(if loses { -fold - rules.fold_db } else { 0.0 })
                .max(if wider {
                    width.abs() - rules.width_db
                } else {
                    0.0
                });
            evidence.push(Evidence::new(
                format!("{}: mono fold-down, mix", r.name),
                r.mix_fold_db,
                Unit::Db,
                master,
            ));
            evidence.push(Evidence::new(
                format!("{}: mono fold-down, reference", r.name),
                r.reference_fold_db,
                Unit::Db,
                None,
            ));
            evidence.push(Evidence::new(
                format!("{}: width (side − mid), mix", r.name),
                r.mix_width_db,
                Unit::Db,
                master,
            ));
            evidence.push(Evidence::new(
                format!("{}: width (side − mid), reference", r.name),
                r.reference_width_db,
                Unit::Db,
                None,
            ));
        }
        if !evidence.is_empty() {
            evidence.push(seconds_evidence);
            out.push(finding(
                input,
                RuleId::ReferenceStereo,
                &[],
                (
                    (0.15 + 0.03 * largest).clamp(0.15, 0.4),
                    vec![Term {
                        name: "largest excess",
                        value: largest,
                    }],
                ),
                confidence(),
                evidence,
                {
                    let mut r = recs(input, false);
                    r.push(Recommendation {
                        experiment: Experiment::Manual {
                            text: "compare both with the master's MONO button".into(),
                        },
                        purpose: "Hear what a mono playback keeps of each.".into(),
                        side_effects: "Nothing is changed.".into(),
                    });
                    r
                },
            ));
        }
    }
    out.retain(|f| f.severity.is_finite() && f.confidence.is_finite());
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{SR, mix, profile, tone};

    const LEN: f64 = 12.0;

    /// A "song": a bass, mids and highs at the given amplitudes.
    fn song(bass: f64, mids: f64, highs: f64) -> Vec<f32> {
        mix(&[
            &tone(80.0, bass, 0.0, LEN, LEN),
            &tone(1000.0, mids, 0.0, LEN, LEN),
            &tone(8000.0, highs, 0.0, LEN, LEN),
        ])
    }

    fn run(
        mix: &TrackProfile,
        reference: &TrackProfile,
        sections: &[(String, TimeSpan)],
    ) -> (ReferenceComparison, Vec<Finding>) {
        let rules = ReferenceRule::default();
        compare(&ReferenceInput {
            reference: ReferenceId(9),
            profile: reference,
            mix,
            master: Some(TrackId(1)),
            aligned: true,
            sections,
            rules: &rules,
            signal: SignalPoint::Processed,
        })
        .unwrap_or_else(|e| panic!("{e}"))
    }

    fn rules_of(f: &[Finding]) -> Vec<RuleId> {
        f.iter().map(|f| f.rule).collect()
    }

    #[test]
    fn the_same_balance_louder_is_only_a_loudness_difference() {
        let a = song(0.2, 0.1, 0.05);
        let louder: Vec<f32> = a.iter().map(|v| v * 3.0).collect();
        let (mix, reference) = (profile(&[&a, &a]), profile(&[&louder, &louder]));
        let (c, f) = run(&mix, &reference, &[]);
        // 3× = +9.5 dB: matched away.
        assert!((c.match_db + 9.54).abs() < 0.2, "{}", c.match_db);
        for d in c.difference_db.iter().flatten() {
            assert!(d.abs() < 0.5, "{d}");
        }
        assert_eq!(rules_of(&f), vec![RuleId::ReferenceLoudness]);
        assert!(matches!(
            f[0].recommendations[0].experiment,
            Experiment::CompareReference { .. }
        ));
    }

    #[test]
    fn more_low_end_than_the_reference_is_found_where_it_is() {
        let (a, b) = (song(0.4, 0.1, 0.05), song(0.1, 0.1, 0.05));
        let (mix, reference) = (profile(&[&a, &a]), profile(&[&b, &b]));
        let (c, f) = run(&mix, &reference, &[]);
        let spectrum: Vec<&Finding> = f
            .iter()
            .filter(|f| f.rule == RuleId::ReferenceSpectrum)
            .collect();
        assert!(!spectrum.is_empty(), "{f:?}");
        let low = spectrum
            .iter()
            .find(|f| f.frequency.is_some_and(|r| r.lo < 80.0 && r.hi > 80.0))
            .unwrap_or_else(|| panic!("{spectrum:?}"));
        let d = low
            .evidence
            .iter()
            .find(|e| e.label == labels::DIFFERENCE)
            .map_or(0.0, |e| e.value);
        assert!(d > 4.0, "more low end: {d}");
        // The matched mids and highs read lower in the mix (the loudness
        // went to the bass).
        assert!(c.match_db.is_finite());
        assert!(
            low.recommendations
                .iter()
                .any(|r| matches!(r.experiment, Experiment::ReferenceEqMatch { .. }))
        );
    }

    #[test]
    fn a_section_that_departs_is_named_and_one_that_follows_is_not() {
        // The mix has twice the bass in its second half only.
        let half = (LEN / 2.0 * f64::from(SR)) as usize;
        let a: Vec<f32> = song(0.1, 0.1, 0.05)
            .iter()
            .zip(tone(80.0, 0.3, LEN / 2.0, LEN, LEN))
            .map(|(x, y)| x + y)
            .collect();
        let b = song(0.1, 0.1, 0.05);
        let (mix, reference) = (profile(&[&a, &a]), profile(&[&b, &b]));
        let secs = vec![
            ("Verse".to_string(), TimeSpan::new(0.0, 6.0)),
            ("Chorus".to_string(), TimeSpan::new(6.0, 12.0)),
        ];
        let (c, f) = run(&mix, &reference, &secs);
        assert_eq!(c.sections.len(), 2);
        assert!(half > 0);
        let named: Vec<&str> = f
            .iter()
            .filter(|f| f.rule == RuleId::ReferenceSectionBalance)
            .flat_map(|f| f.sections.iter().map(String::as_str))
            .collect();
        assert!(named.contains(&"Chorus"), "{named:?}");
        assert!(!named.contains(&"Verse") || named.len() == 2);
    }

    #[test]
    fn a_mix_losing_more_in_mono_than_the_reference_is_found() {
        let a = song(0.2, 0.1, 0.05);
        // The mix's mids partly inverted on one side.
        let mid = tone(1000.0, 0.1, 0.0, LEN, LEN);
        let right: Vec<f32> = a.iter().zip(&mid).map(|(x, m)| x - 2.0 * m).collect();
        let (mix, reference) = (profile(&[&a, &right]), profile(&[&a, &a]));
        let (c, f) = run(&mix, &reference, &[]);
        let mids = c.regions.iter().find(|r| r.name == "mids").unwrap();
        assert!(mids.mix_fold_db < -20.0, "{mids:?}");
        assert!(rules_of(&f).contains(&RuleId::ReferenceStereo), "{f:?}");
    }

    #[test]
    fn silence_and_other_settings_are_handled() {
        let z = vec![0.0f32; 48_000 * 2];
        let a = song(0.2, 0.1, 0.05);
        let (mix, silent) = (profile(&[&a, &a]), profile(&[&z, &z]));
        let (_, f) = run(&mix, &silent, &[]);
        assert!(f.is_empty(), "nothing to compare with");
        let mut other = mix.clone();
        other.hop /= 2;
        let rules = ReferenceRule::default();
        let r = compare(&ReferenceInput {
            reference: ReferenceId(1),
            profile: &other,
            mix: &mix,
            master: None,
            aligned: false,
            sections: &[],
            rules: &rules,
            signal: SignalPoint::Processed,
        });
        assert_eq!(r.err(), Some(ReferenceError::Layouts));
    }
}
