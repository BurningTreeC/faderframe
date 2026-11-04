//! The codec monitor: the master heard as listeners get it — through a
//! lossy codec (Opus at a bitrate) and at a streaming platform's loudness
//! — with the original and the difference (the codec's artefacts alone) a
//! switch away, and "Preview on platforms", a report of what each platform
//! does to a rendered master.
//!
//! Listening only (`faderframe_engine::nodes::codec`, in the listening
//! node after the compare source and before headphones and the mono
//! check): renders, exports, bounces and meters never hear it.
//!
//! **The platform's gain** comes from the song's integrated loudness and
//! true peak (a platform turning quiet masters up stops where the peaks
//! reach its ceiling). The song is measured by a render of the master over
//! the whole project (two seconds of tail, loop off — the export's range),
//! on a thread of its own with two DSP helpers, started once the project
//! has stood still for [`SETTLE`] while a platform is chosen, and kept by
//! the mix fingerprint (`crate::mix::fingerprint`: everything that changes
//! what is heard), so going back to an earlier state reuses it. While a
//! change is measured the gain of the state before it stays (no jump
//! mid-edit). Until the first render of a project is done, the Tools view's
//! running measurement of the master (what played since its last reset)
//! stands in, and says so; with neither the gain is 0 dB.
//!
//! **Delay.** The monitor delays what is heard by the codec's frame and
//! lookahead (26.5 ms at 48 kHz; the resamplers add a little at other
//! rates). The graph's latency stays as it is; the heard clock counts the
//! delay (`EngineController::listening_delay`), so the playhead, the
//! meters and the analysers stay with the ears. Recording does not make
//! up for it (performers may listen on cue mixes, which never pass it): a
//! recording started with the monitor on says so.

use crate::render::{self, RenderChannels, RenderProgress, RenderRange, RenderSettings};
use crate::{NoticeLevel, Result, Session, SessionError};
use faderframe_codec::{CoderKind, Master, PLATFORMS, Platform, PlatformPreview, Source, platform};
use faderframe_core::ChannelLayout;
use faderframe_engine::nodes::{CodecLink, CodecWanted};
use faderframe_project::Project;
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

pub use faderframe_engine::nodes::CodecHear;

/// How long the project stands still before the song is measured again.
pub const SETTLE: Duration = Duration::from_millis(1500);
/// Song measurements kept (by fingerprint).
const KEPT: usize = 8;
/// The span of the short-term artefact level.
const SHORT_TERM: Duration = Duration::from_secs(3);

/// What the codec monitor does (remembered between runs, all but whether
/// it is on).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct CodecMonitorSettings {
    #[serde(with = "hear_id")]
    pub hear: CodecHear,
    /// Kilobits a second for a stereo pair (the platform's when one is
    /// chosen).
    pub kbps: u32,
    pub vbr: bool,
    /// A platform's loudness and codec (its id), else the plain codec.
    pub platform: Option<String>,
    /// The platforms "Preview on platforms" reports on (ids; empty: all).
    pub preview: Vec<String>,
}

impl Default for CodecMonitorSettings {
    fn default() -> Self {
        Self {
            hear: CodecHear::Codec,
            kbps: 128,
            vbr: true,
            platform: None,
            preview: Vec::new(),
        }
    }
}

impl CodecMonitorSettings {
    /// The platform chosen (an unknown id is none).
    pub fn platform(&self) -> Option<&'static Platform> {
        self.platform.as_deref().and_then(platform)
    }

    /// The bitrate and VBR coded at: the platform's, else the settings'.
    pub fn codec(&self) -> (u32, bool) {
        match self.platform() {
            Some(p) => (p.kbps, p.vbr),
            None => (faderframe_codec::clamp_kbps(self.kbps), self.vbr),
        }
    }

    /// The platforms a preview reports on.
    pub fn preview_platforms(&self) -> Vec<&'static Platform> {
        let chosen: Vec<&'static Platform> =
            self.preview.iter().filter_map(|id| platform(id)).collect();
        if chosen.is_empty() {
            PLATFORMS.iter().collect()
        } else {
            chosen
        }
    }
}

