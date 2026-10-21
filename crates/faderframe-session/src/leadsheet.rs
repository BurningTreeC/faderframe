//! Lead sheets from a recording: the melody of a clip, of several clips or
//! of a whole track — or a score of several tracks, a staff each — (audio
//! heard by the pitch analysis in a thread, take folders by their comp,
//! MIDI read), the bars it sits in, the chords — the chord track's;
//! without one, what the other tracks' MIDI plays; without that, what the
//! other audio tracks play, heard by basic-pitch; for a score with none of
//! those, what its own staves play together —, the key, the tempo and the
//! lyrics (under the staff that sings them), written out by
//! [`faderframe_leadsheet`] and engraved for the Lead Sheet view, exported
//! as MusicXML or PDF.
//!
//! A staff's clips make one line on one timeline: the gaps between them
//! are rests, and where clips overlap the one on top (the later) is
//! written.
//!
//! Guitar tab ([`SheetStyle`]): the fretted staves (all of them, or in a
//! score those whose tracks are named like guitars, basses or ukuleles)
//! keep their chords — their audio heard by basic-pitch, not as a melody —
//! and are written as notes with their tablature under them, or as
//! tablature alone, on the tuning and capo chosen (a bass on a bass's
//! strings); exported also as plain-text tab.

use crate::to_midi::{Found, harmony, melody};
use crate::{NoticeLevel, Result, Session, SessionError};
use faderframe_core::{ClipId, TrackId};
use faderframe_engine::Source;
pub use faderframe_leadsheet::Grid;
use faderframe_leadsheet::engrave::{Layout, Page, engrave};
use faderframe_leadsheet::{
    Bar, ChordAt, Input, LeadSheet, Line, Note, Staff, StaffTab, TUNINGS, Tuning,
};
use faderframe_project::{Clip, ClipContent, Project};
use faderframe_timeline::MusicalTime;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::thread::JoinHandle;

/// What a lead sheet is of.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LeadSheetOf {
    Clip(ClipId),
    /// All of a track's clips, the whole song.
    Track(TrackId),
    /// These clips: those of a track on one staff, a staff a track (top to
    /// bottom as the arranger lists the tracks).
    Clips(Vec<ClipId>),
    /// These tracks' clips, a staff each, top to bottom as given (a score).
    Tracks(Vec<TrackId>),
}

/// How the staves are written.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Notation {
    /// Notes on staves.
    #[default]
    Standard,
    /// Notes with their tablature under them.
    WithTab,
    /// Tablature alone, its rhythm under it.
    Tab,
}

/// The notation, and a tab's tuning and capo.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SheetStyle {
    pub notation: Notation,
    /// Into [`faderframe_leadsheet::TUNINGS`].
    pub tuning: usize,
    /// Frets from the nut (0: none).
    pub capo: u8,
}

impl SheetStyle {
    pub fn tab(&self) -> bool {
        self.notation != Notation::Standard
    }

    pub fn tuning(&self) -> Tuning {
        TUNINGS.get(self.tuning).copied().unwrap_or(TUNINGS[0])
    }
}

/// Tracks named like fretted instruments.
fn fretted(name: &str) -> bool {
    let n = name.to_lowercase();
    [
        "guit", "gtr", "bass", "uke", "ukulele", "banjo", "mandolin", "strum",
    ]
    .iter()
    .any(|w| n.contains(w))
        && !drum_track(name)
}

/// A lead sheet made: what of, how it was written and its pages.
#[derive(Clone, Debug)]
pub struct LeadSheetDoc {
    pub of: LeadSheetOf,
    pub grid: Grid,
    pub style: SheetStyle,
    pub sheet: LeadSheet,
    pub pages: Vec<Page>,
}

