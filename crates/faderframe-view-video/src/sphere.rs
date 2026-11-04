//! A 360° picture in the video window: a flat view into it where the
//! session looks ([`Session::sphere_look`]: linked, the listener's head),
//! drawn on the CPU at the window's size (at most [`MAX_PIXELS`], scaled
//! up beyond) and kept until the frame or the view changes.
//!
//! Dragging the picture turns the view (what was under the pointer stays
//! under it); the wheel narrows or widens it. With a track chosen to pan
//! ([`SphereOp::PanTrack`]) a click — or a drag — on the picture pans
//! that track to the direction under the pointer, one undo step a drag
//! (Alt+drag still looks round). The tracks panned by direction are
//! marked where they sound, the chosen one in the accent colour.

use crate::VideoView;
use faderframe_core::ambisonics::{angles, direction};
use faderframe_core::spherical::{
    Crop, MAX_FOV, MIN_FOV, Mapping, Projection, Spherical, StereoMode, Viewport, dragged,
};
use faderframe_core::{TrackId, VideoSourceId};
use faderframe_session::sphere::{SphereOp, SpherePanTarget};
use faderframe_session::video::{VideoOp, VideoShown};
use faderframe_session::{Action, Session};
use faderframe_ui_canvas::{Color, EventCx, MenuItem, Painter, Pixels, Point, Rect};
use faderframe_video::reproject::{Source, reproject};

/// The most pixels a view into a 360° picture is drawn at.
pub const MAX_PIXELS: f32 = 1_300_000.0;

/// Fields of view the menu offers (degrees).
pub const FOVS: [f32; 6] = [50.0, 65.0, 80.0, 90.0, 110.0, 130.0];

/// What the wheel does to the field of view per step.
const WHEEL_ZOOM: f32 = 1.1;

/// Degrees an arrow key turns the view.
pub(crate) const KEY_TURN: f32 = 10.0;

/// A view into a 360° frame, kept until the frame or the view changes.
#[derive(Default)]
pub(crate) struct SphereCache {
    key: u64,
    width: u32,
    height: u32,
    rgba: Vec<u8>,
}

/// A drag on a 360° picture.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum SphereDrag {
    /// Looking round: where it started and the view then.
    Look {
        from: Point,
        yaw: f32,
        pitch: f32,
        roll: f32,
    },
    /// Panning a track (a gesture).
    Pan { track: TrackId },
}

/// FNV-1a over words: a picture's key from what it shows.
fn key_of(words: &[u64]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for w in words {
        for b in w.to_le_bytes() {
            h ^= b as u64;
            h = h.wrapping_mul(0x0100_0000_01b3);
        }
    }
    h
}

/// The device pixels a view of `area` is drawn at.
pub(crate) fn render_size(area: Rect, scale: f32) -> (u32, u32) {
    let (w, h) = ((area.w * scale).max(1.0), (area.h * scale).max(1.0));
    let k = (MAX_PIXELS / (w * h)).sqrt().min(1.0);
    (
        ((w * k).round() as u32).max(1),
        ((h * k).round() as u32).max(1),
    )
}

/// The view `area` shows of the session's look (logical pixels).
pub(crate) fn viewport(model: &Session, area: Rect) -> Viewport {
    let look = model.sphere_look();
    Viewport::new(look.look, look.fov, area.w, area.h)
}

/// The scene direction (azimuth, elevation) under `pos` of the view at
/// `area`.
pub(crate) fn direction_under(model: &Session, area: Rect, pos: Point) -> (f32, f32) {
    angles(viewport(model, area).direction_at(pos.x - area.x, pos.y - area.y))
}

