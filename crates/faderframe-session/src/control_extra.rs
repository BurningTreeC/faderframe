//! The rest of what control surfaces do: the channel-strip pages (the
//! selected track's own parameters, its devices', its EQ's or its
//! instrument's on the pots and the display), track filters (the
//! track-type buttons), the time display as timecode, the modes of the
//! arrows and the jog wheel (zoom, nudge, scrub, shuttle), modifier keys,
//! the numeric keypad, windows, edit commands, punch and the monitor
//! section.

use crate::automation::{AutomationParam, ParamKind};
use crate::{Action, Session, TransportAction};
use faderframe_automation::AutomationTarget;
use faderframe_control::{Button, EditButton, Modes, Modifier, Page, TrackFilter, Window};
use faderframe_core::TrackId;
use faderframe_project::{Command, Track, TrackKind};
use faderframe_timeline::MusicalTime;

/// Pot ticks per full travel of a parameter.
const PARAM_STEP: f64 = 1.0 / 128.0;

/// The surfaces' extra state (see the module docs).
#[derive(Debug, Default)]
pub(crate) struct ExtraState {
    pub modifiers: [bool; 4],
    pub filter: Option<TrackFilter>,
    /// The lower display line shows the pots' values.
    pub values: bool,
    /// The time display shows timecode.
    pub timecode: bool,
    pub zoom: bool,
    pub scrub: bool,
    pub shuttle: bool,
    pub nudge: bool,
    /// Channel-strip pages: the first parameter shown, and which of the
    /// track's devices (the plug-in page).
    pub param_offset: usize,
    pub device: usize,
    /// Keys typed on the numeric keypad.
    pub numpad: String,
}

impl ExtraState {
    pub fn held(&self, m: Modifier) -> bool {
        self.modifiers[m.index()]
    }
}

/// Does `t` pass the strips' track filter?
pub(crate) fn passes(s: &Session, t: &Track, filter: Option<TrackFilter>) -> bool {
    let strip = matches!(
        t.kind,
        TrackKind::Audio | TrackKind::Instrument | TrackKind::Bus | TrackKind::Aux
    );
    match filter {
        None => strip,
        Some(TrackFilter::Midi | TrackFilter::Instruments) => t.kind == TrackKind::Instrument,
        Some(TrackFilter::Inputs) => strip && t.record_arm,
        Some(TrackFilter::Audio) => t.kind == TrackKind::Audio,
        Some(TrackFilter::Aux) => t.kind == TrackKind::Aux,
        Some(TrackFilter::Buses) => t.kind == TrackKind::Bus,
        Some(TrackFilter::Outputs) => t.kind == TrackKind::Vca,
        Some(TrackFilter::User) => strip && s.selection.tracks.contains(&t.id),
    }
}

impl Session {
    /// The track the channel-strip pages show: the selected one, else the
    /// first strip's.
    fn page_track(&self) -> Option<TrackId> {
        self.selection
            .primary_track()
            .or_else(|| self.surface_tracks().first().map(|t| t.id))
    }

    /// The parameters a channel-strip page puts on the pots: the track
    /// and its parameters (and the device shown, on device pages).
    pub(crate) fn page_params(
        &self,
        page: Page,
    ) -> Option<(
        TrackId,
        Vec<AutomationParam>,
        Option<faderframe_core::PluginInstanceId>,
    )> {
        let track = self.page_track()?;
        let t = self.project.track(track)?;
        let all = self.automatable_parameters(track);
        let of_device = |id: faderframe_core::PluginInstanceId| -> Vec<AutomationParam> {
            all.iter()
                .filter(|p| match p.target {
                    AutomationTarget::PluginParameter { plugin, .. } => plugin == id,
                    AutomationTarget::PluginBypass(plugin) => plugin == id,
                    _ => false,
                })
                .cloned()
                .collect()
        };
        let slots: Vec<&faderframe_project::PluginSlot> = t.slots();
        let device = match page {
            Page::Track => {
                let own = all
                    .iter()
                    .filter(|p| {
                        !matches!(
                            p.target,
                            AutomationTarget::PluginParameter { .. }
                                | AutomationTarget::PluginBypass(_)
                        )
                    })
                    .cloned()
                    .collect();
                return Some((track, own, None));
            }
            Page::Plugin if !slots.is_empty() => {
                Some(slots[self.control.extra.device % slots.len()].id)
            }
            Page::Eq => slots
                .iter()
                .find(|s| {
                    s.plugin.id == faderframe_core::builtin::EQ
                        || s.plugin.name.to_ascii_lowercase().contains("eq")
                })
                .map(|s| s.id),
            Page::Instrument => slots
                .iter()
                .find(|s| self.is_instrument_plugin(&s.plugin))
                .map(|s| s.id),
            _ => None,
        };
        let params = device.map(of_device).unwrap_or_default();
        Some((track, params, device))
    }

