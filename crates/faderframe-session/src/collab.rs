//! Live collaboration: several people working in one project at once.
//!
//! **One order.** One session hosts and holds the project's authoritative
//! order of edits; peers join it (an address and the host's invite code,
//! over the encrypted links of `faderframe-collab`). Every change of a
//! project is a [`Command`] applied to it, and while a project is shared
//! the project's [journal](faderframe_project::journal) writes each one
//! down as applied — edits, undo and redo, auditions, a merge replacing it
//! whole. Each tick the session takes what its journal wrote as one step:
//! the host gives it the next place in its log and sends it to every peer
//! (an [`Entry`]); a peer keeps it as *pending* (already applied here: the
//! person sees their edit at once) and proposes it to the host, which
//! applies it where it falls in its order and sends it back as an entry
//! like any other — or refuses it, when it no longer applies (the clip
//! was removed meanwhile).
//!
//! **Rebasing.** A peer's project is the host's order so far plus its own
//! pending steps. An entry from someone else takes the pending steps back
//! (their inverses, newest first), applies the entry, and does the
//! pending steps again on top (their inverses made anew); one that no
//! longer applies goes, and the person is told — the host refuses it too,
//! for it sees the same project. Its own entry coming back confirms the
//! oldest pending step (commands alike: nothing to do). So every session
//! applies the same commands in the same order to the same project and
//! ends with the same project; the host sends the hash of its project's
//! canonical form when edits pause, and a peer whose own differs asks for
//! the whole project again.
//!
//! **Last writer wins.** Commands carry values, not differences: when two
//! people set the same thing, the one ordered later wins, and whoever had
//! set it in the half minute before is told who changed it since.
//!
//! **Each undoes their own.** Remote entries never enter the local undo
//! history: undo takes back the person's own steps (it applies their
//! inverses, which become a step of their own for everyone). What someone
//! else changed since stays, unless they changed the same thing — then the
//! undo sets it back to what it was before the person's own edit.
//!
//! **Ids.** Every session hands out ids from ranges set aside for it
//! alone ([`faderframe_core::IdLease`]): the host sets them aside as
//! entries of its log, so every copy's id counter moves alike, and asks
//! for more before a session runs out. A project replaced whole (a
//! merge, another project opened by the host) sets new ranges aside.
//!
//! **Media** cross by content (see [`form`]): a step names files by their
//! length and SHA-256; a session that lacks one fetches it in the
//! background (from the host; the host from the peer that brought it in)
//! into its media folder, resuming a part kept from a lost connection, and
//! plays it once it is whole. A peer without a plug-in keeps the slot and
//! its state (they cross with the project as they are) and is told; the
//! track plays without it, and Listen Along plays the host's mix.
//!
//! **Presence**: each session sends where its playhead is, what it has
//! selected and the clip open in an editor; the arranger shows the others'
//! in their colours. Chat lines go through the host to everyone.
//!
//! **Coming back.** A peer that loses its connection keeps working (its
//! steps wait) and tries again, saying who it was and how far it got; the
//! host sends the entries it missed (or the project, when its log no
//! longer reaches back) and which of its proposals it answered; the rest
//! are proposed again.

mod form;
pub mod listen;

pub use faderframe_collab::{DEFAULT_PORT, InviteCode, PeerId};

use crate::{NoticeLevel, Result, Session, SessionError};
use faderframe_collab::{
    Client, ClientEvent, ConnId, Entry, EntryBody, Fetched, Hello, Host, HostEvent, Participant,
    Resume, ToHost, ToPeer, Welcome,
};
use faderframe_core::{ClipId, IdLease, TrackId};
use faderframe_project::journal::Applied;
use faderframe_project::{Command, Impact, Project, TrackColor};
use faderframe_timeline::MusicalTime;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

/// Ids set aside for a session at a time.
pub const LEASE: u64 = 1 << 20;
/// A session asks for more ids when fewer than this are left.
const LEASE_LOW: u64 = 1 << 18;
/// Entries the host keeps for peers coming back (bytes; older ones go,
/// and a peer that missed them gets the whole project).
const LOG_BYTES: usize = 64 << 20;
/// A lost connection is tried again after this, then less often.
const RETRY: Duration = Duration::from_secs(2);
const RETRY_MAX: Duration = Duration::from_secs(10);
/// Presence is sent at most this often.
const PRESENCE_EVERY: Duration = Duration::from_millis(100);
/// The host sends its project's hash once edits paused this long.
const CHECKSUM_AFTER: Duration = Duration::from_millis(800);
/// A peer asks for the whole project at most this often.
const RESYNC_EVERY: Duration = Duration::from_secs(5);
/// Own edits this recent are told about when someone overrides them.
const OVERRIDE_WINDOW: Duration = Duration::from_secs(30);
/// Chat lines kept.
const CHAT_KEPT: usize = 500;

/// What collaboration is asked to do.
#[derive(Clone, Debug, PartialEq)]
pub enum CollabOp {
    /// Host the project under a new invite code, as `name`: listen on
    /// `bind` (`None`: every interface, port
    /// [`faderframe_collab::DEFAULT_PORT`] — any free one when that is
    /// taken).
    Host {
        bind: Option<std::net::SocketAddr>,
        name: String,
    },
    /// Join a host: `address` (`host`, `host:port`), its invite code.
    Join {
        address: String,
        code: String,
        name: String,
    },
    /// Leave (a peer) or end the session (the host); the project stays.
    Leave,
    /// A new invite code (the host): who joins needs it from now on.
    NewInvite,
    /// Say something to everyone.
    Chat(String),
    /// Hear the host's mix instead of this session's (peers; see
    /// [`listen`]).
    ListenAlong(bool),
    /// Ask the host for the whole project again (peers).
    Resync,
}

