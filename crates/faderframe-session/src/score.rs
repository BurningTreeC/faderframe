//! The score editor (`faderframe-view-score`): MIDI clips written as
//! notation and edited there — the same notes the piano roll edits, one
//! model.
//!
//! What the score shows ([`ScoreOf`]: the clip in the editor, a clip, or
//! tracks with their MIDI clips, a staff each — a keyboard's part on a
//! grand staff) is written by [`faderframe_leadsheet::notate`] and engraved
//! with a map of where everything went ([`ScoreDoc`], rebuilt when what it
//! reads changes — the notes of its clips, their notation, metre, key,
//! chords — never for edits elsewhere). The editor's choices that MIDI
//! does not hold (a note's voice, spelling, syllable, articulations and
//! dynamic; a clip's display quantise) live in
//! [`faderframe_project::notation`].
//!
//! Every edit is one undo step of the clips' notes ([`Command::SetClipContent`])
//! and their notation ([`Command::SetClipNotation`]):
//!
//! * Entry ([`ScoreKey`], [`ScoreOp::EnterAt`]) puts notes of the chosen
//!   value at the cursor in the chosen voice, replacing what that voice
//!   had there (notes starting inside go, a note sounding into it is cut);
//!   letters take the key's accidentals and the octave nearest the note
//!   before; the cursor moves on. Shift with a letter adds the note to the
//!   chord just entered, `+` ties (lengthens) it, `0` writes a rest.
//! * Durations set notes' lengths (replacing what their voice had after
//!   them), arrows transpose (by semitone spelt sharp going up and flat
//!   going down, by octave, or by step in the key), accidentals and
//!   respelling choose the letter.
//! * Dynamics set the velocity of the marked note and those after it on
//!   its track until the next dynamic; staccato plays a note half its
//!   written value, accents and marcato play it louder; a track with an
//!   articulation map gets the matching attribute too.
//! * Syllables are typed a line at a time from a note on; chord symbols go
//!   to the chord track.
//! * Display quantise is per clip (the MIDI keeps its timing); Quantize to
//!   the Score moves the notes to where the score writes them.

use crate::{Action, NoticeLevel, Result, SelectMode, Session, SessionError, WorkspaceAction};
use faderframe_core::{ClipId, NoteId, TrackId};
use faderframe_leadsheet::build::{Bar, ChordAt};
use faderframe_leadsheet::engrave::{Layout, Map, Page, Pointed, engrave_mapped};
use faderframe_leadsheet::notate::{
    Display, STACCATO_LENGTH, ScoreInput, ScoreStaff, Written, WrittenAt, notate, spellings,
};
use faderframe_leadsheet::score::{Clef, Duration, KeySig, LeadSheet};
use faderframe_project::notation::{ClipNotation, Mark, ScoreGrid};
use faderframe_project::{ClipContent, Command, MidiClip, MidiNote};
use faderframe_timeline::MusicalTime;
use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::Arc;

pub use faderframe_leadsheet::score::Value;
pub use faderframe_project::notation::{Dynamic, Mark as ScoreMark};

/// A letter's natural pitch class (0 = C … 6 = B).
const NATURAL: [u8; 7] = [0, 2, 4, 5, 7, 9, 11];

/// What the score shows.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub enum ScoreOf {
    /// The clip open in the piano roll (else the first selected MIDI
    /// clip).
    #[default]
    Editor,
    Clip(ClipId),
    /// These tracks' MIDI clips, a staff each.
    Tracks(Vec<TrackId>),
}

/// How the score is laid out.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum ScoreLayout {
    /// On pages, as printed.
    #[default]
    Pages,
    /// One long system scrolled sideways.
    Strip,
}

/// What notes are entered as, and how the score is laid out (not part of
/// the project).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ScoreSettings {
    pub value: Value,
    pub dots: u8,
    pub triplet: bool,
    /// The voice notes are entered in (1 or 2).
    pub voice: u8,
    /// Note input: the pointer enters notes, letters enter at the cursor.
    pub entering: bool,
    pub layout: ScoreLayout,
    /// Velocity of entered notes with no dynamic or note before them.
    pub velocity: u8,
}

impl Default for ScoreSettings {
    fn default() -> Self {
        Self {
            value: Value::Quarter,
            dots: 0,
            triplet: false,
            voice: 1,
            entering: false,
            layout: ScoreLayout::Pages,
            velocity: 80,
        }
    }
}

impl ScoreSettings {
    /// The written value chosen.
    pub fn duration(&self) -> Duration {
        Duration {
            value: self.value,
            dots: self.dots.min(2),
            triplet: self.triplet,
        }
    }

    /// Its length.
    pub fn length(&self) -> MusicalTime {
        length_of(self.duration())
    }
}

/// A written value's length.
pub fn length_of(d: Duration) -> MusicalTime {
    MusicalTime::from_quarters(
        f64::from(d.divisions()) / f64::from(faderframe_leadsheet::score::DIVISIONS),
    )
}

/// Where the next note goes: a part (a staff) of the score and a time
/// (project).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ScoreCursor {
    pub part: usize,
    pub at: MusicalTime,
}

/// A transposition.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Interval {
    Semitones(i32),
    Octaves(i32),
    /// Steps of the key.
    Steps(i32),
}

/// A key of keyboard entry.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScoreKey {
    /// A letter (0 = C … 6 = B): a note at the cursor, or (`chord`) added
    /// to the chord entered last.
    Letter {
        step: u8,
        chord: bool,
    },
    /// Choose a value (and set the selected notes to it outside note
    /// input).
    Value(Value),
    Dot,
    Triplet,
    Rest,
    /// The note entered last (or the selection) held on by the value.
    Tie,
    Transpose(Interval),
    Respell,
    Delete,
    /// Enter in a voice (and move the selection to it outside note
    /// input).
    Voice(u8),
    /// The next note (with a selection) or the cursor on by the value.
    Next,
    Previous,
    /// Note input on or off.
    ToggleEntering,
    /// Note input off, the cursor gone.
    Escape,
}

/// Score edits and settings ([`Action::Score`]).
#[derive(Clone, Debug, PartialEq)]
pub enum ScoreOp {
    /// Show these in the score (and the score view).
    Show(ScoreOf),
    Settings(ScoreSettings),
    Cursor(Option<ScoreCursor>),
    Key(ScoreKey),
    /// A note entered with the pointer: on `part` at `at` (project time),
    /// on the staff's place of diatonic step `diatonic` (from C0; the
    /// key's accidental for its letter).
    EnterAt {
        part: usize,
        at: MusicalTime,
        diatonic: i32,
    },
    SetDuration {
        notes: Vec<NoteId>,
        duration: Duration,
    },
    /// The notes on the letter they are spelt on, with this alteration.
    Accidental {
        notes: Vec<NoteId>,
        alter: i8,
    },
    Respell(Vec<NoteId>),
    Transpose {
        notes: Vec<NoteId>,
        by: Interval,
    },
    /// Write the notes in a voice (`None`: by the notes around them).
    Voice {
        notes: Vec<NoteId>,
        voice: Option<u8>,
    },
    Delete(Vec<NoteId>),
    /// A line of words from `from` on, a syllable a note ("Hel-lo world";
    /// `_` holds the word before over a note); empty: `from`'s syllable
    /// cleared.
    Lyrics {
        from: NoteId,
        text: String,
    },
    /// A chord symbol on the chord track at `at` until the next (empty
    /// text: the chord there removed).
    ChordSymbol {
        at: MusicalTime,
        text: String,
    },
    Dynamic {
        notes: Vec<NoteId>,
        dynamic: Option<Dynamic>,
    },
    Mark {
        notes: Vec<NoteId>,
        mark: Mark,
        on: bool,
    },
    /// Clips' display quantise.
    Display {
        clips: Vec<ClipId>,
        grid: ScoreGrid,
        legato: bool,
    },
    /// The notes (all the score's when empty) moved to where the score
    /// writes them.
    Quantize(Vec<NoteId>),
    /// Write the score as PDF or MusicXML.
    Export(PathBuf),
}

/// A staff of the score: whose notes it writes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScorePart {
    pub track: TrackId,
    pub name: String,
    pub clips: Vec<ClipId>,
    pub clef: Clef,
    /// The lower staff of a grand staff (the track's left hand).
    pub lower: bool,
}

/// The score as shown: written, engraved and mapped.
#[derive(Clone, Debug)]
pub struct ScoreDoc {
    pub of: ScoreOf,
    pub parts: Vec<ScorePart>,
    pub sheet: LeadSheet,
    pub pages: Vec<Page>,
    pub map: Map,
    /// Each bar's start (project time) and, last, the score's end.
    pub bars: Vec<MusicalTime>,
    pub written: Vec<WrittenAt>,
    /// Each note's clip.
    pub clips: HashMap<NoteId, ClipId>,
}