impl VideoView {
    /// Draw a 360° frame `f` of `v` into `area`, looked into as the
    /// session looks; whether the exact frame is still to come.
    pub(crate) fn draw_sphere(
        p: &mut dyn Painter,
        model: &Session,
        v: &VideoShown,
        spherical: &Spherical,
        area: Rect,
        scale: f32,
        cache: &mut SphereCache,
    ) -> bool {
        let Some(pic) = &v.picture else {
            return true;
        };
        let f = &pic.frame;
        if f.gpu.is_some() || f.rgba.is_empty() {
            // A frame in video memory (asked for before the picture was
            // marked 360°): the next comes through memory.
            return true;
        }
        let (w, h) = render_size(area, scale);
        let look = model.sphere_look();
        let q = look.look;
        let key = key_of(&[
            v.source.raw(),
            pic.number as u64,
            f.width as u64,
            u64::from(q.w.to_bits()) << 32 | u64::from(q.x.to_bits()),
            u64::from(q.y.to_bits()) << 32 | u64::from(q.z.to_bits()),
            u64::from(look.fov.to_bits()),
            (w as u64) << 32 | h as u64,
            spherical_key(spherical),
        ]);
        if cache.key != key || cache.width != w || cache.height != h {
            let mapping = Mapping::new(*spherical, f.width, f.height);
            let view = Viewport::new(look.look, look.fov, w as f32, h as f32);
            reproject(
                Source {
                    rgba: &f.rgba,
                    width: f.width,
                    height: f.height,
                },
                &mapping,
                &view,
                &mut cache.rgba,
            );
            cache.key = key;
            cache.width = w;
            cache.height = h;
        }
        p.pixels(
            &Pixels {
                key: cache.key,
                width: cache.width,
                height: cache.height,
                rgba: &cache.rgba,
            },
            area,
        );
        !pic.exact
    }

    /// The tracks panned by direction, marked where they sound, and the
    /// view's heading.
    pub(crate) fn paint_sphere_overlay(&self, p: &mut dyn Painter, model: &Session, area: Rect) {
        let theme = &self.theme;
        let view = viewport(model, area);
        let look = model.sphere_look();
        let chosen = look.pan_track;
        for t in model.sphere_pan_targets() {
            let Some((x, y)) = view.point_of(direction(t.azimuth, t.elevation)) else {
                continue;
            };
            let at = Point::new(area.x + x, area.y + y);
            let mine = chosen == Some(t.track);
            let colour = if mine {
                theme.ui.accent
            } else {
                theme.ui.selection
            };
            let r = if mine { 9.0 } else { 6.0 };
            p.stroke_rounded(
                Rect::new(at.x - r, at.y - r, 2.0 * r, 2.0 * r),
                r,
                if mine { 2.5 } else { 1.5 },
                colour,
            );
            p.fill(Rect::new(at.x - 1.0, at.y - 1.0, 2.0, 2.0), colour);
            let label = if t.writing {
                format!("{} ● writing", t.name)
            } else {
                t.name.clone()
            };
            Self::tag(p, &label, Point::new(at.x + r + 4.0, at.y - 9.0));
        }
        let (yaw, pitch, _) = look.look.ypr();
        let mut heading = format!(
            "360° · {} · {} · {:.0}° wide",
            turn_text(yaw),
            tilt_text(pitch),
            look.fov
        );
        if !look.linked {
            heading.push_str(" · sound not turned");
        }
        if let Some(t) = chosen.and_then(|t| model.project().track(t)) {
            heading.push_str(&format!(" · click pans {}", t.name));
        }
        Self::tag(p, &heading, Point::new(area.x + 8.0, area.y + 40.0));
        // The middle of the view.
        let (cx, cy) = (area.x + area.w / 2.0, area.y + area.h / 2.0);
        let mark = Color::rgba(1.0, 1.0, 1.0, 0.5);
        p.fill(Rect::new(cx - 6.0, cy - 0.5, 12.0, 1.0), mark);
        p.fill(Rect::new(cx - 0.5, cy - 6.0, 1.0, 12.0), mark);
    }

