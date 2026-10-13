//! How a plugin behaves on the audio thread. FaderFrame's own code keeps
//! the realtime rules (no allocation, locks, I/O or waiting in a callback;
//! a short reset) and its tests prove it; a plugin from another maker is a
//! compiled binary we can only measure. [`check`] plays one on a thread of
//! its own — noise in, chords for instruments — first while it starts,
//! then steadily, then resets it a few times, and counts per call:
//!
//! * allocations (malloc, free and their kin) — counted by the program,
//!   which routes the C allocator through a counter ([`set_alloc_counter`];
//!   Linux with glibc only);
//! * waits (voluntary context switches: a lock someone else held, I/O, a
//!   sleep) and page faults (Linux);
//! * CPU time per block (the thread's own clock: other programs on the
//!   machine do not count).
//!
//! The [`CheckReport`] says what was found ([`CheckReport::findings`]) and
//! what to do about it. FaderFrame runs each check in a helper process
//! (`faderframe --check-plugin`), so a plugin that crashes takes only that.

use faderframe_audio_graph::{AudioBuffer, NodeIo};
use faderframe_core::ChannelLayout;
use faderframe_midi::{MidiBuffer, MidiEvent, TimedMidiEvent};
use faderframe_plugin_host::{
    NO_HARMONY, PluginInstance, PluginProcessContext, PluginProcessor, ProcessConfig,
};
use faderframe_realtime::usage::{ThreadUsage, thread_cpu_ns, thread_usage};
use faderframe_transport::TransportInfo;
use serde::{Deserialize, Serialize};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

/// The program's allocation counter: `arm` starts (true) or stops counting
/// the calling thread's allocations, `count` reads them.
#[derive(Clone, Copy, Debug)]
pub struct AllocCounter {
    pub arm: fn(bool),
    pub count: fn() -> u64,
}

static COUNTER: OnceLock<AllocCounter> = OnceLock::new();

/// Install the counter (once, at start-up of the program that checks).
pub fn set_alloc_counter(counter: AllocCounter) {
    let _ = COUNTER.set(counter);
}

/// How a check plays the plugin.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CheckOptions {
    pub sample_rate: u32,
    /// Frames per call.
    pub block: u32,
    /// Starting: the first calls (many plugins prepare lazily).
    pub start_seconds: f64,
    /// Playing steadily.
    pub seconds: f64,
    /// Resets, each after a little more playing.
    pub resets: u32,
    /// Called at a device's pace (each block when it is due), not as fast
    /// as it goes: what a plugin syncing with threads of its own meets live.
    pub paced: bool,
}

impl Default for CheckOptions {
    fn default() -> Self {
        Self {
            sample_rate: 48_000,
            block: 128,
            start_seconds: 0.5,
            seconds: 3.0,
            resets: 5,
            paced: true,
        }
    }
}

/// What one kind of call did, over all its calls.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct PhaseStats {
    pub calls: u32,
    /// The plugin's allocations and frees (`None`: not counted on this
    /// system).
    pub allocations: Option<u64>,
    pub calls_allocating: u32,
    /// The host's own (FaderFrame's code around the plugin's: its format
    /// layer) — a FaderFrame bug when not zero.
    pub host_allocations: Option<u64>,
    pub calls_host_allocating: u32,
    /// Waits (`None`: not counted on this system).
    pub waits: Option<u64>,
    pub calls_waiting: u32,
    pub minor_faults: u64,
    pub major_faults: u64,
    /// CPU time per call (µs): median, 99th percentile, most.
    pub cpu_median_us: f64,
    pub cpu_p99_us: f64,
    pub cpu_max_us: f64,
    /// Longest call by the clock on the wall (µs; includes being preempted).
    pub wall_max_us: f64,
}

/// What a check found about one plugin.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct CheckReport {
    pub name: String,
    pub vendor: String,
    pub version: String,
    pub sample_rate: u32,
    pub block: u32,
    pub instrument: bool,
    pub latency: u32,
    /// The first calls.
    pub start: PhaseStats,
    /// Playing (after the start, and after each reset).
    pub steady: PhaseStats,
    /// The resets themselves.
    pub reset: PhaseStats,
    /// It could not be checked (and why).
    pub error: Option<String>,
}

