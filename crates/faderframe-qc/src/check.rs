//! A file against a spec: the spec's own rules (format, length, loudness,
//! true peak) and the checks every delivery gets (clipping, inter-sample
//! overs, DC offset, silence, gaps, phase and mono compatibility).
//!
//! A stem or another part of a programme is not judged on the spec's
//! loudness or ceiling (the programme is: the mix of a null test, the
//! songs of an album); its figures are listed as information.

use crate::report::{Check, Facts, Levels, Role, Verdict, level, time};
use crate::spec::Spec;

/// A DC offset above this warns (dBFS; 0.3 % of full scale).
pub const DC_WARN_DB: f64 = -50.0;
/// Correlation over the file below this fails (more out of phase than
/// in).
pub const CORRELATION_FAIL: f64 = 0.0;
/// A 400 ms window below this warns.
pub const WINDOW_WARN: f64 = -0.5;
/// The mono fold-down quieter than the stereo by more than this warns
/// (LU; unrelated sides lose 3).
pub const MONO_WARN_LU: f64 = -4.5;
/// … and by more than this fails.
pub const MONO_FAIL_LU: f64 = -9.0;

fn check(
    id: &str,
    label: &str,
    verdict: Verdict,
    measured: String,
    requirement: impl Into<String>,
    value: Option<(f64, &str)>,
) -> Check {
    Check {
        id: id.into(),
        label: label.into(),
        verdict,
        measured,
        requirement: requirement.into(),
        value: value.map(|(v, _)| v),
        unit: value.map(|(_, u)| u.to_string()),
    }
}

fn rate_text(r: u32) -> String {
    if r.is_multiple_of(1000) {
        format!("{} kHz", r / 1000)
    } else {
        format!("{:.1} kHz", f64::from(r) / 1000.0)
    }
}

fn bits_text(b: u16) -> String {
    if b == 32 {
        "32-bit float".into()
    } else {
        format!("{b}-bit")
    }
}

fn plural(n: u64, one: &str, many: &str) -> String {
    if n == 1 {
        format!("1 {one}")
    } else {
        format!("{n} {many}")
    }
}

/// The checks of one file (`levels`: `None` when its audio was not
/// measured — a coded file).
pub fn checks(facts: &Facts, levels: Option<&Levels>, spec: &Spec, role: Role) -> Vec<Check> {
    let mut out = Vec::new();
    format_checks(&mut out, facts, spec, role);
    let Some(l) = levels else {
        out.push(check(
            "levels",
            "Levels",
            Verdict::Info,
            "not measured (the file is not decoded here)".into(),
            "—",
            None,
        ));
        return out;
    };
    loudness_checks(&mut out, l, spec, role);
    clipping_checks(&mut out, facts, l, spec);
    signal_checks(&mut out, l, role);
    phase_checks(&mut out, l);
    out
}

fn format_checks(out: &mut Vec<Check>, f: &Facts, spec: &Spec, role: Role) {
    let fails = if spec.format_fails {
        Verdict::Fail
    } else {
        Verdict::Warn
    };
    let wanted: Vec<String> = spec.sample_rates.iter().map(|r| rate_text(*r)).collect();
    out.push(check(
        "format.sample_rate",
        "Sample rate",
        if wanted.is_empty() {
            Verdict::Info
        } else if spec.sample_rates.contains(&f.sample_rate) {
            Verdict::Pass
        } else {
            fails
        },
        rate_text(f.sample_rate),
        if wanted.is_empty() {
            "—".into()
        } else {
            wanted.join(" or ")
        },
        Some((f64::from(f.sample_rate), "Hz")),
    ));
    let depth = f.bits.map(|b| if f.float { 32 } else { b });
    let wanted: Vec<String> = spec.bits.iter().map(|b| bits_text(*b)).collect();
    out.push(check(
        "format.bit_depth",
        "Bit depth",
        match depth {
            _ if wanted.is_empty() => Verdict::Info,
            Some(b) if spec.bits.contains(&b) => Verdict::Pass,
            Some(_) => fails,
            None => Verdict::Info,
        },
        depth.map_or_else(|| f.encoding.clone(), bits_text),
        if wanted.is_empty() {
            "—".into()
        } else {
            wanted.join(" or ")
        },
        depth.map(|b| (f64::from(b), "bits")),
    ));
    out.push(check(
        "format.channels",
        "Channels",
        match spec.channels {
            Some(n) if n == f.channels => Verdict::Pass,
            Some(_) => Verdict::Fail,
            None => Verdict::Info,
        },
        format!("{} ({})", f.channels, f.layout),
        spec.channels.map_or("—".into(), |n| n.to_string()),
        Some((f.channels as f64, "channels")),
    ));
    let mut length = Verdict::Info;
    let mut wanted = Vec::new();
    if role == Role::Programme {
        if let Some(min) = spec.min_seconds {
            wanted.push(format!("at least {}", time(min)));
            length = if f.seconds >= min {
                Verdict::Pass
            } else {
                Verdict::Fail
            };
        }
        if let Some(max) = spec.max_seconds {
            wanted.push(format!("at most {}", time(max)));
            if f.seconds > max {
                length = Verdict::Fail;
            } else if length == Verdict::Info {
                length = Verdict::Pass;
            }
        }
    }
    out.push(check(
        "format.duration",
        "Duration",
        length,
        format!("{} ({} frames)", time(f.seconds), f.frames),
        if wanted.is_empty() {
            "—".into()
        } else {
            wanted.join(", ")
        },
        Some((f.seconds, "s")),
    ));
}

