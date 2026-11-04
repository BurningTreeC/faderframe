//! Presets of batch chains: a few factory ones, and the user's as JSON
//! files in the data folder's `batch-presets` (portable mode keeps them in
//! the portable folder: [`crate::media::data_dir`]), one per preset,
//! `{"format": "faderframe.batch-preset", "version": 1, "name", "chain"}`.
//! Names match whatever their case; a factory preset's name is not taken
//! by a user's.

use super::{
    BatchChain, BatchError, BatchOutput, BatchStep, Channels, ConvertStep, FadeStep, FileFormat,
    LoudnessStep, NormalizeStep, PeakKind, TrimStep,
};
use faderframe_audio_files::{Dither, WavFormat};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// A preset: a name and its chain.
#[derive(Clone, Debug, PartialEq)]
pub struct BatchPreset {
    pub name: String,
    pub chain: BatchChain,
    /// One of the factory presets (cannot be deleted or replaced).
    pub factory: bool,
    /// The user's file.
    pub path: Option<PathBuf>,
}

const FORMAT: &str = "faderframe.batch-preset";
const VERSION: u32 = 1;

/// A name as names are compared: any case, a minus sign as a hyphen (so
/// "Normalise to -1 dBTP" finds "Normalise to −1 dBTP").
fn key(name: &str) -> String {
    name.trim().to_lowercase().replace('\u{2212}', "-")
}

#[derive(Serialize, Deserialize)]
struct PresetFile {
    format: String,
    version: u32,
    name: String,
    chain: BatchChain,
}

/// The folder of the user's presets.
pub fn presets_dir() -> PathBuf {
    crate::media::data_dir().join("batch-presets")
}

/// The factory presets.
pub fn factory_presets() -> Vec<BatchPreset> {
    let output = BatchOutput::default();
    let preset = |name: &str, steps: Vec<BatchStep>, output: BatchOutput| BatchPreset {
        name: name.into(),
        chain: BatchChain { steps, output },
        factory: true,
        path: None,
    };
    let loudness = |target: f32| {
        BatchStep::Loudness(LoudnessStep {
            target_lufs: target,
            true_peak_db: -1.0,
            lower_gain: false,
        })
    };
    vec![
        preset(
            "Normalise to \u{2212}1 dBTP",
            vec![BatchStep::Normalize(NormalizeStep {
                peak: PeakKind::True,
                ceiling_db: -1.0,
            })],
            output.clone(),
        ),
        preset(
            "Podcast \u{2212}16 LUFS",
            vec![
                BatchStep::TrimSilence(TrimStep::default()),
                loudness(-16.0),
                BatchStep::Fade(FadeStep::default()),
            ],
            output.clone(),
        ),
        preset(
            "Streaming \u{2212}14 LUFS",
            vec![loudness(-14.0)],
            output.clone(),
        ),
        preset(
            "Broadcast \u{2212}23 LUFS (EBU R 128)",
            vec![loudness(-23.0)],
            output.clone(),
        ),
        preset(
            "CD 44.1/16",
            vec![BatchStep::Convert(ConvertStep {
                sample_rate: Some(44_100),
                channels: Channels::Stereo,
            })],
            BatchOutput {
                depth: WavFormat::Pcm16,
                dither: Dither::Tpdf,
                ..output.clone()
            },
        ),
        preset(
            "Trim Silence and Fade",
            vec![
                BatchStep::TrimSilence(TrimStep::default()),
                BatchStep::Fade(FadeStep::default()),
            ],
            output.clone(),
        ),
        preset(
            "FLAC Archive (24-bit)",
            Vec::new(),
            BatchOutput {
                format: FileFormat::Flac,
                ..output
            },
        ),
    ]
}

/// The factory presets and the user's (theirs by name).
pub fn presets() -> Vec<BatchPreset> {
    presets_in(&presets_dir())
}

/// [`presets`] with the user's in `dir`.
pub fn presets_in(dir: &Path) -> Vec<BatchPreset> {
    let mut out = factory_presets();
    out.extend(user_presets(dir));
    out
}

fn user_presets(dir: &Path) -> Vec<BatchPreset> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut out: Vec<BatchPreset> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == "json"))
        .filter_map(|path| match read(&path) {
            Ok(p) => Some(p),
            Err(e) => {
                tracing::warn!("batch preset {}: {e}", path.display());
                None
            }
        })
        .collect();
    out.sort_by_key(|p| p.name.to_lowercase());
    out
}

