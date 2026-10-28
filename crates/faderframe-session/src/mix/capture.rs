//! The processed analysis in one pass: one offline render of the whole mix
//! with a tap on every track, bus and return analysed (where it reaches its
//! destination) and a probe after every device; the master is the render's
//! output. See `faderframe_engine::capture` for how the taps are timed
//! (the graph's own delay compensation) and why they cost nothing in other
//! renders.
//!
//! Profiles found in the cache (their track's fingerprint unchanged,
//! [`super::fingerprint`]) are not measured again — their taps are not
//! even built; the render runs only when something is missing (a profile
//! or the health check of this very project).

use super::fingerprint::{Fingerprints, fingerprints, related};
use super::{CacheKey, Output, Planned, Plays, Progress, STAGE_REFERENCE, STAGE_RULES, Shared};
use crate::render::{CaptureReport, render_captured};
use faderframe_core::{PluginInstanceId, TrackId};
use faderframe_engine::capture::CapturePlan;
use faderframe_mix_intelligence::health::{self, DeviceRecord, HealthInput, HealthSubject};
use faderframe_mix_intelligence::profile::ProfileBuilder;
use faderframe_mix_intelligence::reference::{ReferenceComparison, ReferenceInput};
use faderframe_mix_intelligence::rules::{DiagnoseInput, TrackInput};
use faderframe_mix_intelligence::{Fnv, SignalPoint, Subject, TrackProfile};

use faderframe_project::Project;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Instant;

/// The reference a processed analysis compares the master with.
#[derive(Clone, Debug)]
pub(crate) struct ReferencePlan {
    pub id: faderframe_core::ReferenceId,
    /// The media file (absolute).
    pub path: PathBuf,
    /// The file frames that play.
    pub from: i64,
    pub to: i64,
    /// Following the timeline: the file frame of timeline sample `p` is
    /// `p − anchor` (else it loops on its own: its region as a whole).
    pub anchor: Option<i64>,
}

/// The key of a health check: the project and the range it was made for.
pub(super) fn health_key(fp: &Fingerprints, shared: &Shared) -> u64 {
    let mut h = Fnv::new();
    h.write_u64(fp.project);
    h.write_u64(shared.start as u64);
    h.write_u64(shared.frames as u64);
    h.write_u64(u64::from(shared.rate));
    h.write_u64(u64::from(faderframe_mix_intelligence::ALGORITHM_VERSION));
    h.finish()
}

/// What each device's probe measured, as the rules read it.
fn device_records(project: &Project, report: &CaptureReport) -> Vec<DeviceRecord> {
    report
        .probes
        .iter()
        .map(|(info, s)| {
            let inserted = |p: PluginInstanceId| {
                project
                    .track(info.track)
                    .is_some_and(|t| t.inserts.iter().any(|x| x.id == p))
            };
            DeviceRecord {
                position: info.position,
                next: info.next,
                bypassed: info.bypassed,
                instrument: report.instruments.contains(&info.plugin),
                editable: inserted(info.plugin),
                next_editable: info.next.is_some_and(inserted),
                frames: s.frames,
                in_channels: info.in_channels,
                out_channels: info.out_channels,
                in_energy: s.in_energy,
                out_energy: s.out_energy,
                diff_energy: s.diff_energy,
                in_peak: f64::from(s.in_peak),
                out_peak: f64::from(s.out_peak),
                out_above: s.out_above,
                in_above: s.in_above,
                in_flat: s.in_flat,
                out_flat: s.out_flat,
                in_invalid: s.in_invalid,
                out_invalid: s.out_invalid,
                cost: report.costs.get(&info.plugin).copied(),
                ..DeviceRecord::new(info.track, info.plugin)
            }
        })
        .collect()
}

