//! Ripple edits: what lies at or after a cut moves by an offset, on chosen
//! tracks or across the whole project.
//!
//! A ripple is a list of [`RippleShift`]s — at or after `cut` everything
//! moves by `delta`; a negative delta takes `[cut + delta, cut)` out — in
//! the coordinates before any of them (several are applied from the
//! latest cut back, so none moves another's cut; they must not overlap).
//! Clips and automation are separate entities moved by the same shifts:
//! each track's automation through [`AutomationCurve::ripple`] (the curve
//! split and its value duplicated at the cut), its clips here:
//!
//! * [`ClipRipple::Moved`] — a track the edit is on: clips at or after a
//!   cut move (into a span taken out: onto its start), nothing is cut, and
//!   the clips the edit places itself are left to it.
//! * [`ClipRipple::Cut`] — a track kept in sync with the edit: its time is
//!   opened or cut out like the curve's — a clip across the cut is split
//!   there, what lies in a span taken out goes.
//!
//! [`global_commands`] moves the timeline's own parts for a ripple of every
//! track ([`RippleGlobals`] says which): markers (into a span taken out:
//! onto its start), sections and the loop and punch ranges (a range across
//! an opened cut grows, one inside a span taken out goes), the Play Start
//! Marker, tempo and time-signature changes (a ramp across a cut keeps its
//! slope on both sides; signatures move by whole bars only), key changes,
//! chords and lyrics. The picture (video) stays: it is what sound is
//! edited against.
//!
//! [`AutomationCurve::ripple`]: faderframe_automation::AutomationCurve::ripple

use crate::arrange::{rebuild_meter, rebuild_tempo, segment_curve, simplify_meter, whole_bars};
use crate::{ChordEvent, Clip, Command, KeyChange, MusicalRange, Project};
use faderframe_core::{ClipId, IdAllocator, TrackId};
use faderframe_timeline::{MusicalTime, TempoCurve, TempoMap, TempoPoint, TimeSignature, Timeline};
use std::collections::BTreeSet;

/// One ripple: at or after `cut` everything moves by `delta`; a negative
/// delta takes `[cut + delta, cut)` out.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct RippleShift {
    pub cut: MusicalTime,
    pub delta: MusicalTime,
}

impl RippleShift {
    /// A shift at `cut` by `delta`, never taking time out before zero (it
    /// is cut short there).
    pub fn new(cut: MusicalTime, delta: MusicalTime) -> Self {
        let cut = cut.max(MusicalTime::ZERO);
        Self {
            cut,
            delta: MusicalTime(delta.ticks().max(-cut.ticks())),
        }
    }

    /// Opens `len` at `at`.
    pub fn open(at: MusicalTime, len: MusicalTime) -> Self {
        Self::new(at, len.max(MusicalTime::ZERO))
    }

    /// Takes `[a, b)` out.
    pub fn remove(a: MusicalTime, b: MusicalTime) -> Self {
        Self::new(b, MusicalTime(a.min(b).ticks() - b.ticks()))
    }

    pub fn is_empty(&self) -> bool {
        self.delta == MusicalTime::ZERO
    }

    /// The span it takes out (`None`: it opens time).
    pub fn removed(&self) -> Option<(MusicalTime, MusicalTime)> {
        (self.delta < MusicalTime::ZERO).then(|| (self.cut + self.delta, self.cut))
    }

    /// Where a position goes: at or after the cut it moves; inside a span
    /// taken out it lands on the span's start.
    pub fn map(&self, t: MusicalTime) -> MusicalTime {
        if t >= self.cut {
            t + self.delta
        } else if self.delta < MusicalTime::ZERO && t > self.cut + self.delta {
            self.cut + self.delta
        } else {
            t
        }
    }

    /// Where the end of a range goes: as [`Self::map`], except that an end
    /// on the cut of an opening stays (the range lies before it).
    pub fn map_end(&self, t: MusicalTime) -> MusicalTime {
        if self.delta > MusicalTime::ZERO && t == self.cut {
            t
        } else {
            self.map(t)
        }
    }

