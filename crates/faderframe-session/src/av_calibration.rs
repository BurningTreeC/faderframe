//! A/V calibration by itself: the flash-and-beep test watched by a camera
//! and heard by a microphone sets the picture offset.
//!
//! The test is the sync test's: the screen flashes white at every second
//! and a 1 kHz beep sounds with each flash — played here by the session,
//! not from files in the project. The video window paints the flashes
//! ([`Session::av_calibration_screen`]) through the very timing the
//! picture is shown with (the heard position when the frame reaches the
//! screen, less the picture offset: `Session::picture_position`), and the
//! beeps come out of the monitor path in the mix's place (the A/B compare
//! source, following the timeline from the first flash: in step with what
//! is heard by construction). The project plays on underneath (from the
//! playhead, or past the loop when the loop would wrap) and is not
//! changed; only the offset is set at the end, one undo step.
//!
//! A camera ([`faderframe_video::camera`]) films the screen; its frames
//! carry the start of their exposure on the pipeline's monotonic clock,
//! related to the engine's time base (`Instant`, as the heard clock) by
//! reading both clocks together. The microphone is a device input tapped
//! on the audio thread ([`faderframe_engine::input_tap`]): every callback's
//! samples with when the callback started — they came in over the
//! callback's frames before that, and the input latency the device reports
//! before those. [`faderframe_video::calibration`] then times each flash
//! within its frame and each beep to a fraction of a millisecond; each
//! pair says how much later the picture came than the sound, and the
//! robust mean of the pairs corrects the offset (the spread is shown).
//!
//! What the screen adds after the frame clock's estimate (the compositor,
//! the monitor's processing) and what the speakers add after the device's
//! report are measured: that is the point. What the sensors add (the
//! camera's stamp against its exposure, its rolling shutter down to the
//! screen's middle, the microphone's input latency as reported) stays in
//! the result.
//!
//! A virtual camera ([`CameraChoice::Virtual`]) stands in for a real one in
//! tests and as a self-test: it sees the screen as the video window would
//! show it, `display_ms` late.

use crate::{NoticeLevel, Result, Session, SessionError};
use faderframe_engine::EngineClock;
use faderframe_engine::input_tap::InputTap;
use faderframe_engine::nodes::{CompareSettings, CompareSource};
use faderframe_project::Command;
use faderframe_timeline::MusicalTime;
use faderframe_video::calibration::{self, Heard, LightFrame};
use faderframe_video::camera::{CameraInfo, CameraSource, Capture, Scene, VirtualCamera};
use std::path::PathBuf;
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

/// How long a flash lasts (six frames at 25 fps: frames fully inside it
/// tell its brightness, so the frame it starts in times it).
pub const FLASH_SECONDS: f64 = 0.24;
/// The beep: a tone of this pitch, a frame at 25 fps long.
pub const BEEP_HZ: f64 = 1000.0;
pub const BEEP_SECONDS: f64 = 0.04;
/// Flashes this far apart (s).
pub const EVERY_SECONDS: f64 = 1.0;
/// The first flash this long after playback starts (the camera and the
/// listening settle).
pub const LEAD_IN_SECONDS: f64 = 1.0;
/// Flashes when none are asked for.
pub const DEFAULT_FLASHES: u32 = 10;
/// A flash and a beep further apart than this are not a pair (ms).
const PAIRED_MS: f64 = 450.0;

/// Which camera watches the screen.
#[derive(Clone, Debug, PartialEq)]
pub enum CameraChoice {
    /// The first camera found.
    Default,
    /// A camera by its name, label or id ([`CameraInfo`]).
    Named(String),
    /// No camera: a virtual one sees the screen as the video window shows
    /// it, `display_ms` late, its rows read out over `readout` of a frame.
    Virtual { display_ms: f64, readout: f32 },
}

/// How a calibration runs.
#[derive(Clone, Debug, PartialEq)]
pub struct CalibrationSettings {
    pub camera: CameraChoice,
    /// The device input the microphone is on (from 0).
    pub input: u16,
    /// How many flashes (and beeps).
    pub flashes: u32,
}

impl Default for CalibrationSettings {
    fn default() -> Self {
        Self {
            camera: CameraChoice::Default,
            input: 0,
            flashes: DEFAULT_FLASHES,
        }
    }
}

