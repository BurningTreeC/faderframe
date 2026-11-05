//! The phone and web remote (`faderframe_remote` has the server, the
//! protocol and the app): FaderFrame's transport, mixer, cue mixes,
//! setlist, lyrics and clip launcher in a browser on the local network.
//!
//! Off until turned on ([`WebRemoteSettings::on`], kept in the
//! preferences with the paired devices). While it runs, every tick takes
//! what paired devices sent ([`faderframe_remote::Event`]s, read on the
//! server's threads) and turns it into session actions here, on the
//! session's thread — the way control surfaces' inputs go: faders, sends
//! and pans are gestures (`touch` on to off, or 400 ms of rest, is one
//! undo step "Web Remote"), buttons set what they show. Ten times a
//! second the state is built ([`faderframe_remote::State`]) and each
//! client gets what changed for it as its role sees it (a performer only
//! their cue mix); meters go out a dozen times a second to screens that
//! show them. Inputs a device's role does not allow are refused here
//! (`RemoteInput::allowed`), whatever the app shows.
//!
//! While it answers the network, this computer's firewall is read every
//! few seconds (`faderframe_remote::firewall`): one in the way is told
//! when the remote starts and shown in the status with the command that
//! lets devices on the local network in, until it does.

use crate::{Action, NoticeLevel, Session, TransportAction};
use faderframe_control::{SceneLight, SlotKind};
use faderframe_core::{CueId, FaderLaw, MarkerId, MixSceneId, SceneId, SectionId, SendId, TrackId};
use faderframe_project::cue::feeds_cues;
use faderframe_project::{Command, Track, TrackKind};
use faderframe_remote::firewall::{self, FirewallNote};
use faderframe_remote::protocol::{
    self, ColumnView, CueSendView, CueView, LauncherView, LyricView, MacroView, MarkerView,
    MixSceneView, SceneStep, SceneView, SectionView, SendView, SetlistView, ShowControl, SlotView,
    SongView, Step, TrackView, TransportButton, TransportView,
};
use faderframe_remote::{
    ClientId, Event, PairedDevice, RemoteInput, Role, Screen, Server, ServerConfig, State,
};
use faderframe_timeline::MusicalTime;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr};
use std::path::PathBuf;
use std::time::{Duration, Instant};

/// The port the remote listens on unless set otherwise.
pub const DEFAULT_PORT: u16 = 8642;
/// How often clients get what changed.
const STATE_EVERY: Duration = Duration::from_millis(100);
/// How often screens with meters get them.
const METERS_EVERY: Duration = Duration::from_millis(80);
/// A fader at rest this long ends its gesture (as a surface's).
const QUIET: Duration = Duration::from_millis(400);
/// The undo step's name.
const GESTURE: &str = "Web Remote";
/// How often the firewall's settings are read again.
const FIREWALL_EVERY: Duration = Duration::from_secs(3);

/// Who may connect.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RemoteListen {
    /// Devices on the local network (and this computer).
    #[default]
    Network,
    /// This computer only (a browser beside FaderFrame).
    ThisComputer,
}

/// The remote as set up (kept in the preferences).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct WebRemoteSettings {
    /// Serving (off by default).
    pub on: bool,
    /// The TCP port (0: any free one).
    pub port: u16,
    pub listen: RemoteListen,
    /// Answer devices outside the local network's address ranges too (a
    /// VPN's): only with care — there is no encryption.
    pub any_network: bool,
    /// The devices that paired (their tokens' hashes and roles).
    pub devices: Vec<PairedDevice>,
}

impl Default for WebRemoteSettings {
    fn default() -> Self {
        Self {
            on: false,
            port: DEFAULT_PORT,
            listen: RemoteListen::Network,
            any_network: false,
            devices: Vec::new(),
        }
    }
}

impl WebRemoteSettings {
    /// What the server needs restarting for.
    fn server_config(&self) -> ServerConfig {
        ServerConfig {
            bind: match self.listen {
                RemoteListen::Network => IpAddr::V4(Ipv4Addr::UNSPECIFIED),
                RemoteListen::ThisComputer => IpAddr::V4(Ipv4Addr::LOCALHOST),
            },
            port: self.port,
            any_network: self.any_network,
        }
    }
}

/// Changes to the remote from Preferences (not undoable).
#[derive(Clone, Debug, PartialEq)]
pub enum RemoteOp {
    /// On/off, the port, who may connect, the devices (renamed, roles
    /// changed, forgotten).
    Settings(WebRemoteSettings),
    /// A new pairing code, for this role.
    NewCode(Role),
    /// Forget a paired device (it is disconnected).
    Forget(u64),
    SetRole {
        device: u64,
        role: Role,
    },
    Rename {
        device: u64,
        name: String,
    },
}

/// A connected device.
#[derive(Clone, Debug, PartialEq)]
pub struct RemoteClient {
    pub device: u64,
    pub name: String,
    pub role: Role,
    /// Its address.
    pub address: String,
}

/// What Preferences shows.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct WebRemoteStatus {
    pub running: bool,
    /// The port listened on.
    pub port: Option<u16>,
    /// Where devices open the remote.
    pub url: Option<String>,
    /// The same with the pairing code (the QR code's).
    pub pair_url: Option<String>,
    /// The code offered now (`None`: withdrawn after wrong ones).
    pub code: Option<String>,
    pub code_role: Role,
    /// Wrong codes tried since this one was drawn.
    pub wrong_codes: u32,
    /// Why it is not running.
    pub error: Option<String>,
    /// A firewall in the way of devices on the network (and the command
    /// that lets them in).
    pub firewall: Option<FirewallNote>,
    pub connected: Vec<RemoteClient>,
}

