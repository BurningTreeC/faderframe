//! The health check: technical faults and waste — a category of its own
//! ([`crate::Category::Health`]).
//!
//! What it reads: the profiles of every track, bus, return and the master
//! (their DC offsets), and what a probe after every device measured
//! ([`DeviceRecord`]: the session gets them from the engine's capture
//! render, `faderframe_engine::capture`) with the processing time each
//! device took during that render (the engine's per-node timing).
//!
//! # Rules
//!
//! * [`RuleId::DcOffset`]: a channel's mean at or above
//!   [`HealthRule::dc_dbfs`] over at least [`HealthRule::dc_min_seconds`];
//!   severity `clamp(0.2 + 0.03 × dB above the threshold, 0.2, 0.6)`,
//!   confidence 0.9 (processed; 0.75 measured on the source). Experiment:
//!   the Utility's DC filter at the end of the track's devices.
//! * [`RuleId::DeviceInvalidSamples`]: invalid samples at a device's output
//!   while none reached its input; severity 0.9, confidence 0.95.
//! * [`RuleId::DeviceOverFullScale`]: a device's output above 0 dBFS (its
//!   input not, or its peak 1 dB lower) with another device after it;
//!   severity `clamp(0.25 + 0.05 × dB over, 0.25, 0.6)`, confidence 0.6
//!   (whether the next device clips is not known). Experiment: gain
//!   staging around the next device (the overs plus half a dB, rounded up
//!   to half dBs, at most [`HealthRule::max_trim_db`]).
//! * [`RuleId::DeviceClipping`]: at least [`HealthRule::min_flat_runs`]
//!   flattened runs at a device's output and [`HealthRule::flat_ratio`]
//!   times its input's; severity `clamp(0.35 + 0.1 × log10(runs / min),
//!   0.35, 0.7)`, confidence 0.6. Experiment: gain staging around it, 3 dB.
//! * [`RuleId::DeviceSilences`]: input RMS at or above
//!   [`HealthRule::signal_dbfs`], output under [`HealthRule::silent_dbfs`];
//!   severity 0.4, confidence 0.8; a manual check.
//! * [`RuleId::DeviceNoChange`]: not bypassed, input with signal, what it
//!   changes at least [`HealthRule::no_change_db`] under its input;
//!   severity `0.1 + 0.3 × min(1, cost / cost_share)` (0.15 without a
//!   timing), confidence 0.7. Experiment: bypass it.
//! * [`RuleId::DevicesBypassed`]: one finding per track listing its
//!   devices bypassed throughout; severity 0.1, confidence 1. Manual.
//!
//! Experiments around a device are offered only for devices the session
//! can place others beside ([`DeviceRecord::editable`]: a track's own
//! inserts, not a preamp slot or a device inside a container).

use crate::config::HealthRule;
use crate::finding::{
    Evidence, Experiment, Finding, Recommendation, RuleId, SignalPoint, Subject, Term, Unit, labels,
};
use crate::profile::{TrackProfile, power_db};
use faderframe_core::{PluginInstanceId, TrackId};

/// One subject whose signal was measured (a track, bus, return, the
/// master).
pub struct HealthSubject<'a> {
    pub subject: Subject,
    pub profile: &'a TrackProfile,
}

/// What a probe measured of one device (sums over the render; see
/// `faderframe_engine::capture::ProbeStats`).
#[derive(Clone, Debug, PartialEq)]
pub struct DeviceRecord {
    pub track: TrackId,
    pub plugin: PluginInstanceId,
    /// Its place among the track's devices (0: the first).
    pub position: usize,
    /// The device after it (`None`: the strip follows).
    pub next: Option<PluginInstanceId>,
    pub bypassed: bool,
    /// It makes sound of its own (an instrument): its input says nothing.
    pub instrument: bool,
    /// Others can be placed before and after it (a track's own insert).
    pub editable: bool,
    /// … and so can they around the next device.
    pub next_editable: bool,
    pub frames: u64,
    /// Channels at its input and output (energies sum over them).
    pub in_channels: usize,
    pub out_channels: usize,
    pub in_energy: f64,
    pub out_energy: f64,
    /// Σ (output − input)² (`None`: the device changes the channel count).
    pub diff_energy: Option<f64>,
    /// Peaks (linear).
    pub in_peak: f64,
    pub out_peak: f64,
    pub out_above: u64,
    pub in_above: u64,
    pub in_flat: u64,
    pub out_flat: u64,
    pub in_invalid: u64,
    pub out_invalid: u64,
    /// Its processing time over the render's audio time (`None`: not
    /// measured).
    pub cost: Option<f64>,
}

