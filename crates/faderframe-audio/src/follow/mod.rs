//! Several audio interfaces at once, for backends that open devices
//! themselves (the system APIs, the dummy device).
//!
//! One device is the clock master: its callback drives the engine. Every
//! other device the stream uses (a follower) runs a stream of its own, on
//! its own thread and clock; a ring carries its frames to the master's
//! callback (it records) or from it (it plays), and an adaptive resampler
//! ([`resample`]) steered by a drift loop ([`drift`]) turns them into
//! exactly the master's frames: the ring's fill — estimated at the master's
//! callback from both sides' timestamps ([`link::Stamp`]) — is held at a
//! target, so the follower neither drifts away nor gaps. A follower at
//! another nominal rate is resampled by the ratio.
//!
//! [`join`] wraps the engine's callback in a [`Joined`] callback the master
//! device drives: the stream's channels are the master's and the
//! followers' in the order the backend chose ([`Place`]); the engine sees
//! one device. The followers' device threads hold [`FollowerInput`] and
//! [`FollowerOutput`] ends; [`Followers`] (control side) attaches new ends
//! when a follower comes back (hot-plug: the master never stops), and says
//! how each is getting on (drift, gaps, latency). A follower's dropout,
//! stall or unplugging only silences its own channels (counted as gaps).
//!
//! Latency: a follower's input frame read in a master callback was
//! captured `target + device latency` follower frames before it — the
//! target fill includes the resampler's look-ahead —, while the master's
//! own input is a buffer old; so its channels' latency beyond the master's
//! buffer is `(target + device) · master/follower rate − buffer`
//! ([`DeviceStatus::input_latency`], per channel through
//! [`crate::channel_input_latency`]; recording compensation uses it). A
//! follower's output adds its target, the resampler's delay and its
//! device's own latency the same way.
//!
//! Realtime: [`Joined::process`], [`FollowerInput::capture_with`] and
//! [`FollowerOutput::play_with`] allocate nothing; links are made on the
//! control side and handed over through a queue, retired ones come back
//! through another ([`Followers::collect`] drops them).

pub mod drift;
pub mod link;
pub mod resample;

#[cfg(test)]
mod tests;

pub use link::{FollowerInput, FollowerOutput, LinkStatus};

use crate::{AudioCallback, DeviceBuffers, DeviceStatus, MAX_BUFFER_SIZE, StreamInfo};
use link::{InLink, Link, OutLink, Shared};
use std::sync::atomic::Ordering;
use std::sync::{Arc, OnceLock};
use std::time::Instant;

/// Nanoseconds on the process's monotonic clock: what followers stamp
/// their callbacks with and the master's callback reads (realtime safe).
pub fn now_ns() -> u64 {
    static EPOCH: OnceLock<Instant> = OnceLock::new();
    EPOCH.get_or_init(Instant::now).elapsed().as_nanos() as u64
}

/// Which way a follower's audio goes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Direction {
    /// It records: its frames reach the engine's inputs.
    Input,
    /// It plays: the engine's outputs reach it.
    Output,
}

/// Where one of the stream's channels comes from (inputs) or goes to
/// (outputs).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Place {
    /// A channel of the master device.
    Master(u16),
    /// Channel `channel` of follower `follower` (an index into the specs
    /// given to [`join`]).
    Follower { follower: u16, channel: u16 },
}

/// One follower direction of one device.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FollowerSpec {
    /// The device's id (as [`crate::DeviceInfo::id`]).
    pub device: String,
    pub name: String,
    pub direction: Direction,
    pub channels: u16,
    /// The rate it runs at.
    pub sample_rate: u32,
    /// Frames per callback it is expected to deliver (the largest seen
    /// counts once it runs).
    pub block: u32,
}

/// A follower's device end, for its stream's callback.
#[derive(Debug)]
pub enum FollowerEnd {
    Input(FollowerInput),
    Output(FollowerOutput),
}

/// A link waiting in the queue for the master's callback, and the ones it
/// gave back.
const QUEUE: usize = 4;
const RETIRED: usize = 8;

/// Silence for channels nobody feeds.
static SILENCE: [f32; MAX_BUFFER_SIZE as usize] = [0.0; MAX_BUFFER_SIZE as usize];

struct Slot {
    direction: Direction,
    link: Option<Box<Link>>,
    incoming: rtrb::Consumer<Box<Link>>,
    retired: rtrb::Producer<Box<Link>>,
    /// The follower's channels for one block (`MAX_BUFFER_SIZE` each).
    bufs: Box<[Box<[f32]>]>,
}

