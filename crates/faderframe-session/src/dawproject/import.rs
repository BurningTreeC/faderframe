//! A DAWproject document as a new project.
//!
//! Tracks keep their nesting (a folder track with a submix channel is a
//! folder bus, without one a folder), channels their volume, pan, mute,
//! solo, sends, output and devices. The arrangement's timelines are
//! followed wherever they nest — clips in clips (Bitwig's audio clips),
//! lanes, warps, loops played out, linked clips — each content time taken
//! to the project's beats through the tempo map ([`Clock`]), so audio in
//! seconds keeps its samples and notes in beats their beats. Automation
//! finds its parameter by id wherever it is; values are taken to
//! FaderFrame's units (dB faders, −1…1 pan, plug-ins' plain values).
//! What has no place is listed in the notes, never silently dropped.

use super::{Loaded, ids};
use faderframe_automation::{
    AutomationCurve, AutomationLane, AutomationMode, AutomationPoint, AutomationTarget, CurveShape,
};
use faderframe_core::gain::gain_to_db;
use faderframe_core::{
    AudioSourceId, ChannelLayout, ClipId, ParameterId, PluginInstanceId, SendId, TrackId,
};
use faderframe_interchange::dawproject as dp;
use faderframe_interchange::presets;
use faderframe_project::{
    AudioClip, AudioSource, AuxSend, Clip, ClipContent, ClipFades, ControllerLane, ControllerPoint,
    ExpressionKind, ExpressionPoint, MAX_LEVEL_DB, Marker, MidiClip, MidiController, MidiNote,
    NoteExpression, OutputRouting, PluginFormat, PluginRef, PluginSlot, Project, SavedParameter,
    Section, SendTap, SourceSpec, Track, TrackColor, TrackKind, Warp, WarpAlgorithm, WarpMarker,
};
use faderframe_timeline::{
    MeterChange, MusicalTime, TempoCurve, TempoPoint, TimeSignature, Timeline,
};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::rc::Rc;

/// Beats and seconds of the imported tempo map.
struct Clock {
    tl: Timeline,
    rate: f64,
}

impl Clock {
    fn secs(&self, beats: f64) -> f64 {
        self.tl
            .tempo
            .musical_to_seconds(MusicalTime::from_quarters(beats))
    }

    fn beats_at(&self, secs: f64) -> f64 {
        self.tl.tempo.seconds_to_quarters(secs)
    }

    /// `by` content units on from `from` (beats).
    fn advance(&self, from: f64, by: f64, unit: dp::TimeUnit) -> f64 {
        match unit {
            dp::TimeUnit::Beats => from + by,
            dp::TimeUnit::Seconds => self.beats_at(self.secs(from) + by),
        }
    }

    fn samples(&self, beats: f64) -> f64 {
        self.secs(beats) * self.rate
    }
}

/// Where a timeline's local times are on the arrangement.
#[derive(Clone)]
struct Ctx {
    unit: dp::TimeUnit,
    to_abs: Rc<dyn Fn(f64) -> f64>,
    /// The span heard (beats on the arrangement).
    window: (f64, f64),
    gain_db: f64,
    muted: bool,
    /// Fades (seconds) at the window's edges.
    fade_in: f64,
    fade_out: f64,
}

/// A note on the arrangement.
struct NotePiece {
    start: f64,
    end: f64,
    key: u8,
    channel: u8,
    vel: Option<f64>,
    rel: Option<f64>,
    expression: Vec<(ExpressionKind, Vec<(f64, f32)>)>,
}

/// An audio piece: a file between window edges, its anchors (sample on
/// the arrangement, frame of the file).
struct AudioPiece {
    file: String,
    anchors: Vec<(f64, f64)>,
    window: (f64, f64),
    gain_db: f64,
    fade_in: f64,
    fade_out: f64,
    muted: bool,
    algorithm: WarpAlgorithm,
    name: Option<String>,
}

/// What one clip on a track (or a track's loose timelines) holds.
#[derive(Default)]
struct Group {
    name: Option<String>,
    color: Option<TrackColor>,
    muted: bool,
    window: Option<(f64, f64)>,
    notes: Vec<NotePiece>,
    controllers: Vec<(f64, MidiController, u8, ControllerPoint)>,
    audio: Vec<AudioPiece>,
    /// The timeline it plays (linked clips share it).
    link: Option<String>,
}

/// A parameter of the document, by its id.
#[derive(Clone, Debug)]
enum ParamRef {
    Volume(TrackId),
    Pan(TrackId),
    Mute(TrackId),
    SendVolume(TrackId, SendId),
    /// A send's pan or switch (FaderFrame automates neither).
    SendPanOrSwitch,
    DeviceOn(TrackId, PluginInstanceId),
    DeviceParam {
        track: TrackId,
        plugin: PluginInstanceId,
        parameter: ParameterId,
        /// A normalised value's range.
        range: Option<(f64, f64)>,
    },
}

#[derive(Default)]
struct Notes(BTreeMap<String, usize>);

impl Notes {
    fn add(&mut self, what: impl Into<String>) {
        *self.0.entry(what.into()).or_default() += 1;
    }

    fn list(self) -> Vec<String> {
        self.0
            .into_iter()
            .map(|(w, n)| if n == 1 { w } else { format!("{w} (×{n})") })
            .collect()
    }
}

fn parse_color(c: Option<&String>) -> Option<TrackColor> {
    c.and_then(|c| TrackColor::from_hex(c.trim()))
}

/// A volume parameter's value in dB.
fn volume_db(p: &dp::RealParameter, v: f64) -> f32 {
    let db = match p.unit {
        dp::Unit::Decibel => v as f32,
        dp::Unit::Normalized => {
            let max = p.max.filter(|m| *m > 0.0).unwrap_or(1.0);
            gain_to_db((v * max) as f32)
        }
        dp::Unit::Percent => gain_to_db((v / 100.0) as f32),
        _ => gain_to_db(v as f32),
    };
    db.min(MAX_LEVEL_DB)
}

/// A pan parameter's value as −1…1.
fn pan_value(unit: Option<dp::Unit>, min: Option<f64>, v: f64) -> f32 {
    let v = match unit {
        Some(dp::Unit::Percent) => v / 100.0,
        Some(dp::Unit::Normalized) => v * 2.0 - 1.0,
        _ if min.is_some_and(|m| m < 0.0) => v,
        // A 0…1 range written in another unit (Bitwig writes "normalized").
        _ if min == Some(0.0) => v * 2.0 - 1.0,
        _ => v,
    };
    (v as f32).clamp(-1.0, 1.0)
}

/// A 0…1 velocity: MIDI 1.0's when it is one of its steps, else 16 bits.
fn velocity_of(v: f64) -> (u8, Option<u16>) {
    let v = v.clamp(0.0, 1.0);
    let s = v * 127.0;
    if (s - s.round()).abs() < 5e-4 {
        (s.round() as u8, None)
    } else {
        (
            ((v * 65535.0).round() as u16 >> 9) as u8,
            Some((v * 65535.0).round() as u16),
        )
    }
}

/// A controller's 0…1 value at its resolution.
fn controller_point(time: MusicalTime, c: MidiController, unit: f64) -> ControllerPoint {
    let unit = unit.clamp(0.0, 1.0);
    let steps = unit * f64::from(c.max());
    if (steps - steps.round()).abs() < 5e-4 || !c.has_fine() {
        ControllerPoint::new(time, steps.round() as u16)
    } else {
        ControllerPoint::at32(time, c, (unit * f64::from(u32::MAX)).round() as u32)
    }
}

struct Importer<'a> {
    loaded: &'a Loaded,
    p: Project,
    clock: Clock,
    notes: Notes,
    /// Document track/channel ids → tracks.
    tracks: HashMap<String, TrackId>,
    params: HashMap<String, ParamRef>,
    /// Timelines by id (clips that refer to one play it).
    timelines: HashMap<String, &'a dp::Timeline>,
    /// Imported media by its path in the document.
    sources: HashMap<String, (AudioSourceId, i64)>,
    /// Linked clips by the timeline they play.
    links: BTreeMap<String, Vec<ClipId>>,
    /// Where each channel plays (resolved once every track exists).
    destinations: Vec<(TrackId, Option<String>, bool)>,
    sends: Vec<(TrackId, dp::Send)>,
    /// The document had a master channel.
    master_placed: bool,
    /// Tracks with an instrument among their devices.
    instrumented: BTreeSet<TrackId>,
}

