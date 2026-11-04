//! Three-way merge of project versions: two lines of edits that went their
//! own ways from a common state (`base`) — ours, the project as it is, and
//! theirs, another version or a copy someone else worked on — joined into
//! one project, with the places both changed listed as conflicts.
//!
//! Things are matched by their ids, at a fine grain: a track's fields, its
//! plug-ins, sends and automation lanes each on their own (automation
//! points by the stretches of time each side changed), clips field by field
//! (position, gain, fades, content), MIDI notes one by one, markers,
//! sections, tempo and time signatures, mixer settings, the launcher, the
//! album and every side table of the project the same way. A change only
//! one side made is taken; the same change on both sides is taken once.
//! A conflict is where both changed the same thing differently, or one
//! removed what the other changed; each is resolved as a whole for the
//! thing it is in (a track, a clip, a note, a plug-in…): ours (the
//! default), theirs, or — for clips, notes, markers and sections — both
//! (a copy of theirs beside ours). Merging again with other choices
//! ([`MergeOptions::resolutions`], keyed by [`Conflict::key`]) makes the
//! project for them.
//!
//! What must not be combined from two sides is taken whole: another enum
//! variant, a thing replaced by another (a new id), a range (start and end
//! move together), a plug-in's settings once either side loaded a new
//! state (the blob and its values), take folders (their comp names takes
//! by place) and the clip edits whose parts depend on each other (warp,
//! pitch, spectral edits, clip effects).
//!
//! Ids: both sides count from one allocator, so theirs' new ids are moved
//! past ours before anything is compared (read through
//! [`faderframe_core::id::translate`], which reaches every id wherever it
//! sits). Ids below the point where the two lines branched are shared;
//! ranges known from earlier merges ([`crate::lineage`]) are read as the
//! things they already are. The merged project is checked thing by thing
//! as it is built back into types; anything that would not read is taken
//! whole from one side and listed. [`Project::repair`] then makes the
//! cross references consistent (a clip on a track that went goes too).
//!
//! The work is done on the projects' JSON form (`json`), so a field added
//! to the model merges without being named here.

use crate::{Clip, ClipContent, Project, Track};
use faderframe_core::id::translate::{self, IdRange};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

mod json;
mod words;

use json::{Merger, Record, Seg};

/// How a conflict is resolved.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Resolution {
    /// As this project has it.
    Ours,
    /// As the other version has it.
    Theirs,
    /// Ours in place and a copy of theirs beside it.
    Both,
}

impl Resolution {
    pub fn label(self) -> &'static str {
        match self {
            Resolution::Ours => "Keep Ours",
            Resolution::Theirs => "Take Theirs",
            Resolution::Both => "Keep Both",
        }
    }

    /// From a dev action's word.
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "ours" | "mine" | "here" => Some(Resolution::Ours),
            "theirs" | "there" => Some(Resolution::Theirs),
            "both" => Some(Resolution::Both),
            _ => None,
        }
    }
}

/// What kind of disagreement a conflict is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConflictKind {
    /// Both changed it, each its own way.
    BothChanged,
    /// This project removed it, the other version changed it.
    RemovedHere,
    /// The other version removed it, this project changed it.
    RemovedThere,
    /// Both reordered a list, each its own way.
    Order,
    /// Both changed the same stretch of a list placed in time
    /// (automation points, tempo changes, chords…).
    SamePlace,
    /// The two sides' changes would not make one valid thing together.
    Uncombinable,
}

/// One field both sides set differently.
#[derive(Clone, Debug, PartialEq)]
pub struct Difference {
    pub field: String,
    pub ours: String,
    pub theirs: String,
}

/// A place both sides changed.
#[derive(Clone, Debug, PartialEq)]
pub struct Conflict {
    /// Stable between merges of the same versions (resolutions are keyed
    /// by it).
    pub key: String,
    /// What it is about, in words ("Track ‘Bass’", "Clip ‘Melody’ on
    /// ‘Lead’").
    pub what: String,
    pub kind: ConflictKind,
    pub differences: Vec<Difference>,
    /// The choices that make sense, the default first.
    pub choices: Vec<Resolution>,
    pub chosen: Resolution,
}

impl Conflict {
    /// One line: what, and how the sides differ.
    pub fn summary(&self) -> String {
        let how = match self.kind {
            ConflictKind::RemovedHere => "removed here, changed in the other version".to_string(),
            ConflictKind::RemovedThere => "changed here, removed in the other version".to_string(),
            ConflictKind::Order => "reordered differently on both sides".to_string(),
            ConflictKind::Uncombinable => {
                "changed on both sides in ways that do not combine".into()
            }
            ConflictKind::SamePlace | ConflictKind::BothChanged => self
                .differences
                .iter()
                .map(|d| format!("{}: {} here, {} there", d.field, d.ours, d.theirs))
                .collect::<Vec<_>>()
                .join("; "),
        };
        format!("{} — {how}", self.what)
    }
}

