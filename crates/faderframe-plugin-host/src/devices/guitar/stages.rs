//! The line's stages as reservoir workers, and the workshop that builds a
//! pedal's circuits when a place's model changes.
//!
//! A pedal's circuit is made off the audio thread, by the workshop's own
//! thread, and handed to the stage whole: the stage asks (an atomic), takes
//! the delivery when it is there (a `try_lock`), swaps it in and hands the
//! old circuits back the same way, so the workshop frees them. Until it has
//! arrived the place passes its input through. Offline the stage waits for
//! it, so a render does not depend on how fast the workshop was.

use super::bank::{Bank, Lanes, Unit};
use crate::tap::AnalysisTap;
use faderframe_guitar::chain::{self, Chain};
use faderframe_guitar::pedal::{PedalStage, Stomp, StompCircuit, StompSettings};
use faderframe_realtime::TryCell;
use faderframe_realtime::reservoir::{self, Reservoir, Segments, Timing};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, OnceLock};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

impl Unit for PedalStage {
    fn wake_from(&mut self, other: &Self) {
        self.copy_runtime_state_from(other);
    }
}

impl Unit for Chain {
    fn wake_from(&mut self, other: &Self) {
        self.copy_runtime_state_from(other);
    }
}

/// A pedal's circuits, one per channel.
pub(super) struct Bundle {
    stomp: Stomp,
    circuits: Vec<Option<StompCircuit>>,
}

impl Bundle {
    pub(super) fn circuits_mut(&mut self) -> impl Iterator<Item = &mut Option<StompCircuit>> {
        self.circuits.iter_mut()
    }
}

/// One stage's dealings with the workshop.
struct Order {
    /// The stomp wanted, as `Stomp::index() + 1`; 0 for nothing.
    wanted: AtomicUsize,
    delivery: TryCell<Option<Box<Bundle>>>,
    returned: TryCell<Option<Box<Bundle>>>,
}

/// Builds pedals' circuits for the stages, on a thread of its own.
pub(super) struct Workshop {
    orders: Box<[Order]>,
    rate: f64,
    quality: usize,
    channels: usize,
    stop: AtomicBool,
    thread: OnceLock<std::thread::Thread>,
}

impl Workshop {
    pub(super) fn new(stages: usize, rate: f64, quality: usize, channels: usize) -> Arc<Self> {
        Arc::new(Self {
            orders: (0..stages)
                .map(|_| Order {
                    wanted: AtomicUsize::new(0),
                    delivery: TryCell::new(None),
                    returned: TryCell::new(None),
                })
                .collect(),
            rate,
            quality,
            channels,
            stop: AtomicBool::new(false),
            thread: OnceLock::new(),
        })
    }

    /// `stomp`'s circuits for every channel (off the audio thread).
    pub(super) fn build(&self, stomp: Stomp) -> Result<Box<Bundle>, String> {
        let circuits = (0..self.channels)
            .map(|_| StompCircuit::build(stomp, self.rate, self.quality))
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| format!("{}: {e:?}", stomp.name()))?;
        Ok(Box::new(Bundle { stomp, circuits }))
    }

    /// Starts the workshop's thread (when there are stages to serve).
    pub(super) fn start(self: &Arc<Self>) -> std::io::Result<Option<JoinHandle<()>>> {
        if self.orders.is_empty() {
            return Ok(None);
        }
        let me = Arc::clone(self);
        let handle = std::thread::Builder::new()
            .name("faderframe-pedals".into())
            .spawn(move || me.run())?;
        let _ = self.thread.set(handle.thread().clone());
        Ok(Some(handle))
    }

    pub(super) fn stop(&self) {
        self.stop.store(true, Ordering::Release);
        self.wake();
    }

    /// Wakes the workshop's thread: a futex, never a lock.
    fn wake(&self) {
        if let Some(t) = self.thread.get() {
            t.unpark();
        }
    }

    fn run(&self) {
        let mut delivered = vec![0usize; self.orders.len()];
        while !self.stop.load(Ordering::Acquire) {
            for (order, done) in self.orders.iter().zip(delivered.iter_mut()) {
                // What a stage handed back is freed here.
                if let Some(mut back) = order.returned.try_lock() {
                    drop(back.take());
                }
                let wanted = order.wanted.load(Ordering::Acquire);
                if wanted == 0 || wanted == *done {
                    continue;
                }
                *done = wanted;
                let Ok(bundle) = self.build(Stomp::from_index(wanted - 1)) else {
                    // A circuit that does not build stays bypassed.
                    continue;
                };
                if let Some(mut d) = order.delivery.lock_blocking(10_000) {
                    let stale = d.replace(bundle);
                    drop(d);
                    drop(stale);
                }
            }
            std::thread::park_timeout(Duration::from_millis(50));
        }
    }
}