impl DeviceRecord {
    /// A device nothing was measured of yet.
    pub fn new(track: TrackId, plugin: PluginInstanceId) -> Self {
        Self {
            track,
            plugin,
            position: 0,
            next: None,
            bypassed: false,
            instrument: false,
            editable: false,
            next_editable: false,
            frames: 0,
            in_channels: 0,
            out_channels: 0,
            in_energy: 0.0,
            out_energy: 0.0,
            diff_energy: None,
            in_peak: 0.0,
            out_peak: 0.0,
            out_above: 0,
            in_above: 0,
            in_flat: 0,
            out_flat: 0,
            in_invalid: 0,
            out_invalid: 0,
            cost: None,
        }
    }

    fn rms_db(energy: f64, frames: u64, channels: usize) -> f64 {
        let n = (frames as f64 * channels.max(1) as f64).max(1.0);
        power_db(energy / n)
    }

    /// RMS of its input (dBFS, all channels).
    pub fn in_rms(&self) -> f64 {
        Self::rms_db(self.in_energy, self.frames, self.in_channels)
    }

    pub fn out_rms(&self) -> f64 {
        Self::rms_db(self.out_energy, self.frames, self.out_channels)
    }

    /// What it changes, relative to its input (dB; `None`: the channel
    /// count changes, or there is no input).
    pub fn change_db(&self) -> Option<f64> {
        let d = self.diff_energy?;
        (self.in_energy > 0.0).then(|| power_db(d) - power_db(self.in_energy))
    }

    fn db(v: f64) -> f64 {
        if v > 0.0 { 20.0 * v.log10() } else { -150.0 }
    }
}

/// What [`diagnose`] reads.
pub struct HealthInput<'a> {
    pub subjects: Vec<HealthSubject<'a>>,
    pub devices: &'a [DeviceRecord],
    pub signal: SignalPoint,
    pub rules: &'a HealthRule,
}

fn id_of(rule: RuleId, signal: SignalPoint, parts: &[u64]) -> u64 {
    let mut h = crate::Fnv::new();
    h.write_u64(rule as u64);
    h.write_u64(signal as u64);
    for p in parts {
        h.write_u64(*p);
    }
    h.finish()
}

fn round_up(v: f64, step: f64) -> f64 {
    (v / step).ceil() * step
}

#[allow(clippy::too_many_arguments)]
fn finding(
    rule: RuleId,
    signal: SignalPoint,
    subject: Subject,
    parts: &[u64],
    (severity, severity_terms): (f64, Vec<Term>),
    (confidence, confidence_terms): (f64, Vec<Term>),
    evidence: Vec<Evidence>,
    recommendations: Vec<Recommendation>,
    caveats: Vec<String>,
) -> Finding {
    let tracks = match subject {
        Subject::Track(t) | Subject::Master(Some(t)) => vec![t],
        Subject::Pair { target, masker } => vec![target, masker],
        Subject::Tracks { a, b } => vec![a, b],
        Subject::Master(None) => Vec::new(),
    };
    Finding {
        id: id_of(rule, signal, parts),
        rule,
        signal,
        subject,
        tracks,
        time: None,
        events: Vec::new(),
        pattern: None,
        frequency: None,
        severity: severity.clamp(0.0, 1.0),
        severity_terms,
        confidence: confidence.clamp(0.0, 1.0),
        confidence_terms,
        evidence,
        spectrum: None,
        sections: Vec::new(),
        recommendations,
        caveats,
    }
}

fn device_evidence(d: &DeviceRecord) -> Evidence {
    Evidence::new(
        labels::DEVICE,
        d.plugin.raw() as f64,
        Unit::Device,
        Some(d.track),
    )
}

fn manual(text: &str, purpose: &str) -> Recommendation {
    Recommendation {
        experiment: Experiment::Manual { text: text.into() },
        purpose: purpose.into(),
        side_effects: "Nothing is changed.".into(),
    }
}

fn gain_stage(track: TrackId, device: PluginInstanceId, trim_db: f64) -> Recommendation {
    Recommendation {
        experiment: Experiment::GainStage {
            track,
            device,
            trim_db,
        },
        purpose: format!(
            "Drive the device {:.1} dB less and give the level back after it, so it is fed \
             less hot while the track keeps its level.",
            -trim_db
        ),
        side_effects: "A level-dependent device (a compressor, a saturator, a gate) reacts \
                       differently to a quieter input: compare loudness-matched; the level after \
                       it is exactly restored only for a linear device."
            .into(),
    }
}

