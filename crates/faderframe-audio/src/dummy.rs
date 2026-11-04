//! A hardware-less backend driven by a timer thread.
//!
//! It calls the engine at the real-time rate implied by the configured
//! sample rate and buffer size, feeding silence (or a test tone, see
//! [`DummyBackend::with_input_tone`]) and discarding output. This keeps the
//! transport, meters and DSP load display alive on machines without an
//! audio server, and is used in CI and recording tests.
//!
//! The thread sleeps *between* callbacks to emulate a device clock; nothing
//! sleeps inside the processing callback itself.
//!
//! [`DummyDevices`] stands in for a machine's interfaces in tests: devices
//! with their own rates, buffer sizes and channels that can be unplugged
//! (a stream on one shuts down, like a USB interface pulled out) and
//! plugged in again. A stream on several of them makes the first output
//! device the clock master and the others followers ([`crate::follow`]),
//! each on a clock of its own ([`DummyDevices::set_clock`]: parts per
//! million off, jitter, its callbacks' size) and a thread of its own; an
//! unplugged follower only silences its channels and joins again when it is
//! plugged back. [`DummyBackend::simulated`] runs master and followers on a
//! simulated clock instead, as fast as the machine allows up to a speed, so
//! that tests can play minutes in seconds.

use crate::follow::{self, Direction, FollowerEnd, FollowerSpec, Followers, Joined, Place};
use crate::{
    AudioBackend, AudioCallback, AudioError, AudioStream, Capabilities, DeviceBuffers, DeviceInfo,
    DeviceStatus, OwnedBuffers, RateControl, STANDARD_SAMPLE_RATES, StreamConfig, StreamInfo,
    StreamMonitor, StreamRouting, StreamStatus, validate_format,
};
use std::f64::consts::TAU;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

/// How far the device clock may fall behind before it gives up catching up.
const CATCH_UP: Duration = Duration::from_millis(500);
/// How often a follower that went away is looked for.
const WATCH_EVERY: Duration = Duration::from_millis(50);
/// A pulse of [`DummyBackend::with_input_pulses`]: a raised cosine this
/// long (seconds).
pub const PULSE_SECONDS: f64 = 0.002;

pub const DEFAULT_SAMPLE_RATE: u32 = 48_000;
pub const DEFAULT_BUFFER_SIZE: u32 = 256;

/// A pretend device and its presence (cleared when it is unplugged; its
/// streams watch it).
type Plugged = (DeviceInfo, Arc<AtomicBool>);

/// How a pretend device's clock runs (it matters when it follows another).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct PretendClock {
    /// Parts per million fast (positive) or slow against the master's.
    pub ppm: f64,
    /// Its callbacks come up to this many seconds early or late.
    pub jitter: f64,
    /// Frames per callback when it follows (`None`: the stream's buffer
    /// size, within what it takes).
    pub block: Option<u32>,
}

/// Pretend interfaces for a [`DummyBackend`], shared with the test that
/// plugs and unplugs them.
#[derive(Clone, Debug, Default)]
pub struct DummyDevices {
    list: Arc<Mutex<Vec<Plugged>>>,
    clocks: Arc<Mutex<Vec<(String, PretendClock)>>>,
}

fn locked<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

impl DummyDevices {
    pub fn new(devices: Vec<DeviceInfo>) -> Self {
        let this = Self::default();
        for d in devices {
            this.plug(d);
        }
        this
    }

    /// A device appears (again).
    pub fn plug(&self, device: DeviceInfo) {
        let mut list = locked(&self.list);
        list.retain(|(d, _)| d.id != device.id);
        list.push((device, Arc::new(AtomicBool::new(true))));
    }

    /// A device goes away: streams on it shut down (followers only fall
    /// silent).
    pub fn unplug(&self, id: &str) {
        let mut list = locked(&self.list);
        list.retain(|(d, present)| {
            if d.id == id {
                present.store(false, Ordering::Relaxed);
            }
            d.id != id
        });
    }

    pub fn list(&self) -> Vec<DeviceInfo> {
        locked(&self.list).iter().map(|(d, _)| d.clone()).collect()
    }

    /// How device `id`'s clock runs (it keeps it while unplugged).
    pub fn set_clock(&self, id: &str, clock: PretendClock) {
        let mut clocks = locked(&self.clocks);
        clocks.retain(|(d, _)| d != id);
        clocks.push((id.to_string(), clock));
    }

    pub fn clock(&self, id: &str) -> PretendClock {
        locked(&self.clocks)
            .iter()
            .find(|(d, _)| d == id)
            .map_or_else(PretendClock::default, |(_, c)| *c)
    }

    /// A device by id (or name), with its presence flag.
    fn find(&self, wanted: &str) -> Option<Plugged> {
        let list = locked(&self.list);
        list.iter()
            .find(|(d, _)| d.id == wanted)
            .or_else(|| list.iter().find(|(d, _)| d.name == wanted))
            .cloned()
    }

    /// The default device of a direction.
    fn default_for(&self, output: bool) -> Option<Plugged> {
        let list = locked(&self.list);
        let has = |d: &DeviceInfo| {
            if output {
                d.output_channels > 0
            } else {
                d.input_channels > 0
            }
        };
        list.iter()
            .find(|(d, _)| d.is_default && has(d))
            .or_else(|| list.iter().find(|(d, _)| has(d)))
            .cloned()
    }
}

/// A pretend interface: `rates` and `buffers` are what it takes (empty /
/// `None`: anything), the first rate its current one.
pub fn mock_device(
    id: &str,
    inputs: u16,
    outputs: u16,
    rates: &[u32],
    buffers: Option<(u32, u32)>,
) -> DeviceInfo {
    DeviceInfo {
        id: id.into(),
        name: id.into(),
        input_channels: inputs,
        output_channels: outputs,
        input_names: (1..=inputs).map(|n| format!("In {n}")).collect(),
        output_names: (1..=outputs).map(|n| format!("Out {n}")).collect(),
        sample_rates: rates.to_vec(),
        current_sample_rate: rates.first().copied(),
        current_buffer_size: None,
        buffer_sizes: buffers,
        clock: Some(id.into()),
        is_default: false,
    }
}

/// What the pretend inputs carry.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
enum Kind {
    #[default]
    Silence,
    /// A steady 0.5-amplitude sine.
    Tone(f32),
    /// Plucked notes (twice a second, odd inputs an octave up).
    Pluck(f32),
    /// Input n `n × semitones` above input 0, from one second in.
    Spread(f32, f32),
    /// A short pulse every so many seconds of the device's clock.
    Pulses(f64),
}

/// A pretend device's inputs, block by block.
struct Signal {
    kind: Kind,
    /// Frames a second it is timed by (tones, plucks).
    rate: f64,
    /// Pulses: the device clock's time at frame 0 (seconds) and its true
    /// frames a second.
    start: f64,
    true_rate: f64,
    /// The stream input its first channel is (spread tones climb by it,
    /// plucks go up an octave on odd ones).
    first: usize,
    phase: f64,
    phases: Vec<f64>,
    frames: u64,
}

