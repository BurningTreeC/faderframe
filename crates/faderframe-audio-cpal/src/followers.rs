//! Devices that follow the master's clock (`faderframe_audio::follow`):
//! their cpal streams, opened, watched and opened again by the stream's
//! control thread.
//!
//! Each follower is one direction of one device: a capture stream whose
//! callback pushes into its link's ring, or a playback stream whose
//! callback takes from it. A follower's stream that fails (unplugged, the
//! device invalidated) is dropped and its channels fall silent; the
//! control thread looks for the device again every [`RETRY`] and attaches
//! a new link when it opens — the master's stream never stops.

use crate::{device_id, device_name, frames_in, pick_config, stream_err};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{FromSample, Sample, SampleFormat, SizedSample};
use faderframe_audio::follow::{Direction, FollowerEnd, FollowerInput, FollowerOutput, Followers};
use faderframe_audio::{AudioError, MAX_BUFFER_SIZE};
use faderframe_realtime::priority::{Helper, follow_audio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// How often a follower that went away is looked for.
pub const RETRY: Duration = Duration::from_secs(1);

/// What a follower's stream is opened with.
#[derive(Clone, Debug)]
pub struct Opening {
    pub channels: u16,
    pub sample_rate: u32,
    pub format: SampleFormat,
    /// Frames a callback is expected to bring (a hint until it runs).
    pub block: u32,
}

/// Choose how to open `device` for `direction`: at `rate` where it runs
/// at it (else at its own), with as many channels as it has.
pub fn opening(
    device: &cpal::Device,
    direction: Direction,
    rate: u32,
    master_block: u32,
    master_rate: u32,
) -> Result<Opening, AudioError> {
    let (ranges, default) = match direction {
        Direction::Input => (
            device
                .supported_input_configs()
                .map_err(stream_err)?
                .collect::<Vec<_>>(),
            device.default_input_config().map_err(stream_err)?,
        ),
        Direction::Output => (
            device
                .supported_output_configs()
                .map_err(stream_err)?
                .collect::<Vec<_>>(),
            device.default_output_config().map_err(stream_err)?,
        ),
    };
    let most = ranges.iter().map(|c| c.channels()).max().unwrap_or(0);
    let (sample_rate, range) = match pick_config(&ranges, rate, most) {
        Some(r) => (rate, Some(r)),
        None => (
            default.sample_rate(),
            pick_config(&ranges, default.sample_rate(), most),
        ),
    };
    let (channels, format) = range.map_or((default.channels(), default.sample_format()), |r| {
        (r.channels(), r.sample_format())
    });
    let block = (f64::from(master_block) * f64::from(sample_rate) / f64::from(master_rate.max(1)))
        .ceil() as u32;
    Ok(Opening {
        channels,
        sample_rate,
        format,
        block: block.clamp(16, MAX_BUFFER_SIZE),
    })
}

fn failing(failed: &Arc<AtomicBool>) -> impl FnMut(cpal::Error) + Send + 'static {
    let failed = Arc::clone(failed);
    move |e: cpal::Error| match e.kind() {
        // Its link counts the gaps; the stream goes on.
        cpal::ErrorKind::Xrun
        | cpal::ErrorKind::DeviceChanged
        | cpal::ErrorKind::RealtimeDenied => {}
        _ => failed.store(true, Ordering::Relaxed),
    }
}

/// Once on a follower's callback thread: its stack mapped.
fn first_callback(started: &mut bool) {
    if !*started {
        *started = true;
        faderframe_realtime::memory::prefault_stack(128 * 1024);
    }
    // A step below the master's audio thread (followed when it changes).
    follow_audio(Helper::Near);
}

fn input_stream<T>(
    device: &cpal::Device,
    config: &cpal::StreamConfig,
    mut end: FollowerInput,
    failed: &Arc<AtomicBool>,
) -> Result<cpal::Stream, AudioError>
where
    T: SizedSample + Send + 'static,
    f32: FromSample<T>,
{
    let rate = config.sample_rate;
    let channels = usize::from(config.channels.max(1));
    let mut started = false;
    device
        .build_input_stream::<T, _, _>(
            *config,
            move |data: &[T], info: &cpal::InputCallbackInfo| {
                first_callback(&mut started);
                let ts = info.timestamp();
                let own = (data.len() / channels) as u32;
                let since = frames_in(ts.callback.duration_since(ts.capture), rate);
                end.set_device_latency(since.saturating_sub(own));
                end.capture(data, faderframe_audio::follow::now_ns(), f32::from_sample);
            },
            failing(failed),
            None,
        )
        .map_err(stream_err)
}

