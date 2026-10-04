//! The arranger's global lanes: markers, arrangement sections, time
//! signature changes and the tempo map. Edits go through commands (one
//! undo step each; drags inside a gesture merge).

use crate::{Result, Session, SessionError};
use faderframe_core::{MarkerId, SectionId};
use faderframe_project::{Command, Marker, Project, Section, TrackColor, arrange};
use faderframe_timeline::{MusicalTime, TempoCurve, TempoMap, TempoPoint};

/// One of the lanes under the arranger's ruler.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum GlobalLane {
    Markers,
    Arranger,
    Signature,
    Tempo,
}

impl GlobalLane {
    pub const ALL: [GlobalLane; 4] = [
        GlobalLane::Markers,
        GlobalLane::Arranger,
        GlobalLane::Signature,
        GlobalLane::Tempo,
    ];

    pub fn title(self) -> &'static str {
        match self {
            GlobalLane::Markers => "Markers",
            GlobalLane::Arranger => "Arranger",
            GlobalLane::Signature => "Signature",
            GlobalLane::Tempo => "Tempo",
        }
    }
}

/// Which global lanes the arranger shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GlobalLanes {
    pub markers: bool,
    pub arranger: bool,
    pub signature: bool,
    pub tempo: bool,
}

impl Default for GlobalLanes {
    fn default() -> Self {
        Self {
            markers: true,
            arranger: true,
            signature: true,
            tempo: true,
        }
    }
}

impl GlobalLanes {
    pub fn shows(&self, lane: GlobalLane) -> bool {
        match lane {
            GlobalLane::Markers => self.markers,
            GlobalLane::Arranger => self.arranger,
            GlobalLane::Signature => self.signature,
            GlobalLane::Tempo => self.tempo,
        }
    }

    pub fn set(&mut self, lane: GlobalLane, on: bool) {
        match lane {
            GlobalLane::Markers => self.markers = on,
            GlobalLane::Arranger => self.arranger = on,
            GlobalLane::Signature => self.signature = on,
            GlobalLane::Tempo => self.tempo = on,
        }
    }

    /// The shown lanes, top to bottom.
    pub fn shown(&self) -> impl Iterator<Item = GlobalLane> + '_ {
        GlobalLane::ALL.into_iter().filter(|l| self.shows(*l))
    }
}

/// Names new sections cycle through.
const SECTION_NAMES: [&str; 8] = [
    "Intro", "Verse", "Chorus", "Verse", "Chorus", "Bridge", "Chorus", "Outro",
];

impl Session {
    /// Add a marker (named "Marker N").
    pub(crate) fn add_marker(&mut self, at: MusicalTime) -> Result<MarkerId> {
        let id: MarkerId = self.project.ids.allocate();
        let n = self.project.markers.len() + 1;
        self.edit(Command::AddMarker {
            marker: Marker {
                id,
                position: at.max(MusicalTime::ZERO),
                name: format!("Marker {n}"),
            },
        })?;
        Ok(id)
    }

    /// Add a section over `start..end` (named after its place in the song).
    pub(crate) fn add_section(
        &mut self,
        start: MusicalTime,
        end: MusicalTime,
    ) -> Result<SectionId> {
        let (start, end) = (start.min(end).max(MusicalTime::ZERO), start.max(end));
        if end <= start {
            return Err(SessionError::Other(
                "drag across the lane to make a section".into(),
            ));
        }
        let id: SectionId = self.project.ids.allocate();
        let n = self.project.sections.len();
        let name = SECTION_NAMES[n % SECTION_NAMES.len()].to_string();
        // Same names get the same colour.
        let color = self
            .project
            .sections
            .iter()
            .find(|s| s.name == name)
            .map_or_else(|| TrackColor::palette(n * 3 + 1), |s| s.color);
        self.edit(Command::AddSection {
            section: Section {
                id,
                name,
                start,
                end,
                color,
            },
        })?;
        Ok(id)
    }

    fn edit_tempo(&mut self, change: impl FnOnce(&mut TempoMap)) -> Result<()> {
        let mut timeline = self.project.timeline.clone();
        change(&mut timeline.tempo);
        if timeline == self.project.timeline {
            return Ok(());
        }
        self.edit(Command::SetTimeline {
            timeline: Box::new(timeline),
        })
    }