impl Signal {
    fn new(kind: Kind, rate: f64, channels: usize, first: usize) -> Self {
        Self {
            kind,
            rate,
            start: 0.0,
            true_rate: rate,
            first,
            phase: 0.0,
            phases: vec![0.0; channels],
            frames: 0,
        }
    }

    /// The pulse train at `t` seconds.
    fn pulse(every: f64, t: f64) -> f32 {
        if every <= 0.0 || t < every * 0.5 {
            return 0.0;
        }
        let d = t - (t / every).round() * every;
        if d.abs() >= PULSE_SECONDS * 0.5 {
            return 0.0;
        }
        (0.4 * (1.0 + (TAU * d / PULSE_SECONDS).cos())) as f32
    }

    /// The next `n` frames of `channels` channels: sample `(channel,
    /// frame)` to `put`.
    fn fill(&mut self, channels: usize, n: usize, mut put: impl FnMut(usize, usize, f32)) {
        let sr = self.rate;
        match self.kind {
            Kind::Silence => {
                for c in 0..channels {
                    for i in 0..n {
                        put(c, i, 0.0);
                    }
                }
            }
            Kind::Spread(hz, spread) => {
                let begin = sr as u64;
                for c in 0..channels {
                    let Some(ph) = self.phases.get_mut(c) else {
                        break;
                    };
                    let k = (self.first + c) as f64;
                    let f = f64::from(hz) * 2f64.powf(k * f64::from(spread) / 12.0);
                    let inc = TAU * f / sr;
                    for i in 0..n {
                        if self.frames + (i as u64) < begin {
                            put(c, i, 0.0);
                            continue;
                        }
                        put(c, i, (ph.sin() * 0.5) as f32);
                        *ph = (*ph + inc) % TAU;
                    }
                }
            }
            Kind::Tone(hz) | Kind::Pluck(hz) => {
                let pluck = matches!(self.kind, Kind::Pluck(_));
                let inc = TAU * f64::from(hz) / sr;
                let start = self.phase;
                for c in 0..channels {
                    let mut ph = start;
                    let octave = if pluck && (self.first + c) % 2 == 1 {
                        2.0
                    } else {
                        1.0
                    };
                    for i in 0..n {
                        let env = if pluck {
                            let x = ((self.frames + i as u64) as f64 / sr) % 0.5;
                            0.05 + 0.8 * (-x * 9.0).exp()
                        } else {
                            1.0
                        };
                        put(c, i, ((ph * octave).sin() * 0.5 * env) as f32);
                        ph += inc;
                    }
                    self.phase = ph % TAU;
                }
            }
            Kind::Pulses(every) => {
                for i in 0..n {
                    let t = self.start + (self.frames + i as u64) as f64 / self.true_rate;
                    let v = Self::pulse(every, t);
                    for c in 0..channels {
                        put(c, i, v);
                    }
                }
            }
        }
        self.frames += n as u64;
    }
}

#[derive(Clone, Debug, Default)]
pub struct DummyBackend {
    /// What the inputs carry.
    signal: Kind,
    /// A cable from every output to the input of the same number, `n`
    /// frames longer than the device's own buffer (hardware inserts
    /// without hardware).
    loopback: Option<u32>,
    /// Pretend interfaces (`None`: the one silent device).
    devices: Option<DummyDevices>,
    /// Run on a simulated clock at up to this many times real time.
    simulated: Option<f64>,
}

impl DummyBackend {
    /// Inputs carry a steady 0.5-amplitude sine of `hz` instead of silence.
    pub fn with_input_tone(hz: f32) -> Self {
        Self {
            signal: Kind::Tone(hz),
            ..Self::default()
        }
    }

    /// Input n carries a steady sine `n × semitones` above `hz` (several
    /// singers, one on each input), from one second into the stream (a
    /// test can listen and start recording before the first note).
    pub fn with_input_spread(hz: f32, semitones: f32) -> Self {
        Self {
            signal: if semitones == 0.0 {
                Kind::Tone(hz)
            } else {
                Kind::Spread(hz, semitones)
            },
            ..Self::default()
        }
    }

    /// Outputs come back on the inputs of the same number (a patch cable
    /// on every channel), `delay` frames after the next callback's start:
    /// a round trip of one buffer plus `delay`.
    pub fn with_loopback(delay: u32) -> Self {
        Self {
            loopback: Some(delay),
            ..Self::default()
        }
    }

    /// Inputs carry plucked notes of `hz` (decaying, twice a second, input
    /// 2 an octave up) — a test signal with a recognisable waveform.
    pub fn with_input_pluck(hz: f32) -> Self {
        Self {
            signal: Kind::Pluck(hz),
            ..Self::default()
        }
    }

    /// Inputs carry a short pulse ([`PULSE_SECONDS`], a raised cosine of
    /// 0.8 peak) every `seconds` of each device's clock: on a simulated
    /// clock ([`Self::simulated`]) the master's and every follower's
    /// pulses are captured at the same moments, so recordings of them
    /// show how each device's latency is compensated.
    pub fn with_input_pulses(seconds: f64) -> Self {
        Self {
            signal: Kind::Pulses(seconds),
            ..Self::default()
        }
    }

    /// Offer these pretend interfaces instead of the one silent device:
    /// streams open on one of them (by id), refuse rates and buffer sizes
    /// it does not take, and shut down when it is unplugged.
    pub fn with_devices(devices: DummyDevices) -> Self {
        Self::default().devices(devices)
    }

    /// The pretend interfaces, keeping the signal.
    pub fn devices(mut self, devices: DummyDevices) -> Self {
        self.devices = Some(devices);
        self
    }

    /// Run the device (and its followers) on a simulated clock at up to
    /// `speed` times real time: the callbacks come as fast as the
    /// callback and the machine allow, up to that, and the followers'
    /// callbacks fall between the master's by their simulated clocks
    /// (deterministic: what a test measures does not depend on the
    /// machine's load). Followers' bookkeeping runs on the device's
    /// thread then.
    pub fn simulated(mut self, speed: f64) -> Self {
        self.simulated = Some(speed.max(0.01));
        self
    }

    /// The pretend device a stream opens on for a direction.
    fn pick(&self, wanted: &[String], output: bool) -> Result<Option<Plugged>, AudioError> {
        let Some(devices) = &self.devices else {
            return Ok(None);
        };
        match wanted.first() {
            Some(id) => devices
                .find(id)
                .map(Some)
                .ok_or_else(|| AudioError::DeviceNotFound(id.clone())),
            None => Ok(devices.default_for(output)),
        }
    }
}