/// The reference as heard (mapped onto the master's two channels) over the
/// analysed range (following the timeline) or its region (looping),
/// measured like the master.
fn profile_reference(
    plan: &ReferencePlan,
    shared: &Shared,
    cancel: &std::sync::atomic::AtomicBool,
) -> Result<(TrackProfile, bool), String> {
    let file = faderframe_audio_files::wavstream::WavFile::open(&plan.path)
        .map_err(|e| format!("{}: {e}", plan.path.display()))?;
    if file.sample_rate() != shared.rate {
        return Err(format!(
            "the reference is at {} Hz, the analysis at {} Hz",
            file.sample_rate(),
            shared.rate
        ));
    }
    let len = file.frames() as i64;
    let (from, to) = (plan.from.clamp(0, len), plan.to.clamp(0, len));
    // Where to read: the file frames beside the range, or the region.
    let (first, frames, aligned) = match plan.anchor {
        Some(anchor) => (shared.start - anchor, shared.frames as i64, true),
        None => (from, (to - from).max(0), false),
    };
    let mut builder =
        ProfileBuilder::new(shared.rate, 2, &shared.config, &[]).map_err(|e| e.to_string())?;
    const PIECE: i64 = 65_536;
    let channels = file.channels().max(1);
    let mut raw = vec![vec![0.0f32; PIECE as usize]; channels];
    let mut scratch = Vec::new();
    let mut at = 0i64;
    while at < frames {
        if cancel.load(Ordering::Relaxed) {
            return Err("cancelled".into());
        }
        let n = (frames - at).min(PIECE) as usize;
        let start = first + at;
        // The region only: silence outside it.
        let (a, b) = (start.max(from), (start + n as i64).min(to));
        for c in &mut raw {
            c[..n].fill(0.0);
        }
        if b > a {
            let off = (a - start) as usize;
            let k = (b - a) as usize;
            let mut slices: Vec<&mut [f32]> =
                raw.iter_mut().map(|c| &mut c[off..off + k]).collect();
            file.read(a as u64, &mut slices, &mut scratch)
                .map_err(|e| e.to_string())?;
        }
        let parts: Vec<Vec<f32>> = raw.iter().map(|c| c[..n].to_vec()).collect();
        let heard = crate::compare::as_heard(parts, 2);
        let views: Vec<&[f32]> = heard.iter().map(Vec::as_slice).collect();
        builder.push(&views).map_err(|e| e.to_string())?;
        at += n as i64;
    }
    Ok((builder.finish(), aligned))
}

/// A measured reference: its profile, whether it follows the timeline, the
/// cache key of a profile made now (`None`: reused or not to be kept).
type MeasuredReference = (Arc<TrackProfile>, bool, Option<CacheKey>);

/// A reference measured on a thread of its own, wanted after the render.
type ReferenceJob<'a> = Box<dyn FnOnce() -> Option<Result<MeasuredReference, String>> + 'a>;

/// The cache key of a reference's profile: its file, region and placing
/// (media files are never changed in place), the range and settings.
fn reference_key(plan: &ReferencePlan, shared: &Shared) -> CacheKey {
    let mut h = Fnv::new();
    h.write(plan.path.to_string_lossy().as_bytes());
    h.write_u64(plan.from as u64);
    h.write_u64(plan.to as u64);
    h.write_u64(plan.anchor.map_or(u64::MAX, |a| a as u64));
    shared.key(plan.id.raw() | (1 << 62), h.finish(), &[])
}

/// The processed analysis (see the module docs). The reference (when there
/// is one) is read and measured on a thread of its own while the mix
/// renders: it does not depend on the mix.
pub(super) fn processed(
    planned: &[Planned],
    base: &Project,
    shared: Shared,
    progress: &Progress,
    sources: &faderframe_engine::SourceMap,
) -> Result<Output, String> {
    std::thread::scope(|scope| {
        let (shared, cancel) = (&shared, &progress.render.cancel);
        let plan = shared.reference.as_ref();
        let key = plan.map(|p| reference_key(p, shared));
        let cached = key
            .and_then(|k| shared.cache.iter().find(|e| e.0 == k))
            .map(|e| Arc::clone(&e.1));
        let job = match (plan, &cached) {
            (Some(p), None) => Some(
                std::thread::Builder::new()
                    .name("faderframe-mix-reference".into())
                    .spawn_scoped(scope, move || profile_reference(p, shared, cancel)),
            ),
            _ => None,
        };
        let reference: ReferenceJob<'_> = Box::new(move || {
            let aligned = plan.is_some_and(|p| p.anchor.is_some());
            if let Some(found) = cached {
                return Some(Ok((found, aligned, None)));
            }
            job.map(|j| match j {
                Ok(h) => h
                    .join()
                    .unwrap_or_else(|_| Err("its measurement failed".into()))
                    .map(|(p, aligned)| (Arc::new(p), aligned, key)),
                Err(e) => Err(e.to_string()),
            })
        });
        measure_mix(planned, base, shared, progress, sources, reference)
    })
}