    /// A range `[start, end)` after the shift (`None`: taken out).
    pub fn map_range(
        &self,
        start: MusicalTime,
        end: MusicalTime,
    ) -> Option<(MusicalTime, MusicalTime)> {
        let (s, e) = (self.map(start), self.map_end(end));
        (e > s).then_some((s, e))
    }

    /// `shifts` in the order they apply: the latest cut first, empty ones
    /// left out.
    pub fn ordered(shifts: &[RippleShift]) -> Vec<RippleShift> {
        let mut out: Vec<RippleShift> = shifts.iter().copied().filter(|s| !s.is_empty()).collect();
        out.sort_by_key(|s| (std::cmp::Reverse(s.cut), s.delta));
        out.dedup();
        out
    }

    /// Where a position goes after all of `shifts` (as [`Self::ordered`]).
    pub fn map_all(shifts: &[RippleShift], t: MusicalTime) -> MusicalTime {
        Self::ordered(shifts).iter().fold(t, |t, s| s.map(t))
    }

    /// Where a range goes after all of `shifts` (`None`: taken out).
    pub fn map_range_all(
        shifts: &[RippleShift],
        start: MusicalTime,
        end: MusicalTime,
    ) -> Option<(MusicalTime, MusicalTime)> {
        Self::ordered(shifts)
            .iter()
            .try_fold((start, end), |(s, e), sh| sh.map_range(s, e))
    }
}

/// How a track's clips take a ripple (see the module docs).
#[derive(Clone, Copy, Debug)]
pub enum ClipRipple<'a> {
    /// A track the edit is on: clips move, none is cut; `skip` are the
    /// clips the edit places itself.
    Moved { skip: &'a BTreeSet<ClipId> },
    /// A track kept in sync: its time is opened or cut out.
    Cut,
}

/// Samples between two positions.
fn frames(tl: &Timeline, rate: u32, a: MusicalTime, b: MusicalTime) -> i64 {
    let r = f64::from(rate.max(1));
    tl.to_samples(b, r) - tl.to_samples(a, r)
}

/// Does a cut at `at` fall inside `c` (for audio a whole sample from both
/// edges), so that splitting there leaves two clips?
fn cuts_inside(c: &Clip, at: MusicalTime, tl: &Timeline, rate: u32) -> bool {
    let end = c.end(tl, rate);
    at > c.start
        && at < end
        && (!c.content.is_audio()
            || (frames(tl, rate, c.start, at) > 0 && frames(tl, rate, at, end) > 0))
}

/// Is `c` at or after `at` (an audio clip less than a sample before it
/// counts as on it)?
fn starts_from(c: &Clip, at: MusicalTime, tl: &Timeline, rate: u32) -> bool {
    c.start >= at || (c.content.is_audio() && frames(tl, rate, c.start, at) <= 0)
}

/// Does `c` end at or before `at` (an audio clip less than a sample past
/// it counts as ending on it)?
fn ends_by(c: &Clip, at: MusicalTime, tl: &Timeline, rate: u32) -> bool {
    let end = c.end(tl, rate);
    end <= at || (c.content.is_audio() && frames(tl, rate, at, end) <= 0)
}

