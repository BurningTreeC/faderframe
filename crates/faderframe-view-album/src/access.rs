//! The Album view for screen readers and the keyboard: the toolbar's
//! buttons (menu buttons open their menus with Enter, Shift+F10 or the Menu
//! key; "+ Files…" its chooser), the player (previous, play or pause,
//! next, back to the project, the album's position as a slider the arrows
//! move by five seconds), the songs as a list and the export folder.
//!
//! A song is read as its number, title, length and loudness (the rest in
//! its description); Enter selects it (Delete then removes it, the arrows
//! move the selection), its menu is the row's. Its parts that change are
//! controls of their own: the title, fades and ISRC (Enter types them),
//! the pause or crossfade and the trim (spin buttons the arrows step by a
//! tenth, Enter types them) and the inserts (Enter: their menu).
//!
//! Said by themselves: an analysis, export or preparation finishing (with
//! the album's loudness), a song failing, the song playing changing.

use crate::{AlbumView, Button, Column, Layout, ROW_H, db, duration, lufs, minus, update};
use faderframe_core::SongId;
use faderframe_project::album::{AlbumLevel, Song};
use faderframe_session::album::{AlbumAction, AlbumTask};
use faderframe_session::vinyl;
use faderframe_session::{Action, Session};
use faderframe_ui_canvas::{
    AccessNode, AccessPress, AccessRole, CanvasView, EventCx, FileChoice, HostRequest, Point, Rect,
    Size, access_id,
};

/// Kinds of node (with a song's id, or a button's place).
const TOOLBAR: u64 = 0xA1B0;
const BUTTON: u64 = 0xA1B1;
const PLAYER: u64 = 0xA1B2;
const SEEK: u64 = 0xA1B3;
const SONGS: u64 = 0xA1B4;
const SONG: u64 = 0xA1B5;
const PART: u64 = 0xA1B6;
const FOLDER: u64 = 0xA1B7;
const SUMMARY: u64 = 0xA1B8;

/// Seconds the position slider moves per step.
const SEEK_STEP: f64 = 5.0;

/// What the keyboard can ask the view for.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Control {
    Button(Button),
    Folder,
    Song(usize),
    Part(usize, Column),
}

/// What screen readers were last told ([`AlbumView::news`]).
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Told {
    task: Option<AlbumTask>,
    failed: Vec<SongId>,
    playing: Option<usize>,
}

fn button_id(b: Button) -> u64 {
    access_id(&[BUTTON, b as u64])
}

fn part_id(song: SongId, c: Column) -> u64 {
    access_id(&[PART, song.0, c as u64])
}

/// The song's parts that are controls of their own.
const PARTS: [Column; 6] = [
    Column::Title,
    Column::Pause,
    Column::Gain,
    Column::Fades,
    Column::Isrc,
    Column::Inserts,
];

