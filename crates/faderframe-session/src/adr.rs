//! ADR in the session: the cue list (`faderframe_project::adr`) made from
//! the transcript or the edit selection, the talent's beeps on a track of
//! their own, a cue rehearsed or recorded (pre-roll with the beeps, the
//! cue as the punch range on its track, stopping after the post-roll),
//! what goes over the picture (streamer, punch, the line) and takes rated.

use crate::{Action, NoticeLevel, Result, Session, SessionError, TransportAction};
use faderframe_core::{AdrCueId, AudioSourceId, ClipId, TrackId};
use faderframe_project::adr::{Adr, AdrCue, AdrSettings};
use faderframe_project::{
    AudioClip, AudioSource, Clip, ClipContent, ClipFades, Command, MusicalRange, SourceSpec,
    StretchSettings, Track, TrackColor, TrackKind,
};
use faderframe_timeline::MusicalTime;

/// The beeps' track's name.
pub const BEEPS_TRACK: &str = "ADR Beeps";
/// A beep's length (seconds).
const BEEP: f64 = 0.1;

/// ADR edits and runs.
#[derive(Clone, Debug, PartialEq)]
pub enum AdrOp {
    /// Cues from the lyric lines (the transcript), their takes on `track`;
    /// lines a cue already covers are left.
    FromTranscript {
        track: Option<TrackId>,
    },
    /// A cue from `start` to `end` with the line `text`.
    Add {
        start: MusicalTime,
        end: MusicalTime,
        text: String,
        track: Option<TrackId>,
    },
    /// A cue changed (by id).
    Set(AdrCue),
    Remove(AdrCueId),
    Settings(AdrSettings),
    /// The beeps before every cue on the beeps' track (made once).
    MakeBeeps,
    /// Play a cue from its pre-roll (`record`: recording its track).
    Run {
        cue: AdrCueId,
        record: bool,
    },
    /// Rate take `take` of take folder `clip`, 0–5 stars.
    RateTake {
        clip: ClipId,
        take: usize,
        rating: u8,
    },
}

/// A cue playing.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct AdrRun {
    pub(crate) cue: AdrCueId,
    pub(crate) record: bool,
    /// Where it stops (samples).
    stop_at: i64,
    started: bool,
}

/// What goes over the picture at a position.
#[derive(Clone, Debug, PartialEq)]
pub struct AdrOverlay {
    pub number: String,
    pub character: String,
    pub text: String,
    /// The streamer's way across the picture (0–1), before the line.
    pub streamer: Option<f32>,
    /// The punch: the line starts now (two frames).
    pub punch: bool,
    /// Beeps still to come before the line.
    pub beeps_left: Option<u8>,
    /// The line is being said (between start and end).
    pub speaking: bool,
    pub recording: bool,
}

impl Session {
    pub fn adr(&self) -> &Adr {
        &self.project.adr
    }

    /// The cue playing, and whether it records.
    pub fn adr_running(&self) -> Option<(AdrCueId, bool)> {
        self.adr_run.map(|r| (r.cue, r.record))
    }

    fn set_adr(&mut self, adr: Adr) -> Result<()> {
        self.edit(Command::SetAdr { adr: Box::new(adr) })
    }