    /// A parameter's short name (without its device's).
    pub(crate) fn param_label(p: &AutomationParam) -> String {
        p.name
            .rsplit_once(": ")
            .map_or_else(|| p.name.clone(), |(_, n)| n.to_string())
    }

    /// A parameter as the pot shows it: travel, bipolar, value text.
    pub(crate) fn param_pot(&self, track: TrackId, p: &AutomationParam) -> (f32, bool, String) {
        let v = self.display_value(track, p.target).unwrap_or(p.default);
        (p.to_normal(v) as f32, p.kind == ParamKind::Pan, p.format(v))
    }

    /// Turn the channel-strip page's pot `i` by `delta` ticks.
    pub(crate) fn turn_param(&mut self, i: usize, delta: i32) -> crate::Result<()> {
        let Some((track, params, _)) = self.page_params(self.control.page) else {
            return Ok(());
        };
        let Some(p) = params.get(self.control.extra.param_offset + i) else {
            return Ok(());
        };
        let v = self.display_value(track, p.target).unwrap_or(p.default);
        let next = if p.kind == ParamKind::Toggle {
            if delta > 0 { p.max } else { p.min }
        } else {
            p.from_normal((p.to_normal(v) + f64::from(delta) * PARAM_STEP).clamp(0.0, 1.0))
        };
        self.set_param(track, p.target, next)
    }

    /// Set the page's parameter `i` from travel (a flipped fader).
    pub(crate) fn move_param(&mut self, i: usize, travel: f32) -> crate::Result<()> {
        let Some((track, params, _)) = self.page_params(self.control.page) else {
            return Ok(());
        };
        let Some(p) = params.get(self.control.extra.param_offset + i).cloned() else {
            return Ok(());
        };
        let v = p.from_normal(f64::from(travel).clamp(0.0, 1.0));
        self.set_param(track, p.target, v)
    }

    /// The page's parameter `i` back to its default (a pot pressed).
    pub(crate) fn reset_param(&mut self, i: usize) -> crate::Result<()> {
        let Some((track, params, _)) = self.page_params(self.control.page) else {
            return Ok(());
        };
        let Some(p) = params.get(self.control.extra.param_offset + i).cloned() else {
            return Ok(());
        };
        self.set_param(track, p.target, p.default)
    }

    fn set_param(&mut self, track: TrackId, target: AutomationTarget, v: f64) -> crate::Result<()> {
        if let Some(c) = self.command_for(track, target, v) {
            self.dispatch(Action::Edit(c))?;
        }
        Ok(())
    }

    /// The song position as timecode (hours, minutes, seconds, frames) at
    /// the MIDI time code rate.
    pub(crate) fn surface_timecode(&self) -> [i32; 4] {
        let rate = self.sync_settings().mtc_out_rate;
        let seconds =
            self.transport.position.max(0) as f64 / f64::from(self.engine.sample_rate().max(1));
        let tc = faderframe_midi::timecode::Timecode::from_seconds(seconds, rate);
        [
            i32::from(tc.hours),
            i32::from(tc.minutes),
            i32::from(tc.seconds),
            i32::from(tc.frames),
        ]
    }

    /// What the surfaces' mode lights show.
    pub(crate) fn surface_modes(&self) -> Modes {
        let e = &self.control.extra;
        Modes {
            zoom: e.zoom,
            scrub: e.scrub,
            shuttle: e.shuttle,
            nudge: e.nudge,
            punch: self.project.punch_enabled,
            replace: self.record.mode == crate::RecordMode::Replace,
            any_solo: self.project.tracks.iter().any(|t| t.solo),
            values: e.values,
            filter: e.filter,
            modifiers: e.modifiers,
        }
    }