struct Client {
    device: u64,
    /// The state as last sent (`None`: send it whole).
    last: Option<Value>,
    screen: Screen,
    /// Faders it holds.
    held: u32,
}

/// The remote's session state.
#[derive(Default)]
pub(crate) struct WebRemoteState {
    settings: WebRemoteSettings,
    server: Option<Server>,
    error: Option<String>,
    clients: HashMap<ClientId, Client>,
    /// A gesture opened from the remote, and when it last moved.
    gesture: Option<Instant>,
    held: u32,
    last_state: Option<Instant>,
    last_meters: Option<Instant>,
    /// The address phones are told (looked up when it starts).
    lan: Option<Ipv4Addr>,
    /// The firewall in the way, and when it was last read.
    firewall: Option<FirewallNote>,
    firewall_read: Option<Instant>,
    /// The warning was given since the remote was turned on (restarts for
    /// a new port do not give it again).
    firewall_told: bool,
    /// Where the firewall's settings are read (tests; the system's
    /// otherwise).
    firewall_root: Option<PathBuf>,
}

fn rgb(c: faderframe_project::TrackColor) -> String {
    format!("#{:02x}{:02x}{:02x}", c.r, c.g, c.b)
}

fn level_text(db: f32) -> String {
    if db <= -100.0 || !db.is_finite() {
        "-inf".into()
    } else {
        format!("{db:.1}")
    }
}

/// Rounded for sending (4 decimals: no diff for noise).
fn r4(v: f32) -> f32 {
    if v.is_finite() {
        (v * 10_000.0).round() / 10_000.0
    } else {
        0.0
    }
}

fn clock(seconds: f64) -> String {
    let tenths = (seconds.max(0.0) * 10.0).round() as u64;
    format!("{}:{:02}.{}", tenths / 600, tenths % 600 / 10, tenths % 10)
}

fn kind_name(kind: TrackKind) -> &'static str {
    match kind {
        TrackKind::Audio => "audio",
        TrackKind::Instrument => "instrument",
        TrackKind::Midi => "midi",
        TrackKind::Bus => "bus",
        TrackKind::Aux => "aux",
        TrackKind::Master => "master",
        TrackKind::Vca => "vca",
        TrackKind::Folder => "folder",
    }
}

/// A mixer strip the remote shows.
fn has_strip(t: &Track) -> bool {
    matches!(
        t.kind,
        TrackKind::Audio
            | TrackKind::Instrument
            | TrackKind::Bus
            | TrackKind::Aux
            | TrackKind::Vca
            | TrackKind::Master
    )
}

impl Session {
    // --- reading -------------------------------------------------------

    pub fn web_remote_settings(&self) -> &WebRemoteSettings {
        &self.web_remote.settings
    }

    /// Whether it runs, where, the code, who is connected.
    pub fn web_remote_status(&self) -> WebRemoteStatus {
        let r = &self.web_remote;
        let Some(server) = &r.server else {
            return WebRemoteStatus {
                error: r.error.clone(),
                ..WebRemoteStatus::default()
            };
        };
        let port = server.local_addr().port();
        let host = match r.settings.listen {
            RemoteListen::ThisComputer => Some(Ipv4Addr::LOCALHOST),
            RemoteListen::Network => r.lan,
        };
        let url = host.map(|ip| format!("http://{ip}:{port}/"));
        let code = server.code();
        let pair_url = url
            .as_ref()
            .zip(code.as_ref())
            .map(|(u, c)| format!("{u}#pair={c}"));
        let connected = server
            .clients()
            .into_iter()
            .filter_map(|c| {
                let d = r.settings.devices.iter().find(|d| Some(d.id) == c.device)?;
                Some(RemoteClient {
                    device: d.id,
                    name: d.name.clone(),
                    role: d.role.clone(),
                    address: c.peer.ip().to_string(),
                })
            })
            .collect();
        WebRemoteStatus {
            running: true,
            port: Some(port),
            url,
            pair_url,
            code,
            code_role: server.code_role(),
            wrong_codes: server.wrong_codes(),
            error: None,
            firewall: r.firewall.clone(),
            connected,
        }
    }

    /// The address the remote listens on (tests).
    pub fn web_remote_address(&self) -> Option<std::net::SocketAddr> {
        self.web_remote.server.as_ref().map(Server::local_addr)
    }

    // --- setting up ----------------------------------------------------

    /// Set the remote up: started, stopped or restarted as the settings
    /// say; the devices given to the running server (a forgotten one is
    /// disconnected, one whose role changed sees its new screens).
    pub fn set_web_remote(&mut self, settings: WebRemoteSettings) {
        let old = std::mem::replace(&mut self.web_remote.settings, settings.clone());
        let restart = old.on != settings.on
            || old.server_config() != settings.server_config()
            || (settings.on && self.web_remote.server.is_none());
        if !settings.on {
            self.web_remote.firewall_told = false;
        }
        if restart {
            self.stop_web_remote();
            if settings.on {
                self.start_web_remote();
            }
        } else if let Some(server) = &self.web_remote.server {
            server.set_devices(settings.devices.clone());
            // Roles changed: the screens and state anew.
            let changed: Vec<u64> = settings
                .devices
                .iter()
                .filter(|d| {
                    old.devices
                        .iter()
                        .any(|o| o.id == d.id && (o.role != d.role || o.name != d.name))
                })
                .map(|d| d.id)
                .collect();
            for (id, c) in &mut self.web_remote.clients {
                if let Some(d) = settings.devices.iter().find(|d| d.id == c.device)
                    && changed.contains(&d.id)
                {
                    c.last = None;
                    server.send(
                        *id,
                        protocol::message("welcome", json!({ "name": d.name, "role": d.role })),
                    );
                }
            }
        }
        self.revision += 1;
    }

