//! Rearranging the song in time: copying a span with everything in it,
//! cutting a span out (later material moves up), and opening a span
//! (later material moves back) to insert a copy. Clips are split at the
//! span's edges, automation keeps its values on both sides of a cut,
//! tempo and time-signature changes, markers, sections and the loop and
//! punch ranges move along. Section moves, copies and deletes
//! ([`move_section`], [`copy_section`], [`delete_section`]) are built from
//! these.
//!
//! The functions work on a project directly; the session runs them on a
//! copy and applies the result as one [`Command::SetArrangement`]
//! (one undo step).
//!
//! [`Command::SetArrangement`]: crate::Command::SetArrangement

use crate::{ChordEvent, Clip, ClipContent, KeyChange, Marker, MusicalRange, Project, Section};
use faderframe_automation::{AutomationPoint, AutomationSet};
use faderframe_core::{AutomationLaneId, ClipId, IdAllocator, SectionId, TrackId};
use faderframe_timeline::{
    MeterChange, MusicalTime, TempoCurve, TempoMap, TempoPoint, TimeSignature, TimeSignatureMap,
    Timeline,
};
use std::collections::BTreeMap;

/// The parts of a project that time edits change.
#[derive(Clone, Debug, PartialEq)]
pub struct Arrangement {
    pub timeline: Timeline,
    pub clips: BTreeMap<ClipId, Clip>,
    /// Each track's clips and automation.
    pub tracks: Vec<(TrackId, Vec<ClipId>, AutomationSet)>,
    pub markers: Vec<Marker>,
    pub sections: Vec<Section>,
    pub keys: Vec<KeyChange>,
    pub chords: Vec<ChordEvent>,
    pub loop_range: Option<MusicalRange>,
    pub loop_enabled: bool,
    pub punch_range: Option<MusicalRange>,
    pub ids: IdAllocator,
}

impl Arrangement {
    pub fn of(p: &Project) -> Self {
        Self {
            timeline: p.timeline.clone(),
            clips: p.clips.clone(),
            tracks: p
                .tracks
                .iter()
                .map(|t| (t.id, t.clips.clone(), t.automation.clone()))
                .collect(),
            markers: p.markers.clone(),
            sections: p.sections.clone(),
            keys: p.keys.clone(),
            chords: p.chords.clone(),
            loop_range: p.loop_range,
            loop_enabled: p.loop_enabled,
            punch_range: p.punch_range,
            ids: p.ids.clone(),
        }
    }

    /// Put this arrangement into `p`; returns the one it replaces.
    pub(crate) fn swap_into(self, p: &mut Project) -> Arrangement {
        let mut tracks = Vec::new();
        for (track, clips, set) in self.tracks {
            if let Some(t) = p.tracks.iter_mut().find(|t| t.id == track) {
                tracks.push((
                    track,
                    std::mem::replace(&mut t.clips, clips),
                    std::mem::replace(&mut t.automation, set),
                ));
            }
        }
        Arrangement {
            timeline: std::mem::replace(&mut p.timeline, self.timeline),
            clips: std::mem::replace(&mut p.clips, self.clips),
            tracks,
            markers: std::mem::replace(&mut p.markers, self.markers),
            sections: std::mem::replace(&mut p.sections, self.sections),
            keys: std::mem::replace(&mut p.keys, self.keys),
            chords: std::mem::replace(&mut p.chords, self.chords),
            loop_range: std::mem::replace(&mut p.loop_range, self.loop_range),
            loop_enabled: std::mem::replace(&mut p.loop_enabled, self.loop_enabled),
            punch_range: std::mem::replace(&mut p.punch_range, self.punch_range),
            ids: std::mem::replace(&mut p.ids, self.ids),
        }
    }
}

/// Everything in a span of time, positioned relative to its start.
#[derive(Clone, Debug)]
pub struct TimeSlice {
    pub length: MusicalTime,
    /// Whole bars (`None`: the span does not start and end on bar lines;
    /// time-signature changes then stay where they are).
    pub bars: Option<i32>,
    clips: Vec<Clip>,
    /// Points of each lane, with the lane's values at both edges.
    automation: Vec<(TrackId, AutomationLaneId, Vec<AutomationPoint>)>,
    /// The tempo from the start on (first point at 0).
    tempo: Vec<TempoPoint>,
    /// Signatures by bar from the start (first at 0).
    meter: Vec<(i32, TimeSignature)>,
    markers: Vec<Marker>,
    sections: Vec<Section>,
    /// Key changes from the start (the key in effect there at 0).
    keys: Vec<KeyChange>,
    /// Chords, cut to the span.
    chords: Vec<ChordEvent>,
}

const TICK: MusicalTime = MusicalTime(1);

/// Audio clip ends come from sample lengths and can land a hair past a
/// bar line; closer than this (about a millisecond) counts as on it.
const SLACK: MusicalTime = MusicalTime(faderframe_timeline::TICKS_PER_QUARTER / 500);

