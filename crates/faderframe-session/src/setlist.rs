//! The setlist and show mode (see [`faderframe_project::setlist`]).
//!
//! Editing the list ([`SetlistOp`]) is one undo step each. Show mode
//! ([`ShowOp`]) stands on a song (the playhead at its start); playing it,
//! the engine stops on its exact last frame ([`EngineController::stop_at`])
//! unless it plays on; then the show stands on the next song, and plays it
//! after the gap when the song says so. Next and Previous move along the
//! list (while playing: the next song at once). Show mode is not saved.
//!
//! [`EngineController::stop_at`]: faderframe_engine::EngineController::stop_at

use crate::{NoticeLevel, Result, Session, SessionError, TransportAction};
use faderframe_core::SectionId;
use faderframe_project::Command;
use faderframe_project::setlist::{AfterSong, SetSong, Setlist};
use faderframe_timeline::MusicalTime;
use std::time::{Duration, Instant};

/// An edit of the setlist.
#[derive(Clone, Debug, PartialEq)]
pub enum SetlistOp {
    /// Every section a song, in project order (the list replaced).
    FromSections,
    /// A song over a range.
    Add {
        name: String,
        start: MusicalTime,
        end: MusicalTime,
    },
    /// A section as a song.
    AddSection(SectionId),
    Remove(usize),
    Move {
        from: usize,
        to: usize,
    },
    Rename(usize, String),
    Then(usize, AfterSong),
    Notes(usize, String),
    Clear,
}

/// Show mode's controls.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ShowOp {
    Enter,
    Leave,
    /// Stand on a song (stopped: ready to play it).
    Cue(usize),
    /// Play a song from its start.
    Go(usize),
    Next,
    Previous,
    /// Play the song stood on, or stop.
    PlayStop,
}

/// Show mode as it runs (not saved).
#[derive(Debug, Default)]
pub struct ShowState {
    pub on: bool,
    /// The song stood on or playing.
    pub current: usize,
    /// When the show began.
    pub since: Option<Instant>,
    /// A song to play at a moment (after a song's gap).
    pending: Option<(usize, Instant)>,
    /// The stop the engine has been asked for (sample).
    stop_armed: Option<i64>,
    /// The song's end was seen while playing (a stop on it is the song's).
    playing_song: Option<usize>,
}

impl Session {
    /// The setlist.
    pub fn setlist(&self) -> &Setlist {
        &self.project.setlist
    }

    /// Show mode's state.
    pub fn show(&self) -> &ShowState {
        &self.show
    }

    /// Is show mode on?
    pub fn show_mode(&self) -> bool {
        self.show.on
    }

    /// Seconds until a pending song starts.
    pub fn show_countdown(&self) -> Option<f32> {
        self.show
            .pending
            .map(|(_, at)| at.saturating_duration_since(Instant::now()).as_secs_f32())
    }

    /// A song's length in seconds.
    pub fn song_seconds(&self, song: &SetSong) -> f64 {
        let rate = f64::from(self.project.sample_rate.max(1));
        let tl = &self.project.timeline;
        (tl.to_samples(song.end, rate) - tl.to_samples(song.start, rate)).max(0) as f64 / rate
    }

    /// Edit the setlist (one undo step).
    pub(crate) fn edit_setlist(&mut self, op: SetlistOp) -> Result<()> {
        let mut list = self.project.setlist.clone();
        let label = match op {
            SetlistOp::FromSections => {
                let mut sections = self.project.sections.clone();
                sections.sort_by_key(|s| s.start);
                if sections.is_empty() {
                    return Err(SessionError::Other(
                        "the project has no sections to make songs of".into(),
                    ));
                }
                list.songs = sections
                    .into_iter()
                    .map(|s| SetSong {
                        name: s.name,
                        start: s.start,
                        end: s.end,
                        then: AfterSong::Stop,
                        notes: String::new(),
                    })
                    .collect();
                "Setlist from Sections"
            }
            SetlistOp::Add { name, start, end } => {
                if end <= start {
                    return Err(SessionError::Other(
                        "select a range of the song first".into(),
                    ));
                }
                list.songs.push(SetSong {
                    name,
                    start,
                    end,
                    then: AfterSong::Stop,
                    notes: String::new(),
                });
                "Add Song"
            }
            SetlistOp::AddSection(id) => {
                let Some(s) = self.project.sections.iter().find(|s| s.id == id) else {
                    return Ok(());
                };
                list.songs.push(SetSong {
                    name: s.name.clone(),
                    start: s.start,
                    end: s.end,
                    then: AfterSong::Stop,
                    notes: String::new(),
                });
                "Add Song"
            }
            SetlistOp::Remove(i) if i < list.songs.len() => {
                list.songs.remove(i);
                "Remove Song"
            }
            SetlistOp::Move { from, to } if from < list.songs.len() => {
                let s = list.songs.remove(from);
                list.songs.insert(to.min(list.songs.len()), s);
                "Move Song"
            }
            SetlistOp::Rename(i, name) if i < list.songs.len() => {
                list.songs[i].name = name;
                "Rename Song"
            }
            SetlistOp::Then(i, then) if i < list.songs.len() => {
                list.songs[i].then = then;
                "Edit Setlist"
            }
            SetlistOp::Notes(i, notes) if i < list.songs.len() => {
                list.songs[i].notes = notes;
                "Song Notes"
            }
            SetlistOp::Clear => {
                list.songs.clear();
                "Clear Setlist"
            }
            _ => return Ok(()),
        };
        self.batch(
            label,
            vec![Command::SetSetlist {
                setlist: Box::new(list),
            }],
        )?;
        let n = self.project.setlist.songs.len();
        if self.show.current >= n {
            self.show.current = n.saturating_sub(1);
        }
        Ok(())
    }