/// Where a participant is and what it has in hand.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Presence {
    pub playhead: MusicalTime,
    pub playing: bool,
    pub tracks: Vec<TrackId>,
    pub clips: Vec<ClipId>,
    /// The edit selection's range.
    pub range: Option<(MusicalTime, MusicalTime)>,
    /// The clip open in an editor (the piano roll, the pitch editor).
    pub editing: Option<ClipId>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CollabRole {
    Host,
    Peer,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Connection {
    Connecting,
    Connected,
    /// The connection went `seconds` ago; trying again.
    Lost {
        seconds: u64,
        reason: String,
    },
    /// The session ended (the host left, or this one was turned away).
    Ended {
        reason: String,
    },
}

/// The host's invitation: what to give the people invited.
#[derive(Clone, Debug, PartialEq)]
pub struct Invite {
    pub code: String,
    pub port: u16,
    /// This machine's addresses (`ip:port`), the local network's first.
    pub addresses: Vec<String>,
}

impl Invite {
    /// One line to send someone: `<address>/<code>`.
    pub fn text(&self) -> String {
        format!(
            "{}/{}",
            self.addresses.first().map_or("", String::as_str),
            self.code
        )
    }
}

/// Split an invitation line into address and code.
pub fn parse_invite(text: &str) -> Option<(String, String)> {
    let (a, c) = text.trim().rsplit_once('/')?;
    let (a, c) = (a.trim(), c.trim());
    (!a.is_empty() && InviteCode::parse(c).is_ok()).then(|| (a.to_string(), c.to_string()))
}

#[derive(Clone, Debug, PartialEq)]
pub struct Person {
    pub peer: PeerId,
    pub name: String,
    pub colour: TrackColor,
    pub host: bool,
    /// This session.
    pub me: bool,
    pub connected: bool,
    pub listening: bool,
    pub presence: Option<Presence>,
    /// What they did last.
    pub last_edit: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ChatLine {
    pub from: String,
    pub colour: TrackColor,
    pub text: String,
    /// Milliseconds since the Unix epoch.
    pub at: u64,
    pub mine: bool,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct MediaStatus {
    /// Files on their way here.
    pub waiting: usize,
    /// Files here being hashed before they are named to the others.
    pub hashing: usize,
    pub done: u64,
    pub total: u64,
    /// Files that could not be fetched (by name).
    pub failed: Vec<String>,
}

/// What the shell shows about collaboration.
#[derive(Clone, Debug, PartialEq)]
pub struct CollabStatus {
    pub role: CollabRole,
    pub connection: Connection,
    /// The host's address (peers) or `None`.
    pub host_address: Option<String>,
    pub invite: Option<Invite>,
    pub people: Vec<Person>,
    pub chat: Vec<ChatLine>,
    pub media: MediaStatus,
    /// Own steps not yet in the host's order.
    pub waiting: usize,
    /// Plug-ins the project uses that are not installed here.
    pub missing_plugins: Vec<String>,
    pub listening: bool,
    /// The host's log position (entries applied).
    pub seq: u64,
}

/// Someone else's presence, for the editors.
#[derive(Clone, Debug, PartialEq)]
pub struct RemotePresence {
    pub peer: PeerId,
    pub name: String,
    pub colour: TrackColor,
    pub presence: Presence,
}

/// An own step: applied here, proposed, not yet back from the host.
struct Pending {
    local: u64,
    label: String,
    applied: Vec<Applied>,
    /// Proposed on the current connection.
    sent: bool,
}

impl Pending {
    fn forwards(&self) -> Vec<Command> {
        self.applied.iter().map(|a| a.forward.clone()).collect()
    }
}

/// A peer as the host knows it.
struct Guest {
    name: String,
    colour: TrackColor,
    token: u64,
    conn: Option<ConnId>,
    /// The newest of its proposals answered.
    answered: u64,
    presence: Option<Presence>,
    last_edit: Option<String>,
    listening: bool,
}

struct HostSide {
    host: Host,
    code: InviteCode,
    port: u16,
    /// The address listened on (unspecified: every interface).
    bound: std::net::IpAddr,
    name: String,
    seq: u64,
    log: VecDeque<(Entry, usize)>,
    log_bytes: usize,
    next_local: u64,
    guests: BTreeMap<PeerId, Guest>,
    conns: HashMap<ConnId, PeerId>,
    next_peer: PeerId,
    /// Peers waiting for files the host is fetching: key → (conn, from).
    waiting: HashMap<String, Vec<(ConnId, u64)>>,
    checksum_due: Option<Instant>,
    /// Entries in order but not sent yet: an own step naming files still
    /// being hashed holds up those after it.
    unsent: VecDeque<Unsent>,
    /// Peers that said hello and wait for their Welcome (sent once nothing
    /// is unsent and the project's files are hashed).
    joining: VecDeque<Joining>,
    /// Peers that asked for the whole project.
    resyncs: Vec<ConnId>,
}

enum Unsent {
    Ready(Entry),
    Own {
        seq: u64,
        local: u64,
        label: String,
        forwards: Vec<Command>,
    },
}

struct Joining {
    conn: ConnId,
    peer: PeerId,
    name: String,
    from: std::net::SocketAddr,
    back: Option<Resume>,
    lease: Option<(u64, u64)>,
}

#[derive(Clone, Debug, PartialEq)]
enum PeerLink {
    Connecting,
    Joined,
    Lost {
        since: Instant,
        retry_at: Instant,
        tries: u32,
        reason: String,
    },
    Ended(String),
}

struct PeerSide {
    client: Option<Client>,
    address: String,
    code: InviteCode,
    name: String,
    me: Option<PeerId>,
    token: u64,
    seq: u64,
    link: PeerLink,
    pending: VecDeque<Pending>,
    next_local: u64,
    asked_lease: bool,
    people: Vec<Participant>,
    presences: BTreeMap<PeerId, Presence>,
    last_edit: BTreeMap<PeerId, String>,
    resync_asked: Option<Instant>,
    host_address: Option<String>,
}

enum Side {
    Host(Box<HostSide>),
    Peer(Box<PeerSide>),
}

/// A file on its way here.
struct Fetch {
    dest: PathBuf,
    name: String,
    /// The host: the peer it comes from.
    from: Option<ConnId>,
    done: u64,
    len: u64,
}

/// Collaboration's part of the session.
#[derive(Default)]
pub(crate) struct CollabState {
    side: Option<Side>,
    keys: form::Keys,
    /// Files here by key (sent or arrived).
    have: HashMap<String, PathBuf>,
    fetching: BTreeMap<String, Fetch>,
    failed_media: Vec<String>,
    chat: Vec<ChatLine>,
    /// Why the last session ended (shown until another starts).
    ended: Option<(CollabRole, String)>,
    /// Own recent edits ([`touched`]), for telling who overrides them.
    recent: VecDeque<(Instant, String)>,
    told: HashMap<String, Instant>,
    presence_sent: Option<(Presence, Instant)>,
    plugins_seen: BTreeSet<String>,
    missing_plugins: Vec<String>,
    /// The end of every id range set aside so far (alike in every copy).
    granted_until: u64,
    lease: Arc<IdLease>,
    /// Remote changes since the last save (the project differs from its
    /// file though the undo history did not move).
    remote_unsaved: bool,
    listen: listen::ListenState,
}

fn colour_of(c: TrackColor) -> u32 {
    (u32::from(c.r) << 16) | (u32::from(c.g) << 8) | u32::from(c.b)
}

fn colour_from(c: u32) -> TrackColor {
    TrackColor::rgb((c >> 16) as u8, (c >> 8) as u8, c as u8)
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64)
}

/// What a command sets, for telling who overrides whose edit: its
/// coalescing key, or for setters that have none, the setter and its
/// target.
fn touched(c: &Command) -> Option<String> {
    use Command::*;
    if let Some(k) = c.coalesce_key() {
        return Some(format!("{k:?}"));
    }
    let what = match c {
        RenameTrack { track, .. } => format!("name {track}"),
        SetTrackColor { track, .. } => format!("colour {track}"),
        SetTrackMute { track, .. } => format!("mute {track}"),
        SetTrackSolo { track, .. } => format!("solo {track}"),
        SetTrackRecordArm { track, .. } => format!("arm {track}"),
        SetTrackMonitor { track, .. } => format!("monitor {track}"),
        SetTrackPanMode { track, .. } => format!("pan mode {track}"),
        SetTrackOutput { track, .. } => format!("output {track}"),
        SetTrackInput { track, .. } => format!("input {track}"),
        SetTrackPhaseInvert { track, .. } => format!("polarity {track}"),
        SetPluginBypass { plugin, .. } => format!("bypass {plugin}"),
        SetPluginState { plugin, .. } => format!("state {plugin}"),
        SetSendTap { send, .. } => format!("tap {send}"),
        SetSendEnabled { send, .. } => format!("send on {send}"),
        RenameClip { clip, .. } => format!("name {clip}"),
        SetClipMuted { clip, .. } => format!("mute {clip}"),
        RenameProject { .. } => "project name".to_string(),
        _ => return None,
    };
    Some(what)
}

/// Every command a step holds (batches opened).
fn each_command<'a>(cmds: &'a [Command], f: &mut dyn FnMut(&'a Command)) {
    for c in cmds {
        match c {
            Command::Batch { commands, .. } => each_command(commands, f),
            c => f(c),
        }
    }
}

fn replaces_project(cmds: &[Command]) -> bool {
    let mut found = false;
    each_command(cmds, &mut |c| {
        found |= matches!(c, Command::ReplaceProject { .. });
    });
    found
}

/// Neighbouring setters of the same thing as one (the first inverse, the
/// last value).
fn coalesce(applied: Vec<Applied>) -> Vec<Applied> {
    let mut out: Vec<Applied> = Vec::with_capacity(applied.len());
    for a in applied {
        if let Some(last) = out.last_mut()
            && std::mem::discriminant(&last.forward) == std::mem::discriminant(&a.forward)
            && a.forward.coalesce_key().is_some()
            && last.forward.coalesce_key() == a.forward.coalesce_key()
        {
            last.forward = a.forward;
            continue;
        }
        out.push(a);
    }
    out
}

fn label_of(applied: &[Applied]) -> String {
    match applied {
        [] => String::new(),
        [one] => one.forward.label(),
        [.., last] => last.forward.label(),
    }
}

/// Apply commands as one: all or none (the journal is off meanwhile).
fn apply_all(
    project: &mut Project,
    cmds: Vec<Command>,
) -> std::result::Result<(Impact, Vec<Applied>), String> {
    let mut applied: Vec<Applied> = Vec::with_capacity(cmds.len());
    let mut impact = Impact::None;
    for c in cmds {
        let forward = c.clone();
        impact = impact.max(c.impact());
        match c.apply(project) {
            Ok(inverse) => applied.push(Applied { forward, inverse }),
            Err(e) => {
                for a in applied.into_iter().rev() {
                    let _ = a.inverse.apply(project);
                }
                return Err(e.to_string());
            }
        }
    }
    Ok((impact, applied))
}

/// Take back applied commands (newest first).
fn take_back(project: &mut Project, applied: &[Applied]) -> bool {
    let mut ok = true;
    for a in applied.iter().rev() {
        ok &= a.inverse.clone().apply(project).is_ok();
    }
    ok
}

fn impact_of(applied: &[Applied]) -> Impact {
    applied
        .iter()
        .map(|a| a.forward.impact())
        .max()
        .unwrap_or(Impact::None)
}

impl CollabState {
    /// Where the file `key` (named `name` by its sender) is here: a file
    /// here with that content, else the place it is fetched to (noted:
    /// the caller starts the fetch).
    fn resolve(
        &mut self,
        media_dir: &Path,
        from: Option<ConnId>,
        key: &str,
        name: &str,
    ) -> PathBuf {
        if let Some(p) = self.have.get(key)
            && p.is_file()
        {
            return p.clone();
        }
        if let Some(f) = self.fetching.get(key) {
            return f.dest.clone();
        }
        let safe: String = name
            .chars()
            .map(|c| {
                if matches!(c, '/' | '\\' | ':' | '\0') {
                    '_'
                } else {
                    c
                }
            })
            .collect();
        let safe = if safe.is_empty() || safe == "." || safe == ".." {
            "media".to_string()
        } else {
            safe
        };
        let mut dest = media_dir.join(&safe);
        // The same name with other content: a name of its own.
        if dest.exists() && self.keys.key_of(&dest).as_deref() != Some(key) {
            let p = Path::new(&safe);
            let stem = p
                .file_stem()
                .map_or_else(|| "media".into(), |s| s.to_string_lossy());
            let short = key.split_once(':').map_or(key, |(_, h)| h);
            let short = &short[..short.len().min(8)];
            dest = media_dir.join(match p.extension() {
                Some(e) => format!("{stem} {short}.{}", e.to_string_lossy()),
                None => format!("{stem} {short}"),
            });
        }
        if dest.exists() && self.keys.key_of(&dest).as_deref() == Some(key) {
            self.have.insert(key.to_string(), dest.clone());
            return dest;
        }
        self.keys.coming(&dest, key);
        self.fetching.insert(
            key.to_string(),
            Fetch {
                dest: dest.clone(),
                name: safe,
                from,
                done: 0,
                len: form::key_length(key).unwrap_or(0),
            },
        );
        dest
    }

    fn lease_left(&self) -> u64 {
        self.lease.remaining()
    }

    fn note_own(&mut self, applied: &[Applied]) {
        let now = Instant::now();
        let cmds: Vec<Command> = applied.iter().map(|a| a.forward.clone()).collect();
        each_command(&cmds, &mut |c| {
            if let Some(k) = touched(c) {
                self.recent.push_back((now, k));
            }
        });
        while self
            .recent
            .front()
            .is_some_and(|(t, _)| t.elapsed() > OVERRIDE_WINDOW)
        {
            self.recent.pop_front();
        }
    }

    /// Own recent edits a remote step changes again (each told once a
    /// while).
    fn overridden(&mut self, cmds: &[Command]) -> bool {
        let recent: BTreeSet<&String> = self
            .recent
            .iter()
            .filter(|(t, _)| t.elapsed() <= OVERRIDE_WINDOW)
            .map(|(_, k)| k)
            .collect();
        let mut keys = Vec::new();
        each_command(cmds, &mut |c| {
            if let Some(k) = touched(c)
                && recent.contains(&k)
            {
                keys.push(k);
            }
        });
        let mut hit = false;
        for k in keys {
            let fresh = self
                .told
                .get(&k)
                .is_none_or(|t| t.elapsed() > Duration::from_secs(10));
            if fresh {
                self.told.insert(k, Instant::now());
                hit = true;
            }
        }
        hit
    }
}

impl Session {
    // --- reading ------------------------------------------------------------

    /// Whether this session hosts or has joined a live session.
    pub fn collaborating(&self) -> bool {
        self.collab.side.is_some()
    }

    /// What collaboration is doing (`None`: not, and nothing to tell).
    pub fn collaboration(&self) -> Option<CollabStatus> {
        let c = &self.collab;
        let media = MediaStatus {
            waiting: c.fetching.len(),
            hashing: c.keys.hashing(),
            done: c.fetching.values().map(|f| f.done).sum(),
            total: c.fetching.values().map(|f| f.len).sum(),
            failed: c.failed_media.clone(),
        };
        let Some(side) = &c.side else {
            let (role, reason) = c.ended.clone()?;
            return Some(CollabStatus {
                role,
                connection: Connection::Ended { reason },
                host_address: None,
                invite: None,
                people: Vec::new(),
                chat: c.chat.clone(),
                media,
                waiting: 0,
                missing_plugins: c.missing_plugins.clone(),
                listening: false,
                seq: 0,
            });
        };
        Some(match side {
            Side::Host(h) => {
                let mut people = vec![Person {
                    peer: 0,
                    name: h.name.clone(),
                    colour: TrackColor::palette(0),
                    host: true,
                    me: true,
                    connected: true,
                    listening: false,
                    presence: c.presence_sent.as_ref().map(|(p, _)| p.clone()),
                    last_edit: None,
                }];
                people.extend(h.guests.iter().map(|(&peer, g)| Person {
                    peer,
                    name: g.name.clone(),
                    colour: g.colour,
                    host: false,
                    me: false,
                    connected: g.conn.is_some(),
                    listening: g.listening,
                    presence: g.presence.clone(),
                    last_edit: g.last_edit.clone(),
                }));
                let ips = if h.bound.is_unspecified() {
                    faderframe_collab::local_addresses()
                } else {
                    vec![h.bound]
                };
                let addresses = ips
                    .into_iter()
                    .map(|ip| std::net::SocketAddr::new(ip, h.port).to_string())
                    .collect();
                CollabStatus {
                    role: CollabRole::Host,
                    connection: Connection::Connected,
                    host_address: None,
                    invite: Some(Invite {
                        code: h.code.to_string(),
                        port: h.port,
                        addresses,
                    }),
                    people,
                    chat: c.chat.clone(),
                    media,
                    waiting: 0,
                    missing_plugins: c.missing_plugins.clone(),
                    listening: false,
                    seq: h.seq,
                }
            }
            Side::Peer(p) => {
                let connection = match &p.link {
                    PeerLink::Connecting => Connection::Connecting,
                    PeerLink::Joined => Connection::Connected,
                    PeerLink::Lost { since, reason, .. } => Connection::Lost {
                        seconds: since.elapsed().as_secs(),
                        reason: reason.clone(),
                    },
                    PeerLink::Ended(r) => Connection::Ended { reason: r.clone() },
                };
                let people = p
                    .people
                    .iter()
                    .map(|q| Person {
                        peer: q.peer,
                        name: q.name.clone(),
                        colour: colour_from(q.colour),
                        host: q.host,
                        me: Some(q.peer) == p.me,
                        connected: q.connected,
                        listening: q.listening,
                        presence: if Some(q.peer) == p.me {
                            c.presence_sent.as_ref().map(|(x, _)| x.clone())
                        } else {
                            p.presences.get(&q.peer).cloned()
                        },
                        last_edit: p.last_edit.get(&q.peer).cloned(),
                    })
                    .collect();
                CollabStatus {
                    role: CollabRole::Peer,
                    connection,
                    host_address: Some(p.address.clone()),
                    invite: None,
                    people,
                    chat: c.chat.clone(),
                    media,
                    // (What the journal holds becomes a step next tick.)
                    waiting: p.pending.len() + usize::from(!self.project.journal.is_empty()),
                    missing_plugins: c.missing_plugins.clone(),
                    listening: c.listen.listening(),
                    seq: p.seq,
                }
            }
        })
    }

    /// The others' presence (their colours), for the editors.
    pub fn collab_presences(&self) -> Vec<RemotePresence> {
        match &self.collab.side {
            None => Vec::new(),
            Some(Side::Host(h)) => h
                .guests
                .iter()
                .filter(|(_, g)| g.conn.is_some())
                .filter_map(|(&peer, g)| {
                    Some(RemotePresence {
                        peer,
                        name: g.name.clone(),
                        colour: g.colour,
                        presence: g.presence.clone()?,
                    })
                })
                .collect(),
            Some(Side::Peer(p)) => p
                .people
                .iter()
                .filter(|q| Some(q.peer) != p.me && q.connected)
                .filter_map(|q| {
                    Some(RemotePresence {
                        peer: q.peer,
                        name: q.name.clone(),
                        colour: colour_from(q.colour),
                        presence: p.presences.get(&q.peer)?.clone(),
                    })
                })
                .collect(),
        }
    }

    /// Changes from others since the last save.
    pub(crate) fn collab_unsaved(&self) -> bool {
        self.collab.remote_unsaved
    }

    pub(crate) fn collab_saved(&mut self) {
        self.collab.remote_unsaved = false;
    }

    /// Tick until `until` holds or `timeout` passes (tests, scripts).
    pub fn wait_for_collab(&mut self, timeout: Duration, until: impl Fn(&Session) -> bool) -> bool {
        let t = Instant::now();
        loop {
            self.tick_collab();
            if until(self) {
                return true;
            }
            if t.elapsed() > timeout {
                return false;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    /// Nothing on its way: no own step waits for the host, no entry for
    /// the peers, no file is fetched (tests wait for it).
    pub fn collab_settled(&self) -> bool {
        self.project.journal.is_empty()
            && self.collab.fetching.is_empty()
            && match &self.collab.side {
                None => true,
                Some(Side::Host(h)) => h.unsent.is_empty() && h.joining.is_empty(),
                Some(Side::Peer(p)) => p.pending.is_empty() && p.link == PeerLink::Joined,
            }
    }

    /// End the connection to the host as if the network went (tests: a
    /// peer comes back by itself).
    #[doc(hidden)]
    pub fn collab_break_connection(&mut self) {
        if let Some(Side::Peer(p)) = &mut self.collab.side {
            p.client = None;
        }
        self.peer_closed("the connection broke".into());
    }

    // --- asking -------------------------------------------------------------

    pub(crate) fn collab_op(&mut self, op: CollabOp) -> Result<()> {
        match op {
            CollabOp::Host { bind, name } => self.start_hosting(bind, &name),
            CollabOp::Join {
                address,
                code,
                name,
            } => self.join_session(&address, &code, &name),
            CollabOp::Leave => {
                self.leave_session("you left");
                Ok(())
            }
            CollabOp::NewInvite => {
                if let Some(Side::Host(h)) = &mut self.collab.side {
                    let code = InviteCode::new().map_err(|e| SessionError::Other(e.to_string()))?;
                    h.host.set_code(code.clone());
                    h.code = code;
                    self.revision += 1;
                }
                Ok(())
            }
            CollabOp::Chat(text) => {
                self.say(text);
                Ok(())
            }
            CollabOp::ListenAlong(on) => {
                self.set_listen_along(on);
                Ok(())
            }
            CollabOp::Resync => {
                if let Some(Side::Peer(p)) = &mut self.collab.side
                    && let Some(c) = &p.client
                {
                    c.send(&ToHost::Resync);
                    p.resync_asked = Some(Instant::now());
                }
                Ok(())
            }
        }
    }

    fn start_hosting(&mut self, bind: Option<std::net::SocketAddr>, name: &str) -> Result<()> {
        if self.collaborating() {
            return Err(SessionError::Other(
                "already in a live session: leave it first".into(),
            ));
        }
        let code = InviteCode::new().map_err(|e| SessionError::Other(e.to_string()))?;
        let any = std::net::SocketAddr::from(([0, 0, 0, 0], faderframe_collab::DEFAULT_PORT));
        let host = match bind {
            Some(b) => Host::start(b, code.clone()),
            None => Host::start(any, code.clone()).or_else(|e| {
                // The port taken: any free one.
                if e.kind() == std::io::ErrorKind::AddrInUse {
                    Host::start(std::net::SocketAddr::from(([0, 0, 0, 0], 0)), code.clone())
                } else {
                    Err(e)
                }
            }),
        }
        .map_err(|e| {
            SessionError::Other(format!("could not listen on {}: {e}", bind.unwrap_or(any)))
        })?;
        let bound = host.local_addr();
        let port = bound.port();
        let name = if name.trim().is_empty() {
            "Host".to_string()
        } else {
            name.trim().to_string()
        };
        self.collab.ended = None;
        self.collab.chat.clear();
        self.collab.failed_media.clear();
        self.collab.lease = Arc::new(IdLease::default());
        self.collab.granted_until = self.project.ids.peek();
        self.collab.side = Some(Side::Host(Box::new(HostSide {
            host,
            code,
            port,
            bound: bound.ip(),
            name: name.clone(),
            seq: 0,
            log: VecDeque::new(),
            log_bytes: 0,
            next_local: 1,
            guests: BTreeMap::new(),
            conns: HashMap::new(),
            next_peer: 1,
            waiting: HashMap::new(),
            checksum_due: None,
            unsent: VecDeque::new(),
            joining: VecDeque::new(),
            resyncs: Vec::new(),
        })));
        // What was done before is the start everyone gets.
        self.project.journal.stop();
        self.project.journal.start();
        self.grant(0);
        self.offer_project_media();
        self.notify(
            NoticeLevel::Info,
            format!(
                "Hosting a live session on port {port}: invite people with the code in Collaborate"
            ),
        );
        self.revision += 1;
        Ok(())
    }

    fn join_session(&mut self, address: &str, code: &str, name: &str) -> Result<()> {
        if self.collaborating() {
            return Err(SessionError::Other(
                "already in a live session: leave it first".into(),
            ));
        }
        let code = InviteCode::parse(code).map_err(|e| SessionError::Other(e.to_string()))?;
        let name = if name.trim().is_empty() {
            "Guest".to_string()
        } else {
            name.trim().to_string()
        };
        let client = Client::connect(address, code.clone(), self.hello(&name, None));
        self.collab.ended = None;
        self.collab.chat.clear();
        self.collab.failed_media.clear();
        self.collab.lease = Arc::new(IdLease::default());
        self.collab.side = Some(Side::Peer(Box::new(PeerSide {
            client: Some(client),
            address: address.to_string(),
            code,
            name,
            me: None,
            token: 0,
            seq: 0,
            link: PeerLink::Connecting,
            pending: VecDeque::new(),
            next_local: 1,
            asked_lease: false,
            people: Vec::new(),
            presences: BTreeMap::new(),
            last_edit: BTreeMap::new(),
            resync_asked: None,
            host_address: None,
        })));
        self.revision += 1;
        Ok(())
    }

    fn hello(&self, name: &str, resume: Option<Resume>) -> Hello {
        Hello {
            protocol: faderframe_collab::PROTOCOL,
            program: format!("FaderFrame {}", env!("CARGO_PKG_VERSION")),
            name: name.to_string(),
            resume,
        }
    }

    /// Leave or end the session; the project stays as it is here.
    pub(crate) fn leave_session(&mut self, why: &str) {
        // What waits in the journal goes first.
        self.collab_send_local();
        let Some(side) = self.collab.side.take() else {
            return;
        };
        let role = match side {
            Side::Host(mut h) => {
                h.host.stop("the host ended the session");
                CollabRole::Host
            }
            Side::Peer(p) => {
                if let Some(c) = &p.client {
                    c.close();
                }
                if !p.pending.is_empty() {
                    self.notify(
                        NoticeLevel::Warning,
                        format!(
                            "{} of your edits had not reached the host; they stay in your copy",
                            p.pending.len()
                        ),
                    );
                }
                CollabRole::Peer
            }
        };
        self.project.journal.stop();
        self.project.ids.set_lease(None);
        self.collab.fetching.clear();
        self.stop_listen_along();
        self.collab.ended = Some((role, why.to_string()));
        self.notify(NoticeLevel::Info, format!("Live session ended: {why}"));
        self.revision += 1;
    }

    fn say(&mut self, text: String) {
        let text = text.trim().to_string();
        if text.is_empty() {
            return;
        }
        match &mut self.collab.side {
            Some(Side::Host(h)) => {
                let at = now_ms();
                let msg = ToPeer::Chat {
                    peer: 0,
                    text: text.clone(),
                    at,
                };
                for &conn in h.conns.keys() {
                    h.host.send(conn, &msg);
                }
                let line = ChatLine {
                    from: h.name.clone(),
                    colour: TrackColor::palette(0),
                    text,
                    at,
                    mine: true,
                };
                self.push_chat(line);
            }
            Some(Side::Peer(p)) => {
                if let Some(c) = &p.client {
                    c.send(&ToHost::Chat(text));
                }
            }
            None => {}
        }
    }

    fn push_chat(&mut self, line: ChatLine) {
        self.collab.chat.push(line);
        if self.collab.chat.len() > CHAT_KEPT {
            let extra = self.collab.chat.len() - CHAT_KEPT;
            self.collab.chat.drain(..extra);
        }
        self.revision += 1;
    }

    // --- every tick -----------------------------------------------------------

    pub(crate) fn tick_collab(&mut self) {
        if self.collab.side.is_none() {
            return;
        }
        self.keep_journal();
        self.collab_send_local();
        if matches!(self.collab.side, Some(Side::Host(_))) {
            self.host_tick();
        } else {
            self.peer_tick();
        }
        self.presence_tick();
        self.check_plugins();
        self.tick_listen_along();
    }

    /// The journal on and the lease in the project's allocator (a project
    /// replaced outside commands — another one opened — loses both).
    fn keep_journal(&mut self) {
        // A peer not welcomed yet has no shared project (its own is replaced
        // by the host's).
        if matches!(&self.collab.side, Some(Side::Peer(p)) if p.me.is_none()) {
            return;
        }
        if !self.project.journal.is_on() {
            match &self.collab.side {
                Some(Side::Host(_)) => {
                    // Everyone follows the host to the project it opened.
                    self.project.journal.start();
                    let project = self.project.clone();
                    let label = format!("Open ‘{}’", project.name);
                    self.order_own(vec![Applied {
                        forward: Command::ReplaceProject {
                            project: Box::new(project),
                            label: label.clone(),
                        },
                        // (Never taken back: the host's own entries are not
                        // rebased.)
                        inverse: Command::RenameProject {
                            name: self.project.name.clone(),
                        },
                    }]);
                    self.offer_project_media();
                }
                Some(Side::Peer(_)) => {
                    self.leave_session("you opened another project");
                    return;
                }
                None => return,
            }
        }
        let ours = self
            .project
            .ids
            .lease()
            .is_some_and(|l| Arc::ptr_eq(l, &self.collab.lease));
        if !ours {
            self.project
                .ids
                .set_lease(Some(Arc::clone(&self.collab.lease)));
        }
        // No id below what was set aside is handed out by the counter.
        let until = self.collab.granted_until;
        if until > 0 {
            self.project.ids.reserve_through(until - 1);
        }
    }

    /// What the journal wrote since: one step, ordered (host) or proposed
    /// (peer).
    fn collab_send_local(&mut self) {
        if self.collab.side.is_none() || self.project.journal.is_empty() {
            return;
        }
        let applied = coalesce(self.project.journal.take());
        if applied.is_empty() {
            return;
        }
        self.collab.note_own(&applied);
        if matches!(self.collab.side, Some(Side::Host(_))) {
            self.order_own(applied);
        } else {
            self.propose(applied);
        }
    }

    // --- the host ---------------------------------------------------------------

    /// Give the host's own step a place and send it (once the files it
    /// names are hashed).
    fn order_own(&mut self, applied: Vec<Applied>) {
        let label = label_of(&applied);
        let forwards: Vec<Command> = applied.iter().map(|a| a.forward.clone()).collect();
        let replaced = replaces_project(&forwards);
        let Some(Side::Host(h)) = &mut self.collab.side else {
            return;
        };
        let local = h.next_local;
        h.next_local += 1;
        h.seq += 1;
        h.unsent.push_back(Unsent::Own {
            seq: h.seq,
            local,
            label,
            forwards,
        });
        h.checksum_due = Some(Instant::now() + CHECKSUM_AFTER);
        self.flush_unsent();
        if replaced {
            self.renew_leases();
        }
    }

    /// Put an entry in the log and send it to everyone (after what waits).
    fn host_append(&mut self, author: PeerId, local: u64, body: EntryBody) {
        let Some(Side::Host(h)) = &mut self.collab.side else {
            return;
        };
        h.seq += 1;
        h.unsent.push_back(Unsent::Ready(Entry {
            seq: h.seq,
            author,
            local,
            body,
        }));
        h.checksum_due = Some(Instant::now() + CHECKSUM_AFTER);
        self.flush_unsent();
    }

    /// Send what waits, in order, as far as it can be.
    fn flush_unsent(&mut self) {
        loop {
            let front = match &mut self.collab.side {
                Some(Side::Host(h)) => h.unsent.pop_front(),
                _ => return,
            };
            let Some(front) = front else {
                return;
            };
            let entry = match front {
                Unsent::Ready(e) => e,
                Unsent::Own {
                    seq,
                    local,
                    label,
                    forwards,
                } => {
                    let mut named = BTreeMap::new();
                    match form::to_wire(&forwards, &mut self.collab.keys, &mut named) {
                        Ok(step) => {
                            for (k, p) in named {
                                if let Some(Side::Host(h)) = &self.collab.side {
                                    h.host.offer_media(&k, &p);
                                }
                                self.collab.have.insert(k, p);
                            }
                            Entry {
                                seq,
                                author: 0,
                                local,
                                body: EntryBody::Step { label, step },
                            }
                        }
                        Err(form::FormError::Hashing(_)) => {
                            if let Some(Side::Host(h)) = &mut self.collab.side {
                                h.unsent.push_front(Unsent::Own {
                                    seq,
                                    local,
                                    label,
                                    forwards,
                                });
                            }
                            return;
                        }
                        Err(e) => {
                            // Sent as nothing: the peers take the whole
                            // project instead.
                            self.notify(
                                NoticeLevel::Error,
                                format!(
                                    "Live session: ‘{label}’ could not be sent ({e}); \
                                     peers get the whole project"
                                ),
                            );
                            let step = form::to_wire(
                                &Vec::<Command>::new(),
                                &mut self.collab.keys,
                                &mut named,
                            )
                            .unwrap_or_default();
                            if let Some(Side::Host(h)) = &mut self.collab.side {
                                let all: Vec<ConnId> = h.conns.keys().copied().collect();
                                h.resyncs.extend(all);
                            }
                            Entry {
                                seq,
                                author: 0,
                                local,
                                body: EntryBody::Step { label, step },
                            }
                        }
                    }
                }
            };
            self.transmit(entry);
        }
    }

    fn transmit(&mut self, entry: Entry) {
        let Some(Side::Host(h)) = &mut self.collab.side else {
            return;
        };
        match faderframe_collab::wire::encode(&ToPeer::Entry(entry.clone())) {
            Ok(bytes) => {
                for &conn in h.conns.keys() {
                    h.host.send_bytes(conn, bytes.clone());
                }
                h.log_bytes += bytes.len();
                h.log.push_back((entry, bytes.len()));
                while h.log_bytes > LOG_BYTES && h.log.len() > 1 {
                    if let Some((_, n)) = h.log.pop_front() {
                        h.log_bytes -= n;
                    }
                }
            }
            Err(e) => tracing::warn!("collaboration: {e}"),
        }
    }

    /// Set ids aside for `peer` (0: the host) as an entry.
    fn grant(&mut self, peer: PeerId) -> (u64, u64) {
        let (start, end) = self.project.ids.set_aside(LEASE);
        self.collab.granted_until = self.collab.granted_until.max(end);
        if peer == 0 {
            self.collab.lease.grant(start, end);
        }
        self.host_append(0, 0, EntryBody::Grant { peer, start, end });
        (start, end)
    }

    /// After a project replaced whole: the ranges set aside before may be
    /// in its ids; everyone gets new ones.
    fn renew_leases(&mut self) {
        let Some(Side::Host(h)) = &self.collab.side else {
            return;
        };
        let peers: Vec<PeerId> = h.guests.keys().copied().collect();
        self.collab.lease = Arc::new(IdLease::default());
        self.project
            .ids
            .set_lease(Some(Arc::clone(&self.collab.lease)));
        self.grant(0);
        for p in peers {
            self.grant(p);
        }
    }

    /// The project as sent (its media offered on the shelf).
    fn snapshot(&mut self) -> std::result::Result<Vec<u8>, form::FormError> {
        let mut named = BTreeMap::new();
        let bytes = form::to_wire(&self.project, &mut self.collab.keys, &mut named)?;
        if let Some(Side::Host(h)) = &self.collab.side {
            for (k, p) in &named {
                h.host.offer_media(k, p);
            }
        }
        for (k, p) in named {
            self.collab.have.insert(k, p);
        }
        Ok(bytes)
    }

    /// Start hashing the project's media (their keys are wanted soon).
    fn offer_project_media(&mut self) {
        let _ = self.snapshot();
    }

    fn host_tick(&mut self) {
        let mut events = Vec::new();
        if let Some(Side::Host(h)) = &self.collab.side {
            while let Some(e) = h.host.poll() {
                events.push(e);
            }
        }
        for e in events {
            match e {
                HostEvent::Hello { conn, hello, from } => self.host_hello(conn, hello, from),
                HostEvent::Message { conn, message } => self.host_message(conn, message),
                HostEvent::Closed { conn, reason } => self.host_closed(conn, &reason),
                HostEvent::WantMedia { conn, key, from } => self.host_wants(conn, key, from),
                HostEvent::Media { conn: _, fetched } => self.media_fetched(fetched),
                HostEvent::Refused { from, reason } => {
                    self.notify(
                        NoticeLevel::Warning,
                        format!("Live session: {from} was turned away ({reason})"),
                    );
                }
            }
        }
        // The host's own ids.
        if self.collab.lease_left() < LEASE_LOW {
            self.grant(0);
        }
        self.collab.keys.poll();
        self.flush_unsent();
        self.welcome_joining();
        self.answer_resyncs();
        // The project's hash once edits pause (and everything is sent).
        let due = match &self.collab.side {
            Some(Side::Host(h)) => {
                h.unsent.is_empty() && h.checksum_due.is_some_and(|t| Instant::now() >= t)
            }
            _ => false,
        };
        if due {
            match form::canonical_hash(&self.project, &mut self.collab.keys) {
                Ok(hash) => {
                    if let Some(Side::Host(h)) = &mut self.collab.side {
                        let msg = ToPeer::Checksum { seq: h.seq, hash };
                        for &conn in h.conns.keys() {
                            h.host.send(conn, &msg);
                        }
                        h.checksum_due = None;
                    }
                }
                // Asked again next tick.
                Err(form::FormError::Hashing(_)) => {}
                Err(e) => {
                    tracing::warn!("collaboration: {e}");
                    if let Some(Side::Host(h)) = &mut self.collab.side {
                        h.checksum_due = None;
                    }
                }
            }
        }
    }

    fn participants(&self) -> Vec<Participant> {
        let Some(Side::Host(h)) = &self.collab.side else {
            return Vec::new();
        };
        let mut out = vec![Participant {
            peer: 0,
            name: h.name.clone(),
            colour: colour_of(TrackColor::palette(0)),
            host: true,
            connected: true,
            listening: false,
        }];
        out.extend(h.guests.iter().map(|(&peer, g)| Participant {
            peer,
            name: g.name.clone(),
            colour: colour_of(g.colour),
            host: false,
            connected: g.conn.is_some() && h.conns.contains_key(&g.conn.unwrap_or(0)),
            listening: g.listening,
        }));
        out
    }

    fn send_participants(&mut self) {
        let list = self.participants();
        if let Some(Side::Host(h)) = &self.collab.side {
            let msg = ToPeer::Participants(list);
            for &conn in h.conns.keys() {
                h.host.send(conn, &msg);
            }
        }
        self.revision += 1;
    }

    /// Someone with the code said hello: who it is (back, or new), its
    /// ids; welcomed once the project can be sent.
    fn host_hello(&mut self, conn: ConnId, hello: Hello, from: std::net::SocketAddr) {
        let back = match (&self.collab.side, &hello.resume) {
            (Some(Side::Host(h)), Some(r)) => h
                .guests
                .get(&r.peer)
                .filter(|g| g.token == r.token)
                .map(|_| r.clone()),
            _ => None,
        };
        let Some(Side::Host(h)) = &mut self.collab.side else {
            return;
        };
        let peer = match &back {
            Some(r) => {
                if let Some(old) = h.guests.get(&r.peer).and_then(|g| g.conn)
                    && old != conn
                {
                    h.conns.remove(&old);
                    h.joining.retain(|j| j.conn != old);
                    h.host.close(old, "the same peer connected again");
                }
                r.peer
            }
            None => {
                let peer = h.next_peer;
                h.next_peer += 1;
                h.guests.insert(
                    peer,
                    Guest {
                        name: hello.name.clone(),
                        colour: TrackColor::palette(peer as usize),
                        token: faderframe_collab::random_u64(),
                        conn: None,
                        answered: 0,
                        presence: None,
                        last_edit: None,
                        listening: false,
                    },
                );
                peer
            }
        };
        if let Some(g) = h.guests.get_mut(&peer) {
            g.conn = Some(conn);
            g.name = hello.name.clone();
        }
        // A new peer's ids, set aside as an entry before its project is
        // taken.
        let lease = back.is_none().then(|| self.grant(peer));
        if let Some(Side::Host(h)) = &mut self.collab.side {
            h.joining.push_back(Joining {
                conn,
                peer,
                name: hello.name,
                from,
                back,
                lease,
            });
        }
        self.welcome_joining();
    }

    /// Welcome those waiting, once everything is sent and the project's
    /// files are hashed.
    fn welcome_joining(&mut self) {
        loop {
            let ready = match &self.collab.side {
                Some(Side::Host(h)) => h.unsent.is_empty() && !h.joining.is_empty(),
                _ => false,
            };
            if !ready {
                return;
            }
            let Some(Side::Host(h)) = &self.collab.side else {
                return;
            };
            let Some(j) = h.joining.front() else {
                return;
            };
            let since = j.back.as_ref().map(|r| r.seq);
            let reaches = since
                .is_some_and(|s| s >= h.seq || h.log.front().is_some_and(|(e, _)| e.seq <= s + 1));
            let snapshot = if reaches {
                None
            } else {
                match self.snapshot() {
                    Ok(s) => Some(s),
                    Err(form::FormError::Hashing(_)) => return,
                    Err(e) => {
                        self.notify(
                            NoticeLevel::Error,
                            format!("Live session: the project could not be sent ({e})"),
                        );
                        if let Some(Side::Host(h)) = &mut self.collab.side
                            && let Some(j) = h.joining.pop_front()
                        {
                            h.host.close(j.conn, "the host could not send its project");
                        }
                        continue;
                    }
                }
            };
            let participants = self.participants();
            let Some(Side::Host(h)) = &mut self.collab.side else {
                return;
            };
            let Some(j) = h.joining.pop_front() else {
                return;
            };
            let entries: Vec<Entry> = match since {
                Some(s) if reaches => h
                    .log
                    .iter()
                    .filter(|(e, _)| e.seq > s)
                    .map(|(e, _)| e.clone())
                    .collect(),
                _ => Vec::new(),
            };
            let Some(g) = h.guests.get(&j.peer) else {
                continue;
            };
            let welcome = Welcome {
                peer: j.peer,
                token: g.token,
                colour: colour_of(g.colour),
                snapshot,
                seq: h.seq,
                entries,
                answered: g.answered,
                participants,
                sample_rate: self.project.sample_rate,
                lease: j.lease.into_iter().collect(),
            };
            h.host.send(j.conn, &ToPeer::Welcome(Box::new(welcome)));
            h.conns.insert(j.conn, j.peer);
            // Where everyone is.
            let mut presences: Vec<(PeerId, Presence)> = h
                .guests
                .iter()
                .filter(|(p, _)| **p != j.peer)
                .filter_map(|(p, g)| Some((*p, g.presence.clone()?)))
                .collect();
            if let Some((p, _)) = &self.collab.presence_sent {
                presences.push((0, p.clone()));
            }
            for (peer, p) in presences {
                if let Ok(data) = rmp_serde::to_vec_named(&p)
                    && let Ok(m) = faderframe_collab::wire::encode(&ToPeer::Presence { peer, data })
                {
                    h.host.send_presence(j.conn, peer, m);
                }
            }
            let verb = if j.back.is_some() {
                "is back"
            } else {
                "joined"
            };
            self.notify(
                NoticeLevel::Info,
                format!("{} {verb} (from {})", j.name, j.from),
            );
            self.send_participants();
        }
    }

    /// Send the whole project to those that asked.
    fn answer_resyncs(&mut self) {
        let wanted = match &self.collab.side {
            Some(Side::Host(h)) => h.unsent.is_empty() && !h.resyncs.is_empty(),
            _ => false,
        };
        if !wanted {
            return;
        }
        let project = match self.snapshot() {
            Ok(p) => p,
            Err(form::FormError::Hashing(_)) => return,
            Err(e) => {
                tracing::warn!("collaboration: {e}");
                return;
            }
        };
        if let Some(Side::Host(h)) = &mut self.collab.side {
            let msg = ToPeer::Snapshot {
                seq: h.seq,
                project,
            };
            let mut conns = std::mem::take(&mut h.resyncs);
            conns.sort_unstable();
            conns.dedup();
            for c in conns {
                if h.conns.contains_key(&c) {
                    h.host.send(c, &msg);
                }
            }
        }
    }

    fn host_closed(&mut self, conn: ConnId, reason: &str) {
        let Some(Side::Host(h)) = &mut self.collab.side else {
            return;
        };
        h.joining.retain(|j| j.conn != conn);
        h.resyncs.retain(|c| *c != conn);
        let peer = h.conns.remove(&conn);
        let mut name = String::new();
        for g in h.guests.values_mut() {
            if g.conn == Some(conn) {
                g.conn = None;
                g.listening = false;
                name = g.name.clone();
            }
        }
        for w in h.waiting.values_mut() {
            w.retain(|(c, _)| *c != conn);
        }
        if peer.is_some() && !name.is_empty() {
            self.notify(
                NoticeLevel::Info,
                format!("{name} lost the connection ({reason})"),
            );
        }
        self.send_participants();
    }

    fn host_message(&mut self, conn: ConnId, message: ToHost) {
        let Some(peer) = (match &self.collab.side {
            Some(Side::Host(h)) => h.conns.get(&conn).copied(),
            _ => None,
        }) else {
            return;
        };
        match message {
            ToHost::Propose { local, label, step } => {
                self.host_propose(conn, peer, local, label, step)
            }
            ToHost::Presence(data) => {
                let presence: Option<Presence> = rmp_serde::from_slice(&data).ok();
                if let Some(Side::Host(h)) = &mut self.collab.side {
                    if let Some(g) = h.guests.get_mut(&peer) {
                        g.presence = presence;
                    }
                    if let Ok(m) = faderframe_collab::wire::encode(&ToPeer::Presence { peer, data })
                    {
                        for (&c, &p) in &h.conns {
                            if p != peer {
                                h.host.send_presence(c, peer, m.clone());
                            }
                        }
                    }
                }
                self.revision += 1;
            }
            ToHost::Chat(text) => {
                let at = now_ms();
                let Some(Side::Host(h)) = &self.collab.side else {
                    return;
                };
                let msg = ToPeer::Chat {
                    peer,
                    text: text.clone(),
                    at,
                };
                for &c in h.conns.keys() {
                    h.host.send(c, &msg);
                }
                if let Some(g) = h.guests.get(&peer) {
                    let line = ChatLine {
                        from: g.name.clone(),
                        colour: g.colour,
                        text,
                        at,
                        mine: false,
                    };
                    self.push_chat(line);
                }
            }
            ToHost::WantLease => {
                self.grant(peer);
            }
            ToHost::Listen(on) => {
                if let Some(Side::Host(h)) = &mut self.collab.side
                    && let Some(g) = h.guests.get_mut(&peer)
                {
                    g.listening = on;
                }
                self.send_participants();
            }
            ToHost::Resync => {
                if let Some(Side::Host(h)) = &mut self.collab.side {
                    h.resyncs.push(conn);
                }
                self.answer_resyncs();
            }
            ToHost::Bye => {
                let mut name = None;
                if let Some(Side::Host(h)) = &mut self.collab.side {
                    h.conns.remove(&conn);
                    name = h.guests.remove(&peer).map(|g| g.name);
                }
                if let Some(n) = name {
                    self.notify(NoticeLevel::Info, format!("{n} left the session"));
                }
                self.send_participants();
            }
            ToHost::Hello(_) | ToHost::Ping | ToHost::WantMedia { .. } | ToHost::Media(_) => {}
        }
    }

    /// A peer's step: applied where it falls in the order and sent to
    /// everyone, or refused.
    fn host_propose(
        &mut self,
        conn: ConnId,
        peer: PeerId,
        local: u64,
        label: String,
        step: Vec<u8>,
    ) {
        let answered = match &self.collab.side {
            Some(Side::Host(h)) => h.guests.get(&peer).map_or(u64::MAX, |g| g.answered),
            _ => return,
        };
        if local <= answered {
            return;
        }
        let media_dir = self.media_dir.clone();
        let before: BTreeSet<String> = self.collab.fetching.keys().cloned().collect();
        let collab = &mut self.collab;
        let decoded: std::result::Result<Vec<Command>, _> = form::from_wire(&step, &mut |k, n| {
            collab.resolve(&media_dir, Some(conn), k, n)
        });
        let result = match decoded {
            Ok(cmds) => {
                let lww = self.collab.overridden(&cmds);
                let replaced = replaces_project(&cmds);
                self.apply_remote(cmds).map(|i| (i, lww, replaced))
            }
            Err(e) => Err(e.to_string()),
        };
        let Some(Side::Host(h)) = &mut self.collab.side else {
            return;
        };
        if let Some(g) = h.guests.get_mut(&peer) {
            g.answered = local;
        }
        match result {
            Ok((impact, lww, replaced)) => {
                let name = h
                    .guests
                    .get(&peer)
                    .map(|g| g.name.clone())
                    .unwrap_or_default();
                if let Some(g) = h.guests.get_mut(&peer) {
                    g.last_edit = Some(label.clone());
                }
                self.host_append(
                    peer,
                    local,
                    EntryBody::Step {
                        label: label.clone(),
                        step,
                    },
                );
                self.collab.remote_unsaved = true;
                if lww {
                    self.notify(
                        NoticeLevel::Info,
                        format!("{name} changed what you had just changed: {label}"),
                    );
                }
                if replaced {
                    self.renew_leases();
                }
                self.fetch_new_media();
                if let Err(e) = self.sync(impact) {
                    tracing::warn!("collaboration: {e}");
                }
            }
            Err(reason) => {
                // Fetches this step started are not wanted.
                let started: Vec<String> = self
                    .collab
                    .fetching
                    .keys()
                    .filter(|k| !before.contains(*k))
                    .cloned()
                    .collect();
                for k in started {
                    if let Some(f) = self.collab.fetching.remove(&k) {
                        self.collab.keys.not_coming(&f.dest);
                    }
                }
                if let Some(Side::Host(h)) = &self.collab.side {
                    h.host.send(conn, &ToPeer::Reject { local, reason });
                }
            }
        }
    }

    /// Apply commands from someone else (not journaled; all or none).
    fn apply_remote(&mut self, cmds: Vec<Command>) -> std::result::Result<Impact, String> {
        let journal = std::mem::take(&mut self.project.journal);
        let r = apply_all(&mut self.project, cmds);
        self.project.journal = journal;
        let (impact, _) = r?;
        self.after_remote();
        Ok(impact)
    }

    /// After commands from elsewhere: the lease and the counter kept.
    fn after_remote(&mut self) {
        self.project
            .ids
            .set_lease(Some(Arc::clone(&self.collab.lease)));
        let until = self.collab.granted_until;
        if until > 0 {
            self.project.ids.reserve_through(until - 1);
        }
    }

    fn host_wants(&mut self, conn: ConnId, key: String, from: u64) {
        let have = self.collab.have.get(&key).filter(|p| p.is_file()).cloned();
        let Some(Side::Host(h)) = &mut self.collab.side else {
            return;
        };
        match have {
            Some(p) => {
                h.host.offer_media(&key, &p);
                h.host.serve(conn, &key, &p, from);
            }
            None if self.collab.fetching.contains_key(&key) => {
                h.waiting.entry(key).or_default().push((conn, from));
            }
            None => h.host.missing(conn, &key),
        }
    }

    // --- peers ------------------------------------------------------------------

    fn propose(&mut self, applied: Vec<Applied>) {
        let replaced = replaces_project(
            &applied
                .iter()
                .map(|a| a.forward.clone())
                .collect::<Vec<_>>(),
        );
        let Some(Side::Peer(p)) = &mut self.collab.side else {
            return;
        };
        let local = p.next_local;
        p.next_local += 1;
        let label = label_of(&applied);
        p.pending.push_back(Pending {
            local,
            label,
            applied,
            sent: false,
        });
        if replaced {
            // A merge here: the ids set aside before may be in the new
            // project's; new ones come after the host takes it.
            self.collab.lease = Arc::new(IdLease::default());
            self.project
                .ids
                .set_lease(Some(Arc::clone(&self.collab.lease)));
        }
        self.send_proposals();
    }

    /// Propose the pending steps not proposed yet, in order (a step naming
    /// files still being hashed holds up those after it).
    fn send_proposals(&mut self) {
        loop {
            let next = match &self.collab.side {
                Some(Side::Peer(p)) if p.link == PeerLink::Joined && p.client.is_some() => p
                    .pending
                    .iter()
                    .find(|x| !x.sent)
                    .map(|x| (x.local, x.label.clone(), x.forwards())),
                _ => None,
            };
            let Some((local, label, forwards)) = next else {
                return;
            };
            let mut named = BTreeMap::new();
            let step = match form::to_wire(&forwards, &mut self.collab.keys, &mut named) {
                Ok(s) => s,
                Err(form::FormError::Hashing(_)) => return,
                Err(e) => {
                    self.notify(
                        NoticeLevel::Error,
                        format!("Live session: ‘{label}’ could not be sent ({e})"),
                    );
                    self.peer_reject(local, "it could not be sent");
                    continue;
                }
            };
            let Some(Side::Peer(p)) = &mut self.collab.side else {
                return;
            };
            if let Some(c) = &p.client {
                for (k, path) in &named {
                    c.offer_media(k, path);
                }
                c.send(&ToHost::Propose { local, label, step });
            }
            if let Some(x) = p.pending.iter_mut().find(|x| x.local == local) {
                x.sent = true;
            }
            for (k, path) in named {
                self.collab.have.insert(k, path);
            }
        }
    }

    fn peer_tick(&mut self) {
        let mut events = Vec::new();
        if let Some(Side::Peer(p)) = &self.collab.side
            && let Some(c) = &p.client
        {
            while let Some(e) = c.poll() {
                events.push(e);
            }
        }
        for e in events {
            match e {
                ClientEvent::Connected { to } => {
                    if let Some(Side::Peer(p)) = &mut self.collab.side {
                        p.host_address = Some(to.to_string());
                    }
                }
                ClientEvent::Message(m) => self.peer_message(m),
                ClientEvent::Media(f) => self.media_fetched(f),
                ClientEvent::Closed { reason } => self.peer_closed(reason),
            }
            if self.collab.side.is_none() {
                return;
            }
        }
        self.collab.keys.poll();
        self.send_proposals();
        // Trying again.
        let retry = match &self.collab.side {
            Some(Side::Peer(p)) => match &p.link {
                PeerLink::Lost { retry_at, .. } => Instant::now() >= *retry_at,
                _ => false,
            },
            _ => false,
        };
        if retry {
            self.reconnect();
        }
        // More ids before they run out.
        if let Some(Side::Peer(p)) = &mut self.collab.side
            && p.link == PeerLink::Joined
            && !p.asked_lease
            && self.collab.lease.remaining() < LEASE_LOW
            && let Some(c) = &p.client
        {
            c.send(&ToHost::WantLease);
            p.asked_lease = true;
        }
    }

    fn peer_closed(&mut self, reason: String) {
        let Some(Side::Peer(p)) = &mut self.collab.side else {
            return;
        };
        match &p.link {
            PeerLink::Ended(_) => {
                let why = match &p.link {
                    PeerLink::Ended(r) => r.clone(),
                    _ => reason,
                };
                self.leave_session(&why);
            }
            PeerLink::Connecting if p.me.is_none() => {
                // Never got in.
                self.leave_session(&format!("could not join: {reason}"));
            }
            PeerLink::Lost { tries, since, .. } => {
                let tries = tries + 1;
                let wait = (RETRY * tries).min(RETRY_MAX);
                p.link = PeerLink::Lost {
                    since: *since,
                    retry_at: Instant::now() + wait,
                    tries,
                    reason,
                };
                p.client = None;
            }
            _ => {
                p.link = PeerLink::Lost {
                    since: Instant::now(),
                    retry_at: Instant::now() + RETRY,
                    tries: 0,
                    reason: reason.clone(),
                };
                p.client = None;
                self.collab.fetching.clear();
                self.notify(
                    NoticeLevel::Warning,
                    format!("Live session: lost the host ({reason}); trying again — keep working"),
                );
                self.revision += 1;
            }
        }
    }

    fn reconnect(&mut self) {
        let hello = match &self.collab.side {
            Some(Side::Peer(p)) => self.hello(
                &p.name,
                p.me.map(|peer| Resume {
                    peer,
                    token: p.token,
                    seq: p.seq,
                }),
            ),
            _ => return,
        };
        if let Some(Side::Peer(p)) = &mut self.collab.side {
            p.client = Some(Client::connect(&p.address, p.code.clone(), hello));
            if let PeerLink::Lost { retry_at, .. } = &mut p.link {
                // Until this try ends.
                *retry_at = Instant::now() + Duration::from_secs(3600);
            }
        }
    }

    fn peer_message(&mut self, m: ToPeer) {
        match m {
            ToPeer::Welcome(w) => self.peer_welcome(*w),
            ToPeer::Entry(e) => self.peer_entry(e),
            ToPeer::Reject { local, reason } => self.peer_reject(local, &reason),
            ToPeer::Participants(list) => {
                if let Some(Side::Peer(p)) = &mut self.collab.side {
                    let known: BTreeSet<PeerId> = list.iter().map(|q| q.peer).collect();
                    p.presences.retain(|k, _| known.contains(k));
                    p.people = list;
                }
                self.revision += 1;
            }
            ToPeer::Presence { peer, data } => {
                if let Some(Side::Peer(p)) = &mut self.collab.side
                    && let Ok(presence) = rmp_serde::from_slice::<Presence>(&data)
                {
                    p.presences.insert(peer, presence);
                }
                self.revision += 1;
            }
            ToPeer::Chat { peer, text, at } => {
                let Some(Side::Peer(p)) = &self.collab.side else {
                    return;
                };
                let who = p.people.iter().find(|q| q.peer == peer);
                let line = ChatLine {
                    from: who.map_or_else(|| "?".into(), |q| q.name.clone()),
                    colour: who.map_or(TrackColor::palette(0), |q| colour_from(q.colour)),
                    text,
                    at,
                    mine: Some(peer) == p.me,
                };
                self.push_chat(line);
            }
            ToPeer::Snapshot { seq, project } => self.peer_snapshot(seq, &project),
            ToPeer::Checksum { seq, hash } => self.peer_checksum(seq, hash),
            ToPeer::Audio { seq, packet } => self.listen_packet(seq, &packet),
            ToPeer::Bye { reason } => {
                if let Some(Side::Peer(p)) = &mut self.collab.side {
                    p.link = PeerLink::Ended(reason);
                }
            }
            ToPeer::Media(_) | ToPeer::WantMedia { .. } | ToPeer::Ping => {}
        }
    }

    fn peer_welcome(&mut self, w: Welcome) {
        let first = matches!(&self.collab.side, Some(Side::Peer(p)) if p.me.is_none());
        if let Some(Side::Peer(p)) = &mut self.collab.side {
            p.me = Some(w.peer);
            p.token = w.token;
            p.people = w.participants.clone();
            p.link = PeerLink::Joined;
        }
        for (start, end) in &w.lease {
            self.collab.lease.grant(*start, *end);
            self.collab.granted_until = self.collab.granted_until.max(*end);
        }
        if let Some(snapshot) = &w.snapshot {
            if first {
                if let Err(e) = self.take_first_project(snapshot) {
                    self.leave_session(&format!("the host's project could not be read: {e}"));
                    return;
                }
                if let Some(Side::Peer(p)) = &mut self.collab.side {
                    p.seq = w.seq;
                }
                let host = w
                    .participants
                    .iter()
                    .find(|q| q.host)
                    .map_or_else(|| "the host".into(), |q| q.name.clone());
                self.notify(NoticeLevel::Info, format!("Joined {host}’s live session"));
            } else {
                // Proposals the host answered are in its project or were
                // refused: either way not done again on top of it.
                if let Some(Side::Peer(p)) = &mut self.collab.side {
                    p.pending.retain(|x| x.local > w.answered);
                }
                self.peer_snapshot(w.seq, snapshot);
            }
        } else {
            for e in w.entries {
                self.peer_entry(e);
            }
        }
        // Answered, but neither applied nor refused as far as seen: refused
        // while the connection was down.
        let lost: Vec<u64> = match &self.collab.side {
            Some(Side::Peer(p)) => p
                .pending
                .iter()
                .filter(|x| x.local <= w.answered)
                .map(|x| x.local)
                .collect(),
            _ => Vec::new(),
        };
        for local in lost {
            self.peer_reject(local, "the host could not apply it while you were away");
        }
        // The rest again, on this connection (and listening again).
        if let Some(Side::Peer(p)) = &mut self.collab.side {
            for x in &mut p.pending {
                x.sent = false;
            }
            if self.collab.listen.listening()
                && let Some(c) = &p.client
            {
                c.send(&ToHost::Listen(true));
            }
        }
        self.send_proposals();
        self.fetch_new_media();
        self.revision += 1;
    }

    /// The host's project as this session's (the first Welcome).
    fn take_first_project(&mut self, snapshot: &[u8]) -> std::result::Result<(), String> {
        // Media the project names go to a new scratch folder (saving moves
        // them like any media).
        self.discard_unsaved_media();
        self.media_dir = crate::media::new_unsaved_media_dir();
        self.unsaved_media = true;
        self.path = None;
        let media_dir = self.media_dir.clone();
        let collab = &mut self.collab;
        let mut project: Project =
            form::from_wire(snapshot, &mut |k, n| collab.resolve(&media_dir, None, k, n))
                .map_err(|e| e.to_string())?;
        let notes = project.repair();
        for n in notes {
            tracing::info!("collaboration: the host's project: {n}");
        }
        self.merging.set_lineage(None);
        self.replace_project(project, None)
            .map_err(|e| e.to_string())?;
        self.collab.granted_until = self.collab.granted_until.max(self.project.ids.peek());
        self.project.journal.start();
        self.after_remote();
        self.collab.remote_unsaved = true;
        self.mark_fetching_sources();
        Ok(())
    }

    /// The whole project again (a resync, or a peer back after the host's
    /// log moved on): own pending steps on top.
    fn peer_snapshot(&mut self, seq: u64, snapshot: &[u8]) {
        let media_dir = self.media_dir.clone();
        let collab = &mut self.collab;
        let project: std::result::Result<Project, _> =
            form::from_wire(snapshot, &mut |k, n| collab.resolve(&media_dir, None, k, n));
        let mut project = match project {
            Ok(p) => p,
            Err(e) => {
                self.notify(
                    NoticeLevel::Error,
                    format!("Live session: the host's project could not be read ({e})"),
                );
                return;
            }
        };
        project.repair();
        let journal = std::mem::take(&mut self.project.journal);
        let pending = match &mut self.collab.side {
            Some(Side::Peer(p)) => {
                p.seq = seq;
                std::mem::take(&mut p.pending)
            }
            _ => return,
        };
        self.project = project;
        self.project.journal = journal;
        self.collab.granted_until = self.collab.granted_until.max(self.project.ids.peek());
        self.after_remote();
        let kept = self.redo_pending(pending);
        if let Some(Side::Peer(p)) = &mut self.collab.side {
            p.pending = kept;
        }
        self.collab.remote_unsaved = true;
        self.mark_fetching_sources();
        if let Err(e) = self.sync(Impact::Graph) {
            tracing::warn!("collaboration: {e}");
        }
        self.fetch_new_media();
    }

    /// Do pending steps again (fresh inverses); those that no longer apply
    /// go, and the person is told.
    fn redo_pending(&mut self, pending: VecDeque<Pending>) -> VecDeque<Pending> {
        let journal = std::mem::take(&mut self.project.journal);
        let mut kept = VecDeque::with_capacity(pending.len());
        let mut dropped = Vec::new();
        for mut p in pending {
            match apply_all(&mut self.project, p.forwards()) {
                Ok((_, applied)) => {
                    p.applied = applied;
                    kept.push_back(p);
                }
                Err(reason) => dropped.push((p.label, reason)),
            }
        }
        self.project.journal = journal;
        for (label, reason) in dropped {
            self.notify(
                NoticeLevel::Warning,
                format!(
                    "Your ‘{label}’ could not be kept: someone else changed it first ({reason})"
                ),
            );
        }
        kept
    }

    fn peer_entry(&mut self, e: Entry) {
        let (seq, me) = match &self.collab.side {
            Some(Side::Peer(p)) => (p.seq, p.me),
            _ => return,
        };
        if e.seq <= seq {
            return;
        }
        if e.seq != seq + 1 {
            self.ask_resync();
            return;
        }
        // Own edits waiting in the journal are pending first.
        self.collab_send_local();
        match e.body {
            EntryBody::Grant { peer, start, end } => {
                self.collab.granted_until = self.collab.granted_until.max(end);
                self.project.ids.reserve_through(end.saturating_sub(1));
                if Some(peer) == me {
                    self.collab.lease.grant(start, end);
                    if let Some(Side::Peer(p)) = &mut self.collab.side {
                        p.asked_lease = false;
                    }
                }
            }
            EntryBody::Step { label, step } => {
                let media_dir = self.media_dir.clone();
                let collab = &mut self.collab;
                let cmds: std::result::Result<Vec<Command>, _> =
                    form::from_wire(&step, &mut |k, n| collab.resolve(&media_dir, None, k, n));
                let Ok(cmds) = cmds else {
                    self.ask_resync();
                    return;
                };
                let own = Some(e.author) == me;
                if own {
                    // Older own steps never answered: lost.
                    let gone: Vec<u64> = match &self.collab.side {
                        Some(Side::Peer(p)) => p
                            .pending
                            .iter()
                            .filter(|x| x.local < e.local)
                            .map(|x| x.local)
                            .collect(),
                        _ => Vec::new(),
                    };
                    for local in gone {
                        self.peer_reject(local, "it was lost on the way");
                    }
                }
                let front_matches = match &self.collab.side {
                    Some(Side::Peer(p)) => {
                        p.pending.front().is_some_and(|x| own && x.local == e.local)
                    }
                    _ => false,
                };
                if front_matches {
                    let same = match &self.collab.side {
                        Some(Side::Peer(p)) => {
                            p.pending.front().is_some_and(|x| x.forwards() == cmds)
                        }
                        _ => false,
                    };
                    if same {
                        if let Some(Side::Peer(p)) = &mut self.collab.side {
                            p.pending.pop_front();
                        }
                    } else {
                        self.rebase(cmds.clone(), true);
                    }
                } else {
                    if !own && self.collab.overridden(&cmds) {
                        let who = self.person_name(e.author);
                        self.notify(
                            NoticeLevel::Info,
                            format!("{who} changed what you had just changed: {label}"),
                        );
                    }
                    self.rebase(cmds.clone(), false);
                    if let Some(Side::Peer(p)) = &mut self.collab.side {
                        p.last_edit.insert(e.author, label);
                    }
                }
                if replaces_project(&cmds) {
                    // New id ranges follow from the host.
                    self.collab.lease = Arc::new(IdLease::default());
                    self.collab.granted_until = self.project.ids.peek();
                    self.after_remote();
                }
                self.collab.remote_unsaved |= !own;
                self.fetch_new_media();
            }
        }
        if let Some(Side::Peer(p)) = &mut self.collab.side {
            p.seq = e.seq;
        }
        self.revision += 1;
    }

    fn person_name(&self, peer: PeerId) -> String {
        match &self.collab.side {
            Some(Side::Peer(p)) => p
                .people
                .iter()
                .find(|q| q.peer == peer)
                .map_or_else(|| "Someone".into(), |q| q.name.clone()),
            Some(Side::Host(h)) => h
                .guests
                .get(&peer)
                .map_or_else(|| "Someone".into(), |g| g.name.clone()),
            None => "Someone".into(),
        }
    }

    /// Take the pending steps back, apply `cmds`, do the pending steps
    /// again (`own`: `cmds` are the oldest pending step's, as the host
    /// applied them).
    fn rebase(&mut self, cmds: Vec<Command>, own: bool) {
        let mut pending = match &mut self.collab.side {
            Some(Side::Peer(p)) => std::mem::take(&mut p.pending),
            _ => return,
        };
        let journal = std::mem::take(&mut self.project.journal);
        let mut impact = Impact::None;
        let mut broken = false;
        for p in pending.iter().rev() {
            impact = impact.max(impact_of(&p.applied));
            broken |= !take_back(&mut self.project, &p.applied);
        }
        if own {
            pending.pop_front();
        }
        match apply_all(&mut self.project, cmds) {
            Ok((i, _)) => impact = impact.max(i),
            Err(e) => {
                tracing::warn!("collaboration: an entry did not apply here: {e}");
                broken = true;
            }
        }
        self.project.journal = journal;
        self.after_remote();
        let kept = self.redo_pending(pending);
        for p in &kept {
            impact = impact.max(impact_of(&p.applied));
        }
        if let Some(Side::Peer(p)) = &mut self.collab.side {
            p.pending = kept;
        }
        self.mark_fetching_sources();
        if let Err(e) = self.sync(impact) {
            tracing::warn!("collaboration: {e}");
        }
        if broken {
            self.ask_resync();
        }
    }

    fn peer_reject(&mut self, local: u64, reason: &str) {
        let Some(Side::Peer(p)) = &mut self.collab.side else {
            return;
        };
        let Some(i) = p.pending.iter().position(|x| x.local == local) else {
            return;
        };
        let mut later: VecDeque<Pending> = p.pending.split_off(i);
        let Some(refused) = later.pop_front() else {
            return;
        };
        let journal = std::mem::take(&mut self.project.journal);
        let mut impact = impact_of(&refused.applied);
        for x in later.iter().rev() {
            impact = impact.max(impact_of(&x.applied));
            take_back(&mut self.project, &x.applied);
        }
        take_back(&mut self.project, &refused.applied);
        self.project.journal = journal;
        let kept = self.redo_pending(later);
        if let Some(Side::Peer(p)) = &mut self.collab.side {
            p.pending.extend(kept);
        }
        self.notify(
            NoticeLevel::Warning,
            format!(
                "The host could not apply your ‘{}’: {reason}",
                refused.label
            ),
        );
        if let Err(e) = self.sync(impact) {
            tracing::warn!("collaboration: {e}");
        }
    }

    fn ask_resync(&mut self) {
        if let Some(Side::Peer(p)) = &mut self.collab.side
            && p.resync_asked.is_none_or(|t| t.elapsed() > RESYNC_EVERY)
            && let Some(c) = &p.client
        {
            tracing::info!("collaboration: asking the host for the whole project");
            c.send(&ToHost::Resync);
            p.resync_asked = Some(Instant::now());
        }
    }

    fn peer_checksum(&mut self, seq: u64, hash: [u8; 32]) {
        let check = match &self.collab.side {
            Some(Side::Peer(p)) => {
                p.seq == seq && p.pending.is_empty() && self.project.journal.is_empty()
            }
            _ => false,
        };
        if !check {
            return;
        }
        match form::canonical_hash(&self.project, &mut self.collab.keys) {
            Ok(h) if h == hash => {}
            Ok(_) => {
                tracing::warn!(
                    "collaboration: the project here differs from the host's at entry {seq}"
                );
                self.ask_resync();
            }
            Err(e) => tracing::warn!("collaboration: {e}"),
        }
    }

    // --- media ------------------------------------------------------------------

    /// Ask for the files steps named that are not here.
    fn fetch_new_media(&mut self) {
        let wanted: Vec<(String, PathBuf, Option<ConnId>)> = self
            .collab
            .fetching
            .iter()
            .map(|(k, f)| (k.clone(), f.dest.clone(), f.from))
            .collect();
        match &self.collab.side {
            Some(Side::Host(h)) => {
                for (k, dest, from) in wanted {
                    if let Some(c) = from {
                        h.host.fetch(c, &k, &dest);
                    }
                }
            }
            Some(Side::Peer(p)) => {
                if let Some(c) = &p.client
                    && p.link == PeerLink::Joined
                {
                    for (k, dest, _) in wanted {
                        c.fetch(&k, &dest);
                    }
                }
            }
            None => {}
        }
        self.mark_fetching_sources();
    }

    /// Sources whose files are on their way are not "offline".
    fn mark_fetching_sources(&mut self) {
        let coming: BTreeSet<&Path> = self
            .collab
            .fetching
            .values()
            .map(|f| f.dest.as_path())
            .collect();
        for s in self.project.sources.values() {
            if let faderframe_project::SourceSpec::File { path, .. } = &s.spec
                && coming.contains(path.as_path())
            {
                self.missing.insert(s.id);
            }
        }
    }

    fn media_fetched(&mut self, f: Fetched) {
        match f {
            Fetched::Progress { key, done, len } => {
                if let Some(x) = self.collab.fetching.get_mut(&key) {
                    x.done = done;
                    x.len = len;
                }
                self.revision += 1;
            }
            Fetched::Arrived { key, path } => {
                self.collab.fetching.remove(&key);
                self.collab.keys.arrived(&path);
                self.collab.have.insert(key.clone(), path.clone());
                if let Some(Side::Host(h)) = &mut self.collab.side {
                    h.host.offer_media(&key, &path);
                    for (conn, from) in h.waiting.remove(&key).unwrap_or_default() {
                        h.host.serve(conn, &key, &path, from);
                    }
                }
                // The sources it plays open now.
                let ids: Vec<_> = self
                    .project
                    .sources
                    .values()
                    .filter(|s| matches!(&s.spec, faderframe_project::SourceSpec::File { path: p, .. } if *p == path))
                    .map(|s| s.id)
                    .collect();
                for id in ids {
                    self.missing.remove(&id);
                }
                if let Err(e) = self.sync(Impact::Timeline) {
                    tracing::warn!("collaboration: {e}");
                }
                self.reload_samplers_naming(&path);
                self.revision += 1;
            }
            Fetched::Failed { key, reason } => {
                let name = self
                    .collab
                    .fetching
                    .remove(&key)
                    .map_or_else(|| key.clone(), |f| f.name);
                if let Some(Side::Host(h)) = &mut self.collab.side {
                    for (conn, _) in h.waiting.remove(&key).unwrap_or_default() {
                        h.host.missing(conn, &key);
                    }
                }
                self.collab.failed_media.push(name.clone());
                self.notify(
                    NoticeLevel::Warning,
                    format!("Live session: {name} could not be fetched ({reason})"),
                );
            }
        }
    }

    /// Samplers whose state names a file that just arrived load it.
    fn reload_samplers_naming(&mut self, path: &Path) {
        let mut slots = Vec::new();
        for t in &self.project.tracks {
            for s in t.slots() {
                if crate::samples::is_sampler(s)
                    && crate::samples::doc_of(s)
                        .files
                        .iter()
                        .flatten()
                        .any(|f| Path::new(f) == path)
                {
                    slots.push(s.id);
                }
            }
        }
        if slots.is_empty() {
            return;
        }
        for id in slots {
            self.engine.reload_plugin(id);
        }
        if let Err(e) = self.sync(Impact::Graph) {
            tracing::warn!("collaboration: {e}");
        }
    }

    // --- presence and plug-ins ----------------------------------------------------

    fn presence_now(&self) -> Presence {
        Presence {
            playhead: self.playhead(),
            playing: self.transport.playing,
            tracks: self.selection.tracks.iter().copied().collect(),
            clips: self.selection.clips.iter().copied().collect(),
            range: self.selection.range.map(|r| (r.start, r.end)),
            editing: self.editor_clip,
        }
    }

    fn presence_tick(&mut self) {
        if self
            .collab
            .presence_sent
            .as_ref()
            .is_some_and(|(_, t)| t.elapsed() < PRESENCE_EVERY)
        {
            return;
        }
        let now = self.presence_now();
        if self
            .collab
            .presence_sent
            .as_ref()
            .is_some_and(|(p, _)| *p == now)
        {
            return;
        }
        let Ok(data) = rmp_serde::to_vec_named(&now) else {
            return;
        };
        match &self.collab.side {
            Some(Side::Host(h)) => {
                if let Ok(m) = faderframe_collab::wire::encode(&ToPeer::Presence { peer: 0, data })
                {
                    for &c in h.conns.keys() {
                        h.host.send_presence(c, 0, m.clone());
                    }
                }
            }
            Some(Side::Peer(p)) => {
                if p.link == PeerLink::Joined
                    && let Some(c) = &p.client
                {
                    c.send_presence(&ToHost::Presence(data));
                }
            }
            None => return,
        }
        self.collab.presence_sent = Some((now, Instant::now()));
    }

    /// Tell about plug-ins the project uses that are not installed here
    /// (once each).
    fn check_plugins(&mut self) {
        let used: BTreeMap<String, String> = self
            .project
            .tracks
            .iter()
            .flat_map(|t| t.slots().into_iter().map(move |s| (t, s)))
            .map(|(t, s)| {
                (
                    format!("{:?}:{}", s.plugin.format, s.plugin.id),
                    format!("{} on {}", s.plugin.name, t.name),
                )
            })
            .collect();
        if used.keys().all(|k| self.collab.plugins_seen.contains(k)) {
            return;
        }
        let here: BTreeSet<String> = self
            .engine
            .available_plugins()
            .into_iter()
            .map(|d| {
                let format = match d.format {
                    faderframe_plugin_host::PluginFormat::Builtin => {
                        faderframe_project::PluginFormat::Builtin
                    }
                    faderframe_plugin_host::PluginFormat::Clap => {
                        faderframe_project::PluginFormat::Clap
                    }
                    faderframe_plugin_host::PluginFormat::Vst3 => {
                        faderframe_project::PluginFormat::Vst3
                    }
                    faderframe_plugin_host::PluginFormat::AudioUnit => {
                        faderframe_project::PluginFormat::AudioUnit
                    }
                    faderframe_plugin_host::PluginFormat::Lv2 => {
                        faderframe_project::PluginFormat::Lv2
                    }
                };
                format!("{format:?}:{}", d.id)
            })
            .collect();
        let mut new = Vec::new();
        for (k, what) in used {
            if self.collab.plugins_seen.insert(k.clone()) && !here.contains(&k) {
                new.push(what);
            }
        }
        if !new.is_empty() {
            self.notify(
                NoticeLevel::Warning,
                format!(
                    "Not installed here: {}. Their settings are kept (the others hear them); \
                     the tracks play without them here — Listen Along plays the host's mix",
                    new.join(", ")
                ),
            );
            self.collab.missing_plugins.extend(new);
            self.revision += 1;
        }
    }
}

/// What a dev action asks for: `host` (every interface, the default
/// port), `host@<port>`, `host@<ip>:<port>`, `join=<address>/<code>`,
/// `leave`, `chat=<text>`, `listen`, `listen-off`, `invite`, `resync`.
pub fn parse_dev(arg: &str, name: &str) -> Option<CollabOp> {
    let arg = arg.trim();
    if arg == "host" {
        return Some(CollabOp::Host {
            bind: None,
            name: name.into(),
        });
    }
    if let Some(at) = arg.strip_prefix("host@") {
        let at = at.trim();
        let bind = match at.parse::<u16>() {
            Ok(port) => std::net::SocketAddr::from(([0, 0, 0, 0], port)),
            Err(_) => at.parse().ok()?,
        };
        return Some(CollabOp::Host {
            bind: Some(bind),
            name: name.into(),
        });
    }
    if let Some(rest) = arg.strip_prefix("join=") {
        let (address, code) = parse_invite(rest)?;
        return Some(CollabOp::Join {
            address,
            code,
            name: name.into(),
        });
    }
    if let Some(text) = arg.strip_prefix("chat=") {
        return Some(CollabOp::Chat(text.into()));
    }
    match arg {
        "leave" => Some(CollabOp::Leave),
        "listen" => Some(CollabOp::ListenAlong(true)),
        "listen-off" => Some(CollabOp::ListenAlong(false)),
        "invite" => Some(CollabOp::NewInvite),
        "resync" => Some(CollabOp::Resync),
        _ => None,
    }
}

/// The hash of a project's canonical form ([`form::canonical_hash`]):
/// alike for projects alike wherever their media are (tests compare
/// sessions with it). Hashes media files on this thread.
pub fn project_hash(project: &Project) -> Option<[u8; 32]> {
    form::canonical_hash(project, &mut form::Keys::inline()).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn invitations_and_dev_requests_read() {
        let code = InviteCode::new().unwrap().to_string();
        assert_eq!(
            parse_invite(&format!("192.168.1.20:47400/{code}")),
            Some(("192.168.1.20:47400".into(), code.clone()))
        );
        assert_eq!(parse_invite("nonsense"), None);
        assert_eq!(
            parse_dev("host@5000", "Ana"),
            Some(CollabOp::Host {
                bind: Some(std::net::SocketAddr::from(([0, 0, 0, 0], 5000))),
                name: "Ana".into()
            })
        );
        assert_eq!(
            parse_dev("host@127.0.0.1:0", "Ana"),
            Some(CollabOp::Host {
                bind: Some(std::net::SocketAddr::from(([127, 0, 0, 1], 0))),
                name: "Ana".into()
            })
        );
        assert_eq!(
            parse_dev(&format!("join=[::1]:5000/{code}"), "Bo"),
            Some(CollabOp::Join {
                address: "[::1]:5000".into(),
                code,
                name: "Bo".into()
            })
        );
        assert_eq!(parse_dev("leave", ""), Some(CollabOp::Leave));
    }

    #[test]
    fn neighbouring_setters_of_one_thing_cross_as_one() {
        let t = TrackId(3);
        let a = |db: f32, old: f32| Applied {
            forward: Command::SetTrackVolume { track: t, db },
            inverse: Command::SetTrackVolume { track: t, db: old },
        };
        let out = coalesce(vec![a(-1.0, 0.0), a(-2.0, -1.0), a(-3.0, -2.0)]);
        assert_eq!(out, vec![a(-3.0, 0.0)]);
        let pan = Applied {
            forward: Command::SetTrackPan { track: t, pan: 0.5 },
            inverse: Command::SetTrackPan { track: t, pan: 0.0 },
        };
        assert_eq!(
            coalesce(vec![a(-1.0, 0.0), pan.clone(), a(-2.0, -1.0)]).len(),
            3
        );
    }
}
