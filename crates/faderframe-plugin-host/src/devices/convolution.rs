//! Convolution Reverb: a room's (or a plate's, a spring's, anything's)
//! impulse response played without latency (`dsp::convolve`).
//!
//! The response is a file the device keeps in the project (its state's
//! [`SampleDoc`](super::samples::SampleDoc) slot 0, loaded like a
//! sampler's sample: copied into the media's `Samples`, decoded in the
//! background in a live session) — mono, stereo, or true stereo (four
//! channels: left into left, left into right, right into left, right into
//! right) — or one of the built-in spaces ([`spaces`], made here). Space
//! picks which: 0 the file, the others a built-in.
//!
//! What shapes the response ([`shape`]: Start, Length, Fade In/Out,
//! Reverse, Stretch, Normalise) is applied on a thread of the processor's
//! own (the designer), which hands the result to the audio thread: new
//! kernels for the engine playing (crossfaded in, the history kept, so
//! moving Length while a tail rings does not cut it), or, for a new
//! response, a new engine crossfaded in over [`ENGINE_FADE`] samples.
//! These parameters are not automatable. What follows the convolution is:
//! Pre-Delay (a delay line before it, gliding), Low and High Cut (12
//! dB/oct on the wet signal), Width (its side), Output (its level) and
//! Mix.
//!
//! In a live session the first response is designed in the background too
//! (the wet signal comes in when it is ready); everywhere else (renders,
//! tests) the processor is made with it, so a render is the same every
//! time.

pub mod shape;
pub mod spaces;

use super::{on_off, param, pass_through, pick, stepped};
use crate::dsp::convolve::{self, Designer, Engine, Kernels, Routing};
use crate::dsp::delay::DelayLine;
use crate::dsp::filter::{BandShape, BandType, Filter};
use crate::dsp::smooth::Smoothed;
use crate::tap::{AnalysisTap, MeterTap, Watching};
use crate::{
    ParamValues, ParameterInfo, ParameterUnit, PluginProcessContext, PluginProcessor,
    ProcessConfig, ProcessStatus,
};
use faderframe_audio_graph::NodeIo;
use faderframe_core::ParameterId;
use faderframe_realtime::{MailboxReceiver, MailboxSender};
use realfft::RealFftPlanner;
use shape::Shaping;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

pub mod id {
    /// 0: the impulse file; 1…: a built-in space.
    pub const SPACE: u32 = 0;
    pub const MIX: u32 = 1;
    pub const PRE_DELAY: u32 = 2;
    pub const LOW_CUT: u32 = 3;
    pub const HIGH_CUT: u32 = 4;
    pub const WIDTH: u32 = 5;
    /// The wet signal's level (dB).
    pub const OUTPUT: u32 = 6;
    pub const START: u32 = 7;
    pub const LENGTH: u32 = 8;
    pub const FADE_IN: u32 = 9;
    pub const FADE_OUT: u32 = 10;
    pub const REVERSE: u32 = 11;
    pub const STRETCH: u32 = 12;
    pub const NORMALISE: u32 = 13;
}

/// Published: input, output and wet peaks (linear, since the editor last
/// looked); the response playing — its length (s), channels (1, 2, 4), its
/// decay (RT60, s; 0 unknown); whether a response is being made.
pub mod value {
    pub const IN_PEAK: usize = 0;
    pub const OUT_PEAK: usize = 1;
    pub const WET_PEAK: usize = 2;
    pub const SECONDS: usize = 3;
    pub const CHANNELS: usize = 4;
    pub const DECAY: usize = 5;
    pub const BUILDING: usize = 6;
}
pub const TAP_VALUES: usize = 7;

/// The longest pre-delay (ms).
pub const MAX_PRE_MS: f64 = 500.0;
/// A new response's engine crossfades in over this many samples.
pub const ENGINE_FADE: usize = 4_096;