/// The commands rippling `track`'s clips by `shifts` (see the module docs;
/// new clips' ids from `ids`). Positions are worked out with the project's
/// timeline as it is: a batch changing the tempo too must change it after
/// these.
pub fn clip_commands(
    p: &Project,
    ids: &mut IdAllocator,
    track: TrackId,
    shifts: &[RippleShift],
    how: ClipRipple<'_>,
) -> Vec<Command> {
    let shifts = RippleShift::ordered(shifts);
    let (tl, rate) = (&p.timeline, p.sample_rate);
    let mut clips: Vec<Clip> = p.clips_of(track).into_iter().cloned().collect();
    let mut cmds = Vec::new();
    if let ClipRipple::Moved { skip } = how {
        for c in clips.iter().filter(|c| !skip.contains(&c.id)) {
            let start = shifts.iter().fold(c.start, |t, s| s.map(t));
            if start != c.start {
                cmds.push(Command::MoveClip {
                    clip: c.id,
                    track,
                    start,
                });
            }
        }
        return cmds;
    }
    for s in &shifts {
        let mut next: Vec<Clip> = Vec::with_capacity(clips.len() + 2);
        let moved = |c: Clip, start: MusicalTime, cmds: &mut Vec<Command>, next: &mut Vec<Clip>| {
            cmds.push(Command::MoveClip {
                clip: c.id,
                track,
                start,
            });
            next.push(Clip { start, ..c });
        };
        let split = |c: &Clip, at: MusicalTime, cmds: &mut Vec<Command>, ids: &mut IdAllocator| {
            let id: ClipId = ids.allocate();
            cmds.push(Command::SplitClip {
                clip: c.id,
                at,
                new_clip: id,
            });
            c.split_at(at, id, tl, rate).ok()
        };
        match s.removed() {
            // Open at the cut: what is from it on moves, a clip across it
            // splits there.
            None => {
                for c in clips {
                    if starts_from(&c, s.cut, tl, rate) {
                        let start = c.start + s.delta;
                        moved(c, start, &mut cmds, &mut next);
                    } else if cuts_inside(&c, s.cut, tl, rate) {
                        match split(&c, s.cut, &mut cmds, ids) {
                            Some((left, right)) => {
                                next.push(left);
                                moved(right, s.cut + s.delta, &mut cmds, &mut next);
                            }
                            None => next.push(c),
                        }
                    } else {
                        next.push(c);
                    }
                }
            }
            // Take `[a, b)` out: what lies inside goes, clips across an
            // edge are cut there, what follows moves up.
            Some((a, b)) => {
                for c in clips {
                    if starts_from(&c, b, tl, rate) {
                        let start = (c.start + s.delta).max(a);
                        moved(c, start, &mut cmds, &mut next);
                        continue;
                    }
                    if ends_by(&c, a, tl, rate) {
                        next.push(c);
                        continue;
                    }
                    match (cuts_inside(&c, a, tl, rate), cuts_inside(&c, b, tl, rate)) {
                        // Inside: it goes.
                        (false, false) => cmds.push(Command::RemoveClip { clip: c.id }),
                        // Across the whole span: both ends stay, joined.
                        (true, true) => {
                            let Some((left, mid)) = split(&c, a, &mut cmds, ids) else {
                                next.push(c);
                                continue;
                            };
                            match split(&mid, b, &mut cmds, ids) {
                                Some((_, right)) => {
                                    cmds.push(Command::RemoveClip { clip: mid.id });
                                    next.push(left);
                                    moved(right, a, &mut cmds, &mut next);
                                }
                                None => {
                                    next.push(left);
                                    next.push(mid);
                                }
                            }
                        }
                        // Its end is inside.
                        (true, false) => match split(&c, a, &mut cmds, ids) {
                            Some((left, tail)) => {
                                cmds.push(Command::RemoveClip { clip: tail.id });
                                next.push(left);
                            }
                            None => next.push(c),
                        },
                        // Its start is inside.
                        (false, true) => match split(&c, b, &mut cmds, ids) {
                            Some((_, right)) => {
                                cmds.push(Command::RemoveClip { clip: c.id });
                                moved(right, a, &mut cmds, &mut next);
                            }
                            None => next.push(c),
                        },
                    }
                }
            }
        }
        clips = next;
    }
    cmds
}

/// The timeline's parts a ripple of every track moves along.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct RippleGlobals {
    /// Markers, sections, the loop and punch ranges and the Play Start
    /// Marker.
    pub markers: bool,
    /// Tempo and time-signature changes.
    pub tempo: bool,
    /// Key changes, chords and lyrics.
    pub harmony: bool,
}

impl RippleGlobals {
    pub const ALL: RippleGlobals = RippleGlobals {
        markers: true,
        tempo: true,
        harmony: true,
    };

    pub const NONE: RippleGlobals = RippleGlobals {
        markers: false,
        tempo: false,
        harmony: false,
    };

    pub fn any(&self) -> bool {
        self.markers || self.tempo || self.harmony
    }
}

