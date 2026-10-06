//! Control surfaces: Mackie Control (and its extender) and HUI over MIDI,
//! OSC over UDP (`faderframe_control` has the protocols).
//!
//! Each surface shows a bank of the mixer's strips — the surfaces side by
//! side in the order they are set up, from the shared bank start — with
//! the master fader, the transport and the song position; every tick the
//! session builds what they should show and the protocol sends what
//! changed. What is done on a surface becomes session actions: faders and
//! pan pots are gestures (one undo step from touch to release, or until
//! they rest for 400 ms), buttons toggle, select, run the transport or
//! move the bank. A MIDI surface's ports are its own: the MIDI hub and the
//! track outputs leave them alone. OSC listens on a UDP port and answers
//! whoever spoke last, on the reply port.

use crate::{Action, Session, TransportAction};
use faderframe_control::{
    AutomationButton, Button, MASTER, Page, Position, Protocol, StripState, SurfaceInput,
    SurfaceState,
};
use faderframe_core::{FaderLaw, TrackId};
use faderframe_project::{Command, Track, TrackKind};
use serde::{Deserialize, Serialize};
use std::net::{SocketAddr, UdpSocket};
use std::time::{Duration, Instant};

/// A fader or pot at rest this long ends its gesture.
const QUIET: Duration = Duration::from_millis(400);
/// Pan per pot tick.
const PAN_STEP: f32 = 0.02;
/// OSC packets read per tick at most.
const MAX_PACKETS: usize = 256;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum SurfaceKind {
    #[default]
    Mackie,
    /// A Mackie Control extender (the next eight strips).
    MackieExtender,
    Hui,
    Osc,
}

impl SurfaceKind {
    pub const ALL: [SurfaceKind; 4] = [
        SurfaceKind::Mackie,
        SurfaceKind::MackieExtender,
        SurfaceKind::Hui,
        SurfaceKind::Osc,
    ];

    pub fn label(self) -> &'static str {
        match self {
            SurfaceKind::Mackie => "Mackie Control",
            SurfaceKind::MackieExtender => "Mackie Control Extender",
            SurfaceKind::Hui => "HUI",
            SurfaceKind::Osc => "OSC",
        }
    }

    pub fn is_midi(self) -> bool {
        self != SurfaceKind::Osc
    }
}

/// One surface as set up (saved in the preferences).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct SurfaceSettings {
    pub kind: SurfaceKind,
    /// MIDI port keys (MIDI surfaces).
    pub input: String,
    pub output: String,
    /// UDP ports (OSC): listened on, and answered to.
    pub listen: u16,
    pub reply: u16,
    /// Strips an OSC surface shows.
    pub strips: u8,
}

impl Default for SurfaceSettings {
    fn default() -> Self {
        Self {
            kind: SurfaceKind::Mackie,
            input: String::new(),
            output: String::new(),
            listen: 8000,
            reply: 9000,
            strips: 8,
        }
    }
}

enum Link {
    Midi(faderframe_midi_io::SurfacePorts),
    Osc {
        socket: UdpSocket,
        peer: Option<SocketAddr>,
        reply: u16,
    },
}

struct Surface {
    protocol: Box<dyn Protocol>,
    link: Link,
}

/// The surfaces' session state.
pub struct ControlState {
    settings: Vec<SurfaceSettings>,
    surfaces: Vec<Surface>,
    /// What went wrong opening each (by index in `settings`).
    errors: Vec<Option<String>>,
    /// The first strip's index in the mixer's tracks.
    bank: usize,
    started: Instant,
    /// A gesture opened from a surface, and when it last moved.
    gesture: Option<Instant>,
    /// Faders held on a surface (gesture ends on release).
    held: usize,
    /// What the pots control, and whether faders and pots are swapped.
    page: Page,
    flip: bool,
}

impl Default for ControlState {
    fn default() -> Self {
        Self {
            settings: Vec::new(),
            surfaces: Vec::new(),
            errors: Vec::new(),
            bank: 0,
            started: Instant::now(),
            gesture: None,
            held: 0,
            page: Page::Pan,
            flip: false,
        }
    }
}

/// Pot ticks per full travel (levels).
const LEVEL_STEP: f32 = 0.01;

fn pan_text(pan: f32) -> String {
    let p = (pan * 100.0).round() as i32;
    match p {
        0 => "<C>".into(),
        p if p < 0 => format!("L{}", -p),
        p => format!("R{p}"),
    }
}

