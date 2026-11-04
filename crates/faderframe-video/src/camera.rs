//! Cameras for A/V calibration: what is plugged in, and frames captured
//! with the moment their exposure started on a clock the audio engine's
//! can be related to.
//!
//! **The two clocks.** A capture pipeline runs on a monotonic system clock
//! of its own (`GstSystemClock` of type `GST_CLOCK_TYPE_MONOTONIC`, set
//! with `use_clock`, so no element can hand it another). A live source
//! stamps each buffer with its capture time as running time: v4l2src takes
//! the driver's stamp when the driver gives monotonic ones (UVC cameras
//! stamp the start of exposure), else the moment the frame arrived less a
//! frame; pipewiresrc the time PipeWire gives it. The pipeline's base time
//! plus that running time is the capture moment on the pipeline's clock.
//! That clock and `std::time::Instant` — the time base of the audio
//! engine's heard clock (`faderframe_engine::history::ns_since_epoch`) —
//! are related by reading both back to back ([`ClockMap`]: of sixteen
//! tries the pair read closest together, so the error is the gap between
//! two reads, a microsecond or two). Both are the system's monotonic clock
//! on every platform (CLOCK_MONOTONIC, mach_absolute_time,
//! QueryPerformanceCounter), so the relation does not drift. Frames
//! without a time are dropped and counted ([`Capture::untimed`]): only
//! frames on that clock are measured.
//!
//! Frames come out small and grey ([`SIZE`]), as the mean brightness of
//! every cell of a [`GRID`] (what [`crate::calibration::flashes`] reads).
//!
//! A [`Scene`] stands in for a camera in tests and in the calibration's
//! test mode: a virtual camera ([`VirtualCamera`]) integrates its light
//! over each frame's exposure (rows one after another with a rolling
//! shutter) and pushes the frames through the same pipeline, stamped on
//! its clock.

use crate::calibration::Grid;
use crate::{Result, VideoError};
use gst::prelude::*;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

/// The size frames are measured at.
pub const SIZE: (usize, usize) = (160, 120);
/// The grid of cells laid over them.
pub const GRID: Grid = Grid {
    columns: 16,
    rows: 12,
};

/// A camera that can be captured.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CameraInfo {
    /// The name it gives itself.
    pub name: String,
    /// The way it is reached ("v4l2", "pipewire", "avf", "mf", …).
    pub api: String,
    /// What tells it apart (its device path, serial or name).
    pub id: String,
}

impl CameraInfo {
    /// The name as menus show it (with the way it is reached when two
    /// list the same camera).
    pub fn label(&self) -> String {
        if self.api.is_empty() {
            self.name.clone()
        } else {
            format!("{} ({})", self.name, self.api)
        }
    }
}

/// The cameras plugged in (GStreamer's device monitor, "Video/Source").
pub fn cameras() -> Result<Vec<CameraInfo>> {
    crate::init()?;
    Ok(devices()?
        .into_iter()
        .map(|d| info_of(&d))
        .collect::<Vec<_>>())
}

fn devices() -> Result<Vec<gst::Device>> {
    let monitor = gst::DeviceMonitor::new();
    monitor.add_filter(Some("Video/Source"), None);
    monitor
        .start()
        .map_err(|e| VideoError::Gst(format!("cannot list cameras: {e}")))?;
    let found: Vec<gst::Device> = monitor.devices().into_iter().collect();
    monitor.stop();
    Ok(found)
}

fn info_of(d: &gst::Device) -> CameraInfo {
    let props = d.properties();
    let text = |key: &str| {
        props
            .as_ref()
            .and_then(|p| p.get::<String>(key).ok())
            .unwrap_or_default()
    };
    let serial = props
        .as_ref()
        .and_then(|p| p.get::<u64>("object.serial").ok())
        .map(|s| s.to_string())
        .unwrap_or_default();
    let api = text("device.api");
    let id = [
        text("device.path"),
        text("api.v4l2.path"),
        serial,
        text("device.serial"),
    ]
    .into_iter()
    .find(|s| !s.is_empty())
    .unwrap_or_else(|| d.display_name().to_string());
    CameraInfo {
        name: d.display_name().to_string(),
        api,
        id,
    }
}