/// The commands moving the timeline's parts `what` names by `shifts` (see
/// the module docs); a changed tempo map or metre comes last, after
/// whatever the caller puts before (clip commands work with the old one).
pub fn global_commands(p: &Project, shifts: &[RippleShift], what: RippleGlobals) -> Vec<Command> {
    let shifts = RippleShift::ordered(shifts);
    let mut cmds = Vec::new();
    if shifts.is_empty() {
        return cmds;
    }
    if what.markers {
        for m in &p.markers {
            let position = RippleShift::map_all(&shifts, m.position);
            if position != m.position {
                cmds.push(Command::UpdateMarker {
                    marker: crate::Marker {
                        position,
                        ..m.clone()
                    },
                });
            }
        }
        for s in &p.sections {
            match RippleShift::map_range_all(&shifts, s.start, s.end) {
                Some((start, end)) if (start, end) != (s.start, s.end) => {
                    cmds.push(Command::UpdateSection {
                        section: crate::Section {
                            start,
                            end,
                            ..s.clone()
                        },
                    });
                }
                Some(_) => {}
                None => cmds.push(Command::RemoveSection { section: s.id }),
            }
        }
        let range = |r: Option<MusicalRange>| {
            r.and_then(|r| RippleShift::map_range_all(&shifts, r.start, r.end))
                .map(|(start, end)| MusicalRange { start, end })
        };
        let looped = range(p.loop_range);
        if looped != p.loop_range {
            cmds.push(Command::SetLoop {
                range: looped,
                enabled: p.loop_enabled && looped.is_some(),
            });
        }
        let punched = range(p.punch_range);
        if punched != p.punch_range {
            cmds.push(Command::SetPunch {
                range: punched,
                enabled: p.punch_enabled && punched.is_some(),
            });
        }
        if let Some(m) = p.play_start {
            let at = RippleShift::map_all(&shifts, m.at);
            if at != m.at {
                cmds.push(Command::SetPlayStart {
                    marker: Some(crate::PlayStart { at, ..m }),
                });
            }
        }
    }
    if what.harmony {
        let keys = shifts.iter().fold(p.keys.clone(), rippled_keys);
        if keys != p.keys {
            cmds.push(Command::SetKeys { keys });
        }
        let chords = shifts.iter().fold(p.chords.clone(), rippled_chords);
        if chords != p.chords {
            cmds.push(Command::SetChords { chords });
        }
        let lyrics = shifts.iter().fold(p.lyrics.clone(), rippled_lyrics);
        if lyrics != p.lyrics {
            cmds.push(Command::SetLyrics { lyrics });
        }
    }
    if what.tempo {
        let mut timeline = p.timeline.clone();
        for s in &shifts {
            if let Some(meter) = rippled_meter(&timeline.meter, s) {
                timeline.meter = meter;
            }
            timeline.tempo = rippled_tempo(&timeline.tempo, s);
        }
        if timeline != p.timeline {
            cmds.push(Command::SetTimeline {
                timeline: Box::new(timeline),
            });
        }
    }
    cmds
}

/// Key changes after a shift: those from the cut on move; of those inside
/// a span taken out the last one carries on from where it was.
fn rippled_keys(mut keys: Vec<KeyChange>, s: &RippleShift) -> Vec<KeyChange> {
    let carried = s.removed().and_then(|(a, b)| {
        keys.iter()
            .rev()
            .find(|k| k.at >= a && k.at < b)
            .map(|k| KeyChange { at: a, key: k.key })
    });
    if let Some((a, b)) = s.removed() {
        keys.retain(|k| k.at < a || k.at >= b);
    }
    for k in &mut keys {
        if k.at >= s.cut {
            k.at += s.delta;
        }
    }
    if let Some(k) = carried
        && !keys.iter().any(|x| x.at == k.at)
    {
        keys.push(k);
    }
    crate::harmony::normalize_keys(&mut keys);
    keys
}

/// Chords after a shift: ranges (one across an opened cut lasts over the
/// gap; inside a span taken out it goes).
fn rippled_chords(chords: Vec<ChordEvent>, s: &RippleShift) -> Vec<ChordEvent> {
    let mut out: Vec<ChordEvent> = chords
        .into_iter()
        .filter_map(|c| {
            s.map_range(c.start, c.end)
                .map(|(start, end)| ChordEvent { start, end, ..c })
        })
        .collect();
    crate::harmony::normalize_chords(&mut out);
    out
}