/// Whole bars between `a` and `b` if both are bar lines.
fn whole_bars(meter: &TimeSignatureMap, a: MusicalTime, b: MusicalTime) -> Option<(i32, i32)> {
    let (ba, bb) = (meter.bar_at(a), meter.bar_at(b));
    (meter.bar_start(ba) == a && meter.bar_start(bb) == b).then_some((ba, bb))
}

/// The tempo curve after the point at or before `at` (constant past the
/// last point).
fn segment_curve(tempo: &TempoMap, at: MusicalTime) -> TempoCurve {
    let pts = tempo.points();
    let i = pts.partition_point(|p| p.position <= at).saturating_sub(1);
    if i + 1 < pts.len() {
        pts[i].curve
    } else {
        TempoCurve::Constant
    }
}

fn rebuild_tempo(points: Vec<TempoPoint>) -> TempoMap {
    let mut sorted = points;
    sorted.sort_by_key(|p| p.position);
    // Later points at the same position win; steps to the same tempo go.
    let mut kept: Vec<TempoPoint> = Vec::new();
    for p in sorted {
        if kept.last().is_some_and(|l| l.position == p.position) {
            kept.pop();
        }
        let repeats = kept.last().is_some_and(|l| {
            l.bpm == p.bpm && l.curve == TempoCurve::Constant && p.curve == TempoCurve::Constant
        });
        if !repeats {
            kept.push(p);
        }
    }
    let first = kept.first().map_or(120.0, |p| p.bpm);
    let mut map = TempoMap::new(first);
    for p in kept {
        map.set_point(p);
    }
    map
}

fn rebuild_meter(changes: Vec<(i32, TimeSignature)>) -> TimeSignatureMap {
    let mut sorted = changes;
    sorted.sort_by_key(|c| c.0);
    let first = sorted.first().map_or(TimeSignature::FOUR_FOUR, |c| c.1);
    let mut map = TimeSignatureMap::new(first);
    for (bar, signature) in sorted {
        map.set_change(MeterChange { bar, signature });
    }
    map
}

/// Drop changes that repeat the signature before them.
fn simplify_meter(changes: Vec<(i32, TimeSignature)>) -> Vec<(i32, TimeSignature)> {
    let mut sorted = changes;
    sorted.sort_by_key(|c| c.0);
    let mut out: Vec<(i32, TimeSignature)> = Vec::new();
    for c in sorted {
        if out.last().is_some_and(|l| l.0 == c.0) {
            out.pop();
        }
        if out.last().is_none_or(|l| l.1 != c.1) {
            out.push(c);
        }
    }
    out
}

/// Make every track's clip list match the clips that name it (clips were
/// added, split and removed by id).
fn sync_track_clips(p: &mut Project) {
    for t in &mut p.tracks {
        t.clips
            .retain(|id| p.clips.get(id).is_some_and(|c| c.track == t.id));
        for c in p.clips.values() {
            if c.track == t.id && !t.clips.contains(&c.id) {
                t.clips.push(c.id);
            }
        }
    }
}

/// Copy `[a, b)` with everything in it.
pub fn copy_span(p: &Project, a: MusicalTime, b: MusicalTime) -> TimeSlice {
    let tl = &p.timeline;
    let len = b - a;
    let clips = p
        .clips
        .values()
        .filter(|c| c.start + SLACK < b && c.end(tl, p.sample_rate) > a + SLACK)
        .filter_map(|c| c.slice(a, b, c.id, tl, p.sample_rate))
        .map(|mut c| {
            c.start -= a;
            c
        })
        .collect();
    let mut automation = Vec::new();
    for t in &p.tracks {
        for lane in &t.automation.lanes {
            let curve = &lane.curve;
            if curve.is_empty() {
                continue;
            }
            let mut pts: Vec<AutomationPoint> = Vec::new();
            if let Some(v) = curve.value_at(a)
                && !curve.points().iter().any(|q| q.time == a)
            {
                pts.push(AutomationPoint {
                    time: MusicalTime::ZERO,
                    value: v,
                    shape: Default::default(),
                });
            }
            pts.extend(
                curve
                    .points()
                    .iter()
                    .filter(|q| q.time >= a && q.time < b)
                    .map(|q| AutomationPoint {
                        time: q.time - a,
                        ..*q
                    }),
            );
            if let Some(v) = curve.value_at(b - TICK) {
                pts.push(AutomationPoint {
                    time: len,
                    value: v,
                    shape: Default::default(),
                });
            }
            automation.push((t.id, lane.id, pts));
        }
    }
    let mut tempo = vec![TempoPoint {
        position: MusicalTime::ZERO,
        bpm: tl.tempo.bpm_at(a),
        curve: segment_curve(&tl.tempo, a),
    }];
    tempo.extend(
        tl.tempo
            .points()
            .iter()
            .filter(|q| q.position > a && q.position < b)
            .map(|q| TempoPoint {
                position: q.position - a,
                ..*q
            }),
    );
    // A ramp towards a point outside the span holds instead.
    if let Some(last) = tempo.last_mut()
        && !tl.tempo.points().iter().any(|q| q.position == b)
        && tl
            .tempo
            .points()
            .iter()
            .all(|q| q.position <= a + last.position)
    {
        last.curve = TempoCurve::Constant;
    }
    let bars = whole_bars(&tl.meter, a, b);
    let meter = match bars {
        Some((ba, bb)) => {
            let mut m = vec![(0, tl.meter.signature_of_bar(ba))];
            m.extend(
                tl.meter
                    .changes()
                    .iter()
                    .filter(|c| c.bar > ba && c.bar < bb)
                    .map(|c| (c.bar - ba, c.signature)),
            );
            m
        }
        None => Vec::new(),
    };
    TimeSlice {
        length: len,
        bars: bars.map(|(ba, bb)| bb - ba),
        clips,
        automation,
        tempo,
        meter,
        markers: p
            .markers
            .iter()
            .filter(|m| m.position >= a && m.position < b)
            .map(|m| Marker {
                position: m.position - a,
                ..m.clone()
            })
            .collect(),
        sections: p
            .sections
            .iter()
            .filter(|s| s.start >= a && s.end <= b)
            .map(|s| Section {
                start: s.start - a,
                end: s.end - a,
                ..s.clone()
            })
            .collect(),
        keys: p
            .key_at(a)
            .map(|key| KeyChange { at: a, key })
            .into_iter()
            .chain(p.keys.iter().copied().filter(|k| k.at > a && k.at < b))
            .map(|k| KeyChange { at: k.at - a, ..k })
            .collect(),
        chords: p
            .chords
            .iter()
            .filter(|c| c.end > a && c.start < b)
            .map(|c| ChordEvent {
                start: c.start.max(a) - a,
                end: c.end.min(b) - a,
                chord: c.chord,
            })
            .collect(),
    }
}