mod hear_id {
    use super::CodecHear;
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(h: &CodecHear, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(h.id())
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<CodecHear, D::Error> {
        let id = String::deserialize(d)?;
        Ok(CodecHear::from_id(&id).unwrap_or_default())
    }
}

/// Changing the codec monitor.
#[derive(Clone, Debug, PartialEq)]
pub enum CodecOp {
    /// The mix as it is.
    Off,
    /// On with the settings as they are, or off.
    Toggle,
    /// Opus at a bitrate, no loudness normalisation.
    Opus {
        kbps: u32,
        vbr: bool,
    },
    /// A platform's loudness normalisation and codec.
    Platform(String),
    /// What is heard: the decoded signal, the original, the difference.
    Hear(CodecHear),
    /// Variable or constant bitrate (the plain codec).
    Vbr(bool),
    /// Start the measurements anew.
    ResetMeters,
    /// Measure the song's loudness for the platform's gain now.
    Measure,
    /// The settings (restored from the preferences: whether it is on is
    /// kept).
    Settings(CodecMonitorSettings),
    /// Render the master as `settings` say (the render window's range,
    /// rate, channels and finish) and report what each of `platforms`
    /// (ids; empty: all) does to it.
    Preview {
        settings: Box<RenderSettings>,
        platforms: Vec<String>,
    },
    CancelPreview,
}

/// The song's loudness as a render measured it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SongLoudness {
    /// Integrated loudness (LUFS).
    pub integrated: f64,
    /// Highest true peak (dBTP).
    pub true_peak: f64,
}

/// Where the platform's gain comes from.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum GainSource {
    /// No platform chosen: no normalisation.
    None,
    /// The song as rendered.
    Song(SongLoudness),
    /// The Tools view's measurement of what played (the song not measured
    /// yet).
    Playback(SongLoudness),
    /// Nothing measured yet (0 dB).
    Unmeasured,
}

/// What the views show of the codec monitor.
#[derive(Clone, Debug, PartialEq)]
pub struct CodecStatus {
    pub on: bool,
    pub hear: CodecHear,
    pub kbps: u32,
    pub vbr: bool,
    pub platform: Option<&'static Platform>,
    /// "Opus 128 kb/s VBR", "Spotify · Ogg Vorbis 320 kb/s (emulated: …)".
    pub title: String,
    /// The platform's gain (dB) and where it comes from, in words.
    pub gain_db: f64,
    pub gain: GainSource,
    pub gain_text: String,
    /// What the monitor adds to what is heard now (ms).
    pub delay_ms: f64,
    /// How much of the monitor is heard (0 … 1).
    pub heard: f32,
    /// The listening node runs a chain.
    pub ready: bool,
    /// Since the measurements were reset: the highest true peak after
    /// decoding at the platform's gain (dBTP), the frames over full scale,
    /// the level of what the codec removed or added against the original
    /// (dB), and over the last seconds.
    pub true_peak: Option<f64>,
    pub overs: u64,
    pub artefacts_db: Option<f64>,
    pub artefacts_now_db: Option<f64>,
    /// The song being measured (0 … 1).
    pub measuring: Option<f64>,
    pub error: Option<String>,
}

/// A finished preview.
#[derive(Clone, Debug, PartialEq)]
pub struct PreviewReport {
    /// What was rendered ("the project, 48 kHz, stereo, −14 LUFS finish").
    pub rendered: String,
    pub master: Master,
    pub platforms: Vec<PlatformPreview>,
}

/// A preview, as the views show it.
#[derive(Clone, Debug, PartialEq)]
pub struct PreviewStatus {
    /// Running (0 … 1), else done.
    pub progress: Option<f64>,
    /// What it is doing now.
    pub stage: &'static str,
    pub result: Option<std::result::Result<PreviewReport, String>>,
}

struct MeasureJob {
    fingerprint: u64,
    progress: Arc<RenderProgress>,
    handle: JoinHandle<std::result::Result<SongLoudness, String>>,
}

struct PreviewJob {
    render: Arc<RenderProgress>,
    done: Arc<AtomicU64>,
    total: Arc<AtomicU64>,
    stop: Arc<AtomicBool>,
    handle: JoinHandle<std::result::Result<PreviewReport, String>>,
}

