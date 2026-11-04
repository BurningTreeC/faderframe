//! The resampler, the drift loop and the links on simulated clocks: a
//! master and followers whose clocks run off by known amounts, their
//! callbacks jittered, minutes of audio checked sample by sample.

use super::link::{InLink, LinkStatus, OutLink};
use super::resample::Resampler;
use super::*;
use std::f64::consts::TAU;

/// A small deterministic generator for jitter.
struct Lcg(u64);

impl Lcg {
    /// Uniform in −1…1.
    fn next(&mut self) -> f64 {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        ((self.0 >> 11) as f64 / (1u64 << 53) as f64) * 2.0 - 1.0
    }
}

fn bufs(channels: usize) -> Box<[Box<[f32]>]> {
    (0..channels)
        .map(|_| vec![0.0f32; MAX_BUFFER_SIZE as usize].into_boxed_slice())
        .collect()
}

/// Residual of a sine's three-term recurrence (`y[j+1] + y[j−1] =
/// 2 cos ω · y[j]`): small for a clean sine of angular step `w`, large at
/// any slip, gap or click.
fn worst_residual(y: &[f32], w: f64) -> f64 {
    let c = 2.0 * w.cos();
    y.windows(3)
        .map(|v| (f64::from(v[2]) + f64::from(v[0]) - c * f64::from(v[1])).abs())
        .fold(0.0, f64::max)
}

/// Read `out_frames` outputs of a sine of `w` radians per input frame at
/// a step that moves slowly by `wobble` (a share) round `step`; returns
/// the worst error against the ideal sine read at the same positions,
/// after the start, in dB below the amplitude.
fn noise_floor(step: f64, wobble: f64, w: f64) -> f64 {
    let mut r = Resampler::new(1, step, 4096 * 3);
    let mut out = bufs(1);
    let mut input = vec![0.0f32; 4096 * 3];
    let mut pushed = 0u64;
    let mut t = 0.0f64; // input frames, from the first one pushed
    let mut worst = 0.0f64;
    for block in 0..400 {
        let s = step * (1.0 + wobble * (block as f64 * 0.05).sin());
        r.set_step(s);
        let need = r.needed(256);
        for (i, v) in input.iter_mut().take(need).enumerate() {
            *v = (0.5 * ((pushed + i as u64) as f64 * w).sin()) as f32;
        }
        r.push(need, |_| &input[..need]);
        pushed += need as u64;
        let got = r.read(256, &mut out, 0);
        assert_eq!(got, 256);
        for j in 0..256 {
            let ideal = 0.5 * (t * w).sin();
            if block > 4 {
                worst = worst.max((f64::from(out[0][j]) - ideal).abs());
            }
            t += s;
        }
    }
    20.0 * (worst / 0.5).log10()
}

#[test]
fn the_resampler_reads_a_sine_at_any_step_cleanly() {
    let w = TAU * 1000.0 / 48_000.0;
    // Drift (a ratio near one moving by parts per million), and rates.
    for (step, label) in [
        (1.0, "1:1"),
        (1.000_15, "+150 ppm"),
        (44_100.0 / 48_000.0, "44.1 → 48 kHz"),
        (96_000.0 / 48_000.0, "96 → 48 kHz"),
        (48_000.0 / 44_100.0, "48 → 44.1 kHz"),
    ] {
        let db = noise_floor(step, 0.0005, w);
        eprintln!("{label}: worst error {db:.1} dB");
        assert!(db < -105.0, "{label}: error {db:.1} dB");
    }
    // High in the passband (18 kHz at 48 kHz): still clean.
    let db = noise_floor(1.0002, 0.0005, TAU * 18_000.0 / 48_000.0);
    eprintln!("18 kHz: worst error {db:.1} dB");
    assert!(db < -90.0, "18 kHz: error {db:.1} dB");
}