impl AlbumView {
    /// What a screen reader calls a button.
    fn button_name(b: Button, model: &Session) -> String {
        let s = &model.project().album.settings;
        let shown = Self::button_label(b, model);
        let plain = shown.trim_end_matches(" ▾").to_string();
        match b {
            Button::AddSections => "Add sections".into(),
            Button::AddProject => "Add this project".into(),
            Button::AddFiles => "Add files…".into(),
            Button::Level => match s.level {
                AlbumLevel::Album => "Levelling: one gain for the album".into(),
                AlbumLevel::PerSong => "Levelling: per song".into(),
            },
            Button::Target => match s.loudness {
                Some(l) => minus(format!("Loudness target: {l:.1} LUFS")),
                None => "Loudness target: none".into(),
            },
            Button::Ceiling => match s.ceiling {
                Some(c) => minus(format!(
                    "True-peak ceiling: {c:.1} dBTP, {}",
                    if s.limit {
                        "limit the peaks"
                    } else {
                        "less gain"
                    }
                )),
                None => "True-peak ceiling: none".into(),
            },
            Button::Format => format!("Format: {plain}"),
            Button::AlbumFile => "Album file and cue sheet".into(),
            Button::Cd => format!("CD master (DDP): {}", if s.ddp { "on" } else { "off" }),
            Button::Vinyl if s.vinyl.enabled => {
                format!("Vinyl premaster: {}", s.vinyl.format.name())
            }
            Button::Vinyl => "Vinyl premaster: off".into(),
            Button::Release => "Release details…".into(),
            Button::Qc => match plain.strip_prefix("QC · ") {
                Some(spec) => format!("QC report: {spec}"),
                None => "QC report: none".into(),
            },
            Button::Analyse => "Analyse".into(),
            Button::Export => "Export".into(),
            Button::Cancel => "Cancel".into(),
            Button::Previous => "Previous song".into(),
            Button::PlayPause => match model.album_playback() {
                Some(pb) if pb.playing => "Pause".into(),
                Some(pb) if pb.preparing.is_some() && pb.length == 0.0 => {
                    "Preparing the album".into()
                }
                _ => "Play album".into(),
            },
            Button::Next => "Next song".into(),
            Button::BackToProject => "Back to the project".into(),
        }
    }

    /// Buttons whose press is a menu.
    fn is_menu(b: Button) -> bool {
        matches!(
            b,
            Button::Level
                | Button::Target
                | Button::Ceiling
                | Button::Format
                | Button::Cd
                | Button::Vinyl
                | Button::Qc
        )
    }

    /// The action a button's press emits, when it is one (not a menu or a
    /// chooser).
    fn button_action(&self, b: Button, model: &Session) -> Option<Action> {
        let s = &model.project().album.settings;
        let a = |x: AlbumAction| Some(Action::Album(x));
        match b {
            Button::AddSections => a(AlbumAction::AddSections),
            Button::AddProject => a(AlbumAction::AddThisProject),
            Button::AlbumFile => Some(crate::settings_action(
                faderframe_project::album::AlbumSettings {
                    album_file: !s.album_file,
                    ..s.clone()
                },
            )),
            Button::Release => a(AlbumAction::Details(None)),
            Button::Analyse => a(AlbumAction::Analyse),
            Button::Export => a(AlbumAction::Export),
            Button::Cancel => a(AlbumAction::Cancel),
            Button::PlayPause => Some(self.play_action(model)),
            Button::Previous => a(AlbumAction::Skip(-1)),
            Button::Next => a(AlbumAction::Skip(1)),
            Button::BackToProject => a(AlbumAction::StopPlaying),
            _ => None,
        }
    }

    /// A song as read: its number, title, length and loudness, and whether
    /// it plays; the rest for its description.
    fn song_text(model: &Session, i: usize, song: &Song) -> (String, String) {
        let sides = model.vinyl_sides();
        let number = match sides.as_deref().and_then(|s| vinyl::placement(s, i)) {
            Some((side, k)) => vinyl::track_label(side, k),
            None => format!("{}", i + 1),
        };
        let mut label = format!("{number}. {}", song.title);
        if let Some(secs) = Self::seconds(model, song) {
            label.push_str(&format!(", {}", duration(secs)));
        }
        let error = model
            .album_error(song.id)
            .filter(|_| model.album_analysis(song).is_none());
        let analysis = model.album_analysis(song);
        match (error, analysis) {
            (Some(e), _) => label.push_str(&format!(", failed: {e}")),
            (None, Some(a)) => label.push_str(&format!(", {}", lufs(a.report.integrated))),
            (None, None) => label.push_str(", not analysed"),
        }
        if let Some(d) = model.album_delivered(song.id) {
            label.push_str(&format!(", delivered {}", lufs(d.loudness)));
        }
        if model.album_playback().is_some_and(|pb| pb.song == Some(i)) {
            label.push_str(", playing");
        }
        let mut more = vec![Self::source_text(model, song).0];
        if let Some(a) = analysis {
            let r = &a.report;
            more.push(format!("loudness range {:.1} LU", r.range));
            more.push(minus(format!("true peak {:.1} dBTP", r.true_peak)));
        }
        if let Some(d) = model.album_delivered(song.id) {
            more.push(format!("gain {}", db(d.gain_db)));
            if model.project().album.settings.limit && d.limiting >= 0.1 {
                more.push(format!("the limiter takes {:.1} dB", d.limiting));
            }
        }
        for n in model.vinyl_notes() {
            if n.song == Some(song.id) {
                more.push(n.text);
            }
        }
        (label, more.join(", "))
    }

