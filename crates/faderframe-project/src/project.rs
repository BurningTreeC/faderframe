use crate::{Clip, OutputRouting, Track, TrackColor, TrackKind};
use faderframe_audio_files::GeneratorSpec;
use faderframe_core::{AudioSourceId, ClipId, IdAllocator, MarkerId, PluginInstanceId, TrackId};
use faderframe_timeline::{MusicalTime, Timeline};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::PathBuf;

/// Where an audio source's samples come from.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "type")]
pub enum SourceSpec {
    /// An audio file on disk (path relative to the project file when possible).
    File {
        path: PathBuf,
        channels: u16,
        frames: i64,
        sample_rate: u32,
    },
    /// Procedurally generated material, rendered at the engine rate.
    Generated { generator: GeneratorSpec },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AudioSource {
    pub id: AudioSourceId,
    pub name: String,
    pub spec: SourceSpec,
}

impl AudioSource {
    pub fn channels(&self) -> usize {
        match &self.spec {
            SourceSpec::File { channels, .. } => *channels as usize,
            SourceSpec::Generated { generator } => generator.channels(),
        }
    }

    /// Length in frames at `project_rate`.
    pub fn frames(&self, project_rate: u32) -> i64 {
        match &self.spec {
            SourceSpec::File {
                frames,
                sample_rate,
                ..
            } => {
                (*frames as f64 * project_rate as f64 / (*sample_rate).max(1) as f64).round() as i64
            }
            SourceSpec::Generated { generator } => generator.frames(project_rate) as i64,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Marker {
    pub id: MarkerId,
    pub position: MusicalTime,
    pub name: String,
}

/// A named part of the arrangement (Intro, Verse, Chorus …) on the
/// arranger lane.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Section {
    pub id: faderframe_core::SectionId,
    pub name: String,
    pub start: MusicalTime,
    pub end: MusicalTime,
    pub color: crate::TrackColor,
}

/// A range in musical time (`start < end`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MusicalRange {
    pub start: MusicalTime,
    pub end: MusicalTime,
}

impl MusicalRange {
    pub fn new(start: MusicalTime, end: MusicalTime) -> Option<Self> {
        (end > start).then_some(Self { start, end })
    }
}

/// The persistent multitrack project.
///
/// Contains only persistent data: no engine handles, plugin instances, GUI
/// objects or realtime state. All mutation from editors goes through
/// [`crate::Command`]s so it can be undone and synchronised to the engine.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Project {
    pub name: String,
    /// Nominal sample rate: clip offsets/lengths are frames at this rate.
    pub sample_rate: u32,
    pub timeline: Timeline,
    #[serde(default)]
    pub markers: Vec<Marker>,
    #[serde(default)]
    pub loop_range: Option<MusicalRange>,
    #[serde(default)]
    pub loop_enabled: bool,
    /// Recording only happens inside this range while punch is enabled.
    #[serde(default)]
    pub punch_range: Option<MusicalRange>,
    #[serde(default)]
    pub punch_enabled: bool,
    /// Subtle analogue leakage between adjacent mixer audio/instrument channels.
    #[serde(default)]
    pub crosstalk: bool,
    /// Display order. Contains exactly one [`TrackKind::Master`].
    pub tracks: Vec<Track>,
    #[serde(default)]
    pub clips: BTreeMap<ClipId, Clip>,
    #[serde(default)]
    pub sources: BTreeMap<AudioSourceId, AudioSource>,
    /// Controller mappings (MIDI learn).
    #[serde(default)]
    pub midi_mappings: Vec<crate::MidiMapping>,
    /// Sections of the arrangement, by start.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sections: Vec<Section>,
    /// Track groups (members name theirs in `Track::group`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub groups: Vec<crate::TrackGroup>,
    /// The album: songs and how they are delivered.
    #[serde(default, skip_serializing_if = "crate::album::Album::is_default")]
    pub album: crate::album::Album,
    /// Key changes, by position (empty: no key set).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub keys: Vec<crate::KeyChange>,
    /// The chord track, by start.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub chords: Vec<crate::ChordEvent>,
    /// The words along the timeline (transcribed or typed), by start.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub lyrics: Vec<crate::lyrics::LyricLine>,
    /// The ADR cue list.
    #[serde(default, skip_serializing_if = "crate::adr::Adr::is_empty")]
    pub adr: crate::adr::Adr,
    /// The clip launcher's scenes and slots.
    #[serde(default, skip_serializing_if = "crate::launcher::Launcher::is_empty")]
    pub launcher: crate::launcher::Launcher,
    /// Clip aliases: clips with the same link share their content (an edit
    /// of one reaches the others; position, name, colour and mute are each
    /// one's own).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub clip_links: BTreeMap<ClipId, faderframe_core::ClipLinkId>,
    /// Picture: video tracks, their clips and files.
    #[serde(default, skip_serializing_if = "crate::video::Video::is_empty")]
    pub video: crate::video::Video,
    /// The timecode the project counts in (`None`: not set; the display
    /// then counts from 00:00:00:00 at 25 fps).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timecode: Option<crate::video::ProjectTimecode>,
    #[serde(default)]
    pub ids: IdAllocator,
}

impl Project {
    /// An empty project containing only the master track.
    pub fn new(name: impl Into<String>, sample_rate: u32) -> Self {
        let mut ids = IdAllocator::default();
        let master = Track::new(
            ids.allocate(),
            TrackKind::Master,
            "Master",
            TrackColor::rgb(0xb0, 0xb4, 0xbc),
        )
        .with_layout(faderframe_core::ChannelLayout::Stereo);
        Self {
            name: name.into(),
            sample_rate,
            timeline: Timeline::default(),
            markers: Vec::new(),
            loop_range: None,
            loop_enabled: false,
            punch_range: None,
            punch_enabled: false,
            crosstalk: false,
            tracks: vec![master],
            clips: BTreeMap::new(),
            sources: BTreeMap::new(),
            midi_mappings: Vec::new(),
            groups: Vec::new(),
            sections: Vec::new(),
            album: crate::album::Album::default(),
            keys: Vec::new(),
            chords: Vec::new(),
            lyrics: Vec::new(),
            adr: Default::default(),
            launcher: crate::launcher::Launcher::default(),
            clip_links: BTreeMap::new(),
            video: crate::video::Video::default(),
            timecode: None,
            ids,
        }
    }