/// A range after cutting `[a, b)` out (`None`: nothing left).
fn range_after_cut(
    start: MusicalTime,
    end: MusicalTime,
    a: MusicalTime,
    b: MusicalTime,
) -> Option<(MusicalTime, MusicalTime)> {
    let len = b - a;
    let map = |t: MusicalTime| {
        if t <= a {
            t
        } else if t >= b {
            t - len
        } else {
            a
        }
    };
    let (s, e) = (map(start), map(end));
    (e > s).then_some((s, e))
}

/// A range after opening `len` at `at` (ranges across `at` grow; with
/// `grow_at_start` also one that starts there, as the loop does when a
/// section goes in at its start).
fn range_after_insert(
    start: MusicalTime,
    end: MusicalTime,
    at: MusicalTime,
    len: MusicalTime,
    grow_at_start: bool,
) -> (MusicalTime, MusicalTime) {
    let s = if start > at || (start == at && !grow_at_start) {
        start + len
    } else {
        start
    };
    let e = if end > at { end + len } else { end };
    (s, e)
}

/// Cut `[a, b)` out: what is inside goes, what follows moves up.
pub fn remove_span(p: &mut Project, a: MusicalTime, b: MusicalTime) {
    if b <= a {
        return;
    }
    let len = b - a;
    let rate = p.sample_rate;
    // Clips (cut with the old timeline).
    let old: Vec<Clip> = p.clips.values().cloned().collect();
    for c in old {
        let end = c.end(&p.timeline, rate);
        if end <= a + SLACK {
            continue;
        }
        if c.start + SLACK >= b {
            if let Some(clip) = p.clips.get_mut(&c.id) {
                clip.start -= len;
            }
            continue;
        }
        p.clips.remove(&c.id);
        if c.start + SLACK < a
            && let Some(left) = c.slice(c.start, a, c.id, &p.timeline, rate)
        {
            p.clips.insert(left.id, left);
        }
        if end > b + SLACK {
            let id: ClipId = p.ids.allocate();
            if let Some(mut right) = c.slice(b, end, id, &p.timeline, rate) {
                right.start = a;
                p.clips.insert(id, right);
            }
        }
    }
    // Automation: values on both sides stay.
    for t in &mut p.tracks {
        for lane in &mut t.automation.lanes {
            let curve = &lane.curve;
            if curve.is_empty() {
                continue;
            }
            let before = curve.value_at(a - TICK).filter(|_| a > MusicalTime::ZERO);
            let after = curve.value_at(b);
            let has_b = curve.points().iter().any(|q| q.time == b);
            let mut pts: Vec<AutomationPoint> = curve
                .points()
                .iter()
                .filter(|q| q.time < a)
                .copied()
                .collect();
            if let Some(v) = before {
                pts.push(AutomationPoint {
                    time: a,
                    value: v,
                    shape: Default::default(),
                });
            }
            if let Some(v) = after
                && !has_b
            {
                pts.push(AutomationPoint {
                    time: a,
                    value: v,
                    shape: Default::default(),
                });
            }
            pts.extend(
                curve
                    .points()
                    .iter()
                    .filter(|q| q.time >= b)
                    .map(|q| AutomationPoint {
                        time: q.time - len,
                        ..*q
                    }),
            );
            lane.curve = faderframe_automation::AutomationCurve::from_points(pts);
        }
    }
    // Tempo: the tempo at `b` continues from `a`.
    let tempo = &p.timeline.tempo;
    let mut points: Vec<TempoPoint> = tempo
        .points()
        .iter()
        .filter(|q| q.position < a)
        .copied()
        .collect();
    points.push(TempoPoint {
        position: a,
        bpm: tempo.bpm_at(b),
        curve: segment_curve(tempo, b),
    });
    points.extend(
        tempo
            .points()
            .iter()
            .filter(|q| q.position > b)
            .map(|q| TempoPoint {
                position: q.position - len,
                ..*q
            }),
    );
    let new_tempo = rebuild_tempo(points);
    // Meter: whole bars only.
    let new_meter = whole_bars(&p.timeline.meter, a, b).map(|(ba, bb)| {
        let meter = &p.timeline.meter;
        let n = bb - ba;
        let mut changes: Vec<(i32, TimeSignature)> = meter
            .changes()
            .iter()
            .filter(|c| c.bar < ba)
            .map(|c| (c.bar, c.signature))
            .collect();
        changes.push((ba, meter.signature_of_bar(bb)));
        changes.extend(
            meter
                .changes()
                .iter()
                .filter(|c| c.bar > bb)
                .map(|c| (c.bar - n, c.signature)),
        );
        rebuild_meter(simplify_meter(changes))
    });
    p.timeline.tempo = new_tempo;
    if let Some(m) = new_meter {
        p.timeline.meter = m;
    }
    // Markers, sections, loop and punch.
    p.markers.retain(|m| m.position < a || m.position >= b);
    for m in &mut p.markers {
        if m.position >= b {
            m.position -= len;
        }
    }
    // The key reached inside the cut carries on from where it was.
    let last_inside = p.keys.iter().rev().find(|k| k.at >= a && k.at < b).copied();
    p.keys.retain(|k| k.at < a || k.at >= b);
    for k in &mut p.keys {
        if k.at >= b {
            k.at -= len;
        }
    }
    if let Some(k) = last_inside
        && !p.keys.iter().any(|x| x.at == a)
    {
        p.keys.push(KeyChange { at: a, key: k.key });
    }
    crate::harmony::normalize_keys(&mut p.keys);
    p.chords
        .retain_mut(|c| match range_after_cut(c.start, c.end, a, b) {
            Some((start, end)) => {
                c.start = start;
                c.end = end;
                true
            }
            None => false,
        });
    crate::harmony::normalize_chords(&mut p.chords);
    p.sections
        .retain_mut(|s| match range_after_cut(s.start, s.end, a, b) {
            Some((start, end)) => {
                s.start = start;
                s.end = end;
                true
            }
            None => false,
        });
    p.loop_range = p
        .loop_range
        .and_then(|r| range_after_cut(r.start, r.end, a, b))
        .map(|(start, end)| MusicalRange { start, end });
    if p.loop_range.is_none() {
        p.loop_enabled = false;
    }
    p.punch_range = p
        .punch_range
        .and_then(|r| range_after_cut(r.start, r.end, a, b))
        .map(|(start, end)| MusicalRange { start, end });
    sync_track_clips(p);
}