fn loudness_checks(out: &mut Vec<Check>, l: &Levels, spec: &Spec, role: Role) {
    let programme = role == Role::Programme;
    let part = |what: &str| format!("— ({what} is judged on the programme)");
    let integrated = l.loudness.and_then(|x| x.integrated_lufs);
    match (spec.loudness, &l.loudness) {
        (_, None) => out.push(check(
            "loudness.integrated",
            "Integrated loudness",
            Verdict::Info,
            "not measured (see the notes)".into(),
            "—",
            None,
        )),
        (Some(rule), Some(_)) if programme => {
            let lo = rule.target - rule.tolerance;
            let hi = rule.target + rule.tolerance;
            let off = |v: Verdict| if rule.fails { Verdict::Fail } else { v };
            let (verdict, why) = match integrated {
                Some(v) if (lo - 0.05..=hi + 0.05).contains(&v) => (Verdict::Pass, String::new()),
                Some(v) if v > hi => (
                    off(Verdict::Warn),
                    format!(" — {:.1} LU over the target", v - rule.target),
                ),
                Some(v) => (
                    off(Verdict::Warn),
                    format!(" — {:.1} LU under the target", rule.target - v),
                ),
                None => (
                    off(Verdict::Warn),
                    " — silent or shorter than 400 ms".into(),
                ),
            };
            let verdict = if rule.approximate.is_some() {
                verdict.min(Verdict::Warn)
            } else {
                verdict
            };
            let approx = if rule.approximate.is_some() {
                " (programme, not dialogue-gated)"
            } else {
                ""
            };
            out.push(check(
                "loudness.integrated",
                "Integrated loudness",
                verdict,
                format!("{}{approx}{why}", level(integrated, "LUFS")),
                format!("{:.1} ±{:.1} LUFS", rule.target, rule.tolerance).replace('-', "−"),
                integrated.map(|v| (v, "LUFS")),
            ));
        }
        (rule, Some(_)) => out.push(check(
            "loudness.integrated",
            "Integrated loudness",
            Verdict::Info,
            level(integrated, "LUFS"),
            if rule.is_some() {
                part("loudness")
            } else {
                "—".into()
            },
            integrated.map(|v| (v, "LUFS")),
        )),
    }
    let tp = l.true_peak_dbtp;
    let at = l
        .true_peak_at
        .filter(|_| tp.is_some())
        .map_or_else(String::new, |t| format!(" at {}", time(t)));
    match spec.true_peak {
        Some(max) if programme => out.push(check(
            "loudness.true_peak",
            "True peak",
            match tp {
                Some(v) if v > max + 0.05 => {
                    if spec.true_peak_fails {
                        Verdict::Fail
                    } else {
                        Verdict::Warn
                    }
                }
                _ => Verdict::Pass,
            },
            format!("{}{at}", level(tp, "dBTP")),
            format!("at most {}", level(Some(max), "dBTP")),
            tp.map(|v| (v, "dBTP")),
        )),
        rule => out.push(check(
            "loudness.true_peak",
            "True peak",
            Verdict::Info,
            format!("{}{at}", level(tp, "dBTP")),
            if rule.is_some() {
                part("the ceiling")
            } else {
                "—".into()
            },
            tp.map(|v| (v, "dBTP")),
        )),
    }
    let sp = l.sample_peak_dbfs;
    out.push(check(
        "loudness.sample_peak",
        "Sample peak",
        Verdict::Info,
        level(sp, "dBFS"),
        "—",
        sp.map(|v| (v, "dBFS")),
    ));
    out.push(check(
        "loudness.rms",
        "RMS",
        Verdict::Info,
        level(l.rms_dbfs, "dBFS"),
        "—",
        l.rms_dbfs.map(|v| (v, "dBFS")),
    ));
    if let Some(x) = &l.loudness {
        out.push(check(
            "loudness.range",
            "Loudness range",
            Verdict::Info,
            format!("{:.1} LU", x.range_lu),
            "—",
            Some((x.range_lu, "LU")),
        ));
        let st = x.max_short_term_lufs;
        out.push(match spec.max_short_term() {
            Some(max) if programme => check(
                "loudness.max_short_term",
                "Max. short-term loudness",
                match st {
                    Some(v) if v > max + 0.05 => Verdict::Warn,
                    _ => Verdict::Pass,
                },
                level(st, "LUFS"),
                format!("at most {} (recommended)", level(Some(max), "LUFS")),
                st.map(|v| (v, "LUFS")),
            ),
            _ => check(
                "loudness.max_short_term",
                "Max. short-term loudness",
                Verdict::Info,
                level(st, "LUFS"),
                "—",
                st.map(|v| (v, "LUFS")),
            ),
        });
        out.push(check(
            "loudness.max_momentary",
            "Max. momentary loudness",
            Verdict::Info,
            level(x.max_momentary_lufs, "LUFS"),
            "—",
            x.max_momentary_lufs.map(|v| (v, "LUFS")),
        ));
        out.push(check(
            "loudness.plr",
            "Peak to loudness ratio (PLR)",
            Verdict::Info,
            x.plr_lu
                .map_or_else(|| "—".into(), |v| format!("{v:.1} LU")),
            "—",
            x.plr_lu.map(|v| (v, "LU")),
        ));
    }
}

