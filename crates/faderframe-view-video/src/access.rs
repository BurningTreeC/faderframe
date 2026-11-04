//! The video window for screen readers and the keyboard: the picture as
//! one control, read as what it shows (the timecode, the file and frame,
//! or why there is none; the A/V calibration's step), described with the
//! comparison, the ADR cue over it and the jobs running — Enter, Shift+F10
//! or the Menu key open the window's menu (import, picture offset, sync
//! test, calibration, following an object, compare, full screen). While
//! two pictures are wiped, the divider is a slider the arrows move by a
//! twentieth. A 360° picture has three sliders more: which way it looks
//! (the arrows turn it by ten degrees), how far up or down, and how wide.
//!
//! Said by themselves: a job (reading, proxies, writing) finishing, an
//! A/V calibration's result.

use crate::VideoView;
use crate::sphere::{KEY_TURN, tilt_text, turn_text};
use faderframe_core::spherical::{MAX_FOV, MIN_FOV};
use faderframe_session::av_calibration::CalibrationStatus;
use faderframe_session::sphere::SphereOp;
use faderframe_session::video::VideoCompare;
use faderframe_session::video::VideoOp;
use faderframe_session::{Action, Session};
use faderframe_ui_canvas::{
    AccessNode, AccessPress, AccessRole, EventCx, HostRequest, Point, Rect, Size, access_id,
};

const PICTURE: u64 = 0x71D0;
const WIPE: u64 = 0x71D1;
const HEADING: u64 = 0x71D2;
const TILT: u64 = 0x71D3;
const WIDTH: u64 = 0x71D4;
/// The wipe divider's step (a share of the width).
const WIPE_STEP: f32 = 0.05;

/// What screen readers were last told.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Told {
    jobs: Vec<String>,
    calibration: Option<CalibrationStatus>,
}

#[cfg(test)]
impl Told {
    /// As if `jobs` were running when last told.
    pub fn with_jobs(jobs: Vec<String>) -> Self {
        Self {
            jobs,
            calibration: None,
        }
    }
}

impl VideoView {
    /// The picture's description: comparison, ADR cue, jobs.
    fn picture_description(&self, model: &Session) -> String {
        let mut parts = Vec::new();
        let two = model.project().video.shown_tracks().count() >= 2;
        match model.video_compare() {
            VideoCompare::SideBySide if two => parts.push("Two tracks side by side".to_string()),
            VideoCompare::Wipe if two => parts.push("Two tracks wiped".to_string()),
            _ => {}
        }
        if let Some(o) = model.adr_overlay(model.transport().position) {
            let who = if o.character.is_empty() {
                format!("Cue {}", o.number)
            } else {
                format!("Cue {}, {}", o.number, o.character)
            };
            let mut cue = format!("{who}: {}", o.text);
            if o.recording {
                cue.push_str(", recording");
            }
            parts.push(cue);
        }
        for j in model.video_jobs() {
            parts.push(format!("{} {:.0} %", j.label, j.share * 100.0));
        }
        parts.push("Enter: the video menu".into());
        parts.join(". ")
    }