#[derive(Default)]
pub(crate) struct CodecState {
    settings: CodecMonitorSettings,
    on: bool,
    /// The link last handed to (an engine made anew gets it again) and what.
    sent: Option<(Arc<CodecLink>, Option<CodecWanted>)>,
    /// Song measurements by fingerprint, newest last.
    measured: Vec<(u64, SongLoudness)>,
    measure: Option<MeasureJob>,
    /// The project's content as last seen ((serial, history revision))
    /// and when it changed.
    content: (u64, u64),
    changed: Option<Instant>,
    /// The measurement the gain comes from: the project as it stands, or
    /// as it stood until the change being measured now (the gain does not
    /// jump while a new render runs). None for another project.
    song: Option<(u64, SongLoudness)>,
    /// The fingerprint last asked for (a render finishing for an older one
    /// only fills the cache).
    wanted: Option<u64>,
    /// A fingerprint whose measurement failed (not tried again).
    failed: Option<u64>,
    preview: Option<PreviewJob>,
    preview_result: Option<std::result::Result<PreviewReport, String>>,
    was_playing: bool,
    warned: bool,
    /// Readings for the short-term artefact level.
    history: VecDeque<(Instant, f64, f64, u64)>,
    error: Option<String>,
}

impl Drop for CodecState {
    fn drop(&mut self) {
        if let Some(p) = self.preview.take() {
            p.stop.store(true, Ordering::Relaxed);
            p.render.cancel.store(true, Ordering::Relaxed);
            let _ = p.handle.join();
        }
        if let Some(m) = self.measure.take() {
            m.progress.cancel.store(true, Ordering::Relaxed);
            let _ = m.handle.join();
        }
    }
}

/// A rendered file read back as the preview's source.
struct WavSource(faderframe_audio_files::wavstream::WavFile);

impl Source for WavSource {
    fn channels(&self) -> usize {
        self.0.channels()
    }

    fn rate(&self) -> u32 {
        self.0.sample_rate()
    }

    fn frames(&self) -> u64 {
        self.0.frames()
    }

    fn read(&self, start: u64, out: &mut [&mut [f32]]) -> std::result::Result<(), String> {
        self.0
            .read(start, out, &mut Vec::new())
            .map_err(|e| e.to_string())
    }
}

fn signed_db(v: f64) -> String {
    if v.is_finite() {
        format!("{v:+.1}").replace('-', "−")
    } else {
        "−∞".into()
    }
}

/// The song measured: a render of the master over the export's range
/// (loop off, two seconds of tail) with two DSP helpers, its integrated
/// loudness (the layout's BS.1770 weights) and true peak.
fn measure_song(
    mut project: Project,
    rate: u32,
    progress: &RenderProgress,
) -> std::result::Result<SongLoudness, String> {
    project.loop_enabled = false;
    let (a, b) =
        render::resolve_range(&project, RenderRange::Project).map_err(|e| e.to_string())?;
    let sr = f64::from(rate);
    let start = project.timeline.to_samples(a, sr);
    let end = project.timeline.to_samples(b, sr) + (2.0 * sr) as i64;
    let frames = (end - start).max(0) as usize;
    progress.total.store(frames as u64, Ordering::Relaxed);
    let mut sources = faderframe_engine::render_generated_sources(&project, rate);
    for (_, path, e) in crate::media::open_file_sources(&project, None, &mut sources) {
        tracing::warn!("codec monitor: {}: {e}", path.display());
    }
    let layout = crate::compare::master_layout(&project);
    let channels = layout.channel_count().max(1);
    let mut loudness =
        faderframe_analysis::WeightedLoudness::new(rate, &crate::compare::loudness_weights(layout));
    let mut tp = vec![faderframe_analysis::TruePeak::new(); channels];
    let mut peak = 0.0f64;
    render::render_each_on(
        &project,
        rate,
        &sources,
        (start, frames),
        progress,
        channels,
        2,
        &mut |piece| {
            loudness.add(piece);
            for (t, c) in tp.iter_mut().zip(piece) {
                for &x in *c {
                    peak = peak.max(t.run(x));
                }
            }
        },
    )
    .map_err(|e| e.to_string())?;
    Ok(SongLoudness {
        integrated: loudness.integrated(),
        true_peak: faderframe_codec::meter::db(peak),
    })
}

/// The layout a render's channels come out in.
fn rendered_layout(project: &Project, channels: RenderChannels) -> ChannelLayout {
    match channels {
        RenderChannels::Stereo | RenderChannels::Binaural(_) => ChannelLayout::Stereo,
        RenderChannels::Mono | RenderChannels::First => ChannelLayout::Mono,
        _ => crate::compare::master_layout(project),
    }
}

