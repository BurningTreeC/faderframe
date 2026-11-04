//! Batch processing: a chain of steps applied to a list of files offline,
//! each file written anew into an output folder.
//!
//! A [`BatchChain`] is a list of [`BatchStep`]s — plugins (built-in or
//! third-party, with their state and parameter values), peak
//! normalisation, loudness normalisation with a true-peak ceiling, sample
//! rate and channel conversion, trimming silence, fades — and the
//! [`BatchOutput`]: the file format (WAV or FLAC), its depth and dither,
//! and a rename pattern ([`naming`]). [`start`] runs a [`BatchRequest`]
//! on worker threads of its own (never the UI's or the audio's, at a
//! lowered priority), a file per worker: each is decoded from any format
//! the importer reads, at its own rate, taken through the steps in
//! order, measured and written beside its destination under a temporary
//! name that moves into place only when it is complete
//! ([`crate::render::files`]). The returned [`BatchRun`] reports every
//! file's state ([`BatchRun::status`]) and can be cancelled; nothing is
//! a project edit, so nothing reaches the undo history.
//!
//! Plugin steps run through the engine's offline renderer as the master
//! insert of a one-track project holding the file's audio in memory (an
//! engine of their own, never the live one's sources), with the plugin's
//! latency taken off the front and its tail kept as asked
//! ([`PluginStep::tail_seconds`]).
//!
//! An input is never written over: a destination that is one of the
//! batch's inputs is numbered instead, whatever [`IfExists`] says.
//! Presets of chains are JSON files in the data folder's
//! `batch-presets` ([`presets`]), beside a few factory ones.

mod naming;
mod plugins;
mod presets;
mod run;
mod steps;

pub use naming::{TOKENS, check_pattern, rate_text};
pub use presets::{
    BatchPreset, delete_preset, delete_preset_in, factory_presets, preset_named, preset_named_in,
    presets, presets_dir, presets_in, save_preset, save_preset_in,
};
pub use run::{BatchRun, BatchStatus, FileResult, FileState, FileStatus, MEMORY_BUDGET, start};

use faderframe_audio_files::{Dither, WavFormat};
use faderframe_core::ParameterId;
use faderframe_project::{FadeShape, PluginRef, SavedParameter};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Sample rates a convert step offers.
pub const SAMPLE_RATES: [u32; 7] = [22_050, 32_000, 44_100, 48_000, 88_200, 96_000, 192_000];

/// The lowest and highest rate a convert step takes.
pub const RATE_RANGE: (u32, u32) = (8_000, 384_000);

/// One step of a chain.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "step", rename_all = "kebab-case")]
pub enum BatchStep {
    Plugin(PluginStep),
    Normalize(NormalizeStep),
    Loudness(LoudnessStep),
    Convert(ConvertStep),
    TrimSilence(TrimStep),
    Fade(FadeStep),
}

/// The kinds of step (what the "Add Step" menu offers).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StepKind {
    Plugin,
    Normalize,
    Loudness,
    Convert,
    TrimSilence,
    Fade,
}

impl StepKind {
    pub const ALL: [StepKind; 6] = [
        StepKind::Plugin,
        StepKind::Normalize,
        StepKind::Loudness,
        StepKind::Convert,
        StepKind::TrimSilence,
        StepKind::Fade,
    ];

    pub fn label(self) -> &'static str {
        match self {
            StepKind::Plugin => "Plugin",
            StepKind::Normalize => "Normalise",
            StepKind::Loudness => "Loudness",
            StepKind::Convert => "Convert",
            StepKind::TrimSilence => "Trim Silence",
            StepKind::Fade => "Fade In and Out",
        }
    }

    /// A step of this kind with its usual settings (a plugin step needs
    /// its plugin: [`PluginStep::new`]).
    pub fn default_step(self) -> Option<BatchStep> {
        Some(match self {
            StepKind::Plugin => return None,
            StepKind::Normalize => BatchStep::Normalize(NormalizeStep::default()),
            StepKind::Loudness => BatchStep::Loudness(LoudnessStep::default()),
            StepKind::Convert => BatchStep::Convert(ConvertStep::default()),
            StepKind::TrimSilence => BatchStep::TrimSilence(TrimStep::default()),
            StepKind::Fade => BatchStep::Fade(FadeStep::default()),
        })
    }
}

