//! Scratch pads: other arrangements of the project's own tracks, to try
//! an idea without touching the song (Studio One's scratch pads).
//!
//! A pad holds what an arrangement holds — the clips on the tracks (by
//! value, in each track's order), each track's automation, its own
//! timeline (tempo and metre), markers, sections, key changes, chords,
//! lyrics, its loop and its Play Start Marker — and shares everything else
//! with the song: the tracks themselves (mixer, devices, routing, sends,
//! freezes), the sources, video, album, setlist and the launcher. Nothing
//! is duplicated but the clips a pad holds (their media are the song's).
//!
//! The project's arrangement fields always hold the arrangement that is
//! *shown*, the song or one pad; the others are put aside as a [`Stash`].
//! [`Project::show_arrangement`] swaps them (the command
//! [`crate::Command::ShowArrangement`], an undo step of its own, so every
//! edit is undone in the arrangement it was made in), so the editors, the
//! engine, the clipboard and every command work on what is shown without
//! knowing about pads: `Project::clips` never holds a clip put aside (only
//! the shown arrangement's and the launcher's). Renders of the song take
//! [`Project::song`].
//!
//! A file always holds the song in place ([`crate::file::to_string`]
//! writes the song shown and names the pad that was shown in
//! [`ScratchPads::open`]; loading shows it again), so older versions of
//! FaderFrame open the song as it is (they leave the pads out).

use crate::{
    ChordEvent, Clip, KeyChange, Marker, MusicalRange, PlayStart, Project, Section, edit::EditError,
};
use faderframe_automation::AutomationSet;
use faderframe_core::{ClipId, ScratchPadId, TrackId};
use faderframe_timeline::{MusicalTime, TempoMap, TimeSignatureMap, Timeline};
use serde::{Deserialize, Serialize};
use std::borrow::Cow;
use std::collections::{BTreeMap, BTreeSet};

/// The project's scratch pads and which arrangement is shown.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ScratchPads {
    /// In the switcher's order.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pads: Vec<ScratchPad>,
    /// The pad the project's arrangement fields hold (`None`: the song).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shown: Option<ScratchPadId>,
    /// The song, put aside while a pad is shown.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub song: Option<Box<Stash>>,
    /// In a file: the pad to show once it is opened (a file holds the song
    /// in place). Never set in memory.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub open: Option<ScratchPadId>,
}

impl ScratchPads {
    pub fn is_empty(&self) -> bool {
        self.pads.is_empty() && self.shown.is_none() && self.song.is_none() && self.open.is_none()
    }

    pub fn pad(&self, id: ScratchPadId) -> Option<&ScratchPad> {
        self.pads.iter().find(|p| p.id == id)
    }

    pub fn index_of(&self, id: ScratchPadId) -> Option<usize> {
        self.pads.iter().position(|p| p.id == id)
    }

    /// The name an arrangement shows ("Song" for the song).
    pub fn name_of(&self, pad: Option<ScratchPadId>) -> String {
        match pad {
            None => "Song".into(),
            Some(id) => self
                .pad(id)
                .map_or_else(|| "Scratch Pad".into(), |p| p.name.clone()),
        }
    }

    /// A name no pad has yet ("Scratch Pad 1", "Scratch Pad 2" …, or
    /// `base` and a number after it).
    pub fn unused_name(&self, base: &str) -> String {
        let taken = |n: &str| self.pads.iter().any(|p| p.name.eq_ignore_ascii_case(n));
        if !base.is_empty() && !taken(base) {
            return base.to_string();
        }
        let stem = if base.is_empty() { "Scratch Pad" } else { base };
        (1..)
            .map(|i| format!("{stem} {i}"))
            .find(|n| !taken(n))
            .unwrap_or_else(|| stem.to_string())
    }