impl Session {
    // --- what the views read ------------------------------------------------

    pub fn codec_settings(&self) -> &CodecMonitorSettings {
        &self.codec.settings
    }

    pub fn codec_monitor_on(&self) -> bool {
        self.codec.on
    }

    /// The codec monitor's controls (what is heard, the gain, the
    /// measurements as the listening node publishes them).
    pub fn codec_link(&self) -> &Arc<CodecLink> {
        self.engine.listening().link.codec()
    }

    /// The song's loudness for the platform's gain and where it comes
    /// from.
    fn codec_gain(&self) -> (f64, GainSource) {
        let Some(p) = self.codec.settings.platform() else {
            return (0.0, GainSource::None);
        };
        if let Some((_, l)) = self.codec.song {
            return (p.gain(l.integrated, l.true_peak).db, GainSource::Song(l));
        }
        // What played, as the Tools view measured it on the master.
        if self.analysis_source().is_none() {
            let l = self.analyzer().loudness.read();
            if l.integrated.is_finite() {
                let l = SongLoudness {
                    integrated: l.integrated,
                    true_peak: l.true_peak,
                };
                return (
                    p.gain(l.integrated, l.true_peak).db,
                    GainSource::Playback(l),
                );
            }
        }
        (0.0, GainSource::Unmeasured)
    }

    /// What the views show of the codec monitor.
    pub fn codec_status(&self) -> CodecStatus {
        let s = &self.codec.settings;
        let shared = self.codec_link().shared();
        let (kbps, vbr) = s.codec();
        let platform = s.platform();
        let mode = if vbr { "VBR" } else { "CBR" };
        let title = match platform {
            Some(p) => format!("{} · {}", p.name, p.codec_text()),
            None => format!("Opus {kbps} kb/s {mode}"),
        };
        let (gain_db, gain) = self.codec_gain();
        let measuring = self.codec.measure.as_ref().map(|m| m.progress.fraction());
        let gain_text = match (gain, platform) {
            (GainSource::None, _) | (_, None) => "No loudness normalisation".into(),
            (GainSource::Song(l), Some(p)) => {
                let g = p.gain(l.integrated, l.true_peak);
                let limited = if g.peak_limited {
                    format!(" of {} (stopped by the peaks)", signed_db(g.wanted_db))
                } else {
                    String::new()
                };
                let again = if measuring.is_some() {
                    " (measuring the change…)"
                } else {
                    ""
                };
                format!(
                    "{} dB{limited} · the song measures {} LUFS, {} dBTP{again}",
                    signed_db(gain_db),
                    signed_db(l.integrated).trim_start_matches('+'),
                    signed_db(l.true_peak)
                )
            }
            (GainSource::Playback(l), Some(_)) => format!(
                "{} dB · from what played ({} LUFS); measuring the song…",
                signed_db(gain_db),
                signed_db(l.integrated).trim_start_matches('+')
            ),
            (GainSource::Unmeasured, Some(_)) => {
                if measuring.is_some() {
                    "0 dB until the song is measured (measuring…)".into()
                } else {
                    "0 dB until the song is measured".into()
                }
            }
        };
        let reading = shared.reading();
        let measured = reading.frames > 0;
        // Over the last seconds: the readings' growth.
        let artefacts_now_db = self
            .codec
            .history
            .front()
            .zip(self.codec.history.back())
            .and_then(|(a, b)| {
                (b.3 > a.3)
                    .then(|| faderframe_codec::meter::relative_db(b.1 - a.1, b.2 - a.2))
                    .flatten()
            });
        let rate = f64::from(self.engine.sample_rate().max(1));
        CodecStatus {
            on: self.codec.on,
            hear: s.hear,
            kbps,
            vbr,
            platform,
            title,
            gain_db,
            gain,
            gain_text,
            delay_ms: f64::from(shared.delay()) * 1000.0 / rate,
            heard: shared.heard(),
            ready: shared.installed(),
            true_peak: measured.then(|| reading.peak_dbtp()),
            overs: reading.overs,
            artefacts_db: if measured {
                reading.difference_db()
            } else {
                None
            },
            artefacts_now_db,
            measuring,
            error: self.codec.error.clone(),
        }
    }

