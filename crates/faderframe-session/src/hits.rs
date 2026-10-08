//! Tempo through hit points: the project's markers (all of them, the cuts
//! found in the picture, or those in the edit range) become hits, and
//! [`faderframe_timeline::hits::solve`] finds the steadiest tempo map that
//! puts each on a beat or a bar line. Applying it is one undo step: the new
//! tempo map, the hit markers moved onto their beats (where they sound
//! stays), the other markers kept where they sound, and — when asked —
//! audio clips kept where they sound too (MIDI follows the new tempo; the
//! picture is placed in time already).

use crate::{NoticeLevel, Result, Session, SessionError};
use faderframe_project::{ClipContent, Command, Marker};
use faderframe_timeline::hits::{HitGrid, HitSettings, HitSolution, solve};
use faderframe_timeline::{MusicalTime, Timeline};

/// Which markers are hits.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum HitSource {
    #[default]
    Markers,
    /// The cuts found in the picture ("Cut n" markers).
    Cuts,
    /// The markers inside the edit range.
    Range,
}

/// A request to solve (and apply).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct HitRequest {
    pub source: HitSource,
    pub settings: HitSettings,
    /// Audio clips stay where they sound (else they follow the beats).
    pub keep_audio: bool,
}

impl Default for HitRequest {
    fn default() -> Self {
        Self {
            source: HitSource::Markers,
            settings: HitSettings::default(),
            keep_audio: true,
        }
    }
}

/// What a solution would do: the hits used, those left out, the tempos.
#[derive(Clone, Debug, PartialEq)]
pub struct HitPreview {
    pub hits: usize,
    pub dropped: Vec<String>,
    pub slowest: f64,
    pub fastest: f64,
    /// The largest change from one stretch to the next (a share).
    pub largest_change: f64,
}

impl Session {
    /// The hit markers for `source`, in order, with when they sound
    /// (seconds).
    pub fn hit_markers(&self, source: HitSource) -> Vec<(Marker, f64)> {
        let p = &self.project;
        let range = self.selection.range;
        p.markers
            .iter()
            .filter(|m| match source {
                HitSource::Markers => true,
                HitSource::Cuts => m.name.starts_with("Cut "),
                HitSource::Range => {
                    range.is_some_and(|r| r.start <= m.position && m.position < r.end)
                }
            })
            .map(|m| (m.clone(), p.timeline.tempo.musical_to_seconds(m.position)))
            .collect()
    }

    fn hit_solution(&self, req: &HitRequest) -> Result<(Vec<(Marker, f64)>, HitSolution)> {
        let hits = self.hit_markers(req.source);
        if hits.is_empty() {
            return Err(SessionError::Other(match req.source {
                HitSource::Markers => "no markers to land on beats: add markers at the hits (or find the cuts in the picture)".into(),
                HitSource::Cuts => "no cuts found yet: find the cuts in the picture first (the video clip's menu)".into(),
                HitSource::Range => "no markers in the edit range".into(),
            }));
        }
        let mut settings = req.settings;
        if let HitGrid::Bars(_) = settings.grid {
            let bar = self
                .project
                .timeline
                .meter
                .signature_at(MusicalTime::ZERO)
                .bar_length()
                .quarters();
            settings.grid = HitGrid::Bars(bar);
        }
        let seconds: Vec<f64> = hits.iter().map(|(_, s)| *s).collect();
        let solution = solve(&seconds, &settings)
            .ok_or_else(|| SessionError::Other("no tempo lands those hits".into()))?;
        Ok((hits, solution))
    }

    /// What landing the hits would do (for the dialog).
    pub fn preview_hit_tempo(&self, req: &HitRequest) -> Result<HitPreview> {
        let (hits, s) = self.hit_solution(req)?;
        let tempos = if s.tempos.len() > 1 {
            &s.tempos[1..]
        } else {
            &s.tempos[..]
        };
        Ok(HitPreview {
            hits: s.positions.len(),
            dropped: s.dropped.iter().map(|i| hits[*i].0.name.clone()).collect(),
            slowest: tempos.iter().copied().fold(f64::INFINITY, f64::min),
            fastest: tempos.iter().copied().fold(0.0, f64::max),
            largest_change: s.largest_change(),
        })
    }

    /// Land the hits on beats (one undo step; see the module docs).
    pub fn tempo_from_hits(&mut self, req: &HitRequest) -> Result<()> {
        let (hits, s) = self.hit_solution(req)?;
        let p = &self.project;
        let old = p.timeline.tempo.clone();
        let mut timeline: Timeline = p.timeline.clone();
        timeline.tempo = s.tempo.clone();
        let new = &timeline.tempo;
        // Where something sounding at `pos` now goes.
        let keep = |pos: MusicalTime| new.seconds_to_musical(old.musical_to_seconds(pos));
        let mut commands = vec![Command::SetTimeline {
            timeline: Box::new(timeline.clone()),
        }];
        let landed: Vec<_> = s
            .positions
            .iter()
            .map(|(i, q)| (hits[*i].0.id, *q))
            .collect();
        for m in &p.markers {
            let position = landed
                .iter()
                .find(|(id, _)| *id == m.id)
                .map_or_else(|| keep(m.position), |(_, q)| MusicalTime::from_quarters(*q));
            if position != m.position {
                commands.push(Command::UpdateMarker {
                    marker: Marker {
                        position,
                        ..m.clone()
                    },
                });
            }
        }
        if req.keep_audio {
            for t in &p.tracks {
                for c in p.clips_of(t.id) {
                    if matches!(c.content, ClipContent::Audio(_) | ClipContent::Takes(_)) {
                        let start = keep(c.start);
                        if start != c.start {
                            commands.push(Command::MoveClip {
                                clip: c.id,
                                track: t.id,
                                start,
                            });
                        }
                    }
                }
            }
        }
        let (n, dropped) = (s.positions.len(), s.dropped.len());
        let tempos = if s.tempos.len() > 1 {
            &s.tempos[1..]
        } else {
            &s.tempos[..]
        };
        let (lo, hi) = (
            tempos.iter().copied().fold(f64::INFINITY, f64::min),
            tempos.iter().copied().fold(0.0, f64::max),
        );
        self.batch("Tempo from Hit Points", commands)?;
        self.notify(
            NoticeLevel::Info,
            format!(
                "{n} hit{} on {}, {lo:.1}–{hi:.1} BPM{}",
                if n == 1 { "" } else { "s" },
                match req.settings.grid {
                    HitGrid::Bars(_) => "bar lines",
                    HitGrid::Eighths => "eighths",
                    HitGrid::Beats => "beats",
                },
                if dropped > 0 {
                    format!(" ({dropped} too close to the one before, left out)")
                } else {
                    String::new()
                }
            ),
        );
        Ok(())
    }
}
