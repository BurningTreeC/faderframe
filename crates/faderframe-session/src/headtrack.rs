//! The listener's head on headphones: turned by a head tracker over OSC,
//! or by hand.
//!
//! Binaural listening turns the scene the other way when the head turns
//! (`faderframe_binaural::Renderer::turn`: beds, objects and ambisonic
//! fields; speakers never turn, renders never hear it). A head tracker
//! sends its orientation over OSC (the messages are in
//! `faderframe_control::headtrack`: IEM SceneRotator's `/SceneRotator/ypr`
//! and `/SceneRotator/quaternions`, SPARTA's `/ypr` and `/quaternion`, the
//! single angles and components) to a UDP port; when an OSC control
//! surface listens on that port already, its socket takes them too.
//! Recentring makes the way the head points now straight ahead (the
//! tracker's reading is taken relative to it). Without a tracker — or
//! before its first message — the head points where it was set by hand
//! ([`HeadOp::Turn`]). The settings ([`HeadTrackingSettings`]) are the
//! machine's (the shell keeps them in its preferences); the orientation
//! is per session and never saved.

use crate::{Action, Result, Session};
use faderframe_control::headtrack::{HeadInput, HeadReading, HeadTracker};
use faderframe_core::ambisonics::Orientation;
use serde::{Deserialize, Serialize};
use std::net::UdpSocket;
use std::time::{Duration, Instant};

/// Packets read per tick at most.
const MAX_PACKETS: usize = 256;
/// A tracker silent this long counts as gone (the status says so; the
/// head stays where it was last).
pub const TRACKER_QUIET: Duration = Duration::from_secs(2);

/// How head tracking is set up (the machine's: kept in the preferences).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct HeadTrackingSettings {
    /// Listen for a tracker.
    pub enabled: bool,
    /// The UDP port it sends to.
    pub port: u16,
    /// How long the head glides to a new orientation (ms).
    pub smoothing_ms: f32,
    /// Axes the tracker counts the other way (its angle or quaternion
    /// component negated).
    pub invert_yaw: bool,
    pub invert_pitch: bool,
    pub invert_roll: bool,
    /// Its angles are in radians (else degrees).
    pub radians: bool,
}

impl Default for HeadTrackingSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            port: DEFAULT_PORT,
            smoothing_ms: faderframe_engine::nodes::DEFAULT_HEAD_SMOOTHING * 1000.0,
            invert_yaw: false,
            invert_pitch: false,
            invert_roll: false,
            radians: false,
        }
    }
}

/// The port a tracker is listened for unless set otherwise (beside the
/// OSC surface's 8000).
pub const DEFAULT_PORT: u16 = 8001;

impl HeadTrackingSettings {
    fn input(&self) -> HeadInput {
        HeadInput {
            invert_yaw: self.invert_yaw,
            invert_pitch: self.invert_pitch,
            invert_roll: self.invert_roll,
            angles_in_radians: self.radians,
        }
    }
}

/// What can be done with the head.
#[derive(Clone, Debug, PartialEq)]
pub enum HeadOp {
    /// Point the head by hand (degrees; see
    /// `faderframe_core::ambisonics::Orientation` for the axes).
    Turn { yaw: f32, pitch: f32, roll: f32 },
    /// Straight ahead again (by hand; a tracker: recentred).
    Straight,
    /// The way the tracker points the head now is straight ahead.
    Recentre,
    /// Set head tracking up.
    Tracking(HeadTrackingSettings),
}

/// How head tracking is doing (for menus and preferences).
#[derive(Clone, Debug, PartialEq)]
pub struct HeadStatus {
    /// The head as the listening hears it (yaw, pitch, roll in degrees).
    pub ypr: (f32, f32, f32),
    /// Listening for a tracker on this port (its own socket or an OSC
    /// surface's).
    pub listening: Option<u16>,
    /// An OSC control surface's socket takes the messages.
    pub shared: bool,
    /// When the tracker last spoke.
    pub last_heard: Option<Duration>,
    /// Messages taken so far.
    pub messages: u64,
    /// Why it cannot listen.
    pub error: Option<String>,
}

/// The session's head.
pub struct HeadState {
    settings: HeadTrackingSettings,
    socket: Option<UdpSocket>,
    /// An OSC surface listens on the port: it hands the packets over.
    shared: bool,
    error: Option<String>,
    tracker: HeadTracker,
    /// The head as set by hand.
    manual: Orientation,
    /// The tracker's last orientation and the one recentred to.
    tracked: Option<Orientation>,
    reference: Orientation,
    last: Option<Instant>,
    messages: u64,
    /// What the engine was last told.
    applied: Option<Orientation>,
}