fn level_text(db: f32) -> String {
    if db <= -100.0 {
        "-inf".into()
    } else {
        format!("{db:.1}")
    }
}

impl Session {
    /// The tracks with mixer strips, in the editors' order (not the master).
    pub fn surface_tracks(&self) -> Vec<&Track> {
        self.project
            .folder_order()
            .into_iter()
            .filter(|t| {
                matches!(
                    t.kind,
                    TrackKind::Audio | TrackKind::Instrument | TrackKind::Bus | TrackKind::Aux
                )
            })
            .collect()
    }

    pub fn control_surfaces(&self) -> &[SurfaceSettings] {
        &self.control.settings
    }

    /// Why each surface did not open (`None`: running).
    pub fn control_surface_errors(&self) -> &[Option<String>] {
        &self.control.errors
    }

    /// The first strip's track number (1-based).
    pub fn surface_bank(&self) -> usize {
        self.control.bank + 1
    }

    /// Set up the surfaces (opens their ports; MIDI ports they use are
    /// taken from the hub and the track outputs).
    pub fn set_control_surfaces(&mut self, settings: Vec<SurfaceSettings>) {
        self.control.surfaces.clear();
        self.control.errors.clear();
        self.control.settings = settings.clone();
        self.apply_surface_ports();
        for s in settings {
            match open(&s) {
                Ok(surface) => {
                    self.control.surfaces.push(surface);
                    self.control.errors.push(None);
                }
                Err(e) => {
                    tracing::warn!("control surface {}: {e}", s.kind.label());
                    self.control.errors.push(Some(e));
                }
            }
        }
        self.revision += 1;
    }

    /// A surface on the given ports (tests): its protocol on `ports`.
    pub fn add_virtual_surface(&mut self, kind: SurfaceKind) -> faderframe_midi_io::VirtualSurface {
        let (ports, other) = faderframe_midi_io::SurfacePorts::virtual_pair();
        let settings = SurfaceSettings {
            kind,
            ..SurfaceSettings::default()
        };
        self.control.surfaces.push(Surface {
            protocol: protocol_for(&settings),
            link: Link::Midi(ports),
        });
        self.control.settings.push(settings);
        self.control.errors.push(None);
        other
    }

    /// The MIDI ports surfaces use (keys).
    pub(crate) fn surface_ports(&self) -> (Vec<String>, Vec<String>) {
        let midi = self.control.settings.iter().filter(|s| s.kind.is_midi());
        let ins = midi
            .clone()
            .filter(|s| !s.input.is_empty())
            .map(|s| s.input.clone())
            .collect();
        let outs = midi
            .filter(|s| !s.output.is_empty())
            .map(|s| s.output.clone())
            .collect();
        (ins, outs)
    }

    /// Every tick: what came from the surfaces, then what they show.
    pub(crate) fn tick_control(&mut self) {
        if self.control.surfaces.is_empty() {
            return;
        }
        let mut inputs: Vec<(usize, SurfaceInput)> = Vec::new();
        for (k, s) in self.control.surfaces.iter_mut().enumerate() {
            let mut got = Vec::new();
            match &mut s.link {
                Link::Midi(ports) => {
                    while let Some(msg) = ports.recv() {
                        s.protocol.receive(&msg, &mut got);
                    }
                }
                Link::Osc { socket, peer, .. } => {
                    let mut buf = [0u8; 4096];
                    for _ in 0..MAX_PACKETS {
                        match socket.recv_from(&mut buf) {
                            Ok((n, from)) => {
                                *peer = Some(from);
                                s.protocol.receive(&buf[..n], &mut got);
                            }
                            Err(_) => break,
                        }
                    }
                }
            }
            inputs.extend(got.into_iter().map(|i| (k, i)));
        }
        for (k, input) in inputs {
            if let Err(e) = self.surface_input(k, input) {
                self.notify(crate::NoticeLevel::Warning, e.to_string());
            }
        }
        // A gesture at rest ends.
        if self.control.held == 0 && self.control.gesture.is_some_and(|t| t.elapsed() >= QUIET) {
            self.end_surface_gesture();
        }
        self.show_on_surfaces();
    }