/// How to merge.
#[derive(Clone, Debug, Default)]
pub struct MergeOptions {
    /// Ranges reading theirs' ids as this line has them, known from earlier
    /// merges between the two lines ([`crate::lineage::Lineage::known_ranges`]).
    pub known: Vec<IdRange>,
    /// The id allocator where the two lines branched: theirs' ids from
    /// here on are their own (`None`: the base's).
    pub branch_ids: Option<u64>,
    /// Choices made, by [`Conflict::key`] (the rest: ours).
    pub resolutions: BTreeMap<String, Resolution>,
}

/// A merge done.
#[derive(Clone, Debug)]
pub struct Merged {
    /// The merged project (the song shown).
    pub project: Project,
    pub conflicts: Vec<Conflict>,
    /// What the merge changes in ours, in words.
    pub changes: Vec<String>,
    /// What had to be put right afterwards.
    pub notes: Vec<String>,
    /// Theirs as merged: its ids as they are in the merged project.
    pub theirs: Project,
    /// How theirs' own new ids moved: `[start, end)` by `by` (for the
    /// lineage of the merged state).
    pub moved: Option<(u64, u64, i64)>,
}

#[derive(Debug, thiserror::Error)]
pub enum MergeError {
    #[error("cannot merge: {0}")]
    Json(#[from] serde_json::Error),
    #[error("cannot merge: {0}")]
    Other(String),
}

/// A project as JSON, and the largest id in it.
fn as_json(p: &Project) -> Result<(Value, u64), MergeError> {
    let (v, largest) = translate::noting_largest(|| serde_json::to_value(p));
    Ok((v?, largest))
}

/// Where the allocator must be to be past everything in `p`.
fn next_past(p: &Project, largest: u64) -> u64 {
    p.ids.peek().max(largest + 1)
}

/// Merge `theirs` into `ours`, both descending from `base`.
pub fn merge(
    base: &Project,
    ours: &Project,
    theirs: &Project,
    options: &MergeOptions,
) -> Result<Merged, MergeError> {
    let (base, ours, theirs) = (base.song(), ours.song(), theirs.song());
    let (bv, b_largest) = as_json(&base)?;
    let (ov, o_largest) = as_json(&ours)?;
    let (tv_raw, t_largest) = as_json(&theirs)?;
    // Theirs' ids: known ranges first, then their own new ones past ours.
    let from = options
        .branch_ids
        .unwrap_or_else(|| next_past(&base, b_largest));
    let theirs_end = next_past(&theirs, t_largest);
    let ours_next = next_past(&ours, o_largest);
    let mut ranges = options.known.clone();
    let mut moved = None;
    if theirs_end > from {
        let to = ours_next.max(from);
        let by = to as i64 - from as i64;
        if by != 0 {
            ranges.push(IdRange {
                start: from,
                end: theirs_end,
                by,
            });
            moved = Some((from, theirs_end, by));
        }
    }
    let theirs_moved: Project =
        translate::with_shifts(&ranges, || serde_json::from_value(tv_raw.clone()))?;
    let (tv, t2_largest) = as_json(&theirs_moved)?;
    let merged_next = ours_next
        .max(theirs_end.wrapping_add_signed(moved.map_or(0, |m| m.2)))
        .max(t2_largest + 1);

    let mut merger = Merger::new(&options.resolutions);
    let mut value = merger.merge_project(&bv, &ov, &tv);
    let mut records = merger.records;
    // Built back into types thing by thing: what will not read is taken
    // whole from one side.
    let mut project = typed(&mut value, &ov, &tv, &options.resolutions, &mut records)?;
    project.ids.reserve_through(merged_next.saturating_sub(1));
    let names = words::Names {
        base: &base,
        ours: &ours,
        theirs: &theirs_moved,
        json: [&ov, &tv, &bv],
    };
    let conflicts = conflicts_of(&records, &options.resolutions, &names);
    keep_both(&mut project, &theirs_moved, &conflicts);
    rebuild_clip_lists(&mut project, &ours);
    let notes = project.repair();
    let changes = crate::compare::differences(&ours, &project);
    Ok(Merged {
        project,
        conflicts,
        changes,
        notes,
        theirs: theirs_moved,
        moved,
    })
}

/// The merged JSON as a project. Tracks and clips are read one by one
/// first; one that will not read is taken whole (ours, or theirs where
/// that was chosen or ours has none). Then the whole; a top-level field
/// that still keeps it from reading is taken from ours.
fn typed(
    value: &mut Value,
    ours: &Value,
    theirs: &Value,
    resolutions: &BTreeMap<String, Resolution>,
    records: &mut Vec<Record>,
) -> Result<Project, MergeError> {
    let mut fallback = |path: Vec<Seg>, o: Option<&Value>, t: Option<&Value>| -> Option<Value> {
        let unit = json::Unit {
            key: json::path_key(&path),
            path: path.clone(),
            both: false,
        };
        let pick_theirs = resolutions.get(&unit.key) == Some(&Resolution::Theirs) || o.is_none();
        records.push(Record {
            unit,
            kind: ConflictKind::Uncombinable,
            at: path,
            ours: json::brief(o),
            theirs: json::brief(t),
            span: None,
        });
        if pick_theirs { t.cloned() } else { o.cloned() }
    };
    // Tracks by id.
    if let Some(tracks) = value.get_mut("tracks").and_then(Value::as_array_mut) {
        let side = |v: &Value, id: &Value| -> Option<Value> {
            v.get("tracks")?
                .as_array()?
                .iter()
                .find(|t| t.get("id") == Some(id))
                .cloned()
        };
        let mut i = 0;
        while i < tracks.len() {
            if serde_json::from_value::<Track>(tracks[i].clone()).is_err() {
                let id = tracks[i].get("id").cloned().unwrap_or(Value::Null);
                let path = vec![
                    Seg::Field("tracks".into()),
                    Seg::Key(match &id {
                        Value::Number(n) => n.to_string(),
                        other => other.to_string(),
                    }),
                ];
                match fallback(path, side(ours, &id).as_ref(), side(theirs, &id).as_ref()) {
                    Some(v) => tracks[i] = v,
                    None => {
                        tracks.remove(i);
                        continue;
                    }
                }
            }
            i += 1;
        }
    }
    // Clips by id.
    if let Some(clips) = value.get_mut("clips").and_then(Value::as_object_mut) {
        let bad: Vec<String> = clips
            .iter()
            .filter(|(_, c)| serde_json::from_value::<Clip>((*c).clone()).is_err())
            .map(|(k, _)| k.clone())
            .collect();
        for k in bad {
            let side = |v: &Value| v.get("clips").and_then(|c| c.get(&k)).cloned();
            let path = vec![Seg::Field("clips".into()), Seg::Key(k.clone())];
            match fallback(path, side(ours).as_ref(), side(theirs).as_ref()) {
                Some(v) => {
                    clips.insert(k, v);
                }
                None => {
                    clips.remove(&k);
                }
            }
        }
    }
    if let Ok(p) = serde_json::from_value::<Project>(value.clone()) {
        return Ok(p);
    }
    let keys: Vec<String> = value
        .as_object()
        .map(|m| m.keys().cloned().collect())
        .unwrap_or_default();
    // One field at a time, then as many as it takes.
    let differing: Vec<String> = keys
        .into_iter()
        .filter(|k| ours.get(k) != value.get(k))
        .collect();
    let with_ours = |v: &Value, k: &str| -> Value {
        let mut trial = v.clone();
        if let (Some(obj), Some(o)) = (trial.as_object_mut(), ours.get(k)) {
            obj.insert(k.to_string(), o.clone());
        } else if let Some(obj) = trial.as_object_mut() {
            obj.remove(k);
        }
        trial
    };
    for k in &differing {
        let trial = with_ours(value, k);
        if let Ok(p) = serde_json::from_value::<Project>(trial) {
            fallback(vec![Seg::Field(k.clone())], ours.get(k), theirs.get(k));
            return Ok(p);
        }
    }
    for k in &differing {
        *value = with_ours(value, k);
        fallback(vec![Seg::Field(k.clone())], ours.get(k), theirs.get(k));
        if let Ok(p) = serde_json::from_value::<Project>(value.clone()) {
            return Ok(p);
        }
    }
    Ok(serde_json::from_value(ours.clone())?)
}

/// The records grouped into conflicts, one per unit.
fn conflicts_of(
    records: &[Record],
    resolutions: &BTreeMap<String, Resolution>,
    names: &words::Names,
) -> Vec<Conflict> {
    let mut order: Vec<String> = Vec::new();
    let mut by_unit: BTreeMap<String, Vec<&Record>> = BTreeMap::new();
    for r in records {
        if !by_unit.contains_key(&r.unit.key) {
            order.push(r.unit.key.clone());
        }
        by_unit.entry(r.unit.key.clone()).or_default().push(r);
    }
    order
        .into_iter()
        .filter_map(|key| {
            let list = by_unit.get(&key)?;
            let first = list.first()?;
            let kind = list
                .iter()
                .map(|r| r.kind)
                .find(|k| {
                    matches!(
                        k,
                        ConflictKind::RemovedHere
                            | ConflictKind::RemovedThere
                            | ConflictKind::Uncombinable
                    )
                })
                .unwrap_or(first.kind);
            let removed = matches!(kind, ConflictKind::RemovedHere | ConflictKind::RemovedThere);
            let mut choices = vec![Resolution::Ours, Resolution::Theirs];
            if first.unit.both && !removed {
                choices.push(Resolution::Both);
            }
            let chosen = resolutions
                .get(&key)
                .copied()
                .filter(|r| choices.contains(r))
                .unwrap_or(Resolution::Ours);
            Some(Conflict {
                what: names.what(&first.unit.path, &key),
                differences: list
                    .iter()
                    .map(|r| names.difference(&first.unit.path, r))
                    .collect(),
                key,
                kind,
                choices,
                chosen,
            })
        })
        .collect()
}

/// Copies of theirs beside ours where both were chosen (clips, notes,
/// markers, sections), with ids of their own.
fn keep_both(project: &mut Project, theirs: &Project, conflicts: &[Conflict]) {
    for c in conflicts.iter().filter(|c| c.chosen == Resolution::Both) {
        let parts: Vec<&str> = c.key.split('/').collect();
        let id = |s: &str| s.parse::<u64>().ok();
        match parts.as_slice() {
            ["clips", clip] => {
                let Some(theirs_clip) = id(clip).and_then(|c| theirs.clips.get(&c.into())) else {
                    continue;
                };
                if project.track(theirs_clip.track).is_none() {
                    continue;
                }
                let mut copy = theirs_clip.clone();
                copy.id = project.ids.allocate();
                if let ClipContent::Midi(m) = &mut copy.content {
                    for n in &mut m.notes {
                        let old = n.id;
                        n.id = project.ids.allocate();
                        for e in &mut m.expressions {
                            if e.note == old {
                                e.note = n.id;
                            }
                        }
                    }
                }
                project.clips.insert(copy.id, copy);
            }
            ["clips", clip, "content", "notes", note] => {
                let (Some(clip), Some(note)) = (id(clip), id(note)) else {
                    continue;
                };
                let theirs_note = theirs
                    .clips
                    .get(&clip.into())
                    .and_then(Clip::as_midi)
                    .and_then(|m| m.note(note.into()))
                    .cloned();
                let new_id = project.ids.allocate();
                if let (Some(mut n), Some(m)) = (
                    theirs_note,
                    project
                        .clips
                        .get_mut(&clip.into())
                        .and_then(Clip::as_midi_mut),
                ) {
                    n.id = new_id;
                    m.notes.push(n);
                    m.notes.sort_by_key(|n| n.start);
                }
            }
            ["markers", marker] => {
                if let Some(mut m) = id(marker)
                    .and_then(|i| theirs.markers.iter().find(|m| m.id.raw() == i))
                    .cloned()
                {
                    m.id = project.ids.allocate();
                    project.markers.push(m);
                    project.markers.sort_by_key(|m| m.position);
                }
            }
            ["sections", section] => {
                if let Some(mut s) = id(section)
                    .and_then(|i| theirs.sections.iter().find(|s| s.id.raw() == i))
                    .cloned()
                {
                    s.id = project.ids.allocate();
                    project.sections.push(s);
                    project.sections.sort_by_key(|s| s.start);
                }
            }
            _ => {}
        }
    }
}

/// Each track lists the clips on it: ours' order kept, the others after.
fn rebuild_clip_lists(project: &mut Project, ours: &Project) {
    let launcher: BTreeSet<_> = project.launcher_clips().into_iter().collect();
    let mut on: BTreeMap<
        faderframe_core::TrackId,
        Vec<(faderframe_timeline::MusicalTime, faderframe_core::ClipId)>,
    > = BTreeMap::new();
    for c in project.clips.values().filter(|c| !launcher.contains(&c.id)) {
        on.entry(c.track).or_default().push((c.start, c.id));
    }
    for t in &mut project.tracks {
        let mut here = on.remove(&t.id).unwrap_or_default();
        here.sort();
        let was: Vec<_> = ours
            .track(t.id)
            .map(|t| t.clips.clone())
            .unwrap_or_default();
        let mut list: Vec<_> = was
            .into_iter()
            .filter(|c| here.iter().any(|(_, h)| h == c))
            .collect();
        for (_, c) in here {
            if !list.contains(&c) {
                list.push(c);
            }
        }
        t.clips = list;
    }
}

#[cfg(test)]
mod tests;
