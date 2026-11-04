//! The video window: the picture for the sample heard when the frame being
//! drawn reaches the screen ([`Session::video_picture`]), letterboxed on
//! black, with a timecode overlay (project timecode, frame, the file). Its
//! menu imports video, moves the picture against the sound (by frames or
//! milliseconds), makes the flash-and-beep sync test, calibrates the
//! picture offset with a camera, follows an object for a track's pan,
//! spots a clip to its timecode, writes the movie and goes full screen (a
//! double-click too).
//!
//! While A/V calibration runs the window is the test's screen: white for
//! each flash, dark between ([`Session::av_calibration_screen`], timed as
//! the picture is). Following an object: a box drawn on the picture
//! (Shift+drag, or a drag with the box tool on) is a key
//! ([`FollowOp::Key`]); the box followed is drawn over the picture.
//! Screen readers and the keyboard: see [`access`].

#![forbid(unsafe_code)]

mod access;
mod sphere;

use faderframe_session::av_calibration::{
    CalibrationSettings, CalibrationStatus, CameraChoice, DEFAULT_FLASHES,
};
use faderframe_session::object_tracking::{DEFAULT_DEPTH, FollowOp, FollowTarget, Seen};
use faderframe_session::video::{VideoCompare, VideoOp, VideoShown};
use faderframe_session::{Action, Session};
use faderframe_ui_canvas::{
    AccessNode, AccessPress, Align, CanvasView, Color, EventCx, External, FontFamily, FontWeight,
    HostRequest, MenuItem, Painter, Pixels, Point, PointerButton, Rect, Size, TextStyle, Theme,
    ViewEvent,
};
use faderframe_video::track::Area;
use faderframe_workspace::ViewId;

/// Files offered for import.
pub const VIDEO_PATTERNS: [&str; 12] = [
    "*.mov", "*.mp4", "*.m4v", "*.mkv", "*.webm", "*.avi", "*.mxf", "*.mts", "*.m2ts", "*.mpg",
    "*.MOV", "*.MP4",
];

pub struct VideoView {
    theme: Theme,
    /// When the frame being drawn reaches the screen (ns from now).
    lead: i64,
    /// Device pixels per logical pixel (pictures are decoded at device
    /// size).
    scale: f32,
    overlay: bool,
    /// Waiting for the exact frame (keep drawing).
    waiting: bool,
    /// Where the wipe divides A from B (share of the width), and whether
    /// it is being dragged.
    wipe: f32,
    wiping: bool,
    /// The picture last drawn (single view): where, of which clip and
    /// frame — what a box is drawn on.
    shown: Option<(Rect, faderframe_core::VideoClipId, usize)>,
    /// A box being drawn (from, to).
    boxing: Option<(Point, Point)>,
    /// What the picture last drawn shows, as read (timecode, file, frame).
    caption: Option<String>,
    /// What screen readers were last told (jobs, calibration).
    told: Option<access::Told>,
    /// Views into 360° frames (A, B), kept until the frame or view changes.
    sphere_cache: [sphere::SphereCache; 2],
    /// Where a 360° picture was last drawn in the single view.
    sphere_shown: Option<Rect>,
    /// Looking round or panning on it.
    sphere_drag: Option<sphere::SphereDrag>,
}

impl VideoView {
    pub fn new(theme: Theme) -> Self {
        Self {
            theme,
            lead: 16_666_667,
            scale: 1.0,
            overlay: true,
            waiting: false,
            wipe: 0.5,
            wiping: false,
            shown: None,
            boxing: None,
            caption: None,
            told: None,
            sphere_cache: Default::default(),
            sphere_shown: None,
            sphere_drag: None,
        }
    }

    /// `area` (shares of the picture) on the picture drawn at `r`.
    pub fn on_picture(area: Area, r: Rect) -> Rect {
        Rect::new(
            r.x + area.x * r.w,
            r.y + area.y * r.h,
            area.w * r.w,
            area.h * r.h,
        )
    }

    /// The box from `a` to `b` as shares of the picture at `r` (`None`:
    /// too small, or off the picture).
    pub fn area_of(a: Point, b: Point, r: Rect) -> Option<Area> {
        if r.w <= 0.0 || r.h <= 0.0 {
            return None;
        }
        let x0 = ((a.x.min(b.x) - r.x) / r.w).clamp(0.0, 1.0);
        let x1 = ((a.x.max(b.x) - r.x) / r.w).clamp(0.0, 1.0);
        let y0 = ((a.y.min(b.y) - r.y) / r.h).clamp(0.0, 1.0);
        let y1 = ((a.y.max(b.y) - r.y) / r.h).clamp(0.0, 1.0);
        ((x1 - x0) * r.w >= 6.0 && (y1 - y0) * r.h >= 6.0)
            .then(|| Area::new(x0, y0, x1 - x0, y1 - y0))
    }