fn read(path: &Path) -> Result<BatchPreset, String> {
    let text = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
    let file: PresetFile = serde_json::from_str(&text).map_err(|e| e.to_string())?;
    if file.format != FORMAT {
        return Err(format!("not a batch preset ({})", file.format));
    }
    if file.version > VERSION {
        return Err(format!(
            "written by a newer FaderFrame (version {})",
            file.version
        ));
    }
    Ok(BatchPreset {
        name: file.name,
        chain: file.chain,
        factory: false,
        path: Some(path.to_path_buf()),
    })
}

/// The preset of this name (any case).
pub fn preset_named(name: &str) -> Option<BatchPreset> {
    preset_named_in(&presets_dir(), name)
}

/// [`preset_named`] with the user's presets in `dir`.
pub fn preset_named_in(dir: &Path, name: &str) -> Option<BatchPreset> {
    let name = key(name);
    presets_in(dir).into_iter().find(|p| key(&p.name) == name)
}

/// Keep `chain` as the user's preset `name` (replacing theirs of that
/// name). Returns its file.
pub fn save_preset(name: &str, chain: &BatchChain) -> Result<PathBuf, BatchError> {
    save_preset_in(&presets_dir(), name, chain)
}

/// [`save_preset`] into `dir`.
pub fn save_preset_in(dir: &Path, name: &str, chain: &BatchChain) -> Result<PathBuf, BatchError> {
    let name = name.trim();
    if name.is_empty() {
        return Err(BatchError::Preset("a preset needs a name".into()));
    }
    let lower = key(name);
    if factory_presets().iter().any(|p| key(&p.name) == lower) {
        return Err(BatchError::Preset(format!(
            "'{name}' is a factory preset's name: choose another"
        )));
    }
    let io = |path: &Path| {
        let path = path.to_path_buf();
        move |e: std::io::Error| BatchError::Preset(format!("{}: {e}", path.display()))
    };
    std::fs::create_dir_all(dir).map_err(io(dir))?;
    // The file of a preset of that name, else a new one.
    let path = user_presets(dir)
        .into_iter()
        .find(|p| key(&p.name) == lower)
        .and_then(|p| p.path)
        .unwrap_or_else(|| {
            faderframe_audio_files::import::unique_path(
                dir,
                &faderframe_audio_files::import::clean_stem(name),
                "json",
            )
        });
    let text = serde_json::to_string_pretty(&PresetFile {
        format: FORMAT.into(),
        version: VERSION,
        name: name.into(),
        chain: chain.clone(),
    })
    .map_err(|e| BatchError::Preset(e.to_string()))?;
    // Written aside, then moved over: never half a preset.
    let temp = path.with_extension("json.part");
    std::fs::write(&temp, text).map_err(io(&temp))?;
    std::fs::rename(&temp, &path).map_err(io(&path))?;
    Ok(path)
}

/// Delete the user's preset `name`.
pub fn delete_preset(name: &str) -> Result<(), BatchError> {
    delete_preset_in(&presets_dir(), name)
}

/// [`delete_preset`] in `dir`.
pub fn delete_preset_in(dir: &Path, name: &str) -> Result<(), BatchError> {
    let lower = key(name);
    if factory_presets().iter().any(|p| key(&p.name) == lower) {
        return Err(BatchError::Preset(
            "factory presets cannot be deleted".into(),
        ));
    }
    let path = user_presets(dir)
        .into_iter()
        .find(|p| key(&p.name) == lower)
        .and_then(|p| p.path)
        .ok_or_else(|| BatchError::Preset(format!("there is no preset '{}'", name.trim())))?;
    std::fs::remove_file(&path).map_err(|e| BatchError::Preset(format!("{}: {e}", path.display())))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    #[test]
    fn the_factory_presets_can_run() {
        let presets = factory_presets();
        assert!(presets.len() >= 3);
        for p in &presets {
            p.chain.check().unwrap();
            assert!(p.factory);
        }
        // Typed with a hyphen, in another case.
        let none = std::env::temp_dir().join("ff-batch-presets-none");
        let found = preset_named_in(&none, "normalise to -1 dbtp").unwrap();
        assert_eq!(found.name, "Normalise to \u{2212}1 dBTP");
    }
}