    /// A song's part as a control.
    fn part_node(
        &self,
        l: &Layout,
        i: usize,
        song: &Song,
        c: Column,
        model: &Session,
    ) -> AccessNode<Action> {
        let id = part_id(song.id, c);
        let r = self.cell(l, i, c);
        let step = |change: &dyn Fn(f32) -> Song, now: f32| {
            (update(change(now + 0.1)), update(change(now - 0.1)))
        };
        match c {
            Column::Title => {
                AccessNode::new(id, AccessRole::Button, format!("Title: {}", song.title))
                    .at(r)
                    .described("Enter: rename")
                    .on_press()
            }
            Column::Pause if song.crossfade > 0.0 => {
                let (up, down) = step(
                    &|v| Song {
                        crossfade: ((v * 10.0).round() / 10.0).clamp(0.1, 60.0),
                        ..song.clone()
                    },
                    song.crossfade,
                );
                AccessNode::new(
                    id,
                    AccessRole::SpinButton,
                    "Crossfade from the previous song",
                )
                .at(r)
                .value(
                    f64::from(song.crossfade),
                    0.0,
                    60.0,
                    format!("{:.1} s", song.crossfade),
                )
                .described("Enter: type a crossfade (×2) or a pause (2)")
                .on_step(up, down)
                .on_press()
            }
            Column::Pause => {
                let (up, down) = step(
                    &|v| Song {
                        pause: ((v * 10.0).round() / 10.0).clamp(0.0, 60.0),
                        ..song.clone()
                    },
                    song.pause,
                );
                AccessNode::new(id, AccessRole::SpinButton, "Pause before")
                    .at(r)
                    .value(
                        f64::from(song.pause),
                        0.0,
                        60.0,
                        format!("{:.1} s", song.pause),
                    )
                    .described("Enter: type a pause (2) or a crossfade (×2)")
                    .on_step(up, down)
                    .on_press()
            }
            Column::Gain => {
                let (up, down) = step(
                    &|v| Song {
                        gain_db: ((v * 10.0).round() / 10.0).clamp(-24.0, 24.0),
                        ..song.clone()
                    },
                    song.gain_db,
                );
                AccessNode::new(id, AccessRole::SpinButton, "Trim")
                    .at(r)
                    .value(
                        f64::from(song.gain_db),
                        -24.0,
                        24.0,
                        db(f64::from(song.gain_db)),
                    )
                    .described("Before levelling; Enter: type it")
                    .on_step(up, down)
                    .on_press()
            }
            Column::Fades => {
                let text = if song.fade_in == 0.0 && song.fade_out == 0.0 {
                    "Fades: none".to_string()
                } else {
                    format!(
                        "Fades: in {:.1} s, out {:.1} s",
                        song.fade_in, song.fade_out
                    )
                };
                AccessNode::new(id, AccessRole::Button, text)
                    .at(r)
                    .described("Enter: type the fade in and out in seconds (in / out)")
                    .on_press()
            }
            Column::Isrc => {
                let text = if song.isrc.is_empty() {
                    "ISRC: none".to_string()
                } else {
                    format!("ISRC: {}", song.isrc)
                };
                AccessNode::new(id, AccessRole::Button, text)
                    .at(r)
                    .described("Enter: type the International Standard Recording Code")
                    .on_press()
            }
            _ => {
                let names: Vec<String> = song
                    .inserts
                    .iter()
                    .map(|s| {
                        let name = faderframe_project::device_name(&song.inserts, s);
                        if s.bypass {
                            format!("{name} (bypassed)")
                        } else {
                            name
                        }
                    })
                    .collect();
                let mut text = if names.is_empty() {
                    "Inserts: none".to_string()
                } else {
                    format!("Inserts: {}", names.join(", "))
                };
                if model.album_monitor() == Some(song.id) {
                    text.push_str(", heard on the master");
                }
                AccessNode::new(id, AccessRole::Button, text)
                    .at(r)
                    .described("Enter: add, edit, bypass or hear them on the master")
                    .with_menu()
            }
        }
    }