fn measure_mix(
    planned: &[Planned],
    base: &Project,
    shared: &Shared,
    progress: &Progress,
    sources: &faderframe_engine::SourceMap,
    reference_job: ReferenceJob<'_>,
) -> Result<Output, String> {
    let fps = fingerprints(base);
    let key = |p: &Planned| -> Option<CacheKey> {
        let fps = fps.as_ref()?;
        let (track, project) = match p.plays {
            Plays::Master => (u64::MAX, fps.project),
            _ => (p.track.0, fps.of(p.track)),
        };
        Some(shared.key(track, project, &p.spans))
    };
    let keys: Vec<Option<CacheKey>> = planned.iter().map(key).collect();
    let mut profiles: Vec<Option<Arc<TrackProfile>>> = keys
        .iter()
        .map(|k| {
            k.and_then(|k| shared.cache.iter().find(|e| e.0 == k))
                .map(|e| Arc::clone(&e.1))
        })
        .collect();
    let reused: Vec<CacheKey> = keys
        .iter()
        .zip(&profiles)
        .filter_map(|(k, p)| p.as_ref().and(*k))
        .collect();
    let hkey = fps.as_ref().map(|f| health_key(f, shared));
    let cached_health = shared
        .health
        .as_ref()
        .filter(|(k, _)| Some(*k) == hkey)
        .map(|(_, d)| Arc::clone(d));
    let missing: Vec<usize> = (0..planned.len())
        .filter(|&i| profiles[i].is_none())
        .collect();
    let mut renders = 0;
    let mut latency = None;
    let mut notes = Vec::new();
    let mut phases: Vec<(&'static str, f64)> = Vec::new();
    let t0 = Instant::now();
    progress
        .measuring
        .store(missing.len() as u32, Ordering::Relaxed);
    progress.threads.store(
        super::measure::threads(missing.len()) as u32,
        Ordering::Relaxed,
    );
    let devices = match cached_health {
        Some(d) if missing.is_empty() => d,
        _ => {
            // One render: taps for what is missing, every device probed;
            // the profiles built beside it on a few threads.
            let tapped: Vec<TrackId> = missing
                .iter()
                .filter(|&&i| matches!(planned[i].plays, Plays::Tapped))
                .map(|&i| planned[i].track)
                .collect();
            let mut builders: Vec<ProfileBuilder> = Vec::with_capacity(missing.len());
            let mut index: HashMap<Option<TrackId>, usize> = HashMap::new();
            for &i in &missing {
                let p = &planned[i];
                let (k, channels) = match p.plays {
                    Plays::Master => (None, 2),
                    // A tap is as wide as where it goes (a bed: its first
                    // two channels are measured).
                    _ => (
                        Some(p.track),
                        base.track(p.track)
                            .map_or(2, |t| base.destination_layout(t).channel_count()),
                    ),
                };
                let b = ProfileBuilder::new(shared.rate, channels.max(1), &shared.config, &p.spans)
                    .map_err(|e| e.to_string())?;
                index.insert(k, builders.len());
                builders.push(b);
            }
            progress.track.store(0, Ordering::Relaxed);
            let (report, measured) = super::measure::in_parallel(builders, |feed| {
                render_captured(
                    base,
                    shared.rate,
                    sources,
                    (shared.start, shared.frames),
                    &progress.render,
                    2,
                    CapturePlan {
                        tracks: tapped,
                        devices: true,
                        block: 0,
                    },
                    &mut |who, piece| {
                        if let Some(&i) = index.get(&who) {
                            feed(i, piece);
                        }
                    },
                )
            });
            let report = report.map_err(|e| e.to_string())?;
            renders = 1;
            latency = Some(report.latency);
            if report.missed > 0 {
                notes.push(format!(
                    "{} frames of the tapped signals were lost (measured as silence)",
                    report.missed
                ));
            }
            for (&i, m) in missing.iter().zip(measured) {
                profiles[i] = Some(Arc::new(m?));
            }
            Arc::new(device_records(base, &report))
        }
    };
    phases.push(("render and measure", t0.elapsed().as_secs_f64()));
    let mut made: Vec<(CacheKey, Arc<TrackProfile>)> = missing
        .iter()
        .filter_map(|&i| Some((keys[i]?, Arc::clone(profiles[i].as_ref()?))))
        .collect();
    let rendered = missing.len();
    progress.diagnosing.store(true, Ordering::Relaxed);
    let profiles: Vec<Arc<TrackProfile>> = profiles
        .into_iter()
        .map(|p| p.ok_or_else(|| "a signal was not captured".to_string()))
        .collect::<Result<_, _>>()?;
    let cancel = &progress.render.cancel;
    // The reference beside the master.
    let t0 = Instant::now();
    let master = planned
        .iter()
        .zip(&profiles)
        .find(|(p, _)| matches!(p.plays, Plays::Master));
    let mut reference: Option<ReferenceComparison> = None;
    let mut reference_findings = Vec::new();
    let measured_reference = {
        progress.stage.store(STAGE_REFERENCE, Ordering::Relaxed);
        reference_job()
    };
    if let (Some(plan), Some((mp, mprof)), Some(measured)) =
        (&shared.reference, master, measured_reference)
    {
        match measured {
            Ok((rp, aligned, fresh)) => {
                if let Some(k) = fresh {
                    made.push((k, Arc::clone(&rp)));
                }
                match faderframe_mix_intelligence::reference::compare(&ReferenceInput {
                    reference: plan.id,
                    profile: &rp,
                    mix: mprof,
                    master: Some(mp.track),
                    aligned,
                    sections: &shared.sections,
                    rules: &shared.rules.reference,
                    signal: SignalPoint::Processed,
                }) {
                    Ok((c, f)) => {
                        reference = Some(c);
                        reference_findings = f;
                    }
                    Err(e) => notes.push(format!("the reference: {e}")),
                }
            }
            Err(e) if cancel.load(Ordering::Relaxed) => return Err(e),
            Err(e) => notes.push(format!("the reference: {e}")),
        }
        phases.push(("reference", t0.elapsed().as_secs_f64()));
    }
    progress.stage.store(STAGE_RULES, Ordering::Relaxed);
    let t0 = Instant::now();
    // The rules.
    let mut tracks = Vec::new();
    let mut master_input = None;
    for (p, prof) in planned.iter().zip(&profiles) {
        match p.plays {
            Plays::Master => master_input = Some((Some(p.track), prof.as_ref())),
            _ => tracks.push(TrackInput {
                track: p.track,
                role: p.role,
                profile: prof,
                compare: p.compare,
                focus: p.focus,
            }),
        }
    }
    let ids: Vec<TrackId> = tracks.iter().map(|t| t.track).collect();
    let input = DiagnoseInput {
        tracks,
        master: master_input,
        signal: SignalPoint::Processed,
        sections: shared.sections.clone(),
        rules: &shared.rules,
        cancel: Some(cancel),
        related: related(base, &ids),
    };
    let mut report = faderframe_mix_intelligence::diagnose(&input).map_err(|e| e.to_string())?;
    let focus: Vec<TrackId> = planned
        .iter()
        .filter(|p| p.focus)
        .map(|p| p.track)
        .collect();
    let subjects: Vec<HealthSubject<'_>> = planned
        .iter()
        .zip(&profiles)
        .filter(|(p, _)| p.focus)
        .map(|(p, prof)| HealthSubject {
            subject: match p.plays {
                Plays::Master => Subject::Master(Some(p.track)),
                _ => Subject::Track(p.track),
            },
            profile: prof.as_ref(),
        })
        .collect();
    let in_focus: Vec<DeviceRecord> = devices
        .iter()
        .filter(|d| focus.contains(&d.track))
        .cloned()
        .collect();
    report.merge(health::diagnose(&HealthInput {
        subjects,
        devices: &in_focus,
        signal: SignalPoint::Processed,
        rules: &shared.rules.health,
    }));
    report.merge(reference_findings);
    phases.push(("rules", t0.elapsed().as_secs_f64()));
    // The first stage's findings are shown while the second runs.
    if let Ok(mut first) = progress.first.lock() {
        *first = Some(super::Partial {
            report: report.clone(),
            rendered,
            reused: reused.len(),
            fingerprint: fps.as_ref().map(|f| f.project),
            fingerprints: fps.clone(),
            devices: Arc::clone(&devices),
            reference: reference.clone(),
            renders,
            latency,
        });
    }
    // Sources beside the processed signals, pairs for phase and timing
    // (their excerpts tapped in one render each).
    let t0 = Instant::now();
    let extra = super::extra::run(
        &shared.stage(planned, base, fps.as_ref(), sources, progress),
        &input,
        &mut report,
    )?;
    phases.push(("sources and pairs", t0.elapsed().as_secs_f64()));
    Ok(Output {
        report,
        profiles: made,
        reused,
        rendered,
        fingerprint: fps.as_ref().map(|f| f.project),
        fingerprints: fps,
        devices,
        health_key: hkey,
        reference,
        renders,
        latency,
        notes,
        phases,
        extra,
    })
}