    /// The 360° part of the window's menu: the picture's layout, the view,
    /// turning the sound with it, panning by clicks.
    pub(crate) fn sphere_menu(
        model: &Session,
        shown: &Option<VideoShown>,
    ) -> Vec<MenuItem<Action>> {
        let Some(v) = shown else {
            return Vec::new();
        };
        let source = v.source;
        let now = model.video_spherical(source);
        let set = |s: Option<Spherical>| {
            sphere(SphereOp::SetLayout {
                source,
                spherical: s,
            })
        };
        let stereo = now.map_or(StereoMode::Mono, |s| s.stereo);
        let with = |projection: Projection, stereo: StereoMode| Spherical {
            projection,
            stereo,
            ..now.unwrap_or(Spherical::EQUIRECTANGULAR)
        };
        let equi = Projection::Equirectangular {
            crop: match now.map(|s| s.projection) {
                Some(Projection::Equirectangular { crop }) => crop,
                _ => Crop::default(),
            },
        };
        let cube = Projection::Cubemap {
            padding: match now.map(|s| s.projection) {
                Some(Projection::Cubemap { padding }) => padding,
                _ => 0.0,
            },
        };
        let mut layout = vec![
            MenuItem::new("Flat (Not 360°)", set(None)).checked(now.is_none()),
            MenuItem::new("Equirectangular", set(Some(with(equi, stereo))))
                .checked(now.is_some_and(|s| !s.is_cubemap())),
            MenuItem::new("Cubemap (3×2)", set(Some(with(cube, stereo))))
                .checked(now.is_some_and(|s| s.is_cubemap())),
        ];
        if let Some(s) = now {
            for (i, m) in [
                StereoMode::Mono,
                StereoMode::TopBottom,
                StereoMode::LeftRight,
                StereoMode::RightLeft,
            ]
            .into_iter()
            .enumerate()
            {
                let item = MenuItem::new(m.label(), set(Some(with(s.projection, m))))
                    .checked(s.stereo == m);
                layout.push(if i == 0 { item.separated() } else { item });
            }
        }
        let mut items = vec![MenuItem::submenu("360° Picture", layout).separated()];
        if now.is_none() {
            return items;
        }
        let look = model.sphere_look();
        items.push(MenuItem::new(
            "Look Straight Ahead",
            sphere(SphereOp::Ahead),
        ));
        items.push(MenuItem::submenu(
            format!("Field of View ({:.0}°)", look.fov),
            FOVS.iter()
                .map(|&f| {
                    MenuItem::new(format!("{f:.0}°"), sphere(SphereOp::Fov(f)))
                        .checked((look.fov - f).abs() < 0.5)
                })
                .collect(),
        ));
        items.push(
            MenuItem::new(
                "Turn the Sound with the Picture",
                sphere(SphereOp::Link(!look.linked)),
            )
            .checked(look.linked),
        );
        let targets = model.sphere_pan_targets();
        if targets.is_empty() {
            items.push(MenuItem::disabled(
                "Pan by Clicking (no track panned by direction)",
            ));
        } else {
            let mut pan = vec![
                MenuItem::new(
                    "Nobody (Clicks Look Round)",
                    sphere(SphereOp::PanTrack(None)),
                )
                .checked(look.pan_track.is_none()),
            ];
            for (i, t) in targets.iter().enumerate() {
                let item = MenuItem::new(
                    format!("{}{}", t.name, if t.field { "" } else { " (bed)" }),
                    sphere(SphereOp::PanTrack(Some(t.track))),
                )
                .checked(look.pan_track == Some(t.track));
                pan.push(if i == 0 { item.separated() } else { item });
            }
            items.push(MenuItem::submenu("Pan by Clicking", pan));
            if let Some(t) = targets.iter().find(|t| Some(t.track) == look.pan_track) {
                items.push(write_item(t));
            }
        }
        items
    }

    /// A 360° picture shown in the single view at `area`, if one is.
    pub(crate) fn sphere_area(&self) -> Option<Rect> {
        self.sphere_shown
    }