/// What lights a virtual camera's screen: its brightness (0…1) at a
/// moment (asked for moments up to a frame and its readout in the past).
pub trait Scene: Send {
    fn light(&mut self, at: Instant) -> f32;
}

/// A camera standing in for one: it looks at a screen lit by a [`Scene`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct VirtualCamera {
    pub fps: u32,
    /// How long its rows take to read out, as a share of the frame period
    /// (0: a global shutter).
    pub readout: f32,
    /// Where the screen is in the picture (shares: x, y, width, height).
    pub screen: (f32, f32, f32, f32),
}

impl Default for VirtualCamera {
    fn default() -> Self {
        Self {
            fps: 30,
            readout: 0.0,
            screen: (0.2, 0.2, 0.6, 0.6),
        }
    }
}

/// What to capture.
pub enum CameraSource {
    /// The first camera found.
    First,
    /// A camera by its name, label or id.
    Device(String),
    /// A source of GStreamer's own description (tests: a
    /// `videotestsrc is-live=true`).
    Launch(String),
    /// A virtual camera looking at a scene.
    Scene(Box<dyn Scene>, VirtualCamera),
}

/// One frame as measured.
#[derive(Clone, Debug, PartialEq)]
pub struct CameraFrame {
    /// When its exposure started (its first row's).
    pub at: Instant,
    /// Its frame period (taken as its exposure).
    pub exposure: Duration,
    /// The mean brightness (0…1) of each cell of [`GRID`], row by row.
    pub cells: Vec<f32>,
}

/// The pipeline's clock read beside `Instant::now()`.
#[derive(Clone, Copy, Debug)]
pub struct ClockMap {
    /// The clock's time (ns) at `at`.
    gst: u64,
    at: Instant,
}

impl ClockMap {
    /// The closest of sixteen back-to-back reads.
    pub fn take(clock: &gst::Clock) -> Self {
        let mut best: Option<(u64, u64, Instant)> = None;
        for _ in 0..16 {
            let a = clock.time().nseconds();
            let at = Instant::now();
            let b = clock.time().nseconds();
            let gap = b.saturating_sub(a);
            if best.is_none_or(|(g, _, _)| gap < g) {
                best = Some((gap, a + gap / 2, at));
            }
        }
        let (_, gst, at) = best.unwrap_or((0, clock.time().nseconds(), Instant::now()));
        Self { gst, at }
    }

    /// The moment of clock time `t` (ns).
    pub fn instant(&self, t: u64) -> Instant {
        if t >= self.gst {
            self.at + Duration::from_nanos(t - self.gst)
        } else {
            self.at
                .checked_sub(Duration::from_nanos(self.gst - t))
                .unwrap_or(self.at)
        }
    }

    /// The clock time (ns) of a moment.
    pub fn clock_time(&self, at: Instant) -> u64 {
        if at >= self.at {
            self.gst + at.duration_since(self.at).as_nanos() as u64
        } else {
            self.gst
                .saturating_sub(self.at.duration_since(at).as_nanos() as u64)
        }
    }
}

#[derive(Default)]
struct Stats {
    frames: AtomicU64,
    untimed: AtomicU64,
}

/// A camera being captured.
pub struct Capture {
    pipeline: gst::Pipeline,
    rx: Mutex<mpsc::Receiver<CameraFrame>>,
    stats: Arc<Stats>,
    stop: Arc<AtomicBool>,
    feeder: Option<JoinHandle<()>>,
    /// What is captured (the camera's name).
    pub name: String,
}

/// The tail every capture shares: grey at [`SIZE`] into the app sink.
fn tail() -> String {
    format!(
        "videoconvert ! videoscale add-borders=false ! video/x-raw,format=GRAY8,width={},height={} ! appsink name=sink sync=false max-buffers=16",
        SIZE.0, SIZE.1
    )
}

/// The clock every capture runs on.
fn monotonic_clock() -> gst::Clock {
    gst::glib::Object::builder::<gst::SystemClock>()
        .property("clock-type", gst::ClockType::Monotonic)
        .build()
        .upcast()
}