fn clipping_checks(out: &mut Vec<Check>, f: &Facts, l: &Levels, spec: &Spec) {
    if f.float {
        out.push(check(
            "clipping.above_full_scale",
            "Samples past 0 dBFS",
            if l.above_full_scale > 0 {
                Verdict::Warn
            } else {
                Verdict::Pass
            },
            if l.above_full_scale > 0 {
                format!(
                    "{} (kept by the float file, clipped by any fixed-point conversion)",
                    l.above_full_scale
                )
            } else {
                "none".into()
            },
            "none",
            Some((l.above_full_scale as f64, "samples")),
        ));
    }
    let runs = l.clipped_runs;
    let where_ = l
        .clip_runs
        .first()
        .map_or_else(String::new, |s| format!(", the first at {}", time(s.at)));
    let longest = l
        .longest_clip_run
        .filter(|_| runs > 1)
        .map_or_else(String::new, |s| {
            format!(", the longest {:.1} ms", s.seconds * 1000.0)
        });
    let verdict = if runs > 0 {
        Verdict::Fail
    } else if l.full_scale_samples > 0 && (spec.no_full_scale || !f.float) {
        if spec.no_full_scale {
            Verdict::Fail
        } else {
            Verdict::Warn
        }
    } else {
        Verdict::Pass
    };
    out.push(check(
        "clipping.full_scale",
        "Clipping",
        verdict,
        if runs > 0 {
            format!(
                "{} of 3+ samples at full scale{where_}{longest}",
                plural(runs, "run", "runs")
            )
        } else if l.full_scale_samples > 0 {
            format!(
                "{} at full scale, no run of 3",
                plural(l.full_scale_samples, "sample", "samples")
            )
        } else {
            "none".into()
        },
        if spec.no_full_scale {
            "no sample at full scale"
        } else {
            "no run of 3 samples at full scale"
        },
        Some((runs as f64, "runs")),
    ));
    let overs = l.inter_sample_overs;
    out.push(check(
        "clipping.inter_sample",
        "Inter-sample overs",
        if overs > 0 {
            Verdict::Warn
        } else {
            Verdict::Pass
        },
        if overs > 0 {
            format!(
                "{}{}",
                plural(overs, "over", "overs"),
                l.first_inter_sample_over_at
                    .map_or_else(String::new, |t| format!(", the first at {}", time(t)))
            )
        } else {
            "none".into()
        },
        "none (true peak under 0 dBTP)",
        Some((overs as f64, "overs")),
    ));
}