/// Open the slice's length at `at` and put the slice there. With
/// `keep_ids` markers and sections keep their ids (a move: the originals
/// are gone); clips always get new ones.
pub fn insert_span(p: &mut Project, at: MusicalTime, slice: &TimeSlice, keep_ids: bool) {
    let len = slice.length;
    if len <= MusicalTime::ZERO {
        return;
    }
    let rate = p.sample_rate;
    // Clips: split across `at`, move what follows.
    let old: Vec<Clip> = p.clips.values().cloned().collect();
    for c in old {
        let end = c.end(&p.timeline, rate);
        if c.start + SLACK >= at {
            if let Some(clip) = p.clips.get_mut(&c.id) {
                clip.start += len;
            }
        } else if end > at + SLACK {
            let id: ClipId = p.ids.allocate();
            if let Ok((left, mut right)) = c.split_at(at, id, &p.timeline, rate) {
                right.start = at + len;
                p.clips.insert(left.id, left);
                p.clips.insert(id, right);
            }
        }
    }
    for c in &slice.clips {
        let id: ClipId = p.ids.allocate();
        let fits = p.tracks.iter().any(|t| {
            t.id == c.track
                && match c.content {
                    ClipContent::Audio(_) | ClipContent::Takes(_) => {
                        t.kind == crate::TrackKind::Audio
                    }
                    ClipContent::Midi(_) => matches!(
                        t.kind,
                        crate::TrackKind::Instrument | crate::TrackKind::Midi
                    ),
                }
        });
        if fits {
            p.clips.insert(
                id,
                Clip {
                    id,
                    start: at + c.start,
                    ..c.clone()
                },
            );
        }
    }
    // Automation.
    for t in &mut p.tracks {
        for lane in &mut t.automation.lanes {
            let curve = &lane.curve;
            let copied = slice
                .automation
                .iter()
                .find(|(tr, id, _)| *tr == t.id && *id == lane.id)
                .map(|(_, _, pts)| pts);
            if curve.is_empty() && copied.is_none() {
                continue;
            }
            let left = curve.value_at(at - TICK).filter(|_| at > MusicalTime::ZERO);
            let right = curve.value_at(at);
            let has_at = curve.points().iter().any(|q| q.time == at);
            let point = |time, value| AutomationPoint {
                time,
                value,
                shape: Default::default(),
            };
            let mut pts: Vec<AutomationPoint> = curve
                .points()
                .iter()
                .filter(|q| q.time < at)
                .copied()
                .collect();
            if let Some(v) = left {
                pts.push(point(at, v));
            }
            match copied {
                Some(c) => pts.extend(c.iter().map(|q| AutomationPoint {
                    time: at + q.time,
                    ..*q
                })),
                None => {
                    if let Some(v) = right {
                        pts.push(point(at, v));
                        pts.push(point(at + len, v));
                    }
                }
            }
            if let Some(v) = right
                && !has_at
            {
                pts.push(point(at + len, v));
            }
            pts.extend(
                curve
                    .points()
                    .iter()
                    .filter(|q| q.time >= at)
                    .map(|q| AutomationPoint {
                        time: q.time + len,
                        ..*q
                    }),
            );
            lane.curve = faderframe_automation::AutomationCurve::from_points(pts);
        }
    }
    // Tempo.
    let tempo = &p.timeline.tempo;
    let has_at = tempo.points().iter().any(|q| q.position == at);
    let mut points: Vec<TempoPoint> = tempo
        .points()
        .iter()
        .filter(|q| q.position < at)
        .copied()
        .collect();
    points.extend(slice.tempo.iter().map(|q| TempoPoint {
        position: at + q.position,
        ..*q
    }));
    if !has_at {
        points.push(TempoPoint {
            position: at + len,
            bpm: tempo.bpm_at(at),
            curve: segment_curve(tempo, at),
        });
    }
    points.extend(
        tempo
            .points()
            .iter()
            .filter(|q| q.position >= at)
            .map(|q| TempoPoint {
                position: q.position + len,
                ..*q
            }),
    );
    let new_tempo = rebuild_tempo(points);
    // Meter (whole bars at a bar line only).
    let meter = &p.timeline.meter;
    let bar = meter.bar_at(at);
    let new_meter = match slice.bars {
        Some(n) if meter.bar_start(bar) == at => {
            let has_bar = meter.changes().iter().any(|c| c.bar == bar);
            let mut changes: Vec<(i32, TimeSignature)> = meter
                .changes()
                .iter()
                .filter(|c| c.bar < bar)
                .map(|c| (c.bar, c.signature))
                .collect();
            changes.extend(slice.meter.iter().map(|(b, s)| (bar + b, *s)));
            if !has_bar {
                changes.push((bar + n, meter.signature_of_bar(bar)));
            }
            changes.extend(
                meter
                    .changes()
                    .iter()
                    .filter(|c| c.bar >= bar)
                    .map(|c| (c.bar + n, c.signature)),
            );
            Some(rebuild_meter(simplify_meter(changes)))
        }
        _ => None,
    };
    p.timeline.tempo = new_tempo;
    if let Some(m) = new_meter {
        p.timeline.meter = m;
    }
    // Markers, sections, loop and punch.
    for m in &mut p.markers {
        if m.position >= at {
            m.position += len;
        }
    }
    for m in &slice.markers {
        let id = if keep_ids { m.id } else { p.ids.allocate() };
        p.markers.push(Marker {
            id,
            position: at + m.position,
            name: m.name.clone(),
        });
    }
    p.markers.sort_by_key(|m| m.position);
    for s in &mut p.sections {
        (s.start, s.end) = range_after_insert(s.start, s.end, at, len, false);
    }
    for s in &slice.sections {
        let id: SectionId = if keep_ids { s.id } else { p.ids.allocate() };
        p.sections.push(Section {
            id,
            start: at + s.start,
            end: at + s.end,
            ..s.clone()
        });
    }
    p.sections.sort_by_key(|s| s.start);
    // The slice's keys, and the key from before going on after it.
    let before = p.key_at(at);
    for k in &mut p.keys {
        if k.at >= at {
            k.at += len;
        }
    }
    p.keys.extend(slice.keys.iter().map(|k| KeyChange {
        at: at + k.at,
        ..*k
    }));
    if let Some(key) = before
        && !slice.keys.is_empty()
        && !p.keys.iter().any(|k| k.at == at + len)
    {
        p.keys.push(KeyChange { at: at + len, key });
    }
    crate::harmony::normalize_keys(&mut p.keys);
    // Chords across the insertion point split round the new span.
    let mut chords = Vec::with_capacity(p.chords.len() + slice.chords.len() + 1);
    for c in &p.chords {
        if c.start >= at {
            chords.push(ChordEvent {
                start: c.start + len,
                end: c.end + len,
                chord: c.chord,
            });
        } else if c.end > at {
            chords.push(ChordEvent { end: at, ..*c });
            chords.push(ChordEvent {
                start: at + len,
                end: c.end + len,
                chord: c.chord,
            });
        } else {
            chords.push(*c);
        }
    }
    chords.extend(slice.chords.iter().map(|c| ChordEvent {
        start: at + c.start,
        end: at + c.end,
        chord: c.chord,
    }));
    crate::harmony::normalize_chords(&mut chords);
    p.chords = chords;
    let shift = |r: MusicalRange| {
        let (start, end) = range_after_insert(r.start, r.end, at, len, true);
        MusicalRange { start, end }
    };
    p.loop_range = p.loop_range.map(shift);
    p.punch_range = p.punch_range.map(shift);
    sync_track_clips(p);
}

