//! 360° video in the session: the video window looks into a spherical
//! picture through a flat viewport (where it looks, how wide), the
//! listener's head turns with it on headphones — and a head tracker
//! turns both — and a click on the picture pans a track to the direction
//! the picture shows there.
//!
//! A picture is spherical when its file says so (Spherical Video V1/V2,
//! read by `faderframe_video::spherical` at import) or when the user marks
//! it ([`SphereOp::SetLayout`], one undoable step on the project's video).
//! The viewport is the session's, never saved: linked (the default) it
//! *is* the listener's head ([`Session::head_orientation`]), so dragging
//! the picture turns the binaural scene and a tracker's OSC turns the
//! picture; unlinked it looks on its own. Picture and sound share one
//! frame: the projection's middle (after its pose) is the scene's front,
//! azimuth positive to the left, elevation up — so a track panned to a
//! direction (an ambisonic field's encoder, a bed's VBAP panner) sounds
//! where the picture shows that direction.
//!
//! Panning by clicking: [`SphereOp::PanTrack`] chooses the track clicks
//! pan; the view sends a gesture of [`SphereOp::PanTo`]s (one undo step),
//! which go through [`Session::edit`] like the panner's own drags — so
//! lanes in Touch, Latch or Write record them while playing
//! ([`SphereOp::WriteAutomation`] makes the lanes and puts them in Touch).

use crate::{AutomationTarget, Result, Session, SessionError};
use faderframe_automation::{AutomationCurve, AutomationLane, AutomationMode};
use faderframe_core::ambisonics::Orientation;
use faderframe_core::spherical::{DEFAULT_FOV, MAX_FOV, MIN_FOV, Spherical};
use faderframe_core::{AutomationLaneId, SurroundParam, TrackId, VideoSourceId};
use faderframe_project::Command;

/// Frames of a 360° picture are decoded at most this wide (an 8K
/// equirectangular frame is 133 MB of RGBA; this keeps a 90° view of it
/// sharp in a window 1440 pixels wide).
pub const MAX_DECODE_WIDTH: u32 = 5760;

/// Where the session looks into a 360° picture.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SphereLook {
    /// The viewport's turn (yaw left, pitch up, roll to the right).
    pub look: Orientation,
    /// Its field of view across the window (degrees).
    pub fov: f32,
    /// It is the listener's head.
    pub linked: bool,
    /// The track a click on the picture pans.
    pub pan_track: Option<TrackId>,
}

/// The session's viewport.
#[derive(Clone, Copy, Debug)]
pub(crate) struct SphereState {
    fov: f32,
    linked: bool,
    /// Where it looks when not linked.
    look: Orientation,
    pan_track: Option<TrackId>,
}

impl Default for SphereState {
    fn default() -> Self {
        Self {
            fov: DEFAULT_FOV,
            linked: true,
            look: Orientation::IDENTITY,
            pan_track: None,
        }
    }
}

/// Looking into 360° pictures and panning by them.
#[derive(Clone, Debug, PartialEq)]
pub enum SphereOp {
    /// Look this way (degrees; see `Orientation::from_ypr`). Linked, the
    /// listener's head turns with it.
    Look { yaw: f32, pitch: f32, roll: f32 },
    /// The field of view across the window (degrees, 20–150).
    Fov(f32),
    /// Look straight ahead again (linked: the head too).
    Ahead,
    /// Turn picture and listener's head together, or each on its own.
    Link(bool),
    /// Clicks on the picture pan this track (`None`: they only look).
    PanTrack(Option<TrackId>),
    /// Pan `track` to a direction of the scene (degrees; an edit, which
    /// lanes being written record).
    PanTo {
        track: TrackId,
        azimuth: f32,
        elevation: f32,
    },
    /// Mark a video's picture as 360° (laid out so) or flat (`None`).
    SetLayout {
        source: VideoSourceId,
        spherical: Option<Spherical>,
    },
    /// Make `track`'s direction lanes and write them while playing (Touch),
    /// or only read them again.
    WriteAutomation { track: TrackId, on: bool },
}

impl SphereOp {
    /// Whether it changes the project (the others only how it is shown).
    pub fn edits(&self) -> bool {
        matches!(
            self,
            SphereOp::PanTo { .. } | SphereOp::SetLayout { .. } | SphereOp::WriteAutomation { .. }
        )
    }
}