impl LeadSheetDoc {
    /// Its parts' names, top to bottom ("Voice", "Voice, Bass").
    pub fn parts(&self) -> String {
        self.sheet
            .parts
            .iter()
            .map(|p| p.name.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    }
}

/// A staff's track and the clips written on it (by start).
struct StaffClips {
    track: TrackId,
    clips: Vec<Clip>,
}

impl StaffClips {
    /// The staff's line: its clips' notes (MIDI read, audio as heard),
    /// where clips overlap the later one's.
    fn notes(&self, p: &Project, heard: &BTreeMap<ClipId, Vec<Note>>) -> Vec<Note> {
        let span = |c: &Clip| {
            (
                c.start.quarters(),
                c.end(&p.timeline, p.sample_rate).quarters(),
            )
        };
        let mut out = Vec::new();
        for (i, c) in self.clips.iter().enumerate() {
            let notes = match &c.content {
                ClipContent::Midi(_) => midi_notes(c),
                _ => heard.get(&c.id).cloned().unwrap_or_default(),
            };
            // Where the clips on top of it play.
            let covered: Vec<(f64, f64)> = self.clips[i + 1..]
                .iter()
                .map(span)
                .filter(|(a, b)| b > a)
                .collect();
            for mut n in notes {
                if covered.iter().any(|(a, b)| n.start >= *a && n.start < *b) {
                    continue;
                }
                for (a, _) in &covered {
                    if *a > n.start {
                        n.end = n.end.min(*a);
                    }
                }
                if n.end > n.start {
                    out.push(n);
                }
            }
        }
        out
    }
}

/// Audio to listen to: a stretch of a source (project-rate frames).
struct Listen {
    source: Source,
    from: i64,
    span: i64,
}

/// Where audio heard plays: an audio clip (its warp followed), or a
/// piece of a take folder's comp from a frame of the folder on.
#[derive(Clone, Copy, Debug)]
enum Place {
    Clip(ClipId),
    Piece { clip: ClipId, at: i64 },
}

impl Place {
    fn clip(self) -> ClipId {
        match self {
            Place::Clip(c) | Place::Piece { clip: c, .. } => c,
        }
    }

    /// Seconds into what was listened to → quarters on the timeline.
    fn quarters(self, p: &Project, c: &Clip, secs: f64) -> f64 {
        match self {
            Place::Clip(_) => clip_quarters(p, c, secs),
            Place::Piece { at, .. } => {
                let rate = p.sample_rate as f64;
                let base = p.timeline.to_samples(c.start, rate);
                let length = c.as_takes().map_or(0, |f| f.length);
                let f = at + (secs * rate).round() as i64;
                p.timeline
                    .to_musical(base + f.clamp(0, length), rate)
                    .quarters()
            }
        }
    }
}

/// What a listening thread hears, in the order it was asked: the
/// staves' notes (a melody, or a fretted staff's chords) and the chords'
/// notes (seconds into what it listened to; None: unreadable).
type Heard = (Vec<Option<Vec<Found>>>, Vec<Option<Vec<Found>>>);

/// How audio is heard as notes (the melody, or chords).
type Listener = dyn Fn(&[f32], f64) -> Option<Vec<Found>>;

/// A listening job: what of, the grid, where what it hears plays (the
/// melodies', the chords'), the thread.
struct Job {
    of: LeadSheetOf,
    grid: Grid,
    /// The tracks heard as chords (tab staves).
    tab: Vec<TrackId>,
    melody: Vec<Place>,
    chords: Vec<Place>,
    thread: JoinHandle<Heard>,
}

/// What was heard for a lead sheet (kept, so another grid does not listen
/// again): the audio clips' notes by clip, the chords heard, and which
/// tracks were heard as chords.
#[derive(Clone)]
struct Kept {
    of: LeadSheetOf,
    tab: Vec<TrackId>,
    heard: BTreeMap<ClipId, Vec<Note>>,
    chords: Vec<ChordAt>,
}

#[derive(Default)]
pub(crate) struct LeadSheets {
    job: Option<Job>,
    kept: Option<Kept>,
    pub(crate) doc: Option<LeadSheetDoc>,
    pub(crate) style: SheetStyle,
}

/// Today as YYYY-MM-DD (UTC).
fn today() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs()) as i64;
    let days = secs.div_euclid(86_400);
    // Civil date from days since 1970-01-01 (Howard Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}-{m:02}-{d:02}")
}

