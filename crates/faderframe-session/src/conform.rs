//! Conforming the session to a new picture cut (`faderframe_conform`): from
//! the old and the new cut lists (CMX3600 EDL or OpenTimelineIO), or from
//! the old and the new picture matched shot by shot (a job, in `video`).
//! The changes become pieces of the timeline (`faderframe_project::conform`)
//! put in place as one undo step; what comes before the cut stays, what
//! follows it moves with its end, and new material gets a marker each.

use crate::{NoticeLevel, Result, Session, SessionError};
use faderframe_conform::{Changes, CutList, Kind};
use faderframe_project::conform::Piece;
use faderframe_project::{Command, Marker};
use std::path::PathBuf;

/// What to conform to.
#[derive(Clone, Debug, PartialEq)]
pub enum ConformOp {
    /// The cut in `new` from the one in `old` (EDL or OpenTimelineIO).
    Lists { old: PathBuf, new: PathBuf },
}

impl Session {
    pub(crate) fn conform_op(&mut self, op: ConformOp) -> Result<()> {
        match op {
            ConformOp::Lists { old, new } => {
                let rate = self.timecode().rate;
                let read = |p: &PathBuf| -> Result<CutList> {
                    let text = std::fs::read_to_string(p)
                        .map_err(|e| SessionError::Other(format!("{}: {e}", p.display())))?;
                    CutList::read(&text, rate)
                        .map_err(|e| SessionError::Other(format!("{}: {e}", p.display())))
                };
                let (o, n) = (read(&old)?, read(&new)?);
                if o.track(Kind::Video, 0).is_empty() || n.track(Kind::Video, 0).is_empty() {
                    return Err(SessionError::Other(
                        "the cut lists have no picture events (V1)".into(),
                    ));
                }
                let changes = faderframe_conform::changes(&o, &n, Kind::Video, 0);
                // Record times are timecode: the project's start is the
                // timeline's zero (without one set, the hour the old cut
                // starts in, and the project takes that timecode).
                let mut tc = self.project.timecode;
                let first = o.track(Kind::Video, 0)[0].record_in;
                let set = tc.is_none();
                let start = match tc {
                    Some(t) => t.rate.seconds_of(t.start.total_frames(t.rate)),
                    None => {
                        let hour = (first / 3600.0).floor() * 3600.0;
                        tc = Some(faderframe_project::video::ProjectTimecode {
                            rate,
                            start: faderframe_core::timecode::Timecode::from_frames(
                                rate.frame_at(hour),
                                rate,
                            ),
                        });
                        hour
                    }
                };
                let name = new.file_name().map_or_else(
                    || "the new cut".into(),
                    |n| n.to_string_lossy().into_owned(),
                );
                let ends = |l: &CutList| {
                    let v = l.track(Kind::Video, 0);
                    (
                        v.first().map_or(0.0, |e| e.record_in),
                        v.iter().map(|e| e.record_out).fold(0.0, f64::max),
                    )
                };
                self.apply_conform(
                    &changes,
                    |t| t - start,
                    |t| t - start,
                    (ends(&o), ends(&n)),
                    &name,
                    set.then_some(tc).flatten(),
                )
            }
        }
    }

    /// Put `changes` in place: `old` and `new` turn their times into
    /// timeline seconds; `ends` are the old and the new cut's first and
    /// last record times (what lies outside goes with them).
    pub(crate) fn apply_conform(
        &mut self,
        changes: &Changes,
        old: impl Fn(f64) -> f64,
        new: impl Fn(f64) -> f64,
        ends: ((f64, f64), (f64, f64)),
        name: &str,
        timecode: Option<faderframe_project::video::ProjectTimecode>,
    ) -> Result<()> {
        let rate = self.project.sample_rate as f64;
        let at = |t: f64| (t * rate).round() as i64;
        let ((old_in, old_out), (new_in, new_out)) = ends;
        let mut pieces: Vec<Piece> = changes
            .moves
            .iter()
            .map(|m| Piece {
                from: at(old(m.old_in)),
                to: at(old(m.old_out)),
                at: at(new(m.new_in)),
            })
            .collect();
        // Before the cut: stays with its start; after it: with its end.
        let head = at(old(old_in));
        if head > 0 {
            pieces.push(Piece {
                from: 0,
                to: head,
                at: at(new(new_in)) - head,
            });
        }
        pieces.push(Piece {
            from: at(old(old_out)),
            to: i64::MAX / 4,
            at: at(new(new_out)),
        });
        let mut arrangement = faderframe_project::conform::conformed(&self.project, &pieces);
        let mut markers = Vec::new();
        for (n, (a, _)) in changes.added.iter().enumerate() {
            markers.push(Marker {
                id: arrangement.ids.allocate(),
                position: self.project.timeline.to_musical(at(new(*a)).max(0), rate),
                name: format!("New shot {}", n + 1),
            });
        }
        arrangement.markers.extend(markers);
        arrangement.markers.sort_by_key(|m| m.position);
        let mut commands = Vec::new();
        if let Some(t) = timecode {
            commands.push(Command::SetTimecode { timecode: Some(t) });
        }
        if !self.project.adr.cues.is_empty() {
            commands.push(Command::SetAdr {
                adr: Box::new(faderframe_project::conform::conformed_cues(
                    &self.project,
                    &pieces,
                )),
            });
        }
        commands.push(Command::SetArrangement {
            arrangement: Box::new(arrangement),
        });
        self.edit(Command::Batch {
            label: "Conform".into(),
            commands,
        })?;
        let removed: f64 = changes.removed.iter().map(|(a, b)| b - a).sum();
        self.notify(
            NoticeLevel::Info,
            format!(
                "Conformed to {name}: {} span{} placed, {} new shot{} marked, {removed:.1} s cut out",
                changes.moves.len(),
                if changes.moves.len() == 1 { "" } else { "s" },
                changes.added.len(),
                if changes.added.len() == 1 { "" } else { "s" },
            ),
        );
        Ok(())
    }
}