    /// The other clips sharing `clip`'s content (its aliases).
    pub fn linked_clips(&self, clip: ClipId) -> Vec<ClipId> {
        let Some(link) = self.clip_links.get(&clip) else {
            return Vec::new();
        };
        self.clip_links
            .iter()
            .filter(|(c, l)| *l == link && **c != clip && self.clips.contains_key(c))
            .map(|(c, _)| *c)
            .collect()
    }

    pub fn group(&self, id: faderframe_core::GroupId) -> Option<&crate::TrackGroup> {
        self.groups.iter().find(|g| g.id == id)
    }

    /// Members of a group, in track order.
    pub fn group_members(&self, id: faderframe_core::GroupId) -> Vec<TrackId> {
        self.tracks
            .iter()
            .filter(|t| t.group == Some(id))
            .map(|t| t.id)
            .collect()
    }

    /// The VCAs scaling `track`: its own, then the one that VCA is assigned
    /// to, and so on (loops and missing VCAs end the chain).
    pub fn vca_chain(&self, track: &Track) -> Vec<&Track> {
        let mut chain: Vec<&Track> = Vec::new();
        let mut next = track.vca;
        while let Some(id) = next {
            let Some(v) = self.track(id).filter(|v| v.kind == TrackKind::Vca) else {
                break;
            };
            if v.id == track.id || chain.iter().any(|c| c.id == v.id) {
                break;
            }
            chain.push(v);
            next = v.vca;
        }
        chain
    }