fn signal_checks(out: &mut Vec<Check>, l: &Levels, role: Role) {
    let worst = l
        .channels
        .iter()
        .enumerate()
        .filter_map(|(i, c)| c.dc_offset_dbfs.map(|d| (i, d, c.dc_offset)))
        .max_by(|a, b| a.1.total_cmp(&b.1));
    out.push(match worst {
        Some((ch, d, v)) => check(
            "signal.dc_offset",
            "DC offset",
            if d > DC_WARN_DB {
                Verdict::Warn
            } else {
                Verdict::Pass
            },
            format!(
                "{} ({:+.4} %) on channel {}",
                level(Some(d), "dBFS"),
                v * 100.0,
                ch + 1
            ),
            format!("under {}", level(Some(DC_WARN_DB), "dBFS")),
            Some((d, "dBFS")),
        ),
        None => check(
            "signal.dc_offset",
            "DC offset",
            Verdict::Pass,
            "none".into(),
            format!("under {}", level(Some(DC_WARN_DB), "dBFS")),
            None,
        ),
    });
    let Some(head) = l.head_silence else {
        out.push(check(
            "signal.silence",
            "Silence and gaps",
            Verdict::Info,
            "not measured".into(),
            "—",
            None,
        ));
        return;
    };
    if l.silent {
        // A stem of a track that plays nothing in the range may be silent.
        out.push(check(
            "signal.silence",
            "Silence",
            if role == Role::Programme {
                Verdict::Fail
            } else {
                Verdict::Warn
            },
            "the file is silent".into(),
            "sound",
            None,
        ));
        return;
    }
    let floor = l.silence_floor_dbfs.map_or_else(String::new, |f| {
        format!(" (under {})", level(Some(f), "dBFS"))
    });
    out.push(check(
        "signal.head_silence",
        "Silence at the head",
        Verdict::Info,
        format!("{}{floor}", time(head)),
        "—",
        Some((head, "s")),
    ));
    let tail = l.tail_silence.unwrap_or(0.0);
    out.push(check(
        "signal.tail_silence",
        "Silence at the tail",
        Verdict::Info,
        format!("{}{floor}", time(tail)),
        "—",
        Some((tail, "s")),
    ));
    let gaps = l.gap_count;
    out.push(check(
        "signal.gaps",
        "Digital-silence gaps",
        if gaps > 0 {
            Verdict::Warn
        } else {
            Verdict::Pass
        },
        if gaps > 0 {
            let first = l.gaps.first().map_or_else(String::new, |g| {
                format!(", the first at {} ({})", time(g.at), time(g.seconds))
            });
            format!("{}{first}", plural(gaps, "gap", "gaps"))
        } else {
            "none".into()
        },
        "none of 1 s or more inside the programme",
        Some((gaps as f64, "gaps")),
    ));
}

