//! Comping by lyric in the session: every take of a take folder heard by
//! the speech model, the takes' words lined up, the comp chosen word by
//! word, and a suggestion per word of the take that sings it best.
//!
//! **Hearing** ([`LyricCompOp::Hear`]) runs in a thread, one request at a
//! time (the model takes every core): each source the folder's takes use
//! is read as mono, the parts of it not heard yet go to the
//! [`Transcriber`] at 16 kHz (Whisper with word timings unless a test
//! gave another), the words' edges move to where the voice is
//! ([`sung::refine`]) and every word is measured ([`sung::measure`] with
//! the part's pitch track). The words go into the project by source
//! (`Command::SetSourceWords`, one "Hear Words" step); the measurements
//! stay in the session (a project opened again is measured again, from
//! the tick, for folders whose lanes are open).
//!
//! **Comping** ([`LyricCompOp::Comp`], `CompLine`, `UseBest`) plans the
//! boundaries with [`lyric_comp::plan_comp`] — the gaps between the words
//! on either side — and places each at the quietest point of its gap in
//! both takes heard there, then on the incoming take's nearest zero
//! crossing ([`sung::quiet_point`], [`sung::zero_crossing`]); the folder
//! changes through `TakeFolder::set_comp` in one `SetClipContent` step,
//! so its invariants hold and the folder's crossfades play at the new
//! boundaries.
//!
//! **Suggestions** ([`LyricComp::best`]) judge each word's takes against
//! each other ([`sung::judge`]: tuning and steadiness from the pitch
//! track, the start's distance from the sixteenth-note grid, level, noise,
//! clipping and plosive pops); nothing is applied until the user picks
//! it (`UseBest`).
//!
//! The pitch editor shows the syllable under each note
//! ([`Session::pitch_syllables`]): a clip's source words, cut into
//! syllables ([`faderframe_leadsheet::build::syllables_in`]) and dealt
//! out over the notes inside each word.

use crate::pitch::mono_of;
use crate::{NoticeLevel, Result, Session, SessionError};
use faderframe_analysis::melody;
use faderframe_analysis::sung::{self, Judged, WordSound};
use faderframe_core::{AudioSourceId, ClipId};
use faderframe_engine::Source;
use faderframe_leadsheet::build::{Language, syllables_in};
use faderframe_project::lyric_comp::{self, Edge, LyricAlignment, SourceWords, SungWord};
use faderframe_project::{ClipContent, Command, TakeFolder};
use faderframe_speech::Segment;
use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::hash::{Hash, Hasher};
use std::ops::Range;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::thread::JoinHandle;

/// Turns speech into timed words.
pub trait Transcriber: Send + Sync {
    /// Can it listen now (`Err`: why not)?
    fn ready(&self) -> std::result::Result<(), String>;
    /// The segments (lines) heard in each part (16 kHz mono), with their
    /// words; seconds from each part's start. `progress` hears 0…1.
    fn transcribe(
        &self,
        parts: &[Vec<f32>],
        progress: &dyn Fn(f32),
    ) -> std::result::Result<Vec<Vec<Segment>>, String>;
}

/// Whisper from a checkpoint folder (the downloaded one by default:
/// [`crate::speech::model_dir`]).
pub struct WhisperTranscriber {
    dir: PathBuf,
}

impl WhisperTranscriber {
    pub fn new(dir: PathBuf) -> Self {
        Self { dir }
    }

    /// The checkpoint in the data folder.
    pub fn installed() -> Self {
        Self::new(crate::speech::model_dir())
    }
}

impl Transcriber for WhisperTranscriber {
    fn ready(&self) -> std::result::Result<(), String> {
        if faderframe_speech::files::FILES
            .iter()
            .all(|f| self.dir.join(f).is_file())
        {
            Ok(())
        } else {
            Err(format!(
                "the speech model is not on this computer yet: Audio → Download Speech Model ({} MB)",
                crate::speech::CHECKPOINT_MB
            ))
        }
    }

    fn transcribe(
        &self,
        parts: &[Vec<f32>],
        progress: &dyn Fn(f32),
    ) -> std::result::Result<Vec<Vec<Segment>>, String> {
        let w = faderframe_speech::Whisper::load(&self.dir).map_err(|e| e.to_string())?;
        let options = faderframe_speech::Options {
            word_timestamps: true,
            ..Default::default()
        };
        let n = parts.len().max(1) as f32;
        Ok(parts
            .iter()
            .enumerate()
            .map(|(i, part)| w.transcribe(part, &options, |f| progress((i as f32 + f) / n)))
            .collect())
    }
}