    /// The jog wheel: a sixteenth a tick (scrubbing: a 64th, heard;
    /// shuttling: a beat, heard).
    pub(crate) fn surface_jog(&mut self, ticks: i32) -> crate::Result<()> {
        let (scrub, shuttle) = (self.control.extra.scrub, self.control.extra.shuttle);
        let q = MusicalTime::QUARTER.ticks();
        let step = if shuttle {
            q
        } else if scrub {
            q / 16
        } else {
            q / 4
        };
        let at = (self.playhead() + MusicalTime::from_ticks(i64::from(ticks) * step))
            .max(MusicalTime::ZERO);
        self.dispatch(Action::Transport(TransportAction::Locate(at)))?;
        if (scrub || shuttle) && !self.transport.playing {
            self.dispatch(Action::Transport(TransportAction::Scrub(at)))?;
        }
        Ok(())
    }

    /// Buttons beyond the mixer's and the transport's (true: handled).
    pub(crate) fn surface_extra_button(&mut self, button: Button) -> crate::Result<bool> {
        let shift = self.control.extra.held(Modifier::Shift);
        match button {
            Button::TrackPage | Button::PluginPage | Button::EqPage | Button::InstrumentPage => {
                let page = match button {
                    Button::TrackPage => Page::Track,
                    Button::PluginPage => Page::Plugin,
                    Button::EqPage => Page::Eq,
                    _ => Page::Instrument,
                };
                // The plug-in page again: the next device.
                if page == Page::Plugin && self.control.page == Page::Plugin {
                    self.control.extra.device += 1;
                }
                self.control.page = page;
                self.control.extra.param_offset = 0;
            }
            Button::Function(n) => {
                if shift {
                    // A marker (by time).
                    let mut at: Vec<MusicalTime> =
                        self.project.markers.iter().map(|m| m.position).collect();
                    at.sort();
                    if let Some(t) = at.get(usize::from(n)) {
                        self.dispatch(Action::Transport(TransportAction::Locate(*t)))?;
                    }
                } else {
                    self.dispatch(Action::Workspace(crate::WorkspaceAction::Switch(
                        usize::from(n),
                    )))?;
                }
            }
            Button::GlobalView => self.set_filter(None),
            Button::TrackType(f) => {
                let next = (self.control.extra.filter != Some(f)).then_some(f);
                self.set_filter(next);
            }
            Button::NameValue => self.control.extra.values = !self.control.extra.values,
            Button::TimeDisplay => self.control.extra.timecode = !self.control.extra.timecode,
            Button::Group => self.dispatch(Action::GroupSelectedTracks)?,
            Button::Cancel => {
                self.control.extra.numpad.clear();
                self.dispatch(Action::ClearSelection)?;
            }
            Button::Enter => {
                if !self.control.extra.numpad.is_empty() {
                    self.numpad('E')?;
                } else if let Some(r) = self.selection.range {
                    self.dispatch(Action::Transport(TransportAction::Locate(r.start)))?;
                }
            }
            Button::Nudge => {
                let e = &mut self.control.extra;
                e.nudge = !e.nudge;
                e.zoom &= !e.nudge;
            }
            Button::Zoom => {
                let e = &mut self.control.extra;
                e.zoom = !e.zoom;
                e.nudge &= !e.zoom;
            }
            Button::Scrub => {
                let e = &mut self.control.extra;
                e.scrub = !e.scrub;
                e.shuttle &= !e.scrub;
            }
            Button::Shuttle => {
                let e = &mut self.control.extra;
                e.shuttle = !e.shuttle;
                e.scrub &= !e.shuttle;
            }
            Button::Up | Button::Down => {
                let up = button == Button::Up;
                if self.control.extra.zoom {
                    let z = if up {
                        crate::ZoomRequest::In
                    } else {
                        crate::ZoomRequest::Out
                    };
                    self.dispatch(Action::Zoom(z))?;
                } else {
                    self.select_neighbour(if up { -1 } else { 1 })?;
                }
            }
            Button::Drop => self.dispatch(Action::Transport(TransportAction::TogglePunch))?,
            Button::Replace => {
                let mut r = self.record;
                r.mode = match r.mode {
                    crate::RecordMode::Replace => crate::RecordMode::Takes,
                    crate::RecordMode::Takes => crate::RecordMode::Replace,
                };
                self.dispatch(Action::SetRecordSettings(r))?;
            }
            Button::PunchIn | Button::PunchOut => {
                let here = self.playhead();
                let now = self.project.punch_range;
                let (a, b) = match (button, now) {
                    (Button::PunchIn, Some(r)) => (here, r.end.max(here)),
                    (Button::PunchIn, None) => (here, here + MusicalTime::from_quarters(4.0)),
                    (_, Some(r)) => (r.start.min(here), here),
                    (_, None) => (MusicalTime::ZERO, here),
                };
                if let Some(range) = faderframe_project::MusicalRange::new(a, b) {
                    self.dispatch(Action::Transport(TransportAction::SetPunch(Some(range))))?;
                }
            }
            Button::PreRoll => {
                let mut r = self.record;
                r.preroll_bars = if r.preroll_bars == 0 { 1 } else { 0 };
                self.dispatch(Action::SetRecordSettings(r))?;
            }
            Button::ClearSolo => self.clear_all(
                |t| t.solo,
                |track| Command::SetTrackSolo { track, on: false },
                "Unsolo All",
            )?,
            Button::ArmAll => self.arm_all()?,
            Button::MonitorMute => {
                if let Some(m) = self.project.master() {
                    let (track, on) = (m.id, !self.shown_mute(m));
                    self.dispatch(Action::Edit(Command::SetTrackMute { track, on }))?;
                }
            }
            Button::InputMonitor => {
                use faderframe_project::MonitorMode as M;
                if let Some(t) = self
                    .selection
                    .primary_track()
                    .and_then(|t| self.project.track(t))
                {
                    let mode = match t.monitor {
                        M::Off => M::Input,
                        M::Input => M::Auto,
                        M::Auto => M::Off,
                    };
                    let track = t.id;
                    self.dispatch(Action::Edit(Command::SetTrackMonitor { track, mode }))?;
                }
            }
            Button::Bypass => {
                if let Some((track, _, Some(plugin))) = self.page_params(self.control.page) {
                    let on = self
                        .project
                        .track(track)
                        .and_then(|t| {
                            t.slots()
                                .into_iter()
                                .find(|s| s.id == plugin)
                                .map(|s| s.bypass)
                        })
                        .unwrap_or(false);
                    self.dispatch(Action::Edit(Command::SetPluginBypass {
                        track,
                        plugin,
                        bypass: !on,
                    }))?;
                }
            }
            Button::Window(w) => {
                let view = match w {
                    Window::Mixer => faderframe_workspace::ViewId::mixer(),
                    Window::Editor => faderframe_workspace::ViewId::arranger(),
                    Window::Launcher => faderframe_workspace::ViewId::launcher(),
                };
                self.dispatch(Action::Workspace(crate::WorkspaceAction::ShowView(view)))?;
            }
            Button::EditMode => {
                use crate::EditMode as M;
                let i = M::ALL
                    .iter()
                    .position(|m| *m == self.editor.edit_mode)
                    .unwrap_or(0);
                self.dispatch(Action::SetEditMode(M::ALL[(i + 1) % M::ALL.len()]))?;
            }
            Button::EditTool => {
                use crate::EditTool as T;
                let i = T::ALL
                    .iter()
                    .position(|t| *t == self.editor.tool)
                    .unwrap_or(0);
                self.dispatch(Action::SetEditTool(T::ALL[(i + 1) % T::ALL.len()]))?;
            }
            Button::Edit(e) => {
                let action = match e {
                    EditButton::Cut => Action::CutRange,
                    EditButton::Copy => Action::CopyRange,
                    EditButton::Paste => Action::PasteRange,
                    EditButton::Delete => Action::DeleteSelection,
                    EditButton::Separate => Action::Separate,
                    EditButton::Capture => Action::CaptureMidi,
                };
                self.dispatch(action)?;
            }
            Button::Footswitch(0) => {
                self.dispatch(Action::Transport(TransportAction::TogglePlay))?
            }
            Button::Footswitch(_) => {
                self.dispatch(Action::Transport(TransportAction::ToggleRecord))?;
            }
            Button::Numpad(c) => self.numpad(c)?,
            _ => return Ok(false),
        }
        self.revision += 1;
        Ok(true)
    }