    /// A press on the 360° picture: a pan (a track chosen, no Alt) or a
    /// look round.
    pub(crate) fn sphere_press(
        &mut self,
        pos: Point,
        alt: bool,
        model: &Session,
        cx: &mut EventCx<'_, Action>,
    ) {
        let Some(area) = self.sphere_shown else {
            return;
        };
        let look = model.sphere_look();
        match look.pan_track {
            Some(track) if !alt => {
                cx.emit(Action::BeginGesture("Pan on the Picture".into()));
                let (azimuth, elevation) = direction_under(model, area, pos);
                cx.emit(sphere(SphereOp::PanTo {
                    track,
                    azimuth,
                    elevation,
                }));
                self.sphere_drag = Some(SphereDrag::Pan { track });
            }
            _ => {
                let (yaw, pitch, roll) = look.look.ypr();
                self.sphere_drag = Some(SphereDrag::Look {
                    from: pos,
                    yaw,
                    pitch,
                    roll,
                });
            }
        }
        cx.redraw();
    }

    /// The pointer moved with the button held.
    pub(crate) fn sphere_move(
        &mut self,
        pos: Point,
        model: &Session,
        cx: &mut EventCx<'_, Action>,
    ) {
        let (Some(drag), Some(area)) = (self.sphere_drag, self.sphere_shown) else {
            return;
        };
        match drag {
            SphereDrag::Look {
                from,
                yaw,
                pitch,
                roll,
            } => {
                let view = viewport(model, area);
                let (yaw, pitch) = dragged(yaw, pitch, pos.x - from.x, pos.y - from.y, &view);
                cx.emit(sphere(SphereOp::Look { yaw, pitch, roll }));
            }
            SphereDrag::Pan { track } => {
                let (azimuth, elevation) = direction_under(model, area, pos);
                cx.emit(sphere(SphereOp::PanTo {
                    track,
                    azimuth,
                    elevation,
                }));
            }
        }
        cx.redraw();
    }

    /// The button let go.
    pub(crate) fn sphere_release(&mut self, cx: &mut EventCx<'_, Action>) {
        if let Some(SphereDrag::Pan { .. }) = self.sphere_drag.take() {
            cx.emit(Action::EndGesture);
        }
        cx.redraw();
    }

    /// The wheel over the picture: narrower (away) or wider (towards).
    pub(crate) fn sphere_zoom(&self, dy: f32, model: &Session, cx: &mut EventCx<'_, Action>) {
        let fov = model.sphere_look().fov * WHEEL_ZOOM.powf(dy.clamp(-10.0, 10.0));
        cx.emit(sphere(SphereOp::Fov(fov.clamp(MIN_FOV, MAX_FOV))));
    }
}

fn sphere(op: SphereOp) -> Action {
    Action::Video(VideoOp::Sphere(op))
}

/// The pan track's automation toggle.
fn write_item(t: &SpherePanTarget) -> MenuItem<Action> {
    MenuItem::new(
        format!("Write {}'s Pan Automation (Touch)", t.name),
        sphere(SphereOp::WriteAutomation {
            track: t.track,
            on: !t.writing,
        }),
    )
    .checked(t.writing)
}

/// A layout's words for a picture's key.
fn spherical_key(s: &Spherical) -> u64 {
    let (kind, a, b) = match s.projection {
        Projection::Equirectangular { crop } => (
            1u64,
            (crop.top.to_bits() ^ crop.left.to_bits().rotate_left(17)),
            (crop.bottom.to_bits() ^ crop.right.to_bits().rotate_left(29)),
        ),
        Projection::Cubemap { padding } => (2, u64::from(padding.to_bits()), 0),
    };
    let pose = u64::from(s.pose.yaw.to_bits())
        ^ u64::from(s.pose.pitch.to_bits()).rotate_left(21)
        ^ u64::from(s.pose.roll.to_bits()).rotate_left(42);
    key_of(&[kind, a, b, s.stereo as u64, pose])
}