    fn show_on_surfaces(&mut self) {
        let now = self.control.started.elapsed().as_secs_f64();
        let tracks: Vec<TrackId> = self.surface_tracks().iter().map(|t| t.id).collect();
        let law = FaderLaw::console();
        let tl = &self.project.timeline;
        let bbt = tl.meter.to_bbt(self.playhead());
        let display = bbt.tick * 960 / faderframe_timeline::TICKS_PER_QUARTER;
        let position = Position {
            bar: bbt.bar + 1,
            beat: bbt.beat as i32 + 1,
            sixteenth: (display / 240) as i32 + 1,
            tick: (display % 240) as i32,
        };
        let master = self
            .project
            .master()
            .map(|m| law.db_to_position(self.shown_volume_db(m)));
        let mut first = self.control.bank;
        let mut sends: Vec<(usize, Vec<Vec<u8>>)> = Vec::new();
        for k in 0..self.control.surfaces.len() {
            let n = self.control.surfaces[k].protocol.strips();
            let shown: Vec<Option<TrackId>> =
                (first..first + n).map(|i| tracks.get(i).copied()).collect();
            let strips = shown
                .iter()
                .map(|t| {
                    let Some(t) = t.and_then(|t| self.project.track(t)) else {
                        return StripState::default();
                    };
                    let db = self.shown_volume_db(t);
                    let m = self.meter(t.id);
                    let volume = law.db_to_position(db);
                    let (pot, bipolar, pot_text) =
                        self.pot_value(t).unwrap_or((0.0, false, String::new()));
                    // Flipped: the fader moves what the pot did, the pot
                    // the volume.
                    let (fader, pot, pot_bipolar) = if self.control.flip {
                        (pot, volume, false)
                    } else {
                        (volume, pot, bipolar)
                    };
                    let level = match self.control.page {
                        Page::Pan if !self.control.flip => level_text(db),
                        _ => pot_text,
                    };
                    StripState {
                        present: true,
                        name: t.name.clone(),
                        fader,
                        level,
                        pan: self.shown_pan(t),
                        pot,
                        pot_bipolar,
                        mute: self.shown_mute(t),
                        solo: t.solo,
                        arm: t.record_arm,
                        selected: self.selection.tracks.contains(&t.id),
                        meter_db: m.left.level_db.max(m.right.level_db),
                    }
                })
                .collect();
            let state = SurfaceState {
                strips,
                master,
                playing: self.transport.playing,
                recording: self.transport.recording,
                looping: self.project.loop_enabled,
                click: self.record.metronome != faderframe_engine::MetronomeMode::Off,
                position,
                first_track: first + 1,
                page: self.control.page,
                flip: self.control.flip,
                automation: self.surface_automation(),
            };
            let s = &mut self.control.surfaces[k];
            let mut out = Vec::new();
            s.protocol.update(&state, now, &mut out);
            if !out.is_empty() {
                sends.push((k, out));
            }
            first += n;
        }
        for (k, out) in sends {
            let s = &mut self.control.surfaces[k];
            match &mut s.link {
                Link::Midi(ports) => {
                    for m in out {
                        ports.send(&m);
                    }
                }
                Link::Osc {
                    socket,
                    peer: Some(peer),
                    reply,
                } => {
                    let to = SocketAddr::new(peer.ip(), *reply);
                    for m in out {
                        let _ = socket.send_to(&m, to);
                    }
                }
                // Nobody to answer yet: send everything once someone speaks.
                Link::Osc { peer: None, .. } => s.protocol.reset(),
            }
        }
    }

    /// The track on strip `strip` of surface `k`.
    fn strip_track(&self, k: usize, strip: usize) -> Option<TrackId> {
        if strip == MASTER {
            return self.project.master().map(|m| m.id);
        }
        let before: usize = self.control.surfaces[..k]
            .iter()
            .map(|s| s.protocol.strips())
            .sum();
        self.surface_tracks()
            .get(self.control.bank + before + strip)
            .map(|t| t.id)
    }

    fn begin_surface_gesture(&mut self) -> crate::Result<()> {
        if self.control.gesture.is_none() {
            self.dispatch(Action::BeginGesture("Control Surface".into()))?;
        }
        self.control.gesture = Some(Instant::now());
        Ok(())
    }

    fn end_surface_gesture(&mut self) {
        if self.control.gesture.take().is_some() {
            let _ = self.dispatch(Action::EndGesture);
        }
    }

