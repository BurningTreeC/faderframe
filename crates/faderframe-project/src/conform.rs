//! Conforming the timeline to a new picture cut: the old timeline in
//! pieces, each placed where the new cut has its picture
//! ([`Piece`], absolute time: tempo and meter stay as they are). Clips are
//! cut at the pieces' edges (a short fade where a cut lands inside one),
//! automation goes with its pieces (the values at the edges kept),
//! markers, sections and lyric lines move with the piece they are in;
//! what no piece holds is gone. The result is an [`Arrangement`] to put in
//! place with `Command::SetArrangement` (one undo step).

use crate::arrange::Arrangement;
use crate::clip::ClipContent;
use crate::project::Project;
use faderframe_automation::{AutomationCurve, AutomationPoint, CurveShape};
use faderframe_timeline::MusicalTime;

/// The old timeline's `[from, to)` placed at `at` (samples).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Piece {
    pub from: i64,
    pub to: i64,
    pub at: i64,
}

impl Piece {
    fn holds(&self, s: i64) -> bool {
        s >= self.from && s < self.to
    }

    fn place(&self, s: i64) -> i64 {
        self.at + (s - self.from)
    }
}

/// Fades where a piece's edge cuts a clip (samples at 48 kHz: 5 ms).
fn edit_fade(rate: u32) -> i64 {
    (rate as i64 / 200).max(1)
}