impl BatchStep {
    pub fn kind(&self) -> StepKind {
        match self {
            BatchStep::Plugin(_) => StepKind::Plugin,
            BatchStep::Normalize(_) => StepKind::Normalize,
            BatchStep::Loudness(_) => StepKind::Loudness,
            BatchStep::Convert(_) => StepKind::Convert,
            BatchStep::TrimSilence(_) => StepKind::TrimSilence,
            BatchStep::Fade(_) => StepKind::Fade,
        }
    }

    /// Its name in lists: the kind, or a plugin's name.
    pub fn label(&self) -> String {
        match self {
            BatchStep::Plugin(p) => p.plugin.name.clone(),
            other => other.kind().label().to_string(),
        }
    }

    /// What it does, in a line.
    pub fn summary(&self) -> String {
        match self {
            BatchStep::Plugin(p) => {
                let mut s = if p.state.is_some() {
                    "its saved settings".to_string()
                } else {
                    "its defaults".to_string()
                };
                if !p.parameters.is_empty() {
                    s.push_str(&format!(
                        ", {} value{} set",
                        p.parameters.len(),
                        if p.parameters.len() == 1 { "" } else { "s" }
                    ));
                }
                if p.tail_seconds > 0.0 {
                    s.push_str(&format!(", tail up to {:.1} s", p.tail_seconds));
                }
                s
            }
            BatchStep::Normalize(n) => match n.peak {
                PeakKind::Sample => format!("sample peak to {}", db_text(n.ceiling_db, "dBFS")),
                PeakKind::True => format!("true peak to {}", db_text(n.ceiling_db, "dBTP")),
            },
            BatchStep::Loudness(l) => format!(
                "{} integrated, true peak at most {}{}",
                db_text(l.target_lufs, "LUFS"),
                db_text(l.true_peak_db, "dBTP"),
                if l.lower_gain {
                    " (less gain rather than limiting)"
                } else {
                    ""
                }
            ),
            BatchStep::Convert(c) => {
                let rate = c
                    .sample_rate
                    .map_or_else(|| "rate kept".to_string(), |r| format!("{} Hz", r));
                let channels = match c.channels {
                    Channels::Keep => "channels kept",
                    Channels::Mono => "mono",
                    Channels::Stereo => "stereo",
                };
                format!("{rate}, {channels}")
            }
            BatchStep::TrimSilence(t) => {
                let ends = match (t.start, t.end) {
                    (true, true) => "start and end",
                    (true, false) => "start",
                    (false, true) => "end",
                    (false, false) => "nothing",
                };
                format!(
                    "{ends} below {}, {:.0} ms kept before, {:.0} ms after",
                    db_text(t.threshold_db, "dBFS"),
                    t.pre_pad * 1000.0,
                    t.post_pad * 1000.0
                )
            }
            BatchStep::Fade(f) => format!(
                "in {:.0} ms ({}), out {:.0} ms ({})",
                f.fade_in * 1000.0,
                f.shape_in.label(),
                f.fade_out * 1000.0,
                f.shape_out.label()
            ),
        }
    }