/// A comp-by-lyric request.
#[derive(Clone, Debug, PartialEq)]
pub enum LyricCompOp {
    /// Hear the words of a take folder's takes (or of an audio clip's part
    /// of its source): only what was not heard yet.
    Hear { clip: ClipId },
    /// Hear them again (what was heard is replaced).
    HearAgain { clip: ClipId },
    /// Play `take` for the words (columns) `from..=to`.
    Comp {
        clip: ClipId,
        from: usize,
        to: usize,
        take: usize,
    },
    /// Play `take` for the line holding word `word`.
    CompLine {
        clip: ClipId,
        word: usize,
        take: usize,
    },
    /// The suggested take for every word of `words` (`None`: all), in one
    /// step.
    UseBest {
        clip: ClipId,
        words: Option<(usize, usize)>,
    },
    /// Forget the words heard in the clip's sources.
    Forget { clip: ClipId },
}

/// A folder's words across its takes, judged.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct LyricComp {
    pub alignment: LyricAlignment,
    /// Per word (column), each take's judgement (`None`: not sung there or
    /// not measured yet).
    pub judged: Vec<Vec<Option<Judged>>>,
    /// Per word, the take suggested.
    pub best: Vec<Option<usize>>,
}

impl LyricComp {
    /// Why `take` was judged as it was for word `c` (its strongest
    /// reasons, as text).
    pub fn reasons(&self, c: usize, take: usize, most: usize) -> Vec<String> {
        self.judged
            .get(c)
            .and_then(|j| j.get(take))
            .and_then(Option::as_ref)
            .map(|j| j.reasons.iter().take(most).map(|r| r.to_string()).collect())
            .unwrap_or_default()
    }
}

/// A syllable under a pitch-editor note.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NoteSyllable {
    pub text: String,
    /// The word goes on in the next syllable (shown with a hyphen).
    pub hyphen: bool,
    /// The note holds the syllable before it (a melisma).
    pub held: bool,
}

/// What a hearing job does for one source.
struct SourceTask {
    source: AudioSourceId,
    data: Source,
    words: SourceWords,
    /// Source spans to hear (project-rate frames).
    hear: Vec<(i64, i64)>,
    /// Source spans whose words to measure.
    measure: Vec<(i64, i64)>,
    /// Words already measured (start, end).
    measured: HashSet<(i64, i64)>,
}

/// What a job found for one source.
struct SourceFound {
    source: AudioSourceId,
    /// The source's words now (`None`: nothing was heard).
    words: Option<SourceWords>,
    sounds: Vec<((i64, i64), WordSound)>,
}

type JobResult = std::result::Result<Vec<SourceFound>, String>;

struct Job {
    clip: ClipId,
    serial: u64,
    handle: JoinHandle<JobResult>,
    progress: Arc<AtomicU32>,
}

/// Comping by lyric: the job running, those waiting, the words' sounds.
#[derive(Default)]
pub(crate) struct LyricCompState {
    transcriber: Option<Arc<dyn Transcriber>>,
    job: Option<Job>,
    /// Clips waiting (and whether they are heard or only measured).
    waiting: VecDeque<(ClipId, bool)>,
    /// What each source's words sound like, by (start, end).
    sounds: HashMap<AudioSourceId, HashMap<(i64, i64), WordSound>>,
    sounds_revision: u64,
    /// Sources measured from the tick already (once each).
    measured_once: HashSet<AudioSourceId>,
    /// The tick's last look for folders to measure (the session revision).
    looked: u64,
    /// The last lyric comp of each folder and what it was made from.
    memo: RefCell<HashMap<ClipId, (u64, Arc<LyricComp>)>>,
    /// The project the sounds are of (`Session::project_serial`): another
    /// project's sources may have the same ids.
    serial: u64,
}

/// The frames (project rate) of the sources a clip plays: per source the
/// spans (merged).
fn source_spans(content: &ClipContent) -> BTreeMap<AudioSourceId, Vec<(i64, i64)>> {
    let mut out: BTreeMap<AudioSourceId, Vec<(i64, i64)>> = BTreeMap::new();
    match content {
        ClipContent::Takes(f) => {
            for t in &f.takes {
                if t.end > t.start {
                    out.entry(t.source)
                        .or_default()
                        .push((t.source_offset + t.start, t.source_offset + t.end));
                }
            }
        }
        ClipContent::Audio(a) => {
            out.entry(a.source)
                .or_default()
                .push((a.source_offset, a.source_offset + a.source_span()));
        }
        _ => {}
    }
    for spans in out.values_mut() {
        spans.sort();
        let mut merged: Vec<(i64, i64)> = Vec::new();
        for &(a, b) in spans.iter() {
            let a = a.max(0);
            match merged.last_mut() {
                Some(l) if a <= l.1 => l.1 = l.1.max(b),
                _ if b > a => merged.push((a, b)),
                _ => {}
            }
        }
        *spans = merged;
    }
    out
}

