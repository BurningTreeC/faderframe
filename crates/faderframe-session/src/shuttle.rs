//! Shuttle (J/K/L): the playhead runs at a speed other than playing's —
//! forward or in reverse, up to eight times — heard as a stream of scrub
//! snippets from where it is, the picture following it; and stepping one
//! picture frame at a time.
//!
//! Forward at 1× is plain playback. Any other speed moves a position of its
//! own (`Shuttle`), ticked with the session: every `SNIPPET` of wall time a
//! scrub snippet plays from there, and the shown playhead and the video
//! window's picture are the shuttle's position. Stopping the shuttle leaves
//! the playhead where it got to.

use crate::{Action, Result, Session, TransportAction};
use faderframe_transport::TransportCommand;
use std::time::{Duration, Instant};

/// Shuttle speeds, slowest first (each press of J or L doubles).
pub const SPEEDS: [f64; 4] = [1.0, 2.0, 4.0, 8.0];

/// How long each scrub snippet plays (and how often one starts).
const SNIPPET: Duration = Duration::from_millis(60);

/// What J, K and L (and the frame steps) ask.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ShuttleOp {
    /// L: play forward; again: faster; while reversing: forward at 1×.
    Forward,
    /// J: play in reverse; again: faster; while going forward: reverse
    /// at 1×.
    Reverse,
    /// K: stop where it is.
    Stop,
    /// A speed of its own (negative: reverse; 1: plain playback; 0: stop).
    Speed(f64),
    /// K held with J or L: one picture frame back (−1) or on (+1).
    Step(i32),
}

/// A shuttle in motion.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Shuttle {
    pub(crate) speed: f64,
    /// The position (samples) at `since`.
    from: i64,
    since: Instant,
    /// When the next snippet is due.
    next: Instant,
}

impl Shuttle {
    /// The position (samples) at `at`, never before the project start.
    fn position_at(&self, at: Instant, rate: u32) -> i64 {
        let secs = if at >= self.since {
            at.duration_since(self.since).as_secs_f64()
        } else {
            -self.since.duration_since(at).as_secs_f64()
        };
        (self.from + (self.speed * secs * rate as f64).round() as i64).max(0)
    }
}

impl Session {
    /// The shuttle's speed (`None`: not shuttling; plain playback is not
    /// shuttling either).
    pub fn shuttle_speed(&self) -> Option<f64> {
        self.shuttle.map(|s| s.speed)
    }

    /// The shuttle's position `ahead_ns` from now (for the picture).
    pub(crate) fn shuttle_position(&self, ahead_ns: i64) -> Option<i64> {
        let s = self.shuttle.as_ref()?;
        let at = if ahead_ns >= 0 {
            Instant::now() + Duration::from_nanos(ahead_ns as u64)
        } else {
            Instant::now()
        };
        Some(s.position_at(at, self.project.sample_rate))
    }

    pub(crate) fn shuttle_op(&mut self, op: ShuttleOp) -> Result<()> {
        let now_speed = match self.shuttle {
            Some(s) => s.speed,
            None if self.transport.playing => 1.0,
            None => 0.0,
        };
        let speed = match op {
            ShuttleOp::Forward if now_speed > 0.0 => next_speed(now_speed),
            ShuttleOp::Forward => 1.0,
            ShuttleOp::Reverse if now_speed < 0.0 => -next_speed(-now_speed),
            ShuttleOp::Reverse => -1.0,
            ShuttleOp::Stop => 0.0,
            ShuttleOp::Speed(v) => v.clamp(-SPEEDS[3], SPEEDS[3]),
            ShuttleOp::Step(n) => {
                self.stop_shuttle()?;
                return self.step_frames(n);
            }
        };
        self.set_shuttle_speed(speed)
    }