impl<'a> Importer<'a> {
    fn index_timeline(&mut self, t: &'a dp::Timeline) {
        if let Some(id) = &t.common().meta.id {
            self.timelines.insert(id.clone(), t);
        }
        let clip = |c: &'a dp::Clip, me: &mut Self| {
            if let Some(x) = &c.content {
                me.index_timeline(x);
            }
        };
        match t {
            dp::Timeline::Lanes(l) => {
                for x in &l.lanes {
                    self.index_timeline(x);
                }
            }
            dp::Timeline::Clips(c) => {
                for k in &c.clips {
                    clip(k, self);
                }
            }
            dp::Timeline::Warps(w) => {
                if let Some(x) = &w.content {
                    self.index_timeline(x);
                }
            }
            dp::Timeline::ClipSlot(s) => {
                if let Some(k) = &s.clip {
                    clip(k, self);
                }
            }
            dp::Timeline::Notes(_)
            | dp::Timeline::Points(_)
            | dp::Timeline::Audio(_)
            | dp::Timeline::Video(_)
            | dp::Timeline::Markers(_) => {}
        }
    }

    // --- structure ---------------------------------------------------------

    fn device_slot(&mut self, track: TrackId, d: &dp::Device) -> Option<PluginSlot> {
        let id: PluginInstanceId = self.p.ids.allocate();
        let state_bytes = d.state.as_ref().and_then(|s| {
            let bytes = self.loaded.states.get(&s.path).cloned();
            if bytes.is_none() {
                self.notes
                    .add(format!("a device's state file is missing ({})", s.path));
            }
            bytes
        });
        let device_id = d.device_id.clone().unwrap_or_default();
        let builtin =
            d.vendor.as_deref() == Some(ids::VENDOR) || device_id.starts_with("faderframe.");
        let (plugin, state) = match d.kind {
            dp::DeviceKind::Clap => {
                let state = state_bytes.map(|b| match presets::read_clap_preset(&b) {
                    Some((pid, s)) => {
                        if pid != device_id {
                            self.notes.add(format!(
                                "a CLAP state saved by {pid} was given to {device_id}"
                            ));
                        }
                        s.to_vec()
                    }
                    None => b,
                });
                (
                    PluginRef {
                        format: PluginFormat::Clap,
                        id: device_id,
                        name: d.device_name.clone(),
                    },
                    state,
                )
            }
            dp::DeviceKind::Vst3 => {
                let Some(class) = presets::class_of_uuid(&device_id) else {
                    self.notes.add(format!(
                        "a VST3 plug-in without a class id ({})",
                        d.device_name
                    ));
                    return None;
                };
                let state = state_bytes.map(|b| match presets::read_vst_preset(&b) {
                    Some(v) => ids::pack_vst3_state(&v.component, &v.controller),
                    None => b,
                });
                (
                    PluginRef {
                        format: PluginFormat::Vst3,
                        id: ids::vst3_raw(&class),
                        name: d.device_name.clone(),
                    },
                    state,
                )
            }
            dp::DeviceKind::Au => (
                PluginRef {
                    format: PluginFormat::AudioUnit,
                    id: device_id,
                    name: d.device_name.clone(),
                },
                state_bytes,
            ),
            dp::DeviceKind::Device if device_id.starts_with(ids::LV2_PREFIX) => (
                PluginRef {
                    format: PluginFormat::Lv2,
                    id: device_id[ids::LV2_PREFIX.len()..].to_string(),
                    name: d.device_name.clone(),
                },
                state_bytes,
            ),
            dp::DeviceKind::Builtin | dp::DeviceKind::Device if builtin => {
                (PluginRef::builtin(&device_id, &d.device_name), state_bytes)
            }
            dp::DeviceKind::Vst2 => {
                self.notes.add(format!(
                    "VST2 plug-ins are not hosted ({} left out)",
                    d.device_name
                ));
                return None;
            }
            other => {
                self.notes.add(format!(
                    "{}'s own devices have no FaderFrame form ({} <{}> left out)",
                    if self.loaded.doc.application.name.is_empty() {
                        "the other program"
                    } else {
                        self.loaded.doc.application.name.as_str()
                    },
                    d.device_name,
                    other.element()
                ));
                return None;
            }
        };
        let mut parameters = Vec::new();
        for p in &d.parameters {
            let Some(pid) = p.parameter_id() else {
                continue;
            };
            let parameter = ParameterId(pid as u32);
            let range = match p {
                dp::Parameter::Real(r) if r.unit == dp::Unit::Normalized => r
                    .min
                    .zip(r.max)
                    .filter(|(a, b)| b > a && (*a, *b) != (0.0, 1.0)),
                _ => None,
            };
            if let Some(v) = p.number() {
                let v = range.map_or(v, |(a, b)| a + v * (b - a));
                if v.is_finite() {
                    parameters.push(SavedParameter {
                        id: parameter,
                        value: v,
                    });
                }
            }
            if let Some(pid) = &p.meta().id {
                self.params.insert(
                    pid.clone(),
                    ParamRef::DeviceParam {
                        track,
                        plugin: id,
                        parameter,
                        range,
                    },
                );
            }
        }
        if let Some(e) = d.enabled.as_ref().and_then(|e| e.meta.id.clone()) {
            self.params.insert(e, ParamRef::DeviceOn(track, id));
        }
        Some(PluginSlot {
            id,
            plugin,
            bypass: d.enabled.as_ref().and_then(|e| e.value) == Some(false),
            parameters,
            state: state.map(|s| faderframe_engine::encode_state(&s)),
            sidechain: None,
            mono_to_stereo: false,
            mix_origin: None,
        })
    }

    /// A track from a document track (or a channel standing alone).
    fn track(
        &mut self,
        t: Option<&'a dp::Track>,
        c: Option<&'a dp::Channel>,
        folder: Option<TrackId>,
    ) {
        let role = c.and_then(|c| c.role);
        let types: &[dp::ContentType] = t.map_or(&[], |t| &t.content_types);
        let holds_tracks =
            t.is_some_and(|t| !t.tracks.is_empty() || types.contains(&dp::ContentType::Tracks));
        let notes = types.contains(&dp::ContentType::Notes);
        let name = t
            .and_then(|t| t.meta.name.clone())
            .or_else(|| c.and_then(|c| c.meta.name.clone()))
            .unwrap_or_else(|| "Track".into());
        let color = parse_color(t.and_then(|t| t.meta.color.as_ref()))
            .or_else(|| parse_color(c.and_then(|c| c.meta.color.as_ref())))
            .unwrap_or_else(|| TrackColor::palette(self.p.tracks.len()));
        let master = role == Some(dp::MixerRole::Master);
        let kind = if master {
            TrackKind::Master
        } else if holds_tracks {
            if c.is_some_and(|c| {
                matches!(
                    c.role,
                    Some(dp::MixerRole::Submix) | Some(dp::MixerRole::Regular) | None
                ) && (c.volume.is_some() || !c.devices.is_empty())
            }) {
                TrackKind::Bus
            } else {
                TrackKind::Folder
            }
        } else {
            match role {
                Some(dp::MixerRole::Effect) => TrackKind::Aux,
                Some(dp::MixerRole::Submix) => TrackKind::Bus,
                Some(dp::MixerRole::Vca) => TrackKind::Vca,
                _ if notes && !types.contains(&dp::ContentType::Audio) => TrackKind::Instrument,
                _ => TrackKind::Audio,
            }
        };
        let id: TrackId = if master {
            match self.p.master_id() {
                Some(m) => m,
                None => self.p.ids.allocate(),
            }
        } else {
            self.p.ids.allocate()
        };
        let mut track = if master {
            self.p
                .tracks
                .iter()
                .find(|t| t.id == id)
                .cloned()
                .unwrap_or_else(|| Track::new(id, TrackKind::Master, name.clone(), color))
        } else {
            Track::new(id, kind, name.clone(), color)
        };
        if !master {
            track.name = name;
            track.color = color;
        }
        track.folder = folder;
        track.folder_bus = kind == TrackKind::Bus && holds_tracks;
        if let Some(t) = t
            && let Some(tid) = &t.meta.id
        {
            self.tracks.insert(tid.clone(), id);
        }
        if let Some(c) = c {
            if let Some(cid) = &c.meta.id {
                self.tracks.insert(cid.clone(), id);
            }
            if let Some(n) = c.audio_channels.filter(|n| *n > 0)
                && kind.has_audio()
            {
                track.layout = ChannelLayout::from_channel_count(n as usize);
            }
            if let Some(v) = &c.volume {
                if let Some(x) = v.value {
                    track.volume_db = volume_db(v, x);
                }
                if let Some(vid) = &v.meta.id {
                    self.params.insert(vid.clone(), ParamRef::Volume(id));
                }
            }
            if let Some(p) = &c.pan {
                if let Some(x) = p.value {
                    track.pan = pan_value(Some(p.unit), p.min, x);
                }
                if let Some(pid) = &p.meta.id {
                    self.params.insert(pid.clone(), ParamRef::Pan(id));
                }
            }
            if let Some(m) = &c.mute {
                track.mute = m.value.unwrap_or(false);
                if let Some(mid) = &m.meta.id {
                    self.params.insert(mid.clone(), ParamRef::Mute(id));
                }
            }
            track.solo = c.solo.unwrap_or(false);
            for d in &c.devices {
                if d.role == dp::DeviceRole::Instrument {
                    self.instrumented.insert(id);
                }
                if let Some(slot) = self.device_slot(id, d) {
                    // The input stage (a preamp, a console's bus) is a
                    // slot of its own.
                    if slot.plugin.format == PluginFormat::Builtin
                        && faderframe_core::builtin::is_input_stage(&slot.plugin.id)
                        && track.preamp.is_none()
                    {
                        track.preamp = Some(slot);
                    } else {
                        track.inserts.push(slot);
                    }
                }
            }
            if !kind.has_audio() && !track.inserts.is_empty() && kind != TrackKind::Instrument {
                self.notes
                    .add(format!("devices of '{}', which has no audio", track.name));
                track.inserts.clear();
            }
            for s in &c.sends {
                self.sends.push((id, s.clone()));
            }
            self.destinations.push((
                id,
                c.destination.clone(),
                c.destination.is_none() && !master,
            ));
        }
        if master {
            // Where the document has it.
            self.p.tracks.retain(|t| t.id != id);
            self.master_placed = true;
        }
        self.p.tracks.push(track);
        if let Some(t) = t {
            let folder = (kind == TrackKind::Folder || kind == TrackKind::Bus && holds_tracks)
                .then_some(id)
                .or(folder);
            for child in &t.tracks {
                self.track(Some(child), child.channel.as_ref(), folder);
            }
        }
    }

    /// Outputs, sends and MIDI tracks, once every track is there.
    fn connect(&mut self) {
        let master = self.p.master_id();
        for (id, dest, unsaid) in std::mem::take(&mut self.destinations) {
            let target = dest.as_ref().and_then(|d| self.tracks.get(d).copied());
            let kind = self.p.track(id).map(|t| t.kind);
            let output = match (target, kind) {
                (_, Some(TrackKind::Master)) => OutputRouting::Hardware { first_channel: 0 },
                (Some(t), _) if Some(t) == master => OutputRouting::Master,
                (Some(t), _) => {
                    // A folder bus its track is in: the default reaches it.
                    let parent_bus = self.p.track(id).and_then(|x| x.folder) == Some(t);
                    if parent_bus {
                        OutputRouting::Master
                    } else {
                        OutputRouting::Track { track: t }
                    }
                }
                (None, _) => {
                    if !unsaid && dest.is_some() {
                        self.notes
                            .add("outputs to channels that are not there (to the master)");
                    }
                    OutputRouting::Master
                }
            };
            if let Some(t) = self.p.track_mut(id) {
                t.output = output;
            }
        }
        // Notes tracks playing another track's instrument are MIDI tracks.
        let plays: Vec<(TrackId, TrackId)> = self
            .p
            .tracks
            .iter()
            .filter(|t| t.kind == TrackKind::Instrument && !self.instrumented.contains(&t.id))
            .filter_map(|t| match t.output {
                OutputRouting::Track { track } => Some((t.id, track)),
                _ => None,
            })
            .filter(|(_, to)| {
                self.p
                    .track(*to)
                    .is_some_and(|x| x.kind == TrackKind::Instrument)
                    && self.instrumented.contains(to)
            })
            .collect();
        for (id, _) in plays {
            if let Some(t) = self.p.track_mut(id) {
                t.kind = TrackKind::Midi;
            }
        }
        for (id, s) in std::mem::take(&mut self.sends) {
            let Some(target) = s
                .destination
                .as_ref()
                .and_then(|d| self.tracks.get(d).copied())
            else {
                self.notes.add("sends to channels that are not there");
                continue;
            };
            let send: SendId = self.p.ids.allocate();
            let level = s.volume.value.map_or(0.0, |v| volume_db(&s.volume, v));
            let pan = s
                .pan
                .as_ref()
                .and_then(|p| p.value.map(|v| pan_value(Some(p.unit), p.min, v)));
            for (pid, r) in [
                (s.volume.meta.id.clone(), ParamRef::SendVolume(id, send)),
                (
                    s.pan.as_ref().and_then(|p| p.meta.id.clone()),
                    ParamRef::SendPanOrSwitch,
                ),
                (
                    s.enable.as_ref().and_then(|p| p.meta.id.clone()),
                    ParamRef::SendPanOrSwitch,
                ),
            ] {
                if let Some(pid) = pid {
                    self.params.insert(pid, r);
                }
            }
            if let Some(t) = self.p.track_mut(id) {
                t.sends.push(AuxSend {
                    id: send,
                    target,
                    level_db: level,
                    tap: if s.kind == Some(dp::SendType::Pre) {
                        SendTap::PreFader
                    } else {
                        SendTap::PostFader
                    },
                    enabled: s.enable.as_ref().and_then(|e| e.value).unwrap_or(true),
                    pan,
                });
            }
        }
    }

    // --- timelines ---------------------------------------------------------

    /// The context of a clip's content, one per pass of its loop.
    fn enter_clip(&self, c: &dp::Clip, parent: &Ctx) -> Vec<Ctx> {
        let unit = c.content_time_unit.unwrap_or(parent.unit);
        let anchor = (parent.to_abs)(c.time);
        let ps = c.play_start.unwrap_or(0.0);
        let clock = &self.clock;
        let end = match (c.duration, c.play_stop) {
            (Some(d), _) => (parent.to_abs)(c.time + d.max(0.0)),
            (None, Some(stop)) => clock.advance(anchor, (stop - ps).max(0.0), unit),
            (None, None) => f64::INFINITY,
        };
        let window = (anchor.max(parent.window.0), end.min(parent.window.1));
        // A clip's own fades, else its parent's where they share an edge.
        let fades = |me: &Ctx| -> (f64, f64) {
            let unit = c.fade_time_unit.unwrap_or(parent.unit);
            let length = |v: Option<f64>, from: f64, to: f64| -> f64 {
                let v = v.unwrap_or(0.0).abs();
                match unit {
                    dp::TimeUnit::Seconds => v,
                    dp::TimeUnit::Beats if from.is_finite() && to.is_finite() => {
                        (clock.secs(to) - clock.secs(from)).abs()
                    }
                    dp::TimeUnit::Beats => 0.0,
                }
            };
            let (a, z) = me.window;
            let fin = length(c.fade_in_time, a, a + c.fade_in_time.unwrap_or(0.0).abs());
            let fout = length(c.fade_out_time, z - c.fade_out_time.unwrap_or(0.0).abs(), z);
            let shares = |x: f64, y: f64| (x - y).abs() < 1e-9 || (x.is_infinite() && x == y);
            (
                if fin > 0.0 || !shares(a, parent.window.0) {
                    fin
                } else {
                    parent.fade_in
                },
                if fout > 0.0 || !shares(z, parent.window.1) {
                    fout
                } else {
                    parent.fade_out
                },
            )
        };
        let base = Ctx {
            unit,
            to_abs: parent.to_abs.clone(),
            window,
            gain_db: parent.gain_db,
            muted: parent.muted || c.enable == Some(false),
            fade_in: 0.0,
            fade_out: 0.0,
        };
        let make = |anchor: f64, from: f64, window: (f64, f64)| -> Ctx {
            let mut x = base.clone();
            let tempo = clock.tl.tempo.clone();
            x.to_abs = Rc::new(move |t: f64| match unit {
                dp::TimeUnit::Beats => anchor + (t - from),
                dp::TimeUnit::Seconds => {
                    let s =
                        tempo.musical_to_seconds(MusicalTime::from_quarters(anchor)) + (t - from);
                    tempo.seconds_to_quarters(s)
                }
            });
            x.window = window;
            x
        };
        let looped = match (c.loop_start, c.loop_end) {
            (Some(ls), Some(le)) if le > ls && ps < le && end.is_finite() => Some((ls, le)),
            _ => None,
        };
        let Some((ls, le)) = looped else {
            let mut x = make(anchor, ps, window);
            (x.fade_in, x.fade_out) = fades(&x);
            return vec![x];
        };
        let mut out = Vec::new();
        let mut at = anchor;
        let mut from = ps;
        while at < window.1 && out.len() < 4096 {
            let stop = clock.advance(at, le - from, unit);
            let w = (at.max(window.0), stop.min(window.1));
            if w.1 > w.0 {
                out.push(make(at, from, w));
            }
            at = stop;
            from = ls;
        }
        if let Some(first) = out.first_mut() {
            first.fade_in = fades(first).0;
        }
        if let Some(last) = out.last_mut() {
            last.fade_out = fades(last).1;
        }
        out
    }

    /// The same timeline counted in `unit` from the same origin.
    fn rebase(&self, ctx: &Ctx, unit: Option<dp::TimeUnit>) -> Ctx {
        match unit {
            Some(u) if u != ctx.unit => {
                let origin = (ctx.to_abs)(0.0);
                let tempo = self.clock.tl.tempo.clone();
                let mut x = ctx.clone();
                x.unit = u;
                x.to_abs = Rc::new(move |t| match u {
                    dp::TimeUnit::Beats => origin + t,
                    dp::TimeUnit::Seconds => tempo.seconds_to_quarters(
                        tempo.musical_to_seconds(MusicalTime::from_quarters(origin)) + t,
                    ),
                });
                x
            }
            _ => ctx.clone(),
        }
    }

    fn walk(&mut self, t: &'a dp::Timeline, ctx: &Ctx, group: &mut Group, depth: usize) {
        if depth > 32 {
            self.notes.add("timelines nested too deep (left out)");
            return;
        }
        let ctx = self.rebase(ctx, t.common().time_unit);
        match t {
            dp::Timeline::Lanes(l) => {
                // A gain beside audio is the clip's gain.
                let mut inner = ctx.clone();
                for x in &l.lanes {
                    if let dp::Timeline::Points(p) = x
                        && p.target.expression == Some(dp::ExpressionType::Gain)
                        && let Some(v) = p.points.first().and_then(dp::Point::number)
                    {
                        let db = match p.unit {
                            Some(dp::Unit::Decibel) => v,
                            _ => f64::from(gain_to_db(v as f32)),
                        };
                        inner.gain_db += db;
                        if p.points
                            .iter()
                            .filter_map(dp::Point::number)
                            .any(|x| (x - v).abs() > 1e-6)
                        {
                            self.notes
                                .add("clip gain envelopes (their first value kept)");
                        }
                    }
                }
                for x in &l.lanes {
                    if let dp::Timeline::Points(p) = x
                        && p.target.expression == Some(dp::ExpressionType::Gain)
                    {
                        continue;
                    }
                    self.walk(x, &inner, group, depth + 1);
                }
            }
            dp::Timeline::Clips(c) => {
                for clip in &c.clips {
                    self.clip(clip, &ctx, group, depth);
                }
            }
            dp::Timeline::Notes(n) => self.notes_of(n, &ctx, group),
            dp::Timeline::Points(p) => self.points_in_clip(p, &ctx, group),
            dp::Timeline::Warps(w) => self.warps(w, &ctx, group, depth),
            dp::Timeline::Audio(m) => {
                let len = m.duration;
                let anchors = vec![
                    (self.clock.samples((ctx.to_abs)(0.0)), 0.0),
                    (
                        self.clock.samples((ctx.to_abs)(len.max(1e-6))),
                        len.max(1e-6),
                    ),
                ];
                self.audio(m, anchors, &ctx, group);
            }
            dp::Timeline::Video(_) => self.notes.add("video (left out)"),
            dp::Timeline::Markers(_) => self.notes.add("markers inside clips (left out)"),
            dp::Timeline::ClipSlot(_) => self.notes.add("clip slots outside scenes (left out)"),
        }
    }

    fn clip(&mut self, c: &'a dp::Clip, ctx: &Ctx, group: &mut Group, depth: usize) {
        let content: Option<&'a dp::Timeline> = match (&c.content, &c.reference) {
            (Some(x), _) => Some(x),
            (None, Some(r)) => {
                let t = self.timelines.get(r).copied();
                if t.is_none() {
                    self.notes.add("linked clips whose timeline is not there");
                }
                t
            }
            (None, None) => None,
        };
        let Some(content) = content else { return };
        if group.name.is_none() {
            group.name = c.meta.name.clone();
        }
        if depth == 0 {
            group.color = parse_color(c.meta.color.as_ref());
            group.muted = c.enable == Some(false);
            group.link = c
                .reference
                .clone()
                .or_else(|| content.common().meta.id.clone());
        }
        let passes = self.enter_clip(c, ctx);
        if depth == 0
            && let (Some(first), Some(last)) = (passes.first(), passes.last())
        {
            group.window = Some((first.window.0, last.window.1));
        }
        for x in passes {
            self.walk(content, &x, group, depth + 1);
        }
    }

    fn warps(&mut self, w: &'a dp::Warps, ctx: &Ctx, group: &mut Group, depth: usize) {
        let Some(content) = w.content.as_deref() else {
            return;
        };
        let mut pts: Vec<(f64, f64)> = w.warps.iter().map(|x| (x.content_time, x.time)).collect();
        pts.sort_by(|a, b| a.0.total_cmp(&b.0));
        if pts.len() < 2 {
            // One point (or none): content at the warps' own speed.
            let (c0, t0) = pts.first().copied().unwrap_or((0.0, 0.0));
            pts = vec![(c0, t0), (c0 + 1.0, t0 + 1.0)];
        }
        let warp = Rc::new(pts);
        let outer = ctx.to_abs.clone();
        let inner_unit = w.content_time_unit;
        let to_time = {
            let warp = warp.clone();
            move |c: f64| interpolate(&warp, c)
        };
        if let dp::Timeline::Audio(m) = content {
            // Anchors at every warp point (and the file's ends).
            let mut anchors: Vec<(f64, f64)> = warp
                .iter()
                .map(|&(c, t)| (self.clock.samples(outer(t)), c))
                .collect();
            for c in [0.0, m.duration] {
                if !anchors.iter().any(|a| (a.1 - c).abs() < 1e-9) {
                    anchors.push((self.clock.samples(outer(to_time(c))), c));
                }
            }
            anchors.sort_by(|a, b| a.1.total_cmp(&b.1));
            if inner_unit != dp::TimeUnit::Seconds {
                self.notes.add("audio warped in beats (read as seconds)");
            }
            self.audio(m, anchors, ctx, group);
            return;
        }
        let inner = Ctx {
            unit: inner_unit,
            to_abs: Rc::new(move |c| outer(to_time(c))),
            ..ctx.clone()
        };
        self.walk(content, &inner, group, depth + 1);
    }

    fn audio(&mut self, m: &dp::MediaFile, anchors: Vec<(f64, f64)>, ctx: &Ctx, group: &mut Group) {
        if !self.sources.contains_key(&m.file.path) {
            if !self.loaded.media.contains_key(&m.file.path) {
                self.notes.add(format!(
                    "audio files that could not be read ({})",
                    m.file.path
                ));
            }
            return;
        }
        let algorithm = match m
            .algorithm
            .as_deref()
            .map(str::to_ascii_lowercase)
            .as_deref()
        {
            Some("repitch" | "varispeed" | "resample" | "tape") => WarpAlgorithm::Varispeed,
            Some(a) if a.contains("rhythm") || a.contains("beat") || a.contains("drum") => {
                WarpAlgorithm::Rhythmic
            }
            _ => WarpAlgorithm::Polyphonic,
        };
        group.audio.push(AudioPiece {
            file: m.file.path.clone(),
            anchors,
            window: ctx.window,
            gain_db: ctx.gain_db,
            fade_in: ctx.fade_in,
            fade_out: ctx.fade_out,
            muted: ctx.muted,
            algorithm,
            name: m.common.meta.name.clone(),
        });
    }

    fn notes_of(&mut self, n: &'a dp::Notes, ctx: &Ctx, group: &mut Group) {
        for note in &n.notes {
            let start = (ctx.to_abs)(note.time);
            let end = (ctx.to_abs)(note.time + note.duration.max(0.0));
            if start < ctx.window.0 - 1e-9 || start >= ctx.window.1 - 1e-9 {
                continue;
            }
            if !(0..=127).contains(&note.key) {
                self.notes.add("notes beyond MIDI's keys");
                continue;
            }
            let mut expression = Vec::new();
            if let Some(x) = note.content.as_deref() {
                // Times from the note's start.
                let ctx_note = Ctx {
                    unit: ctx.unit,
                    to_abs: {
                        let f = ctx.to_abs.clone();
                        let t0 = note.time;
                        Rc::new(move |t| f(t0 + t))
                    },
                    ..ctx.clone()
                };
                self.note_expression(x, &ctx_note, start, &mut expression);
            }
            group.notes.push(NotePiece {
                start,
                end: end.min(ctx.window.1),
                key: note.key as u8,
                channel: note.channel.clamp(0, 15) as u8,
                vel: note.vel,
                rel: note.rel,
                expression,
            });
        }
    }

    fn note_expression(
        &mut self,
        t: &'a dp::Timeline,
        ctx: &Ctx,
        start: f64,
        out: &mut Vec<(ExpressionKind, Vec<(f64, f32)>)>,
    ) {
        match t {
            dp::Timeline::Lanes(l) => {
                for x in &l.lanes {
                    self.note_expression(x, ctx, start, out);
                }
            }
            dp::Timeline::Points(p) => {
                use dp::ExpressionType as E;
                let negative = p
                    .points
                    .iter()
                    .filter_map(dp::Point::number)
                    .any(|v| v < 0.0);
                let (kind, map): (ExpressionKind, Box<dyn Fn(f64) -> f32>) =
                    match p.target.expression {
                        Some(E::Transpose) => (ExpressionKind::Pitch, Box::new(|v| v as f32)),
                        Some(E::Pressure) => (ExpressionKind::Pressure, Box::new(|v| v as f32)),
                        Some(E::Timbre) => (ExpressionKind::Timbre, Box::new(|v| v as f32)),
                        Some(E::Gain) => {
                            let db = p.unit == Some(dp::Unit::Decibel);
                            (
                                ExpressionKind::Volume,
                                Box::new(move |v| if db { v as f32 } else { gain_to_db(v as f32) }),
                            )
                        }
                        Some(E::Pan) => (
                            ExpressionKind::Pan,
                            Box::new(move |v| {
                                if negative {
                                    v as f32
                                } else {
                                    (v * 2.0 - 1.0) as f32
                                }
                            }),
                        ),
                        other => {
                            self.notes.add(format!(
                                "note expressions of {} (no FaderFrame form)",
                                other.map_or("an unnamed kind", |e| e.as_str())
                            ));
                            return;
                        }
                    };
                let (lo, hi) = kind.range();
                let pts: Vec<(f64, f32)> = p
                    .points
                    .iter()
                    .filter_map(|pt| {
                        let v = pt.number()?;
                        Some(((ctx.to_abs)(pt.time()) - start, map(v).clamp(lo, hi)))
                    })
                    .collect();
                if !pts.is_empty() {
                    out.push((kind, pts));
                }
            }
            _ => self
                .notes
                .add("note content other than expression (left out)"),
        }
    }

    /// Points inside a clip: MIDI controllers (expressions of a channel).
    fn points_in_clip(&mut self, p: &'a dp::Points, ctx: &Ctx, group: &mut Group) {
        use dp::ExpressionType as E;
        if p.target.parameter.is_some() {
            self.notes
                .add("parameter automation inside clips (left out)");
            return;
        }
        let channel = p.target.channel.unwrap_or(0).clamp(0, 15) as u8;
        let numbers: Vec<f64> = p.points.iter().filter_map(dp::Point::number).collect();
        let negative = numbers.iter().any(|v| *v < 0.0);
        let above_one = numbers.iter().any(|v| *v > 1.0 + 1e-9);
        let controller = match p.target.expression {
            Some(E::ChannelController) => match p.target.controller {
                Some(n) if (0..=127).contains(&n) => MidiController::Cc { number: n as u8 },
                _ => {
                    self.notes
                        .add("controller lanes without a controller number");
                    return;
                }
            },
            Some(E::PitchBend) => MidiController::PitchBend,
            Some(E::ChannelPressure) | Some(E::Pressure) => MidiController::ChannelPressure,
            Some(E::ProgramChange) => MidiController::Program,
            Some(E::PolyPressure) => match p.target.key {
                Some(k) if (0..=127).contains(&k) => MidiController::PolyPressure { key: k as u8 },
                _ => {
                    self.notes.add("poly pressure lanes without a key");
                    return;
                }
            },
            Some(E::Gain) => return,
            other => {
                self.notes.add(format!(
                    "clip expressions of {} (no FaderFrame form)",
                    other.map_or("an unnamed kind", |e| e.as_str())
                ));
                return;
            }
        };
        for pt in &p.points {
            let at = (ctx.to_abs)(pt.time());
            if at < ctx.window.0 - 1e-9 || at >= ctx.window.1 {
                continue;
            }
            let Some(v) = pt.number() else { continue };
            // To 0…1 of the controller's range.
            let unit = match controller {
                MidiController::Program => {
                    if above_one || matches!(pt, dp::Point::Integer { .. } | dp::Point::Enum { .. })
                    {
                        v / 127.0
                    } else {
                        v
                    }
                }
                MidiController::PitchBend if p.unit == Some(dp::Unit::Semitones) => {
                    // A bend range of two semitones.
                    0.5 + v / 4.0
                }
                MidiController::PitchBend if negative => (v + 1.0) / 2.0,
                _ if above_one => v / f64::from(controller.max()),
                _ => v,
            };
            group.controllers.push((
                at,
                controller,
                channel,
                controller_point(MusicalTime::ZERO, controller, unit),
            ));
        }
    }

    /// Automation of the document's parameters (wherever its points are).
    fn automation(&mut self, t: &'a dp::Timeline, ctx: &Ctx) {
        let ctx = self.rebase(ctx, t.common().time_unit);
        match t {
            dp::Timeline::Lanes(l) => {
                for x in &l.lanes {
                    self.automation(x, &ctx);
                }
            }
            dp::Timeline::Points(p) => {
                let Some(param) = p.target.parameter.as_ref() else {
                    return;
                };
                let Some(r) = self.params.get(param).cloned() else {
                    if param != ids::TEMPO && param != ids::TIME_SIGNATURE {
                        self.notes
                            .add("automation of parameters that are not there");
                    }
                    return;
                };
                self.lane(&r, p, &ctx);
            }
            _ => {}
        }
    }

    fn lane(&mut self, r: &ParamRef, p: &dp::Points, ctx: &Ctx) {
        let unit = p.unit;
        let (track, target, value, fader): (
            TrackId,
            AutomationTarget,
            Box<dyn Fn(f64) -> f64>,
            bool,
        ) = match *r {
            ParamRef::Volume(t) => (
                t,
                AutomationTarget::TrackVolume,
                Box::new(move |v| f64::from(lane_db(unit, v))),
                true,
            ),
            ParamRef::Pan(t) => (
                t,
                AutomationTarget::TrackPan,
                Box::new(move |v| {
                    f64::from(pan_value(unit.or(Some(dp::Unit::Normalized)), None, v))
                }),
                false,
            ),
            ParamRef::Mute(t) => (
                t,
                AutomationTarget::TrackMute,
                Box::new(|v| f64::from(u8::from(v >= 0.5))),
                false,
            ),
            ParamRef::SendVolume(t, s) => (
                t,
                AutomationTarget::SendLevel(s),
                Box::new(move |v| f64::from(lane_db(unit, v))),
                true,
            ),
            ParamRef::SendPanOrSwitch => {
                self.notes.add("automation of send pans and switches");
                return;
            }
            ParamRef::DeviceOn(t, plugin) => (
                t,
                AutomationTarget::PluginBypass(plugin),
                Box::new(|v| f64::from(u8::from(v < 0.5))),
                false,
            ),
            ParamRef::DeviceParam {
                track,
                plugin,
                parameter,
                range,
            } => (
                track,
                AutomationTarget::PluginParameter { plugin, parameter },
                Box::new(move |v| match (unit, range) {
                    (Some(dp::Unit::Normalized), Some((a, b))) => a + v * (b - a),
                    _ => v,
                }),
                false,
            ),
        };
        let mut points: Vec<AutomationPoint> = Vec::with_capacity(p.points.len());
        let pts: Vec<(f64, f64, bool)> = p
            .points
            .iter()
            .filter_map(|pt| Some(((ctx.to_abs)(pt.time()), pt.number()?, pt.is_linear())))
            .collect();
        for (i, &(at, v, linear)) in pts.iter().enumerate() {
            let shape = if linear {
                CurveShape::Linear
            } else {
                CurveShape::Step
            };
            points.push(AutomationPoint {
                time: MusicalTime::from_quarters(at.max(0.0)),
                value: value(v),
                shape,
            });
            // A fader's line in gain is a curve in dB: follow it.
            if fader
                && linear
                && let Some(&(bt, bv, _)) = pts.get(i + 1)
                && bt > at
            {
                for k in 1..16 {
                    let f = f64::from(k) / 16.0;
                    points.push(AutomationPoint {
                        time: MusicalTime::from_quarters((at + (bt - at) * f).max(0.0)),
                        value: value(v + (bv - v) * f),
                        shape: CurveShape::Linear,
                    });
                }
            }
        }
        if fader {
            points = thin_linear(&points, 0.05);
        }
        if points.is_empty() {
            return;
        }
        let id = self.p.ids.allocate();
        if let Some(t) = self.p.track_mut(track) {
            if t.automation.lanes.iter().any(|l| l.target == target) {
                self.notes
                    .add("several automation lanes of one parameter (the first kept)");
                return;
            }
            t.automation.lanes.push(AutomationLane {
                id,
                target,
                curve: AutomationCurve::from_points(points),
                mode: AutomationMode::Read,
                visible: false,
            });
        }
    }

    // --- clips from groups -------------------------------------------------

    fn make_clips(&mut self, track: TrackId, g: Group) {
        let rate = self.clock.rate;
        let kind = self.p.track(track).map(|t| t.kind);
        let mut made: Vec<ClipId> = Vec::new();
        if !g.notes.is_empty() || !g.controllers.is_empty() {
            if !matches!(kind, Some(TrackKind::Instrument | TrackKind::Midi)) {
                if let Some(t) = self.p.track_mut(track)
                    && t.kind == TrackKind::Audio
                    && t.clips.is_empty()
                {
                    t.kind = TrackKind::Instrument;
                } else {
                    self.notes.add("notes on tracks of audio (left out)");
                }
            }
            if let Some(id) = self.midi_clip(track, &g) {
                made.push(id);
            }
        }
        for a in &g.audio {
            if !matches!(self.p.track(track).map(|t| t.kind), Some(TrackKind::Audio)) {
                self.notes.add("audio on tracks of notes (left out)");
                continue;
            }
            if let Some(id) = self.audio_clip(track, a, &g, rate) {
                made.push(id);
            }
        }
        if made.len() == 1
            && let Some(link) = g.link
        {
            self.links.entry(link).or_default().push(made[0]);
        }
    }

    fn midi_clip(&mut self, track: TrackId, g: &Group) -> Option<ClipId> {
        let first = g
            .notes
            .iter()
            .map(|n| n.start)
            .chain(g.controllers.iter().map(|c| c.0))
            .fold(f64::INFINITY, f64::min);
        let last = g
            .notes
            .iter()
            .map(|n| n.end)
            .chain(g.controllers.iter().map(|c| c.0))
            .fold(f64::NEG_INFINITY, f64::max);
        let (start, end) = match g.window {
            Some((a, b)) if a.is_finite() && b.is_finite() => (a, b),
            Some((a, _)) if a.is_finite() => (a, self.bar_after(last)),
            _ => {
                let meter = &self.clock.tl.meter;
                let bar = meter.bar_at(MusicalTime::from_quarters(first.max(0.0)));
                (meter.bar_start(bar).quarters(), self.bar_after(last))
            }
        };
        if end.partial_cmp(&start) != Some(std::cmp::Ordering::Greater) {
            return None;
        }
        let start_t = MusicalTime::from_quarters(start.max(0.0));
        let rel = |b: f64| MusicalTime::from_quarters((b - start).max(0.0));
        let mut m = MidiClip {
            length: MusicalTime::from_quarters(end - start),
            ..MidiClip::default()
        };
        for n in &g.notes {
            let (velocity, fine) = velocity_of(n.vel.unwrap_or(100.0 / 127.0));
            let mut note = MidiNote {
                id: self.p.ids.allocate(),
                start: rel(n.start),
                length: (rel(n.end) - rel(n.start)).max(MusicalTime(1)),
                key: n.key,
                velocity: velocity.max(1),
                channel: n.channel,
                muted: false,
                release: None,
                time_locked: false,
                articulation: None,
                fine: None,
            };
            if let Some(f) = fine {
                note.set_velocity16(f);
            }
            if let Some(r) = n.rel {
                let (r7, rf) = velocity_of(r);
                match rf {
                    Some(f) => note.set_release16(Some(f)),
                    None => note.release = Some(r7),
                }
            }
            if !n.expression.is_empty() {
                let mut e = NoteExpression::new(note.id);
                for (kind, pts) in &n.expression {
                    let c = e.curve_mut(*kind);
                    c.extend(pts.iter().map(|&(t, v)| ExpressionPoint {
                        time: MusicalTime::from_quarters(t.max(0.0)),
                        value: v,
                    }));
                    c.sort_by_key(|p| p.time);
                    c.dedup_by_key(|p| p.time);
                }
                m.expressions.push(e);
            }
            m.notes.push(note);
        }
        for &(at, c, channel, point) in &g.controllers {
            let lane = m.lane_mut(c, channel);
            lane.points.push(ControllerPoint {
                time: rel(at),
                ..point
            });
        }
        for l in &mut m.controllers {
            l.normalize();
        }
        m.controllers
            .retain(|l: &ControllerLane| !l.points.is_empty());
        m.sort_notes();
        m.prune_expressions();
        let id: ClipId = self.p.ids.allocate();
        let name = g
            .name
            .clone()
            .or_else(|| self.p.track(track).map(|t| t.name.clone()))
            .unwrap_or_default();
        self.add_clip(Clip {
            id,
            track,
            name,
            color: g.color,
            start: start_t,
            muted: g.muted,
            content: ClipContent::Midi(m),
        });
        Some(id)
    }

    /// The bar line at or after `beats`.
    fn bar_after(&self, beats: f64) -> f64 {
        let meter = &self.clock.tl.meter;
        let t = MusicalTime::from_quarters(beats.max(0.0));
        let bar = meter.bar_at(t);
        if meter.bar_start(bar) == t {
            t.quarters()
        } else {
            meter.bar_start(bar + 1).quarters()
        }
    }

    fn audio_clip(
        &mut self,
        track: TrackId,
        a: &AudioPiece,
        g: &Group,
        rate: f64,
    ) -> Option<ClipId> {
        let &(source, frames) = self.sources.get(&a.file)?;
        // Anchors in samples on the arrangement and frames of the source.
        let mut pts: Vec<(f64, f64)> = a
            .anchors
            .iter()
            .map(|&(s, secs)| (s, secs * rate))
            .collect();
        pts.sort_by(|x, y| x.0.total_cmp(&y.0));
        pts.dedup_by(|y, x| (x.0 - y.0).abs() < 1e-6);
        if pts.windows(2).any(|w| w[1].1 < w[0].1) {
            self.notes.add("audio played backwards (left out)");
            return None;
        }
        // The heard window: the file's frames there and the clip's span.
        // A clip starting where its window does keeps its place in beats
        // (audio only lasts whole samples).
        let first = pts.first()?.0;
        let win0 = if a.window.0.is_finite() {
            self.clock.samples(a.window.0)
        } else {
            f64::NEG_INFINITY
        };
        let at_window_start = win0 >= first - 0.5;
        let w0 = if at_window_start { win0 } else { first };
        let file_end = interpolate_inverse(&pts, frames as f64);
        let win1 = if a.window.1.is_finite() {
            self.clock.samples(a.window.1)
        } else {
            f64::INFINITY
        };
        let at_window_end = win1 <= file_end + 0.5;
        let w1 = win1.min(file_end);
        let length = (w1 - w0).round() as i64;
        if length < 1 {
            return None;
        }
        let src = |s: f64| interpolate(&pts, s);
        let offset = src(w0).round().max(0.0) as i64;
        let span = (src(w0 + length as f64).round() as i64 - offset).max(1);
        let markers: Vec<WarpMarker> = pts
            .iter()
            .filter(|(s, _)| *s > w0 + 0.5 && *s < w1 - 0.5)
            .map(|&(s, f)| WarpMarker {
                at: (s - w0).round() as i64,
                source: f.round() as i64,
            })
            .collect();
        let warp = (!markers.is_empty() || (span - length).abs() > 1).then_some(Warp {
            source_length: span,
            markers,
            algorithm: a.algorithm,
        });
        let fades = ClipFades {
            fade_in: if at_window_start {
                ((a.fade_in * rate).round() as i64).min(length / 2)
            } else {
                0
            },
            fade_out: if at_window_end {
                ((a.fade_out * rate).round() as i64).min(length / 2)
            } else {
                0
            },
            ..ClipFades::default()
        };
        let start = if at_window_start && a.window.0.is_finite() {
            MusicalTime::from_quarters(a.window.0.max(0.0))
        } else {
            self.clock.tl.at_sample(w0.round() as i64, rate)
        };
        let id: ClipId = self.p.ids.allocate();
        let name = g
            .name
            .clone()
            .or_else(|| a.name.clone())
            .or_else(|| self.p.sources.get(&source).map(|s| s.name.clone()))
            .unwrap_or_default();
        self.add_clip(Clip {
            id,
            track,
            name,
            color: g.color,
            start,
            muted: a.muted || g.muted,
            content: ClipContent::Audio(AudioClip {
                envelope: None,
                source,
                source_offset: offset,
                length,
                gain_db: (a.gain_db as f32).clamp(-144.0, MAX_LEVEL_DB),
                fades,
                stretch: Default::default(),
                reversed: false,
                warp,
                pitch: None,
                effects: None,
                spectral: None,
            }),
        });
        Some(id)
    }

    fn add_clip(&mut self, clip: Clip) {
        if let Some(t) = self.p.track_mut(clip.track) {
            t.clips.push(clip.id);
        }
        self.p.clips.insert(clip.id, clip);
    }
}