impl AudioBackend for DummyBackend {
    fn id(&self) -> &'static str {
        "dummy"
    }

    fn display_name(&self) -> &'static str {
        "No audio device (silent)"
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            rate: RateControl::Device,
            live_buffer_size: true,
            live_sample_rate: false,
            // Pretend interfaces join several: the first output's clock
            // drives, the others follow.
            output_devices: if self.devices.is_some() { 4 } else { 1 },
            input_devices: if self.devices.is_some() { 4 } else { 1 },
            clocked_by_server: false,
            resamples_followers: self.devices.is_some(),
            follows_devices: false,
            free_channel_counts: true,
            force_format: false,
        }
    }

    fn is_available(&self) -> bool {
        true
    }

    fn enumerate_devices(&self) -> Result<Vec<DeviceInfo>, AudioError> {
        if let Some(devices) = &self.devices {
            return Ok(devices.list());
        }
        Ok(vec![DeviceInfo {
            id: "dummy".into(),
            name: "Silent dummy device".into(),
            // Any count opens; these are what a stream opens by default.
            input_channels: 8,
            output_channels: 2,
            sample_rates: STANDARD_SAMPLE_RATES.to_vec(),
            current_sample_rate: None,
            current_buffer_size: None,
            is_default: true,
            ..DeviceInfo::default()
        }])
    }

    fn open_stream(
        &mut self,
        config: StreamConfig,
        callback: Box<dyn AudioCallback>,
    ) -> Result<Box<dyn AudioStream>, AudioError> {
        let output = self.pick(&config.output_devices, true)?;
        let plan = match (&self.devices, &output) {
            (Some(devices), Some(master)) => Some(plan_devices(devices, master, &config)?),
            _ => None,
        };
        // The output device records where it can.
        let input = match output.clone().filter(|(d, _)| d.input_channels > 0) {
            Some(same) if config.input_devices.is_empty() => Some(same),
            _ if plan.is_some() => None,
            _ => self.pick(&config.input_devices, false)?,
        };
        let device = output.as_ref().map(|(d, _)| d);
        let sample_rate = config
            .sample_rate
            .or(device.and_then(|d| d.current_sample_rate))
            .unwrap_or(DEFAULT_SAMPLE_RATE);
        let buffer_size = config
            .buffer_size
            .or(device.and_then(|d| d.current_buffer_size))
            .unwrap_or(DEFAULT_BUFFER_SIZE);
        validate_format(sample_rate, buffer_size)?;
        // The devices on the master's clock must take its format; the
        // followers run at their own.
        for (d, _) in output.iter().chain(input.iter()) {
            if !d.sample_rates.is_empty() && !d.sample_rates.contains(&sample_rate) {
                return Err(AudioError::UnsupportedConfig(format!(
                    "{} cannot run at {sample_rate} Hz",
                    d.name
                )));
            }
            if let Some((lo, hi)) = d.buffer_sizes
                && !(lo..=hi).contains(&buffer_size)
            {
                return Err(AudioError::UnsupportedConfig(format!(
                    "{} takes buffers of {lo}–{hi} frames, not {buffer_size}",
                    d.name
                )));
            }
        }
        let present: Vec<Arc<AtomicBool>> = output
            .iter()
            .chain(input.iter())
            .map(|(_, p)| Arc::clone(p))
            .collect();
        let buffers = output
            .iter()
            .chain(input.iter())
            .filter_map(|(d, _)| d.buffer_sizes)
            .fold(None, |acc: Option<(u32, u32)>, (lo, hi)| {
                Some(acc.map_or((lo, hi), |(a, b)| (a.max(lo), b.min(hi))))
            });
        let names: Vec<String> = output
            .iter()
            .map(|(d, _)| d.name.clone())
            .chain(
                plan.iter()
                    .flat_map(|p| p.specs.iter().map(|s| s.name.clone())),
            )
            .chain(input.iter().map(|(d, _)| d.name.clone()))
            .fold(Vec::new(), |mut v, n| {
                if !v.contains(&n) {
                    v.push(n);
                }
                v
            });
        let info = StreamInfo {
            backend: "dummy",
            // The clock master's name (its followers are in `devices()`
            // and the routing).
            device: device.map_or_else(|| "Silent dummy device".into(), |d| d.name.clone()),
            sample_rate,
            buffer_size,
            input_channels: config.input_channels,
            output_channels: config.output_channels,
            input_latency: 0,
            output_latency: 0,
        };
        let routing = output.as_ref().map(|(o, _)| {
            let name = |d: &DeviceInfo, names: &[String], c: usize| {
                names.get(c).map(|n| format!("{} · {n}", d.name))
            };
            StreamRouting {
                outputs: (0..config.output_channels as usize)
                    .map(|c| name(o, &o.output_names, c))
                    .collect(),
                inputs: (0..config.input_channels as usize)
                    .map(|c| input.as_ref().and_then(|(i, _)| name(i, &i.input_names, c)))
                    .collect(),
                missing: Vec::new(),
                devices: names.clone(),
            }
        });
        let monitor = StreamMonitor::new(sample_rate, buffer_size);
        let stop = Arc::new(AtomicBool::new(false));
        let requested = Arc::new(AtomicU32::new(buffer_size));
        // With followers: the engine's callback joined to theirs, the master
        // device's own buffers as many as its share.
        let (driven, master_ins, master_outs, joined) = match &plan {
            Some(p) if !p.specs.is_empty() => {
                let (joined, followers, ends) = follow::join(
                    callback,
                    &info,
                    p.inputs.clone(),
                    p.outputs.clone(),
                    p.specs.clone(),
                );
                (
                    Driven::Joined(Box::new(joined)),
                    p.master_inputs,
                    p.master_outputs,
                    Some((Arc::new(Mutex::new(followers)), ends)),
                )
            }
            _ => (
                Driven::Plain(callback),
                config.input_channels,
                config.output_channels,
                None,
            ),
        };
        let pretend = plan.as_ref().and_then(|_| self.devices.clone());
        let mut master_info = info.clone();
        master_info.input_channels = master_ins;
        master_info.output_channels = master_outs;
        let (followers, ends) = match joined {
            Some((f, ends)) => (Some(f), ends),
            None => (None, Vec::new()),
        };
        let simulated = self.simulated;
        // The pretend followers: their device ends, signals and clocks.
        let mut pretend_followers = Vec::new();
        if let (Some(plan), Some(devices)) = (&plan, &pretend) {
            for (k, end) in ends.into_iter().enumerate() {
                let spec = &plan.specs[k];
                let Some((_, present)) = devices.find(&spec.device) else {
                    continue;
                };
                pretend_followers.push(Some(PretendFollower::new(
                    k,
                    spec,
                    end,
                    devices.clock(&spec.device),
                    present,
                    self.signal,
                    plan.first_input(k),
                    0.0,
                )));
            }
        }
        let master = MasterRun {
            driven,
            info: master_info,
            signal: Signal::new(
                self.signal,
                f64::from(sample_rate),
                usize::from(master_ins),
                0,
            ),
            loopback: self.loopback,
            monitor: Arc::clone(&monitor),
            stop: Arc::clone(&stop),
            requested: Arc::clone(&requested),
            present,
        };
        let mut threads = Vec::new();
        match simulated {
            Some(speed) => {
                let sim = Simulated {
                    followers: pretend_followers,
                    control: followers.clone(),
                    devices: pretend.clone(),
                    plan: plan.clone(),
                    signal: self.signal,
                };
                threads.push(spawn("faderframe-dummy-audio", move || {
                    master.run_simulated(speed, sim)
                })?);
            }
            None => {
                let watch = Watch {
                    stop: Arc::clone(&stop),
                    control: followers.clone(),
                    devices: pretend.clone(),
                    plan: plan.clone(),
                    signal: self.signal,
                };
                threads.push(spawn("faderframe-dummy-audio", move || master.run())?);
                if followers.is_some() {
                    threads.push(spawn("faderframe-dummy-watch", move || {
                        watch.run(pretend_followers)
                    })?);
                }
            }
        }
        Ok(Box::new(DummyStream {
            info,
            monitor,
            stop,
            requested,
            buffers,
            routing,
            followers,
            plan,
            pretend,
            threads,
        }))
    }
}