/// A MIDI clip's notes in quarters on the timeline (those it plays:
/// unmuted, cut at its end).
fn midi_notes(c: &Clip) -> Vec<Note> {
    match &c.content {
        ClipContent::Midi(m) => m
            .notes
            .iter()
            .filter(|n| !n.muted && n.start < m.length)
            .map(|n| Note {
                start: (c.start + n.start).quarters(),
                end: (c.start + (n.start + n.length).min(m.length)).quarters(),
                key: n.key,
            })
            .collect(),
        _ => Vec::new(),
    }
}

/// Seconds into an audio clip's part → quarters on the timeline.
fn clip_quarters(p: &Project, c: &Clip, secs: f64) -> f64 {
    let Some(a) = c.as_audio() else {
        return c.start.quarters();
    };
    let rate = p.sample_rate as f64;
    let base = p.timeline.to_samples(c.start, rate);
    let f = a.source_offset + (secs * rate).round() as i64;
    let out = match &a.warp {
        Some(w) => w.output_of(a.source_offset, a.length, f),
        None => f - a.source_offset,
    };
    p.timeline
        .to_musical(base + out.clamp(0, a.length), rate)
        .quarters()
}

/// Tracks whose name says they are drums (no chords in them).
fn drum_track(name: &str) -> bool {
    let n = name.to_lowercase();
    ["drum", "kick", "snare", "hat", "perc", "cymbal", "tom"]
        .iter()
        .any(|w| n.contains(w))
}

/// The staff the lyrics go under: the first whose track is named like a
/// voice, else the top one.
fn singer(names: &[String]) -> usize {
    names
        .iter()
        .position(|n| {
            let n = n.to_lowercase();
            ["vocal", "vox", "voice", "sing", "choir", "lyric", "melody"]
                .iter()
                .any(|w| n.contains(w))
        })
        .unwrap_or(0)
}

impl Session {
    /// How lead sheets are written.
    pub fn sheet_style(&self) -> SheetStyle {
        self.lead_sheets.style
    }

    /// Write lead sheets so (the one made, again).
    pub fn set_sheet_style(&mut self, style: SheetStyle) -> Result<()> {
        if style == self.lead_sheets.style {
            return Ok(());
        }
        self.lead_sheets.style = style;
        self.revision += 1;
        match self
            .lead_sheets
            .doc
            .as_ref()
            .map(|d| (d.of.clone(), d.grid))
        {
            Some((of, grid)) => self.make_lead_sheet(of, grid),
            None => Ok(()),
        }
    }

    /// Guitar tab of `of`: as the style says, with tablature (notes and
    /// tab when it was plain notation).
    pub fn make_tab(&mut self, of: LeadSheetOf) -> Result<()> {
        if !self.lead_sheets.style.tab() {
            self.lead_sheets.style.notation = Notation::WithTab;
        }
        let grid = self.lead_sheets.doc.as_ref().map_or(Grid::Auto, |d| d.grid);
        self.make_lead_sheet(of, grid)
    }

    /// The staves written as tab under the style: every staff, or in a
    /// score those named like fretted instruments (when any is).
    fn tab_tracks(&self, staves: &[StaffClips]) -> Vec<TrackId> {
        if !self.lead_sheets.style.tab() {
            return Vec::new();
        }
        let name = |t: TrackId| {
            self.project
                .track(t)
                .map_or_else(String::new, |t| t.name.clone())
        };
        let any = staves.len() > 1 && staves.iter().any(|s| fretted(&name(s.track)));
        staves
            .iter()
            .map(|s| s.track)
            .filter(|t| !drum_track(&name(*t)) && (!any || fretted(&name(*t))))
            .collect()
    }