    /// The folders `track` is in: its own, then the one that folder is in,
    /// and so on (loops and missing folders end the chain).
    pub fn folder_chain(&self, track: &Track) -> Vec<&Track> {
        let mut chain: Vec<&Track> = Vec::new();
        let mut next = track.folder;
        while let Some(id) = next {
            let Some(f) = self.track(id).filter(|f| f.kind == TrackKind::Folder) else {
                break;
            };
            if f.id == track.id || chain.iter().any(|c| c.id == f.id) {
                break;
            }
            chain.push(f);
            next = f.folder;
        }
        chain
    }

    /// Is `track` inside `folder` (directly or in a folder inside it)?
    pub fn in_folder(&self, track: &Track, folder: TrackId) -> bool {
        self.folder_chain(track).iter().any(|f| f.id == folder)
    }

    /// The tracks as the editors show them: each folder followed by what it
    /// holds (in project order), whatever the project order is.
    pub fn folder_order(&self) -> Vec<&Track> {
        let parent = |t: &Track| self.folder_chain(t).first().map(|f| f.id);
        let mut out = Vec::with_capacity(self.tracks.len());
        let mut stack: Vec<(Option<TrackId>, usize)> = vec![(None, 0)];
        // Depth first: the children of a folder right after it.
        while let Some((at, from)) = stack.pop() {
            let next = self.tracks[from..]
                .iter()
                .position(|t| parent(t) == at)
                .map(|i| from + i);
            let Some(i) = next else { continue };
            let t = &self.tracks[i];
            out.push(t);
            stack.push((at, i + 1));
            if t.kind == TrackKind::Folder {
                stack.push((Some(t.id), 0));
            }
        }
        out
    }

    /// Tracks assigned to a VCA directly.
    pub fn vca_members(&self, vca: TrackId) -> Vec<TrackId> {
        self.tracks
            .iter()
            .filter(|t| t.vca == Some(vca))
            .map(|t| t.id)
            .collect()
    }

    pub fn track(&self, id: TrackId) -> Option<&Track> {
        self.tracks.iter().find(|t| t.id == id)
    }

    pub fn track_mut(&mut self, id: TrackId) -> Option<&mut Track> {
        self.tracks.iter_mut().find(|t| t.id == id)
    }

    pub fn track_index(&self, id: TrackId) -> Option<usize> {
        self.tracks.iter().position(|t| t.id == id)
    }

    pub fn master(&self) -> Option<&Track> {
        self.tracks.iter().find(|t| t.kind == TrackKind::Master)
    }

    pub fn master_id(&self) -> Option<TrackId> {
        self.master().map(|t| t.id)
    }

    pub fn clip(&self, id: ClipId) -> Option<&Clip> {
        self.clips.get(&id)
    }

    pub fn clip_mut(&mut self, id: ClipId) -> Option<&mut Clip> {
        self.clips.get_mut(&id)
    }

    /// Clips of a track, sorted by start.
    pub fn clips_of(&self, track: TrackId) -> Vec<&Clip> {
        let mut v: Vec<&Clip> = self
            .track(track)
            .map(|t| t.clips.iter().filter_map(|c| self.clips.get(c)).collect())
            .unwrap_or_default();
        v.sort_by_key(|c| (c.start, c.id));
        v
    }

    /// Musical end of the last clip (at least one bar).
    pub fn content_end(&self) -> MusicalTime {
        self.clips
            .values()
            .map(|c| c.end(&self.timeline, self.sample_rate))
            .max()
            .unwrap_or(MusicalTime::ZERO)
            .max(self.timeline.meter.bar_start(1))
    }

    /// The track that `track`'s output feeds, if it is another track.
    pub fn output_target(&self, track: &Track) -> Option<TrackId> {
        match track.output {
            OutputRouting::Master => {
                if track.kind == TrackKind::Master {
                    None
                } else {
                    self.master_id()
                }
            }
            OutputRouting::Track { track: t } => Some(t),
            OutputRouting::Hardware { .. } | OutputRouting::None => None,
        }
    }

    /// The channel format `track`'s strip mixes into: its destination
    /// track's, else its own (hardware outputs, not connected).
    pub fn destination_layout(&self, track: &Track) -> faderframe_core::ChannelLayout {
        match self.output_target(track).and_then(|t| self.track(t)) {
            Some(dst) if dst.kind.has_audio() => dst.layout,
            _ => track.layout,
        }
    }