fn spawn(name: &str, f: impl FnOnce() + Send + 'static) -> Result<JoinHandle<()>, AudioError> {
    std::thread::Builder::new()
        .name(name.into())
        .spawn(f)
        .map_err(|e| AudioError::Stream(format!("cannot spawn {name}: {e}")))
}

/// How a stream on pretend devices is laid out: the master's channels and
/// the followers'.
#[derive(Clone, Debug)]
struct Plan {
    master: DeviceInfo,
    inputs: Vec<Place>,
    outputs: Vec<Place>,
    specs: Vec<FollowerSpec>,
    master_inputs: u16,
    master_outputs: u16,
}

impl Plan {
    /// The stream input follower `k`'s first channel is.
    fn first_input(&self, k: usize) -> usize {
        self.inputs
            .iter()
            .position(
                |p| matches!(p, Place::Follower { follower, .. } if usize::from(*follower) == k),
            )
            .unwrap_or(0)
    }
}

/// Lay out a stream on pretend devices: the master's outputs first, then
/// the other output devices' (followers); inputs in the order asked for —
/// the master's own where it is one of them, followers for the rest —, or
/// the master's own (else the default input device) when none is asked
/// for. Channels the stream has beyond the devices' are the master's.
fn plan_devices(
    devices: &DummyDevices,
    master: &Plugged,
    config: &StreamConfig,
) -> Result<Plan, AudioError> {
    let (m, _) = master;
    let rate = config
        .sample_rate
        .or(m.current_sample_rate)
        .unwrap_or(DEFAULT_SAMPLE_RATE);
    let buffer = config
        .buffer_size
        .or(m.current_buffer_size)
        .unwrap_or(DEFAULT_BUFFER_SIZE);
    let find = |id: &String| {
        devices
            .find(id)
            .map(|(d, _)| d)
            .ok_or_else(|| AudioError::DeviceNotFound(id.clone()))
    };
    let mut plan = Plan {
        master: m.clone(),
        inputs: Vec::new(),
        outputs: Vec::new(),
        specs: Vec::new(),
        master_inputs: 0,
        master_outputs: 0,
    };
    let follower = |plan: &mut Plan, d: &DeviceInfo, direction: Direction| {
        let clock = devices.clock(&d.id);
        let sample_rate = if d.sample_rates.is_empty() || d.sample_rates.contains(&rate) {
            rate
        } else {
            d.current_sample_rate
                .or(d.sample_rates.first().copied())
                .unwrap_or(rate)
        };
        let block = clock.block.unwrap_or_else(|| match d.buffer_sizes {
            Some((lo, hi)) => buffer.clamp(lo, hi.max(lo)),
            None => buffer,
        });
        let channels = match direction {
            Direction::Input => d.input_channels,
            Direction::Output => d.output_channels,
        };
        let k = plan.specs.len() as u16;
        plan.specs.push(FollowerSpec {
            device: d.id.clone(),
            name: d.name.clone(),
            direction,
            channels,
            sample_rate,
            block,
        });
        let places = (0..channels).map(|channel| Place::Follower {
            follower: k,
            channel,
        });
        match direction {
            Direction::Input => plan.inputs.extend(places),
            Direction::Output => plan.outputs.extend(places),
        }
    };
    for _ in 0..m.output_channels {
        plan.outputs.push(Place::Master(plan.master_outputs));
        plan.master_outputs += 1;
    }
    let mut seen = vec![m.id.clone()];
    for id in config.output_devices.iter().skip(1) {
        let d = find(id)?;
        if !seen.contains(&d.id) {
            seen.push(d.id.clone());
            follower(&mut plan, &d, Direction::Output);
        }
    }
    let inputs: Vec<DeviceInfo> = if config.input_devices.is_empty() {
        if m.input_channels > 0 {
            vec![m.clone()]
        } else {
            devices
                .default_for(false)
                .map(|(d, _)| d)
                .into_iter()
                .collect()
        }
    } else {
        config
            .input_devices
            .iter()
            .map(find)
            .collect::<Result<_, _>>()?
    };
    let mut seen: Vec<String> = Vec::new();
    for d in inputs {
        if seen.contains(&d.id) {
            continue;
        }
        seen.push(d.id.clone());
        if d.id == m.id {
            for _ in 0..d.input_channels {
                plan.inputs.push(Place::Master(plan.master_inputs));
                plan.master_inputs += 1;
            }
        } else {
            follower(&mut plan, &d, Direction::Input);
        }
    }
    // What the stream has beyond the devices' channels plays on (records
    // from) the master: silent channels of its own.
    let fill = |places: &mut Vec<Place>, master: &mut u16, wanted: u16| {
        while places.len() < usize::from(wanted) {
            places.push(Place::Master(*master));
            *master += 1;
        }
        places.truncate(usize::from(wanted));
    };
    fill(
        &mut plan.inputs,
        &mut plan.master_inputs,
        config.input_channels,
    );
    fill(
        &mut plan.outputs,
        &mut plan.master_outputs,
        config.output_channels,
    );
    Ok(plan)
}

/// What the device thread calls: the engine's callback, or it joined to
/// followers.
enum Driven {
    Plain(Box<dyn AudioCallback>),
    Joined(Box<Joined>),
}

impl Driven {
    fn prepare(&mut self, info: &StreamInfo) {
        match self {
            Driven::Plain(c) => c.prepare(info),
            Driven::Joined(j) => j.prepare(info),
        }
    }

    /// One callback; on a simulated clock at `at` ns.
    fn process(&mut self, io: &mut dyn DeviceBuffers, at: Option<u64>) {
        match (self, at) {
            (Driven::Plain(c), _) => c.process(io),
            (Driven::Joined(j), Some(at)) => j.process_at(io, at),
            (Driven::Joined(j), None) => j.process(io),
        }
    }
}