/// Space's choices: the file, then the built-ins.
pub fn space_names() -> Vec<&'static str> {
    std::iter::once("Impulse File")
        .chain(spaces::SPACES.iter().map(|s| s.name))
        .collect()
}

/// Space's value for the built-in named `name`.
pub fn space_named(name: &str) -> Option<f64> {
    spaces::SPACES
        .iter()
        .position(|s| s.name.eq_ignore_ascii_case(name))
        .map(|i| (i + 1) as f64)
}

/// The default space (a hall).
const DEFAULT_SPACE: f64 = 5.0;

/// A parameter that makes the response anew (not automatable).
fn reshapes(info: ParameterInfo) -> ParameterInfo {
    ParameterInfo {
        automatable: false,
        ..info
    }
}

pub fn parameters() -> Vec<ParameterInfo> {
    use ParameterUnit::*;
    vec![
        reshapes(stepped(
            id::SPACE,
            "Space",
            spaces::SPACES.len() as f64,
            DEFAULT_SPACE,
        )),
        param(id::MIX, "Mix", 0.0, 1.0, 0.3, Percent),
        param(
            id::PRE_DELAY,
            "Pre-Delay",
            0.0,
            MAX_PRE_MS,
            0.0,
            Milliseconds,
        ),
        param(id::LOW_CUT, "Low Cut", 20.0, 1_000.0, 20.0, Hertz),
        param(id::HIGH_CUT, "High Cut", 1_000.0, 20_000.0, 20_000.0, Hertz),
        param(id::WIDTH, "Width", 0.0, 1.5, 1.0, Percent),
        param(id::OUTPUT, "Output", -24.0, 12.0, 0.0, Decibels),
        reshapes(param(id::START, "Start", 0.0, 2_000.0, 0.0, Milliseconds)),
        reshapes(param(id::LENGTH, "Length", 0.01, 1.0, 1.0, Percent)),
        reshapes(param(id::FADE_IN, "Fade In", 0.0, 500.0, 0.0, Milliseconds)),
        reshapes(param(id::FADE_OUT, "Fade Out", 0.0, 1.0, 0.0, Percent)),
        reshapes(stepped(id::REVERSE, "Reverse", 1.0, 0.0)),
        reshapes(param(
            id::STRETCH,
            "Stretch",
            shape::MIN_STRETCH,
            shape::MAX_STRETCH,
            1.0,
            None,
        )),
        reshapes(stepped(id::NORMALISE, "Normalise", 1.0, 1.0)),
    ]
}

pub fn format(pid: ParameterId, v: f64) -> Option<String> {
    Some(match pid.0 {
        id::SPACE => pick(&space_names(), v),
        id::REVERSE | id::NORMALISE => on_off(v),
        id::STRETCH => format!("{v:.2}×"),
        id::LOW_CUT if v <= 20.5 => "Off".into(),
        id::HIGH_CUT if v >= 19_500.0 => "Off".into(),
        id::FADE_IN | id::FADE_OUT | id::START if v <= 0.0 => "Off".into(),
        _ => return None,
    })
}

pub fn latency(_params: &ParamValues, _rate: f64) -> u32 {
    0
}

fn get(params: &ParamValues, pid: u32) -> f64 {
    f64::from(params.get(pid as usize))
}

fn shaping(params: &ParamValues) -> Shaping {
    Shaping {
        start_ms: get(params, id::START),
        length: get(params, id::LENGTH),
        fade_in_ms: get(params, id::FADE_IN),
        fade_out: get(params, id::FADE_OUT),
        reverse: get(params, id::REVERSE) >= 0.5,
        stretch: get(params, id::STRETCH),
        normalise: get(params, id::NORMALISE) >= 0.5,
    }
}

/// The space chosen (0: the file).
fn space_of(params: &ParamValues) -> usize {
    (get(params, id::SPACE).round().max(0.0) as usize).min(spaces::SPACES.len())
}

