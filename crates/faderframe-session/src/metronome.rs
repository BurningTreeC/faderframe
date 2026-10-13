//! The metronome's sound (Preferences → Metronome): a built-in click or
//! the user's audio files, the volume, and the level of each kind of click
//! — the first beat of a bar, the other beats, and the off-beats (the
//! eighths between beats, off unless asked for). The sounds are made here
//! at the stream's rate and handed to the engine whole; a change of rate
//! makes them again ([`Session::tick_metronome`]).

use crate::Session;
use faderframe_engine::{ClickKind, ClickSound, ClickSounds, MAX_CLICK_SECONDS};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;

/// What the metronome plays.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum MetronomeSound {
    #[default]
    Click,
    Woodblock,
    Stick,
    HiHat,
    Cowbell,
    Beep,
    /// The user's audio files.
    Custom,
}

impl MetronomeSound {
    pub const ALL: [MetronomeSound; 7] = [
        MetronomeSound::Click,
        MetronomeSound::Woodblock,
        MetronomeSound::Stick,
        MetronomeSound::HiHat,
        MetronomeSound::Cowbell,
        MetronomeSound::Beep,
        MetronomeSound::Custom,
    ];

    /// The built-in sound (`None`: the user's files).
    pub fn built_in(self) -> Option<ClickSound> {
        Some(match self {
            Self::Click => ClickSound::Classic,
            Self::Woodblock => ClickSound::Woodblock,
            Self::Stick => ClickSound::Stick,
            Self::HiHat => ClickSound::HiHat,
            Self::Cowbell => ClickSound::Cowbell,
            Self::Beep => ClickSound::Beep,
            Self::Custom => return None,
        })
    }

    pub fn label(self) -> &'static str {
        match self.built_in() {
            Some(s) => s.label(),
            None => "Audio file",
        }
    }

    pub fn id(self) -> &'static str {
        match self.built_in() {
            Some(s) => s.id(),
            None => "custom",
        }
    }

    pub fn from_id(id: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|s| s.id() == id)
    }
}

/// How the metronome sounds.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct MetronomeSettings {
    pub sound: MetronomeSound,
    /// The user's click (any audio file FaderFrame imports, up to
    /// [`MAX_CLICK_SECONDS`]), for [`MetronomeSound::Custom`].
    pub file: Option<PathBuf>,
    /// Another file for the first beat of a bar (`None`: `file` there too).
    pub accent_file: Option<PathBuf>,
    /// The metronome's volume (dB).
    pub volume_db: f32,
    /// The first beat of a bar (dB).
    pub accent_db: f32,
    /// The other beats (dB).
    pub beat_db: f32,
    /// Clicks on the off-beats too, and their level (dB).
    pub off_beats: bool,
    pub off_beat_db: f32,
}

impl Default for MetronomeSettings {
    fn default() -> Self {
        Self {
            sound: MetronomeSound::Click,
            file: None,
            accent_file: None,
            // The levels the metronome always had: 0.35, the beats at 0.7
            // of the accent.
            volume_db: -9.1,
            accent_db: 0.0,
            beat_db: -3.1,
            off_beats: false,
            off_beat_db: -9.0,
        }
    }
}

/// Lowest level the settings offer (dB); anything below is silent.
pub const MIN_LEVEL_DB: f32 = -60.0;

fn gain(db: f32) -> f32 {
    if db <= MIN_LEVEL_DB || !db.is_finite() {
        0.0
    } else {
        10f32.powf(db.min(12.0) / 20.0)
    }
}

/// A file's first two channels as stereo frames at `rate` (a mono file in
/// both), cut at [`MAX_CLICK_SECONDS`].
fn read_click(path: &Path, rate: u32) -> Result<Vec<[f32; 2]>, String> {
    let planes =
        faderframe_audio_files::decode::decode_at_rate(path, rate, &AtomicBool::new(false))
            .map_err(|e| format!("{}: {e}", path.display()))?;
    let Some(left) = planes.first() else {
        return Err(format!("{}: no audio", path.display()));
    };
    let right = planes.get(1).unwrap_or(left);
    let max = (MAX_CLICK_SECONDS * f64::from(rate)) as usize;
    Ok(left
        .iter()
        .zip(right)
        .take(max)
        .map(|(l, r)| [*l, *r])
        .collect())
}

/// The sounds `settings` ask for at `rate` (control thread: files are
/// decoded and resampled here).
pub(crate) fn click_sounds(settings: &MetronomeSettings, rate: u32) -> Result<ClickSounds, String> {
    if let Some(sound) = settings.sound.built_in() {
        return Ok(ClickSounds::synth(sound, rate));
    }
    let Some(file) = &settings.file else {
        return Err("no audio file chosen for the metronome".into());
    };
    let beat = read_click(file, rate)?;
    let accent = match &settings.accent_file {
        Some(f) => read_click(f, rate)?,
        None => beat.clone(),
    };
    Ok(ClickSounds::new(rate, accent, beat.clone(), beat))
}