    fn set_filter(&mut self, filter: Option<TrackFilter>) {
        self.control.extra.filter = filter;
        self.control.bank = 0;
    }

    /// The previous (−1) or next (+1) strip's track selected.
    fn select_neighbour(&mut self, by: i32) -> crate::Result<()> {
        let tracks: Vec<TrackId> = self.surface_tracks().iter().map(|t| t.id).collect();
        if tracks.is_empty() {
            return Ok(());
        }
        let at = self
            .selection
            .primary_track()
            .and_then(|t| tracks.iter().position(|x| *x == t));
        let next = match at {
            Some(i) => (i as i32 + by).clamp(0, tracks.len() as i32 - 1) as usize,
            None => 0,
        };
        self.dispatch(Action::SelectTracks {
            tracks: vec![tracks[next]],
            mode: crate::SelectMode::Replace,
        })
    }

    /// Every track `on` returns true for gets `cmd` (one undo step).
    fn clear_all(
        &mut self,
        on: impl Fn(&Track) -> bool,
        cmd: impl Fn(TrackId) -> Command,
        label: &str,
    ) -> crate::Result<()> {
        let commands: Vec<Command> = self
            .project
            .tracks
            .iter()
            .filter(|t| on(t))
            .map(|t| cmd(t.id))
            .collect();
        if !commands.is_empty() {
            self.edit(Command::Batch {
                label: label.into(),
                commands,
            })?;
        }
        Ok(())
    }