    pub(crate) fn access_nodes(&self, size: Size, model: &Session) -> Vec<AccessNode<Action>> {
        let label = if model.av_calibrating() {
            match model.av_calibration() {
                Some(CalibrationStatus::Measuring { flash, of }) => {
                    format!("A/V calibration, flash {flash} of {of}")
                }
                _ => "A/V calibration, opening the camera".into(),
            }
        } else {
            match &self.caption {
                Some(c) => format!("Picture {c}"),
                None => "No video".into(),
            }
        };
        let mut picture = AccessNode::new(access_id(&[PICTURE]), AccessRole::Region, label)
            .at(Rect::from_size(size))
            .described(self.picture_description(model))
            .with_menu();
        let two = model.project().video.shown_tracks().count() >= 2;
        if two && model.video_compare() == VideoCompare::Wipe {
            let x = self.wipe * size.w;
            picture = picture.child(
                AccessNode::new(
                    access_id(&[WIPE]),
                    AccessRole::Slider,
                    "Wipe between A and B",
                )
                .at(Rect::new(x - 4.0, 0.0, 8.0, size.h))
                .value(
                    f64::from(self.wipe) * 100.0,
                    0.0,
                    100.0,
                    format!("{:.0} % A", self.wipe * 100.0),
                )
                .view_steps(),
            );
        }
        if self.sphere_area().is_some() {
            let look = model.sphere_look();
            let (yaw, pitch, _) = look.look.ypr();
            let at = Rect::new(size.w / 2.0 - 8.0, size.h / 2.0 - 8.0, 16.0, 16.0);
            picture = picture
                .child(
                    AccessNode::new(
                        access_id(&[HEADING]),
                        AccessRole::Slider,
                        "Look left or right",
                    )
                    .at(at)
                    .value(f64::from(-yaw), -180.0, 180.0, turn_text(yaw))
                    .view_steps(),
                )
                .child(
                    AccessNode::new(access_id(&[TILT]), AccessRole::Slider, "Look up or down")
                        .at(at)
                        .value(f64::from(pitch), -90.0, 90.0, tilt_text(pitch))
                        .view_steps(),
                )
                .child(
                    AccessNode::new(access_id(&[WIDTH]), AccessRole::Slider, "Field of view")
                        .at(at)
                        .value(
                            f64::from(look.fov),
                            f64::from(MIN_FOV),
                            f64::from(MAX_FOV),
                            format!("{:.0}° wide", look.fov),
                        )
                        .view_steps(),
                );
        }
        vec![picture]
    }

    pub(crate) fn press(
        &mut self,
        id: u64,
        press: AccessPress,
        size: Size,
        model: &Session,
        cx: &mut EventCx<'_, Action>,
    ) -> bool {
        if id == access_id(&[PICTURE]) && matches!(press, AccessPress::Menu | AccessPress::Activate)
        {
            cx.request(HostRequest::ContextMenu {
                at: Point::new(size.w / 2.0, size.h / 2.0),
                items: self.menu(model),
            });
            return true;
        }
        if [HEADING, TILT, WIDTH]
            .iter()
            .any(|k| id == access_id(&[*k]))
        {
            let by = match press {
                AccessPress::Increment => 1.0,
                AccessPress::Decrement => -1.0,
                _ => return false,
            };
            let look = model.sphere_look();
            let (yaw, pitch, roll) = look.look.ypr();
            let op = if id == access_id(&[WIDTH]) {
                SphereOp::Fov((look.fov + by * KEY_TURN).clamp(MIN_FOV, MAX_FOV))
            } else if id == access_id(&[TILT]) {
                SphereOp::Look {
                    yaw,
                    pitch: (pitch + by * KEY_TURN).clamp(-90.0, 90.0),
                    roll,
                }
            } else {
                // The heading counts to the right.
                SphereOp::Look {
                    yaw: yaw - by * KEY_TURN,
                    pitch,
                    roll,
                }
            };
            cx.emit(Action::Video(VideoOp::Sphere(op)));
            cx.redraw();
            return true;
        }
        if id == access_id(&[WIPE]) {
            let by = match press {
                AccessPress::Increment => WIPE_STEP,
                AccessPress::Decrement => -WIPE_STEP,
                _ => return false,
            };
            self.wipe = (self.wipe + by).clamp(0.0, 1.0);
            cx.redraw();
            return true;
        }
        false
    }

    /// Jobs finishing and a calibration's result, as said.
    pub(crate) fn news(&mut self, model: &Session) -> Vec<String> {
        let now = Told {
            jobs: model.video_jobs().into_iter().map(|j| j.label).collect(),
            calibration: model
                .av_calibration()
                .filter(|c| matches!(c, CalibrationStatus::Done(_) | CalibrationStatus::Failed(_))),
        };
        let Some(before) = self.told.replace(now.clone()) else {
            return Vec::new();
        };
        let mut out: Vec<String> = before
            .jobs
            .iter()
            .filter(|j| !now.jobs.contains(j))
            .map(|j| format!("{j}: done"))
            .collect();
        if now.calibration != before.calibration {
            match &now.calibration {
                Some(CalibrationStatus::Done(r)) => out.push(format!(
                    "A/V calibration: the picture offset is {:+.1} ms (within {:.1} ms, {} of {} flashes)",
                    r.offset_ms, r.spread_ms, r.used, r.shown
                )),
                Some(CalibrationStatus::Failed(e)) => {
                    out.push(format!("A/V calibration failed: {e}"));
                }
                _ => {}
            }
        }
        out
    }
}