    /// The latest "Preview on platforms".
    pub fn platform_preview(&self) -> Option<PreviewStatus> {
        if let Some(p) = &self.codec.preview {
            let total = p.total.load(Ordering::Relaxed);
            let rendering = !p.render.measuring.load(Ordering::Relaxed) && total == 0;
            let progress = if total == 0 {
                0.5 * p.render.fraction()
            } else {
                0.5 + 0.5 * (p.done.load(Ordering::Relaxed) as f64 / total as f64).min(1.0)
            };
            return Some(PreviewStatus {
                progress: Some(progress),
                stage: if rendering {
                    "Rendering the master…"
                } else {
                    "Coding it as each platform streams it…"
                },
                result: None,
            });
        }
        self.codec.preview_result.as_ref().map(|r| PreviewStatus {
            progress: None,
            stage: "",
            result: Some(r.clone()),
        })
    }

    // --- actions --------------------------------------------------------------

    pub(crate) fn codec_op(&mut self, op: CodecOp) -> Result<()> {
        match op {
            CodecOp::Off => self.codec.on = false,
            CodecOp::Toggle => self.codec.on = !self.codec.on,
            CodecOp::Opus { kbps, vbr } => {
                self.codec.settings.kbps = faderframe_codec::clamp_kbps(kbps);
                self.codec.settings.vbr = vbr;
                self.codec.settings.platform = None;
                self.codec.on = true;
            }
            CodecOp::Platform(id) => {
                let p = platform(&id)
                    .ok_or_else(|| SessionError::Other(format!("no streaming platform ‘{id}’")))?;
                self.codec.settings.platform = Some(p.id.to_string());
                self.codec.on = true;
                // Measured at the next tick when the project stands still,
                // else once it does.
                let now = Instant::now();
                self.codec.changed = Some(
                    self.codec
                        .changed
                        .unwrap_or(now.checked_sub(SETTLE).unwrap_or(now)),
                );
            }
            CodecOp::Hear(h) => {
                self.codec.settings.hear = h;
                self.codec.on = true;
            }
            CodecOp::Vbr(on) => {
                self.codec.settings.vbr = on;
                if let Some(p) = self.codec.settings.platform() {
                    // Leaving the platform's own setting: the plain codec at
                    // its bitrate.
                    self.codec.settings.kbps = p.kbps;
                    self.codec.settings.platform = None;
                }
            }
            CodecOp::ResetMeters => {
                self.codec_link().shared().reset_measurements();
                self.codec.history.clear();
            }
            CodecOp::Measure => {
                if self.codec.settings.platform().is_none() {
                    return Err(SessionError::Other(
                        "choose a streaming platform first: its loudness needs the song's".into(),
                    ));
                }
                self.codec.failed = None;
                self.start_song_measurement(true)?;
            }
            CodecOp::Settings(s) => self.codec.settings = s,
            CodecOp::Preview {
                settings,
                platforms,
            } => self.start_preview(*settings, platforms)?,
            CodecOp::CancelPreview => {
                if let Some(p) = &self.codec.preview {
                    p.stop.store(true, Ordering::Relaxed);
                    p.render.cancel.store(true, Ordering::Relaxed);
                }
            }
        }
        self.send_codec()?;
        self.revision += 1;
        Ok(())
    }

    /// Hand the engine what is wanted (again to an engine made anew).
    fn send_codec(&mut self) -> Result<()> {
        let link = Arc::clone(self.codec_link());
        let (gain_db, source) = self.codec_gain();
        let wanted = self.codec.on.then(|| {
            let (kbps, vbr) = self.codec.settings.codec();
            let kind = if faderframe_codec::opus_available() {
                CoderKind::Opus
            } else {
                CoderKind::Identity
            };
            CodecWanted {
                settings: faderframe_codec::CodecSettings { kind, kbps, vbr },
                hear: self.codec.settings.hear,
                // In tenths of a decibel: a running estimate's wobble is
                // not handed on every tick.
                gain_db: (gain_db * 10.0).round() as f32 / 10.0,
            }
        });
        let before = match &self.codec.sent {
            Some((l, w)) if Arc::ptr_eq(l, &link) => {
                if *w == wanted {
                    return Ok(());
                }
                *w
            }
            _ => None,
        };
        // A measured gain arrived or changed: new measurements (they count
        // at the gain heard).
        if let (Some(b), Some(w)) = (before, wanted)
            && matches!(source, GainSource::Song(_))
            && (b.gain_db - w.gain_db).abs() > 0.05
        {
            link.shared().reset_measurements();
            self.codec.history.clear();
        }
        let result = link.set(wanted);
        self.codec.sent = Some((link, wanted));
        match result {
            Ok(()) => {
                self.codec.error = None;
                Ok(())
            }
            Err(e) => {
                let text = format!("codec monitor: {e}");
                self.codec.error = Some(text.clone());
                self.codec.on = false;
                let link = Arc::clone(self.codec_link());
                if link.set(None).is_ok() {
                    self.codec.sent = Some((link, None));
                }
                Err(SessionError::Other(text))
            }
        }
    }