impl ScoreDoc {
    pub fn start(&self) -> MusicalTime {
        self.bars.first().copied().unwrap_or(MusicalTime::ZERO)
    }

    pub fn end(&self) -> MusicalTime {
        self.bars.last().copied().unwrap_or(MusicalTime::ZERO)
    }

    /// Project time of `at` divisions into bar `bar`.
    pub fn time_of(&self, bar: usize, at: f32) -> MusicalTime {
        let start = self.bars.get(bar).copied().unwrap_or_else(|| self.end());
        start
            + MusicalTime::from_quarters(
                f64::from(at) / f64::from(faderframe_leadsheet::score::DIVISIONS),
            )
    }

    /// The bar `t` falls in and how far into it (divisions).
    pub fn place_of(&self, t: MusicalTime) -> Option<(usize, f32)> {
        let n = self.bars.len().checked_sub(1)?;
        let i = self.bars[..n]
            .iter()
            .rposition(|b| *b <= t)
            .unwrap_or(0)
            .min(n.saturating_sub(1));
        let q = (t - self.bars[i]).quarters();
        Some((
            i,
            (q * f64::from(faderframe_leadsheet::score::DIVISIONS)) as f32,
        ))
    }

    /// The note an engraved event's pitch was written from.
    pub fn note_at(&self, p: Pointed) -> Option<NoteId> {
        let m = self.sheet.parts.get(p.part)?.measures.get(p.bar)?;
        let e = m.all_events().nth(p.event)?;
        e.sources
            .get(p.pitch?)
            .map(|id| NoteId(*id))
            .filter(|n| self.clips.contains_key(n))
    }

    /// Where a note was written.
    pub fn written(&self, note: NoteId) -> Option<&WrittenAt> {
        self.written.iter().find(|w| w.id == note.raw())
    }

    /// The spelling a note was written with: its letter and alteration.
    pub fn spelt(&self, note: NoteId) -> Option<(u8, i8, i8)> {
        let w = self.written(note)?;
        self.sheet.parts.get(w.part)?.measures.iter().find_map(|m| {
            m.all_events().find_map(|e| {
                let i = e.sources.iter().position(|s| *s == note.raw())?;
                e.pitches().nth(i).map(|p| (p.step, p.alter, p.octave))
            })
        })
    }

    /// The score's notes in `part` and `voice` (1 or 2) in time order.
    pub fn line(&self, part: usize, voice: u8) -> Vec<&WrittenAt> {
        let mut out: Vec<&WrittenAt> = self
            .written
            .iter()
            .filter(|w| w.part == part && w.voice == voice)
            .collect();
        out.sort_by(|a, b| a.start.total_cmp(&b.start));
        out
    }

    /// The part a note was written in.
    pub fn part_of(&self, note: NoteId) -> Option<usize> {
        self.written(note).map(|w| w.part)
    }

    /// The key the score is written in.
    pub fn key(&self) -> KeySig {
        self.sheet.key
    }
}

/// The score's state in the session.
#[derive(Debug, Default)]
pub(crate) struct ScoreState {
    of: ScoreOf,
    settings: ScoreSettings,
    cursor: Option<ScoreCursor>,
    /// The notes entered last (a chord is added to, a tie holds them on).
    entered: Vec<NoteId>,
    /// The doc and the key of what it was made from.
    cache: RefCell<Option<(u64, Arc<ScoreDoc>)>>,
}

/// One clip's notes and notation as an edit leaves them.
struct Change {
    midi: MidiClip,
    notation: ClipNotation,
}

/// Several clips' changes, one undo step.
#[derive(Default)]
struct Changes {
    clips: BTreeMap<ClipId, Change>,
}

impl Changes {
    fn of<'a>(&'a mut self, s: &Session, clip: ClipId) -> Option<&'a mut Change> {
        match self.clips.entry(clip) {
            std::collections::btree_map::Entry::Occupied(e) => Some(e.into_mut()),
            std::collections::btree_map::Entry::Vacant(e) => {
                let midi = s.project.clip(clip)?.as_midi()?.clone();
                Some(e.insert(Change {
                    midi,
                    notation: s.project.clip_notation(clip),
                }))
            }
        }
    }
}

/// Map a project dynamic to the sheet's.
fn sheet_dynamic(d: Dynamic) -> Option<faderframe_leadsheet::score::Dynamic> {
    faderframe_leadsheet::score::Dynamic::of_name(d.name())
}

fn grid_of(g: ScoreGrid) -> faderframe_leadsheet::Grid {
    match g {
        ScoreGrid::Auto => faderframe_leadsheet::Grid::Auto,
        ScoreGrid::Straight => faderframe_leadsheet::Grid::Straight,
        ScoreGrid::Triplets => faderframe_leadsheet::Grid::Triplets,
    }
}

/// Letter `step` (with the key's accidental) in the octave whose place
/// on the staff is nearest `near`'s (a fourth up beats a fifth down), or
/// (`above`) the lowest place above it.
fn letter_near(step: u8, near: u8, sig: KeySig, above: bool) -> u8 {
    let (s, _, o) = faderframe_leadsheet::build::spell(near, sig);
    let from = i32::from(s) + 7 * i32::from(o);
    let step = i32::from(step % 7);
    let mut best = None::<i32>;
    for octave in -1..10 {
        let d = step + 7 * octave;
        if above && d <= from {
            continue;
        }
        let k = letter_key(step as u8, octave, sig);
        if !(0..128).contains(&k) {
            continue;
        }
        let dist = (d - from).abs();
        let better = best.is_none_or(|b: i32| {
            let bd = (b - from).abs();
            dist < bd || (dist == bd && d > b)
        });
        if better {
            best = Some(d);
        }
    }
    best.map_or(near, |d| {
        letter_key((d.rem_euclid(7)) as u8, d.div_euclid(7), sig).clamp(0, 127) as u8
    })
}

/// The key a letter takes in a key signature in octave `octave` (4 from
/// middle C).
fn letter_key(step: u8, octave: i32, sig: KeySig) -> i32 {
    (octave + 1) * 12 + i32::from(NATURAL[usize::from(step % 7)]) + i32::from(sig.alter_of(step))
}

impl Session {
    // --- reading ---------------------------------------------------------

    /// What the score shows (as asked).
    pub fn score_of(&self) -> &ScoreOf {
        &self.score.of
    }

    pub fn score_settings(&self) -> ScoreSettings {
        self.score.settings
    }

    /// Where the next note goes.
    pub fn score_cursor(&self) -> Option<ScoreCursor> {
        self.score.cursor
    }

    /// The notes entered last.
    pub fn score_entered(&self) -> &[NoteId] {
        &self.score.entered
    }

    /// The score as shown, made again only when what it reads changed
    /// (cheap to ask every paint).
    pub fn score(&self) -> Option<Arc<ScoreDoc>> {
        let parts = self.score_parts()?;
        let key = self.score_key(&parts);
        if let Some((k, doc)) = self.score.cache.borrow().as_ref()
            && *k == key
        {
            return Some(doc.clone());
        }
        let doc = Arc::new(self.make_score(parts, self.score.settings.layout, true)?);
        *self.score.cache.borrow_mut() = Some((key, doc.clone()));
        Some(doc)
    }

    /// The tracks (and their MIDI clips) the score shows.
    fn score_parts(&self) -> Option<Vec<(TrackId, Vec<ClipId>)>> {
        let p = &self.project;
        let midi = |c: &ClipId| p.clip(*c).is_some_and(|c| c.as_midi().is_some());
        match &self.score.of {
            ScoreOf::Editor => {
                let clip = self
                    .editor_clip
                    .filter(midi)
                    .or_else(|| self.selection.clips.iter().copied().find(midi))?;
                Some(vec![(p.clip(clip)?.track, vec![clip])])
            }
            ScoreOf::Clip(c) => {
                let clip = p.clip(*c).filter(|c| c.as_midi().is_some())?;
                Some(vec![(clip.track, vec![*c])])
            }
            ScoreOf::Tracks(tracks) => {
                let launcher = p.launcher_clips();
                let out: Vec<(TrackId, Vec<ClipId>)> = p
                    .folder_order()
                    .into_iter()
                    .filter(|t| tracks.contains(&t.id))
                    .filter_map(|t| {
                        let mut clips: Vec<ClipId> = t
                            .clips
                            .iter()
                            .copied()
                            .filter(|c| midi(c) && !launcher.contains(c))
                            .collect();
                        clips.sort_by_key(|c| p.clip(*c).map(|c| c.start));
                        (!clips.is_empty()).then_some((t.id, clips))
                    })
                    .collect();
                (!out.is_empty()).then_some(out)
            }
        }
    }