/// "ahead", "30° left", "45° right", "behind".
pub(crate) fn turn_text(yaw: f32) -> String {
    let y = yaw.round();
    if y.abs() < 1.0 {
        "ahead".into()
    } else if y.abs() >= 179.0 {
        "behind".into()
    } else if y > 0.0 {
        format!("{y:.0}° left")
    } else {
        format!("{:.0}° right", -y)
    }
}

/// "level", "20° up", "35° down".
pub(crate) fn tilt_text(pitch: f32) -> String {
    let p = pitch.round();
    if p.abs() < 1.0 {
        "level".into()
    } else if p > 0.0 {
        format!("{p:.0}° up")
    } else {
        format!("{:.0}° down", -p)
    }
}

/// The source of the picture shown, when it is 360°.
pub(crate) fn spherical_of(
    model: &Session,
    shown: &Option<VideoShown>,
) -> Option<(VideoSourceId, Spherical)> {
    let v = shown.as_ref()?;
    Some((v.source, model.video_spherical(v.source)?))
}

#[cfg(test)]
mod tests {
    use super::*;
    use faderframe_core::ambisonics::Orientation;
    use faderframe_core::{AmbiOrder, ChannelLayout, VideoClipId};
    use faderframe_engine::EngineConfig;
    use faderframe_project::video::{Video, VideoSource};
    use faderframe_project::{Command, Project, TrackKind};
    use faderframe_ui_canvas::{
        CanvasView, HostRequest, Modifiers, PointerButton, RecordingPainter, Size, Theme, ViewEvent,
    };
    use faderframe_video::{Frame, Picture};
    use std::sync::Arc;

    const SIZE: Size = Size { w: 640.0, h: 360.0 };

    /// A session with an ambisonic master, a mono track into it and a
    /// video source marked 360° (no file: what the view reads of it).
    fn session() -> (Session, TrackId, VideoSourceId) {
        let mut s =
            Session::new(Project::new("360", 48_000), None, EngineConfig::default()).unwrap();
        let m = s.project().master_id().unwrap();
        s.dispatch(Action::Edit(Command::SetTrackLayout {
            track: m,
            layout: ChannelLayout::Ambisonic(AmbiOrder::First),
        }))
        .unwrap();
        let t = s.add_track(TrackKind::Audio).unwrap();
        s.dispatch(Action::Edit(Command::SetTrackLayout {
            track: t,
            layout: ChannelLayout::Mono,
        }))
        .unwrap();
        let source = VideoSourceId(900);
        let mut video = Video::default();
        video.sources.insert(
            source,
            VideoSource {
                path: "/nowhere/360.mov".into(),
                width: 3840,
                height: 1920,
                par: (1, 1),
                fps: (30, 1),
                codec: "H.264".into(),
                duration: 1_000_000_000,
                timecode: None,
                sound: Vec::new(),
                spherical: Some(Spherical::EQUIRECTANGULAR),
            },
        );
        s.dispatch(Action::Edit(Command::SetVideo {
            video: Box::new(video),
        }))
        .unwrap();
        (s, t, source)
    }

    fn shown(source: VideoSourceId, picture: Option<Picture>) -> Option<VideoShown> {
        Some(VideoShown {
            picture,
            source,
            clip: VideoClipId(901),
            frame: 3,
            file_time: 100_000_000,
            position: 4800,
        })
    }

    fn send(
        view: &mut VideoView,
        ev: ViewEvent,
        s: &Session,
    ) -> (Vec<Action>, Vec<HostRequest<Action>>) {
        let mut actions = Vec::new();
        let mut requests = Vec::new();
        let mut cx = EventCx::new(&mut actions, &mut requests);
        view.event(&ev, SIZE, s, &mut cx);
        (actions, requests)
    }

    fn down(pos: Point, modifiers: Modifiers) -> ViewEvent {
        ViewEvent::PointerDown {
            pos,
            button: PointerButton::Primary,
            modifiers,
            clicks: 1,
        }
    }