/// What a calibration found.
#[derive(Clone, Debug, PartialEq)]
pub struct CalibrationResult {
    /// The picture offset before and as set (ms).
    pub before_ms: f64,
    pub offset_ms: f64,
    /// How much later the picture came than the sound (ms, before).
    pub late_ms: f64,
    /// The pairs' spread (standard deviation, ms).
    pub spread_ms: f64,
    /// Pairs kept, flashes seen, beeps heard, flashes shown.
    pub used: usize,
    pub seen: usize,
    pub heard: usize,
    pub shown: u32,
    /// Every pair's difference (flash − beep, ms).
    pub pairs: Vec<f64>,
}

/// Where a calibration is.
#[derive(Clone, Debug, PartialEq)]
pub enum CalibrationStatus {
    /// Opening the camera.
    Opening,
    /// Flashing: flash `flash` (from 1) of `of`.
    Measuring {
        flash: u32,
        of: u32,
    },
    Done(CalibrationResult),
    Failed(String),
}

/// The session's calibration state (kept in the video state).
#[derive(Default)]
pub(crate) struct CalibrationState {
    run: Option<Run>,
    last: Option<CalibrationStatus>,
    cameras: Vec<CameraInfo>,
    cameras_found: bool,
    finding: Option<JoinHandle<std::result::Result<Vec<CameraInfo>, String>>>,
}

/// A calibration under way.
struct Run {
    settings: CalibrationSettings,
    stage: Stage,
    tap: Option<InputTap>,
    frames: Vec<LightFrame>,
    samples: Vec<f32>,
    stamps: Vec<(usize, i64)>,
    /// The device's input latency (ns) and rate as the tap began.
    input_latency_ns: i64,
    rate: u32,
    /// The first flash's timeline position, the run's end, and where the
    /// playhead was before.
    anchor: i64,
    end: i64,
    back_to: MusicalTime,
    beeps: PathBuf,
    started: Instant,
    played: bool,
    untimed: u64,
}

enum Stage {
    Opening(Option<JoinHandle<faderframe_video::Result<Capture>>>),
    Measuring(Capture),
}

/// The flash schedule (timeline samples).
#[derive(Clone, Copy, Debug)]
struct Schedule {
    anchor: i64,
    flashes: u32,
    every: f64,
    length: f64,
}

impl Schedule {
    fn of(anchor: i64, flashes: u32, rate: f64) -> Self {
        Self {
            anchor,
            flashes,
            every: EVERY_SECONDS * rate,
            length: FLASH_SECONDS * rate,
        }
    }

    /// Whether the screen is white for timeline position `pos`.
    fn lit(&self, pos: f64) -> bool {
        let into = pos - self.anchor as f64;
        if into < 0.0 {
            return false;
        }
        let k = (into / self.every).floor();
        k < f64::from(self.flashes) && into - k * self.every < self.length
    }
}

/// The screen a virtual camera looks at: lit as the video window would
/// show it — the heard position (carried on from now: playback is steady)
/// less the picture offset and the screen's own delay.
struct VirtualScreen {
    clock: EngineClock,
    schedule: Schedule,
    /// The picture offset and the screen's delay (s).
    shift: f64,
    heard: Option<(Instant, faderframe_engine::Heard)>,
}

impl Scene for VirtualScreen {
    fn light(&mut self, at: Instant) -> f32 {
        let now = Instant::now();
        if self
            .heard
            .is_none_or(|(t, _)| now.duration_since(t) > Duration::from_millis(2))
        {
            self.heard = self.clock.heard_ahead(now).map(|h| (now, h));
        }
        let Some((t0, h)) = self.heard else {
            return 0.0;
        };
        if !h.playing {
            return 0.0;
        }
        let dt = if at >= t0 {
            at.duration_since(t0).as_secs_f64()
        } else {
            -t0.duration_since(at).as_secs_f64()
        };
        let pos = h.position as f64 + (dt - self.shift) * h.per_second;
        if self.schedule.lit(pos) { 1.0 } else { 0.0 }
    }
}

impl Session {
    /// Where a calibration is (`None`: none ran in this session).
    pub fn av_calibration(&self) -> Option<CalibrationStatus> {
        let c = &self.video.calibration;
        match &c.run {
            Some(r) => Some(match r.stage {
                Stage::Opening(_) => CalibrationStatus::Opening,
                Stage::Measuring(_) => {
                    let rate = f64::from(self.project.sample_rate);
                    let into = (self.transport.position - r.anchor) as f64 / (EVERY_SECONDS * rate);
                    CalibrationStatus::Measuring {
                        flash: ((into.floor() + 1.0).max(0.0) as u32).min(r.settings.flashes),
                        of: r.settings.flashes,
                    }
                }
            }),
            None => c.last.clone(),
        }
    }