    pub(crate) fn access_nodes(&self, size: Size, model: &Session) -> Vec<AccessNode<Action>> {
        let l = self.layout(size, model);
        let playing = model
            .album_playback()
            .is_some_and(|pb| pb.playing || pb.length > 0.0);
        let button = |b: Button, r: Rect| {
            let mut n = AccessNode::new(
                button_id(b),
                AccessRole::Button,
                Self::button_name(b, model),
            )
            .at(r)
            .fixed();
            if let Some(tip) = self.tooltip(r.center(), size, model) {
                n = n.described(tip);
            }
            if b == Button::AlbumFile {
                n.role = AccessRole::ToggleButton;
                n = n.checked(model.project().album.settings.album_file);
            }
            if Self::is_menu(b) {
                n = n.with_menu();
            } else if let Some(a) = self.button_action(b, model) {
                n = n.on_activate(a);
            } else {
                n = n.on_press();
            }
            let idle = matches!(b, Button::Previous | Button::Next | Button::BackToProject);
            n.disabled(idle && !playing)
        };
        let player_buttons = [
            Button::Previous,
            Button::PlayPause,
            Button::Next,
            Button::BackToProject,
        ];
        let toolbar = l
            .buttons
            .iter()
            .filter(|(b, _)| !player_buttons.contains(b))
            .map(|(b, r)| button(*b, *r));
        let mut out = vec![
            AccessNode::new(access_id(&[TOOLBAR]), AccessRole::Toolbar, "Album toolbar")
                .at(Rect::new(0.0, 0.0, size.w, l.player.y))
                .fixed()
                .with_children(toolbar),
        ];
        let mut player = AccessNode::new(access_id(&[PLAYER]), AccessRole::Group, "Album playback")
            .at(l.player)
            .fixed()
            .with_children(
                l.buttons
                    .iter()
                    .filter(|(b, _)| player_buttons.contains(b))
                    .map(|(b, r)| button(*b, *r)),
            );
        if let (Some(pb), Some((_, length))) = (model.album_playback(), model.album_marks()) {
            let songs = &model.project().album.songs;
            let mut text = format!("{} of {}", duration(pb.position), duration(length));
            if let Some(s) = pb.song.and_then(|i| songs.get(i).map(|s| (i, s))) {
                text.push_str(&format!(", {}. {}", s.0 + 1, s.1.title));
            }
            let seek = |t: f64| Action::Album(AlbumAction::Seek(t.clamp(0.0, length)));
            player = player.child(
                AccessNode::new(access_id(&[SEEK]), AccessRole::Slider, "Album position")
                    .at(l.seek)
                    .fixed()
                    .value(pb.position, 0.0, length, text)
                    .on_step(seek(pb.position + SEEK_STEP), seek(pb.position - SEEK_STEP)),
            );
        }
        out.push(player);
        let songs = &model.project().album.songs;
        let items = songs.iter().enumerate().map(|(i, song)| {
            let (label, more) = Self::song_text(model, i, song);
            let parts = PARTS
                .iter()
                .filter(|c| **c != Column::Pause || i > 0)
                .map(|c| self.part_node(&l, i, song, *c, model));
            AccessNode::new(access_id(&[SONG, song.id.0]), AccessRole::ListItem, label)
                .at(self.row_rect(&l, i))
                .described(more)
                .selected(self.selected == Some(song.id))
                .on_press()
                .with_menu()
                .with_children(parts)
        });
        out.push(
            AccessNode::new(access_id(&[SONGS]), AccessRole::List, "Songs")
                .at(l.list)
                .with_children(items),
        );
        out.push(
            AccessNode::new(
                access_id(&[SUMMARY]),
                AccessRole::Label,
                self.summary(model),
            )
            .at(Rect::new(
                l.footer.x,
                l.footer.y,
                l.folder.x - l.footer.x,
                l.footer.h,
            ))
            .fixed(),
        );
        out.push(
            AccessNode::new(
                access_id(&[FOLDER]),
                AccessRole::Button,
                format!("Export to {}", model.album_folder().display()),
            )
            .at(l.folder)
            .fixed()
            .described("Enter: choose the export folder")
            .on_press(),
        );
        out
    }