impl Session {
    pub fn metronome_settings(&self) -> &MetronomeSettings {
        &self.metronome
    }

    /// Change how the metronome sounds. A custom file that cannot be read
    /// leaves the built-in click playing and says why (also the error).
    pub fn set_metronome(&mut self, settings: MetronomeSettings) -> Result<(), String> {
        let sounds_changed = settings.sound != self.metronome.sound
            || settings.file != self.metronome.file
            || settings.accent_file != self.metronome.accent_file;
        self.metronome = settings;
        self.apply_metronome_levels();
        let result = if sounds_changed {
            self.make_click_sounds()
        } else {
            Ok(())
        };
        self.revision += 1;
        result
    }

    /// The volume and levels into the engine.
    pub(crate) fn apply_metronome_levels(&self) {
        let m = &self.metronome;
        let shared = self.engine.metronome();
        shared.set_gain(gain(m.volume_db));
        shared.set_level(ClickKind::Accent, gain(m.accent_db));
        shared.set_level(ClickKind::Beat, gain(m.beat_db));
        shared.set_level(
            ClickKind::OffBeat,
            if m.off_beats {
                gain(m.off_beat_db)
            } else {
                0.0
            },
        );
    }

    /// The rate the clicks are made at: the stream's, else the project's.
    fn click_rate(&self) -> u32 {
        match self.engine.stream_sample_rate() {
            0 => self.project.sample_rate,
            r => r,
        }
    }

    /// Make the sounds for the current rate and hand them over.
    fn make_click_sounds(&mut self) -> Result<(), String> {
        let rate = self.click_rate();
        self.metronome_rate = rate;
        match click_sounds(&self.metronome, rate) {
            Ok(sounds) => {
                self.engine.set_click_sounds(sounds);
                Ok(())
            }
            Err(e) => {
                self.engine
                    .set_click_sounds(ClickSounds::synth(ClickSound::Classic, rate));
                self.notify(
                    crate::NoticeLevel::Warning,
                    format!("Metronome: {e} — playing the built-in click"),
                );
                Err(e)
            }
        }
    }

    /// In the tick: the sounds follow the stream's rate (a new engine, a
    /// device at another rate).
    pub(crate) fn tick_metronome(&mut self) {
        if self.click_rate() != self.metronome_rate {
            let _ = self.make_click_sounds();
        }
    }

    /// A new engine: the metronome as it was.
    pub(crate) fn reapply_metronome(&mut self) {
        self.engine.metronome().set_mode(self.record.metronome);
        self.apply_metronome_levels();
        // Its sounds at its rate, made in the next tick.
        self.metronome_rate = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_wav(path: &Path, rate: u32, channels: Vec<Vec<f32>>) {
        faderframe_audio_files::write_wav(
            path,
            &channels,
            rate,
            faderframe_audio_files::WavFormat::Float32,
            false,
        )
        .unwrap();
    }

    /// The user's files become the clicks at the stream's rate: the accent
    /// from its own file, beats and off-beats from the other; a mono file
    /// in both channels.
    #[test]
    fn custom_files_become_the_clicks_at_the_streams_rate() {
        let dir = std::env::temp_dir().join(format!("ff-click-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (beat, accent) = (dir.join("beat.wav"), dir.join("accent.wav"));
        write_wav(&beat, 44_100, vec![vec![0.5; 4_410]]);
        write_wav(&accent, 48_000, vec![vec![0.25; 960], vec![-0.25; 960]]);
        let mut settings = MetronomeSettings {
            sound: MetronomeSound::Custom,
            file: Some(beat.clone()),
            ..MetronomeSettings::default()
        };
        let s = click_sounds(&settings, 48_000).unwrap();
        let b = s.sound(ClickKind::Beat);
        // 100 ms at 48 kHz (resampled from 44.1 kHz).
        assert!((b.len() as i64 - 4_800).abs() <= 48, "{}", b.len());
        assert!((b[2_400][0] - 0.5).abs() < 1e-3 && b[2_400][0] == b[2_400][1]);
        assert_eq!(s.sound(ClickKind::Accent).len(), b.len(), "the beat's file");
        assert_eq!(s.sound(ClickKind::OffBeat).len(), b.len());
        settings.accent_file = Some(accent);
        let s = click_sounds(&settings, 48_000).unwrap();
        assert_eq!(s.sound(ClickKind::Accent).len(), 960);
        assert_eq!(s.sound(ClickKind::Accent)[10], [0.25, -0.25]);
        // A file that is not there.
        settings.file = Some(dir.join("missing.wav"));
        assert!(click_sounds(&settings, 48_000).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn levels_in_db_and_off() {
        assert_eq!(gain(0.0), 1.0);
        assert!((gain(-6.0) - 0.501).abs() < 1e-3);
        assert_eq!(gain(MIN_LEVEL_DB), 0.0);
        let d = MetronomeSettings::default();
        // The levels it always had.
        assert!((gain(d.volume_db) - 0.35).abs() < 0.005);
        assert!((gain(d.beat_db) - 0.7).abs() < 0.005);
    }
}