/// A pretend follower's device side: its end of the link, its inputs'
/// signal and its clock.
struct PretendFollower {
    index: usize,
    end: FollowerEnd,
    signal: Signal,
    channels: usize,
    block: usize,
    /// Seconds a callback on its own clock.
    period: f64,
    jitter: f64,
    rng: u64,
    present: Arc<AtomicBool>,
    /// One block of its inputs (or of what it plays).
    scratch: Vec<Vec<f32>>,
}

impl PretendFollower {
    #[allow(clippy::too_many_arguments)]
    fn new(
        index: usize,
        spec: &FollowerSpec,
        end: FollowerEnd,
        clock: PretendClock,
        present: Arc<AtomicBool>,
        signal: Kind,
        first: usize,
        start: f64,
    ) -> Self {
        let channels = usize::from(spec.channels);
        let block = spec.block.max(1) as usize;
        let rate = f64::from(spec.sample_rate.max(1));
        let true_rate = rate * (1.0 + clock.ppm * 1e-6);
        let mut signal = Signal::new(signal, rate, channels, first);
        signal.start = start;
        signal.true_rate = true_rate;
        Self {
            index,
            end,
            signal,
            channels,
            block,
            period: block as f64 / true_rate,
            jitter: clock.jitter,
            rng: 0x9e37_79b9_7f4a_7c15 ^ index as u64,
            present,
            scratch: vec![vec![0.0; block]; channels],
        }
    }

    /// Uniform in −1…1 (deterministic).
    fn wobble(&mut self) -> f64 {
        self.rng = self
            .rng
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        ((self.rng >> 11) as f64 / (1u64 << 53) as f64) * 2.0 - 1.0
    }

    /// One callback, stamped `now_ns`.
    fn callback(&mut self, now_ns: u64) {
        let n = self.block;
        match &mut self.end {
            FollowerEnd::Input(e) => {
                let scratch = &mut self.scratch;
                self.signal
                    .fill(self.channels, n, |c, i, v| scratch[c][i] = v);
                e.capture_with(n, now_ns, |i, c| scratch[c][i]);
            }
            FollowerEnd::Output(e) => {
                let scratch = &mut self.scratch;
                e.play_with(n, now_ns, |i, c, v| scratch[c][i] = v);
            }
        }
    }

    /// On its own thread at its own pace until the stream stops or the
    /// device goes away.
    fn run(mut self, stop: Arc<AtomicBool>) {
        let start = Instant::now();
        let mut next = self.period;
        while !stop.load(Ordering::Relaxed) && self.present.load(Ordering::Relaxed) {
            faderframe_realtime::priority::follow_audio(
                faderframe_realtime::priority::Helper::Near,
            );
            let due =
                start + Duration::from_secs_f64((next + self.jitter * self.wobble()).max(0.0));
            let now = Instant::now();
            if due > now {
                std::thread::sleep(due - now);
            }
            self.callback(follow::now_ns());
            next += self.period;
            if Instant::now() > start + Duration::from_secs_f64(next) + CATCH_UP {
                // Fell far behind (suspend, debugger): carry on from now.
                next = start.elapsed().as_secs_f64() + self.period;
            }
        }
    }
}

/// The master device's thread.
struct MasterRun {
    driven: Driven,
    /// The master device's own buffers' layout and format.
    info: StreamInfo,
    signal: Signal,
    loopback: Option<u32>,
    monitor: Arc<StreamMonitor>,
    stop: Arc<AtomicBool>,
    requested: Arc<AtomicU32>,
    /// The master device's presence (and that of devices on its clock).
    present: Vec<Arc<AtomicBool>>,
}

/// The loopback cable: a ring per channel.
struct Cable {
    rings: Vec<Vec<f32>>,
    len: usize,
    write: usize,
    read: usize,
}

impl MasterRun {
    fn cable(&self) -> Option<Cable> {
        // Output at stream frame n is input at n + buffer + delay: the
        // write head that far ahead of the read head (for the buffer size
        // the stream starts with).
        let d = self.loopback? as usize;
        let ahead = d + self.info.buffer_size as usize;
        let len = ahead + 16_384;
        let ch = usize::from(self.info.input_channels.min(self.info.output_channels));
        Some(Cable {
            rings: vec![vec![0.0f32; len]; ch],
            len,
            write: ahead,
            read: 0,
        })
    }

    /// One block: inputs filled, the callback, the cable fed. `at`: the
    /// simulated clock (ns), if any.
    fn block(&mut self, bufs: &mut OwnedBuffers, cable: &mut Option<Cable>, at: Option<u64>) {
        let n = self.info.buffer_size as usize;
        bufs.set_frames(n);
        let ins = usize::from(self.info.input_channels);
        if self.signal.kind != Kind::Silence {
            self.signal.fill(ins, n, |c, i, v| bufs.input_mut(c)[i] = v);
        }
        if let Some(cable) = cable.as_mut() {
            for (c, ring) in cable.rings.iter().enumerate() {
                for (i, s) in bufs.input_mut(c).iter_mut().enumerate() {
                    *s = ring[(cable.read + i) % cable.len];
                }
            }
            cable.read = (cable.read + n) % cable.len.max(1);
        }
        self.driven.process(bufs, at);
        if let Some(cable) = cable.as_mut() {
            for (c, ring) in cable.rings.iter_mut().enumerate() {
                for (i, s) in bufs.output(c).iter().enumerate().take(n) {
                    ring[(cable.write + i) % cable.len] = *s;
                }
            }
            cable.write = (cable.write + n) % cable.len.max(1);
        }
        self.monitor.record_callback();
    }

    /// A pretend interface pulled out: the stream ends as a real one
    /// would. A buffer size asked for: taken.
    fn check(&mut self, bufs: &mut OwnedBuffers) -> bool {
        if self.present.iter().any(|p| !p.load(Ordering::Relaxed)) {
            self.monitor.mark_shut_down();
            return false;
        }
        let want = self.requested.load(Ordering::Relaxed);
        if want != self.info.buffer_size {
            self.info.buffer_size = want;
            *bufs = OwnedBuffers::new(
                usize::from(self.info.input_channels),
                usize::from(self.info.output_channels),
                want as usize,
            );
            self.monitor.set_buffer_size(want);
            self.driven.prepare(&self.info);
        }
        true
    }

    fn start(&mut self) -> (OwnedBuffers, Option<Cable>) {
        let bufs = OwnedBuffers::new(
            usize::from(self.info.input_channels),
            usize::from(self.info.output_channels),
            self.info.buffer_size as usize,
        );
        self.driven.prepare(&self.info);
        self.monitor.set_running(true);
        let cable = self.cable();
        (bufs, cable)
    }