/// What the editor shows of the response playing (the tap's assets).
#[derive(Clone, Debug, Default)]
pub struct ImpulseView {
    /// The space's name or the file's.
    pub name: String,
    pub seconds: f64,
    pub channels: usize,
    /// Per point across the response: its lowest and highest sample (every
    /// channel together, scaled to the loudest).
    pub envelope: Vec<(f32, f32)>,
    /// Per point: the energy still to come (dB, 0 at the start; Schroeder).
    pub decay_db: Vec<f32>,
    pub rt60: Option<f64>,
    /// Why there is nothing to play (no file, a file that would not load).
    pub problem: Option<String>,
}

/// Points of an [`ImpulseView`]'s curves.
const VIEW_POINTS: usize = 600;

impl ImpulseView {
    fn of(name: String, channels: &[Vec<f32>], rate: f64) -> Self {
        let len = channels.iter().map(Vec::len).max().unwrap_or(0);
        let step = len.div_ceil(VIEW_POINTS).max(1);
        let peak = channels
            .iter()
            .flatten()
            .fold(0.0f32, |m, v| m.max(v.abs()))
            .max(1e-9);
        let mut envelope = Vec::with_capacity(VIEW_POINTS);
        let mut energy = vec![0.0f64; len.div_ceil(step)];
        for (p, e) in energy.iter_mut().enumerate() {
            let (a, b) = (p * step, ((p + 1) * step).min(len));
            let (mut lo, mut hi) = (0.0f32, 0.0f32);
            for c in channels {
                for v in c.get(a..b.min(c.len())).unwrap_or(&[]) {
                    lo = lo.min(*v);
                    hi = hi.max(*v);
                    *e += f64::from(*v).powi(2);
                }
            }
            envelope.push((lo / peak, hi / peak));
        }
        let mut acc = 0.0;
        let mut rest: Vec<f64> = energy
            .iter()
            .rev()
            .map(|e| {
                acc += e;
                acc
            })
            .collect();
        rest.reverse();
        let total = rest.first().copied().unwrap_or(0.0).max(1e-30);
        let decay_db = rest
            .iter()
            .map(|e| (10.0 * (e / total).max(1e-12).log10()) as f32)
            .collect();
        let mono: Vec<f32> = (0..len)
            .map(|i| {
                channels
                    .iter()
                    .map(|c| c.get(i).copied().unwrap_or(0.0))
                    .sum()
            })
            .collect();
        Self {
            name,
            seconds: len as f64 / rate,
            channels: channels.len(),
            envelope,
            decay_db,
            rt60: spaces::rt60(&mono, rate),
            problem: None,
        }
    }
}

/// The paths the engine plays for a response of `channels`.
fn routing_for(channels: usize) -> Routing {
    if channels >= 4 {
        Routing::true_stereo()
    } else {
        Routing::parallel()
    }
}

/// Where a response comes from (a new one makes a new engine).
#[derive(Clone, Copy, Debug, PartialEq)]
enum Source {
    Nothing,
    File(u64),
    Space(usize),
}

/// What makes a response: its source and how it is shaped.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Key {
    source: Source,
    shaping: Shaping,
}

/// What the audio thread hands back to be freed (held only to be dropped
/// on the designer's thread).
#[allow(dead_code)]
enum Retired {
    Kernels(Box<Kernels>),
    Engine(Box<Engine>),
}

/// The engine sent last.
#[derive(Clone, Copy)]
struct Sent {
    tag: u64,
    channels: usize,
    capacity: usize,
    source: Source,
}

/// The designer's side: makes responses from the parameters and the
/// loaded file.
struct Design {
    params: ParamValues,
    files: super::samples::Shared,
    tap: Arc<AnalysisTap>,
    rate: f64,
    planner: RealFftPlanner<f32>,
    engines: MailboxSender<Engine>,
    kernels: MailboxSender<Kernels>,
    retired: rtrb::Consumer<Retired>,
    /// The engine the audio thread plays (its tag).
    installed: Arc<AtomicU64>,
    /// Nothing to make (the last poll found what it had made).
    idle: Arc<AtomicBool>,
    last: Option<Key>,
    sent: Sent,
    next_tag: u64,
    /// What the editor was last shown.
    view: Option<Arc<ImpulseView>>,
}

