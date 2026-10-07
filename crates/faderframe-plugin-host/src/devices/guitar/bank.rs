//! A stage's channels, solved side by side exactly as the preamps' are
//! ([`crate::devices::preamp::PreampBank`]): the thread running the stage
//! and helper threads (which adopt its scheduling, see [`WorkerPool`]) take
//! a channel each, so a stereo signal is two threads. While a stereo pair is
//! exactly mono one unit runs; once the channels differ the dormant one
//! wakes from the running one's exact history, and both keep their tails
//! until a reset.

use faderframe_realtime::{PoolConfig, PoolJob, TryCell, WorkerPool};
use std::ops::Range;
use std::sync::atomic::{AtomicUsize, Ordering};

/// Channels solved in one pass of the helpers.
pub(super) const BATCH: usize = 32;

/// What a stage keeps per channel.
pub(super) trait Unit: Send {
    /// Take an identically configured unit's runtime state (never allocates).
    fn wake_from(&mut self, other: &Self);
}

/// One channel's audio and its guitar (DI) lane.
pub(super) struct Lanes<'a> {
    pub audio: &'a mut [f32],
    pub raw: &'a mut [f32],
}

pub(super) struct Bank<U> {
    units: Vec<TryCell<U>>,
    pub(super) stereo_seen: bool,
    helpers: Option<WorkerPool>,
}

impl<U: Unit> Bank<U> {
    /// Starts the helpers off the audio thread.
    pub(super) fn new(units: Vec<U>) -> Self {
        let channels = units.len();
        let helpers = (channels > 1).then(|| {
            let cores = faderframe_realtime::physical_cores()
                .saturating_sub(1)
                .max(1);
            WorkerPool::new(PoolConfig::new((channels - 1).min(cores).min(BATCH - 1)))
        });
        Self {
            units: units.into_iter().map(TryCell::new).collect(),
            stereo_seen: false,
            helpers,
        }
    }

    pub(super) fn len(&self) -> usize {
        self.units.len()
    }

    pub(super) fn units_mut(&mut self) -> impl Iterator<Item = &mut U> {
        self.units.iter_mut().map(TryCell::get_mut)
    }

    pub(super) fn first_mut(&mut self) -> Option<&mut U> {
        self.units.first_mut().map(TryCell::get_mut)
    }

    /// How many of the channels this segment solves: one while a stereo
    /// pair is still exactly mono, all of them once it is not -- waking the
    /// second from the first's exact history.
    pub(super) fn active(&mut self, audio: &[&mut [f32]]) -> usize {
        let count = self.units.len().min(audio.len());
        if !self.stereo_seen && count == 2 {
            if audio[0] == audio[1] {
                return 1;
            }
            let (left, right) = self.units.split_at_mut(1);
            right[0].get_mut().wake_from(left[0].get_mut());
            self.stereo_seen = true;
        }
        count
    }

    /// `work(unit, lanes)` for frames `range` of the first `active`
    /// channels, side by side.
    pub(super) fn run<F>(
        &mut self,
        audio: &mut [&mut [f32]],
        raw: &mut [&mut [f32]],
        active: usize,
        range: Range<usize>,
        work: &F,
    ) where
        F: Fn(&mut U, Lanes<'_>) + Sync,
    {
        let active = active.min(self.units.len()).min(audio.len()).min(raw.len());
        match &self.helpers {
            Some(helpers) if active > 1 => {
                for start in (0..active).step_by(BATCH) {
                    let n = (active - start).min(BATCH);
                    let mut lanes: [TryCell<Option<Lanes<'_>>>; BATCH] =
                        std::array::from_fn(|_| TryCell::new(None));
                    for ((cell, a), r) in lanes
                        .iter_mut()
                        .zip(audio[start..start + n].iter_mut())
                        .zip(raw[start..start + n].iter_mut())
                    {
                        *cell.get_mut() = Some(Lanes {
                            audio: &mut a[range.clone()],
                            raw: &mut r[range.clone()],
                        });
                    }
                    let job = Job {
                        units: &self.units[start..start + n],
                        lanes: &lanes[..n],
                        next: AtomicUsize::new(0),
                        work,
                    };
                    helpers.run(&job, n - 1);
                }
            }
            _ => {
                for ((unit, a), r) in self
                    .units
                    .iter_mut()
                    .zip(audio.iter_mut())
                    .zip(raw.iter_mut())
                    .take(active)
                {
                    work(
                        unit.get_mut(),
                        Lanes {
                            audio: &mut a[range.clone()],
                            raw: &mut r[range.clone()],
                        },
                    );
                }
            }
        }
    }

    pub(super) fn reset(&mut self) {
        self.stereo_seen = false;
    }
}

/// One pass of the helpers: each thread takes the next channel.
struct Job<'a, 'b, U, F> {
    units: &'a [TryCell<U>],
    lanes: &'a [TryCell<Option<Lanes<'b>>>],
    next: AtomicUsize,
    work: &'a F,
}

impl<U, F> PoolJob for Job<'_, '_, U, F>
where
    U: Send,
    F: Fn(&mut U, Lanes<'_>) + Sync,
{
    fn work(&self) {
        loop {
            let c = self.next.fetch_add(1, Ordering::Relaxed);
            let (Some(unit), Some(lanes)) = (self.units.get(c), self.lanes.get(c)) else {
                return;
            };
            // Each channel is taken by one thread only: these never fail.
            if let (Some(mut unit), Some(mut lanes)) = (unit.try_lock(), lanes.try_lock())
                && let Some(lanes) = lanes.take()
            {
                (self.work)(&mut unit, lanes);
            }
        }
    }
}