    fn start_web_remote(&mut self) {
        let s = &self.web_remote.settings;
        let role = Role::Full;
        match Server::start(s.server_config(), s.devices.clone(), role) {
            Ok(server) => {
                tracing::info!("remote: listening on {}", server.local_addr());
                self.web_remote.lan = match s.listen {
                    RemoteListen::Network => faderframe_remote::net::lan_address(),
                    RemoteListen::ThisComputer => Some(Ipv4Addr::LOCALHOST),
                };
                self.web_remote.server = Some(server);
                self.web_remote.error = None;
                self.read_firewall();
                if let Some(note) = self
                    .web_remote
                    .firewall
                    .as_ref()
                    .filter(|_| !self.web_remote.firewall_told)
                {
                    let text = format!(
                        "The firewall ({}) {} the remote's port {}: phones and tablets {}. \
                         Preferences → Remote has the command that lets them in.",
                        note.firewall.name(),
                        if note.certain { "blocks" } else { "may block" },
                        note.port,
                        if note.certain {
                            "cannot open it"
                        } else {
                            "may not reach it"
                        },
                    );
                    self.web_remote.firewall_told = true;
                    self.notify(NoticeLevel::Warning, text);
                }
            }
            Err(e) => {
                let why = if e.kind() == std::io::ErrorKind::AddrInUse {
                    format!("port {} is taken by another program", s.port)
                } else {
                    format!("port {}: {e}", s.port)
                };
                tracing::warn!("remote: {why}");
                self.web_remote.error = Some(why);
            }
        }
    }

    fn stop_web_remote(&mut self) {
        self.end_remote_gesture();
        self.web_remote.held = 0;
        self.web_remote.clients.clear();
        self.web_remote.server = None;
        self.web_remote.error = None;
        self.web_remote.firewall = None;
        self.web_remote.firewall_read = None;
    }

    /// Read where the firewall's settings are under `root` from now on
    /// (tests; `None`: the system's), and read them.
    pub fn set_firewall_root(&mut self, root: Option<PathBuf>) {
        self.web_remote.firewall_root = root;
        if self.read_firewall() {
            self.revision += 1;
        }
    }

    /// Read whether a firewall is in the way of devices on the network
    /// (nothing to read while the remote is off or answers this computer
    /// only): `true` when that changed.
    fn read_firewall(&mut self) -> bool {
        let r = &mut self.web_remote;
        r.firewall_read = Some(Instant::now());
        let port = r
            .server
            .as_ref()
            .filter(|_| r.settings.listen == RemoteListen::Network)
            .map(|s| s.local_addr().port());
        let note = port.and_then(|port| match &r.firewall_root {
            Some(root) => firewall::check_in(root, port, r.lan),
            None => firewall::check(port, r.lan),
        });
        if note == r.firewall {
            return false;
        }
        let was = std::mem::replace(&mut r.firewall, note);
        match (&was, &r.firewall) {
            (_, Some(n)) => tracing::warn!("remote: {} {}", n.text(), n.command),
            (Some(n), None) if port.is_some() => {
                let text = format!(
                    "The firewall ({}) lets phones and tablets reach the remote now.",
                    n.firewall.name()
                );
                self.notify(NoticeLevel::Info, text);
            }
            _ => {}
        }
        true
    }

    pub(crate) fn web_remote_op(&mut self, op: RemoteOp) -> crate::Result<()> {
        let mut settings = self.web_remote.settings.clone();
        match op {
            RemoteOp::Settings(s) => settings = s,
            RemoteOp::NewCode(role) => {
                if let Some(server) = &self.web_remote.server {
                    server.new_code(role);
                }
                self.revision += 1;
                return Ok(());
            }
            RemoteOp::Forget(device) => settings.devices.retain(|d| d.id != device),
            RemoteOp::SetRole { device, role } => {
                if let Some(d) = settings.devices.iter_mut().find(|d| d.id == device) {
                    d.role = role;
                }
            }
            RemoteOp::Rename { device, name } => {
                if let Some(d) = settings.devices.iter_mut().find(|d| d.id == device) {
                    d.name = faderframe_remote::pairing::clean_name(&name);
                }
            }
        }
        self.set_web_remote(settings);
        Ok(())
    }

    // --- every tick ----------------------------------------------------

