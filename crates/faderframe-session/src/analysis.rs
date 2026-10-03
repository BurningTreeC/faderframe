//! The Tools view's meters: the engine copies the analysed track's
//! post-fader output (the master unless another is chosen) into a scope
//! ring; each tick feeds what arrived to the [`Analyzer`].

use crate::Session;
use faderframe_analysis::{Analyzer, Level};
use faderframe_core::TrackId;

/// Loudness targets offered by the Tools view: (LUFS, label).
pub const LOUDNESS_TARGETS: [(f32, &str); 5] = [
    (-14.0, "−14 LUFS · Spotify, YouTube, Tidal"),
    (-16.0, "−16 LUFS · Apple Music"),
    (-23.0, "−23 LUFS · EBU R128 broadcast"),
    (-24.0, "−24 LUFS · ATSC A/85"),
    (-9.0, "−9 LUFS · loud club master"),
];

/// How the level meter is scaled.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum LevelScale {
    /// dBFS.
    #[default]
    Digital,
    /// K-System (Bob Katz): 0 on the scale is −`n` dBFS RMS.
    K(u8),
}

impl LevelScale {
    pub const ALL: [LevelScale; 4] = [
        LevelScale::Digital,
        LevelScale::K(12),
        LevelScale::K(14),
        LevelScale::K(20),
    ];

    pub fn label(self) -> String {
        match self {
            LevelScale::Digital => "dBFS".into(),
            LevelScale::K(n) => format!("K-{n}"),
        }
    }

    /// Where 0 on the scale lies in dBFS.
    pub fn zero_db(self) -> f32 {
        match self {
            LevelScale::Digital => 0.0,
            LevelScale::K(n) => -(n as f32),
        }
    }
}

/// Tools view settings (per session).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct AnalysisSettings {
    pub target_lufs: f32,
    pub scale: LevelScale,
    /// Starting playback starts a new measurement.
    pub reset_on_play: bool,
}

impl Default for AnalysisSettings {
    fn default() -> Self {
        Self {
            target_lufs: -14.0,
            scale: LevelScale::Digital,
            reset_on_play: true,
        }
    }
}

pub(crate) struct AnalysisState {
    pub analyzer: Analyzer,
    /// Chosen source (`None`: the master).
    pub source: Option<TrackId>,
    pub settings: AnalysisSettings,
    /// Scope frames consumed so far.
    pos: u64,
    left: Vec<f32>,
    right: Vec<f32>,
    pub levels: [Level; 2],
    was_playing: bool,
}

impl AnalysisState {
    pub fn new(sample_rate: u32) -> Self {
        Self {
            analyzer: Analyzer::new(sample_rate),
            source: None,
            settings: AnalysisSettings::default(),
            pos: 0,
            left: Vec::new(),
            right: Vec::new(),
            levels: [Level {
                peak: -200.0,
                rms: -200.0,
                hold: -200.0,
            }; 2],
            was_playing: false,
        }
    }
}

impl Session {
    /// The meters of the Tools view.
    pub fn analyzer(&self) -> &Analyzer {
        &self.analysis.analyzer
    }

    /// Current peak/RMS levels (left, right).
    pub fn analysis_levels(&self) -> [Level; 2] {
        self.analysis.levels
    }

    pub fn analysis_settings(&self) -> AnalysisSettings {
        self.analysis.settings
    }

    /// The analysed track.
    pub fn analysis_source(&self) -> Option<TrackId> {
        self.analysis
            .source
            .filter(|t| self.project.track(*t).is_some_and(|t| t.kind.has_audio()))
            .or_else(|| self.project.master_id())
    }

    pub(crate) fn set_analysis_source(&mut self, track: Option<TrackId>) {
        self.analysis.source = track;
        self.analysis.analyzer.reset();
        self.apply_analysis_source();
    }

    pub(crate) fn update_analysis_settings(&mut self, change: impl FnOnce(&mut AnalysisSettings)) {
        change(&mut self.analysis.settings);
        self.revision += 1;
    }

    pub(crate) fn reset_analysis(&mut self) {
        self.analysis.analyzer.reset();
        self.revision += 1;
    }

    /// Point the engine's scope at the analysed track (after a new engine
    /// or a source change); the new ring starts from scratch.
    pub(crate) fn apply_analysis_source(&mut self) {
        let source = self.analysis_source();
        self.engine.set_analysis_source(source);
        self.analysis.pos = self.engine.scope().written();
    }

    /// Feed the audio that arrived since the last tick (from `tick`).
    pub(crate) fn poll_analysis(&mut self, dt: f32) {
        let rate = self.engine.sample_rate();
        let a = &mut self.analysis;
        if a.analyzer.sample_rate() != rate {
            a.analyzer = Analyzer::new(rate);
        }
        let playing = self.transport.playing;
        if playing && !a.was_playing && a.settings.reset_on_play {
            a.analyzer.reset();
        }
        a.was_playing = playing;
        a.left.clear();
        a.right.clear();
        let (pos, _lost) = self
            .engine
            .scope()
            .read_since(a.pos, &mut a.left, &mut a.right);
        a.pos = pos;
        a.analyzer.process(&a.left, &a.right, playing);
        a.analyzer.spectrum.decay_peaks(dt * 12.0);
        a.levels = a.analyzer.level.read(dt);
    }
}