/// A response made: its paths at the engine's rate and what to show.
struct Made {
    channels: Vec<Vec<f32>>,
    view: ImpulseView,
    capacity: usize,
}

impl Design {
    /// The key and the loaded file, read together (`None`: the file is
    /// being swapped this moment — the next poll looks again).
    fn read(&self) -> Option<(Key, Option<Arc<super::samples::SampleSet>>)> {
        let (set, generation) = {
            let g = self.files.lock_blocking(400)?;
            (g.set.clone(), g.generation)
        };
        let source = match space_of(&self.params) {
            0 if set.as_ref().is_some_and(|s| s.slot(0).is_some()) => Source::File(generation),
            0 => Source::Nothing,
            s => Source::Space(s - 1),
        };
        let key = Key {
            source,
            shaping: shaping(&self.params),
        };
        Some((key, set))
    }

    /// The response for `key` (`set`: the file loaded).
    fn make(&self, key: &Key, set: Option<&super::samples::SampleSet>) -> Made {
        let rate = self.rate;
        let most = (shape::MAX_SECONDS * rate) as usize;
        match key.source {
            Source::Space(i) => {
                let r = spaces::render(i, rate);
                let refs: Vec<&[f32]> = r.channels.iter().map(Vec::as_slice).collect();
                let channels = shape::shape(r.rate, &refs, &key.shaping, rate);
                let len = r.channels.first().map_or(0, Vec::len);
                let capacity =
                    (((len as f64) * rate / r.rate * shape::MAX_STRETCH) as usize).min(most);
                let view = ImpulseView::of(spaces::SPACES[i].name.into(), &channels, rate);
                Made {
                    channels,
                    view,
                    capacity,
                }
            }
            Source::File(_) => {
                let Some(sample) = set.and_then(|s| s.slot(0)) else {
                    return self.nothing(None);
                };
                let mut refs: Vec<&[f32]> = vec![&sample.data[0]];
                if sample.extra.len() >= 2 {
                    refs = vec![
                        &sample.data[0],
                        &sample.data[1],
                        &sample.extra[0],
                        &sample.extra[1],
                    ];
                } else if sample.stereo {
                    refs.push(&sample.data[1]);
                }
                let channels = shape::shape(sample.rate, &refs, &key.shaping, rate);
                let capacity = (((sample.frames as f64) * rate / sample.rate.max(1.0)
                    * shape::MAX_STRETCH) as usize)
                    .min(most);
                let view = ImpulseView::of(sample.name.clone(), &channels, rate);
                Made {
                    channels,
                    view,
                    capacity,
                }
            }
            Source::Nothing => {
                let problem = set.and_then(|s| s.errors.first().cloned());
                self.nothing(problem)
            }
        }
    }

    fn nothing(&self, problem: Option<String>) -> Made {
        Made {
            channels: vec![Vec::new()],
            view: ImpulseView {
                name: "No impulse response".into(),
                problem: Some(problem.unwrap_or_else(|| "Load an impulse response file".into())),
                ..ImpulseView::default()
            },
            capacity: 0,
        }
    }

    /// Kernels for the engine's paths from a response's channels.
    fn kernels(&mut self, channels: &[Vec<f32>]) -> Kernels {
        let paths: Vec<&[f32]> = match channels.len() {
            4 => channels.iter().map(Vec::as_slice).collect(),
            2 => vec![&channels[0], &channels[1]],
            _ => vec![&channels[0], &channels[0]],
        };
        Kernels::new(&paths, &mut self.planner)
    }

    fn publish(&self, made: &Made) {
        self.tap.set_value(value::SECONDS, made.view.seconds as f32);
        self.tap
            .set_value(value::CHANNELS, made.view.channels as f32);
        self.tap
            .set_value(value::DECAY, made.view.rt60.unwrap_or(0.0) as f32);
        self.tap.set_assets(Arc::new(made.view.clone()));
    }