impl Capture {
    /// Start capturing `source` (waits until the camera runs, at most a
    /// few seconds).
    pub fn start(source: CameraSource) -> Result<Self> {
        crate::init()?;
        match source {
            CameraSource::Scene(scene, camera) => Self::start_scene(scene, camera),
            CameraSource::Launch(desc) => {
                let src = gst::parse::bin_from_description(&desc, true)
                    .map_err(|e| VideoError::Gst(format!("{desc}: {e}")))?;
                Self::start_element(src.upcast(), desc, None)
            }
            CameraSource::First | CameraSource::Device(_) => {
                let wanted = match &source {
                    CameraSource::Device(name) => Some(name.clone()),
                    _ => None,
                };
                let found = devices()?;
                let device = found
                    .iter()
                    .find(|d| {
                        let i = info_of(d);
                        wanted
                            .as_ref()
                            .is_none_or(|w| *w == i.name || *w == i.id || *w == i.label())
                    })
                    .ok_or_else(|| match &wanted {
                        Some(w) => VideoError::Unavailable(format!("no camera ‘{w}’")),
                        None => VideoError::Unavailable("no camera is plugged in".into()),
                    })?;
                let name = device.display_name().to_string();
                // A frame rate of 25 or more where the camera has one (a
                // camera's highest sizes often run slowly); any otherwise.
                let fast = "video/x-raw,framerate=[25/1,1000/1];image/jpeg,framerate=[25/1,1000/1]";
                let first = device
                    .create_element(None)
                    .map_err(|e| VideoError::Gst(e.to_string()))?;
                match Self::start_element(first, name.clone(), Some(fast)) {
                    Ok(c) => Ok(c),
                    Err(e) => {
                        tracing::info!("{name} at 25 fps or more: {e}; any rate");
                        let again = device
                            .create_element(None)
                            .map_err(|e| VideoError::Gst(e.to_string()))?;
                        Self::start_element(again, name, None)
                    }
                }
            }
        }
    }

    fn start_element(src: gst::Element, name: String, caps: Option<&str>) -> Result<Self> {
        let desc = format!(
            "capsfilter name=want{} ! decodebin ! {}",
            caps.map(|c| format!(" caps=\"{c}\"")).unwrap_or_default(),
            tail()
        );
        let pipeline = gst::parse::launch(&desc)?
            .downcast::<gst::Pipeline>()
            .map_err(|_| VideoError::Gst("not a pipeline".into()))?;
        let want = pipeline
            .by_name("want")
            .ok_or_else(|| VideoError::Gst("no caps filter".into()))?;
        pipeline.add(&src)?;
        src.link(&want)?;
        Self::run(pipeline, name, None)
    }

    fn start_scene(scene: Box<dyn Scene>, camera: VirtualCamera) -> Result<Self> {
        let fps = camera.fps.max(1);
        let desc = format!(
            "appsrc name=src is-live=true format=time do-timestamp=false caps=video/x-raw,format=GRAY8,width={},height={},framerate={fps}/1 ! {}",
            SIZE.0,
            SIZE.1,
            tail()
        );
        let pipeline = gst::parse::launch(&desc)?
            .downcast::<gst::Pipeline>()
            .map_err(|_| VideoError::Gst("not a pipeline".into()))?;
        let src = pipeline
            .by_name("src")
            .and_then(|e| e.downcast::<gst_app::AppSrc>().ok())
            .ok_or_else(|| VideoError::Gst("no app source".into()))?;
        Self::run(
            pipeline,
            "Virtual camera".into(),
            Some((src, scene, camera)),
        )
    }