    fn surface_input(&mut self, k: usize, input: SurfaceInput) -> crate::Result<()> {
        let law = FaderLaw::console();
        match input {
            SurfaceInput::Fader { strip, travel } => {
                let Some(track) = self.strip_track(k, strip) else {
                    return Ok(());
                };
                self.begin_surface_gesture()?;
                if self.control.flip && strip != MASTER {
                    self.set_pot(track, travel)?;
                } else {
                    self.dispatch(Action::Edit(Command::SetTrackVolume {
                        track,
                        db: law.position_to_db(travel),
                    }))?;
                }
            }
            SurfaceInput::Touch { touched, .. } => {
                if touched {
                    self.control.held += 1;
                    self.begin_surface_gesture()?;
                } else {
                    self.control.held = self.control.held.saturating_sub(1);
                    if self.control.held == 0 {
                        self.end_surface_gesture();
                    }
                }
            }
            SurfaceInput::Pot { strip, delta } => {
                let Some(t) = self
                    .strip_track(k, strip)
                    .and_then(|t| self.project.track(t))
                else {
                    return Ok(());
                };
                let track = t.id;
                let volume = law.db_to_position(self.shown_volume_db(t));
                let pot = self.pot_value(t).map(|(v, _, _)| v);
                self.begin_surface_gesture()?;
                if self.control.flip {
                    let travel = (volume + delta as f32 * LEVEL_STEP).clamp(0.0, 1.0);
                    self.dispatch(Action::Edit(Command::SetTrackVolume {
                        track,
                        db: law.position_to_db(travel),
                    }))?;
                } else if let Some(v) = pot {
                    let step = match self.control.page {
                        Page::Pan => PAN_STEP / 2.0,
                        Page::Send(_) => LEVEL_STEP,
                    };
                    self.set_pot(track, (v + delta as f32 * step).clamp(0.0, 1.0))?;
                }
            }
            SurfaceInput::Pan { strip, pan } => {
                let Some(track) = self.strip_track(k, strip) else {
                    return Ok(());
                };
                self.begin_surface_gesture()?;
                self.dispatch(Action::Edit(Command::SetTrackPan {
                    track,
                    pan: pan.clamp(-1.0, 1.0),
                }))?;
            }
            SurfaceInput::Jog(ticks) => {
                // A sixteenth a tick.
                let q = faderframe_timeline::MusicalTime::QUARTER.ticks() / 4;
                let at = self.playhead()
                    + faderframe_timeline::MusicalTime::from_ticks(i64::from(ticks) * q);
                let at = at.max(faderframe_timeline::MusicalTime::ZERO);
                self.dispatch(Action::Transport(TransportAction::Locate(at)))?;
            }
            SurfaceInput::Button { button, pressed } => {
                if pressed {
                    self.surface_button(k, button)?;
                }
            }
            SurfaceInput::LaunchClip { strip, scene } => {
                let track = self.strip_track(k, strip);
                let scene = self.project.launcher.scenes.get(scene).map(|s| s.id);
                if let (Some(track), Some(scene)) = (track, scene) {
                    self.dispatch(Action::Launcher(crate::launcher::LauncherOp::Launch {
                        track,
                        scene,
                    }))?;
                }
            }
            SurfaceInput::LaunchScene(n) => {
                if let Some(scene) = self.project.launcher.scenes.get(n).map(|s| s.id) {
                    self.dispatch(Action::Launcher(crate::launcher::LauncherOp::LaunchScene(
                        scene,
                    )))?;
                }
            }
            SurfaceInput::StopClips => {
                self.dispatch(Action::Launcher(crate::launcher::LauncherOp::StopAll))?;
            }
            SurfaceInput::BackToArrangement => {
                self.dispatch(Action::Launcher(
                    crate::launcher::LauncherOp::BackToArrangement,
                ))?;
            }
            SurfaceInput::Refresh => self.control.surfaces[k].protocol.reset(),
        }
        Ok(())
    }