#[test]
fn the_resampler_follows_its_step_frame_for_frame() {
    // Over a long run at a step a little above one, the frames taken in
    // equal the frames read out times the step (no frame lost or made up).
    let step = 1.000_25;
    let mut r = Resampler::new(2, step, 8192);
    let mut out = bufs(2);
    let ones = vec![1.0f32; 8192];
    let mut pushed = 0usize;
    let mut read = 0usize;
    for _ in 0..2000 {
        let need = r.needed(512);
        r.push(need, |_| &ones[..need]);
        pushed += need;
        read += r.read(512, &mut out, 0);
    }
    let consumed = pushed as f64 - r.ahead();
    let expected = read as f64 * step;
    assert!(
        (consumed - expected).abs() < 2.0,
        "{consumed} frames consumed, {expected} expected"
    );
    // Unity gain (a DC signal stays one).
    assert!(out[1][..512].iter().all(|v| (v - 1.0).abs() < 1e-5));
}

/// A simulated follower that records a sine in its own frames.
struct Recorder {
    end: FollowerInput,
    /// Its frames a (virtual) second.
    true_rate: f64,
    block: usize,
    next: f64,
    frames: u64,
    w: f64,
    jitter: f64,
    rng: Lcg,
}

impl Recorder {
    /// Run its callbacks up to `t` (seconds).
    fn run_until(&mut self, t: f64) {
        while self.next <= t {
            let at = self.next + self.jitter * self.rng.next();
            let (f0, w) = (self.frames, self.w);
            self.end
                .capture_with(self.block, (at.max(0.0) * 1e9) as u64, |i, _| {
                    (0.5 * ((f0 + i as u64) as f64 * w).sin()) as f32
                });
            self.frames += self.block as u64;
            self.next += self.block as f64 / self.true_rate;
        }
    }
}

/// Minutes of a follower recording into a master, each on its own clock:
/// returns the master's frames of it and the link's status.
fn record_through(
    master_rate: f64,
    master_block: usize,
    follower_rate: f64,
    ppm: f64,
    block: usize,
    jitter: f64,
    seconds: f64,
) -> (Vec<f32>, Arc<LinkStatus>, f64) {
    let status = Arc::new(LinkStatus::default());
    let (mut link, end) = InLink::new(
        1,
        follower_rate,
        master_rate,
        block as u32,
        Arc::clone(&status),
        0.0,
    );
    let tone = 997.0;
    let mut f = Recorder {
        end,
        true_rate: follower_rate * (1.0 + ppm * 1e-6),
        block,
        next: block as f64 / (follower_rate * (1.0 + ppm * 1e-6)),
        frames: 0,
        w: TAU * tone / follower_rate,
        jitter,
        rng: Lcg(7),
    };
    let mut out = bufs(1);
    let mut heard = Vec::new();
    let callbacks = (seconds * master_rate / master_block as f64) as usize;
    for k in 0..callbacks {
        let t = k as f64 * master_block as f64 / master_rate;
        f.run_until(t);
        link.pull(master_block, master_block, (t * 1e9) as u64, &mut out);
        heard.extend_from_slice(&out[0][..master_block]);
    }
    // The tone in the master's frames: the follower's clock runs `ppm`
    // fast, so it sounds that much higher.
    let w = TAU * tone * (1.0 + ppm * 1e-6) / master_rate;
    (heard, status, w)
}

#[test]
fn a_drifting_follower_records_for_minutes_without_a_slip() {
    // 10 minutes, the follower 120 ppm fast with 10 ms callbacks jittered
    // by ±0.5 ms against the master's 256-frame ones.
    let (heard, status, w) = record_through(48_000.0, 256, 48_000.0, 120.0, 480, 0.0005, 600.0);
    let settled = 48_000 * 2;
    let worst = worst_residual(&heard[settled..], w);
    assert!(worst < 2e-3, "a slip or click: residual {worst}");
    assert_eq!(
        status.gaps.load(Ordering::Relaxed),
        0,
        "the ring never ran dry or over"
    );
    let drift = status.drift() * 1e6;
    assert!(
        (drift - 120.0).abs() < 1.0,
        "the drift found: {drift:.2} ppm"
    );
    assert!(status.locked.load(Ordering::Relaxed));
    // The fill stays at its target, so the latency reported holds.
    assert!(
        status.error().abs() < 4.0,
        "fill error {:.2} frames",
        status.error()
    );
}