    /// An engine for `key` (made now: the processor's first).
    fn engine_for(
        &mut self,
        key: &Key,
        set: Option<&super::samples::SampleSet>,
        tag: u64,
    ) -> Box<Engine> {
        let made = self.make(key, set);
        let kernels = Box::new(self.kernels(&made.channels));
        let channels = made.channels.len();
        let engine = Engine::new(
            routing_for(channels),
            made.capacity,
            kernels,
            &mut self.planner,
        );
        self.sent = Sent {
            tag,
            channels,
            capacity: engine.capacity(),
            source: key.source,
        };
        self.publish(&made);
        Box::new(engine)
    }

    /// Look at the parameters and the file; send what changed.
    fn poll(&mut self) {
        while let Ok(old) = self.retired.pop() {
            drop(old);
        }
        let Some((key, set)) = self.read() else {
            return;
        };
        self.idle.store(self.last == Some(key), Ordering::Release);
        if self.last == Some(key) {
            // The file's view may have been replaced by the sampler host's
            // (a document loaded that this space does not use).
            if self.tap.assets::<ImpulseView>().is_none()
                && let Some(view) = self.view.take()
            {
                self.tap.set_assets(view.clone());
                self.view = Some(view);
            }
            return;
        }
        self.tap.set_value(value::BUILDING, 1.0);
        // The same source reshaped is new kernels for the engine sent
        // last: wait until it plays (else they would go to one about to
        // be replaced).
        if key.source == self.sent.source && self.installed.load(Ordering::Acquire) != self.sent.tag
        {
            return;
        }
        let made = self.make(&key, set.as_deref());
        let channels = made.channels.len();
        let len = made.channels.iter().map(Vec::len).max().unwrap_or(0);
        let fits = key.source == self.sent.source
            && channels == self.sent.channels
            && len <= self.sent.capacity;
        if fits {
            // New kernels for the engine playing.
            let k = Box::new(self.kernels(&made.channels).tagged(self.sent.tag));
            drop(self.kernels.send(k));
        } else {
            self.next_tag += 1;
            let tag = self.next_tag;
            let kernels = Box::new(self.kernels(&made.channels));
            let engine = Engine::new(
                routing_for(channels),
                made.capacity.max(len),
                kernels,
                &mut self.planner,
            )
            .tagged(tag);
            self.sent = Sent {
                tag,
                channels,
                capacity: engine.capacity(),
                source: key.source,
            };
            drop(self.engines.send(Box::new(engine)));
        }
        self.publish(&made);
        self.view = Some(Arc::new(made.view));
        self.last = Some(key);
        self.tap.set_value(value::BUILDING, 0.0);
    }
}

const STEP: usize = 32;
/// Room for what a block can retire: an engine faded out, kernels
/// switched from, kernels refused.
const HELD: usize = 4;

pub struct ConvolutionProcessor {
    params: ParamValues,
    tap: Arc<AnalysisTap>,
    watching: Watching,
    sr: f64,
    engine: Box<Engine>,
    /// The engine being crossfaded out and how far (samples).
    old: Option<(Box<Engine>, usize)>,
    engines: MailboxReceiver<Engine>,
    kernels: MailboxReceiver<Kernels>,
    back: rtrb::Producer<Retired>,
    /// What could not be handed back yet (the ring was full): updates wait
    /// until it has gone.
    held: [Option<Retired>; HELD],
    installed: Arc<AtomicU64>,
    /// The designer has nothing left to make (tests wait on it).
    #[cfg_attr(not(test), allow(dead_code))]
    idle: Arc<AtomicBool>,
    _designer: Option<Designer>,
    pre: [DelayLine; 2],
    pre_samples: Smoothed,
    low_cut: Filter<2>,
    high_cut: Filter<2>,
    mix: Smoothed,
    output: Smoothed,
    width: Smoothed,
    meters: [[MeterTap; 2]; 2],
    dry: [Vec<f32>; 2],
    fed: [Vec<f32>; 2],
    wet: [Vec<f32>; 2],
    wet_old: [Vec<f32>; 2],
}