    /// Every arrangement put aside, with whose it is (`None`: the song's).
    pub fn stashes(&self) -> impl Iterator<Item = (Option<ScratchPadId>, &Stash)> + '_ {
        self.song.iter().map(|s| (None, s.as_ref())).chain(
            self.pads
                .iter()
                .filter_map(|p| p.stash.as_deref().map(|s| (Some(p.id), s))),
        )
    }

    /// The stash of an arrangement put aside (`None`: the song's).
    pub fn stash_mut(&mut self, of: Option<ScratchPadId>) -> Option<&mut Stash> {
        match of {
            None => self.song.as_deref_mut(),
            Some(id) => self
                .pads
                .iter_mut()
                .find(|p| p.id == id)
                .and_then(|p| p.stash.as_deref_mut()),
        }
    }

    fn stashes_mut(&mut self) -> impl Iterator<Item = (Option<ScratchPadId>, &mut Stash)> + '_ {
        self.song.iter_mut().map(|s| (None, s.as_mut())).chain(
            self.pads
                .iter_mut()
                .filter_map(|p| p.stash.as_deref_mut().map(|s| (Some(p.id), s))),
        )
    }
}

/// One scratch pad.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ScratchPad {
    pub id: ScratchPadId,
    pub name: String,
    /// What it holds while it is not shown (`None` while it is).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stash: Option<Box<Stash>>,
}

impl ScratchPad {
    /// A pad holding nothing, in `timeline`'s tempo and metre at its start.
    pub fn empty(id: ScratchPadId, name: impl Into<String>, timeline: &Timeline) -> Self {
        Self {
            id,
            name: name.into(),
            stash: Some(Box::new(Stash::empty(timeline))),
        }
    }
}

/// A track's part of an arrangement put aside.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct StashedTrack {
    /// Its clips, in the track's order.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub clips: Vec<ClipId>,
    #[serde(default, skip_serializing_if = "AutomationSet::is_empty")]
    pub automation: AutomationSet,
}

impl StashedTrack {
    pub fn is_empty(&self) -> bool {
        self.clips.is_empty() && self.automation.is_empty()
    }
}

/// An arrangement put aside: what [`Project::show_arrangement`] swaps.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Stash {
    #[serde(default)]
    pub timeline: Timeline,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub clips: BTreeMap<ClipId, Clip>,
    /// Each track's clips and automation (tracks with neither are left
    /// out: they have none there).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub tracks: BTreeMap<TrackId, StashedTrack>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub markers: Vec<Marker>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sections: Vec<Section>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub keys: Vec<KeyChange>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub chords: Vec<ChordEvent>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub lyrics: Vec<crate::lyrics::LyricLine>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub loop_range: Option<MusicalRange>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub loop_enabled: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub play_start: Option<PlayStart>,
}

impl Stash {
    /// Nothing in it, in `timeline`'s tempo and metre at its start (no
    /// changes).
    pub fn empty(timeline: &Timeline) -> Self {
        Self {
            timeline: Timeline {
                tempo: TempoMap::new(timeline.tempo.bpm_at(MusicalTime::ZERO)),
                meter: TimeSignatureMap::new(timeline.meter.signature_of_bar(0)),
            },
            ..Self::default()
        }
    }

    /// No clips, automation, markers, sections, keys, chords or lyrics.
    pub fn is_empty(&self) -> bool {
        self.clips.is_empty()
            && self.tracks.values().all(StashedTrack::is_empty)
            && self.markers.is_empty()
            && self.sections.is_empty()
            && self.keys.is_empty()
            && self.chords.is_empty()
            && self.lyrics.is_empty()
    }

    /// Take the shown arrangement out of `p` (the launcher's clips stay);
    /// `p` is left with an empty one.
    fn take_from(p: &mut Project) -> Self {
        let launcher = p.launcher_clips();
        let ids: Vec<ClipId> = p
            .clips
            .keys()
            .filter(|c| !launcher.contains(c))
            .copied()
            .collect();
        let mut clips = BTreeMap::new();
        for id in ids {
            if let Some(c) = p.clips.remove(&id) {
                clips.insert(id, c);
            }
        }
        let mut tracks = BTreeMap::new();
        for t in &mut p.tracks {
            let entry = StashedTrack {
                clips: std::mem::take(&mut t.clips),
                automation: std::mem::take(&mut t.automation),
            };
            if !entry.is_empty() {
                tracks.insert(t.id, entry);
            }
        }
        Self {
            timeline: std::mem::take(&mut p.timeline),
            clips,
            tracks,
            markers: std::mem::take(&mut p.markers),
            sections: std::mem::take(&mut p.sections),
            keys: std::mem::take(&mut p.keys),
            chords: std::mem::take(&mut p.chords),
            lyrics: std::mem::take(&mut p.lyrics),
            loop_range: p.loop_range.take(),
            loop_enabled: std::mem::replace(&mut p.loop_enabled, false),
            play_start: p.play_start.take(),
        }
    }