    fn run(
        pipeline: gst::Pipeline,
        name: String,
        feed: Option<(gst_app::AppSrc, Box<dyn Scene>, VirtualCamera)>,
    ) -> Result<Self> {
        let clock = monotonic_clock();
        pipeline.use_clock(Some(&clock));
        let map = ClockMap::take(&clock);
        let sink = pipeline
            .by_name("sink")
            .and_then(|e| e.downcast::<gst_app::AppSink>().ok())
            .ok_or_else(|| VideoError::Gst("no app sink".into()))?;
        let (tx, rx) = mpsc::channel();
        let stats = Arc::new(Stats::default());
        let s2 = Arc::clone(&stats);
        let tx = Mutex::new(tx);
        sink.set_callbacks(
            gst_app::AppSinkCallbacks::builder()
                .new_sample(move |sink| {
                    let sample = sink.pull_sample().map_err(|_| gst::FlowError::Eos)?;
                    match measure(&sample, sink.base_time(), &map) {
                        Some(f) => {
                            s2.frames.fetch_add(1, Ordering::Relaxed);
                            let _ = tx.lock().unwrap_or_else(|p| p.into_inner()).send(f);
                        }
                        None => {
                            s2.untimed.fetch_add(1, Ordering::Relaxed);
                        }
                    }
                    Ok(gst::FlowSuccess::Ok)
                })
                .build(),
        );
        pipeline.set_state(gst::State::Playing)?;
        let stop = Arc::new(AtomicBool::new(false));
        let feeder = match feed {
            Some((src, scene, camera)) => {
                let (p, s) = (pipeline.clone(), Arc::clone(&stop));
                Some(
                    std::thread::Builder::new()
                        .name("faderframe-virtual-camera".into())
                        .spawn(move || feed_scene(&p, &src, scene, camera, map, &s))
                        .map_err(VideoError::Io)?,
                )
            }
            None => None,
        };
        let capture = Self {
            pipeline,
            rx: Mutex::new(rx),
            stats,
            stop,
            feeder,
            name,
        };
        capture.wait_running(Duration::from_secs(8))?;
        Ok(capture)
    }

    /// Until the pipeline plays (or fails).
    fn wait_running(&self, limit: Duration) -> Result<()> {
        let bus = self
            .pipeline
            .bus()
            .ok_or_else(|| VideoError::Gst("no bus".into()))?;
        let end = Instant::now() + limit;
        loop {
            let left = end.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Err(VideoError::Gst(format!("{} did not start", self.name)));
            }
            let msg = bus.timed_pop_filtered(
                gst::ClockTime::from_nseconds(left.as_nanos() as u64),
                &[gst::MessageType::Error, gst::MessageType::StateChanged],
            );
            match msg.as_ref().map(|m| m.view()) {
                Some(gst::MessageView::Error(e)) => {
                    return Err(VideoError::Gst(format!("{}: {}", self.name, e.error())));
                }
                Some(gst::MessageView::StateChanged(s))
                    if s.src().is_some_and(|o| o == &self.pipeline)
                        && s.current() == gst::State::Playing =>
                {
                    return Ok(());
                }
                Some(_) => {}
                None => return Err(VideoError::Gst(format!("{} did not start", self.name))),
            }
        }
    }

    /// The frames measured since the last call.
    pub fn take(&self) -> Vec<CameraFrame> {
        let rx = self.rx.lock().unwrap_or_else(|p| p.into_inner());
        rx.try_iter().collect()
    }

    /// Frames measured so far.
    pub fn frames(&self) -> u64 {
        self.stats.frames.load(Ordering::Relaxed)
    }

    /// Frames left out for want of a time.
    pub fn untimed(&self) -> u64 {
        self.stats.untimed.load(Ordering::Relaxed)
    }

    /// The camera's failure, if it failed since it started.
    pub fn error(&self) -> Option<String> {
        let bus = self.pipeline.bus()?;
        let msg = bus.pop_filtered(&[gst::MessageType::Error])?;
        match msg.view() {
            gst::MessageView::Error(e) => Some(format!("{}: {}", self.name, e.error())),
            _ => None,
        }
    }
}

impl Drop for Capture {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(f) = self.feeder.take() {
            let _ = f.join();
        }
        let _ = self.pipeline.set_state(gst::State::Null);
    }
}