    fn surface_button(&mut self, k: usize, button: Button) -> crate::Result<()> {
        let track = |s: &Self, i: usize| {
            s.strip_track(k, i)
                .and_then(|t| s.project.track(t))
                .cloned()
        };
        let edit = |cmd| Action::Edit(cmd);
        let width = self.control.surfaces[k].protocol.strips();
        let count = self.surface_tracks().len();
        match button {
            Button::Mute(i) => {
                if let Some(t) = track(self, i) {
                    let on = !self.shown_mute(&t);
                    self.dispatch(edit(Command::SetTrackMute { track: t.id, on }))?;
                }
            }
            Button::Solo(i) => {
                if let Some(t) = track(self, i) {
                    self.dispatch(edit(Command::SetTrackSolo {
                        track: t.id,
                        on: !t.solo,
                    }))?;
                }
            }
            Button::Arm(i) => {
                if let Some(t) = track(self, i) {
                    self.dispatch(edit(Command::SetTrackRecordArm {
                        track: t.id,
                        on: !t.record_arm,
                    }))?;
                }
            }
            Button::Select(i) => {
                if let Some(t) = track(self, i) {
                    self.dispatch(Action::SelectTracks {
                        tracks: vec![t.id],
                        mode: crate::SelectMode::Replace,
                    })?;
                }
            }
            Button::PotPress(i) => {
                // Pan to the centre, a send (or, flipped, the volume) to 0 dB.
                if let Some(t) = track(self, i) {
                    let unity = FaderLaw::console().unity_position();
                    if self.control.flip {
                        self.dispatch(edit(Command::SetTrackVolume {
                            track: t.id,
                            db: 0.0,
                        }))?;
                    } else {
                        match self.control.page {
                            Page::Pan => self.set_pot(t.id, 0.5)?,
                            Page::Send(_) => self.set_pot(t.id, unity)?,
                        }
                    }
                }
            }
            Button::Flip => {
                self.control.flip = !self.control.flip;
                self.revision += 1;
            }
            Button::PanPage => self.control.page = Page::Pan,
            Button::SendPage(Some(n)) => self.control.page = Page::Send(n),
            Button::SendPage(None) => {
                // Cycles through the sends the shown tracks have.
                let most = self
                    .surface_tracks()
                    .iter()
                    .map(|t| t.sends.len())
                    .max()
                    .unwrap_or(0)
                    .max(1);
                self.control.page = match self.control.page {
                    Page::Send(n) => Page::Send((n + 1) % most),
                    Page::Pan => Page::Send(0),
                };
            }
            Button::Automation(b) => self.surface_automation_mode(b)?,
            Button::Play => self.dispatch(Action::Transport(TransportAction::Play))?,
            Button::Stop => self.dispatch(Action::Transport(TransportAction::Stop))?,
            Button::Record => self.dispatch(Action::Transport(TransportAction::ToggleRecord))?,
            Button::Rewind => self.dispatch(Action::Transport(TransportAction::NudgeBars(-1)))?,
            Button::Forward => self.dispatch(Action::Transport(TransportAction::NudgeBars(1)))?,
            Button::Loop => self.dispatch(Action::Transport(TransportAction::ToggleLoop))?,
            Button::Start => self.dispatch(Action::Transport(TransportAction::ReturnToStart))?,
            Button::End => {
                let end = self.project.content_end();
                self.dispatch(Action::Transport(TransportAction::Locate(end)))?;
            }
            Button::Click => {
                let mut r = self.record;
                r.metronome = match r.metronome {
                    faderframe_engine::MetronomeMode::Off => {
                        faderframe_engine::MetronomeMode::Always
                    }
                    _ => faderframe_engine::MetronomeMode::Off,
                };
                self.dispatch(Action::SetRecordSettings(r))?;
            }
            Button::Undo => self.dispatch(Action::Undo)?,
            Button::Marker => {
                let at = self.playhead();
                self.dispatch(Action::AddMarker(at))?;
            }
            // A project without a file name is saved from the window (it
            // needs a file chooser).
            Button::Save => match self.save() {
                Ok(()) => self.notify(crate::NoticeLevel::Info, "saved"),
                Err(e) => self.notify(crate::NoticeLevel::Warning, e.to_string()),
            },
            Button::BankLeft => self.control.bank = self.control.bank.saturating_sub(width),
            Button::BankRight => {
                if self.control.bank + width < count {
                    self.control.bank += width;
                }
            }
            Button::ChannelLeft => self.control.bank = self.control.bank.saturating_sub(1),
            Button::ChannelRight => {
                if self.control.bank + 1 < count {
                    self.control.bank += 1;
                }
            }
        }
        Ok(())
    }
}