    /// The A/V calibration submenu.
    fn calibration_menu(model: &Session) -> MenuItem<Action> {
        let start = |camera: CameraChoice| {
            Action::Video(VideoOp::Calibrate(CalibrationSettings {
                camera,
                input: 0,
                flashes: DEFAULT_FLASHES,
            }))
        };
        let mut items = Vec::new();
        if model.av_calibrating() {
            items.push(MenuItem::new(
                "Stop Calibrating",
                Action::Video(VideoOp::CancelCalibration),
            ));
        } else {
            let (cameras, searched) = model.av_cameras();
            for c in cameras {
                items.push(MenuItem::new(
                    format!("With {}", c.label()),
                    start(CameraChoice::Named(c.label())),
                ));
            }
            if cameras.is_empty() {
                items.push(MenuItem::new(
                    "With the First Camera",
                    start(CameraChoice::Default),
                ));
            }
            items.push(
                MenuItem::new(
                    if searched {
                        "Look for Cameras Again"
                    } else {
                        "Look for Cameras"
                    },
                    Action::Video(VideoOp::FindCameras),
                )
                .separated(),
            );
            items.push(MenuItem::new(
                "Test Without a Camera",
                start(CameraChoice::Virtual {
                    display_ms: 0.0,
                    readout: 0.0,
                }),
            ));
        }
        match model.av_calibration() {
            Some(CalibrationStatus::Done(r)) => items.push(
                MenuItem::disabled(format!(
                    "Last: {:+.1} ms (±{:.1} ms, {} of {} flashes)",
                    r.offset_ms, r.spread_ms, r.used, r.shown
                ))
                .separated(),
            ),
            Some(CalibrationStatus::Failed(e)) => {
                items.push(MenuItem::disabled(format!("Last: {e}")).separated());
            }
            _ => {}
        }
        MenuItem::submenu("Calibrate with a Camera", items)
    }

    /// The menu of following an object (about the frame shown).
    fn follow_menu(model: &Session, shown: &Option<VideoShown>) -> Vec<MenuItem<Action>> {
        let drawing = model.follow_drawing();
        let mut items = vec![
            MenuItem::new(
                "Follow an Object (Draw a Box Around It)",
                Action::Video(VideoOp::Follow(FollowOp::Draw(!drawing))),
            )
            .checked(drawing)
            .separated(),
        ];
        let Some(status) = model.follow_status() else {
            return items;
        };
        if let Some(v) = shown
            && v.clip == status.clip
            && status.keys.contains(&v.frame)
            && status.keys.len() > 1
        {
            items.push(MenuItem::new(
                "Take Back the Box on This Frame",
                Action::Video(VideoOp::Follow(FollowOp::RemoveKey { frame: v.frame })),
            ));
        }
        let mut apply = Vec::new();
        for (track, name, target) in model.follow_tracks() {
            let with = |depth| Action::Video(VideoOp::Follow(FollowOp::Apply { track, depth }));
            apply.push(MenuItem::new(
                format!("{name} — {}", target.label()),
                with(None),
            ));
            if target == FollowTarget::Room {
                apply.push(MenuItem::new(
                    format!("{name} — {} and Depth", target.label()),
                    with(Some(DEFAULT_DEPTH)),
                ));
            }
        }
        if apply.is_empty() {
            items.push(MenuItem::disabled("Pan a Track with It (no audio tracks)"));
        } else {
            items.push(MenuItem::submenu("Pan a Track with It", apply));
        }
        items.push(MenuItem::new(
            "Forget the Object",
            Action::Video(VideoOp::Follow(FollowOp::Forget)),
        ));
        items
    }

    /// The calibration's screen: white for a flash, dark between, and
    /// what is going on along the bottom.
    fn paint_calibration(p: &mut dyn Painter, size: Size, lit: bool, model: &Session) {
        let area = Rect::from_size(size);
        // The test's light itself: pure white and near black, whatever the
        // skin (what the camera measures).
        p.fill(
            area,
            if lit {
                Color::rgb(1.0, 1.0, 1.0)
            } else {
                Color::rgb(0.02, 0.02, 0.02)
            },
        );
        let text = match model.av_calibration() {
            Some(CalibrationStatus::Opening) => "A/V calibration — opening the camera…".to_string(),
            Some(CalibrationStatus::Measuring { flash, of }) => {
                format!("A/V calibration — flash {flash} of {of} · keep the camera on this window")
            }
            _ => "A/V calibration".to_string(),
        };
        let colour = if lit {
            Color::rgb(0.75, 0.75, 0.75)
        } else {
            Color::rgb(0.35, 0.35, 0.35)
        };
        p.text(
            &text,
            Rect::new(0.0, size.h - 26.0, size.w, 18.0),
            &TextStyle::new(11.0, colour).align(Align::Center),
        );
    }

    /// The object's box on the picture at `r`, and the one being drawn.
    fn paint_follow(&self, p: &mut dyn Painter, model: &Session, v: &VideoShown, r: Rect) {
        let theme = &self.theme;
        if let Some((area, seen)) = model.followed_box(v.clip, v.frame) {
            let b = Self::on_picture(area, r);
            let (colour, width) = match seen {
                Seen::Key => (theme.ui.accent, 2.5),
                Seen::Followed => (theme.ui.selection, 2.0),
                Seen::Lost => (theme.console.meter.red, 2.0),
            };
            p.stroke_rounded(b, 3.0, width, colour);
            let (cx, cy) = (b.x + b.w / 2.0, b.y + b.h / 2.0);
            p.fill(Rect::new(cx - 4.0, cy - 0.75, 8.0, 1.5), colour);
            p.fill(Rect::new(cx - 0.75, cy - 4.0, 1.5, 8.0), colour);
        }
        if let Some(status) = model.follow_status().filter(|s| s.clip == v.clip) {
            let text = if status.busy {
                "Following the object…".to_string()
            } else if status.lost > 0 {
                format!(
                    "Object followed · {} frames lost: draw the box again there",
                    status.lost
                )
            } else {
                format!("Object followed · {} frames", status.followed)
            };
            Self::tag(p, &text, Point::new(8.0, 40.0));
        }
    }