    pub(crate) fn tick_web_remote(&mut self) {
        if self.web_remote.server.is_some()
            && self
                .web_remote
                .firewall_read
                .is_none_or(|t| t.elapsed() >= FIREWALL_EVERY)
            && self.read_firewall()
        {
            self.revision += 1;
        }
        let Some(server) = &self.web_remote.server else {
            return;
        };
        let events = server.poll();
        for e in events {
            match e {
                Event::Paired { device } => {
                    if !self
                        .web_remote
                        .settings
                        .devices
                        .iter()
                        .any(|d| d.id == device.id)
                    {
                        self.notify(
                            NoticeLevel::Info,
                            format!(
                                "{} paired with the remote ({})",
                                device.name,
                                device.role.label()
                            ),
                        );
                        self.web_remote.settings.devices.push(device);
                        self.revision += 1;
                    }
                }
                Event::Joined { client, device } => {
                    self.web_remote.clients.insert(
                        client,
                        Client {
                            device,
                            last: None,
                            screen: Screen::default(),
                            held: 0,
                        },
                    );
                    // The state at once, not at the next round.
                    self.web_remote.last_state = None;
                    self.revision += 1;
                }
                Event::Left { client } => {
                    if let Some(c) = self.web_remote.clients.remove(&client) {
                        self.release_remote_touches(c.held);
                    }
                    self.revision += 1;
                }
                Event::View { client, screen } => {
                    if let Some(c) = self.web_remote.clients.get_mut(&client) {
                        c.screen = screen;
                    }
                }
                Event::Input { client, input } => {
                    if let Err(e) = self.remote_input(client, input) {
                        self.tell_client(client, &e.to_string());
                    }
                }
            }
        }
        // A gesture at rest ends.
        if self.web_remote.held == 0
            && self
                .web_remote
                .gesture
                .is_some_and(|t| t.elapsed() >= QUIET)
        {
            self.end_remote_gesture();
        }
        if self.web_remote.clients.is_empty() {
            return;
        }
        let now = Instant::now();
        if self
            .web_remote
            .last_state
            .is_none_or(|t| now - t >= STATE_EVERY)
        {
            self.web_remote.last_state = Some(now);
            self.send_remote_state();
        }
        if self
            .web_remote
            .last_meters
            .is_none_or(|t| now - t >= METERS_EVERY)
        {
            self.web_remote.last_meters = Some(now);
            self.send_remote_meters();
        }
    }

    fn tell_client(&self, client: ClientId, text: &str) {
        if let Some(server) = &self.web_remote.server {
            server.send(client, protocol::message("notice", json!({ "text": text })));
        }
    }

    /// The role of a client's device.
    fn client_role(&self, client: ClientId) -> Role {
        let device = self.web_remote.clients.get(&client).map(|c| c.device);
        self.web_remote
            .settings
            .devices
            .iter()
            .find(|d| Some(d.id) == device)
            .map_or(Role::Viewer, |d| d.role.clone())
    }

    /// The cue mix a performer's role names (by name, without case).
    fn role_cue(&self, role: &Role) -> Option<CueId> {
        let Role::Performer { cue } = role else {
            return None;
        };
        self.project
            .cue_mixes
            .iter()
            .find(|c| c.name.trim().eq_ignore_ascii_case(cue.trim()))
            .map(|c| c.id)
    }

    fn send_remote_state(&mut self) {
        let state = self.remote_state();
        let roles: Vec<(ClientId, Role)> = self
            .web_remote
            .clients
            .keys()
            .map(|id| (*id, self.client_role(*id)))
            .collect();
        let cues: Vec<Option<u64>> = roles
            .iter()
            .map(|(_, r)| self.role_cue(r).map(|c| c.0))
            .collect();
        let Some(server) = &self.web_remote.server else {
            return;
        };
        for ((id, role), cue) in roles.iter().zip(cues) {
            let Some(c) = self.web_remote.clients.get_mut(id) else {
                continue;
            };
            let now = state.for_role(role, cue);
            let text = match &c.last {
                None => protocol::message("state", json!({ "state": now })),
                Some(last) => {
                    let ops = faderframe_remote::diff::diff(last, &now);
                    if ops.is_empty() {
                        continue;
                    }
                    protocol::message("patch", json!({ "ops": ops }))
                }
            };
            // Dropped (its queue is full): the whole state next time.
            c.last = server.send(*id, text).then_some(now);
        }
    }

    fn send_remote_meters(&mut self) {
        let wanted: Vec<(ClientId, Role)> = self
            .web_remote
            .clients
            .iter()
            .filter(|(_, c)| c.screen.metered())
            .map(|(id, _)| (*id, self.client_role(*id)))
            .collect();
        if wanted.is_empty() {
            return;
        }
        let level = |m: &crate::meters::MeterDisplay| (m.left.level_db, m.right.level_db);
        let strips: Vec<(u64, f32, f32)> = self
            .project
            .tracks
            .iter()
            .filter(|t| has_strip(t) && t.kind != TrackKind::Vca)
            .map(|t| {
                let (l, r) = level(&self.meter(t.id));
                (t.id.0, l, r)
            })
            .collect();
        let cues: Vec<(u64, f32, f32)> = self
            .project
            .cue_mixes
            .iter()
            .map(|c| {
                let (l, r) = level(&self.meter(c.channel()));
                (c.id.0, l, r)
            })
            .collect();
        let Some(server) = &self.web_remote.server else {
            return;
        };
        for (id, role) in wanted {
            let text = match &role {
                Role::Full => {
                    let all: Vec<(u64, f32, f32)> =
                        strips.iter().chain(cues.iter()).copied().collect();
                    protocol::meters_message(&all)
                }
                Role::Performer { .. } => {
                    let Some(cue) = self.role_cue(&role) else {
                        continue;
                    };
                    let mine: Vec<(u64, f32, f32)> =
                        cues.iter().filter(|c| c.0 == cue.0).copied().collect();
                    protocol::meters_message(&mine)
                }
                Role::Viewer => continue,
            };
            server.send(id, text);
        }
    }

    /// Seconds from the project's start to `t`.
    fn remote_seconds(&self, t: MusicalTime) -> f64 {
        const RATE: f64 = 1_000_000.0;
        self.project.timeline.to_samples(t, RATE) as f64 / RATE
    }

    fn remote_bars(&self, t: MusicalTime) -> String {
        let bbt = self.project.timeline.meter.to_bbt(t);
        let display = bbt.tick * 960 / faderframe_timeline::TICKS_PER_QUARTER;
        format!("{}.{}.{}", bbt.bar + 1, bbt.beat + 1, display / 240 + 1)
    }