fn phase_checks(out: &mut Vec<Check>, l: &Levels) {
    let Some(st) = l.stereo else {
        return;
    };
    let window = st
        .min_correlation
        .zip(st.min_correlation_at)
        .map_or_else(String::new, |(c, at)| {
            format!("; lowest {c:+.2} at {}", time(at))
        });
    let verdict = if st.correlation < CORRELATION_FAIL {
        Verdict::Fail
    } else if st.min_correlation.is_some_and(|c| c < WINDOW_WARN) {
        Verdict::Warn
    } else {
        Verdict::Pass
    };
    out.push(check(
        "phase.correlation",
        "Phase correlation",
        verdict,
        format!(
            "{:+.2} over the file{window}; {:.0} % of the time out of phase",
            st.correlation,
            st.negative_share * 100.0
        ),
        format!(
            "at least {:+.1} over the file, no 400 ms under {:+.1}",
            CORRELATION_FAIL, WINDOW_WARN
        ),
        Some((st.correlation, "")),
    ));
    let change = st.mono_change_lu;
    out.push(check(
        "phase.mono",
        "Mono compatibility",
        match change {
            Some(c) if c < MONO_FAIL_LU => Verdict::Fail,
            Some(c) if c < MONO_WARN_LU => Verdict::Warn,
            Some(_) => Verdict::Pass,
            None if st.mono_lufs.is_none()
                && l.loudness.is_some_and(|x| x.integrated_lufs.is_some()) =>
            {
                Verdict::Fail
            }
            None => Verdict::Info,
        },
        match change {
            Some(c) => format!(
                "the mono fold-down {:.1} LU {} the stereo",
                c.abs(),
                if c < 0.0 { "under" } else { "over" }
            ),
            None if st.mono_lufs.is_none() => "the mono fold-down is silent".into(),
            None => "—".into(),
        },
        format!("losing at most {:.1} LU", -MONO_WARN_LU),
        change.map(|c| (c, "LU")),
    ));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::report::{ChannelFigures, Loudness, Stereo};

    fn facts(rate: u32, bits: u16, float: bool, seconds: f64) -> Facts {
        Facts {
            container: "WAV".into(),
            encoding: format!("{bits}-bit"),
            sample_rate: rate,
            bits: Some(bits),
            float,
            channels: 2,
            layout: "Stereo".into(),
            frames: (seconds * f64::from(rate)) as u64,
            seconds,
            bytes: 0,
            from_file: true,
        }
    }

    fn levels(integrated: f64, tp: f64) -> Levels {
        Levels {
            sample_peak_dbfs: Some(tp - 0.5),
            sample_peak_at: Some(1.0),
            true_peak_dbtp: Some(tp),
            true_peak_at: Some(1.0),
            rms_dbfs: Some(integrated - 3.0),
            loudness: Some(Loudness {
                integrated_lufs: Some(integrated),
                range_lu: 6.0,
                max_momentary_lufs: Some(integrated + 4.0),
                max_short_term_lufs: Some(integrated + 2.0),
                plr_lu: Some(tp - integrated),
            }),
            channels: vec![
                ChannelFigures {
                    sample_peak_dbfs: Some(tp - 0.5),
                    true_peak_dbtp: Some(tp),
                    rms_dbfs: Some(integrated - 3.0),
                    dc_offset: 1e-5,
                    dc_offset_dbfs: Some(-100.0),
                };
                2
            ],
            full_scale_samples: 0,
            above_full_scale: 0,
            clipped_runs: 0,
            clip_runs: Vec::new(),
            longest_clip_run: None,
            inter_sample_overs: 0,
            first_inter_sample_over_at: None,
            silence_floor_dbfs: Some(-114.0),
            head_silence: Some(0.01),
            tail_silence: Some(0.5),
            silent: false,
            gaps: Vec::new(),
            gap_count: 0,
            stereo: Some(Stereo {
                correlation: 0.6,
                min_correlation: Some(0.1),
                min_correlation_at: Some(3.0),
                negative_share: 0.0,
                mono_lufs: Some(integrated - 1.5),
                mono_change_lu: Some(-1.5),
            }),
        }
    }

    fn verdict(cs: &[Check], id: &str) -> Verdict {
        cs.iter()
            .find(|c| c.id == id)
            .map_or(Verdict::Info, |c| c.verdict)
    }

    #[test]
    fn a_broadcast_programme_is_judged_on_its_loudness_and_peak() {
        let spec = Spec::by_id("ebu-r128").unwrap();
        let f = facts(48_000, 24, false, 30.0);
        let ok = checks(&f, Some(&levels(-23.2, -1.5)), spec, Role::Programme);
        assert_eq!(verdict(&ok, "loudness.integrated"), Verdict::Pass);
        assert_eq!(verdict(&ok, "loudness.true_peak"), Verdict::Pass);
        assert_eq!(verdict(&ok, "format.sample_rate"), Verdict::Pass);
        assert_eq!(Verdict::worst(ok.iter().map(|c| c.verdict)), Verdict::Pass);
        let loud = checks(&f, Some(&levels(-20.0, -0.4)), spec, Role::Programme);
        assert_eq!(verdict(&loud, "loudness.integrated"), Verdict::Fail);
        assert_eq!(verdict(&loud, "loudness.true_peak"), Verdict::Fail);
        let c = loud.iter().find(|c| c.id == "loudness.integrated").unwrap();
        assert_eq!(c.value, Some(-20.0));
        assert!(c.measured.contains("3.0 LU over"), "{}", c.measured);
        assert_eq!(c.requirement, "−23.0 ±0.5 LUFS");
        // A stem is not judged on them.
        let stem = checks(&f, Some(&levels(-20.0, -0.4)), spec, Role::Stem);
        assert_eq!(verdict(&stem, "loudness.integrated"), Verdict::Info);
        assert_eq!(verdict(&stem, "loudness.true_peak"), Verdict::Info);
        // 44.1 kHz warns for broadcast.
        let cd_rate = checks(
            &facts(44_100, 24, false, 30.0),
            Some(&levels(-23.0, -2.0)),
            spec,
            Role::Programme,
        );
        assert_eq!(verdict(&cd_rate, "format.sample_rate"), Verdict::Warn);
    }

    #[test]
    fn streaming_warns_netflix_never_fails_on_loudness_and_cd_wants_red_book() {
        let f = facts(48_000, 24, false, 200.0);
        let s = checks(
            &f,
            Some(&levels(-9.0, -0.2)),
            Spec::by_id("streaming").unwrap(),
            Role::Programme,
        );
        assert_eq!(verdict(&s, "loudness.integrated"), Verdict::Warn);
        assert_eq!(verdict(&s, "loudness.true_peak"), Verdict::Warn);
        let n = checks(
            &f,
            Some(&levels(-14.0, -3.0)),
            Spec::by_id("netflix").unwrap(),
            Role::Programme,
        );
        assert_eq!(verdict(&n, "loudness.integrated"), Verdict::Warn);
        assert!(
            n.iter().any(|c| c.measured.contains("not dialogue-gated")),
            "says what it measured"
        );
        let cd = Spec::by_id("cd").unwrap();
        let wrong = checks(&f, Some(&levels(-9.0, -0.2)), cd, Role::Programme);
        assert_eq!(verdict(&wrong, "format.sample_rate"), Verdict::Fail);
        assert_eq!(verdict(&wrong, "format.bit_depth"), Verdict::Fail);
        let mut hot = levels(-9.0, 0.3);
        hot.full_scale_samples = 2;
        let right = checks(
            &facts(44_100, 16, false, 3.0),
            Some(&hot),
            cd,
            Role::Programme,
        );
        assert_eq!(verdict(&right, "format.sample_rate"), Verdict::Pass);
        assert_eq!(
            verdict(&right, "format.duration"),
            Verdict::Fail,
            "under 4 s"
        );
        assert_eq!(verdict(&right, "clipping.full_scale"), Verdict::Fail);
    }

    #[test]
    fn general_checks_find_clipping_offsets_gaps_and_phase() {
        let spec = Spec::none();
        let f = facts(48_000, 24, false, 60.0);
        let mut l = levels(-12.0, 0.4);
        l.clipped_runs = 3;
        l.clip_runs = vec![crate::report::Spot {
            at: 62.25,
            seconds: 0.001,
            channel: Some(0),
        }];
        l.inter_sample_overs = 5;
        l.first_inter_sample_over_at = Some(10.0);
        l.channels[1].dc_offset = 0.01;
        l.channels[1].dc_offset_dbfs = Some(-40.0);
        l.gap_count = 1;
        l.gaps = vec![crate::report::Spot {
            at: 20.0,
            seconds: 1.5,
            channel: None,
        }];
        l.stereo = Some(Stereo {
            correlation: -0.3,
            min_correlation: Some(-0.9),
            min_correlation_at: Some(4.0),
            negative_share: 0.7,
            mono_lufs: Some(-24.0),
            mono_change_lu: Some(-12.0),
        });
        let cs = checks(&f, Some(&l), spec, Role::Programme);
        assert_eq!(verdict(&cs, "clipping.full_scale"), Verdict::Fail);
        let clip = cs.iter().find(|c| c.id == "clipping.full_scale").unwrap();
        assert!(clip.measured.contains("1:02.250"), "{}", clip.measured);
        assert_eq!(verdict(&cs, "clipping.inter_sample"), Verdict::Warn);
        assert_eq!(verdict(&cs, "signal.dc_offset"), Verdict::Warn);
        assert_eq!(verdict(&cs, "signal.gaps"), Verdict::Warn);
        assert_eq!(verdict(&cs, "phase.correlation"), Verdict::Fail);
        assert_eq!(verdict(&cs, "phase.mono"), Verdict::Fail);
        // Nothing asked of the format without a spec.
        assert_eq!(verdict(&cs, "format.sample_rate"), Verdict::Info);
        // A silent programme fails, a silent stem warns.
        let mut silent = levels(-12.0, -1.0);
        silent.silent = true;
        let cs = checks(&f, Some(&silent), spec, Role::Programme);
        assert_eq!(verdict(&cs, "signal.silence"), Verdict::Fail);
        let cs = checks(&f, Some(&silent), spec, Role::Stem);
        assert_eq!(verdict(&cs, "signal.silence"), Verdict::Warn);
        // A coded file without levels is facts only.
        let cs = checks(&f, None, spec, Role::Programme);
        assert_eq!(Verdict::worst(cs.iter().map(|c| c.verdict)), Verdict::Info);
    }
}