/// `t` at `x` along the sorted (x, t) points (extrapolated past the ends).
fn interpolate(pts: &[(f64, f64)], x: f64) -> f64 {
    match pts {
        [] => x,
        [(a, b)] => b + (x - a),
        _ => {
            let i = pts
                .partition_point(|p| p.0 <= x)
                .saturating_sub(1)
                .min(pts.len() - 2);
            let ((x0, y0), (x1, y1)) = (pts[i], pts[i + 1]);
            if (x1 - x0).abs() < 1e-12 {
                y0
            } else {
                y0 + (x - x0) * (y1 - y0) / (x1 - x0)
            }
        }
    }
}

/// `x` where the (x, t) points reach `t` (they rise).
fn interpolate_inverse(pts: &[(f64, f64)], t: f64) -> f64 {
    let flipped: Vec<(f64, f64)> = pts.iter().map(|&(x, y)| (y, x)).collect();
    interpolate(&flipped, t)
}

/// A gain lane's value in dB.
fn lane_db(unit: Option<dp::Unit>, v: f64) -> f32 {
    match unit {
        Some(dp::Unit::Decibel) => (v as f32).min(MAX_LEVEL_DB),
        Some(dp::Unit::Percent) => gain_to_db((v / 100.0) as f32).min(MAX_LEVEL_DB),
        _ => gain_to_db(v as f32).min(MAX_LEVEL_DB),
    }
}

