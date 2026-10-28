//! Mix Intelligence's one pass: every track, bus and return measured
//! where it reaches its destination in one render of the whole mix, the
//! health check (probes after the devices, their cost), the comparison
//! with a reference track, and the one listening gain.
#![allow(clippy::unwrap_used)]

use faderframe_audio_files::{GeneratorSpec, WavFormat, write_wav};
use faderframe_core::{ParameterId, PluginInstanceId, TrackId, builtin, db_to_gain};
use faderframe_engine::EngineConfig;
use faderframe_mix_intelligence::{Category, Experiment, RuleId, SignalPoint, Subject};
use faderframe_project::{
    AudioClip, AudioSource, Clip, ClipContent, ClipFades, Command, OutputRouting, PluginRef,
    PluginSlot, Project, SavedParameter, SourceSpec, StretchSettings, Track, TrackColor, TrackKind,
};
use faderframe_session::mix::{MixOp, MixScope};
use faderframe_session::references::ReferenceOp;
use faderframe_session::{Action, Session};
use faderframe_timeline::MusicalTime;
use std::time::Duration;

const SR: u32 = 48_000;

fn temp(name: &str) -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!("ff-mix-capture-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn clip_of(
    p: &mut Project,
    track: TrackId,
    source: faderframe_core::AudioSourceId,
    seconds: f64,
    gain_db: f32,
) {
    let rate = f64::from(SR);
    let clip = Clip {
        id: p.ids.allocate(),
        track,
        name: "clip".into(),
        color: None,
        start: MusicalTime::ZERO,
        muted: false,
        content: ClipContent::Audio(AudioClip {
            source,
            source_offset: 0,
            length: (seconds * rate) as i64,
            gain_db,
            fades: ClipFades::default(),
            stretch: StretchSettings::Off,
            reversed: false,
            warp: None,
            pitch: None,
            effects: None,
            spectral: None,
        }),
    };
    p.track_mut(track).unwrap().clips.push(clip.id);
    p.clips.insert(clip.id, clip);
}

/// A sine track at `hz`, peak `amp` (generators play at 0.7: clip gain).
fn sine(p: &mut Project, name: &str, hz: f64, amp: f32, seconds: f64) -> TrackId {
    let src = p.ids.allocate();
    p.sources.insert(
        src,
        AudioSource {
            id: src,
            name: name.into(),
            spec: SourceSpec::Generated {
                generator: GeneratorSpec::Sine {
                    frequency: hz,
                    seconds,
                    amplitude: amp,
                },
            },
        },
    );
    let id: TrackId = p.ids.allocate();
    let at = p.tracks.len() - 1;
    p.tracks.insert(
        at,
        Track::new(id, TrackKind::Audio, name, TrackColor::palette(1)),
    );
    clip_of(p, id, src, seconds, 20.0 * (amp / 0.7).log10());
    id
}

/// A track playing a file of `channels`.
fn file_track(p: &mut Project, name: &str, path: &std::path::Path, frames: usize) -> TrackId {
    let src = p.ids.allocate();
    p.sources.insert(
        src,
        AudioSource {
            id: src,
            name: name.into(),
            spec: SourceSpec::File {
                path: path.to_path_buf(),
                channels: 1,
                frames: frames as i64,
                sample_rate: SR,
            },
        },
    );
    let id: TrackId = p.ids.allocate();
    let at = p.tracks.len() - 1;
    p.tracks.insert(
        at,
        Track::new(id, TrackKind::Audio, name, TrackColor::palette(2)),
    );
    clip_of(p, id, src, frames as f64 / f64::from(SR), 0.0);
    id
}

fn bus(p: &mut Project, name: &str, kind: TrackKind) -> TrackId {
    let id: TrackId = p.ids.allocate();
    let at = p.tracks.len() - 1;
    let mut t = Track::new(id, kind, name, TrackColor::palette(3));
    t.layout = faderframe_core::ChannelLayout::Stereo;
    p.tracks.insert(at, t);
    id
}