/// One thing a check found.
#[derive(Clone, Debug, PartialEq)]
pub enum Finding {
    /// It allocates (or frees) memory while playing.
    Allocates { calls: u32, of: u32 },
    /// FaderFrame's own code allocated while hosting it (a FaderFrame bug).
    HostAllocates { calls: u32, of: u32 },
    /// It waits (locks, I/O, sleeps) while playing.
    Waits { calls: u32, of: u32 },
    /// It touches memory it had not used before while playing.
    Faults { faults: u64 },
    /// Typically more than half of what a call may take.
    Heavy { median_us: f64, period_us: f64 },
    /// Its slowest calls take far more than its typical ones.
    Uneven {
        max_us: f64,
        median_us: f64,
        period_us: f64,
    },
    /// A reset takes long (a panic resets every plugin at once).
    SlowReset { max_us: f64 },
    /// It allocates or waits when reset.
    ResetUnsafe,
    /// Only while starting: allocations, waits or page faults in its first
    /// calls (harmless once it plays).
    StartsLazily,
    /// The check could not run (it would not load or start, or crashed).
    Failed(String),
}

/// How a plugin did overall.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Verdict {
    /// Nothing to worry about.
    Clean,
    /// Worth knowing (uneven timing, page faults, a slow reset).
    Caution,
    /// Breaks the realtime rules while playing: dropouts at small buffers.
    Problem,
    /// It could not be checked.
    Failed,
}

impl Finding {
    pub fn verdict(&self) -> Verdict {
        match self {
            Self::Allocates { .. }
            | Self::HostAllocates { .. }
            | Self::Waits { .. }
            | Self::Heavy { .. } => Verdict::Problem,
            Self::Faults { .. }
            | Self::Uneven { .. }
            | Self::SlowReset { .. }
            | Self::ResetUnsafe => Verdict::Caution,
            Self::StartsLazily => Verdict::Clean,
            Self::Failed(_) => Verdict::Failed,
        }
    }

    /// What was found, for people.
    pub fn text(&self) -> String {
        match self {
            Self::Allocates { calls, of } => {
                format!("Allocates memory while playing ({calls} of {of} blocks)")
            }
            Self::HostAllocates { calls, of } => format!(
                "FaderFrame's own code allocated while hosting it ({calls} of {of} blocks) — \
                 a FaderFrame bug, please report it"
            ),
            Self::Waits { calls, of } => {
                format!("Waits (a lock, disk or a sleep) while playing ({calls} of {of} blocks)")
            }
            Self::Faults { faults } => {
                format!("Touches memory it had not used while playing ({faults} page faults)")
            }
            Self::Heavy {
                median_us,
                period_us,
            } => format!(
                "Heavy: a block typically takes {:.0} % of the time it has",
                median_us / period_us * 100.0
            ),
            Self::Uneven {
                max_us,
                median_us,
                period_us,
            } => format!(
                "Uneven: its slowest block took {:.0} µs (typically {:.0} µs; a block has {:.0} µs)",
                max_us, median_us, period_us
            ),
            Self::SlowReset { max_us } => {
                format!(
                    "Slow reset: {:.1} ms (a panic resets every plugin)",
                    max_us / 1000.0
                )
            }
            Self::ResetUnsafe => "Allocates or waits when reset".into(),
            Self::StartsLazily => {
                "Allocates or prepares memory in its first blocks only (harmless once playing)"
                    .into()
            }
            Self::Failed(why) => format!("Could not be checked: {why}"),
        }
    }
}

impl Verdict {
    pub fn label(self) -> &'static str {
        match self {
            Self::Clean => "Realtime-safe",
            Self::Caution => "Mostly safe",
            Self::Problem => "Not realtime-safe",
            Self::Failed => "Not checked",
        }
    }

    /// What to do about it.
    pub fn advice(self) -> &'static str {
        match self {
            Self::Clean => "Fine on live tracks at small buffers.",
            Self::Caution => {
                "Usually fine. If it drops out at small buffers, use a larger buffer or let its \
                 track render ahead (not armed, not monitored)."
            }
            Self::Problem => {
                "Can cause dropouts at small buffers. Keep it sandboxed (Preferences → General: \
                 a late block then costs only its own track), let its track render ahead (not \
                 armed, not monitored) or use a larger buffer — and tell its maker."
            }
            Self::Failed => {
                "It would not run in the check (or crashed it). Sandboxed, a crash costs only \
                 its own track."
            }
        }
    }
}

impl CheckReport {
    /// One block's time (µs).
    pub fn period_us(&self) -> f64 {
        f64::from(self.block) / f64::from(self.sample_rate.max(1)) * 1e6
    }