fn section(p: &Project, id: SectionId) -> Option<Section> {
    p.sections.iter().find(|s| s.id == id).cloned()
}

/// Move a section with its content so that it starts at `to` (a position
/// in the current arrangement; the material in between closes up and
/// opens again). Returns false when nothing moves.
pub fn move_section(p: &mut Project, id: SectionId, to: MusicalTime) -> bool {
    let Some(s) = section(p, id) else {
        return false;
    };
    let len = s.end - s.start;
    if to >= s.start && to <= s.end {
        return false;
    }
    let slice = copy_span(p, s.start, s.end);
    remove_span(p, s.start, s.end);
    let to = if to > s.end { to - len } else { to };
    insert_span(p, to, &slice, true);
    true
}

/// Insert a copy of a section with its content at `to`. Returns the copy.
pub fn copy_section(p: &mut Project, id: SectionId, to: MusicalTime) -> Option<SectionId> {
    let s = section(p, id)?;
    let slice = copy_span(p, s.start, s.end);
    insert_span(p, to, &slice, false);
    p.sections
        .iter()
        .find(|c| c.start == to && c.id != id && c.name == s.name)
        .map(|c| c.id)
}

/// Remove a section and its content; what follows moves up.
pub fn delete_section(p: &mut Project, id: SectionId) -> bool {
    let Some(s) = section(p, id) else {
        return false;
    };
    remove_span(p, s.start, s.end);
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{MidiClip, MidiNote, Track, TrackColor, TrackKind};
    use faderframe_automation::{AutomationCurve, AutomationLane, AutomationTarget};

    const GREY: TrackColor = TrackColor {
        r: 128,
        g: 128,
        b: 128,
    };

    fn bars(n: i64) -> MusicalTime {
        MusicalTime::from_quarters(4.0 * n as f64)
    }

    fn note(p: &mut Project, key: u8, start: MusicalTime) -> MidiNote {
        MidiNote {
            id: p.ids.allocate(),
            start,
            length: MusicalTime::QUARTER,
            key,
            velocity: 100,
            channel: 0,
            muted: false,
        }
    }

    fn midi(
        id: ClipId,
        track: TrackId,
        start: MusicalTime,
        len: MusicalTime,
        notes: Vec<MidiNote>,
    ) -> Clip {
        Clip {
            id,
            track,
            name: String::new(),
            color: None,
            start,
            muted: false,
            content: ClipContent::Midi(MidiClip {
                length: len,
                notes,
                ..Default::default()
            }),
        }
    }

    /// Four one-bar sections A B C D on a MIDI track (a note per bar),
    /// volume automation 0, -10, -20, -30 dB per bar, a tempo change in C.
    fn song() -> (Project, TrackId, Vec<SectionId>) {
        let mut p = Project::new("Song", 48_000);
        let track: TrackId = p.ids.allocate();
        let mut t = Track::new(track, TrackKind::Instrument, "Keys", GREY);
        let lane: AutomationLaneId = p.ids.allocate();
        let pts = (0..4)
            .map(|i| AutomationPoint {
                time: bars(i),
                value: -10.0 * i as f64,
                shape: Default::default(),
            })
            .collect();
        t.automation.lanes.push(AutomationLane {
            id: lane,
            target: AutomationTarget::TrackVolume,
            curve: AutomationCurve::from_points(pts),
            mode: Default::default(),
            visible: true,
        });
        p.tracks.push(t);
        let mut ids = Vec::new();
        for (i, name) in ["A", "B", "C", "D"].iter().enumerate() {
            let clip: ClipId = p.ids.allocate();
            let n = note(&mut p, 60 + i as u8, MusicalTime::ZERO);
            p.clips
                .insert(clip, midi(clip, track, bars(i as i64), bars(1), vec![n]));
            p.tracks.last_mut().unwrap().clips.push(clip);
            let id: SectionId = p.ids.allocate();
            p.sections.push(Section {
                id,
                name: (*name).into(),
                start: bars(i as i64),
                end: bars(i as i64 + 1),
                color: GREY,
            });
            ids.push(id);
        }
        p.timeline.tempo.set_point(TempoPoint {
            position: bars(2),
            bpm: 90.0,
            curve: TempoCurve::Constant,
        });
        p.timeline.tempo.set_point(TempoPoint {
            position: bars(3),
            bpm: 120.0,
            curve: TempoCurve::Constant,
        });
        (p, track, ids)
    }

    fn order(p: &Project) -> String {
        // Every track lists exactly the clips that name it.
        for t in &p.tracks {
            let mut listed = t.clips.clone();
            listed.sort();
            let mut owned: Vec<ClipId> = p
                .clips
                .values()
                .filter(|c| c.track == t.id)
                .map(|c| c.id)
                .collect();
            owned.sort();
            assert_eq!(listed, owned, "{}'s clip list", t.name);
        }
        p.sections.iter().map(|s| s.name.as_str()).collect()
    }

    fn note_at(p: &Project, at: MusicalTime) -> Option<u8> {
        p.clips.values().find_map(|c| match &c.content {
            ClipContent::Midi(m) if c.start == at => m.notes.first().map(|n| n.key),
            _ => None,
        })
    }

    fn volume_at(p: &Project, t: TrackId, at: MusicalTime) -> Option<f64> {
        let track = p.tracks.iter().find(|x| x.id == t)?;
        track.automation.lanes[0].curve.value_at(at)
    }

    #[test]
    fn moving_a_section_carries_its_content() {
        let (mut p, t, ids) = song();
        // C before A.
        assert!(move_section(&mut p, ids[2], MusicalTime::ZERO));
        assert_eq!(order(&p), "CABD");
        assert_eq!(p.sections[0].id, ids[2], "a move keeps the id");
        let keys: Vec<_> = (0..4).map(|i| note_at(&p, bars(i)).unwrap()).collect();
        assert_eq!(keys, vec![62, 60, 61, 63]);
        assert_eq!(p.clips.len(), 4);
        // C's tempo and automation went with it; D's stayed.
        assert_eq!(p.timeline.tempo.bpm_at(bars(0)), 90.0);
        assert_eq!(p.timeline.tempo.bpm_at(bars(1)), 120.0);
        assert_eq!(p.timeline.tempo.bpm_at(bars(3)), 120.0);
        assert_eq!(volume_at(&p, t, bars(0)), Some(-20.0));
        assert_eq!(volume_at(&p, t, bars(1)), Some(0.0));
        assert_eq!(volume_at(&p, t, bars(3)), Some(-30.0));
        // Later: A after D.
        assert!(move_section(&mut p, ids[0], bars(4)));
        assert_eq!(order(&p), "CBDA");
        assert_eq!(note_at(&p, bars(3)), Some(60));
        // Onto itself: nothing.
        assert!(!move_section(&mut p, ids[0], bars(3)));
    }

    #[test]
    fn copying_and_deleting_sections() {
        let (mut p, t, ids) = song();
        let copy = copy_section(&mut p, ids[1], bars(4)).unwrap();
        assert_ne!(copy, ids[1]);
        assert_eq!(order(&p), "ABCDB");
        assert_eq!(note_at(&p, bars(4)), Some(61));
        assert_eq!(volume_at(&p, t, bars(4)), Some(-10.0));
        assert_eq!(p.clips.len(), 5);
        // Duplicate right after itself.
        copy_section(&mut p, ids[0], bars(1)).unwrap();
        assert_eq!(order(&p), "AABCDB");
        assert_eq!(note_at(&p, bars(1)), Some(60));
        assert_eq!(note_at(&p, bars(2)), Some(61));
        // Delete C: D moves up, the tempo after it is D's.
        assert!(delete_section(&mut p, ids[2]));
        assert_eq!(order(&p), "AABDB");
        assert_eq!(note_at(&p, bars(3)), Some(63));
        assert_eq!(p.timeline.tempo.bpm_at(bars(3)), 120.0);
        assert!(p.timeline.tempo.points().iter().all(|q| q.bpm != 90.0));
    }

    #[test]
    fn clips_across_the_edges_are_split() {
        let (mut p, t, ids) = song();
        // One long audio-free MIDI clip over B and C.
        let long: ClipId = p.ids.allocate();
        let notes = vec![
            note(&mut p, 70, MusicalTime::ZERO),
            note(&mut p, 72, bars(1)),
        ];
        p.clips.insert(long, midi(long, t, bars(1), bars(2), notes));
        p.tracks
            .iter_mut()
            .find(|x| x.id == t)
            .unwrap()
            .clips
            .push(long);
        assert!(delete_section(&mut p, ids[1]));
        let at_b: Vec<u8> = p
            .clips
            .values()
            .filter(|c| c.start == bars(1))
            .filter_map(|c| match &c.content {
                ClipContent::Midi(m) => m.notes.first().map(|n| n.key),
                _ => None,
            })
            .collect();
        assert!(
            at_b.contains(&72),
            "the part after the cut moved up: {at_b:?}"
        );
        assert!(!at_b.contains(&70));
    }

    #[test]
    fn meter_changes_follow_whole_bars() {
        let (mut p, _, ids) = song();
        p.timeline.meter.set_change(MeterChange {
            bar: 1,
            signature: TimeSignature::new(3, 4).unwrap(),
        });
        p.timeline.meter.set_change(MeterChange {
            bar: 2,
            signature: TimeSignature::FOUR_FOUR,
        });
        // B is now a 3/4 bar: [4, 7) quarters.
        let b = p.sections.iter_mut().find(|s| s.id == ids[1]).unwrap();
        b.end = MusicalTime::from_quarters(7.0);
        let c = p.sections.iter_mut().find(|s| s.id == ids[2]).unwrap();
        c.start = MusicalTime::from_quarters(7.0);
        let slice = copy_span(&p, bars(1), MusicalTime::from_quarters(7.0));
        assert_eq!(slice.bars, Some(1));
        insert_span(&mut p, MusicalTime::ZERO, &slice, false);
        let sigs: Vec<(i32, u8)> = p
            .timeline
            .meter
            .changes()
            .iter()
            .map(|c| (c.bar, c.signature.numerator))
            .collect();
        assert_eq!(sigs, vec![(0, 3), (1, 4), (2, 3), (3, 4)]);
    }

    #[test]
    fn the_loop_and_tempo_stay_tidy() {
        let (mut p, _, ids) = song();
        p.timeline.tempo = TempoMap::new(120.0);
        p.loop_range = Some(MusicalRange {
            start: MusicalTime::ZERO,
            end: bars(4),
        });
        p.loop_enabled = true;
        assert!(move_section(&mut p, ids[3], MusicalTime::ZERO));
        assert_eq!(
            p.loop_range,
            Some(MusicalRange {
                start: MusicalTime::ZERO,
                end: bars(4)
            }),
            "the loop still covers the song"
        );
        assert_eq!(p.timeline.tempo.points().len(), 1, "no repeated tempo");
    }

    #[test]
    fn arrangement_swaps_back() {
        let (mut p, _, ids) = song();
        let before = Arrangement::of(&p);
        let mut q = p.clone();
        move_section(&mut q, ids[3], MusicalTime::ZERO);
        let after = Arrangement::of(&q);
        let undo = after.swap_into(&mut p);
        assert_eq!(order(&p), "DABC");
        undo.swap_into(&mut p);
        assert_eq!(order(&p), "ABCD");
        assert_eq!(p.clips.len(), before.clips.len());
    }
}