/// A track a click on a 360° picture can pan, as it is panned now.
#[derive(Clone, Debug, PartialEq)]
pub struct SpherePanTarget {
    pub track: TrackId,
    pub name: String,
    /// Where it sounds now (degrees, as shown: automation included).
    pub azimuth: f32,
    pub elevation: f32,
    /// Its elevation applies (a field, or a bed with heights).
    pub has_elevation: bool,
    /// Into an ambisonic field (else a bed's direction panner).
    pub field: bool,
    /// Its direction lanes are written while playing.
    pub writing: bool,
}

impl Session {
    /// Where 360° pictures are looked at from.
    pub fn sphere_look(&self) -> SphereLook {
        let s = &self.video.sphere;
        SphereLook {
            look: if s.linked {
                self.head_orientation()
            } else {
                s.look
            },
            fov: s.fov,
            linked: s.linked,
            pan_track: s.pan_track.filter(|t| self.sphere_pannable(*t)),
        }
    }

    /// The 360° layout of a video's picture, if it is one.
    pub fn video_spherical(&self, source: VideoSourceId) -> Option<Spherical> {
        self.project.video.sources.get(&source)?.spherical
    }

    /// Whether a click on a 360° picture can pan `track` (it is panned by
    /// direction: into an ambisonic field, or into a bed by VBAP).
    pub fn sphere_pannable(&self, track: TrackId) -> bool {
        self.project.track(track).is_some_and(|t| {
            crate::automation::has_automation(t.kind)
                && self.project.spatially_panned(t).is_some_and(|dst| {
                    SurroundParam::Azimuth.applies_into(
                        t.chain_layout(),
                        dst,
                        &self.project.effective_surround(t),
                    )
                })
        })
    }

    /// The tracks clicks can pan, in the editors' order, as panned now.
    pub fn sphere_pan_targets(&self) -> Vec<SpherePanTarget> {
        self.project
            .folder_order()
            .into_iter()
            .filter(|t| self.sphere_pannable(t.id))
            .filter_map(|t| {
                let dst = self.project.spatially_panned(t)?;
                let pan = self.shown_surround(t);
                let effective = self.project.effective_surround(t);
                let lanes = [SurroundParam::Azimuth, SurroundParam::Elevation].map(|p| {
                    t.automation
                        .lane(AutomationTarget::Surround(p))
                        .map(|l| l.mode)
                });
                Some(SpherePanTarget {
                    track: t.id,
                    name: t.name.clone(),
                    azimuth: pan.azimuth,
                    elevation: pan.elevation,
                    has_elevation: SurroundParam::Elevation.applies_into(
                        t.chain_layout(),
                        dst,
                        &effective,
                    ),
                    field: matches!(dst, faderframe_core::ChannelLayout::Ambisonic(_)),
                    writing: lanes[0].is_some_and(|m| {
                        matches!(
                            m,
                            AutomationMode::Touch | AutomationMode::Latch | AutomationMode::Write
                        )
                    }),
                })
            })
            .collect()
    }

    /// The size a frame of 360° picture `s` is decoded at for a view of
    /// `max` pixels: as wide as keeps the view sharp at the field of view
    /// (the picture's own size at most).
    pub(crate) fn sphere_decode_size(&self, s: &Spherical, max: (u32, u32)) -> (u32, u32) {
        let w = s
            .width_for(max.0 as f32, self.video.sphere.fov)
            .ceil()
            .clamp(16.0, MAX_DECODE_WIDTH as f32) as u32;
        (w, u32::MAX)
    }