    /// Arm every track that records (or disarm them all when one is).
    pub(crate) fn arm_all(&mut self) -> crate::Result<()> {
        let records = |t: &Track| {
            matches!(
                t.kind,
                TrackKind::Audio | TrackKind::Instrument | TrackKind::Midi
            )
        };
        let any = self
            .project
            .tracks
            .iter()
            .any(|t| records(t) && t.record_arm);
        let commands: Vec<Command> = self
            .project
            .tracks
            .iter()
            .filter(|t| records(t) && t.record_arm == any)
            .map(|t| Command::SetTrackRecordArm {
                track: t.id,
                on: !any,
            })
            .collect();
        if !commands.is_empty() {
            self.edit(Command::Batch {
                label: if any { "Disarm All" } else { "Arm All" }.into(),
                commands,
            })?;
        }
        Ok(())
    }

    /// Every track's mute (or solo) set like `on` (Option + a strip's
    /// button).
    pub(crate) fn set_all(&mut self, solo: bool, on: bool) -> crate::Result<()> {
        let commands: Vec<Command> = self
            .surface_tracks()
            .iter()
            .map(|t| {
                if solo {
                    Command::SetTrackSolo { track: t.id, on }
                } else {
                    Command::SetTrackMute { track: t.id, on }
                }
            })
            .collect();
        self.edit(Command::Batch {
            label: if solo { "Solo All" } else { "Mute All" }.into(),
            commands,
        })
    }

    /// A key of the numeric keypad: digits (and '.') are typed; enter
    /// locates to the typed bar (`.n.`: marker n) or, with nothing typed,
    /// adds a marker; '+' and '-' move a bar; 'C' clears.
    pub(crate) fn numpad(&mut self, c: char) -> crate::Result<()> {
        match c {
            '0'..='9' => self.control.extra.numpad.push(c),
            '.' => {
                let typed = std::mem::take(&mut self.control.extra.numpad);
                match typed.strip_prefix('.') {
                    // `.n.`: marker n.
                    Some(n) if !n.is_empty() => {
                        let mut at: Vec<MusicalTime> =
                            self.project.markers.iter().map(|m| m.position).collect();
                        at.sort();
                        if let Some(t) = n
                            .parse::<usize>()
                            .ok()
                            .and_then(|n| at.get(n.saturating_sub(1)))
                        {
                            self.dispatch(Action::Transport(TransportAction::Locate(*t)))?;
                        }
                    }
                    _ => self.control.extra.numpad = format!("{typed}."),
                }
            }
            'C' => self.control.extra.numpad.clear(),
            '+' | '-' => {
                let by = if c == '+' { 1 } else { -1 };
                self.dispatch(Action::Transport(TransportAction::NudgeBars(by)))?;
            }
            'E' => {
                let typed = std::mem::take(&mut self.control.extra.numpad);
                match typed.trim_matches('.').parse::<i32>() {
                    Ok(bar) if bar >= 1 => {
                        let at = self.project.timeline.meter.bar_start(bar - 1);
                        self.dispatch(Action::Transport(TransportAction::Locate(at)))?;
                    }
                    _ => {
                        let at = self.playhead();
                        self.dispatch(Action::AddMarker(at))?;
                    }
                }
            }
            _ => {}
        }
        Ok(())
    }
}