    /// Whether a calibration runs.
    pub fn av_calibrating(&self) -> bool {
        self.video.calibration.run.is_some()
    }

    /// What the video window shows while calibrating, for the frame
    /// reaching the screen `lead_ns` from now: `Some(true)` white,
    /// `Some(false)` dark; `None`: no calibration (the picture).
    pub fn av_calibration_screen(&self, lead_ns: i64) -> Option<bool> {
        let r = self.video.calibration.run.as_ref()?;
        if !matches!(r.stage, Stage::Measuring(_)) || !self.transport.playing {
            return Some(false);
        }
        let schedule = Schedule::of(
            r.anchor,
            r.settings.flashes,
            f64::from(self.project.sample_rate),
        );
        Some(schedule.lit(self.picture_position(lead_ns) as f64))
    }

    /// The cameras found by the last search ([`Self::find_cameras`]), and
    /// whether one ran.
    pub fn av_cameras(&self) -> (&[CameraInfo], bool) {
        let c = &self.video.calibration;
        (&c.cameras, c.cameras_found)
    }

    /// Look for cameras (in the background).
    pub(crate) fn find_cameras(&mut self) {
        let c = &mut self.video.calibration;
        if c.finding.is_some() {
            return;
        }
        c.finding = std::thread::Builder::new()
            .name("faderframe-cameras".into())
            .spawn(|| faderframe_video::camera::cameras().map_err(|e| e.to_string()))
            .ok();
    }

    /// Start calibrating.
    pub(crate) fn start_av_calibration(&mut self, settings: CalibrationSettings) -> Result<()> {
        if self.video.calibration.run.is_some() {
            return Err(SessionError::Other("a calibration is running".into()));
        }
        if self.audio.is_none() {
            return Err(SessionError::Other(
                "A/V calibration needs the audio device running (its input hears the beeps)".into(),
            ));
        }
        let inputs = self
            .stream_info()
            .map_or(0, |i| usize::from(i.input_channels))
            .max(self.engine.stream_inputs());
        if usize::from(settings.input) >= inputs {
            return Err(SessionError::Other(format!(
                "the audio device has no input {} for the microphone ({inputs} inputs)",
                settings.input + 1
            )));
        }
        if self.comparing().is_some() {
            return Err(SessionError::Other(
                "turn A/B listening off first: the calibration's beeps play in its place".into(),
            ));
        }
        faderframe_video::init().map_err(|e| SessionError::Other(e.to_string()))?;
        let flashes = settings.flashes.clamp(3, 60);
        let settings = CalibrationSettings {
            flashes,
            ..settings
        };
        if self.transport.playing {
            self.dispatch(crate::Action::Transport(crate::TransportAction::Stop))?;
        }
        let rate = f64::from(self.project.sample_rate);
        // From the playhead, or past the loop when it would wrap inside.
        let here = self.transport.position.max(0);
        let start = match self
            .project
            .loop_range
            .filter(|_| self.project.loop_enabled)
        {
            Some(r) => {
                let end = self.engine.musical_to_samples(&self.project, r.end);
                if end > here { end + rate as i64 } else { here }
            }
            None => here,
        };
        let anchor = start + (LEAD_IN_SECONDS * rate) as i64;
        let end =
            anchor + ((f64::from(flashes - 1) * EVERY_SECONDS + FLASH_SECONDS + 0.5) * rate) as i64;
        // The beeps, following the timeline from the first flash.
        let beeps = calibration::beep_track(
            self.project.sample_rate,
            0.0,
            EVERY_SECONDS,
            flashes as usize,
            BEEP_HZ,
            BEEP_SECONDS,
            1.0,
        );
        let frames = beeps.len() as i64;
        let dir = self.compare.dir();
        std::fs::create_dir_all(&dir).map_err(|e| SessionError::Other(e.to_string()))?;
        // A name of its own (a file still open elsewhere is not written
        // over).
        let path = crate::media::unique_path(&dir, "av-calibration");
        faderframe_audio_files::write_wav(
            &path,
            &[beeps.clone(), beeps],
            self.project.sample_rate,
            faderframe_audio_files::WavFormat::Float32,
            false,
        )
        .map_err(|e| SessionError::Other(format!("{}: {e}", path.display())))?;
        let source = crate::media::open_stream(&path)
            .map_err(|e| SessionError::Other(format!("{}: {e}", path.display())))?;
        // The camera opens in the background (a real one takes a moment).
        let camera = match &settings.camera {
            CameraChoice::Default => CameraSource::First,
            CameraChoice::Named(n) => CameraSource::Device(n.clone()),
            CameraChoice::Virtual {
                display_ms,
                readout,
            } => CameraSource::Scene(
                Box::new(VirtualScreen {
                    clock: self.engine.clock(),
                    schedule: Schedule::of(anchor, flashes, rate),
                    shift: (display_ms + self.project.video.offset_ms) / 1000.0,
                    heard: None,
                }),
                VirtualCamera {
                    readout: *readout,
                    ..VirtualCamera::default()
                },
            ),
        };
        let opening = std::thread::Builder::new()
            .name("faderframe-camera-open".into())
            .spawn(move || Capture::start(camera))
            .map_err(|e| SessionError::Other(e.to_string()))?;
        let tap = self
            .engine
            .tap_input(settings.input, 4.0)
            .map_err(|e| SessionError::Other(e.to_string()))?;
        let stream_rate = self.engine.stream_rate().max(1);
        // The input's own device: one following another's clock comes later.
        let input_latency_ns = (f64::from(self.input_latency_of(settings.input)) * 1e9
            / f64::from(stream_rate)) as i64;
        self.hear_instead(Some(CompareSource {
            source: Arc::clone(&source),
            settings: CompareSettings::follow(anchor, frames),
        }));
        let back_to = self.engine.samples_to_musical(&self.project, here);
        let start_at = self.engine.samples_to_musical(&self.project, start);
        self.video.calibration.run = Some(Run {
            settings,
            stage: Stage::Opening(Some(opening)),
            tap: Some(tap),
            frames: Vec::new(),
            samples: Vec::new(),
            stamps: Vec::new(),
            input_latency_ns,
            rate: stream_rate,
            anchor,
            end,
            back_to,
            beeps: path,
            started: Instant::now(),
            played: false,
            untimed: 0,
        });
        // Positioned now: the playhead waits there while the camera opens.
        self.dispatch(crate::Action::Transport(crate::TransportAction::Locate(
            start_at,
        )))?;
        self.revision += 1;
        Ok(())
    }