    /// Settings that cannot work, said plainly (`None`: fine).
    pub fn problem(&self) -> Option<String> {
        match self {
            BatchStep::Plugin(p) if p.plugin.id.is_empty() => Some("no plugin chosen".into()),
            BatchStep::Plugin(p) if !batchable(&p.plugin) => Some(format!(
                "{} works only on a track (it needs the audio device, its chains or the \
                 track's input stage)",
                p.plugin.name
            )),
            BatchStep::Convert(c) => c
                .sample_rate
                .filter(|r| !(RATE_RANGE.0..=RATE_RANGE.1).contains(r))
                .map(|r| format!("{r} Hz is outside {}–{} Hz", RATE_RANGE.0, RATE_RANGE.1)),
            BatchStep::Normalize(n) if !n.ceiling_db.is_finite() || n.ceiling_db > 0.0 => {
                Some("a normalising ceiling is at most 0 dB".into())
            }
            BatchStep::Loudness(l)
                if !l.target_lufs.is_finite()
                    || !l.true_peak_db.is_finite()
                    || l.true_peak_db > 0.0 =>
            {
                Some("a loudness step needs a target and a ceiling of at most 0 dBTP".into())
            }
            BatchStep::Fade(f) if !(f.fade_in >= 0.0 && f.fade_out >= 0.0) => {
                Some("fade lengths are zero or more".into())
            }
            _ => None,
        }
    }
}

/// Can a batch run `plugin`? Not the Hardware Insert (the audio device's
/// outputs and inputs), a container (its chains live on a track) or an
/// input-stage device (a preamp, a console bus).
pub fn batchable(plugin: &PluginRef) -> bool {
    use faderframe_core::builtin;
    plugin.format != faderframe_project::PluginFormat::Builtin
        || !(plugin.id == builtin::HARDWARE_INSERT
            || plugin.id == builtin::CONTAINER
            || builtin::is_input_stage(&plugin.id))
}

/// "−1.0 dBTP" (a true minus sign).
fn db_text(v: f32, unit: &str) -> String {
    let t = format!("{v:.1} {unit}");
    t.replacen('-', "\u{2212}", 1)
}

/// A plugin and how it is set.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PluginStep {
    pub plugin: PluginRef,
    /// The plugin's own state (as projects store it), loaded first.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state: Option<String>,
    /// Values set after the state (plain units).
    #[serde(default)]
    pub parameters: Vec<SavedParameter>,
    /// Seconds kept after the audio for what the plugin rings on (cut
    /// where it falls silent); 0 keeps the file's length.
    #[serde(default)]
    pub tail_seconds: f32,
}

impl PluginStep {
    pub fn new(plugin: PluginRef) -> Self {
        Self {
            plugin,
            state: None,
            parameters: Vec::new(),
            tail_seconds: 0.0,
        }
    }

    /// A value set explicitly.
    pub fn parameter(&self, id: ParameterId) -> Option<f64> {
        self.parameters.iter().find(|p| p.id == id).map(|p| p.value)
    }

    /// Set a value (replacing one set before).
    pub fn set_parameter(&mut self, id: ParameterId, value: f64) {
        match self.parameters.iter_mut().find(|p| p.id == id) {
            Some(p) => p.value = value,
            None => self.parameters.push(SavedParameter { id, value }),
        }
    }

    /// A built-in's factory preset `index`: every value it sets, the state
    /// dropped (`false`: no such preset).
    pub fn use_factory_preset(&mut self, index: usize) -> bool {
        if self.plugin.format != faderframe_project::PluginFormat::Builtin {
            return false;
        }
        let Some(values) =
            faderframe_plugin_host::presets::factory_preset_values(&self.plugin.id, index)
        else {
            return false;
        };
        self.state = None;
        self.parameters = values
            .into_iter()
            .map(|(id, value)| SavedParameter { id, value })
            .collect();
        true
    }
}

/// Which peak a normalise step measures.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum PeakKind {
    /// The highest sample.
    Sample,
    /// The highest peak between the samples too (4× oversampled, BS.1770).
    #[default]
    True,
}

/// Peak normalisation: the gain that brings the peak to the ceiling.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct NormalizeStep {
    #[serde(default)]
    pub peak: PeakKind,
    pub ceiling_db: f32,
}

impl Default for NormalizeStep {
    fn default() -> Self {
        Self {
            peak: PeakKind::True,
            ceiling_db: -1.0,
        }
    }
}

