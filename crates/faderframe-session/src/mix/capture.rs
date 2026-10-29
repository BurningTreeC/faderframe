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
//! or the health check of this very project). After a change, the tracks
//! it reached get a first look before that render: a render of only what
//! reaches them ([`pruned`]) in segments side by side ([`render_look`]),
//! shown with their pairs until the whole render measures them exactly.
//! When only tracks are missing (the master and the health check are
//! there), one render of what they need is all ([`pruned`] taps are the
//! whole mix's, bit for bit).

use super::fingerprint::{Fingerprints, fingerprints, related};
use super::held::{HOLD_BYTES, HeldAudio};
use super::{
    CacheKey, Output, Planned, Plays, Progress, STAGE_LOOK, STAGE_REFERENCE, STAGE_RULES, Shared,
};
use crate::render::{CaptureReport, render_captured, render_captured_on};
use faderframe_core::{PluginInstanceId, TrackId};
use faderframe_engine::capture::CapturePlan;
use faderframe_mix_intelligence::health::{self, DeviceRecord, HealthInput, HealthSubject};
use faderframe_mix_intelligence::profile::ProfileBuilder;
use faderframe_mix_intelligence::reference::{ReferenceComparison, ReferenceInput};
use faderframe_mix_intelligence::rules::{DiagnoseInput, TrackInput};
use faderframe_mix_intelligence::{Fnv, Report, SignalPoint, Subject, TrackProfile};

use faderframe_project::{InputRouting, Project};
use std::collections::{BTreeSet, HashMap};
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

/// `base` with only what the taps of `keep` need still sounding: every
/// other track's clips, freeze, devices, sends, cue sends, modulators and
/// automation taken out. Buses, returns and the master keep their strips
/// (routing and layouts stay as they are), so a render of it costs what
/// the kept tracks cost. `keep` must be closed upstream (each kept track's
/// [`super::fingerprint::upstream`]): then nothing reaching a kept track is
/// touched, and its tap is the same as in the whole mix — taps are aligned
/// as heard outputs by the graph, whatever else it delays.
pub(super) fn pruned(base: &Project, keep: &BTreeSet<TrackId>) -> Project {
    let mut p = base.clone();
    let mut dropped = Vec::new();
    for t in &mut p.tracks {
        if keep.contains(&t.id) {
            continue;
        }
        dropped.append(&mut t.clips);
        t.freeze = None;
        t.instrument = None;
        t.preamp = None;
        t.inserts.clear();
        t.containers.clear();
        t.modulators.clear();
        t.sends.clear();
        t.cue_sends.clear();
        t.automation = Default::default();
        if matches!(t.input, InputRouting::Plugin { .. }) {
            t.input = InputRouting::None;
        }
    }
    for c in dropped {
        p.clips.remove(&c);
    }
    p
}

/// The tracks a render tapping `taps` needs ([`pruned`]).
pub(super) fn needed(base: &Project, taps: &[TrackId]) -> BTreeSet<TrackId> {
    let mut keep = BTreeSet::new();
    for &t in taps {
        keep.extend(super::fingerprint::upstream(base, t));
    }
    keep
}