/// The words of a part's segments as source words: `from` is the part's
/// first source frame (project rate); `k` file samples per source frame;
/// the edges refined on `mono` (the file's mono mix at `rate`).
pub(crate) fn sung_words(
    segments: &[Segment],
    from: i64,
    k: f64,
    mono: &[f32],
    rate: f64,
) -> Vec<SungWord> {
    let base = (from as f64 * k).round() as usize;
    let mut edges: Vec<(usize, usize)> = Vec::new();
    let mut words: Vec<SungWord> = Vec::new();
    for s in segments {
        for (i, w) in s.words.iter().enumerate() {
            let a = base + (w.start.max(0.0) * rate).round() as usize;
            let b = (base + (w.end.max(w.start) * rate).round() as usize).max(a + 1);
            edges.push((a.min(mono.len()), b.min(mono.len())));
            words.push(SungWord {
                start: 0,
                end: 0,
                text: w.text.trim().to_string(),
                line: i == 0,
                confidence: (w.probability.clamp(0.0, 1.0) * 100.0).round() as u8,
            });
        }
    }
    sung::refine(mono, rate, &mut edges);
    for (w, (a, b)) in words.iter_mut().zip(edges) {
        w.start = (a as f64 / k).round() as i64;
        w.end = ((b as f64 / k).round() as i64).max(w.start + 1);
    }
    words.retain(|w| !w.text.is_empty());
    words
}

/// Hear and measure one source (a job's thread).
fn work_source(
    task: SourceTask,
    project_rate: f64,
    transcriber: &dyn Transcriber,
    progress: &dyn Fn(f32),
) -> std::result::Result<SourceFound, String> {
    let (mono, rate) = mono_of(&task.data).ok_or("a take's audio could not be read")?;
    let k = rate / project_rate;
    let at = |f: i64| ((f.max(0) as f64 * k).round() as usize).min(mono.len());
    let mut words = task.words;
    let mut heard_any = false;
    if !task.hear.is_empty() {
        let parts: Vec<Vec<f32>> = task
            .hear
            .iter()
            .map(|&(a, b)| {
                crate::to_midi::resample(
                    &mono[at(a)..at(b).max(at(a))],
                    rate,
                    faderframe_speech::mel::RATE as f64,
                )
                .ok_or_else(|| "a take's audio could not be resampled".to_string())
            })
            .collect::<std::result::Result<_, _>>()?;
        let heard = transcriber.transcribe(&parts, progress)?;
        for (&(a, b), segments) in task.hear.iter().zip(&heard) {
            let found = sung_words(segments, a, k, &mono, rate);
            words.add((a, b), found);
            heard_any = true;
        }
        words.rate = project_rate.round() as u32;
    }
    // Every word in the spans measured once, with its part's pitch track.
    let mut sounds = Vec::new();
    let pad = (project_rate * 1.0) as i64;
    for &(a, b) in &task.measure {
        let todo: Vec<&SungWord> = words
            .words
            .iter()
            .filter(|w| w.start < b && w.end > a && !task.measured.contains(&(w.start, w.end)))
            .collect();
        if todo.is_empty() {
            continue;
        }
        let (ra, rb) = (at(a - pad), at(b + pad));
        let region = &mono[ra..rb.max(ra)];
        let track = melody::track(region, rate);
        for w in todo {
            let (ws, we) = (at(w.start).saturating_sub(ra), at(w.end).saturating_sub(ra));
            let around = ws.saturating_sub(rate as usize)..(we + rate as usize).min(region.len());
            let s = sung::measure(region, rate, ws..we.max(ws + 1), around, Some(&track));
            sounds.push(((w.start, w.end), s));
        }
    }
    Ok(SourceFound {
        source: task.source,
        words: heard_any.then_some(words),
        sounds,
    })
}

/// Deal a word's syllables out over its notes (see the module).
fn deal(word: &str, notes: usize, lang: Language) -> Vec<NoteSyllable> {
    if notes == 0 {
        return Vec::new();
    }
    let mut syl = syllables_in(word, lang);
    if syl.is_empty() {
        syl.push(word.to_string());
    }
    let mut out = Vec::with_capacity(notes);
    if notes >= syl.len() {
        let n = syl.len();
        for (i, s) in syl.into_iter().enumerate() {
            out.push(NoteSyllable {
                text: s,
                hyphen: i + 1 < n,
                held: false,
            });
        }
        while out.len() < notes {
            out.push(NoteSyllable {
                text: String::new(),
                hyphen: false,
                held: true,
            });
        }
    } else {
        let rest = syl.split_off(notes - 1).concat();
        for s in syl {
            out.push(NoteSyllable {
                text: s,
                hyphen: true,
                held: false,
            });
        }
        out.push(NoteSyllable {
            text: rest,
            hyphen: false,
            held: false,
        });
    }
    out
}