    fn drag(pos: Point) -> ViewEvent {
        ViewEvent::PointerMove {
            pos,
            modifiers: Modifiers::NONE,
            dragging: true,
        }
    }

    fn up(pos: Point) -> ViewEvent {
        ViewEvent::PointerUp {
            pos,
            button: PointerButton::Primary,
            modifiers: Modifiers::NONE,
        }
    }

    /// Dragging the picture right looks left by the degrees crossed; the
    /// wheel narrows the view.
    #[test]
    fn dragging_a_360_picture_looks_round() {
        let (s, _, _) = session();
        let mut view = VideoView::new(Theme::default());
        view.sphere_shown = Some(Rect::from_size(SIZE));
        let (a, _) = send(
            &mut view,
            down(Point::new(320.0, 180.0), Modifiers::NONE),
            &s,
        );
        assert!(a.is_empty());
        let (a, _) = send(&mut view, drag(Point::new(420.0, 180.0)), &s);
        let [Action::Video(VideoOp::Sphere(SphereOp::Look { yaw, pitch, roll }))] = a.as_slice()
        else {
            panic!("{a:?}")
        };
        let k = viewport(&s, Rect::from_size(SIZE)).degrees_per_pixel();
        assert!((yaw - 100.0 * k).abs() < 1e-3 && *pitch == 0.0 && *roll == 0.0);
        let (a, _) = send(&mut view, up(Point::new(420.0, 180.0)), &s);
        assert!(a.is_empty(), "looking is no gesture");
        assert!(view.sphere_drag.is_none());
        let (a, _) = send(
            &mut view,
            ViewEvent::Scroll {
                pos: Point::new(100.0, 100.0),
                dx: 0.0,
                dy: 1.0,
                modifiers: Modifiers::NONE,
                precise: false,
            },
            &s,
        );
        let [Action::Video(VideoOp::Sphere(SphereOp::Fov(f)))] = a.as_slice() else {
            panic!("{a:?}")
        };
        assert!((f - 99.0).abs() < 0.01);
        // A flat picture: none of it.
        view.sphere_shown = None;
        let (a, _) = send(
            &mut view,
            down(Point::new(320.0, 180.0), Modifiers::NONE),
            &s,
        );
        assert!(a.is_empty() && view.sphere_drag.is_none());
    }

    /// With a track chosen, a press pans it to the direction under the
    /// pointer and a drag carries it on, one gesture; Alt still looks.
    #[test]
    fn a_click_on_a_360_picture_pans_the_chosen_track() {
        let (mut s, t, _) = session();
        s.dispatch(Action::Video(VideoOp::Sphere(SphereOp::PanTrack(Some(t)))))
            .unwrap();
        let mut view = VideoView::new(Theme::default());
        view.sphere_shown = Some(Rect::from_size(SIZE));
        let (a, _) = send(
            &mut view,
            down(Point::new(320.0, 180.0), Modifiers::NONE),
            &s,
        );
        assert!(matches!(a[0], Action::BeginGesture(_)));
        let Action::Video(VideoOp::Sphere(SphereOp::PanTo {
            track,
            azimuth,
            elevation,
        })) = &a[1]
        else {
            panic!("{a:?}")
        };
        assert_eq!(*track, t);
        assert!(azimuth.abs() < 0.2 && elevation.abs() < 0.2);
        // The right edge of a 90° view: 45° to the right.
        let (a, _) = send(&mut view, drag(Point::new(640.0, 180.0)), &s);
        let [Action::Video(VideoOp::Sphere(SphereOp::PanTo { azimuth, .. }))] = a.as_slice() else {
            panic!("{a:?}")
        };
        assert!((azimuth + 45.0).abs() < 0.2, "{azimuth}");
        let (a, _) = send(&mut view, up(Point::new(640.0, 180.0)), &s);
        assert_eq!(a, vec![Action::EndGesture]);
        // Alt: looking round still.
        let alt = Modifiers {
            alt: true,
            ..Modifiers::NONE
        };
        let (a, _) = send(&mut view, down(Point::new(320.0, 180.0), alt), &s);
        assert!(a.is_empty());
        assert!(matches!(view.sphere_drag, Some(SphereDrag::Look { .. })));
    }