/// Loudness normalisation (BS.1770 integrated loudness) with a true-peak
/// ceiling ([`faderframe_analysis::delivery::normalize_loudness`]).
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct LoudnessStep {
    pub target_lufs: f32,
    pub true_peak_db: f32,
    /// Use less gain where the peaks would pass the ceiling (the loudness
    /// may stay under the target) rather than limiting them.
    #[serde(default)]
    pub lower_gain: bool,
}

impl Default for LoudnessStep {
    fn default() -> Self {
        Self {
            target_lufs: -16.0,
            true_peak_db: -1.0,
            lower_gain: false,
        }
    }
}

/// What a convert step does to the channels.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Channels {
    #[default]
    Keep,
    /// The channels' average.
    Mono,
    /// A mono file on both sides; a file of more channels keeps its first
    /// two.
    Stereo,
}

impl Channels {
    pub const ALL: [Channels; 3] = [Channels::Keep, Channels::Mono, Channels::Stereo];

    pub fn label(self) -> &'static str {
        match self {
            Channels::Keep => "Keep",
            Channels::Mono => "Mono",
            Channels::Stereo => "Stereo",
        }
    }
}

/// Sample rate (the importer's resampler) and channel conversion.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConvertStep {
    /// `None`: the file's rate.
    #[serde(default)]
    pub sample_rate: Option<u32>,
    #[serde(default)]
    pub channels: Channels,
}

/// Trimming silence at the start and the end (what lies before the first
/// and after the last sound [`faderframe_analysis::silence::regions`]
/// finds). Times in seconds.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct TrimStep {
    pub threshold_db: f32,
    /// Sound shorter than this (a click in the silence) does not count.
    #[serde(default)]
    pub min_length: f64,
    /// Kept before the first sound.
    #[serde(default)]
    pub pre_pad: f64,
    /// Kept after the last sound.
    #[serde(default)]
    pub post_pad: f64,
    #[serde(default = "yes")]
    pub start: bool,
    #[serde(default = "yes")]
    pub end: bool,
}

fn yes() -> bool {
    true
}

impl Default for TrimStep {
    fn default() -> Self {
        Self {
            threshold_db: -60.0,
            min_length: 0.01,
            pre_pad: 0.01,
            post_pad: 0.1,
            start: true,
            end: true,
        }
    }
}

impl TrimStep {
    /// As the silence finder takes it.
    pub fn settings(&self) -> faderframe_analysis::silence::SilenceSettings {
        faderframe_analysis::silence::SilenceSettings {
            threshold_db: self.threshold_db,
            min_length: self.min_length,
            pre_pad: self.pre_pad,
            post_pad: self.post_pad,
            ..Default::default()
        }
        .clamped()
    }
}

/// Fades at the start and the end (seconds; 0 = none).
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct FadeStep {
    pub fade_in: f64,
    pub fade_out: f64,
    #[serde(default)]
    pub shape_in: FadeShape,
    #[serde(default)]
    pub shape_out: FadeShape,
}

impl Default for FadeStep {
    fn default() -> Self {
        Self {
            fade_in: 0.005,
            fade_out: 0.02,
            shape_in: FadeShape::Linear,
            shape_out: FadeShape::Linear,
        }
    }
}

/// The file format written.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum FileFormat {
    /// WAV (RF64 past 4 GiB).
    #[default]
    Wav,
    /// FLAC (16 or 24 bits, up to eight channels).
    Flac,
}

impl FileFormat {
    pub const ALL: [FileFormat; 2] = [FileFormat::Wav, FileFormat::Flac];

    pub fn label(self) -> &'static str {
        match self {
            FileFormat::Wav => "WAV",
            FileFormat::Flac => "FLAC",
        }
    }

    pub fn extension(self) -> &'static str {
        match self {
            FileFormat::Wav => "wav",
            FileFormat::Flac => "flac",
        }
    }

    /// The depths it holds.
    pub fn depths(self) -> &'static [WavFormat] {
        match self {
            FileFormat::Wav => &[WavFormat::Pcm16, WavFormat::Pcm24, WavFormat::Float32],
            FileFormat::Flac => &[WavFormat::Pcm16, WavFormat::Pcm24],
        }
    }
}