    /// The staves a lead sheet has, top to bottom, each with its clips.
    fn lead_sheet_staves(&self, of: &LeadSheetOf) -> Result<Vec<StaffClips>> {
        let p = &self.project;
        // A track's clips (launcher clips are no track's).
        let on_track = |c: &Clip| p.track(c.track).is_some_and(|t| t.clips.contains(&c.id));
        let mut staves = match of {
            LeadSheetOf::Clip(id) => {
                let c = p
                    .clip(*id)
                    .cloned()
                    .ok_or_else(|| SessionError::Other(format!("no clip {id}")))?;
                vec![StaffClips {
                    track: c.track,
                    clips: vec![c],
                }]
            }
            LeadSheetOf::Clips(ids) => {
                let mut by_track: BTreeMap<TrackId, Vec<Clip>> = BTreeMap::new();
                for c in ids.iter().filter_map(|id| p.clip(*id)) {
                    let clips = by_track.entry(c.track).or_default();
                    if on_track(c) && !clips.iter().any(|x| x.id == c.id) {
                        clips.push(c.clone());
                    }
                }
                // Top to bottom as the arranger shows the tracks.
                p.folder_order()
                    .iter()
                    .filter_map(|t| {
                        let clips = by_track.remove(&t.id)?;
                        (!clips.is_empty()).then_some(StaffClips { track: t.id, clips })
                    })
                    .collect()
            }
            LeadSheetOf::Track(t) => self.track_staves(&[*t]),
            LeadSheetOf::Tracks(ts) => self.track_staves(ts),
        };
        for s in &mut staves {
            s.clips.sort_by_key(|c| (c.start, c.id));
        }
        if staves.is_empty() {
            return Err(SessionError::Other(
                match of {
                    LeadSheetOf::Track(_) => "the track has no clips",
                    LeadSheetOf::Tracks(_) => "the tracks have no clips",
                    _ => "no clips to write",
                }
                .into(),
            ));
        }
        Ok(staves)
    }

    /// A staff for each of `tracks` that has clips (its unmuted ones).
    fn track_staves(&self, tracks: &[TrackId]) -> Vec<StaffClips> {
        let p = &self.project;
        let mut seen = Vec::new();
        let mut out = Vec::new();
        for t in tracks {
            if seen.contains(t) {
                continue;
            }
            seen.push(*t);
            let clips: Vec<Clip> = p
                .clips_of(*t)
                .into_iter()
                .filter(|c| !c.muted)
                .cloned()
                .collect();
            if !clips.is_empty() {
                out.push(StaffClips { track: *t, clips });
            }
        }
        out
    }

    /// What a lead sheet is of, for notices: a clip's name, else its
    /// tracks' names (quoted).
    fn lead_sheet_subject(&self, of: &LeadSheetOf, staves: &[StaffClips]) -> String {
        if let (LeadSheetOf::Clip(_), Some(c)) = (of, staves.first().and_then(|s| s.clips.first()))
        {
            return format!("‘{}’", c.name);
        }
        staves
            .iter()
            .map(|s| {
                let name = self
                    .project
                    .track(s.track)
                    .map_or_else(String::new, |t| t.name.clone());
                format!("‘{name}’")
            })
            .collect::<Vec<_>>()
            .join(", ")
    }

    /// The span a lead sheet covers.
    fn lead_sheet_span(&self, staves: &[StaffClips]) -> (MusicalTime, MusicalTime) {
        let p = &self.project;
        let clips = || staves.iter().flat_map(|s| s.clips.iter());
        let start = clips().map(|c| c.start).min().unwrap_or(MusicalTime::ZERO);
        let end = clips()
            .map(|c| c.end(&p.timeline, p.sample_rate))
            .max()
            .unwrap_or(start);
        (start, end)
    }