/// The engine's callback with followers joined to the master's channels
/// (see the module docs). The master device's backend drives it like any
/// callback.
pub struct Joined {
    inner: Box<dyn AudioCallback>,
    /// As the inner callback sees the stream.
    info: StreamInfo,
    inputs: Box<[Place]>,
    outputs: Box<[Place]>,
    slots: Box<[Slot]>,
    /// The master's nominal buffer (frames).
    block: usize,
    /// Written for outputs the master device does not have.
    spare: Box<[f32]>,
}

/// The control side of a stream's followers.
pub struct Followers {
    slots: Vec<ControlSlot>,
    master_rate: u32,
    master_block: u32,
}

struct ControlSlot {
    spec: FollowerSpec,
    to_rt: rtrb::Producer<Box<Link>>,
    retired: rtrb::Consumer<Box<Link>>,
    status: Arc<LinkStatus>,
    /// The newest link's shared state (its device's misses).
    shared: Arc<Shared>,
}

fn make_link(
    spec: &FollowerSpec,
    master_rate: u32,
    master_block: u32,
    status: &Arc<LinkStatus>,
    drift: f64,
) -> (Box<Link>, FollowerEnd, Arc<Shared>) {
    let channels = usize::from(spec.channels);
    let rate = f64::from(spec.sample_rate.max(1));
    let master = f64::from(master_rate.max(1));
    let (mut link, end) = match spec.direction {
        Direction::Input => {
            let (l, e) = InLink::new(
                channels,
                rate,
                master,
                spec.block,
                Arc::clone(status),
                drift,
            );
            (Link::In(l), FollowerEnd::Input(e))
        }
        Direction::Output => {
            let (l, e) = OutLink::new(
                channels,
                rate,
                master,
                spec.block,
                Arc::clone(status),
                drift,
            );
            (Link::Out(l), FollowerEnd::Output(e))
        }
    };
    link.note_block(master_block as usize);
    let shared = link.shared();
    (Box::new(link), end, shared)
}

/// Join `followers` to a master device running at `master_rate` with
/// buffers of `master_block` frames: the stream's input channels are
/// `inputs`, its outputs `outputs` (see [`Place`]); `inner` sees them as
/// one device. Returns the callback the master drives, the control side
/// and a device end per follower (in the order of `followers`) for the
/// streams the backend opens. Control side (allocates).
pub fn join(
    inner: Box<dyn AudioCallback>,
    info: &StreamInfo,
    inputs: Vec<Place>,
    outputs: Vec<Place>,
    followers: Vec<FollowerSpec>,
) -> (Joined, Followers, Vec<FollowerEnd>) {
    now_ns();
    let master_rate = info.sample_rate;
    let master_block = info.buffer_size;
    let mut slots = Vec::with_capacity(followers.len());
    let mut control = Vec::with_capacity(followers.len());
    let mut ends = Vec::with_capacity(followers.len());
    for spec in followers {
        let status = Arc::new(LinkStatus::default());
        let (link, end, shared) = make_link(&spec, master_rate, master_block, &status, 0.0);
        let (to_rt, incoming) = rtrb::RingBuffer::new(QUEUE);
        let (back, retired) = rtrb::RingBuffer::new(RETIRED);
        slots.push(Slot {
            direction: spec.direction,
            link: Some(link),
            incoming,
            retired: back,
            bufs: (0..spec.channels)
                .map(|_| vec![0.0f32; MAX_BUFFER_SIZE as usize].into_boxed_slice())
                .collect(),
        });
        control.push(ControlSlot {
            spec,
            to_rt,
            retired,
            status,
            shared,
        });
        ends.push(end);
    }
    let mut info = info.clone();
    info.input_channels = inputs.len() as u16;
    info.output_channels = outputs.len() as u16;
    (
        Joined {
            inner,
            info,
            inputs: inputs.into_boxed_slice(),
            outputs: outputs.into_boxed_slice(),
            slots: slots.into_boxed_slice(),
            block: master_block as usize,
            spare: vec![0.0f32; MAX_BUFFER_SIZE as usize].into_boxed_slice(),
        },
        Followers {
            slots: control,
            master_rate,
            master_block,
        },
        ends,
    )
}

/// The stream's buffers as the inner callback sees them: the master's
/// channels and the followers', one block (`frames` from `offset` of the
/// master's).
struct View<'a> {
    io: &'a mut dyn DeviceBuffers,
    slots: &'a mut [Slot],
    inputs: &'a [Place],
    outputs: &'a [Place],
    spare: &'a mut [f32],
    offset: usize,
    frames: usize,
}