    pub fn findings(&self) -> Vec<Finding> {
        let mut out = Vec::new();
        if let Some(e) = &self.error {
            out.push(Finding::Failed(e.clone()));
            return out;
        }
        let s = &self.steady;
        let period = self.period_us();
        if s.calls_allocating > 0 {
            out.push(Finding::Allocates {
                calls: s.calls_allocating,
                of: s.calls,
            });
        }
        if s.calls_host_allocating > 0 {
            out.push(Finding::HostAllocates {
                calls: s.calls_host_allocating,
                of: s.calls,
            });
        }
        if s.calls_waiting > 0 {
            out.push(Finding::Waits {
                calls: s.calls_waiting,
                of: s.calls,
            });
        }
        if s.cpu_median_us > period * 0.5 {
            out.push(Finding::Heavy {
                median_us: s.cpu_median_us,
                period_us: period,
            });
        } else if s.cpu_max_us > period * 0.5 && s.cpu_max_us > s.cpu_median_us * 4.0 {
            out.push(Finding::Uneven {
                max_us: s.cpu_max_us,
                median_us: s.cpu_median_us,
                period_us: period,
            });
        }
        if s.minor_faults + s.major_faults > 0 {
            out.push(Finding::Faults {
                faults: s.minor_faults + s.major_faults,
            });
        }
        let r = &self.reset;
        if r.cpu_max_us.max(r.wall_max_us) > 1000.0 {
            out.push(Finding::SlowReset {
                max_us: r.cpu_max_us.max(r.wall_max_us),
            });
        }
        if r.calls_allocating > 0 || r.calls_waiting > 0 || r.calls_host_allocating > 0 {
            out.push(Finding::ResetUnsafe);
        }
        let st = &self.start;
        if out.is_empty()
            && (st.calls_allocating > 0 || st.calls_waiting > 0 || st.minor_faults > 0)
        {
            out.push(Finding::StartsLazily);
        }
        out
    }

    pub fn verdict(&self) -> Verdict {
        self.findings()
            .iter()
            .map(Finding::verdict)
            .max()
            .unwrap_or(Verdict::Clean)
    }

    /// A report for a plugin that could not be checked.
    pub fn failed(why: impl Into<String>) -> Self {
        Self {
            error: Some(why.into()),
            ..Self::default()
        }
    }
}

/// One measured call.
#[derive(Clone, Copy, Debug, Default)]
struct Sample {
    /// The plugin's (all the thread's less the host's).
    allocations: Option<u64>,
    host_allocations: Option<u64>,
    usage: Option<ThreadUsage>,
    cpu_ns: Option<u64>,
    wall_ns: u64,
}

/// Measure `f` on this thread.
fn measure(f: impl FnOnce()) -> Sample {
    use faderframe_realtime::memory::{allocations_counted, count_allocations};
    let counter = COUNTER.get().copied();
    let u0 = thread_usage();
    let c0 = thread_cpu_ns();
    let a0 = counter.map(|c| (c.count)());
    let h0 = allocations_counted();
    let t0 = Instant::now();
    if let Some(c) = counter {
        (c.arm)(true);
    }
    count_allocations(true);
    f();
    count_allocations(false);
    if let Some(c) = counter {
        (c.arm)(false);
    }
    let wall_ns = t0.elapsed().as_nanos() as u64;
    let h1 = allocations_counted();
    let a1 = counter.map(|c| (c.count)());
    let c1 = thread_cpu_ns();
    let u1 = thread_usage();
    let host = h0.zip(h1).map(|(a, b)| b.saturating_sub(a));
    // Rust's allocations reach the C allocator too: the plugin's are the
    // rest.
    let all = a0.zip(a1).map(|(a, b)| b.saturating_sub(a));
    Sample {
        allocations: all.map(|a| a.saturating_sub(host.unwrap_or(0))),
        host_allocations: host,
        usage: u0.zip(u1).map(|(a, b)| b.since(a)),
        cpu_ns: c0.zip(c1).map(|(a, b)| b.saturating_sub(a)),
        wall_ns,
    }
}