/// Lyrics after a shift: lines from the cut on move, one across an opened
/// cut lasts over the gap; lines starting in a span taken out go with it.
fn rippled_lyrics(
    lines: Vec<crate::lyrics::LyricLine>,
    s: &RippleShift,
) -> Vec<crate::lyrics::LyricLine> {
    let mut out: Vec<crate::lyrics::LyricLine> = lines
        .into_iter()
        .filter(|l| s.removed().is_none_or(|(a, b)| l.start < a || l.start >= b))
        .map(|l| crate::lyrics::LyricLine {
            start: s.map(l.start),
            end: s.map_end(l.end).max(s.map(l.start)),
            ..l
        })
        .collect();
    crate::lyrics::normalize(&mut out);
    out
}

/// The tempo map after a shift. Opening: the tempo at the cut holds over
/// the gap (a point on the cut is kept there and moves too; a ramp across
/// it goes on after the gap from where it was). Taking out: the tempo
/// reached at the span's start stays up to it and the tempo at its end
/// goes on from there (a ramp before keeps its slope up to a tick short of
/// the seam: two tempo points cannot share a place).
pub fn rippled_tempo(tempo: &TempoMap, s: &RippleShift) -> TempoMap {
    let pts = tempo.points();
    let mut out: Vec<TempoPoint> = Vec::with_capacity(pts.len() + 2);
    match s.removed() {
        None => {
            let (at, len) = (s.cut, s.delta);
            let i = pts.partition_point(|q| q.position < at);
            if i == pts.len() {
                return tempo.clone();
            }
            out.extend_from_slice(&pts[..i]);
            let held = TempoPoint {
                position: at,
                bpm: tempo.bpm_at(at),
                curve: TempoCurve::Constant,
            };
            if pts[i].position == at {
                // On a point: its tempo holds over the gap, it moves.
                if i > 0 {
                    out.push(held);
                }
            } else if segment_curve(tempo, at) == TempoCurve::Linear {
                // Across a ramp: it stops at the cut and goes on after.
                out.push(held);
                out.push(TempoPoint {
                    position: at + len,
                    curve: TempoCurve::Linear,
                    ..held
                });
            }
            out.extend(pts[i..].iter().map(|q| TempoPoint {
                position: q.position + len,
                ..*q
            }));
        }
        Some((a, b)) => {
            if !pts.iter().any(|q| q.position >= a) {
                return tempo.clone();
            }
            out.extend(pts.iter().filter(|q| q.position < a).copied());
            let (reached, goes_on) = (tempo.bpm_at(a), tempo.bpm_at(b));
            let tick = MusicalTime(1);
            let ramp_before = a > tick
                && segment_curve(tempo, a - tick) == TempoCurve::Linear
                && out.last().is_some_and(|q| q.position < a - tick);
            if ramp_before && reached != goes_on {
                out.push(TempoPoint {
                    position: a - tick,
                    bpm: tempo.bpm_at(a - tick),
                    curve: TempoCurve::Constant,
                });
            }
            out.push(TempoPoint {
                position: a,
                bpm: goes_on,
                curve: segment_curve(tempo, b),
            });
            out.extend(pts.iter().filter(|q| q.position > b).map(|q| TempoPoint {
                position: q.position + s.delta,
                ..*q
            }));
        }
    }
    rebuild_tempo(out)
}