impl Default for HeadState {
    fn default() -> Self {
        Self {
            settings: HeadTrackingSettings::default(),
            socket: None,
            shared: false,
            error: None,
            tracker: HeadTracker::default(),
            manual: Orientation::IDENTITY,
            tracked: None,
            reference: Orientation::IDENTITY,
            last: None,
            messages: 0,
            applied: None,
        }
    }
}

impl HeadState {
    /// The head as the listening should hear it.
    fn wanted(&self) -> Orientation {
        match self.tracked {
            Some(t) if self.settings.enabled => self.reference.inverse().then(t),
            _ => self.manual,
        }
    }

    /// Take one tracker reading.
    fn take(&mut self, reading: HeadReading) {
        let o = match reading {
            HeadReading::Angles { yaw, pitch, roll } => Orientation::from_ypr(yaw, pitch, roll),
            HeadReading::Quaternion { w, x, y, z } => Orientation::from_quaternion(w, x, y, z),
        };
        self.tracked = Some(o);
        self.last = Some(Instant::now());
        self.messages += 1;
    }

    /// Every head reading in an OSC packet (a surface's socket hands its
    /// packets over when it listens on the tracker's port).
    pub(crate) fn feed_packet(&mut self, packet: &[u8]) {
        if !self.settings.enabled {
            return;
        }
        for r in self.tracker.read_packet(packet) {
            self.take(r);
        }
    }

    /// Whether an OSC surface listening on `port` should hand its packets
    /// over.
    pub(crate) fn wants_port(&self, port: u16) -> bool {
        self.settings.enabled && self.shared && self.settings.port == port
    }
}

impl Session {
    pub(crate) fn head_op(&mut self, op: HeadOp) -> Result<()> {
        match op {
            HeadOp::Turn { yaw, pitch, roll } => {
                self.set_head_orientation(Orientation::from_ypr(yaw, pitch, roll));
            }
            HeadOp::Straight => {
                self.head.manual = Orientation::IDENTITY;
                if let Some(t) = self.head.tracked {
                    self.head.reference = t;
                }
                self.apply_head();
            }
            HeadOp::Recentre => self.recentre_head(),
            HeadOp::Tracking(settings) => self.set_head_tracking(settings),
        }
        self.revision += 1;
        Ok(())
    }

    /// Point the listener's head by hand (heard when no tracker turns it).
    pub fn set_head_orientation(&mut self, head: Orientation) {
        self.head.manual = head;
        self.apply_head();
    }

    /// Point the head at `head` whatever turns it: by hand, or — while a
    /// tracker turns it — by taking the tracker's reading relative to
    /// where it would then point (a 360° picture dragged round under a
    /// tracked head).
    pub fn point_head(&mut self, head: Orientation) {
        match self.head.tracked {
            Some(t) if self.head.settings.enabled => {
                self.head.reference = t.then(head.inverse());
            }
            _ => self.head.manual = head,
        }
        self.apply_head();
    }

    /// The way the tracker points the head now becomes straight ahead
    /// (without a tracker: the head set by hand is straightened).
    pub fn recentre_head(&mut self) {
        match self.head.tracked {
            Some(t) if self.head.settings.enabled => self.head.reference = t,
            _ => self.head.manual = Orientation::IDENTITY,
        }
        self.apply_head();
    }

    /// The head as the listening hears it (before the renderer's glide).
    pub fn head_orientation(&self) -> Orientation {
        self.head.wanted()
    }

    pub fn head_tracking(&self) -> &HeadTrackingSettings {
        &self.head.settings
    }

    /// Set head tracking up: listen on its port (or through the OSC
    /// surface that listens there), the glide and the axes. A port that
    /// cannot be opened is reported in [`Self::head_status`] (and a
    /// notice); the rest still applies.
    pub fn set_head_tracking(&mut self, settings: HeadTrackingSettings) {
        let reopen = settings.enabled != self.head.settings.enabled
            || settings.port != self.head.settings.port
            || (settings.enabled && self.head.socket.is_none() && !self.head.shared);
        self.head.tracker.set_input(settings.input());
        self.engine
            .set_head_smoothing(settings.smoothing_ms.max(0.0) / 1000.0);
        if !settings.enabled {
            self.head.tracked = None;
            self.head.last = None;
        }
        self.head.settings = settings;
        if reopen {
            self.open_head_socket();
        }
        self.apply_head();
    }