/// "16", "24", "32f".
pub fn depth_text(depth: WavFormat) -> &'static str {
    match depth {
        WavFormat::Pcm16 => "16",
        WavFormat::Pcm24 => "24",
        WavFormat::Float32 => "32f",
    }
}

/// How the files are written and named.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct BatchOutput {
    #[serde(default)]
    pub format: FileFormat,
    pub depth: WavFormat,
    /// For integer depths.
    #[serde(default)]
    pub dither: Dither,
    /// The file name without extension ([`TOKENS`]).
    pub pattern: String,
}

impl Default for BatchOutput {
    fn default() -> Self {
        Self {
            format: FileFormat::Wav,
            depth: WavFormat::Pcm24,
            dither: Dither::Tpdf,
            pattern: "{name}".into(),
        }
    }
}

/// The steps and the output.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct BatchChain {
    #[serde(default)]
    pub steps: Vec<BatchStep>,
    #[serde(default)]
    pub output: BatchOutput,
}

impl BatchChain {
    /// Can it run? (The first problem, said plainly.)
    pub fn check(&self) -> Result<(), BatchError> {
        check_pattern(&self.output.pattern).map_err(BatchError::Pattern)?;
        if !self.output.format.depths().contains(&self.output.depth) {
            return Err(BatchError::Settings(format!(
                "{} files are written with 16 or 24 bits",
                self.output.format.label()
            )));
        }
        for (i, step) in self.steps.iter().enumerate() {
            if let Some(p) = step.problem() {
                return Err(BatchError::Settings(format!(
                    "step {} ({}): {p}",
                    i + 1,
                    step.label()
                )));
            }
        }
        Ok(())
    }
}

/// What happens when a file of the output's name is in the folder.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum IfExists {
    /// Keep both: the new file gets a number (" 2", " 3" …).
    #[default]
    Number,
    /// Replace it (never one of the batch's inputs: those are numbered).
    Replace,
    /// Leave it and skip the input.
    Skip,
}

impl IfExists {
    pub const ALL: [IfExists; 3] = [IfExists::Number, IfExists::Replace, IfExists::Skip];

    pub fn label(self) -> &'static str {
        match self {
            IfExists::Number => "Keep Both (Number the New File)",
            IfExists::Replace => "Replace It",
            IfExists::Skip => "Skip the Input",
        }
    }
}

/// The moment names are stamped with ([`TOKENS`] `{date}` and `{time}`):
/// the shell passes its local time; [`Stamp::utc_now`] otherwise.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Stamp {
    /// "2026-11-03".
    pub date: String,
    /// "14-03-12".
    pub time: String,
}

impl Stamp {
    pub fn utc_now() -> Self {
        // "2026-11-03T14:03:12Z".
        let iso = faderframe_qc::date::Utc::now().iso();
        let (date, time) = iso.split_once('T').unwrap_or((iso.as_str(), ""));
        Self {
            date: date.to_string(),
            time: time.trim_end_matches('Z').replace(':', "-"),
        }
    }
}

/// A batch to run.
#[derive(Clone, Debug)]
pub struct BatchRequest {
    pub files: Vec<PathBuf>,
    pub chain: BatchChain,
    /// Where the files go (made when missing).
    pub folder: PathBuf,
    pub existing: IfExists,
    /// Files processed at once (at least one).
    pub workers: usize,
    /// The preset's name (`{preset}`).
    pub preset: String,
    pub stamp: Stamp,
}

impl BatchRequest {
    /// A request with the usual settings.
    pub fn new(files: Vec<PathBuf>, chain: BatchChain, folder: PathBuf) -> Self {
        Self {
            files,
            chain,
            folder,
            existing: IfExists::default(),
            workers: default_workers(),
            preset: String::new(),
            stamp: Stamp::utc_now(),
        }
    }
}