#[cfg(test)]
mod harmony_tests {
    use super::*;
    use crate::harmony::{Chord, Key, Scale};

    fn bars(n: i64) -> MusicalTime {
        MusicalTime::from_quarters(4.0 * n as f64)
    }

    /// Four one-bar sections A B C D with a chord each (C Am F G), in C
    /// major turning to A minor at C.
    fn song() -> (Project, Vec<SectionId>) {
        let mut p = Project::new("Song", 48_000);
        let mut ids = Vec::new();
        for (i, (name, chord)) in [("A", "C"), ("B", "Am"), ("C", "F"), ("D", "G")]
            .iter()
            .enumerate()
        {
            let id: SectionId = p.ids.allocate();
            p.sections.push(Section {
                id,
                name: (*name).into(),
                start: bars(i as i64),
                end: bars(i as i64 + 1),
                color: crate::TrackColor::palette(i),
            });
            p.chords.push(ChordEvent {
                start: bars(i as i64),
                end: bars(i as i64 + 1),
                chord: Chord::parse(chord).unwrap_or(Chord::new(0, crate::harmony::Quality::Major)),
            });
            ids.push(id);
        }
        p.keys = vec![
            KeyChange {
                at: bars(0),
                key: Key::new(0, Scale::Major),
            },
            KeyChange {
                at: bars(2),
                key: Key::new(9, Scale::Minor),
            },
        ];
        (p, ids)
    }