    /// The surround bed `track` is panned into, if any: a mono or stereo
    /// track feeding a bed (a bed into another is folded, not panned).
    pub fn surround_panned(&self, track: &Track) -> Option<faderframe_core::SurroundFormat> {
        if !track.kind.has_audio()
            || matches!(track.layout, faderframe_core::ChannelLayout::Surround(_))
        {
            return None;
        }
        match self.destination_layout(track) {
            faderframe_core::ChannelLayout::Surround(f) => Some(f),
            _ => None,
        }
    }

    /// Whether `track` is an object: marked so, panned into the master's
    /// bed and feeding the master directly. Objects skip the master's
    /// inserts and fader (they join its output, as a renderer adds them)
    /// and are delivered with their place as metadata.
    pub fn is_object(&self, track: &Track) -> bool {
        track.object && self.may_be_object(track)
    }

    /// Whether `track` can be an object: panned into the master's bed,
    /// feeding the master directly.
    pub fn may_be_object(&self, track: &Track) -> bool {
        track.kind != TrackKind::Master
            && self.surround_panned(track).is_some()
            && self
                .output_target(track)
                .is_some_and(|d| Some(d) == self.master_id())
    }

    /// The object tracks, in track order.
    pub fn objects(&self) -> impl Iterator<Item = &Track> {
        self.tracks.iter().filter(|t| self.is_object(t))
    }

    /// Track-to-track signal edges (outputs and enabled sends).
    pub fn routing_edges(&self) -> Vec<(TrackId, TrackId)> {
        let mut edges = Vec::new();
        for t in &self.tracks {
            if let Some(dst) = self.output_target(t) {
                edges.push((t.id, dst));
            }
            for s in t.sends.iter().filter(|s| s.enabled) {
                edges.push((t.id, s.target));
            }
            // A plugin's extra output feeding this track.
            if let Some(src) = self.plugin_output_source(t) {
                edges.push((src, t.id));
            }
        }
        edges
    }

    /// The track whose plugin's output bus `t` takes as its input.
    pub fn plugin_output_source(&self, t: &Track) -> Option<TrackId> {
        let (plugin, _) = t.input.plugin_output()?;
        self.tracks
            .iter()
            .find(|o| o.id != t.id && o.slots().iter().any(|s| s.id == plugin))
            .map(|o| o.id)
    }

    /// Per plugin, the highest of its output buses a track takes (0: only
    /// its main one).
    pub fn taken_plugin_outputs(&self) -> HashMap<PluginInstanceId, u16> {
        let mut out: HashMap<PluginInstanceId, u16> = HashMap::new();
        for t in &self.tracks {
            if let Some((plugin, bus)) = t.input.plugin_output()
                && t.kind.has_audio()
                && t.kind != TrackKind::Midi
            {
                let e = out.entry(plugin).or_default();
                *e = (*e).max(bus);
            }
        }
        out
    }

    /// Per plugin, which of its output buses tracks take (bit `b` for bus
    /// `b` < 64; the main's bit only when a track takes it).
    pub fn taken_plugin_buses(&self) -> HashMap<PluginInstanceId, u64> {
        let mut out: HashMap<PluginInstanceId, u64> = HashMap::new();
        for t in &self.tracks {
            if let Some((plugin, bus)) = t.input.plugin_output()
                && t.kind.has_audio()
                && t.kind != TrackKind::Midi
                && bus < 64
            {
                *out.entry(plugin).or_default() |= 1 << bus;
            }
        }
        out
    }

    /// Tracks taking output buses of `plugin`.
    pub fn plugin_output_tracks(&self, plugin: PluginInstanceId) -> Vec<&Track> {
        self.tracks
            .iter()
            .filter(|t| t.input.plugin_output().is_some_and(|(p, _)| p == plugin))
            .collect()
    }