    /// The menu marks the picture's layout, and for a 360° one offers the
    /// view, turning the sound and panning by clicks.
    #[test]
    fn the_menu_marks_the_layout_and_offers_panning() {
        let (s, t, source) = session();
        let items = VideoView::sphere_menu(&s, &shown(source, None));
        let flat = flat_items(&items);
        let find = |label: &str| {
            flat.iter()
                .find(|(l, _, _)| l == label)
                .unwrap_or_else(|| panic!("no {label} in {flat:?}"))
                .clone()
        };
        assert!(find("Equirectangular").2);
        assert!(!find("Flat (Not 360°)").2);
        assert_eq!(
            find("Flat (Not 360°)").1,
            Some(sphere(SphereOp::SetLayout {
                source,
                spherical: None
            }))
        );
        assert_eq!(
            find("Stereo Top-Bottom").1,
            Some(sphere(SphereOp::SetLayout {
                source,
                spherical: Some(Spherical {
                    stereo: StereoMode::TopBottom,
                    ..Spherical::EQUIRECTANGULAR
                })
            }))
        );
        assert!(find("Turn the Sound with the Picture").2);
        assert!(find("90°").2);
        let name = s.project().track(t).unwrap().name.clone();
        assert_eq!(find(&name).1, Some(sphere(SphereOp::PanTrack(Some(t)))));
        // Not chosen yet: no automation item.
        assert!(!flat.iter().any(|(l, _, _)| l.starts_with("Write ")));
        // A flat picture: only its layout to choose.
        let mut s = s;
        s.dispatch(sphere(SphereOp::SetLayout {
            source,
            spherical: None,
        }))
        .unwrap();
        let items = flat_items(&VideoView::sphere_menu(&s, &shown(source, None)));
        assert!(items.iter().any(|(l, _, on)| l == "Flat (Not 360°)" && *on));
        assert!(!items.iter().any(|(l, _, _)| l == "Look Straight Ahead"));
    }

    fn flat_items(items: &[MenuItem<Action>]) -> Vec<(String, Option<Action>, bool)> {
        let mut out = Vec::new();
        for i in items {
            out.push((i.label.clone(), i.action.clone(), i.checked == Some(true)));
            out.extend(flat_items(&i.children));
        }
        out
    }