    /// Measure the song now for the fingerprint as it stands (`force`: even
    /// with one measured).
    fn start_song_measurement(&mut self, force: bool) -> Result<()> {
        let project = self.absolute_copy();
        let Some(fp) = crate::mix::fingerprint(&project) else {
            return Err(SessionError::Other(
                "the project cannot be measured (it does not serialise)".into(),
            ));
        };
        self.codec.wanted = Some(fp);
        if !force {
            // Measured before (an undo back to it): at once.
            if let Some(&(_, l)) = self.codec.measured.iter().rev().find(|(f, _)| *f == fp) {
                self.codec.song = Some((fp, l));
                return Ok(());
            }
            if self.codec.failed == Some(fp) {
                return Ok(());
            }
        }
        if let Some(m) = &self.codec.measure {
            if m.fingerprint == fp {
                return Ok(());
            }
            m.progress.cancel.store(true, Ordering::Relaxed);
        }
        let rate = self.engine.sample_rate();
        let progress = Arc::new(RenderProgress::default());
        let p = Arc::clone(&progress);
        let handle = std::thread::Builder::new()
            .name("faderframe-codec-measure".into())
            .spawn(move || measure_song(project, rate, &p))
            .map_err(|e| SessionError::Other(format!("cannot start the measurement: {e}")))?;
        // The one before ends on its own (cancelled); it is not waited for.
        if let Some(old) = self.codec.measure.replace(MeasureJob {
            fingerprint: fp,
            progress,
            handle,
        }) {
            std::thread::spawn(move || drop(old.handle.join()));
        }
        Ok(())
    }