impl ConvolutionProcessor {
    /// `files`: the device's loaded file; `now`: make the first response
    /// here (else the designer makes it, and the wet signal waits).
    pub fn new(
        params: ParamValues,
        tap: Arc<AnalysisTap>,
        config: &ProcessConfig,
        files: super::samples::Shared,
        now: bool,
    ) -> Self {
        let sr = config.sample_rate.max(1.0);
        let block = config.max_block_size.max(1) as usize;
        let ((engines_tx, retired_rx), (engines_rx, back)) =
            convolve::channels::<Engine, Retired>();
        let (kernels_tx, kernels_rx) = faderframe_realtime::mailbox::<Kernels>();
        let installed = Arc::new(AtomicU64::new(0));
        let idle = Arc::new(AtomicBool::new(false));
        let mut design = Design {
            params: params.clone(),
            files,
            tap: Arc::clone(&tap),
            rate: sr,
            planner: RealFftPlanner::new(),
            engines: engines_tx,
            kernels: kernels_tx,
            retired: retired_rx,
            installed: Arc::clone(&installed),
            idle: Arc::clone(&idle),
            last: None,
            sent: Sent {
                tag: 0,
                channels: 2,
                capacity: 0,
                source: Source::Nothing,
            },
            next_tag: 0,
            view: None,
        };
        let engine = match design.read().filter(|_| now) {
            Some((key, set)) => {
                let e = design.engine_for(&key, set.as_deref(), 0);
                design.view = design.tap.assets::<ImpulseView>();
                design.last = Some(key);
                e
            }
            None => Box::new(Engine::silent(Routing::parallel(), &mut design.planner)),
        };
        let designer = Designer::spawn("faderframe-impulse", move || design.poll());
        let mt = MeterTap::new(sr as f32);
        let pre_len = (MAX_PRE_MS * 1e-3 * sr) as usize + 4;
        let mut p = Self {
            watching: Watching::new(sr as f32),
            sr,
            engine,
            old: None,
            engines: engines_rx,
            kernels: kernels_rx,
            back,
            held: [None, None, None, None],
            installed,
            idle,
            _designer: Some(designer),
            pre: [DelayLine::new(pre_len), DelayLine::new(pre_len)],
            pre_samples: Smoothed::new(0.0, 60.0, sr),
            low_cut: Filter::default(),
            high_cut: Filter::default(),
            mix: Smoothed::new(0.3, 20.0, sr),
            output: Smoothed::new(1.0, 20.0, sr),
            width: Smoothed::new(1.0, 20.0, sr),
            meters: [[mt; 2]; 2],
            dry: [vec![0.0; block], vec![0.0; block]],
            fed: [vec![0.0; block], vec![0.0; block]],
            wet: [vec![0.0; block], vec![0.0; block]],
            wet_old: [vec![0.0; block], vec![0.0; block]],
            params,
            tap,
        };
        p.control();
        p.pre_samples.snap();
        p.mix.snap();
        p.output.snap();
        p.width.snap();
        p
    }

    fn get(&self, pid: u32) -> f64 {
        get(&self.params, pid)
    }

    fn control(&mut self) {
        let sr = self.sr;
        self.pre_samples
            .set(self.get(id::PRE_DELAY).clamp(0.0, MAX_PRE_MS) * 1e-3 * sr);
        self.mix.set(self.get(id::MIX).clamp(0.0, 1.0));
        self.output.set(crate::dsp::gain(self.get(id::OUTPUT)));
        self.width.set(self.get(id::WIDTH).clamp(0.0, 1.5));
        let low = self.get(id::LOW_CUT);
        if low > 20.5 {
            self.low_cut.set(cut(BandType::LowCut, low), sr);
        } else {
            self.low_cut.clear();
        }
        let high = self.get(id::HIGH_CUT);
        if high < 19_500.0 && high < 0.45 * sr {
            self.high_cut.set(cut(BandType::HighCut, high), sr);
        } else {
            self.high_cut.clear();
        }
    }