    /// Stop a calibration (the offset stays as it was).
    pub(crate) fn cancel_av_calibration(&mut self, why: Option<String>) {
        if self.video.calibration.run.is_none() {
            return;
        }
        self.end_av_run();
        let text = why.unwrap_or_else(|| "A/V calibration stopped".into());
        self.notify(NoticeLevel::Info, text.clone());
        self.video.calibration.last = Some(CalibrationStatus::Failed(text));
    }

    /// Wind a run down: the camera, the tap, the beeps; the transport back
    /// where it was. The measurements, if any.
    fn end_av_run(&mut self) -> Option<Run> {
        let mut run = self.video.calibration.run.take()?;
        if self.transport.playing {
            let _ = self.dispatch(crate::Action::Transport(crate::TransportAction::Stop));
        }
        let _ = self.engine.untap_input();
        if let Some(tap) = run.tap.as_mut() {
            take_heard(
                &mut run.samples,
                &mut run.stamps,
                tap,
                run.input_latency_ns,
                run.rate,
            );
        }
        run.tap = None;
        if let Stage::Measuring(c) = &run.stage {
            take_frames(&mut run.frames, c);
            run.untimed = c.untimed();
        }
        // The camera closes (an opening one closes when it is open).
        run.stage = Stage::Opening(None);
        self.hear_instead(None);
        let path = std::mem::take(&mut run.beeps);
        self.compare.throw_away(path);
        let _ = self.dispatch(crate::Action::Transport(crate::TransportAction::Locate(
            run.back_to,
        )));
        self.revision += 1;
        Some(run)
    }