    pub(crate) fn adr_op(&mut self, op: AdrOp) -> Result<()> {
        match op {
            AdrOp::FromTranscript { track } => {
                let mut adr = self.project.adr.clone();
                let covered = |a: &Adr, s: MusicalTime, e: MusicalTime| {
                    a.cues.iter().any(|c| c.start < e && c.end > s)
                };
                let mut n = 0;
                for l in self.project.lyrics.clone() {
                    if l.text.trim().is_empty() || covered(&adr, l.start, l.end) {
                        continue;
                    }
                    adr.cues.push(AdrCue {
                        id: self.project.ids.allocate(),
                        number: String::new(),
                        character: String::new(),
                        text: l.text.trim().to_string(),
                        start: l.start,
                        end: l.end,
                        track,
                        note: String::new(),
                        done: false,
                    });
                    n += 1;
                }
                if n == 0 {
                    self.notify(
                        NoticeLevel::Info,
                        "No lines to cue: transcribe the dialogue first (a clip's menu → Transcribe)",
                    );
                    return Ok(());
                }
                self.set_adr(adr)?;
                self.notify(
                    NoticeLevel::Info,
                    format!(
                        "{n} cue{} from the transcript",
                        if n == 1 { "" } else { "s" }
                    ),
                );
                Ok(())
            }
            AdrOp::Add {
                start,
                end,
                text,
                track,
            } => {
                if end <= start {
                    return Err(SessionError::Other(
                        "a cue needs a length: select a range in the arranger".into(),
                    ));
                }
                let mut adr = self.project.adr.clone();
                adr.cues.push(AdrCue {
                    id: self.project.ids.allocate(),
                    number: String::new(),
                    character: String::new(),
                    text,
                    start,
                    end,
                    track,
                    note: String::new(),
                    done: false,
                });
                self.set_adr(adr)
            }
            AdrOp::Set(cue) => {
                let mut adr = self.project.adr.clone();
                let Some(c) = adr.cues.iter_mut().find(|c| c.id == cue.id) else {
                    return Err(SessionError::Other("no such cue".into()));
                };
                *c = cue;
                self.set_adr(adr)
            }
            AdrOp::Remove(id) => {
                let mut adr = self.project.adr.clone();
                adr.cues.retain(|c| c.id != id);
                self.set_adr(adr)
            }
            AdrOp::Settings(settings) => {
                let mut adr = self.project.adr.clone();
                adr.settings = settings;
                self.set_adr(adr)
            }
            AdrOp::MakeBeeps => self.make_beeps(),
            AdrOp::Run { cue, record } => self.run_cue(cue, record),
            AdrOp::RateTake { clip, take, rating } => {
                let Some(c) = self.project.clip(clip) else {
                    return Err(SessionError::Other("no such clip".into()));
                };
                let start = c.start;
                let ClipContent::Takes(folder) = &c.content else {
                    return Err(SessionError::Other("only takes are rated".into()));
                };
                let mut folder = folder.clone();
                let Some(t) = folder.takes.get_mut(take) else {
                    return Err(SessionError::Other("no such take".into()));
                };
                t.rating = rating.min(5);
                self.edit(Command::SetClipContent {
                    clip,
                    start,
                    content: Box::new(ClipContent::Takes(folder)),
                })
            }
        }
    }

    /// The beeps' track: the beeps before every cue, made again.
    fn make_beeps(&mut self) -> Result<()> {
        let adr = self.project.adr.clone();
        if adr.cues.is_empty() {
            return Err(SessionError::Other("there are no cues to beep for".into()));
        }
        let s = adr.settings;
        let rate = self.project.sample_rate;
        let mut commands = Vec::new();
        let existing = self
            .project
            .tracks
            .iter()
            .find(|t| t.name == BEEPS_TRACK && t.kind == TrackKind::Audio)
            .map(|t| (t.id, t.clips.clone()));
        let track = match existing {
            Some((id, clips)) => {
                for c in clips {
                    commands.push(Command::RemoveClip { clip: c });
                }
                id
            }
            None => {
                let id: TrackId = self.project.ids.allocate();
                let mut t = Track::new(id, TrackKind::Audio, BEEPS_TRACK, TrackColor::palette(5));
                t.layout = faderframe_core::ChannelLayout::Mono;
                let index = self
                    .project
                    .tracks
                    .iter()
                    .position(|t| t.kind == TrackKind::Master)
                    .unwrap_or(self.project.tracks.len());
                commands.push(Command::AddTrack {
                    track: Box::new(t),
                    index,
                });
                id
            }
        };
        let source: AudioSourceId = self.project.ids.allocate();
        commands.push(Command::AddSource {
            source: Box::new(AudioSource {
                id: source,
                name: format!("ADR beep {:.0} Hz", s.beep_hz),
                spec: SourceSpec::Generated {
                    generator: faderframe_audio_files::GeneratorSpec::Sine {
                        frequency: f64::from(s.beep_hz),
                        seconds: BEEP,
                        amplitude: 0.25,
                    },
                },
            }),
        });
        let r = rate as f64;
        let len = (BEEP * r) as i64;
        let fade = (r * 0.004) as i64;
        for c in &adr.cues {
            let start = self.project.timeline.to_samples(c.start, r);
            for k in 1..=i64::from(s.beeps) {
                let at = start - k * rate as i64;
                if at < 0 {
                    continue;
                }
                let id: ClipId = self.project.ids.allocate();
                commands.push(Command::AddClip {
                    clip: Box::new(Clip {
                        id,
                        track,
                        name: format!("{} beep", c.number),
                        color: None,
                        start: self.project.timeline.to_musical(at, r),
                        muted: false,
                        content: ClipContent::Audio(AudioClip {
                            source,
                            source_offset: 0,
                            length: len,
                            gain_db: 0.0,
                            fades: ClipFades {
                                fade_in: fade,
                                fade_out: fade,
                                ..Default::default()
                            },
                            stretch: StretchSettings::Off,
                            reversed: false,
                            warp: None,
                            pitch: None,
                            effects: None,
                            spectral: None,
                        }),
                    }),
                });
            }
        }
        self.edit(Command::Batch {
            label: "ADR Beeps".into(),
            commands,
        })
    }