/// A pedal's place on the line, for every channel.
pub(super) struct PedalWorker {
    bank: Bank<PedalStage>,
    workshop: Arc<Workshop>,
    order: usize,
    asked: Option<Stomp>,
    /// Old circuits waiting for the workshop to take them back.
    pending: Option<Box<Bundle>>,
    /// Offline: wait for a delivery rather than play without it.
    wait: bool,
    tap: Option<Arc<AnalysisTap>>,
}

impl PedalWorker {
    pub(super) fn new(
        bank: Bank<PedalStage>,
        workshop: Arc<Workshop>,
        order: usize,
        wait: bool,
        tap: Option<Arc<AnalysisTap>>,
    ) -> Self {
        Self {
            bank,
            workshop,
            order,
            asked: None,
            pending: None,
            wait,
            tap,
        }
    }

    /// Hand back what is pending; `false` while it cannot be.
    fn hand_back(&mut self) -> bool {
        let Some(bundle) = self.pending.take() else {
            return true;
        };
        let order = &self.workshop.orders[self.order];
        match order.returned.try_lock() {
            Some(mut slot) if slot.is_none() => {
                *slot = Some(bundle);
                drop(slot);
                self.workshop.wake();
                true
            }
            _ => {
                self.pending = Some(bundle);
                false
            }
        }
    }

    /// Ask for `want`'s circuits and swap them in once delivered; `true`
    /// when the installed circuits are `want`'s (or none are needed).
    fn fetch(&mut self, want: Stomp) -> bool {
        let installed = self
            .bank
            .first_mut()
            .map_or(Stomp::Empty, |u| u.installed());
        if want == Stomp::Empty || installed == want {
            self.hand_back();
            return true;
        }
        if !self.hand_back() {
            return false;
        }
        let order = &self.workshop.orders[self.order];
        if self.asked != Some(want) {
            order.wanted.store(want.index() + 1, Ordering::Release);
            self.asked = Some(want);
            self.workshop.wake();
        }
        let Some(mut delivery) = order.delivery.try_lock() else {
            return false;
        };
        if !delivery.as_ref().is_some_and(|b| b.stomp == want) {
            return false;
        }
        let Some(mut bundle) = delivery.take() else {
            return false;
        };
        drop(delivery);
        for (unit, circuit) in self.bank.units_mut().zip(bundle.circuits.iter_mut()) {
            unit.swap(circuit);
        }
        bundle.stomp = installed;
        self.pending = Some(bundle);
        self.hand_back();
        true
    }
}

impl Segments for PedalWorker {
    /// Which place it is (for the published treadle) and its settings.
    type Controls = (usize, StompSettings);
    type Reset = ();