#[test]
fn a_follower_at_another_rate_is_resampled_by_the_ratio() {
    // A 44.1 kHz interface 60 ppm slow following a 48 kHz master.
    let (heard, status, w) = record_through(48_000.0, 128, 44_100.0, -60.0, 441, 0.0002, 120.0);
    let worst = worst_residual(&heard[48_000 * 2..], w);
    assert!(worst < 2e-3, "residual {worst}");
    assert_eq!(status.gaps.load(Ordering::Relaxed), 0);
    let drift = status.drift() * 1e6;
    assert!((drift + 60.0).abs() < 1.0, "drift {drift:.2} ppm");
    // And down: a 96 kHz device into a 48 kHz master.
    let (heard, status, w) = record_through(48_000.0, 512, 96_000.0, 35.0, 1024, 0.0003, 120.0);
    assert!(worst_residual(&heard[48_000 * 2..], w) < 2e-3);
    assert_eq!(status.gaps.load(Ordering::Relaxed), 0);
    assert!((status.drift() * 1e6 - 35.0).abs() < 1.0);
}

#[test]
fn the_reported_latency_is_how_old_a_followers_frames_are() {
    // A click captured by the follower at a known moment arrives in the
    // master's frames `latency + buffer` frames after that moment (the
    // master's own input is a buffer old).
    let (rate, block) = (48_000.0, 256usize);
    let status = Arc::new(LinkStatus::default());
    let (mut link, mut end) = InLink::new(1, rate, rate, 480, Arc::clone(&status), 0.0);
    let ppm = 40.0;
    let true_rate = rate * (1.0 + ppm * 1e-6);
    let mut next = 480.0 / true_rate;
    let mut frames = 0u64;
    // A smooth pulse (2 ms raised cosine) centred at 20 s on the
    // follower's clock: its frame `20 s × true rate`.
    let centre = 20.0 * true_rate;
    let pulse = |x: f64| {
        let d = (x - centre) / (0.002 * rate);
        if d.abs() < 0.5 {
            0.5 * (1.0 + (TAU * d).cos())
        } else {
            0.0
        }
    };
    let mut out = bufs(1);
    let mut heard = Vec::new();
    for k in 0..(25.0 * rate / block as f64) as usize {
        let t = k as f64 * block as f64 / rate;
        while next <= t {
            let f0 = frames;
            end.capture_with(480, (next * 1e9) as u64, |i, _| {
                pulse((f0 + i as u64) as f64) as f32
            });
            frames += 480;
            next += 480.0 / true_rate;
        }
        link.pull(block, block, (t * 1e9) as u64, &mut out);
        heard.extend_from_slice(&out[0][..block]);
    }
    // Where the pulse is in the master's frames (its centroid).
    let (mut m0, mut m1) = (0.0, 0.0);
    for (j, v) in heard.iter().enumerate() {
        m0 += f64::from(*v);
        m1 += f64::from(*v) * j as f64;
    }
    let at = m1 / m0;
    // The follower's frame x was captured at x / true rate (seconds); the
    // master's frame j of a callback at t was captured at t − (block − j)
    // / rate plus the latency beyond the buffer.
    let latency = f64::from(status.latency.load(Ordering::Relaxed));
    let expected = 20.0 * rate + latency + block as f64;
    assert!(
        (at - expected).abs() < 1.0,
        "the pulse at {at:.1}, expected {expected:.1} (latency {latency})"
    );
}