    /// Where a picture of `w`×`h` goes in `size` (aspect kept, centred).
    pub fn fit(w: f32, h: f32, size: Size) -> Rect {
        if w <= 0.0 || h <= 0.0 {
            return Rect::new(0.0, 0.0, 0.0, 0.0);
        }
        let s = (size.w / w).min(size.h / h);
        let (dw, dh) = (w * s, h * s);
        Rect::new((size.w - dw) / 2.0, (size.h - dh) / 2.0, dw, dh)
    }

    fn menu(&self, model: &Session) -> Vec<MenuItem<Action>> {
        let frame_ms = 1000.0 / model.timecode().rate.fps();
        let offset = model.project().video.offset_ms;
        let set = |ms: f64| Action::Video(VideoOp::SetOffset((ms * 10.0).round() / 10.0));
        let shown = model.video_picture(0, (16, 16));
        let mut items = vec![
            MenuItem::new("Import Video…", Action::Video(VideoOp::ChooseImport)),
            MenuItem::new("Export Movie…", Action::Video(VideoOp::ChooseExport)),
            MenuItem::submenu(
                format!("Picture Offset ({offset:+.1} ms)"),
                vec![
                    MenuItem::new("Earlier by a Frame", set(offset - frame_ms)),
                    MenuItem::new("Later by a Frame", set(offset + frame_ms)),
                    MenuItem::new("Earlier by 5 ms", set(offset - 5.0)),
                    MenuItem::new("Later by 5 ms", set(offset + 5.0)),
                    MenuItem::new("Earlier by 1 ms", set(offset - 1.0)),
                    MenuItem::new("Later by 1 ms", set(offset + 1.0)),
                    MenuItem::new("No Offset", set(0.0)).separated(),
                ],
            )
            .separated(),
            MenuItem::new("Flash-and-Beep Sync Test", Action::Video(VideoOp::SyncTest)),
            Self::calibration_menu(model),
        ];
        items.extend(Self::sphere_menu(model, &shown));
        if sphere::spherical_of(model, &shown).is_none() {
            items.extend(Self::follow_menu(model, &shown));
        }
        let two = model.project().video.shown_tracks().count() >= 2;
        let now = model.video_compare();
        let compare = |label: &str, c: VideoCompare| {
            if two || c == VideoCompare::Single {
                MenuItem::new(label, Action::Video(VideoOp::SetCompare(c))).checked(now == c)
            } else {
                MenuItem::disabled(label)
            }
        };
        items.push(
            MenuItem::submenu(
                "Compare",
                vec![
                    compare("Top Track", VideoCompare::Single),
                    compare("Side by Side (A | B)", VideoCompare::SideBySide),
                    compare("Wipe (A / B, drag the divider)", VideoCompare::Wipe),
                ],
            )
            .separated(),
        );
        if let Some(v) = &shown {
            let has_tc = model
                .project()
                .video
                .sources
                .get(&v.source)
                .is_some_and(|s| s.timecode.is_some());
            if has_tc {
                items.push(MenuItem::new(
                    "Spot Clip to Its Timecode",
                    Action::Video(VideoOp::SpotToTimecode(v.clip)),
                ));
            }
            items.push(MenuItem::new(
                "Remove Video Clip",
                Action::Video(VideoOp::RemoveClip(v.clip)),
            ));
        }
        items.push(MenuItem::new("Full Screen", Action::FullScreen(ViewId::video())).separated());
        items
    }

    /// Draw `shown` letterboxed in `area` (or what stands for it; a 360°
    /// picture looked into, filling it); whether the exact frame is still
    /// to come.
    fn draw_picture(
        p: &mut dyn Painter,
        model: &Session,
        shown: &Option<VideoShown>,
        area: Rect,
        theme: &Theme,
        scale: f32,
        cache: &mut sphere::SphereCache,
    ) -> bool {
        let small = TextStyle::new(11.0, theme.ui.text_dim).align(Align::Center);
        let middle = Rect::new(area.x, area.y + area.h / 2.0 - 10.0, area.w, 20.0);
        match shown {
            None => {
                let empty = model
                    .project()
                    .video
                    .tracks
                    .iter()
                    .all(|t| t.clips.is_empty());
                let text = if empty {
                    "No video — File → Import Video…, or drop a movie here"
                } else {
                    "No picture here"
                };
                p.text(text, middle, &small);
                false
            }
            Some(v) => match &v.picture {
                Some(_) if model.video_spherical(v.source).is_some() => {
                    let Some(s) = model.video_spherical(v.source) else {
                        return true;
                    };
                    Self::draw_sphere(p, model, v, &s, area, scale, cache)
                }
                Some(pic) => {
                    let f = &pic.frame;
                    let fit = Self::fit(f.width as f32, f.height as f32, Size::new(area.w, area.h));
                    let dst = Rect::new(area.x + fit.x, area.y + fit.y, fit.w, fit.h);
                    let key = (v.source.raw() << 40) ^ ((pic.number as u64) << 12) ^ f.width as u64;
                    match &f.gpu {
                        // In video memory: the display takes it as it is
                        // (or the frames after come through memory).
                        Some(g) => {
                            let shown = p.external(
                                &External {
                                    key,
                                    width: f.width,
                                    height: f.height,
                                    handle: g,
                                },
                                dst,
                            );
                            if !shown {
                                return true;
                            }
                        }
                        None => p.pixels(
                            &Pixels {
                                key,
                                width: f.width,
                                height: f.height,
                                rgba: &f.rgba,
                            },
                            dst,
                        ),
                    }
                    !pic.exact
                }
                None => {
                    p.text(&Self::missing_text(model, v), middle, &small);
                    true
                }
            },
        }
    }