/// What rendering `tracks` costs, roughly: a track that sounds, and each
/// of its devices.
pub(super) fn weight<'a>(tracks: impl Iterator<Item = &'a faderframe_project::Track>) -> usize {
    tracks
        .map(|t| {
            let sounds = !t.clips.is_empty() || t.freeze.is_some();
            usize::from(sounds) + t.slots().len()
        })
        .sum()
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

/// Run-up rendered before each segment of a first look (seconds).
const LOOK_RUN_UP: f64 = 2.0;
/// The shortest segment of a first look (seconds).
const LOOK_SEGMENT: f64 = 2.0;

/// A first look at `taps` ([`measure_mix`]): `project` rendered in segments
/// side by side — each from [`LOOK_RUN_UP`] before it (within the range),
/// with an engine of its own on a thread of its own — so a chain one thread
/// renders slowly is measured in a fraction of the time. Where a device
/// remembers longer than the run-up (a long reverb, a free-running LFO) its
/// samples are not the whole render's: a first look is shown until the
/// whole render replaces it and is never kept. A project holding devices
/// other than the built-in ones renders in one piece (a plugin made once a
/// segment could cost more than it saves). Each tap's channels over the
/// range, in order.
fn render_look(
    project: &Project,
    taps: &[TrackId],
    shared: &Shared,
    progress: &Progress,
    sources: &faderframe_engine::SourceMap,
) -> Result<HashMap<TrackId, Vec<Vec<f32>>>, String> {
    let fr = f64::from(shared.rate.max(1));
    let builtin = project.tracks.iter().all(|t| {
        t.slots()
            .iter()
            .all(|s| s.plugin.format == faderframe_project::PluginFormat::Builtin)
    });
    let cores = std::thread::available_parallelism().map_or(1, |n| n.get());
    let most = ((shared.frames as f64 / (LOOK_SEGMENT * fr)) as usize).max(1);
    let n = if builtin {
        cores.saturating_sub(1).clamp(1, most)
    } else {
        1
    };
    let run_up = (LOOK_RUN_UP * fr) as usize;
    let len = shared.frames.div_ceil(n);
    let segments: Vec<(usize, usize)> = (0..n)
        .map(|k| (k * len, ((k + 1) * len).min(shared.frames)))
        .filter(|(a, b)| b > a)
        .collect();
    let total: usize = segments.iter().map(|&(a, b)| b - a + run_up.min(a)).sum();
    progress
        .render
        .total
        .fetch_add(total as u64, Ordering::Relaxed);
    type Piece = HashMap<TrackId, Vec<Vec<f32>>>;
    // One piece: every core for it.
    let workers = if segments.len() == 1 {
        faderframe_realtime::default_worker_count()
    } else {
        0
    };
    let pieces: Vec<Result<Piece, String>> = std::thread::scope(|scope| {
        let handles: Vec<_> = segments
            .iter()
            .enumerate()
            .map(|(k, &(a, b))| {
                std::thread::Builder::new()
                    .name(format!("faderframe-mix-look-{k}"))
                    .spawn_scoped(scope, move || -> Result<Piece, String> {
                        // Beside the other segments: streams of its own.
                        let sources = super::own_sources(project, sources);
                        let lead = run_up.min(a);
                        let mut out: Piece = HashMap::new();
                        let mut skip: HashMap<TrackId, usize> = HashMap::new();
                        render_captured_on(
                            project,
                            shared.rate,
                            &sources,
                            (shared.start + (a - lead) as i64, b - a + lead),
                            &progress.render,
                            2,
                            CapturePlan {
                                tracks: taps.to_vec(),
                                devices: false,
                                block: 0,
                            },
                            workers,
                            &mut |who, piece| {
                                let Some(t) = who else {
                                    return;
                                };
                                let left = skip.entry(t).or_insert(lead);
                                let m = piece.first().map_or(0, |c| c.len());
                                let from = (*left).min(m);
                                *left -= from;
                                let dst = out.entry(t).or_insert_with(|| {
                                    (0..piece.len())
                                        .map(|_| Vec::with_capacity(b - a))
                                        .collect()
                                });
                                for (o, c) in dst.iter_mut().zip(piece) {
                                    o.extend_from_slice(&c[from..]);
                                }
                            },
                        )
                        .map_err(|e| e.to_string())?;
                        Ok(out)
                    })
                    .map_err(|e| e.to_string())
            })
            .collect();
        handles
            .into_iter()
            .map(|h| {
                h.and_then(|h| {
                    h.join()
                        .unwrap_or_else(|_| Err("a segment's render failed".into()))
                })
            })
            .collect()
    });
    // The segments joined in order (a tap a segment missed: silence).
    let mut joined: HashMap<TrackId, Vec<Vec<f32>>> = HashMap::new();
    for (piece, &(a, b)) in pieces.into_iter().zip(&segments) {
        let mut piece = piece?;
        for &t in taps {
            let width = project
                .track(t)
                .map_or(2, |x| project.destination_layout(x).channel_count().max(1));
            let mut chans = piece.remove(&t).unwrap_or_else(|| vec![Vec::new(); width]);
            let dst = joined.entry(t).or_insert_with(|| {
                (0..chans.len())
                    .map(|_| Vec::with_capacity(shared.frames))
                    .collect()
            });
            for (o, c) in dst.iter_mut().zip(&mut chans) {
                c.resize(b - a, 0.0);
                o.extend_from_slice(c);
            }
        }
    }
    Ok(joined)
}

/// What a render measured: each signal's profile and held audio, by its
/// index in the plan.
type Rendered = Vec<(usize, Result<super::measure::MeasuredSignal, String>)>;

/// One capture render of `project` measuring `which` (indices into
/// `planned`; the master is the render's output), probing every device with
/// `devices`. Tapped signals are held as well while `room` allows.
#[allow(clippy::too_many_arguments)]
fn render_and_measure(
    project: &Project,
    planned: &[Planned],
    which: &[usize],
    devices: bool,
    room: &mut usize,
    shared: &Shared,
    progress: &Progress,
    sources: &faderframe_engine::SourceMap,
) -> Result<(CaptureReport, Rendered), String> {
    let tapped: Vec<TrackId> = which
        .iter()
        .filter(|&&i| matches!(planned[i].plays, Plays::Tapped))
        .map(|&i| planned[i].track)
        .collect();
    let mut builders = Vec::with_capacity(which.len());
    let mut index: HashMap<Option<TrackId>, usize> = HashMap::new();
    for &i in which {
        let p = &planned[i];
        let (k, channels) = match p.plays {
            Plays::Master => (None, 2),
            // A tap is as wide as where it goes (a bed: its first two
            // channels are measured).
            _ => (
                Some(p.track),
                project
                    .track(p.track)
                    .map_or(2, |t| project.destination_layout(t).channel_count()),
            ),
        };
        let b = ProfileBuilder::new(shared.rate, channels.max(1), &shared.config, &p.spans)
            .map_err(|e| e.to_string())?;
        // The pairs read their excerpts from held signals (never the
        // master's: it is in no pair).
        let bytes = HeldAudio::bytes_for(channels, shared.frames);
        let hold = (k.is_some() && bytes <= *room).then(|| {
            *room -= bytes;
            HeldAudio::new(channels, shared.frames)
        });
        index.insert(k, builders.len());
        builders.push((b, hold));
    }
    progress.track.store(0, Ordering::Relaxed);
    let (report, measured) = super::measure::in_parallel(builders, |feed| {
        render_captured(
            project,
            shared.rate,
            sources,
            (shared.start, shared.frames),
            &progress.render,
            2,
            CapturePlan {
                tracks: tapped,
                devices,
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
    Ok((report, which.iter().copied().zip(measured).collect()))
}

/// The rules over the signals that have a profile, the health check of the
/// signals and devices in focus.
fn diagnose_signals<'a>(
    planned: &'a [Planned],
    profiles: &'a [Option<Arc<TrackProfile>>],
    base: &Project,
    shared: &'a Shared,
    devices: &[DeviceRecord],
    cancel: &'a std::sync::atomic::AtomicBool,
) -> Result<(DiagnoseInput<'a>, Report), String> {
    let mut tracks = Vec::new();
    let mut master_input = None;
    for (p, prof) in planned.iter().zip(profiles) {
        let Some(prof) = prof else { continue };
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
        .zip(profiles)
        .filter(|(p, _)| p.focus)
        .filter_map(|(p, prof)| {
            Some(HealthSubject {
                subject: match p.plays {
                    Plays::Master => Subject::Master(Some(p.track)),
                    _ => Subject::Track(p.track),
                },
                profile: prof.as_deref()?,
            })
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
    Ok((input, report))
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
        .iter()
        .find(|(k, _)| Some(*k) == hkey)
        .map(|(_, d)| Arc::clone(d));
    let missing: Vec<usize> = (0..planned.len())
        .filter(|&i| profiles[i].is_none())
        .collect();
    let cancel = &progress.render.cancel;
    let mut renders = 0;
    let mut latency = None;
    let mut notes = Vec::new();
    let mut phases: Vec<(&'static str, f64)> = Vec::new();
    // Signals held for the pairs' excerpts: from earlier renders (the
    // signal unchanged) or held now.
    let mut held: HashMap<TrackId, Arc<HeldAudio>> = HashMap::new();
    let mut held_used = Vec::new();
    for (p, k) in planned.iter().zip(&keys) {
        if let (Plays::Tapped, Some(k)) = (p.plays, k)
            && let Some(e) = shared.held.iter().find(|e| e.0 == *k)
        {
            held.insert(p.track, Arc::clone(&e.1));
            held_used.push(*k);
        }
    }
    let mut held_made: Vec<(CacheKey, Arc<HeldAudio>)> = Vec::new();
    let mut room = HOLD_BYTES;
    let mut take = |rendered: Rendered,
                    profiles: &mut Vec<Option<Arc<TrackProfile>>>,
                    held: &mut HashMap<TrackId, Arc<HeldAudio>>|
     -> Result<(), String> {
        for (i, m) in rendered {
            let (profile, audio) = m?;
            profiles[i] = Some(Arc::new(profile));
            if let Some(a) = audio {
                let a = Arc::new(a);
                if let Some(k) = keys[i] {
                    held_made.push((k, Arc::clone(&a)));
                }
                held.insert(planned[i].track, a);
            }
        }
        Ok(())
    };
    // The whole mix is rendered for the master and the health check; the
    // tracks a change reached are measured first, alone with what reaches
    // them ([`pruned`]), when that renders much less — their findings are
    // shown while the whole mix renders.
    let master_missing = missing
        .iter()
        .any(|&i| matches!(planned[i].plays, Plays::Master));
    let whole = master_missing || cached_health.is_none();
    let first: Vec<usize> = missing
        .iter()
        .copied()
        .filter(|&i| matches!(planned[i].plays, Plays::Tapped))
        .filter(|&i| {
            !whole
                || base.track(planned[i].track).is_some_and(|t| {
                    matches!(
                        t.kind,
                        faderframe_project::TrackKind::Audio
                            | faderframe_project::TrackKind::Instrument
                    )
                })
        })
        .collect();
    let first_tracks: Vec<TrackId> = first.iter().map(|&i| planned[i].track).collect();
    let keep = needed(base, &first_tracks);
    let alone = !first.is_empty()
        && (!whole
            || weight(base.tracks.iter().filter(|t| keep.contains(&t.id))) * 10
                <= weight(base.tracks.iter()) * 6);
    progress
        .measuring
        .store(missing.len() as u32, Ordering::Relaxed);
    progress.threads.store(
        super::measure::threads(missing.len()) as u32,
        Ordering::Relaxed,
    );
    // The extra stage run on the changed tracks' findings (its measurements
    // reused by the final run).
    let mut earlier: Option<super::extra::ExtraOutcome> = None;
    let mut provisional: Vec<TrackId> = Vec::new();
    let width = |i: usize| {
        base.track(planned[i].track)
            .map_or(2, |t| base.destination_layout(t).channel_count().max(1))
    };
    let look_bytes: usize = first.iter().map(|&i| 4 * width(i) * shared.frames).sum();
    if alone && whole && look_bytes <= HOLD_BYTES {
        // A first look at the changed tracks (rendered in segments side
        // by side), shown while the whole mix renders: the master and
        // buses as the last analysis measured them, the devices as last
        // checked. The whole render measures the changed tracks again,
        // exactly; nothing of the first look is kept.
        let t0 = Instant::now();
        progress
            .measuring
            .store(first.len() as u32, Ordering::Relaxed);
        progress.stage.store(STAGE_LOOK, Ordering::Relaxed);
        let project = pruned(base, &keep);
        let audio = render_look(&project, &first_tracks, shared, progress, sources)?;
        renders += 1;
        let looks = faderframe_mix_intelligence::map_parallel(&first, |&i| {
            let p = &planned[i];
            let chans = audio
                .get(&p.track)
                .ok_or_else(|| format!("{}: not rendered", p.name))?;
            let mut b = ProfileBuilder::new(shared.rate, chans.len(), &shared.config, &p.spans)
                .map_err(|e| e.to_string())?;
            let views: Vec<&[f32]> = chans.iter().map(Vec::as_slice).collect();
            b.push(&views).map_err(|e| e.to_string())?;
            let mut h = HeldAudio::new(chans.len(), shared.frames);
            h.push(&views);
            Ok::<_, String>((Arc::new(b.finish()), Arc::new(h.finish())))
        });
        drop(audio);
        let mut shown = profiles.clone();
        let mut look_held = held.clone();
        for (&i, r) in first.iter().zip(looks) {
            let (profile, audio) = r?;
            shown[i] = Some(profile);
            look_held.insert(planned[i].track, audio);
        }
        phases.push((
            "a first look at the changed tracks",
            t0.elapsed().as_secs_f64(),
        ));
        for (i, p) in planned.iter().enumerate() {
            if shown[i].is_some() {
                continue;
            }
            let track = match p.plays {
                Plays::Master => u64::MAX,
                _ => p.track.0,
            };
            shown[i] = shared
                .previous
                .iter()
                .find(|e| e.0 == track)
                .map(|e| Arc::clone(&e.1));
            provisional.push(p.track);
        }
        let devices = shared
            .health
            .last()
            .map(|(_, d)| Arc::clone(d))
            .unwrap_or_default();
        let (input, mut report) =
            diagnose_signals(planned, &shown, base, shared, &devices, cancel)?;
        let publish = |report: &Report, pairs: bool| {
            if let Ok(mut first) = progress.first.lock() {
                *first = Some(super::Partial {
                    report: report.clone(),
                    provisional: provisional.clone(),
                    pairs,
                    rendered: first_tracks.len(),
                    reused: reused.len(),
                    fingerprint: fps.as_ref().map(|f| f.project),
                    fingerprints: fps.clone(),
                    devices: Arc::clone(&devices),
                    reference: None,
                    renders,
                    latency,
                });
            }
        };
        publish(&report, false);
        // Their pairs (an unchanged partner's excerpts from its held
        // signal), shown too; sources rendered for them are kept.
        let t0 = Instant::now();
        let out = super::extra::run(
            &shared.stage(planned, base, fps.as_ref(), sources, progress, &look_held),
            &input,
            &mut report,
        )?;
        phases.push(("their pairs", t0.elapsed().as_secs_f64()));
        publish(&report, true);
        earlier = Some(out);
        progress.stage.store(super::STAGE_RENDER, Ordering::Relaxed);
        progress
            .measuring
            .store(missing.len() as u32, Ordering::Relaxed);
    } else if alone && !whole {
        // Only tracks are missing: measured in a render of what they need,
        // the rest as it was.
        let t0 = Instant::now();
        let project = pruned(base, &keep);
        let (report, rendered) = render_and_measure(
            &project, planned, &first, false, &mut room, shared, progress, sources,
        )?;
        renders += 1;
        latency = Some(report.latency);
        take(rendered, &mut profiles, &mut held)?;
        phases.push(("the changed tracks", t0.elapsed().as_secs_f64()));
    }
    let t0 = Instant::now();
    let rest: Vec<usize> = (0..planned.len())
        .filter(|&i| profiles[i].is_none())
        .collect();
    let devices = match cached_health {
        Some(d) if !whole => d,
        _ => {
            // One render of the whole mix: taps for what is missing, every
            // device probed; the profiles built beside it on a few threads.
            let (report, rendered) = render_and_measure(
                base, planned, &rest, true, &mut room, shared, progress, sources,
            )?;
            renders += 1;
            latency = Some(report.latency);
            if report.missed > 0 {
                notes.push(format!(
                    "{} frames of the tapped signals were lost (measured as silence)",
                    report.missed
                ));
            }
            take(rendered, &mut profiles, &mut held)?;
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
    if profiles.iter().any(Option::is_none) {
        return Err("a signal was not captured".to_string());
    }
    // The reference beside the master.
    let t0 = Instant::now();
    let master = planned
        .iter()
        .zip(&profiles)
        .find(|(p, _)| matches!(p.plays, Plays::Master))
        .and_then(|(p, prof)| Some((p, prof.as_ref()?)));
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
    let (input, mut report) = diagnose_signals(planned, &profiles, base, shared, &devices, cancel)?;
    report.merge(reference_findings);
    phases.push(("rules", t0.elapsed().as_secs_f64()));
    // The first stage's findings are shown while the second runs (unless
    // the changed tracks' pairs are shown already: they would go missing
    // until the second stage takes its measurements up again).
    if earlier.is_none()
        && let Ok(mut first) = progress.first.lock()
    {
        *first = Some(super::Partial {
            report: report.clone(),
            provisional: Vec::new(),
            pairs: false,
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
    // (their excerpts from the held signals, else tapped in one render of
    // what they need).
    let t0 = Instant::now();
    let mut stage = shared.stage(planned, base, fps.as_ref(), sources, progress, &held);
    // The sources the first look's pairs rendered are exact: kept (its
    // pairs are measured again here, from the whole render's signals).
    let cache;
    if let Some(e) = &earlier {
        cache = shared
            .cache
            .iter()
            .cloned()
            .chain(e.made_profiles().cloned())
            .collect::<Vec<_>>();
        stage.cache = &cache;
    }
    let mut extra = super::extra::run(&stage, &input, &mut report)?;
    if let Some(e) = earlier {
        extra.absorb_sources(e);
    }
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
        held: held_made,
        held_used,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Session;
    use crate::render::RenderProgress;
    use faderframe_project::TrackKind;

    /// Each tapped signal of a render of `project` (frames from the start).
    fn taps(project: &Project, taps: &[TrackId], frames: usize) -> HashMap<TrackId, Vec<Vec<f32>>> {
        let rate = project.sample_rate;
        let mut sources = faderframe_engine::render_generated_sources(project, rate);
        assert!(crate::media::open_file_sources(project, None, &mut sources).is_empty());
        let mut out: HashMap<TrackId, Vec<Vec<f32>>> = HashMap::new();
        render_captured(
            project,
            rate,
            &sources,
            (0, frames),
            &RenderProgress::default(),
            2,
            CapturePlan {
                tracks: taps.to_vec(),
                devices: false,
                block: 0,
            },
            &mut |who, piece| {
                if let Some(t) = who {
                    let o = out
                        .entry(t)
                        .or_insert_with(|| vec![Vec::new(); piece.len()]);
                    for (o, c) in o.iter_mut().zip(piece) {
                        o.extend_from_slice(c);
                    }
                }
            },
        )
        .unwrap_or_else(|e| panic!("{e}"));
        out
    }

    #[test]
    fn a_pruned_render_taps_what_the_whole_mix_taps() {
        let mut s = Session::demo(faderframe_engine::EngineConfig::default())
            .unwrap_or_else(|e| panic!("{e}"));
        let base = s.render_copy();
        let frames = 3 * base.sample_rate as usize;
        let tracks: Vec<TrackId> = base
            .tracks
            .iter()
            .filter(|t| super::super::analysable(t.kind) && !t.mute)
            .map(|t| t.id)
            .collect();
        let whole = taps(&base, &tracks, frames);
        // A track alone, a pair, and a bus with what reaches it.
        let mut checked = 0;
        let sources: Vec<TrackId> = base
            .tracks
            .iter()
            .filter(|t| matches!(t.kind, TrackKind::Audio | TrackKind::Instrument) && !t.mute)
            .map(|t| t.id)
            .collect();
        let bus = base
            .tracks
            .iter()
            .find(|t| t.kind == TrackKind::Bus && tracks.contains(&t.id))
            .map(|t| t.id);
        let sets: Vec<Vec<TrackId>> = [
            vec![sources[0]],
            vec![sources[1], sources[sources.len() - 1]],
        ]
        .into_iter()
        .chain(bus.map(|b| vec![b]))
        .collect();
        for set in sets {
            let keep = needed(&base, &set);
            let cut = pruned(&base, &keep);
            assert!(
                weight(cut.tracks.iter()) < weight(base.tracks.iter()),
                "pruning leaves less to render"
            );
            let got = taps(&cut, &set, frames);
            for t in &set {
                let a = &whole[t];
                let b = &got[t];
                assert_eq!(a.len(), b.len());
                let mut energy = 0.0;
                for (x, y) in a.iter().zip(b) {
                    assert_eq!(x.len(), y.len());
                    energy += x.iter().map(|v| f64::from(*v).powi(2)).sum::<f64>();
                    assert!(x == y, "{t}: the same samples as in the whole mix");
                }
                if energy > 0.0 {
                    checked += 1;
                }
            }
        }
        assert!(checked >= 2, "sounding taps compared: {checked}");
    }
}