    /// Put this arrangement into `p`, whose own was taken out.
    fn put_into(mut self, p: &mut Project) {
        p.timeline = self.timeline;
        p.clips.extend(self.clips);
        for t in &mut p.tracks {
            if let Some(e) = self.tracks.remove(&t.id) {
                t.clips = e.clips;
                t.automation = e.automation;
            }
        }
        p.markers = self.markers;
        p.sections = self.sections;
        p.keys = self.keys;
        p.chords = self.chords;
        p.lyrics = self.lyrics;
        p.loop_range = self.loop_range;
        p.loop_enabled = self.loop_enabled;
        p.play_start = self.play_start;
    }

    /// Where its content lies, out to whole bars (see
    /// [`Project::arrangement_span`]) at the project rate `rate`.
    pub fn span(&self, rate: u32) -> Option<(MusicalTime, MusicalTime)> {
        content_span(self.clips.values(), &self.sections, &self.timeline, rate)
    }

    /// A copy with new ids for its clips, markers and sections (a
    /// duplicated pad: nothing in it is the original's). Automation lanes
    /// keep theirs (a lane is the same lane in every arrangement).
    pub fn with_new_ids(&self, ids: &mut faderframe_core::IdAllocator) -> Self {
        let mut out = self.clone();
        let renamed: BTreeMap<ClipId, ClipId> =
            self.clips.keys().map(|c| (*c, ids.allocate())).collect();
        out.clips = self
            .clips
            .values()
            .filter_map(|c| {
                let id = *renamed.get(&c.id)?;
                Some((id, Clip { id, ..c.clone() }))
            })
            .collect();
        for e in out.tracks.values_mut() {
            for c in &mut e.clips {
                if let Some(n) = renamed.get(c) {
                    *c = *n;
                }
            }
        }
        for m in &mut out.markers {
            m.id = ids.allocate();
        }
        for s in &mut out.sections {
            s.id = ids.allocate();
        }
        out
    }

    /// Take out `track`'s part (its entry and its clips), when it has one.
    pub(crate) fn take_track(&mut self, track: TrackId) -> Option<(StashedTrack, Vec<Clip>)> {
        let entry = self.tracks.remove(&track);
        let ids: Vec<ClipId> = self
            .clips
            .values()
            .filter(|c| c.track == track)
            .map(|c| c.id)
            .collect();
        let clips: Vec<Clip> = ids.iter().filter_map(|c| self.clips.remove(c)).collect();
        (entry.is_some() || !clips.is_empty()).then(|| (entry.unwrap_or_default(), clips))
    }

    /// Put back what [`Self::take_track`] took.
    pub(crate) fn put_track(&mut self, track: TrackId, entry: StashedTrack, clips: Vec<Clip>) {
        for c in clips {
            self.clips.insert(c.id, c);
        }
        if !entry.is_empty() {
            self.tracks.insert(track, entry);
        }
    }