/// The syllable under each note (`notes`: source frames, by start) from
/// the words heard there (same frames): a note belongs to the word its
/// start is in (from `reach` frames before the word).
pub fn syllables_for_notes(
    notes: &[(i64, i64)],
    words: &[SungWord],
    reach: i64,
) -> Vec<Option<NoteSyllable>> {
    let lang = Language::of(words.iter().map(|w| w.text.as_str()));
    let mut out = vec![None; notes.len()];
    let mut taken = vec![false; notes.len()];
    for (wi, w) in words.iter().enumerate() {
        // Up to where the next word starts (its reach included).
        let until = words
            .get(wi + 1)
            .map_or(w.end, |n| w.end.min(n.start - reach).max(w.start + 1));
        let mine: Vec<usize> = (0..notes.len())
            .filter(|&i| !taken[i] && notes[i].0 >= w.start - reach && notes[i].0 < until)
            .collect();
        for (i, s) in mine.iter().zip(deal(&w.text, mine.len(), lang)) {
            taken[*i] = true;
            out[*i] = Some(s);
        }
    }
    out
}

impl Session {
    /// Use another transcriber (tests: a stand-in for the model).
    pub fn set_transcriber(&mut self, transcriber: Arc<dyn Transcriber>) {
        self.lyric_comp.transcriber = Some(transcriber);
    }

    fn transcriber(&self) -> Arc<dyn Transcriber> {
        self.lyric_comp
            .transcriber
            .clone()
            .unwrap_or_else(|| Arc::new(WhisperTranscriber::installed()))
    }

    pub(crate) fn lyric_comp_op(&mut self, op: LyricCompOp) -> Result<()> {
        match op {
            LyricCompOp::Hear { clip } => self.hear_words(clip, false),
            LyricCompOp::HearAgain { clip } => self.hear_words(clip, true),
            LyricCompOp::Comp {
                clip,
                from,
                to,
                take,
            } => self.comp_words(clip, from.min(to)..from.max(to) + 1, take),
            LyricCompOp::CompLine { clip, word, take } => {
                let comp = self.lyric_comp(clip).ok_or_else(no_words)?;
                let line = comp.alignment.line(word);
                self.comp_words(clip, line, take)
            }
            LyricCompOp::UseBest { clip, words } => self.use_best(clip, words),
            LyricCompOp::Forget { clip } => self.forget_words(clip),
        }
    }

    /// Hear (or `again`: hear anew) the words of a clip's sources.
    fn hear_words(&mut self, clip: ClipId, again: bool) -> Result<()> {
        self.lyric_project_changed();
        self.transcriber().ready().map_err(SessionError::Other)?;
        let c = self
            .project
            .clip(clip)
            .ok_or_else(|| SessionError::Other(format!("no clip {clip}")))?;
        if source_spans(&c.content).is_empty() {
            return Err(SessionError::Other(
                "only audio clips and take folders have words to hear".into(),
            ));
        }
        if again {
            self.lyric_comp.memo.borrow_mut().remove(&clip);
        }
        if self.lyric_comp.job.as_ref().is_some_and(|j| j.clip == clip)
            || self.lyric_comp.waiting.iter().any(|w| w.0 == clip)
        {
            return Ok(());
        }
        if again {
            // Hearing again replaces what was heard: forget it first so
            // the job hears everything.
            let spans = source_spans(&c.content);
            let mut commands = Vec::new();
            for (source, spans) in spans {
                if let Some(w) = self.project.source_words.get(&source) {
                    let mut w = w.clone();
                    w.words
                        .retain(|x| !spans.iter().any(|&(a, b)| x.start < b && x.end > a));
                    w.heard = without(&w.heard, &spans);
                    commands.push(Command::SetSourceWords {
                        source,
                        words: (!w.heard.is_empty()).then(|| Box::new(w)),
                    });
                }
                self.lyric_comp.sounds.remove(&source);
            }
            self.batch("Forget Words", commands)?;
        }
        self.lyric_comp.waiting.push_back((clip, true));
        self.start_lyric_job();
        Ok(())
    }

    /// Another project took this one's place: forget the old one's sounds
    /// and requests (its sources' ids may come again).
    fn lyric_project_changed(&mut self) {
        if self.lyric_comp.serial == self.project_serial {
            return;
        }
        let st = &mut self.lyric_comp;
        st.serial = self.project_serial;
        st.sounds.clear();
        st.measured_once.clear();
        st.waiting.clear();
        st.memo.borrow_mut().clear();
        st.sounds_revision += 1;
    }