    /// A tempo change at `at`, keeping the tempo there.
    /// Apply a rearrangement computed on a copy of the project as one undo
    /// step (`f` returns false when nothing changes).
    fn rearrange(&mut self, label: &str, f: impl FnOnce(&mut Project) -> bool) -> Result<()> {
        let mut copy = self.project.clone();
        if !f(&mut copy) {
            return Ok(());
        }
        let arrangement = arrange::Arrangement::of(&copy);
        // Clips were split and renumbered.
        self.selection.clips.clear();
        self.selection.range = None;
        self.batch(
            label,
            vec![Command::SetArrangement {
                arrangement: Box::new(arrangement),
            }],
        )
    }

    pub(crate) fn move_section(
        &mut self,
        id: SectionId,
        to: MusicalTime,
        copy: bool,
    ) -> Result<()> {
        if copy {
            self.rearrange("Copy Section", |p| {
                arrange::copy_section(p, id, to).is_some()
            })
        } else {
            self.rearrange("Move Section", |p| arrange::move_section(p, id, to))
        }
    }

    pub(crate) fn duplicate_section(&mut self, id: SectionId) -> Result<()> {
        let Some(end) = self
            .project
            .sections
            .iter()
            .find(|s| s.id == id)
            .map(|s| s.end)
        else {
            return Ok(());
        };
        self.rearrange("Duplicate Section", |p| {
            arrange::copy_section(p, id, end).is_some()
        })
    }

    /// Swap with the neighbouring section (the one before or after it).
    pub(crate) fn swap_section(&mut self, id: SectionId, later: bool) -> Result<()> {
        let sections = &self.project.sections;
        let Some(i) = sections.iter().position(|s| s.id == id) else {
            return Ok(());
        };
        let to = if later {
            sections.get(i + 1).map(|n| n.end)
        } else {
            i.checked_sub(1).map(|j| sections[j].start)
        };
        let Some(to) = to else {
            return Ok(());
        };
        self.rearrange(
            if later {
                "Move Section Later"
            } else {
                "Move Section Earlier"
            },
            |p| arrange::move_section(p, id, to),
        )
    }

    pub(crate) fn delete_section_content(&mut self, id: SectionId) -> Result<()> {
        self.rearrange("Delete Section", |p| arrange::delete_section(p, id))
    }

    pub(crate) fn add_tempo_point(&mut self, at: MusicalTime) -> Result<()> {
        let bpm = self.project.timeline.tempo.bpm_at(at);
        self.edit_tempo(|t| {
            t.set_point(TempoPoint {
                position: at.max(MusicalTime::ZERO),
                bpm,
                curve: TempoCurve::Constant,
            });
        })
    }

    /// Move point `index` and/or change its tempo (the first point stays
    /// at the start).
    pub(crate) fn set_tempo_point(
        &mut self,
        index: usize,
        position: MusicalTime,
        bpm: f64,
    ) -> Result<()> {
        let points = self.project.timeline.tempo.points();
        let Some(old) = points.get(index).copied() else {
            return Ok(());
        };
        // Between the neighbours (points keep their order).
        let lo = if index == 0 {
            MusicalTime::ZERO
        } else {
            points[index - 1].position + MusicalTime::from_ticks(1)
        };
        let hi = points
            .get(index + 1)
            .map_or(MusicalTime::from_quarters(1e7), |p| {
                p.position - MusicalTime::from_ticks(1)
            });
        let position = if index == 0 {
            MusicalTime::ZERO
        } else {
            position.clamp(lo, hi)
        };
        let bpm = bpm.clamp(TempoMap::MIN_BPM, TempoMap::MAX_BPM);
        self.edit_tempo(|t| {
            if position != old.position {
                t.remove_point(index);
            }
            t.set_point(TempoPoint {
                position,
                bpm,
                curve: old.curve,
            });
        })
    }

    pub(crate) fn remove_tempo_point(&mut self, index: usize) -> Result<()> {
        self.edit_tempo(|t| {
            t.remove_point(index);
        })
    }

    /// Ramp from point `index` to the next one, or step.
    pub(crate) fn set_tempo_ramp(&mut self, index: usize, ramp: bool) -> Result<()> {
        let Some(p) = self.project.timeline.tempo.points().get(index).copied() else {
            return Ok(());
        };
        self.edit_tempo(|t| {
            t.set_point(TempoPoint {
                curve: if ramp {
                    TempoCurve::Linear
                } else {
                    TempoCurve::Constant
                },
                ..p
            });
        })
    }
}