    fn process(
        &mut self,
        channels: &mut [&mut [f32]],
        controls: &(usize, StompSettings),
        timing: &Timing,
        publish: &mut dyn FnMut(&[&mut [f32]], usize),
    ) {
        let (place, settings) = *controls;
        let count = self.bank.len();
        if channels.len() < 2 * count {
            return;
        }
        if !self.fetch(settings.stomp) && self.wait {
            let since = Instant::now();
            while !self.fetch(settings.stomp) && since.elapsed() < Duration::from_secs(5) {
                std::thread::sleep(Duration::from_millis(1));
            }
        }
        let deadline = timing
            .realtime
            .then(|| timing.due + timing.delay.mul_f64(0.7));
        for unit in self.bank.units_mut() {
            unit.set_realtime_deadline(deadline);
            unit.apply(&settings);
        }
        let frames = channels.first().map_or(0, |c| c.len());
        let first = if (1..frames).contains(&timing.first) {
            timing.first
        } else {
            frames
        };
        let work = |stage: &mut PedalStage, lanes: Lanes<'_>| {
            if stage.needs_operating_point() {
                stage.find_operating_point();
            }
            for x in lanes.audio.iter_mut() {
                *x = stage.process(f64::from(*x)) as f32;
            }
        };
        let active = {
            let (audio, _) = channels.split_at_mut(count);
            self.bank.active(audio)
        };
        for range in [0..first, first..frames] {
            if range.is_empty() {
                continue;
            }
            if range.start > 0 {
                publish(channels, range.start);
            }
            let (audio, raw) = channels.split_at_mut(count);
            self.bank
                .run(audio, &mut raw[..count], active, range.clone(), &work);
            if active == 1 && count == 2 {
                let (left, right) = audio.split_at_mut(1);
                right[0][range.clone()].copy_from_slice(&left[0][range]);
            }
        }
        if let (Some(tap), Some(unit)) = (&self.tap, self.bank.first_mut())
            && let Some(position) = unit.wah_position()
        {
            tap.set_value(
                super::value::TREADLE + place.min(super::MAX_PEDALS - 1),
                position as f32,
            );
        }
    }

    fn reset(&mut self, _: &()) {
        for unit in self.bank.units_mut() {
            unit.reset();
        }
        self.bank.reset();
    }
}

/// The amplifier stage, for every channel.
pub(super) struct AmpWorker {
    bank: Bank<Chain>,
}

impl AmpWorker {
    pub(super) fn new(bank: Bank<Chain>) -> Self {
        Self { bank }
    }
}

#[cfg(test)]
pub(super) trait Probe {
    fn force_stereo(&mut self);
    fn installed(&mut self) -> Stomp;
}

#[cfg(test)]
impl Probe for PedalWorker {
    fn force_stereo(&mut self) {
        self.bank.stereo_seen = true;
    }
    fn installed(&mut self) -> Stomp {
        self.bank
            .first_mut()
            .map_or(Stomp::Empty, |u| u.installed())
    }
}

#[cfg(test)]
impl Probe for AmpWorker {
    fn force_stereo(&mut self) {
        self.bank.stereo_seen = true;
    }
    fn installed(&mut self) -> Stomp {
        Stomp::Empty
    }
}

#[cfg(test)]
impl<W: Segments<Reset = ()> + Probe> StageRun<W> {
    pub(super) fn probe(&mut self) -> Option<&mut W> {
        match self {
            Self::Inline { worker, .. } => Some(worker),
            Self::Worker(_) => None,
        }
    }
}

impl Segments for AmpWorker {
    type Controls = chain::Settings;
    type Reset = ();

    fn process(
        &mut self,
        channels: &mut [&mut [f32]],
        controls: &chain::Settings,
        timing: &Timing,
        publish: &mut dyn FnMut(&[&mut [f32]], usize),
    ) {
        let count = self.bank.len();
        if channels.len() < 2 * count {
            return;
        }
        let deadline = timing
            .realtime
            .then(|| timing.due + timing.delay.mul_f64(0.7));
        for chain in self.bank.units_mut() {
            chain.set_realtime_deadline(deadline);
            chain.apply(controls);
        }
        let frames = channels.first().map_or(0, |c| c.len());
        let first = if (1..frames).contains(&timing.first) {
            timing.first
        } else {
            frames
        };
        let work = |chain: &mut Chain, lanes: Lanes<'_>| {
            if chain.needs_operating_point() {
                chain.find_operating_point();
            }
            for (x, raw) in lanes.audio.iter_mut().zip(lanes.raw.iter_mut()) {
                let f = chain.process(f64::from(*x), f64::from(*raw), false);
                *x = f.left as f32;
                *raw = f.dry as f32;
            }
        };
        let active = {
            let (audio, _) = channels.split_at_mut(count);
            self.bank.active(audio)
        };
        for range in [0..first, first..frames] {
            if range.is_empty() {
                continue;
            }
            if range.start > 0 {
                publish(channels, range.start);
            }
            let (audio, raw) = channels.split_at_mut(count);
            if active == 1 && count == 2 {
                // One amplifier for a mono source on a stereo bus: its two
                // microphones are placed in the stereo field.
                let Some(chain) = self.bank.first_mut() else {
                    return;
                };
                if chain.needs_operating_point() {
                    chain.find_operating_point();
                }
                let (left, right) = audio.split_at_mut(1);
                let (raw_left, raw_right) = raw[..count].split_at_mut(1);
                for i in range {
                    let f = chain.process(f64::from(left[0][i]), f64::from(raw_left[0][i]), true);
                    left[0][i] = f.left as f32;
                    right[0][i] = f.right as f32;
                    raw_left[0][i] = f.dry as f32;
                    raw_right[0][i] = f.dry as f32;
                }
            } else {
                self.bank
                    .run(audio, &mut raw[..count], active, range, &work);
            }
        }
    }

    fn reset(&mut self, _: &()) {
        for chain in self.bank.units_mut() {
            chain.reset();
        }
        self.bank.reset();
    }
}