    /// Start the next waiting request (none runs).
    fn start_lyric_job(&mut self) {
        while self.lyric_comp.job.is_none() {
            let Some((clip, hear)) = self.lyric_comp.waiting.pop_front() else {
                return;
            };
            let Some(c) = self.project.clip(clip) else {
                continue;
            };
            let name = c.name.clone();
            let rate = self.project.sample_rate;
            let mut tasks = Vec::new();
            for (source, spans) in source_spans(&c.content) {
                let Some(data) = self.sources.get(&source).cloned() else {
                    continue;
                };
                let words = self
                    .project
                    .source_words
                    .get(&source)
                    .cloned()
                    .unwrap_or_else(|| SourceWords::new(rate));
                let hear: Vec<(i64, i64)> = if hear {
                    spans
                        .iter()
                        .flat_map(|&(a, b)| words.missing(a, b))
                        .collect()
                } else {
                    Vec::new()
                };
                let measured: HashSet<(i64, i64)> = self
                    .lyric_comp
                    .sounds
                    .get(&source)
                    .map(|m| m.keys().copied().collect())
                    .unwrap_or_default();
                tasks.push(SourceTask {
                    source,
                    data,
                    words,
                    hear,
                    measure: spans,
                    measured,
                });
            }
            if tasks.is_empty() {
                continue;
            }
            let hearing = tasks.iter().any(|t| !t.hear.is_empty());
            let transcriber = self.transcriber();
            let progress = Arc::new(AtomicU32::new(0));
            let p = Arc::clone(&progress);
            let project_rate = f64::from(rate);
            let spawned = std::thread::Builder::new()
                .name("faderframe-lyric-comp".into())
                .spawn(move || {
                    let n = tasks.len() as f32;
                    let mut out = Vec::with_capacity(tasks.len());
                    for (i, task) in tasks.into_iter().enumerate() {
                        let report = |f: f32| {
                            p.store(((i as f32 + f) / n * 1000.0) as u32, Ordering::Relaxed);
                        };
                        out.push(work_source(
                            task,
                            project_rate,
                            transcriber.as_ref(),
                            &report,
                        )?);
                    }
                    Ok(out)
                });
            match spawned {
                Ok(handle) => {
                    if hearing {
                        self.notify(
                            NoticeLevel::Info,
                            format!("Listening to the words in ‘{name}’…"),
                        );
                    }
                    self.lyric_comp.job = Some(Job {
                        clip,
                        serial: self.project_serial,
                        handle,
                        progress,
                    });
                    self.revision += 1;
                }
                Err(e) => {
                    self.notify(NoticeLevel::Error, format!("comp by lyric: {e}"));
                }
            }
        }
    }

    /// Finish a hearing job, start the next, and measure folders opened
    /// whose words have no measurements (from the session tick).
    pub(crate) fn poll_lyric_comp(&mut self) {
        self.lyric_project_changed();
        if self
            .lyric_comp
            .job
            .as_ref()
            .is_some_and(|j| j.handle.is_finished())
            && let Some(job) = self.lyric_comp.job.take()
        {
            let result = job
                .handle
                .join()
                .unwrap_or_else(|_| Err("hearing the words failed".into()));
            if job.serial == self.project_serial {
                match result {
                    Ok(found) => self.place_words(job.clip, found),
                    Err(e) => self.notify(NoticeLevel::Error, format!("comp by lyric: {e}")),
                }
            }
            self.revision += 1;
        }
        self.start_lyric_job();
        if self.lyric_comp.job.is_none() && self.lyric_comp.looked != self.revision {
            self.lyric_comp.looked = self.revision;
            let unmeasured = self.open_takes.iter().copied().find(|&clip| {
                self.project.clip(clip).is_some_and(|c| {
                    source_spans(&c.content).keys().any(|s| {
                        self.project.source_words.contains_key(s)
                            && !self.lyric_comp.sounds.contains_key(s)
                            && !self.lyric_comp.measured_once.contains(s)
                    })
                })
            });
            if let Some(clip) = unmeasured {
                if let Some(c) = self.project.clip(clip) {
                    self.lyric_comp
                        .measured_once
                        .extend(source_spans(&c.content).into_keys());
                }
                self.lyric_comp.waiting.push_back((clip, false));
                self.start_lyric_job();
            }
        }
    }

    /// Put a job's words into the project (one step) and keep its sounds.
    fn place_words(&mut self, clip: ClipId, found: Vec<SourceFound>) {
        let mut commands = Vec::new();
        let mut heard = 0;
        for f in found {
            let sounds = self.lyric_comp.sounds.entry(f.source).or_default();
            sounds.extend(f.sounds);
            self.lyric_comp.measured_once.insert(f.source);
            if let Some(w) = f.words {
                heard += 1;
                if self.project.source_words.get(&f.source) != Some(&w) {
                    commands.push(Command::SetSourceWords {
                        source: f.source,
                        words: Some(Box::new(w)),
                    });
                }
            }
        }
        self.lyric_comp.sounds_revision += 1;
        if let Err(e) = self.batch("Hear Words", commands) {
            self.notify(NoticeLevel::Error, format!("comp by lyric: {e}"));
            return;
        }
        if heard > 0 {
            let words = self
                .lyric_comp(clip)
                .map_or(0, |c| c.alignment.columns.len());
            let name = self
                .project
                .clip(clip)
                .map(|c| c.name.clone())
                .unwrap_or_default();
            self.notify(
                NoticeLevel::Info,
                if words == 0 {
                    format!("No words heard in ‘{name}’")
                } else {
                    format!(
                        "‘{name}’: {words} word{} — click a word in a take to use it there",
                        if words == 1 { "" } else { "s" }
                    )
                },
            );
        }
    }