    /// The chords the chord track or the MIDI of tracks other than
    /// `parts` give over `from..to` (empty: none).
    fn known_chords(&self, parts: &[TrackId], from: MusicalTime, to: MusicalTime) -> Vec<ChordAt> {
        let p = &self.project;
        let chords: Vec<ChordAt> = p
            .chords
            .iter()
            .filter(|ch| ch.end > from && ch.start < to)
            .map(|ch| ChordAt {
                start: ch.start.max(from).quarters(),
                chord: ch.chord,
            })
            .collect();
        if !chords.is_empty() {
            return chords;
        }
        let mut sounding = Vec::new();
        for t in &p.tracks {
            if parts.contains(&t.id) || drum_track(&t.name) {
                continue;
            }
            for other in p.clips_of(t.id) {
                for n in midi_notes(other) {
                    sounding.push(faderframe_project::harmony::Sounding {
                        start: MusicalTime::from_quarters(n.start),
                        end: MusicalTime::from_quarters(n.end),
                        key: n.key,
                    });
                }
            }
        }
        self.chords_of(&sounding, from, to)
    }

    fn chords_of(
        &self,
        sounding: &[faderframe_project::harmony::Sounding],
        from: MusicalTime,
        to: MusicalTime,
    ) -> Vec<ChordAt> {
        let meter = &self.project.timeline.meter;
        let half = meter.signature_at(from).bar_length() / 2;
        faderframe_project::harmony::detect_chords(sounding, from, to, half)
            .into_iter()
            .map(|ch| ChordAt {
                start: ch.start.quarters(),
                chord: ch.chord,
            })
            .collect()
    }

    /// What of a clip to listen to and where it plays: an audio clip's
    /// part of its source, a take folder's comp piece by piece (MIDI is
    /// read, not heard).
    fn listens(&self, c: &Clip) -> Vec<(Place, Listen)> {
        match &c.content {
            ClipContent::Audio(a) => self
                .sources
                .get(&a.source)
                .map(|s| {
                    (
                        Place::Clip(c.id),
                        Listen {
                            source: s.clone(),
                            from: a.source_offset,
                            span: a.source_span(),
                        },
                    )
                })
                .into_iter()
                .collect(),
            ClipContent::Takes(f) => f
                .pieces()
                .into_iter()
                .filter_map(|piece| {
                    let take = f.takes.get(piece.take)?;
                    let source = self.sources.get(&take.source)?.clone();
                    Some((
                        Place::Piece {
                            clip: c.id,
                            at: piece.start,
                        },
                        Listen {
                            source,
                            from: take.source_offset + piece.start,
                            span: piece.end - piece.start,
                        },
                    ))
                })
                .collect(),
            ClipContent::Midi(_) => Vec::new(),
        }
    }