fn dc_offsets(input: &HealthInput<'_>, out: &mut Vec<Finding>) {
    let r = input.rules;
    let conf = match input.signal {
        SignalPoint::Processed => 0.9,
        SignalPoint::Source => 0.75,
    };
    for s in &input.subjects {
        let p = s.profile;
        let dc = p.dc_dbfs();
        if p.seconds() < r.dc_min_seconds || dc < r.dc_dbfs {
            continue;
        }
        let track = match s.subject {
            Subject::Track(t) | Subject::Master(Some(t)) => Some(t),
            _ => None,
        };
        let severity = (0.2 + 0.03 * (dc - r.dc_dbfs)).clamp(0.2, 0.6);
        let mut evidence = vec![Evidence::new(labels::DC, dc, Unit::Dbfs, track)];
        for (c, v) in p.dc.iter().enumerate() {
            evidence.push(Evidence::new(
                format!("Mean of channel {}", c + 1),
                if v.abs() > 0.0 {
                    20.0 * v.abs().log10()
                } else {
                    -150.0
                },
                Unit::Dbfs,
                track,
            ));
        }
        let mut recs = Vec::new();
        if let Some(t) = track {
            recs.push(Recommendation {
                experiment: Experiment::DcFilter { track: t },
                purpose: "Take the offset out where the signal leaves the track (the Utility's \
                          DC filter, a gentle high pass far under the music)."
                    .into(),
                side_effects: "The filter also takes away content under about 5 Hz, which is \
                               not heard."
                    .into(),
            });
        }
        recs.push(manual(
            "look for the source of the offset: a recording with an offset, a device adding \
             one, or a bus summing several",
            "Fix it where it comes from.",
        ));
        out.push(finding(
            RuleId::DcOffset,
            input.signal,
            s.subject,
            &[track.map_or(u64::MAX, |t| t.0)],
            (
                severity,
                vec![Term {
                    name: "dB above the threshold",
                    value: dc - r.dc_dbfs,
                }],
            ),
            (conf, Vec::new()),
            evidence,
            recs,
            Vec::new(),
        ));
    }
}

