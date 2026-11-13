//! Timing alignment of doubles: dub clips warped to follow a guide track
//! (a doubled vocal to the lead, backing vocals to the lead, an ADR line
//! to the production take).
//!
//! [`Action::AlignTiming`](crate::Action::AlignTiming) reads, in a thread,
//! the guide track's audio as it plays over each dub clip (its clips and
//! take folders' comps, through their warps) and the dub as it plays now,
//! both mixed to mono at [`RATE`]; [`faderframe_analysis::timing_align`]
//! finds where the dub sang each moment of the guide, and the map becomes
//! the dub's warp markers (composed with the warp it had, so a dub warped
//! before is aligned from where it plays). With
//! [`TimingAlignSettings::pitch`] above zero the dub's notes also move
//! toward the guide's: each note of its (melodic) pitch edit is shifted by
//! that share of the distance to the guide's median pitch where it now
//! plays — folded into an octave, and only when that is within
//! [`PITCH_REACH`] (a double sung out of tune; a harmony keeps its notes).
//! Every dub of a request is changed in one step, "Align Timing"; a dub
//! edited while its alignment ran is left as it is, with a notice.

use crate::pitch::{edit_from, mono_of};
use crate::{NoticeLevel, Result, Session, SessionError};
use faderframe_analysis::melody;
use faderframe_analysis::timing_align::{self, Settings, TimeMap};
use faderframe_core::{AudioSourceId, ClipId, TrackId};
use faderframe_engine::Source;
use faderframe_project::{AudioClip, ClipContent, Command, Warp, WarpAlgorithm, WarpMarker};
use std::collections::HashMap;
use std::thread::JoinHandle;

/// The analysis rate (Hz): the voice's bands up to 7 kHz.
pub const RATE: f64 = 16_000.0;
/// The map is kept within this of the analysis's (seconds) by the warp
/// markers it becomes.
const TOLERANCE: f64 = 0.0005;
/// A note moves toward the guide's only when they are this close
/// (semitones, folded into an octave).
pub const PITCH_REACH: f32 = 2.0;
/// The guide must sing at least this much of a note's span for its pitch
/// to count.
const VOICED_SHARE: f64 = 0.3;

/// How dubs are aligned (the editor's, for the menus).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TimingAlignSettings {
    /// 0 (loose: phrases) … 1 (tight: syllables).
    pub tightness: f32,
    /// How far the dub's notes move toward the guide's pitch (0: not at
    /// all, 1: onto it).
    pub pitch: f32,
    /// Sibilants keep their own length.
    pub protect_consonants: bool,
}

impl Default for TimingAlignSettings {
    fn default() -> Self {
        Self {
            tightness: 0.6,
            pitch: 0.0,
            protect_consonants: true,
        }
    }
}