/// How a line runs its stages.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Run {
    /// Offline and ahead: on the calling thread, with the same delays.
    Inline,
    /// Live: each stage on its reservoir's worker.
    Live,
    /// On workers that wait for every frame (tests: parity with inline).
    #[cfg_attr(not(test), allow(dead_code))]
    Deterministic,
}

/// A stage run on its reservoir's worker, or inline with the same delay.
pub(super) enum StageRun<W: Segments<Reset = ()>> {
    Inline {
        worker: W,
        delay: Vec<Vec<f32>>,
        cursor: usize,
    },
    Worker(Box<Reservoir<W>>),
}

impl<W: Segments<Reset = ()>> StageRun<W> {
    /// `lanes` channels (audio and guitar lanes), `delay` frames behind.
    pub(super) fn new(
        worker: W,
        lanes: usize,
        delay: usize,
        max_block: usize,
        sample_rate: f64,
        run: Run,
    ) -> std::io::Result<Self> {
        Ok(if run != Run::Inline && lanes <= reservoir::MAX_CHANNELS {
            Self::Worker(Box::new(Reservoir::try_new(
                reservoir::Config {
                    delay,
                    channels: lanes,
                    max_block,
                    sample_rate,
                    offline: run == Run::Deterministic,
                },
                Box::new(worker),
            )?))
        } else {
            Self::Inline {
                worker,
                delay: vec![vec![0.0; delay.max(1)]; lanes],
                cursor: 0,
            }
        })
    }

    pub(super) fn is_worker(&self) -> bool {
        matches!(self, Self::Worker(_))
    }

    /// `lanes` in place: what the stage made of the frames `delay` before.
    /// `false` when the worker has died.
    pub(super) fn process(
        &mut self,
        lanes: &mut [&mut [f32]],
        controls: W::Controls,
        deadline: Option<Instant>,
    ) -> bool {
        match self {
            Self::Inline {
                worker,
                delay,
                cursor,
            } => {
                let len = lanes.first().map_or(0, |l| l.len());
                worker.process(
                    lanes,
                    &controls,
                    &Timing {
                        due: Instant::now(),
                        delay: Duration::ZERO,
                        realtime: false,
                        first: len,
                    },
                    &mut |_, _| {},
                );
                let d = delay.first().map_or(1, Vec::len);
                for (lane, ring) in lanes.iter_mut().zip(delay.iter_mut()) {
                    for (i, sample) in lane.iter_mut().enumerate() {
                        std::mem::swap(sample, &mut ring[(*cursor + i) % d]);
                    }
                }
                *cursor = (*cursor + len) % d;
                true
            }
            Self::Worker(r) => {
                if !r.worker_alive() {
                    return false;
                }
                r.process_with_deadline(lanes, controls, deadline);
                true
            }
        }
    }

    pub(super) fn reset(&mut self) {
        match self {
            Self::Inline {
                worker,
                delay,
                cursor,
            } => {
                worker.reset(&());
                delay.iter_mut().for_each(|c| c.fill(0.0));
                *cursor = 0;
            }
            Self::Worker(r) => r.reset(()),
        }
    }

    /// Late frames and abandoned input so far.
    pub(super) fn underruns(&self) -> u64 {
        match self {
            Self::Inline { .. } => 0,
            Self::Worker(r) => r
                .stats()
                .underruns()
                .saturating_add(r.stats().input_overflows()),
        }
    }
}