    /// Routing edges plus sidechain feeds (the source must be processed
    /// first; not a signal route for solo).
    pub fn dependency_edges(&self) -> Vec<(TrackId, TrackId)> {
        let mut edges = self.routing_edges();
        for t in &self.tracks {
            for s in t
                .inserts
                .iter()
                .chain(t.instrument.iter())
                .chain(t.preamp.iter())
            {
                if let Some(src) = s.sidechain {
                    edges.push((src, t.id));
                }
            }
        }
        edges
    }

    /// Is `to` reachable from `from` along dependency edges (plus an
    /// optional extra edge being considered)?
    pub fn reaches(&self, from: TrackId, to: TrackId, extra: Option<(TrackId, TrackId)>) -> bool {
        let mut edges = self.dependency_edges();
        edges.extend(extra);
        let mut stack = vec![from];
        let mut seen = HashSet::new();
        while let Some(n) = stack.pop() {
            if n == to {
                return true;
            }
            if !seen.insert(n) {
                continue;
            }
            stack.extend(edges.iter().filter(|(a, _)| *a == n).map(|(_, b)| *b));
        }
        false
    }

    /// Would routing `from → to` create a feedback loop?
    pub fn would_cycle(&self, from: TrackId, to: TrackId) -> bool {
        from == to || self.reaches(to, from, None)
    }

    /// Tracks audible under the current solo state, or `None` when nothing
    /// is soloed.
    ///
    /// Solo-in-place semantics: a soloed track stays audible together with
    /// everything it feeds (its buses, aux returns, master) and everything
    /// feeding it (sources of a soloed bus). Explicit mutes still apply.
    pub fn solo_audible(&self) -> Option<HashSet<TrackId>> {
        // A soloed VCA solos the tracks it scales, a soloed folder the
        // tracks in it.
        let soloed: Vec<TrackId> = self
            .tracks
            .iter()
            .filter(|t| {
                t.solo
                    || self.vca_chain(t).iter().any(|v| v.solo)
                    || self.folder_chain(t).iter().any(|f| f.solo)
            })
            .filter(|t| !matches!(t.kind, TrackKind::Vca | TrackKind::Folder))
            .map(|t| t.id)
            .collect();
        if soloed.is_empty()
            && self
                .tracks
                .iter()
                .any(|t| matches!(t.kind, TrackKind::Vca | TrackKind::Folder) && t.solo)
        {
            // A soloed VCA or folder without members silences everything
            // else.
            return Some(self.master_id().into_iter().collect());
        }
        if soloed.is_empty() {
            return None;
        }
        let edges = self.routing_edges();
        let mut audible: HashSet<TrackId> = HashSet::new();
        let walk = |start: TrackId, forward: bool, audible: &mut HashSet<TrackId>| {
            let mut stack = vec![start];
            let mut seen = HashSet::new();
            while let Some(n) = stack.pop() {
                if !seen.insert(n) {
                    continue;
                }
                audible.insert(n);
                for &(a, b) in &edges {
                    if forward && a == n {
                        stack.push(b);
                    } else if !forward && b == n {
                        stack.push(a);
                    }
                }
            }
        };
        for s in soloed {
            walk(s, true, &mut audible);
            walk(s, false, &mut audible);
        }
        audible.extend(self.master_id());
        Some(audible)
    }

    /// Whether a track is silenced by its own mute, a muted VCA or another
    /// track's solo.
    pub fn effectively_muted(&self, track: &Track, solo: Option<&HashSet<TrackId>>) -> bool {
        track.mute
            || self.vca_chain(track).iter().any(|v| v.mute)
            || solo.is_some_and(|set| !set.contains(&track.id))
    }

    /// Next unused palette colour.
    pub fn next_color(&self) -> TrackColor {
        TrackColor::palette(self.tracks.len().saturating_sub(1))
    }