    /// Paced by the wall clock.
    fn run(mut self) {
        let (mut bufs, mut cable) = self.start();
        let mut next = Instant::now();
        while !self.stop.load(Ordering::Relaxed) {
            if !self.check(&mut bufs) {
                return;
            }
            let period = Duration::from_secs_f64(
                f64::from(self.info.buffer_size) / f64::from(self.info.sample_rate),
            );
            self.block(&mut bufs, &mut cable, None);
            next += period;
            let now = Instant::now();
            if next > now {
                std::thread::sleep(next - now);
            } else if now - next > (period * 4).max(CATCH_UP) {
                // Fell far behind (system suspend, debugger): count it like
                // an xrun and resynchronise. Shorter delays (a busy machine
                // oversleeping) are caught up, so audio time keeps pace
                // with the wall clock.
                self.monitor.record_xrun();
                next = now;
            }
        }
        self.monitor.set_running(false);
    }

    /// On a simulated clock, the followers' callbacks between the master's.
    fn run_simulated(mut self, speed: f64, mut sim: Simulated) {
        let (mut bufs, mut cable) = self.start();
        let rate = f64::from(self.info.sample_rate);
        // The master's input frame F is captured at (F − buffer) / rate.
        self.signal.start = -f64::from(self.info.buffer_size) / rate;
        self.signal.true_rate = rate;
        let began = Instant::now();
        let mut t = 0.0f64;
        let mut next_watch = 0.0f64;
        let mut next: Vec<f64> = sim
            .followers
            .iter()
            .map(|f| f.as_ref().map_or(0.0, |f| f.period))
            .collect();
        while !self.stop.load(Ordering::Relaxed) {
            if !self.check(&mut bufs) {
                return;
            }
            for (k, slot) in sim.followers.iter_mut().enumerate() {
                let gone = slot
                    .as_ref()
                    .is_some_and(|f| !f.present.load(Ordering::Relaxed));
                if gone {
                    *slot = None;
                }
                if let Some(f) = slot.as_mut() {
                    while next[k] <= t {
                        let at = next[k] + f.jitter * f.wobble();
                        f.callback((at.max(0.0) * 1e9) as u64);
                        next[k] += f.period;
                    }
                }
            }
            if t >= next_watch {
                next_watch = t + WATCH_EVERY.as_secs_f64();
                for (k, f) in sim.attach_returned(t).into_iter() {
                    next[k] = t + f.period;
                    sim.followers[k] = Some(f);
                }
            }
            self.block(&mut bufs, &mut cable, Some((t * 1e9) as u64));
            t += f64::from(self.info.buffer_size) / rate;
            let due = began + Duration::from_secs_f64(t / speed);
            let now = Instant::now();
            if due > now {
                std::thread::sleep(due - now);
            }
        }
        self.monitor.set_running(false);
    }
}

/// What brings followers back: their device ends anew once a device is
/// plugged in again.
struct Attacher {
    control: Option<Arc<Mutex<Followers>>>,
    devices: Option<DummyDevices>,
    plan: Option<Plan>,
    signal: Kind,
}

impl Attacher {
    /// Followers `away` lists whose device is back: attached again, their
    /// clocks starting at `t` seconds.
    fn back(&self, away: &[usize], t: f64) -> Vec<(usize, PretendFollower)> {
        let (Some(control), Some(devices), Some(plan)) = (&self.control, &self.devices, &self.plan)
        else {
            return Vec::new();
        };
        let mut out = Vec::new();
        for &k in away {
            let Some(spec) = plan.specs.get(k) else {
                continue;
            };
            let Some((_, present)) = devices.find(&spec.device) else {
                continue;
            };
            if !present.load(Ordering::Relaxed) {
                continue;
            }
            let mut followers = locked(control);
            followers.collect();
            let Some(end) = followers.attach(k, spec.sample_rate, spec.block) else {
                continue;
            };
            drop(followers);
            out.push((
                k,
                PretendFollower::new(
                    k,
                    spec,
                    end,
                    devices.clock(&spec.device),
                    present,
                    self.signal,
                    plan.first_input(k),
                    t,
                ),
            ));
        }
        out
    }
}

/// A simulated stream's followers, run on the device's thread.
struct Simulated {
    followers: Vec<Option<PretendFollower>>,
    control: Option<Arc<Mutex<Followers>>>,
    devices: Option<DummyDevices>,
    plan: Option<Plan>,
    signal: Kind,
}

impl Simulated {
    fn attach_returned(&mut self, t: f64) -> Vec<(usize, PretendFollower)> {
        let away: Vec<usize> = self
            .followers
            .iter()
            .enumerate()
            .filter(|(_, f)| f.is_none())
            .map(|(k, _)| k)
            .collect();
        if away.is_empty() {
            return Vec::new();
        }
        Attacher {
            control: self.control.clone(),
            devices: self.devices.clone(),
            plan: self.plan.clone(),
            signal: self.signal,
        }
        .back(&away, t)
    }
}

/// A wall-clock stream's followers: a thread each, and this one bringing
/// them back when their device is plugged in again.
struct Watch {
    stop: Arc<AtomicBool>,
    control: Option<Arc<Mutex<Followers>>>,
    devices: Option<DummyDevices>,
    plan: Option<Plan>,
    signal: Kind,
}

impl Watch {
    fn run(self, first: Vec<Option<PretendFollower>>) {
        let start = |f: PretendFollower, stop: &Arc<AtomicBool>| {
            let stop = Arc::clone(stop);
            std::thread::Builder::new()
                .name(format!("faderframe-dummy-follower-{}", f.index))
                .spawn(move || f.run(stop))
                .ok()
        };
        let mut threads: Vec<Option<JoinHandle<()>>> = first
            .into_iter()
            .map(|f| f.and_then(|f| start(f, &self.stop)))
            .collect();
        let attacher = Attacher {
            control: self.control.clone(),
            devices: self.devices.clone(),
            plan: self.plan.clone(),
            signal: self.signal,
        };
        while !self.stop.load(Ordering::Relaxed) {
            std::thread::sleep(WATCH_EVERY);
            let mut away = Vec::new();
            for (k, t) in threads.iter_mut().enumerate() {
                if t.as_ref().is_none_or(|t| t.is_finished()) {
                    if let Some(t) = t.take() {
                        let _ = t.join();
                    }
                    away.push(k);
                }
            }
            if let Some(c) = &self.control {
                locked(c).collect();
            }
            for (k, f) in attacher.back(&away, 0.0) {
                if let Some(slot) = threads.get_mut(k) {
                    *slot = start(f, &self.stop);
                }
            }
        }
        for t in threads.into_iter().flatten() {
            let _ = t.join();
        }
    }
}

struct DummyStream {
    info: StreamInfo,
    monitor: Arc<StreamMonitor>,
    stop: Arc<AtomicBool>,
    requested: Arc<AtomicU32>,
    /// Buffer sizes the pretend devices on the master's clock take.
    buffers: Option<(u32, u32)>,
    routing: Option<StreamRouting>,
    /// With followers: their control side, the layout, the devices.
    followers: Option<Arc<Mutex<Followers>>>,
    plan: Option<Plan>,
    pretend: Option<DummyDevices>,
    threads: Vec<JoinHandle<()>>,
}