    /// Is a clip's audio being heard or measured (its progress, 0…1)?
    pub fn hearing_words(&self, clip: ClipId) -> Option<f32> {
        if let Some(j) = self.lyric_comp.job.as_ref().filter(|j| j.clip == clip) {
            return Some(j.progress.load(Ordering::Relaxed) as f32 / 1000.0);
        }
        self.lyric_comp
            .waiting
            .iter()
            .any(|w| w.0 == clip)
            .then_some(0.0)
    }

    /// Anything being heard or measured?
    pub fn hearing_any_words(&self) -> bool {
        self.lyric_comp.job.is_some() || !self.lyric_comp.waiting.is_empty()
    }

    /// Wait until every hearing job is done (tests, scripts).
    pub fn wait_for_words(&mut self) {
        let start = std::time::Instant::now();
        while self.hearing_any_words() && start.elapsed().as_secs() < 600 {
            self.poll_lyric_comp();
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    }

    /// A take folder's words across its takes, judged (`None`: not a
    /// folder, or nothing heard in it yet).
    pub fn lyric_comp(&self, clip: ClipId) -> Option<Arc<LyricComp>> {
        let p = &self.project;
        let c = p.clip(clip)?;
        let f = c.as_takes()?;
        let key = self.lyric_key(clip);
        if let Some((k, comp)) = self.lyric_comp.memo.borrow().get(&clip)
            && *k == key
        {
            return Some(Arc::clone(comp));
        }
        let rate = p.sample_rate;
        let words = lyric_comp::take_words(f, &p.source_words, rate);
        if words.iter().all(Vec::is_empty) {
            return None;
        }
        let alignment = lyric_comp::align(words, rate);
        // Each take's word, measured; its start against the grid.
        let srate = f64::from(rate.max(1));
        let base = p.timeline.to_samples(c.start, srate);
        let step = faderframe_timeline::TICKS_PER_QUARTER / 4;
        let timing = |folder_frame: i64| {
            let at = base + folder_frame;
            let t = p.timeline.to_musical(at.max(0), srate);
            let grid =
                faderframe_timeline::MusicalTime((t.0 as f64 / step as f64).round() as i64 * step);
            (at - p.timeline.to_samples(grid, srate)) as f64 * 1000.0 / srate
        };
        let mut judged = Vec::with_capacity(alignment.columns.len());
        let mut best = Vec::with_capacity(alignment.columns.len());
        for (ci, col) in alignment.columns.iter().enumerate() {
            let mut sounds = Vec::with_capacity(col.takes.len());
            let mut times = Vec::with_capacity(col.takes.len());
            for t in 0..col.takes.len() {
                let w = alignment.word(ci, t);
                let take = f.takes.get(t);
                let fresh = self.lyric_comp.serial == self.project_serial;
                let sound = w.zip(take).filter(|_| fresh).and_then(|(w, take)| {
                    let src = p.source_words.get(&take.source)?;
                    let sw = src.words.get(w.index)?;
                    let mut s = *self
                        .lyric_comp
                        .sounds
                        .get(&take.source)?
                        .get(&(sw.start, sw.end))?;
                    // The take's gain counts in its level.
                    s.level_db += take.gain_db;
                    s.floor_db += take.gain_db;
                    Some(s)
                });
                times.push(sound.and(w.map(|w| timing(w.start))));
                sounds.push(sound);
            }
            let (j, b) = sung::judge(&sounds, &times);
            judged.push(j);
            best.push(b);
        }
        let comp = Arc::new(LyricComp {
            alignment,
            judged,
            best,
        });
        self.lyric_comp
            .memo
            .borrow_mut()
            .insert(clip, (key, Arc::clone(&comp)));
        Some(comp)
    }

    /// What a folder's lyric comp is made from (its memo's key).
    fn lyric_key(&self, clip: ClipId) -> u64 {
        let mut h = std::hash::DefaultHasher::new();
        let p = &self.project;
        if let Some(c) = p.clip(clip) {
            c.start.0.hash(&mut h);
            if let Some(f) = c.as_takes() {
                f.length.hash(&mut h);
                for t in &f.takes {
                    (t.source.raw(), t.source_offset, t.start, t.end).hash(&mut h);
                    t.gain_db.to_bits().hash(&mut h);
                    if let Some(w) = p.source_words.get(&t.source) {
                        w.rate.hash(&mut h);
                        for x in &w.words {
                            (x.start, x.end, &x.text, x.line).hash(&mut h);
                        }
                    }
                }
            }
        }
        p.sample_rate.hash(&mut h);
        self.project_serial.hash(&mut h);
        p.timeline.tempo.fingerprint().hash(&mut h);
        self.lyric_comp.sounds_revision.hash(&mut h);
        h.finish()
    }

    /// Play `take` for the words `columns` of a folder.
    fn comp_words(&mut self, clip: ClipId, columns: Range<usize>, take: usize) -> Result<()> {
        let comp = self.lyric_comp(clip).ok_or_else(no_words)?;
        let c = self.project.clip(clip).cloned().ok_or_else(no_words)?;
        let f = c.as_takes().ok_or_else(no_words)?;
        let mut g = f.clone();
        if !self.comp_into(&mut g, &comp.alignment, columns.clone(), take) {
            return Err(SessionError::Other(format!(
                "take {} does not sing {}",
                take + 1,
                if columns.len() == 1 {
                    "that word"
                } else {
                    "those words"
                }
            )));
        }
        if g == *f {
            return Ok(());
        }
        let words: Vec<&str> = columns
            .clone()
            .filter_map(|i| comp.alignment.columns.get(i).map(|x| x.text.as_str()))
            .collect();
        let label = match words.as_slice() {
            [one] => format!("Comp “{one}” from Take {}", take + 1),
            _ => format!("Comp {} Words from Take {}", words.len(), take + 1),
        };
        self.batch(
            &label,
            vec![Command::SetClipContent {
                clip,
                start: c.start,
                content: Box::new(ClipContent::Takes(g)),
            }],
        )
    }

    /// Plan and apply a word comp on `folder` (its boundaries placed by
    /// the audio); false when the take sings none of the words.
    fn comp_into(
        &self,
        folder: &mut TakeFolder,
        al: &LyricAlignment,
        columns: Range<usize>,
        take: usize,
    ) -> bool {
        let rate = self.project.sample_rate;
        let Some(plan) = lyric_comp::plan_comp(al, folder, columns, take, rate) else {
            return false;
        };
        let start = plan.start.map(|e| self.place_edge(folder, take, &e, true));
        let end = plan.end.map(|e| self.place_edge(folder, take, &e, false));
        lyric_comp::apply_comp(folder, &plan, start, end);
        true
    }

    /// Where a boundary goes in its gap: an existing one; else the quiet
    /// point of both takes heard there (nearest the word), on the comped
    /// take's nearest zero crossing.
    fn place_edge(&self, f: &TakeFolder, take: usize, e: &Edge, before_word: bool) -> i64 {
        if let Some(x) = e.existing {
            return x;
        }
        if e.hi <= e.lo {
            return e.lo;
        }
        let rate = f64::from(self.project.sample_rate.max(1));
        let span = ((0.005 * rate) as usize).max(8);
        let pad = span / 2;
        let len = (e.hi - e.lo) as usize + 2 * pad;
        let window = |t: usize| -> Option<Vec<f32>> {
            let tk = f.takes.get(t)?;
            let frames =
                self.source_frames(tk.source, tk.source_offset + e.lo - pad as i64, len)?;
            let g = faderframe_core::gain::db_to_gain(tk.gain_db);
            Some(
                (0..len)
                    .map(|i| frames.iter().map(|ch| ch[i]).sum::<f32>() * g)
                    .collect(),
            )
        };
        let Some(own) = window(take) else {
            return e.plain();
        };
        let other = e.other.and_then(window);
        let mut signals: Vec<&[f32]> = vec![&own];
        if let Some(o) = &other {
            signals.push(o);
        }
        let prefer = if before_word { len - pad - 1 } else { pad };
        let q = sung::quiet_point(&signals, pad, span, prefer);
        let reach = (crate::editing::ZERO_CROSSING_REACH * rate) as usize;
        let z = sung::zero_crossing(
            &own,
            q,
            q.saturating_sub(reach).max(pad),
            (q + reach).min(len - pad),
        );
        (e.lo + z as i64 - pad as i64).clamp(e.lo, e.hi)
    }

    /// The suggested takes for `words` (all: `None`) in one step: runs of
    /// words with the same suggestion are comped together.
    fn use_best(&mut self, clip: ClipId, words: Option<(usize, usize)>) -> Result<()> {
        let comp = self.lyric_comp(clip).ok_or_else(no_words)?;
        let c = self.project.clip(clip).cloned().ok_or_else(no_words)?;
        let f = c.as_takes().ok_or_else(no_words)?;
        let n = comp.alignment.columns.len();
        let (a, b) = words.map_or((0, n), |(a, b)| (a.min(b), (a.max(b) + 1).min(n)));
        let mut g = f.clone();
        let mut i = a;
        let mut runs = 0;
        while i < b {
            let Some(take) = comp.best[i] else {
                i += 1;
                continue;
            };
            let mut j = i + 1;
            while j < b && comp.best[j] == Some(take) {
                j += 1;
            }
            if self.comp_into(&mut g, &comp.alignment, i..j, take) {
                runs += 1;
            }
            i = j;
        }
        if runs == 0 {
            return Err(SessionError::Other(
                "there are no suggestions yet (the takes are still being measured)".into(),
            ));
        }
        if g == *f {
            return Ok(());
        }
        self.batch(
            "Use the Suggested Takes",
            vec![Command::SetClipContent {
                clip,
                start: c.start,
                content: Box::new(ClipContent::Takes(g)),
            }],
        )
    }

    /// Forget the words heard in a clip's sources (one step).
    fn forget_words(&mut self, clip: ClipId) -> Result<()> {
        let c = self.project.clip(clip).ok_or_else(no_words)?;
        let commands: Vec<Command> = source_spans(&c.content)
            .into_keys()
            .filter(|s| self.project.source_words.contains_key(s))
            .map(|source| Command::SetSourceWords {
                source,
                words: None,
            })
            .collect();
        self.batch("Forget Words", commands)
    }

    /// The syllable under each note of an audio clip's pitch edit (empty
    /// without words heard in its source).
    pub fn pitch_syllables(&self, clip: ClipId) -> Vec<Option<NoteSyllable>> {
        let p = &self.project;
        let Some(a) = p.clip(clip).and_then(|c| c.as_audio()) else {
            return Vec::new();
        };
        let (Some(edit), Some(words)) = (a.pitch.as_ref(), p.source_words.get(&a.source)) else {
            return Vec::new();
        };
        let k = if words.rate == 0 || words.rate == p.sample_rate {
            1.0
        } else {
            f64::from(p.sample_rate) / f64::from(words.rate)
        };
        let scaled: Vec<SungWord> = words
            .words
            .iter()
            .map(|w| SungWord {
                start: (w.start as f64 * k).round() as i64,
                end: (w.end as f64 * k).round() as i64,
                ..w.clone()
            })
            .collect();
        let notes: Vec<(i64, i64)> = edit.notes.iter().map(|n| (n.start, n.end)).collect();
        let reach = i64::from(p.sample_rate) / 20;
        syllables_for_notes(&notes, &scaled, reach)
    }
}

/// `spans` (sorted, apart) without `cut` (sorted).
fn without(spans: &[(i64, i64)], cut: &[(i64, i64)]) -> Vec<(i64, i64)> {
    let mut out = spans.to_vec();
    for &(s, e) in cut {
        out = out
            .into_iter()
            .flat_map(|(x, y)| [(x, y.min(s)), (x.max(e), y)])
            .filter(|(p, q)| q > p)
            .collect();
    }
    out
}

fn no_words() -> SessionError {
    SessionError::Other("no words heard in that folder yet: Comp by Lyrics first".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn word(a: i64, b: i64, t: &str) -> SungWord {
        SungWord {
            start: a,
            end: b,
            text: t.into(),
            line: false,
            confidence: 90,
        }
    }

    #[test]
    fn syllables_are_dealt_over_the_notes_of_each_word() {
        let words = [
            word(0, 100, "Beautiful"),
            word(150, 300, "love"),
            word(400, 600, "me"),
        ];
        // Three notes in "Beautiful", two in "love" (a melisma), none in
        // "me" (spoken), one note before any word.
        let notes = [
            (-200, -150),
            (0, 30),
            (35, 60),
            (65, 100),
            (150, 220),
            (225, 300),
        ];
        let s = syllables_for_notes(&notes, &words, 10);
        assert_eq!(s[0], None);
        let texts: Vec<(String, bool, bool)> = s[1..]
            .iter()
            .map(|x| {
                let x = x.clone().unwrap();
                (x.text, x.hyphen, x.held)
            })
            .collect();
        assert_eq!(
            texts,
            [
                ("Beau".into(), true, false),
                ("ti".into(), true, false),
                ("ful".into(), false, false),
                ("love".into(), false, false),
                (String::new(), false, true),
            ]
        );
        // Fewer notes than syllables: the last note takes the rest.
        let s = syllables_for_notes(&[(0, 50), (50, 100)], &[word(0, 100, "Beautiful")], 10);
        assert_eq!(s[1].as_ref().unwrap().text, "tiful");
        assert!(s[0].as_ref().unwrap().hyphen);
    }
}
