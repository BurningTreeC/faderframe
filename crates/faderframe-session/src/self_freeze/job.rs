//! A background render of one track's chain (see [`super`]): the track and
//! what reaches its chain alone, into a plain master, at the engine's rate
//! and callback size, the latency kept at the front; written to the cache
//! folder as it goes, on a thread below the UI's priority.

use faderframe_audio_files::{WavFormat, wavstream::WavWriter};
use faderframe_automation::AutomationTarget as A;
use faderframe_core::{ChannelLayout, TrackId};
use faderframe_engine::offline::OfflineRenderer;
use faderframe_engine::{EngineConfig, NodeWork, render_generated_sources};
use faderframe_project::{OutputRouting, Project, TrackKind};
use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::thread::JoinHandle;

/// Below this (about −120 dBFS) a chain is silent.
pub(crate) const SILENT: f32 = 1.0e-6;
/// The tail ends after this long silent …
pub(crate) const SILENT_SECONDS: f64 = 0.5;
/// … or the chain sounds on its own (it is never cached).
pub(crate) const MAX_TAIL_SECONDS: f64 = 30.0;
/// Rendered before the cost is first judged.
const PROBE_SECONDS: f64 = 6.0;

/// What a render needs (made on the control thread).
pub(crate) struct JobInput {
    /// The track and what reaches its chain alone ([`trimmed`]).
    pub project: Project,
    pub track: TrackId,
    pub key: u64,
    pub rate: u32,
    /// Device callback and engine block (the live engine's).
    pub block: usize,
    pub max_block: usize,
    /// Timeline samples the clips playing into the chain span.
    pub start: i64,
    pub end: i64,
    /// Cheaper than this (a share of one core) is not worth a cache.
    pub min_cost: f64,
    pub temp: PathBuf,
    pub dest: PathBuf,
}

/// A finished render.
#[derive(Clone, Debug)]
pub(crate) enum Outcome {
    Cached {
        frames: u64,
        channels: u16,
        file_latency: u32,
        chain_latency: u32,
        cost: f64,
    },
    /// Not worth a cache.
    Cheap { cost: f64 },
    /// It never falls silent: a cache could not stand for it outside the
    /// span it covers.
    Sounding,
}

#[derive(Debug, Default)]
pub(crate) struct Progress {
    pub done: AtomicU64,
    pub total: AtomicU64,
    pub cancel: AtomicBool,
}

impl Progress {
    pub fn fraction(&self) -> f32 {
        let total = self.total.load(Ordering::Relaxed).max(1);
        (self.done.load(Ordering::Relaxed) as f64 / total as f64).min(1.0) as f32
    }
}

pub(crate) struct Job {
    pub track: TrackId,
    pub key: u64,
    pub start: i64,
    pub rate: u32,
    pub progress: Arc<Progress>,
    handle: Option<JoinHandle<Result<Outcome, String>>>,
}

impl Job {
    pub fn spawn(input: JobInput) -> Result<Self, String> {
        let progress = Arc::new(Progress::default());
        let p = Arc::clone(&progress);
        let (track, key, start, rate) = (input.track, input.key, input.start, input.rate);
        let handle = std::thread::Builder::new()
            .name("faderframe-self-freeze".into())
            .spawn(move || {
                faderframe_realtime::priority::lower_this_thread();
                let temp = input.temp.clone();
                let r = render(input, &p);
                if !matches!(r, Ok(Outcome::Cached { .. })) {
                    let _ = std::fs::remove_file(&temp);
                }
                r
            })
            .map_err(|e| e.to_string())?;
        Ok(Self {
            track,
            key,
            start,
            rate,
            progress,
            handle: Some(handle),
        })
    }

    pub fn is_finished(&self) -> bool {
        self.handle.as_ref().is_none_or(JoinHandle::is_finished)
    }

    pub fn cancel(&self) {
        self.progress.cancel.store(true, Ordering::Relaxed);
    }

    pub fn join(mut self) -> Result<Outcome, String> {
        match self.handle.take().map(JoinHandle::join) {
            Some(Ok(r)) => r,
            _ => Err("the render stopped".into()),
        }
    }
}