/// Files processed at once unless asked: half the cores, at most four
/// (each holds its file in memory).
pub fn default_workers() -> usize {
    (faderframe_realtime::physical_cores() / 2).clamp(1, 4)
}

/// The most files a batch processes at once.
pub const MAX_WORKERS: usize = 16;

/// Why a batch cannot start (or a preset cannot be kept).
#[derive(Debug, thiserror::Error)]
pub enum BatchError {
    #[error("there are no files to process")]
    NoFiles,
    #[error("{0}")]
    Pattern(String),
    #[error("{0}")]
    Settings(String),
    #[error("cannot write into the folder {path}: {source}")]
    Folder {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("{0}")]
    Preset(String),
    #[error("cannot start the batch: {0}")]
    Thread(std::io::Error),
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    /// Every kind of step survives JSON, and old files missing optional
    /// fields read with their defaults.
    #[test]
    fn chains_round_trip_through_json() {
        let mut plugin = PluginStep::new(PluginRef::builtin(
            faderframe_core::builtin::GAIN,
            "Utility",
        ));
        plugin.set_parameter(ParameterId(0), -6.0);
        plugin.tail_seconds = 1.5;
        let chain = BatchChain {
            steps: vec![
                BatchStep::Plugin(plugin),
                BatchStep::Normalize(NormalizeStep::default()),
                BatchStep::Loudness(LoudnessStep::default()),
                BatchStep::Convert(ConvertStep {
                    sample_rate: Some(44_100),
                    channels: Channels::Stereo,
                }),
                BatchStep::TrimSilence(TrimStep::default()),
                BatchStep::Fade(FadeStep {
                    shape_out: FadeShape::EqualPower,
                    ..FadeStep::default()
                }),
            ],
            output: BatchOutput {
                format: FileFormat::Flac,
                depth: WavFormat::Pcm16,
                dither: Dither::Shaped,
                pattern: "{n:2} {name}".into(),
            },
        };
        let text = serde_json::to_string_pretty(&chain).unwrap();
        let back: BatchChain = serde_json::from_str(&text).unwrap();
        assert_eq!(back, chain);
        let old: BatchChain = serde_json::from_str(
            r#"{"steps":[{"step":"trim-silence","threshold_db":-50.0}],
                "output":{"depth":"Pcm24","pattern":"{name}"}}"#,
        )
        .unwrap();
        let BatchStep::TrimSilence(t) = &old.steps[0] else {
            panic!("{old:?}")
        };
        assert!(t.start && t.end);
        assert_eq!(old.output.format, FileFormat::Wav);
    }

    #[test]
    fn checks_say_what_cannot_work() {
        let mut chain = BatchChain::default();
        assert!(chain.check().is_ok());
        chain.output.format = FileFormat::Flac;
        chain.output.depth = WavFormat::Float32;
        assert!(chain.check().is_err());
        chain.output.depth = WavFormat::Pcm24;
        chain.output.pattern = "{nmae}".into();
        assert!(matches!(chain.check(), Err(BatchError::Pattern(_))));
        chain.output.pattern = "{name}".into();
        chain.steps.push(BatchStep::Convert(ConvertStep {
            sample_rate: Some(1000),
            channels: Channels::Keep,
        }));
        assert!(matches!(chain.check(), Err(BatchError::Settings(_))));
        chain.steps = vec![BatchStep::Plugin(PluginStep::new(PluginRef::builtin(
            faderframe_core::builtin::HARDWARE_INSERT,
            "Hardware Insert",
        )))];
        assert!(matches!(chain.check(), Err(BatchError::Settings(_))));
    }

    #[test]
    fn a_built_in_takes_its_factory_presets_values() {
        let mut step = PluginStep::new(PluginRef::builtin(
            faderframe_core::builtin::COMPRESSOR,
            "Compressor",
        ));
        assert!(step.use_factory_preset(0));
        assert!(!step.parameters.is_empty());
        assert!(!step.use_factory_preset(10_000));
    }
}