/// Drop points of linear stretches that the line between their neighbours
/// gives within `tolerance` (dB).
fn thin_linear(points: &[AutomationPoint], tolerance: f64) -> Vec<AutomationPoint> {
    let mut out: Vec<AutomationPoint> = Vec::with_capacity(points.len());
    let mut run: Vec<AutomationPoint> = Vec::new();
    let flush = |run: &mut Vec<AutomationPoint>, out: &mut Vec<AutomationPoint>| {
        out.extend(faderframe_automation::thin(run, tolerance));
        run.clear();
    };
    for p in points {
        run.push(*p);
        if p.shape != CurveShape::Linear {
            flush(&mut run, &mut out);
        }
    }
    flush(&mut run, &mut out);
    out
}

/// The tempo map and metre of the document.
fn timeline_of(doc: &dp::Project, notes: &mut Notes) -> Timeline {
    let mut tl = Timeline::default();
    let transport = doc.transport.as_ref();
    let initial = transport
        .and_then(|t| t.tempo.as_ref())
        .and_then(|t| t.value)
        .filter(|v| v.is_finite() && *v > 0.0);
    let arrangement = doc.arrangement.as_ref();
    let points: Vec<(f64, f64, bool)> = arrangement
        .and_then(|a| a.tempo_automation.as_ref())
        .map(|p| {
            let seconds = p.common.time_unit == Some(dp::TimeUnit::Seconds);
            if seconds {
                notes.add("tempo changes placed in seconds");
            }
            p.points
                .iter()
                .filter_map(|x| Some((x.time(), x.number()?, x.is_linear())))
                .collect()
        })
        .unwrap_or_default();
    let clamp = |b: f64| {
        b.clamp(
            faderframe_timeline::TempoMap::MIN_BPM,
            faderframe_timeline::TempoMap::MAX_BPM,
        )
    };
    let first = points
        .iter()
        .find(|p| p.0 <= 1e-9)
        .map(|p| p.1)
        .or(initial)
        .unwrap_or(120.0);
    tl.tempo.set_initial_bpm(clamp(first));
    let seconds = arrangement
        .and_then(|a| a.tempo_automation.as_ref())
        .is_some_and(|p| p.common.time_unit == Some(dp::TimeUnit::Seconds));
    for &(at, bpm, linear) in &points {
        let beats = if seconds {
            tl.tempo.seconds_to_quarters(at)
        } else {
            at
        };
        tl.tempo.set_point(TempoPoint {
            position: MusicalTime::from_quarters(beats.max(0.0)),
            bpm: clamp(bpm),
            curve: if linear {
                TempoCurve::Linear
            } else {
                TempoCurve::Constant
            },
        });
    }
    let sig = |n: i64, d: i64, notes: &mut Notes| {
        let s = TimeSignature::new(n.clamp(1, 64) as u8, d.clamp(1, 64) as u8);
        if s.is_none() {
            notes.add(format!("a time signature of {n}/{d} (4/4 instead)"));
        }
        s.unwrap_or(TimeSignature::FOUR_FOUR)
    };
    if let Some(ts) = transport.and_then(|t| t.time_signature.as_ref()) {
        tl.meter =
            faderframe_timeline::TimeSignatureMap::new(sig(ts.numerator, ts.denominator, notes));
    }
    if let Some(p) = arrangement.and_then(|a| a.time_signature_automation.as_ref()) {
        let mut changes: Vec<(f64, i64, i64)> = p
            .points
            .iter()
            .filter_map(|x| match *x {
                dp::Point::TimeSignature {
                    time,
                    numerator,
                    denominator,
                } => Some((time, numerator, denominator)),
                _ => None,
            })
            .collect();
        changes.sort_by(|a, b| a.0.total_cmp(&b.0));
        for (at, n, d) in changes {
            let s = sig(n, d, notes);
            let t = MusicalTime::from_quarters(at.max(0.0));
            let mut bar = tl.meter.bar_at(t);
            if tl.meter.bar_start(bar) != t {
                notes.add("time signature changes inside a bar (moved to the next bar line)");
                bar += 1;
            }
            tl.meter.set_change(MeterChange { bar, signature: s });
        }
    }
    tl
}