    /// Move a calibration on (from the tick).
    pub(crate) fn tick_av_calibration(&mut self) {
        if let Some(h) = self.video.calibration.finding.take_if(|h| h.is_finished()) {
            let found = h.join().unwrap_or_else(|_| Err("the search failed".into()));
            let c = &mut self.video.calibration;
            c.cameras_found = true;
            match found {
                Ok(list) => c.cameras = list,
                Err(e) => {
                    c.cameras.clear();
                    self.notify(NoticeLevel::Warning, format!("cameras: {e}"));
                }
            }
            self.revision += 1;
        }
        let Some(run) = self.video.calibration.run.as_mut() else {
            return;
        };
        // The camera opened: play.
        if let Stage::Opening(slot) = &mut run.stage {
            let ready = slot.as_ref().is_some_and(JoinHandle::is_finished);
            if !ready {
                if run.started.elapsed() > Duration::from_secs(15) {
                    self.cancel_av_calibration(Some(
                        "A/V calibration: the camera did not open".into(),
                    ));
                }
                return;
            }
            let opened = slot
                .take()
                .and_then(|h| h.join().ok())
                .unwrap_or_else(|| Err(faderframe_video::VideoError::Gst("no camera".into())));
            let capture = match opened {
                Ok(c) => c,
                Err(e) => {
                    self.cancel_av_calibration(Some(format!("A/V calibration: {e}")));
                    return;
                }
            };
            run.stage = Stage::Measuring(capture);
            run.started = Instant::now();
            // What the microphone heard while the camera opened is not
            // used.
            if let Some(tap) = run.tap.as_mut() {
                let _ = tap.drain();
            }
            self.play_here = true;
            let played = self.dispatch(crate::Action::Transport(crate::TransportAction::Play));
            self.play_here = false;
            if let Err(e) = played {
                self.cancel_av_calibration(Some(format!("A/V calibration: {e}")));
            }
            self.revision += 1;
            return;
        }
        let Stage::Measuring(capture) = &run.stage else {
            return;
        };
        take_frames(&mut run.frames, capture);
        let failed = capture.error();
        if let Some(tap) = run.tap.as_mut() {
            take_heard(
                &mut run.samples,
                &mut run.stamps,
                tap,
                run.input_latency_ns,
                run.rate,
            );
        }
        let limit = Duration::from_secs_f64(
            LEAD_IN_SECONDS + f64::from(run.settings.flashes) * EVERY_SECONDS + 10.0,
        );
        let late = run.started.elapsed() > limit;
        let playing = self.transport.playing;
        let past = self.transport.position >= run.end;
        run.played |= playing;
        let stopped = run.played && !playing && !past;
        self.revision += 1;
        if let Some(e) = failed {
            self.cancel_av_calibration(Some(format!("A/V calibration: {e}")));
        } else if self.audio.is_none() {
            self.cancel_av_calibration(Some("A/V calibration: the audio device stopped".into()));
        } else if stopped {
            self.cancel_av_calibration(None);
        } else if past {
            self.finish_av_calibration();
        } else if late {
            self.cancel_av_calibration(Some(
                "A/V calibration: the transport did not play the test through".into(),
            ));
        }
    }

    /// The test is through: measure, set the offset.
    fn finish_av_calibration(&mut self) {
        let Some(run) = self.end_av_run() else {
            return;
        };
        let status = match self.measure_av(&run) {
            Ok(result) => {
                let label = "Calibrate Picture Offset";
                let mut video = self.project.video.clone();
                video.offset_ms = result.offset_ms;
                let edit = self.edit(Command::Batch {
                    label: label.into(),
                    commands: vec![Command::SetVideo {
                        video: Box::new(video),
                    }],
                });
                match edit {
                    Ok(()) => {
                        self.notify(
                            NoticeLevel::Info,
                            format!(
                                "Picture offset {:+.1} ms (was {:+.1}; ±{:.1} ms over {} of {} flashes)",
                                result.offset_ms,
                                result.before_ms,
                                result.spread_ms,
                                result.used,
                                result.shown
                            ),
                        );
                        CalibrationStatus::Done(result)
                    }
                    Err(e) => CalibrationStatus::Failed(e.to_string()),
                }
            }
            Err(text) => {
                self.notify(NoticeLevel::Warning, text.clone());
                CalibrationStatus::Failed(text)
            }
        };
        self.video.calibration.last = Some(status);
        self.revision += 1;
    }