    /// Hand something back to the designer to be freed (or hold it until
    /// the ring has room: never freed here).
    fn retire(&mut self, r: Retired) {
        if let Err(rtrb::PushError::Full(r)) = self.back.push(r) {
            match self.held.iter_mut().find(|h| h.is_none()) {
                Some(slot) => *slot = Some(r),
                // Updates are only taken with every slot free, and a block
                // retires fewer than there are: unreachable. Keep it rather
                // than free it on the audio thread.
                None => std::mem::forget(r),
            }
        }
    }

    /// Take what the designer sent: a new engine (when none is fading
    /// out), new kernels for the engine playing.
    fn take_updates(&mut self) {
        for i in 0..HELD {
            if let Some(h) = self.held[i].take()
                && let Err(rtrb::PushError::Full(h)) = self.back.push(h)
            {
                self.held[i] = Some(h);
            }
        }
        if self.held.iter().any(Option::is_some) {
            return;
        }
        if self.old.is_none()
            && let Some(e) = self.engines.take()
        {
            let old = std::mem::replace(&mut self.engine, e);
            self.installed.store(self.engine.tag(), Ordering::Release);
            self.old = Some((old, 0));
        }
        if !self.engine.switching()
            && let Some(k) = self.kernels.take()
        {
            // Only for the engine they were made for (one sent before a
            // new engine may arrive after it).
            if k.tag() != self.engine.tag() {
                self.retire(Retired::Kernels(k));
            } else if let Err(k) = self.engine.offer(k) {
                self.retire(Retired::Kernels(k));
            }
        }
        if let Some(k) = self.engine.take_retired() {
            self.retire(Retired::Kernels(k));
        }
    }
}

fn cut(kind: BandType, freq: f64) -> BandShape {
    BandShape {
        kind,
        freq,
        gain: 0.0,
        q: std::f64::consts::FRAC_1_SQRT_2,
        slope: 12.0,
    }
}