    /// Everything the remote shows, as the engineer sees it.
    pub fn web_remote_state(&self) -> State {
        self.remote_state()
    }

    fn remote_state(&self) -> State {
        let law = FaderLaw::console();
        let travel = |db: f32| r4(law.db_to_position(db));
        let p = &self.project;
        let at = self.playhead();
        let seconds = self.remote_seconds(at);
        let sig = p.timeline.meter.signature_at(at);
        let transport = TransportView {
            playing: self.transport.playing,
            recording: self.transport.recording,
            looping: p.loop_enabled,
            click: self.record.metronome != faderframe_engine::MetronomeMode::Off,
            seconds: (seconds * 1000.0).round() / 1000.0,
            bars: self.remote_bars(at),
            time: clock(seconds),
            bpm: (p.timeline.tempo.bpm_at(at) * 100.0).round() / 100.0,
            signature: format!("{}/{}", sig.numerator, sig.denominator),
            countdown: self
                .show_countdown()
                .map(|s| (f64::from(s) * 10.0).round() / 10.0),
        };
        let shown: Vec<&Track> = p
            .folder_order()
            .into_iter()
            .filter(|t| has_strip(t))
            .collect();
        let mut tracks = std::collections::BTreeMap::new();
        let mut order = Vec::new();
        let mut master = None;
        for t in &shown {
            let db = self.shown_volume_db(t);
            let sends = t
                .sends
                .iter()
                .map(|s| {
                    let db = self.shown_send_db(t, s);
                    SendView {
                        id: s.id.0,
                        name: p
                            .track(s.target)
                            .map_or_else(|| "Send".to_string(), |d| d.name.clone()),
                        travel: travel(db),
                        level: level_text(db),
                        on: s.enabled,
                    }
                })
                .collect();
            let pan = if p.dual_panned(t) {
                let d = self.shown_dual_pan(t);
                0.5 * (d.left + d.right)
            } else {
                self.shown_pan(t)
            };
            tracks.insert(
                t.id.0.to_string(),
                TrackView {
                    id: t.id.0,
                    name: t.name.clone(),
                    kind: kind_name(t.kind).into(),
                    color: rgb(t.color),
                    fader: travel(db),
                    level: level_text(db),
                    pan: r4(pan),
                    pans: t.kind != TrackKind::Vca,
                    mute: self.shown_mute(t),
                    solo: t.solo,
                    arm: t.record_arm,
                    armable: matches!(t.kind, TrackKind::Audio | TrackKind::Instrument),
                    sends,
                },
            );
            if t.kind == TrackKind::Master {
                master = Some(t.id.0);
            } else {
                order.push(t.id.0);
            }
        }
        let cue_tracks: Vec<&&Track> = shown.iter().filter(|t| feeds_cues(t.kind)).collect();
        let cues = p
            .cue_mixes
            .iter()
            .map(|c| {
                let db = self.shown_cue_level_db(c.id);
                CueView {
                    id: c.id.0,
                    name: c.name.clone(),
                    color: rgb(c.color),
                    travel: travel(db),
                    level: level_text(db),
                    mute: c.mute,
                    sends: cue_tracks
                        .iter()
                        .map(|t| {
                            let s = self.shown_cue_send(t, c.id);
                            (
                                t.id.0.to_string(),
                                CueSendView {
                                    travel: travel(s.level_db),
                                    level: level_text(s.level_db),
                                    pan: r4(s.pan),
                                    on: s.on,
                                },
                            )
                        })
                        .collect(),
                }
            })
            .collect();
        let mut markers: Vec<&faderframe_project::Marker> = p.markers.iter().collect();
        markers.sort_by_key(|m| m.position);
        let markers = markers
            .into_iter()
            .map(|m| MarkerView {
                id: m.id.0,
                name: m.name.clone(),
                seconds: self.remote_seconds(m.position),
                bars: self.remote_bars(m.position),
            })
            .collect();
        let mut sections: Vec<&faderframe_project::Section> = p.sections.iter().collect();
        sections.sort_by_key(|s| s.start);
        let sections = sections
            .into_iter()
            .map(|s| SectionView {
                id: s.id.0,
                name: s.name.clone(),
                start: self.remote_seconds(s.start),
                end: self.remote_seconds(s.end),
                color: rgb(s.color),
            })
            .collect();
        let setlist = SetlistView {
            songs: p
                .setlist
                .songs
                .iter()
                .map(|s| SongView {
                    name: s.name.clone(),
                    seconds: (self.song_seconds(s) * 10.0).round() / 10.0,
                    start: self.remote_seconds(s.start),
                    then: s.then.label(),
                    notes: s.notes.clone(),
                })
                .collect(),
            show: self.show.on,
            current: self.show.on.then_some(self.show.current),
        };
        let lyrics = p
            .lyrics
            .iter()
            .map(|l| LyricView {
                start: self.remote_seconds(l.start),
                end: self.remote_seconds(l.end),
                text: l.text.clone(),
            })
            .collect();
        let current_scene = self.current_mix_scene();
        let mut macros: Vec<MacroView> = self
            .script_library()
            .iter()
            .filter_map(|e| {
                e.number.map(|number| MacroView {
                    number,
                    name: e.name.clone(),
                })
            })
            .collect();
        macros.sort_by_key(|m| m.number);
        macros.dedup_by_key(|m| m.number);
        State {
            project: p.name.clone(),
            role: Default::default(),
            transport,
            unity: r4(law.unity_position()),
            tracks,
            order,
            master,
            cues,
            cue_order: cue_tracks.iter().map(|t| t.id.0).collect(),
            markers,
            sections,
            setlist,
            lyrics,
            launcher: self.remote_launcher(),
            mix_scenes: p
                .mix_scenes
                .scenes
                .iter()
                .map(|s| MixSceneView {
                    id: s.id.0,
                    name: s.name.clone(),
                    current: current_scene == Some(s.id),
                })
                .collect(),
            macros,
        }
    }