    fn chords(p: &Project) -> Vec<String> {
        p.chords.iter().map(|c| c.chord.name(false)).collect()
    }

    #[test]
    fn moving_a_section_takes_its_chords_and_key() {
        let (mut p, ids) = song();
        assert!(move_section(&mut p, ids[0], bars(4)));
        assert_eq!(chords(&p), ["Am", "F", "G", "C"]);
        assert_eq!(p.chords[3].start, bars(3));
        // B stays in C major, C and D in A minor, the moved A back in C
        // major, and A minor again after it.
        let keys: Vec<(MusicalTime, String)> =
            p.keys.iter().map(|k| (k.at, k.key.name())).collect();
        assert_eq!(
            keys,
            vec![
                (bars(0), "C Major".to_string()),
                (bars(1), "A Minor".to_string()),
                (bars(3), "C Major".to_string()),
                (bars(4), "A Minor".to_string()),
            ]
        );
    }

    #[test]
    fn deleting_and_copying_sections_keep_the_harmony_in_place() {
        let (mut p, ids) = song();
        // Deleting C (where A minor starts): D is still in A minor.
        assert!(delete_section(&mut p, ids[2]));
        assert_eq!(chords(&p), ["C", "Am", "G"]);
        assert_eq!(p.key_at(bars(2)).map(|k| k.name()), Some("A Minor".into()));
        assert_eq!(p.key_at(bars(1)).map(|k| k.name()), Some("C Major".into()));
        // Copying B to the end.
        let (mut p, ids) = song();
        assert!(copy_section(&mut p, ids[1], bars(4)).is_some());
        assert_eq!(chords(&p), ["C", "Am", "F", "G", "Am"]);
        assert_eq!(p.key_at(bars(4)).map(|k| k.name()), Some("C Major".into()));
        // A chord across an insertion point is split round it.
        let (mut p, ids) = song();
        p.chords[0].end = bars(2);
        p.chords.remove(1);
        let slice = copy_span(&p, bars(3), bars(4));
        insert_span(&mut p, bars(1), &slice, false);
        assert_eq!(chords(&p), ["C", "G", "C", "F", "G"]);
        let _ = ids;
    }
}