/// A sample's start of exposure and cells (`None`: no time on the clock).
fn measure(
    sample: &gst::Sample,
    base: Option<gst::ClockTime>,
    map: &ClockMap,
) -> Option<CameraFrame> {
    let buffer = sample.buffer()?;
    let pts = buffer.pts()?;
    let running = sample
        .segment()?
        .downcast_ref::<gst::ClockTime>()?
        .to_running_time(pts)?;
    let at = map.instant((base? + running).nseconds());
    let info = gst_video::VideoInfo::from_caps(sample.caps()?).ok()?;
    let exposure = buffer
        .duration()
        .map(|d| Duration::from_nanos(d.nseconds()))
        .or_else(|| {
            let fps = info.fps();
            (fps.numer() > 0)
                .then(|| Duration::from_secs_f64(fps.denom() as f64 / fps.numer() as f64))
        })?;
    let (w, h) = (info.width() as usize, info.height() as usize);
    let stride = info.stride()[0].max(0) as usize;
    let map = buffer.map_readable().ok()?;
    let data = map.as_slice();
    if w == 0 || h == 0 || data.len() < stride * (h - 1) + w {
        return None;
    }
    let mut cells = vec![0.0f32; GRID.cells()];
    for (r, row) in cells.chunks_mut(GRID.columns).enumerate() {
        let (y0, y1) = (r * h / GRID.rows, (r + 1) * h / GRID.rows);
        for (c, cell) in row.iter_mut().enumerate() {
            let (x0, x1) = (c * w / GRID.columns, (c + 1) * w / GRID.columns);
            let mut sum = 0u64;
            for y in y0..y1 {
                sum += data[y * stride + x0..y * stride + x1]
                    .iter()
                    .map(|&v| v as u64)
                    .sum::<u64>();
            }
            let n = ((y1 - y0) * (x1 - x0)).max(1);
            *cell = sum as f32 / n as f32 / 255.0;
        }
    }
    Some(CameraFrame {
        at,
        exposure,
        cells,
    })
}

/// The virtual camera's thread: a frame per period, each row band's
/// light integrated over its own exposure, pushed with its exposure start
/// as running time on the pipeline's clock.
fn feed_scene(
    pipeline: &gst::Pipeline,
    src: &gst_app::AppSrc,
    mut scene: Box<dyn Scene>,
    camera: VirtualCamera,
    map: ClockMap,
    stop: &AtomicBool,
) {
    let period = Duration::from_secs_f64(1.0 / f64::from(camera.fps.max(1)));
    let readout = period.mul_f32(camera.readout.clamp(0.0, 1.0));
    // The base time is set as the pipeline goes to playing (the clock's
    // time then: never 0 on a monotonic clock). A live pipeline only gets
    // there once the sink has a frame, so frames go in from then on.
    let base = loop {
        if stop.load(Ordering::Relaxed) {
            return;
        }
        if let Some(b) = pipeline.base_time().filter(|b| b.nseconds() > 0) {
            break b.nseconds();
        }
        std::thread::sleep(Duration::from_millis(2));
    };
    let (w, h) = SIZE;
    let (sx, sy, sw, sh) = camera.screen;
    let x0 = (sx * w as f32) as usize;
    let x1 = ((sx + sw) * w as f32) as usize;
    let y0 = (sy * h as f32) as usize;
    let y1 = ((sy + sh) * h as f32) as usize;
    let bands = GRID.rows;
    const STEPS: u32 = 64;
    let mut start = Instant::now() + period;
    while !stop.load(Ordering::Relaxed) {
        // Once the last row's exposure is over.
        let done = start + period + readout;
        let now = Instant::now();
        if done > now {
            std::thread::sleep(done - now);
        }
        let mut px = vec![12u8; w * h];
        for b in 0..bands {
            let a = start + readout.mul_f64((b as f64 + 0.5) / bands as f64);
            let mut sum = 0.0f32;
            for k in 0..STEPS {
                sum += scene.light(a + period.mul_f64((k as f64 + 0.5) / f64::from(STEPS)));
            }
            let v = (20.0 + 200.0 * (sum / STEPS as f32).clamp(0.0, 1.0)).round() as u8;
            for y in (b * h / bands)..((b + 1) * h / bands) {
                if (y0..y1).contains(&y) {
                    px[y * w + x0..y * w + x1].fill(v);
                }
            }
        }
        let t = map.clock_time(start);
        if t > base {
            let mut buf = gst::Buffer::from_mut_slice(px);
            if let Some(b) = buf.get_mut() {
                b.set_pts(gst::ClockTime::from_nseconds(t - base));
                b.set_duration(gst::ClockTime::from_nseconds(period.as_nanos() as u64));
            }
            if src.push_buffer(buf).is_err() {
                return;
            }
        }
        start += period;
    }
}