    /// What the score is made from, hashed.
    fn score_key(&self, parts: &[(TrackId, Vec<ClipId>)]) -> u64 {
        let p = &self.project;
        let mut h = std::hash::DefaultHasher::new();
        self.score.settings.layout.hash(&mut h);
        self.score.settings.entering.hash(&mut h);
        p.name.hash(&mut h);
        for (t, clips) in parts {
            t.hash(&mut h);
            p.track(*t).map(|t| t.name.as_str()).hash(&mut h);
            for c in clips {
                c.hash(&mut h);
                if let Some(clip) = p.clip(*c)
                    && let Some(m) = clip.as_midi()
                {
                    clip.start.0.hash(&mut h);
                    m.length.0.hash(&mut h);
                    for n in &m.notes {
                        (n.id, n.start.0, n.length.0, n.key, n.muted).hash(&mut h);
                    }
                }
                p.notation.get(c).hash(&mut h);
            }
        }
        format!("{:?}", p.timeline.meter).hash(&mut h);
        format!("{:?}{:?}", p.keys, p.chords).hash(&mut h);
        h.finish()
    }

    /// Write and engrave the score of `parts` (`editing`: an empty bar
    /// after the last while entering notes, for going on).
    fn make_score(
        &self,
        parts: Vec<(TrackId, Vec<ClipId>)>,
        layout: ScoreLayout,
        editing: bool,
    ) -> Option<ScoreDoc> {
        let p = &self.project;
        let meter = &p.timeline.meter;
        let clips: Vec<(&faderframe_project::Clip, &MidiClip)> = parts
            .iter()
            .flat_map(|(_, cs)| cs.iter())
            .filter_map(|c| p.clip(*c).and_then(|c| Some((c, c.as_midi()?))))
            .collect();
        let start = clips.iter().map(|(c, _)| c.start).min()?;
        let end = clips
            .iter()
            .map(|(c, m)| c.start + m.length.max(MusicalTime(1)))
            .max()?;
        let first = meter.bar_at(start);
        let mut last = meter.bar_at(end - MusicalTime(1));
        if editing && self.score.settings.entering {
            last += 1;
        }
        let bars: Vec<Bar> = (first..=last.max(first))
            .map(|b| {
                let sig = meter.signature_of_bar(b);
                Bar {
                    start: meter.bar_start(b).quarters(),
                    time: (sig.numerator, sig.denominator),
                }
            })
            .collect();
        let mut starts: Vec<MusicalTime> = (first..=last.max(first))
            .map(|b| meter.bar_start(b))
            .collect();
        starts.push(meter.bar_start(last.max(first) + 1));
        let from = starts[0];
        let to = *starts.last()?;
        // The staves: a track each, a keyboard's (or a wide chordal part)
        // on a grand staff.
        let mut staves: Vec<ScoreStaff> = Vec::new();
        let mut score_parts: Vec<ScorePart> = Vec::new();
        let mut owner: HashMap<NoteId, ClipId> = HashMap::new();
        for (track, clips) in &parts {
            let name = p.track(*track).map_or_else(String::new, |t| t.name.clone());
            let mut notes: Vec<Written> = Vec::new();
            let mut display = Vec::new();
            for c in clips {
                let Some(clip) = p.clip(*c) else { continue };
                let Some(m) = clip.as_midi() else { continue };
                let notation = p.notation.get(c);
                display.push((
                    clip.start.quarters(),
                    (clip.start + m.length).quarters(),
                    notation.map_or_else(Display::default, |n| Display {
                        grid: grid_of(n.grid),
                        legato: n.legato,
                    }),
                ));
                for n in m.notes.iter().filter(|n| !n.muted && n.start < m.length) {
                    owner.insert(n.id, *c);
                    let mut w = Written::new(
                        n.id.raw(),
                        (clip.start + n.start).quarters(),
                        (clip.start + n.end().min(m.length)).quarters(),
                        n.key,
                    );
                    if let Some(x) = notation.and_then(|x| x.notes.get(&n.id)) {
                        w.voice = x.voice;
                        w.step = x.step;
                        w.lyric.clone_from(&x.lyric);
                        w.marks = faderframe_leadsheet::score::Marks {
                            staccato: x.marks.staccato,
                            accent: x.marks.accent,
                            tenuto: x.marks.tenuto,
                            marcato: x.marks.marcato,
                        };
                        w.dynamic = x.dynamic.and_then(sheet_dynamic);
                    }
                    notes.push(w);
                }
            }
            let plain: Vec<faderframe_leadsheet::Note> = notes
                .iter()
                .map(|w| faderframe_leadsheet::Note {
                    start: w.start,
                    end: w.end,
                    key: w.key,
                })
                .collect();
            let grand = crate::leadsheet::keyboard(&name)
                || faderframe_leadsheet::build::wants_grand_staff(&plain);
            if grand && !notes.is_empty() {
                let right = faderframe_leadsheet::build::hand_of(&plain);
                let (mut r, mut l) = (Vec::new(), Vec::new());
                for (w, is_right) in notes.into_iter().zip(right) {
                    if is_right { r.push(w) } else { l.push(w) }
                }
                staves.push(ScoreStaff {
                    name: name.clone(),
                    notes: r,
                    clef: Some(Clef::Treble),
                    brace: true,
                    display: display.clone(),
                });
                staves.push(ScoreStaff {
                    name: String::new(),
                    notes: l,
                    clef: Some(Clef::Bass),
                    brace: false,
                    display,
                });
                for (lower, clef) in [(false, Clef::Treble), (true, Clef::Bass)] {
                    score_parts.push(ScorePart {
                        track: *track,
                        name: name.clone(),
                        clips: clips.clone(),
                        clef,
                        lower,
                    });
                }
            } else {
                staves.push(ScoreStaff {
                    name: name.clone(),
                    notes,
                    clef: None,
                    brace: false,
                    display,
                });
                score_parts.push(ScorePart {
                    track: *track,
                    name,
                    clips: clips.clone(),
                    clef: Clef::Treble,
                    lower: false,
                });
            }
        }
        let chords: Vec<ChordAt> = p
            .chords
            .iter()
            .filter(|c| c.end > from && c.start < to)
            .map(|c| ChordAt {
                start: c.start.quarters(),
                chord: c.chord,
            })
            .collect();
        let title = if parts.len() == 1 {
            p.track(parts[0].0)
                .map_or_else(|| p.name.clone(), |t| t.name.clone())
        } else if p.name.trim().is_empty() {
            "Score".into()
        } else {
            p.name.clone()
        };
        let notated = notate(&ScoreInput {
            title,
            composer: String::new(),
            key: p.key_at(start).or_else(|| p.main_key()),
            bars,
            tempo: Some(p.timeline.tempo.bpm_at(start)),
            staves,
            chords,
        });
        // The clefs as written.
        for (part, sp) in score_parts.iter_mut().zip(&notated.sheet.parts) {
            part.clef = sp.clef;
        }
        let n = notated.sheet.parts.len();
        let layout = match layout {
            ScoreLayout::Pages => Layout::for_staves(n),
            ScoreLayout::Strip => Layout {
                continuous: true,
                margin: 24.0,
                ..Layout::for_staves(n)
            },
        };
        let (pages, map) = engrave_mapped(&notated.sheet, &layout);
        Some(ScoreDoc {
            of: self.score.of.clone(),
            parts: score_parts,
            sheet: notated.sheet,
            pages,
            map,
            bars: starts,
            written: notated.written,
            clips: owner,
        })
    }

    // --- actions ---------------------------------------------------------