    /// Why a picture is not there yet: its file's error, or being read.
    fn missing_text(model: &Session, v: &VideoShown) -> String {
        let state = model.video_source_state(v.source);
        match &state.error {
            Some(e) => e.clone(),
            None if !state.indexed => "Reading the video…".into(),
            None => "…".into(),
        }
    }

    /// The cue over the picture `a`.
    fn paint_adr(p: &mut dyn Painter, a: Rect, o: &faderframe_session::adr::AdrOverlay) {
        let white = Color::rgb(1.0, 1.0, 1.0);
        let solid = faderframe_ui_canvas::Paint::Solid;
        p.push_clip(a);
        if let Some(x) = o.streamer {
            let x = a.x + x * a.w;
            p.fill(Rect::new(x - 4.0, a.y, 8.0, a.h), white.with_alpha(0.85));
            p.fill(Rect::new(x - 10.0, a.y, 6.0, a.h), white.with_alpha(0.25));
        }
        if o.punch {
            let d = a.h.min(a.w) * 0.35;
            let r = Rect::new(a.x + (a.w - d) / 2.0, a.y + (a.h - d) / 2.0, d, d);
            p.fill_rounded(r, d / 2.0, &solid(white.with_alpha(0.8)));
        }
        // The line, boxed at the bottom third.
        let who = if o.character.is_empty() {
            format!("{} ·", o.number)
        } else {
            format!("{} · {}:", o.number, o.character)
        };
        let line = format!("{who} {}", o.text);
        let colour = if o.speaking {
            Color::rgb(1.0, 0.95, 0.55)
        } else {
            white
        };
        let style = TextStyle::new(16.0, colour)
            .weight(FontWeight::Bold)
            .align(Align::Center);
        let bw = (p.text_width(&line, &style) + 32.0).min(a.w - 16.0);
        let b = Rect::new(a.x + (a.w - bw) / 2.0, a.y + a.h * 0.72, bw, 30.0);
        p.fill_rounded(b, 5.0, &solid(Color::rgba(0.0, 0.0, 0.0, 0.7)));
        p.text(&line, b, &style);
        // Recording, and the beeps to come.
        let mut x = a.x + 12.0;
        let y = a.y + 40.0;
        if o.recording {
            let r = Rect::new(x, y, 44.0, 20.0);
            p.fill_rounded(r, 4.0, &solid(Color::hex(0xd83030)));
            p.text(
                "REC",
                r,
                &TextStyle::new(11.0, white)
                    .weight(FontWeight::Bold)
                    .align(Align::Center),
            );
            x += 52.0;
        }
        if let Some(n) = o.beeps_left
            && n > 0
        {
            let back = Rect::new(x - 4.0, y, n as f32 * 18.0 + 2.0, 20.0);
            p.fill_rounded(back, 4.0, &solid(Color::rgba(0.0, 0.0, 0.0, 0.7)));
            for i in 0..n {
                let r = Rect::new(x + i as f32 * 18.0, y + 4.0, 12.0, 12.0);
                p.fill_rounded(r, 6.0, &solid(white.with_alpha(0.9)));
            }
        }
        p.pop_clip();
    }

    /// A small label in the corner of a compared picture.
    fn tag(p: &mut dyn Painter, text: &str, at: Point) {
        let w = text.chars().count() as f32 * 6.4 + 12.0;
        let r = Rect::new(at.x, at.y, w, 18.0);
        p.fill_rounded(
            r,
            4.0,
            &faderframe_ui_canvas::Paint::Solid(Color::rgba(0.0, 0.0, 0.0, 0.6)),
        );
        p.text(
            text,
            r,
            &TextStyle::new(10.0, Color::rgb(1.0, 1.0, 1.0)).align(Align::Center),
        );
    }

    fn overlay_text(model: &Session, v: &VideoShown) -> (String, String) {
        let tc = model.timecode();
        let label = tc.label(v.position, model.project().sample_rate);
        let name = model
            .project()
            .video
            .sources
            .get(&v.source)
            .map(|s| s.name())
            .unwrap_or_default();
        let mapped = model
            .video_source_state(v.source)
            .colour
            .map(|c| format!(" · {c} → SDR"))
            .unwrap_or_default();
        (label, format!("{name} · frame {}{mapped}", v.frame))
    }
}

impl CanvasView<Session, Action> for VideoView {
    fn accessible(&self, size: Size, model: &Session) -> Vec<AccessNode<Action>> {
        self.access_nodes(size, model)
    }

    fn accessible_name(&self) -> Option<String> {
        Some("Video".into())
    }

    fn access_press(
        &mut self,
        id: u64,
        press: AccessPress,
        size: Size,
        model: &Session,
        cx: &mut EventCx<'_, Action>,
    ) -> bool {
        self.press(id, press, size, model, cx)
    }

    fn access_news(&mut self, model: &Session) -> Vec<String> {
        self.news(model)
    }