    /// What the run saw and heard, and the offset it gives.
    fn measure_av(&self, run: &Run) -> std::result::Result<CalibrationResult, String> {
        let seen = calibration::flashes(&run.frames, faderframe_video::camera::GRID);
        let flashes: Vec<i64> = seen.flashes.iter().map(|f| f.at).collect();
        let heard = Heard {
            rate: run.rate,
            samples: run.samples.clone(),
            stamps: run.stamps.clone(),
        };
        let beeps = calibration::beeps(&heard, BEEP_HZ, BEEP_SECONDS);
        tracing::info!(
            "A/V calibration: {} frames ({} untimed), {} flashes, {} beeps",
            run.frames.len(),
            run.untimed,
            flashes.len(),
            beeps.len()
        );
        let shown = run.settings.flashes;
        if run.frames.is_empty() {
            return Err(if run.untimed > 0 {
                "A/V calibration: the camera's frames carry no time to measure by".into()
            } else {
                "A/V calibration: the camera gave no pictures".into()
            });
        }
        if flashes.is_empty() {
            return Err(
                "A/V calibration: the camera saw no flashes — point it at the video \
                        window (full screen is best)"
                    .into(),
            );
        }
        if beeps.is_empty() {
            return Err(format!(
                "A/V calibration: the microphone on input {} heard no beeps — turn the speakers \
                 up or choose its input",
                run.settings.input + 1
            ));
        }
        let min = (shown as usize / 2).max(3);
        let e = calibration::estimate(&flashes, &beeps, (PAIRED_MS * 1e6) as i64, min).ok_or_else(
            || {
                format!(
                    "A/V calibration: too few flashes went with beeps ({} flashes, {} beeps of \
                     {shown})",
                    flashes.len(),
                    beeps.len()
                )
            },
        )?;
        let before = self.project.video.offset_ms;
        let offset = ((before - e.late_ms) * 10.0).round() / 10.0;
        if offset.abs() > 1000.0 {
            return Err(format!(
                "A/V calibration: the picture is {:.0} ms off, more than an offset covers",
                e.late_ms
            ));
        }
        Ok(CalibrationResult {
            before_ms: before,
            offset_ms: offset,
            late_ms: e.late_ms,
            spread_ms: e.spread_ms,
            used: e.used,
            seen: flashes.len(),
            heard: beeps.len(),
            shown,
            pairs: e.pairs,
        })
    }

    /// Wait for a calibration to finish (tests, scripts); whether it did.
    pub fn wait_for_av_calibration(&mut self, timeout: Duration) -> bool {
        let end = Instant::now() + timeout;
        while self.video.calibration.run.is_some() {
            if Instant::now() > end {
                return false;
            }
            self.tick(0.01);
            std::thread::sleep(Duration::from_millis(5));
        }
        true
    }

    /// Wait for the camera search (tests, scripts).
    pub fn wait_for_cameras(&mut self, timeout: Duration) -> bool {
        let end = Instant::now() + timeout;
        while self.video.calibration.finding.is_some() {
            if Instant::now() > end {
                return false;
            }
            self.tick_av_calibration();
            std::thread::sleep(Duration::from_millis(5));
        }
        true
    }
}

fn take_frames(into: &mut Vec<LightFrame>, capture: &Capture) {
    into.extend(capture.take().into_iter().map(|f| LightFrame {
        at: faderframe_engine::history::ns_since_epoch(f.at) as i64,
        exposure: f.exposure.as_nanos() as i64,
        cells: f.cells,
    }));
}

/// What the tap has: its samples, each block's first sample stamped with
/// when it was taken. A callback's input came in over the callback's own
/// frames before it started, and the device's input latency before that
/// (the latencies devices report leave their buffer out: the engine counts
/// the output's buffer the same way).
fn take_heard(
    samples: &mut Vec<f32>,
    stamps: &mut Vec<(usize, i64)>,
    tap: &mut InputTap,
    latency_ns: i64,
    rate: u32,
) {
    let (more, blocks) = tap.drain();
    let base = samples.len();
    let per_frame = 1e9 / f64::from(rate.max(1));
    for (k, &(i, ns)) in blocks.iter().enumerate() {
        let next = blocks.get(k + 1).map_or(more.len(), |b| b.0);
        let frames = next.saturating_sub(i) as f64;
        stamps.push((
            base + i,
            ns as i64 - latency_ns - (frames * per_frame) as i64,
        ));
    }
    samples.extend(more);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_schedule_lights_the_flashes_only() {
        let s = Schedule::of(48_000, 3, 48_000.0);
        assert!(!s.lit(47_999.0));
        assert!(s.lit(48_000.0));
        assert!(s.lit(48_000.0 + 0.239 * 48_000.0));
        assert!(!s.lit(48_000.0 + 0.241 * 48_000.0));
        assert!(s.lit(96_000.5));
        assert!(s.lit(144_000.0));
        // Three only.
        assert!(!s.lit(192_000.0));
    }
}