    /// The footer's line: the album summed up, or the job running.
    fn summary(&self, model: &Session) -> String {
        if let Some(pr) = model.album_progress() {
            let verb = match pr.task {
                AlbumTask::Analyse => "Analysing",
                AlbumTask::Export => "Exporting",
                AlbumTask::Prepare => "Preparing playback:",
            };
            return format!(
                "{verb} song {} of {}, {:.0} %",
                pr.song + 1,
                pr.songs,
                pr.fraction * 100.0
            );
        }
        let songs = &model.project().album.songs;
        let mut text = format!(
            "{} song{}",
            songs.len(),
            if songs.len() == 1 { "" } else { "s" }
        );
        if let Some(r) = model.album_loudness() {
            text.push_str(&format!(
                ", album {}, highest true peak {}",
                lufs(r.integrated),
                minus(format!("{:.1} dBTP", r.true_peak))
            ));
        }
        text
    }

    /// The control a node id names.
    fn control_of(&self, id: u64, size: Size, model: &Session) -> Option<Control> {
        let l = self.layout(size, model);
        if let Some((b, _)) = l.buttons.iter().find(|(b, _)| button_id(*b) == id) {
            return Some(Control::Button(*b));
        }
        if id == access_id(&[FOLDER]) {
            return Some(Control::Folder);
        }
        let songs = &model.project().album.songs;
        for (i, song) in songs.iter().enumerate() {
            if id == access_id(&[SONG, song.id.0]) {
                return Some(Control::Song(i));
            }
            if let Some(c) = PARTS.iter().find(|c| part_id(song.id, **c) == id) {
                return Some(Control::Part(i, *c));
            }
        }
        None
    }

    /// Scroll so the song `i` is in view.
    pub(crate) fn keep_in_view(&mut self, l: &Layout, i: usize, model: &Session) {
        let top = i as f32 * ROW_H;
        if top < self.scroll {
            self.scroll = top;
        } else if top + ROW_H > self.scroll + l.list.h {
            self.scroll = top + ROW_H - l.list.h;
        }
        self.scroll = self.scroll.clamp(0.0, self.max_scroll(l, model));
    }

    /// The export folder's chooser.
    pub(crate) fn choose_folder(model: &Session, cx: &mut EventCx<'_, Action>) {
        let s = model.project().album.settings.clone();
        cx.request(HostRequest::ChooseFiles {
            choice: FileChoice::Folder {
                title: "Export the Album to".into(),
                initial: Some(model.album_folder()),
            },
            commit: Box::new(move |mut paths| {
                paths.pop().map(|p| {
                    crate::settings_action(faderframe_project::album::AlbumSettings {
                        output: Some(p),
                        ..s.clone()
                    })
                })
            }),
        });
    }