    /// Make a lead sheet (audio is listened to first: the staves' clips,
    /// and the other audio tracks when nothing else gives the chords; the
    /// result arrives from the tick).
    pub fn make_lead_sheet(&mut self, of: LeadSheetOf, grid: Grid) -> Result<()> {
        let staves = self.lead_sheet_staves(&of)?;
        let tab = self.tab_tracks(&staves);
        let audio = || {
            staves
                .iter()
                .flat_map(|s| s.clips.iter())
                .filter(|c| !matches!(c.content, ClipContent::Midi(_)))
        };
        // Heard already, every audio clip of it (none added since), the
        // same staves as chords.
        if self.lead_sheets.kept.as_ref().is_some_and(|k| {
            k.of == of && k.tab == tab && audio().all(|c| k.heard.contains_key(&c.id))
        }) {
            return self.write_lead_sheet(of, grid);
        }
        let (start, end) = self.lead_sheet_span(&staves);
        let tracks: Vec<TrackId> = staves.iter().map(|s| s.track).collect();
        let mut melody_places: Vec<Place> = Vec::new();
        let mut melody_parts: Vec<(Listen, bool)> = Vec::new();
        for st in &staves {
            let chords = tab.contains(&st.track);
            for c in &st.clips {
                for (place, listen) in self.listens(c) {
                    melody_places.push(place);
                    melody_parts.push((listen, chords));
                }
            }
        }
        // Chords to hear: the other audio tracks' clips over the span, when
        // neither the chord track nor MIDI gives any.
        let (chord_places, chord_parts): (Vec<Place>, Vec<Listen>) = if self
            .known_chords(&tracks, start, end)
            .is_empty()
        {
            let p = &self.project;
            p.tracks
                .iter()
                .filter(|t| !tracks.contains(&t.id) && !drum_track(&t.name))
                .flat_map(|t| p.clips_of(t.id))
                .filter(|c| !c.muted && c.start < end && c.end(&p.timeline, p.sample_rate) > start)
                .flat_map(|c| self.listens(c))
                .unzip()
        } else {
            (Vec::new(), Vec::new())
        };
        if melody_parts.is_empty() && chord_parts.is_empty() {
            self.lead_sheets.kept = Some(Kept {
                of: of.clone(),
                tab,
                heard: audio().map(|c| (c.id, Vec::new())).collect(),
                chords: Vec::new(),
            });
            return self.write_lead_sheet(of, grid);
        }
        let project_rate = self.project.sample_rate as f64;
        let listen_chords = !chord_parts.is_empty();
        let thread = std::thread::Builder::new()
            .name("faderframe-lead-sheet".into())
            .spawn(move || {
                let hear = |part: &Listen, how: &Listener| {
                    let (mono, rate) = crate::pitch::mono_of(&part.source)?;
                    let k = rate / project_rate;
                    let a = ((part.from as f64 * k) as usize).min(mono.len());
                    let b = (((part.from + part.span) as f64 * k) as usize).clamp(a, mono.len());
                    how(&mono[a..b], rate)
                };
                // A fretted staff's chords, else its melody.
                let melody_found = melody_parts
                    .iter()
                    .map(|(p, chords)| {
                        if *chords {
                            hear(p, &harmony)
                        } else {
                            hear(p, &|x, r| Some(melody(x, r)))
                        }
                    })
                    .collect();
                let chords_found = chord_parts.iter().map(|p| hear(p, &harmony)).collect();
                (melody_found, chords_found)
            })
            .map_err(|e| SessionError::Other(e.to_string()))?;
        let what = self.lead_sheet_subject(&of, &staves);
        self.lead_sheets.job = Some(Job {
            of,
            grid,
            tab,
            melody: melody_places,
            chords: chord_places,
            thread,
        });
        self.notify(
            NoticeLevel::Info,
            if listen_chords {
                format!("Listening to {what} and to the chords around it…")
            } else {
                format!("Listening to {what}…")
            },
        );
        self.revision += 1;
        Ok(())
    }

    /// Whether audio is being listened to for a lead sheet.
    pub fn making_lead_sheet(&self) -> bool {
        self.lead_sheets.job.is_some()
    }

    /// The lead sheet made last.
    pub fn lead_sheet(&self) -> Option<&LeadSheetDoc> {
        self.lead_sheets.doc.as_ref()
    }

    /// The audio heard: write the sheet (from the tick).
    pub(crate) fn poll_lead_sheets(&mut self) {
        let finished = self
            .lead_sheets
            .job
            .as_ref()
            .is_some_and(|j| j.thread.is_finished());
        if !finished {
            return;
        }
        let Some(job) = self.lead_sheets.job.take() else {
            return;
        };
        self.revision += 1;
        let Ok((melody_found, chords_found)) = job.thread.join() else {
            self.notify(NoticeLevel::Warning, "listening to the audio failed");
            return;
        };
        let p = &self.project;
        // Every clip listened to has its entry (empty when nothing was
        // found in it).
        let mut heard: BTreeMap<ClipId, Vec<Note>> = BTreeMap::new();
        for (place, found) in job.melody.iter().zip(&melody_found) {
            let notes = heard.entry(place.clip()).or_default();
            let (Some(found), Some(c)) = (found, p.clip(place.clip())) else {
                continue;
            };
            notes.extend(found.iter().map(|n| Note {
                start: place.quarters(p, c, n.start),
                end: place.quarters(p, c, n.end),
                key: n.key,
            }));
        }
        let mut sounding = Vec::new();
        for (place, found) in job.chords.iter().zip(&chords_found) {
            let (Some(found), Some(c)) = (found, p.clip(place.clip())) else {
                continue;
            };
            sounding.extend(found.iter().map(|n| faderframe_project::harmony::Sounding {
                start: MusicalTime::from_quarters(place.quarters(p, c, n.start)),
                end: MusicalTime::from_quarters(place.quarters(p, c, n.end)),
                key: n.key,
            }));
        }
        let staves = self.lead_sheet_staves(&job.of).unwrap_or_default();
        // Audio clips with nothing to hear (their sources not loaded) count
        // as heard, so another grid does not listen again.
        for c in staves.iter().flat_map(|s| s.clips.iter()) {
            if !matches!(c.content, ClipContent::Midi(_)) {
                heard.entry(c.id).or_default();
            }
        }
        let chords = if sounding.is_empty() || staves.is_empty() {
            Vec::new()
        } else {
            let (start, end) = self.lead_sheet_span(&staves);
            let meter = &self.project.timeline.meter;
            let from = meter.bar_start(meter.bar_at(start));
            self.chords_of(&sounding, from, end)
        };
        self.lead_sheets.kept = Some(Kept {
            of: job.of.clone(),
            tab: job.tab.clone(),
            heard,
            chords,
        });
        if let Err(e) = self.write_lead_sheet(job.of, job.grid) {
            self.notify(NoticeLevel::Warning, e.to_string());
        }
    }

