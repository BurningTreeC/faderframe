//! A plugin step: the file's audio through one plugin, offline.
//!
//! The audio becomes the only clip of a one-track project held in memory
//! (never opened from disk, never the live engine's sources) with the
//! plugin as the master's insert — mono through a mono master, stereo
//! through a stereo one, at unity — and the engine's offline renderer
//! plays it on the calling worker thread alone (no worker pool: the
//! batch runs files side by side). The plugin's latency comes off the
//! front; up to [`PluginStep::tail_seconds`] of what it rings on are
//! kept, until it falls silent (−120 dBFS).

use super::PluginStep;
use super::steps::{Audio, StepError, Work};
use faderframe_core::{ChannelLayout, PluginInstanceId};
use faderframe_engine::EngineConfig;
use faderframe_engine::offline::OfflineRenderer;
use faderframe_project::{
    AudioClip, AudioSource, Clip, ClipContent, ClipFades, PluginSlot, Project, SourceSpec,
    StretchSettings, Track, TrackColor, TrackKind,
};
use faderframe_timeline::MusicalTime;
use std::path::PathBuf;
use std::sync::Arc;

/// Frames rendered between looks at the cancel flag.
const PIECE: usize = 16_384;

/// Below this the tail has ended.
const SILENT: f32 = 1e-6;

/// The project that plays `audio` through `step`, and the plugin's slot.
fn project(
    audio: Audio,
    step: &PluginStep,
) -> (Project, faderframe_engine::SourceMap, PluginInstanceId) {
    let channels = audio.channels.len();
    let frames = audio.frames();
    let layout = if channels == 1 {
        ChannelLayout::Mono
    } else {
        ChannelLayout::Stereo
    };
    let mut p = Project::new("Batch", audio.rate);
    let source = p.ids.allocate();
    p.sources.insert(
        source,
        AudioSource {
            id: source,
            name: "Batch".into(),
            // Never opened: the audio is handed over in memory.
            spec: SourceSpec::File {
                path: PathBuf::from("batch"),
                channels: channels as u16,
                frames: frames as i64,
                sample_rate: audio.rate,
            },
        },
    );
    let track = p.ids.allocate();
    let clip = p.ids.allocate();
    let slot: PluginInstanceId = p.ids.allocate();
    let mut t =
        Track::new(track, TrackKind::Audio, "Batch", TrackColor::palette(0)).with_layout(layout);
    t.clips.push(clip);
    p.tracks.push(t);
    p.clips.insert(
        clip,
        Clip {
            id: clip,
            track,
            name: "Batch".into(),
            color: None,
            start: MusicalTime::ZERO,
            muted: false,
            content: ClipContent::Audio(AudioClip {
                source,
                source_offset: 0,
                length: frames as i64,
                gain_db: 0.0,
                fades: ClipFades::default(),
                stretch: StretchSettings::Off,
                reversed: false,
                warp: None,
                pitch: None,
                effects: None,
                spectral: None,
                envelope: None,
            }),
        },
    );
    if let Some(m) = p.tracks.iter_mut().find(|t| t.kind == TrackKind::Master) {
        m.layout = layout;
        m.inserts = vec![PluginSlot {
            id: slot,
            plugin: step.plugin.clone(),
            bypass: false,
            parameters: step.parameters.clone(),
            state: step.state.clone(),
            sidechain: None,
            mono_to_stereo: false,
            mix_origin: None,
        }];
    }
    let mut sources = faderframe_engine::SourceMap::new();
    sources.insert(
        source,
        faderframe_engine::Source::Memory(Arc::new(
            faderframe_audio_files::AudioData::from_channels(audio.rate, audio.channels),
        )),
    );
    (p, sources, slot)
}

/// `audio` through `step`'s plugin (see the module docs).
pub(super) fn process(
    audio: Audio,
    step: &PluginStep,
    work: &mut Work<'_>,
) -> Result<Audio, StepError> {
    let name = step.plugin.name.clone();
    let channels = audio.channels.len();
    if !(1..=2).contains(&channels) {
        return Err(StepError::Failed(format!(
            "{name} takes mono or stereo audio, and the file has {channels} channels \
             (a Convert step to mono or stereo before it helps)"
        )));
    }
    let frames = audio.frames();
    if frames == 0 {
        return Ok(audio);
    }
    let rate = audio.rate;
    let (p, sources, slot) = project(audio, step);
    let outputs = p
        .master_id()
        .and_then(|m| p.track(m))
        .map_or(channels, |m| m.chain_layout().channel_count());
    let config = EngineConfig {
        sample_rate: rate,
        max_block_size: 1024,
        measure_nodes: false,
        ..EngineConfig::default()
    };
    let mut r = OfflineRenderer::new(&p, &sources, config, 1024, outputs)
        .map_err(|e| StepError::Failed(format!("{name}: {e}")))?;
    let latency = r
        .controller
        .plugin_latency(slot)
        .ok_or_else(|| StepError::Failed(format!("{name} could not be loaded")))?
        as usize;
    let tail = (f64::from(step.tail_seconds.max(0.0)) * f64::from(rate)) as usize;
    let total = latency + frames + tail;
    r.play_from(0)
        .map_err(|e| StepError::Failed(format!("{name}: {e}")))?;
    let mut out: Vec<Vec<f32>> = vec![Vec::with_capacity(total); outputs];
    while out[0].len() < total {
        work.check()?;
        let n = (total - out[0].len()).min(PIECE);
        for (o, c) in out.iter_mut().zip(r.render(n)) {
            o.extend_from_slice(&c[..n.min(c.len())]);
        }
        work.report(out[0].len() as f32 / total as f32);
    }
    if r.controller.plugin_failed(slot) {
        return Err(StepError::Failed(format!("{name} stopped working")));
    }
    // The tail until it falls silent.
    let audible = out
        .iter()
        .map(|c| {
            c.iter()
                .rposition(|s| s.abs() > SILENT)
                .map_or(0, |i| i + 1)
        })
        .max()
        .unwrap_or(0);
    let end = audible.clamp(latency + frames, total);
    for c in &mut out {
        c.truncate(end);
        c.drain(..latency.min(c.len()));
    }
    if latency > 0 {
        work.note(format!("{name}: its {latency} frames of latency taken off"));
    }
    if outputs != channels {
        work.note(format!(
            "{name} made it {}",
            if outputs == 2 { "stereo" } else { "mono" }
        ));
    }
    Ok(Audio {
        channels: out,
        rate,
    })
}