    pub(crate) fn score_op(&mut self, op: ScoreOp) -> Result<()> {
        match op {
            ScoreOp::Show(of) => {
                self.score.of = of;
                self.score.cursor = None;
                self.score.entered.clear();
                self.workspace_action(WorkspaceAction::ShowView(
                    faderframe_workspace::ViewId::score(),
                ))?;
            }
            ScoreOp::Settings(s) => {
                let was = self.score.settings;
                self.score.settings = ScoreSettings {
                    voice: s.voice.clamp(1, 2),
                    dots: s.dots.min(2),
                    velocity: s.velocity.clamp(1, 127),
                    ..s
                };
                if s.entering && !was.entering && self.score.cursor.is_none() {
                    self.score.cursor = self.cursor_after_selection();
                }
                // Step input enters the value chosen.
                if let Some(st) = self.step_input() {
                    self.set_step_input(Some(crate::StepInput {
                        step: self.score.settings.length(),
                        ..st
                    }));
                }
            }
            ScoreOp::Cursor(c) => {
                self.score.cursor = c;
                self.score.entered.clear();
            }
            ScoreOp::Key(k) => self.score_key_pressed(k)?,
            ScoreOp::EnterAt { part, at, diatonic } => {
                let doc = self.score_doc()?;
                let step = diatonic.rem_euclid(7) as u8;
                let key = letter_key(step, diatonic.div_euclid(7), doc.key());
                self.score.cursor = Some(ScoreCursor { part, at });
                self.enter(&[(key.clamp(0, 127) as u8, Some(step))], false)?;
            }
            ScoreOp::SetDuration { notes, duration } => self.set_durations(&notes, duration)?,
            ScoreOp::Accidental { notes, alter } => self.set_accidental(&notes, alter)?,
            ScoreOp::Respell(notes) => self.respell(&notes)?,
            ScoreOp::Transpose { notes, by } => self.score_transpose(&notes, by)?,
            ScoreOp::Voice { notes, voice } => self.set_voice(&notes, voice)?,
            ScoreOp::Delete(notes) => self.score_delete(&notes)?,
            ScoreOp::Lyrics { from, text } => self.set_lyrics(from, &text)?,
            ScoreOp::ChordSymbol { at, text } => self.set_chord_symbol(at, &text)?,
            ScoreOp::Dynamic { notes, dynamic } => self.set_dynamic(&notes, dynamic)?,
            ScoreOp::Mark { notes, mark, on } => self.set_mark(&notes, mark, on)?,
            ScoreOp::Display {
                clips,
                grid,
                legato,
            } => {
                let mut ch = Changes::default();
                for c in &clips {
                    if let Some(x) = ch.of(self, *c) {
                        x.notation.grid = grid;
                        x.notation.legato = legato;
                    }
                }
                self.commit_score("Score Display", ch, None)?;
            }
            ScoreOp::Quantize(notes) => self.quantize_to_score(&notes)?,
            ScoreOp::Export(path) => {
                self.export_score(&path)?;
            }
        }
        self.revision += 1;
        Ok(())
    }

    fn score_doc(&self) -> Result<Arc<ScoreDoc>> {
        self.score()
            .ok_or_else(|| SessionError::Other("the score shows no MIDI clip".into()))
    }

    /// The selected notes the score shows.
    fn score_selection(&self, doc: &ScoreDoc) -> Vec<NoteId> {
        self.selection
            .notes
            .iter()
            .copied()
            .filter(|n| doc.clips.contains_key(n))
            .collect()
    }

    /// The cursor after the selected notes (or at the score's start).
    fn cursor_after_selection(&self) -> Option<ScoreCursor> {
        let doc = self.score()?;
        let sel = self.score_selection(&doc);
        let last = sel
            .iter()
            .filter_map(|n| doc.written(*n))
            .max_by(|a, b| a.end.total_cmp(&b.end));
        Some(match last {
            Some(w) => ScoreCursor {
                part: w.part,
                at: MusicalTime::from_quarters(w.end),
            },
            None => ScoreCursor {
                part: 0,
                at: doc.start(),
            },
        })
    }

    fn score_key_pressed(&mut self, k: ScoreKey) -> Result<()> {
        let doc = self.score_doc()?;
        let sel = self.score_selection(&doc);
        let entering = self.score.settings.entering;
        // Outside note input, durations and voices go to the selection.
        let apply_to_selection = !entering && !sel.is_empty();
        match k {
            ScoreKey::Letter { step, chord } => {
                if self.score.cursor.is_none() {
                    self.score.cursor = self.cursor_after_selection();
                }
                let sig = doc.key();
                let key = if chord && !self.score.entered.is_empty() {
                    let top = self.top_entered(&doc).unwrap_or(60);
                    letter_near(step, top, sig, true)
                } else {
                    let near = self.note_before_cursor(&doc);
                    letter_near(step, near, sig, false)
                };
                self.enter(&[(key, Some(step % 7))], chord)?;
            }
            ScoreKey::Value(v) => {
                self.score.settings.value = v;
                self.score.settings.dots = 0;
                if apply_to_selection {
                    self.set_durations(&sel, self.score.settings.duration())?;
                }
                self.follow_step_input();
            }
            ScoreKey::Dot => {
                let s = &mut self.score.settings;
                s.dots = if s.dots == 0 { 1 } else { 0 };
                if apply_to_selection {
                    self.set_durations(&sel, self.score.settings.duration())?;
                }
                self.follow_step_input();
            }
            ScoreKey::Triplet => {
                self.score.settings.triplet = !self.score.settings.triplet;
                if apply_to_selection {
                    self.set_durations(&sel, self.score.settings.duration())?;
                }
                self.follow_step_input();
            }
            ScoreKey::Rest => {
                let cursor = self.score.cursor.or_else(|| self.cursor_after_selection());
                if let Some(c) = cursor {
                    let len = self.score.settings.length();
                    let mut ch = Changes::default();
                    self.clear_voice(
                        &doc,
                        &mut ch,
                        c.part,
                        self.score.settings.voice,
                        c.at,
                        c.at + len,
                    );
                    if !ch.clips.is_empty() {
                        self.commit_score("Enter Rest", ch, None)?;
                    }
                    self.score.cursor = Some(ScoreCursor {
                        at: c.at + len,
                        ..c
                    });
                    self.score.entered.clear();
                }
            }
            ScoreKey::Tie => {
                let notes = if self.score.entered.is_empty() {
                    sel
                } else {
                    self.score.entered.clone()
                };
                self.tie(&notes)?;
            }
            ScoreKey::Transpose(by) => {
                let notes = if sel.is_empty() {
                    self.score.entered.clone()
                } else {
                    sel
                };
                self.score_transpose(&notes, by)?;
            }
            ScoreKey::Respell => {
                let notes = if sel.is_empty() {
                    self.score.entered.clone()
                } else {
                    sel
                };
                self.respell(&notes)?;
            }
            ScoreKey::Delete => self.score_delete(&sel)?,
            ScoreKey::Voice(v) => {
                self.score.settings.voice = v.clamp(1, 2);
                if apply_to_selection {
                    self.set_voice(&sel, Some(v.clamp(1, 2)))?;
                }
            }
            ScoreKey::Next | ScoreKey::Previous => {
                let forward = k == ScoreKey::Next;
                if !sel.is_empty() && !entering {
                    if let Some(n) = self.neighbour(&doc, &sel, forward) {
                        self.selection.select_notes(&[n], SelectMode::Replace);
                    }
                } else if let Some(c) = self.score.cursor.or_else(|| self.cursor_after_selection())
                {
                    let len = self.score.settings.length();
                    let at = if forward {
                        (c.at + len).min(doc.end())
                    } else {
                        (c.at - len).max(doc.start())
                    };
                    self.score.cursor = Some(ScoreCursor { at, ..c });
                    self.score.entered.clear();
                }
            }
            ScoreKey::ToggleEntering => {
                let mut s = self.score.settings;
                s.entering = !s.entering;
                if s.entering && self.score.cursor.is_none() {
                    self.score.cursor = self.cursor_after_selection();
                }
                self.score.settings = s;
            }
            ScoreKey::Escape => {
                self.score.settings.entering = false;
                self.score.cursor = None;
                self.score.entered.clear();
                self.selection.select_notes(&[], SelectMode::Replace);
            }
        }
        Ok(())
    }

    /// Step input (on) enters the score's value.
    fn follow_step_input(&mut self) {
        if let Some(st) = self.step_input() {
            self.set_step_input(Some(crate::StepInput {
                step: self.score.settings.length(),
                ..st
            }));
        }
    }

    /// The highest of the notes entered last.
    fn top_entered(&self, doc: &ScoreDoc) -> Option<u8> {
        self.score
            .entered
            .iter()
            .filter_map(|n| self.note_of(doc, *n))
            .map(|(_, n)| n.key)
            .max()
    }

    /// A note of the score and its clip.
    fn note_of(&self, doc: &ScoreDoc, note: NoteId) -> Option<(ClipId, MidiNote)> {
        let clip = *doc.clips.get(&note)?;
        let n = self.project.clip(clip)?.as_midi()?.note(note)?;
        Some((clip, *n))
    }

    /// The key a letter is placed near: the note before the cursor in its
    /// part (the one entered last, else the latest one starting before it),
    /// else the middle of the staff.
    fn note_before_cursor(&self, doc: &ScoreDoc) -> u8 {
        if let Some(k) = self.top_entered(doc) {
            return k;
        }
        let Some(c) = self.score.cursor else {
            return 67;
        };
        let at = c.at.quarters();
        doc.written
            .iter()
            .filter(|w| w.part == c.part && w.start < at)
            .max_by(|a, b| a.start.total_cmp(&b.start))
            .and_then(|w| self.note_of(doc, NoteId(w.id)).map(|(_, n)| n.key))
            .unwrap_or(match doc.parts.get(c.part).map(|p| p.clef) {
                Some(Clef::Bass) => 50,
                Some(Clef::Bass8vb) => 38,
                Some(Clef::Treble8vb) => 55,
                _ => 71,
            })
    }