    fn start_preview(&mut self, settings: RenderSettings, platforms: Vec<String>) -> Result<()> {
        if let Some(p) = self.codec.preview.take() {
            p.stop.store(true, Ordering::Relaxed);
            p.render.cancel.store(true, Ordering::Relaxed);
            std::thread::spawn(move || drop(p.handle.join()));
        }
        let chosen: Vec<&'static Platform> = if platforms.is_empty() {
            self.codec.settings.preview_platforms()
        } else {
            platforms.iter().filter_map(|id| platform(id)).collect()
        };
        if chosen.is_empty() {
            return Err(SessionError::Other(
                "no streaming platform to preview".into(),
            ));
        }
        if !platforms.is_empty() {
            self.codec.settings.preview = chosen.iter().map(|p| p.id.to_string()).collect();
        }
        let project = self.absolute_copy();
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let n = NEXT.fetch_add(1, Ordering::Relaxed);
        let output = std::env::temp_dir().join(format!(
            "faderframe-platforms-{}-{n}.wav",
            std::process::id()
        ));
        let channels = match settings.channels {
            // A file the preview can read back: the bed for object and
            // IAMF masters.
            RenderChannels::Adm(_) | RenderChannels::Iamf(_) => RenderChannels::Master,
            other => other,
        };
        let settings = RenderSettings {
            source: render::RenderSource::Master,
            channels,
            format: faderframe_audio_files::WavFormat::Float32,
            dither: faderframe_audio_files::Dither::Off,
            report: false,
            overwrite: render::Overwrite::Always,
            keep_latency: false,
            head: settings.head.or_else(|| Some(self.head().clone())),
            output: output.clone(),
            ..settings
        };
        let layout = rendered_layout(&project, channels);
        let rendered = describe_render(&project, &settings);
        let job =
            render::start(project, settings).map_err(|e| SessionError::Other(e.to_string()))?;
        let render_progress = Arc::clone(&job.progress);
        let done = Arc::new(AtomicU64::new(0));
        let total = Arc::new(AtomicU64::new(0));
        let stop = Arc::new(AtomicBool::new(false));
        let (d, t, s) = (Arc::clone(&done), Arc::clone(&total), Arc::clone(&stop));
        let handle = std::thread::Builder::new()
            .name("faderframe-platforms".into())
            .spawn(move || {
                let result = run_preview(job, &output, layout, &chosen, &d, &t, &s, rendered);
                let _ = std::fs::remove_file(&output);
                result
            })
            .map_err(|e| SessionError::Other(format!("cannot start the preview: {e}")))?;
        self.codec.preview = Some(PreviewJob {
            render: render_progress,
            done,
            total,
            stop,
            handle,
        });
        self.codec.preview_result = None;
        Ok(())
    }

    /// The tick: an engine made anew, the gain, the song's measurement, the
    /// preview, resets on play, the short-term level.
    pub(crate) fn tick_codec(&mut self) {
        let shared = Arc::clone(self.codec_link().shared());
        // The project's content moved: measured again once it stands still.
        let content = (self.project_serial, self.history.revision());
        if content != self.codec.content {
            if content.0 != self.codec.content.0 {
                // Another project: another song.
                self.codec.song = None;
            }
            self.codec.content = content;
            self.codec.changed = Some(Instant::now());
        }
        if self.codec.on
            && self.codec.settings.platform().is_some()
            && self.codec.changed.is_some_and(|t| t.elapsed() >= SETTLE)
        {
            self.codec.changed = None;
            if let Err(e) = self.start_song_measurement(false) {
                tracing::warn!("codec monitor: {e}");
            }
        }
        // The song measured.
        if self
            .codec
            .measure
            .as_ref()
            .is_some_and(|m| m.handle.is_finished())
            && let Some(m) = self.codec.measure.take()
        {
            match m.handle.join() {
                Ok(Ok(l)) => {
                    self.codec.measured.retain(|(f, _)| *f != m.fingerprint);
                    self.codec.measured.push((m.fingerprint, l));
                    if self.codec.measured.len() > KEPT {
                        self.codec.measured.remove(0);
                    }
                    if self.codec.wanted == Some(m.fingerprint) {
                        self.codec.song = Some((m.fingerprint, l));
                    }
                }
                Ok(Err(e)) if e != render::RenderError::Cancelled.to_string() => {
                    self.codec.failed = Some(m.fingerprint);
                    self.notify(
                        NoticeLevel::Warning,
                        format!("codec monitor: the song could not be measured: {e}"),
                    );
                }
                _ => {}
            }
            self.revision += 1;
        } else if self.codec.measure.is_some() {
            // The progress moves.
            self.shown_changed();
        }
        // The preview done.
        if self
            .codec
            .preview
            .as_ref()
            .is_some_and(|p| p.handle.is_finished())
            && let Some(p) = self.codec.preview.take()
        {
            let result = p
                .handle
                .join()
                .unwrap_or_else(|_| Err("the preview failed".into()));
            match &result {
                Ok(_) => self.notify(NoticeLevel::Info, "platform preview ready"),
                Err(e) => self.notify(NoticeLevel::Warning, format!("platform preview: {e}")),
            }
            self.codec.preview_result = Some(result);
            self.revision += 1;
        } else if self.codec.preview.is_some() {
            self.shown_changed();
        }
        // New measurements when playback starts (as the Tools view does).
        let playing = self.transport.playing;
        if playing && !self.codec.was_playing && self.analysis_settings().reset_on_play {
            shared.reset_measurements();
            self.codec.history.clear();
        }
        self.codec.was_playing = playing;
        // Recording: the delay is not made up for.
        if self.codec.on && self.recording.is_some() && !self.codec.warned {
            self.codec.warned = true;
            let ms =
                f64::from(shared.delay()) * 1000.0 / f64::from(self.engine.sample_rate().max(1));
            self.notify(
                NoticeLevel::Warning,
                format!(
                    "The codec monitor delays what you hear by {ms:.0} ms and recording does not \
                     make up for it: turn it off to play along"
                ),
            );
        }
        if !self.recording.is_some() {
            self.codec.warned = false;
        }
        // The short-term level of the artefacts.
        if self.codec.on {
            let r = shared.reading();
            if self.codec.history.back().is_some_and(|b| r.frames < b.3) {
                self.codec.history.clear();
            }
            let now = Instant::now();
            self.codec
                .history
                .push_back((now, r.difference, r.original, r.frames));
            while self
                .codec
                .history
                .front()
                .is_some_and(|f| now.duration_since(f.0) > SHORT_TERM)
            {
                self.codec.history.pop_front();
            }
            self.shown_changed();
        }
        if let Err(e) = self.send_codec() {
            tracing::warn!("{e}");
        }
        if let Err(e) = self.codec_link().poll() {
            tracing::warn!("codec monitor: {e}");
        }
    }

    /// Wait for the song's measurement and the preview (tests, scripts).
    pub fn wait_for_codec(&mut self, timeout: Duration) -> bool {
        let until = Instant::now() + timeout;
        loop {
            // Settled at once: no need to wait for the project to stand
            // still.
            if self.codec.on
                && self.codec.settings.platform().is_some()
                && self.codec.changed.is_some()
            {
                let now = Instant::now();
                self.codec.changed = Some(now.checked_sub(SETTLE).unwrap_or(now));
            }
            self.tick_codec();
            if self.codec.measure.is_none()
                && self.codec.preview.is_none()
                && self.codec.changed.is_none()
            {
                return true;
            }
            if Instant::now() > until {
                return false;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
    }
}

/// "the project, 48 kHz, stereo, finished to −14 LUFS / −1 dBTP".
fn describe_render(project: &Project, settings: &RenderSettings) -> String {
    let range = match settings.range {
        RenderRange::Project => "the project".to_string(),
        RenderRange::Loop => "the loop".to_string(),
        RenderRange::Bars { start, end } => format!("bars {}–{}", start + 1, end),
        RenderRange::Span { .. } => "the selection".to_string(),
    };
    let channels = match settings.channels {
        RenderChannels::Stereo => "stereo".to_string(),
        RenderChannels::Mono | RenderChannels::First => "mono".to_string(),
        RenderChannels::Binaural(_) => "binaural".to_string(),
        _ => match crate::compare::master_layout(project) {
            ChannelLayout::Surround(f) => f.name().to_string(),
            l => format!("{} channels", l.channel_count()),
        },
    };
    let rate = faderframe_audio::format_sample_rate(settings.sample_rate);
    let f = &settings.finish;
    let finish = match (f.loudness, f.ceiling) {
        (Some(l), Some(c)) => format!("finished to {l:.1} LUFS under {c:.1} dBTP"),
        (Some(l), None) => format!("finished to {l:.1} LUFS"),
        (None, Some(c)) => format!("limited to {c:.1} dBTP"),
        (None, None) => String::new(),
    }
    .replace('-', "−");
    if finish.is_empty() {
        format!("{range}, {rate}, {channels}")
    } else {
        format!("{range}, {rate}, {channels}, {finish}")
    }
}

#[allow(clippy::too_many_arguments)]
fn run_preview(
    job: render::RenderJob,
    output: &std::path::Path,
    layout: ChannelLayout,
    platforms: &[&'static Platform],
    done: &AtomicU64,
    total: &AtomicU64,
    stop: &AtomicBool,
    rendered: String,
) -> std::result::Result<PreviewReport, String> {
    let written = job.join().map_err(|e| e.to_string())?;
    let path: PathBuf = written
        .first()
        .map_or_else(|| output.to_path_buf(), |r| r.path.clone());
    let file = faderframe_audio_files::wavstream::WavFile::open(&path)
        .map_err(|e| format!("{}: {e}", path.display()))?;
    let src = WavSource(file);
    total.store(
        faderframe_codec::preview::work(src.frames(), platforms.len()),
        Ordering::Relaxed,
    );
    let layout = if layout.channel_count() == src.channels() {
        layout
    } else if src.channels() == 1 {
        ChannelLayout::Mono
    } else if src.channels() == 2 {
        ChannelLayout::Stereo
    } else {
        ChannelLayout::Discrete(src.channels() as u16)
    };
    let master = faderframe_codec::preview::measure_master(
        &src,
        &crate::compare::loudness_weights(layout),
        stop,
        done,
    )
    .map_err(|e| e.to_string())?;
    let kind = if faderframe_codec::opus_available() {
        CoderKind::Opus
    } else {
        CoderKind::Identity
    };
    let previews = faderframe_codec::preview::preview(
        &src,
        &faderframe_codec::substreams(layout),
        &master,
        platforms,
        kind,
        stop,
        done,
    )
    .map_err(|e| e.to_string())?;
    Ok(PreviewReport {
        rendered,
        master,
        platforms: previews,
    })
}
