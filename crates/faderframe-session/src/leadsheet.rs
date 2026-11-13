//! Lead sheets from a recording: the melody of a clip, of several clips or
//! of a whole track — or a score of several tracks, a staff each, or a
//! part written as played — with the bars it sits in, the chords, the key,
//! the tempo and the lyrics (under the staff that sings them), written out
//! by [`faderframe_leadsheet`] and engraved for the Lead Sheet view,
//! exported as MusicXML or PDF.
//!
//! Hearing is polyphonic first: every audio clip (take folders by their
//! comp) is heard in a thread as the Pitch editor's polyphonic detection
//! hears it ([`crate::polypitch::hear_notes`]: every note, chords too,
//! each followed to its pitch), cleaned and its melody picked out
//! ([`faderframe_leadsheet::heard`]: ghosts and fragments, a skyline that
//! holds the most salient upper voice, a voice's vibrato and glides, octave
//! slips checked against the audio); MIDI clips are read and their melody
//! picked out the same way. A staff's clips make one line on one timeline:
//! the gaps between them are rests, and where clips overlap the one on top
//! (the later) is written.
//!
//! The chords: the chord track's; without one, named
//! ([`faderframe_leadsheet::harmony`]) from the notes under the staves'
//! melodies together with what the other tracks play — their MIDI, or
//! without any, their audio heard the same way — the melodies counting a
//! little; a single line with nothing under or around it gets none, a
//! score with nothing else goes by what its staves play together.
//!
//! How it is written ([`SheetStyle`]): the melody (a lead sheet), every
//! note as played (chords on one stem, a piano's part on a grand staff),
//! or guitar tab — the fretted staves (all of them, or in a score those
//! whose tracks are named like guitars, basses or ukuleles) as played,
//! written as notes with their tablature under them, or as tablature
//! alone, on the tuning and capo chosen (a bass on a bass's strings);
//! exported also as plain-text tab. Changing it writes the sheet again
//! from what was heard.

use crate::polypitch::hear_notes;
use crate::{NoticeLevel, Result, Session, SessionError};
use faderframe_core::{ClipId, TrackId};
use faderframe_engine::Source;
pub use faderframe_leadsheet::Grid;
use faderframe_leadsheet::engrave::{Layout, Page, engrave};
use faderframe_leadsheet::harmony::Sounded;
pub use faderframe_leadsheet::heard::Voiced;
use faderframe_leadsheet::heard::{self, Heard, Octave, Scale};
use faderframe_leadsheet::score::Clef;
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
    /// A lead sheet: each staff's melody, the chords over it.
    #[default]
    Standard,
    /// Notes with their tablature under them (fretted staves as played).
    WithTab,
    /// Tablature alone, its rhythm under it.
    Tab,
    /// Every note as played: notes sounding together on one stem, a
    /// piano's part on a grand staff (the chords still over it).
    AsPlayed,
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
        matches!(self.notation, Notation::WithTab | Notation::Tab)
    }

    pub fn tuning(&self) -> Tuning {
        TUNINGS.get(self.tuning).copied().unwrap_or(TUNINGS[0])
    }
}

/// How much a melody's notes count towards the chords when other notes
/// are heard with them (passing notes are many).
const MELODY_WEIGHT: f32 = 0.3;