/// The metre after a shift of whole bars from a bar line (`None`: it
/// stays — a shift of anything else would cut a bar in two).
fn rippled_meter(
    meter: &faderframe_timeline::TimeSignatureMap,
    s: &RippleShift,
) -> Option<faderframe_timeline::TimeSignatureMap> {
    let changes: Vec<(i32, TimeSignature)> = meter
        .changes()
        .iter()
        .map(|c| (c.bar, c.signature))
        .collect();
    match s.removed() {
        Some((a, b)) => {
            let (ba, bb) = whole_bars(meter, a, b)?;
            let n = bb - ba;
            let mut out: Vec<(i32, TimeSignature)> =
                changes.iter().copied().filter(|c| c.0 < ba).collect();
            out.push((ba, meter.signature_of_bar(bb)));
            out.extend(changes.iter().filter(|c| c.0 > bb).map(|c| (c.0 - n, c.1)));
            Some(rebuild_meter(simplify_meter(out)))
        }
        None => {
            let bar = meter.bar_at(s.cut);
            if meter.bar_start(bar) != s.cut {
                return None;
            }
            // The gap takes the signature before a change on the cut.
            let on_cut = changes.iter().any(|c| c.0 == bar);
            let gap = if on_cut && bar > 0 {
                meter.signature_of_bar(bar - 1)
            } else {
                meter.signature_of_bar(bar)
            };
            let bar_len = gap.bar_length().ticks();
            if bar_len <= 0 || s.delta.ticks() % bar_len != 0 {
                return None;
            }
            let n = i32::try_from(s.delta.ticks() / bar_len).ok()?;
            let mut out: Vec<(i32, TimeSignature)> =
                changes.iter().copied().filter(|c| c.0 < bar).collect();
            if !on_cut {
                out.push((bar + n, meter.signature_of_bar(bar)));
            } else if bar == 0 {
                out.push((0, gap));
            }
            out.extend(
                changes
                    .iter()
                    .filter(|c| c.0 >= bar)
                    .map(|c| (c.0 + n, c.1)),
            );
            Some(rebuild_meter(simplify_meter(out)))
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;
    use crate::{AudioClip, ClipContent, Section, Track, TrackColor, TrackKind};
    use faderframe_core::{AudioSourceId, MarkerId, SectionId};
    use faderframe_timeline::MeterChange;

    fn q(n: f64) -> MusicalTime {
        MusicalTime::from_quarters(n)
    }

    const GREY: TrackColor = TrackColor {
        r: 128,
        g: 128,
        b: 128,
    };

    /// A project at 120 bpm (24 000 frames a quarter at 48 kHz) with one
    /// audio track holding clips at the given quarters.
    fn project(spans: &[(f64, f64)]) -> (Project, TrackId, Vec<ClipId>) {
        let mut p = Project::new("R", 48_000);
        p.timeline.tempo = TempoMap::new(120.0);
        let track: TrackId = p.ids.allocate();
        p.tracks
            .push(Track::new(track, TrackKind::Audio, "A", GREY));
        let source: AudioSourceId = p.ids.allocate();
        let mut ids = Vec::new();
        for &(a, b) in spans {
            let id: ClipId = p.ids.allocate();
            let frames = ((b - a) * 24_000.0) as i64;
            p.clips.insert(
                id,
                Clip {
                    id,
                    track,
                    name: String::new(),
                    color: None,
                    start: q(a),
                    muted: false,
                    content: ClipContent::Audio(AudioClip {
                        source,
                        source_offset: 0,
                        length: frames,
                        gain_db: 0.0,
                        fades: Default::default(),
                        stretch: Default::default(),
                        reversed: false,
                        warp: None,
                        pitch: None,
                        effects: None,
                        spectral: None,
                    }),
                },
            );
            if let Some(t) = p.track_mut(track) {
                t.clips.push(id);
            }
            ids.push(id);
        }
        (p, track, ids)
    }

    fn apply(p: &mut Project, cmds: Vec<Command>) {
        for c in cmds {
            c.apply(p).unwrap();
        }
    }

    fn spans(p: &Project, track: TrackId) -> Vec<(f64, f64)> {
        p.clips_of(track)
            .iter()
            .map(|c| {
                (
                    c.start.quarters(),
                    c.end(&p.timeline, p.sample_rate).quarters(),
                )
            })
            .collect()
    }

    #[test]
    fn shifts_map_positions_and_ranges() {
        let open = RippleShift::open(q(4.0), q(2.0));
        assert_eq!(open.map(q(3.0)), q(3.0));
        assert_eq!(open.map(q(4.0)), q(6.0));
        assert_eq!(
            open.map_range(q(2.0), q(4.0)),
            Some((q(2.0), q(4.0))),
            "ends on the cut"
        );
        assert_eq!(
            open.map_range(q(2.0), q(5.0)),
            Some((q(2.0), q(7.0))),
            "across: grows"
        );
        let out = RippleShift::remove(q(4.0), q(6.0));
        assert_eq!(out.removed(), Some((q(4.0), q(6.0))));
        assert_eq!(out.map(q(5.0)), q(4.0), "inside: onto the start");
        assert_eq!(out.map(q(7.0)), q(5.0));
        assert_eq!(out.map_range(q(4.5), q(5.5)), None, "inside: gone");
        assert_eq!(out.map_range(q(3.0), q(7.0)), Some((q(3.0), q(5.0))));
        // Never before zero.
        let s = RippleShift::new(q(1.0), MusicalTime::from_quarters(-3.0));
        assert_eq!(s.removed(), Some((q(0.0), q(1.0))));
        // Several: the latest first, so neither moves the other's cut.
        let both = [
            RippleShift::remove(q(1.0), q(2.0)),
            RippleShift::remove(q(5.0), q(6.0)),
        ];
        assert_eq!(RippleShift::map_all(&both, q(8.0)), q(6.0));
        assert_eq!(RippleShift::map_all(&both, q(3.0)), q(2.0));
    }

    /// A track kept in sync is cut: a clip across the opened cut splits,
    /// a span taken out goes from the clips across it.
    #[test]
    fn a_track_kept_in_sync_is_cut() {
        let (p, track, _) = project(&[(0.0, 8.0), (10.0, 12.0)]);
        let mut q_ = p.clone();
        let mut ids = q_.ids.clone();
        let cmds = clip_commands(
            &q_,
            &mut ids,
            track,
            &[RippleShift::open(q(4.0), q(2.0))],
            ClipRipple::Cut,
        );
        q_.ids = ids;
        apply(&mut q_, cmds);
        assert_eq!(
            spans(&q_, track),
            vec![(0.0, 4.0), (6.0, 10.0), (12.0, 14.0)]
        );
        let mut r = p.clone();
        let mut ids = r.ids.clone();
        let cmds = clip_commands(
            &r,
            &mut ids,
            track,
            &[RippleShift::remove(q(6.0), q(11.0))],
            ClipRipple::Cut,
        );
        r.ids = ids;
        apply(&mut r, cmds);
        assert_eq!(spans(&r, track), vec![(0.0, 6.0), (6.0, 7.0)]);
    }

    /// The track the edit is on: clips move, none is cut.
    #[test]
    fn the_edited_track_moves_and_cuts_nothing() {
        let (p, track, ids) = project(&[(0.0, 8.0), (10.0, 12.0), (14.0, 15.0)]);
        let mut r = p.clone();
        let skip: BTreeSet<ClipId> = [ids[1]].into();
        let mut alloc = r.ids.clone();
        let cmds = clip_commands(
            &r,
            &mut alloc,
            track,
            &[RippleShift::remove(q(10.0), q(12.0))],
            ClipRipple::Moved { skip: &skip },
        );
        apply(&mut r, cmds);
        assert_eq!(
            spans(&r, track),
            vec![(0.0, 8.0), (10.0, 12.0), (12.0, 13.0)]
        );
    }

    #[test]
    fn markers_sections_and_ranges_move_along() {
        let mut p = Project::new("R", 48_000);
        let m: MarkerId = p.ids.allocate();
        p.markers.push(crate::Marker {
            id: m,
            position: q(9.0),
            name: "M".into(),
        });
        let (s1, s2): (SectionId, SectionId) = (p.ids.allocate(), p.ids.allocate());
        p.sections = vec![
            Section {
                id: s1,
                name: "Verse".into(),
                start: q(0.0),
                end: q(8.0),
                color: GREY,
            },
            Section {
                id: s2,
                name: "Fill".into(),
                start: q(8.0),
                end: q(10.0),
                color: GREY,
            },
        ];
        p.loop_range = Some(MusicalRange {
            start: q(4.0),
            end: q(12.0),
        });
        p.loop_enabled = true;
        let cmds = global_commands(
            &p,
            &[RippleShift::remove(q(8.0), q(10.0))],
            RippleGlobals::ALL,
        );
        apply(&mut p, cmds);
        assert_eq!(
            p.markers[0].position,
            q(8.0),
            "inside the span: onto its start"
        );
        assert_eq!(p.sections.len(), 1, "the section inside goes");
        assert_eq!(p.loop_range.map(|r| r.end), Some(q(10.0)));
        assert!(p.loop_enabled);
        let cmds = global_commands(&p, &[RippleShift::open(q(2.0), q(4.0))], RippleGlobals::ALL);
        apply(&mut p, cmds);
        assert_eq!(p.sections[0].end, q(12.0), "across the cut: grows");
        assert_eq!(p.markers[0].position, q(12.0));
        // Nothing asked for, nothing moves.
        assert!(
            global_commands(
                &p,
                &[RippleShift::open(q(0.0), q(4.0))],
                RippleGlobals::NONE
            )
            .is_empty()
        );
    }

    /// A ramp across a cut keeps its slope on both sides; whole bars move
    /// the signatures.
    #[test]
    fn tempo_ramps_and_signatures_ripple() {
        let mut tempo = TempoMap::new(100.0);
        tempo.set_point(TempoPoint {
            position: q(0.0),
            bpm: 100.0,
            curve: TempoCurve::Linear,
        });
        tempo.set_point(TempoPoint {
            position: q(8.0),
            bpm: 140.0,
            curve: TempoCurve::Constant,
        });
        let opened = rippled_tempo(&tempo, &RippleShift::open(q(4.0), q(4.0)));
        assert!((opened.bpm_at(q(2.0)) - 110.0).abs() < 1e-9);
        assert!(
            (opened.bpm_at(q(6.0)) - 120.0).abs() < 1e-9,
            "the gap holds"
        );
        assert!(
            (opened.bpm_at(q(10.0)) - 130.0).abs() < 1e-9,
            "the ramp goes on"
        );
        assert!((opened.bpm_at(q(12.0)) - 140.0).abs() < 1e-9);
        let closed = rippled_tempo(&tempo, &RippleShift::remove(q(2.0), q(6.0)));
        assert!(
            (closed.bpm_at(q(1.0)) - 105.0).abs() < 1e-6,
            "the ramp before keeps its slope"
        );
        assert!(
            (closed.bpm_at(q(2.0)) - 130.0).abs() < 1e-9,
            "goes on from the span's end"
        );
        assert!((closed.bpm_at(q(3.0)) - 135.0).abs() < 1e-9);
        assert!((closed.bpm_at(q(4.0)) - 140.0).abs() < 1e-9);
        // Signatures: a 3/4 bar from bar 2 on; two 4/4 bars opened at bar 1.
        let mut meter = faderframe_timeline::TimeSignatureMap::new(TimeSignature::FOUR_FOUR);
        let three = TimeSignature::new(3, 4).unwrap();
        meter.set_change(MeterChange {
            bar: 2,
            signature: three,
        });
        let m = rippled_meter(&meter, &RippleShift::open(q(4.0), q(8.0))).unwrap();
        assert_eq!(m.signature_of_bar(3), TimeSignature::FOUR_FOUR);
        assert_eq!(m.signature_of_bar(4), three);
        assert!(
            rippled_meter(&meter, &RippleShift::open(q(4.0), q(2.0))).is_none(),
            "half a bar"
        );
        let m = rippled_meter(&meter, &RippleShift::remove(q(0.0), q(4.0))).unwrap();
        assert_eq!(m.signature_of_bar(1), three);
    }

    #[test]
    fn keys_chords_and_lyrics_ripple() {
        use crate::harmony::{Chord, Key, Scale};
        let mut p = Project::new("R", 48_000);
        p.keys = vec![
            KeyChange {
                at: q(0.0),
                key: Key::new(0, Scale::Major),
            },
            KeyChange {
                at: q(5.0),
                key: Key::new(7, Scale::Major),
            },
        ];
        let chord = Chord::parse("Am").unwrap();
        p.chords = vec![ChordEvent {
            start: q(2.0),
            end: q(6.0),
            chord,
        }];
        p.lyrics = vec![crate::lyrics::LyricLine {
            start: q(8.0),
            end: q(9.0),
            text: "la".into(),
        }];
        let cmds = global_commands(
            &p,
            &[RippleShift::remove(q(4.0), q(6.0))],
            RippleGlobals::ALL,
        );
        apply(&mut p, cmds);
        assert_eq!(p.keys.len(), 2);
        assert_eq!(p.keys[1].at, q(4.0), "the key reached inside carries on");
        assert_eq!(p.chords[0].end, q(4.0));
        assert_eq!(p.lyrics[0].start, q(6.0));
    }
}