fn stats(samples: &[Sample]) -> PhaseStats {
    let mut cpu: Vec<f64> = samples
        .iter()
        .map(|s| s.cpu_ns.unwrap_or(s.wall_ns) as f64 / 1000.0)
        .collect();
    cpu.sort_by(f64::total_cmp);
    let at = |q: f64| {
        cpu.get(((cpu.len() as f64 - 1.0) * q).round() as usize)
            .copied()
            .unwrap_or(0.0)
    };
    let counted = samples.iter().all(|s| s.allocations.is_some());
    let usage = samples.iter().all(|s| s.usage.is_some());
    PhaseStats {
        calls: samples.len() as u32,
        allocations: counted.then(|| samples.iter().filter_map(|s| s.allocations).sum()),
        calls_allocating: samples
            .iter()
            .filter(|s| s.allocations.is_some_and(|a| a > 0))
            .count() as u32,
        host_allocations: samples
            .iter()
            .all(|s| s.host_allocations.is_some())
            .then(|| samples.iter().filter_map(|s| s.host_allocations).sum()),
        calls_host_allocating: samples
            .iter()
            .filter(|s| s.host_allocations.is_some_and(|a| a > 0))
            .count() as u32,
        waits: usage.then(|| {
            samples
                .iter()
                .filter_map(|s| s.usage.map(|u| u.voluntary_switches))
                .sum()
        }),
        calls_waiting: samples
            .iter()
            .filter(|s| s.usage.is_some_and(|u| u.voluntary_switches > 0))
            .count() as u32,
        minor_faults: samples
            .iter()
            .filter_map(|s| s.usage.map(|u| u.minor_faults))
            .sum(),
        major_faults: samples
            .iter()
            .filter_map(|s| s.usage.map(|u| u.major_faults))
            .sum(),
        cpu_median_us: at(0.5),
        cpu_p99_us: at(0.99),
        cpu_max_us: cpu.last().copied().unwrap_or(0.0),
        wall_max_us: samples
            .iter()
            .map(|s| s.wall_ns as f64 / 1000.0)
            .fold(0.0, f64::max),
    }
}

/// The audio a check plays: noise in, chords for instruments.
struct Player {
    ins: Vec<AudioBuffer>,
    outs: Vec<AudioBuffer>,
    events_in: Vec<MidiBuffer>,
    events_out: Vec<MidiBuffer>,
    transport: TransportInfo,
    block: usize,
    rate: f64,
    frame: u64,
    seed: u32,
    notes: bool,
}

impl Player {
    fn new(inputs: usize, outputs: usize, block: usize, rate: f64, notes: bool) -> Self {
        let buffer = |channels: usize| {
            let mut b = AudioBuffer::new(ChannelLayout::from_channel_count(channels.max(1)), block);
            b.set_len(block);
            b
        };
        Self {
            ins: (inputs > 0).then(|| buffer(inputs)).into_iter().collect(),
            outs: vec![buffer(outputs.max(1))],
            events_in: vec![MidiBuffer::with_capacity(64)],
            events_out: vec![MidiBuffer::with_capacity(256)],
            transport: TransportInfo {
                playing: true,
                sample_rate: rate,
                tempo: 120.0,
                ..TransportInfo::default()
            },
            block,
            rate,
            frame: 0,
            seed: 0x1234_5678,
            notes,
        }
    }

    /// The next block's input (before it is measured).
    fn prepare(&mut self) {
        for b in &mut self.ins {
            for c in 0..b.num_channels() {
                for s in b.channel_mut(c).iter_mut() {
                    self.seed = self
                        .seed
                        .wrapping_mul(1_664_525)
                        .wrapping_add(1_013_904_223);
                    *s = ((self.seed >> 9) as f32 / (1u32 << 23) as f32 - 0.5) * 0.5;
                }
            }
        }
        let events = &mut self.events_in[0];
        events.clear();
        self.events_out[0].clear();
        if self.notes {
            // A chord every quarter second, held for a fifth of one.
            let period = (self.rate * 0.25) as u64;
            let held = (self.rate * 0.2) as u64;
            for i in 0..self.block as u64 {
                let t = self.frame + i;
                let (on, off) = (t.is_multiple_of(period), t % period == held);
                if on || off {
                    for key in [60u8, 64, 67] {
                        let event = if on {
                            MidiEvent::NoteOn {
                                channel: 0,
                                key,
                                velocity: 100,
                            }
                        } else {
                            MidiEvent::NoteOff {
                                channel: 0,
                                key,
                                velocity: 0,
                            }
                        };
                        let _ = events.push(TimedMidiEvent::new(i as u32, event));
                    }
                }
            }
        }
        self.transport.sample_position = self.frame as i64;
        self.transport.quarter_position = self.frame as f64 / self.rate * 2.0;
    }