impl TimingAlignSettings {
    /// The tightness presets the menus offer.
    pub const TIGHTNESS: [(&'static str, f32); 3] =
        [("Loose", 0.2), ("Medium", 0.6), ("Tight", 1.0)];
}

/// Part of the guide's audio: timeline samples `start..end` read from a
/// source.
#[derive(Clone)]
struct GuidePiece {
    start: i64,
    end: i64,
    source: AudioSourceId,
    reads: Reads,
    gain: f32,
}

/// How a piece finds its source frame (project rate) for a frame of it.
#[derive(Clone)]
enum Reads {
    /// An audio clip (through its warp; reversed or not) — a take folder's
    /// piece is its take's clip cut there.
    Clip(AudioClip),
}

impl GuidePiece {
    /// The source frame (project rate) played at timeline sample `t`.
    fn source_at(&self, t: f64) -> f64 {
        match &self.reads {
            Reads::Clip(a) => clip_source_at(a, t - self.start as f64),
        }
    }
}

/// The source frame a clip plays at clip-relative frame `out`.
fn clip_source_at(a: &AudioClip, out: f64) -> f64 {
    let s = a.source_at(out);
    if a.reversed {
        // Played from its end backwards.
        (2 * a.source_offset + a.source_span()) as f64 - 1.0 - s
    } else {
        s
    }
}

/// One dub to align.
struct Dub {
    clip: ClipId,
    name: String,
    /// Timeline sample of its start.
    start: i64,
    audio: AudioClip,
}

/// What became of a dub.
enum Outcome {
    Aligned {
        clip: ClipId,
        name: String,
        before: Box<AudioClip>,
        after: Box<AudioClip>,
        /// The furthest it moved (ms) and the notes moved toward the
        /// guide's pitch.
        moved_ms: f64,
        notes: usize,
    },
    Failed {
        name: String,
        why: String,
    },
}

/// A request under way.
struct Job {
    guide: String,
    handle: JoinHandle<Vec<Outcome>>,
}

/// Alignments under way.
#[derive(Default)]
pub(crate) struct TimingAlignState {
    jobs: Vec<Job>,
}

/// A source's mono mix, low-passed for [`RATE`], and its rate.
struct Heard {
    mono: Vec<f32>,
    rate: f64,
}

impl Heard {
    fn read(source: &Source) -> Option<Self> {
        let (mut mono, rate) = mono_of(source)?;
        if rate > RATE * 1.2 {
            lowpass(&mut mono, rate, 0.45 * RATE);
        }
        Some(Self { mono, rate })
    }

    /// The sample at project-rate source frame `f` (linear; silence
    /// outside).
    fn at(&self, f: f64, project_rate: f64) -> f32 {
        let x = f * self.rate / project_rate;
        if x < 0.0 {
            return 0.0;
        }
        let i = x.floor() as usize;
        let (Some(a), b) = (self.mono.get(i), self.mono.get(i + 1)) else {
            return 0.0;
        };
        let b = b.copied().unwrap_or(0.0);
        a + (b - a) * (x - i as f64) as f32
    }

    /// Project-rate frames of it.
    fn frames(&self, project_rate: f64) -> i64 {
        (self.mono.len() as f64 * project_rate / self.rate).floor() as i64
    }
}

/// A fourth-order Butterworth low-pass at `cutoff` in place (two biquads).
fn lowpass(x: &mut [f32], rate: f64, cutoff: f64) {
    let w = std::f64::consts::TAU * cutoff / rate;
    for q in [0.541_196_1, 1.306_563] {
        let (s, c) = w.sin_cos();
        let alpha = s / (2.0 * q);
        let a0 = 1.0 + alpha;
        let b0 = (1.0 - c) / 2.0 / a0;
        let b1 = (1.0 - c) / a0;
        let a1 = -2.0 * c / a0;
        let a2 = (1.0 - alpha) / a0;
        let (mut x1, mut x2, mut y1, mut y2) = (0.0f64, 0.0f64, 0.0f64, 0.0f64);
        for v in x.iter_mut() {
            let x0 = f64::from(*v);
            let y = b0 * x0 + b1 * x1 + b0 * x2 - a1 * y1 - a2 * y2;
            x2 = x1;
            x1 = x0;
            y2 = y1;
            y1 = y;
            *v = y as f32;
        }
    }
}

/// The settings the analysis takes.
fn analysis_settings(tightness: f32, protect: bool) -> Settings {
    Settings {
        tightness: f64::from(tightness.clamp(0.0, 1.0)),
        protect_consonants: protect,
        ..Settings::default()
    }
}

/// The dub's new warp from the map (and the source's length, project
/// frames): the anchors where the map puts the clip's edges, markers
/// where it bends.
fn warp_from(a: &AudioClip, map: &TimeMap, project_rate: f64, source_frames: i64) -> AudioClip {
    let len = a.length;
    let old = |dub_seconds: f64| a.source_at(dub_seconds * project_rate);
    let last = (source_frames - 1).max(1);
    let offset = (old(map.at(0.0)).round() as i64).clamp(0, last - 1);
    let end = (old(map.at(len as f64 / project_rate)).round() as i64).clamp(offset + 1, last + 1);
    let mut markers: Vec<WarpMarker> = map
        .points(TOLERANCE)
        .into_iter()
        .filter_map(|(g, d)| {
            let at = (g * project_rate).round() as i64;
            (at > 0 && at < len).then(|| WarpMarker {
                at,
                source: old(d).round() as i64,
            })
        })
        .collect();
    markers.dedup_by_key(|m| m.at);
    let mut warp = Warp {
        source_length: end - offset,
        markers,
        algorithm: a
            .warp
            .as_ref()
            .map_or(WarpAlgorithm::Polyphonic, |w| w.algorithm),
    };
    warp.normalize(offset, len);
    AudioClip {
        source_offset: offset,
        warp: Some(warp),
        ..a.clone()
    }
}

/// Move the notes of `after`'s pitch edit (made when it has none) toward
/// the guide's pitch where they now play; how many moved.
fn align_pitch(
    after: &mut AudioClip,
    dub: &Heard,
    guide: &melody::PitchTrack,
    amount: f32,
    project_rate: f64,
) -> std::result::Result<usize, String> {
    let mut edit = match after.pitch.take() {
        Some(e) if e.polyphonic.is_some() => {
            after.pitch = Some(e);
            return Err("its pitch edit is polyphonic (pitch left alone)".into());
        }
        Some(e) => e,
        None => {
            let track = melody::track(&dub.mono, dub.rate);
            edit_from(&track, dub.rate, project_rate, after)
        }
    };
    let warp = after
        .warp
        .clone()
        .unwrap_or_else(|| Warp::uniform(after.length));
    let frame = |source: i64| {
        let out = warp.output_of(after.source_offset, after.length, source) as f64;
        out / project_rate / melody::HOP_SECONDS
    };
    let mut moved = 0;
    for note in &mut edit.notes {
        let (a, b) = note.placed();
        let (ia, ib) = (frame(a).max(0.0) as usize, frame(b).max(0.0) as usize);
        let mut sung: Vec<f32> = guide
            .frames
            .get(ia..ib.min(guide.frames.len()))
            .unwrap_or_default()
            .iter()
            .filter(|f| f.voiced())
            .map(|f| f.note)
            .collect();
        if sung.is_empty() || (sung.len() as f64) < VOICED_SHARE * (ib - ia).max(1) as f64 {
            continue;
        }
        sung.sort_by(f32::total_cmp);
        let target = sung[sung.len() / 2];
        let mut by = target - note.pitch;
        by -= 12.0 * (by / 12.0).round();
        if by.abs() > PITCH_REACH {
            continue;
        }
        note.shift = (amount.clamp(0.0, 1.0) * by).clamp(
            -faderframe_project::pitch::MAX_SHIFT,
            faderframe_project::pitch::MAX_SHIFT,
        );
        moved += 1;
    }
    after.pitch = Some(edit);
    Ok(moved)
}

/// Align every dub (helper thread).
fn work(
    guide: Vec<GuidePiece>,
    dubs: Vec<Dub>,
    sources: HashMap<AudioSourceId, Source>,
    project_rate: f64,
    settings: TimingAlignSettings,
) -> Vec<Outcome> {
    let mut heard: HashMap<AudioSourceId, Option<Heard>> = HashMap::new();
    let mut hear = |id: AudioSourceId| {
        heard
            .entry(id)
            .or_insert_with(|| sources.get(&id).and_then(Heard::read));
    };
    for p in &guide {
        hear(p.source);
    }
    for d in &dubs {
        hear(d.audio.source);
    }
    let step = project_rate / RATE;
    dubs.into_iter()
        .map(|d| {
            let fail = |why: &str| Outcome::Failed {
                name: d.name.clone(),
                why: why.into(),
            };
            let Some(Some(dub_heard)) = heard.get(&d.audio.source) else {
                return fail("its audio could not be read");
            };
            let n = (d.audio.length as f64 / step).floor() as usize;
            // The guide as it plays over the dub, and the dub as it plays.
            let mut g = vec![0.0f32; n];
            for p in &guide {
                let Some(Some(h)) = heard.get(&p.source) else {
                    continue;
                };
                let from = (((p.start - d.start) as f64 / step).ceil().max(0.0)) as usize;
                let to = ((((p.end - d.start) as f64) / step).floor().max(0.0) as usize).min(n);
                for (k, v) in g.iter_mut().enumerate().take(to).skip(from) {
                    let t = d.start as f64 + k as f64 * step;
                    *v += p.gain * h.at(p.source_at(t), project_rate);
                }
            }
            let x: Vec<f32> = (0..n)
                .map(|k| dub_heard.at(clip_source_at(&d.audio, k as f64 * step), project_rate))
                .collect();
            let Some(map) = timing_align::align(
                &g,
                &x,
                RATE,
                &analysis_settings(settings.tightness, settings.protect_consonants),
            ) else {
                return fail("the guide and the dub hold nothing to align by where they meet");
            };
            let mut after = warp_from(&d.audio, &map, project_rate, dub_heard.frames(project_rate));
            let mut notes = 0;
            if settings.pitch > 0.0 {
                let track = melody::track(&g, RATE);
                match align_pitch(&mut after, dub_heard, &track, settings.pitch, project_rate) {
                    Ok(moved) => notes = moved,
                    Err(why) => tracing::info!("align timing: {}: {why}", d.name),
                }
            }
            Outcome::Aligned {
                clip: d.clip,
                name: d.name,
                before: Box::new(d.audio),
                after: Box::new(after),
                moved_ms: map.largest_move() * 1000.0,
                notes,
            }
        })
        .collect()
}

impl Session {
    /// Align `dubs` (audio clips) to the audio of the `guide` track over
    /// each: warped in one step when the analysis is done (see the
    /// module). `tightness` 0 … 1; the pitch and consonant settings are
    /// the editor's ([`Session::timing_align_settings`]).
    pub fn align_timing(&mut self, guide: TrackId, dubs: &[ClipId], tightness: f32) -> Result<()> {
        let p = &self.project;
        let rate = f64::from(p.sample_rate);
        let guide_track = p
            .track(guide)
            .ok_or_else(|| SessionError::Other(format!("no track {guide}")))?;
        let guide_name = guide_track.name.clone();
        let at = |start| p.timeline.to_samples(start, rate);
        let mut pieces = Vec::new();
        for c in p.clips_of(guide) {
            if c.muted {
                continue;
            }
            let start = at(c.start);
            match &c.content {
                ClipContent::Audio(a) => pieces.push(GuidePiece {
                    start,
                    end: start + a.length,
                    source: a.source,
                    reads: Reads::Clip(a.clone()),
                    gain: 10f32.powf(a.gain_db / 20.0),
                }),
                ClipContent::Takes(f) => {
                    for piece in f.pieces() {
                        let Some(take) = f.takes.get(piece.take) else {
                            continue;
                        };
                        let Some(cut) = take
                            .audio
                            .cut(piece.start - take.start, piece.end - take.start)
                        else {
                            continue;
                        };
                        pieces.push(GuidePiece {
                            start: start + piece.start,
                            end: start + piece.end,
                            source: cut.source,
                            gain: 10f32.powf((cut.gain_db + f.gain_db) / 20.0),
                            reads: Reads::Clip(cut),
                        });
                    }
                }
                ClipContent::Midi(_) => {}
            }
        }
        if pieces.is_empty() {
            return Err(SessionError::Other(format!(
                "‘{guide_name}’ has no audio to align to"
            )));
        }
        let mut list = Vec::new();
        for &id in dubs {
            let Some(c) = p.clip(id) else { continue };
            if c.track == guide || p.is_launcher_clip(id) {
                continue;
            }
            match &c.content {
                ClipContent::Audio(a) if a.reversed => {
                    return Err(SessionError::Other(format!(
                        "‘{}’ plays reversed: it cannot be warped onto a guide",
                        c.name
                    )));
                }
                ClipContent::Audio(a) => list.push(Dub {
                    clip: id,
                    name: c.name.clone(),
                    start: at(c.start),
                    audio: a.clone(),
                }),
                ClipContent::Takes(_) => {
                    return Err(SessionError::Other(format!(
                        "flatten the comp of ‘{}’ to align it",
                        c.name
                    )));
                }
                ClipContent::Midi(_) => {}
            }
        }
        if list.is_empty() {
            return Err(SessionError::Other(
                "choose audio clips on other tracks to align".into(),
            ));
        }
        let mut sources = HashMap::new();
        for id in pieces
            .iter()
            .map(|p| p.source)
            .chain(list.iter().map(|d| d.audio.source))
        {
            if let Some(s) = self.sources.get(&id) {
                sources.insert(id, s.clone());
            }
        }
        let settings = TimingAlignSettings {
            tightness: tightness.clamp(0.0, 1.0),
            ..self.editor.timing_align
        };
        let handle = std::thread::Builder::new()
            .name("faderframe-align-timing".into())
            .spawn(move || work(pieces, list, sources, rate, settings))
            .map_err(|e| SessionError::Other(e.to_string()))?;
        self.timing_align.jobs.push(Job {
            guide: guide_name,
            handle,
        });
        self.revision += 1;
        Ok(())
    }

    /// Alignments still running.
    pub fn aligning_timing(&self) -> bool {
        !self.timing_align.jobs.is_empty()
    }

    /// The editor's alignment settings.
    pub fn timing_align_settings(&self) -> TimingAlignSettings {
        self.editor.timing_align
    }

    /// Wait for the alignments (tests, scripted runs).
    pub fn wait_for_timing_alignment(&mut self) {
        let start = std::time::Instant::now();
        while self.aligning_timing() && start.elapsed().as_secs() < 300 {
            self.poll_timing_alignment();
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        self.poll_timing_alignment();
    }

    /// Apply finished alignments (from the session tick).
    pub(crate) fn poll_timing_alignment(&mut self) {
        let mut i = 0;
        while i < self.timing_align.jobs.len() {
            if !self.timing_align.jobs[i].handle.is_finished() {
                i += 1;
                continue;
            }
            let job = self.timing_align.jobs.remove(i);
            let outcomes = match job.handle.join() {
                Ok(o) => o,
                Err(_) => {
                    self.notify(NoticeLevel::Error, "align timing: the analysis failed");
                    continue;
                }
            };
            self.apply_alignment(&job.guide, outcomes);
            self.revision += 1;
        }
    }

    fn apply_alignment(&mut self, guide: &str, outcomes: Vec<Outcome>) {
        let mut commands = Vec::new();
        let mut said = Vec::new();
        for o in outcomes {
            match o {
                Outcome::Aligned {
                    clip,
                    name,
                    before,
                    after,
                    moved_ms,
                    notes,
                } => {
                    let Some(c) = self.project.clip(clip) else {
                        continue;
                    };
                    if c.content != ClipContent::Audio(*before) {
                        said.push(format!(
                            "‘{name}’ changed while it was aligned: left as it is"
                        ));
                        continue;
                    }
                    commands.push(Command::SetClipContent {
                        clip,
                        start: c.start,
                        content: Box::new(ClipContent::Audio(*after)),
                    });
                    let pitch = if notes > 0 {
                        format!(", {notes} notes toward its pitch")
                    } else {
                        String::new()
                    };
                    said.push(format!("‘{name}’ moved by up to {moved_ms:.0} ms{pitch}"));
                }
                Outcome::Failed { name, why } => said.push(format!("‘{name}’: {why}")),
            }
        }
        let aligned = commands.len();
        if let Err(e) = self.batch("Align Timing", commands) {
            self.notify(NoticeLevel::Error, format!("align timing: {e}"));
            return;
        }
        let level = if aligned > 0 {
            NoticeLevel::Info
        } else {
            NoticeLevel::Warning
        };
        self.notify(level, format!("Aligned to ‘{guide}’: {}", said.join("; ")));
    }

    /// The "Align Timing" entries of an audio clip's menu: a guide track
    /// for each other audio track, then the settings.
    pub fn align_timing_menu(&self, dubs: &[ClipId]) -> Vec<crate::GroupMenuEntry> {
        let p = &self.project;
        let dubs: Vec<ClipId> = dubs
            .iter()
            .copied()
            .filter(|c| {
                p.clip(*c)
                    .is_some_and(|c| c.as_audio().is_some() && !p.is_launcher_clip(c.id))
            })
            .collect();
        let Some(first) = dubs.first().and_then(|c| p.clip(*c)) else {
            return Vec::new();
        };
        let own = first.track;
        let settings = self.editor.timing_align;
        let entry = |label: String, action: crate::Action| crate::GroupMenuEntry {
            label,
            action: Some(action),
            checked: None,
            separated: false,
        };
        let mut out: Vec<crate::GroupMenuEntry> = p
            .folder_order()
            .into_iter()
            .filter(|t| {
                t.id != own
                    && p.clips_of(t.id)
                        .iter()
                        .any(|c| matches!(c.content, ClipContent::Audio(_) | ClipContent::Takes(_)))
            })
            .map(|t| {
                entry(
                    format!("To ‘{}’", t.name),
                    crate::Action::AlignTiming {
                        guide: t.id,
                        dubs: dubs.clone(),
                        tightness: settings.tightness,
                    },
                )
            })
            .collect();
        if out.is_empty() {
            out.push(crate::GroupMenuEntry {
                label: "No other track has audio to align to".into(),
                action: None,
                checked: None,
                separated: false,
            });
        }
        let set = |s: TimingAlignSettings| crate::Action::SetTimingAlign(s);
        for (i, (label, t)) in TimingAlignSettings::TIGHTNESS.into_iter().enumerate() {
            out.push(crate::GroupMenuEntry {
                label: label.into(),
                action: Some(set(TimingAlignSettings {
                    tightness: t,
                    ..settings
                })),
                checked: Some((settings.tightness - t).abs() < 0.05),
                separated: i == 0,
            });
        }
        out.push(crate::GroupMenuEntry {
            label: "Protect Consonants".into(),
            action: Some(set(TimingAlignSettings {
                protect_consonants: !settings.protect_consonants,
                ..settings
            })),
            checked: Some(settings.protect_consonants),
            separated: true,
        });
        out.push(crate::GroupMenuEntry {
            label: "Align Pitch Too".into(),
            action: Some(set(TimingAlignSettings {
                pitch: if settings.pitch > 0.0 { 0.0 } else { 1.0 },
                ..settings
            })),
            checked: Some(settings.pitch > 0.0),
            separated: false,
        });
        out
    }
}

/// The request a script writes as `<guide track>=<dub track>[|<dub
/// track>…][@<tightness>]` (every audio clip of the dub tracks; tightness
/// 0 … 1, else `default`): the [`crate::Action::AlignTiming`] it asks for.
pub fn parse_request(
    project: &faderframe_project::Project,
    text: &str,
    default: f32,
) -> std::result::Result<crate::Action, String> {
    let (rest, tightness) = match text.rsplit_once('@') {
        Some((rest, t)) => (
            rest,
            t.trim()
                .parse::<f32>()
                .map_err(|_| format!("'{t}' is no tightness (0 … 1)"))?,
        ),
        None => (text, default),
    };
    let (guide, dubs) = rest
        .split_once('=')
        .ok_or_else(|| format!("'{text}' is not <guide>=<dub>[|<dub>…][@<tightness>]"))?;
    let track = |name: &str| {
        project
            .tracks
            .iter()
            .find(|t| t.name == name.trim())
            .map(|t| t.id)
            .ok_or_else(|| format!("no track '{}'", name.trim()))
    };
    let guide = track(guide)?;
    let mut clips = Vec::new();
    for name in dubs.split('|') {
        let t = track(name)?;
        clips.extend(
            project
                .clips_of(t)
                .into_iter()
                .filter(|c| matches!(c.content, ClipContent::Audio(_)))
                .map(|c| c.id),
        );
    }
    Ok(crate::Action::AlignTiming {
        guide,
        dubs: clips,
        tightness: tightness.clamp(0.0, 1.0),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_scripted_request_names_tracks_and_a_tightness() {
        let s = crate::Session::demo(faderframe_engine::EngineConfig::default()).unwrap();
        let p = s.project();
        let audio: Vec<&faderframe_project::Track> = p
            .tracks
            .iter()
            .filter(|t| p.clips_of(t.id).iter().any(|c| c.as_audio().is_some()))
            .collect();
        let (guide, dub) = (audio[1], audio[0]);
        let want: Vec<ClipId> = p
            .clips_of(dub.id)
            .iter()
            .filter(|c| c.as_audio().is_some())
            .map(|c| c.id)
            .collect();
        let text = format!("{}={}@0.9", guide.name, dub.name);
        assert_eq!(
            parse_request(p, &text, 0.5),
            Ok(crate::Action::AlignTiming {
                guide: guide.id,
                dubs: want.clone(),
                tightness: 0.9,
            })
        );
        let text = format!("{}={}", guide.name, dub.name);
        assert_eq!(
            parse_request(p, &text, 0.5),
            Ok(crate::Action::AlignTiming {
                guide: guide.id,
                dubs: want,
                tightness: 0.5,
            })
        );
        assert!(parse_request(p, "Nobody=Else", 0.5).is_err());
        assert!(parse_request(p, &guide.name, 0.5).is_err());
    }

    #[test]
    fn the_lowpass_keeps_the_voice_and_takes_what_would_fold() {
        let rate = 48_000.0;
        let tone = |f: f64| -> Vec<f32> {
            (0..48_000)
                .map(|n| (std::f64::consts::TAU * f * n as f64 / rate).sin() as f32)
                .collect()
        };
        let level = |x: &[f32]| {
            let tail = &x[24_000..];
            (tail.iter().map(|v| f64::from(*v).powi(2)).sum::<f64>() / tail.len() as f64).sqrt()
        };
        let mut low = tone(1_000.0);
        lowpass(&mut low, rate, 0.45 * RATE);
        assert!((level(&low) - std::f64::consts::FRAC_1_SQRT_2).abs() < 0.01);
        let mut high = tone(12_000.0);
        lowpass(&mut high, rate, 0.45 * RATE);
        assert!(level(&high) < 0.05);
    }
}