#[test]
fn a_follower_that_plays_keeps_its_ring_at_the_target() {
    // The master feeds a 50 ppm slow follower for two minutes; what the
    // follower plays is a clean sine at its own rate.
    let (rate, block) = (48_000.0, 256usize);
    let status = Arc::new(LinkStatus::default());
    let (mut link, mut end) = OutLink::new(1, 44_100.0, rate, 441, Arc::clone(&status), 0.0);
    let ppm = -50.0;
    let true_rate = 44_100.0 * (1.0 + ppm * 1e-6);
    let mut next = 441.0 / true_rate;
    let mut src = bufs(1);
    let w = TAU * 1000.0 / rate;
    let mut played = Vec::new();
    let mut rng = Lcg(3);
    let mut j = 0u64;
    for k in 0..(120.0 * rate / block as f64) as usize {
        let t = k as f64 * block as f64 / rate;
        for v in src[0][..block].iter_mut() {
            *v = (0.5 * (j as f64 * w).sin()) as f32;
            j += 1;
        }
        link.push(block, block, (t * 1e9) as u64, &src);
        let t_next = (k + 1) as f64 * block as f64 / rate;
        while next <= t_next {
            let at = next + 0.0003 * rng.next();
            let mut buf = [0.0f32; 441];
            end.play_with(441, (at * 1e9) as u64, |i, _, v| buf[i] = v);
            played.extend_from_slice(&buf);
            next += 441.0 / true_rate;
        }
    }
    // In the follower's frames the tone is 1 kHz played at its true rate:
    // 1000 / true_rate radians-per-frame turns.
    let w_out = TAU * 1000.0 / true_rate;
    let worst = worst_residual(&played[44_100 * 2..], w_out);
    assert!(worst < 2e-3, "residual {worst}");
    assert_eq!(status.gaps.load(Ordering::Relaxed), 0);
    let drift = status.drift() * 1e6;
    assert!((drift - ppm).abs() < 1.0, "drift {drift:.2} ppm");
}

/// An inner callback that copies input 0 to every output and counts.
struct Copy {
    frames: Arc<std::sync::atomic::AtomicU64>,
}

impl AudioCallback for Copy {
    fn prepare(&mut self, _info: &StreamInfo) {}
    fn process(&mut self, io: &mut dyn DeviceBuffers) {
        let n = io.frames();
        let mut first = [0.0f32; MAX_BUFFER_SIZE as usize];
        if io.input_channels() > 0 {
            first[..n].copy_from_slice(io.input(io.input_channels() - 1));
        }
        for c in 0..io.output_channels() {
            io.output(c).copy_from_slice(&first[..n]);
        }
        self.frames.fetch_add(n as u64, Ordering::Relaxed);
    }
}

fn info(rate: u32, block: u32) -> StreamInfo {
    StreamInfo {
        backend: "test",
        device: "master".into(),
        sample_rate: rate,
        buffer_size: block,
        input_channels: 2,
        output_channels: 2,
        input_latency: 0,
        output_latency: 0,
    }
}

/// A follower recording a sine into a joined stream, on a simulated clock.
struct Sim {
    true_rate: f64,
    next: f64,
    phase: u64,
    w: f64,
    heard: Vec<f32>,
}

impl Sim {
    /// Master callbacks `range` (256 frames at 48 kHz), the follower
    /// delivering through `end` (away without one); output 0 heard.
    fn run(
        &mut self,
        mut end: Option<&mut FollowerInput>,
        range: std::ops::Range<usize>,
        joined: &mut Joined,
        io: &mut crate::OwnedBuffers,
    ) {
        for k in range {
            let t = k as f64 * 256.0 / 48_000.0;
            if let Some(e) = end.as_deref_mut() {
                while self.next <= t {
                    let (p0, w) = (self.phase, self.w);
                    e.capture_with(256, (self.next * 1e9) as u64, |i, _| {
                        (0.5 * ((p0 + i as u64) as f64 * w).sin()) as f32
                    });
                    self.phase += 256;
                    self.next += 256.0 / self.true_rate;
                }
            } else {
                self.next = t;
            }
            joined.process_at(io, (t * 1e9) as u64);
            self.heard.extend_from_slice(io.output_ref(0));
        }
    }
}