impl DummyStream {
    /// A follower device not plugged in now.
    fn away(&self, id: &str) -> bool {
        self.pretend
            .as_ref()
            .is_some_and(|d| d.find(id).is_none_or(|(_, p)| !p.load(Ordering::Relaxed)))
    }
}

impl AudioStream for DummyStream {
    fn info(&self) -> StreamInfo {
        let mut info = self.info.clone();
        info.buffer_size = self.monitor.status().buffer_size;
        info
    }

    fn status(&self) -> StreamStatus {
        self.monitor.status()
    }

    fn request_buffer_size(&mut self, frames: u32) -> Result<(), AudioError> {
        validate_format(self.info.sample_rate, frames)?;
        if let Some((lo, hi)) = self.buffers
            && !(lo..=hi).contains(&frames)
        {
            return Err(AudioError::UnsupportedConfig(format!(
                "{} takes buffers of {lo}–{hi} frames, not {frames}",
                self.info.device
            )));
        }
        self.requested.store(frames, Ordering::Relaxed);
        Ok(())
    }

    fn routing(&self) -> StreamRouting {
        let (Some(plan), Some(devices)) = (&self.plan, &self.pretend) else {
            return self.routing.clone().unwrap_or_default();
        };
        let named = |d: &DeviceInfo, names: &[String], c: u16| {
            names
                .get(usize::from(c))
                .map(|n| format!("{} · {n}", d.name))
        };
        let follower = |k: u16, c: u16, output: bool| {
            let spec = plan.specs.get(usize::from(k))?;
            if self.away(&spec.device) {
                return None;
            }
            let (d, _) = devices.find(&spec.device)?;
            named(
                &d,
                if output {
                    &d.output_names
                } else {
                    &d.input_names
                },
                c,
            )
        };
        let place = |p: &Place, output: bool| match p {
            Place::Master(c) => named(
                &plan.master,
                if output {
                    &plan.master.output_names
                } else {
                    &plan.master.input_names
                },
                *c,
            ),
            Place::Follower {
                follower: k,
                channel,
            } => follower(*k, *channel, output),
        };
        let mut missing: Vec<String> = Vec::new();
        for s in &plan.specs {
            if self.away(&s.device) && !missing.contains(&s.name) {
                missing.push(s.name.clone());
            }
        }
        StreamRouting {
            inputs: plan.inputs.iter().map(|p| place(p, false)).collect(),
            outputs: plan.outputs.iter().map(|p| place(p, true)).collect(),
            missing,
            devices: self
                .routing
                .as_ref()
                .map(|r| r.devices.clone())
                .unwrap_or_default(),
        }
    }

    fn devices(&self) -> Vec<DeviceStatus> {
        let (Some(followers), Some(plan)) = (&self.followers, &self.plan) else {
            return Vec::new();
        };
        let channels = |places: &[Place]| {
            places
                .iter()
                .enumerate()
                .filter(|(_, p)| matches!(p, Place::Master(_)))
                .map(|(c, _)| c as u16)
                .collect()
        };
        let master = DeviceStatus {
            id: plan.master.id.clone(),
            name: plan.master.name.clone(),
            master: true,
            present: true,
            inputs: channels(&plan.inputs),
            outputs: channels(&plan.outputs),
            sample_rate: self.info.sample_rate,
            input_latency: self.info.input_latency,
            output_latency: self.info.output_latency,
            drift_ppm: 0.0,
            locked: true,
            gaps: 0,
        };
        locked(followers).describe(master, &plan.inputs, &plan.outputs, |id| self.away(id))
    }
}

impl Drop for DummyStream {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        for t in self.threads.drain(..) {
            let _ = t.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicU64;

    struct Counting {
        frames: Arc<AtomicU64>,
        prepared: Arc<AtomicU32>,
    }

    impl AudioCallback for Counting {
        fn prepare(&mut self, info: &StreamInfo) {
            self.prepared.store(info.buffer_size, Ordering::Relaxed);
        }
        fn process(&mut self, io: &mut dyn DeviceBuffers) {
            for c in 0..io.output_channels() {
                io.output(c).fill(0.0);
            }
            self.frames.fetch_add(io.frames() as u64, Ordering::Relaxed);
        }
    }

    #[test]
    fn drives_callback_and_changes_buffer_size() {
        let frames = Arc::new(AtomicU64::new(0));
        let prepared = Arc::new(AtomicU32::new(0));
        let mut backend = DummyBackend::default();
        let mut stream = backend
            .open_stream(
                StreamConfig {
                    sample_rate: Some(96_000),
                    buffer_size: Some(128),
                    ..StreamConfig::default()
                },
                Box::new(Counting {
                    frames: Arc::clone(&frames),
                    prepared: Arc::clone(&prepared),
                }),
            )
            .unwrap();
        assert_eq!(stream.info().sample_rate, 96_000);
        let deadline = Instant::now() + Duration::from_secs(5);
        while frames.load(Ordering::Relaxed) < 128 * 8 && Instant::now() < deadline {
            std::thread::yield_now();
        }
        assert!(frames.load(Ordering::Relaxed) >= 128 * 8);
        stream.request_buffer_size(512).unwrap();
        while prepared.load(Ordering::Relaxed) != 512 && Instant::now() < deadline {
            std::thread::yield_now();
        }
        assert_eq!(prepared.load(Ordering::Relaxed), 512);
        assert!(stream.request_buffer_size(100_000).is_err());
        drop(stream);
    }

    #[test]
    fn pretend_devices_check_formats_and_go_away() {
        let devices = DummyDevices::new(vec![
            mock_device("a", 2, 2, &[44_100], Some((64, 512))),
            mock_device("b", 8, 8, &[48_000, 96_000], None),
        ]);
        let mut backend = DummyBackend::with_devices(devices.clone());
        assert_eq!(backend.enumerate_devices().unwrap().len(), 2);
        let counting = || {
            Box::new(Counting {
                frames: Arc::default(),
                prepared: Arc::default(),
            })
        };
        let open = |backend: &mut DummyBackend, device: &str, rate: Option<u32>| {
            backend.open_stream(
                StreamConfig {
                    output_devices: vec![device.into()],
                    sample_rate: rate,
                    buffer_size: Some(128),
                    ..StreamConfig::default()
                },
                counting(),
            )
        };
        assert!(matches!(
            open(&mut backend, "a", Some(48_000)),
            Err(AudioError::UnsupportedConfig(_))
        ));
        assert!(matches!(
            open(&mut backend, "c", None),
            Err(AudioError::DeviceNotFound(_))
        ));
        let mut stream = open(&mut backend, "a", None).unwrap();
        assert_eq!(stream.info().sample_rate, 44_100, "the device's own rate");
        assert_eq!(stream.info().device, "a");
        assert_eq!(stream.routing().output(1), Some("a · Out 2"));
        assert!(
            stream.request_buffer_size(1024).is_err(),
            "beyond the device"
        );
        stream.request_buffer_size(256).unwrap();
        devices.unplug("a");
        let deadline = Instant::now() + Duration::from_secs(5);
        while !stream.status().shut_down && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(stream.status().shut_down, "unplugged");
        assert_eq!(backend.enumerate_devices().unwrap().len(), 1);
        devices.plug(mock_device("a", 2, 2, &[44_100], Some((64, 512))));
        if let Err(e) = open(&mut backend, "a", None) {
            panic!("back: {e}");
        }
    }