    /// Show mode (see the module docs).
    pub(crate) fn show_op(&mut self, op: ShowOp) -> Result<()> {
        let n = self.project.setlist.songs.len();
        match op {
            ShowOp::Enter => {
                if n == 0 {
                    return Err(SessionError::Other(
                        "the setlist is empty: add songs first (Songs from Sections)".into(),
                    ));
                }
                self.show.on = true;
                self.show.since = Some(Instant::now());
                self.show.pending = None;
                let current = self.show.current.min(n - 1);
                self.cue_song(current)?;
                self.workspace_action(crate::WorkspaceAction::ShowView(
                    faderframe_workspace::ViewId::setlist(),
                ))?;
                // The show on a screen of its own.
                self.ui_requests.push(crate::UiRequest::FullScreen(
                    faderframe_workspace::ViewId::setlist(),
                ));
            }
            ShowOp::Leave => {
                self.show.on = false;
                self.show.pending = None;
                self.disarm_stop()?;
            }
            ShowOp::Cue(i) if i < n => self.cue_song(i)?,
            ShowOp::Go(i) if i < n => {
                self.cue_song(i)?;
                self.transport_action(TransportAction::Play)?;
                self.show.playing_song = Some(i);
            }
            ShowOp::Next | ShowOp::Previous if n > 0 => {
                let i = match op {
                    ShowOp::Next => (self.show.current + 1).min(n - 1),
                    _ => self.show.current.saturating_sub(1),
                };
                if self.transport.playing {
                    self.show_op(ShowOp::Go(i))?;
                } else {
                    self.cue_song(i)?;
                }
            }
            ShowOp::PlayStop => {
                if self.transport.playing {
                    self.show.playing_song = None;
                    self.disarm_stop()?;
                    self.transport_action(TransportAction::Stop)?;
                    // Back to the song's start, ready.
                    self.cue_song(self.show.current)?;
                } else if n > 0 {
                    let i = self.show.current.min(n - 1);
                    self.show.pending = None;
                    // From where the playhead stands inside the song.
                    let song = self.project.setlist.songs[i].clone();
                    let at = self.playhead();
                    if at < song.start || at >= song.end {
                        self.cue_song(i)?;
                    }
                    self.transport_action(TransportAction::Play)?;
                    self.show.playing_song = Some(i);
                }
            }
            _ => {}
        }
        self.revision += 1;
        Ok(())
    }

    /// Stand on song `i`: the playhead at its start.
    fn cue_song(&mut self, i: usize) -> Result<()> {
        let Some(song) = self.project.setlist.songs.get(i).cloned() else {
            return Ok(());
        };
        self.show.current = i;
        self.show.pending = None;
        self.disarm_stop()?;
        self.transport_action(TransportAction::Locate(song.start))?;
        Ok(())
    }

    fn disarm_stop(&mut self) -> Result<()> {
        if self.show.stop_armed.take().is_some() {
            self.engine.stop_at(None)?;
        }
        Ok(())
    }

    /// Run the show (from the session tick): arm the song's stop, follow
    /// its end, start a pending song.
    pub(crate) fn tick_show(&mut self) {
        if !self.show.on {
            return;
        }
        if let Err(e) = self.run_show() {
            self.notify(NoticeLevel::Error, format!("show: {e}"));
        }
    }

    fn run_show(&mut self) -> Result<()> {
        let songs = self.project.setlist.songs.clone();
        let n = songs.len();
        if n == 0 {
            self.show.on = false;
            return Ok(());
        }
        let i = self.show.current.min(n - 1);
        let song = songs[i].clone();
        let rate = f64::from(self.project.sample_rate.max(1));
        let end = self.project.timeline.to_samples(song.end, rate);
        let position = self.transport.position;
        // A song to start now?
        if let Some((next, at)) = self.show.pending
            && Instant::now() >= at
        {
            self.show.pending = None;
            return self.show_op(ShowOp::Go(next));
        }
        if self.transport.playing {
            self.show.playing_song = Some(i);
            match song.then {
                AfterSong::Continue => {
                    self.disarm_stop()?;
                    // Past its end: the next song is the current one (when
                    // it is not where playback went on, it is jumped to).
                    if position >= end && i + 1 < n {
                        let next = songs[i + 1].clone();
                        let next_start = self.project.timeline.to_samples(next.start, rate);
                        if (next_start - end).abs() > (rate * 0.05) as i64 {
                            return self.show_op(ShowOp::Go(i + 1));
                        }
                        self.show.current = i + 1;
                        self.revision += 1;
                    }
                }
                AfterSong::Stop | AfterSong::Next { .. } => {
                    if self.show.stop_armed != Some(end) && position < end {
                        self.engine.stop_at(Some(end))?;
                        self.show.stop_armed = Some(end);
                    }
                }
            }
            return Ok(());
        }
        // Stopped on the song's last frame: it is over.
        let ended = self.show.playing_song == Some(i)
            && self.show.stop_armed == Some(end)
            && (position - end).abs() <= 1;
        if ended {
            self.show.stop_armed = None;
            self.show.playing_song = None;
            if i + 1 < n {
                self.cue_song(i + 1)?;
                if let AfterSong::Next { gap } = song.then {
                    self.show.pending = Some((
                        i + 1,
                        Instant::now() + Duration::from_secs_f32(gap.max(0.0)),
                    ));
                }
            } else {
                self.notify(NoticeLevel::Info, "The show is over".to_string());
            }
            self.revision += 1;
        }
        Ok(())
    }
}