    /// The note after (or before) the selection in its part's line.
    fn neighbour(&self, doc: &ScoreDoc, sel: &[NoteId], forward: bool) -> Option<NoteId> {
        let ws: Vec<&WrittenAt> = sel.iter().filter_map(|n| doc.written(*n)).collect();
        let pivot = if forward {
            ws.iter().max_by(|a, b| a.start.total_cmp(&b.start))?
        } else {
            ws.iter().min_by(|a, b| a.start.total_cmp(&b.start))?
        };
        let line = doc.line(pivot.part, pivot.voice);
        let next = if forward {
            line.iter().find(|w| w.start > pivot.start + 1e-9)
        } else {
            line.iter().rev().find(|w| w.start < pivot.start - 1e-9)
        }?;
        Some(NoteId(next.id))
    }

    /// The clip on `part`'s track at `at`: the one covering it, else the
    /// last one starting before it (lengthened to reach `to`).
    fn clip_for_entry(
        &self,
        doc: &ScoreDoc,
        ch: &mut Changes,
        part: usize,
        at: MusicalTime,
        to: MusicalTime,
    ) -> Result<ClipId> {
        let p = doc
            .parts
            .get(part)
            .ok_or_else(|| SessionError::Other("no such staff".into()))?;
        let covering = p.clips.iter().copied().find(|c| {
            self.project
                .clip(*c)
                .and_then(|clip| Some((clip.start, clip.as_midi()?.length)))
                .is_some_and(|(s, l)| s <= at && at < s + l)
        });
        let clip = covering
            .or_else(|| {
                p.clips
                    .iter()
                    .copied()
                    .filter(|c| self.project.clip(*c).is_some_and(|clip| clip.start <= at))
                    .max_by_key(|c| self.project.clip(*c).map(|clip| clip.start))
            })
            .ok_or_else(|| {
                SessionError::Other(format!("no MIDI clip of '{}' starts before here", p.name))
            })?;
        let start = self.project.clip(clip).map_or(at, |c| c.start);
        let meter = &self.project.timeline.meter;
        if let Some(x) = ch.of(self, clip)
            && start + x.midi.length < to
        {
            // Lengthened to the end of the bar the note ends in.
            let bar = meter.bar_at(to - MusicalTime(1));
            x.midi.length = meter.bar_start(bar + 1) - start;
        }
        Ok(clip)
    }

    /// Clear `part`'s voice `voice` over `from..to`: notes starting inside
    /// go, a note sounding into it is cut where it starts.
    fn clear_voice(
        &self,
        doc: &ScoreDoc,
        ch: &mut Changes,
        part: usize,
        voice: u8,
        from: MusicalTime,
        to: MusicalTime,
    ) {
        let (a, z) = (from.quarters(), to.quarters());
        for w in doc.written.iter().filter(|w| {
            w.part == part && w.voice == voice && w.start < z - 1e-9 && w.end > a + 1e-9
        }) {
            let id = NoteId(w.id);
            let Some(clip) = doc.clips.get(&id).copied() else {
                continue;
            };
            let Some(start) = self.project.clip(clip).map(|c| c.start) else {
                continue;
            };
            let Some(x) = ch.of(self, clip) else { continue };
            if w.start >= a - 1e-9 {
                x.midi.notes.retain(|n| n.id != id);
            } else if let Some(n) = x.midi.notes.iter_mut().find(|n| n.id == id) {
                n.length = (from - start - n.start).max(MusicalTime(1));
            }
        }
    }

    /// Enter `keys` (with the letters they are spelt on) at the cursor for
    /// the chosen value, in the chosen voice; `chord`: added to the chord
    /// entered last instead.
    fn enter(&mut self, keys: &[(u8, Option<u8>)], chord: bool) -> Result<()> {
        let doc = self.score_doc()?;
        let settings = self.score.settings;
        let sig = doc.key();
        let chord_of = (chord && !self.score.entered.is_empty())
            .then(|| self.note_of(&doc, self.score.entered[0]))
            .flatten();
        let (part, at, length) = match chord_of {
            Some((clip, n)) => {
                let start = self
                    .project
                    .clip(clip)
                    .map_or(MusicalTime::ZERO, |c| c.start);
                let part = doc.part_of(self.score.entered[0]).unwrap_or(0);
                (part, start + n.start, n.length)
            }
            None => {
                let c = self.score.cursor.unwrap_or(ScoreCursor {
                    part: 0,
                    at: doc.start(),
                });
                (
                    c.part.min(doc.parts.len().saturating_sub(1)),
                    c.at,
                    settings.length(),
                )
            }
        };
        let voice = settings.voice;
        let mut ch = Changes::default();
        if chord_of.is_none() {
            self.clear_voice(&doc, &mut ch, part, voice, at, at + length);
        }
        let clip = self.clip_for_entry(&doc, &mut ch, part, at, at + length)?;
        let clip_start = self.project.clip(clip).map_or(at, |c| c.start);
        // Played as loud as the dynamic in effect, else the note before.
        let velocity = self
            .velocity_at(&doc, part, at)
            .unwrap_or(settings.velocity);
        let channel = self
            .project
            .clip(clip)
            .and_then(|c| c.as_midi())
            .and_then(|m| {
                m.notes
                    .iter()
                    .filter(|n| clip_start + n.start <= at)
                    .max_by_key(|n| n.start)
            })
            .map_or(0, |n| n.channel);
        // A second voice sounding there: a note of the first is told so.
        let (a, z) = (at.quarters(), (at + length).quarters());
        let other = doc
            .written
            .iter()
            .any(|w| w.part == part && w.voice != voice && w.start < z && w.end > a);
        let explicit = (voice == 2 || other).then_some(voice);
        let mut ids = Vec::new();
        for (key, step) in keys {
            let id: NoteId = self.project.ids.allocate();
            ids.push(id);
            let Some(x) = ch.of(self, clip) else { continue };
            x.midi.notes.push(MidiNote {
                id,
                start: at - clip_start,
                length,
                key: *key,
                velocity,
                channel,
                muted: false,
                release: None,
                time_locked: false,
                articulation: None,
                fine: None,
            });
            // The letter kept where the key would spell it otherwise.
            let spelt = faderframe_leadsheet::build::spell(*key, sig).0;
            let keep = step.filter(|s| *s != spelt);
            x.notation.edit(id, |n| {
                n.step = keep;
                n.voice = explicit;
            });
        }
        let label = if chord_of.is_some() {
            "Add to Chord"
        } else if keys.len() > 1 {
            "Enter Chord"
        } else {
            "Enter Note"
        };
        self.commit_score(label, ch, Some(&ids))?;
        if chord_of.is_some() {
            self.score.entered.extend(ids);
        } else {
            self.score.entered = ids;
            self.score.cursor = Some(ScoreCursor {
                part,
                at: at + length,
            });
        }
        Ok(())
    }

    /// The velocity a note entered at `at` in `part` is played at: the
    /// dynamic in effect there, else the note before's.
    fn velocity_at(&self, doc: &ScoreDoc, part: usize, at: MusicalTime) -> Option<u8> {
        let track = doc.parts.get(part)?.track;
        let mut best: Option<(MusicalTime, u8, bool)> = None;
        for c in &doc.parts.get(part)?.clips {
            let Some(clip) = self.project.clip(*c) else {
                continue;
            };
            let Some(m) = clip.as_midi() else { continue };
            let notation = self.project.notation.get(c);
            for n in &m.notes {
                let t = clip.start + n.start;
                if t > at || clip.track != track {
                    continue;
                }
                let marked = notation
                    .and_then(|x| x.notes.get(&n.id))
                    .and_then(|x| x.dynamic);
                let cand = match marked {
                    Some(d) => (t, d.velocity(), true),
                    None => (t, n.velocity, false),
                };
                // A dynamic before wins over a plain note before; among
                // the same kind the latest.
                best = match best {
                    None => Some(cand),
                    Some(b) if cand.2 && (!b.2 || cand.0 >= b.0) => Some(cand),
                    Some(b) if !cand.2 && !b.2 && cand.0 >= b.0 => Some(cand),
                    Some(b) => Some(b),
                };
            }
        }
        best.map(|(_, v, _)| v)
    }