    /// Make the stash consistent with `tracks` (what a file may get
    /// wrong): clips of tracks that are gone go, every clip is in its
    /// track's list and lists name only clips there are. Returns how many
    /// clips went.
    fn repair(&mut self, tracks: &BTreeSet<TrackId>) -> usize {
        let before = self.clips.len();
        self.clips.retain(|_, c| tracks.contains(&c.track));
        self.tracks.retain(|t, _| tracks.contains(t));
        let clips = &self.clips;
        for (t, e) in &mut self.tracks {
            let mut seen = BTreeSet::new();
            e.clips
                .retain(|c| clips.get(c).is_some_and(|clip| clip.track == *t) && seen.insert(*c));
        }
        let listed: BTreeSet<ClipId> = self
            .tracks
            .values()
            .flat_map(|e| e.clips.iter().copied())
            .collect();
        let unlisted: Vec<(ClipId, TrackId)> = self
            .clips
            .values()
            .filter(|c| !listed.contains(&c.id))
            .map(|c| (c.id, c.track))
            .collect();
        for (clip, track) in unlisted {
            self.tracks.entry(track).or_default().clips.push(clip);
        }
        self.tracks.retain(|_, e| !e.is_empty());
        before - self.clips.len()
    }

    /// The highest id anything in it has.
    fn max_id(&self) -> u64 {
        let clips = self.clips.values().map(|c| {
            c.as_midi()
                .map_or(0, |m| m.notes.iter().map(|n| n.id.raw()).max().unwrap_or(0))
                .max(c.id.raw())
        });
        let lanes = self
            .tracks
            .values()
            .flat_map(|e| e.automation.lanes.iter().map(|l| l.id.raw()));
        clips
            .chain(lanes)
            .chain(self.markers.iter().map(|m| m.id.raw()))
            .chain(self.sections.iter().map(|s| s.id.raw()))
            .max()
            .unwrap_or(0)
    }
}

/// From the bar line at or before the first clip or section to the one at
/// or after the last one's end (`None`: neither).
fn content_span<'a>(
    clips: impl Iterator<Item = &'a Clip>,
    sections: &[Section],
    tl: &Timeline,
    rate: u32,
) -> Option<(MusicalTime, MusicalTime)> {
    let mut spans: Vec<(MusicalTime, MusicalTime)> =
        clips.map(|c| (c.start, c.end(tl, rate))).collect();
    spans.extend(sections.iter().map(|s| (s.start, s.end)));
    let start = spans.iter().map(|s| s.0).min()?;
    let end = spans.iter().map(|s| s.1).max()?;
    Some(crate::arrange::bar_lines(
        &tl.meter,
        start.max(MusicalTime::ZERO),
        end,
    ))
}

impl Project {
    /// The scratch pad shown (`None`: the song).
    pub fn shown_pad(&self) -> Option<ScratchPadId> {
        self.scratch_pads.shown
    }

    /// Show the song (`None`) or a scratch pad: the arrangement shown now
    /// is put aside and the other one takes its place. Returns the one
    /// shown before.
    pub fn show_arrangement(
        &mut self,
        pad: Option<ScratchPadId>,
    ) -> Result<Option<ScratchPadId>, EditError> {
        let from = self.scratch_pads.shown;
        if pad == from {
            return Ok(from);
        }
        if let Some(id) = pad
            && self.scratch_pads.pad(id).is_none()
        {
            return Err(EditError::Invalid(format!("no scratch pad {id}")));
        }
        let incoming = match pad {
            None => self.scratch_pads.song.take(),
            Some(id) => self
                .scratch_pads
                .pads
                .iter_mut()
                .find(|p| p.id == id)
                .and_then(|p| p.stash.take()),
        };
        let outgoing = Stash::take_from(self);
        incoming.map(|s| *s).unwrap_or_default().put_into(self);
        let outgoing = Some(Box::new(outgoing));
        match from {
            None => self.scratch_pads.song = outgoing,
            Some(id) => {
                if let Some(p) = self.scratch_pads.pads.iter_mut().find(|p| p.id == id) {
                    p.stash = outgoing;
                }
            }
        }
        self.scratch_pads.shown = pad;
        Ok(from)
    }