/// Tracks named like keyboards (written on a grand staff as played).
pub(crate) fn keyboard(name: &str) -> bool {
    let n = name.to_lowercase();
    [
        "piano", "keys", "keyboard", "rhodes", "wurli", "organ", "clav", "harpsi", "celest",
    ]
    .iter()
    .any(|w| n.contains(w))
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
    /// Its parts' names, top to bottom ("Voice", "Voice, Bass"; a grand
    /// staff's and a tab under its notes once).
    pub fn parts(&self) -> String {
        self.sheet
            .parts
            .iter()
            .map(|p| p.name.as_str())
            .filter(|n| !n.is_empty())
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
    /// The staff's line: its clips' notes (MIDI read, audio as heard; the
    /// melody's marked), where clips overlap the later one's.
    fn notes(&self, p: &Project, heard: &BTreeMap<ClipId, Vec<Voiced>>) -> Vec<Voiced> {
        let span = |c: &Clip| {
            (
                c.start.quarters(),
                c.end(&p.timeline, p.sample_rate).quarters(),
            )
        };
        let mut out = Vec::new();
        for (i, c) in self.clips.iter().enumerate() {
            let notes = match &c.content {
                ClipContent::Midi(_) => midi_voiced(p, c),
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
/// staves' notes (the melody's marked) and the other tracks' notes for the
/// chords (seconds into what it listened to; None: unreadable).
type Listened = (Vec<Option<Vec<Voiced>>>, Vec<Option<Vec<Voiced>>>);

/// A listening job: what of, the grid, where what it hears plays (the
/// staves', the chords'), the thread.
struct Job {
    of: LeadSheetOf,
    grid: Grid,
    staves: Vec<Place>,
    chords: Vec<Place>,
    thread: JoinHandle<Listened>,
}

/// What was heard for a lead sheet (kept, so another grid or style does
/// not listen again): the audio clips' notes by clip (quarters, the
/// melody's marked) and the other tracks' notes heard for the chords.
#[derive(Clone)]
struct Kept {
    of: LeadSheetOf,
    heard: BTreeMap<ClipId, Vec<Voiced>>,
    evidence: Vec<Sounded>,
}

#[derive(Default)]
pub(crate) struct LeadSheets {
    job: Option<Job>,
    kept: Option<Kept>,
    pub(crate) doc: Option<LeadSheetDoc>,
    pub(crate) style: SheetStyle,
}

/// Today as YYYY-MM-DD (UTC).
pub(crate) fn today() -> String {
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
/// unmuted, cut at its end), velocities 0…1.
fn midi_notes(c: &Clip) -> Vec<Heard> {
    match &c.content {
        ClipContent::Midi(m) => m
            .notes
            .iter()
            .filter(|n| !n.muted && n.start < m.length)
            .map(|n| {
                Heard::midi(
                    (c.start + n.start).quarters(),
                    (c.start + (n.start + n.length).min(m.length)).quarters(),
                    n.key,
                    f32::from(n.velocity) / 127.0,
                )
            })
            .collect(),
        _ => Vec::new(),
    }
}

/// A MIDI clip's notes with its melody marked (picked out as a
/// recording's is, at the tempo where the clip starts).
fn midi_voiced(p: &Project, c: &Clip) -> Vec<Voiced> {
    let mut notes = midi_notes(c);
    notes.sort_by(|a, b| a.start.total_cmp(&b.start).then(a.key.cmp(&b.key)));
    let scale = Scale {
        per_second: p.timeline.tempo.bpm_at(c.start).max(1.0) / 60.0,
    };
    let line = heard::melody(&notes, scale);
    heard::voiced(&notes, &line)
}

/// What a part of a recording (`mono[lo..hi]`) plays, as a lead sheet
/// writes from it (seconds into the part): every note the polyphonic
/// detection hears, cleaned, and — with `melody` — the melody picked out,
/// a note of it an octave off its neighbours moved by the octave its audio
/// says (each octave's harmonics weighed where the note sounds). None: the
/// audio could not be heard.
fn hear_part(mono: &[f32], rate: f64, lo: usize, hi: usize, melody: bool) -> Option<Vec<Voiced>> {
    let hearing = hear_notes(mono, rate, lo, hi)?;
    let hop = hearing.hop as f64 / rate;
    let notes: Vec<Heard> = hearing
        .notes
        .into_iter()
        .map(|h| {
            let heard: Vec<f32> = h.level.iter().copied().filter(|l| *l > 0.0).collect();
            Heard {
                start: h.found.start,
                end: h.found.end,
                key: h.found.key,
                pitch: h.pitch,
                velocity: f32::from(h.found.velocity) / 127.0,
                loudness: heard::median(&heard).map_or(f32::NAN, |l| 20.0 * l.log10()),
                track: h.track,
                levels: h.level,
                hop,
            }
        })
        .collect();
    let notes = heard::clean(notes, Scale::SECONDS);
    if !melody {
        return Some(heard::voiced(&notes, &[]));
    }
    let mut line = heard::melody(&notes, Scale::SECONDS);
    let suspects = heard::octave_suspects(&line);
    if !suspects.is_empty() {
        // Each suspect an octave down, as named, an octave up.
        let around: Vec<faderframe_polypitch::Heard> = suspects
            .iter()
            .flat_map(|(i, _)| {
                let n = &notes[line[*i].index];
                let at = |s: f64| lo as i64 + (s * rate) as i64;
                [-12.0, 0.0, 12.0].map(|d| faderframe_polypitch::Heard {
                    start: at(n.start),
                    end: at(n.end),
                    key: n.sounding() + d,
                })
            })
            .collect();
        let followed = faderframe_polypitch::follow(mono, rate, &around, hearing.hop);
        let mean = |f: &faderframe_polypitch::Followed| {
            f.level.iter().sum::<f32>() / f.level.len().max(1) as f32
        };
        let evidence: Vec<(usize, i8, Octave)> = suspects
            .iter()
            .zip(followed.chunks(3))
            .filter_map(|(&(i, way), f)| {
                let [below, here, above] = f else {
                    return None;
                };
                Some((
                    i,
                    way,
                    Octave {
                        below: mean(below),
                        here: mean(here),
                        above: mean(above),
                    },
                ))
            })
            .collect();
        heard::fix_octaves(&mut line, &evidence);
    }
    Some(heard::voiced(&notes, &line))
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

    /// The chord track's chords over `from..to` (empty: none).
    fn chord_track(&self, from: MusicalTime, to: MusicalTime) -> Vec<ChordAt> {
        self.project
            .chords
            .iter()
            .filter(|ch| ch.end > from && ch.start < to)
            .map(|ch| ChordAt {
                start: ch.start.max(from).quarters(),
                chord: ch.chord,
            })
            .collect()
    }

    /// What the MIDI of tracks other than `parts` plays over `from..to`,
    /// as harmonic evidence (drums left out).
    fn midi_evidence(&self, parts: &[TrackId], from: MusicalTime, to: MusicalTime) -> Vec<Sounded> {
        let p = &self.project;
        let (from, to) = (from.quarters(), to.quarters());
        p.tracks
            .iter()
            .filter(|t| !parts.contains(&t.id) && !drum_track(&t.name))
            .flat_map(|t| p.clips_of(t.id))
            .filter(|c| !c.muted)
            .flat_map(midi_notes)
            .filter(|n| n.end > from && n.start < to)
            .map(|n| Sounded {
                start: n.start,
                end: n.end,
                key: n.key,
                weight: n.velocity,
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
                    // The take's clip cut to the piece: the source it
                    // plays there, as an audio clip's is heard.
                    let take = f.takes.get(piece.take)?;
                    let cut = take
                        .audio
                        .cut(piece.start - take.start, piece.end - take.start)?;
                    let source = self.sources.get(&cut.source)?.clone();
                    Some((
                        Place::Piece {
                            clip: c.id,
                            at: piece.start,
                        },
                        Listen {
                            source,
                            from: cut.source_offset,
                            span: cut.source_span(),
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
        let audio = || {
            staves
                .iter()
                .flat_map(|s| s.clips.iter())
                .filter(|c| !matches!(c.content, ClipContent::Midi(_)))
        };
        // Heard already, every audio clip of it (none added since).
        if self
            .lead_sheets
            .kept
            .as_ref()
            .is_some_and(|k| k.of == of && audio().all(|c| k.heard.contains_key(&c.id)))
        {
            return self.write_lead_sheet(of, grid);
        }
        let (start, end) = self.lead_sheet_span(&staves);
        let tracks: Vec<TrackId> = staves.iter().map(|s| s.track).collect();
        let (staff_places, staff_parts): (Vec<Place>, Vec<Listen>) = staves
            .iter()
            .flat_map(|st| st.clips.iter())
            .flat_map(|c| self.listens(c))
            .unzip();
        // The chords' notes to hear: the other audio tracks' clips over the
        // span, when neither the chord track nor MIDI gives any.
        let (chord_places, chord_parts): (Vec<Place>, Vec<Listen>) = if self
            .chord_track(start, end)
            .is_empty()
            && self.midi_evidence(&tracks, start, end).is_empty()
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
        if staff_parts.is_empty() && chord_parts.is_empty() {
            self.lead_sheets.kept = Some(Kept {
                of: of.clone(),
                heard: audio().map(|c| (c.id, Vec::new())).collect(),
                evidence: Vec::new(),
            });
            return self.write_lead_sheet(of, grid);
        }
        let project_rate = self.project.sample_rate as f64;
        let listen_chords = !chord_parts.is_empty();
        let thread = std::thread::Builder::new()
            .name("faderframe-lead-sheet".into())
            .spawn(move || {
                let hear = |part: &Listen, melody: bool| {
                    let (mono, rate) = crate::pitch::mono_of(&part.source)?;
                    let k = rate / project_rate;
                    let a = ((part.from as f64 * k) as usize).min(mono.len());
                    let b = (((part.from + part.span) as f64 * k) as usize).clamp(a, mono.len());
                    hear_part(&mono, rate, a, b, melody)
                };
                let staves = staff_parts.iter().map(|p| hear(p, true)).collect();
                let chords = chord_parts.iter().map(|p| hear(p, false)).collect();
                (staves, chords)
            })
            .map_err(|e| SessionError::Other(e.to_string()))?;
        let what = self.lead_sheet_subject(&of, &staves);
        self.lead_sheets.job = Some(Job {
            of,
            grid,
            staves: staff_places,
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

    /// What the lead sheet made last was written from: each staff's notes
    /// as heard (MIDI read), in quarters on the timeline, the melody's
    /// marked — top to bottom (empty before one is made).
    pub fn lead_sheet_notes(&self) -> Vec<Vec<Voiced>> {
        let Some(kept) = self
            .lead_sheets
            .kept
            .as_ref()
            .filter(|k| self.lead_sheets.doc.as_ref().is_some_and(|d| d.of == k.of))
        else {
            return Vec::new();
        };
        self.lead_sheet_staves(&kept.of)
            .unwrap_or_default()
            .iter()
            .map(|s| s.notes(&self.project, &kept.heard))
            .collect()
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
        let Ok((staves_heard, chords_heard)) = job.thread.join() else {
            self.notify(NoticeLevel::Warning, "listening to the audio failed");
            return;
        };
        let p = &self.project;
        // Seconds into what was heard → quarters on the timeline.
        let placed = |place: &Place, v: &Voiced| -> Option<Voiced> {
            let c = p.clip(place.clip())?;
            Some(Voiced {
                start: place.quarters(p, c, v.start),
                end: place.quarters(p, c, v.end),
                ..*v
            })
        };
        // Every clip listened to has its entry (empty when nothing was
        // found in it).
        let mut heard: BTreeMap<ClipId, Vec<Voiced>> = BTreeMap::new();
        for (place, found) in job.staves.iter().zip(&staves_heard) {
            let notes = heard.entry(place.clip()).or_default();
            notes.extend(
                found
                    .iter()
                    .flatten()
                    .filter_map(|v| placed(place, v))
                    .filter(|v| v.end > v.start),
            );
        }
        let evidence: Vec<Sounded> = job
            .chords
            .iter()
            .zip(&chords_heard)
            .flat_map(|(place, found)| found.iter().flatten().map(move |v| (place, v)))
            .filter_map(|(place, v)| placed(place, v))
            .map(|v| Sounded {
                start: v.start,
                end: v.end,
                key: v.key,
                weight: v.level,
            })
            .collect();
        let staves = self.lead_sheet_staves(&job.of).unwrap_or_default();
        // Audio clips with nothing to hear (their sources not loaded) count
        // as heard, so another grid does not listen again.
        for c in staves.iter().flat_map(|s| s.clips.iter()) {
            if !matches!(c.content, ClipContent::Midi(_)) {
                heard.entry(c.id).or_default();
            }
        }
        self.lead_sheets.kept = Some(Kept {
            of: job.of.clone(),
            heard,
            evidence,
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

    /// The chords over `bars` when the chord track has none: named from the
    /// notes under the staves' melodies and what the other tracks play
    /// (`outside`), the melodies counting a little; for a score with
    /// nothing else, from what its staves play together; for a single line
    /// alone, none.
    fn heard_chords(
        &self,
        lines: &[Vec<Voiced>],
        outside: Vec<Sounded>,
        bars: &[Bar],
        start: MusicalTime,
    ) -> Vec<ChordAt> {
        let sound = |v: &Voiced, weight: f32| Sounded {
            start: v.start,
            end: v.end,
            key: v.key,
            weight,
        };
        let mut evidence = outside;
        evidence.extend(
            lines
                .iter()
                .flatten()
                .filter(|v| !v.melody)
                .map(|v| sound(v, v.level)),
        );
        let sung = lines.iter().flatten().filter(|v| v.melody);
        if !evidence.is_empty() {
            evidence.extend(sung.map(|v| sound(v, MELODY_WEIGHT * v.level)));
        } else if lines.len() > 1 {
            evidence.extend(sung.map(|v| sound(v, v.level)));
        }
        if evidence.is_empty() {
            return Vec::new();
        }
        let p = &self.project;
        let key = p
            .keys
            .iter()
            .rev()
            .find(|k| k.at <= start)
            .or(p.keys.first())
            .map(|k| k.key)
            .or_else(|| {
                let mut h = [0.0f64; 12];
                for e in &evidence {
                    h[usize::from(e.key % 12)] += (e.end - e.start).max(0.0) * f64::from(e.weight);
                }
                faderframe_midi::theory::detect_key(&h)
            });
        faderframe_leadsheet::harmony::chords(&evidence, bars, key)
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
        let lines: Vec<Vec<Voiced>> = staves.iter().map(|s| s.notes(p, &kept.heard)).collect();
        if lines.iter().all(Vec::is_empty) {
            return Err(SessionError::Other(format!("no notes heard in {what}")));
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
        let mut chords = self.chord_track(from, to);
        if chords.is_empty() {
            let mut outside = self.midi_evidence(&tracks, from, to);
            if outside.is_empty() {
                outside = kept.evidence.clone();
            }
            chords = self.heard_chords(&lines, outside, &bars, start);
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
        // The staves: a fretted one as played with its tab; as played,
        // every note (a keyboard's part on a grand staff); else the melody.
        let mut written: Vec<Staff> = Vec::new();
        for (i, (name, line)) in names.into_iter().zip(&lines).enumerate() {
            let fretted_here = tab.contains(&staves[i].track);
            let as_played = fretted_here || style.notation == Notation::AsPlayed;
            let notes: Vec<Note> = line
                .iter()
                .filter(|v| as_played || v.melody)
                .map(|v| Note {
                    start: v.start,
                    end: v.end,
                    key: v.key,
                })
                .collect();
            let lines = if i == sings {
                words.clone()
            } else {
                Vec::new()
            };
            if fretted_here {
                written.push(Staff {
                    tab: Some(StaffTab {
                        tuning: Tuning::for_part(style.tuning(), &name),
                        capo: style.capo,
                        notation: style.notation == Notation::WithTab,
                    }),
                    name,
                    notes,
                    lines,
                    chords: true,
                    ..Staff::default()
                });
            } else if as_played
                && !fretted(&name)
                && (keyboard(&name) || faderframe_leadsheet::build::wants_grand_staff(&notes))
            {
                let (right, left) = faderframe_leadsheet::hands(&notes);
                written.push(Staff {
                    name,
                    notes: right,
                    lines,
                    chords: true,
                    clef: Some(Clef::Treble),
                    brace: true,
                    ..Staff::default()
                });
                written.push(Staff {
                    notes: left,
                    chords: true,
                    clef: Some(Clef::Bass),
                    ..Staff::default()
                });
            } else {
                written.push(Staff {
                    name,
                    notes,
                    lines,
                    chords: as_played,
                    ..Staff::default()
                });
            }
        }
        let input = Input {
            title,
            composer: String::new(),
            key,
            bars,
            tempo: Some(p.timeline.tempo.bpm_at(start)),
            staves: written,
            chords,
            grid,
        };
        let sheet = faderframe_leadsheet::build(&input);
        let pages = engrave(&sheet, &Layout::for_staves(sheet.parts.len()));
        let (count, parts) = (sheet.bars(), sheet.instruments().len());
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
            } else if style.notation == Notation::AsPlayed {
                format!("{what} as played: {count} bars")
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