    pub(crate) fn sphere_op(&mut self, op: SphereOp) -> Result<()> {
        match op {
            SphereOp::Look { yaw, pitch, roll } => {
                let o = Orientation::from_ypr(yaw, pitch.clamp(-90.0, 90.0), roll);
                self.look_at(o);
            }
            SphereOp::Fov(fov) => {
                if fov.is_finite() {
                    self.video.sphere.fov = fov.clamp(MIN_FOV, MAX_FOV);
                }
            }
            SphereOp::Ahead => self.look_at(Orientation::IDENTITY),
            SphereOp::Link(on) => {
                if on != self.video.sphere.linked {
                    if on {
                        // The head takes the picture's way.
                        let look = self.video.sphere.look;
                        self.video.sphere.linked = true;
                        self.point_head(look);
                    } else {
                        self.video.sphere.look = self.head_orientation();
                        self.video.sphere.linked = false;
                    }
                }
            }
            SphereOp::PanTrack(track) => {
                if let Some(t) = track
                    && !self.sphere_pannable(t)
                {
                    return Err(SessionError::Other(
                        "only a track panned by direction (into an ambisonic field, or a \
                         bed's VBAP panner) can be panned on the picture"
                            .into(),
                    ));
                }
                self.video.sphere.pan_track = track;
            }
            SphereOp::PanTo {
                track,
                azimuth,
                elevation,
            } => return self.pan_to_direction(track, azimuth, elevation),
            SphereOp::SetLayout { source, spherical } => {
                let mut video = self.project.video.clone();
                let Some(s) = video.sources.get_mut(&source) else {
                    return Err(SessionError::Other("no such video".into()));
                };
                if s.spherical == spherical {
                    return Ok(());
                }
                s.spherical = spherical;
                self.edit(Command::SetVideo {
                    video: Box::new(video),
                })?;
            }
            SphereOp::WriteAutomation { track, on } => {
                return self.write_direction_automation(track, on);
            }
        }
        self.revision += 1;
        Ok(())
    }

    /// The viewport looks at `o` (linked: the head points there).
    fn look_at(&mut self, o: Orientation) {
        if self.video.sphere.linked {
            self.point_head(o);
        } else {
            self.video.sphere.look = o;
        }
    }

    /// `track` panned to (`azimuth`, `elevation`), as far as its panning
    /// goes (a bed's elevation range; nothing it does not pan).
    fn pan_to_direction(&mut self, track: TrackId, azimuth: f32, elevation: f32) -> Result<()> {
        if !self.sphere_pannable(track) {
            return Err(SessionError::Other(
                "the track is not panned by direction".into(),
            ));
        }
        let Some(t) = self.project.track(track) else {
            return Ok(());
        };
        let Some(dst) = self.project.spatially_panned(t) else {
            return Ok(());
        };
        let effective = self.project.effective_surround(t);
        let mut pan = t.surround;
        pan = SurroundParam::Azimuth.set(pan, azimuth);
        if SurroundParam::Elevation.applies_into(t.chain_layout(), dst, &effective) {
            let (lo, hi, _) = SurroundParam::Elevation.range_into(dst);
            pan = SurroundParam::Elevation.set(pan, elevation.clamp(lo, hi));
        }
        self.edit(Command::SetTrackSurround { track, pan })
    }

    /// Lanes for `track`'s direction (made where missing) written while
    /// playing (Touch), or only read; one undo step.
    fn write_direction_automation(&mut self, track: TrackId, on: bool) -> Result<()> {
        if !self.sphere_pannable(track) {
            return Err(SessionError::Other(
                "the track is not panned by direction".into(),
            ));
        }
        let Some(t) = self.project.track(track).cloned() else {
            return Ok(());
        };
        let t = &t;
        let dst = self.project.spatially_panned(t);
        let effective = self.project.effective_surround(t);
        let mode = if on {
            AutomationMode::Touch
        } else {
            AutomationMode::Read
        };
        let mut commands = Vec::new();
        let mut shown: Vec<AutomationLaneId> = Vec::new();
        for p in [SurroundParam::Azimuth, SurroundParam::Elevation] {
            if !dst.is_some_and(|d| p.applies_into(t.chain_layout(), d, &effective)) {
                continue;
            }
            let target = AutomationTarget::Surround(p);
            match t.automation.lane(target) {
                Some(l) => {
                    shown.push(l.id);
                    if l.mode != mode {
                        commands.push(Command::SetAutomationLane {
                            track,
                            lane: Box::new(AutomationLane { mode, ..l.clone() }),
                        });
                    }
                }
                None if on => {
                    let id: AutomationLaneId = self.project.ids.allocate();
                    shown.push(id);
                    commands.push(Command::AddAutomationLane {
                        track,
                        lane: Box::new(AutomationLane {
                            id,
                            target,
                            curve: AutomationCurve::new(),
                            mode,
                            visible: true,
                        }),
                    });
                }
                None => {}
            }
        }
        let label = if on {
            "Write Pan Automation"
        } else {
            "Read Pan Automation"
        };
        if !commands.is_empty() {
            self.edit(Command::Batch {
                label: label.into(),
                commands,
            })?;
        }
        if on {
            self.workspace.automation_shown.extend(shown);
        }
        self.revision += 1;
        Ok(())
    }
}