    /// The project with the song shown: itself when it is, else a copy
    /// (what renders of the song and files take).
    pub fn song(&self) -> Cow<'_, Project> {
        if self.scratch_pads.shown.is_none() {
            return Cow::Borrowed(self);
        }
        Cow::Owned(self.clone().into_song())
    }

    /// This project with the song shown.
    pub fn into_song(mut self) -> Project {
        // Showing the song never fails (it is always there).
        let _ = self.show_arrangement(None);
        self
    }

    /// The clips of the arrangements put aside.
    pub fn stashed_clips(&self) -> impl Iterator<Item = &Clip> + '_ {
        self.scratch_pads
            .stashes()
            .flat_map(|(_, s)| s.clips.values())
    }

    /// Where the shown arrangement's content lies, out to whole bars: from
    /// the bar line at or before the first clip or section to the one at or
    /// after the last one's end (`None`: no content).
    pub fn arrangement_span(&self) -> Option<(MusicalTime, MusicalTime)> {
        let launcher = self.launcher_clips();
        content_span(
            self.clips.values().filter(|c| !launcher.contains(&c.id)),
            &self.sections,
            &self.timeline,
            self.sample_rate,
        )
    }

    /// Take a track's parts out of every arrangement put aside (its
    /// removal), with whose they are.
    pub(crate) fn take_stashed_track(
        &mut self,
        track: TrackId,
    ) -> Vec<(Option<ScratchPadId>, StashedTrack, Vec<Clip>)> {
        self.scratch_pads
            .stashes_mut()
            .filter_map(|(of, s)| s.take_track(track).map(|(e, c)| (of, e, c)))
            .collect()
    }

    /// Put back what [`Self::take_stashed_track`] took.
    pub(crate) fn put_stashed_track(
        &mut self,
        track: TrackId,
        parts: Vec<(Option<ScratchPadId>, StashedTrack, Vec<Clip>)>,
    ) {
        for (of, entry, clips) in parts {
            if let Some(s) = self.scratch_pads.stash_mut(of) {
                s.put_track(track, entry, clips);
            }
        }
    }

    /// Repair the scratch pads after loading (see [`Project::repair`],
    /// which has reserved the other ids): reserves theirs, returns notes.
    pub(crate) fn repair_scratch_pads(&mut self) -> Vec<String> {
        let mut notes = Vec::new();
        let used = self.scratch_pad_max_id();
        self.ids.reserve_through(used);
        let pads = &mut self.scratch_pads;
        let mut seen = BTreeSet::new();
        pads.pads.retain(|p| seen.insert(p.id));
        // A pad shown without the song put aside: the fields are taken as
        // the song's.
        if pads.shown.is_some() && pads.song.is_none() {
            pads.shown = None;
            notes.push("a scratch pad was shown without the song: showing the song".into());
        }
        // The song put aside without a pad shown: kept as a pad.
        if pads.shown.is_none()
            && let Some(song) = pads.song.take()
        {
            let id = self.ids.allocate();
            let name = pads.unused_name("Recovered Song");
            pads.pads.push(ScratchPad {
                id,
                name,
                stash: Some(song),
            });
            notes.push("kept a stray copy of the song as a scratch pad".into());
        }
        if let Some(id) = pads.shown
            && pads.pad(id).is_none()
        {
            let name = pads.unused_name("");
            pads.pads.push(ScratchPad {
                id,
                name,
                stash: None,
            });
        }
        let shown = pads.shown;
        for p in &mut pads.pads {
            if Some(p.id) == shown {
                p.stash = None;
            } else if p.stash.is_none() {
                p.stash = Some(Box::default());
            }
        }
        let tracks: BTreeSet<TrackId> = self.tracks.iter().map(|t| t.id).collect();
        for (of, s) in self.scratch_pads.stashes_mut() {
            let gone = s.repair(&tracks);
            if gone > 0 {
                notes.push(format!(
                    "removed {gone} clip{} of missing tracks from {}",
                    if gone == 1 { "" } else { "s" },
                    if of.is_some() {
                        "a scratch pad"
                    } else {
                        "the song"
                    }
                ));
            }
        }
        notes
    }

    /// The highest id the scratch pads and what they hold use.
    fn scratch_pad_max_id(&self) -> u64 {
        let pads = &self.scratch_pads;
        pads.stashes()
            .map(|(_, s)| s.max_id())
            .chain(pads.pads.iter().map(|p| p.id.raw()))
            .chain(pads.shown.iter().chain(pads.open.iter()).map(|p| p.raw()))
            .max()
            .unwrap_or(0)
    }
}
