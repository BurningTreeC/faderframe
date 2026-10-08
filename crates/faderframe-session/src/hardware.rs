//! Hardware inserts in the session: the round trip pinged (an impulse out
//! of the insert's send channel, timed back on its return channel) and
//! kept as the insert's Round Trip, which the engine compensates.

use crate::{NoticeLevel, Result, Session, SessionError};
use faderframe_core::{ParameterId, PluginInstanceId, TrackId};
use faderframe_plugin_host::devices::hardware_insert::id;
use faderframe_project::Command;
use std::time::Instant;

/// A ping on its way.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Ping {
    track: TrackId,
    plugin: PluginInstanceId,
    since: Instant,
}

impl Session {
    /// The device channels the project wants open (inputs, outputs): at
    /// least 8 in and 2 out, more for the master's bed, tracks routed to
    /// interface outputs, and hardware inserts' sends and returns.
    pub fn wanted_channels(&self) -> (u16, u16) {
        let p = &self.project;
        let (mut ins, mut outs) = (8usize, 2usize);
        for t in &p.tracks {
            let width = t.chain_layout().channel_count().max(1);
            if let faderframe_project::OutputRouting::Hardware { first_channel } = t.output {
                let dest = p.destination_layout(t).channel_count().max(1);
                outs = outs.max(first_channel as usize + dest);
            }
            if t.kind == faderframe_project::TrackKind::Master {
                outs = outs.max(t.layout.channel_count());
            }
            for s in t.slots() {
                if s.plugin.id != faderframe_core::builtin::HARDWARE_INSERT {
                    continue;
                }
                // As saved (the default is the third channel).
                let at = |p: u32| {
                    s.parameters
                        .iter()
                        .find(|v| v.id == ParameterId(p))
                        .map_or(2.0, |v| v.value)
                        .round()
                        .max(0.0) as usize
                };
                outs = outs.max(at(id::SEND_CHANNEL) + width);
                ins = ins.max(at(id::RETURN_CHANNEL) + width);
            }
        }
        (ins.min(256) as u16, outs.min(256) as u16)
    }

    /// Ask for the device to be opened again when the project needs more
    /// channels than are open (once for each count: a device that cannot
    /// open more is not asked again), not while recording.
    pub(crate) fn check_channels(&mut self) {
        let Some(info) = self.stream_info() else {
            return;
        };
        let want = self.wanted_channels();
        let short = want.0 > info.input_channels || want.1 > info.output_channels;
        if !short || self.reopened_for == Some(want) || self.recording.is_some() {
            return;
        }
        self.reopened_for = Some(want);
        self.ui_requests.push(crate::UiRequest::ReopenAudio);
        self.notify(
            NoticeLevel::Info,
            format!(
                "Opening the audio device with {} outputs and {} inputs (a hardware insert's)",
                want.1, want.0
            ),
        );
    }

    /// Measure hardware insert `plugin`'s round trip (stopped, with audio
    /// running): the result becomes its Round Trip.
    pub fn ping_hardware_insert(&mut self, track: TrackId, plugin: PluginInstanceId) -> Result<()> {
        if self.stream_info().is_none() {
            return Err(SessionError::Other(
                "start the audio device to measure the round trip".into(),
            ));
        }
        if self.transport.playing {
            return Err(SessionError::Other(
                "stop playback to measure the round trip (the ping is a click)".into(),
            ));
        }
        let mut value = |p: u32| {
            self.plugin_parameter_value(plugin, ParameterId(p))
                .ok_or_else(|| SessionError::Other("no such hardware insert".into()))
        };
        let (send, ret) = (value(id::SEND_CHANNEL)?, value(id::RETURN_CHANNEL)?);
        if let Some(info) = self.stream_info()
            && (send as u16 >= info.output_channels || ret as u16 >= info.input_channels)
        {
            return Err(SessionError::Other(format!(
                "the audio device has {} outputs and {} inputs open: restart it (Audio → Device) to open the insert's",
                info.output_channels, info.input_channels
            )));
        }
        self.engine
            .ping(send.round().max(0.0) as u16, ret.round().max(0.0) as u16);
        self.hardware_ping = Some(Ping {
            track,
            plugin,
            since: Instant::now(),
        });
        self.revision += 1;
        Ok(())
    }

    /// Whether a ping is on its way.
    pub fn pinging(&self) -> bool {
        self.hardware_ping.is_some()
    }

    /// The ping's answer, when it came (from the tick).
    pub(crate) fn tick_hardware_ping(&mut self) {
        let Some(ping) = self.hardware_ping else {
            return;
        };
        let answer = self.engine.ping_result();
        let answer = match answer {
            None if ping.since.elapsed().as_secs() >= 3 => Some(Err(())),
            a => a,
        };
        let Some(answer) = answer else {
            return;
        };
        self.hardware_ping = None;
        self.revision += 1;
        match answer {
            Ok(frames) => {
                let rate = self.engine.stream_sample_rate().max(1);
                if let Err(e) = self.edit(Command::Batch {
                    label: "Measure Round Trip".into(),
                    commands: vec![Command::SetPluginParameter {
                        track: ping.track,
                        plugin: ping.plugin,
                        parameter: ParameterId(id::ROUND_TRIP),
                        value: Some(f64::from(frames)),
                    }],
                }) {
                    self.notify(NoticeLevel::Error, format!("round trip: {e}"));
                    return;
                }
                self.notify(
                    NoticeLevel::Info,
                    format!(
                        "Round trip {frames} samples ({:.1} ms), compensated",
                        f64::from(frames) * 1000.0 / f64::from(rate)
                    ),
                );
            }
            Err(()) => self.notify(
                NoticeLevel::Warning,
                "The ping did not come back: check the cables, the send and return channels and the gear's level",
            ),
        }
    }
}
