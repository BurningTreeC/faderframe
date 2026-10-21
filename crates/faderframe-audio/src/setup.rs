//! What a backend lets the user set up, how a running stream is connected,
//! and a request (the Preferences) checked against the devices a backend
//! reports before a stream is opened.

use crate::{DeviceInfo, MAX_BUFFER_SIZE, SAMPLE_RATE_RANGE, format_sample_rate};

/// Who decides a stream's sample rate.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum RateControl {
    /// The server runs at its own rate; a request is ignored (JACK).
    Server,
    /// The stream asks the server, which switches when nothing else holds
    /// the graph and its allowed rates include it (PipeWire).
    Requested,
    /// The device opens at the rate asked for, if it supports it (system
    /// APIs, the dummy device).
    #[default]
    Device,
}

/// What a backend can do, so that Preferences offers only that.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Capabilities {
    pub rate: RateControl,
    /// A buffer size asked of the running stream takes effect without
    /// reopening it.
    pub live_buffer_size: bool,
    /// A sample rate asked of the running stream is passed on without
    /// reopening it (the server may still keep its own).
    pub live_sample_rate: bool,
    /// How many output devices one stream can play on at once (0: the
    /// backend's own default only, nothing to choose).
    pub output_devices: usize,
    /// How many input devices one stream can record from at once (0:
    /// capture comes from the output device itself, e.g. an ASIO driver).
    pub input_devices: usize,
    /// Devices of different clocks are kept in step by the backend
    /// (PipeWire resamples the devices that follow another's clock).
    /// Without it, an input device other than the output device drifts.
    pub clocked_by_server: bool,
    /// The stream reconnects devices that come and go by itself (a server
    /// graph): a missing device is not an error, the stream plays on the
    /// default meanwhile and goes back when it returns.
    pub follows_devices: bool,
    /// Any number of channels can be opened (server ports); otherwise the
    /// device's channel counts limit them.
    pub free_channel_counts: bool,
    /// The rate and buffer size can be forced on the whole graph while the
    /// stream runs ([`crate::StreamConfig::force_format`]).
    pub force_format: bool,
}

impl Default for Capabilities {
    fn default() -> Self {
        Self {
            rate: RateControl::Device,
            live_buffer_size: false,
            live_sample_rate: false,
            output_devices: 1,
            input_devices: 1,
            clocked_by_server: false,
            follows_devices: false,
            free_channel_counts: false,
            force_format: false,
        }
    }
}

/// How a running stream is connected, as far as the backend knows.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct StreamRouting {
    /// For each input channel of the stream, what feeds it ("SSL 2+ ·
    /// capture_AUX0"), `None` where nothing is connected.
    pub inputs: Vec<Option<String>>,
    /// For each output channel, where it goes.
    pub outputs: Vec<Option<String>>,
    /// Devices asked for that are not there now (their channels play on
    /// the default device meanwhile, or are not connected).
    pub missing: Vec<String>,
    /// Display names of the devices in use, outputs then inputs (each once).
    pub devices: Vec<String>,
}

impl StreamRouting {
    /// The device channel an input of the stream comes from, if known.
    pub fn input(&self, channel: usize) -> Option<&str> {
        self.inputs.get(channel).and_then(|c| c.as_deref())
    }

    pub fn output(&self, channel: usize) -> Option<&str> {
        self.outputs.get(channel).and_then(|c| c.as_deref())
    }
}

/// A stream set-up as asked for (the Preferences): `None`/empty means the
/// backend's default.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Request {
    pub sample_rate: Option<u32>,
    pub buffer_size: Option<u32>,
    /// Device ids (or names, from older settings) to play on.
    pub output_devices: Vec<String>,
    /// Device ids to record from (empty: the output device where it can
    /// record, else the default input).
    pub input_devices: Vec<String>,
}

/// A request checked against a backend's devices: what to open, and what
/// was changed on the way (one sentence each, for a notice).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Resolution {
    pub request: Request,
    pub notes: Vec<String>,
    /// Asked-for devices that are not connected now.
    pub missing: Vec<String>,
}