impl Drop for Job {
    fn drop(&mut self) {
        // Never left running unwatched (a session going away).
        if let Some(h) = self.handle.take() {
            self.progress.cancel.store(true, Ordering::Relaxed);
            let _ = h.join();
        }
    }
}

/// `project` with only `track` and what reaches its chain (`closure`),
/// played the way the chain hears it: the track at unity and centre, no
/// sends or delay, straight into a plain master (mono for a mono chain:
/// no pan law); the MIDI tracks playing it as muted as they are now; the
/// tracks keying it muted (they still key: sidechains tap them before
/// their faders). Nothing else of the project's mix is built.
pub(crate) fn trimmed(project: &Project, track: TrackId, closure: &[TrackId]) -> Project {
    let solo = project.solo_silenced_except();
    let muted: Vec<(TrackId, bool)> = closure
        .iter()
        .filter_map(|id| project.track(*id))
        .map(|t| (t.id, super::key::midi_muted(project, t, solo.as_ref())))
        .collect();
    let mut p = project.clone();
    let master = p.master_id();
    let keep: HashSet<TrackId> = closure.iter().copied().chain(master).collect();
    p.tracks.retain(|t| keep.contains(&t.id));
    let listed: HashSet<faderframe_core::ClipId> = p
        .tracks
        .iter()
        .flat_map(|t| t.clips.iter().copied())
        .collect();
    p.clips
        .retain(|id, c| keep.contains(&c.track) && listed.contains(id));
    let used: HashSet<_> = p
        .clips
        .values()
        .flat_map(|c| c.content.referenced_sources())
        .chain(
            p.tracks
                .iter()
                .filter_map(|t| t.freeze.as_ref().map(|f| f.source)),
        )
        .collect();
    p.sources.retain(|id, _| used.contains(id));
    p.launcher = Default::default();
    p.cue_mixes.clear();
    p.album = Default::default();
    p.references = Default::default();
    p.console = None;
    p.crosstalk = false;
    p.loop_enabled = false;
    p.punch_enabled = false;
    let mono = project
        .track(track)
        .is_some_and(|t| t.chain_layout() == ChannelLayout::Mono);
    let to_master = master.map_or(OutputRouting::Master, |m| OutputRouting::Track { track: m });
    for t in &mut p.tracks {
        t.folder = None;
        t.vca = None;
        t.group = None;
        t.solo = false;
        t.sends.clear();
        t.cue_sends.clear();
        t.record_arm = false;
        t.midi_output = None;
        if t.kind == TrackKind::Master {
            if mono {
                t.layout = ChannelLayout::Mono;
            }
            t.volume_db = 0.0;
            t.pan = 0.0;
            t.pan_mode = faderframe_core::PanMode::Balance;
            t.mute = false;
            t.phase_invert = false;
            t.inserts.clear();
            t.instrument = None;
            t.preamp = None;
            t.containers.clear();
            t.automation.lanes.clear();
            t.modulators.clear();
            continue;
        }
        if t.kind != TrackKind::Midi {
            t.output = to_master;
        }
        if t.id == track {
            t.volume_db = 0.0;
            t.pan = 0.0;
            t.pan_mode = faderframe_core::PanMode::Balance;
            t.dual_pan = faderframe_core::DualPan::ORIGINAL;
            t.mute = false;
            t.phase_invert = false;
            t.delay_ms = 0.0;
            t.object = false;
            t.freeze = None;
            t.automation
                .lanes
                .retain(|l| matches!(l.target, A::PluginParameter { .. } | A::PluginBypass(_)));
        } else if t.kind == TrackKind::Midi {
            t.mute = muted.iter().any(|(id, m)| *id == t.id && *m);
        } else {
            // A key: heard by the chain, not by the master.
            t.mute = true;
            t.automation.lanes.retain(|l| l.target != A::TrackMute);
        }
    }
    p
}