    fn frame_timing(&mut self, lead_ns: i64, scale: f32) {
        self.lead = lead_ns;
        self.scale = scale.clamp(0.5, 4.0);
    }

    fn set_theme(&mut self, theme: &Theme) {
        self.theme = theme.clone();
    }

    fn wants_frames(&self, model: &Session) -> bool {
        model.transport().playing || self.waiting || !model.video_jobs().is_empty()
    }

    fn paint(&mut self, p: &mut dyn Painter, size: Size, model: &Session, theme: &Theme) {
        if let Some(lit) = model.av_calibration_screen(self.lead) {
            self.waiting = true;
            self.shown = None;
            self.caption = None;
            Self::paint_calibration(p, size, lit, model);
            return;
        }
        let area = Rect::new(0.0, 0.0, size.w, size.h);
        // Picture is framed in black, whatever the skin.
        p.fill(area, Color::rgb(0.0, 0.0, 0.0));
        // At device pixels: sharp on HiDPI screens.
        let max = (
            (size.w * self.scale).max(1.0) as u32,
            (size.h * self.scale).max(1.0) as u32,
        );
        // A/B: the first two shown tracks, side by side or wiped.
        let pair: Vec<(faderframe_core::VideoTrackId, String)> = model
            .project()
            .video
            .shown_tracks()
            .take(2)
            .map(|t| (t.id, t.name.clone()))
            .collect();
        let compare = if pair.len() == 2 {
            model.video_compare()
        } else {
            VideoCompare::Single
        };
        let shown = match compare {
            VideoCompare::Single => {
                let shown = model.video_picture(self.lead, max);
                let scale = self.scale;
                self.waiting = Self::draw_picture(
                    p,
                    model,
                    &shown,
                    area,
                    theme,
                    scale,
                    &mut self.sphere_cache[0],
                );
                // A 360° picture fills the window and is looked round in;
                // a flat one: where it is (a box drawn on it), and the
                // object followed over it.
                self.sphere_shown = sphere::spherical_of(model, &shown).map(|_| area);
                self.shown = shown
                    .as_ref()
                    .filter(|_| self.sphere_shown.is_none())
                    .and_then(|v| {
                        let f = &v.picture.as_ref()?.frame;
                        let r = Self::fit(f.width as f32, f.height as f32, size);
                        Some((r, v.clip, v.frame))
                    });
                if let (Some(v), Some((r, _, _))) = (&shown, self.shown) {
                    self.paint_follow(p, model, v, r);
                }
                if self.sphere_shown.is_some() {
                    self.paint_sphere_overlay(p, model, area);
                }
                if let Some((a, b)) = self.boxing {
                    let r = Rect::new(
                        a.x.min(b.x),
                        a.y.min(b.y),
                        (a.x - b.x).abs(),
                        (a.y - b.y).abs(),
                    );
                    p.stroke_rounded(r, 2.0, 1.5, self.theme.ui.accent);
                }
                shown
            }
            VideoCompare::SideBySide => {
                self.shown = None;
                self.sphere_shown = None;
                let half = (max.0 / 2, max.1);
                let a = model.video_picture_on(Some(pair[0].0), self.lead, half);
                let b = model.video_picture_on(Some(pair[1].0), self.lead, half);
                let left = Rect::new(0.0, 0.0, size.w / 2.0 - 1.0, size.h);
                let right = Rect::new(size.w / 2.0 + 1.0, 0.0, size.w / 2.0 - 1.0, size.h);
                let scale = self.scale;
                let [ca, cb] = &mut self.sphere_cache;
                let wa = Self::draw_picture(p, model, &a, left, theme, scale, ca);
                let wb = Self::draw_picture(p, model, &b, right, theme, scale, cb);
                self.waiting = wa || wb;
                Self::tag(p, &format!("A · {}", pair[0].1), Point::new(8.0, 40.0));
                Self::tag(
                    p,
                    &format!("B · {}", pair[1].1),
                    Point::new(size.w / 2.0 + 8.0, 40.0),
                );
                a
            }
            VideoCompare::Wipe => {
                self.shown = None;
                self.sphere_shown = None;
                let a = model.video_picture_on(Some(pair[0].0), self.lead, max);
                let b = model.video_picture_on(Some(pair[1].0), self.lead, max);
                let scale = self.scale;
                let [ca, cb] = &mut self.sphere_cache;
                let wb = Self::draw_picture(p, model, &b, area, theme, scale, cb);
                let x = (self.wipe * size.w).round();
                p.push_clip(Rect::new(0.0, 0.0, x, size.h));
                p.fill(area, Color::rgb(0.0, 0.0, 0.0));
                let wa = Self::draw_picture(p, model, &a, area, theme, scale, ca);
                p.pop_clip();
                p.fill(
                    Rect::new(x - 1.0, 0.0, 2.0, size.h),
                    Color::rgba(1.0, 1.0, 1.0, 0.85),
                );
                self.waiting = wa || wb;
                Self::tag(p, &format!("A · {}", pair[0].1), Point::new(8.0, 40.0));
                let bt = format!("B · {}", pair[1].1);
                let bw = bt.chars().count() as f32 * 6.4 + 12.0;
                Self::tag(p, &bt, Point::new(size.w - bw - 8.0, 40.0));
                a
            }
        };
        self.caption = shown.as_ref().map(|v| {
            let (tc, what) = Self::overlay_text(model, v);
            match &v.picture {
                Some(_) => format!("{tc}, {what}"),
                None => format!("{tc}, {what}, {}", Self::missing_text(model, v)),
            }
        });
        if self.overlay
            && let Some(v) = &shown
        {
            let (tc, what) = Self::overlay_text(model, v);
            let mono = TextStyle::new(16.0, Color::rgb(1.0, 1.0, 1.0))
                .family(FontFamily::Mono)
                .weight(FontWeight::Bold)
                .align(Align::Center);
            let bw = 150.0f32.min(size.w);
            let b = Rect::new((size.w - bw) / 2.0, 8.0, bw, 24.0);
            p.fill_rounded(
                b,
                4.0,
                &faderframe_ui_canvas::Paint::Solid(Color::rgba(0.0, 0.0, 0.0, 0.6)),
            );
            p.text(&tc, b, &mono);
            // The file and frame below, boxed too (picture may be bright).
            let info = TextStyle::new(10.0, Color::rgba(1.0, 1.0, 1.0, 0.85)).align(Align::Center);
            let iw = (what.chars().count() as f32 * 6.2 + 16.0).min(size.w);
            let ib = Rect::new((size.w - iw) / 2.0, size.h - 24.0, iw, 18.0);
            p.fill_rounded(
                ib,
                4.0,
                &faderframe_ui_canvas::Paint::Solid(Color::rgba(0.0, 0.0, 0.0, 0.6)),
            );
            p.text(&what, ib, &info);
        }
        // ADR: the streamer crossing the picture before the line, the punch
        // where it starts, the line and the beeps still to come.
        let at = shown
            .as_ref()
            .map_or(model.transport().position, |v| v.position);
        if let Some(o) = model.adr_overlay(at) {
            // Over the picture (not the letterbox).
            let area = shown
                .as_ref()
                .filter(|_| self.sphere_shown.is_none())
                .and_then(|v| v.picture.as_ref())
                .map_or(Rect::from_size(size), |pic| {
                    Self::fit(pic.frame.width as f32, pic.frame.height as f32, size)
                });
            Self::paint_adr(p, area, &o);
        }
        // The late-frame meter: frames shown while playing and how many
        // were not the one wanted when they reached the screen.
        let (shown_n, late) = model.video_frames_late();
        if self.overlay && shown_n > 0 {
            let text = format!("late {late} / {shown_n}");
            let w = text.chars().count() as f32 * 6.2 + 14.0;
            let r = Rect::new(size.w - w - 8.0, size.h - 24.0, w, 18.0);
            let ok = late * 100 <= shown_n;
            p.fill_rounded(
                r,
                4.0,
                &faderframe_ui_canvas::Paint::Solid(Color::rgba(0.0, 0.0, 0.0, 0.6)),
            );
            let colour = if ok {
                Color::rgba(0.6, 0.9, 0.6, 0.9)
            } else {
                Color::rgba(1.0, 0.75, 0.3, 0.95)
            };
            p.text(&text, r, &TextStyle::new(10.0, colour).align(Align::Center));
        }
        // Jobs (reading, proxies, writing) above the bottom line.
        let jobs = model.video_jobs();
        for (i, j) in jobs.iter().enumerate() {
            let y = size.h - 30.0 - 18.0 * (jobs.len() - i) as f32;
            p.text(
                &format!("{} — {:.0} %", j.label, j.share * 100.0),
                Rect::new(10.0, y, size.w - 20.0, 16.0),
                &TextStyle::new(10.0, Color::rgba(1.0, 1.0, 1.0, 0.7)),
            );
        }
    }