    /// Commit the changes as one step; `select`: those notes selected.
    fn commit_score(&mut self, label: &str, ch: Changes, select: Option<&[NoteId]>) -> Result<()> {
        let mut commands = Vec::new();
        for (
            clip,
            Change {
                mut midi,
                mut notation,
            },
        ) in ch.clips
        {
            let Some(c) = self.project.clip(clip) else {
                continue;
            };
            let start = c.start;
            for n in &mut midi.notes {
                n.start = n.start.max(MusicalTime::ZERO);
                n.length = n.length.max(MusicalTime(1));
                n.velocity = n.velocity.clamp(1, 127);
            }
            midi.sort_notes();
            midi.prune_expressions();
            notation.tidy();
            if c.as_midi() != Some(&midi) {
                commands.push(Command::SetClipContent {
                    clip,
                    start,
                    content: Box::new(ClipContent::Midi(midi)),
                });
            }
            if self.project.clip_notation(clip) != notation {
                commands.push(Command::SetClipNotation {
                    clip,
                    notation: (!notation.is_default()).then(|| Box::new(notation)),
                });
            }
        }
        if !commands.is_empty() {
            self.edit(Command::Batch {
                label: label.into(),
                commands,
            })?;
        }
        if let Some(ids) = select {
            self.selection.select_notes(ids, SelectMode::Replace);
        }
        Ok(())
    }

    /// The notes' written lengths set (their MIDI lengths, a staccato
    /// note's half of it); what their voices had after them inside goes.
    fn set_durations(&mut self, notes: &[NoteId], duration: Duration) -> Result<()> {
        let doc = self.score_doc()?;
        let length = length_of(duration);
        let mut ch = Changes::default();
        for note in notes {
            let (Some(w), Some((clip, n))) = (doc.written(*note), self.note_of(&doc, *note)) else {
                continue;
            };
            let start = MusicalTime::from_quarters(w.start);
            let (part, voice) = (w.part, w.voice);
            // Notes of the voice starting inside the new length go (not
            // this note's chord).
            let (a, z) = (w.start, (start + length).quarters());
            let gone: Vec<NoteId> = doc
                .written
                .iter()
                .filter(|x| {
                    x.part == part && x.voice == voice && x.start > a + 1e-9 && x.start < z - 1e-9
                })
                .map(|x| NoteId(x.id))
                .collect();
            for g in gone {
                if let Some(c) = doc.clips.get(&g).copied()
                    && let Some(x) = ch.of(self, c)
                {
                    x.midi.notes.retain(|n| n.id != g);
                }
            }
            let Some(x) = ch.of(self, clip) else { continue };
            let staccato = x.notation.note(*note).marks.staccato;
            let played = if staccato {
                MusicalTime((length.0 as f64 * STACCATO_LENGTH) as i64)
            } else {
                length
            };
            if let Some(m) = x.midi.notes.iter_mut().find(|m| m.id == n.id) {
                // From where the score writes it.
                let clip_start = self
                    .project
                    .clip(clip)
                    .map_or(MusicalTime::ZERO, |c| c.start);
                m.start = (start - clip_start).max(MusicalTime::ZERO);
                m.length = played;
            }
        }
        self.commit_score("Change Duration", ch, Some(notes))
    }

    /// Hold the notes on by the chosen value (a tie).
    fn tie(&mut self, notes: &[NoteId]) -> Result<()> {
        let doc = self.score_doc()?;
        let add = self.score.settings.length();
        let mut ch = Changes::default();
        let mut end = None::<(usize, MusicalTime)>;
        for note in notes {
            let (Some(w), Some((clip, _))) = (doc.written(*note), self.note_of(&doc, *note)) else {
                continue;
            };
            let from = MusicalTime::from_quarters(w.end);
            self.clear_voice(&doc, &mut ch, w.part, w.voice, from, from + add);
            if let Some(x) = ch.of(self, clip)
                && let Some(m) = x.midi.notes.iter_mut().find(|m| m.id == *note)
            {
                let clip_start = self
                    .project
                    .clip(clip)
                    .map_or(MusicalTime::ZERO, |c| c.start);
                m.length = (from + add) - clip_start - m.start;
                if clip_start + x.midi.length < from + add {
                    x.midi.length = from + add - clip_start;
                }
            }
            end = Some((w.part, from + add));
        }
        self.commit_score("Tie", ch, Some(notes))?;
        if let Some((part, at)) = end {
            self.score.cursor = Some(ScoreCursor { part, at });
        }
        Ok(())
    }

    /// The notes on their letters with alteration `alter`.
    fn set_accidental(&mut self, notes: &[NoteId], alter: i8) -> Result<()> {
        let doc = self.score_doc()?;
        let mut ch = Changes::default();
        for note in notes {
            let (Some((step, now, _)), Some((clip, n))) =
                (doc.spelt(*note), self.note_of(&doc, *note))
            else {
                continue;
            };
            let key =
                (i32::from(n.key) - i32::from(now) + i32::from(alter.clamp(-2, 2))).clamp(0, 127);
            let Some(x) = ch.of(self, clip) else { continue };
            if let Some(m) = x.midi.notes.iter_mut().find(|m| m.id == *note) {
                m.key = key as u8;
            }
            x.notation.edit(*note, |e| e.step = Some(step));
        }
        self.commit_score("Accidental", ch, Some(notes))
    }

    /// The notes spelt the next way (C♯ → D♭ → B𝄪 → C♯).
    fn respell(&mut self, notes: &[NoteId]) -> Result<()> {
        let doc = self.score_doc()?;
        let sig = doc.key();
        let mut ch = Changes::default();
        for note in notes {
            let (Some((step, alter, _)), Some((clip, n))) =
                (doc.spelt(*note), self.note_of(&doc, *note))
            else {
                continue;
            };
            let ways = spellings(n.key, sig);
            let i = ways.iter().position(|w| *w == (step, alter)).unwrap_or(0);
            let next = ways[(i + 1) % ways.len()];
            let keep = (next != ways[0]).then_some(next.0);
            if let Some(x) = ch.of(self, clip) {
                x.notation.edit(*note, |e| e.step = keep);
            }
        }
        self.commit_score("Respell", ch, Some(notes))
    }

    /// Transpose the notes: by semitones spelt sharp going up and flat
    /// going down (the key's own notes as the key has them), by octaves
    /// keeping their spelling, or by steps of the key.
    fn score_transpose(&mut self, notes: &[NoteId], by: Interval) -> Result<()> {
        let doc = self.score_doc()?;
        let sig = doc.key();
        let mut ch = Changes::default();
        for note in notes {
            let Some((clip, n)) = self.note_of(&doc, *note) else {
                continue;
            };
            let spelt = doc.spelt(*note);
            let (key, step) = match by {
                Interval::Semitones(s) => {
                    let key = (i32::from(n.key) + s).clamp(0, 127) as u8;
                    let ways = spellings(key, sig);
                    let in_key = ways[0].1 == sig.alter_of(ways[0].0);
                    let step = if in_key {
                        None
                    } else {
                        let want = if s > 0 { 1 } else { -1 };
                        ways.iter().find(|w| w.1 == want).map(|w| w.0)
                    };
                    (i32::from(key), step.filter(|s| *s != ways[0].0))
                }
                Interval::Octaves(o) => (
                    i32::from(n.key) + 12 * o,
                    self.project.clip_notation(clip).note(*note).step,
                ),
                Interval::Steps(d) => {
                    let (step, _, octave) =
                        spelt.unwrap_or_else(|| faderframe_leadsheet::build::spell(n.key, sig));
                    let dia = i32::from(step) + 7 * i32::from(octave) + d;
                    let s = dia.rem_euclid(7) as u8;
                    (letter_key(s, dia.div_euclid(7), sig), None)
                }
            };
            if !(0..=127).contains(&key) {
                continue;
            }
            let Some(x) = ch.of(self, clip) else { continue };
            if let Some(m) = x.midi.notes.iter_mut().find(|m| m.id == *note) {
                m.key = key as u8;
            }
            x.notation.edit(*note, |e| e.step = step);
        }
        self.commit_score("Transpose", ch, Some(notes))
    }

    fn set_voice(&mut self, notes: &[NoteId], voice: Option<u8>) -> Result<()> {
        let doc = self.score_doc()?;
        let mut ch = Changes::default();
        for note in notes {
            if let Some(clip) = doc.clips.get(note).copied()
                && let Some(x) = ch.of(self, clip)
            {
                x.notation
                    .edit(*note, |e| e.voice = voice.map(|v| v.clamp(1, 2)));
            }
        }
        self.commit_score("Voice", ch, Some(notes))
    }

    fn score_delete(&mut self, notes: &[NoteId]) -> Result<()> {
        let doc = self.score_doc()?;
        let mut ch = Changes::default();
        for note in notes {
            if let Some(clip) = doc.clips.get(note).copied()
                && let Some(x) = ch.of(self, clip)
            {
                x.midi.notes.retain(|n| n.id != *note);
                x.notation.notes.remove(note);
            }
        }
        if ch.clips.is_empty() {
            return Ok(());
        }
        self.commit_score("Delete Notes", ch, Some(&[]))?;
        self.score.entered.retain(|n| !notes.contains(n));
        Ok(())
    }