#[test]
fn a_follower_that_goes_away_leaves_the_master_running_and_comes_back() {
    let frames = Arc::new(std::sync::atomic::AtomicU64::new(0));
    let spec = FollowerSpec {
        device: "mic".into(),
        name: "Mic".into(),
        direction: Direction::Input,
        channels: 1,
        sample_rate: 48_000,
        block: 256,
    };
    let (mut joined, mut followers, ends) = join(
        Box::new(Copy {
            frames: Arc::clone(&frames),
        }),
        &info(48_000, 256),
        vec![
            Place::Master(0),
            Place::Follower {
                follower: 0,
                channel: 0,
            },
        ],
        vec![Place::Master(0), Place::Master(1)],
        vec![spec],
    );
    assert_eq!(joined.info().input_channels, 2);
    let Some(FollowerEnd::Input(mut end)) = ends.into_iter().next() else {
        panic!("an input end");
    };
    let mut io = crate::OwnedBuffers::new(1, 2, 256);
    let mut sim = Sim {
        true_rate: 48_000.0 * (1.0 + 80e-6),
        next: 0.0,
        phase: 0,
        w: TAU * 500.0 / 48_000.0,
        heard: Vec::new(),
    };
    // Thirty seconds with the follower, two without (unplugged), then the
    // backend attaches its stream again.
    sim.run(Some(&mut end), 0..5625, &mut joined, &mut io);
    let state = followers.state(0).unwrap();
    assert!(state.present && state.locked, "{state:?}");
    let drift = state.drift_ppm;
    sim.run(None, 5625..6000, &mut joined, &mut io);
    let state = followers.state(0).unwrap();
    assert!(!state.present, "away");
    assert_eq!(
        frames.load(Ordering::Relaxed),
        6000 * 256,
        "the master never stopped"
    );
    let quiet = &sim.heard[5750 * 256..6000 * 256];
    assert!(quiet.iter().all(|v| *v == 0.0), "its channels are silent");
    let Some(FollowerEnd::Input(mut again)) = followers.attach(0, 48_000, 256) else {
        panic!("attached");
    };
    sim.phase = 0;
    sim.run(Some(&mut again), 6000..7500, &mut joined, &mut io);
    followers.collect();
    let state = followers.state(0).unwrap();
    assert!(state.present && state.attached == 2, "{state:?}");
    assert!(
        (state.drift_ppm - drift).abs() < 2.0 && (state.drift_ppm - 80.0).abs() < 2.0,
        "the drift carried over: {} then {}",
        drift,
        state.drift_ppm
    );
    let heard = sim.heard;
    // Clean again after the fade-in.
    let back = &heard[6350 * 256..7500 * 256];
    assert!(worst_residual(back, TAU * 500.0 * (1.0 + 80e-6) / 48_000.0) < 2e-3);
    assert!(back.iter().any(|v| v.abs() > 0.4), "it plays again");
}

#[test]
fn the_followers_devices_are_described_with_their_channels() {
    let spec = |device: &str, direction, channels| FollowerSpec {
        device: device.into(),
        name: device.to_uppercase(),
        direction,
        channels,
        sample_rate: 44_100,
        block: 441,
    };
    let (_joined, followers, _ends) = join(
        Box::new(Copy {
            frames: Arc::default(),
        }),
        &info(48_000, 256),
        vec![
            Place::Follower {
                follower: 0,
                channel: 0,
            },
            Place::Follower {
                follower: 0,
                channel: 1,
            },
            Place::Master(0),
        ],
        vec![
            Place::Master(0),
            Place::Master(1),
            Place::Follower {
                follower: 1,
                channel: 0,
            },
            Place::Follower {
                follower: 1,
                channel: 1,
            },
        ],
        vec![
            spec("box", Direction::Input, 2),
            spec("box", Direction::Output, 2),
        ],
    );
    let master = DeviceStatus {
        id: "desk".into(),
        master: true,
        present: true,
        inputs: vec![2],
        outputs: vec![0, 1],
        ..DeviceStatus::default()
    };
    let d = followers.describe(
        master,
        &[
            Place::Follower {
                follower: 0,
                channel: 0,
            },
            Place::Follower {
                follower: 0,
                channel: 1,
            },
            Place::Master(0),
        ],
        &[
            Place::Master(0),
            Place::Master(1),
            Place::Follower {
                follower: 1,
                channel: 0,
            },
            Place::Follower {
                follower: 1,
                channel: 1,
            },
        ],
        |_| false,
    );
    assert_eq!(d.len(), 2, "one device, both ways");
    assert_eq!(d[1].inputs, vec![0, 1]);
    assert_eq!(d[1].outputs, vec![2, 3]);
    assert_eq!(d[1].sample_rate, 44_100);
    assert!(d[1].input_latency > 0, "its ring and resampler count");
    let i = StreamInfo {
        input_latency: 7,
        ..info(48_000, 256)
    };
    assert_eq!(crate::channel_input_latency(&d, &i, 2), 0, "the master's");
    assert_eq!(crate::channel_input_latency(&d, &i, 9), 7, "the stream's");
    assert_eq!(
        crate::channel_input_latency(&d, &i, 1),
        d[1].input_latency,
        "the follower's"
    );
}