    /// Remembers the loudest sample of each input lately.
    struct Loudest {
        peaks: Arc<Mutex<Vec<f32>>>,
        frames: Arc<AtomicU64>,
    }

    impl AudioCallback for Loudest {
        fn prepare(&mut self, _info: &StreamInfo) {}
        fn process(&mut self, io: &mut dyn DeviceBuffers) {
            if let Ok(mut p) = self.peaks.try_lock() {
                p.resize(io.input_channels(), 0.0);
                for (c, peak) in p.iter_mut().enumerate() {
                    let m = io.input(c).iter().fold(0.0f32, |m, v| m.max(v.abs()));
                    *peak = 0.99 * *peak + 0.01 * m;
                }
            }
            for c in 0..io.output_channels() {
                io.output(c).fill(0.0);
            }
            self.frames.fetch_add(io.frames() as u64, Ordering::Relaxed);
        }
    }

    fn desk_and_mic() -> DummyDevices {
        let mut desk = mock_device("desk", 2, 2, &[48_000], None);
        desk.is_default = true;
        let mic = mock_device("mic", 1, 0, &[44_100], None);
        let d = DummyDevices::new(vec![desk, mic]);
        d.set_clock(
            "mic",
            PretendClock {
                ppm: 250.0,
                jitter: 0.0005,
                block: Some(441),
            },
        );
        d
    }

    fn wait(what: &str, mut done: impl FnMut() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while !done() {
            assert!(Instant::now() < deadline, "timed out waiting for {what}");
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    /// A second device records through its ring and resampler: its
    /// channels follow the master's, it runs at its own rate and clock,
    /// and pulling it out only silences them until it is back.
    #[test]
    fn a_pretend_follower_records_and_comes_back_after_unplugging() {
        let d = desk_and_mic();
        let mut backend = DummyBackend::with_input_tone(440.0).devices(d.clone());
        assert!(backend.capabilities().resamples_followers);
        let peaks = Arc::new(Mutex::new(Vec::new()));
        let frames = Arc::new(AtomicU64::new(0));
        let stream = backend
            .open_stream(
                StreamConfig {
                    output_devices: vec!["desk".into()],
                    input_devices: vec!["desk".into(), "mic".into()],
                    input_channels: 3,
                    output_channels: 2,
                    buffer_size: Some(256),
                    ..StreamConfig::default()
                },
                Box::new(Loudest {
                    peaks: Arc::clone(&peaks),
                    frames: Arc::clone(&frames),
                }),
            )
            .unwrap();
        assert_eq!(stream.info().device, "desk");
        assert_eq!(stream.routing().devices, vec!["desk", "mic"]);
        assert_eq!(stream.routing().input(2), Some("mic · In 1"));
        let mic = |s: &dyn AudioStream| s.devices().into_iter().find(|d| d.id == "mic");
        let peak = |c: usize| peaks.lock().unwrap().get(c).copied().unwrap_or(0.0);
        wait("the follower's tone", || peak(2) > 0.3);
        let m = mic(stream.as_ref()).unwrap();
        assert!(m.present && !m.master);
        assert_eq!(m.inputs, vec![2]);
        assert_eq!(m.sample_rate, 44_100, "its own rate, resampled");
        assert!(
            m.input_latency > 441,
            "its ring and resampler: {}",
            m.input_latency
        );
        let desk = &stream.devices()[0];
        assert!(desk.master && desk.inputs == vec![0, 1]);
        d.unplug("mic");
        wait("silence on its channel", || peak(2) < 0.01);
        assert!(!stream.status().shut_down, "the master plays on");
        assert!(peak(0) > 0.3, "the master's own inputs too");
        assert_eq!(stream.routing().missing, vec!["mic".to_string()]);
        assert_eq!(stream.routing().input(2), None);
        assert!(!mic(stream.as_ref()).unwrap().present);
        let before = frames.load(Ordering::Relaxed);
        d.plug(mock_device("mic", 1, 0, &[44_100], None));
        wait("the follower back", || peak(2) > 0.3);
        assert!(mic(stream.as_ref()).unwrap().present);
        assert!(frames.load(Ordering::Relaxed) > before, "never stopped");
        assert!(stream.routing().missing.is_empty());
    }

    /// On a simulated clock the stream runs many times faster than real
    /// time.
    #[test]
    fn a_simulated_stream_runs_ahead_of_the_wall_clock() {
        let d = desk_and_mic();
        let mut backend = DummyBackend::with_input_tone(440.0)
            .devices(d)
            .simulated(40.0);
        let frames = Arc::new(AtomicU64::new(0));
        let peaks = Arc::new(Mutex::new(Vec::new()));
        let started = Instant::now();
        let stream = backend
            .open_stream(
                StreamConfig {
                    output_devices: vec!["desk".into()],
                    input_devices: vec!["mic".into()],
                    input_channels: 1,
                    output_channels: 2,
                    ..StreamConfig::default()
                },
                Box::new(Loudest {
                    peaks: Arc::clone(&peaks),
                    frames: Arc::clone(&frames),
                }),
            )
            .unwrap();
        wait("ten simulated seconds", || {
            frames.load(Ordering::Relaxed) >= 480_000
        });
        assert!(started.elapsed() < Duration::from_secs(5));
        let peak = peaks.lock().unwrap().first().copied().unwrap_or(0.0);
        assert!(peak > 0.3, "the follower's tone: {peak}");
        let mic = stream
            .devices()
            .into_iter()
            .find(|d| d.id == "mic")
            .unwrap();
        assert!(mic.present, "{mic:?}");
        assert!(
            (mic.drift_ppm - 250.0).abs() < 60.0,
            "the drift found so far: {}",
            mic.drift_ppm
        );
    }

    #[test]
    fn rejects_invalid_formats() {
        let mut backend = DummyBackend::default();
        let res = backend.open_stream(
            StreamConfig {
                sample_rate: Some(1),
                ..StreamConfig::default()
            },
            Box::new(Counting {
                frames: Arc::default(),
                prepared: Arc::default(),
            }),
        );
        assert!(matches!(res, Err(AudioError::UnsupportedConfig(_))));
    }
}