    /// A line of words from `from` on: a syllable a note of its part's
    /// first voice (a chord takes one).
    fn set_lyrics(&mut self, from: NoteId, text: &str) -> Result<()> {
        let doc = self.score_doc()?;
        let Some(w) = doc.written(from).copied() else {
            return Err(SessionError::Other("that note is not in the score".into()));
        };
        // The notes from it on, a chord one (its lowest).
        let mut line: Vec<NoteId> = Vec::new();
        let mut last = f64::NEG_INFINITY;
        for x in doc.line(w.part, w.voice) {
            if x.start < w.start - 1e-9 || (x.start - last).abs() < 1e-9 {
                continue;
            }
            last = x.start;
            line.push(NoteId(x.id));
        }
        let syllables: Vec<String> = text
            .split_whitespace()
            .flat_map(|word| {
                if word == "_" {
                    return vec!["_".to_string()];
                }
                let parts: Vec<&str> = word.split('-').filter(|p| !p.is_empty()).collect();
                let n = parts.len();
                parts
                    .into_iter()
                    .enumerate()
                    .map(|(i, p)| {
                        if i + 1 < n {
                            format!("{p}-")
                        } else {
                            p.to_string()
                        }
                    })
                    .collect()
            })
            .collect();
        let mut ch = Changes::default();
        if syllables.is_empty()
            && let Some(clip) = doc.clips.get(&from).copied()
            && let Some(x) = ch.of(self, clip)
        {
            x.notation.edit(from, |e| e.lyric = None);
        }
        let mut prev: Option<NoteId> = None;
        for (note, syl) in line.iter().zip(&syllables) {
            let Some(clip) = doc.clips.get(note).copied() else {
                continue;
            };
            if syl == "_" {
                // Held from the word before: its syllable draws the line.
                if let Some(p) = prev
                    && let Some(pc) = doc.clips.get(&p).copied()
                    && let Some(x) = ch.of(self, pc)
                {
                    x.notation.edit(p, |e| {
                        if let Some(l) = &mut e.lyric
                            && !l.ends_with(['_', '-'])
                        {
                            l.push('_');
                        }
                    });
                }
                if let Some(x) = ch.of(self, clip) {
                    x.notation.edit(*note, |e| e.lyric = None);
                }
                continue;
            }
            if let Some(x) = ch.of(self, clip) {
                x.notation.edit(*note, |e| e.lyric = Some(syl.clone()));
            }
            prev = Some(*note);
        }
        self.commit_score("Lyrics", ch, None)
    }

    /// A chord symbol at `at` until the next chord (or a bar on).
    fn set_chord_symbol(&mut self, at: MusicalTime, text: &str) -> Result<()> {
        let text = text.trim();
        let p = &self.project;
        let chords = if text.is_empty() {
            p.chords.iter().filter(|c| c.start != at).copied().collect()
        } else {
            let chord = faderframe_project::harmony::Chord::parse(text)
                .ok_or_else(|| SessionError::Other(format!("'{text}' is no chord symbol")))?;
            let meter = &p.timeline.meter;
            let bar_on = meter.bar_start(meter.bar_at(at) + 1);
            let next = p
                .chords
                .iter()
                .map(|c| c.start)
                .filter(|s| *s > at)
                .min()
                .unwrap_or(bar_on);
            let end = p
                .chord_at(at)
                .filter(|c| c.start == at)
                .map_or(next, |c| c.end.max(next));
            faderframe_project::harmony::set_chord(&p.chords, at, end, Some(chord))
        };
        self.edit(Command::Batch {
            label: "Chord Symbol".into(),
            commands: vec![Command::SetChords { chords }],
        })
    }

    /// Mark the first of the notes with a dynamic: it and the notes after
    /// it on its track are played at its velocity until the next dynamic.
    fn set_dynamic(&mut self, notes: &[NoteId], dynamic: Option<Dynamic>) -> Result<()> {
        let doc = self.score_doc()?;
        let mut ch = Changes::default();
        let Some(dynamic) = dynamic else {
            for note in notes {
                if let Some(clip) = doc.clips.get(note).copied()
                    && let Some(x) = ch.of(self, clip)
                {
                    x.notation.edit(*note, |e| e.dynamic = None);
                }
            }
            return self.commit_score("Dynamic", ch, Some(notes));
        };
        let Some(first) = notes
            .iter()
            .filter_map(|n| doc.written(*n).map(|w| (*n, *w)))
            .min_by(|a, b| a.1.start.total_cmp(&b.1.start))
        else {
            return Ok(());
        };
        let (_, w) = first;
        let Some(track) = doc.parts.get(w.part).map(|p| p.track) else {
            return Ok(());
        };
        // Notes starting with it take the dynamic too (a chord).
        for n in notes {
            if let Some(x) = doc.written(*n)
                && (x.start - w.start).abs() < 1e-9
                && let Some(clip) = doc.clips.get(n).copied()
                && let Some(c) = ch.of(self, clip)
            {
                c.notation.edit(*n, |e| e.dynamic = Some(dynamic));
            }
        }
        // Velocities from it to the next dynamic on the track.
        let clips: BTreeSet<ClipId> = doc
            .parts
            .iter()
            .filter(|p| p.track == track)
            .flat_map(|p| p.clips.iter().copied())
            .collect();
        let mut timed: Vec<(MusicalTime, ClipId, NoteId, Option<Dynamic>)> = Vec::new();
        for c in &clips {
            let Some(clip) = self.project.clip(*c) else {
                continue;
            };
            let Some(m) = clip.as_midi() else { continue };
            let notation = self.project.notation.get(c);
            for n in &m.notes {
                let t = clip.start + n.start;
                let marked = notation
                    .and_then(|x| x.notes.get(&n.id))
                    .and_then(|x| x.dynamic);
                timed.push((t, *c, n.id, marked));
            }
        }
        timed.sort_by_key(|x| x.0);
        let from = MusicalTime::from_quarters(w.start);
        let until = timed
            .iter()
            .filter(|x| x.0 > from && x.3.is_some() && !notes.contains(&x.2))
            .map(|x| x.0)
            .min();
        for (t, c, n, _) in timed {
            if t < from || until.is_some_and(|u| t >= u) {
                continue;
            }
            if let Some(x) = ch.of(self, c)
                && let Some(m) = x.midi.notes.iter_mut().find(|m| m.id == n)
            {
                let marks = x.notation.note(n).marks;
                m.velocity = accented(dynamic.velocity(), &marks);
                m.fine = None;
            }
        }
        self.commit_score("Dynamic", ch, Some(notes))
    }

    /// An articulation on (or off) the notes, played: staccato half the
    /// written value, accents and marcato louder; with an articulation
    /// map the matching attribute too.
    fn set_mark(&mut self, notes: &[NoteId], mark: Mark, on: bool) -> Result<()> {
        let doc = self.score_doc()?;
        let mut ch = Changes::default();
        for note in notes {
            let (Some(w), Some((clip, _))) =
                (doc.written(*note).copied(), self.note_of(&doc, *note))
            else {
                continue;
            };
            let track = self.project.clip(clip).map(|c| c.track);
            let attribute = track.and_then(|t| self.mark_articulation(t, mark));
            let clip_start = self
                .project
                .clip(clip)
                .map_or(MusicalTime::ZERO, |c| c.start);
            let Some(x) = ch.of(self, clip) else { continue };
            let before = x.notation.note(*note).marks;
            if mark.of(&before) == on {
                continue;
            }
            x.notation.edit(*note, |e| mark.set(&mut e.marks, on));
            let Some(m) = x.midi.notes.iter_mut().find(|m| m.id == *note) else {
                continue;
            };
            let written = MusicalTime::from_quarters(w.end) - (clip_start + m.start);
            match mark {
                Mark::Staccato => {
                    m.length = if on {
                        MusicalTime((written.0 as f64 * STACCATO_LENGTH) as i64)
                    } else {
                        written
                    }
                    .max(MusicalTime(1));
                }
                Mark::Tenuto => {
                    if on {
                        m.length = written.max(MusicalTime(1));
                    }
                }
                Mark::Accent | Mark::Marcato => {
                    let by = if mark == Mark::Accent {
                        ACCENT
                    } else {
                        MARCATO
                    };
                    let v = i32::from(m.velocity) + if on { by } else { -by };
                    m.velocity = v.clamp(1, 127) as u8;
                    m.fine = None;
                }
            }
            if let Some(a) = attribute {
                m.articulation = if on {
                    Some(a)
                } else if m.articulation == Some(a) {
                    None
                } else {
                    m.articulation
                };
            }
        }
        self.commit_score(mark.label(), ch, Some(notes))
    }

