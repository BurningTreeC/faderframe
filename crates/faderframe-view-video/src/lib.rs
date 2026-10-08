//! The video window: the picture for the sample heard when the frame being
//! drawn reaches the screen ([`Session::video_picture`]), letterboxed on
//! black, with a timecode overlay (project timecode, frame, the file). Its
//! menu imports video, moves the picture against the sound (by frames or
//! milliseconds), makes the flash-and-beep sync test, spots a clip to its
//! timecode, writes the movie and goes full screen (a double-click too).

#![forbid(unsafe_code)]

use faderframe_session::video::{VideoOp, VideoShown};
use faderframe_session::{Action, Session};
use faderframe_ui_canvas::{
    Align, CanvasView, Color, EventCx, FontFamily, FontWeight, HostRequest, MenuItem, Painter,
    Pixels, PointerButton, Rect, Size, TextStyle, Theme, ViewEvent,
};
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
}

impl VideoView {
    pub fn new(theme: Theme) -> Self {
        Self {
            theme,
            lead: 16_666_667,
            scale: 1.0,
            overlay: true,
            waiting: false,
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
        ];
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
        (label, format!("{name} · frame {}", v.frame))
    }
}

impl CanvasView<Session, Action> for VideoView {
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
        let area = Rect::new(0.0, 0.0, size.w, size.h);
        // Picture is framed in black, whatever the skin.
        p.fill(area, Color::rgb(0.0, 0.0, 0.0));
        let ui = &theme.ui;
        // At device pixels: sharp on HiDPI screens.
        let max = (
            (size.w * self.scale).max(1.0) as u32,
            (size.h * self.scale).max(1.0) as u32,
        );
        let shown = model.video_picture(self.lead, max);
        self.waiting = false;
        let small = TextStyle::new(11.0, ui.text_dim).align(Align::Center);
        match &shown {
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
                p.text(
                    text,
                    Rect::new(0.0, size.h / 2.0 - 10.0, size.w, 20.0),
                    &small,
                );
            }
            Some(v) => match &v.picture {
                Some(pic) => {
                    let f = &pic.frame;
                    let dst = Self::fit(f.width as f32, f.height as f32, size);
                    let key = (v.source.raw() << 40) ^ ((pic.number as u64) << 12) ^ f.width as u64;
                    p.pixels(
                        &Pixels {
                            key,
                            width: f.width,
                            height: f.height,
                            rgba: &f.rgba,
                        },
                        dst,
                    );
                    self.waiting = !pic.exact;
                }
                None => {
                    self.waiting = true;
                    let state = model.video_source_state(v.source);
                    let text = match &state.error {
                        Some(e) => e.clone(),
                        None if !state.indexed => "Reading the video…".into(),
                        None => "…".into(),
                    };
                    p.text(
                        &text,
                        Rect::new(0.0, size.h / 2.0 - 10.0, size.w, 20.0),
                        &small,
                    );
                }
            },
        }
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
        _size: Size,
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
                button: PointerButton::Primary,
                clicks: 2,
                ..
            } => {
                cx.emit(Action::FullScreen(ViewId::video()));
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
