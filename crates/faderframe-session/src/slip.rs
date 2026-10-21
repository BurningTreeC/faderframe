//! Slip editing: the audio or MIDI inside a clip moves while the clip
//! stays (issue #9).
//!
//! The arranger slips with Ctrl+Alt+drag in a clip's body (the Cubase and
//! Studio One gesture; Pro Tools' "Slip mode" is free *moving* and is
//! [`crate::EditMode::Slip`]). The clip keeps its place, its length, its
//! fades and its gain; what it plays under them moves:
//!
//! * **Audio**: the source offset changes, kept inside the source (the
//!   drag stops where the audio ends). Pitch edits and spectral edits are
//!   in source frames, so they stay with the audio; a clip with clip
//!   effects plays their render and slips within it, never into the
//!   devices' latency before the original audio. A warped clip's time map
//!   moves with the audio (markers keep their source frames and move in
//!   the clip; those slipping out of it go). A reversed clip plays its
//!   source backwards, so its offset moves the other way.
//! * **MIDI**: notes, controllers, expression and SysEx move; what moves
//!   before the clip's start is cut there, as trimming the start does
//!   (notes moved past the end are kept unheard, as trimming the end
//!   does).
//! * Take folders do not slip (as they do not trim): flatten the comp
//!   first.
//!
//! A drag sends the whole distance from where it started; every step
//! recomputes from the clip as the gesture found it
//! ([`Session::gesture_clip`]), so dragging back restores it exactly, and
//! the drag is one undo step. All selected clips slip together when the
//! dragged clip is selected (by the same distance in time).

use crate::{NoticeLevel, Result, Session};
use faderframe_core::ClipId;
use faderframe_project::{
    AudioClip, ClipContent, Command, ControllerPoint, MidiClip, Warp, WarpMarker,
};
use faderframe_timeline::MusicalTime;

/// `d` within `[lo, hi]`, which always holds 0 (a clip already outside
/// its bounds may still move back in).
fn bounded(d: i64, lo: i64, hi: i64) -> i64 {
    d.clamp(lo.min(0), hi.max(0))
}

/// The output frame where source frame `src` plays, over a warp's anchors
/// (linear beyond them: the inverse of [`Warp::source_at_points`]).
fn output_at_points(pts: &[WarpMarker], src: f64) -> f64 {
    if pts.len() < 2 {
        return pts
            .first()
            .map_or(src, |p| p.at as f64 + src - p.source as f64);
    }
    let i = pts
        .partition_point(|p| (p.source as f64) <= src)
        .saturating_sub(1)
        .min(pts.len() - 2);
    let (a, b) = (pts[i], pts[i + 1]);
    let span = (b.source - a.source).max(1) as f64;
    a.at as f64 + (src - a.source as f64) * (b.at - a.at) as f64 / span
}

/// `a` with its audio `df` output frames later (earlier when negative) in
/// the clip, inside source frames `lo..hi`.
pub fn slip_audio(a: &AudioClip, df: i64, lo: i64, hi: i64) -> AudioClip {
    let mut out = a.clone();
    let (so, len) = (a.source_offset, a.length);
    if a.reversed {
        // Played backwards: later in the clip is earlier in the source.
        let d = bounded(df, lo - so, hi - len - so);
        out.source_offset = so + d;
        if let Some(w) = &mut out.warp {
            for m in &mut w.markers {
                m.source += d;
            }
        }
        return out;
    }
    match &a.warp {
        None => {
            let d = bounded(df, so + len - hi, so - lo);
            out.source_offset = so - d;
        }
        Some(w) => {
            // The clip's output frame u now plays what u - d played.
            let pts = w.points(so, len);
            let d_max = (-output_at_points(&pts, lo as f64)).floor() as i64;
            let d_min = (len as f64 - output_at_points(&pts, hi as f64)).ceil() as i64;
            let d = bounded(df, d_min, d_max);
            let start = (Warp::source_at_points(&pts, -d as f64).round() as i64).max(lo);
            let end = (Warp::source_at_points(&pts, (len - d) as f64).round() as i64)
                .min(hi)
                .max(start + 1);
            let mut moved = Warp {
                source_length: end - start,
                markers: w
                    .markers
                    .iter()
                    .map(|m| WarpMarker {
                        at: m.at + d,
                        source: m.source,
                    })
                    .filter(|m| m.at > 0 && m.at < len && m.source > start && m.source < end)
                    .collect(),
                algorithm: w.algorithm,
            };
            moved.normalize(start, len);
            out.source_offset = start;
            out.warp = Some(moved);
        }
    }
    out
}