fn device(p: &mut Project, track: TrackId, id: &str, params: &[(u32, f64)]) -> PluginInstanceId {
    let plugin = p.ids.allocate();
    p.track_mut(track).unwrap().inserts.push(PluginSlot {
        id: plugin,
        plugin: PluginRef::builtin(id, if id == builtin::GAIN { "Utility" } else { "EQ" }),
        bypass: false,
        parameters: params
            .iter()
            .map(|&(id, value)| SavedParameter {
                id: ParameterId(id),
                value,
            })
            .collect(),
        state: None,
        sidechain: None,
    });
    plugin
}

fn analyse(s: &mut Session) {
    s.dispatch(Action::Mix(MixOp::Analyse {
        scope: MixScope::Project,
        signal: SignalPoint::Processed,
    }))
    .unwrap();
    s.wait_for_mix();
    assert!(s.mix_results().is_some(), "{:?}", s.mix_status());
    // The final results (the first stage's are shown while the second
    // runs, then replaced).
    assert!(s.mix_results().is_some_and(|r| r.complete));
}

#[test]
fn one_render_measures_tracks_buses_and_the_master_in_context() {
    use faderframe_plugin_host::devices::utility::id as utility;
    let mut p = Project::new("Buses", SR);
    // Two low tones summed on a bus that is turned down 6 dB, one direct.
    let a = sine(&mut p, "Low A", 80.0, 0.3, 8.0);
    let b = sine(&mut p, "Low B", 84.0, 0.3, 8.0);
    let c = sine(&mut p, "Low C", 82.0, 0.3, 8.0);
    let sub = bus(&mut p, "Sub", TrackKind::Bus);
    for t in [a, b] {
        p.track_mut(t).unwrap().output = OutputRouting::Track { track: sub };
    }
    device(&mut p, sub, builtin::GAIN, &[(utility::GAIN, -6.0206)]);
    let mut s = Session::new(p, None, EngineConfig::default()).unwrap();
    analyse(&mut s);
    let r = s.mix_results().unwrap();
    assert_eq!(r.renders, 1, "one render for everything");
    assert!(r.tracks.contains(&sub), "the bus is analysed");
    assert_eq!(
        (r.rendered, r.reused),
        (5, 0),
        "three tracks, the bus, the master"
    );
    let summary = |t: TrackId| {
        r.report
            .summaries
            .iter()
            .find(|x| x.subject == Subject::Track(t))
            .unwrap()
            .clone()
    };
    // Each track as it reaches its destination (the bus's −6 dB is the
    // bus's, not theirs).
    let (sa, sc) = (summary(a), summary(c));
    assert!((sa.rms - sc.rms).abs() < 0.3, "{} {}", sa.rms, sc.rms);
    // The bus carries both, 6 dB down: the sum of two tones a few Hz apart
    // (+3 dB of power) − 6 dB.
    let sb = summary(sub);
    assert!(
        (sb.rms - (sa.rms + 3.0 - 6.0)).abs() < 0.6,
        "{} {}",
        sb.rms,
        sa.rms
    );
    // The bus is never compared with the tracks it sums; the tracks are,
    // with each other and with the direct one.
    let pairs: Vec<(TrackId, TrackId)> = r
        .report
        .findings
        .iter()
        .filter_map(|f| match f.subject {
            Subject::Pair { target, masker } => Some((target, masker)),
            _ => None,
        })
        .collect();
    assert!(!pairs.is_empty());
    assert!(
        pairs.iter().all(|(t, m)| *t != sub && *m != sub),
        "{pairs:?}"
    );
    assert!(
        pairs
            .iter()
            .any(|(t, m)| [a, b].contains(t) && *m == c || [a, b].contains(m) && *t == c)
    );
    // Analysed again unchanged: nothing rendered at all.
    analyse(&mut s);
    let r = s.mix_results().unwrap();
    assert_eq!((r.renders, r.rendered, r.reused), (0, 0, 5));
    // The bus's device changes: the bus and the master are measured again,
    // the tracks reaching it are not (nothing downstream changes a tap).
    let gain = s.project().track(sub).unwrap().inserts[0].id;
    s.dispatch(Action::Edit(Command::SetPluginParameter {
        track: sub,
        plugin: gain,
        parameter: ParameterId(utility::GAIN),
        value: Some(-3.0),
    }))
    .unwrap();
    analyse(&mut s);
    let r = s.mix_results().unwrap();
    assert_eq!((r.renders, r.rendered, r.reused), (1, 2, 3));
}