    /// Repair invariants after loading a file written by an older/foreign
    /// version or edited by hand. Returns human-readable notes.
    pub fn repair(&mut self) -> Vec<String> {
        let mut notes = Vec::new();
        // Exactly one master.
        let masters: Vec<usize> = self
            .tracks
            .iter()
            .enumerate()
            .filter(|(_, t)| t.kind == TrackKind::Master)
            .map(|(i, _)| i)
            .collect();
        if masters.is_empty() {
            let id = self.ids.allocate();
            self.tracks.push(
                Track::new(
                    id,
                    TrackKind::Master,
                    "Master",
                    TrackColor::rgb(0xb0, 0xb4, 0xbc),
                )
                .with_layout(faderframe_core::ChannelLayout::Stereo),
            );
            notes.push("added missing master track".into());
        } else {
            for &i in masters.iter().skip(1).rev() {
                self.tracks[i].kind = TrackKind::Bus;
                notes.push(format!(
                    "extra master '{}' converted to a bus",
                    self.tracks[i].name
                ));
            }
        }
        // Reserve IDs past everything in use.
        let mut max_id = 0;
        for t in &self.tracks {
            max_id = max_id.max(t.id.raw());
            for s in &t.sends {
                max_id = max_id.max(s.id.raw());
            }
            for p in t
                .inserts
                .iter()
                .chain(t.instrument.iter())
                .chain(t.preamp.iter())
            {
                max_id = max_id.max(p.id.raw());
            }
            for l in &t.automation.lanes {
                max_id = max_id.max(l.id.raw());
            }
        }
        for c in self.clips.values() {
            max_id = max_id.max(c.id.raw());
            if let Some(m) = c.as_midi() {
                for n in &m.notes {
                    max_id = max_id.max(n.id.raw());
                }
            }
        }
        for s in &self.album.songs {
            max_id = max_id.max(s.id.raw());
            for p in &s.inserts {
                max_id = max_id.max(p.id.raw());
            }
        }
        // Links of clips that are gone (and of a clip alone) go.
        let clips = &self.clips;
        self.clip_links.retain(|c, _| clips.contains_key(c));
        let mut counts: BTreeMap<faderframe_core::ClipLinkId, usize> = BTreeMap::new();
        for l in self.clip_links.values() {
            *counts.entry(*l).or_default() += 1;
        }
        self.clip_links
            .retain(|_, l| counts.get(l).is_some_and(|n| *n > 1));
        max_id = max_id.max(self.clip_links.values().map(|l| l.raw()).max().unwrap_or(0));
        max_id = max_id
            .max(self.sources.keys().map(|k| k.raw()).max().unwrap_or(0))
            .max(self.markers.iter().map(|m| m.id.raw()).max().unwrap_or(0))
            .max(self.adr.cues.iter().map(|c| c.id.raw()).max().unwrap_or(0))
            .max(
                self.launcher
                    .scenes
                    .iter()
                    .map(|s| s.id.raw())
                    .max()
                    .unwrap_or(0),
            );
        self.ids.reserve_through(max_id);

        // Clip ↔ track membership consistency.
        let track_ids: BTreeSet<TrackId> = self.tracks.iter().map(|t| t.id).collect();
        let orphans: Vec<ClipId> = self
            .clips
            .values()
            .filter(|c| !track_ids.contains(&c.track))
            .map(|c| c.id)
            .collect();
        for id in orphans {
            self.clips.remove(&id);
            notes.push(format!("removed clip {id} on a missing track"));
        }
        // Launcher slots of clips, tracks or scenes that are gone.
        let scenes: BTreeSet<faderframe_core::SceneId> =
            self.launcher.scenes.iter().map(|s| s.id).collect();
        let clips = &self.clips;
        self.launcher.slots.retain(|k, c| {
            clips.get(c).is_some_and(|clip| clip.track == k.track) && scenes.contains(&k.scene)
        });
        let slots = &self.launcher.slots;
        self.launcher.follow.retain(|k, _| slots.contains_key(k));
        let in_slots = self.launcher_clips();
        for t in &mut self.tracks {
            t.clips
                .retain(|c| self.clips.get(c).is_some_and(|clip| clip.track == t.id));
        }
        let listed: BTreeSet<ClipId> = self
            .tracks
            .iter()
            .flat_map(|t| t.clips.iter().copied())
            .collect();
        let unlisted: Vec<(ClipId, TrackId)> = self
            .clips
            .values()
            .filter(|c| !listed.contains(&c.id) && !in_slots.contains(&c.id))
            .map(|c| (c.id, c.track))
            .collect();
        for (clip, track) in unlisted {
            if let Some(t) = self.track_mut(track) {
                t.clips.push(clip);
            }
        }
        // Dangling routing.
        let ids: BTreeSet<TrackId> = self.tracks.iter().map(|t| t.id).collect();
        for t in &mut self.tracks {
            if let OutputRouting::Track { track } = t.output
                && !ids.contains(&track)
            {
                t.output = OutputRouting::Master;
                notes.push(format!(
                    "'{}' routed to a missing track; reset to master",
                    t.name
                ));
            }
            let before = t.sends.len();
            t.sends
                .retain(|s| ids.contains(&s.target) && s.target != t.id);
            if t.sends.len() != before {
                notes.push(format!("removed dangling sends on '{}'", t.name));
            }
        }
        notes
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::AuxSend;
    use faderframe_core::SendId;

    fn project_with_bus() -> (Project, TrackId, TrackId, TrackId, TrackId) {
        let mut p = Project::new("t", 48_000);
        let a: TrackId = p.ids.allocate();
        let b: TrackId = p.ids.allocate();
        let bus: TrackId = p.ids.allocate();
        let aux: TrackId = p.ids.allocate();
        let mut ta = Track::new(a, TrackKind::Audio, "A", TrackColor::palette(0));
        ta.output = OutputRouting::Track { track: bus };
        ta.sends.push(AuxSend {
            id: SendId(100),
            target: aux,
            level_db: -6.0,
            tap: Default::default(),
            enabled: true,
        });
        let mut tb = Track::new(b, TrackKind::Audio, "B", TrackColor::palette(1));
        tb.output = OutputRouting::Track { track: bus };
        p.tracks.insert(0, ta);
        p.tracks.insert(1, tb);
        p.tracks.insert(
            2,
            Track::new(bus, TrackKind::Bus, "Bus", TrackColor::palette(2)),
        );
        p.tracks.insert(
            3,
            Track::new(aux, TrackKind::Aux, "Aux", TrackColor::palette(3)),
        );
        (p, a, b, bus, aux)
    }

    #[test]
    fn cycle_detection_follows_outputs_and_sends() {
        let (p, a, _b, bus, aux) = project_with_bus();
        assert!(p.would_cycle(bus, a)); // a → bus already
        assert!(p.would_cycle(aux, a)); // a → aux via send
        assert!(!p.would_cycle(a, aux));
        assert!(p.would_cycle(a, a));
        let master = p.master_id().unwrap();
        assert!(p.would_cycle(master, bus));
    }

    #[test]
    fn solo_in_place_semantics() {
        let (mut p, a, b, bus, aux) = project_with_bus();
        assert!(p.solo_audible().is_none());
        p.track_mut(a).unwrap().solo = true;
        let set = p.solo_audible().unwrap();
        // A plus everything downstream of it is audible; B is not.
        assert!(set.contains(&a) && set.contains(&bus) && set.contains(&aux));
        assert!(!set.contains(&b));
        let tb = p.track(b).unwrap();
        assert!(p.effectively_muted(tb, Some(&set)));
        // Soloing the bus makes its sources audible too.
        p.track_mut(a).unwrap().solo = false;
        p.track_mut(bus).unwrap().solo = true;
        let set = p.solo_audible().unwrap();
        assert!(set.contains(&a) && set.contains(&b));
        assert!(!set.contains(&aux), "aux is fed by A, not by the bus");
    }

    #[test]
    fn repair_fixes_dangling_references() {
        let (mut p, a, _b, bus, _aux) = project_with_bus();
        p.tracks.retain(|t| t.id != bus);
        p.tracks.retain(|t| t.kind != TrackKind::Master);
        let notes = p.repair();
        assert!(p.master().is_some());
        assert_eq!(p.track(a).unwrap().output, OutputRouting::Master);
        assert!(!notes.is_empty());
        let next: TrackId = p.ids.allocate();
        assert!(p.tracks.iter().all(|t| t.id.raw() < next.raw()));
    }
}