/// `m` with everything in it `by` later (earlier when negative); what
/// moves before the clip's start is cut there.
pub fn slip_midi(m: &MidiClip, by: MusicalTime) -> MidiClip {
    let mut out = m.clone();
    for n in &mut out.notes {
        n.start += by;
    }
    for l in &mut out.controllers {
        for p in &mut l.points {
            p.time += by;
        }
    }
    for e in &mut out.sysex {
        e.time += by;
    }
    let zero = MusicalTime::ZERO;
    out.notes.retain(|n| n.end() > zero);
    for n in &mut out.notes {
        if n.start < zero {
            n.length = n.end();
            n.start = zero;
        }
    }
    for l in &mut out.controllers {
        // The value moved to before the start still holds at it.
        let carried = l.value_at(zero);
        l.points.retain(|p| p.time >= zero);
        if let Some(v) = carried
            && l.points.first().is_none_or(|p| p.time > zero)
        {
            l.points.insert(
                0,
                ControllerPoint {
                    time: zero,
                    value: v,
                },
            );
        }
    }
    out.sysex.retain(|e| e.time >= zero);
    out.prune_expressions();
    out
}

impl Session {
    /// Slip `clips` by `by` ticks (later when positive) from where the
    /// running gesture found them (see the module docs).
    pub fn slip_clips(&mut self, clips: &[ClipId], by: i64) -> Result<()> {
        let first_step = clips.iter().all(|c| !self.gesture_base.contains_key(c));
        let mut cmds = Vec::new();
        let mut folders = 0;
        for id in clips {
            let c = self.gesture_clip(*id)?;
            let content = match &c.content {
                ClipContent::Audio(a) => {
                    // As far in frames as the clip's start would move.
                    let to = MusicalTime(c.start.ticks() + by.abs());
                    let d = self.frames_between(c.start, to);
                    let df = if by < 0 { -d } else { d };
                    let p = &self.project;
                    let hi = p
                        .sources
                        .get(&a.source)
                        .map_or(i64::MAX / 4, |s| s.frames(p.sample_rate));
                    // Never into the devices' latency before a render.
                    let lo = a.effects.as_ref().map_or(0, |e| e.rendered_offset.max(0));
                    ClipContent::Audio(slip_audio(a, df, lo, hi))
                }
                ClipContent::Midi(m) => ClipContent::Midi(slip_midi(m, MusicalTime(by))),
                ClipContent::Takes(_) => {
                    folders += 1;
                    continue;
                }
            };
            cmds.push(Command::SetClipContent {
                clip: c.id,
                start: c.start,
                content: Box::new(content),
            });
        }
        if folders > 0 && first_step {
            self.notify(
                NoticeLevel::Info,
                "Take folders do not slip: flatten the comp first",
            );
        }
        self.batch(
            if cmds.len() > 1 {
                "Slip Clips"
            } else {
                "Slip Clip"
            },
            cmds,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use faderframe_core::{AudioSourceId, NoteId};
    use faderframe_project::{
        ClipFades, ControllerLane, MidiController, MidiNote, StretchSettings,
    };

    fn audio(offset: i64, length: i64) -> AudioClip {
        AudioClip {
            source: AudioSourceId(1),
            source_offset: offset,
            length,
            gain_db: -3.0,
            fades: ClipFades {
                fade_in: 100,
                fade_out: 200,
                ..ClipFades::default()
            },
            stretch: StretchSettings::Off,
            reversed: false,
            warp: None,
            pitch: None,
            effects: None,
            spectral: None,
        }
    }

    #[test]
    fn audio_slips_inside_its_source() {
        let a = audio(1_000, 4_000);
        // Later in the clip: an earlier source offset.
        let b = slip_audio(&a, 300, 0, 10_000);
        assert_eq!(b.source_offset, 700);
        assert_eq!(
            (b.length, b.fades, b.gain_db),
            (a.length, a.fades, a.gain_db)
        );
        // Stops at the source's start and end.
        assert_eq!(slip_audio(&a, 5_000, 0, 10_000).source_offset, 0);
        assert_eq!(slip_audio(&a, -9_000, 0, 10_000).source_offset, 6_000);
        // A reversed clip moves its offset the other way.
        let r = AudioClip {
            reversed: true,
            ..a.clone()
        };
        assert_eq!(slip_audio(&r, 300, 0, 10_000).source_offset, 1_300);
    }

    #[test]
    fn a_warped_clip_keeps_its_time_map_with_the_audio() {
        // Twice as slow: 4000 output frames play 2000 source frames, a
        // marker pins source 1500 at output 1000.
        let mut a = audio(1_000, 4_000);
        a.warp = Some(Warp {
            source_length: 2_000,
            markers: vec![WarpMarker {
                at: 1_000,
                source: 1_500,
            }],
            algorithm: Default::default(),
        });
        let b = slip_audio(&a, 400, 0, 10_000);
        let w = b.warp.clone().unwrap_or_else(|| Warp::uniform(1));
        assert_eq!(
            w.markers,
            vec![WarpMarker {
                at: 1_400,
                source: 1_500
            }]
        );
        // Output frame 1400 now plays what 1000 played.
        assert!((b.source_at(1_400.0) - a.source_at(1_000.0)).abs() < 1.0);
        assert!((b.source_at(3_000.0) - a.source_at(2_600.0)).abs() < 1.0);
        assert_eq!(b.length, a.length);
    }

    #[test]
    fn midi_moves_and_what_leaves_the_start_is_cut() {
        let q = MusicalTime::from_quarters;
        let note = |id, start: f64, len: f64| MidiNote {
            id: NoteId(id),
            start: q(start),
            length: q(len),
            key: 60,
            velocity: 100,
            channel: 0,
            muted: false,
            release: None,
        };
        let mut lane = ControllerLane::new(MidiController::Cc { number: 1 }, 0);
        lane.points = vec![
            ControllerPoint {
                time: q(0.0),
                value: 10,
            },
            ControllerPoint {
                time: q(2.0),
                value: 90,
            },
        ];
        let m = MidiClip {
            length: q(8.0),
            notes: vec![note(1, 0.0, 1.0), note(2, 1.0, 2.0), note(3, 7.0, 1.0)],
            controllers: vec![lane],
            ..MidiClip::default()
        };
        let later = slip_midi(&m, q(1.0));
        assert_eq!(
            later.notes.iter().map(|n| n.start).collect::<Vec<_>>(),
            vec![q(1.0), q(2.0), q(8.0)],
            "the last note moves past the end, kept unheard"
        );
        let earlier = slip_midi(&m, -q(1.5));
        assert_eq!(
            earlier
                .notes
                .iter()
                .map(|n| (n.id, n.start, n.length))
                .collect::<Vec<_>>(),
            vec![(NoteId(2), q(0.0), q(1.5)), (NoteId(3), q(5.5), q(1.0))]
        );
        assert_eq!(earlier.controllers[0].value_at(q(0.0)), Some(10));
        assert_eq!(earlier.controllers[0].value_at(q(0.5)), Some(90));
        assert_eq!(earlier.length, m.length);
    }
}