/// The document as a project at `rate`, and what had no place in it.
pub(crate) fn build(loaded: &Loaded, rate: u32) -> (Project, Vec<String>) {
    let doc = &loaded.doc;
    let mut notes = Notes::default();
    let name = loaded
        .metadata
        .title
        .clone()
        .filter(|t| !t.trim().is_empty())
        .unwrap_or_else(|| loaded.name.clone());
    let mut p = Project::new(name, rate);
    p.timeline = timeline_of(doc, &mut notes);
    let clock = Clock {
        tl: p.timeline.clone(),
        rate: f64::from(rate),
    };
    let mut x = Importer {
        loaded,
        p,
        clock,
        notes,
        tracks: HashMap::new(),
        params: HashMap::new(),
        timelines: HashMap::new(),
        sources: HashMap::new(),
        links: BTreeMap::new(),
        destinations: Vec::new(),
        sends: Vec::new(),
        master_placed: false,
        instrumented: BTreeSet::new(),
    };
    // Media.
    for (path, m) in &loaded.media {
        match m {
            Ok(m) => {
                let id: AudioSourceId = x.p.ids.allocate();
                x.p.sources.insert(
                    id,
                    AudioSource {
                        id,
                        name: m.name.clone(),
                        spec: SourceSpec::File {
                            path: m.path.clone(),
                            channels: m.channels as u16,
                            frames: m.frames as i64,
                            sample_rate: m.sample_rate,
                        },
                    },
                );
                let frames = (m.frames as f64 * f64::from(rate) / f64::from(m.sample_rate.max(1)))
                    .round() as i64;
                x.sources.insert(path.clone(), (id, frames));
            }
            Err(e) => x
                .notes
                .add(format!("audio files that could not be read: {e}")),
        }
    }
    // The structure.
    for lane in &doc.structure {
        match lane {
            dp::Lane::Track(t) => x.track(Some(t), t.channel.as_ref(), None),
            dp::Lane::Channel(c) => x.track(None, Some(c), None),
        }
    }
    x.connect();
    // A document without a master: FaderFrame's after its tracks.
    if !x.master_placed
        && let Some(i) = x.p.tracks.iter().position(|t| t.kind == TrackKind::Master)
    {
        let m = x.p.tracks.remove(i);
        x.p.tracks.push(m);
    }
    // The arrangement.
    if let Some(a) = &doc.arrangement {
        if let Some(l) = &a.lanes {
            for t in &l.lanes {
                x.index_timeline(t);
            }
            for s in &doc.scenes {
                if let Some(c) = &s.content {
                    x.index_timeline(c);
                }
            }
            let top = Ctx {
                unit: l.common.time_unit.unwrap_or(dp::TimeUnit::Beats),
                to_abs: Rc::new(|t| t),
                window: (f64::NEG_INFINITY, f64::INFINITY),
                gain_db: 0.0,
                muted: false,
                fade_in: 0.0,
                fade_out: 0.0,
            };
            let track = l
                .common
                .track
                .as_ref()
                .and_then(|id| x.tracks.get(id).copied());
            for t in &l.lanes {
                x.arrangement(t, &top, track);
            }
            for t in &l.lanes {
                x.automation(t, &top);
            }
        }
        if let Some(m) = &a.markers {
            x.markers(m);
        }
    }
    if doc.scenes.iter().any(|s| s.content.is_some()) {
        x.notes.add(format!(
            "clip launcher scenes ({}: their clips are not placed)",
            doc.scenes.len()
        ));
    }
    // Clips that played one timeline are aliases.
    for clips in x.links.values().filter(|c| c.len() > 1) {
        let link: faderframe_core::ClipLinkId = x.p.ids.allocate();
        for c in clips {
            x.p.clip_links.insert(*c, link);
        }
    }
    // Audio tracks as wide as their widest file (when the channel did not
    // say).
    let widths: Vec<(TrackId, usize)> =
        x.p.tracks
            .iter()
            .filter(|t| t.kind == TrackKind::Audio)
            .map(|t| {
                let w = t
                    .clips
                    .iter()
                    .filter_map(|c| x.p.clips.get(c))
                    .flat_map(|c| c.content.sources())
                    .filter_map(|s| x.p.sources.get(&s).map(AudioSource::channels))
                    .max()
                    .unwrap_or(0);
                (t.id, w)
            })
            .collect();
    let said: BTreeSet<TrackId> = doc
        .channels()
        .iter()
        .filter(|c| c.audio_channels.is_some())
        .filter_map(|c| c.meta.id.as_ref().and_then(|i| x.tracks.get(i).copied()))
        .collect();
    for (id, w) in widths {
        if w > 0
            && !said.contains(&id)
            && let Some(t) = x.p.track_mut(id)
        {
            t.layout = ChannelLayout::from_channel_count(w);
        }
    }
    let notes_out = std::mem::take(&mut x.notes);
    let mut p = x.p;
    let mut list = notes_out.list();
    list.extend(p.repair());
    (p, list)
}