    fn set_shuttle_speed(&mut self, speed: f64) -> Result<()> {
        // Recording is never shuttled.
        if self.recording.is_some() {
            return Ok(());
        }
        let rate = self.project.sample_rate;
        let now = Instant::now();
        let here = match self.shuttle {
            Some(s) => s.position_at(now, rate),
            None => self.transport.position,
        };
        if (speed - 1.0).abs() < 1e-9 {
            // Plain playback from here.
            self.shuttle = None;
            if !self.transport.playing {
                self.engine.transport(TransportCommand::Locate(here))?;
                self.show_position(here);
                self.dispatch(Action::Transport(TransportAction::Play))?;
            }
            self.revision += 1;
            return Ok(());
        }
        if self.transport.playing && self.shuttle.is_none() {
            // Playback stops where it is; the shuttle carries on.
            self.engine.transport(TransportCommand::Stop)?;
            self.automation_stop_sent();
        }
        if speed == 0.0 {
            return self.stop_shuttle();
        }
        self.shuttle = Some(Shuttle {
            speed,
            from: here,
            since: now,
            next: now,
        });
        self.tick_shuttle(false);
        self.revision += 1;
        Ok(())
    }

    /// End the shuttle where it got to.
    fn stop_shuttle(&mut self) -> Result<()> {
        let Some(s) = self.shuttle.take() else {
            if self.transport.playing {
                self.engine.transport(TransportCommand::Stop)?;
                self.automation_stop_sent();
            }
            return Ok(());
        };
        let at = s.position_at(Instant::now(), self.project.sample_rate);
        self.engine.transport(TransportCommand::Locate(at))?;
        self.show_position(at);
        self.revision += 1;
        Ok(())
    }

    /// The transport was asked something while shuttling: stopping (or
    /// toggling) ends the shuttle where it is — and is all that happens;
    /// anything else ends it and then happens. Whether it was handled.
    pub(crate) fn shuttle_transport(&mut self, action: &TransportAction) -> Result<bool> {
        if self.shuttle.is_none() {
            return Ok(false);
        }
        match action {
            TransportAction::Stop | TransportAction::TogglePlay => {
                self.stop_shuttle()?;
                Ok(true)
            }
            TransportAction::Play => {
                self.stop_shuttle()?;
                Ok(false)
            }
            TransportAction::Locate(_) | TransportAction::ReturnToStart => {
                self.shuttle = None;
                Ok(false)
            }
            _ => Ok(false),
        }
    }

    /// Play the next snippet when due, and show where the shuttle is.
    /// `started_playing`: playback began since the last tick (it takes
    /// over).
    pub(crate) fn tick_shuttle(&mut self, started_playing: bool) {
        let Some(mut s) = self.shuttle else {
            return;
        };
        if started_playing {
            self.shuttle = None;
            return;
        }
        let rate = self.project.sample_rate;
        let now = Instant::now();
        let at = s.position_at(now, rate);
        if at == 0 && s.speed < 0.0 {
            // Back at the start: it stops there.
            let _ = self.stop_shuttle();
            return;
        }
        if now >= s.next {
            let frames = (rate as f64 * SNIPPET.as_secs_f64()) as u32;
            let _ = self.engine.transport(TransportCommand::Scrub {
                position: at,
                frames,
            });
            s.next = now + SNIPPET;
            self.shuttle = Some(s);
        }
        self.transport.position = at;
        self.revision += 1;
    }

    /// Move the playhead `n` picture frames (the shown video's own frames
    /// where there is one, else the project timecode's).
    fn step_frames(&mut self, n: i32) -> Result<()> {
        let rate = self.project.sample_rate;
        let pos = self.transport.position;
        let to = self.video_frame_step(pos, n).unwrap_or_else(|| {
            let tc = self.project.timecode.unwrap_or_default().rate;
            let frame = tc.frame_at(pos as f64 / rate.max(1) as f64 + 1e-9);
            let target = (frame + n as i64).max(0);
            (tc.seconds_of(target) * rate as f64).round() as i64
        });
        let to = to.max(0);
        self.engine.transport(TransportCommand::Locate(to))?;
        self.show_position(to);
        self.revision += 1;
        Ok(())
    }
}

/// The next speed up from `v` (staying at the fastest).
fn next_speed(v: f64) -> f64 {
    SPEEDS
        .iter()
        .copied()
        .find(|&s| s > v + 1e-9)
        .unwrap_or(SPEEDS[SPEEDS.len() - 1])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn speeds_double_up_to_eight() {
        assert_eq!(next_speed(1.0), 2.0);
        assert_eq!(next_speed(2.0), 4.0);
        assert_eq!(next_speed(8.0), 8.0);
        assert_eq!(next_speed(0.5), 1.0);
    }
}