#[test]
fn the_health_check_names_the_device_and_its_experiment_is_one_step() {
    use faderframe_plugin_host::devices::utility::id as utility;
    let dir = temp("health");
    // A recording with a DC offset.
    let frames = SR as usize * 6;
    let offset: Vec<f32> = (0..frames)
        .map(|i| 0.2 * (i as f32 * 0.05).sin() + 0.02)
        .collect();
    let path = dir.join("offset.wav");
    write_wav(&path, &[offset], SR, WavFormat::Float32, false).unwrap();
    let mut p = Project::new("Health", SR);
    let hot = sine(&mut p, "Hot", 220.0, 0.5, 6.0);
    // +12 dB into an EQ: over full scale between the two devices.
    let loud = device(&mut p, hot, builtin::GAIN, &[(utility::GAIN, 12.0)]);
    let eq = device(&mut p, hot, builtin::EQ, &[]);
    let idle = sine(&mut p, "Idle", 1000.0, 0.2, 6.0);
    // A Utility at rest changes nothing; a bypassed EQ is passed by.
    let rest = device(&mut p, idle, builtin::GAIN, &[]);
    let off = device(&mut p, idle, builtin::EQ, &[]);
    p.track_mut(idle).unwrap().inserts[1].bypass = true;
    let dc = file_track(&mut p, "DC", &path, frames);
    let mut s = Session::new(p, None, EngineConfig::default()).unwrap();
    analyse(&mut s);
    let r = s.mix_results().unwrap();
    assert!(!r.devices.is_empty());
    let health: Vec<_> = r
        .report
        .findings
        .iter()
        .filter(|f| f.category() == Category::Health)
        .cloned()
        .collect();
    let of = |rule: RuleId| health.iter().find(|f| f.rule == rule).cloned();
    let over = of(RuleId::DeviceOverFullScale).unwrap_or_else(|| panic!("{health:#?}"));
    assert_eq!(over.device(), Some(loud));
    assert_eq!(over.tracks, vec![hot]);
    let title = over.title_named(&|t| s.mix_track_name(t), &|d| s.mix_device_name(d));
    assert!(title.contains("Utility"), "{title}");
    // The Utility at rest changes nothing, nor does the EQ without a band
    // (both reported, each with its cost).
    let unchanged: Vec<_> = health
        .iter()
        .filter(|f| f.rule == RuleId::DeviceNoChange)
        .collect();
    assert!(
        unchanged.iter().any(|f| f.device() == Some(eq)),
        "{unchanged:#?}"
    );
    let same = unchanged
        .iter()
        .find(|f| f.device() == Some(rest))
        .unwrap_or_else(|| panic!("{health:#?}"));
    assert!(
        same.evidence
            .iter()
            .any(|e| e.label == faderframe_mix_intelligence::finding::labels::COST),
        "its processing time is measured"
    );
    let bypassed = of(RuleId::DevicesBypassed).unwrap();
    assert!(bypassed.evidence.iter().any(|e| e.device() == Some(off)));
    let offset = of(RuleId::DcOffset).unwrap_or_else(|| panic!("{health:#?}"));
    assert_eq!(offset.tracks, vec![dc]);
    // Gain staging around the EQ the overs reach: auditioned (two
    // Utilities, A/B switching their gains), rejected, then applied as one
    // step and undone.
    let i = over
        .recommendations
        .iter()
        .position(|r| matches!(r.experiment, Experiment::GainStage { .. }))
        .unwrap();
    match over.recommendations[i].experiment {
        Experiment::GainStage {
            device, trim_db, ..
        } => {
            assert_eq!(device, eq);
            assert!(trim_db < 0.0);
        }
        _ => unreachable!(),
    }
    let before = s.project().tracks.clone();
    let steps = s.history_steps().0.len();
    s.dispatch(Action::Mix(MixOp::Audition {
        finding: over.id,
        recommendation: i,
    }))
    .unwrap();
    let names: Vec<String> = s
        .project()
        .track(hot)
        .unwrap()
        .inserts
        .iter()
        .map(|x| x.plugin.id.clone())
        .collect();
    assert_eq!(
        names,
        [builtin::GAIN, builtin::GAIN, builtin::EQ, builtin::GAIN],
        "a trim before the EQ, its level given back after it"
    );
    s.dispatch(Action::Mix(MixOp::Compare(false))).unwrap();
    let trims: Vec<f64> = s.project().track(hot).unwrap().inserts[1..]
        .iter()
        .filter(|x| x.plugin.id == builtin::GAIN)
        .map(|x| {
            x.parameters
                .iter()
                .find(|p| p.id == ParameterId(utility::GAIN))
                .map_or(0.0, |p| p.value)
        })
        .collect();
    assert_eq!(trims, [0.0, 0.0], "the original: both trims at 0 dB");
    s.dispatch(Action::Mix(MixOp::Reject)).unwrap();
    assert_eq!(s.project().tracks, before);
    assert_eq!(s.history_steps().0.len(), steps);
    s.dispatch(Action::Mix(MixOp::Apply {
        finding: over.id,
        recommendation: i,
    }))
    .unwrap();
    let (done, _) = s.history_steps();
    assert_eq!(done.len(), steps + 1);
    assert!(
        done.last()
            .unwrap()
            .starts_with("Mix Intelligence: Gain staging")
    );
    assert_eq!(s.project().track(hot).unwrap().inserts.len(), 4);
    // The finding on the changed track is stale now; the DC one is not.
    s.tick(0.016);
    assert!(s.mix_finding_stale(over.id));
    assert!(!s.mix_finding_stale(offset.id));
    s.dispatch(Action::Undo).unwrap();
    assert_eq!(s.project().tracks, before);
    // The DC filter: one Utility with its filter on.
    let j = offset
        .recommendations
        .iter()
        .position(|r| matches!(r.experiment, Experiment::DcFilter { .. }))
        .unwrap();
    s.dispatch(Action::Mix(MixOp::Apply {
        finding: offset.id,
        recommendation: j,
    }))
    .unwrap();
    let last = s
        .project()
        .track(dc)
        .unwrap()
        .inserts
        .last()
        .unwrap()
        .clone();
    assert_eq!(last.plugin.id, builtin::GAIN);
    assert!(
        last.parameters
            .iter()
            .any(|p| p.id == ParameterId(utility::DC) && p.value == 1.0)
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn the_master_is_compared_with_the_reference_loudness_matched() {
    let dir = temp("reference");
    // The reference: the same tones, 10 dB louder, with much more low end.
    let n = SR as usize * 8;
    let tone = |hz: f64, amp: f64| -> Vec<f32> {
        (0..n)
            .map(|i| (amp * (std::f64::consts::TAU * hz * i as f64 / f64::from(SR)).sin()) as f32)
            .collect()
    };
    let (low, high) = (tone(80.0, 0.6), tone(2000.0, 0.15));
    let record: Vec<f32> = low.iter().zip(&high).map(|(a, b)| a + b).collect();
    let file = dir.join("Record.wav");
    write_wav(
        &file,
        &[record.clone(), record],
        SR,
        WavFormat::Float32,
        false,
    )
    .unwrap();
    let mut p = Project::new("Reference", SR);
    sine(&mut p, "Bass", 80.0, 0.05, 8.0);
    sine(&mut p, "Lead", 2000.0, 0.1, 8.0);
    let mut s = Session::new(p, None, EngineConfig::default()).unwrap();
    s.dispatch(Action::Reference(ReferenceOp::Add(vec![file])))
        .unwrap();
    assert!(s.wait_for_compare(Duration::from_secs(300)));
    let reference = s.references().tracks[0].id;
    analyse(&mut s);
    let c = s.mix_reference().unwrap().clone();
    assert_eq!(c.reference, reference);
    assert!(c.match_db < -5.0, "the record is louder: {}", c.match_db);
    // Analysed again unchanged: nothing rendered or read, the same
    // comparison (the reference's profile is kept too).
    analyse(&mut s);
    let r = s.mix_results().unwrap();
    assert_eq!(r.renders, 0);
    assert_eq!(s.mix_reference(), Some(&c));
    let low = c
        .centres
        .iter()
        .position(|f| (*f - 80.0).abs() < 5.0)
        .unwrap();
    assert!(
        c.difference_db[low].unwrap() < -4.0,
        "less low end than the record, loudness-matched: {:?}",
        c.difference_db[low]
    );
    let findings: Vec<_> = s
        .mix_findings()
        .into_iter()
        .filter(|f| f.category() == Category::Reference)
        .cloned()
        .collect();
    assert!(findings.iter().any(|f| f.rule == RuleId::ReferenceLoudness));
    let spectrum = findings
        .iter()
        .find(|f| f.rule == RuleId::ReferenceSpectrum)
        .unwrap_or_else(|| panic!("{findings:#?}"));
    // Hand-offs: never auditioned or applied here …
    let listen = spectrum
        .recommendations
        .iter()
        .position(|r| matches!(r.experiment, Experiment::CompareReference { .. }))
        .unwrap();
    assert!(s.mix_can_try(spectrum.id, listen).is_err());
    // … but heard: the reference instead of the mix, loudness-matched.
    s.dispatch(Action::Mix(MixOp::Open {
        finding: spectrum.id,
        recommendation: listen,
    }))
    .unwrap();
    assert_eq!(s.current_reference(), Some(reference));
    let status = s.compare_status().unwrap();
    assert!(status.hearing_other);
    let gain = s.listening_gain();
    assert!(gain.other_matched && gain.other_db < -5.0, "{gain:?}");
    assert_eq!(gain.mix_db, 0.0);
    // EQ Match: the master gets an EQ (one step) whose editor opens.
    let eq = spectrum
        .recommendations
        .iter()
        .position(|r| matches!(r.experiment, Experiment::ReferenceEqMatch { .. }))
        .unwrap();
    let master = s.project().master_id().unwrap();
    s.dispatch(Action::Mix(MixOp::Open {
        finding: spectrum.id,
        recommendation: eq,
    }))
    .unwrap();
    assert!(
        s.project()
            .track(master)
            .unwrap()
            .inserts
            .iter()
            .any(|x| x.plugin.id == builtin::EQ)
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn the_candidate_is_matched_by_the_one_listening_gain() {
    let mut p = Project::new("Gain", SR);
    sine(&mut p, "Low A", 80.0, 0.3, 8.0);
    sine(&mut p, "Low B", 84.0, 0.3, 8.0);
    let mut s = Session::new(p, None, EngineConfig::default()).unwrap();
    analyse(&mut s);
    let f = s
        .mix_findings()
        .into_iter()
        .find(|f| f.rule == RuleId::SpectralMasking)
        .unwrap()
        .clone();
    let i = f
        .recommendations
        .iter()
        .position(|r| matches!(r.experiment, Experiment::EqCut { .. }))
        .unwrap();
    s.dispatch(Action::Mix(MixOp::Audition {
        finding: f.id,
        recommendation: i,
    }))
    .unwrap();
    s.wait_for_mix_measure();
    let a = s.mix_audition().unwrap();
    let m = a.measured.clone().unwrap().unwrap();
    // The fairness gain is the mix side of the listening gain, the real
    // change kept apart.
    let gain = s.listening_gain();
    assert!(
        (f64::from(gain.mix_db) - a.trim_db).abs() < 1e-4,
        "{gain:?} {}",
        a.trim_db
    );
    assert!((a.trim_db + m.change()).abs() < 1e-6);
    assert!(m.change() < 0.0, "the cut makes it quieter: {m:?}");
    let shared = s.compare_link().shared();
    assert!((shared.mix_gain() - db_to_gain(gain.mix_db)).abs() < 1e-6);
    // The original: no fairness gain.
    s.dispatch(Action::Mix(MixOp::Compare(false))).unwrap();
    assert_eq!(s.listening_gain().mix_db, 0.0);
    s.dispatch(Action::Mix(MixOp::Compare(true))).unwrap();
    s.dispatch(Action::Mix(MixOp::Reject)).unwrap();
    assert_eq!(s.listening_gain().mix_db, 0.0);
    assert_eq!(s.compare_link().shared().mix_gain(), 1.0);
}