fn output_stream<T>(
    device: &cpal::Device,
    config: &cpal::StreamConfig,
    mut end: FollowerOutput,
    failed: &Arc<AtomicBool>,
) -> Result<cpal::Stream, AudioError>
where
    T: SizedSample + FromSample<f32> + Send + 'static,
{
    let rate = config.sample_rate;
    let channels = usize::from(config.channels.max(1));
    let mut started = false;
    device
        .build_output_stream::<T, _, _>(
            *config,
            move |data: &mut [T], info: &cpal::OutputCallbackInfo| {
                first_callback(&mut started);
                let ts = info.timestamp();
                let own = (data.len() / channels) as u32;
                let ahead = frames_in(ts.playback.duration_since(ts.callback), rate);
                end.set_device_latency(ahead.saturating_sub(own));
                end.play(data, faderframe_audio::follow::now_ns(), T::from_sample);
            },
            failing(failed),
            None,
        )
        .map_err(stream_err)
}

/// Open and start a follower's stream on `device` with its link's `end`.
pub fn start(
    device: &cpal::Device,
    open: &Opening,
    end: FollowerEnd,
    failed: &Arc<AtomicBool>,
) -> Result<cpal::Stream, AudioError> {
    let config = cpal::StreamConfig {
        channels: open.channels,
        sample_rate: open.sample_rate,
        buffer_size: cpal::BufferSize::Default,
    };
    let stream = match end {
        FollowerEnd::Input(e) => match open.format {
            SampleFormat::F32 => input_stream::<f32>(device, &config, e, failed),
            SampleFormat::I32 => input_stream::<i32>(device, &config, e, failed),
            _ => input_stream::<i16>(device, &config, e, failed),
        },
        FollowerEnd::Output(e) => match open.format {
            SampleFormat::F32 => output_stream::<f32>(device, &config, e, failed),
            SampleFormat::I32 => output_stream::<i32>(device, &config, e, failed),
            _ => output_stream::<i16>(device, &config, e, failed),
        },
    }?;
    stream.play().map_err(stream_err)?;
    Ok(stream)
}

/// One follower's stream, kept by the control thread.
pub struct FollowerStream {
    pub index: usize,
    pub device: String,
    pub direction: Direction,
    pub stream: Option<cpal::Stream>,
    /// Set by its error callback: the stream is dropped and opened again.
    pub failed: Arc<AtomicBool>,
    /// Shared with the stream handle: not there now.
    pub away: Arc<AtomicBool>,
    pub last_try: Instant,
}

/// The control thread's side of the followers: watch, drop failed
/// streams, look for their devices again and attach new links.
pub struct Watcher {
    pub host: cpal::Host,
    pub streams: Vec<FollowerStream>,
    pub followers: Arc<Mutex<Followers>>,
    pub master_rate: u32,
    pub master_block: u32,
}

impl Watcher {
    pub fn tick(&mut self) {
        let now = Instant::now();
        let mut followers = self.followers.lock().unwrap_or_else(|e| e.into_inner());
        followers.collect();
        for f in &mut self.streams {
            if f.stream.is_some() && f.failed.load(Ordering::Relaxed) {
                // Unplugged (or its device otherwise gone): its channels
                // fall silent, the master plays on.
                tracing::warn!(
                    "{}: the device went away; it joins again when it is back",
                    f.device
                );
                f.stream = None;
                f.away.store(true, Ordering::Relaxed);
                f.last_try = now;
            }
            if f.stream.is_some() || now.duration_since(f.last_try) < RETRY {
                continue;
            }
            f.last_try = now;
            let found = match f.direction {
                Direction::Input => self.host.input_devices(),
                Direction::Output => self.host.output_devices(),
            }
            .ok()
            .and_then(|mut all| {
                all.find(|d| device_id(d) == f.device || device_name(d) == f.device)
            });
            let Some(device) = found else {
                continue;
            };
            let Ok(open) = opening(
                &device,
                f.direction,
                self.master_rate,
                self.master_block,
                self.master_rate,
            ) else {
                continue;
            };
            let Some(end) = followers.attach(f.index, open.sample_rate, open.block) else {
                continue;
            };
            f.failed.store(false, Ordering::Relaxed);
            match start(&device, &open, end, &f.failed) {
                Ok(s) => {
                    tracing::info!("{}: back, following again", f.device);
                    f.stream = Some(s);
                    f.away.store(false, Ordering::Relaxed);
                }
                Err(e) => tracing::info!("{}: not yet ({e})", f.device),
            }
        }
    }
}