    fn process(&mut self, p: &mut dyn PluginProcessor) {
        let ctx = PluginProcessContext {
            transport: &self.transport,
            param_events: &[],
            harmony: &NO_HARMONY,
            param_mods: &[],
            note_mods: &[],
        };
        let mut io = NodeIo {
            frames: self.block,
            audio_in: &self.ins,
            audio_out: &mut self.outs,
            events_in: &self.events_in,
            events_out: &mut self.events_out,
        };
        p.process(&ctx, &mut io);
        self.frame += self.block as u64;
    }
}

/// Check `instance` (on this thread, which keeps polling it while its
/// processor plays on a thread of its own).
pub fn check(instance: &mut dyn PluginInstance, opts: &CheckOptions) -> CheckReport {
    let d = instance.descriptor().clone();
    let main_in = d
        .audio_inputs
        .iter()
        .find(|p| p.is_main)
        .or(d.audio_inputs.first())
        .map_or(0, |p| usize::from(p.channels));
    let main_out = d
        .audio_outputs
        .iter()
        .find(|p| p.is_main)
        .or(d.audio_outputs.first())
        .map_or(2, |p| usize::from(p.channels));
    let instrument = d.note_inputs > 0
        || matches!(
            d.category,
            faderframe_plugin_host::PluginCategory::Instrument
        );
    let mut report = CheckReport {
        name: d.name.clone(),
        vendor: d.vendor.clone(),
        version: d.version.clone(),
        sample_rate: opts.sample_rate,
        block: opts.block,
        instrument,
        ..CheckReport::default()
    };
    instance.configure_channels(main_out.max(1));
    instance.configure_realtime(true);
    instance.configure_device_block(opts.block as usize);
    instance.configure_outputs(1);
    let config = ProcessConfig {
        sample_rate: f64::from(opts.sample_rate),
        max_block_size: opts.block,
        sidechain: false,
        double_precision: false,
    };
    let mut processor = match instance.create_processor(&config) {
        Ok(p) => p,
        Err(e) => return CheckReport::failed(format!("it would not start: {e}")),
    };
    report.latency = instance.latency_samples();
    // Its code mapped before it runs (as the program does), so first runs
    // of a path are not counted as its faults.
    faderframe_realtime::memory::prefault_code();
    let opts = *opts;
    let worker = std::thread::Builder::new()
        .name("faderframe-check".into())
        .spawn(move || {
            let rate = f64::from(opts.sample_rate);
            let block = opts.block.max(1) as usize;
            let blocks = |seconds: f64| ((seconds * rate) as usize / block).max(1);
            let mut player = Player::new(main_in, main_out, block, rate, instrument);
            faderframe_realtime::memory::prefault_stack(256 * 1024);
            let period = Duration::from_secs_f64(block as f64 / rate);
            let mut due = Instant::now();
            let mut run = |n: usize, p: &mut Box<dyn PluginProcessor>| {
                let mut samples = Vec::with_capacity(n);
                for _ in 0..n {
                    player.prepare();
                    if opts.paced {
                        // The next block when a device would ask for it.
                        due += period;
                        let now = Instant::now();
                        if due > now {
                            std::thread::sleep(due - now);
                        } else {
                            due = now;
                        }
                    }
                    samples.push(measure(|| player.process(p.as_mut())));
                }
                samples
            };
            let start = run(blocks(opts.start_seconds), &mut processor);
            let mut steady = run(blocks(opts.seconds), &mut processor);
            let mut resets = Vec::new();
            for _ in 0..opts.resets {
                resets.push(measure(|| processor.reset()));
                steady.extend(run(blocks(0.2), &mut processor));
            }
            (processor, stats(&start), stats(&steady), stats(&resets))
        });
    let handle = match worker {
        Ok(h) => h,
        Err(e) => return CheckReport::failed(format!("no thread for it: {e}")),
    };
    // The plugin's main-thread requests while it plays.
    while !handle.is_finished() {
        let _ = instance.poll();
        std::thread::sleep(Duration::from_millis(5));
    }
    match handle.join() {
        Ok((processor, start, steady, reset)) => {
            drop(processor);
            let _ = instance.poll();
            report.start = start;
            report.steady = steady;
            report.reset = reset;
            report
        }
        Err(_) => CheckReport::failed("it panicked while playing"),
    }
}

pub mod run;
pub use run::{MARKER, in_subprocess, print_report};

#[cfg(test)]
mod tests;