    /// The launcher as the surfaces show it: every track that holds clips
    /// by every scene.
    fn remote_launcher(&self) -> LauncherView {
        let tracks: Vec<(TrackId, String, String)> = self
            .launcher_tracks()
            .iter()
            .map(|t| (t.id, t.name.clone(), rgb(t.color)))
            .collect();
        let scenes = &self.project.launcher.scenes;
        if tracks.is_empty() || scenes.is_empty() {
            return LauncherView::default();
        }
        let ids: Vec<Option<TrackId>> = tracks.iter().map(|t| Some(t.0)).collect();
        let v = self.surface_launcher(&ids, 0, scenes.len());
        let hex = |c: [u8; 3]| format!("#{:02x}{:02x}{:02x}", c[0], c[1], c[2]);
        LauncherView {
            scenes: scenes
                .iter()
                .zip(&v.scenes)
                .map(|(s, light)| SceneView {
                    id: s.id.0,
                    name: s.name.clone(),
                    light: match light {
                        SceneLight::Off => "off",
                        SceneLight::Clips => "clips",
                        SceneLight::Queued => "queued",
                        SceneLight::Playing => "playing",
                    }
                    .into(),
                })
                .collect(),
            columns: tracks
                .iter()
                .enumerate()
                .map(|(i, (id, name, color))| ColumnView {
                    track: id.0,
                    name: name.clone(),
                    color: color.clone(),
                    playing: v.playing.get(i).copied().unwrap_or(false),
                    slots: v
                        .slots
                        .get(i)
                        .map(|row| {
                            row.iter()
                                .map(|s| SlotView {
                                    kind: match s.kind {
                                        SlotKind::Empty => "empty",
                                        SlotKind::Armed => "armed",
                                        SlotKind::Clip => "clip",
                                        SlotKind::Queued => "queued",
                                        SlotKind::Playing => "playing",
                                        SlotKind::Stopping => "stopping",
                                        SlotKind::Recording => "recording",
                                    }
                                    .into(),
                                    name: s.name.clone(),
                                    color: hex(s.color),
                                })
                                .collect()
                        })
                        .unwrap_or_default(),
                })
                .collect(),
        }
    }

    // --- what devices do -----------------------------------------------

    fn begin_remote_gesture(&mut self) -> crate::Result<()> {
        if self.web_remote.gesture.is_none() {
            self.dispatch(Action::BeginGesture(GESTURE.into()))?;
        }
        self.web_remote.gesture = Some(Instant::now());
        Ok(())
    }

    fn end_remote_gesture(&mut self) {
        if self.web_remote.gesture.take().is_some() {
            let _ = self.dispatch(Action::EndGesture);
        }
    }

    /// A client went holding faders: let go of them.
    fn release_remote_touches(&mut self, held: u32) {
        self.web_remote.held = self.web_remote.held.saturating_sub(held);
        if self.web_remote.held == 0 {
            self.end_remote_gesture();
        }
    }

    /// A track the remote may move (a mixer strip).
    fn remote_track(&self, id: u64) -> Option<&Track> {
        self.project.track(TrackId(id)).filter(|t| has_strip(t))
    }

    /// An edit from the remote, as a user's (groups and selected tracks
    /// follow it, automation writes it).
    fn remote_edit(&mut self, cmd: Command) -> crate::Result<()> {
        self.dispatch(Action::Edit(cmd))
    }