impl<'a> Importer<'a> {
    /// Clips of the arrangement's lanes, by the track a lane is for.
    fn arrangement(&mut self, t: &'a dp::Timeline, ctx: &Ctx, track: Option<TrackId>) {
        let ctx = self.rebase(ctx, t.common().time_unit);
        let track = t
            .common()
            .track
            .as_ref()
            .and_then(|id| self.tracks.get(id).copied())
            .or(track);
        match t {
            dp::Timeline::Lanes(l) => {
                for x in &l.lanes {
                    self.arrangement(x, &ctx, track);
                }
            }
            dp::Timeline::Clips(c) => {
                let Some(track) = track else {
                    if !c.clips.is_empty() {
                        self.notes.add("clips on no track (left out)");
                    }
                    return;
                };
                for clip in &c.clips {
                    let mut g = Group::default();
                    self.clip(clip, &ctx, &mut g, 0);
                    self.make_clips(track, g);
                }
            }
            dp::Timeline::Points(_) => {}
            dp::Timeline::Markers(m) if track.is_none() => self.markers(m),
            other => {
                // Notes or audio straight on a track's lane: one clip.
                let Some(track) = track else {
                    self.notes.add("timelines on no track (left out)");
                    return;
                };
                let mut g = Group::default();
                self.walk(other, &ctx, &mut g, 1);
                self.make_clips(track, g);
            }
        }
    }

    fn markers(&mut self, m: &dp::Markers) {
        for k in &m.markers {
            let at = MusicalTime::from_quarters(k.time.max(0.0));
            let name = k.meta.name.clone().unwrap_or_default();
            let section_end = k
                .meta
                .comment
                .as_deref()
                .and_then(|c| c.strip_prefix(ids::SECTION_COMMENT))
                .and_then(|e| e.trim().parse::<f64>().ok())
                .filter(|e| *e > k.time);
            match section_end {
                Some(end) => self.p.sections.push(Section {
                    id: self.p.ids.allocate(),
                    name,
                    start: at,
                    end: MusicalTime::from_quarters(end),
                    color: parse_color(k.meta.color.as_ref())
                        .unwrap_or_else(|| TrackColor::palette(self.p.sections.len())),
                    time_base: Default::default(),
                }),
                None => self.p.markers.push(Marker {
                    id: self.p.ids.allocate(),
                    position: at,
                    name,
                    time_base: Default::default(),
                }),
            }
        }
        self.p.markers.sort_by_key(|m| m.position);
        self.p.sections.sort_by_key(|s| s.start);
    }
}