fn devices(input: &HealthInput<'_>, out: &mut Vec<Finding>) {
    let r = input.rules;
    let sig = input.signal;
    for d in input.devices {
        let subject = Subject::Track(d.track);
        let parts = [d.track.0, d.plugin.raw()];
        let cost = d
            .cost
            .map(|c| Evidence::new(labels::COST, c, Unit::Share, Some(d.track)));
        if d.out_invalid > 0 && d.in_invalid == 0 {
            out.push(finding(
                RuleId::DeviceInvalidSamples,
                sig,
                subject,
                &parts,
                (0.9, Vec::new()),
                (0.95, Vec::new()),
                vec![
                    device_evidence(d),
                    Evidence::new(
                        labels::OUT_INVALID,
                        d.out_invalid as f64,
                        Unit::Count,
                        Some(d.track),
                    ),
                ],
                vec![manual(
                    "bypass the device and check its settings; update or replace it if it \
                     keeps failing",
                    "Stop the invalid samples where they start.",
                )],
                Vec::new(),
            ));
            continue;
        }
        if d.bypassed {
            continue;
        }
        let peak_out = DeviceRecord::db(d.out_peak);
        let peak_in = DeviceRecord::db(d.in_peak);
        if let Some(next) = d.next
            && d.out_above > 0
            && (d.in_above == 0 || peak_out - peak_in >= 1.0)
        {
            let over = peak_out.max(0.0);
            let trim = -round_up(over + 0.5, 0.5).min(r.max_trim_db);
            let mut recs = Vec::new();
            if d.next_editable {
                recs.push(gain_stage(d.track, next, trim));
            }
            recs.push(manual(
                "lower the device's output (or the level into it) so what it passes on stays \
                 under 0 dBFS",
                "Keep the signal between the devices under full scale.",
            ));
            out.push(finding(
                RuleId::DeviceOverFullScale,
                sig,
                subject,
                &parts,
                (
                    (0.25 + 0.05 * over).clamp(0.25, 0.6),
                    vec![Term {
                        name: "dB over",
                        value: over,
                    }],
                ),
                (0.6, Vec::new()),
                vec![
                    device_evidence(d),
                    Evidence::new(
                        labels::NEXT_DEVICE,
                        next.raw() as f64,
                        Unit::Device,
                        Some(d.track),
                    ),
                    Evidence::new(labels::OUT_PEAK, peak_out, Unit::Dbfs, Some(d.track)),
                    Evidence::new(labels::IN_PEAK, peak_in, Unit::Dbfs, Some(d.track)),
                    Evidence::new(
                        labels::OUT_ABOVE,
                        d.out_above as f64,
                        Unit::Count,
                        Some(d.track),
                    ),
                ],
                recs,
                Vec::new(),
            ));
        }
        if !d.instrument
            && d.out_flat >= r.min_flat_runs
            && d.out_flat as f64 >= r.flat_ratio * d.in_flat.max(1) as f64
        {
            let ratio = d.out_flat as f64 / r.min_flat_runs.max(1) as f64;
            let mut recs = Vec::new();
            if d.editable {
                recs.push(gain_stage(d.track, d.plugin, -3.0));
            }
            recs.push(manual(
                "lower the device's drive or input level, or raise its ceiling",
                "Stop it from flattening the peaks if that is not the sound wanted.",
            ));
            out.push(finding(
                RuleId::DeviceClipping,
                sig,
                subject,
                &parts,
                (
                    (0.35 + 0.1 * ratio.log10()).clamp(0.35, 0.7),
                    vec![Term {
                        name: "runs over the minimum",
                        value: ratio,
                    }],
                ),
                (0.6, Vec::new()),
                vec![
                    device_evidence(d),
                    Evidence::new(
                        labels::OUT_FLAT,
                        d.out_flat as f64,
                        Unit::Count,
                        Some(d.track),
                    ),
                    Evidence::new(
                        labels::IN_FLAT,
                        d.in_flat as f64,
                        Unit::Count,
                        Some(d.track),
                    ),
                    Evidence::new(labels::OUT_PEAK, peak_out, Unit::Dbfs, Some(d.track)),
                ],
                recs,
                vec!["Clippers, limiters and saturators do this by design.".into()],
            ));
        }
        if d.instrument || d.frames == 0 {
            continue;
        }
        let (in_rms, out_rms) = (d.in_rms(), d.out_rms());
        if in_rms < r.signal_dbfs {
            continue;
        }
        if out_rms < r.silent_dbfs {
            out.push(finding(
                RuleId::DeviceSilences,
                sig,
                subject,
                &parts,
                (0.4, Vec::new()),
                (0.8, Vec::new()),
                vec![
                    device_evidence(d),
                    Evidence::new(labels::IN_RMS, in_rms, Unit::Dbfs, Some(d.track)),
                    Evidence::new(labels::OUT_RMS, out_rms, Unit::Dbfs, Some(d.track)),
                ],
                vec![manual(
                    "check the device's settings: a mute, a gate's threshold, a sidechain it \
                     waits for, an output turned down",
                    "Find out whether the silence is meant.",
                )],
                Vec::new(),
            ));
            continue;
        }
        if let Some(change) = d.change_db()
            && change <= -r.no_change_db
        {
            let weight = d
                .cost
                .map_or(0.15, |c| 0.1 + 0.3 * (c / r.cost_share.max(1e-9)).min(1.0));
            let mut evidence = vec![
                device_evidence(d),
                Evidence::new(labels::CHANGE, change, Unit::Db, Some(d.track)),
                Evidence::new(labels::IN_RMS, in_rms, Unit::Dbfs, Some(d.track)),
            ];
            evidence.extend(cost.clone());
            let mut recs = Vec::new();
            if d.editable {
                recs.push(Recommendation {
                    experiment: Experiment::Bypass {
                        track: d.track,
                        device: d.plugin,
                    },
                    purpose: "Bypass it and listen: if nothing changes, its processing is spent \
                              for nothing here."
                        .into(),
                    side_effects: "It may act elsewhere in the song, or on louder input; a \
                                   bypass of a device with latency changes the timing of the \
                                   track by its delay compensation (the graph rebuilds)."
                        .into(),
                });
            }
            recs.push(manual(
                "if the device is not needed here, remove it (or keep it bypassed)",
                "Spend the processing where it changes something.",
            ));
            out.push(finding(
                RuleId::DeviceNoChange,
                sig,
                subject,
                &parts,
                (
                    weight,
                    vec![Term {
                        name: "cost weight",
                        value: weight,
                    }],
                ),
                (0.7, Vec::new()),
                evidence,
                recs,
                if d.cost.is_none() {
                    vec!["Its processing time was not measured.".into()]
                } else {
                    Vec::new()
                },
            ));
        }
    }
    // Bypassed devices, a finding per track.
    let mut tracks: Vec<TrackId> = input
        .devices
        .iter()
        .filter(|d| d.bypassed)
        .map(|d| d.track)
        .collect();
    tracks.dedup();
    for t in tracks {
        let list: Vec<&DeviceRecord> = input
            .devices
            .iter()
            .filter(|d| d.bypassed && d.track == t)
            .collect();
        let mut evidence: Vec<Evidence> = list.iter().map(|d| device_evidence(d)).collect();
        let cost: f64 = list.iter().filter_map(|d| d.cost).sum();
        if list.iter().any(|d| d.cost.is_some()) {
            evidence.push(Evidence::new(labels::COST, cost, Unit::Share, Some(t)));
        }
        let mut parts = vec![t.0];
        parts.extend(list.iter().map(|d| d.plugin.raw()));
        out.push(finding(
            RuleId::DevicesBypassed,
            sig,
            Subject::Track(t),
            &parts,
            (0.1, Vec::new()),
            (1.0, Vec::new()),
            evidence,
            vec![manual(
                "remove the bypassed devices that are no longer wanted",
                "Keep the project lean (their instances stay loaded).",
            )],
            Vec::new(),
        ));
    }
}