    fn remote_input(&mut self, client: ClientId, input: RemoteInput) -> crate::Result<()> {
        let role = self.client_role(client);
        let cue = self.role_cue(&role).map(|c| c.0);
        if !input.allowed(&role, cue) {
            self.tell_client(client, "This device may not do that.");
            return Ok(());
        }
        let law = FaderLaw::console();
        let unit = |v: f32| v.is_finite().then(|| v.clamp(0.0, 1.0));
        let pan = |v: f32| v.is_finite().then(|| v.clamp(-1.0, 1.0));
        match input {
            RemoteInput::Transport { button } => self.remote_transport(button)?,
            RemoteInput::LocateMarker { id } => {
                if let Some(m) = self.project.markers.iter().find(|m| m.id == MarkerId(id)) {
                    let at = m.position;
                    self.dispatch(Action::Transport(TransportAction::Locate(at)))?;
                }
            }
            RemoteInput::LocateSection { id } => {
                if let Some(s) = self.project.sections.iter().find(|s| s.id == SectionId(id)) {
                    let at = s.start;
                    self.dispatch(Action::Transport(TransportAction::Locate(at)))?;
                }
            }
            RemoteInput::Step { step } => {
                let at = self.playhead();
                let (mut times, forward): (Vec<MusicalTime>, bool) = match step {
                    Step::MarkerNext | Step::MarkerPrevious => (
                        self.project.markers.iter().map(|m| m.position).collect(),
                        step == Step::MarkerNext,
                    ),
                    Step::SectionNext | Step::SectionPrevious => (
                        self.project.sections.iter().map(|s| s.start).collect(),
                        step == Step::SectionNext,
                    ),
                };
                times.sort();
                self.locate_step(at, &times, forward)?;
            }
            RemoteInput::Touch { on } => {
                if on {
                    if let Some(c) = self.web_remote.clients.get_mut(&client) {
                        c.held += 1;
                        self.web_remote.held += 1;
                    }
                    self.begin_remote_gesture()?;
                } else {
                    let had = self.web_remote.clients.get_mut(&client).is_some_and(|c| {
                        let had = c.held > 0;
                        c.held = c.held.saturating_sub(1);
                        had
                    });
                    if had {
                        self.release_remote_touches(1);
                    }
                }
            }
            RemoteInput::Fader { track, travel } => {
                let (Some(t), Some(v)) = (self.remote_track(track), unit(travel)) else {
                    return Ok(());
                };
                let track = t.id;
                self.begin_remote_gesture()?;
                self.remote_edit(Command::SetTrackVolume {
                    track,
                    db: law.position_to_db(v),
                })?;
            }
            RemoteInput::Pan { track, pan: p } => {
                let (Some(t), Some(v)) = (self.remote_track(track), pan(p)) else {
                    return Ok(());
                };
                if t.kind == TrackKind::Vca {
                    return Ok(());
                }
                let (track, dual) = (t.id, self.project.dual_panned(t));
                self.begin_remote_gesture()?;
                if dual {
                    self.set_dual_pan_from_surface(track, v)?;
                } else {
                    self.remote_edit(Command::SetTrackPan { track, pan: v })?;
                }
            }
            RemoteInput::Mute { track, on } => {
                if let Some(t) = self.remote_track(track) {
                    let track = t.id;
                    self.remote_edit(Command::SetTrackMute { track, on })?;
                }
            }
            RemoteInput::Solo { track, on } => {
                if let Some(t) = self
                    .remote_track(track)
                    .filter(|t| t.kind != TrackKind::Master)
                {
                    let track = t.id;
                    self.remote_edit(Command::SetTrackSolo { track, on })?;
                }
            }
            RemoteInput::Arm { track, on } => {
                if let Some(t) = self
                    .remote_track(track)
                    .filter(|t| matches!(t.kind, TrackKind::Audio | TrackKind::Instrument))
                {
                    let track = t.id;
                    self.remote_edit(Command::SetTrackRecordArm { track, on })?;
                }
            }
            RemoteInput::Send {
                track,
                send,
                travel,
            } => {
                let Some(v) = unit(travel) else {
                    return Ok(());
                };
                let Some(t) = self
                    .remote_track(track)
                    .filter(|t| t.sends.iter().any(|s| s.id == SendId(send)))
                else {
                    return Ok(());
                };
                let track = t.id;
                self.begin_remote_gesture()?;
                self.remote_edit(Command::SetSendLevel {
                    track,
                    send: SendId(send),
                    db: law.position_to_db(v),
                })?;
            }
            RemoteInput::SendOn { track, send, on } => {
                if let Some(t) = self
                    .remote_track(track)
                    .filter(|t| t.sends.iter().any(|s| s.id == SendId(send)))
                {
                    let track = t.id;
                    self.remote_edit(Command::SetSendEnabled {
                        track,
                        send: SendId(send),
                        enabled: on,
                    })?;
                }
            }
            RemoteInput::CueLevel { cue, travel } => {
                let (Some(cue), Some(v)) = (self.remote_cue(cue), unit(travel)) else {
                    return Ok(());
                };
                self.begin_remote_gesture()?;
                self.remote_edit(Command::SetCueLevel {
                    cue,
                    db: law.position_to_db(v),
                })?;
            }
            RemoteInput::CueMute { cue, on } => {
                if let Some(cue) = self.remote_cue(cue) {
                    self.remote_edit(Command::SetCueMute { cue, on })?;
                }
            }
            RemoteInput::CueSend { cue, track, travel } => {
                let (Some((cue, track)), Some(v)) =
                    (self.remote_cue_send(cue, track), unit(travel))
                else {
                    return Ok(());
                };
                self.begin_remote_gesture()?;
                self.remote_edit(Command::SetCueSendLevel {
                    track,
                    cue,
                    db: law.position_to_db(v),
                })?;
            }
            RemoteInput::CuePan { cue, track, pan: p } => {
                let (Some((cue, track)), Some(v)) = (self.remote_cue_send(cue, track), pan(p))
                else {
                    return Ok(());
                };
                self.begin_remote_gesture()?;
                self.remote_edit(Command::SetCueSendPan { track, cue, pan: v })?;
            }
            RemoteInput::CueOn { cue, track, on } => {
                if let Some((cue, track)) = self.remote_cue_send(cue, track) {
                    self.remote_edit(Command::SetCueSendOn { track, cue, on })?;
                }
            }
            RemoteInput::Show { control } => {
                use crate::setlist::ShowOp;
                let op = match control {
                    ShowControl::Enter => ShowOp::Enter,
                    ShowControl::Leave => ShowOp::Leave,
                    ShowControl::Next => ShowOp::Next,
                    ShowControl::Previous => ShowOp::Previous,
                    ShowControl::PlayStop => ShowOp::PlayStop,
                };
                self.dispatch(Action::Show(op))?;
            }
            RemoteInput::ShowSong { song, go } => {
                use crate::setlist::ShowOp;
                if song < self.project.setlist.songs.len() {
                    if !self.show.on {
                        self.dispatch(Action::Show(ShowOp::Enter))?;
                    }
                    let op = if go {
                        ShowOp::Go(song)
                    } else {
                        ShowOp::Cue(song)
                    };
                    self.dispatch(Action::Show(op))?;
                }
            }
            RemoteInput::Launch { track, scene } | RemoteInput::Release { track, scene } => {
                use crate::launcher::LauncherOp;
                let (track, scene) = (TrackId(track), SceneId(scene));
                let known = self.project.track(track).is_some()
                    && self.project.launcher.scenes.iter().any(|s| s.id == scene);
                if known {
                    let op = match input {
                        RemoteInput::Launch { .. } => LauncherOp::Launch { track, scene },
                        _ => LauncherOp::Release { track, scene },
                    };
                    self.dispatch(Action::Launcher(op))?;
                }
            }
            RemoteInput::StopTrack { track } => {
                if self.project.track(TrackId(track)).is_some() {
                    self.dispatch(Action::Launcher(crate::launcher::LauncherOp::StopTrack(
                        TrackId(track),
                    )))?;
                }
            }
            RemoteInput::LaunchScene { scene } => {
                let scene = SceneId(scene);
                if self.project.launcher.scenes.iter().any(|s| s.id == scene) {
                    self.dispatch(Action::Launcher(crate::launcher::LauncherOp::LaunchScene(
                        scene,
                    )))?;
                }
            }
            RemoteInput::StopClips => {
                self.dispatch(Action::Launcher(crate::launcher::LauncherOp::StopAll))?;
            }
            RemoteInput::BackToArrangement => {
                self.dispatch(Action::Launcher(
                    crate::launcher::LauncherOp::BackToArrangement,
                ))?;
            }
            RemoteInput::MixScene { step } => {
                let op = match step {
                    SceneStep::Next => crate::scenes::SceneOp::Next,
                    SceneStep::Previous => crate::scenes::SceneOp::Previous,
                };
                self.dispatch(Action::MixScene(op))?;
            }
            RemoteInput::RecallScene { scene } => {
                let scene = MixSceneId(scene);
                if self.project.mix_scenes.get(scene).is_some() {
                    self.dispatch(Action::MixScene(crate::scenes::SceneOp::Recall {
                        scene,
                        fade: None,
                    }))?;
                }
            }
            RemoteInput::Macro { number } => {
                self.dispatch(Action::Script(crate::script::ScriptOp::RunMacro(number)))?;
            }
        }
        Ok(())
    }