    fn event(
        &mut self,
        ev: &ViewEvent,
        size: Size,
        model: &Session,
        cx: &mut EventCx<'_, Action>,
    ) -> bool {
        match ev {
            ViewEvent::PointerDown {
                pos,
                button: PointerButton::Secondary,
                ..
            } => {
                let items = self.menu(model);
                cx.request(HostRequest::ContextMenu { at: *pos, items });
                true
            }
            ViewEvent::PointerDown {
                pos,
                button: PointerButton::Primary,
                modifiers,
                clicks: 1,
            } if (modifiers.shift || model.follow_drawing())
                && self.shown.is_some_and(|(r, _, _)| r.contains(*pos)) =>
            {
                self.boxing = Some((*pos, *pos));
                cx.redraw();
                true
            }
            ViewEvent::PointerMove {
                pos,
                dragging: true,
                ..
            } if self.boxing.is_some() => {
                if let Some((a, _)) = self.boxing {
                    self.boxing = Some((a, *pos));
                }
                cx.redraw();
                true
            }
            ViewEvent::PointerUp { pos, .. } if self.boxing.is_some() => {
                if let (Some((a, _)), Some((r, clip, frame))) = (self.boxing.take(), self.shown)
                    && let Some(area) = Self::area_of(a, *pos, r)
                {
                    cx.emit(Action::Video(VideoOp::Follow(FollowOp::Key {
                        clip,
                        frame,
                        area,
                    })));
                }
                cx.redraw();
                true
            }
            // A 360° picture: dragged round, or clicked to pan a track.
            ViewEvent::PointerDown {
                pos,
                button: PointerButton::Primary,
                modifiers,
                clicks: 1,
            } if self.sphere_area().is_some_and(|r| r.contains(*pos)) => {
                self.sphere_press(*pos, modifiers.alt, model, cx);
                true
            }
            ViewEvent::PointerMove {
                pos,
                dragging: true,
                ..
            } if self.sphere_drag.is_some() => {
                self.sphere_move(*pos, model, cx);
                true
            }
            ViewEvent::PointerUp { .. } if self.sphere_drag.is_some() => {
                self.sphere_release(cx);
                true
            }
            ViewEvent::Scroll { pos, dy, .. }
                if self.sphere_area().is_some_and(|r| r.contains(*pos)) =>
            {
                self.sphere_zoom(*dy, model, cx);
                true
            }
            ViewEvent::PointerDown {
                button: PointerButton::Primary,
                clicks: 2,
                ..
            } => {
                cx.emit(Action::FullScreen(ViewId::video()));
                true
            }
            ViewEvent::PointerDown {
                pos,
                button: PointerButton::Primary,
                ..
            } if model.video_compare() == VideoCompare::Wipe => {
                self.wiping = true;
                self.wipe = (pos.x / size.w.max(1.0)).clamp(0.0, 1.0);
                cx.redraw();
                true
            }
            ViewEvent::PointerMove {
                pos,
                dragging: true,
                ..
            } if self.wiping => {
                self.wipe = (pos.x / size.w.max(1.0)).clamp(0.0, 1.0);
                cx.redraw();
                true
            }
            ViewEvent::PointerUp { .. } if self.wiping => {
                self.wiping = false;
                true
            }
            _ => false,
        }
    }