/// Run the health rules (see the module docs): findings in no particular
/// order (the caller ranks them with the rest).
pub fn diagnose(input: &HealthInput<'_>) -> Vec<Finding> {
    let mut out = Vec::new();
    dc_offsets(input, &mut out);
    devices(input, &mut out);
    out.retain(|f| f.severity.is_finite() && f.confidence.is_finite());
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{profile, tone};

    fn record(plugin: u64) -> DeviceRecord {
        DeviceRecord {
            track: TrackId(1),
            plugin: PluginInstanceId(plugin),
            position: 0,
            next: Some(PluginInstanceId(plugin + 1)),
            editable: true,
            next_editable: true,
            frames: 48_000,
            in_channels: 2,
            out_channels: 2,
            in_energy: 0.1 * 96_000.0,
            out_energy: 0.1 * 96_000.0,
            diff_energy: Some(0.1 * 96_000.0 * 0.01),
            in_peak: 0.5,
            out_peak: 0.5,
            cost: Some(0.01),
            ..DeviceRecord::new(TrackId(1), PluginInstanceId(plugin))
        }
    }

    fn run(subjects: Vec<HealthSubject<'_>>, devices: &[DeviceRecord]) -> Vec<Finding> {
        let rules = HealthRule::default();
        diagnose(&HealthInput {
            subjects,
            devices,
            signal: SignalPoint::Processed,
            rules: &rules,
        })
    }

    fn rules_of(f: &[Finding]) -> Vec<RuleId> {
        f.iter().map(|f| f.rule).collect()
    }

    #[test]
    fn a_dc_offset_is_found_and_a_clean_signal_is_not() {
        let clean = tone(100.0, 0.5, 0.0, 4.0, 4.0);
        let offset: Vec<f32> = clean.iter().map(|v| v + 0.01).collect();
        let (a, b) = (profile(&[&clean]), profile(&[&offset]));
        let f = run(
            vec![
                HealthSubject {
                    subject: Subject::Track(TrackId(1)),
                    profile: &a,
                },
                HealthSubject {
                    subject: Subject::Track(TrackId(2)),
                    profile: &b,
                },
            ],
            &[],
        );
        assert_eq!(rules_of(&f), vec![RuleId::DcOffset]);
        assert_eq!(f[0].tracks, vec![TrackId(2)]);
        assert!(matches!(
            f[0].recommendations[0].experiment,
            Experiment::DcFilter { track: TrackId(2) }
        ));
        // A short stretch says nothing.
        let short: Vec<f32> = offset[..48_000].to_vec();
        let c = profile(&[&short]);
        let f = run(
            vec![HealthSubject {
                subject: Subject::Track(TrackId(3)),
                profile: &c,
            }],
            &[],
        );
        assert!(f.is_empty());
    }

    #[test]
    fn devices_are_checked_one_by_one() {
        // An ordinary device: nothing to say.
        assert!(run(Vec::new(), &[record(10)]).is_empty());
        // Over full scale into the next one.
        let hot = DeviceRecord {
            out_peak: 1.5,
            out_above: 400,
            ..record(10)
        };
        let f = run(Vec::new(), std::slice::from_ref(&hot));
        assert_eq!(rules_of(&f), vec![RuleId::DeviceOverFullScale]);
        assert_eq!(f[0].device(), Some(PluginInstanceId(10)));
        match f[0].recommendations[0].experiment {
            Experiment::GainStage {
                device, trim_db, ..
            } => {
                assert_eq!(device, PluginInstanceId(11), "around the next device");
                // 1.5 is 3.52 dB over, + 0.5, rounded up to half dBs.
                assert_eq!(trim_db, -4.5);
            }
            ref e => panic!("{e:?}"),
        }
        // The last device's overs are the track's (no finding here).
        let last = DeviceRecord { next: None, ..hot };
        assert!(run(Vec::new(), &[last]).is_empty());
        // Overs passed on from before are not this device's.
        let passed = DeviceRecord {
            in_peak: 1.5,
            in_above: 400,
            out_peak: 1.5,
            out_above: 400,
            ..record(10)
        };
        assert!(run(Vec::new(), &[passed]).is_empty());
        // Flattened peaks the input did not have.
        let clip = DeviceRecord {
            out_flat: 500,
            in_flat: 2,
            ..record(10)
        };
        assert_eq!(
            rules_of(&run(Vec::new(), &[clip])),
            vec![RuleId::DeviceClipping]
        );
        // A square wave in, a square wave out: not the device's doing.
        let square = DeviceRecord {
            out_flat: 500,
            in_flat: 480,
            ..record(10)
        };
        assert!(run(Vec::new(), &[square]).is_empty());
        // Silence out of signal.
        let mute = DeviceRecord {
            out_energy: 0.0,
            out_peak: 0.0,
            diff_energy: Some(0.1 * 96_000.0),
            ..record(10)
        };
        assert_eq!(
            rules_of(&run(Vec::new(), &[mute])),
            vec![RuleId::DeviceSilences]
        );
        // Changes nothing: reported with its cost, bypassing offered.
        let idle = DeviceRecord {
            diff_energy: Some(0.1 * 96_000.0 * 1e-12),
            ..record(10)
        };
        let f = run(Vec::new(), std::slice::from_ref(&idle));
        assert_eq!(rules_of(&f), vec![RuleId::DeviceNoChange]);
        assert!(matches!(
            f[0].recommendations[0].experiment,
            Experiment::Bypass { .. }
        ));
        let cheap = run(
            Vec::new(),
            &[DeviceRecord {
                cost: Some(0.0001),
                ..idle.clone()
            }],
        );
        assert!(cheap[0].severity < f[0].severity, "costlier weighs more");
        // A device with nothing reaching it says nothing.
        let quiet = DeviceRecord {
            in_energy: 1e-20,
            out_energy: 1e-20,
            diff_energy: Some(0.0),
            ..record(10)
        };
        assert!(run(Vec::new(), &[quiet]).is_empty());
        // An instrument makes sound from nothing: not "changes nothing" or
        // "silences".
        let synth = DeviceRecord {
            instrument: true,
            in_energy: 0.0,
            ..record(10)
        };
        assert!(run(Vec::new(), &[synth]).is_empty());
        // Invalid samples where they start only.
        let nan = DeviceRecord {
            out_invalid: 10,
            ..record(10)
        };
        let after = DeviceRecord {
            in_invalid: 10,
            out_invalid: 10,
            ..record(11)
        };
        let f = run(Vec::new(), &[nan, after]);
        assert_eq!(rules_of(&f), vec![RuleId::DeviceInvalidSamples]);
        // Bypassed ones, one finding per track.
        let by = |p| DeviceRecord {
            bypassed: true,
            cost: Some(0.0),
            ..record(p)
        };
        let f = run(Vec::new(), &[by(20), by(21)]);
        assert_eq!(rules_of(&f), vec![RuleId::DevicesBypassed]);
        assert_eq!(
            f[0].evidence
                .iter()
                .filter(|e| e.unit == Unit::Device)
                .count(),
            2
        );
        // Devices that cannot be worked around get manual suggestions only.
        let fixed = DeviceRecord {
            editable: false,
            diff_energy: Some(0.1 * 96_000.0 * 1e-12),
            ..record(10)
        };
        let f = run(Vec::new(), &[fixed]);
        assert!(
            f[0].recommendations
                .iter()
                .all(|r| !r.experiment.previewable())
        );
    }
}