    fn remote_cue(&self, cue: u64) -> Option<CueId> {
        self.project.cue(CueId(cue)).map(|c| c.id)
    }

    /// A cue mix and a track that feeds it.
    fn remote_cue_send(&self, cue: u64, track: u64) -> Option<(CueId, TrackId)> {
        let cue = self.remote_cue(cue)?;
        let t = self
            .project
            .track(TrackId(track))
            .filter(|t| feeds_cues(t.kind))?;
        Some((cue, t.id))
    }

    fn remote_transport(&mut self, button: TransportButton) -> crate::Result<()> {
        let t = |a| Action::Transport(a);
        match button {
            TransportButton::Play => self.dispatch(t(TransportAction::Play)),
            TransportButton::Stop => self.dispatch(t(TransportAction::Stop)),
            TransportButton::PlayStop => self.dispatch(t(TransportAction::TogglePlay)),
            TransportButton::Record => self.dispatch(t(TransportAction::ToggleRecord)),
            TransportButton::Loop => self.dispatch(t(TransportAction::ToggleLoop)),
            TransportButton::Start => self.dispatch(t(TransportAction::ReturnToStart)),
            TransportButton::End => {
                let end = self.project.content_end();
                self.dispatch(t(TransportAction::Locate(end)))
            }
            TransportButton::Rewind => self.dispatch(t(TransportAction::NudgeBars(-1))),
            TransportButton::Forward => self.dispatch(t(TransportAction::NudgeBars(1))),
            TransportButton::Click => {
                let mut r = self.record;
                r.metronome = match r.metronome {
                    faderframe_engine::MetronomeMode::Off => {
                        faderframe_engine::MetronomeMode::Always
                    }
                    _ => faderframe_engine::MetronomeMode::Off,
                };
                self.dispatch(Action::SetRecordSettings(r))
            }
            TransportButton::Undo | TransportButton::Redo => {
                // A drag still open ends first.
                self.end_remote_gesture();
                self.dispatch(if button == TransportButton::Undo {
                    Action::Undo
                } else {
                    Action::Redo
                })
            }
            TransportButton::AddMarker => {
                let at = self.playhead();
                self.dispatch(Action::AddMarker(at))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_keep_in_the_preferences_and_start_off() {
        let s = WebRemoteSettings::default();
        assert!(!s.on);
        assert_eq!(s.port, DEFAULT_PORT);
        let json = serde_json::to_string(&s).unwrap_or_default();
        let back: WebRemoteSettings = serde_json::from_str(&json).unwrap_or_default();
        assert_eq!(back, s);
        let partial: WebRemoteSettings =
            serde_json::from_str(r#"{"on":true,"listen":"this_computer"}"#).unwrap_or_default();
        assert!(partial.on);
        assert_eq!(partial.listen, RemoteListen::ThisComputer);
        assert_eq!(partial.port, DEFAULT_PORT);
        assert_eq!(
            partial.server_config().bind,
            IpAddr::V4(Ipv4Addr::LOCALHOST)
        );
    }

    #[test]
    fn times_and_levels_read_as_the_transport_shows_them() {
        assert_eq!(clock(83.44), "1:23.4");
        assert_eq!(clock(59.97), "1:00.0");
        assert_eq!(clock(5.0), "0:05.0");
        assert_eq!(level_text(-3.04), "-3.0");
        assert_eq!(level_text(-120.0), "-inf");
        assert_eq!(r4(0.123_456), 0.1235);
        assert_eq!(r4(f32::NAN), 0.0);
    }
}