impl DeviceBuffers for View<'_> {
    fn frames(&self) -> usize {
        self.frames
    }

    fn input_channels(&self) -> usize {
        self.inputs.len()
    }

    fn output_channels(&self) -> usize {
        self.outputs.len()
    }

    fn input(&self, channel: usize) -> &[f32] {
        let (o, n) = (self.offset, self.frames);
        match self.inputs.get(channel) {
            Some(Place::Master(m)) if usize::from(*m) < self.io.input_channels() => self
                .io
                .input(usize::from(*m))
                .get(o..o + n)
                .unwrap_or(&SILENCE[..n]),
            Some(Place::Follower { follower, channel }) => self
                .slots
                .get(usize::from(*follower))
                .and_then(|s| s.bufs.get(usize::from(*channel)))
                .map_or(&SILENCE[..n], |b| &b[..n]),
            _ => &SILENCE[..n],
        }
    }

    fn output(&mut self, channel: usize) -> &mut [f32] {
        let (o, n) = (self.offset, self.frames);
        match self.outputs.get(channel) {
            Some(Place::Master(m)) if usize::from(*m) < self.io.output_channels() => {
                let out = self.io.output(usize::from(*m));
                if out.len() >= o + n {
                    return &mut out[o..o + n];
                }
                &mut self.spare[..n]
            }
            Some(Place::Follower { follower, channel }) => {
                match self
                    .slots
                    .get_mut(usize::from(*follower))
                    .and_then(|s| s.bufs.get_mut(usize::from(*channel)))
                {
                    Some(b) => &mut b[..n],
                    None => &mut self.spare[..n],
                }
            }
            _ => &mut self.spare[..n],
        }
    }
}

impl Joined {
    /// The stream as the inner callback sees it.
    pub fn info(&self) -> &StreamInfo {
        &self.info
    }

    /// Take links the control side attached (a follower back).
    fn take_new_links(&mut self) {
        for slot in self.slots.iter_mut() {
            while slot.retired.slots() > 0 {
                let Ok(new) = slot.incoming.pop() else {
                    break;
                };
                if let Some(old) = slot.link.replace(new) {
                    // Room checked above: nothing is dropped here.
                    let _ = slot.retired.push(old);
                }
            }
        }
    }

    /// One master callback at `now_ns` ([`now_ns`], or a simulated clock).
    /// Realtime safe.
    pub fn process_at(&mut self, io: &mut dyn DeviceBuffers, now_ns: u64) {
        self.take_new_links();
        let total = io.frames();
        let rate = f64::from(self.info.sample_rate.max(1));
        let mut offset = 0;
        while offset < total {
            let n = (total - offset).min(MAX_BUFFER_SIZE as usize);
            let now = now_ns + (offset as f64 * 1e9 / rate) as u64;
            let block = self.block.max(n);
            for slot in self.slots.iter_mut() {
                match (slot.direction, slot.link.as_deref_mut()) {
                    (Direction::Input, Some(Link::In(l))) => l.pull(n, block, now, &mut slot.bufs),
                    _ => {
                        for b in slot.bufs.iter_mut() {
                            b[..n].fill(0.0);
                        }
                    }
                }
            }
            let mut view = View {
                io: &mut *io,
                slots: &mut self.slots,
                inputs: &self.inputs,
                outputs: &self.outputs,
                spare: &mut self.spare,
                offset,
                frames: n,
            };
            self.inner.process(&mut view);
            for slot in self.slots.iter_mut() {
                if let Some(Link::Out(l)) = slot.link.as_deref_mut() {
                    l.push(n, block, now, &slot.bufs);
                }
            }
            offset += n;
        }
    }
}

impl AudioCallback for Joined {
    fn prepare(&mut self, info: &StreamInfo) {
        // Field by field: no allocation (backends may call this from their
        // audio thread when a latency changes).
        self.info.sample_rate = info.sample_rate;
        self.info.buffer_size = info.buffer_size;
        self.info.input_latency = info.input_latency;
        self.info.output_latency = info.output_latency;
        self.block = info.buffer_size as usize;
        self.inner.prepare(&self.info);
    }

    fn process(&mut self, io: &mut dyn DeviceBuffers) {
        self.process_at(io, now_ns());
    }
}

/// How a follower is getting on.
#[derive(Clone, Debug, PartialEq)]
pub struct FollowerState {
    pub spec: FollowerSpec,
    /// Delivering (callbacks arriving).
    pub present: bool,
    /// The drift loop has settled.
    pub locked: bool,
    /// Its clock against the master's (parts per million; positive: it
    /// runs fast).
    pub drift_ppm: f64,
    /// Latency of its channels beyond the master's buffer (master frames).
    pub latency: u32,
    /// The fill aimed at and how far off it is (follower frames).
    pub target: u32,
    pub error: f64,
    /// Gaps so far (it went dry or over, went away, came back).
    pub gaps: u64,
    /// Frames its device lost to a full ring (input) or callbacks it was
    /// short of frames (output), for the newest link.
    pub misses: u64,
    /// Times its stream was (re)attached.
    pub attached: u64,
}