    fn drag_files(
        &mut self,
        _pos: Option<faderframe_ui_canvas::Point>,
        _size: Size,
        _model: &Session,
    ) -> bool {
        true
    }

    fn drop_files(
        &mut self,
        files: &[std::path::PathBuf],
        _pos: faderframe_ui_canvas::Point,
        _size: Size,
        _model: &Session,
    ) -> Option<Action> {
        files.first().map(|f| {
            Action::Video(VideoOp::Import {
                path: f.clone(),
                sound: true,
            })
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use faderframe_engine::EngineConfig;
    use faderframe_project::Project;
    use faderframe_ui_canvas::{DrawOp, Modifiers, RecordingPainter};

    fn session() -> Session {
        Session::new(Project::new("Film", 48_000), None, EngineConfig::default()).unwrap()
    }

    fn send(view: &mut VideoView, ev: ViewEvent, s: &Session) -> Vec<Action> {
        let mut actions = Vec::new();
        let mut requests = Vec::new();
        let mut cx = EventCx::new(&mut actions, &mut requests);
        view.event(&ev, Size::new(640.0, 360.0), s, &mut cx);
        actions
    }

    fn flat(items: &[MenuItem<Action>]) -> Vec<(String, Option<Action>)> {
        let mut out = Vec::new();
        for i in items {
            out.push((i.label.clone(), i.action.clone()));
            out.extend(flat(&i.children));
        }
        out
    }

    #[test]
    fn a_box_maps_onto_the_picture_and_back() {
        let r = Rect::new(80.0, 0.0, 480.0, 270.0);
        let a = VideoView::area_of(Point::new(320.0, 135.0), Point::new(200.0, 27.0), r).unwrap();
        assert!((a.x - 0.25).abs() < 1e-6 && (a.y - 0.1).abs() < 1e-6);
        assert!((a.w - 0.25).abs() < 1e-6 && (a.h - 0.4).abs() < 1e-6);
        let back = VideoView::on_picture(a, r);
        assert_eq!(back, Rect::new(200.0, 27.0, 120.0, 108.0));
        // A click is no box.
        assert!(
            VideoView::area_of(Point::new(100.0, 100.0), Point::new(102.0, 101.0), r).is_none()
        );
    }

    #[test]
    fn a_shift_drag_on_the_picture_boxes_an_object_to_follow() {
        let s = session();
        let mut view = VideoView::new(Theme::default());
        let clip = faderframe_core::VideoClipId(7);
        let picture = Rect::new(80.0, 0.0, 480.0, 360.0);
        view.shown = Some((picture, clip, 12));
        let shift = Modifiers {
            shift: true,
            ..Modifiers::NONE
        };
        // Without Shift (and the tool off): no box.
        let none = send(
            &mut view,
            ViewEvent::PointerDown {
                pos: Point::new(200.0, 100.0),
                button: PointerButton::Primary,
                modifiers: Modifiers::NONE,
                clicks: 1,
            },
            &s,
        );
        assert!(none.is_empty());
        assert!(view.boxing.is_none());
        send(
            &mut view,
            ViewEvent::PointerDown {
                pos: Point::new(200.0, 90.0),
                button: PointerButton::Primary,
                modifiers: shift,
                clicks: 1,
            },
            &s,
        );
        send(
            &mut view,
            ViewEvent::PointerMove {
                pos: Point::new(260.0, 150.0),
                modifiers: shift,
                dragging: true,
            },
            &s,
        );
        // Being drawn while dragging.
        assert_eq!(
            view.boxing,
            Some((Point::new(200.0, 90.0), Point::new(260.0, 150.0)))
        );
        let actions = send(
            &mut view,
            ViewEvent::PointerUp {
                pos: Point::new(260.0, 150.0),
                button: PointerButton::Primary,
                modifiers: shift,
            },
            &s,
        );
        let [
            Action::Video(VideoOp::Follow(FollowOp::Key {
                clip: c,
                frame,
                area,
            })),
        ] = actions.as_slice()
        else {
            panic!("{actions:?}");
        };
        assert_eq!((*c, *frame), (clip, 12));
        assert!((area.x - 0.25).abs() < 1e-6 && (area.y - 0.25).abs() < 1e-6);
        assert!((area.w - 0.125).abs() < 1e-6 && (area.h - 1.0 / 6.0).abs() < 1e-6);
    }

    #[test]
    fn the_menu_calibrates_with_a_camera_and_follows_an_object() {
        let s = session();
        let view = VideoView::new(Theme::default());
        let items = flat(&view.menu(&s));
        let labels: Vec<&str> = items.iter().map(|(l, _)| l.as_str()).collect();
        assert!(labels.contains(&"Calibrate with a Camera"), "{labels:?}");
        let test = items
            .iter()
            .find(|(l, _)| l == "Test Without a Camera")
            .and_then(|(_, a)| a.clone());
        assert!(matches!(
            test,
            Some(Action::Video(VideoOp::Calibrate(CalibrationSettings {
                camera: CameraChoice::Virtual { .. },
                ..
            })))
        ));
        assert!(labels.contains(&"Look for Cameras"));
        assert!(labels.contains(&"With the First Camera"));
        let draw = items
            .iter()
            .find(|(l, _)| l == "Follow an Object (Draw a Box Around It)")
            .and_then(|(_, a)| a.clone());
        assert_eq!(
            draw,
            Some(Action::Video(VideoOp::Follow(FollowOp::Draw(true))))
        );
        // Nothing followed: nothing to apply yet.
        assert!(!labels.contains(&"Pan a Track with It"));
    }

    #[test]
    fn the_calibration_screen_is_the_whole_window() {
        let s = session();
        let size = Size::new(640.0, 360.0);
        let mut p = RecordingPainter::new();
        VideoView::paint_calibration(&mut p, size, true, &s);
        assert_eq!(p.ops.first(), Some(&DrawOp::Rect(Rect::from_size(size))));
        assert!(p.texts().iter().any(|t| t.starts_with("A/V calibration")));
        // Not calibrating: the picture as ever.
        let mut view = VideoView::new(Theme::default());
        let mut p = RecordingPainter::new();
        view.paint(&mut p, size, &s, &Theme::default());
        assert!(p.texts().iter().any(|t| t.starts_with("No video")));
    }

    /// The picture is one control: read as what it shows, its menu (the
    /// window's) opened with Enter or the Menu key; a wiped comparison's
    /// divider is a slider the arrows move.
    #[test]
    fn the_picture_reads_as_what_it_shows_and_opens_its_menu() {
        use faderframe_ui_canvas::access::{focus_order, press_key};
        use faderframe_ui_canvas::{AccessRole, Key};
        let size = Size::new(640.0, 360.0);
        let s = session();
        let mut view = VideoView::new(Theme::default());
        assert_eq!(view.accessible_name().as_deref(), Some("Video"));
        let mut p = RecordingPainter::new();
        view.paint(&mut p, size, &s, &Theme::default());
        let nodes = view.accessible(size, &s);
        let order = focus_order(&nodes);
        assert_eq!(order.len(), 1);
        let picture = order[0].clone();
        assert_eq!(
            (picture.role, picture.label.as_str()),
            (AccessRole::Region, "No video")
        );
        assert!(picture.menu);
        for k in [Key::Enter, Key::Menu] {
            let (_, requests) =
                press_key(&mut view, &nodes, picture.id, k, Modifiers::NONE, size, &s).unwrap();
            let Some(HostRequest::ContextMenu { items, .. }) = requests.first() else {
                panic!("the video menu")
            };
            assert!(flat(items).iter().any(|(l, _)| l == "Import Video…"));
        }
        // What is shown, as read.
        view.caption = Some("01:00:00:00, film.mov · frame 0".into());
        let nodes = view.accessible(size, &s);
        assert_eq!(nodes[0].label, "Picture 01:00:00:00, film.mov · frame 0");
        // The divider: only while two tracks are wiped; the view's own.
        assert!(nodes[0].children.is_empty());
        let mut wiped =
            Session::new(Project::new("Film", 48_000), None, EngineConfig::default()).unwrap();
        wiped.dispatch(Action::Video(VideoOp::AddTrack)).unwrap();
        wiped.dispatch(Action::Video(VideoOp::AddTrack)).unwrap();
        wiped
            .dispatch(Action::Video(VideoOp::SetCompare(VideoCompare::Wipe)))
            .unwrap();
        let nodes = view.accessible(size, &wiped);
        let wipe = nodes[0].children.first().cloned().unwrap();
        assert_eq!(wipe.role, AccessRole::Slider);
        press_key(
            &mut view,
            &nodes,
            wipe.id,
            Key::Right,
            Modifiers::NONE,
            size,
            &wiped,
        )
        .unwrap();
        assert!((view.wipe - 0.55).abs() < 1e-6);
    }

    /// Jobs finishing are said; nothing on the first look.
    #[test]
    fn finished_jobs_are_said() {
        let s = session();
        let mut view = VideoView::new(Theme::default());
        assert!(view.access_news(&s).is_empty());
        view.told = Some(access::Told::with_jobs(vec!["Reading film.mov".into()]));
        assert_eq!(view.access_news(&s), ["Reading film.mov: done"]);
        assert!(view.access_news(&s).is_empty());
    }
}