    /// The attribute of a track's articulation map a mark plays by (named
    /// so, or by its symbol).
    fn mark_articulation(
        &self,
        track: TrackId,
        mark: Mark,
    ) -> Option<faderframe_project::articulation::ArticulationId> {
        let map = self.project.track_articulation_map(track)?;
        let words: &[&str] = match mark {
            Mark::Staccato => &["stacc"],
            Mark::Accent => &["accent", "sfz"],
            Mark::Tenuto => &["tenuto", "ten."],
            Mark::Marcato => &["marcato", "marc"],
        };
        map.articulations
            .iter()
            .filter(|a| a.kind == faderframe_project::articulation::ArticulationKind::Attribute)
            .find(|a| {
                let name = a.name.to_lowercase();
                let symbol = a.symbol.to_lowercase();
                words.iter().any(|w| name.contains(w) || symbol.contains(w))
            })
            .map(|a| a.id)
    }

    /// Move the notes (all the score's when none) to where the score
    /// writes them.
    fn quantize_to_score(&mut self, notes: &[NoteId]) -> Result<()> {
        let doc = self.score_doc()?;
        let mut ch = Changes::default();
        let chosen: Vec<NoteId> = if notes.is_empty() {
            doc.clips.keys().copied().collect()
        } else {
            notes.to_vec()
        };
        for note in &chosen {
            let (Some(w), Some(clip)) = (doc.written(*note).copied(), doc.clips.get(note).copied())
            else {
                continue;
            };
            let clip_start = self
                .project
                .clip(clip)
                .map_or(MusicalTime::ZERO, |c| c.start);
            let Some(x) = ch.of(self, clip) else { continue };
            let staccato = x.notation.note(*note).marks.staccato;
            let Some(m) = x.midi.notes.iter_mut().find(|m| m.id == *note) else {
                continue;
            };
            // A note placed in time keeps its timing.
            if m.time_locked {
                continue;
            }
            let start = MusicalTime::from_quarters(w.start) - clip_start;
            let written = MusicalTime::from_quarters(w.end) - MusicalTime::from_quarters(w.start);
            m.start = start.max(MusicalTime::ZERO);
            m.length = if staccato {
                MusicalTime((written.0 as f64 * STACCATO_LENGTH) as i64)
            } else {
                written
            }
            .max(MusicalTime(1));
        }
        self.commit_score("Quantize to the Score", ch, None)
    }

    /// Write the score (as printed: on pages, without note input's extra
    /// bar) to `path`: PDF (`.pdf`) or MusicXML (`.musicxml`, `.xml`).
    pub fn export_score(&mut self, path: &Path) -> Result<PathBuf> {
        let parts = self
            .score_parts()
            .ok_or_else(|| SessionError::Other("the score shows no MIDI clip".into()))?;
        let doc = self
            .make_score(parts, ScoreLayout::Pages, false)
            .ok_or_else(|| SessionError::Other("the score shows no MIDI clip".into()))?;
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .map(str::to_ascii_lowercase);
        let bytes = match ext.as_deref() {
            Some("pdf") => faderframe_leadsheet::pdf::write(&doc.pages, &doc.sheet.title),
            Some("musicxml" | "xml") => {
                faderframe_leadsheet::musicxml::write(&doc.sheet, &crate::leadsheet::today())
                    .into_bytes()
            }
            _ => {
                return Err(SessionError::Other(
                    "a score is written as .pdf or .musicxml".into(),
                ));
            }
        };
        crate::exports::make_parent(path)?;
        let shown = path.to_path_buf();
        self.write_export("Score", vec![(path.to_path_buf(), bytes)], move |s, _| {
            s.notify(
                NoticeLevel::Info,
                format!("Score written to {}", shown.display()),
            );
        })?;
        Ok(path.to_path_buf())
    }

    /// The score's keys typed out (`score-enter:` scripts, see
    /// [`parse_keys`]), one action each.
    pub fn score_keys(&mut self, text: &str) -> Result<()> {
        for k in parse_keys(text) {
            self.dispatch(Action::Score(ScoreOp::Key(k)))?;
        }
        Ok(())
    }
}

/// How much louder an accent and a marcato play (velocity steps).
const ACCENT: i32 = 20;
const MARCATO: i32 = 30;

/// A dynamic's velocity with the note's accents on it.
fn accented(v: u8, marks: &faderframe_project::notation::NoteMarks) -> u8 {
    let by = if marks.marcato {
        MARCATO
    } else if marks.accent {
        ACCENT
    } else {
        0
    };
    (i32::from(v) + by).clamp(1, 127) as u8
}

/// Keys typed as text (`score-enter:`): letters `a`–`g` enter notes
/// (upper case: added to the chord), `1`/`2`/`4`/`8`/`16` (whole …
/// sixteenth) choose a value, `.` a dot, `t`
/// triplets, `r` a rest, `~` a tie, `^`/`v` a semitone up/down, `'`/`,`
/// an octave, `j` respell, `x` delete, `v1`/`v2` voices, `>`/`<` next and
/// previous, `n` note input, `esc` leave it. Tokens are separated by
/// spaces; a token of letters enters each (`cdef`).
pub fn parse_keys(text: &str) -> Vec<ScoreKey> {
    let mut out = Vec::new();
    for token in text.split_whitespace() {
        let k = match token {
            "1" => Some(ScoreKey::Value(Value::Whole)),
            "2" => Some(ScoreKey::Value(Value::Half)),
            "4" => Some(ScoreKey::Value(Value::Quarter)),
            "8" => Some(ScoreKey::Value(Value::Eighth)),
            "16" => Some(ScoreKey::Value(Value::Sixteenth)),
            "." => Some(ScoreKey::Dot),
            "t" => Some(ScoreKey::Triplet),
            "r" => Some(ScoreKey::Rest),
            "~" => Some(ScoreKey::Tie),
            "^" => Some(ScoreKey::Transpose(Interval::Semitones(1))),
            "v" => Some(ScoreKey::Transpose(Interval::Semitones(-1))),
            "'" => Some(ScoreKey::Transpose(Interval::Octaves(1))),
            "," => Some(ScoreKey::Transpose(Interval::Octaves(-1))),
            "j" => Some(ScoreKey::Respell),
            "x" => Some(ScoreKey::Delete),
            "v1" => Some(ScoreKey::Voice(1)),
            "v2" => Some(ScoreKey::Voice(2)),
            ">" => Some(ScoreKey::Next),
            "<" => Some(ScoreKey::Previous),
            "n" => Some(ScoreKey::ToggleEntering),
            "esc" => Some(ScoreKey::Escape),
            _ => None,
        };
        match k {
            Some(k) => out.push(k),
            None => {
                for c in token.chars() {
                    if let Some(step) = "cdefgab".find(c.to_ascii_lowercase()) {
                        out.push(ScoreKey::Letter {
                            step: step as u8,
                            chord: c.is_ascii_uppercase(),
                        });
                    }
                }
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn typed_keys_read_as_score_keys() {
        assert_eq!(
            parse_keys("8 cde . F r ~ ^ ' v2 x"),
            vec![
                ScoreKey::Value(Value::Eighth),
                ScoreKey::Letter {
                    step: 0,
                    chord: false
                },
                ScoreKey::Letter {
                    step: 1,
                    chord: false
                },
                ScoreKey::Letter {
                    step: 2,
                    chord: false
                },
                ScoreKey::Dot,
                ScoreKey::Letter {
                    step: 3,
                    chord: true
                },
                ScoreKey::Rest,
                ScoreKey::Tie,
                ScoreKey::Transpose(Interval::Semitones(1)),
                ScoreKey::Transpose(Interval::Octaves(1)),
                ScoreKey::Voice(2),
                ScoreKey::Delete,
            ]
        );
    }

    #[test]
    fn letters_go_to_the_nearest_place_on_the_staff() {
        let c = KeySig::C;
        // E near G4 is E4; C near G4 is C5 (a fourth up beats a fifth
        // down); above B4 the next C is C5, above C5 the next E is E5.
        assert_eq!(letter_near(2, 67, c, false), 64);
        assert_eq!(letter_near(0, 67, c, false), 72);
        assert_eq!(letter_near(0, 71, c, true), 72);
        assert_eq!(letter_near(2, 72, c, true), 76);
        // F♯ in G, nearest B4: three steps down (F♯4).
        let g = faderframe_leadsheet::build::key_signature(faderframe_midi::theory::Key::new(
            7,
            faderframe_midi::theory::Scale::Major,
        ));
        assert_eq!(letter_key(3, 4, g), 66);
        assert_eq!(letter_near(3, 71, g, false), 66);
        assert_eq!(
            length_of(Duration::plain(Value::Eighth)),
            MusicalTime::from_quarters(0.5)
        );
    }
}