impl Followers {
    pub fn len(&self) -> usize {
        self.slots.len()
    }

    pub fn is_empty(&self) -> bool {
        self.slots.is_empty()
    }

    pub fn spec(&self, follower: usize) -> Option<&FollowerSpec> {
        self.slots.get(follower).map(|s| &s.spec)
    }

    pub fn master_rate(&self) -> u32 {
        self.master_rate
    }

    /// A new link for `follower` (its device is back, perhaps at another
    /// rate or block size): its device end for the stream the backend
    /// opens; the master's callback takes the link at its next block, the
    /// drift found so far carried over. `None` if the queue is full.
    pub fn attach(&mut self, follower: usize, sample_rate: u32, block: u32) -> Option<FollowerEnd> {
        self.collect();
        let (master_rate, master_block) = (self.master_rate, self.master_block);
        let slot = self.slots.get_mut(follower)?;
        if slot.to_rt.slots() == 0 {
            return None;
        }
        slot.spec.sample_rate = sample_rate;
        slot.spec.block = block;
        let drift = slot.status.drift();
        let (link, end, shared) =
            make_link(&slot.spec, master_rate, master_block, &slot.status, drift);
        slot.shared = shared;
        slot.to_rt.push(link).ok()?;
        Some(end)
    }

    /// The master now runs at `rate` with buffers of `block` frames (its
    /// audio system changed them while running): links attached from now
    /// on resample to it — attach every follower again
    /// ([`Self::attach`]); the old links' channels are silent meanwhile.
    pub fn set_master(&mut self, rate: u32, block: u32) {
        self.master_rate = rate;
        self.master_block = block;
    }

    /// Drop links the master's callback gave back.
    pub fn collect(&mut self) {
        for slot in &mut self.slots {
            while slot.retired.pop().is_ok() {}
        }
    }

    pub fn state(&self, follower: usize) -> Option<FollowerState> {
        let s = self.slots.get(follower)?;
        let st = &s.status;
        Some(FollowerState {
            spec: s.spec.clone(),
            present: st.present.load(Ordering::Relaxed),
            locked: st.locked.load(Ordering::Relaxed),
            drift_ppm: st.drift() * 1e6,
            latency: st.latency.load(Ordering::Relaxed),
            target: st.target.load(Ordering::Relaxed),
            error: st.error(),
            gaps: st.gaps.load(Ordering::Relaxed),
            misses: s.shared.misses.load(Ordering::Relaxed),
            attached: st.attached.load(Ordering::Relaxed),
        })
    }

    pub fn states(&self) -> Vec<FollowerState> {
        (0..self.slots.len())
            .filter_map(|k| self.state(k))
            .collect()
    }

    /// The stream's devices for [`crate::AudioStream::devices`]: `master`
    /// (filled in by the backend: its channels, latencies) first, then each
    /// follower device (both directions of one device together), present
    /// or not as `away` says (the backend's view: a device unplugged).
    pub fn describe(
        &self,
        master: DeviceStatus,
        inputs: &[Place],
        outputs: &[Place],
        away: impl Fn(&str) -> bool,
    ) -> Vec<DeviceStatus> {
        let mut out = vec![master];
        for (k, state) in self.states().into_iter().enumerate() {
            let k = k as u16;
            let i = match out
                .iter()
                .position(|d| !d.master && d.id == state.spec.device)
            {
                Some(i) => i,
                None => {
                    out.push(DeviceStatus {
                        id: state.spec.device.clone(),
                        name: state.spec.name.clone(),
                        master: false,
                        present: true,
                        sample_rate: state.spec.sample_rate,
                        locked: true,
                        ..DeviceStatus::default()
                    });
                    out.len() - 1
                }
            };
            let d = &mut out[i];
            let places = match state.spec.direction {
                Direction::Input => inputs,
                Direction::Output => outputs,
            };
            let channels: Vec<u16> = places
                .iter()
                .enumerate()
                .filter(|(_, p)| matches!(p, Place::Follower { follower, .. } if *follower == k))
                .map(|(c, _)| c as u16)
                .collect();
            match state.spec.direction {
                Direction::Input => {
                    d.inputs = channels;
                    d.input_latency = state.latency;
                }
                Direction::Output => {
                    d.outputs = channels;
                    d.output_latency = state.latency;
                }
            }
            d.present &= state.present && !away(&state.spec.device);
            d.locked &= state.locked;
            d.drift_ppm = state.drift_ppm;
            d.gaps += state.gaps;
            d.sample_rate = state.spec.sample_rate;
        }
        out
    }
}

impl std::fmt::Debug for Followers {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Followers")
            .field("master_rate", &self.master_rate)
            .field(
                "followers",
                &self.slots.iter().map(|s| &s.spec).collect::<Vec<_>>(),
            )
            .finish()
    }
}