    /// Play cue `id` from its pre-roll; with `record` record its track
    /// over the cue (the punch range).
    fn run_cue(&mut self, id: AdrCueId, record: bool) -> Result<()> {
        let Some(cue) = self.project.adr.cue(id).cloned() else {
            return Err(SessionError::Other("no such cue".into()));
        };
        let s = self.project.adr.settings;
        let r = self.project.sample_rate as f64;
        let start = self.project.timeline.to_samples(cue.start, r);
        let end = self.project.timeline.to_samples(cue.end, r);
        let lead = (f64::from(s.beeps).max(s.streamer) + s.preroll) * r;
        let from = (start - lead as i64).max(0);
        if self.transport.playing {
            self.dispatch(Action::Transport(TransportAction::Stop))?;
        }
        if record {
            let Some(track) = cue.track else {
                return Err(SessionError::Other(format!(
                    "cue {}: choose the track its takes go on",
                    cue.number
                )));
            };
            let mut commands = vec![Command::SetPunch {
                range: MusicalRange::new(cue.start, cue.end),
                enabled: true,
            }];
            if self.project.track(track).is_some_and(|t| !t.record_arm) {
                commands.push(Command::SetTrackRecordArm { track, on: true });
            }
            self.edit(Command::Batch {
                label: format!("Record Cue {}", cue.number),
                commands,
            })?;
        }
        let at = self.project.timeline.to_musical(from, r);
        self.dispatch(Action::Transport(TransportAction::Locate(at)))?;
        self.dispatch(Action::Transport(if record {
            TransportAction::ToggleRecord
        } else {
            TransportAction::Play
        }))?;
        self.adr_run = Some(AdrRun {
            cue: id,
            record,
            stop_at: end + (s.postroll * r) as i64,
            started: false,
        });
        self.revision += 1;
        Ok(())
    }

    /// Stop a cue run after its post-roll (from the session tick).
    pub(crate) fn tick_adr(&mut self) {
        let Some(mut run) = self.adr_run else {
            return;
        };
        if self.transport.playing {
            run.started = true;
            self.adr_run = Some(run);
            if self.transport.position >= run.stop_at {
                let _ = self.dispatch(Action::Transport(TransportAction::Stop));
                self.adr_run = None;
            }
        } else if run.started {
            // Stopped by hand.
            self.adr_run = None;
        }
    }

    /// What goes over the picture at `position` (samples): the cue about
    /// to start or being said, while playing (or a cue run).
    pub fn adr_overlay(&self, position: i64) -> Option<AdrOverlay> {
        let adr = &self.project.adr;
        let s = adr.settings;
        if !s.on_picture || (!self.transport.playing && self.adr_run.is_none()) {
            return None;
        }
        let r = self.project.sample_rate as f64;
        let lead = f64::from(s.beeps).max(s.streamer);
        let cue = adr.cues.iter().find(|c| {
            let a = self.project.timeline.to_samples(c.start, r);
            let b = self.project.timeline.to_samples(c.end, r);
            position >= a - (lead * r) as i64 && position < b
        })?;
        let a = self.project.timeline.to_samples(cue.start, r);
        let before = (a - position) as f64 / r;
        let frame = 1.0 / self.timecode().rate.fps();
        Some(AdrOverlay {
            number: cue.number.clone(),
            character: cue.character.clone(),
            text: cue.text.clone(),
            streamer: (before > 0.0 && before <= s.streamer)
                .then(|| (1.0 - before / s.streamer) as f32),
            punch: before <= 0.0 && -before < 2.0 * frame,
            beeps_left: (before > 0.0).then(|| (before.ceil() as u8).min(s.beeps)),
            speaking: before <= 0.0,
            recording: self.adr_run.is_some_and(|r| r.record && r.cue == cue.id),
        })
    }

    /// The takes recorded for a cue: (clip, take index, its name, rating)
    /// of the take folders on its track over the cue.
    pub fn adr_takes(&self, id: AdrCueId) -> Vec<(ClipId, usize, String, u8)> {
        let Some(cue) = self.project.adr.cue(id) else {
            return Vec::new();
        };
        let Some(t) = cue.track.and_then(|t| self.project.track(t)) else {
            return Vec::new();
        };
        let p = &self.project;
        let mut out = Vec::new();
        for c in t.clips.iter().filter_map(|c| p.clips.get(c)) {
            let end = c.end(&p.timeline, p.sample_rate);
            if c.start >= cue.end || end <= cue.start {
                continue;
            }
            if let ClipContent::Takes(f) = &c.content {
                for (i, take) in f.takes.iter().enumerate() {
                    out.push((c.id, i, take.name.clone(), take.rating));
                }
            }
        }
        out
    }
}