    pub(crate) fn press(
        &mut self,
        id: u64,
        press: AccessPress,
        size: Size,
        model: &Session,
        cx: &mut EventCx<'_, Action>,
    ) -> bool {
        let Some(control) = self.control_of(id, size, model) else {
            return false;
        };
        let l = self.layout(size, model);
        let under = |r: Rect| Point::new(r.x, r.bottom());
        match (control, press) {
            (Control::Button(b), AccessPress::Activate | AccessPress::Menu) => {
                let Some((_, r)) = l.buttons.iter().find(|(x, _)| *x == b) else {
                    return false;
                };
                self.press_button(b, model, under(*r), cx);
            }
            (Control::Folder, AccessPress::Activate) => Self::choose_folder(model, cx),
            (Control::Song(i), AccessPress::Activate) => {
                self.selected = model.project().album.songs.get(i).map(|s| s.id);
                self.keep_in_view(&l, i, model);
                cx.redraw();
            }
            (Control::Song(i), AccessPress::Menu) => {
                self.keep_in_view(&l, i, model);
                let r = self.row_rect(&l, i);
                match Self::row_menu(model, i, under(r)) {
                    Some(req) => cx.request(req),
                    None => return false,
                }
            }
            (Control::Part(i, Column::Inserts), AccessPress::Menu | AccessPress::Activate) => {
                let Some(song) = model.project().album.songs.get(i) else {
                    return false;
                };
                cx.request(Self::inserts_menu(
                    model,
                    song,
                    under(self.cell(&l, i, Column::Inserts)),
                ));
            }
            (Control::Part(i, c), AccessPress::Activate) => {
                match self.text_input(model, &l, i, c) {
                    Some(req) => cx.request(req),
                    None => return false,
                }
            }
            _ => return false,
        }
        true
    }

    /// What finished or changed by itself: a job (with the album's
    /// loudness after an analysis), songs failing, the song playing.
    pub(crate) fn news(&mut self, model: &Session) -> Vec<String> {
        let songs = &model.project().album.songs;
        let pb = model.album_playback();
        let now = Told {
            task: model.album_progress().map(|p| p.task),
            failed: songs
                .iter()
                .filter(|s| model.album_error(s.id).is_some())
                .map(|s| s.id)
                .collect(),
            playing: pb.filter(|p| p.playing).and_then(|p| p.song),
        };
        let Some(before) = self.told.replace(now.clone()) else {
            return Vec::new();
        };
        let mut out = Vec::new();
        if let (Some(task), None) = (before.task, now.task) {
            out.push(match task {
                AlbumTask::Analyse => match model.album_loudness() {
                    Some(r) => format!(
                        "Analysis finished: album {}, highest true peak {}",
                        lufs(r.integrated),
                        minus(format!("{:.1} dBTP", r.true_peak))
                    ),
                    None => "Analysis finished".into(),
                },
                AlbumTask::Export => match model.album_export() {
                    Some(e) => format!("Export finished: {}", e.folder.display()),
                    None => "Export finished".into(),
                },
                AlbumTask::Prepare => "The album is ready to play".into(),
            });
        } else if let (None, Some(task)) = (before.task, now.task) {
            out.push(
                match task {
                    AlbumTask::Analyse => "Analysing the album",
                    AlbumTask::Export => "Exporting the album",
                    AlbumTask::Prepare => "Preparing the album to play",
                }
                .into(),
            );
        }
        for (i, s) in songs.iter().enumerate() {
            if now.failed.contains(&s.id)
                && !before.failed.contains(&s.id)
                && let Some(e) = model.album_error(s.id)
            {
                out.push(format!("Song {} failed: {e}", i + 1));
            }
        }
        if now.playing != before.playing
            && let Some(i) = now.playing
            && let Some(s) = songs.get(i)
        {
            out.push(format!("Playing {}. {}", i + 1, s.title));
        }
        out
    }
}