    /// Wait for the audio being heard (scripts and tests).
    pub fn wait_for_lead_sheet(&mut self) {
        while let Some(job) = &self.lead_sheets.job {
            if job.thread.is_finished() {
                self.poll_lead_sheets();
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    }

    /// Write the sheet from what was heard and the project around it.
    fn write_lead_sheet(&mut self, of: LeadSheetOf, grid: Grid) -> Result<()> {
        let Some(kept) = self.lead_sheets.kept.clone().filter(|k| k.of == of) else {
            return Err(SessionError::Other("nothing heard yet".into()));
        };
        let staves = self.lead_sheet_staves(&of)?;
        let (start, end) = self.lead_sheet_span(&staves);
        let what = self.lead_sheet_subject(&of, &staves);
        let p = &self.project;
        let lines: Vec<Vec<Note>> = staves.iter().map(|s| s.notes(p, &kept.heard)).collect();
        if lines.iter().all(Vec::is_empty) {
            return Err(SessionError::Other(format!("no melody heard in {what}")));
        }
        let meter = &p.timeline.meter;
        // The bars from the first clip's to the last's.
        let (first, last) = (
            meter.bar_at(start),
            meter.bar_at(end - MusicalTime::from_ticks(1).max(MusicalTime::ZERO)),
        );
        let bars: Vec<Bar> = (first..=last.max(first))
            .map(|b| {
                let sig = meter.signature_of_bar(b);
                Bar {
                    start: meter.bar_start(b).quarters(),
                    time: (sig.numerator, sig.denominator),
                }
            })
            .collect();
        let from = meter.bar_start(first);
        let to = meter.bar_start(last.max(first) + 1);
        let tracks: Vec<TrackId> = staves.iter().map(|s| s.track).collect();
        let mut chords = self.known_chords(&tracks, from, to);
        if chords.is_empty() {
            chords = kept.chords.clone();
        }
        // A score with nothing else to go by: what its staves play together.
        if chords.is_empty() && staves.len() > 1 {
            let sounding: Vec<_> = lines
                .iter()
                .flatten()
                .map(|n| faderframe_project::harmony::Sounding {
                    start: MusicalTime::from_quarters(n.start),
                    end: MusicalTime::from_quarters(n.end),
                    key: n.key,
                })
                .collect();
            chords = self.chords_of(&sounding, from, to);
        }
        let p = &self.project;
        let key = p
            .keys
            .iter()
            .rev()
            .find(|k| k.at <= start)
            .or(p.keys.first())
            .map(|k| k.key);
        let names: Vec<String> = staves
            .iter()
            .map(|s| {
                p.track(s.track)
                    .map_or_else(String::new, |t| t.name.clone())
            })
            .collect();
        let words: Vec<Line> = p
            .lyrics
            .iter()
            .filter(|l| l.end > from && l.start < to)
            .map(|l| Line {
                start: l.start.quarters(),
                end: l.end.quarters(),
                text: l.text.clone(),
            })
            .collect();
        let sings = singer(&names);
        let style = self.lead_sheets.style;
        let tab = self.tab_tracks(&staves);
        let title = if p.name.trim().is_empty() {
            what.replace(['‘', '’'], "")
        } else {
            p.name.clone()
        };
        let input = Input {
            title,
            composer: String::new(),
            key,
            bars,
            tempo: Some(p.timeline.tempo.bpm_at(start)),
            staves: names
                .into_iter()
                .zip(lines)
                .enumerate()
                .map(|(i, (name, notes))| Staff {
                    tab: tab.contains(&staves[i].track).then(|| StaffTab {
                        tuning: Tuning::for_part(style.tuning(), &name),
                        capo: style.capo,
                        notation: style.notation == Notation::WithTab,
                    }),
                    name,
                    notes,
                    lines: if i == sings {
                        words.clone()
                    } else {
                        Vec::new()
                    },
                })
                .collect(),
            chords,
            grid,
        };
        let sheet = faderframe_leadsheet::build(&input);
        let pages = engrave(&sheet, &Layout::for_staves(sheet.parts.len()));
        let (count, parts) = (sheet.bars(), sheet.parts.len());
        let tabs = sheet.parts.iter().filter(|p| p.tab.is_some()).count();
        self.lead_sheets.doc = Some(LeadSheetDoc {
            of,
            grid,
            style,
            sheet,
            pages,
        });
        self.workspace_action(crate::WorkspaceAction::ShowView(
            faderframe_workspace::ViewId::lead_sheet(),
        ))?;
        self.notify(
            NoticeLevel::Info,
            if tabs > 0 {
                format!("Tab of {what}: {count} bars on {}", style.tuning().name)
            } else if parts > 1 {
                format!("Score of {what}: {count} bars, {parts} staves")
            } else {
                format!("Lead sheet of {what}: {count} bars")
            },
        );
        self.revision += 1;
        Ok(())
    }

    /// Write the lead sheet to `path`: MusicXML (`.musicxml`, `.xml`), PDF
    /// (`.pdf`) or, with tablature, plain-text tab (`.txt`).
    pub fn export_lead_sheet(&mut self, path: &Path) -> Result<PathBuf> {
        let doc = self
            .lead_sheets
            .doc
            .as_ref()
            .ok_or_else(|| SessionError::Other("make a lead sheet first".into()))?;
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .map(str::to_ascii_lowercase);
        let bytes = match ext.as_deref() {
            Some("pdf") => faderframe_leadsheet::pdf::write(&doc.pages, &doc.sheet.title),
            Some("musicxml" | "xml") => {
                faderframe_leadsheet::musicxml::write(&doc.sheet, &today()).into_bytes()
            }
            Some("txt") => faderframe_leadsheet::tab::text(&doc.sheet)
                .ok_or_else(|| {
                    SessionError::Other("plain-text tab needs a sheet with tablature".into())
                })?
                .into_bytes(),
            _ => {
                return Err(SessionError::Other(
                    "a lead sheet is written as .pdf, .musicxml or (tab) .txt".into(),
                ));
            }
        };
        // Staged and moved into place; an existing file is asked about
        // ([`crate::exports`]).
        crate::exports::make_parent(path)?;
        let shown = path.to_path_buf();
        self.write_export(
            "Lead sheet",
            vec![(path.to_path_buf(), bytes)],
            move |s, _| {
                s.notify(
                    NoticeLevel::Info,
                    format!("Lead sheet written to {}", shown.display()),
                );
            },
        )?;
        Ok(path.to_path_buf())
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn dates_are_civil() {
        assert_eq!(super::today().len(), 10);
    }

    #[test]
    fn the_words_go_to_the_voice() {
        let names = |n: &[&str]| n.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(super::singer(&names(&["Keys", "Lead Vocals", "Bass"])), 1);
        assert_eq!(super::singer(&names(&["Guitar", "Bass"])), 0);
    }
}