/// `p`'s timeline content rearranged by `pieces`.
pub fn conformed(p: &Project, pieces: &[Piece]) -> Arrangement {
    let rate = p.sample_rate;
    let tl = &p.timeline;
    let r = rate as f64;
    let samples = |t: MusicalTime| tl.to_samples(t, r);
    let musical = |s: i64| tl.to_musical(s.max(0), r);
    let mut a = Arrangement::of(p);
    let mut ids = p.ids.clone();
    let fade = edit_fade(rate);
    // Clips, track by track.
    let launcher = p.launcher_clips();
    for (track, clips, automation) in &mut a.tracks {
        let mut placed = Vec::new();
        for id in std::mem::take(clips) {
            if launcher.contains(&id) {
                continue;
            }
            let Some(c) = p.clips.get(&id) else { continue };
            a.clips.remove(&id);
            let (s, e) = (samples(c.start), samples(c.end(tl, rate)));
            let hits: Vec<&Piece> = pieces.iter().filter(|q| q.from < e && q.to > s).collect();
            let whole = hits.len() == 1 && hits[0].from <= s && hits[0].to >= e;
            for q in hits {
                let (cs, ce) = (s.max(q.from), e.min(q.to));
                let new_id = if whole { id } else { ids.allocate() };
                let Some(mut part) = c.slice(musical(cs), musical(ce), new_id, tl, rate) else {
                    continue;
                };
                part.start = musical(q.place(cs));
                // A cut inside the clip gets a short fade.
                if let ClipContent::Audio(au) = &mut part.content {
                    if cs > s {
                        au.fades.fade_in = au.fades.fade_in.max(fade);
                    }
                    if ce < e {
                        au.fades.fade_out = au.fades.fade_out.max(fade);
                    }
                }
                placed.push(part.id);
                a.clips.insert(part.id, part);
            }
        }
        *clips = placed;
        // Automation: each lane's points by piece, its values at the
        // pieces' edges kept.
        for lane in &mut automation.lanes {
            if lane.curve.is_empty() {
                continue;
            }
            let old = lane.curve.clone();
            let mut points = Vec::new();
            for q in pieces {
                let edge = |s: i64, v: Option<f64>| {
                    v.map(|value| AutomationPoint {
                        time: musical(s),
                        value,
                        shape: CurveShape::Linear,
                    })
                };
                points.extend(edge(q.at, old.value_at(musical(q.from))));
                for pt in old.points() {
                    let s = samples(pt.time);
                    if q.holds(s) && s > q.from {
                        points.push(AutomationPoint {
                            time: musical(q.place(s)),
                            ..*pt
                        });
                    }
                }
                points.extend(edge(q.place(q.to) - 1, old.value_at(musical(q.to - 1))));
            }
            lane.curve = AutomationCurve::from_points(points);
        }
        let _ = track;
    }
    // Markers, sections and lyric lines with the piece they start in.
    let moved = |t: MusicalTime| {
        let s = samples(t);
        pieces
            .iter()
            .find(|q| q.holds(s))
            .map(|q| musical(q.place(s)))
    };
    a.markers = p
        .markers
        .iter()
        .filter_map(|m| {
            moved(m.position).map(|position| crate::Marker {
                position,
                ..m.clone()
            })
        })
        .collect();
    a.sections = p
        .sections
        .iter()
        .filter_map(|sec| {
            let start = moved(sec.start)?;
            let len = samples(sec.end) - samples(sec.start);
            Some(crate::Section {
                start,
                end: musical(samples(start) + len),
                ..sec.clone()
            })
        })
        .collect();
    a.lyrics = p
        .lyrics
        .iter()
        .filter_map(|l| {
            let start = moved(l.start)?;
            let len = samples(l.end) - samples(l.start);
            Some(crate::lyrics::LyricLine {
                start,
                end: musical(samples(start) + len),
                text: l.text.clone(),
            })
        })
        .collect();
    let range = |r: Option<crate::MusicalRange>| {
        r.and_then(|r| {
            let start = moved(r.start)?;
            let len = samples(r.end) - samples(r.start);
            crate::MusicalRange::new(start, musical(samples(start) + len))
        })
        .or(r)
    };
    a.loop_range = range(p.loop_range);
    a.punch_range = range(p.punch_range);
    a.ids = ids;
    a
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clip::{AudioClip, Clip, ClipFades, StretchSettings};
    use crate::{Marker, Track, TrackColor, TrackKind};
    use faderframe_automation::{AutomationLane, AutomationTarget};
    use faderframe_core::{AudioSourceId, ClipId, TrackId};

    const S: i64 = 48_000;

    /// A track with one ten-second clip from source time 0, a volume ramp
    /// over it and markers at 1 s and 7 s.
    fn project() -> (Project, TrackId, ClipId) {
        let mut p = Project::new("t", 48_000);
        let track: TrackId = p.ids.allocate();
        let id: ClipId = p.ids.allocate();
        let mut t = Track::new(track, TrackKind::Audio, "Dialogue", TrackColor::palette(0));
        let at = |s: i64| p.timeline.to_musical(s, 48_000.0);
        let clip = Clip {
            id,
            track,
            name: "take".into(),
            color: None,
            start: MusicalTime::ZERO,
            muted: false,
            content: ClipContent::Audio(AudioClip {
                source: AudioSourceId(1),
                source_offset: 0,
                length: 10 * S,
                gain_db: 0.0,
                fades: ClipFades::default(),
                stretch: StretchSettings::Off,
                reversed: false,
                warp: None,
                pitch: None,
                effects: None,
            }),
        };
        t.clips.push(id);
        let lane_id = p.ids.allocate();
        let mut lane = AutomationLane {
            id: lane_id,
            target: AutomationTarget::TrackVolume,
            curve: AutomationCurve::new(),
            mode: Default::default(),
            visible: true,
        };
        lane.curve = AutomationCurve::from_points(vec![
            AutomationPoint {
                time: at(0),
                value: 0.0,
                shape: CurveShape::Linear,
            },
            AutomationPoint {
                time: at(10 * S),
                value: 1.0,
                shape: CurveShape::Linear,
            },
        ]);
        t.automation.lanes.push(lane);
        for (n, s) in [(1, S), (7, 7 * S)] {
            p.markers.push(Marker {
                id: p.ids.allocate(),
                position: at(s),
                name: format!("m{n}"),
            });
        }
        p.clips.insert(id, clip);
        p.tracks.push(t);
        (p, track, id)
    }

    #[test]
    fn pieces_cut_move_and_drop() {
        let (p, track, id) = project();
        // 6–10 s first, then 2–6 s a second later; 0–2 s is cut out.
        let pieces = [
            Piece {
                from: 6 * S,
                to: 10 * S,
                at: 0,
            },
            Piece {
                from: 2 * S,
                to: 6 * S,
                at: 5 * S,
            },
        ];
        let a = conformed(&p, &pieces);
        let (_, clips, automation) = a.tracks.iter().find(|t| t.0 == track).unwrap();
        assert_eq!(clips.len(), 2);
        let parts: Vec<(i64, i64, i64)> = clips
            .iter()
            .map(|c| {
                let c = &a.clips[c];
                let au = c.as_audio().unwrap();
                (
                    p.timeline.to_samples(c.start, 48_000.0),
                    au.source_offset,
                    au.length,
                )
            })
            .collect();
        assert_eq!(parts, vec![(0, 6 * S, 4 * S), (5 * S, 2 * S, 4 * S)]);
        assert!(!clips.contains(&id), "a clip cut in pieces gets new ids");
        // Cut inside: short fades at the new edges.
        let first = a.clips[&clips[0]].as_audio().unwrap();
        assert!(first.fades.fade_in > 0 && first.fades.fade_out == 0);
        // The ramp goes with its pieces: 0.6 at the start, 0.2 at 5 s.
        let curve = &automation.lanes[0].curve;
        let v = |s: i64| curve.value_at(p.timeline.to_musical(s, 48_000.0)).unwrap();
        assert!((v(0) - 0.6).abs() < 1e-3, "{}", v(0));
        assert!((v(5 * S) - 0.2).abs() < 1e-3, "{}", v(5 * S));
        assert!((v(5 * S + 2 * S) - 0.4).abs() < 1e-3);
        // The marker at 7 s is at 1 s now; the one at 1 s was cut out.
        assert_eq!(a.markers.len(), 1);
        assert_eq!(p.timeline.to_samples(a.markers[0].position, 48_000.0), S);
    }

    #[test]
    fn a_whole_clip_keeps_its_id() {
        let (p, track, id) = project();
        let a = conformed(
            &p,
            &[Piece {
                from: 0,
                to: 20 * S,
                at: 3 * S,
            }],
        );
        let (_, clips, _) = a.tracks.iter().find(|t| t.0 == track).unwrap();
        assert_eq!(clips, &vec![id]);
        let c = &a.clips[&id];
        assert_eq!(p.timeline.to_samples(c.start, 48_000.0), 3 * S);
        assert_eq!(c.as_audio().unwrap().fades, ClipFades::default());
    }
}