#[test]
fn a_master_whose_rate_changes_takes_its_followers_along() {
    let frames = Arc::new(std::sync::atomic::AtomicU64::new(0));
    let spec = FollowerSpec {
        device: "net".into(),
        name: "Network".into(),
        direction: Direction::Input,
        channels: 1,
        sample_rate: 48_000,
        block: 48,
    };
    let (mut joined, mut followers, ends) = join(
        Box::new(Copy {
            frames: Arc::clone(&frames),
        }),
        &info(48_000, 256),
        vec![Place::Follower {
            follower: 0,
            channel: 0,
        }],
        vec![Place::Master(0)],
        vec![spec],
    );
    let Some(FollowerEnd::Input(mut end)) = ends.into_iter().next() else {
        panic!("an input end");
    };
    let w = TAU * 500.0 / 48_000.0;
    let mut io = crate::OwnedBuffers::new(0, 1, 256);
    let mut heard = Vec::new();
    // The follower delivers 48 frames every millisecond of its clock.
    let mut run = |rate: f64,
                   from: f64,
                   seconds: f64,
                   end: &mut FollowerInput,
                   phase: &mut u64,
                   next: &mut f64,
                   joined: &mut Joined,
                   heard: &mut Vec<f32>| {
        let mut t = from;
        while t < from + seconds {
            while *next <= t {
                let p0 = *phase;
                end.capture_with(48, (*next * 1e9) as u64, |i, _| {
                    (0.5 * ((p0 + i as u64) as f64 * w).sin()) as f32
                });
                *phase += 48;
                *next += 0.001;
            }
            joined.process_at(&mut io, (t * 1e9) as u64);
            heard.extend_from_slice(io.output_ref(0));
            t += 256.0 / rate;
        }
        t
    };
    let (mut phase, mut next) = (0u64, 0.0f64);
    let t = run(
        48_000.0,
        0.0,
        10.0,
        &mut end,
        &mut phase,
        &mut next,
        &mut joined,
        &mut heard,
    );
    // The audio system moves to 44.1 kHz while running: the master is
    // prepared anew, the followers attached again at the new rate.
    joined.prepare(&info(44_100, 256));
    followers.set_master(44_100, 256);
    let Some(FollowerEnd::Input(mut again)) = followers.attach(0, 48_000, 48) else {
        panic!("attached");
    };
    let start = heard.len();
    run(
        44_100.0,
        t,
        12.0,
        &mut again,
        &mut phase,
        &mut next,
        &mut joined,
        &mut heard,
    );
    followers.collect();
    let state = followers.state(0).unwrap();
    assert!(state.present, "{state:?}");
    assert!(state.drift_ppm.abs() < 5.0, "{state:?}");
    // The same 500 Hz, now in 44.1 kHz frames: clean once settled.
    let tail = &heard[start + 6 * 44_100..];
    assert!(worst_residual(tail, TAU * 500.0 / 44_100.0) < 2e-3);
    assert!(tail.iter().any(|v| v.abs() > 0.4), "it plays");
}