impl Session {
    /// What a track's pot controls on the current page: travel (0…1),
    /// whether it is bipolar, its value as text.
    fn pot_value(&self, t: &Track) -> Option<(f32, bool, String)> {
        match self.control.page {
            Page::Pan => {
                let pan = self.shown_pan(t);
                Some(((pan + 1.0) / 2.0, true, pan_text(pan)))
            }
            Page::Send(n) => t.sends.get(n).map(|s| {
                let db = self.shown_send_db(t, s);
                (
                    FaderLaw::console().db_to_position(db),
                    false,
                    level_text(db),
                )
            }),
        }
    }

    /// Set what the pot controls on the current page from travel (0…1).
    fn set_pot(&mut self, track: TrackId, travel: f32) -> crate::Result<()> {
        match self.control.page {
            Page::Pan => self.dispatch(Action::Edit(Command::SetTrackPan {
                track,
                pan: (travel * 2.0 - 1.0).clamp(-1.0, 1.0),
            })),
            Page::Send(n) => {
                let Some(send) = self
                    .project
                    .track(track)
                    .and_then(|t| t.sends.get(n))
                    .map(|s| s.id)
                else {
                    return Ok(());
                };
                self.dispatch(Action::Edit(Command::SetSendLevel {
                    track,
                    send,
                    db: FaderLaw::console().position_to_db(travel),
                }))
            }
        }
    }

    /// The selected track's automation mode (its first lane's).
    fn surface_automation(&self) -> Option<AutomationButton> {
        use faderframe_automation::AutomationMode as M;
        let t = self.project.track(self.selection.primary_track()?)?;
        Some(match t.automation.lanes.first()?.mode {
            M::Off => AutomationButton::Off,
            M::Read => AutomationButton::Read,
            M::Touch => AutomationButton::Touch,
            M::Latch => AutomationButton::Latch,
            M::Write => AutomationButton::Write,
        })
    }

    /// The selected track's lanes in a mode (a volume lane made if it has
    /// none).
    fn surface_automation_mode(&mut self, b: AutomationButton) -> crate::Result<()> {
        use faderframe_automation::AutomationMode as M;
        let Some(track) = self.selection.primary_track() else {
            self.notify(
                crate::NoticeLevel::Info,
                "select a track for its automation mode",
            );
            return Ok(());
        };
        let none = self
            .project
            .track(track)
            .is_some_and(|t| t.automation.lanes.is_empty());
        if none {
            self.dispatch(Action::ShowAutomation {
                track,
                target: faderframe_automation::AutomationTarget::TrackVolume,
            })?;
        }
        let lanes: Vec<_> = self
            .project
            .track(track)
            .map(|t| t.automation.lanes.iter().map(|l| (track, l.id)).collect())
            .unwrap_or_default();
        let mode = match b {
            AutomationButton::Off => M::Off,
            AutomationButton::Read => M::Read,
            AutomationButton::Touch => M::Touch,
            AutomationButton::Latch => M::Latch,
            AutomationButton::Write => M::Write,
        };
        self.dispatch(Action::SetAutomationModes { lanes, mode })
    }
}

fn protocol_for(s: &SurfaceSettings) -> Box<dyn Protocol> {
    match s.kind {
        SurfaceKind::Mackie => Box::new(faderframe_control::mackie::Mackie::new(
            faderframe_control::mackie::MCU,
        )),
        SurfaceKind::MackieExtender => Box::new(faderframe_control::mackie::Mackie::new(
            faderframe_control::mackie::XT,
        )),
        SurfaceKind::Hui => Box::new(faderframe_control::hui::Hui::new()),
        SurfaceKind::Osc => Box::new(faderframe_control::osc::OscSurface::new(usize::from(
            s.strips.max(1),
        ))),
    }
}

fn open(s: &SurfaceSettings) -> Result<Surface, String> {
    let link = if s.kind.is_midi() {
        Link::Midi(faderframe_midi_io::SurfacePorts::open(&s.input, &s.output)?)
    } else {
        let socket = UdpSocket::bind(("0.0.0.0", s.listen))
            .map_err(|e| format!("UDP port {}: {e}", s.listen))?;
        socket.set_nonblocking(true).map_err(|e| e.to_string())?;
        Link::Osc {
            socket,
            peer: None,
            reply: s.reply,
        }
    };
    Ok(Surface {
        protocol: protocol_for(s),
        link,
    })
}