    /// A frame is looked into where the session looks (the compass's
    /// green to the left), kept while nothing changes, and the tracks
    /// panned by direction are marked on it.
    #[test]
    fn a_360_frame_is_drawn_as_a_view_into_it() {
        let (mut s, t, source) = session();
        let (w, h) = (360u32, 180u32);
        let m = Mapping::new(Spherical::EQUIRECTANGULAR, w, h);
        let mut rgba = Vec::new();
        for y in 0..h {
            for x in 0..w {
                let (az, _) = angles(m.direction_at(x as f32 + 0.5, y as f32 + 0.5).unwrap());
                rgba.extend_from_slice(if az > 45.0 && az < 135.0 {
                    &[0, 255, 0, 255]
                } else {
                    &[255, 0, 0, 255]
                });
            }
        }
        let picture = Picture {
            frame: Arc::new(Frame {
                width: w,
                height: h,
                time: 0,
                rgba,
                gpu: None,
            }),
            number: 3,
            exact: true,
        };
        s.dispatch(Action::Video(VideoOp::Sphere(SphereOp::Look {
            yaw: 90.0,
            pitch: 0.0,
            roll: 0.0,
        })))
        .unwrap();
        let v = shown(source, Some(picture)).unwrap();
        let mut cache = SphereCache::default();
        let mut p = RecordingPainter::new();
        let area = Rect::from_size(SIZE);
        let waiting = VideoView::draw_sphere(
            &mut p,
            &s,
            &v,
            &Spherical::EQUIRECTANGULAR,
            area,
            1.0,
            &mut cache,
        );
        assert!(!waiting);
        assert_eq!((cache.width, cache.height), (640, 360));
        let mid = ((180 * 640 + 320) * 4) as usize;
        assert_eq!(&cache.rgba[mid..mid + 4], &[0, 255, 0, 255]);
        let key = cache.key;
        VideoView::draw_sphere(
            &mut p,
            &s,
            &v,
            &Spherical::EQUIRECTANGULAR,
            area,
            1.0,
            &mut cache,
        );
        assert_eq!(cache.key, key, "kept");
        // Turned: drawn again.
        s.dispatch(Action::Video(VideoOp::Sphere(SphereOp::Ahead)))
            .unwrap();
        VideoView::draw_sphere(
            &mut p,
            &s,
            &v,
            &Spherical::EQUIRECTANGULAR,
            area,
            2.0,
            &mut cache,
        );
        assert_ne!(cache.key, key);
        assert_eq!(
            &cache.rgba[cache.rgba.len() / 2 + 4 * 640..][..4],
            &[255, 0, 0, 255]
        );
        // A large window at a HiDPI scale stays within the budget.
        let (bw, bh) = render_size(Rect::new(0.0, 0.0, 2560.0, 1440.0), 2.0);
        assert!((bw * bh) as f32 <= MAX_PIXELS * 1.01 && bw > bh);
        // The panned track is marked, the chosen one named with the
        // heading.
        s.dispatch(Action::Video(VideoOp::Sphere(SphereOp::PanTrack(Some(t)))))
            .unwrap();
        let view = VideoView::new(Theme::default());
        let mut p = RecordingPainter::new();
        view.paint_sphere_overlay(&mut p, &s, area);
        let name = s.project().track(t).unwrap().name.clone();
        let texts = p.texts();
        assert!(texts.contains(&name.as_str()), "{texts:?}");
        assert!(
            texts
                .iter()
                .any(|t| t.starts_with("360° · ahead · level · 90° wide")
                    && t.contains("click pans")),
            "{texts:?}"
        );
        let _ = Orientation::IDENTITY;
    }

    /// The keyboard: three sliders turn, tilt and widen the view.
    #[test]
    fn the_keyboard_turns_a_360_picture() {
        use faderframe_ui_canvas::access::{focus_order, press_key};
        use faderframe_ui_canvas::{AccessRole, Key};
        let (s, _, _) = session();
        let mut view = VideoView::new(Theme::default());
        view.sphere_shown = Some(Rect::from_size(SIZE));
        let nodes = view.accessible(SIZE, &s);
        let order = focus_order(&nodes);
        let heading = order
            .iter()
            .find(|n| n.label == "Look left or right")
            .cloned()
            .unwrap();
        assert_eq!(heading.role, AccessRole::Slider);
        let (actions, _) = press_key(
            &mut view,
            &nodes,
            heading.id,
            Key::Right,
            Modifiers::NONE,
            SIZE,
            &s,
        )
        .unwrap();
        let [Action::Video(VideoOp::Sphere(SphereOp::Look { yaw, .. }))] = actions.as_slice()
        else {
            panic!("{actions:?}")
        };
        assert!((yaw + KEY_TURN).abs() < 1e-3, "right turns right: {yaw}");
        let width = order
            .iter()
            .find(|n| n.label == "Field of view")
            .cloned()
            .unwrap();
        let (actions, _) = press_key(
            &mut view,
            &nodes,
            width.id,
            Key::Up,
            Modifiers::NONE,
            SIZE,
            &s,
        )
        .unwrap();
        assert_eq!(actions, vec![sphere(SphereOp::Fov(90.0 + KEY_TURN))]);
        assert!(order.iter().any(|n| n.label == "Look up or down"));
        assert_eq!(turn_text(-30.0), "30° right");
        assert_eq!(tilt_text(12.4), "12° up");
    }
}