/// Find `wanted` among `devices` by id, else by name (settings saved by
/// name, or ids that changed while the name stayed).
pub fn find_device<'a>(devices: &'a [DeviceInfo], wanted: &str) -> Option<&'a DeviceInfo> {
    devices
        .iter()
        .find(|d| d.id == wanted)
        .or_else(|| devices.iter().find(|d| d.name == wanted))
}

/// Check `request` against what a backend can do and the devices it
/// reports: missing devices fall back to the default (unless the backend
/// follows devices itself), a sample rate the device cannot run at falls
/// back to its own, a buffer size outside its range is moved inside it.
pub fn resolve(caps: &Capabilities, devices: &[DeviceInfo], request: &Request) -> Resolution {
    let mut out = Resolution {
        request: request.clone(),
        ..Resolution::default()
    };
    let keep = |list: &[String], max: usize, what: &str, out: &mut Resolution| {
        let mut kept = Vec::new();
        for id in list {
            if kept.len() >= max {
                out.notes.push(format!(
                    "This audio system takes {max} {what} device{} at once: {id} is left out",
                    if max == 1 { "" } else { "s" }
                ));
                continue;
            }
            match find_device(devices, id) {
                Some(d) => kept.push(d.id.clone()),
                None if caps.follows_devices => {
                    out.missing.push(id.clone());
                    kept.push(id.clone());
                    out.notes.push(format!(
                        "The {what} device {id} is not connected: its channels connect when it \
                         is back"
                    ));
                }
                None => {
                    out.missing.push(id.clone());
                    out.notes.push(format!(
                        "The {what} device {id} is not connected: using the default until it is back"
                    ));
                }
            }
        }
        kept
    };
    out.request.output_devices = keep(
        &request.output_devices,
        caps.output_devices,
        "output",
        &mut out,
    );
    out.request.input_devices = keep(
        &request.input_devices,
        caps.input_devices,
        "input",
        &mut out,
    );
    // The device whose limits apply: the first output device chosen, else
    // the default one.
    let device = out
        .request
        .output_devices
        .first()
        .and_then(|id| find_device(devices, id))
        .or_else(|| {
            devices
                .iter()
                .find(|d| d.is_default && d.output_channels > 0)
        })
        .or_else(|| devices.iter().find(|d| d.output_channels > 0));
    if let Some(rate) = request.sample_rate {
        if !SAMPLE_RATE_RANGE.contains(&rate) {
            out.request.sample_rate = None;
            out.notes.push(format!(
                "{} is outside {}–{}: using the device's rate",
                format_sample_rate(rate),
                format_sample_rate(*SAMPLE_RATE_RANGE.start()),
                format_sample_rate(*SAMPLE_RATE_RANGE.end())
            ));
        } else if caps.rate == RateControl::Server {
            if let Some(now) = device.and_then(|d| d.current_sample_rate)
                && now != rate
            {
                out.notes.push(format!(
                    "The server runs at {}: it owns the rate, {} is not used",
                    format_sample_rate(now),
                    format_sample_rate(rate)
                ));
            }
            out.request.sample_rate = None;
        } else if caps.rate == RateControl::Device
            && let Some(d) = device
            && !d.sample_rates.is_empty()
            && !d.sample_rates.contains(&rate)
        {
            let fallback = d
                .current_sample_rate
                .filter(|r| d.sample_rates.contains(r))
                .or_else(|| {
                    d.sample_rates
                        .iter()
                        .copied()
                        .min_by_key(|r| r.abs_diff(rate))
                });
            out.request.sample_rate = fallback;
            out.notes.push(format!(
                "{} cannot run at {}: opened at {}",
                d.name,
                format_sample_rate(rate),
                fallback.map_or_else(|| "its default rate".into(), format_sample_rate)
            ));
        }
    }
    if let Some(frames) = request.buffer_size {
        let (lo, hi) = device
            .and_then(|d| d.buffer_sizes)
            .unwrap_or((1, MAX_BUFFER_SIZE));
        let (lo, hi) = (lo.max(1), hi.min(MAX_BUFFER_SIZE).max(lo.max(1)));
        if !(lo..=hi).contains(&frames) {
            let clamped = frames.clamp(lo, hi);
            out.request.buffer_size = Some(clamped);
            out.notes.push(format!(
                "{} takes buffers of {lo}–{hi} frames: {frames} became {clamped}",
                device.map_or("The device", |d| d.name.as_str())
            ));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn device(id: &str, rates: &[u32], buffers: Option<(u32, u32)>, default: bool) -> DeviceInfo {
        DeviceInfo {
            id: id.into(),
            name: format!("{id} name"),
            input_channels: 2,
            output_channels: 2,
            sample_rates: rates.to_vec(),
            current_sample_rate: rates.first().copied(),
            buffer_sizes: buffers,
            is_default: default,
            ..DeviceInfo::default()
        }
    }

    #[test]
    fn a_device_that_is_not_there_falls_back_to_the_default() {
        let devices = [device("a", &[], None, true)];
        let r = resolve(
            &Capabilities::default(),
            &devices,
            &Request {
                output_devices: vec!["gone".into()],
                input_devices: vec!["a name".into()],
                ..Request::default()
            },
        );
        assert!(r.request.output_devices.is_empty(), "default instead");
        assert_eq!(
            r.request.input_devices,
            vec!["a".to_string()],
            "found by name"
        );
        assert_eq!(r.missing, vec!["gone".to_string()]);
        assert_eq!(r.notes.len(), 1, "{:?}", r.notes);
    }

    #[test]
    fn a_backend_following_devices_keeps_the_missing_one() {
        let caps = Capabilities {
            follows_devices: true,
            output_devices: 4,
            ..Capabilities::default()
        };
        let r = resolve(
            &caps,
            &[],
            &Request {
                output_devices: vec!["gone".into()],
                ..Request::default()
            },
        );
        assert_eq!(r.request.output_devices, vec!["gone".to_string()]);
        assert_eq!(r.missing, vec!["gone".to_string()]);
        assert_eq!(r.notes.len(), 1, "said, not dropped");
    }

    #[test]
    fn an_unsupported_rate_and_buffer_are_corrected_with_a_note() {
        let devices = [device("a", &[44_100, 48_000], Some((64, 1024)), true)];
        let r = resolve(
            &Capabilities::default(),
            &devices,
            &Request {
                sample_rate: Some(96_000),
                buffer_size: Some(16),
                ..Request::default()
            },
        );
        assert_eq!(r.request.sample_rate, Some(44_100), "the device's own rate");
        assert_eq!(r.request.buffer_size, Some(64));
        assert_eq!(r.notes.len(), 2, "{:?}", r.notes);
        // A supported request passes unchanged.
        let ok = Request {
            sample_rate: Some(48_000),
            buffer_size: Some(256),
            ..Request::default()
        };
        let r = resolve(&Capabilities::default(), &devices, &ok);
        assert_eq!(r.request, ok);
        assert!(r.notes.is_empty());
    }

    #[test]
    fn a_server_owned_rate_is_dropped_and_explained() {
        let caps = Capabilities {
            rate: RateControl::Server,
            ..Capabilities::default()
        };
        let mut d = device("jack", &[], None, true);
        d.current_sample_rate = Some(48_000);
        let r = resolve(
            &caps,
            &[d],
            &Request {
                sample_rate: Some(96_000),
                ..Request::default()
            },
        );
        assert_eq!(r.request.sample_rate, None);
        assert_eq!(r.notes.len(), 1);
    }

    #[test]
    fn more_devices_than_the_backend_takes_are_left_out() {
        let devices = [device("a", &[], None, true), device("b", &[], None, false)];
        let r = resolve(
            &Capabilities::default(),
            &devices,
            &Request {
                output_devices: vec!["a".into(), "b".into()],
                ..Request::default()
            },
        );
        assert_eq!(r.request.output_devices, vec!["a".to_string()]);
        assert_eq!(r.notes.len(), 1);
    }
}