impl PluginProcessor for ConvolutionProcessor {
    fn process(&mut self, ctx: &PluginProcessContext<'_>, io: &mut NodeIo<'_>) -> ProcessStatus {
        let frames = io.frames;
        let watched = self.watching.check(&self.tap, frames);
        self.take_updates();
        let Some(channels) = pass_through(io) else {
            return ProcessStatus::Continue;
        };
        let Some(out) = io.audio_out.first_mut() else {
            return ProcessStatus::Continue;
        };
        if channels == 0 {
            return ProcessStatus::Continue;
        }
        let n = frames.min(self.dry[0].len());
        for c in 0..2 {
            let src = out.channel(c.min(channels - 1));
            self.dry[c][..n].copy_from_slice(&src[..n]);
        }
        let mut events = ctx.param_events.iter().peekable();
        let (mut in_peak, mut out_peak, mut wet_peak) = (0.0f32, 0.0f32, 0.0f32);
        let mut at = 0;
        while at < n {
            let end = (at + STEP).min(n);
            let mut moved = false;
            while let Some(e) = events.peek() {
                if (e.sample_offset as usize) < end {
                    self.params.apply_event(e.parameter, e.value);
                    events.next();
                    moved = true;
                } else {
                    break;
                }
            }
            if moved || at == 0 {
                self.control();
            }
            // Pre-delay into the convolution.
            for i in at..end {
                let d = self.pre_samples.tick();
                for c in 0..2 {
                    let x = f64::from(self.dry[c][i]);
                    in_peak = in_peak.max(x.abs() as f32);
                    let line = &mut self.pre[c];
                    line.push(x);
                    let y = if d < 1.0 {
                        line.tap(0) * (1.0 - d) + line.tap(1) * d
                    } else {
                        line.read(d)
                    };
                    self.fed[c][i] = y as f32;
                }
            }
            {
                let [wl, wr] = &mut self.wet;
                convolve::run(
                    &mut self.engine,
                    &[&self.fed[0][at..end], &self.fed[1][at..end]],
                    &mut [&mut wl[at..end], &mut wr[at..end]],
                );
            }
            // A new response comes in over the old.
            if let Some((old, done)) = &mut self.old {
                let [ol, or] = &mut self.wet_old;
                convolve::run(
                    old,
                    &[&self.fed[0][at..end], &self.fed[1][at..end]],
                    &mut [&mut ol[at..end], &mut or[at..end]],
                );
                for i in at..end {
                    let t = ((*done + i - at) as f32 / ENGINE_FADE as f32).min(1.0);
                    for c in 0..2 {
                        let (a, b) = (self.wet_old[c][i], self.wet[c][i]);
                        self.wet[c][i] = a + (b - a) * t;
                    }
                }
                *done += end - at;
            }
            if self.old.as_ref().is_some_and(|(_, d)| *d >= ENGINE_FADE)
                && let Some((old, _)) = self.old.take()
            {
                self.retire(Retired::Engine(old));
            }
            // After the convolution: cuts, width, level, mix.
            for i in at..end {
                let mix = self.mix.tick();
                let gain = self.output.tick();
                let width = self.width.tick();
                let mut w = [0.0f64; 2];
                for (c, wc) in w.iter_mut().enumerate() {
                    let v = f64::from(self.wet[c][i]);
                    *wc = self.high_cut.process(c, self.low_cut.process(c, v));
                }
                let mid = 0.5 * (w[0] + w[1]);
                let side = 0.5 * (w[0] - w[1]) * width;
                let w = [(mid + side) * gain, (mid - side) * gain];
                wet_peak = wet_peak.max(w[0].abs().max(w[1].abs()) as f32);
                let o = [
                    f64::from(self.dry[0][i]) * (1.0 - mix) + w[0] * mix,
                    f64::from(self.dry[1][i]) * (1.0 - mix) + w[1] * mix,
                ];
                out_peak = out_peak.max(o[0].abs().max(o[1].abs()) as f32);
                if channels == 1 {
                    out.channel_mut(0)[i] = (0.5 * (o[0] + o[1])) as f32;
                } else {
                    out.channel_mut(0)[i] = o[0] as f32;
                    out.channel_mut(1)[i] = o[1] as f32;
                }
            }
            at = end;
        }
        for e in events {
            self.params.apply_event(e.parameter, e.value);
        }
        self.low_cut.flush();
        self.high_cut.flush();
        for c in 2..channels {
            out.channel_mut(c).fill(0.0);
        }
        self.tap.raise_value(value::IN_PEAK, in_peak);
        self.tap.raise_value(value::OUT_PEAK, out_peak);
        self.tap.raise_value(value::WET_PEAK, wet_peak);
        for c in 0..2 {
            let o = out.channel(c.min(channels - 1));
            for (x, y) in self.dry[c][..n].iter().zip(&o[..n]) {
                self.meters[0][c].add(*x);
                self.meters[1][c].add(*y);
            }
            self.meters[0][c].publish(&self.tap.meter_in, c, n);
            self.meters[1][c].publish(&self.tap.meter_out, c, n);
        }
        if watched {
            self.tap.input.push(&self.dry[0][..n], &self.dry[1][..n]);
            let r = out.channel(1.min(channels - 1));
            self.tap.output.push(&out.channel(0)[..n], &r[..n]);
        }
        ProcessStatus::Continue
    }

    fn reset(&mut self) {
        self.engine.reset();
        if let Some((old, _)) = self.old.take() {
            self.retire(Retired::Engine(old));
        }
        for p in &mut self.pre {
            p.reset();
        }
        self.low_cut.reset();
        self.high_cut.reset();
        self.meters.iter_mut().flatten().for_each(MeterTap::reset);
    }
}

#[cfg(test)]
mod tests;