fn render(input: JobInput, progress: &Progress) -> Result<Outcome, String> {
    let JobInput {
        project,
        track,
        rate,
        block,
        max_block,
        start,
        end,
        min_cost,
        temp,
        dest,
        ..
    } = input;
    let mut sources = render_generated_sources(&project, rate);
    if let Some((_, path, e)) = crate::media::open_file_sources(&project, None, &mut sources)
        .into_iter()
        .next()
    {
        return Err(format!("{}: {e}", path.display()));
    }
    let channels = project
        .track(track)
        .map_or(2, |t| t.chain_layout().channel_count().clamp(1, 2));
    let config = EngineConfig {
        sample_rate: rate,
        max_block_size: max_block.max(1),
        measure_nodes: true,
        ..EngineConfig::default()
    };
    let block = block.max(1);
    let mut r =
        OfflineRenderer::new(&project, &sources, config, block, 2).map_err(|e| e.to_string())?;
    r.controller.set_node_timing(true);
    let file_latency = r.controller.graph_stats().output_latency;
    let chain_latency = r.controller.track_latency(track);
    let span = (end - start).max(0) as u64 + u64::from(file_latency);
    let silent_run = (SILENT_SECONDS * f64::from(rate)) as u64;
    let most = span + (MAX_TAIL_SECONDS * f64::from(rate)) as u64;
    progress.total.store(span + silent_run, Ordering::Relaxed);
    let probe = (PROBE_SECONDS * f64::from(rate)) as u64;
    if let Some(dir) = temp.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    let mut writer = WavWriter::create(&temp, channels as u16, rate, WavFormat::Float32, false)
        .map_err(|e| format!("{}: {e}", temp.display()))?;
    let cost_now = |r: &OfflineRenderer, frames: u64| -> f64 {
        let Some(profile) = r.controller.graph_profile() else {
            return 0.0;
        };
        let seconds = frames as f64 / f64::from(rate.max(1));
        if seconds <= 0.0 {
            return 0.0;
        }
        let ns: u64 = profile
            .owners
            .iter()
            .enumerate()
            .filter(|(_, o)| {
                o.is_some_and(|o| {
                    o.plugin.is_some() && matches!(o.work, NodeWork::Insert | NodeWork::Instrument)
                })
            })
            .map(|(i, _)| profile.timings.total_ns(i))
            .sum();
        ns as f64 / 1e9 / seconds
    };
    r.play_from(start).map_err(|e| e.to_string())?;
    let mut frames = 0u64;
    let mut quiet = 0u64;
    let mut judged = false;
    loop {
        if progress.cancel.load(Ordering::Relaxed) {
            return Err("cancelled".into());
        }
        let out = r.step();
        let n = out.output_ref(0).len();
        let chs: Vec<&[f32]> = (0..channels).map(|c| out.output_ref(c)).collect();
        writer
            .write_planar(&chs, n)
            .map_err(|e| format!("{}: {e}", temp.display()))?;
        // The tail: silent long enough after the span ends it.
        let loud = chs
            .iter()
            .flat_map(|c| c.iter().rposition(|s| s.abs() > SILENT))
            .max();
        quiet = match loud {
            Some(i) => (n - 1 - i) as u64,
            None => quiet + n as u64,
        };
        frames += n as u64;
        progress.done.store(frames, Ordering::Relaxed);
        if !judged && frames >= probe.min(span) {
            judged = true;
            // Plainly cheap: not worth rendering on.
            let cost = cost_now(&r, frames);
            if cost < min_cost * 0.5 {
                return Ok(Outcome::Cheap { cost });
            }
        }
        if frames >= span && quiet >= silent_run {
            break;
        }
        if frames >= most {
            return Ok(Outcome::Sounding);
        }
    }
    let cost = cost_now(&r, frames);
    if cost < min_cost {
        return Ok(Outcome::Cheap { cost });
    }
    writer
        .finish()
        .map_err(|e| format!("{}: {e}", temp.display()))?;
    std::fs::rename(&temp, &dest).map_err(|e| format!("{}: {e}", dest.display()))?;
    Ok(Outcome::Cached {
        frames,
        channels: channels as u16,
        file_latency,
        chain_latency,
        cost,
    })
}