    /// (Re)open the tracker's socket: none when tracking is off or an OSC
    /// surface listens on the port (it hands the packets over).
    pub(crate) fn open_head_socket(&mut self) {
        self.head.socket = None;
        self.head.shared = false;
        self.head.error = None;
        if !self.head.settings.enabled {
            return;
        }
        let port = self.head.settings.port;
        if self.osc_surface_ports().contains(&port) {
            self.head.shared = true;
            return;
        }
        let opened = UdpSocket::bind(("0.0.0.0", port)).and_then(|s| {
            s.set_nonblocking(true)?;
            Ok(s)
        });
        match opened {
            Ok(s) => self.head.socket = Some(s),
            Err(e) => {
                let msg = format!("head tracking: UDP port {port}: {e}");
                tracing::warn!("{msg}");
                self.notify(crate::NoticeLevel::Warning, msg.clone());
                self.head.error = Some(msg);
            }
        }
    }

    /// Before the control surfaces open their sockets: the tracker's own
    /// socket lets go of a port one of them wants.
    pub(crate) fn release_head_port_for(&mut self, ports: &[u16]) {
        if self.head.socket.is_some() && ports.contains(&self.head.settings.port) {
            self.head.socket = None;
        }
    }

    pub fn head_status(&self) -> HeadStatus {
        let h = &self.head;
        let listening =
            (h.settings.enabled && (h.socket.is_some() || h.shared)).then_some(h.settings.port);
        HeadStatus {
            ypr: h.wanted().ypr(),
            listening,
            shared: h.shared,
            last_heard: h.last.map(|t| t.elapsed()),
            messages: h.messages,
            error: h.error.clone(),
        }
    }

    /// Every tick: what the tracker sent, then the head to the engine
    /// when it moved.
    pub(crate) fn tick_head(&mut self) {
        if let Some(socket) = self.head.socket.as_ref() {
            let mut buf = [0u8; 2048];
            let mut readings = Vec::new();
            for _ in 0..MAX_PACKETS {
                match socket.recv_from(&mut buf) {
                    Ok((n, _)) => readings.extend(self.head.tracker.read_packet(&buf[..n])),
                    Err(_) => break,
                }
            }
            for r in readings {
                self.head.take(r);
            }
        }
        self.apply_head();
    }

    /// The head to the engine, when it differs from what it was told (a
    /// 360° picture looking where the head points is shown again).
    fn apply_head(&mut self) {
        let wanted = self.head.wanted();
        if self.head.applied != Some(wanted) {
            self.engine.set_head_orientation(wanted);
            self.head.applied = Some(wanted);
            self.shown_changed();
        }
    }

    /// After a new engine: the head and its glide carried over.
    pub(crate) fn reapply_head(&mut self) {
        self.engine
            .set_head_smoothing(self.head.settings.smoothing_ms.max(0.0) / 1000.0);
        self.head.applied = None;
        self.apply_head();
    }

    /// The head-tracking action for menus: on or off as it is now.
    pub fn head_tracking_toggle(&self) -> Action {
        Action::Head(HeadOp::Tracking(HeadTrackingSettings {
            enabled: !self.head.settings.enabled,
            ..self.head.settings.clone()
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recentring_takes_the_tracker_relative_to_where_it_pointed() {
        let mut h = HeadState {
            settings: HeadTrackingSettings {
                enabled: true,
                ..HeadTrackingSettings::default()
            },
            ..HeadState::default()
        };
        h.take(HeadReading::Angles {
            yaw: 40.0,
            pitch: 0.0,
            roll: 0.0,
        });
        assert!((h.wanted().ypr().0 - 40.0).abs() < 1e-3);
        h.reference = h.tracked.unwrap();
        assert!(h.wanted().is_identity());
        h.take(HeadReading::Angles {
            yaw: 70.0,
            pitch: 0.0,
            roll: 0.0,
        });
        assert!((h.wanted().ypr().0 - 30.0).abs() < 1e-3);
        // Off: the head set by hand.
        h.settings.enabled = false;
        h.manual = Orientation::from_ypr(-10.0, 0.0, 0.0);
        assert!((h.wanted().ypr().0 + 10.0).abs() < 1e-3);
    }

    #[test]
    fn settings_read_back_and_default() {
        let s = HeadTrackingSettings {
            enabled: true,
            port: 9001,
            invert_pitch: true,
            ..HeadTrackingSettings::default()
        };
        let json = serde_json::to_string(&s).unwrap();
        assert_eq!(
            serde_json::from_str::<HeadTrackingSettings>(&json).unwrap(),
            s
        );
        let empty: HeadTrackingSettings = serde_json::from_str("{}").unwrap();
        assert_eq!(empty, HeadTrackingSettings::default());
        assert_eq!(empty.port, DEFAULT_PORT);
    }
}
