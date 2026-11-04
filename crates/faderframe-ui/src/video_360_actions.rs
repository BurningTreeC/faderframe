//! Development aid for 360° video (`FADERFRAME_STARTUP_ACTIONS`):
//! `video-360:<what>` with `what` one of
//!
//! * `look=<yaw>/<pitch>[/<roll>]` — where the picture looks (degrees,
//!   yaw left, pitch up; linked: the listener's head too);
//! * `fov=<degrees>` — the field of view across the window;
//! * `ahead` — straight ahead again;
//! * `link=<on|off>` — the sound turns with the picture or not;
//! * `pan-track=<track|none>` — the track clicks on the picture pan;
//! * `pan=<track>@<azimuth>/<elevation>` — pan it there (one step);
//! * `write=<track>=<on|off>` — its direction lanes in Touch (made) or
//!   Read;
//! * `layout=<flat|equirect|cubemap>[/<mono|top-bottom|left-right|right-left>]`
//!   — mark the picture shown (else the first video) as 360° or flat;
//! * `status` — logged.

use crate::state::AppState;
use faderframe_core::spherical::{Spherical, StereoMode};
use faderframe_core::{TrackId, VideoSourceId};
use faderframe_session::Action;
use faderframe_session::sphere::SphereOp;
use faderframe_session::video::VideoOp;
use std::rc::Rc;

fn track_named(a: &Rc<AppState>, name: &str) -> Option<TrackId> {
    a.session
        .borrow()
        .project()
        .tracks
        .iter()
        .find(|t| t.name == name.trim())
        .map(|t| t.id)
}

fn numbers(text: &str) -> Vec<f32> {
    text.split('/')
        .filter_map(|v| v.trim().parse::<f32>().ok())
        .collect()
}

/// The video shown at the playhead, else the first.
fn shown_source(a: &Rc<AppState>) -> Option<VideoSourceId> {
    let s = a.session.borrow();
    s.video_picture(0, (16, 16))
        .map(|v| v.source)
        .or_else(|| s.project().video.sources.keys().next().copied())
}

/// A layout's words (`None`: not 360°).
pub(crate) fn parse_layout(text: &str) -> Result<Option<Spherical>, String> {
    let (kind, stereo) = text.split_once('/').unwrap_or((text, "mono"));
    let mut s = match kind.trim() {
        "flat" | "none" => return Ok(None),
        "equirect" | "equirectangular" => Spherical::EQUIRECTANGULAR,
        "cubemap" | "cube" => Spherical::CUBEMAP,
        other => return Err(format!("'{other}' is not flat|equirect|cubemap")),
    };
    s.stereo = match stereo.trim() {
        "mono" => StereoMode::Mono,
        "top-bottom" => StereoMode::TopBottom,
        "left-right" => StereoMode::LeftRight,
        "right-left" => StereoMode::RightLeft,
        other => {
            return Err(format!(
                "'{other}' is not mono|top-bottom|left-right|right-left"
            ));
        }
    };
    Ok(Some(s))
}

pub(crate) fn video_360(a: &Rc<AppState>, arg: &str) {
    let (what, value) = arg.split_once('=').unwrap_or((arg, ""));
    let op = match what.trim() {
        "look" => {
            let v = numbers(value);
            if v.is_empty() {
                return tracing::warn!("video-360: look='{value}' is not <yaw>/<pitch>[/<roll>]");
            }
            SphereOp::Look {
                yaw: v[0],
                pitch: v.get(1).copied().unwrap_or(0.0),
                roll: v.get(2).copied().unwrap_or(0.0),
            }
        }
        "fov" => match value.trim().parse::<f32>() {
            Ok(f) => SphereOp::Fov(f),
            Err(_) => return tracing::warn!("video-360: fov='{value}' is not degrees"),
        },
        "ahead" => SphereOp::Ahead,
        "link" => SphereOp::Link(value.trim() != "off"),
        "pan-track" => match value.trim() {
            "none" | "" => SphereOp::PanTrack(None),
            name => match track_named(a, name) {
                Some(t) => SphereOp::PanTrack(Some(t)),
                None => return tracing::warn!("video-360: no track '{name}'"),
            },
        },
        "pan" => {
            let Some((name, dir)) = value.rsplit_once('@') else {
                return tracing::warn!("video-360: pan='{value}' is not <track>@<az>/<el>");
            };
            let Some(track) = track_named(a, name) else {
                return tracing::warn!("video-360: no track '{name}'");
            };
            let v = numbers(dir);
            if v.is_empty() {
                return tracing::warn!("video-360: pan='{value}' is not <track>@<az>/<el>");
            }
            SphereOp::PanTo {
                track,
                azimuth: v[0],
                elevation: v.get(1).copied().unwrap_or(0.0),
            }
        }
        "write" => {
            let (name, on) = value.rsplit_once('=').unwrap_or((value, "on"));
            let Some(track) = track_named(a, name) else {
                return tracing::warn!("video-360: no track '{name}'");
            };
            SphereOp::WriteAutomation {
                track,
                on: on.trim() != "off",
            }
        }
        "layout" => {
            let Some(source) = shown_source(a) else {
                return tracing::warn!("video-360: there is no video");
            };
            match parse_layout(value) {
                Ok(spherical) => SphereOp::SetLayout { source, spherical },
                Err(e) => return tracing::warn!("video-360: layout: {e}"),
            }
        }
        "status" => {
            let s = a.session.borrow();
            let look = s.sphere_look();
            let (yaw, pitch, roll) = look.look.ypr();
            let layout = shown_source(a)
                .and_then(|src| s.video_spherical(src))
                .map_or_else(|| "flat".to_string(), |sp| sp.label());
            tracing::info!(
                "video-360: {layout}; looking yaw {yaw:.1}° pitch {pitch:.1}° roll {roll:.1}°, {:.0}° wide, {}; pan track {:?}; targets {:?}",
                look.fov,
                if look.linked {
                    "sound turns with it"
                } else {
                    "sound apart"
                },
                look.pan_track,
                s.sphere_pan_targets()
                    .iter()
                    .map(|t| format!("{} {:.0}°/{:.0}°", t.name, t.azimuth, t.elevation))
                    .collect::<Vec<_>>()
            );
            return;
        }
        other => {
            return tracing::warn!(
                "video-360: '{other}' is not look|fov|ahead|link|pan-track|pan|write|layout|status"
            );
        }
    };
    a.dispatch(Action::Video(VideoOp::Sphere(op)));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layouts_are_read_from_their_words() {
        assert_eq!(parse_layout("flat"), Ok(None));
        assert_eq!(
            parse_layout("equirect"),
            Ok(Some(Spherical::EQUIRECTANGULAR))
        );
        let s = parse_layout("cubemap/top-bottom").unwrap().unwrap();
        assert!(s.is_cubemap() && s.stereo == StereoMode::TopBottom);
        assert!(parse_layout("fisheye").is_err());
        assert_eq!(numbers("30/-5/x"), vec![30.0, -5.0]);
    }
}
